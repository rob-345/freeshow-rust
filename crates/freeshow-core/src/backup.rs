use crate::{
    archive::{self, Source},
    storage::{self, Stores, STORE_IDS},
    Error, Result,
};
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub path: PathBuf,
    pub name: String,
    pub date: f64,
    pub size: u64,
}

/// Private copies keep compression independent of later source-file changes.
/// Preparation must run in the ordered library phase. External writers are not
/// locked: this is a stable completed copy, not a filesystem-wide atomic view.
pub struct Snapshot {
    _directory: tempfile::TempDir,
    entries: Vec<Source>,
    path: PathBuf,
    data: PathBuf,
    automatic: bool,
    now: DateTime<Local>,
}

impl Snapshot {
    pub fn finish(self) -> Result<PathBuf> {
        archive::write_zip(&self.entries, &self.path)?;
        if self.automatic {
            if let Some(cutoff) = retention_cutoff(self.now) {
                for old in retention_paths(&list(&self.data)?, cutoff, false) {
                    remove(&self.data, &old)?;
                }
            }
        }
        Ok(self.path.clone())
    }
}

pub fn create(stores: &Stores, automatic: bool) -> Result<PathBuf> {
    prepare(stores, automatic)?.finish()
}

pub fn prepare(stores: &Stores, automatic: bool) -> Result<Snapshot> {
    let folder = stores.data.join("Backups");
    fs::create_dir_all(&folder)?;
    let now = Local::now();
    // Upstream uses UTC for the date and local time for hours/minutes.
    let prefix = format!(
        "{}_{}",
        now.with_timezone(&Utc).format("%Y-%m-%d"),
        now.format("%H-%M")
    );
    let suffix = if automatic { "_auto" } else { "" };
    let mut path = folder.join(format!("{prefix}{suffix}.zip"));
    let mut collision = 1;
    // Retain earlier backups when two saves occur in the same minute.
    while path.exists() {
        path = folder.join(format!("{prefix}-{collision}{suffix}.zip"));
        collision += 1;
    }
    let mut entries = Vec::new();
    for id in STORE_IDS {
        if storage::portable(id) {
            entries.push(Source {
                name: format!("{id}.json"),
                path: stores.path(id)?,
            });
        }
    }
    entries.push(Source {
        name: "SETTINGS.json".into(),
        path: stores.path("SETTINGS")?,
    });
    // Despite the old upstream comment, both manual and automatic ZIPs include
    // shows. Preserve the actual implementation's behavior.
    for entry in fs::read_dir(stores.data.join("Shows"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.to_lowercase().ends_with(".show") {
            entries.push(Source {
                name: format!("SHOWS/{name}"),
                path: entry.path(),
            });
        }
    }
    let directory = tempfile::Builder::new()
        .prefix("freeshow-backup-")
        .tempdir()?;
    let mut copied = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        archive::validate_name(&entry.name)?;
        match fs::metadata(&entry.path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let destination = directory.path().join(index.to_string());
        // Copy bytes rather than hardlinking. In-place external edits must not
        // change an already prepared backup. No file contents accumulate in RAM.
        fs::copy(entry.path, &destination)?;
        #[cfg(windows)]
        {
            // Windows copies the read-only attribute too, which would prevent
            // TempDir from removing this private copy after compression.
            let mut permissions = fs::metadata(&destination)?.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&destination, permissions)?;
        }
        copied.push(Source {
            name: entry.name,
            path: destination,
        });
    }
    Ok(Snapshot {
        _directory: directory,
        entries: copied,
        path,
        data: stores.data.clone(),
        automatic,
        now,
    })
}

pub fn list(data: &Path) -> Result<Vec<Backup>> {
    let folder = data.join("Backups");
    fs::create_dir_all(&folder)?;
    let mut result = Vec::new();
    let mut entries = fs::read_dir(folder)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let metadata = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let size = if metadata.is_dir() {
            fs::read_dir(entry.path())?
                .filter_map(|item| {
                    item.ok()
                        .and_then(|entry| entry.metadata().ok())
                        .map(|metadata| metadata.len())
                })
                .sum()
        } else {
            metadata.len()
        };
        if name.ends_with(".zip") || (metadata.is_dir() && size > 0) {
            result.push(Backup {
                path: entry.path(),
                name: if name.ends_with(".zip") {
                    name.replacen(".zip", "", 1)
                } else {
                    name
                },
                date: change_time_ms(&metadata),
                size,
            });
        }
    }
    Ok(result)
}

pub fn remove(data: &Path, selected: &Path) -> Result<()> {
    let folder = data.join("Backups");
    fs::create_dir_all(&folder)?;
    let full = if selected.is_absolute() {
        selected.to_path_buf()
    } else {
        folder.join(selected)
    };
    let parent = full
        .parent()
        .ok_or_else(|| Error::Invalid("backup has no parent".into()))?;
    if fs::canonicalize(parent)? != fs::canonicalize(&folder)? {
        return Err(Error::Invalid(
            "backup must belong to this data directory".into(),
        ));
    }
    let metadata = match fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(full)?;
    } else {
        fs::remove_file(full)?;
    }
    Ok(())
}

pub fn retention_paths(backups: &[Backup], cutoff: i64, ignore_floor: bool) -> Vec<PathBuf> {
    let mut automatic = backups
        .iter()
        .filter(|backup| backup.name.ends_with("_auto"))
        .collect::<Vec<_>>();
    automatic.sort_by(|a, b| a.date.total_cmp(&b.date));
    let count = automatic.len();
    if count <= 10 {
        return Vec::new();
    }
    const FLOOR: f64 = 1773964800000.0; // 2026-03-20T00:00:00Z
    automatic
        .into_iter()
        .enumerate()
        .filter(|(index, backup)| {
            *index >= 3
                && *index < count - 10
                && (ignore_floor || backup.date >= FLOOR)
                && backup.date < cutoff as f64
        })
        .map(|(_, backup)| backup.path.clone())
        .collect()
}

pub fn retention_cutoff(now: DateTime<Local>) -> Option<i64> {
    let months = now.year() * 12 + now.month0() as i32 - 5;
    let first =
        NaiveDate::from_ymd_opt(months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1)?;
    // Date.setMonth normalizes overflowing days instead of clamping them.
    let date = first.checked_add_days(Days::new(now.day() as u64 - 1))?;
    // Retain backups if the local cutoff falls into an unresolvable DST gap.
    Local
        .from_local_datetime(&date.and_time(now.time()))
        .earliest()
        .map(|date| date.timestamp_millis())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(unix)]
fn change_time_ms(metadata: &fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ctime() as f64 * 1000.0 + metadata.ctime_nsec() as f64 / 1_000_000.0
}

#[cfg(not(unix))]
fn change_time_ms(metadata: &fs::Metadata) -> f64 {
    metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepared_backup_keeps_copied_bytes_after_in_place_edits_renames_and_deletes() {
        let temp = tempfile::tempdir().unwrap();
        let mut stores = Stores::new(temp.path().join("app"), temp.path().join("data")).unwrap();
        stores.set("PROJECTS", json!({"original":true})).unwrap();
        let shows = stores.data.join("Shows");
        fs::write(shows.join("Original.show"), b"original show").unwrap();
        fs::write(shows.join("Deleted.show"), b"deleted later").unwrap();
        let snapshot = prepare(&stores, false).unwrap();
        let staging = snapshot._directory.path().to_path_buf();
        // fs::write truncates in place: a hardlink would corrupt the snapshot.
        fs::write(stores.path("PROJECTS").unwrap(), b"{\"changed\":true}").unwrap();
        fs::write(shows.join("Original.show"), b"changed show").unwrap();
        fs::rename(shows.join("Original.show"), shows.join("Renamed.show")).unwrap();
        fs::remove_file(shows.join("Deleted.show")).unwrap();
        fs::write(shows.join("Added.show"), b"new show").unwrap();
        let archive = snapshot.finish().unwrap();
        assert!(!staging.exists());
        let entries = archive::read_zip(&archive)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name, entry.content))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&entries["PROJECTS.json"]).unwrap(),
            json!({"original":true})
        );
        assert_eq!(entries["SHOWS/Original.show"], b"original show");
        assert_eq!(entries["SHOWS/Deleted.show"], b"deleted later");
        assert!(!entries.contains_key("SHOWS/Renamed.show"));
        assert!(!entries.contains_key("SHOWS/Added.show"));
        let abandoned = prepare(&stores, false).unwrap();
        let staging = abandoned._directory.path().to_path_buf();
        drop(abandoned);
        assert!(!staging.exists());
    }

    #[test]
    fn failed_compression_cleans_snapshot_without_publishing_an_archive() {
        let temp = tempfile::tempdir().unwrap();
        let stores = Stores::new(temp.path().join("app"), temp.path().join("data")).unwrap();
        let snapshot = prepare(&stores, false).unwrap();
        let staging = snapshot._directory.path().to_path_buf();
        let output = snapshot.path.clone();
        // Removing the destination directory represents a disconnected volume.
        fs::remove_dir(stores.data.join("Backups")).unwrap();
        assert!(snapshot.finish().is_err());
        assert!(!output.exists());
        assert!(!staging.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_read_only_sources_do_not_leave_read_only_temporary_copies() {
        let temp = tempfile::tempdir().unwrap();
        let stores = Stores::new(temp.path().join("app"), temp.path().join("data")).unwrap();
        let show = stores.data.join("Shows/Read only.show");
        fs::write(&show, b"read only source").unwrap();
        let mut permissions = fs::metadata(&show).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&show, permissions).unwrap();
        let prepared = prepare(&stores, false);
        let mut permissions = fs::metadata(&show).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(show, permissions).unwrap();
        let snapshot = prepared.unwrap();
        let staging = snapshot._directory.path().to_path_buf();
        snapshot.finish().unwrap();
        assert!(!staging.exists());
    }
}
