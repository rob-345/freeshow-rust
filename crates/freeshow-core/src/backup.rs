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

pub fn create(stores: &Stores, automatic: bool) -> Result<PathBuf> {
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
    archive::write_zip(&entries, &path)?;
    if automatic {
        if let Some(cutoff) = retention_cutoff(now) {
            for old in retention_paths(&list(&stores.data)?, cutoff, false) {
                remove(&stores.data, &old)?;
            }
        }
    }
    Ok(path)
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
