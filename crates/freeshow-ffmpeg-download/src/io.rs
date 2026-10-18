use crate::*;
use std::{
    fs::{self, Metadata, OpenOptions},
    net::{SocketAddr, ToSocketAddrs},
    time::SystemTime,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit};
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    length: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change: (i64, i64),
}
impl Identity {
    fn new(meta: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            length: meta.len(),
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            #[cfg(unix)]
            device: meta.dev(),
            #[cfg(unix)]
            inode: meta.ino(),
            #[cfg(unix)]
            change: (meta.ctime(), meta.ctime_nsec()),
        }
    }
    fn same_object(&self, meta: &Metadata) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.device == meta.dev() && self.inode == meta.ino()
        }
        #[cfg(not(unix))]
        {
            self.created.is_some() && self.created == meta.created().ok()
        }
    }
}
#[derive(Default)]
pub(crate) struct Artifact {
    root: Option<Identity>,
    directory: Option<Identity>,
    file: Option<Identity>,
    finished: Option<Identity>,
}
pub(crate) enum Work {
    Resolve {
        host: String,
        port: u16,
        control: Control,
        reply: oneshot::Sender<Result<Vec<SocketAddr>>>,
    },
    Authorize {
        root: PathBuf,
        artifact: Arc<Mutex<Artifact>>,
        control: Control,
        reply: oneshot::Sender<Result<()>>,
    },
    Archive {
        job: service::ArchiveJob,
        source: tokio::sync::mpsc::Receiver<Vec<u8>>,
        reply: oneshot::Sender<Result<TransferStats>>,
    },
    Cleanup {
        path: PathBuf,
        artifact: Artifact,
        root: PathBuf,
        shared: Arc<service::Shared>,
        permit: OwnedSemaphorePermit,
        completion: tokio::sync::watch::Sender<Option<Result<()>>>,
    },
}
pub(crate) fn run(work: Work) {
    match work {
        Work::Resolve {
            host,
            port,
            control,
            reply,
        } => {
            let result = (|| {
                control.check()?;
                let mut addresses = Vec::new();
                for address in (host.as_str(), port)
                    .to_socket_addrs()
                    .map_err(|_| Error::source("fetch failed"))?
                {
                    control.check()?;
                    if addresses.len() == 16 {
                        return Err(Error::limit());
                    }
                    addresses.push(address);
                }
                if addresses.is_empty() {
                    return Err(Error::source("fetch failed"));
                }
                Ok(addresses)
            })();
            let _ = reply.send(result);
        }
        Work::Authorize {
            root,
            artifact,
            control,
            reply,
        } => {
            let result = (|| {
                control.check()?;
                let meta = directory(&root)?;
                locked(&artifact).root = Some(Identity::new(&meta));
                control.check()
            })();
            let _ = reply.send(result);
        }
        Work::Archive { job, source, reply } => {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| archive(&job, source)))
                    .unwrap_or_else(|_| {
                        Err(Error::new("download_worker", "Archive worker panicked"))
                    });
            let _ = reply.send(result);
        }
        Work::Cleanup {
            path,
            artifact,
            root,
            shared,
            permit,
            completion,
        } => {
            let result = cleanup(&path, &root, &artifact);
            if let Err(error) = &result {
                *locked(&shared.failure) = Some(error.clone());
                shared.cancel.cancel();
            }
            drop(permit);
            completion.send_replace(Some(result));
        }
    }
}
fn io_error(error: std::io::Error) -> Error {
    Error::new(
        "download_filesystem",
        format!(
            "Private download filesystem operation failed ({:?})",
            error.kind()
        ),
    )
}
fn directory(path: &Path) -> Result<Metadata> {
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::new(
            "download_identity",
            "Trusted download directory is not a regular directory",
        ));
    }
    Ok(meta)
}
fn check_directory(path: &Path, identity: &Option<Identity>) -> Result<()> {
    let meta = directory(path)?;
    if !identity.as_ref().is_some_and(|id| id.same_object(&meta)) {
        return Err(Error::new(
            "download_identity",
            "Private download directory identity changed",
        ));
    }
    Ok(())
}
fn archive(
    job: &service::ArchiveJob,
    source: tokio::sync::mpsc::Receiver<Vec<u8>>,
) -> Result<TransferStats> {
    job.control.check()?;
    check_directory(&job.root, &locked(&job.artifact).root)?;
    fs::create_dir(&job.private).map_err(io_error)?;
    locked(&job.artifact).directory = Some(Identity::new(&directory(&job.private)?));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&job.private, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    check_directory(&job.root, &locked(&job.artifact).root)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options.open(&job.staged).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(Error::new(
            "download_identity",
            "Private artifact is not regular",
        ));
    }
    locked(&job.artifact).file = Some(Identity::new(&meta));
    let stats = gzip::decode(source, &mut file, &job.control, &job.config)?;
    job.control.check()?;
    file.sync_all().map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    job.control.check()?;
    locked(&job.artifact).finished = Some(Identity::new(&meta));
    Ok(stats)
}
pub(crate) fn verify(job: &service::Job) -> Result<()> {
    let artifact = locked(&job.artifact);
    check_directory(&job.plan.root, &artifact.root)?;
    check_directory(&job.private, &artifact.directory)?;
    let meta = fs::symlink_metadata(&job.staged).map_err(io_error)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || artifact.finished.as_ref() != Some(&Identity::new(&meta))
    {
        return Err(Error::new(
            "download_identity",
            "Staged artifact changed before install",
        ));
    }
    Ok(())
}
fn cleanup(path: &Path, root: &Path, artifact: &Artifact) -> Result<()> {
    if artifact.directory.is_some() {
        check_directory(root, &artifact.root)?;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if artifact.directory.is_some() {
                return Err(Error::new(
                    "download_cleanup_identity",
                    "Private staging directory disappeared before cleanup",
                ));
            }
            return Ok(());
        }
        Err(error) => return Err(io_error(error)),
    };
    check_directory(root, &artifact.root)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !artifact
            .directory
            .as_ref()
            .is_some_and(|id| id.same_object(&metadata))
    {
        return Err(Error::new(
            "download_cleanup_identity",
            "Refusing cleanup of a changed private directory",
        ));
    }
    let file = path.join("artifact");
    match fs::symlink_metadata(&file) {
        Ok(meta) => {
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || !artifact
                    .file
                    .as_ref()
                    .is_some_and(|id| id.same_object(&meta))
            {
                return Err(Error::new(
                    "download_cleanup_identity",
                    "Refusing cleanup of a changed artifact",
                ));
            }
            fs::remove_file(file).map_err(io_error)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    // No recursive deletion: unknown contents cause an explicit cleanup fault.
    fs::remove_dir(path).map_err(io_error)
}
