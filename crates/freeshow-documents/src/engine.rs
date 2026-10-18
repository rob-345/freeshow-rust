use crate::*;
pub(crate) fn process(request: Request, budget: &mut Budget) -> Result<PreparedDocumentReply> {
    budget.check()?;
    #[cfg(not(target_os = "linux"))]
    {
        match request {
            Request::File(path) => drop(path),
            Request::Bibles(context) => drop(context),
        }
        Err(DocumentError::new(
            "unsupported_platform",
            "Document filesystem/path contract verified on Linux only",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let reply = match request {
            Request::File(path) => linux::read_file(&path, budget)?,
            Request::Bibles(context) => linux::read_bibles(&context, budget)?,
        };
        // Bound serialized data without constructing an additional JSON buffer.
        let mut counter = Counter {
            bytes: 0,
            limit: budget.config.output_bytes,
            budget,
            error: None,
        };
        let counted = serde_json::to_writer(&mut counter, &reply);
        if let Some(error) = counter.error {
            return Err(error);
        }
        if counted.is_err() {
            return Err(DocumentError::resource("Document encoded output limit"));
        }
        budget.check()?;
        Ok(reply)
    }
}
#[cfg(target_os = "linux")]
struct Counter<'a> {
    bytes: usize,
    limit: usize,
    budget: &'a mut Budget,
    error: Option<DocumentError>,
}
#[cfg(target_os = "linux")]
impl std::io::Write for Counter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Err(error) = self.budget.check() {
            self.error = Some(error);
            return Err(std::io::Error::other("document budget"));
        }
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("document output limit"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use serde_json::json;
    use std::{
        fs::{self, OpenOptions},
        io::Read,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    const CHUNK: usize = 32768;
    fn reply(data: Value) -> PreparedDocumentReply {
        PreparedDocumentReply {
            data,
            diagnostics: vec![],
            folder_intents: vec![],
        }
    }
    fn diagnostic(
        code: &'static str,
        operation: &'static str,
        error: &std::io::Error,
        logged: bool,
    ) -> DocumentDiagnostic {
        DocumentDiagnostic {
            code,
            operation,
            os_code: error.raw_os_error(),
            source_error_logged: logged,
        }
    }
    fn empty_read(error: Option<&std::io::Error>) -> PreparedDocumentReply {
        let mut result = reply(json!({"content":""}));
        if let Some(error) = error {
            result
                .diagnostics
                .push(diagnostic("source_read_error", "readFile", error, true));
        }
        result
    }
    fn exists(path: &str, budget: &mut Budget) -> Result<bool> {
        budget.check()?;
        // Node existsSync returns false (does not throw) for embedded NUL paths.
        let Ok(path) = std::ffi::CString::new(path) else {
            return Ok(false);
        };
        // SAFETY: a valid C string remains alive for this synchronous F_OK call.
        let exists = unsafe { libc::access(path.as_ptr(), libc::F_OK) } == 0;
        budget.check()?;
        Ok(exists)
    }
    pub(super) fn read_file(path: &str, budget: &mut Budget) -> Result<PreparedDocumentReply> {
        if !exists(path, budget)? {
            return Ok(empty_read(None));
        }
        budget.check()?;
        let link = fs::symlink_metadata(path);
        budget.check()?;
        match link {
            Ok(stat) if stat.is_dir() => return Ok(empty_read(None)),
            Err(error) => return Ok(empty_read(Some(&error))),
            _ => {}
        }
        // Check before opening devices/FIFOs; O_NONBLOCK and a second fstat also
        // prevent a path replacement with a FIFO from blocking a worker in open.
        budget.check()?;
        let stat = fs::metadata(path);
        budget.check()?;
        let stat = match stat {
            Ok(stat) => stat,
            Err(error) => return Ok(empty_read(Some(&error))),
        };
        if stat.is_dir() {
            return Ok(empty_read(Some(&std::io::Error::from_raw_os_error(
                libc::EISDIR,
            ))));
        }
        if !stat.is_file() {
            return Err(DocumentError::new(
                "unsupported_file_type",
                "Document reads require a regular file",
            ));
        }
        if stat.len() > budget.config.read_bytes as u64 {
            return Err(DocumentError::resource("Document declared file size limit"));
        }
        budget.check()?;
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path);
        budget.check()?;
        let mut file = match opened {
            Ok(file) => file,
            Err(error) => return Ok(empty_read(Some(&error))),
        };
        let stat = file.metadata();
        budget.check()?;
        let stat = match stat {
            Ok(stat) => stat,
            Err(error) => return Ok(empty_read(Some(&error))),
        };
        if stat.is_dir() {
            return Ok(empty_read(Some(&std::io::Error::from_raw_os_error(
                libc::EISDIR,
            ))));
        }
        if !stat.is_file() {
            return Err(DocumentError::new(
                "unsupported_file_type",
                "Document opened target is not a regular file",
            ));
        }
        if stat.len() > budget.config.read_bytes as u64 {
            return Err(DocumentError::resource("Document opened file size limit"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(budget.config.read_bytes)
            .map_err(|_| DocumentError::resource("Document read buffer allocation failed"))?;
        let mut chunk = [0_u8; CHUNK];
        loop {
            budget.check()?;
            let remaining = budget.config.read_bytes.saturating_sub(bytes.len());
            let read = file.read(&mut chunk[..CHUNK.min(remaining + 1)]);
            budget.check()?;
            let count = match read {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Ok(empty_read(Some(&error))),
            };
            if count == 0 {
                break;
            }
            if count > remaining {
                return Err(DocumentError::resource("Document actual read byte limit"));
            }
            bytes.extend_from_slice(&chunk[..count]);
            #[cfg(test)]
            {
                let hook = budget.hooks.chunk.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
        }
        budget.check()?;
        // Buffer.toString('utf8') and Rust use maximal-subpart replacement. BOM
        // remains a literal U+FEFF; newline/NUL/Unicode data is otherwise untouched.
        let content = String::from_utf8_lossy(&bytes).into_owned();
        budget.check()?;
        Ok(reply(json!({"content": content})))
    }
    fn needs_directory(context: &DocumentContext) -> PreparedDocumentReply {
        let mut result = reply(json!([]));
        result
            .folder_intents
            .push(FolderIntent::EnsureBiblesDirectory {
                path: context.bibles_root.clone(),
            });
        result.diagnostics.push(DocumentDiagnostic {
            code: "bibles_directory_preparation_required",
            operation: "getDataFolderPath",
            os_code: None,
            source_error_logged: false,
        });
        result
    }
    fn folder_error(error: &std::io::Error) -> PreparedDocumentReply {
        let mut result = reply(json!([]));
        let logged = !matches!(error.raw_os_error(), Some(libc::EACCES | libc::EPERM));
        result.diagnostics.push(diagnostic(
            "source_folder_error",
            "readFolder",
            error,
            logged,
        ));
        result
    }
    pub(super) fn read_bibles(
        context: &DocumentContext,
        budget: &mut Budget,
    ) -> Result<PreparedDocumentReply> {
        let root = context.bibles_root.to_str().ok_or_else(|| {
            DocumentError::new("unsupported_path_encoding", "Bibles root is not Unicode")
        })?;
        if !exists(root, budget)? {
            return Ok(needs_directory(context));
        }
        budget.check()?;
        let stat = fs::symlink_metadata(root);
        budget.check()?;
        let stat = stat.map_err(|_| {
            DocumentError::new("source_stat_error", "Original createFolder lstat failed")
        })?;
        if !stat.is_dir() {
            budget.check()?;
            let followed = fs::metadata(root);
            budget.check()?;
            if !followed.is_ok_and(|stat| stat.is_dir()) {
                return Ok(needs_directory(context));
            }
            // Source recursive mkdir follows an existing directory symlink and
            // succeeds without a write; no unresolved preparation is necessary.
        }
        budget.check()?;
        let listing = fs::read_dir(root);
        budget.check()?;
        let listing = match listing {
            Ok(listing) => listing,
            Err(error) => return Ok(folder_error(&error)),
        };
        let mut names = Vec::new();
        let mut stored = 0_usize;
        for entry in listing {
            budget.check()?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => return Ok(folder_error(&error)),
            };
            if names.len() >= budget.config.entries {
                return Err(DocumentError::resource("Bibles entry limit"));
            }
            let name = entry.file_name();
            if name.as_bytes().len() > budget.config.path_bytes {
                return Err(DocumentError::resource("Bibles entry path length limit"));
            }
            let name = name.into_string().map_err(|_| {
                DocumentError::new(
                    "unsupported_path_encoding",
                    "Non-Unicode Bibles entry names are not supported",
                )
            })?;
            stored = stored
                .checked_add(name.len())
                .ok_or_else(|| DocumentError::resource("Bibles name byte limit"))?;
            if stored > budget.config.output_bytes {
                return Err(DocumentError::resource("Bibles aggregate name byte limit"));
            }
            names.push(name);
            #[cfg(test)]
            {
                let hook = budget.hooks.entry.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
        }
        // libuv scandir uses strcmp (UTF-8 bytes) on Linux, before Node decodes
        // names. Iterative merge keeps cancellation and comparison count bounded.
        let order = ordered(&names, budget)?;
        let mut rows = Vec::with_capacity(names.len());
        let mut output_strings = 0_usize;
        for index in order {
            budget.check()?;
            let name = &names[index];
            let path = posix_join(root, name);
            if path.len() > budget.config.path_bytes {
                return Err(DocumentError::resource("Bibles joined path length limit"));
            }
            let display = if name.len() >= 4
                && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".fsb")
            {
                &name[..name.len() - 4]
            } else {
                name
            };
            output_strings = output_strings
                .checked_add(path.len() + display.len())
                .ok_or_else(|| DocumentError::resource("Bibles output string limit"))?;
            if output_strings > budget.config.output_bytes {
                return Err(DocumentError::resource("Bibles output string limit"));
            }
            rows.push(json!({"path":path,"name":display}));
        }
        Ok(reply(Value::Array(rows)))
    }
    fn ordered(names: &[String], budget: &mut Budget) -> Result<Vec<usize>> {
        let mut order: Vec<usize> = (0..names.len()).collect();
        let mut scratch = vec![0; names.len()];
        let mut width = 1;
        while width < order.len() {
            for start in (0..order.len()).step_by(width * 2) {
                let middle = (start + width).min(order.len());
                let end = (middle + width).min(order.len());
                let (mut left, mut right) = (start, middle);
                for slot in &mut scratch[start..end] {
                    budget.check()?;
                    if left < middle
                        && (right >= end
                            || names[order[left]].as_bytes() <= names[order[right]].as_bytes())
                    {
                        *slot = order[left];
                        left += 1;
                    } else {
                        *slot = order[right];
                        right += 1;
                    }
                }
            }
            std::mem::swap(&mut order, &mut scratch);
            width *= 2;
        }
        Ok(order)
    }
}
