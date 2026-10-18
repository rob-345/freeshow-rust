use crate::*;
use tokio::sync::OwnedSemaphorePermit;
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        _ => true,
    }
}
pub(crate) fn bounded_payload(
    command: &str,
    payload: &Value,
    config: &CopyConfig,
) -> Result<Value> {
    match command {
        "BUNDLE_MEDIA_FILES" => {
            let output = payload.get("outputPath");
            let output = if truthy(output) {
                output.and_then(Value::as_str).ok_or_else(|| {
                    CopyError::new(
                        "unsupported_variant",
                        "Bundle outputPath must be a string when truthy",
                    )
                })?
            } else {
                ""
            };
            if output.len() > config.path_bytes {
                return Err(CopyError::resource("Bundle output path limit"));
            }
            Ok(json!({"outputPath":output,"openFolder":truthy(payload.get("openFolder"))}))
        }
        "MEDIA_FOLDER_COPY" => {
            let paths = payload
                .get("paths")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    CopyError::new("invalid_request", "MEDIA_FOLDER_COPY requires paths array")
                })?;
            if paths.len() > config.max_files {
                return Err(CopyError::resource("Media copy path count limit"));
            }
            let mut total = 0;
            let mut out = vec![];
            for path in paths {
                let path = path.as_str().ok_or_else(|| {
                    CopyError::new("unsupported_variant", "Media copy paths must be strings")
                })?;
                total += path.len();
                if path.len() > config.path_bytes || total > config.output_bytes {
                    return Err(CopyError::resource("Media copy aggregate/path byte limit"));
                }
                out.push(path);
            }
            Ok(json!({"paths":out}))
        }
        _ => Err(CopyError::new(
            "unsupported_command",
            "Unsupported media copy command",
        )),
    }
}
pub(crate) fn prepare(
    command: &str,
    payload: Value,
    context: CopyContext,
    budget: &mut Budget,
    lease: OwnedSemaphorePermit,
    cleanup: service::Cleanup,
) -> Result<PreparedMediaCopy> {
    budget.check()?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, payload, context, lease, cleanup);
        Err(CopyError::new(
            "unsupported_platform",
            "Media copy filesystem contract verified on Linux only",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let result = linux::prepare(command, payload, context, budget, lease, cleanup);
        result.map_err(|mut error| {
            error.diagnostics = std::mem::take(&mut budget.diagnostics);
            error.omitted_diagnostics = budget.omitted;
            error
        })
    }
}
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashSet,
        fs::{self, Metadata, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    const CHUNK: usize = 32768;
    fn identity(stat: &Metadata) -> FileIdentity {
        FileIdentity {
            device: stat.dev(),
            inode: stat.ino(),
            length: stat.len(),
            mode: stat.mode(),
            mtime_seconds: stat.mtime(),
            mtime_nanos: stat.mtime_nsec(),
            ctime_seconds: stat.ctime(),
            ctime_nanos: stat.ctime_nsec(),
        }
    }
    fn path_check(path: &str, budget: &mut Budget) -> Result<()> {
        budget.check()?;
        if path.len() > budget.config.path_bytes {
            return Err(CopyError::resource("Media copy path length limit"));
        }
        if path.contains('\0') {
            return Err(CopyError::new(
                "unsupported_path_encoding",
                "Embedded NUL in media path",
            ));
        }
        Ok(())
    }
    fn access(path: &str, budget: &mut Budget) -> Result<bool> {
        path_check(path, budget)?;
        let path = std::ffi::CString::new(path).unwrap();
        // SAFETY: valid C path for synchronous source-compatible F_OK check.
        let exists = unsafe { libc::access(path.as_ptr(), libc::F_OK) } == 0;
        budget.check()?;
        Ok(exists)
    }
    fn lstat(path: &Path, budget: &mut Budget) -> Result<PathGuard> {
        budget.check()?;
        let stat = fs::symlink_metadata(path);
        budget.check()?;
        match stat {
            Ok(s) => Ok(PathGuard::Existing(identity(&s))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PathGuard::Absent),
            Err(_) => Err(CopyError::new(
                "copy_guard_error",
                "Could not inspect destination guard",
            )),
        }
    }
    fn directory_guard(path: &Path, budget: &mut Budget) -> Result<DirectoryGuard> {
        let guard = lstat(path, budget)?;
        if matches!(&guard,PathGuard::Existing(s) if s.mode&0o170000!=0o040000) {
            return Err(CopyError::new(
                "unsupported_destination_type",
                "Destination ancestors must be real directories",
            ));
        }
        Ok(match guard {
            PathGuard::Absent => DirectoryGuard::Absent,
            PathGuard::Existing(s) => DirectoryGuard::Existing(DirectoryIdentity {
                device: s.device,
                inode: s.inode,
                mode: s.mode,
            }),
        })
    }
    fn ancestors(root: &str, budget: &mut Budget) -> Result<Vec<AncestorGuard>> {
        path_check(root, budget)?;
        let normalized = paths::normalize(root);
        let mut path = PathBuf::from("/");
        let mut result = vec![AncestorGuard {
            path: PathBuf::from("/"),
            expected: directory_guard(Path::new("/"), budget)?,
        }];
        let mut absent = false;
        for component in Path::new(&normalized).components() {
            if component == std::path::Component::RootDir {
                continue;
            }
            if result.len() >= budget.config.max_depth {
                return Err(CopyError::resource("Destination ancestor depth limit"));
            }
            path.push(component);
            let expected = if absent {
                DirectoryGuard::Absent
            } else {
                directory_guard(&path, budget)?
            };
            absent = matches!(expected, DirectoryGuard::Absent);
            result.push(AncestorGuard {
                path: path.clone(),
                expected,
            });
        }
        Ok(result)
    }
    fn values<'a>(value: Option<&'a Value>, budget: &mut Budget) -> Result<Vec<&'a Value>> {
        budget.check()?;
        match value {
            Some(Value::Array(items)) => Ok(items.iter().collect()),
            Some(Value::Object(items)) => {
                fn index(key: &str) -> Option<u32> {
                    let index = key.parse::<u32>().ok()?;
                    if index == u32::MAX || index.to_string() != key {
                        None
                    } else {
                        Some(index)
                    }
                }
                let mut integer = vec![];
                let mut others = vec![];
                for (key, value) in items {
                    budget.check()?;
                    if let Some(index) = index(key) {
                        integer.push((index, value));
                    } else {
                        others.push(value);
                    }
                }
                integer.sort_by_key(|(index, _)| *index);
                let mut ordered: Vec<_> = integer.into_iter().map(|(_, v)| v).collect();
                ordered.extend(others);
                Ok(ordered)
            }
            // Primitive Object.values never supplies an entry with media/items/
            // shows properties. Ignoring those character/boxed values is exact
            // for these typed traversals; no arbitrary strings are scanned.
            _ => Ok(vec![]),
        }
    }
    fn property<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>> {
        if value.is_null() {
            return Err(CopyError::new(
                "source_extraction_error",
                "Original extraction dereferenced null",
            ));
        }
        Ok(value.get(key))
    }
    fn push_path(value: Option<&Value>, out: &mut Vec<String>, budget: &mut Budget) -> Result<()> {
        if !truthy(value) {
            return Ok(());
        }
        let path = value.and_then(Value::as_str).ok_or_else(|| {
            CopyError::new(
                "unsupported_variant",
                "Truthy extracted media path is not a string",
            )
        })?;
        if path.len() > budget.config.path_bytes || out.len() >= budget.config.max_files {
            return Err(CopyError::resource(
                "Extracted media path count/length limit",
            ));
        }
        let total: usize = out.iter().map(String::len).sum();
        if total + path.len() > budget.config.output_bytes {
            return Err(CopyError::resource("Extracted path byte limit"));
        }
        out.push(path.into());
        Ok(())
    }
    fn items(items: Option<&Value>, out: &mut Vec<String>, budget: &mut Budget) -> Result<()> {
        if !truthy(items) {
            return Ok(());
        }
        let list = items.and_then(Value::as_array).ok_or_else(|| {
            CopyError::new(
                "source_extraction_error",
                "Original media items.forEach requires an array",
            )
        })?;
        for item in list {
            budget.check()?;
            if item.get("type").and_then(Value::as_str) == Some("media") {
                push_path(item.get("src"), out, budget)?;
            }
        }
        Ok(())
    }
    fn validate(value: &Value, budget: &mut Budget, limit: usize) -> Result<usize> {
        let mut stack = vec![(value, 0)];
        let (mut nodes, mut bytes) = (0usize, 0usize);
        while let Some((value, depth)) = stack.pop() {
            budget.check()?;
            nodes += 1;
            if nodes > budget.config.max_nodes || depth > budget.config.max_depth {
                return Err(CopyError::resource("Snapshot JSON node/depth limit"));
            }
            match value {
                Value::String(s) => {
                    bytes = bytes
                        .checked_add(s.len())
                        .ok_or_else(|| CopyError::resource("Snapshot byte limit"))?
                }
                Value::Object(map) => {
                    if map.len() > budget.config.max_nodes.saturating_sub(nodes) {
                        return Err(CopyError::resource("Snapshot container limit"));
                    }
                    for (k, v) in map {
                        bytes = bytes
                            .checked_add(k.len())
                            .ok_or_else(|| CopyError::resource("Snapshot key byte limit"))?;
                        if bytes > limit {
                            return Err(CopyError::resource("Snapshot string byte limit"));
                        }
                        stack.push((v, depth + 1));
                    }
                }
                Value::Array(array) => {
                    if array.len() > budget.config.max_nodes.saturating_sub(nodes) {
                        return Err(CopyError::resource("Snapshot array limit"));
                    }
                    for v in array {
                        stack.push((v, depth + 1));
                    }
                }
                _ => {}
            }
            if bytes > limit || stack.len() > budget.config.max_nodes {
                return Err(CopyError::resource("Snapshot byte/container limit"));
            }
        }
        Ok(bytes)
    }
    fn parse_json(text: &str, budget: &mut Budget) -> Result<Option<Value>> {
        fn preflight(text: &str, budget: &mut Budget) -> Result<()> {
            let (mut depth, mut tokens, mut quoted, mut escape) = (0usize, 0usize, false, false);
            for (index, byte) in text.bytes().enumerate() {
                if index % 4096 == 0 {
                    budget.check()?;
                }
                if quoted {
                    if escape {
                        escape = false;
                    } else if byte == b'\\' {
                        escape = true;
                    } else if byte == b'"' {
                        quoted = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => quoted = true,
                    b'[' | b'{' => {
                        depth += 1;
                        tokens += 1;
                    }
                    b']' | b'}' => depth = depth.saturating_sub(1),
                    b',' | b':' => tokens += 1,
                    _ => {}
                }
                if depth > budget.config.max_depth || tokens > budget.config.max_nodes * 2 {
                    return Err(CopyError::resource("Show JSON declared structure limit"));
                }
            }
            Ok(())
        }
        preflight(text, budget)?;
        let parsed = serde_json::from_str::<Value>(text);
        budget.check()?;
        let value = match parsed {
            Ok(v) => Some(v),
            Err(error) => {
                if error.to_string().contains("surrogate")
                    || error.to_string().contains("number out of range")
                {
                    return Err(CopyError::new(
                        "unsupported_json_variant",
                        "Show JSON Unicode/number variant unsupported",
                    ));
                }
                let end = text.find("}}]").map(|i| i + 3).unwrap_or_else(|| {
                    text.char_indices()
                        .nth(2)
                        .map(|(i, _)| i)
                        .unwrap_or(text.len())
                });
                match serde_json::from_str::<Value>(&text[..end]) {
                    Ok(v) => Some(v),
                    Err(_) => {
                        budget.diagnostic("source_show_parse_error", None, true);
                        None
                    }
                }
            }
        };
        if let Some(v) = &value {
            validate(v, budget, budget.config.text_bytes)?;
        }
        Ok(value)
    }
    fn read_show(path: &str, budget: &mut Budget) -> Result<String> {
        if !access(path, budget)? {
            return Ok(String::new());
        }
        budget.check()?;
        let link = fs::symlink_metadata(path);
        budget.check()?;
        if link.as_ref().is_ok_and(|s| s.is_dir()) {
            return Ok(String::new());
        }
        let stat = fs::metadata(path);
        budget.check()?;
        let stat = match stat {
            Ok(s) => s,
            Err(e) => {
                budget.diagnostic("source_show_read_error", e.raw_os_error(), true);
                return Ok(String::new());
            }
        };
        if stat.is_dir() {
            budget.diagnostic("source_show_read_error", Some(libc::EISDIR), true);
            return Ok(String::new());
        }
        if !stat.is_file() {
            return Err(CopyError::new(
                "unsupported_file_type",
                "Show input must be a regular file",
            ));
        }
        if stat.len() > budget.config.show_bytes as u64 {
            return Err(CopyError::resource("Declared show byte limit"));
        }
        let open = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path);
        budget.check()?;
        let mut file = match open {
            Ok(f) => f,
            Err(e) => {
                budget.diagnostic("source_show_read_error", e.raw_os_error(), true);
                return Ok(String::new());
            }
        };
        let opened = file
            .metadata()
            .map_err(|_| CopyError::new("copy_guard_error", "Opened show metadata unavailable"))?;
        if !opened.is_file() {
            return Err(CopyError::new(
                "unsupported_file_type",
                "Opened show is not regular",
            ));
        }
        let mut bytes = Vec::new();
        let mut chunk = [0u8; CHUNK];
        loop {
            budget.check()?;
            let remaining = budget.config.show_bytes.saturating_sub(bytes.len());
            let read = file.read(&mut chunk[..CHUNK.min(remaining + 1)]);
            budget.check()?;
            let count = match read {
                Ok(n) => n,
                Err(e) => {
                    budget.diagnostic("source_show_read_error", e.raw_os_error(), true);
                    return Ok(String::new());
                }
            };
            if count == 0 {
                break;
            }
            if count > remaining || budget.text + count > budget.config.text_bytes {
                return Err(CopyError::resource("Actual/aggregate show byte limit"));
            }
            budget.text += count;
            bytes.extend_from_slice(&chunk[..count]);
        }
        let after = fs::metadata(path)
            .map_err(|_| CopyError::new("stale_source_guard", "Show disappeared while reading"))?;
        let link_after = fs::symlink_metadata(path).map_err(|_| {
            CopyError::new("stale_source_guard", "Show link disappeared while reading")
        })?;
        let fd_after = file
            .metadata()
            .map_err(|_| CopyError::new("stale_source_guard", "Show handle unavailable"))?;
        let link = link
            .map_err(|_| CopyError::new("stale_source_guard", "Show link metadata unavailable"))?;
        if identity(&stat) != identity(&after)
            || identity(&stat) != identity(&opened)
            || identity(&stat) != identity(&fd_after)
            || identity(&link) != identity(&link_after)
            || bytes.len() as u64 != stat.len()
        {
            return Err(CopyError::new(
                "stale_source_guard",
                "Show changed while reading",
            ));
        }
        budget.show_guards.push(SourceGuard {
            path: path.into(),
            link: identity(&link),
            target: identity(&stat),
        });
        budget.check()?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
    fn extract(context: &CopyContext, budget: &mut Budget) -> Result<Vec<String>> {
        let mut store_bytes = 0;
        for value in [
            &context.stores.projects,
            &context.stores.overlays,
            &context.stores.templates,
        ] {
            store_bytes += validate(value, budget, budget.config.store_bytes - store_bytes)?;
        }
        let mut out = vec![];
        budget.check()?;
        let listing = fs::read_dir(&context.shows_root);
        budget.check()?;
        let mut names = vec![];
        match listing {
            Err(error) => budget.diagnostic(
                if error.kind() == std::io::ErrorKind::NotFound {
                    "shows_root_preparation_required"
                } else {
                    "source_shows_folder_error"
                },
                error.raw_os_error(),
                !matches!(
                    error.raw_os_error(),
                    Some(libc::EACCES | libc::EPERM | libc::ENOENT)
                ),
            ),
            Ok(listing) => {
                for entry in listing {
                    budget.check()?;
                    if names.len() >= budget.config.max_entries {
                        return Err(CopyError::resource("Shows directory entry limit"));
                    }
                    let entry = entry.map_err(|_| {
                        CopyError::new("copy_io_error", "Shows directory iteration failed")
                    })?;
                    let name = entry.file_name().into_string().map_err(|_| {
                        CopyError::new("unsupported_path_encoding", "Show entry is not Unicode")
                    })?;
                    if name.len() > budget.config.path_bytes {
                        return Err(CopyError::resource("Show entry path limit"));
                    }
                    names.push(name);
                }
            }
        }
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        for name in names {
            budget.check()?;
            if !name.to_ascii_lowercase().ends_with(".show") {
                continue;
            }
            let path = paths::join(&context.shows_root, &name);
            path_check(&path, budget)?;
            let text = read_show(&path, budget)?;
            let parsed = parse_json(if text.is_empty() { "{}" } else { &text }, budget)?;
            let show = parsed.as_ref().and_then(|v| match v {
                Value::Array(a) => a.get(1),
                Value::Object(o) => o.get("1"),
                _ => None,
            });
            if !truthy(show) {
                continue;
            }
            let show = show.unwrap();
            for media in values(property(show, "media")?, budget)? {
                let path = property(media, "path")?;
                let id = if truthy(path) {
                    path
                } else {
                    property(media, "id")?
                };
                push_path(id, &mut out, budget)?;
            }
            for slide in values(property(show, "slides")?, budget)? {
                items(property(slide, "items")?, &mut out, budget)?;
            }
        }
        if context.stores.projects.is_null() {
            return Err(CopyError::new(
                "source_extraction_error",
                "Original PROJECTS store was null",
            ));
        }
        for project in values(context.stores.projects.get("projects"), budget)? {
            let shows = project.get("shows");
            if shows.is_none_or(Value::is_null) {
                continue;
            }
            let shows = shows.and_then(Value::as_array).ok_or_else(|| {
                CopyError::new(
                    "source_extraction_error",
                    "Original project shows.forEach requires an array",
                )
            })?;
            for show in shows {
                budget.check()?;
                if !truthy(Some(show)) {
                    continue;
                }
                let kind = show.get("type");
                let kind = if truthy(kind) {
                    kind.and_then(Value::as_str)
                } else {
                    Some("show")
                };
                if matches!(kind, Some("image" | "video" | "audio" | "pdf" | "ppt")) {
                    push_path(show.get("id"), &mut out, budget)?;
                }
            }
        }
        for source in [&context.stores.overlays, &context.stores.templates] {
            for entry in values(Some(source), budget)? {
                items(property(entry, "items")?, &mut out, budget)?;
            }
        }
        let mut unique = HashSet::new();
        out.retain(|p| unique.insert(p.clone()));
        Ok(out)
    }
    fn stage_file(
        source: &str,
        destination: MediaRelativePath,
        stage: &Path,
        index: usize,
        budget: &mut Budget,
    ) -> Result<std::result::Result<Install, Option<i32>>> {
        budget.check()?;
        let link = fs::symlink_metadata(source);
        let target = fs::metadata(source);
        budget.check()?;
        let (link, target) = match (link, target) {
            (Ok(l), Ok(t)) => (l, t),
            (Err(e), _) | (_, Err(e)) => return Ok(Err(e.raw_os_error())),
        };
        if target.is_dir() {
            return Ok(Err(Some(libc::EISDIR)));
        }
        if !target.is_file() {
            return Err(CopyError::new(
                "unsupported_file_type",
                "Media input must be a regular file",
            ));
        }
        if target.len() > budget.config.file_bytes
            || budget.copied.saturating_add(target.len()) > budget.config.copied_bytes
        {
            return Err(CopyError::resource(
                "Declared media file/aggregate byte limit",
            ));
        }
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(source);
        budget.check()?;
        let mut file = match opened {
            Ok(f) => f,
            Err(e) => return Ok(Err(e.raw_os_error())),
        };
        let opened = file
            .metadata()
            .map_err(|_| CopyError::new("copy_guard_error", "Opened media metadata unavailable"))?;
        if !opened.is_file() {
            return Err(CopyError::new(
                "unsupported_file_type",
                "Opened media is not regular",
            ));
        }
        if identity(&opened) != identity(&target) {
            return Err(CopyError::new(
                "stale_source_guard",
                "Media source changed before staging",
            ));
        }
        let path = stage.join(format!("{index}.media"));
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| {
                CopyError::new("staging_io_error", "Could not create private staged media")
            })?;
        let mut count = 0u64;
        let mut hash = Sha256::new();
        let mut chunk = [0u8; CHUNK];
        loop {
            budget.check()?;
            let remaining =
                (budget.config.file_bytes - count).min(budget.config.copied_bytes - budget.copied);
            let length = CHUNK.min(remaining.saturating_add(1) as usize);
            let read = file.read(&mut chunk[..length]);
            budget.check()?;
            let read = match read {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    drop(staged);
                    fs::remove_file(&path).map_err(|_| {
                        CopyError::new(
                            "staging_io_error",
                            "Could not remove incomplete staged media",
                        )
                    })?;
                    return Ok(Err(e.raw_os_error()));
                }
            };
            if read == 0 {
                break;
            }
            if read as u64 > remaining {
                return Err(CopyError::resource(
                    "Actual media file/aggregate byte limit",
                ));
            }
            staged.write_all(&chunk[..read]).map_err(|_| {
                CopyError::new("staging_io_error", "Could not write private staged media")
            })?;
            hash.update(&chunk[..read]);
            count += read as u64;
            budget.copied += read as u64;
            #[cfg(test)]
            {
                let hook = budget.hooks.chunk.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
        }
        staged.flush().map_err(|_| {
            CopyError::new("staging_io_error", "Could not flush private staged media")
        })?;
        budget.check()?;
        let link_after = fs::symlink_metadata(source)
            .map_err(|_| CopyError::new("stale_source_guard", "Media source disappeared"))?;
        let target_after = fs::metadata(source)
            .map_err(|_| CopyError::new("stale_source_guard", "Media target disappeared"))?;
        let fd_after = file
            .metadata()
            .map_err(|_| CopyError::new("stale_source_guard", "Media handle became unavailable"))?;
        budget.check()?;
        if identity(&link) != identity(&link_after)
            || identity(&target) != identity(&target_after)
            || identity(&target) != identity(&fd_after)
            || count != target.len()
        {
            return Err(CopyError::new(
                "stale_source_guard",
                "Media changed while staging",
            ));
        }
        let expected_staged = identity(
            &staged
                .metadata()
                .map_err(|_| CopyError::new("staging_io_error", "Staged identity unavailable"))?,
        );
        Ok(Ok(Install {
            destination,
            expected_destination: PathGuard::Absent,
            staged_path: path,
            expected_staged,
            source: SourceGuard {
                path: source.into(),
                link: identity(&link),
                target: identity(&target),
            },
            sha256: format!("{:x}", hash.finalize()),
        }))
    }
    pub(super) fn prepare(
        command: &str,
        payload: Value,
        context: CopyContext,
        budget: &mut Budget,
        lease: OwnedSemaphorePermit,
        cleanup: service::Cleanup,
    ) -> Result<PreparedMediaCopy> {
        let bundle = command == "BUNDLE_MEDIA_FILES";
        let mut effects = vec![];
        if bundle {
            let guards = ancestors(&context.shows_root, budget)?;
            effects.push(CopyEffect::EnsureShowsRoot {
                path: context.shows_root.clone().into(),
                ancestors: guards,
                listing_guard: lstat(Path::new(&context.shows_root), budget)?,
            });
        }
        let candidates = if bundle {
            extract(&context, budget)?
        } else {
            payload["paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        };
        let requested = payload["outputPath"].as_str().unwrap_or("");
        let destination_root = if bundle {
            if requested.is_empty() || candidates.is_empty() {
                context.default_media.clone()
            } else {
                if !candidates.is_empty()
                    && context.approved_bundle_root.as_deref() != Some(requested)
                {
                    return Err(CopyError::new(
                        "invalid_context",
                        "Requested bundle root was not frozen/approved by host",
                    ));
                }
                requested.into()
            }
        } else {
            context.sync_root.clone()
        };
        let ensure_destination = (!bundle && context.sync_default)
            || (bundle && !candidates.is_empty() && requested.is_empty());
        let destination_ancestors = if ensure_destination {
            ancestors(&destination_root, budget)?
        } else {
            vec![]
        };
        if ensure_destination {
            effects.push(CopyEffect::EnsureDestinationRoot);
        }
        let stage = match budget.config.staging_root.as_deref() {
            Some(root) => tempfile::Builder::new()
                .prefix("freeshow-copy-")
                .tempdir_in(root),
            None => tempfile::Builder::new().prefix("freeshow-copy-").tempdir(),
        }
        .map_err(|_| CopyError::new("staging_io_error", "Could not create private copy staging"))?;
        let stage_path = stage.path().to_owned();
        let state = Arc::new(StageState {
            status: AtomicU8::new(LIVE),
        });
        let created = Instant::now();
        cleanup.register(
            stage,
            lease,
            state.clone(),
            created,
            budget.config.prepared_ttl,
        )?;
        let mut plan = PreparedMediaCopy {
            data_root: context.data_root.clone(),
            destination_root,
            destination_ancestors,
            show_guards: std::mem::take(&mut budget.show_guards),
            effects,
            observations: vec![],
            candidates,
            diagnostics: vec![],
            omitted_diagnostics: 0,
            deferred_reply: DeferredReply::NoReply,
            open_folder: bundle
                && !payload["openFolder"].is_null()
                && truthy(payload.get("openFolder")),
            stage_path,
            state,
            cleanup,
            cancel: budget.cancel.clone(),
            request_cancel: budget.request_cancel.clone(),
            created,
            ttl: budget.config.prepared_ttl,
        };
        if bundle && plan.candidates.is_empty() {
            plan.open_folder = false;
        }
        if bundle {
            if let Some(CopyEffect::EnsureShowsRoot {
                path,
                listing_guard,
                ..
            }) = plan.effects.first()
            {
                if &lstat(path, budget)? != listing_guard {
                    return Err(CopyError::new(
                        "stale_source_guard",
                        "Shows directory changed while enumerating",
                    ));
                }
            }
        }
        let mut captured_destination = ensure_destination;
        let mut directories = HashSet::new();
        let mut targets = HashSet::new();
        let mut attempted = false;
        for (index, source) in plan.candidates.iter().enumerate() {
            budget.check()?;
            if source.starts_with(&plan.destination_root) {
                plan.observations.push(CopyObservation::PrefixSkipped {
                    source: source.clone(),
                });
                continue;
            }
            if !access(source, budget)? {
                plan.observations.push(CopyObservation::Missing {
                    source: source.clone(),
                });
                continue;
            }
            if !captured_destination {
                plan.destination_ancestors = ancestors(&plan.destination_root, budget)?;
                captured_destination = true;
            }
            let folder = paths::parent_folder_id(source);
            let directory = MediaRelativePath::new(folder.clone())?;
            let parent = Path::new(&plan.destination_root).join(&folder);
            path_check(parent.to_str().unwrap(), budget)?;
            if directories.insert(folder.clone()) {
                let expected = directory_guard(&parent, budget)?;
                plan.effects.push(CopyEffect::EnsureDirectory {
                    destination: directory.clone(),
                    expected,
                });
            }
            let name = paths::basename(source);
            if matches!(name, "" | "." | "..") {
                plan.observations
                    .push(CopyObservation::EnsuredDirectoryDestination {
                        source: source.clone(),
                        destination: if name == ".." { None } else { Some(directory) },
                    });
                continue;
            }
            let relative = MediaRelativePath::new(format!("{folder}/{name}"))?;
            let target = paths::join(&plan.destination_root, &relative.0);
            path_check(&target, budget)?;
            if access(&target, budget)? {
                let expected = match lstat(Path::new(&target), budget)? {
                    PathGuard::Existing(e) => e,
                    PathGuard::Absent => {
                        return Err(CopyError::new(
                            "stale_destination_guard",
                            "Existing destination disappeared",
                        ))
                    }
                };
                let followed = if expected.mode & 0o170000 == 0o120000 {
                    Some(identity(&fs::metadata(&target).map_err(|_| {
                        CopyError::new(
                            "stale_destination_guard",
                            "Destination symlink target disappeared",
                        )
                    })?))
                } else {
                    None
                };
                plan.effects.push(CopyEffect::SkipExisting {
                    destination: relative,
                    expected,
                    followed,
                });
                continue;
            }
            if !matches!(lstat(Path::new(&target), budget)?, PathGuard::Absent) {
                return Err(CopyError::new(
                    "unsupported_destination_type",
                    "Dangling/unsearchable destination cannot be safely overwritten",
                ));
            }
            if !targets.insert(relative.0.clone()) {
                plan.observations
                    .push(CopyObservation::DuplicateDestination {
                        source: source.clone(),
                        destination: relative,
                    });
                continue;
            }
            attempted = true;
            match stage_file(source, relative.clone(), &plan.stage_path, index, budget)? {
                Ok(file) => plan.effects.push(CopyEffect::Install {
                    file: Box::new(file),
                }),
                Err(os_code) => {
                    budget.diagnostic("source_copy_attempt_error", os_code, true);
                    plan.effects.push(CopyEffect::CopyAttemptFailure {
                        destination: relative,
                        source: source.clone(),
                        os_code,
                    });
                }
            }
        }
        if !bundle {
            plan.deferred_reply = DeferredReply::MediaFolderCopy {
                changed_if_effects_resolved: attempted,
            };
        }
        plan.diagnostics = budget.diagnostics.clone();
        plan.omitted_diagnostics = budget.omitted;
        budget.check()?;
        // Stream the manifest into a capped counter; do not duplicate output JSON.
        let manifest = plan.projection();
        let mut counter = Count {
            bytes: 0,
            limit: budget.config.output_bytes,
        };
        serde_json::to_writer(&mut counter, &manifest)
            .map_err(|_| CopyError::resource("Copy manifest output limit"))?;
        budget.check()?;
        plan.ensure_current()?;
        Ok(plan)
    }
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl Write for Count {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if data.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("copy output limit"));
            }
            self.bytes += data.len();
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
