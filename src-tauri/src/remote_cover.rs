//! A per-run native fixed-cover source. Only its actual run owns the emitter.
use crate::{
    now_playing_cover_file::{self as file, CoverFiles, Observation, Outcome, Validation},
    remote_cover_publications::{Publications, Receipt},
};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
};
use freeshow_remotes::{
    CoverControl, CoverErrorRendering, CoverFileStat, CoverFuture, CoverSource, CoverState,
    PreparedCover, RemoteError,
};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

type Sink = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
pub struct Source {
    context: RuntimeContext,
    library: LibraryService,
    files: CoverFiles,
    publications: Publications,
    sink: Sink,
    rendering: CoverErrorRendering,
}
impl Source {
    pub fn new(
        context: RuntimeContext,
        library: LibraryService,
        files: CoverFiles,
        publications: Publications,
        sink: Sink,
        rendering: CoverErrorRendering,
    ) -> Self {
        Self {
            context,
            library,
            files,
            publications,
            sink,
            rendering,
        }
    }
}
fn error(code: &'static str, message: impl ToString) -> RemoteError {
    let mut message = message.to_string();
    let mut end = message.len().min(4096);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    RemoteError { code, message }
}
fn file_error(failure: file::Failure) -> RemoteError {
    error(failure.code(), failure.message)
}
fn context(library: &LibraryService, expected: &RuntimeContext) -> Result<(), RemoteError> {
    if &library.context().map_err(|e| error("cover_context", e))? != expected {
        return Err(error(
            "cover_context",
            "Fixed cover belongs to an obsolete library",
        ));
    }
    Ok(())
}
fn request(context: &RuntimeContext) -> Request {
    Request {
        channel: "MAIN".into(),
        data: serde_json::Value::Null,
        listener_id: None,
        context: Some(context.clone()),
    }
}
fn control(control: &CoverControl) -> file::Control {
    file::Control {
        cancel: control.cancel.clone(),
        deadline: control.deadline,
    }
}
enum NativeObservation {
    Plain(Observation),
    Published(Receipt),
}
impl NativeObservation {
    fn observation(&self) -> &Observation {
        match self {
            Self::Plain(value) => value,
            Self::Published(value) => value.observation(),
        }
    }
}
struct Prepared {
    // Dispose actual validation/file guards before the publication observation.
    validation: Option<Validation>,
    outcome: Outcome,
    observation: NativeObservation,
    state: CoverState,
    projection_failure: Option<RemoteError>,
    library: LibraryService,
    context: RuntimeContext,
    control: CoverControl,
    delivery_deadline: Instant,
}
impl CoverSource for Source {
    fn error_rendering(&self) -> CoverErrorRendering {
        self.rendering
    }
    fn prepare(&self, input: CoverControl) -> CoverFuture<'_, Box<dyn PreparedCover>> {
        Box::pin(async move {
            context(&self.library, &self.context)?;
            let phase = self
                .library
                .begin_phase(request(&self.context), "main")
                .await
                .map_err(|e| error("cover_phase", e))?;
            let result = match self.files.submit(&phase, control(&input)) {
                Ok(ticket) => ticket.finish().await.map_err(file_error),
                Err(failure) => Err(file_error(failure)),
            };
            // Always settle the entered phase, including failed submission/work.
            let fenced = phase.finish().await.map_err(|e| error("cover_phase", e));
            let prepared = result?;
            fenced?;
            let (observation, outcome) = prepared.into_parts();
            for diagnostic in observation.diagnostics() {
                eprintln!(
                    "Fixed cover preparation {} (OS {:?}): {}",
                    diagnostic.code(),
                    diagnostic.os_code,
                    diagnostic.message
                );
            }
            if let Some(failure) = observation.folder_failure() {
                let diagnostic = failure.diagnostic();
                eprintln!(
                    "Fixed Audio mkdir {} (OS {:?})",
                    diagnostic.code(),
                    diagnostic.os_code
                );
            }
            // Representation failure cannot erase an earlier genuine mkdir
            // observation. Retain it in the holder through publication/finish.
            let projected = match &outcome {
                Outcome::Missing => Ok(CoverState::Missing),
                Outcome::File(file) if file.context() != &self.context => {
                    Err(error("cover_context", "Held cover context mismatch"))
                }
                Outcome::File(file) => {
                    let stat = file.stat();
                    CoverFileStat::from_posix(
                        stat.length(),
                        stat.modified_seconds(),
                        stat.modified_nanoseconds(),
                    )
                    .map(CoverState::File)
                    .map_err(|e| error(e.code, e.message))
                }
                Outcome::Failed(_) => Ok(CoverState::Failed),
            };
            let (state, projection_failure) = match projected {
                Ok(state) => (state, None),
                Err(failure) => (CoverState::Failed, Some(failure)),
            };
            let delivery_deadline = Instant::now() + DELIVERY_TIMEOUT;
            let observation = if observation.folder_failure().is_some() {
                let receipt = self
                    .publications
                    .publish(
                        observation,
                        &CancellationToken::new(),
                        delivery_deadline,
                        |delivery| {
                            // The publication owner holds the process-wide ordered lane
                            // through this synchronous trusted main-window enqueue.
                            if context(&self.library, &self.context).is_err() {
                                return false;
                            }
                            let mut event = delivery.event.clone();
                            let Ok(private) = serde_json::to_value(&delivery.metadata) else {
                                return false;
                            };
                            event.native_cover = Some(private);
                            (self.sink)(event).is_ok()
                        },
                    )
                    .map_err(|e| error(e.code, e.message))?;
                NativeObservation::Published(receipt)
            } else {
                NativeObservation::Plain(observation)
            };
            Ok(Box::new(Prepared {
                validation: None,
                outcome,
                observation,
                state,
                projection_failure,
                library: self.library.clone(),
                context: self.context.clone(),
                control: input,
                delivery_deadline,
            }) as Box<dyn PreparedCover>)
        })
    }
}
impl PreparedCover for Prepared {
    fn state(&self) -> CoverState {
        self.state
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        match &self.outcome {
            Outcome::File(file) => file.read_at(offset, buffer),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "No prepared cover body",
            )),
        }
    }
    fn before_headers(&mut self) -> CoverFuture<'_, ()> {
        Box::pin(async move {
            context(&self.library, &self.context)?;
            if let Some(failure) = &self.projection_failure {
                return Err(failure.clone());
            }
            if let Outcome::Failed(failure) = &self.outcome {
                return Err(file_error(failure.clone()));
            }
            let phase = self
                .library
                .begin_phase(request(&self.context), "main")
                .await
                .map_err(|e| error("cover_phase", e))?;
            let ticket = match &self.outcome {
                Outcome::File(file) => file.revalidate(&phase, control(&self.control)),
                _ => self
                    .observation
                    .observation()
                    .revalidate(&phase, control(&self.control)),
            };
            let result = match ticket {
                Ok(ticket) => ticket.finish().await.map_err(file_error),
                Err(failure) => Err(file_error(failure)),
            };
            let fenced = phase.finish().await.map_err(|e| error("cover_phase", e));
            let validation = result?;
            fenced?;
            if validation.context() != &self.context {
                return Err(error("cover_context", "Cover validation context mismatch"));
            }
            validation.stat().map_err(|e| file_error(e.clone()))?;
            context(&self.library, &self.context)?;
            self.validation = Some(validation);
            Ok(())
        })
    }
    fn finish(self: Box<Self>) -> CoverFuture<'static, ()> {
        Box::pin(async move {
            if let NativeObservation::Published(receipt) = &self.observation {
                // Normal HTTP-body disposal cancels its token. Private delivery
                // belongs to the independent native registry/context lifecycle.
                if let Err(failure) = receipt
                    .observe(&CancellationToken::new(), self.delivery_deadline)
                    .await
                {
                    eprintln!(
                        "Fixed cover notification {}: {}",
                        failure.code, failure.message
                    );
                }
            }
            // Unknown UI delivery remains bounded in the process registry.
            // This only proves actual holder/FD disposal, never queued UI drain.
            drop(self);
            Ok(())
        })
    }
}
