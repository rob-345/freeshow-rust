use crate::{
    archive::{self, Entry},
    storage::{self, Stores, STORE_IDS},
    Error, Result,
};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::PathBuf};

struct Change {
    destination: PathBuf,
    content: Vec<u8>,
    store: Option<(String, Value)>,
}

pub struct Restored {
    pub stores: Vec<(String, Value)>,
    pub shows: bool,
    pub recognized: bool,
}

pub fn restore(stores: &Stores, paths: &[PathBuf]) -> Result<Restored> {
    let mut entries = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut files = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            files.sort_by_key(|entry| entry.file_name());
            for entry in files {
                if entry.file_type()?.is_file() {
                    entries.push(Entry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        content: fs::read(entry.path())?,
                    });
                }
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            entries.extend(archive::read_zip(path)?);
        } else {
            entries.push(Entry {
                name: path
                    .file_name()
                    .ok_or_else(|| Error::Invalid("restore path has no filename".into()))?
                    .to_string_lossy()
                    .into_owned(),
                content: fs::read(path)?,
            });
        }
    }
    restore_entries(stores, entries)
}

pub fn restore_entries(stores: &Stores, entries: Vec<Entry>) -> Result<Restored> {
    let mut changes = BTreeMap::new();
    let mut has_shows = false;
    for entry in entries {
        archive::validate_name(&entry.name)?;
        let base = entry.name.rsplit('/').next().unwrap_or(&entry.name);
        if base.contains("SHOWS_CONTENT") {
            let shows: Value = serde_json::from_slice(&entry.content)?;
            let shows = shows
                .as_object()
                .ok_or_else(|| Error::Invalid("legacy shows backup must be an object".into()))?;
            for (id, value) in shows {
                if value.is_null() || value == &Value::Bool(false) {
                    continue;
                }
                let name = value["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id);
                let destination = storage::leaf_path(&stores.data.join("Shows"), name, "show")?;
                changes.insert(
                    destination.clone(),
                    Change {
                        destination,
                        content: serde_json::to_vec(&serde_json::json!([id, value]))?,
                        store: None,
                    },
                );
            }
            has_shows = true;
        } else if entry.name.starts_with("SHOWS/") {
            let _: Value = serde_json::from_slice(&entry.content)?;
            let destination = stores.data.join("Shows").join(base);
            changes.insert(
                destination.clone(),
                Change {
                    destination,
                    content: entry.content,
                    store: None,
                },
            );
            has_shows = true;
        } else if let Some(id) = store_id(base) {
            let mut value: Value = serde_json::from_slice(&entry.content)?;
            let map = value
                .as_object_mut()
                .ok_or_else(|| Error::Invalid(format!("restored {id} must be an object")))?;
            if id == "SETTINGS" {
                map.remove("dataPath");
                map.remove("showsPath");
            }
            let destination = stores.path(&id)?;
            changes.insert(
                destination.clone(),
                Change {
                    destination,
                    content: serde_json::to_vec_pretty(&value)?,
                    store: Some((id, value)),
                },
            );
        }
    }
    let changes = changes.into_values().collect::<Vec<_>>();
    // Validate and stage every previous file before changing any destination.
    // Failed commits roll back; failed rollback retains a recovery journal.
    commit(stores, &changes, &mut |_| Ok(()))?;
    Ok(Restored {
        stores: changes
            .iter()
            .filter_map(|change| change.store.clone())
            .collect(),
        shows: has_shows,
        recognized: !changes.is_empty(),
    })
}

fn store_id(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".json")?;
    // Resolve SYNCED_SETTINGS before SETTINGS. The upstream substring branch
    // incorrectly sends synced settings to the local SETTINGS store.
    STORE_IDS
        .iter()
        .filter(|id| storage::portable(id) || **id == "SETTINGS")
        .find(|id| {
            stem.eq_ignore_ascii_case(id)
                || storage::definitions()[**id]["fileName"]
                    .as_str()
                    .is_some_and(|file| stem.eq_ignore_ascii_case(file))
        })
        .map(|id| (*id).to_string())
}

fn commit(
    stores: &Stores,
    changes: &[Change],
    before_write: &mut impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let stage = tempfile::Builder::new()
        .prefix("freeshow-restore-")
        .tempdir_in(&stores.data)?;
    let mut previous = Vec::new();
    let mut journal = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let old = match fs::symlink_metadata(&change.destination) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(Error::Invalid(format!(
                        "restore destination must be a regular file: {}",
                        change.destination.display()
                    )));
                }
                let _ = fs::OpenOptions::new()
                    .write(true)
                    .open(&change.destination)?;
                let old = stage.path().join(format!("previous-{index}"));
                fs::copy(&change.destination, &old)?;
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
        journal.push(serde_json::json!({"destination":change.destination,"previous":old.as_ref().and_then(|path| path.file_name()).map(|name| name.to_string_lossy())}));
        previous.push(old);
    }
    storage::atomic_write(
        &stage.path().join("journal.json"),
        &serde_json::json!({"version":1,"changes":journal}),
        true,
    )?;
    for (index, change) in changes.iter().enumerate() {
        if let Err(error) = before_write(index)
            .and_then(|_| storage::atomic_write_bytes(&change.destination, &change.content))
        {
            let mut rollback_errors = Vec::new();
            for restored in (0..=index).rev() {
                let destination = &changes[restored].destination;
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
                    "restore failed: {error}; rollback needs recovery from {}: {}",
                    recovery.display(),
                    rollback_errors.join("; ")
                )));
            }
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_io_failure_after_one_write_rolls_back_all_changes() {
        let temp = tempfile::tempdir().unwrap();
        let stores = Stores::new(temp.path().join("app"), temp.path().join("data")).unwrap();
        let first = stores.data.join("Shows/First.show");
        let second = stores.data.join("Shows/Second.show");
        fs::write(&first, b"original").unwrap();
        let changes = vec![
            Change {
                destination: first.clone(),
                content: b"changed".to_vec(),
                store: None,
            },
            Change {
                destination: second.clone(),
                content: b"new".to_vec(),
                store: None,
            },
        ];
        let result = commit(&stores, &changes, &mut |index| {
            if index == 1 {
                Err(std::io::Error::other("injected disk failure").into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(first).unwrap(), b"original");
        assert!(!second.exists());
        assert!(fs::read_dir(&stores.data).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("freeshow-restore-")));
    }

    #[cfg(unix)]
    #[test]
    fn failed_rollback_retains_the_original_files_and_journal_for_recovery() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let stores = Stores::new(temp.path().join("app"), temp.path().join("data")).unwrap();
        let first = stores.data.join("Shows/First.show");
        fs::write(&first, b"original").unwrap();
        let changes = vec![
            Change {
                destination: first.clone(),
                content: b"changed".to_vec(),
                store: None,
            },
            Change {
                destination: stores.data.join("Shows/Second.show"),
                content: b"second".to_vec(),
                store: None,
            },
        ];
        let result = commit(&stores, &changes, &mut |index| {
            if index == 1 {
                fs::set_permissions(&first, fs::Permissions::from_mode(0o400))?;
                return Err(std::io::Error::other("injected failure").into());
            }
            Ok(())
        });
        fs::set_permissions(&first, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("rollback needs recovery"));
        let stage = fs::read_dir(&stores.data)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("freeshow-restore-")
            })
            .unwrap();
        assert!(stage.join("journal.json").exists());
        assert_eq!(fs::read(stage.join("previous-0")).unwrap(), b"original");
    }
}
