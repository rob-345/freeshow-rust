//! Bounded audio conversion, global ordered resampling/accumulation and sealed
//! sink-call preparation. No encoder, device, network, port or delivery backend.
mod engine;
mod js;
mod service;
pub use engine::{convert_stereo, StereoResampler};
use serde::Serialize;
use serde_json::Value;
pub use service::{AcceptedAudio, AudioPipelineService, PreparedAudio};
use std::{fmt, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
pub const OPUS_PCM_FRAME_BYTES: usize = 3840;
pub const CPU_WORKERS: usize = 1;
#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub queued_jobs: usize,
    pub retained_plans: usize,
    pub input_bytes: usize,
    pub buffer_bytes: usize,
    pub max_frames: usize,
    pub metadata_bytes: usize,
    pub operations: usize,
    pub cpu_timeout: Duration,
    pub prepared_ttl: Duration,
}
impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            queued_jobs: 4,
            retained_plans: 4,
            input_bytes: 128 * 1024,
            buffer_bytes: 512 * 1024,
            max_frames: 64,
            metadata_bytes: 8192,
            operations: 100000,
            cpu_timeout: Duration::from_millis(250),
            prepared_ttl: Duration::from_secs(2),
        }
    }
}
impl PipelineConfig {
    fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.queued_jobs)
            || !(1..=8).contains(&self.retained_plans)
            || !(1..=512 * 1024).contains(&self.input_bytes)
            || !(1..=2 * 1024 * 1024).contains(&self.buffer_bytes)
            || self.buffer_bytes < self.input_bytes
            || !(1..=256).contains(&self.max_frames)
            || !(1..=32768).contains(&self.metadata_bytes)
            || !(1..=1_000_000).contains(&self.operations)
            || self.cpu_timeout.is_zero()
            || self.cpu_timeout > Duration::from_secs(2)
            || self.prepared_ttl < self.cpu_timeout
            || self.prepared_ttl > Duration::from_secs(10)
        {
            return Err(Error::new(
                "invalid_configuration",
                "Invalid audio pipeline limits",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextId(pub u64, pub u64);
/// Actual source coercions need IEEE nonfinite numbers which JSON cannot hold.
#[derive(Clone, Debug, Default)]
pub enum JsInput {
    #[default]
    Undefined,
    Json(Value),
    Number(f64),
}
#[derive(Clone, Debug, Default)]
pub struct AudioMetadata {
    pub sample_rate: JsInput,
    pub target_id: JsInput,
    pub icecast: Option<Value>,
}
/// Trusted source registration/activity snapshot, not proof of hardware or
/// network availability. NDI/OMT calls may themselves no-op downstream.
#[derive(Clone, Debug)]
pub struct SinkSnapshot {
    context: ContextId,
    ndi_keys: Vec<String>,
    omt_keys: Vec<String>,
    blackmagic_keys: Vec<String>,
    output_stream_audio: bool,
    webrtc: bool,
    rtmp: bool,
}
impl SinkSnapshot {
    pub fn new(
        context: ContextId,
        ndi_keys: Vec<String>,
        omt_keys: Vec<String>,
        blackmagic_keys: Vec<String>,
        output_stream_audio: bool,
        webrtc: bool,
        rtmp: bool,
    ) -> Result<Self> {
        let mut bytes = 0;
        for keys in [&ndi_keys, &omt_keys, &blackmagic_keys] {
            if keys.len() > 64 {
                return Err(Error::resource("Sink key count"));
            }
            for key in keys {
                bytes += key.len();
                if key.len() > 1024 {
                    return Err(Error::resource("Sink key length"));
                }
            }
        }
        if bytes > 8192 {
            return Err(Error::resource("Sink keys aggregate"));
        }
        Ok(Self {
            context,
            ndi_keys,
            omt_keys,
            blackmagic_keys,
            output_stream_audio,
            webrtc,
            rtmp,
        })
    }
    pub fn context(&self) -> ContextId {
        self.context
    }
    pub fn ndi_keys(&self) -> &[String] {
        &self.ndi_keys
    }
    pub fn omt_keys(&self) -> &[String] {
        &self.omt_keys
    }
    pub fn blackmagic_keys(&self) -> &[String] {
        &self.blackmagic_keys
    }
    pub fn output_stream_audio(&self) -> bool {
        self.output_stream_audio
    }
    pub fn webrtc_running(&self) -> bool {
        self.webrtc
    }
    pub fn rtmp_running(&self) -> bool {
        self.rtmp
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SinkKind {
    NdiBroadcast,
    NdiTarget,
    OmtBroadcast,
    BlackmagicBroadcast,
    OutputServer,
    WebRtc,
    Rtmp,
}
/// Immutable shared bytes. Views never copy a frame or detach a backing buffer.
pub struct AudioBytes {
    data: Arc<[u8]>,
    start: usize,
    end: usize,
}
impl AudioBytes {
    fn whole(data: Arc<[u8]>) -> Self {
        let end = data.len();
        Self {
            data,
            start: 0,
            end,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data[self.start..self.end]
    }
}
pub struct SinkIntent {
    kind: SinkKind,
    buffer: AudioBytes,
    sample_rate: f64,
    target: Option<String>,
}
impl SinkIntent {
    pub fn kind(&self) -> SinkKind {
        self.kind
    }
    pub fn buffer(&self) -> &[u8] {
        self.buffer.bytes()
    }
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
    pub fn channel_count(&self) -> usize {
        2
    }
    pub fn target_id(&self) -> Option<&str> {
        self.target.as_deref()
    }
}
pub struct OpusPcmFrame {
    buffer: AudioBytes,
}
impl OpusPcmFrame {
    pub fn bytes(&self) -> &[u8] {
        self.buffer.bytes()
    }
    pub fn sample_rate(&self) -> usize {
        48000
    }
    pub fn channel_count(&self) -> usize {
        2
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FailureStage {
    Encode,
    Send,
}
#[derive(Clone, Debug)]
pub enum EncodingOutcome {
    AllSucceeded,
    Failed {
        index: usize,
        stage: FailureStage,
        message: String,
    },
}
#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
}
/// Host-private result; no original successful AUDIO/MAIN response is implied.
pub struct FinishedAudio {
    payload: Arc<service::Payload>,
    diagnostics: Vec<Diagnostic>,
    #[cfg(any(test, feature = "test-harness"))]
    pub(crate) state: engine::State,
}
impl FinishedAudio {
    pub fn sinks(&self) -> &SinkSnapshot {
        &self.payload.sinks
    }
    pub fn legacy_intents(&self) -> &[SinkIntent] {
        &self.payload.legacy
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn context(&self) -> ContextId {
        self.payload.context
    }
    #[cfg(any(test, feature = "test-harness"))]
    pub fn debug_accumulator(&self) -> &[u8] {
        &self.state.accumulator
    }
    #[cfg(any(test, feature = "test-harness"))]
    pub fn debug_resampler_bits(&self) -> Option<(i16, i16, u64, u64)> {
        self.state.resampler.state_bits()
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 2048 {
            let mut n = 2048;
            while !message.is_char_boundary(n) {
                n -= 1;
            }
            message.truncate(n);
        }
        Self { code, message }
    }
    fn resource(message: impl Into<String>) -> Self {
        Self::new("audio_pipeline_resource_limit", message)
    }
    fn cancelled() -> Self {
        Self::new("cancelled", "Audio preparation cancelled")
    }
    fn shutdown() -> Self {
        Self::new("audio_pipeline_shutdown", "Audio pipeline is shutting down")
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[cfg(test)]
mod tests;
