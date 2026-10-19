//! Native-only preparation. The caller's fixed owner must retain this actual
//! future through accepted dependency work; dropping this helper is not a drain.
use crate::assets::NativeAssets;
use freeshow_assets::{Control, MediaAuthority};
use freeshow_audio::{AudioError, AudioService, FileObservations, FormatHint, NativeCommon};
use freeshow_core::{protocol::RuntimeContext, service::LibraryService, WireError};
use freeshow_cover_images::{
    Cancel, Context as CoverContext, CoverImageService, Picture, Reply, Session, TrustedMediaFile,
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Actual input/parser rejection, not a successful null metadata result.
    Input,
    Authorization,
    Context,
    Cancelled,
    Unsupported,
    Resource,
    Dependency,
}

#[derive(Debug)]
pub struct PrepareError {
    pub kind: FailureKind,
    pub code: String,
    pub message: String,
}
impl PrepareError {
    fn new(kind: FailureKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 2048 {
            let mut end = 2048;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self {
            kind,
            code: code.into(),
            message,
        }
    }
    fn wire(error: WireError) -> Self {
        let kind = classify(error.code);
        Self::new(kind, error.code, error.message)
    }
    fn asset(error: freeshow_assets::Error) -> Self {
        let kind = classify(&error.code);
        Self::new(kind, error.code, error.message)
    }
    fn audio(error: AudioError) -> Self {
        let kind = classify(error.code());
        Self::new(kind, error.code(), error.to_string())
    }
    fn cover(error: freeshow_cover_images::Error) -> Self {
        let kind = classify(error.code);
        Self::new(kind, error.code, error.message)
    }
}
impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for PrepareError {}

fn classify(code: &str) -> FailureKind {
    match code {
        "stale_library" | "assets_stale_context" | "cover_context" => FailureKind::Context,
        "assets_unregistered"
        | "assets_revoked"
        | "assets_changed"
        | "assets_protected"
        | "assets_permission"
        | "assets_missing"
        | "audio_input_changed"
        | "cover_authority" => FailureKind::Authorization,
        "cancelled" | "assets_cancelled" | "cover_cancelled" => FailureKind::Cancelled,
        "assets_unsupported" | "audio_unsupported_variant" | "cover_unsupported" => {
            FailureKind::Unsupported
        }
        "assets_limit"
        | "assets_busy"
        | "assets_deadline"
        | "audio_busy"
        | "audio_resource_limit"
        | "cover_resource_limit"
        | "cover_busy"
        | "cover_session_busy"
        | "cover_deadline"
        | "now_playing_deadline" => FailureKind::Resource,
        "audio_input_rejected" | "cover_invalid" | "cover_io" => FailureKind::Input,
        _ => FailureKind::Dependency,
    }
}

/// Native cancellation handle. `cancel()` synchronously reaches both dependency
/// mechanisms without a forwarding task. The deadline is cooperative, not a
/// preemption or a reason to abandon an accepted File operation.
#[derive(Clone)]
pub struct PreparationControl {
    deadline: Instant,
    cancel: CancellationToken,
    cover_cancel: Cancel,
}
impl Default for PreparationControl {
    fn default() -> Self {
        Self::new(Instant::now() + Duration::from_secs(30))
    }
}
impl PreparationControl {
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            cancel: CancellationToken::new(),
            cover_cancel: Cancel::default(),
        }
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
        self.cover_cancel.cancel();
    }
    fn check(&self) -> Result<(), PrepareError> {
        if self.cancel.is_cancelled() {
            return Err(PrepareError::new(
                FailureKind::Cancelled,
                "cancelled",
                "Native now-playing preparation canceled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(PrepareError::new(
                FailureKind::Resource,
                "now_playing_deadline",
                "Cooperative preparation deadline exceeded",
            ));
        }
        Ok(())
    }
    fn assets(&self) -> Control {
        Control::new(self.deadline, self.cancel.clone())
    }
}

/// No Deserialize or renderer constructor: only genuine complete dependency
/// results can produce this value. Retain it through later cover/image effects,
/// then drop it before shutting down assets or the cover service.
pub struct Prepared {
    captured: RuntimeContext,
    common: NativeCommon,
    all_pictures: Vec<Picture>,
    authority: MediaAuthority,
    session: Session,
    observations: FileObservations,
}
impl Prepared {
    pub fn context(&self) -> &RuntimeContext {
        &self.captured
    }
    pub fn common(&self) -> &NativeCommon {
        &self.common
    }
    pub fn all_pictures(&self) -> &[Picture] {
        &self.all_pictures
    }
    pub fn source_path(&self) -> &str {
        self.authority.source_path()
    }
    pub fn authority(&self) -> &MediaAuthority {
        &self.authority
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn audio_observations(&self) -> &FileObservations {
        &self.observations
    }
    /// Actual source/context checks again at a later native publication/effect
    /// boundary. Observations do not freeze bytes against subsequent mutation.
    pub async fn revalidate(
        &self,
        library: &LibraryService,
        control: &PreparationControl,
    ) -> Result<(), PrepareError> {
        validate(library, &self.captured, &self.authority, control).await
    }
    /// Consuming handoff to the next fixed native owner. The authority/session
    /// must remain alive with the common/pictures through all later effects.
    pub fn into_parts(
        self,
    ) -> (
        NativeCommon,
        Vec<Picture>,
        MediaAuthority,
        Session,
        RuntimeContext,
    ) {
        (
            self.common,
            self.all_pictures,
            self.authority,
            self.session,
            self.captured,
        )
    }
}

fn current(library: &LibraryService, captured: &RuntimeContext) -> Result<(), PrepareError> {
    if library
        .context()
        .map_err(WireError::from)
        .map_err(PrepareError::wire)?
        != *captured
    {
        return Err(PrepareError::new(
            FailureKind::Context,
            "stale_library",
            "Now-playing preparation belongs to an obsolete full native context",
        ));
    }
    Ok(())
}
async fn validate(
    library: &LibraryService,
    captured: &RuntimeContext,
    authority: &MediaAuthority,
    control: &PreparationControl,
) -> Result<(), PrepareError> {
    control.check()?;
    current(library, captured)?;
    if authority.context() != &crate::assets::context(captured).map_err(PrepareError::wire)? {
        return Err(PrepareError::new(
            FailureKind::Context,
            "assets_stale_context",
            "Transferred file authority differs from the captured full context",
        ));
    }
    // Worker checks against the initial identity/length/native times, canonical
    // path, protected roots and the still-current exact registry entry.
    authority
        .revalidate(control.assets())
        .await
        .map_err(PrepareError::asset)?;
    current(library, captured)?;
    control.check()
}

/// Borrow existing services. No replacement service, task, thread or IO timer is
/// created here. Four whole-operation futures/results must be bounded by the
/// caller, independently of these dependencies' smaller admission limits.
pub struct PreparationServices<'a> {
    library: &'a LibraryService,
    assets: &'a NativeAssets,
    audio: &'a AudioService,
    covers: &'a CoverImageService,
    #[cfg(test)]
    pub hook: Option<&'a TestHook>,
}
impl<'a> PreparationServices<'a> {
    pub fn new(
        library: &'a LibraryService,
        assets: &'a NativeAssets,
        audio: &'a AudioService,
        covers: &'a CoverImageService,
    ) -> Self {
        Self {
            library,
            assets,
            audio,
            covers,
            #[cfg(test)]
            hook: None,
        }
    }
    pub async fn prepare(
        &self,
        captured: &RuntimeContext,
        cover_context: &CoverContext,
        registered_source_key: &str,
        control: &PreparationControl,
    ) -> Result<Prepared, PrepareError> {
        control.check()?;
        current(self.library, captured)?;
        if captured.runtime_id != cover_context.runtime
            || captured.library_generation != cover_context.generation
            || captured.data_path.to_str() != Some(cover_context.data_path.as_str())
        {
            return Err(PrepareError::new(
                FailureKind::Context,
                "cover_context",
                "Cover run does not match the complete native library context",
            ));
        }
        // Preserve the caller's seal; reconstructing Context would create a
        // different opaque run. Session Drop closes admission and actual queued
        // cleanup even on an early failure below.
        let session = self
            .covers
            .begin_session(cover_context)
            .map_err(PrepareError::cover)?;
        let read = self
            .assets
            .open_registered_media(
                self.library,
                captured,
                registered_source_key,
                control.assets(),
            )
            .await
            .map_err(PrepareError::wire)?;
        let (file, authority) = read.into_parts();
        #[cfg(test)]
        self.pause(TestStage::Opened).await;
        validate(self.library, captured, &authority, control).await?;
        let ticket = self
            .audio
            .reserve_opened()
            .map_err(PrepareError::audio)?
            .submit(
                file,
                authority.source_path(),
                FormatHint::FromSourceName,
                control.cancel.clone(),
            )
            .map_err(PrepareError::audio)?;
        #[cfg(test)]
        self.pause(TestStage::AudioAccepted).await;
        let read = ticket.finish().await.map_err(PrepareError::audio)?;
        let (common, file, observations) = read.into_parts();
        #[cfg(test)]
        self.pause(TestStage::Common).await;
        // Authorization/context/cancellation wins over a parser-null policy;
        // never reinterpret stale or changed media as a harmless parse failure.
        validate(self.library, captured, &authority, control).await?;
        let common = common.map_err(PrepareError::audio)?;
        validate(self.library, captured, &authority, control).await?;
        let trusted =
            TrustedMediaFile::from_opened_host(cover_context, authority.canonical_path(), file)
                .map_err(PrepareError::cover)?;
        let ticket = self
            .covers
            .covers(&session, trusted, control.cover_cancel.clone())
            .map_err(PrepareError::cover)?;
        #[cfg(test)]
        self.pause(TestStage::CoverAccepted).await;
        let covers = ticket.finish().await.map_err(PrepareError::cover)?;
        #[cfg(test)]
        self.pause(TestStage::Covers).await;
        validate(self.library, captured, &authority, control).await?;
        // The extractor bounds eight pictures/4MiB aggregate data plus bounded
        // fields. Clone only this certified result, preserving source ordering
        // and own-undefined metadata. Drop the observation before later image
        // commands, which require this Session to be idle.
        let all_pictures = covers
            .inspect(|reply| match reply {
                Reply::Covers(pictures) => Some(pictures.clone()),
                _ => None,
            })
            .map_err(PrepareError::cover)?
            .ok_or_else(|| {
                PrepareError::new(
                    FailureKind::Dependency,
                    "now_playing_cover_reply",
                    "Cover owner returned a different operation result",
                )
            })?;
        drop(covers);
        #[cfg(test)]
        self.pause(TestStage::Ready).await;
        validate(self.library, captured, &authority, control).await?;
        Ok(Prepared {
            captured: captured.clone(),
            common,
            all_pictures,
            authority,
            session,
            observations,
        })
    }

    #[cfg(test)]
    async fn pause(&self, stage: TestStage) {
        if let Some(hook) = self.hook.filter(|hook| hook.stage == stage) {
            hook.entered.notify_one();
            hook.resume.notified().await;
        }
    }
}

// Deterministic integration seams between real worker reads, never fake parser
// success. Absent from production and do not add a task/worker.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestStage {
    Opened,
    AudioAccepted,
    Common,
    CoverAccepted,
    Covers,
    Ready,
}
#[cfg(test)]
pub struct TestHook {
    pub stage: TestStage,
    pub entered: tokio::sync::Notify,
    pub resume: tokio::sync::Notify,
}
