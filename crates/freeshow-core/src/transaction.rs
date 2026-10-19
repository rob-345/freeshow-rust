//! Atomic file commits with rollback and retained recovery evidence.
use crate::{storage, Error, Result};
use std::{fs, path::Path};

pub(crate) fn commit(
    root: &Path,
    changes: &[(&Path, &[u8])],
    prefix: &str,
    before_write: &mut impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let stage = tempfile::Builder::new().prefix(prefix).tempdir_in(root)?;
    let mut previous = Vec::new();
    let mut journal = Vec::new();
    for (index, (destination, _)) in changes.iter().enumerate() {
        let old = match fs::symlink_metadata(destination) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(Error::Invalid(format!(
                        "transaction destination must be a regular file: {}",
                        destination.display()
                    )));
                }
                let _ = fs::OpenOptions::new().write(true).open(destination)?;
                let old = stage.path().join(format!("previous-{index}"));
                fs::copy(destination, &old)?;
                // Windows FlushFileBuffers requires a write-capable handle.
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&old)?
                    .sync_all()?;
                Some(old)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        journal.push(serde_json::json!({"destination":destination,"previous":old.as_ref().and_then(|path| path.file_name()).map(|name| name.to_string_lossy())}));
        previous.push(old);
    }
    storage::atomic_write(
        &stage.path().join("journal.json"),
        &serde_json::json!({"version":1,"changes":journal}),
        true,
    )?;
    for (index, (destination, content)) in changes.iter().enumerate() {
        if let Err(error) =
            before_write(index).and_then(|_| storage::atomic_write_bytes(destination, content))
        {
            let mut rollback_errors = Vec::new();
            for restored in (0..=index).rev() {
                let destination = changes[restored].0;
                let result = if let Some(old) = &previous[restored] {
                    fs::read(old)
                        .map_err(Error::from)
                        .and_then(|content| storage::atomic_write_bytes(destination, &content))
                } else {
                    match fs::remove_file(destination) {
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            Err(error.into())
                        }
                        _ => Ok(()),
                    }
                };
                if let Err(error) = result {
                    rollback_errors.push(error.to_string());
                }
            }
            if !rollback_errors.is_empty() {
                let recovery = stage.keep();
                return Err(Error::Invalid(format!(
                    "file transaction failed: {error}; rollback needs recovery from {}: {}",
                    recovery.display(),
                    rollback_errors.join("; ")
                )));
            }
            return Err(error);
        }
    }
    Ok(())
}
