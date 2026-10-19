//! Bounded source nowPlaying.ts decisions. No filesystem, codec, task or clock.
use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
pub const MAX_ACTIVE: usize = 4;
pub const MAX_TEXT: usize = 1024 * 1024;
pub const MAX_COVER: usize = 4 * 1024 * 1024;
pub const IMAGE_BACKEND_IMPLEMENTED: bool = false;
pub const COMMANDS: &[&str] = &["NOW_PLAYING", "NOW_PLAYING_UNSET", "OPEN_NOW_PLAYING"];
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn native(code: &'static str, text: &str) -> Self {
        let mut n = text.len().min(1024);
        while !text.is_char_boundary(n) {
            n -= 1
        }
        Self {
            code,
            message: text[..n].into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
fn error(code: &'static str) -> Error {
    Error::native(code, code)
}
#[derive(Clone)]
pub struct Context {
    runtime: String,
    generation: u64,
    data_path: String,
    run: Arc<()>,
}
impl Context {
    pub fn from_host(runtime: &str, generation: u64, data_path: &str) -> Result<Self> {
        if runtime.is_empty()
            || runtime.contains('\0')
            || data_path.is_empty()
            || runtime.len() > 256
            || data_path.len() > 32768
            || data_path.contains('\0')
        {
            return Err(error("now_playing_context"));
        }
        Ok(Self {
            runtime: runtime.into(),
            generation,
            data_path: data_path.into(),
            run: Arc::new(()),
        })
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run)
            && self.runtime == other.runtime
            && self.generation == other.generation
            && self.data_path == other.data_path
    }
}
#[derive(Clone, Copy)]
pub enum Platform {
    Posix,
    Windows,
}
pub struct TrustedPaths {
    context: Context,
    folder: String,
    text: String,
    cover: String,
}
impl TrustedPaths {
    pub fn from_host(context: &Context, platform: Platform, audio_folder: &str) -> Result<Self> {
        bounded_path(audio_folder)?;
        let sep = match platform {
            Platform::Posix => '/',
            Platform::Windows => '\\',
        };
        let valid = match platform {
            Platform::Posix => audio_folder.starts_with('/'),
            Platform::Windows => {
                audio_folder.starts_with(['\\', '/'])
                    || (audio_folder.as_bytes().get(1) == Some(&b':')
                        && audio_folder
                            .as_bytes()
                            .get(2)
                            .is_some_and(|b| matches!(b, b'/' | b'\\')))
            }
        };
        if !valid {
            return Err(error("now_playing_path"));
        }
        let folder = normalize_folder(audio_folder, platform)?;
        let join = |name: &str| {
            if folder.ends_with(sep) {
                format!("{folder}{name}")
            } else {
                format!("{folder}{sep}{name}")
            }
        };
        Ok(Self {
            context: context.clone(),
            text: join("NowPlaying.txt"),
            cover: join("NowPlayingCover.png"),
            folder,
        })
    }
}
fn normalize_folder(input: &str, platform: Platform) -> Result<String> {
    let windows = matches!(platform, Platform::Windows);
    let input = if windows {
        input.replace('/', "\\")
    } else {
        input.into()
    };
    let sep = if windows { '\\' } else { '/' };
    let (prefix, rest) = if windows && input.starts_with("\\\\") {
        let parts = input[2..]
            .split(sep)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if parts.len() < 2 {
            return Err(error("now_playing_path"));
        }
        (
            format!("\\\\{}\\{}\\", parts[0], parts[1]),
            parts[2..].join("\\"),
        )
    } else if windows && input.as_bytes().get(1) == Some(&b':') {
        (input[..3].into(), input[3..].into())
    } else {
        (sep.to_string(), input.trim_start_matches(sep).into())
    };
    let mut parts = vec![];
    for part in rest.split(sep) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    Ok(format!("{prefix}{}", parts.join(&sep.to_string())))
}
pub struct TrustedMediaPath {
    context: Context,
    path: String,
}
impl TrustedMediaPath {
    /// Host must authorize provenance; this constructor is not renderer IPC.
    pub fn from_host(context: &Context, path: &str) -> Result<Self> {
        bounded_path(path)?;
        Ok(Self {
            context: context.clone(),
            path: path.into(),
        })
    }
}
fn bounded_path(path: &str) -> Result<()> {
    if path.len() > 32768 || path.contains('\0') {
        Err(error("now_playing_path"))
    } else {
        Ok(())
    }
}
#[derive(Clone)]
enum Scalar {
    Undefined,
    Json(Value),
    Number(f64),
}
impl Scalar {
    fn from(v: Option<&Value>) -> Self {
        v.map_or(Self::Undefined, |v| Self::Json(v.clone()))
    }
    fn truthy(&self) -> bool {
        match self {
            Self::Undefined => false,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::Json(v) => truthy(v),
        }
    }
    fn string(&self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Number(n) => num(*n),
            Self::Json(v) => string(v),
        }
    }
    fn number(&self) -> f64 {
        match self {
            Self::Undefined => f64::NAN,
            Self::Number(n) => *n,
            Self::Json(v) => match v {
                Value::Null => 0.0,
                Value::Bool(b) => {
                    if *b {
                        1.0
                    } else {
                        0.0
                    }
                }
                Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                _ => {
                    let s = string(v);
                    let s = s.trim_matches(space);
                    js_number(s)
                }
            },
        }
    }
}
pub struct SourceData {
    payload: Value,
    duration: Scalar,
}
impl SourceData {
    pub fn from_payload(payload: Value) -> Result<Self> {
        bounded_json(&payload, 65536)?;
        if !payload.is_object() {
            return Err(error("now_playing_payload"));
        }
        if !payload.get("filePath").is_some_and(Value::is_string) {
            return Err(error("now_playing_payload"));
        }
        Ok(Self {
            duration: Scalar::from(payload.get("duration")),
            payload,
        })
    }
    /// Native structured-clone number receipt, not a tagged renderer JSON API.
    pub fn set_duration_number(&mut self, value: f64) {
        self.duration = Scalar::Number(value)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Picture {
    pub fields: Value,
    pub bytes: Vec<u8>,
}
impl Picture {
    pub fn from_host(fields: Value, bytes: Vec<u8>) -> Result<Self> {
        bounded_json(&fields, 65536)?;
        if !fields.is_object() || fields.get("data").is_some() || bytes.len() > MAX_COVER {
            return Err(error("now_playing_picture"));
        }
        Ok(Self { fields, bytes })
    }
}
#[derive(Clone)]
pub enum Covers {
    Unknown,
    Complete(Vec<Picture>),
}
pub struct Metadata {
    common: Value,
    covers: Covers,
    wire_projection: bool,
}
impl Metadata {
    pub fn from_host(common: Value, covers: Covers) -> Result<Self> {
        bounded_json(&common, 1024 * 1024)?;
        if !common.is_object() || common.get("picture").is_some() {
            return Err(error("now_playing_metadata"));
        }
        if let Covers::Complete(pictures) = &covers {
            if pictures.len() > 8
                || pictures.iter().map(|p| p.bytes.len()).sum::<usize>() > MAX_COVER
            {
                return Err(error("now_playing_picture"));
            }
            for p in pictures {
                bounded_json(&p.fields, 65536)?;
                if !p.fields.is_object() || p.fields.get("data").is_some() {
                    return Err(error("now_playing_picture"));
                }
            }
        }
        // Never project away unrelated common tags or invent picture absence.
        Ok(Self {
            common,
            covers,
            wire_projection: false,
        })
    }
    /// READ_AUDIO_METADATA skips covers. Unknown requires another real receipt.
    pub fn from_audio_reply(
        reply: &freeshow_audio::AudioReply,
        covers: Covers,
    ) -> Result<Option<Self>> {
        if let Some(diagnostic) = reply.diagnostics.first() {
            return Err(Error::native(
                "now_playing_metadata_unavailable",
                &diagnostic.message,
            ));
        }
        if reply.data.is_null() {
            return Ok(None);
        }
        let common = reply
            .data
            .get("common")
            .ok_or_else(|| error("now_playing_metadata"))?;
        bounded_json(common, 1024 * 1024)?;
        if common.get("year").is_some_and(Value::is_null) {
            return Err(error("now_playing_ambiguous_year"));
        }
        let mut metadata = Self::from_host(common.clone(), covers)?;
        metadata.wire_projection = true;
        Ok(Some(metadata))
    }
}
#[derive(Debug, Serialize)]
pub enum WritePolicy {
    SkipIfExistsAndSourceReadUtf8Equals,
    BufferAlways,
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Effect {
    ParseMetadata {
        path: String,
    },
    ReadCovers {
        path: String,
    },
    PublishMetadata {
        file_path: String,
        common: Value,
        pictures: Vec<Picture>,
        json_projection_only: bool,
    },
    IcecastMetadata {
        title: String,
    },
    EnsureAudioFolder {
        path: String,
    },
    Exists {
        path: String,
    },
    Delete {
        path: String,
    },
    ImageCreate {
        bytes: Vec<u8>,
    },
    ImagePng {
        image: u64,
    },
    ImageSize {
        image: u64,
    },
    ImageResizeJpeg {
        image: u64,
        width: u32,
        height: u32,
        quality: u8,
    },
    /// Private native resource disposal, not an Electron renderer event.
    ReleaseImage {
        image: u64,
    },
    WriteCover {
        path: String,
        bytes: Vec<u8>,
        policy: WritePolicy,
    },
    WriteText {
        path: String,
        text: String,
        policy: WritePolicy,
    },
    Reveal {
        path: String,
    },
    Alert {
        message: String,
    },
}
pub enum Receipt {
    Metadata(Option<Metadata>),
    Covers(Vec<Picture>),
    Done { success: bool },
    Exists(bool),
    Image(u64),
    Bytes(Vec<u8>),
    Size { width: u32, height: u32 },
    Failed(Error),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Parse,
    Covers,
    Publish,
    Icecast,
    CoverFolder,
    ExistsCover,
    DeleteCover,
    ImageCreate,
    ImagePng,
    WriteCover,
    ImageSize,
    Resize,
    Convert,
    DynamicFolder,
    TextFolder,
    WriteText,
    UnsetIcecast,
    UnsetTextFolder,
    UnsetWriteText,
    UnsetCoverFolder,
    UnsetExists,
    UnsetDelete,
    OpenFolder,
    OpenExists,
    Reveal,
    Alert,
    PathFolder,
    ReleaseImage,
    Done,
}
struct Operation {
    data: Option<SourceData>,
    metadata: Option<Metadata>,
    original_cover: Option<Vec<u8>>,
    cover_buffer: Option<Vec<u8>>,
    png: Vec<u8>,
    image: Option<u64>,
    index: usize,
    stage: Stage,
    pending: Option<u64>,
    accepted: bool,
    cancelled: bool,
    steps: usize,
    result: Option<Finished>,
    resize: (u32, u32),
    media_path: bool,
    failure: Option<Error>,
}
#[derive(Debug, Serialize)]
pub struct Finished {
    pub mutated_input: Option<Value>,
    pub media_path: Option<String>,
    pub error: Option<Error>,
}
pub struct Token {
    seal: Arc<()>,
    context: Context,
    operation: u64,
    serial: u64,
}
pub struct Accepted {
    token: Token,
}
pub struct Issued {
    pub effect: Effect,
    pub token: Token,
}
pub struct Started {
    pub operation: u64,
    pub effect: Option<Issued>,
}
pub struct Router {
    seal: Arc<()>,
    context: Context,
    paths: TrustedPaths,
    operations: BTreeMap<u64, Operation>,
    next: u64,
}
impl Router {
    pub fn new(context: Context, paths: TrustedPaths) -> Result<Self> {
        if !context.same(&paths.context) {
            return Err(error("now_playing_context"));
        }
        Ok(Self {
            seal: Arc::new(()),
            context,
            paths,
            operations: BTreeMap::new(),
            next: 0,
        })
    }
    pub fn begin_set(&mut self, data: SourceData, path: TrustedMediaPath) -> Result<Started> {
        if !self.context.same(&path.context)
            || data.payload["filePath"].as_str() != Some(&path.path)
        {
            return Err(error("now_playing_authority"));
        }
        self.start(Some(data), Stage::Parse)
    }
    pub fn begin_unset(&mut self) -> Result<Started> {
        self.start(None, Stage::UnsetIcecast)
    }
    pub fn begin_open(&mut self) -> Result<Started> {
        self.start(None, Stage::OpenFolder)
    }
    pub fn begin_media_path(&mut self) -> Result<Started> {
        self.start(None, Stage::PathFolder)
    }
    fn start(&mut self, data: Option<SourceData>, stage: Stage) -> Result<Started> {
        if self.operations.len() >= MAX_ACTIVE {
            return Err(error("now_playing_busy"));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| error("now_playing_limit"))?;
        let id = self.next;
        self.operations.insert(
            id,
            Operation {
                data,
                metadata: None,
                original_cover: None,
                cover_buffer: None,
                png: vec![],
                image: None,
                index: 0,
                stage,
                pending: None,
                accepted: false,
                cancelled: false,
                steps: 0,
                result: None,
                resize: (0, 0),
                media_path: stage == Stage::PathFolder,
                failure: None,
            },
        );
        Ok(Started {
            operation: id,
            effect: self.advance(id)?,
        })
    }
    pub fn accept(&mut self, token: Token) -> Result<Accepted> {
        self.validate(&token, false)?;
        self.operations.get_mut(&token.operation).unwrap().accepted = true;
        Ok(Accepted { token })
    }
    fn validate(&self, token: &Token, accepted: bool) -> Result<()> {
        if !Arc::ptr_eq(&self.seal, &token.seal)
            || !self.context.same(&token.context)
            || !self
                .operations
                .get(&token.operation)
                .is_some_and(|o| o.pending == Some(token.serial) && o.accepted == accepted)
        {
            Err(error("now_playing_token"))
        } else {
            Ok(())
        }
    }
    pub fn complete(&mut self, accepted: &Accepted, receipt: Receipt) -> Result<Option<Issued>> {
        let token = &accepted.token;
        self.validate(token, true)?;
        let o = self.operations.get_mut(&token.operation).unwrap();
        // Wrong receipt leaves accepted ownership retained, never a fake drain.
        if !receipt_matches(o.stage, &receipt) {
            return Err(error("now_playing_receipt"));
        }
        if o.stage == Stage::ReleaseImage {
            match &receipt {
                Receipt::Done { success: true } => {}
                Receipt::Failed(e) => return Err(e.clone()),
                _ => return Err(error("now_playing_cleanup_pending")),
            }
        }
        o.pending = None;
        o.accepted = false;
        if let Receipt::Failed(e) = receipt {
            o.failure = Some(e);
            o.stage = Stage::Done;
            return self.advance(token.operation);
        }
        let result = apply(o, receipt, &self.paths);
        if let Err(e) = result {
            o.failure = Some(e);
            o.stage = Stage::Done;
            return self.advance(token.operation);
        }
        if o.cancelled {
            o.failure = Some(error("now_playing_cancelled"));
            o.stage = Stage::Done;
            return self.advance(token.operation);
        }
        self.advance(token.operation)
    }
    pub fn cancel(&mut self, id: u64) -> Result<Option<Issued>> {
        let o = self
            .operations
            .get_mut(&id)
            .ok_or_else(|| error("now_playing_operation"))?;
        o.cancelled = true;
        if !o.accepted && o.result.is_none() {
            o.pending = None;
            o.failure = Some(error("now_playing_cancelled"));
            o.stage = Stage::Done;
            return self.advance(id);
        }
        Ok(None)
    }
    pub fn retained(&self) -> usize {
        self.operations.len()
    }
    pub fn accepted(&self) -> usize {
        self.operations.values().filter(|o| o.accepted).count()
    }
    pub fn take_result(&mut self, id: u64) -> Result<Finished> {
        if !self.operations.get(&id).is_some_and(|o| o.result.is_some()) {
            return Err(error("now_playing_pending"));
        }
        Ok(self.operations.remove(&id).unwrap().result.unwrap())
    }
    fn advance(&mut self, id: u64) -> Result<Option<Issued>> {
        let o = self.operations.get_mut(&id).unwrap();
        loop {
            o.steps += 1;
            if o.steps > 128 {
                o.failure = Some(error("now_playing_limit"));
                o.stage = Stage::Done;
            }
            let next = next_effect(o, &self.paths);
            let effect = match next {
                Ok(Some(e)) => e,
                Ok(None) => {
                    let path = o.media_path.then(|| self.paths.cover.clone());
                    let failure = o.failure.take();
                    o.result = Some(finished(o, failure, path));
                    return Ok(None);
                }
                Err(e) => {
                    o.failure = Some(e);
                    o.stage = Stage::Done;
                    continue;
                }
            };
            // Convert may advance several pure passes without issuing host work.
            if o.stage == Stage::Convert {
                continue;
            }
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| error("now_playing_limit"))?;
            o.pending = Some(self.next);
            return Ok(Some(Issued {
                effect,
                token: Token {
                    seal: self.seal.clone(),
                    context: self.context.clone(),
                    operation: id,
                    serial: self.next,
                },
            }));
        }
    }
}
fn finished(o: &mut Operation, error: Option<Error>, media_path: Option<String>) -> Finished {
    Finished {
        mutated_input: o.data.as_ref().map(|d| d.payload.clone()),
        media_path,
        error,
    }
}
fn next_effect(o: &mut Operation, p: &TrustedPaths) -> Result<Option<Effect>> {
    use Stage::*;
    Ok(Some(match o.stage {
        Parse => {
            let path = o.data.as_ref().unwrap().payload["filePath"]
                .as_str()
                .unwrap();
            if path.is_empty() {
                o.stage = Icecast;
                return next_effect(o, p);
            }
            Effect::ParseMetadata { path: path.into() }
        }
        Covers => Effect::ReadCovers {
            path: o.data.as_ref().unwrap().payload["filePath"]
                .as_str()
                .unwrap()
                .into(),
        },
        Publish => {
            let m = o.metadata.as_ref().unwrap();
            let crate::Covers::Complete(pictures) = &m.covers else {
                return Err(error("now_playing_covers_unknown"));
            };
            Effect::PublishMetadata {
                file_path: o.data.as_ref().unwrap().payload["filePath"]
                    .as_str()
                    .unwrap()
                    .into(),
                common: m.common.clone(),
                pictures: pictures.clone(),
                json_projection_only: m.wire_projection,
            }
        }
        Icecast => {
            let data = o.data.as_ref().unwrap();
            let artist = artist(o.metadata.as_ref(), &data.payload)?;
            let title = title(o.metadata.as_ref(), &data.payload)?;
            Effect::IcecastMetadata {
                title: format!("{artist} - {title}"),
            }
        }
        CoverFolder | DynamicFolder | TextFolder | UnsetTextFolder | UnsetCoverFolder
        | OpenFolder | PathFolder => Effect::EnsureAudioFolder {
            path: p.folder.clone(),
        },
        ExistsCover | UnsetExists => Effect::Exists {
            path: p.cover.clone(),
        },
        DeleteCover | UnsetDelete => Effect::Delete {
            path: p.cover.clone(),
        },
        ImageCreate => Effect::ImageCreate {
            bytes: o.original_cover.as_ref().unwrap().clone(),
        },
        ImagePng => Effect::ImagePng {
            image: o.image.unwrap(),
        },
        WriteCover => Effect::WriteCover {
            path: p.cover.clone(),
            bytes: std::mem::take(&mut o.png),
            policy: WritePolicy::BufferAlways,
        },
        ImageSize => Effect::ImageSize {
            image: o.image.unwrap(),
        },
        Resize => Effect::ImageResizeJpeg {
            image: o.image.unwrap(),
            width: o.resize.0,
            height: o.resize.1,
            quality: 90,
        },
        Convert => {
            if o.index == 0 {
                let data = o.data.as_mut().unwrap();
                if !data.payload.get("format").is_some_and(truthy) {
                    data.payload["format"] = Value::String("{artist} - {title} - {album}".into())
                }
            }
            if o.index == 4 || o.index == 5 {
                o.stage = DynamicFolder;
                return next_effect(o, p);
            }
            if o.index == 8 {
                o.stage = TextFolder;
                return next_effect(o, p);
            }
            replace_pass(o, p)?;
            return next_effect(o, p);
        }
        WriteText => Effect::WriteText {
            path: p.text.clone(),
            text: o.data.as_ref().unwrap().payload["format"]
                .as_str()
                .ok_or_else(|| error("now_playing_js_type"))?
                .into(),
            policy: WritePolicy::SkipIfExistsAndSourceReadUtf8Equals,
        },
        UnsetIcecast => Effect::IcecastMetadata {
            title: String::new(),
        },
        UnsetWriteText => Effect::WriteText {
            path: p.text.clone(),
            text: String::new(),
            policy: WritePolicy::SkipIfExistsAndSourceReadUtf8Equals,
        },
        OpenExists => Effect::Exists {
            path: p.text.clone(),
        },
        Reveal => Effect::Reveal {
            path: p.text.clone(),
        },
        Alert => Effect::Alert {
            message: "This does not exist!".into(),
        },
        Done => {
            if o.image.is_some() {
                o.stage = ReleaseImage;
                return next_effect(o, p);
            }
            return Ok(None);
        }
        ReleaseImage => Effect::ReleaseImage {
            image: o.image.unwrap(),
        },
    }))
}
fn receipt_matches(stage: Stage, r: &Receipt) -> bool {
    use Stage::*;
    matches!(r, Receipt::Failed(_))
        || match stage {
            Parse => matches!(r, Receipt::Metadata(_)),
            Covers => matches!(r, Receipt::Covers(_)),
            ExistsCover | UnsetExists | OpenExists => matches!(r, Receipt::Exists(_)),
            ImageCreate => matches!(r, Receipt::Image(_)),
            ImagePng | Resize => matches!(r, Receipt::Bytes(_)),
            ImageSize => matches!(r, Receipt::Size { .. }),
            _ => matches!(r, Receipt::Done { .. }),
        }
}
fn apply(o: &mut Operation, r: Receipt, p: &TrustedPaths) -> Result<()> {
    use Stage::*;
    o.stage = match (o.stage, r) {
        (Parse, Receipt::Metadata(m)) => {
            o.metadata = m;
            if o.metadata
                .as_ref()
                .is_some_and(|m| matches!(m.covers, crate::Covers::Unknown))
            {
                Covers
            } else if o.metadata.is_some() {
                Publish
            } else {
                Icecast
            }
        }
        (Covers, Receipt::Covers(pictures)) => {
            let m = o.metadata.take().unwrap();
            let mut metadata = Metadata::from_host(m.common, crate::Covers::Complete(pictures))?;
            metadata.wire_projection = m.wire_projection;
            o.metadata = Some(metadata);
            Publish
        }
        (Publish, _) => Icecast,
        (Icecast, _) => CoverFolder,
        (CoverFolder, _) => {
            o.original_cover = o.metadata.as_ref().and_then(|m| match &m.covers {
                crate::Covers::Complete(p) => p.first().map(|p| p.bytes.clone()),
                crate::Covers::Unknown => None,
            });
            if o.original_cover.is_some() {
                ImageCreate
            } else {
                ExistsCover
            }
        }
        (ExistsCover, Receipt::Exists(true)) => DeleteCover,
        (ExistsCover, _) | (DeleteCover, _) => Convert,
        (ImageCreate, Receipt::Image(id)) => {
            o.image = Some(id);
            ImagePng
        }
        (ImagePng, Receipt::Bytes(bytes)) => {
            if bytes.len() > MAX_COVER {
                return Err(error("now_playing_limit"));
            }
            o.png = bytes;
            WriteCover
        }
        (WriteCover, _) => ImageSize,
        (ImageSize, Receipt::Size { width, height }) => {
            if (width == 0) != (height == 0) {
                return Err(error("now_playing_image_size"));
            }
            if u64::from(width) * u64::from(height) > 16_777_216 {
                return Err(error("now_playing_image_limit"));
            }
            if width > 640 || height > 480 {
                let scale = (640.0 / f64::from(width)).min(480.0 / f64::from(height));
                let w = (f64::from(width) * scale + 0.5).floor() as u32;
                let h = (f64::from(height) * scale + 0.5).floor() as u32;
                o.resize = (w, h);
                Resize
            } else {
                o.cover_buffer = o.original_cover.take();
                Convert
            }
        }
        (Resize, Receipt::Bytes(bytes)) => {
            if bytes.len() > MAX_COVER {
                return Err(error("now_playing_limit"));
            }
            o.cover_buffer = Some(bytes);
            o.index = 0;
            Convert
        }
        (DynamicFolder, _) => {
            o.stage = Convert;
            replace_pass(o, p)?;
            Convert
        }
        (TextFolder, _) => WriteText,
        (WriteText, _) => Done,
        (UnsetIcecast, _) => UnsetTextFolder,
        (UnsetTextFolder, _) => UnsetWriteText,
        (UnsetWriteText, _) => UnsetCoverFolder,
        (UnsetCoverFolder, _) => UnsetExists,
        (UnsetExists, Receipt::Exists(true)) => UnsetDelete,
        (UnsetExists, _) | (UnsetDelete, _) => Done,
        (OpenFolder, _) => OpenExists,
        (OpenExists, Receipt::Exists(true)) => Reveal,
        (OpenExists, _) => Alert,
        (Reveal, _) | (Alert, _) => Done,
        (PathFolder, _) => Done,
        (ReleaseImage, _) => {
            o.image.take();
            Done
        }
        _ => return Err(error("now_playing_receipt")),
    };
    Ok(())
}
const DYNAMIC: [&str; 8] = [
    "{artist}",
    "{title}",
    "{album}",
    "{year}",
    "{artwork_path}",
    "{artwork_base64}",
    "{duration}",
    "{duration_s}",
];
fn replace_pass(o: &mut Operation, p: &TrustedPaths) -> Result<()> {
    let data = o.data.as_mut().unwrap();
    let m = o.metadata.as_ref();
    let replacement = match o.index {
        0 => artist(m, &data.payload)?,
        1 => title(m, &data.payload)?,
        2 => {
            let album = common(m, "album");
            if truthy(&album) {
                string(&album)
            } else {
                choose(&[unknown(&data.payload, 2)?], "Unknown Album")
            }
        }
        3 => {
            let year = common(m, "year");
            if year.is_null() {
                "Unknown Year".into()
            } else {
                let value = string(&year);
                if value.is_empty() {
                    "Unknown Year".into()
                } else {
                    value
                }
            }
        }
        4 => p.cover.clone(),
        5 => cover_base64(o.cover_buffer.as_deref())?,
        6 => {
            if !data.duration.truthy() {
                "00:00".into()
            } else {
                let total = data.duration.number().floor();
                let min = num((total / 60.0).floor());
                let sec = num(total % 60.0);
                format!("{}:{}", pad(&min), pad(&sec))
            }
        }
        7 => {
            if matches!(data.duration, Scalar::Undefined | Scalar::Json(Value::Null)) {
                return Err(error("now_playing_js_type"));
            }
            data.duration.string()
        }
        _ => return Err(error("now_playing_state")),
    };
    let format = data
        .payload
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| error("now_playing_js_type"))?;
    data.payload["format"] = Value::String(replace_all(format, DYNAMIC[o.index], &replacement)?);
    o.index += 1;
    Ok(())
}
fn common(m: Option<&Metadata>, key: &str) -> Value {
    m.and_then(|m| m.common.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}
fn cover_base64(bytes: Option<&[u8]>) -> Result<String> {
    let Some(bytes) = bytes else {
        return Ok(String::new());
    };
    const PREFIX: &str = "data:image/jpeg;base64,";
    let length = bytes
        .len()
        .checked_add(2)
        .and_then(|n| (n / 3).checked_mul(4))
        .and_then(|n| n.checked_add(PREFIX.len()))
        .ok_or_else(|| error("now_playing_text_limit"))?;
    if length > MAX_TEXT {
        return Err(error("now_playing_text_limit"));
    }
    let mut text = String::with_capacity(length);
    text.push_str(PREFIX);
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut text);
    Ok(text)
}
fn unknown(data: &Value, index: usize) -> Result<Value> {
    let value = data
        .get("unknownLang")
        .ok_or_else(|| error("now_playing_js_type"))?;
    if value.is_null() {
        return Err(error("now_playing_js_type"));
    }
    match value {
        Value::Array(a) => Ok(a.get(index).cloned().unwrap_or(Value::Null)),
        Value::Object(o) => Ok(o.get(&index.to_string()).cloned().unwrap_or(Value::Null)),
        Value::String(s) => {
            if s.chars().any(|c| c.len_utf16() > 1) {
                return Err(error("now_playing_unsupported_utf16"));
            }
            Ok(s.chars()
                .nth(index)
                .map_or(Value::Null, |c| Value::String(c.to_string())))
        }
        _ => Ok(Value::Null),
    }
}
fn title(m: Option<&Metadata>, data: &Value) -> Result<String> {
    let title = common(m, "title");
    if truthy(&title) {
        return Ok(string(&title));
    }
    if data.get("name").is_some_and(truthy) {
        return Ok(string(&data["name"]));
    }
    Ok(choose(&[unknown(data, 1)?], "Unknown Title"))
}
fn artist(m: Option<&Metadata>, data: &Value) -> Result<String> {
    let mut values = vec![];
    if let Some(m) = m {
        let selected = m.common.get("artists");
        let artists = match selected {
            Some(Value::Array(a)) if !a.is_empty() => a.clone(),
            Some(Value::String(s)) if !s.is_empty() => return Err(error("now_playing_js_type")),
            Some(Value::Object(o)) if o.get("length").is_some_and(truthy) => {
                return Err(error("now_playing_js_type"))
            }
            _ => vec![m
                .common
                .get("artist")
                .filter(|v| truthy(v))
                .or_else(|| m.common.get("albumartist").filter(|v| truthy(v)))
                .or_else(|| m.common.get("originalartist"))
                .cloned()
                .unwrap_or(Value::Null)],
        };
        for value in artists.into_iter().filter(truthy) {
            let text = value
                .as_str()
                .ok_or_else(|| error("now_playing_js_type"))?
                .trim_matches(space);
            if !text.is_empty() && !values.iter().any(|v| v == text) {
                values.push(text.to_string())
            }
        }
    }
    if !values.is_empty() {
        Ok(values.join(", "))
    } else {
        Ok(choose(&[unknown(data, 0)?], "Unknown Artist"))
    }
}
fn choose(values: &[Value], fallback: &str) -> String {
    values
        .iter()
        .find(|v| truthy(v))
        .map_or_else(|| fallback.into(), string)
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn num(n: f64) -> String {
    ryu_js::Buffer::new().format(n).into()
}
fn js_number(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    if s.len() > 2 && s.starts_with('0') {
        let radix = match s.as_bytes()[1] {
            b'x' | b'X' => 16,
            b'b' | b'B' => 2,
            b'o' | b'O' => 8,
            _ => 0,
        };
        if radix != 0 {
            let mut n = 0.0;
            for c in s[2..].chars() {
                let Some(d) = c.to_digit(radix) else {
                    return f64::NAN;
                };
                n = n * f64::from(radix) + f64::from(d)
            }
            return n;
        }
    }
    let b = s.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        digits += 1
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            digits += 1
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1
        }
        if i == start {
            return f64::NAN;
        }
    }
    if i != b.len() {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}
fn string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num(n.as_f64().unwrap()),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn pad(s: &str) -> String {
    if s.len() < 2 {
        format!("0{s}")
    } else {
        s.into()
    }
}
fn space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|' '| '\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn append(out: &mut String, s: &str) -> Result<()> {
    if s.len() > MAX_TEXT.saturating_sub(out.len()) {
        return Err(error("now_playing_text_limit"));
    }
    out.push_str(s);
    Ok(())
}
pub fn replace_all(input: &str, needle: &str, replacement: &str) -> Result<String> {
    if needle.is_empty() || input.len() > MAX_TEXT || replacement.len() > MAX_TEXT {
        return Err(error("now_playing_text_limit"));
    }
    let mut out = String::new();
    let mut end = 0;
    for (at, _) in input.match_indices(needle) {
        append(&mut out, &input[end..at])?;
        let mut chars = replacement.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '$' {
                if let Some(&(_, next)) = chars.peek() {
                    let special = match next {
                        '$' => Some("$"),
                        '&' => Some(needle),
                        '`' => Some(&input[..at]),
                        '\'' => Some(&input[at + needle.len()..]),
                        _ => None,
                    };
                    if let Some(text) = special {
                        append(&mut out, text)?;
                        chars.next();
                        continue;
                    }
                }
            }
            append(&mut out, &replacement[i..i + c.len_utf8()])?;
        }
        end = at + needle.len();
    }
    append(&mut out, &input[end..])?;
    Ok(out)
}
fn bounded_json(value: &Value, max: usize) -> Result<()> {
    fn visit(
        v: &Value,
        depth: usize,
        nodes: &mut usize,
        bytes: &mut usize,
        max: usize,
    ) -> Result<()> {
        *nodes += 1;
        if depth > 8 || *nodes > 1024 || *bytes > max {
            return Err(error("now_playing_input_limit"));
        }
        match v {
            Value::String(s) => *bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                for v in a {
                    visit(v, depth + 1, nodes, bytes, max)?
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    *bytes = bytes.saturating_add(k.len());
                    visit(v, depth + 1, nodes, bytes, max)?
                }
            }
            _ => *bytes = bytes.saturating_add(16),
        }
        if *bytes > max {
            return Err(error("now_playing_input_limit"));
        }
        Ok(())
    }
    visit(value, 0, &mut 0, &mut 0, max)
}
#[cfg(test)]
mod tests;
