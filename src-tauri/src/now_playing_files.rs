//! Fixed now-playing leaves on the existing ordered library IO owner.
//! Native-only; no path, filename or callback is deserialized from a renderer.
use freeshow_core::{
    protocol::Request,
    service::{LibraryPhase, PhaseTicket},
    Error, Result,
};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::sync::oneshot;

pub const MAX_TEXT: usize = 1024 * 1024;
pub const MAX_COVER: usize = 4 * 1024 * 1024;
pub const MAX_COMPARE: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum Leaf {
    Text,
    Cover,
}
pub enum Intent {
    Paths,
    EnsureFolder,
    Exists(Leaf),
    DeleteCover,
    WriteText(String),
    WriteCover(Vec<u8>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPaths {
    pub folder: PathBuf,
    pub text: PathBuf,
    pub cover: PathBuf,
}
#[derive(Debug)]
pub struct Observation {
    pub paths: AudioPaths,
    /// The original helper's boolean. None for the path-only observation.
    pub success: Option<bool>,
    pub skipped_equal_text: bool,
    /// At most two caught filesystem failures, each at most1024 UTF8 bytes.
    pub diagnostics: Vec<String>,
}
/// Retains the real ordered ticket and its completed/uncollected result lease.
pub struct FileTicket {
    ordered: PhaseTicket,
    reply: oneshot::Receiver<Observation>,
}
impl FileTicket {
    pub async fn finish(self) -> Result<Observation> {
        // Observe the real context/IO/reply fence even if the private sender
        // already produced its small typed result. Never publish these as IPC.
        self.ordered.finish().await?;
        self.reply
            .await
            .map_err(|_| Error::WorkerUnavailable("now-playing file observation lost".into()))
    }
}
fn resource(message: &str) -> Error {
    Error::NativeOperation {
        code: "now_playing_file_resource",
        message: message.into(),
    }
}
fn diagnostic(errors: &mut Vec<String>, error: impl ToString) {
    if errors.len() == 2 {
        return;
    }
    let mut text = error.to_string();
    let mut end = text.len().min(1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    errors.push(text);
}
fn exists(path: &Path, errors: &mut Vec<String>) -> bool {
    match path.try_exists() {
        Ok(found) => found,
        Err(error) => {
            diagnostic(errors, error);
            false
        }
    }
}
fn source_read_text(path: &Path, errors: &mut Vec<String>) -> Result<String> {
    // Source readFile returns "" for directories and caught read errors. It
    // follows symlinks when reading, while its directory check is lstat.
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => return Ok(String::new()),
        Ok(_) => {}
        Err(error) => {
            diagnostic(errors, error);
            return Ok(String::new());
        }
    }
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.is_dir() {
            return Ok(String::new());
        }
        if !metadata.is_file() {
            return Err(resource("Nonregular text comparison is unsupported"));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Follow the source's ordinary symlinks, while a FIFO swap cannot make
        // open wait for a peer. The held descriptor is checked again below.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            diagnostic(errors, error);
            return Ok(String::new());
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            diagnostic(errors, error);
            return Ok(String::new());
        }
    };
    if metadata.is_dir() {
        return Ok(String::new());
    }
    if !metadata.is_file() {
        return Err(resource("Nonregular text comparison is unsupported"));
    }
    if metadata.len() > MAX_COMPARE as u64 {
        return Err(resource("Existing text exceeds bounded source comparison"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if let Err(error) = (&mut file)
        .take(MAX_COMPARE as u64 + 1)
        .read_to_end(&mut bytes)
    {
        diagnostic(errors, error);
        return Ok(String::new());
    }
    if bytes.len() > MAX_COMPARE {
        return Err(resource(
            "Existing text grew beyond bounded source comparison",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn source_write(path: &Path, bytes: &[u8], errors: &mut Vec<String>) -> Result<bool> {
    if let Ok(metadata) = fs::metadata(path) {
        // Directories keep the ordinary caught source write-error behavior.
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(resource("Nonregular now-playing output is unsupported"));
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            diagnostic(errors, error);
            return Ok(false);
        }
    };
    match file.metadata() {
        Ok(metadata) if !metadata.is_file() => {
            return Err(resource("Opened now-playing output is nonregular"))
        }
        Ok(_) => {}
        Err(error) => {
            diagnostic(errors, error);
            return Ok(false);
        }
    }
    match file.write_all(bytes) {
        Ok(()) => Ok(true),
        Err(error) => {
            diagnostic(errors, error);
            Ok(false)
        }
    }
}

pub fn submit(phase: &LibraryPhase, intent: Intent) -> Result<FileTicket> {
    match &intent {
        Intent::WriteText(text) if text.len() > MAX_TEXT => {
            return Err(resource("Text exceeds1MiB"))
        }
        Intent::WriteCover(bytes) if bytes.len() > MAX_COVER => {
            return Err(resource("Cover exceeds4MiB"))
        }
        _ => {}
    }
    let request = Request {
        channel: "MAIN".into(),
        data: serde_json::Value::Null,
        listener_id: None,
        context: Some(phase.context().clone()),
    };
    let (send, reply) = oneshot::channel();
    let ordered = phase.submit(request, move |runtime| {
        // Resolve on the actual ordered owner after its exact-context check.
        let folder = runtime.stores.data.join("Audio");
        let paths = AudioPaths {
            text: folder.join("NowPlaying.txt"),
            cover: folder.join("NowPlayingCover.png"),
            folder,
        };
        let mut observation = Observation {
            paths,
            success: None,
            skipped_equal_text: false,
            diagnostics: Vec::new(),
        };
        let paths = &observation.paths;
        let errors = &mut observation.diagnostics;
        observation.success = match intent {
            Intent::Paths => None,
            Intent::EnsureFolder => Some(match fs::create_dir_all(&paths.folder) {
                Ok(()) => true,
                Err(error) => {
                    diagnostic(errors, error);
                    false
                }
            }),
            Intent::Exists(leaf) => Some(exists(
                match leaf {
                    Leaf::Text => &paths.text,
                    Leaf::Cover => &paths.cover,
                },
                errors,
            )),
            Intent::DeleteCover => Some(match fs::remove_file(&paths.cover) {
                Ok(()) => true,
                Err(error) => {
                    diagnostic(errors, error);
                    false
                }
            }),
            Intent::WriteCover(bytes) => Some(source_write(&paths.cover, &bytes, errors)?),
            Intent::WriteText(text) => {
                if exists(&paths.text, errors) && source_read_text(&paths.text, errors)? == text {
                    observation.skipped_equal_text = true;
                    Some(false)
                } else {
                    Some(source_write(&paths.text, text.as_bytes(), errors)?)
                }
            }
        };
        let _ = send.send(observation);
        Ok(Vec::new())
    })?;
    Ok(FileTicket { ordered, reply })
}
