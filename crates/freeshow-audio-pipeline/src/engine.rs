use super::*;
use std::time::Instant;
pub(super) struct Budget<'a> {
    config: &'a PipelineConfig,
    cancel: &'a CancellationToken,
    shutdown: &'a CancellationToken,
    started: Instant,
    operations: usize,
}
impl<'a> Budget<'a> {
    pub(super) fn new(
        config: &'a PipelineConfig,
        cancel: &'a CancellationToken,
        shutdown: &'a CancellationToken,
    ) -> Self {
        Self {
            config,
            cancel,
            shutdown,
            started: Instant::now(),
            operations: 0,
        }
    }
    fn check(&mut self) -> Result<()> {
        if self.shutdown.is_cancelled() {
            return Err(Error::shutdown());
        }
        if self.cancel.is_cancelled() {
            return Err(Error::cancelled());
        }
        if self.started.elapsed() >= self.config.cpu_timeout {
            return Err(Error::new(
                "audio_pipeline_timeout",
                "Audio CPU deadline exceeded",
            ));
        }
        self.operations += 1;
        if self.operations > self.config.operations {
            return Err(Error::resource("Audio CPU operation limit"));
        }
        Ok(())
    }
}
// Avoid x+0.5: that rounds the representable value immediately below0.5 up
// before floor. ECMAScript rounds halfway toward positive infinity.
fn js_round(x: f64) -> f64 {
    let lower = x.floor();
    if x - lower < 0.5 {
        lower
    } else {
        lower + 1.0
    }
}
fn convert(input: &[u8], budget: &mut Budget<'_>) -> Result<Vec<u8>> {
    budget.check()?;
    if input.len() > budget.config.input_bytes {
        return Err(Error::resource("Planar input limit"));
    }
    let count = input.len() / 8;
    let length = count
        .checked_mul(4)
        .ok_or_else(|| Error::resource("PCM size"))?;
    if length > budget.config.buffer_bytes {
        return Err(Error::resource("PCM output limit"));
    }
    let mut out = vec![0; length];
    for i in 0..count {
        budget.check()?;
        for channel in 0..2 {
            let offset = (channel * count + i) * 4;
            let f = f32::from_le_bytes(input[offset..offset + 4].try_into().unwrap()) as f64;
            let s = if f.is_finite() {
                f.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            let sample = js_round(s * if s < 0.0 { 32768.0 } else { 32767.0 }) as i16;
            out[i * 4 + channel * 2..i * 4 + channel * 2 + 2]
                .copy_from_slice(&sample.to_le_bytes());
        }
    }
    Ok(out)
}
pub fn convert_stereo(
    input: &[u8],
    config: &PipelineConfig,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    config.validate()?;
    convert(
        input,
        &mut Budget::new(config, cancel, &CancellationToken::new()),
    )
}
#[derive(Clone, Default, Debug)]
pub struct StereoResampler {
    state: Option<ResampleState>,
}
#[derive(Clone, Debug)]
struct ResampleState {
    prev_l: i16,
    prev_r: i16,
    phase: f64,
    rate: f64,
}
impl StereoResampler {
    fn capacity(&self, input: usize, rate: f64, limit: usize) -> Result<usize> {
        if rate == 48000.0 || input == 0 {
            return Ok(input);
        }
        let frames = input / 4;
        if frames == 0 {
            return Ok(0);
        }
        let ratio = rate / 48000.0;
        if ratio <= 0.0 || !ratio.is_finite() {
            return Ok(input);
        }
        if !(8000.0..=192000.0).contains(&rate) {
            return Err(Error::new(
                "unsupported_sample_rate",
                "Positive resampling rate outside verified8k–192k scope",
            ));
        }
        let phase = self
            .state
            .as_ref()
            .filter(|s| s.rate == rate)
            .map_or(0.0, |s| s.phase);
        let estimate = (((frames as f64 - phase) / ratio).ceil() + 2.0) * 4.0;
        if !estimate.is_finite() || estimate < 0.0 || estimate > limit as f64 {
            return Err(Error::resource("Resample expansion limit"));
        }
        Ok(estimate as usize)
    }
    pub fn state_bits(&self) -> Option<(i16, i16, u64, u64)> {
        self.state
            .as_ref()
            .map(|s| (s.prev_l, s.prev_r, s.phase.to_bits(), s.rate.to_bits()))
    }
    pub fn process(
        &mut self,
        input: &[u8],
        in_rate: f64,
        config: &PipelineConfig,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        config.validate()?;
        if input.len() > config.buffer_bytes {
            return Err(Error::resource("Resampler input"));
        }
        let mut next = self.clone();
        let out = next.resample(
            input,
            in_rate,
            &mut Budget::new(config, cancel, &CancellationToken::new()),
        )?;
        *self = next;
        Ok(out)
    }
    fn resample(&mut self, input: &[u8], rate: f64, budget: &mut Budget<'_>) -> Result<Vec<u8>> {
        budget.check()?;
        if rate == 48000.0 || input.is_empty() {
            return Ok(input.into());
        }
        if self.state.as_ref().is_none_or(|s| s.rate != rate) {
            self.state = Some(ResampleState {
                prev_l: 0,
                prev_r: 0,
                phase: 0.0,
                rate,
            });
        }
        let frames = input.len() / 4;
        if frames == 0 {
            return Ok(vec![]);
        }
        let ratio = rate / 48000.0;
        if ratio <= 0.0 || !ratio.is_finite() {
            return Ok(input.into());
        }
        if !(8000.0..=192000.0).contains(&rate) {
            return Err(Error::new(
                "unsupported_sample_rate",
                "Positive resampling rate outside verified8k–192k scope",
            ));
        }
        let capacity = self.capacity(input.len(), rate, budget.config.buffer_bytes)?;
        let state = self.state.as_mut().unwrap();
        let mut phase = state.phase;
        let mut out = Vec::with_capacity(capacity);
        let sample = |frame: usize, channel: usize| {
            i16::from_le_bytes(
                input[frame * 4 + channel * 2..frame * 4 + channel * 2 + 2]
                    .try_into()
                    .unwrap(),
            )
        };
        while phase < frames as f64 {
            budget.check()?;
            let index = phase.floor() as usize;
            let frac = phase - index as f64;
            for channel in 0..2 {
                let (a, b) = if index == 0 && phase < 1.0 {
                    (
                        if channel == 0 {
                            state.prev_l
                        } else {
                            state.prev_r
                        },
                        sample(0, channel),
                    )
                } else {
                    (
                        sample(index, channel),
                        sample((index + 1).min(frames - 1), channel),
                    )
                };
                let v = js_round(a as f64 + (b as f64 - a as f64) * frac).clamp(-32768.0, 32767.0)
                    as i16;
                out.extend_from_slice(&v.to_le_bytes());
            }
            if out.len() > budget.config.buffer_bytes {
                return Err(Error::resource("Actual resample output limit"));
            }
            phase += ratio;
        }
        state.prev_l = sample(frames - 1, 0);
        state.prev_r = sample(frames - 1, 1);
        state.phase = phase - frames as f64;
        Ok(out)
    }
}
#[derive(Clone, Default)]
pub(super) struct State {
    pub(super) latest: Option<Arc<Value>>,
    pub(super) resampler: StereoResampler,
    pub(super) accumulator: Arc<[u8]>,
}
pub(super) struct Transition {
    pub(super) next: State,
    pub(super) pre: Vec<SinkIntent>,
    pub(super) legacy: Vec<SinkIntent>,
    pub(super) frames: Vec<OpusPcmFrame>,
    pub(super) config: Option<Arc<Value>>,
    pub(super) combined: Arc<[u8]>,
}
fn intent(kind: SinkKind, buffer: Arc<[u8]>, rate: f64, target: Option<String>) -> SinkIntent {
    SinkIntent {
        kind,
        buffer: AudioBytes::whole(buffer),
        sample_rate: rate,
        target,
    }
}
pub(super) fn prepare(
    bytes: Arc<[u8]>,
    meta: &AudioMetadata,
    sinks: &SinkSnapshot,
    opus: bool,
    state: &State,
    budget: &mut Budget<'_>,
) -> Result<Transition> {
    budget.check()?;
    let mut next = state.clone();
    let mut out = Transition {
        next: next.clone(),
        pre: vec![],
        legacy: vec![],
        frames: vec![],
        config: None,
        combined: Arc::from([]),
    };
    if bytes.is_empty() {
        return Ok(out);
    }
    if meta.icecast.as_ref().is_some_and(js::truthy) {
        next.latest = meta.icecast.as_ref().map(|v| Arc::new(v.clone()));
    }
    let raw_rate = js::number(&meta.sample_rate)?;
    let rate = if raw_rate == 0.0 || raw_rate.is_nan() {
        48000.0
    } else {
        raw_rate
    };
    let target = if js::input_truthy(&meta.target_id) {
        Some(js::string(&meta.target_id)?)
    } else {
        None
    };
    if target.as_ref().is_some_and(|s| s.len() > 1024) {
        return Err(Error::resource("Target ID limit"));
    }
    let broadcast = target.as_ref().is_none_or(String::is_empty);
    if broadcast || target.as_ref().is_some_and(|t| sinks.ndi_keys.contains(t)) {
        out.pre.push(intent(
            if broadcast {
                SinkKind::NdiBroadcast
            } else {
                SinkKind::NdiTarget
            },
            bytes.clone(),
            rate,
            if broadcast { None } else { target.clone() },
        ));
    }
    if broadcast || target.as_ref().is_some_and(|t| sinks.omt_keys.contains(t)) {
        out.pre
            .push(intent(SinkKind::OmtBroadcast, bytes.clone(), rate, None));
    }
    let normalized = next
        .latest
        .as_ref()
        .filter(|v| js::truthy(v) && v.get("enabled") != Some(&Value::Bool(false)))
        .cloned();
    let icecast =
        opus && normalized.is_some() && (broadcast || target.as_deref() == Some("icecast"));
    let legacy = (broadcast || target.as_deref() != Some("icecast"))
        && (!sinks.blackmagic_keys.is_empty()
            || sinks.output_stream_audio
            || sinks.webrtc
            || sinks.rtmp);
    if !icecast && !legacy {
        out.next = next;
        return Ok(out);
    }
    let pcm_length = bytes.len() / 8 * 4;
    if bytes
        .len()
        .saturating_add(pcm_length)
        .saturating_add(state.accumulator.len())
        > budget.config.buffer_bytes
    {
        return Err(Error::resource("Input/PCM/retained accumulator aggregate"));
    }
    if icecast && rate != 48000.0 {
        // Expansion is checked against the aggregate before the resampler's
        // allocation. Count its data twice for temporary Vec→Arc/concat work.
        let capacity = next
            .resampler
            .capacity(pcm_length, rate, budget.config.buffer_bytes)?;
        if bytes
            .len()
            .saturating_add(pcm_length)
            .saturating_add(state.accumulator.len())
            .saturating_add(capacity.saturating_mul(2))
            > budget.config.buffer_bytes
        {
            return Err(Error::resource(
                "Prepared resample aggregate before allocation",
            ));
        }
    }
    let pcm: Arc<[u8]> = convert(&bytes, budget)?.into();
    let mut total = bytes.len() + pcm.len();
    if icecast {
        let resampled: Arc<[u8]> = if rate == 48000.0 {
            pcm.clone()
        } else {
            next.resampler.resample(&pcm, rate, budget)?.into()
        };
        if !Arc::ptr_eq(&resampled, &pcm) {
            total = total.saturating_add(resampled.len());
        }
        let length = state
            .accumulator
            .len()
            .checked_add(resampled.len())
            .ok_or_else(|| Error::resource("Accumulator size overflow"))?;
        total = total.saturating_add(length);
        if total > budget.config.buffer_bytes
            || length / OPUS_PCM_FRAME_BYTES > budget.config.max_frames
        {
            return Err(Error::resource(
                "Accumulated PCM/frame/aggregate buffer limit",
            ));
        }
        let combined: Arc<[u8]> = if state.accumulator.is_empty() {
            resampled
        } else {
            let mut all = Vec::with_capacity(length);
            all.extend_from_slice(&state.accumulator);
            all.extend_from_slice(&resampled);
            all.into()
        };
        for index in 0..combined.len() / OPUS_PCM_FRAME_BYTES {
            budget.check()?;
            out.frames.push(OpusPcmFrame {
                buffer: AudioBytes {
                    data: combined.clone(),
                    start: index * OPUS_PCM_FRAME_BYTES,
                    end: (index + 1) * OPUS_PCM_FRAME_BYTES,
                },
            });
        }
        out.combined = combined;
        out.config = normalized;
    }
    if total > budget.config.buffer_bytes {
        return Err(Error::resource("Prepared buffers aggregate limit"));
    }
    if legacy {
        out.legacy.push(intent(
            SinkKind::BlackmagicBroadcast,
            pcm.clone(),
            rate,
            None,
        ));
        if sinks.output_stream_audio {
            out.legacy
                .push(intent(SinkKind::OutputServer, pcm.clone(), rate, None));
        }
        if sinks.webrtc {
            out.legacy
                .push(intent(SinkKind::WebRtc, pcm.clone(), rate, None));
        }
        if sinks.rtmp {
            out.legacy.push(intent(SinkKind::Rtmp, pcm, rate, target));
        }
    }
    out.next = next;
    budget.check()?;
    Ok(out)
}
pub(super) fn resolve(
    mut next: State,
    combined: &Arc<[u8]>,
    frames: usize,
    outcome: &EncodingOutcome,
    has_icecast: bool,
) -> Result<(State, Vec<Diagnostic>)> {
    let mut diagnostics = vec![];
    let consumed = match outcome {
        EncodingOutcome::AllSucceeded => frames,
        EncodingOutcome::Failed {
            index,
            stage,
            message,
        } => {
            if *index >= frames {
                return Err(Error::new(
                    "invalid_completion",
                    "Failure index is not a prepared frame",
                ));
            }
            diagnostics.push(Diagnostic {
                code: match stage {
                    FailureStage::Encode => "opus_encode_failed",
                    FailureStage::Send => "icecast_send_failed",
                },
                message: Error::new(
                    "source_log",
                    format!("Could not encode Opus for Icecast: {message}"),
                )
                .message,
            });
            index + 1
        }
    };
    if has_icecast {
        next.accumulator = Arc::from(&combined[consumed * OPUS_PCM_FRAME_BYTES..]);
    }
    Ok((next, diagnostics))
}
