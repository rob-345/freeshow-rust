//! Bounded filesystem installation for native prepared imports. The caller must
//! hold the ordered library owner and validate its frozen runtime context.
//! Checks are path-based: they detect static symlinks and observed changes, but
//! do not claim confinement against concurrent ancestor rename/replacement.
use freeshow_imports::{FileIdentity, MediaCommitPlan, MediaInstall};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};

const CHUNK: usize = 32 * 1024;
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ROLLBACK_DETAILS: usize = 64;
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

fn diagnostic(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.into();
    }
    const MARKER: &str = " [truncated]";
    let mut end = bytes.saturating_sub(MARKER.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &text[..end])
}

#[derive(Clone, Debug)]
pub struct EffectConfig {
    pub max_bytes: u64,
    pub max_entries: usize,
    pub max_operations: usize,
    pub timeout: Duration,
}
impl Default for EffectConfig {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            max_entries: 8192,
            max_operations: 200_000,
            timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct InstalledMedia {
    pub destination: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
pub struct CommitReceipt {
    pub installed: Vec<InstalledMedia>,
    /// Relative to the trusted root; an empty path denotes creation of the root.
    pub created_directories: Vec<PathBuf>,
    pub existing_media: usize,
    /// Intentionally unresolved original references are not installation errors.
    pub unresolved_media: usize,
}
#[derive(Debug)]
pub struct EffectError {
    pub code: &'static str,
    pub message: String,
    pub rollback_failures: Vec<String>,
    pub rollback_omitted: usize,
}
impl EffectError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: diagnostic(&message.into(), 8192),
            rollback_failures: vec![],
            rollback_omitted: 0,
        }
    }
    fn io(path: &Path, error: std::io::Error) -> Self {
        Self::new("import_effect_io", format!("{}: {error}", path.display()))
    }
    fn rollback_failure(&mut self, path: &Path, error: &EffectError) {
        let used: usize = self.rollback_failures.iter().map(String::len).sum();
        if self.rollback_failures.len() >= MAX_ROLLBACK_DETAILS || used + 32 > MAX_DIAGNOSTIC_BYTES
        {
            self.rollback_omitted += 1;
            return;
        }
        let detail = format!("{}: {error}", diagnostic(&path.to_string_lossy(), 2048));
        self.rollback_failures
            .push(diagnostic(&detail, (MAX_DIAGNOSTIC_BYTES - used).min(4096)));
    }
}
impl fmt::Display for EffectError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}: {}", self.code, self.message)?;
        if !self.rollback_failures.is_empty() {
            write!(
                out,
                "; rollback incomplete: {}",
                self.rollback_failures.join("; ")
            )?;
        }
        if self.rollback_omitted > 0 {
            write!(
                out,
                "; {} further rollback details omitted",
                self.rollback_omitted
            )?;
        }
        Ok(())
    }
}
impl std::error::Error for EffectError {}
pub type Result<T> = std::result::Result<T, EffectError>;

struct Budget<'a> {
    plan: &'a MediaCommitPlan,
    config: EffectConfig,
    started: Instant,
    operations: usize,
}
impl Budget<'_> {
    fn check(&mut self) -> Result<()> {
        self.plan
            .ensure_current()
            .map_err(|error| EffectError::new(error.code, error.message))?;
        self.operations += 1;
        if self.operations > self.config.max_operations
            || self.started.elapsed() >= self.config.timeout
        {
            return Err(EffectError::new(
                "import_effect_limit",
                "Import install operation/deadline limit",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created_nanos: Option<u128>,
}
fn directory_identity(path: &Path) -> Result<DirectoryIdentity> {
    let metadata = fs::symlink_metadata(path).map_err(|e| EffectError::io(path, e))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(EffectError::new(
            "import_invalid_path",
            format!("Expected non-symlink directory: {}", path.display()),
        ));
    }
    Ok(DirectoryIdentity {
        #[cfg(unix)]
        device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
        #[cfg(not(unix))]
        created_nanos: metadata
            .created()
            .ok()
            .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
            .map(|v| v.as_nanos()),
    })
}
fn file_identity(metadata: &Metadata) -> Result<FileIdentity> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(EffectError::new(
            "import_invalid_path",
            "Expected regular non-symlink file",
        ));
    }
    Ok(FileIdentity {
        length: metadata.len(),
        modified_nanos: metadata
            .modified()
            .ok()
            .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
            .map(|v| v.as_nanos()),
        #[cfg(unix)]
        device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
    })
}
fn path_identity(path: &Path) -> Result<FileIdentity> {
    file_identity(&fs::symlink_metadata(path).map_err(|e| EffectError::io(path, e))?)
}
fn checked_file(path: &Path, expected: &FileIdentity) -> Result<File> {
    if &path_identity(path)? != expected {
        return Err(EffectError::new(
            "import_source_changed",
            format!("File identity changed: {}", path.display()),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| EffectError::io(path, e))?;
    if &file_identity(&file.metadata().map_err(|e| EffectError::io(path, e))?)? != expected
        || &path_identity(path)? != expected
    {
        return Err(EffectError::new(
            "import_source_changed",
            format!("File changed during open: {}", path.display()),
        ));
    }
    Ok(file)
}
fn normalized(path: &Path, absolute: bool) -> Result<()> {
    let text = path
        .to_str()
        .filter(|text| !text.contains(['\0', '\n', '\r']))
        .ok_or_else(|| {
            EffectError::new("import_invalid_path", "Non-Unicode/control import path")
        })?;
    let components: Vec<_> = path.components().collect();
    if text.is_empty()
        || text.len() > 32768
        || path.is_absolute() != absolute
        || components.len() > 64
    {
        return Err(EffectError::new(
            "import_invalid_path",
            "Import path length/depth/absolute limit",
        ));
    }
    if components.iter().any(|part| {
        !matches!(
            part,
            Component::Normal(_) | Component::RootDir | Component::Prefix(_)
        )
    }) || (!absolute
        && components
            .iter()
            .any(|part| !matches!(part, Component::Normal(_))))
        || components
            .iter()
            .map(|component| component.as_os_str())
            .collect::<PathBuf>()
            .as_os_str()
            != path.as_os_str()
    {
        return Err(EffectError::new(
            "import_invalid_path",
            "Import path must be normalized",
        ));
    }
    #[cfg(unix)]
    if text.contains('\\') {
        return Err(EffectError::new(
            "import_invalid_path",
            "Import path must use native separators",
        ));
    }
    Ok(())
}
fn destination(path: &Path) -> Result<()> {
    normalized(path, false)?;
    if !matches!(
        path.components()
            .next()
            .and_then(|v| v.as_os_str().to_str()),
        Some("Projects" | "Templates" | "Overlays")
    ) {
        return Err(EffectError::new(
            "import_invalid_path",
            "Unexpected import media namespace",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Parents(BTreeMap<PathBuf, DirectoryIdentity>);
impl Parents {
    fn observe(&mut self, path: &Path) -> Result<()> {
        let current = directory_identity(path)?;
        if self
            .0
            .get(path)
            .is_some_and(|expected| expected != &current)
        {
            return Err(EffectError::new(
                "import_parent_changed",
                format!("Directory identity changed: {}", path.display()),
            ));
        }
        if !self.0.contains_key(path) && self.0.len() >= 32768 {
            return Err(EffectError::new(
                "import_effect_limit",
                "Directory identity guard limit",
            ));
        }
        self.0.entry(path.into()).or_insert(current);
        Ok(())
    }
    fn absolute(
        &mut self,
        path: &Path,
        allow_missing: bool,
        budget: &mut Budget<'_>,
    ) -> Result<()> {
        let mut cursor = PathBuf::new();
        for component in path.components() {
            cursor.push(component.as_os_str());
            if !cursor.is_absolute() {
                continue;
            }
            budget.check()?;
            match fs::symlink_metadata(&cursor) {
                Ok(_) => self.observe(&cursor)?,
                Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                    break
                }
                Err(error) => return Err(EffectError::io(&cursor, error)),
            }
        }
        Ok(())
    }
    fn stage(&mut self, stage: &Path, file: &Path, budget: &mut Budget<'_>) -> Result<()> {
        normalized(stage, true)?;
        normalized(file, true)?;
        let relative = file.strip_prefix(stage).map_err(|_| {
            EffectError::new(
                "import_invalid_path",
                "Staged file escaped private staging root",
            )
        })?;
        normalized(relative, false)?;
        self.observe(stage)?;
        let mut cursor = stage.to_owned();
        for component in relative.parent().into_iter().flat_map(Path::components) {
            budget.check()?;
            cursor.push(component);
            self.observe(&cursor)?;
        }
        Ok(())
    }
}
fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EffectError::io(path, error)),
        Ok(_) => Err(EffectError::new(
            "import_destination_exists",
            format!("Import destination exists: {}", path.display()),
        )),
    }
}
fn hash_file(
    file: &mut File,
    path: &Path,
    expected: u64,
    budget: &mut Budget<'_>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; CHUNK];
    loop {
        budget.check()?;
        let count = file
            .read(&mut buffer)
            .map_err(|e| EffectError::io(path, e))?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > expected {
            return Err(EffectError::new(
                "import_source_changed",
                "Source grew beyond declared size",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    if bytes != expected {
        return Err(EffectError::new(
            "import_source_changed",
            "Source length changed",
        ));
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn verify_source(
    install: &MediaInstall,
    stage: &Path,
    parents: &mut Parents,
    budget: &mut Budget<'_>,
    digest: bool,
) -> Result<File> {
    parents.stage(stage, &install.staged_path, budget)?;
    let mut source = checked_file(&install.staged_path, &install.expected_source)?;
    if digest
        && hash_file(
            &mut source,
            &install.staged_path,
            install.expected_source.length,
            budget,
        )? != install.sha256
    {
        return Err(EffectError::new(
            "import_source_hash",
            "Staged media SHA256 differs from prepared plan",
        ));
    }
    if file_identity(
        &source
            .metadata()
            .map_err(|e| EffectError::io(&install.staged_path, e))?,
    )? != install.expected_source
        || path_identity(&install.staged_path)? != install.expected_source
    {
        return Err(EffectError::new(
            "import_source_changed",
            "Staged media changed during verification",
        ));
    }
    Ok(source)
}

#[derive(Clone)]
struct OwnedFile {
    path: PathBuf,
    expected: FileIdentity,
    sha256: String,
}
#[derive(Default)]
struct Journal {
    files: Vec<OwnedFile>,
    directories: Vec<(PathBuf, DirectoryIdentity)>,
    installed: Vec<usize>,
}
fn create_directory(
    path: &Path,
    parents: &mut Parents,
    journal: &mut Journal,
    budget: &mut Budget<'_>,
) -> Result<()> {
    budget.check()?;
    parents.absolute(
        path.parent()
            .ok_or_else(|| EffectError::new("import_invalid_path", "Directory without parent"))?,
        false,
        budget,
    )?;
    if parents.0.contains_key(path) {
        return parents.observe(path);
    }
    if journal.directories.len() >= budget.config.max_entries || parents.0.len() >= 32768 {
        return Err(EffectError::new(
            "import_effect_limit",
            "Created directory/identity guard limit",
        ));
    }
    fs::create_dir(path).map_err(|e| EffectError::io(path, e))?;
    let identity = directory_identity(path)?;
    journal.directories.push((path.into(), identity.clone()));
    parents.0.insert(path.into(), identity);
    Ok(())
}
fn create_below(
    root: &Path,
    relative: &Path,
    parents: &mut Parents,
    journal: &mut Journal,
    budget: &mut Budget<'_>,
) -> Result<()> {
    create_directory(root, parents, journal, budget)?;
    let mut cursor = root.to_owned();
    for component in relative.components() {
        cursor.push(component.as_os_str());
        create_directory(&cursor, parents, journal, budget)?;
    }
    Ok(())
}

/// Synchronous: call only inside the trusted ordered library mutation owner.
pub fn apply(trusted_imports_root: &Path, plan: &MediaCommitPlan) -> Result<CommitReceipt> {
    apply_with_config(trusted_imports_root, plan, EffectConfig::default())
}
pub fn apply_with_config(
    trusted_imports_root: &Path,
    plan: &MediaCommitPlan,
    config: EffectConfig,
) -> Result<CommitReceipt> {
    execute(trusted_imports_root, plan, config, |_, _, _| Ok(()))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checkpoint {
    AfterPreflight,
    CopyChunk,
    BeforePersist,
    AfterInstall,
}

fn execute(
    root: &Path,
    plan: &MediaCommitPlan,
    config: EffectConfig,
    mut checkpoint: impl FnMut(Checkpoint, usize, &Path) -> Result<()>,
) -> Result<CommitReceipt> {
    if config.max_bytes > MAX_BYTES
        || config.max_entries == 0
        || config.max_entries > 8192
        || config.max_operations == 0
        || config.max_operations > 1_000_000
        || config.timeout.is_zero()
        || config.timeout > Duration::from_secs(120)
    {
        return Err(EffectError::new(
            "import_effect_configuration",
            "Invalid effect resource limits",
        ));
    }
    normalized(root, true)?;
    if root.as_os_str() != plan.imports_root.as_os_str() {
        return Err(EffectError::new(
            "import_root_mismatch",
            "Prepared import root differs from trusted library root",
        ));
    }
    let count = plan
        .directories
        .len()
        .saturating_add(plan.installs.len())
        .saturating_add(plan.existing.len())
        .saturating_add(plan.unresolved.len());
    if count > config.max_entries {
        return Err(EffectError::new(
            "import_effect_limit",
            "Import plan entry limit",
        ));
    }
    let mut budget = Budget {
        plan,
        config,
        started: Instant::now(),
        operations: 0,
    };
    budget.check()?;
    let mut parents = Parents::default();
    parents.absolute(root, true, &mut budget)?;
    // Only the trusted root itself may be absent outside its confined subtree.
    parents.absolute(
        root.parent()
            .ok_or_else(|| EffectError::new("import_invalid_path", "Import root has no parent"))?,
        false,
        &mut budget,
    )?;
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for relative in &plan.directories {
        destination(relative.as_path())?;
        parents.absolute(&root.join(relative.as_path()), true, &mut budget)?;
    }
    for install in &plan.installs {
        budget.check()?;
        destination(install.destination.as_path())?;
        if !seen.insert(install.destination.as_path().to_owned()) {
            return Err(EffectError::new(
                "import_invalid_plan",
                "Duplicate install destination",
            ));
        }
        if install.sha256.len() != 64
            || !install
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(EffectError::new(
                "import_invalid_plan",
                "Invalid prepared SHA256",
            ));
        }
        total = total
            .checked_add(install.expected_source.length)
            .ok_or_else(|| EffectError::new("import_effect_limit", "Install byte overflow"))?;
        if total > budget.config.max_bytes {
            return Err(EffectError::new(
                "import_effect_limit",
                "Aggregate installed media exceeds byte limit",
            ));
        }
        let target = root.join(install.destination.as_path());
        parents.absolute(target.parent().unwrap(), true, &mut budget)?;
        absent(&target)?;
        let stage = plan.staging_directory().ok_or_else(|| {
            EffectError::new(
                "import_invalid_plan",
                "Install plan has no private staging directory",
            )
        })?;
        verify_source(install, stage, &mut parents, &mut budget, true)?;
    }
    validate_existing(root, plan, &mut parents, &mut budget)?;
    for relative in &plan.unresolved {
        destination(relative.as_path())?;
    }
    checkpoint(Checkpoint::AfterPreflight, 0, root)?;
    let mut journal = Journal {
        files: Vec::with_capacity(plan.installs.len() * 2),
        directories: vec![],
        installed: Vec::with_capacity(plan.installs.len()),
    };
    let result = (|| {
        budget.check()?;
        // Plans without directory/install requests have no directory effects.
        for relative in &plan.directories {
            create_below(
                root,
                relative.as_path(),
                &mut parents,
                &mut journal,
                &mut budget,
            )?;
        }
        for (index, install) in plan.installs.iter().enumerate() {
            let target = root.join(install.destination.as_path());
            create_below(
                root,
                install.destination.as_path().parent().unwrap(),
                &mut parents,
                &mut journal,
                &mut budget,
            )?;
            install_one(
                index,
                install,
                &target,
                plan,
                &mut parents,
                &mut journal,
                &mut budget,
                &mut checkpoint,
            )?;
        }
        validate_existing(root, plan, &mut parents, &mut budget)?;
        for relative in &plan.directories {
            parents.absolute(&root.join(relative.as_path()), false, &mut budget)?;
        }
        for index in &journal.installed {
            let owned = &journal.files[*index];
            parents.absolute(owned.path.parent().unwrap(), false, &mut budget)?;
            let mut file = checked_file(&owned.path, &owned.expected)
                .map_err(|error| EffectError::new("import_installed_changed", error.to_string()))?;
            if hash_file(&mut file, &owned.path, owned.expected.length, &mut budget)?
                != owned.sha256
                || path_identity(&owned.path)? != owned.expected
            {
                return Err(EffectError::new(
                    "import_installed_changed",
                    "Installed file changed before completion",
                ));
            }
        }
        budget.check()?;
        Ok(CommitReceipt {
            installed: plan
                .installs
                .iter()
                .map(|install| InstalledMedia {
                    destination: install.destination.as_path().into(),
                    bytes: install.expected_source.length,
                    sha256: install.sha256.clone(),
                })
                .collect(),
            created_directories: journal
                .directories
                .iter()
                .map(|(path, _)| path.strip_prefix(root).unwrap().to_owned())
                .collect(),
            existing_media: plan.existing.len(),
            unresolved_media: plan.unresolved.len(),
        })
    })();
    match result {
        Ok(receipt) => Ok(receipt),
        Err(mut error) => {
            rollback(&journal, &parents, &mut error, budget.config.timeout);
            Err(error)
        }
    }
}
fn validate_existing(
    root: &Path,
    plan: &MediaCommitPlan,
    parents: &mut Parents,
    budget: &mut Budget<'_>,
) -> Result<()> {
    for guard in &plan.existing {
        budget.check()?;
        destination(guard.destination.as_path())?;
        let path = root.join(guard.destination.as_path());
        parents.absolute(path.parent().unwrap(), false, budget)?;
        checked_file(&path, &guard.expected)
            .map_err(|error| EffectError::new("import_existing_changed", error.to_string()))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // One transaction's bounded install state.
fn install_one(
    index: usize,
    install: &MediaInstall,
    target: &Path,
    plan: &MediaCommitPlan,
    parents: &mut Parents,
    journal: &mut Journal,
    budget: &mut Budget<'_>,
    checkpoint: &mut impl FnMut(Checkpoint, usize, &Path) -> Result<()>,
) -> Result<()> {
    budget.check()?;
    parents.absolute(target.parent().unwrap(), false, budget)?;
    absent(target)?;
    let mut source = verify_source(
        install,
        plan.staging_directory().unwrap(),
        parents,
        budget,
        false,
    )?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".freeshow-import-")
        .tempfile_in(target.parent().unwrap())
        .map_err(|e| EffectError::io(target, e))?;
    let temporary_index = journal.files.len();
    let mut hasher = Sha256::new();
    let initial_identity = file_identity(
        &temporary
            .as_file()
            .metadata()
            .map_err(|e| EffectError::io(temporary.path(), e))?,
    )?;
    temporary.disable_cleanup(true);
    journal.files.push(OwnedFile {
        path: temporary.path().into(),
        expected: initial_identity,
        sha256: format!("{:x}", hasher.clone().finalize()),
    });
    let mut bytes = 0u64;
    let mut buffer = [0; CHUNK];
    loop {
        budget.check()?;
        let count = source
            .read(&mut buffer)
            .map_err(|e| EffectError::io(&install.staged_path, e))?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > install.expected_source.length {
            return Err(EffectError::new(
                "import_source_changed",
                "Staged file grew while installing",
            ));
        }
        let mut offset = 0;
        while offset < count {
            budget.check()?;
            let written = temporary
                .write(&buffer[offset..count])
                .map_err(|e| EffectError::io(temporary.path(), e))?;
            if written == 0 {
                return Err(EffectError::new(
                    "import_effect_io",
                    "Zero-length media write",
                ));
            }
            hasher.update(&buffer[offset..offset + written]);
            offset += written;
            journal.files[temporary_index].expected = file_identity(
                &temporary
                    .as_file()
                    .metadata()
                    .map_err(|e| EffectError::io(temporary.path(), e))?,
            )?;
            journal.files[temporary_index].sha256 = format!("{:x}", hasher.clone().finalize());
            checkpoint(Checkpoint::CopyChunk, index, target)?;
        }
    }
    if bytes != install.expected_source.length
        || format!("{:x}", hasher.finalize()) != install.sha256
    {
        return Err(EffectError::new(
            "import_source_hash",
            "Copied media differs from prepared length/SHA256",
        ));
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| EffectError::io(temporary.path(), e))?;
    // Capture handle identity before persist; never establish ownership from a
    // destination lookup that could already refer to a concurrent replacement.
    let expected = file_identity(
        &temporary
            .as_file()
            .metadata()
            .map_err(|e| EffectError::io(temporary.path(), e))?,
    )?;
    checkpoint(Checkpoint::BeforePersist, index, target)?;
    budget.check()?;
    parents.absolute(target.parent().unwrap(), false, budget)?;
    verify_source(
        install,
        plan.staging_directory().unwrap(),
        parents,
        budget,
        false,
    )?;
    absent(target)?;
    if path_identity(temporary.path())? != expected {
        return Err(EffectError::new(
            "import_source_changed",
            "Installation temporary changed before persist",
        ));
    }
    temporary
        .persist_noclobber(target)
        .map_err(|e| EffectError::io(target, e.error))?;
    journal.files.push(OwnedFile {
        path: target.into(),
        expected,
        sha256: install.sha256.clone(),
    });
    journal.installed.push(journal.files.len() - 1);
    // tempfile's hard-link fallback can leave the old temporary name behind.
    remove_owned_file(
        &journal.files[temporary_index],
        parents,
        &mut CleanupBudget::new(budget.config.timeout),
    )?;
    #[cfg(unix)]
    File::open(target.parent().unwrap())
        .and_then(|file| file.sync_all())
        .map_err(|e| EffectError::io(target.parent().unwrap(), e))?;
    checkpoint(Checkpoint::AfterInstall, index, target)?;
    budget.check()?;
    Ok(())
}

struct CleanupBudget {
    started: Instant,
    timeout: Duration,
    operations: usize,
    bytes: u64,
}
impl CleanupBudget {
    fn new(timeout: Duration) -> Self {
        Self {
            started: Instant::now(),
            timeout,
            operations: 0,
            bytes: 0,
        }
    }
    fn check(&mut self, bytes: usize) -> Result<()> {
        self.operations += 1;
        self.bytes += bytes as u64;
        if self.started.elapsed() >= self.timeout
            || self.operations > 1_000_000
            || self.bytes > 2 * MAX_BYTES + 8192 * CHUNK as u64
        {
            return Err(EffectError::new(
                "import_rollback_limit",
                "Cleanup work/deadline limit; remaining paths preserved",
            ));
        }
        Ok(())
    }
}
fn unchanged_parents(path: &Path, parents: &Parents, budget: &mut CleanupBudget) -> Result<()> {
    for ancestor in path.parent().into_iter().flat_map(Path::ancestors) {
        budget.check(0)?;
        let expected = parents
            .0
            .get(ancestor)
            .ok_or_else(|| EffectError::new("import_parent_changed", "Unknown rollback parent"))?;
        if &directory_identity(ancestor)? != expected {
            return Err(EffectError::new(
                "import_parent_changed",
                format!("Rollback parent changed: {}", ancestor.display()),
            ));
        }
    }
    Ok(())
}
fn remove_owned_file(
    owned: &OwnedFile,
    parents: &Parents,
    budget: &mut CleanupBudget,
) -> Result<()> {
    budget.check(0)?;
    unchanged_parents(&owned.path, parents, budget)?;
    match fs::symlink_metadata(&owned.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(EffectError::io(&owned.path, error)),
        Ok(_) => (),
    }
    let mut file = checked_file(&owned.path, &owned.expected)?;
    let mut hasher = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0; CHUNK];
    loop {
        budget.check(0)?;
        let count = file
            .read(&mut buffer)
            .map_err(|e| EffectError::io(&owned.path, e))?;
        if count == 0 {
            break;
        }
        budget.check(count)?;
        read += count as u64;
        if read > owned.expected.length {
            return Err(EffectError::new(
                "import_rollback_changed",
                "Owned file grew; preserved",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    if read != owned.expected.length
        || format!("{:x}", hasher.finalize()) != owned.sha256
        || path_identity(&owned.path)? != owned.expected
    {
        return Err(EffectError::new(
            "import_rollback_changed",
            "Owned file changed; preserved",
        ));
    }
    drop(file);
    fs::remove_file(&owned.path).map_err(|e| EffectError::io(&owned.path, e))
}
fn rollback(journal: &Journal, parents: &Parents, error: &mut EffectError, timeout: Duration) {
    // Cancellation cannot skip cleanup; it has a separate finite work/deadline budget.
    let mut budget = CleanupBudget::new(timeout);
    for file in journal.files.iter().rev() {
        if let Err(failure) = remove_owned_file(file, parents, &mut budget) {
            error.rollback_failure(&file.path, &failure);
        }
    }
    for (path, expected) in journal.directories.iter().rev() {
        let remove = (|| {
            unchanged_parents(path, parents, &mut budget)?;
            if &directory_identity(path)? != expected {
                return Err(EffectError::new(
                    "import_rollback_changed",
                    "Created directory replaced; preserved",
                ));
            }
            #[cfg(not(unix))]
            if expected.created_nanos.is_none() {
                return Err(EffectError::new(
                    "import_rollback_changed",
                    "No stable directory metadata; preserved",
                ));
            }
            fs::remove_dir(path).map_err(|e| EffectError::io(path, e))
        })();
        if let Err(failure) = remove {
            error.rollback_failure(path, &failure);
        }
    }
}

#[cfg(test)]
mod tests;
