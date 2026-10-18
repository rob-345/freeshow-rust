use crate::{paths, CancelToken, Error, JobControl, Limits};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocateRequest {
    pub file_path: String,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRoot {
    pub path: String,
    pub create_if_missing: bool,
}

impl MediaRoot {
    pub fn from_host(configured: Option<&str>, data_root: &str) -> Self {
        match configured.filter(|path| !path.is_empty()) {
            Some(path) => Self {
                path: path.into(),
                create_if_missing: false,
            },
            None => Self {
                path: paths::join(&[data_root, "Media"]),
                create_if_missing: true,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Located {
    pub path: String,
    pub has_changed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    pub cache_key: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailInvalidation {
    pub file_path: String,
    pub hash: String,
    pub sizes: [u16; 4],
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkStats {
    pub operations: usize,
    pub entries: usize,
    pub directories: usize,
    pub work_bytes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocateOutcome {
    pub reply: Option<Located>,
    pub replacement: Option<Replacement>,
    pub thumbnail_invalidations: Vec<ThumbnailInvalidation>,
    pub diagnostics: Vec<String>,
    pub work: WorkStats,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FolderOutcome {
    pub reply: String,
    pub diagnostics: Vec<String>,
}

pub(crate) struct Budget {
    limits: Arc<Limits>,
    cancel: CancelToken,
    closing: Arc<AtomicBool>,
    deadline: Instant,
    work: WorkStats,
    invalidations: Vec<ThumbnailInvalidation>,
    diagnostics: Vec<String>,
}

impl Budget {
    pub(crate) fn new(limits: Arc<Limits>, control: JobControl, closing: Arc<AtomicBool>) -> Self {
        Self {
            limits,
            cancel: control.cancel,
            closing,
            deadline: control.deadline,
            work: WorkStats::default(),
            invalidations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn error(&self, code: &str, message: impl Into<String>) -> Error {
        Error {
            code: code.into(),
            message: message.into(),
            thumbnail_invalidations: self.invalidations.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    pub(crate) fn check(&mut self) -> Result<(), Error> {
        if self.closing.load(Ordering::Acquire) {
            return Err(self.error("files_shutdown", "Filesystem service is shutting down"));
        }
        if self.cancel.is_cancelled() {
            return Err(self.error("files_cancelled", "Filesystem request was cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(self.error("files_deadline", "Filesystem request deadline elapsed"));
        }
        self.work.operations += 1;
        if self.work.operations > self.limits.max_operations {
            return Err(self.error("files_limit", "Filesystem operation limit exceeded"));
        }
        Ok(())
    }

    fn validate(&self, paths: impl Iterator<Item = impl AsRef<str>>) -> Result<(), Error> {
        let mut bytes = 0;
        for path in paths {
            let path = path.as_ref();
            if path.contains('\0') {
                return Err(self.error("files_invalid", "File paths cannot contain NUL"));
            }
            if path.len() > self.limits.max_path_bytes {
                return Err(self.error("files_limit", "File path exceeds the byte limit"));
            }
            bytes += path.len();
            if bytes > self.limits.max_input_bytes {
                return Err(self.error("files_limit", "Filesystem request exceeds the byte limit"));
            }
        }
        Ok(())
    }

    fn exists(&mut self, path: &str) -> Result<bool, Error> {
        self.check()?;
        self.validate(std::iter::once(path))?;
        let exists = !path.is_empty() && fs::metadata(path).is_ok();
        self.check()?;
        Ok(exists)
    }

    fn media_exists(&mut self, path: &str) -> Result<bool, Error> {
        let exists = self.exists(path)?;
        if !exists {
            self.invalidations.push(ThumbnailInvalidation {
                file_path: path.into(),
                hash: paths::file_path_hash(path),
                sizes: [900, 500, 250, 100],
            });
        }
        Ok(exists)
    }

    fn media_root(&mut self, root: &MediaRoot) -> Result<String, Error> {
        self.check()?;
        self.validate(std::iter::once(&root.path))?;
        if root.create_if_missing {
            // The source logs mkdir failure but still returns/searches this
            // path. Keep that reply and make the diagnostic inspectable.
            if let Err(error) = fs::create_dir_all(&root.path) {
                self.diagnostics.push(format!(
                    "Unable to create media folder {}: {error}",
                    root.path
                ));
            }
            self.check()?;
        }
        Ok(root.path.clone())
    }

    fn search(
        &mut self,
        folder: &str,
        parent: &str,
        file_name: &str,
        normalized_name: &str,
        level: usize,
    ) -> Result<Option<String>, Error> {
        self.check()?;
        if level > 8 {
            return Ok(None);
        }
        if !self.exists(folder)? {
            return Ok(None);
        }
        self.work.directories += 1;
        if self.work.directories > self.limits.max_directories {
            return Err(self.error("files_limit", "Directory search limit exceeded"));
        }
        let potential = paths::join(&[folder, parent, file_name]);
        if self.exists(&potential)? {
            return Ok(Some(potential));
        }
        self.check()?;
        let directory = match fs::read_dir(folder) {
            Ok(directory) => directory,
            Err(_) => return Ok(None),
        };
        let mut entries = Vec::new();
        for entry in directory {
            self.check()?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return Ok(None),
            };
            let name = entry.file_name().into_string().map_err(|_| {
                self.error(
                    "files_encoding",
                    "Non-Unicode directory entry names are not supported",
                )
            })?;
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => return Ok(None),
            };
            self.work.entries += 1;
            self.work.work_bytes += name.len() + folder.len();
            if self.work.entries > self.limits.max_entries
                || self.work.work_bytes > self.limits.max_work_bytes
            {
                return Err(self.error("files_limit", "Directory entry/work byte limit exceeded"));
            }
            entries.push((name, kind.is_dir()));
        }
        // libuv scandir uses bytewise name order on Unix. Windows read_dir and
        // Node both use the filesystem's native directory enumeration there.
        #[cfg(unix)]
        entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut folders = Vec::new();
        for (name, directory) in entries {
            self.check()?;
            let path = paths::join(&[folder, &name]);
            if directory {
                folders.push(path);
            } else if paths::normalized_file_name(&name) == normalized_name {
                return Ok(Some(path));
            }
        }
        // Bounded sequential child scans preserve controlled precedence and
        // unique-match behavior. Electron's duplicate nested matches race in
        // asyncPool(8); no universal duplicate-winner parity is claimed.
        for folder in folders {
            if let Some(found) =
                self.search(&folder, parent, file_name, normalized_name, level + 1)?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    pub(crate) fn locate(
        mut self,
        request: LocateRequest,
        root: MediaRoot,
    ) -> Result<LocateOutcome, Error> {
        self.check()?;
        if request.folders.len() > self.limits.max_folders {
            return Err(self.error("files_limit", "Search root count exceeded"));
        }
        self.validate(
            std::iter::once(&request.file_path)
                .chain(std::iter::once(&root.path))
                .chain(request.folders.iter()),
        )?;
        let original = paths::normalize_local_path(&request.file_path);
        self.validate(std::iter::once(&original))?;
        let mut replacement = None;
        let reply = if self.media_exists(&original)? {
            Some(Located {
                path: original,
                has_changed: false,
            })
        } else {
            let media_root = self.media_root(&root)?;
            let folder_id = paths::parent_folder_id(&original);
            let file_name = paths::basename(&original);
            let sync_path = paths::join(&[&media_root, &folder_id, &file_name]);
            if self.media_exists(&sync_path)? {
                Some(Located {
                    path: sync_path,
                    has_changed: true,
                })
            } else {
                let parent = paths::basename(&paths::dirname(&original));
                let normalized_name = paths::normalized_file_name(&file_name);
                let mut found = None;
                for folder in std::iter::once(media_root).chain(request.folders) {
                    let folder = paths::normalize_local_path(&folder);
                    if folder.is_empty() {
                        continue;
                    }
                    self.validate(std::iter::once(&folder))?;
                    if let Some(path) =
                        self.search(&folder, &parent, &file_name, &normalized_name, 1)?
                    {
                        found = Some(path);
                        break;
                    }
                }
                found.map(|path| {
                    replacement = Some(Replacement {
                        cache_key: format!("{folder_id}_{file_name}"),
                        path: path.clone(),
                    });
                    Located {
                        path,
                        has_changed: true,
                    }
                })
            }
        };
        self.check()?;
        Ok(LocateOutcome {
            reply,
            replacement,
            thumbnail_invalidations: self.invalidations,
            diagnostics: self.diagnostics,
            work: self.work,
        })
    }

    pub(crate) fn folder(mut self, root: MediaRoot) -> Result<FolderOutcome, Error> {
        let reply = self.media_root(&root)?;
        Ok(FolderOutcome {
            reply,
            diagnostics: self.diagnostics,
        })
    }
}
