use super::*;
use freeshow_media_copy::{
    parent_folder_id, AncestorGuard, CopyEffect, CopyObservation, DeferredReply, DirectoryGuard,
    DirectoryIdentity, FileIdentity, Install, PathGuard, SourceGuard,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{CString, OsStr, OsString},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Component,
    sync::Arc,
    time::Instant,
};
const CHUNK: usize = 32768;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    AfterPreflight,
    CopyChunk,
    BeforePromotion(usize),
    PromotionRenamed(usize),
    AfterPromotion(usize),
    BeforeRollback,
}
struct Budget<'a> {
    plan: &'a PreparedMediaCopy,
    lease: &'a CommitLease,
    limits: EffectLimits,
    cancel: &'a CancellationToken,
    started: Instant,
    operations: usize,
    read: u64,
    written: u64,
    chunks: usize,
    hook: &'a mut dyn FnMut(Point),
}
impl Budget<'_> {
    fn check(&mut self) -> Result<()> {
        self.plan
            .validate_commit_lease(self.lease)
            .map_err(|e| EffectError::new(e.code, e.message))?;
        if self.cancel.is_cancelled() {
            return Err(EffectError::new(
                "cancelled",
                "Ordered media copy owner cancelled",
            ));
        }
        if self.started.elapsed() >= self.limits.timeout {
            return Err(EffectError::new(
                "copy_effect_timeout",
                "Media copy effect deadline exceeded",
            ));
        }
        self.operations += 1;
        if self.operations > self.limits.max_operations {
            return Err(EffectError::new(
                "copy_effect_limit",
                "Media copy operation limit",
            ));
        }
        Ok(())
    }
    fn read(&mut self, bytes: usize) -> Result<()> {
        self.read += bytes as u64;
        self.chunks += 1;
        if self.read > self.limits.max_read_bytes || self.chunks > self.limits.max_chunks {
            return Err(EffectError::new(
                "copy_effect_limit",
                "Media copy read/chunk limit",
            ));
        }
        self.check()
    }
    fn write(&mut self, bytes: usize) -> Result<()> {
        self.written += bytes as u64;
        if self.written > self.limits.max_install_bytes {
            return Err(EffectError::new(
                "copy_effect_limit",
                "Media copy install byte limit",
            ));
        }
        self.check()
    }
    fn event(&mut self, point: Point) -> Result<()> {
        (self.hook)(point);
        self.check()
    }
}
fn id(stat: &fs::Metadata) -> FileIdentity {
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
fn identity(file: &File) -> Result<FileIdentity> {
    file.metadata()
        .map(|s| id(&s))
        .map_err(|e| EffectError::io("fstat", e))
}
fn dir_id(id: &FileIdentity) -> Result<DirectoryIdentity> {
    if id.mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(EffectError::new(
            "copy_effect_guard_changed",
            "Expected real directory",
        ));
    }
    Ok(DirectoryIdentity {
        device: id.device,
        inode: id.inode,
        mode: id.mode,
    })
}
fn same_promoted(before: &FileIdentity, after: &FileIdentity) -> bool {
    before.device == after.device
        && before.inode == after.inode
        && before.length == after.length
        && before.mode == after.mode
        && before.mtime_seconds == after.mtime_seconds
        && before.mtime_nanos == after.mtime_nanos
}
fn cstr(s: &OsStr) -> Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| EffectError::new("copy_effect_path", "Embedded NUL"))
}
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().len() > 32768 || path.to_str().is_none() {
        return Err(EffectError::new(
            "copy_effect_path",
            "Invalid path encoding/length",
        ));
    }
    let full = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| EffectError::io("current directory", e))?
            .join(path)
    };
    let mut out = PathBuf::from("/");
    for part in full.components() {
        match part {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir => {}
            _ => {
                return Err(EffectError::new(
                    "copy_effect_path",
                    "Unsupported path prefix",
                ))
            }
        }
        if out.components().count() > 64 {
            return Err(EffectError::new(
                "copy_effect_limit",
                "Effect path depth limit",
            ));
        }
    }
    Ok(out)
}
fn open_at(parent: &File, name: &OsStr, flags: i32, mode: u32) -> Result<File> {
    let name = cstr(name)?;
    // SAFETY: descriptor is live and C name is valid through the call.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        Err(path_open_error(
            "openat",
            std::io::Error::last_os_error(),
            flags,
        ))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn path_open_error(action: &str, error: std::io::Error, flags: i32) -> EffectError {
    let mut e = EffectError::io(action, error);
    if flags & libc::O_PATH != 0
        && matches!(
            e.os_code,
            Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
        )
    {
        e.code = "unsupported_path_handles";
    }
    e
}
fn entry(parent: &File, name: &OsStr) -> Result<Option<(File, FileIdentity)>> {
    match open_at(parent, name, libc::O_PATH | libc::O_NOFOLLOW, 0) {
        Ok(file) => {
            let expected = identity(&file)?;
            Ok(Some((file, expected)))
        }
        Err(e) if e.os_code == Some(libc::ENOENT) => Ok(None),
        Err(e) => Err(e),
    }
}
fn rename(parent: &File, old: &OsStr, new: &OsStr) -> Result<()> {
    rename_between(parent, old, parent, new)
}
fn rename_between(from: &File, old: &OsStr, to: &File, new: &OsStr) -> Result<()> {
    let old = cstr(old)?;
    let new = cstr(new)?;
    // SAFETY: both single-component names are valid, anchored parent is live.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.as_raw_fd(),
            old.as_ptr(),
            to.as_raw_fd(),
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP) => Err(EffectError::new(
            "unsupported_atomic_rename",
            "renameat2 NOREPLACE is unavailable on this kernel/filesystem",
        )),
        Some(libc::EEXIST) => Err(EffectError::new(
            "copy_effect_collision",
            "No-clobber destination collision",
        )),
        _ => Err(EffectError::io("renameat2", e)),
    }
}
fn unlink(parent: &File, name: &OsStr, directory: bool) -> Result<()> {
    let name = cstr(name)?;
    // SAFETY: relative C name and descriptor are live.
    if unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } == 0
    {
        Ok(())
    } else {
        Err(EffectError::io("unlinkat", std::io::Error::last_os_error()))
    }
}
fn random_name() -> Result<OsString> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| EffectError::new("copy_effect_random", e.to_string()))?;
    Ok(OsString::from(format!(
        ".freeshow-copy-{}",
        bytes.iter().map(|v| format!("{v:02x}")).collect::<String>()
    )))
}
#[derive(Clone)]
struct Anchor {
    path: PathBuf,
    file: Arc<File>,
    chain: Vec<DirectoryIdentity>,
}
impl Anchor {
    fn open(path: &Path, mut tick: impl FnMut() -> Result<()>) -> Result<Self> {
        let path = absolute(path)?;
        // SAFETY: static absolute C root and owned returned descriptor.
        let fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(path_open_error(
                "open filesystem root",
                std::io::Error::last_os_error(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            ));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut chain = vec![dir_id(&identity(&file)?)?];
        for component in path.components() {
            if let Component::Normal(name) = component {
                tick()?;
                file = open_at(
                    &file,
                    name,
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    0,
                )?;
                chain.push(dir_id(&identity(&file)?)?);
            }
        }
        Ok(Self {
            path,
            file: Arc::new(file),
            chain,
        })
    }
    fn check(&self, tick: impl FnMut() -> Result<()>) -> Result<()> {
        let current = Self::open(&self.path, tick)?;
        if current.chain != self.chain
            || dir_id(&identity(&self.file)?)? != *self.chain.last().unwrap()
        {
            return Err(EffectError::new(
                "copy_effect_parent_changed",
                "Anchored parent path changed",
            ));
        }
        Ok(())
    }
}
struct Slot {
    expected: DirectoryGuard,
    anchor: Option<Anchor>,
}
struct Graph {
    slots: BTreeMap<PathBuf, Slot>,
}
impl Graph {
    fn add(&mut self, path: &Path, expected: &DirectoryGuard) -> Result<()> {
        let path = absolute(path)?;
        if let Some(slot) = self.slots.get(&path) {
            if !matches!(
                (&slot.expected, expected),
                (DirectoryGuard::Absent, DirectoryGuard::Absent)
            ) && !matches!((&slot.expected,expected),(DirectoryGuard::Existing(a),DirectoryGuard::Existing(b)) if a==b)
            {
                return Err(EffectError::new(
                    "copy_effect_invalid_plan",
                    "Inconsistent sealed directory guards",
                ));
            }
        } else {
            self.slots.insert(
                path,
                Slot {
                    expected: expected.clone(),
                    anchor: None,
                },
            );
        }
        Ok(())
    }
    fn guards(&mut self, list: &[AncestorGuard]) -> Result<()> {
        for guard in list {
            self.add(guard.path(), guard.expected())?;
        }
        Ok(())
    }
    fn preflight(&mut self, budget: &mut Budget<'_>) -> Result<()> {
        for (path, slot) in &mut self.slots {
            budget.check()?;
            match &slot.expected {
                DirectoryGuard::Existing(expected) => {
                    let anchor = Anchor::open(path, || budget.check())?;
                    if anchor.chain.last() != Some(expected) {
                        return Err(EffectError::new(
                            "copy_effect_guard_changed",
                            "Directory guard differs",
                        ));
                    }
                    slot.anchor = Some(anchor);
                }
                DirectoryGuard::Absent => match fs::symlink_metadata(path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        return Err(EffectError::new(
                            "copy_effect_guard_changed",
                            "Expected absent directory appeared",
                        ))
                    }
                    Err(e) => return Err(EffectError::io("inspect absent directory", e)),
                },
            }
        }
        Ok(())
    }
    fn check_all(&self, budget: &mut Budget<'_>) -> Result<()> {
        for (path, slot) in &self.slots {
            budget.check()?;
            if let Some(anchor) = &slot.anchor {
                anchor.check(|| budget.check())?;
            } else if fs::symlink_metadata(path).is_ok() {
                return Err(EffectError::new(
                    "copy_effect_guard_changed",
                    "Absent directory appeared",
                ));
            }
        }
        Ok(())
    }
    fn ensure(
        &mut self,
        path: &Path,
        journal: &mut Vec<Journal>,
        budget: &mut Budget<'_>,
    ) -> Result<Anchor> {
        budget.check()?;
        let path = absolute(path)?;
        let slot = self.slots.get(&path).ok_or_else(|| {
            EffectError::new(
                "copy_effect_invalid_plan",
                "Directory is not a sealed effect/ancestor",
            )
        })?;
        if let Some(anchor) = &slot.anchor {
            anchor.check(|| budget.check())?;
            return Ok(anchor.clone());
        }
        let parent_path = path.parent().ok_or_else(|| {
            EffectError::new("copy_effect_invalid_plan", "Missing root directory guard")
        })?;
        let parent = self.ensure(parent_path, journal, budget)?;
        let name = path.file_name().unwrap();
        if entry(&parent.file, name)?.is_some() {
            return Err(EffectError::new(
                "copy_effect_collision",
                "Missing directory appeared",
            ));
        }
        let temporary = random_name()?;
        let c = cstr(&temporary)?;
        // Source mkdirSync(recursive:true) uses 0777 filtered by process umask.
        // SAFETY: parent fd/name live; no global umask is modified.
        if unsafe { libc::mkdirat(parent.file.as_raw_fd(), c.as_ptr(), 0o777) } != 0 {
            let mut e = EffectError::io("mkdirat", std::io::Error::last_os_error());
            e.code = "copy_effect_mkdir_io";
            return Err(e);
        }
        let file = open_at(
            &parent.file,
            &temporary,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|mut e| {
            e.rollback_incomplete = true;
            e.detail(format!(
                "Unanchored temporary directory retained: {}",
                parent.path.join(&temporary).display()
            ));
            e
        })?;
        let metadata = file.metadata().map_err(|error| {
            // No directory inode was yet accepted as owned; preserve the name.
            let mut e = EffectError::io("stat new directory", error);
            e.rollback_incomplete = true;
            e.detail(format!(
                "Unidentified directory retained {}",
                parent.path.join(&temporary).display()
            ));
            e
        })?;
        // SAFETY: geteuid has no pointer arguments or side effects. A writer of
        // a shared parent must not substitute its own inode before our anchor.
        if metadata.uid() != unsafe { libc::geteuid() } {
            let mut e = EffectError::new(
                "copy_effect_guard_changed",
                "New directory is not owned by the executor uid",
            );
            e.rollback_incomplete = true;
            e.detail(format!(
                "Unowned directory retained {}",
                parent.path.join(&temporary).display()
            ));
            return Err(e);
        }
        let settled = id(&metadata);
        journal.push(Journal {
            parent: parent.clone(),
            name: temporary.clone(),
            final_name: name.to_owned(),
            held: Held {
                file: Arc::new(file),
                settled,
            },
            directory: true,
        });
        parent.check(|| budget.check())?;
        let record = journal.last_mut().unwrap();
        checked_entry(record)?;
        if let Err(mut e) = rename(&parent.file, &temporary, name) {
            if let Err(cleanup) = remove_owned(record) {
                e.rollback_incomplete = true;
                e.detail(format!("Temporary directory cleanup failed: {cleanup}"));
            } else {
                journal.pop();
            }
            return Err(e);
        }
        record.name = name.to_owned();
        settle(record)?;
        let mut chain = parent.chain.clone();
        chain.push(dir_id(&record.held.settled)?);
        let anchor = Anchor {
            path: path.clone(),
            file: record.held.file.clone(),
            chain,
        };
        let slot = self.slots.get_mut(&path).unwrap();
        slot.expected = DirectoryGuard::Existing(dir_id(&record.held.settled)?);
        slot.anchor = Some(anchor.clone());
        anchor.check(|| budget.check())?;
        Ok(anchor)
    }
    fn parent_for(&self, target: &Path, budget: &mut Budget<'_>) -> Result<Option<Anchor>> {
        let parent = target
            .parent()
            .ok_or_else(|| EffectError::new("copy_effect_path", "Target has no parent"))?;
        let slot = self.slots.get(parent).ok_or_else(|| {
            EffectError::new("copy_effect_invalid_plan", "Target parent is not sealed")
        })?;
        if let Some(anchor) = &slot.anchor {
            anchor.check(|| budget.check())?;
            Ok(Some(anchor.clone()))
        } else {
            Ok(None)
        }
    }
    fn target(&self, path: &Path, budget: &mut Budget<'_>) -> Result<Option<FileIdentity>> {
        budget.check()?;
        match self.parent_for(path, budget)? {
            Some(a) => Ok(entry(&a.file, path.file_name().unwrap())?.map(|(_, i)| i)),
            None => Ok(None),
        }
    }
}
struct Held {
    file: Arc<File>,
    settled: FileIdentity,
}
struct Journal {
    parent: Anchor,
    name: OsString,
    final_name: OsString,
    held: Held,
    directory: bool,
}
fn checked_entry(record: &Journal) -> Result<()> {
    let current = identity(&record.held.file)?;
    let named = entry(&record.parent.file, &record.name)?.map(|(_, i)| i);
    let same = if record.directory {
        dir_id(&current)? == dir_id(&record.held.settled)?
            && named.as_ref().map(dir_id).transpose()? == Some(dir_id(&current)?)
    } else {
        current == record.held.settled && named.as_ref() == Some(&current)
    };
    if !same {
        return Err(EffectError::new(
            "copy_effect_guard_changed",
            "Owned entry changed externally",
        ));
    }
    Ok(())
}
fn settle(record: &mut Journal) -> Result<()> {
    let after = identity(&record.held.file)?;
    let same = if record.directory {
        dir_id(&record.held.settled)? == dir_id(&after)?
    } else {
        same_promoted(&record.held.settled, &after)
    };
    if !same {
        return Err(EffectError::new(
            "copy_effect_guard_changed",
            "Promoted anchor metadata changed",
        ));
    }
    record.held.settled = after;
    checked_entry(record)
}
fn source(guard: &SourceGuard, budget: &mut Budget<'_>) -> Result<()> {
    budget.check()?;
    let path = Path::new(guard.path());
    let link = fs::symlink_metadata(path).map_err(|e| EffectError::io("source lstat", e))?;
    let target = fs::metadata(path).map_err(|e| EffectError::io("source stat", e))?;
    if id(&link) != *guard.link() || id(&target) != *guard.target() {
        return Err(EffectError::new(
            "copy_effect_source_changed",
            "Original source metadata guard changed",
        ));
    }
    budget.check()
}
fn hash(file: &File, budget: &mut Budget<'_>) -> Result<String> {
    let mut file = file;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| EffectError::io("seek staged", e))?;
    let mut sha = Sha256::new();
    let mut chunk = [0; CHUNK];
    loop {
        budget.check()?;
        let n = file
            .read(&mut chunk)
            .map_err(|e| EffectError::io("read staged", e))?;
        if n == 0 {
            break;
        }
        budget.read(n)?;
        sha.update(&chunk[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
struct Staged {
    file: File,
    parent: Anchor,
}
fn staged(install: &Install, plan: &PreparedMediaCopy, budget: &mut Budget<'_>) -> Result<Staged> {
    let path = install.staged_path();
    if path.parent().map(Path::as_os_str) != Some(plan.staging_directory().as_os_str()) {
        return Err(EffectError::new(
            "copy_effect_invalid_plan",
            "Staged media is outside private captured directory",
        ));
    }
    let parent = Anchor::open(path.parent().unwrap(), || budget.check())?;
    let file = open_at(
        &parent.file,
        path.file_name().unwrap(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    if install.expected_staged().mode & libc::S_IFMT != libc::S_IFREG
        || identity(&file)? != *install.expected_staged()
    {
        return Err(EffectError::new(
            "copy_effect_stage_changed",
            "Staged media identity/type changed",
        ));
    }
    if hash(&file, budget)? != install.sha256() || identity(&file)? != *install.expected_staged() {
        return Err(EffectError::new(
            "copy_effect_hash_changed",
            "Staged media hash/identity changed",
        ));
    }
    parent.check(|| budget.check())?;
    if entry(&parent.file, path.file_name().unwrap())?
        .as_ref()
        .map(|(_, i)| i)
        != Some(install.expected_staged())
    {
        return Err(EffectError::new(
            "copy_effect_stage_changed",
            "Staged path changed during open",
        ));
    }
    Ok(Staged { file, parent })
}
fn check_stage(install: &Install, stage: &Staged, budget: &mut Budget<'_>) -> Result<()> {
    budget.check()?;
    stage.parent.check(|| budget.check())?;
    if identity(&stage.file)? != *install.expected_staged()
        || fs::symlink_metadata(install.staged_path())
            .map(|s| id(&s))
            .ok()
            .as_ref()
            != Some(install.expected_staged())
    {
        return Err(EffectError::new(
            "copy_effect_stage_changed",
            "Staged media anchor/path changed",
        ));
    }
    Ok(())
}
fn relative(root: &Path, path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || path.components().count() != 2
        || path.as_os_str().len() > 32768
    {
        return Err(EffectError::new(
            "copy_effect_invalid_plan",
            "Invalid sealed relative copy destination",
        ));
    }
    absolute(&root.join(path))
}
fn guard_target(
    graph: &Graph,
    path: &Path,
    expected: &PathGuard,
    budget: &mut Budget<'_>,
) -> Result<()> {
    let actual = graph.target(path, budget)?;
    if !matches!((expected, &actual), (PathGuard::Absent, None))
        && !matches!((expected,&actual),(PathGuard::Existing(e),Some(a)) if e==a)
    {
        return Err(EffectError::new(
            "copy_effect_guard_changed",
            "Copy target guard changed",
        ));
    }
    Ok(())
}
fn ordinary(e: &EffectError) -> bool {
    !e.rollback_incomplete
        && matches!(e.code, "copy_effect_mkdir_io" | "copy_effect_copy_io")
        && matches!(
            e.os_code,
            Some(
                libc::EACCES
                    | libc::EPERM
                    | libc::EROFS
                    | libc::ENOSPC
                    | libc::EDQUOT
                    | libc::EIO
                    | libc::ENAMETOOLONG
                    | libc::EFBIG
            )
        )
}
fn remove_owned(record: &mut Journal) -> Result<()> {
    record.parent.check(|| Ok(()))?;
    checked_entry(record)?;
    // Quarantine under a 0700 directory before unlinking. Parent writers cannot
    // swap the checked leaf between identity validation and unlink inside this
    // private directory. Concurrent hostile code under the same uid is outside
    // this protection (as it is for the preparation's private stage).
    let name = random_name()?;
    let c = cstr(&name)?;
    // SAFETY: live anchored descriptor and single relative name.
    if unsafe { libc::mkdirat(record.parent.file.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
        return Err(EffectError::io(
            "mkdir rollback quarantine",
            std::io::Error::last_os_error(),
        ));
    }
    let file = open_at(
        &record.parent.file,
        &name,
        libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
    .map_err(|mut e| {
        e.detail(format!(
            "Unanchored rollback quarantine retained {}",
            record.parent.path.join(&name).display()
        ));
        e
    })?;
    let metadata = file
        .metadata()
        .map_err(|e| EffectError::io("stat quarantine", e))?;
    // SAFETY: geteuid takes no pointers and has no side effects.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(EffectError::new(
            "copy_effect_guard_changed",
            "Rollback quarantine is not private and owned",
        ));
    }
    let qid = dir_id(&id(&metadata))?;
    let leaf = OsStr::new("owned");
    let result = (|| {
        checked_entry(record)?;
        rename_between(&record.parent.file, &record.name, &file, leaf)?;
        let after = identity(&record.held.file)?;
        let content_same = if record.directory {
            dir_id(&after)? == dir_id(&record.held.settled)?
        } else {
            same_promoted(&record.held.settled, &after)
        };
        let named = entry(&file, leaf)?.map(|(_, i)| i);
        let valid = content_same && named.as_ref() == Some(&after);
        let removed = if valid {
            unlink(&file, leaf, record.directory)
        } else {
            Err(EffectError::new(
                "copy_effect_guard_changed",
                "Quarantined inode differs from owned anchor",
            ))
        };
        if let Err(mut e) = removed {
            // Restore a changed/occupied directory to its old name without
            // replacing any new arrival. Otherwise retain the private backup.
            if let Err(restore) = rename_between(&file, leaf, &record.parent.file, &record.name) {
                e.detail(format!(
                    "Private quarantine retained {}: {restore}",
                    record.parent.path.join(&name).display()
                ));
            } else if content_same {
                record.held.settled = identity(&record.held.file)?;
            }
            return Err(e);
        }
        Ok(())
    })();
    let cleanup = (|| {
        record.parent.check(|| Ok(()))?;
        if entry(&record.parent.file, &name)?
            .as_ref()
            .map(|(_, i)| dir_id(i))
            .transpose()?
            != Some(qid)
        {
            return Err(EffectError::new(
                "copy_effect_parent_changed",
                "Private quarantine name changed",
            ));
        }
        unlink(&record.parent.file, &name, true)
    })();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(mut e), cleanup) => {
            if let Err(c) = cleanup {
                e.detail(format!(
                    "Rollback quarantine retained {}: {c}",
                    record.parent.path.join(&name).display()
                ));
            }
            Err(e)
        }
        (Ok(()), Err(mut e)) => {
            e.detail(format!(
                "Empty rollback quarantine retained {}",
                record.parent.path.join(&name).display()
            ));
            Err(e)
        }
    }
}
fn rollback(journal: &mut [Journal], error: &mut EffectError) {
    // Independent bounded cleanup: cancellation/exhausted work budgets do not
    // stop attempts to preserve/remove every previously anchored owned entry.
    for record in journal.iter_mut().rev() {
        if let Err(e) = remove_owned(record) {
            error.rollback_incomplete = true;
            error.detail(format!(
                "Retained changed/occupied owned entry {}: {e}",
                record.parent.path.join(&record.name).display()
            ));
            for detail in e.observed.diagnostics {
                error.detail(detail);
            }
            error.observed.diagnostics_omitted = error
                .observed
                .diagnostics_omitted
                .saturating_add(e.observed.diagnostics_omitted);
        }
    }
}
fn install(
    index: usize,
    item: &Install,
    staged: &mut Staged,
    parent: &Anchor,
    journal: &mut Vec<Journal>,
    budget: &mut Budget<'_>,
) -> Result<()> {
    parent.check(|| budget.check())?;
    source(item.source(), budget)?;
    check_stage(item, staged, budget)?;
    let destination = item.destination().as_path().file_name().unwrap();
    if entry(&parent.file, destination)?.is_some() {
        return Err(EffectError::new(
            "copy_effect_collision",
            "Destination appeared before copy",
        ));
    }
    let temporary = random_name()?;
    let file = Arc::new(
        open_at(
            &parent.file,
            &temporary,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        )
        .map_err(|mut e| {
            e.code = "copy_effect_copy_io";
            e
        })?,
    );
    let settled = identity(&file).map_err(|mut e| {
        e.rollback_incomplete = true;
        e.detail(format!(
            "Unidentified temporary file retained {}",
            parent.path.join(&temporary).display()
        ));
        e
    })?;
    journal.push(Journal {
        parent: parent.clone(),
        name: temporary,
        final_name: destination.to_owned(),
        held: Held {
            settled,
            file: file.clone(),
        },
        directory: false,
    });
    let record = journal.last_mut().unwrap();
    staged
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|e| EffectError::io("seek staged media", e))?;
    let mut sha = Sha256::new();
    let mut length = 0u64;
    let mut chunk = [0; CHUNK];
    loop {
        budget.check()?;
        let n = staged
            .file
            .read(&mut chunk)
            .map_err(|e| EffectError::io("copy staged media", e))?;
        if n == 0 {
            break;
        }
        budget.read(n)?;
        budget.write(n)?;
        length += n as u64;
        if length > item.expected_staged().length {
            return Err(EffectError::new(
                "copy_effect_stage_changed",
                "Staged media grew",
            ));
        }
        let mut pending = &chunk[..n];
        while !pending.is_empty() {
            budget.check()?;
            let written = file.as_ref().write(pending);
            // Settle even partial/failed writes before observing cancellation.
            record.held.settled = identity(&file)?;
            match written {
                Ok(0) => {
                    let mut e = EffectError::io(
                        "write destination temporary",
                        std::io::Error::from(std::io::ErrorKind::WriteZero),
                    );
                    e.code = "copy_effect_copy_io";
                    return Err(e);
                }
                Ok(n) => pending = &pending[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let mut e = EffectError::io("write destination temporary", e);
                    e.code = "copy_effect_copy_io";
                    return Err(e);
                }
            }
        }
        sha.update(&chunk[..n]);
        budget.event(Point::CopyChunk)?;
    }
    if length != item.expected_staged().length || format!("{:x}", sha.finalize()) != item.sha256() {
        return Err(EffectError::new(
            "copy_effect_hash_changed",
            "Copied bytes differ from sealed staged hash",
        ));
    }
    // SAFETY: own regular file descriptor, preserving source permission bits.
    let chmod = unsafe { libc::fchmod(file.as_raw_fd(), item.source().target().mode & 0o7777) };
    let chmod_error = if chmod != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    record.held.settled = identity(&file)?;
    if let Some(chmod_error) = chmod_error {
        let mut e = EffectError::io("chmod copied file", chmod_error);
        e.code = "copy_effect_copy_io";
        return Err(e);
    }
    let sync = file.sync_all();
    record.held.settled = identity(&file)?;
    sync.map_err(|e| EffectError::io("sync copied file", e))?;
    source(item.source(), budget)?;
    check_stage(item, staged, budget)?;
    budget.event(Point::BeforePromotion(index))?;
    parent.check(|| budget.check())?;
    source(item.source(), budget)?;
    check_stage(item, staged, budget)?;
    checked_entry(record)?;
    rename(&parent.file, &record.name, &record.final_name)?;
    record.name = record.final_name.clone();
    (budget.hook)(Point::PromotionRenamed(index));
    settle(record)?;
    budget.event(Point::AfterPromotion(index))?;
    checked_entry(record)?;
    // Hash while holding the pre-promotion inode anchor; never adopt a new path.
    if hash(&file, budget)? != item.sha256() {
        return Err(EffectError::new(
            "copy_effect_hash_changed",
            "Installed content changed",
        ));
    }
    checked_entry(record)?;
    source(item.source(), budget)?;
    parent.check(|| budget.check())?;
    Ok(())
}
pub(crate) fn apply(
    plan: &PreparedMediaCopy,
    lease: &CommitLease,
    limits: EffectLimits,
    cancel: &CancellationToken,
    hook: &mut dyn FnMut(Point),
) -> Result<Receipt> {
    let mut budget = Budget {
        plan,
        lease,
        limits,
        cancel,
        started: Instant::now(),
        operations: 0,
        read: 0,
        written: 0,
        chunks: 0,
        hook,
    };
    budget.check()?;
    let root = absolute(plan.destination_root())?;
    let mut graph = Graph {
        slots: BTreeMap::new(),
    };
    graph.guards(plan.destination_ancestors())?;
    let mut dirs = HashMap::new();
    let mut targets = HashMap::new();
    let mut stages = HashMap::new();
    let mut declared = 0u64;
    let mut output = 32768usize.saturating_add(root.as_os_str().len() * 6 + 512);
    for effect in plan.effects() {
        budget.check()?;
        match effect {
            CopyEffect::EnsureShowsRoot {
                path, ancestors, ..
            } => {
                graph.guards(ancestors)?;
                output = output.saturating_add(path.as_os_str().len() * 6 + 512);
            }
            CopyEffect::EnsureDestinationRoot => {
                output = output.saturating_add(root.as_os_str().len() * 6 + 512);
            }
            CopyEffect::EnsureDirectory {
                destination,
                expected,
            } => {
                if destination.as_path().components().count() != 1 {
                    return Err(EffectError::new(
                        "copy_effect_invalid_plan",
                        "Invalid sealed hash directory",
                    ));
                }
                let path = absolute(&root.join(destination.as_path()))?;
                graph.add(&path, expected)?;
                dirs.insert(destination.as_path().as_os_str().to_owned(), path);
            }
            CopyEffect::Install { file } => {
                declared = declared.saturating_add(file.expected_staged().length);
                targets.insert(file.destination().as_path().to_owned(), effect);
            }
            CopyEffect::SkipExisting { destination, .. }
            | CopyEffect::CopyAttemptFailure { destination, .. } => {
                targets.insert(destination.as_path().to_owned(), effect);
            }
        }
    }
    // Every candidate can generate one source mkdir ALERT, one failure/install
    // record, and an early missing-open ALERT. Reject before effects if they
    // could exceed the aggregate output budget; required source alerts aren't
    // silently truncated into diagnostic omission counters.
    for candidate in plan.candidates() {
        output = output.saturating_add(candidate.len() * 24 + root.as_os_str().len() * 12 + 2048);
    }
    if output > budget.limits.max_output_bytes
        || declared > budget.limits.max_install_bytes
        || declared.saturating_mul(3) > budget.limits.max_read_bytes
    {
        return Err(EffectError::new(
            "copy_effect_limit",
            "Declared copy/output byte limit",
        ));
    }
    graph.preflight(&mut budget)?;
    for guard in plan.show_guards() {
        source(guard, &mut budget)?;
    }
    for effect in plan.effects() {
        match effect {
            CopyEffect::EnsureShowsRoot {
                path,
                listing_guard,
                ..
            } => {
                budget.check()?;
                let current = fs::symlink_metadata(path);
                let valid = match (listing_guard, current) {
                    (PathGuard::Absent, Err(e)) => e.kind() == std::io::ErrorKind::NotFound,
                    (PathGuard::Existing(expected), Ok(s)) => *expected == id(&s),
                    _ => false,
                };
                if !valid {
                    return Err(EffectError::new(
                        "copy_effect_source_changed",
                        "Shows listing guard changed",
                    ));
                }
            }
            CopyEffect::Install { file } => {
                if !matches!(file.expected_destination(), PathGuard::Absent) {
                    return Err(EffectError::new(
                        "copy_effect_invalid_plan",
                        "Copy installation must be no-clobber",
                    ));
                }
                source(file.source(), &mut budget)?;
                let path = relative(&root, file.destination().as_path())?;
                guard_target(&graph, &path, file.expected_destination(), &mut budget)?;
                let handle = staged(file, plan, &mut budget)?;
                stages.insert(file.destination().as_path().to_owned(), handle);
            }
            CopyEffect::CopyAttemptFailure { destination, .. } => {
                guard_target(
                    &graph,
                    &relative(&root, destination.as_path())?,
                    &PathGuard::Absent,
                    &mut budget,
                )?;
            }
            CopyEffect::SkipExisting {
                destination,
                expected,
                followed,
            } => {
                let path = relative(&root, destination.as_path())?;
                guard_target(
                    &graph,
                    &path,
                    &PathGuard::Existing(expected.clone()),
                    &mut budget,
                )?;
                if let Some(expected) = followed {
                    if fs::metadata(path).map(|s| id(&s)).ok().as_ref() != Some(expected) {
                        return Err(EffectError::new(
                            "copy_effect_guard_changed",
                            "Existing destination symlink target changed",
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    budget.event(Point::AfterPreflight)?;
    graph.check_all(&mut budget)?;
    let mut receipt = Receipt {
        reply: match plan.deferred_reply() {
            DeferredReply::NoReply => None,
            _ => Some(false),
        },
        installed: vec![],
        failures: vec![],
        skipped_existing: 0,
        duplicate_destinations: 0,
        alerts: vec![],
        open_folder: None,
        diagnostics: vec![],
        diagnostics_omitted: 0,
        preparation_diagnostics: plan.diagnostics().to_vec(),
        preparation_diagnostics_omitted: plan.omitted_diagnostics(),
    };
    let mut journal = vec![];
    let result = (|| {
        for effect in plan.effects() {
            let path = match effect {
                CopyEffect::EnsureShowsRoot { path, .. } => Some(absolute(path)?),
                CopyEffect::EnsureDestinationRoot => Some(root.clone()),
                _ => None,
            };
            if let Some(path) = path {
                if let Err(e) = graph.ensure(&path, &mut journal, &mut budget) {
                    if ordinary(&e) {
                        receipt.alerts.push(format!(
                            "Error: Could not create folder at: {}!",
                            path.display()
                        ));
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        if plan.open_folder_intent() {
            budget.check()?;
            // Source uses existsSync (F_OK), following symlinks; root parents
            // were already no-follow validated when relevant effects exist.
            if plan.destination_root().exists() {
                receipt.open_folder = Some(OpenFolderIntent::OpenDirectory {
                    path: plan.destination_root().to_owned(),
                });
            } else {
                receipt.alerts.push("This does not exist!".into());
                receipt.open_folder = Some(OpenFolderIntent::Missing {
                    path: plan.destination_root().to_owned(),
                });
            }
        }
        let skipped: HashSet<_> =
            plan.observations()
                .iter()
                .filter_map(|o| match o {
                    CopyObservation::PrefixSkipped { source }
                    | CopyObservation::Missing { source } => Some(source.as_str()),
                    _ => None,
                })
                .collect();
        let mut visited = HashSet::new();
        for (index, candidate) in plan.candidates().iter().enumerate() {
            budget.check()?;
            if skipped.contains(candidate.as_str()) {
                continue;
            }
            let folder = parent_folder_id(candidate);
            let directory = dirs.get(OsStr::new(&folder)).ok_or_else(|| {
                EffectError::new(
                    "copy_effect_invalid_plan",
                    "Candidate has no sealed directory effect",
                )
            })?;
            let (parent, parent_error) = match graph.ensure(directory, &mut journal, &mut budget) {
                Ok(a) => (Some(a), None),
                Err(e) if ordinary(&e) => {
                    receipt.alerts.push(format!(
                        "Error: Could not create folder at: {}!",
                        directory.display()
                    ));
                    (None, e.os_code)
                }
                Err(e) => return Err(e),
            };
            let name = candidate
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("");
            if matches!(name, "" | "." | "..") {
                let target = if name == ".." { &root } else { directory };
                budget.check()?;
                if target.exists() {
                    receipt.skipped_existing += 1;
                } else {
                    if receipt.reply.is_some() {
                        receipt.reply = Some(true);
                    }
                    receipt.failures.push(CopyFailure {
                        source: candidate.clone(),
                        destination: target.clone(),
                        os_code: None,
                        phase: FailurePhase::DirectoryDestinationMissing,
                    });
                }
                continue;
            }
            let key = PathBuf::from(&folder).join(name);
            let target = relative(&root, &key)?;
            let effect = targets.get(&key).ok_or_else(|| {
                EffectError::new(
                    "copy_effect_invalid_plan",
                    "Candidate has no sealed target effect",
                )
            })?;
            if let CopyEffect::SkipExisting {
                expected, followed, ..
            } = effect
            {
                guard_target(
                    &graph,
                    &target,
                    &PathGuard::Existing(expected.clone()),
                    &mut budget,
                )?;
                if let Some(expected) = followed {
                    if fs::metadata(&target).map(|s| id(&s)).ok().as_ref() != Some(expected) {
                        return Err(EffectError::new(
                            "copy_effect_guard_changed",
                            "Skipped target changed",
                        ));
                    }
                }
                receipt.skipped_existing += 1;
                continue;
            }
            if !visited.insert(key.clone()) {
                receipt.duplicate_destinations += 1;
                continue;
            }
            guard_target(&graph, &target, &PathGuard::Absent, &mut budget)?;
            if receipt.reply.is_some() {
                receipt.reply = Some(true);
            }
            if let CopyEffect::CopyAttemptFailure { os_code, .. } = effect {
                receipt.failures.push(CopyFailure {
                    source: candidate.clone(),
                    destination: target,
                    os_code: *os_code,
                    phase: FailurePhase::PreparedSourceRead,
                });
                continue;
            }
            let CopyEffect::Install { file } = effect else {
                return Err(EffectError::new(
                    "copy_effect_invalid_plan",
                    "Unexpected candidate target effect",
                ));
            };
            let Some(parent) = parent else {
                receipt.failures.push(CopyFailure {
                    source: candidate.clone(),
                    destination: target,
                    os_code: parent_error,
                    phase: FailurePhase::ParentUnavailable,
                });
                continue;
            };
            let start = journal.len();
            match install(
                index,
                file,
                stages.get_mut(&key).unwrap(),
                &parent,
                &mut journal,
                &mut budget,
            ) {
                Ok(()) => receipt.installed.push(InstalledCopy {
                    destination: target,
                    length: file.expected_staged().length,
                    sha256: file.sha256().into(),
                }),
                Err(mut e) if ordinary(&e) => {
                    rollback(&mut journal[start..], &mut e);
                    if e.rollback_incomplete {
                        return Err(e);
                    }
                    journal.truncate(start);
                    receipt.failures.push(CopyFailure {
                        source: candidate.clone(),
                        destination: target,
                        os_code: e.os_code,
                        phase: FailurePhase::FinalCopy,
                    });
                }
                Err(e) => return Err(e),
            }
        }
        graph.check_all(&mut budget)?;
        for guard in plan.show_guards() {
            source(guard, &mut budget)?;
        }
        for effect in plan.effects() {
            if let CopyEffect::Install { file } = effect {
                source(file.source(), &mut budget)?;
                check_stage(
                    file,
                    stages.get(file.destination().as_path()).unwrap(),
                    &mut budget,
                )?;
            }
        }
        for record in &journal {
            budget.check()?;
            checked_entry(record)?;
        }
        budget.check()?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(receipt),
        Err(mut error) => {
            error.observed.alerts = receipt.alerts;
            error.observed.failures = receipt.failures;
            error.observed.open_folder = receipt.open_folder;
            error.observed.preparation_diagnostics = receipt.preparation_diagnostics;
            error.observed.preparation_diagnostics_omitted =
                receipt.preparation_diagnostics_omitted;
            (budget.hook)(Point::BeforeRollback);
            rollback(&mut journal, &mut error);
            Err(error)
        }
    }
}
