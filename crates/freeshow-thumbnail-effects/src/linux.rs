use super::*;
use freeshow_thumbnails::{trusted_file_identity, CacheEffect, CacheGuard, SourceGuard};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::{CString, OsStr, OsString},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::Component,
    time::Instant,
};
const CHUNK: usize = 32 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    AfterPreflight,
    CopyChunk,
    AfterBackup(usize),
    BeforePromotion(usize),
    AfterPromotion(usize),
    PromotionRenamed(usize),
    BeforeRollback,
}
struct Budget<'a> {
    plan: &'a PreparedThumbnail,
    limits: EffectLimits,
    started: Instant,
    operations: usize,
    bytes: u64,
    chunks: usize,
    hook: &'a mut dyn FnMut(Point),
}
impl Budget<'_> {
    fn check(&mut self) -> Result<()> {
        self.plan
            .ensure_current()
            .map_err(|e| EffectError::new(e.code, e.message))?;
        self.operations += 1;
        if self.operations > self.limits.max_operations
            || self.started.elapsed() > self.limits.timeout
        {
            return Err(EffectError::new(
                "thumbnail_effect_limit",
                "Thumbnail effect operation/deadline limit",
            ));
        }
        Ok(())
    }
    fn bytes(&mut self, n: usize) -> Result<()> {
        self.check()?;
        self.bytes = self.bytes.saturating_add(n as u64);
        self.chunks += 1;
        if self.bytes > self.limits.max_bytes || self.chunks > self.limits.max_chunks {
            return Err(EffectError::new(
                "thumbnail_effect_limit",
                "Thumbnail aggregate byte/chunk limit",
            ));
        }
        Ok(())
    }
    fn guarded_bytes(&mut self, length: u64) -> Result<()> {
        self.check()?;
        self.bytes = self.bytes.saturating_add(length);
        if self.bytes > self.limits.max_bytes {
            return Err(EffectError::new(
                "thumbnail_effect_limit",
                "Guarded cache bytes exceed effect budget",
            ));
        }
        Ok(())
    }
    fn event(&mut self, point: Point) -> Result<()> {
        (self.hook)(point);
        self.check()
    }
}
fn identity(file: &File) -> Result<FileIdentity> {
    trusted_file_identity(file).map_err(|e| EffectError::new(e.code, e.message))
}
fn cstr(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| EffectError::new("thumbnail_effect_path", "Path contains NUL"))
}
fn regular(id: &FileIdentity) -> bool {
    id.mode & 0o170000 == 0o100000
}
fn same_inode(a: &FileIdentity, b: &FileIdentity) -> bool {
    a.device == b.device && a.inode == b.inode
}
// Own rename/link operations change ctime. Ownership always comes from a handle
// opened before promotion, then settled metadata for that same inode.
fn same_content_metadata(a: &FileIdentity, b: &FileIdentity) -> bool {
    same_inode(a, b)
        && a.length == b.length
        && a.mode == b.mode
        && a.mtime_seconds == b.mtime_seconds
        && a.mtime_nanos == b.mtime_nanos
}
fn openat(dir: &File, name: &OsStr, flags: i32, mode: u32) -> Result<File> {
    let name = cstr(name)?;
    // SAFETY: descriptor and C string are live; a successful new fd is owned below.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(EffectError::io("openat", std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn entry(dir: &File, name: &OsStr) -> Result<Option<(File, FileIdentity)>> {
    let name_c = cstr(name)?;
    // SAFETY: same ownership rule as openat; O_PATH cannot read arbitrary contents.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(EffectError::io("stat target", error))
        };
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let id = identity(&file)?;
    Ok(Some((file, id)))
}
fn rename(from: &File, name: &OsStr, to: &File, dest: &OsStr) -> Result<()> {
    let name = cstr(name)?;
    let dest = cstr(dest)?;
    // SAFETY: all descriptors/strings remain owned throughout this atomic syscall.
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.as_raw_fd(),
            name.as_ptr(),
            to.as_raw_fd(),
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    let raw = error.raw_os_error().unwrap_or(0);
    Err(EffectError::new(
        if [libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP].contains(&raw) {
            "unsupported_atomic_rename"
        } else if raw == libc::EEXIST {
            "thumbnail_effect_collision"
        } else {
            "thumbnail_effect_io"
        },
        format!("renameat2 NOREPLACE: {error}"),
    ))
}
fn unlink(dir: &File, name: &OsStr, directory: bool) -> Result<()> {
    let name = cstr(name)?;
    // SAFETY: unlink is confined to this anchored directory and one relative name.
    let result = unsafe {
        libc::unlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(EffectError::io("unlinkat", std::io::Error::last_os_error()))
    }
}
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().len() > 32768 {
        return Err(EffectError::new("thumbnail_effect_path", "Path byte limit"));
    }
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| EffectError::io("current directory", e))?
            .join(path)
    };
    let mut normalized = PathBuf::from("/");
    let mut depth = 0;
    for component in joined.components() {
        match component {
            Component::Normal(value) => {
                depth += 1;
                if depth > 64 {
                    return Err(EffectError::new(
                        "thumbnail_effect_path",
                        "Path depth limit",
                    ));
                }
                normalized.push(value)
            }
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::CurDir => {}
            _ => {
                return Err(EffectError::new(
                    "thumbnail_effect_path",
                    "Unverified path prefix",
                ))
            }
        }
    }
    if normalized.as_os_str().len() > 32768 {
        return Err(EffectError::new(
            "thumbnail_effect_path",
            "Resolved path byte limit",
        ));
    }
    Ok(normalized)
}
#[derive(Clone, PartialEq, Eq)]
struct DirId {
    device: u64,
    inode: u64,
    mode: u32,
}
fn dirid(file: &File) -> Result<DirId> {
    let id = identity(file)?;
    if id.mode & 0o170000 != 0o040000 {
        return Err(EffectError::new(
            "thumbnail_effect_path",
            "Expected a directory",
        ));
    }
    Ok(DirId {
        device: id.device,
        inode: id.inode,
        mode: id.mode,
    })
}
struct Directory {
    path: PathBuf,
    file: File,
    chain: Vec<DirId>,
}
impl Directory {
    fn open(path: &Path, budget: &mut Budget<'_>) -> Result<Self> {
        let path = absolute(path)?;
        let name = cstr(OsStr::new("/"))?;
        // SAFETY: absolute root is a static C string; own the returned descriptor.
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(EffectError::io(
                "open root",
                std::io::Error::last_os_error(),
            ));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut chain = vec![dirid(&file)?];
        for component in path.components() {
            if let Component::Normal(name) = component {
                budget.check()?;
                file = openat(
                    &file,
                    name,
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    0,
                )?;
                chain.push(dirid(&file)?);
            }
        }
        Ok(Self { path, file, chain })
    }
    fn check(&self, budget: &mut Budget<'_>) -> Result<()> {
        budget.check()?;
        let now = Self::open(&self.path, budget)?;
        if now.chain != self.chain || dirid(&self.file)? != *self.chain.last().unwrap() {
            return Err(EffectError::new(
                "thumbnail_effect_parent_changed",
                "Anchored directory path/parent changed",
            ));
        }
        Ok(())
    }
}
fn checked_file(
    path: &Path,
    expected: &FileIdentity,
    read: bool,
    budget: &mut Budget<'_>,
) -> Result<File> {
    let absolute = absolute(path)?;
    let parent = Directory::open(
        absolute
            .parent()
            .ok_or_else(|| EffectError::new("thumbnail_effect_path", "File has no parent"))?,
        budget,
    )?;
    let name = absolute
        .file_name()
        .ok_or_else(|| EffectError::new("thumbnail_effect_path", "File has no name"))?;
    let file = openat(
        &parent.file,
        name,
        libc::O_NOFOLLOW | libc::O_NONBLOCK | if read { libc::O_RDONLY } else { libc::O_PATH },
        0,
    )?;
    let current = identity(&file)?;
    if !regular(&current) || &current != expected {
        return Err(EffectError::new(
            "thumbnail_effect_guard_changed",
            "Source/staged identity differs",
        ));
    }
    parent.check(budget)?;
    if entry(&parent.file, name)?.as_ref().map(|(_, id)| id) != Some(expected) {
        return Err(EffectError::new(
            "thumbnail_effect_guard_changed",
            "Source/staged path changed during open",
        ));
    }
    Ok(file)
}
fn check_source(source: &SourceGuard, budget: &mut Budget<'_>) -> Result<()> {
    let _ = checked_file(Path::new(&source.path), &source.expected, false, budget)?;
    Ok(())
}
fn hash(file: &mut File, budget: &mut Budget<'_>) -> Result<String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| EffectError::io("seek PNG", e))?;
    let mut sha = Sha256::new();
    let mut block = [0; CHUNK];
    loop {
        budget.check()?;
        let read = file
            .read(&mut block)
            .map_err(|e| EffectError::io("read PNG", e))?;
        if read == 0 {
            break;
        }
        budget.bytes(read)?;
        sha.update(&block[..read]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
struct Ready {
    destination: OsString,
    expected: CacheGuard,
    staged: Option<File>,
    staged_guard: Option<(PathBuf, FileIdentity)>,
    sha: Option<String>,
    length: u64,
}
fn preflight(
    root: &Directory,
    plan: &PreparedThumbnail,
    budget: &mut Budget<'_>,
) -> Result<Vec<Ready>> {
    let mut seen = HashSet::new();
    let mut ready = vec![];
    for effect in &plan.effects {
        budget.check()?;
        let (destination, expected, stage) = match effect {
            CacheEffect::Invalidate {
                destination,
                expected,
            } => (destination, CacheGuard::Existing(expected.clone()), None),
            CacheEffect::Install {
                destination,
                expected_destination,
                staged_path,
                expected_staged,
                sha256,
                source,
                ..
            } => {
                if plan.outcome != ThumbnailOutcome::GeneratedPng
                    || plan.source_guard() != Some(source.as_ref())
                {
                    return Err(EffectError::new(
                        "thumbnail_effect_invalid_plan",
                        "Install is not a completed source-guarded PNG",
                    ));
                }
                check_source(source, budget)?;
                let private = plan.staging_directory().ok_or_else(|| {
                    EffectError::new("thumbnail_effect_path", "Missing private stage")
                })?;
                if staged_path.parent().map(Path::as_os_str) != Some(private.as_os_str()) {
                    return Err(EffectError::new(
                        "thumbnail_effect_path",
                        "Staged PNG is outside captured private directory",
                    ));
                }
                let mut file = checked_file(staged_path, expected_staged, true, budget)?;
                let digest = hash(&mut file, budget)?;
                if digest != *sha256 || identity(&file)? != *expected_staged {
                    return Err(EffectError::new(
                        "thumbnail_effect_hash_changed",
                        "Staged PNG hash/identity changed",
                    ));
                }
                (
                    destination,
                    expected_destination.clone(),
                    Some((
                        file,
                        digest,
                        expected_staged.length,
                        staged_path.clone(),
                        expected_staged.clone(),
                    )),
                )
            }
        };
        let path = destination.as_path();
        let components: Vec<_> = path.components().collect();
        if components.len() != 1
            || !matches!(components[0], Component::Normal(_))
            || path.as_os_str().len() > 128
            || !seen.insert(path.to_owned())
        {
            return Err(EffectError::new(
                "thumbnail_effect_path",
                "Invalid/duplicate sealed cache filename",
            ));
        }
        let name = path.as_os_str().to_owned();
        let actual = entry(&root.file, &name)?;
        match (&expected, &actual) {
            (CacheGuard::Absent, None) => {}
            (CacheGuard::Existing(expected), Some((_, id))) if expected == id && regular(id) => {
                budget.guarded_bytes(id.length)?;
            }
            _ => {
                return Err(EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Cache target guard differs",
                ))
            }
        };
        let (staged, sha, length, staged_guard) = stage
            .map_or((None, None, 0, None), |(file, sha, len, path, id)| {
                (Some(file), Some(sha), len, Some((path, id)))
            });
        ready.push(Ready {
            destination: name,
            expected,
            staged,
            staged_guard,
            sha,
            length,
        });
    }
    root.check(budget)?;
    Ok(ready)
}
struct Held {
    file: File,
    settled: FileIdentity,
}
struct Journal {
    destination: OsString,
    backup: Option<(OsString, Held)>,
    installed: Option<Held>,
    temporary: Option<(OsString, Held)>,
}
struct Transaction {
    file: File,
    name: OsString,
    path: PathBuf,
}
impl Transaction {
    fn create(root: &Directory, budget: &mut Budget<'_>) -> Result<Self> {
        for _ in 0..8 {
            budget.check()?;
            let mut random = [0; 12];
            getrandom::fill(&mut random)
                .map_err(|e| EffectError::new("thumbnail_effect_io", e.to_string()))?;
            let name = OsString::from(format!(
                ".freeshow-thumbnail-effects-{}",
                random
                    .iter()
                    .map(|v| format!("{v:02x}"))
                    .collect::<String>()
            ));
            let c = cstr(&name)?;
            // SAFETY: name is one generated relative component, parent fd stays live.
            let result = unsafe { libc::mkdirat(root.file.as_raw_fd(), c.as_ptr(), 0o700) };
            if result < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(EffectError::io("mkdir private transaction", e));
            }
            let file = openat(
                &root.file,
                &name,
                libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                0,
            )?;
            return Ok(Self {
                file,
                path: root.path.join(&name),
                name,
            });
        }
        Err(EffectError::new(
            "thumbnail_effect_collision",
            "Private transaction name collisions",
        ))
    }
}
fn settle(file: File, before: &FileIdentity, dir: &File, name: &OsStr) -> Result<Held> {
    let settled = identity(&file)?;
    if !same_content_metadata(before, &settled)
        || entry(dir, name)?.as_ref().map(|(_, id)| id) != Some(&settled)
    {
        return Err(EffectError::new(
            "thumbnail_effect_guard_changed",
            "Promoted object differs from its pre-existing anchor",
        ));
    }
    Ok(Held { file, settled })
}
fn checked_entry(dir: &File, name: &OsStr, held: &Held) -> Result<()> {
    if identity(&held.file)? != held.settled
        || entry(dir, name)?.as_ref().map(|(_, id)| id) != Some(&held.settled)
    {
        return Err(EffectError::new(
            "thumbnail_effect_guard_changed",
            "Owned entry changed externally",
        ));
    }
    Ok(())
}
fn unchanged_guard(root: &Directory, item: &Ready) -> Result<()> {
    let actual = entry(&root.file, &item.destination)?;
    match (&item.expected, actual) {
        (CacheGuard::Absent, None) => Ok(()),
        (CacheGuard::Existing(expected), Some((_, id))) if *expected == id => Ok(()),
        _ => Err(EffectError::new(
            "thumbnail_effect_guard_changed",
            "Target changed after preflight",
        )),
    }
}
fn perform(
    root: &Directory,
    txn: &Transaction,
    ready: &mut [Ready],
    journal: &mut Vec<Journal>,
    budget: &mut Budget<'_>,
) -> Result<Vec<AppliedEffect>> {
    let mut applied = vec![];
    for (index, item) in ready.iter_mut().enumerate() {
        if let Some((path, expected)) = &item.staged_guard {
            let _ = checked_file(path, expected, true, budget)?;
            if identity(item.staged.as_ref().unwrap())? != *expected {
                return Err(EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Staged anchor changed while copying",
                ));
            }
        }
        root.check(budget)?;
        if let Some(source) = budget.plan.source_guard() {
            check_source(source, budget)?;
        }
        unchanged_guard(root, item)?;
        journal.push(Journal {
            destination: item.destination.clone(),
            backup: None,
            installed: None,
            temporary: None,
        });
        let record = journal.last_mut().unwrap();
        if let Some(staged) = item.staged.as_mut() {
            let name = OsString::from(format!("new-{index}"));
            let mut file = openat(
                &txn.file,
                &name,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
                0o600,
            )?;
            record.temporary = Some((
                name.clone(),
                Held {
                    settled: identity(&file)?,
                    file: file
                        .try_clone()
                        .map_err(|e| EffectError::io("clone temp", e))?,
                },
            ));
            staged
                .seek(SeekFrom::Start(0))
                .map_err(|e| EffectError::io("seek staged", e))?;
            let mut block = [0; CHUNK];
            let mut digest = Sha256::new();
            let mut copied = 0;
            loop {
                budget.check()?;
                let n = staged
                    .read(&mut block)
                    .map_err(|e| EffectError::io("copy staged", e))?;
                if n == 0 {
                    break;
                }
                budget.bytes(n)?;
                copied += n as u64;
                if copied > item.length {
                    return Err(EffectError::new(
                        "thumbnail_effect_guard_changed",
                        "Staged PNG grew during copy",
                    ));
                }
                file.write_all(&block[..n])
                    .map_err(|e| EffectError::io("write cache temp", e))?;
                digest.update(&block[..n]);
                record.temporary.as_mut().unwrap().1.settled = identity(&file)?;
                budget.event(Point::CopyChunk)?;
            }
            if copied != item.length || Some(format!("{:x}", digest.finalize())) != item.sha {
                return Err(EffectError::new(
                    "thumbnail_effect_hash_changed",
                    "Copied PNG hash differs",
                ));
            }
            file.sync_all()
                .map_err(|e| EffectError::io("sync cache temp", e))?;
            record.temporary.as_mut().unwrap().1.settled = identity(&file)?;
        }
        if let Some((path, expected)) = &item.staged_guard {
            let _ = checked_file(path, expected, true, budget)?;
            if identity(item.staged.as_ref().unwrap())? != *expected {
                return Err(EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Staged anchor changed while copying",
                ));
            }
        }
        root.check(budget)?;
        if let Some(source) = budget.plan.source_guard() {
            check_source(source, budget)?;
        }
        unchanged_guard(root, item)?;
        if let CacheGuard::Existing(expected) = &item.expected {
            let (file, id) = entry(&root.file, &item.destination)?.ok_or_else(|| {
                EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Existing cache target disappeared",
                )
            })?;
            if &id != expected {
                return Err(EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Cache target changed before quarantine",
                ));
            }
            let name = OsString::from(format!("backup-{index}"));
            rename(&root.file, &item.destination, &txn.file, &name)?;
            // Keep a journal entry immediately; an identity race leaves a recoverable
            // quarantine and is never silently adopted as an owned cache file.
            let held = Held { settled: id, file };
            record.backup = Some((name.clone(), held));
            record.backup.as_mut().unwrap().1.settled =
                identity(&record.backup.as_ref().unwrap().1.file)?;
            let h = &record.backup.as_ref().unwrap().1;
            if !same_content_metadata(expected, &h.settled) {
                return Err(EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Cache anchor changed across quarantine",
                ));
            }
            checked_entry(&txn.file, &name, h)?;
            budget.event(Point::AfterBackup(index))?;
        }
        if record.temporary.is_some() {
            budget.event(Point::BeforePromotion(index))?;
            root.check(budget)?;
            if let Some(source) = budget.plan.source_guard() {
                check_source(source, budget)?;
            }
            let (name, held) = record.temporary.as_ref().unwrap();
            checked_entry(&txn.file, name, held)?;
            rename(&txn.file, name, &root.file, &item.destination)?;
            let (_, held) = record.temporary.take().unwrap();
            let before = held.settled.clone();
            record.installed = Some(held);
            (budget.hook)(Point::PromotionRenamed(index));
            let installed = record.installed.as_mut().unwrap();
            let settled = identity(&installed.file)?;
            if !same_content_metadata(&before, &settled) {
                return Err(EffectError::new(
                    "thumbnail_effect_guard_changed",
                    "Promoted inode content/metadata changed",
                ));
            }
            installed.settled = settled;
            checked_entry(&root.file, &item.destination, installed)?;
            budget.event(Point::AfterPromotion(index))?;
            let installed = record.installed.as_mut().unwrap();
            checked_entry(&root.file, &item.destination, installed)?;
            if hash(&mut installed.file, budget)? != *item.sha.as_ref().unwrap() {
                return Err(EffectError::new(
                    "thumbnail_effect_hash_changed",
                    "Installed PNG changed",
                ));
            }
            checked_entry(&root.file, &item.destination, installed)?;
        }
        root.check(budget)?;
        if let Some(source) = budget.plan.source_guard() {
            check_source(source, budget)?;
        }
        budget.check()?;
        applied.push(AppliedEffect {
            destination: PathBuf::from(&item.destination),
            installed: item.staged.is_some(),
            bytes: item.length,
            sha256: item.sha.clone(),
        });
    }
    Ok(applied)
}
// Cleanup intentionally ignores the exhausted/canceled work budget. Its object
// count is bounded by four effects; filling diagnostics never stops later work.
fn cleanup_temp(txn: &Transaction, record: &mut Journal, error: &mut EffectError) {
    if let Some((name, held)) = record.temporary.take() {
        match checked_entry(&txn.file, &name, &held).and_then(|_| unlink(&txn.file, &name, false)) {
            Ok(()) => {}
            Err(e) => {
                error.rollback_incomplete = true;
                error.detail(format!(
                    "Retained cache temp {}: {e}",
                    txn.path.join(name).display()
                ));
            }
        }
    }
}
fn rollback(root: &Directory, txn: &Transaction, journal: &mut [Journal], error: &mut EffectError) {
    for (index, record) in journal.iter_mut().enumerate().rev() {
        if let Some(held) = record.installed.take() {
            let removal = OsString::from(format!("rollback-new-{index}"));
            let result = checked_entry(&root.file, &record.destination, &held)
                .and_then(|_| rename(&root.file, &record.destination, &txn.file, &removal));
            match result {
                Ok(()) => {
                    match settle(held.file, &held.settled, &txn.file, &removal)
                        .and_then(|held| checked_entry(&txn.file, &removal, &held))
                        .and_then(|_| unlink(&txn.file, &removal, false))
                    {
                        Ok(()) => {}
                        Err(e) => {
                            error.rollback_incomplete = true;
                            error.detail(format!(
                                "Retained changed promoted object {}: {e}",
                                txn.path.join(&removal).display()
                            ));
                        }
                    }
                }
                Err(e) => {
                    error.rollback_incomplete = true;
                    error.detail(format!(
                        "Preserved changed/colliding cache target {:?}: {e}",
                        record.destination
                    ));
                }
            }
        }
        if let Some((name, held)) = record.backup.take() {
            let result = checked_entry(&txn.file, &name, &held)
                .and_then(|_| rename(&txn.file, &name, &root.file, &record.destination));
            if let Err(e) = result {
                error.rollback_incomplete = true;
                error.detail(format!(
                    "Retained original cache backup {}: {e}",
                    txn.path.join(&name).display()
                ));
            } else if let Err(e) = settle(held.file, &held.settled, &root.file, &record.destination)
            {
                error.rollback_incomplete = true;
                error.detail(format!(
                    "Restored cache anchor changed {:?}: {e}",
                    record.destination
                ));
            }
        }
        cleanup_temp(txn, record, error);
    }
}
fn remove_transaction(root: &Directory, txn: &Transaction) -> Result<()> {
    let expected = dirid(&txn.file)?;
    let current = entry(&root.file, &txn.name)?.ok_or_else(|| {
        EffectError::new(
            "thumbnail_effect_parent_changed",
            "Transaction directory name disappeared",
        )
    })?;
    if dirid(&current.0)? != expected {
        return Err(EffectError::new(
            "thumbnail_effect_parent_changed",
            "Transaction directory was replaced",
        ));
    }
    unlink(&root.file, &txn.name, true)
}
fn finish(root: &Directory, txn: &Transaction, journal: &mut [Journal], warning: &mut EffectError) {
    for record in journal {
        if let Some((name, held)) = record.backup.take() {
            if let Err(e) =
                checked_entry(&txn.file, &name, &held).and_then(|_| unlink(&txn.file, &name, false))
            {
                warning.detail(format!(
                    "Retained changed original cache backup {}: {e}",
                    txn.path.join(&name).display()
                ));
            }
        }
        cleanup_temp(txn, record, warning);
    }
    if let Err(e) = remove_transaction(root, txn) {
        warning.detail(format!(
            "Retained transaction directory {}: {e}",
            txn.path.display()
        ));
    }
}
pub(crate) fn apply(
    root: &Path,
    plan: &PreparedThumbnail,
    limits: EffectLimits,
    hook: &mut dyn FnMut(Point),
) -> Result<Receipt> {
    let mut budget = Budget {
        plan,
        limits,
        started: Instant::now(),
        operations: 0,
        bytes: 0,
        chunks: 0,
        hook,
    };
    budget.check()?;
    if let Some(source) = plan.source_guard() {
        check_source(source, &mut budget)?;
    }
    let mut receipt = Receipt {
        root: root.into(),
        identity: plan.identity(),
        effects: vec![],
        invalidation: None,
        acknowledged: false,
        diagnostics: vec![],
        diagnostics_omitted: 0,
    };
    if plan.effects.is_empty() {
        return Ok(receipt);
    }
    let root = Directory::open(root, &mut budget)?;
    let mut ready = preflight(&root, plan, &mut budget)?;
    budget.event(Point::AfterPreflight)?;
    root.check(&mut budget)?;
    let txn = Transaction::create(&root, &mut budget)?;
    let mut journal = vec![];
    let result = perform(&root, &txn, &mut ready, &mut journal, &mut budget).and_then(|effects| {
        root.check(&mut budget)?;
        if let Some(source) = plan.source_guard() {
            check_source(source, &mut budget)?;
        }
        budget.check()?;
        // Commit point: all installed anchors/guards and context cancellation
        // passed. Irreversible backup cleanup follows; later cancellation is a
        // host publication decision, not a claim of batch rollback atomicity.
        Ok(effects)
    });
    match result {
        Ok(effects) => {
            let mut warning = EffectError::new(
                "thumbnail_effect_cleanup",
                "Post-commit cleanup diagnostics",
            );
            finish(&root, &txn, &mut journal, &mut warning);
            receipt.effects = effects;
            receipt.diagnostics = warning.diagnostics;
            receipt.diagnostics_omitted = warning.diagnostics_omitted;
            if plan.outcome == ThumbnailOutcome::CapturePending && plan.effects.len() == 1 {
                if let CacheEffect::Invalidate {
                    destination,
                    expected,
                } = &plan.effects[0]
                {
                    receipt.invalidation = Some((destination.clone(), expected.clone()));
                }
            }
            Ok(receipt)
        }
        Err(mut error) => {
            (budget.hook)(Point::BeforeRollback);
            rollback(&root, &txn, &mut journal, &mut error);
            if let Err(e) = remove_transaction(&root, &txn) {
                error.rollback_incomplete = true;
                error.detail(format!(
                    "Retained rollback directory {}: {e}",
                    txn.path.display()
                ));
            }
            Err(error)
        }
    }
}
