use crate::{archive::Archive, *};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(crate) fn read_bytes(
    path: &Path,
    limit: u64,
    budget: &mut Budget,
    prefix: bool,
) -> Result<Option<Vec<u8>>> {
    budget.check()?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let Ok(mut file) = options.open(path) else {
        return Ok(None);
    };
    let Ok(metadata) = file.metadata() else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if !prefix && metadata.len() > limit {
        return Err(ImportError::resource("Import file byte limit"));
    }
    let mut data = vec![];
    let mut buffer = [0u8; 32768];
    loop {
        budget.check()?;
        let available = if prefix {
            (limit as usize)
                .saturating_sub(data.len())
                .min(buffer.len())
        } else {
            buffer.len()
        };
        if available == 0 {
            break;
        }
        let read = match file.read(&mut buffer[..available]) {
            Ok(read) => read,
            Err(_) => return Ok(None),
        };
        if read == 0 {
            break;
        }
        budget.charge_input(read as u64)?;
        if data.len() as u64 + read as u64 > limit {
            return Err(ImportError::resource("Import file actual byte limit"));
        }
        data.extend_from_slice(&buffer[..read]);
    }
    Ok(Some(data))
}
fn extension(name: &str) -> String {
    extension_case(name).to_lowercase()
}
fn extension_case(name: &str) -> &str {
    let name = name.rsplit('/').next().unwrap_or(name);
    match name.rfind('.') {
        Some(index) if index > 0 && name != ".." => &name[index..],
        _ => "",
    }
}
fn ext(name: &str) -> String {
    extension(name).trim_start_matches('.').into()
}
fn plain(path: &str, id: &str, budget: &mut Budget) -> Result<Value> {
    #[cfg(windows)]
    let native_path = path.replace('\\', "/");
    #[cfg(windows)]
    let basename = native_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    #[cfg(not(windows))]
    let basename = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let name = if let Some(index) = basename.rfind('.') {
        basename[..index].to_owned()
    } else {
        let mut units: Vec<u16> = basename.encode_utf16().collect();
        units.pop();
        String::from_utf16(&units).map_err(|_| {
            ImportError::new(
                "unsupported_format",
                "Extensionless filename creates a lone UTF-16 surrogate",
            )
        })?
    };
    let extension = ext(basename);
    // .pro is ordinary text for ChordPro, but the source's ProPresenter
    // branch decodes protobuf (and every ZIP .pro invokes checkSpecial).
    if (extension == "pro" && id != "chordpro")
        || matches!(
            extension.as_str(),
            "probundle"
                | "vpc"
                | "qsp"
                | "pdf"
                | "ppt"
                | "pptx"
                | "doc"
                | "docx"
                | "sqlite"
                | "mdb"
        )
    {
        return Err(ImportError::new(
            "unsupported_format",
            "Special/binary import file requires its original converter",
        ));
    }
    let content = String::from_utf8_lossy(
        &read_bytes(Path::new(path), budget.config.text_bytes, budget, false)?.unwrap_or_default(),
    )
    .into_owned();
    Ok(json!({"content":content,"path":path,"name":name,"extension":extension}))
}
// upath 2.0.1 toUnix also collapses repeated separators, preserving two
// initial slashes. Its raw-path hash and normalized-path hash are different.
fn to_unix(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let mut output = String::with_capacity(replaced.len());
    let mut slash = false;
    for (index, ch) in replaced.char_indices() {
        if ch == '/' && slash && index != 1 {
            continue;
        }
        output.push(ch);
        slash = ch == '/';
    }
    output
}
fn hash(path: &str) -> String {
    let mut hash = 0i32;
    for unit in path.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(unit as i32);
    }
    if hash < 0 {
        format!("i{}", -(hash as i64))
    } else {
        format!("a{hash}")
    }
}
fn alert(events: &mut Vec<ImportEvent>, path: &str, error: &ImportError) {
    let message = if error.code == "import_encrypted_archive" {
        "Can't decompress, this file is password protected!".into()
    } else {
        format!("Failed to open zip file: {path}")
    };
    events.push(ImportEvent::new("ALERT", json!(message)));
}
fn read_archive(
    path: &str,
    budget: &mut Budget,
    events: &mut Vec<ImportEvent>,
) -> Result<Option<Archive>> {
    match Archive::open(Path::new(path), budget) {
        Ok(archive) => Ok(Some(archive)),
        Err(error) => {
            if error.code == "import_zip_error" {
                alert(events, path, &error);
                Ok(None)
            } else {
                if error.code == "import_encrypted_archive" {
                    alert(events, path, &error);
                }
                Err(error)
            }
        }
    }
}
// JavaScript String.replace's replacement-string tokens, with a bound checked at
// every append. Regex is literal and global; $$, $&, $`, $' remain observable.
fn replace(input: &str, pattern: &str, replacement: &str, budget: &mut Budget) -> Result<String> {
    if pattern.is_empty() {
        return Ok(input.into());
    }
    let mut output = String::new();
    let mut previous = 0;
    fn append(output: &mut String, value: &str, limit: usize) -> Result<()> {
        if output.len().saturating_add(value.len()) > limit {
            return Err(ImportError::resource("Remapped import text limit"));
        }
        output.push_str(value);
        Ok(())
    }
    for (offset, matched) in input.match_indices(pattern) {
        budget.check()?;
        append(
            &mut output,
            &input[previous..offset],
            budget.config.output_bytes,
        )?;
        let mut index = 0;
        let bytes = replacement.as_bytes();
        let mut begin = 0;
        while index + 1 < bytes.len() {
            if bytes[index] == b'$' && matches!(bytes[index + 1], b'$' | b'&' | b'`' | b'\'') {
                append(
                    &mut output,
                    &replacement[begin..index],
                    budget.config.output_bytes,
                )?;
                let value = match bytes[index + 1] {
                    b'$' => "$",
                    b'&' => matched,
                    b'`' => &input[..offset],
                    _ => &input[offset + matched.len()..],
                };
                append(&mut output, value, budget.config.output_bytes)?;
                index += 2;
                begin = index;
            } else {
                index += 1;
            }
        }
        append(
            &mut output,
            &replacement[begin..],
            budget.config.output_bytes,
        )?;
        previous = offset + matched.len();
    }
    append(&mut output, &input[previous..], budget.config.output_bytes)?;
    Ok(output)
}
struct HashedWriter {
    file: File,
    hash: Sha256,
}
impl Write for HashedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
fn bundle(
    mut archive: Archive,
    directory: &str,
    budget: &mut Budget,
    plan: &mut MediaCommitPlan,
) -> Result<Option<Value>> {
    let Some(data_index) = archive.names.iter().position(|name| name == "data.json") else {
        for index in 0..archive.names.len() {
            if !archive.names[index].ends_with('/') {
                archive.discard(index, budget)?;
            }
        }
        return Ok(None);
    };
    let bytes = archive.read(data_index, budget, budget.config.text_bytes, None)?;
    let mut content = String::from_utf8_lossy(&bytes).into_owned();
    budget.check()?;
    let parsed: Value = serde_json::from_str(&content)
        .map_err(|error| ImportError::new("import_invalid_bundle", error.to_string()))?;
    if parsed.is_null() {
        return Err(ImportError::new(
            "import_invalid_bundle",
            "Null bundle data has no files property",
        ));
    }
    let files = match parsed.get("files") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(files)) => files.as_slice(),
        _ => {
            return Err(ImportError::new(
                "import_invalid_bundle",
                "Bundle files must be an array",
            ))
        }
    };
    budget.references = budget
        .references
        .checked_add(files.len())
        .ok_or_else(|| ImportError::resource("Bundle reference count overflow"))?;
    if budget.references > budget.config.max_entries {
        return Err(ImportError::resource("Bundle referenced file count limit"));
    }
    let mut mappings: HashMap<String, ImportRelativePath> = HashMap::new();
    let mut replacements: Vec<(String, String)> = vec![];
    let mut seen = HashSet::new();
    for file in files {
        budget.check()?;
        let Some(raw) = file.as_str().filter(|raw| !raw.is_empty()) else {
            continue;
        };
        if raw.len() > 32768 || raw.contains('\0') {
            return Err(ImportError::resource(
                "Bundle reference path length/NUL limit",
            ));
        }
        if [
            "data:",
            "http://",
            "https://",
            "blob:",
            "freeshow-protected://",
        ]
        .iter()
        .any(|prefix| raw.starts_with(prefix))
            || Path::new(raw).exists()
        {
            continue;
        }
        let unix = to_unix(raw);
        let basename = unix.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        let extension = extension_case(basename);
        let stem = &basename[..basename.len() - extension.len()];
        let name = format!("{stem}_{}{extension}", hash(&unix));
        if name.is_empty()
            || name.contains([':', '\0'])
            || matches!(name.as_str(), "." | "..")
            || name.len() > 255
        {
            return Err(ImportError::new(
                "import_invalid_path",
                "Invalid remapped media filename",
            ));
        }
        let relative = ImportRelativePath::new(PathBuf::from(directory).join(name))?;
        let final_path = plan.imports_root.join(relative.as_path());
        let final_string = to_unix(final_path.to_str().ok_or_else(|| {
            ImportError::new("import_invalid_path", "Non-Unicode import destination")
        })?);
        if raw != "__proto__" && seen.insert(raw.to_owned()) {
            replacements.push((raw.into(), final_string));
        }
        if plan
            .installs
            .iter()
            .any(|install| install.destination.as_path() == relative.as_path())
        {
            continue;
        }
        if std::fs::symlink_metadata(&final_path).is_ok() {
            if !plan
                .existing
                .iter()
                .any(|guard| guard.destination.as_path() == relative.as_path())
            {
                plan.existing.push(ExistingMediaGuard {
                    destination: relative,
                    expected: FileIdentity::read(&final_path)?,
                });
            }
            continue;
        }
        mappings.insert(
            format!("{stem}__{}{extension}", hash(raw)),
            relative.clone(),
        );
        mappings.insert(basename.into(), relative);
    }
    let mut staged: HashMap<PathBuf, MediaInstall> = HashMap::new();
    for index in 0..archive.names.len() {
        budget.check()?;
        if index == data_index || archive.names[index].ends_with('/') {
            continue;
        }
        let name = &archive.names[index];
        if let Some(relative) = mappings.get(name) {
            if plan.stage.is_none() {
                plan.stage = Some(
                    plan.cleanup.stage(
                        match &budget.config.staging_root {
                            Some(root) => tempfile::Builder::new()
                                .prefix("freeshow-import-")
                                .tempdir_in(root),
                            None => tempfile::Builder::new()
                                .prefix("freeshow-import-")
                                .tempdir(),
                        }
                        .map_err(ImportError::io)?,
                    ),
                );
            }
            let staged_path = plan.stage.as_ref().unwrap().path().join(format!(
                "media-{}-{}",
                plan.installs.len(),
                index
            ));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged_path)
                .map_err(ImportError::io)?;
            let mut writer = HashedWriter {
                file,
                hash: Sha256::new(),
            };
            archive.read(index, budget, budget.config.entry_bytes, Some(&mut writer))?;
            writer.flush().map_err(ImportError::io)?;
            let digest = format!("{:x}", writer.hash.finalize());
            drop(writer.file);
            let expected_source = FileIdentity::read(&staged_path)?;
            // Original stream overwrites when both legacy and hashed names map to
            // the same target. Keep only the final install, never overwrite final.
            if let Some(previous) = staged.insert(
                relative.as_path().into(),
                MediaInstall {
                    staged_path,
                    destination: relative.clone(),
                    expected_source,
                    expected_destination: DestinationGuard::Absent,
                    sha256: digest,
                },
            ) {
                std::fs::remove_file(previous.staged_path).map_err(ImportError::io)?;
            }
        } else {
            archive.discard(index, budget)?;
        }
    }
    let mut destinations: Vec<_> = mappings.values().cloned().collect();
    destinations.sort_by(|a, b| a.as_path().cmp(b.as_path()));
    destinations.dedup_by(|a, b| a.as_path() == b.as_path());
    for relative in destinations {
        if let Some(install) = staged.remove(relative.as_path()) {
            if plan
                .installs
                .iter()
                .any(|old| old.destination.as_path() == relative.as_path())
            {
                return Err(ImportError::new(
                    "import_destination_conflict",
                    "Multiple bundles propose the same destination",
                ));
            }
            plan.unresolved
                .retain(|old| old.as_path() != relative.as_path());
            plan.installs.push(install);
        } else {
            plan.unresolved.push(relative);
        }
    }
    replacements.sort_by_key(|(old, _)| {
        old.parse::<u32>()
            .ok()
            .filter(|number| *number < u32::MAX && number.to_string() == *old)
            .map(|number| (0, number))
            .unwrap_or((1, 0))
    });
    for (old, new) in replacements {
        let windows = old.replace('/', "\\");
        let unix = old.replace('\\', "/");
        let escaped = windows.replace('\\', "\\\\");
        let replacement = new.replace('\\', "\\\\");
        for variant in [&old, &windows, &unix, &escaped] {
            content = replace(&content, variant, &replacement, budget)?;
        }
    }
    Ok(Some(
        json!({"content":content,"name":"data.json","extension":"json"}),
    ))
}
pub(crate) struct Input {
    pub id: String,
    pub paths: Vec<String>,
    pub roots: ImportRoots,
    pub cancel: CancellationToken,
    pub request_cancel: CancellationToken,
    pub started: Instant,
    pub lease: OwnedSemaphorePermit,
}
pub(crate) fn prepare(
    input: Input,
    config: ImportConfig,
    cleanup: service::CleanupSender,
) -> Result<PreparedImport> {
    let Input {
        id,
        paths,
        roots,
        cancel,
        request_cancel,
        started,
        lease,
    } = input;
    let id = id.as_str();
    let mut events = vec![];
    let mut plan = MediaCommitPlan {
        imports_root: roots.imports_root,
        directories: vec![],
        installs: vec![],
        existing: vec![],
        unresolved: vec![],
        stage: None,
        cancel: cancel.clone(),
        request_cancel,
        lease: Some(lease),
        cleanup,
    };
    let mut budget = Budget::new(config, cancel);
    budget.started = started;
    let result = (|| {
        budget.check()?;
        if paths.is_empty() {
            return Ok(());
        }
        // Source chooses and filters these case-sensitive suffixes BEFORE its
        // ZIP branch. Never silently fall back to XML, including mixed input.
        if (id == "openlp" && paths.iter().any(|path| path.ends_with(".sqlite")))
            || (id == "mediashout" && paths.iter().any(|path| path.ends_with(".mdb")))
        {
            return Err(ImportError::new(
                "unsupported_format",
                "Selected files require the original SQLite/MDB import handler",
            ));
        }
        let specialized = match id {
            "freeshow_project" => Some("Projects"),
            "freeshow_template" => Some("Templates"),
            "freeshow_overlay" => Some("Overlays"),
            _ => None,
        };
        let mut data = vec![];
        let text_limit = budget.config.text_bytes;
        if let Some(directory) = specialized {
            events.push(ImportEvent::new("ALERT", json!("popup.importing")));
            let mut archives = vec![];
            for path in &paths {
                let prefix = read_bytes(Path::new(path), 4, &mut budget, true)?.unwrap_or_default();
                if prefix.len() == 4
                    && prefix[0..2] == *b"PK"
                    && [3, 5, 7].contains(&prefix[2])
                    && [4, 6, 8].contains(&prefix[3])
                {
                    archives.push(path);
                } else {
                    let file = plain(path, id, &mut budget)?;
                    budget.charge_output(&file)?;
                    data.push(file);
                }
            }
            plan.directories
                .push(ImportRelativePath::new(PathBuf::from(directory))?);
            for path in archives {
                if let Some(archive) = read_archive(path, &mut budget, &mut events)? {
                    if let Some(file) = bundle(archive, directory, &mut budget, &mut plan)? {
                        budget.charge_output(&file)?;
                        data.push(file);
                    }
                }
            }
        } else {
            let archives: Vec<_> = paths
                .iter()
                .filter(|path| {
                    matches!(
                        path.rsplit('.')
                            .next()
                            .unwrap_or("")
                            .to_lowercase()
                            .as_str(),
                        "zip" | "probundle" | "vpc" | "qsp"
                    )
                })
                .collect();
            if archives.is_empty() {
                for path in &paths {
                    let file = plain(path, id, &mut budget)?;
                    budget.charge_output(&file)?;
                    data.push(file);
                }
            } else {
                for path in archives {
                    if ext(path) != "zip" {
                        return Err(ImportError::new(
                            "unsupported_format",
                            "Third-party bundle is unsupported",
                        ));
                    }
                    let Some(mut archive) = read_archive(path, &mut budget, &mut events)? else {
                        continue;
                    };
                    for index in 0..archive.names.len() {
                        let name = archive.names[index].clone();
                        if name.ends_with('/') {
                            continue;
                        }
                        let extension = ext(&name);
                        let retained_text_entry = match id {
                            "openlp" | "mediashout" | "quelea" | "easyslides" | "verseview" => {
                                extension == "xml" || (id == "mediashout" && extension == "ssc")
                            }
                            "opensong" => extension.is_empty() || extension == "xml",
                            "songbeamer" => extension == "sng",
                            _ => false,
                        };
                        if !retained_text_entry
                            && ![
                                "show",
                                "project",
                                "fstemplate",
                                "fsoverlay",
                                "fstheme",
                                "fsaction",
                                "fsstage",
                                "txt",
                                "chordpro",
                                "cho",
                                "csv",
                                "json",
                            ]
                            .contains(&extension.as_str())
                        {
                            return Err(ImportError::new(
                                "unsupported_format",
                                format!("Unsupported generic ZIP entry: {name}"),
                            ));
                        }
                        let content = String::from_utf8_lossy(&archive.read(
                            index,
                            &mut budget,
                            text_limit,
                            None,
                        )?)
                        .into_owned();
                        let file = json!({"content":content,"name":name,"extension":extension});
                        budget.charge_output(&file)?;
                        data.push(file);
                    }
                }
            }
        }
        if !data.is_empty() || matches!(id, "freeshow_template" | "freeshow_overlay") {
            events.push(ImportEvent::new(
                "IMPORT2",
                json!({"channel":id,"data":data}),
            ));
        }
        budget.check()?;
        let encoded = serde_json::to_vec(&events)
            .map_err(|error| ImportError::new("import_serialization", error.to_string()))?;
        if encoded.len() > budget.config.output_bytes {
            return Err(ImportError::resource("Aggregate import event byte limit"));
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(PreparedImport { events, plan }),
        Err(mut error) => {
            error.prior_events = events
                .into_iter()
                .filter(|event| event.message.channel == "ALERT")
                .collect();
            Err(error)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_is_js_utf16() {
        assert_eq!(hash(""), "a0");
        assert_eq!(hash("/🙌.jpg"), "i238418927");
    }
    #[test]
    fn js_replacement_tokens() {
        let mut budget = Budget::new(ImportConfig::default(), CancellationToken::new());
        assert_eq!(
            replace("aXaX", "X", "$$:$&:$`:$'", &mut budget).unwrap(),
            "a$:X:a:aXa$:X:aXa:"
        );
    }
}
