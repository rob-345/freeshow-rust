//! Pure source routing and trusted effect receipts. No codec/device is opened here.
use freeshow_midi::{MtcReceiver, Number, Status, StatusGate, TimecodeEffect};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
pub const COMMANDS: &[&str] = &[
    "TIMECODE_START",
    "TIMECODE_STOP",
    "TIMECODE_VALUE",
    "TIMECODE_STATUS",
    "TIMECODE_AUDIO_DATA",
];
pub const MAX_PENDING: usize = 8;
pub const MAX_LISTENERS: usize = 8;
pub const MAX_AUDIO_BYTES: usize = 65536;
pub const SAMPLE_RATE: u32 = 48000;
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn native(message: &str) -> Self {
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            code: "timecode_native",
            message: message[..end].into(),
        }
    }
    fn state(message: &str) -> Self {
        Self {
            code: "timecode_state",
            message: message.into(),
        }
    }
    fn resource() -> Self {
        Self {
            code: "timecode_resource_limit",
            message: "Timecode resource limit exceeded".into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Scalar {
    Undefined,
    Null,
    Bool(bool),
    Number(Number),
    Text(String),
}
impl Scalar {
    pub fn number(n: f64) -> Self {
        Self::Number(Number::new(n))
    }
    fn validate(&self) -> Result<(), Error> {
        if matches!(self,Self::Text(s) if s.len()>4096) {
            Err(Error::resource())
        } else {
            Ok(())
        }
    }
    fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => n.value() != 0.0 && !n.value().is_nan(),
            Self::Text(s) => !s.is_empty(),
        }
    }
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.value() == b.value(),
            _ => self == other,
        }
    }
    pub fn numeric(&self) -> f64 {
        match self {
            Self::Undefined => f64::NAN,
            Self::Null => 0.0,
            Self::Bool(b) => u8::from(*b).into(),
            Self::Number(n) => n.value(),
            Self::Text(s) => to_number(s),
        }
    }
}
fn to_number(text: &str) -> f64 {
    let s=text.trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
    if s.is_empty() {
        return 0.0;
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    for (p, r) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = s.strip_prefix(p) {
            if digits.is_empty() {
                return f64::NAN;
            }
            let mut n = 0.0;
            for c in digits.chars() {
                let Some(d) = c.to_digit(r) else {
                    return f64::NAN;
                };
                n = n * r as f64 + d as f64;
            }
            return n;
        }
    }
    if s.eq_ignore_ascii_case("nan") || s.to_ascii_lowercase().contains("inf") {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Data {
    Undefined,
    Json(Value),
}
impl Data {
    fn validate(&self) -> Result<(), Error> {
        if let Self::Json(v) = self {
            bounded_json(v)?;
        }
        Ok(())
    }
}
fn bounded_json(root: &Value) -> Result<(), Error> {
    let mut stack = vec![(root, 0)];
    let (mut nodes, mut bytes) = (0usize, 0usize);
    while let Some((v, d)) = stack.pop() {
        nodes += 1;
        if nodes > 1024 || d > 8 {
            return Err(Error::resource());
        }
        match v {
            Value::String(s) => bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                if a.len() > 1024 - nodes - stack.len() {
                    return Err(Error::resource());
                }
                stack.extend(a.iter().map(|v| (v, d + 1)));
            }
            Value::Object(o) => {
                if o.len() > 1024 - nodes - stack.len() {
                    return Err(Error::resource());
                }
                for (k, v) in o {
                    bytes = bytes.saturating_add(k.len());
                    stack.push((v, d + 1));
                }
            }
            _ => (),
        }
        if bytes > 65536 {
            return Err(Error::resource());
        }
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub enum Command {
    Start {
        kind: String,
        mode: String,
        framerate: Scalar,
        data: Data,
    },
    Stop,
    Value(Number),
    Status(Data),
    Audio {
        mode: String,
        bytes: Vec<u8>,
    },
}
#[derive(Clone, Debug)]
pub struct EffectToken {
    owner: Arc<()>,
    id: u64,
}
impl EffectToken {
    pub fn serial(&self) -> u64 {
        self.id
    }
}
impl Serialize for EffectToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.id)
    }
}
#[derive(Clone, Debug)]
pub struct ListenerToken {
    owner: Arc<()>,
    id: u64,
}
impl ListenerToken {
    pub fn serial(&self) -> u64 {
        self.id
    }
}
impl Serialize for ListenerToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.id)
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Effect {
    OpenMtc {
        framerate: Scalar,
        data: Data,
        listener: ListenerToken,
    },
    SendMtc {
        time: Number,
        framerate: Scalar,
        data: Data,
    },
    StopMtc,
    CreateDecoder {
        sample_rate: u32,
        framerate: Scalar,
        format: &'static str,
    },
    CreateEncoder {
        sample_rate: u32,
        framerate: Scalar,
        replacing: bool,
    },
    Decode {
        bytes: Vec<u8>,
    },
    Encode {
        frame: LtcFrame,
    },
    StopEncoder,
    CloseListener {
        listener: ListenerToken,
        mode: String,
    },
    Value {
        value: Number,
    },
    Status {
        status: Status,
    },
    Audio {
        bytes: Vec<u8>,
    },
    Alert {
        message: String,
    },
    LogStatus {
        data: Data,
    },
    Cancel {
        operation: EffectToken,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct Intent {
    pub token: Option<EffectToken>,
    pub effect: Effect,
}
#[derive(Debug, Default, Serialize)]
pub struct Step {
    pub effects: Vec<Intent>,
    pub errors: Vec<Error>,
}
impl Step {
    fn event(effect: Effect) -> Self {
        Self {
            effects: vec![Intent {
                token: None,
                effect,
            }],
            errors: vec![],
        }
    }
    fn error(e: Error) -> Self {
        Self {
            effects: vec![],
            errors: vec![e],
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct LtcFrame {
    pub hours: Number,
    pub minutes: Number,
    pub seconds: Number,
    pub frames: Number,
}
impl LtcFrame {
    pub fn new(h: f64, m: f64, s: f64, f: f64) -> Self {
        Self {
            hours: Number::new(h),
            minutes: Number::new(m),
            seconds: Number::new(s),
            frames: Number::new(f),
        }
    }
}
pub fn ltc_frame(time: f64, fps: f64) -> LtcFrame {
    LtcFrame::new(
        (time / 3600000.0 % 24.0).floor(),
        (time / 60000.0 % 60.0).floor(),
        (time / 1000.0 % 60.0).floor(),
        (time % 1000.0 / 1000.0 * fps).floor(),
    )
}
pub fn ltc_time(frame: &LtcFrame, fps: f64) -> Number {
    Number::new(
        (frame.hours.value() * 3600.0
            + frame.minutes.value() * 60.0
            + frame.seconds.value()
            + frame.frames.value() / fps)
            * 1000.0,
    )
}
/// Trusted native completion, supplied only after the real effect has settled.
pub enum Completion {
    Opened,
    Failed(Error),
    Done,
    Decoded(Option<LtcFrame>),
    Encoded(Vec<u8>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Accepted,
    Live,
    Closed,
}
struct Listener {
    token: ListenerToken,
    mode: &'static str,
    phase: Phase,
    mtc: MtcReceiver,
}
enum Pending {
    Open(ListenerToken),
    Send { accepted: bool },
    Close(ListenerToken),
    RetireEncoder,
    RetireMtc,
}
struct AsyncOp {
    token: EffectToken,
    kind: Pending,
}
enum SyncKind {
    Decoder {
        listener: ListenerToken,
        fps: Scalar,
    },
    Encoder {
        time: Number,
        fps: Scalar,
    },
    Encode,
    Decode,
    StopSender {
        receivers: Vec<ListenerToken>,
    },
    StopReceiver {
        receivers: Vec<ListenerToken>,
        index: usize,
    },
}
struct SyncOp {
    token: EffectToken,
    kind: SyncKind,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub framerate: Scalar,
    pub current_mode: Option<String>,
    pub send_data: Data,
    pub receivers: Vec<String>,
    pub owned_listeners: usize,
    pub pending: usize,
    pub decoder: bool,
    pub decoder_failed: bool,
    pub decoder_framerate: Scalar,
    pub encoder: bool,
    pub encoder_failed: bool,
    pub encoder_framerate: Option<Scalar>,
    pub last_time: Number,
    pub retired: bool,
    pub mtc_sender: bool,
}
pub struct Router {
    owner: Arc<()>,
    next: u64,
    windows: bool,
    retired: bool,
    mtc_sender: bool,
    framerate: Scalar,
    current_mode: Option<String>,
    send_data: Data,
    status: StatusGate,
    receivers: Vec<ListenerToken>,
    listeners: Vec<Listener>,
    pending: Vec<AsyncOp>,
    sync: Option<SyncOp>,
    decoder: bool,
    decoder_failed: bool,
    decoder_framerate: Scalar,
    encoder: bool,
    encoder_failed: bool,
    encoder_framerate: Option<Scalar>,
    last_time: Number,
}
impl Router {
    pub fn new(windows: bool) -> Self {
        Self {
            owner: Arc::new(()),
            next: 1,
            windows,
            retired: false,
            mtc_sender: false,
            framerate: Scalar::number(25.0),
            current_mode: None,
            send_data: Data::Undefined,
            status: StatusGate::default(),
            receivers: vec![],
            listeners: vec![],
            pending: vec![],
            sync: None,
            decoder: false,
            decoder_failed: false,
            decoder_framerate: Scalar::number(25.0),
            encoder: false,
            encoder_failed: false,
            encoder_framerate: None,
            last_time: Number::new(-1.0),
        }
    }
    fn token(&mut self) -> Result<EffectToken, Error> {
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or_else(Error::resource)?;
        Ok(EffectToken {
            owner: self.owner.clone(),
            id,
        })
    }
    fn valid(&self, t: &EffectToken) -> bool {
        Arc::ptr_eq(&self.owner, &t.owner)
    }
    fn listener(&self, t: &ListenerToken) -> Option<usize> {
        if !Arc::ptr_eq(&self.owner, &t.owner) {
            return None;
        }
        self.listeners.iter().position(|l| l.token.id == t.id)
    }
    fn emit_sync(&mut self, kind: SyncKind, effect: Effect) -> Result<Step, Error> {
        let token = self.token()?;
        self.sync = Some(SyncOp {
            token: token.clone(),
            kind,
        });
        Ok(Step {
            effects: vec![Intent {
                token: Some(token),
                effect,
            }],
            errors: vec![],
        })
    }
    fn emit_async(&mut self, kind: Pending, effect: Effect) -> Result<Step, Error> {
        if self.pending.len() >= MAX_PENDING {
            return Err(Error::resource());
        }
        let token = self.token()?;
        self.pending.push(AsyncOp {
            token: token.clone(),
            kind,
        });
        Ok(Step {
            effects: vec![Intent {
                token: Some(token),
                effect,
            }],
            errors: vec![],
        })
    }
    fn reserve(&mut self, mode: &'static str) -> Result<ListenerToken, Error> {
        if self.listeners.len() >= MAX_LISTENERS {
            return Err(Error::resource());
        }
        let token = self.token()?;
        let listener = ListenerToken {
            owner: token.owner,
            id: token.id,
        };
        self.listeners.push(Listener {
            token: listener.clone(),
            mode,
            phase: Phase::Pending,
            mtc: MtcReceiver::default(),
        });
        Ok(listener)
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            framerate: self.framerate.clone(),
            current_mode: self.current_mode.clone(),
            send_data: self.send_data.clone(),
            receivers: self
                .receivers
                .iter()
                .filter_map(|t| self.listener(t).map(|i| self.listeners[i].mode.into()))
                .collect(),
            owned_listeners: self
                .listeners
                .iter()
                .filter(|l| matches!(l.phase, Phase::Accepted | Phase::Live))
                .count(),
            pending: self.pending.len() + usize::from(self.sync.is_some()),
            decoder: self.decoder,
            decoder_failed: self.decoder_failed,
            decoder_framerate: self.decoder_framerate.clone(),
            encoder: self.encoder,
            encoder_failed: self.encoder_failed,
            encoder_framerate: self.encoder_framerate.clone(),
            last_time: self.last_time,
            retired: self.retired,
            mtc_sender: self.mtc_sender,
        }
    }
    pub fn begin(&mut self, command: Command) -> Result<Step, Error> {
        if self.retired {
            return Err(Error::state("Timecode router retired"));
        }
        if self.sync.is_some() {
            return Err(Error::state(
                "A synchronous source effect still owns its continuation",
            ));
        }
        match command {
            Command::Start {
                kind,
                mode,
                framerate,
                data,
            } => {
                if kind.len() > 32 || mode.len() > 4096 {
                    return Err(Error::resource());
                }
                framerate.validate()?;
                data.validate()?;
                if framerate.truthy() {
                    self.framerate = framerate;
                }
                if kind == "send" {
                    self.current_mode = Some(mode);
                    self.send_data = data;
                    return Ok(Step::default());
                }
                if kind != "receive"
                    || self.receivers.iter().any(|t| {
                        self.listener(t)
                            .is_some_and(|i| self.listeners[i].mode == mode)
                    })
                {
                    return Ok(Step::default());
                }
                match mode.as_str() {
                    "LTC" => {
                        if self.decoder_failed {
                            return Ok(Step::default());
                        }
                        let listener = self.reserve("LTC")?;
                        let fps = self.framerate.clone();
                        self.decoder_framerate = fps.clone();
                        self.emit_sync(
                            SyncKind::Decoder {
                                listener,
                                fps: fps.clone(),
                            },
                            Effect::CreateDecoder {
                                sample_rate: SAMPLE_RATE,
                                framerate: fps,
                                format: "u8",
                            },
                        )
                    }
                    "MTC" => {
                        if self.pending.len() >= MAX_PENDING {
                            return Err(Error::resource());
                        }
                        let listener = self.reserve("MTC")?;
                        self.emit_async(
                            Pending::Open(listener.clone()),
                            Effect::OpenMtc {
                                framerate: self.framerate.clone(),
                                data,
                                listener,
                            },
                        )
                    }
                    _ => Ok(Step::default()),
                }
            }
            Command::Value(time) => match self.current_mode.as_deref() {
                Some("LTC") => {
                    if self.encoder_failed {
                        return Ok(Step::default());
                    }
                    let fps = self.framerate.clone();
                    if !self.encoder
                        || self
                            .encoder_framerate
                            .as_ref()
                            .is_none_or(|old| !old.same(&fps))
                        || time.value() < self.last_time.value()
                    {
                        self.emit_sync(
                            SyncKind::Encoder {
                                time,
                                fps: fps.clone(),
                            },
                            Effect::CreateEncoder {
                                sample_rate: SAMPLE_RATE,
                                framerate: fps,
                                replacing: self.encoder,
                            },
                        )
                    } else {
                        self.encode(time)
                    }
                }
                Some("MTC") => self.emit_async(
                    Pending::Send { accepted: false },
                    Effect::SendMtc {
                        time,
                        framerate: self.framerate.clone(),
                        data: self.send_data.clone(),
                    },
                ),
                _ => Ok(Step::default()),
            },
            Command::Stop => {
                let receivers = self.receivers.clone();
                match self.current_mode.as_deref() {
                    Some("LTC") => {
                        self.emit_sync(SyncKind::StopSender { receivers }, Effect::StopEncoder)
                    }
                    Some("MTC") => {
                        self.emit_sync(SyncKind::StopSender { receivers }, Effect::StopMtc)
                    }
                    _ => self.next_stop(receivers, 0),
                }
            }
            Command::Status(data) => {
                data.validate()?;
                Ok(Step::event(Effect::LogStatus { data }))
            }
            Command::Audio { mode, bytes } => {
                if mode.len() > 4096 || bytes.len() > MAX_AUDIO_BYTES {
                    return Err(Error::resource());
                }
                if mode == "LTC" && self.decoder {
                    self.emit_sync(SyncKind::Decode, Effect::Decode { bytes })
                } else {
                    Ok(Step::default())
                }
            }
        }
    }
    fn encode(&mut self, time: Number) -> Result<Step, Error> {
        self.last_time = time;
        self.emit_sync(
            SyncKind::Encode,
            Effect::Encode {
                frame: ltc_frame(time.value(), self.framerate.numeric()),
            },
        )
    }
    fn register(&mut self, token: ListenerToken) {
        let mode = self.listeners[self.listener(&token).expect("owned listener")].mode;
        if let Some(i) = self.receivers.iter().position(|t| {
            self.listener(t)
                .is_some_and(|i| self.listeners[i].mode == mode)
        }) {
            self.receivers[i] = token;
        } else {
            self.receivers.push(token);
        }
    }
    fn next_stop(&mut self, receivers: Vec<ListenerToken>, index: usize) -> Result<Step, Error> {
        if let Some(listener) = receivers.get(index) {
            let mode = self
                .listener(listener)
                .map(|i| self.listeners[i].mode)
                .unwrap_or("MTC")
                .to_owned();
            self.emit_sync(
                SyncKind::StopReceiver {
                    receivers: receivers.clone(),
                    index,
                },
                Effect::CloseListener {
                    listener: listener.clone(),
                    mode,
                },
            )
        } else {
            self.receivers.clear();
            self.listeners.retain(|l| l.phase != Phase::Closed);
            Ok(Step::default())
        }
    }
    fn alert(&self, encoder: bool) -> String {
        let mut s = format!(
            "Could not initialize LTC {}!",
            if encoder { "Encoder" } else { "Decoder" }
        );
        if self.windows {
            s.push_str("\n\nThis is likely because the required Visual C++ Redistributable is not installed.\nPlease install it from https://aka.ms/vs/17/release/vc_redist.x64.exe");
        }
        s
    }
    /// A real MTC open/connect may accept a handle before its source promise resolves.
    pub fn accept_listener(&mut self, token: &EffectToken) -> Result<ListenerToken, Error> {
        if !self.valid(token) {
            return Err(Error::state("Foreign effect token"));
        }
        let listener = self
            .pending
            .iter()
            .find_map(|o| {
                if o.token.id == token.id {
                    if let Pending::Open(t) = &o.kind {
                        Some(t.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::state("No pending listener open"))?;
        let i = self
            .listener(&listener)
            .ok_or_else(|| Error::state("Listener reservation missing"))?;
        if self.listeners[i].phase != Phase::Pending {
            return Err(Error::state("Listener already accepted"));
        }
        self.listeners[i].phase = Phase::Accepted;
        Ok(listener)
    }
    /// Register real output ownership even when a later packet/open continuation
    /// fails. The host retains the actual binding until StopMtc settles.
    pub fn accept_sender(&mut self, token: &EffectToken) -> Result<(), Error> {
        if !self.valid(token) {
            return Err(Error::state("Foreign effect token"));
        }
        let operation = self
            .pending
            .iter_mut()
            .find(|op| op.token.id == token.id)
            .ok_or_else(|| Error::state("No pending MTC send"))?;
        if let Pending::Send { accepted } = &mut operation.kind {
            if *accepted {
                return Err(Error::state("Sender already accepted"));
            }
            *accepted = true;
            self.mtc_sender = true;
            Ok(())
        } else {
            Err(Error::state("Not an MTC send token"))
        }
    }
    pub fn mtc_message(
        &mut self,
        token: &ListenerToken,
        bytes: &[u8],
        now: f64,
    ) -> Result<Step, Error> {
        if self.retired {
            return Err(Error::state("Router retired"));
        }
        if self.sync.is_some() {
            return Err(Error::state(
                "A synchronous source continuation precedes this message",
            ));
        }
        let i = self
            .listener(token)
            .ok_or_else(|| Error::state("Unknown listener token"))?;
        if !matches!(self.listeners[i].phase, Phase::Accepted | Phase::Live)
            || self.listeners[i].mode != "MTC"
        {
            return Err(Error::state("Listener is not an accepted MTC input"));
        }
        let effects = self.listeners[i].mtc.feed(bytes, now).map_err(|e| Error {
            code: e.code,
            message: e.message,
        })?;
        let mut out = Step::default();
        for effect in effects {
            let part = match effect {
                TimecodeEffect::Frame(value) => self.process_frame(value),
                TimecodeEffect::Status(status) => self.update_status(status),
            };
            out.effects.extend(part.effects);
        }
        Ok(out)
    }
    pub fn process_frame(&self, value: Number) -> Step {
        if self.retired {
            Step::default()
        } else if self.sync.is_some() {
            Step::error(Error::state(
                "A synchronous source continuation precedes this frame",
            ))
        } else {
            Step::event(Effect::Value { value })
        }
    }
    pub fn update_status(&mut self, status: Status) -> Step {
        if self.retired {
            return Step::default();
        }
        if self.sync.is_some() {
            return Step::error(Error::state(
                "A synchronous source continuation precedes this status",
            ));
        }
        if self.status.update(status).is_some() {
            Step::event(Effect::Status { status })
        } else {
            Step::default()
        }
    }
    pub fn complete(&mut self, token: &EffectToken, outcome: Completion) -> Result<Step, Error> {
        if !self.valid(token) {
            return Err(Error::state("Foreign effect token"));
        }
        let outcome = match outcome {
            Completion::Failed(error) => Completion::Failed(Error {
                code: error.code,
                ..Error::native(&error.message)
            }),
            outcome => outcome,
        };
        if self.sync.as_ref().is_some_and(|s| s.token.id == token.id) {
            let valid = match (&self.sync.as_ref().unwrap().kind, &outcome) {
                (_, Completion::Failed(_)) => true,
                (SyncKind::Decoder { .. } | SyncKind::Encoder { .. }, Completion::Opened) => true,
                (SyncKind::Encode, Completion::Encoded(b)) => b.len() <= MAX_AUDIO_BYTES,
                (SyncKind::Decode, Completion::Decoded(_)) => true,
                (SyncKind::StopSender { .. } | SyncKind::StopReceiver { .. }, Completion::Done) => {
                    true
                }
                _ => false,
            };
            if !valid {
                return Err(Error::state("Wrong or oversized synchronous receipt"));
            }
            let operation = self.sync.take().unwrap();
            let mut result = self.complete_sync(operation.kind, outcome)?;
            if self.retired {
                let mut next = self.retire()?;
                result.effects.append(&mut next.effects);
                result.errors.append(&mut next.errors);
            }
            return Ok(result);
        }
        if self.sync.is_some() {
            return Err(Error::state(
                "A synchronous source continuation precedes this completion",
            ));
        }
        let i = self
            .pending
            .iter()
            .position(|o| o.token.id == token.id)
            .ok_or_else(|| Error::state("Effect token is stale or already consumed"))?;
        let valid = matches!(
            (&self.pending[i].kind, &outcome),
            (_, Completion::Failed(_))
                | (Pending::Open(_), Completion::Opened)
                | (
                    Pending::Send { .. }
                        | Pending::Close(_)
                        | Pending::RetireEncoder
                        | Pending::RetireMtc,
                    Completion::Done
                )
        );
        if !valid {
            return Err(Error::state("Wrong asynchronous receipt"));
        }
        let retry_later = matches!(
            (&self.pending[i].kind, &outcome),
            (
                Pending::Close(_) | Pending::RetireEncoder | Pending::RetireMtc,
                Completion::Failed(_)
            )
        );
        let op = self.pending.remove(i);
        let mut step = Step::default();
        match (op.kind, outcome) {
            (Pending::Open(listener), Completion::Opened) => {
                let i = self.listener(&listener).unwrap();
                self.listeners[i].phase = Phase::Live;
                if !self.retired {
                    self.register(listener);
                } else {
                    return self.close_async(listener);
                }
            }
            (Pending::Open(listener), Completion::Failed(error)) => {
                let i = self.listener(&listener).unwrap();
                if self.listeners[i].phase == Phase::Pending {
                    self.listeners.remove(i);
                }
                step.errors.push(error);
            }
            (Pending::Close(listener), Completion::Done) => {
                self.remove_listener(&listener);
            }
            (Pending::Send { .. }, Completion::Done) => {
                self.mtc_sender = true;
            }
            (Pending::Send { accepted: true }, Completion::Failed(error)) => {
                self.mtc_sender = true;
                step.errors.push(error);
            }
            (Pending::RetireMtc, Completion::Done) => {
                self.mtc_sender = false;
            }
            (Pending::RetireEncoder, Completion::Done) => {
                self.encoder = false;
                self.last_time = Number::new(-1.0);
            }
            (_, Completion::Failed(error)) => step.errors.push(error),
            _ => (),
        }
        if self.retired && !retry_later {
            let mut close = self.retire()?;
            step.effects.append(&mut close.effects);
            step.errors.append(&mut close.errors);
        }
        Ok(step)
    }
    fn complete_sync(&mut self, kind: SyncKind, outcome: Completion) -> Result<Step, Error> {
        match (kind, outcome) {
            (SyncKind::Decoder { listener, fps }, Completion::Opened) => {
                self.decoder = true;
                self.decoder_framerate = fps;
                let i = self.listener(&listener).unwrap();
                self.listeners[i].phase = Phase::Live;
                if !self.retired {
                    self.register(listener);
                }
                Ok(Step::default())
            }
            (SyncKind::Decoder { listener, .. }, Completion::Failed(error)) => {
                self.remove_listener(&listener);
                self.decoder_failed = true;
                let mut s = if self.retired {
                    Step::default()
                } else {
                    Step::event(Effect::Alert {
                        message: self.alert(false),
                    })
                };
                s.errors.push(error);
                Ok(s)
            }
            (SyncKind::Encoder { time, fps }, Completion::Opened) => {
                self.encoder = true;
                self.encoder_framerate = Some(fps);
                if self.retired {
                    Ok(Step::default())
                } else {
                    self.encode(time)
                }
            }
            (SyncKind::Encoder { .. }, Completion::Failed(error)) => {
                self.encoder_failed = true;
                let mut s = if self.retired {
                    Step::default()
                } else {
                    Step::event(Effect::Alert {
                        message: self.alert(true),
                    })
                };
                s.errors.push(error);
                Ok(s)
            }
            (SyncKind::Encode, Completion::Encoded(bytes)) => Ok(if self.retired {
                Step::default()
            } else {
                Step::event(Effect::Audio { bytes })
            }),
            (SyncKind::Decode, Completion::Decoded(frame)) => Ok(frame
                .map_or_else(Step::default, |frame| {
                    self.process_frame(ltc_time(&frame, self.decoder_framerate.numeric()))
                })),
            (SyncKind::StopSender { receivers }, Completion::Done) => {
                if self.current_mode.as_deref() == Some("MTC") {
                    self.mtc_sender = false;
                }
                if self.current_mode.as_deref() == Some("LTC") {
                    self.encoder = false;
                    self.last_time = Number::new(-1.0);
                }
                if self.retired {
                    Ok(Step::default())
                } else {
                    self.next_stop(receivers, 0)
                }
            }
            (SyncKind::StopReceiver { receivers, index }, Completion::Done) => {
                self.remove_listener(&receivers[index]);
                if self.retired {
                    Ok(Step::default())
                } else {
                    self.next_stop(receivers, index + 1)
                }
            }
            (_, Completion::Failed(error)) => Ok(Step::error(error)),
            _ => unreachable!("receipt validated before state consumption"),
        }
    }
    fn remove_listener(&mut self, t: &ListenerToken) {
        if let Some(i) = self.listener(t) {
            if self.listeners[i].mode == "LTC" {
                self.decoder = false;
            }
            if self.receivers.iter().any(|r| r.id == t.id) {
                self.listeners[i].phase = Phase::Closed;
            } else {
                self.listeners.remove(i);
            }
        }
    }
    fn close_async(&mut self, listener: ListenerToken) -> Result<Step, Error> {
        let mode = self
            .listener(&listener)
            .map(|i| self.listeners[i].mode)
            .ok_or_else(|| Error::state("Listener missing before retirement"))?
            .into();
        self.emit_async(
            Pending::Close(listener.clone()),
            Effect::CloseListener { listener, mode },
        )
    }
    /// Terminal native lifecycle, distinct from source's reopenable STOP. Retry
    /// failed closes; never discard accepted handles or outstanding IO tokens.
    pub fn retire(&mut self) -> Result<Step, Error> {
        self.retired = true;
        self.receivers.clear();
        self.listeners.retain(|l| l.phase != Phase::Closed);
        let mut out = Step::default();
        for op in &self.pending {
            if matches!(op.kind, Pending::Open(_) | Pending::Send { .. }) {
                out.effects.push(Intent {
                    token: None,
                    effect: Effect::Cancel {
                        operation: op.token.clone(),
                    },
                });
            }
        }
        if let Some(sync) = &self.sync {
            out.effects.push(Intent {
                token: None,
                effect: Effect::Cancel {
                    operation: sync.token.clone(),
                },
            });
            return Ok(out);
        }
        let close=self.listeners.iter().filter(|l|matches!(l.phase,Phase::Accepted|Phase::Live)&&!self.pending.iter().any(|p|matches!(&p.kind,Pending::Open(t)|Pending::Close(t) if t.id==l.token.id))).map(|l|l.token.clone()).collect::<Vec<_>>();
        for listener in close {
            if self.pending.len() == MAX_PENDING {
                break;
            }
            let mut next = self.close_async(listener)?;
            out.effects.append(&mut next.effects);
        }
        if self.pending.len() < MAX_PENDING
            && self.encoder
            && !self
                .pending
                .iter()
                .any(|p| matches!(p.kind, Pending::RetireEncoder))
        {
            let mut next = self.emit_async(Pending::RetireEncoder, Effect::StopEncoder)?;
            out.effects.append(&mut next.effects);
        }
        if self.pending.len() < MAX_PENDING
            && self.mtc_sender
            && !self
                .pending
                .iter()
                .any(|p| matches!(p.kind, Pending::RetireMtc))
        {
            let mut next = self.emit_async(Pending::RetireMtc, Effect::StopMtc)?;
            out.effects.append(&mut next.effects);
        }
        Ok(out)
    }
    pub fn drained(&self) -> bool {
        self.retired
            && self.sync.is_none()
            && self.pending.is_empty()
            && self.listeners.is_empty()
            && !self.encoder
            && !self.decoder
            && !self.mtc_sender
    }
}
#[cfg(test)]
mod tests;
