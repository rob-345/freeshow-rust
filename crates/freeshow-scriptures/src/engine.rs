use crate::*;
#[cfg(target_os = "linux")]
use serde_json::json;
pub(crate) fn process(request: Request, budget: &mut Budget) -> Result<PreparedScriptureReply> {
    budget.check()?;
    #[cfg(not(target_os = "linux"))]
    {
        let Request { context, id, name } = request;
        drop((context, id, name));
        Err(Error::unsupported(
            "Scripture filesystem/path backend verified on Linux only",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let mut diagnostics = Vec::new();
        if !request.context.directory_attempted
            && !linux::exists(request.context.bibles_root(), budget)?
        {
            return Ok(PreparedScriptureReply {
                data: None,
                diagnostics,
                folder_intents: vec![FolderIntent::EnsureBiblesDirectory {
                    path: request.context.bibles.clone(),
                }],
                stats: std::mem::take(&mut budget.stats),
            });
        }
        let filename = format!("{}.fsb", request.name);
        let primary = join(request.context.bibles.to_str().unwrap(), &filename);
        let legacy = join(request.context.legacy.to_str().unwrap(), &filename);
        if primary.len() > budget.limits.path_bytes || legacy.len() > budget.limits.path_bytes {
            return Err(Error::limit("Joined scripture path bound"));
        }
        budget.charge(primary.len() + legacy.len())?;
        let retained = budget.allocated;
        let first = linux::load(&primary, &request.id, budget, &mut diagnostics)?;
        let fallback = first.get("error").is_some();
        drop(first);
        // The first source result is intentionally discarded before the second
        // read. Release that stage's conservative allocation account, retaining
        // peak evidence and cumulative byte/node/operation work counters.
        budget.allocated = retained;
        #[cfg(test)]
        {
            let hook = budget.hooks.after_first.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let mut data = linux::load(
            if fallback { &legacy } else { &primary },
            &request.id,
            budget,
            &mut diagnostics,
        )?;
        migrate(&mut data, budget)?;
        struct Counter<'a> {
            bytes: usize,
            budget: &'a mut Budget,
            error: Option<Error>,
        }
        impl std::io::Write for Counter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Err(error) = self.budget.check() {
                    self.error = Some(error);
                    return Err(std::io::Error::other("scripture budget"));
                }
                if bytes.len() > self.budget.limits.output_bytes.saturating_sub(self.bytes) {
                    return Err(std::io::Error::other("scripture output"));
                }
                self.bytes += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Counter {
            bytes: 0,
            budget,
            error: None,
        };
        let encoded = serde_json::to_writer(&mut count, &data);
        if let Some(error) = count.error {
            return Err(error);
        }
        if encoded.is_err() {
            return Err(Error::limit("Scripture encoded reply bound"));
        }
        budget.check()?;
        Ok(PreparedScriptureReply {
            data: Some(data),
            diagnostics,
            folder_intents: vec![],
            stats: std::mem::take(&mut budget.stats),
        })
    }
}
#[cfg(target_os = "linux")]
fn indexed(value: &Value, index: usize) -> Option<&Value> {
    match value {
        Value::Array(values) => values.get(index),
        Value::Object(values) => values.get(&index.to_string()),
        _ => None,
    }
}
#[cfg(target_os = "linux")]
fn indexed_mut(value: &mut Value, index: usize) -> Option<&mut Value> {
    match value {
        Value::Array(values) => values.get_mut(index),
        Value::Object(values) => values.get_mut(&index.to_string()),
        _ => None,
    }
}
#[cfg(target_os = "linux")]
fn migrate(data: &mut Value, budget: &mut Budget) -> Result<()> {
    let first = data
        .get("content")
        .and_then(|v| indexed(v, 1))
        .and_then(|v| v.get("books"))
        .and_then(|v| indexed(v, 0))
        .and_then(|v| v.get("chapters"))
        .and_then(|v| indexed(v, 0))
        .and_then(|v| v.get("verses"))
        .and_then(|v| indexed(v, 0))
        .and_then(|v| v.get("value"));
    if !first.is_some_and(truthy) {
        return Ok(());
    }
    let books = indexed_mut(data.get_mut("content").unwrap(), 1)
        .and_then(|v| v.get_mut("books"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::unsupported("Legacy scripture books has no Array.forEach"))?;
    for book in books {
        budget.check()?;
        let chapters = book
            .get_mut("chapters")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| Error::unsupported("Legacy scripture chapters has no Array.forEach"))?;
        for chapter in chapters {
            budget.check()?;
            let verses = chapter
                .get_mut("verses")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    Error::unsupported("Legacy scripture verses has no Array.forEach")
                })?;
            if verses.len()
                > budget
                    .limits
                    .verses
                    .saturating_sub(budget.stats.visited_verses)
            {
                return Err(Error::limit("Scripture verse traversal bound"));
            }
            for verse in verses {
                budget.check()?;
                budget.stats.visited_verses += 1;
                let verse = verse
                    .as_object_mut()
                    .ok_or_else(|| Error::unsupported("Legacy scripture verse is not an object"))?;
                if !verse.get("text").is_some_and(truthy) {
                    let value = verse
                        .remove("value")
                        .filter(truthy)
                        .unwrap_or_else(|| json!(""));
                    // Move the original value without cloning unknown nested data.
                    budget.charge(256)?;
                    verse.insert("text".into(), value);
                    budget.stats.migrated_verses += 1;
                }
            }
        }
    }
    Ok(())
}
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        fs::{self, OpenOptions},
        io::Read,
        os::unix::fs::OpenOptionsExt,
    };
    pub(super) fn exists(path: &Path, budget: &mut Budget) -> Result<bool> {
        budget.check()?;
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return Ok(false);
        };
        // SAFETY: the valid owned C string lives through access; F_OK has no writes.
        let result = unsafe { libc::access(path.as_ptr(), libc::F_OK) } == 0;
        budget.check()?;
        Ok(result)
    }
    fn diagnostic(
        diagnostics: &mut Vec<Diagnostic>,
        operation: &'static str,
        error: Option<&std::io::Error>,
    ) {
        debug_assert!(diagnostics.len() < 8);
        diagnostics.push(Diagnostic {
            code: if error.is_some() {
                "source_read_error"
            } else {
                "source_parse_error"
            },
            operation,
            os_code: error.and_then(std::io::Error::raw_os_error),
            source_error_logged: true,
        });
    }
    fn read(
        path: &str,
        budget: &mut Budget,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Option<String>> {
        if !exists(Path::new(path), budget)? {
            return Ok(None);
        }
        budget.check()?;
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => return Ok(None),
            Err(error) => {
                diagnostic(diagnostics, "readFile", Some(&error));
                return Ok(None);
            }
            _ => {}
        }
        budget.check()?;
        let metadata = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(error) => {
                diagnostic(diagnostics, "readFile", Some(&error));
                return Ok(None);
            }
        };
        budget.check()?;
        if metadata.is_dir() {
            diagnostic(
                diagnostics,
                "readFile",
                Some(&std::io::Error::from_raw_os_error(libc::EISDIR)),
            );
            return Ok(None);
        }
        if !metadata.is_file() {
            return Err(Error::unsupported("Scripture target is not a regular file"));
        }
        let remaining = budget
            .limits
            .read_bytes
            .saturating_sub(budget.stats.read_bytes);
        if metadata.len() > remaining as u64 {
            return Err(Error::limit("Aggregate scripture read bound"));
        }
        budget.check()?;
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => file,
            Err(error) => {
                diagnostic(diagnostics, "readFile", Some(&error));
                return Ok(None);
            }
        };
        budget.check()?;
        let metadata = match file.metadata() {
            Ok(meta) => meta,
            Err(error) => {
                diagnostic(diagnostics, "readFile", Some(&error));
                return Ok(None);
            }
        };
        if metadata.is_dir() {
            diagnostic(
                diagnostics,
                "readFile",
                Some(&std::io::Error::from_raw_os_error(libc::EISDIR)),
            );
            return Ok(None);
        }
        if !metadata.is_file() {
            return Err(Error::unsupported("Opened scripture target is not regular"));
        }
        if metadata.len() > remaining as u64 {
            return Err(Error::limit("Opened scripture read bound"));
        }
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 32768];
        loop {
            budget.check()?;
            let remaining = budget
                .limits
                .read_bytes
                .saturating_sub(budget.stats.read_bytes);
            let size = chunk.len().min(remaining.saturating_add(1));
            let size = match file.read(&mut chunk[..size]) {
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    diagnostic(diagnostics, "readFile", Some(&error));
                    return Ok(None);
                }
            };
            budget.check()?;
            if size == 0 {
                break;
            }
            if size > remaining {
                return Err(Error::limit("Actual scripture read bound"));
            }
            budget.charge(size.saturating_mul(2))?;
            bytes
                .try_reserve_exact(size)
                .map_err(|_| Error::limit("Scripture input allocation"))?;
            bytes.extend_from_slice(&chunk[..size]);
            budget.stats.read_bytes += size;
        }
        // Same maximal-subpart UTF-8 replacement as Buffer.toString. Keep BOM.
        budget.charge(bytes.len().saturating_mul(3))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
    pub(super) fn load(
        path: &str,
        id: &Value,
        budget: &mut Budget,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Value> {
        budget.check()?;
        budget.charge(path.len() + 256)?;
        budget.stats.load_paths.push(path.into());
        let missing = || json!({"error":"not_found","id":id});
        // Original loadFile probes once, then readFile probes independently.
        if !exists(Path::new(path), budget)? {
            return Ok(missing());
        }
        let Some(content) = read(path, budget, diagnostics)? else {
            return Ok(missing());
        };
        if content.is_empty() {
            return Ok(missing());
        }
        let retained = budget.allocated;
        let parsed = crate::json::parse(&content, budget)?;
        let parsed = match parsed {
            Some(value) => Some(value),
            None => {
                budget.allocated = retained;
                let end = content.find("}}]").map(|i| i + 3).unwrap_or_else(|| {
                    let mut units = 0;
                    let mut end = 0;
                    for (at, ch) in content.char_indices() {
                        if units + ch.len_utf16() > 2 {
                            break;
                        }
                        units += ch.len_utf16();
                        end = at + ch.len_utf8();
                    }
                    end
                });
                crate::json::parse(&content[..end], budget)?
            }
        };
        let Some(mut value) = parsed else {
            diagnostic(diagnostics, "parseJSON", None);
            return Ok(missing());
        };
        if !truthy(&value) {
            return Ok(missing());
        }
        if truthy(id) {
            match &mut value {
                Value::Array(values) => {
                    if values.is_empty() {
                        budget.charge(256)?;
                        values.push(id.clone());
                    } else {
                        values[0] = id.clone();
                    }
                }
                Value::Object(values) => {
                    budget.charge(256)?;
                    values.insert("0".into(), id.clone());
                }
                _ => {
                    return Err(Error::unsupported(
                        "Truthy primitive content cannot accept source content ID",
                    ))
                }
            }
        }
        let mut data = serde_json::Map::new();
        data.insert("id".into(), id.clone());
        data.insert("content".into(), value);
        Ok(Value::Object(data))
    }
}
