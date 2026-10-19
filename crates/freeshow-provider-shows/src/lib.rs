//! Pure source-shaped ChurchAppsShowBuilder preparation. No provider/FS/IPC IO.
use indexmap::IndexMap;
use serde::Serialize;
use std::{cell::Cell, collections::HashMap};
mod case_table;
mod date;
mod lyrics;
mod value;
pub use lyrics::Section;
pub use value::SourceValue;
pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 65_536;
pub const MAX_DEPTH: usize = 16;
pub const MAX_COPIED_DEPTH: usize = 8;
pub const MAX_LYRICS: usize = 1024 * 1024;
pub const MAX_SLIDES: usize = 4096;
pub const MAX_FILES: usize = 1024;
pub const MAX_UIDS: usize = 8193;
pub const MAX_WORK_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OPERATIONS: usize = 2_000_000;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
fn limit() -> Error {
    Error {
        code: "provider_shows_limit",
        message: "Show preparation exceeded a fixed resource limit",
    }
}
fn unsupported(message: &'static str) -> Error {
    Error {
        code: "provider_shows_unsupported",
        message,
    }
}
#[derive(Default)]
struct Budget {
    work: Cell<usize>,
    operations: Cell<usize>,
    output: Cell<usize>,
    nodes: Cell<usize>,
}
impl Budget {
    fn work(&self, bytes: usize) -> Result<()> {
        let n = self.work.get().checked_add(bytes).ok_or_else(limit)?;
        if n > MAX_WORK_BYTES {
            return Err(limit());
        }
        self.work.set(n);
        self.op(1)
    }
    fn op(&self, count: usize) -> Result<()> {
        let n = self.operations.get().checked_add(count).ok_or_else(limit)?;
        if n > MAX_OPERATIONS {
            return Err(limit());
        }
        self.operations.set(n);
        Ok(())
    }
    fn value(&self, bytes: usize) -> Result<()> {
        let n = self.output.get().checked_add(bytes).ok_or_else(limit)?;
        if n > MAX_OUTPUT_BYTES || self.nodes.get() >= MAX_NODES {
            return Err(limit());
        }
        self.work(bytes)?;
        self.output.set(n);
        self.nodes.set(self.nodes.get() + 1);
        Ok(())
    }
    fn string(&self, s: &str) -> Result<SourceValue> {
        self.value(
            s.len()
                .checked_mul(6)
                .and_then(|n| n.checked_add(2))
                .ok_or_else(limit)?,
        )?;
        Ok(SourceValue::String(s.into()))
    }
    fn number(&self, n: f64) -> Result<SourceValue> {
        self.value(32)?;
        Ok(SourceValue::Number(n))
    }
    fn null(&self) -> Result<SourceValue> {
        self.value(4)?;
        Ok(SourceValue::Null)
    }
    fn boolean(&self, b: bool) -> Result<SourceValue> {
        self.value(5)?;
        Ok(SourceValue::Bool(b))
    }
    fn object(&self, fields: Vec<(&str, SourceValue)>) -> Result<SourceValue> {
        let n = fields
            .iter()
            .try_fold(2usize, |n, (k, _)| {
                n.checked_add(k.len().checked_mul(6)?.checked_add(4)?)
            })
            .ok_or_else(limit)?;
        self.value(n)?;
        Ok(SourceValue::Object(
            fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        ))
    }
    fn array(&self, values: Vec<SourceValue>) -> Result<SourceValue> {
        self.value(values.len().checked_add(2).ok_or_else(limit)?)?;
        Ok(SourceValue::Array(values))
    }
    fn copy(&self, v: &SourceValue) -> Result<SourceValue> {
        self.copy_at(v, 0)
    }
    fn copy_at(&self, v: &SourceValue, depth: usize) -> Result<SourceValue> {
        if depth > MAX_COPIED_DEPTH {
            return Err(limit());
        }
        match v {
            SourceValue::String(s) => self.string(s),
            SourceValue::Number(n) => self.number(*n),
            SourceValue::Null => self.null(),
            SourceValue::Bool(b) => self.boolean(*b),
            SourceValue::Undefined => {
                self.value(4)?;
                Ok(SourceValue::Undefined)
            }
            SourceValue::Array(a) => {
                self.work(a.len() * std::mem::size_of::<SourceValue>())?;
                let a = a
                    .iter()
                    .map(|v| self.copy_at(v, depth + 1))
                    .collect::<Result<Vec<_>>>()?;
                self.array(a)
            }
            SourceValue::Object(o) => {
                self.work(o.len() * 128)?;
                let mut fields = Vec::with_capacity(o.len());
                for (k, v) in o {
                    fields.push((k.as_str(), self.copy_at(v, depth + 1)?));
                }
                let mut v = self.object(fields)?;
                v.sort_keys();
                Ok(v)
            }
        }
    }
    fn fallback(&self, v: &SourceValue, other: SourceValue) -> Result<SourceValue> {
        if v.truthy() {
            self.copy(v)
        } else {
            Ok(other)
        }
    }
}
/// Borrowed native nondeterministic boundary. Values are consumed only at their
/// original uid()/Date.now() sites; unused entries have no semantic effect.
pub struct Environment<'a> {
    pub uids: &'a [&'a str],
    pub now_millis: &'a [f64],
}
struct Cursor<'a> {
    source: Environment<'a>,
    uids: Cell<usize>,
    now: Cell<usize>,
}
impl Cursor<'_> {
    fn uid(&self, b: &Budget) -> Result<String> {
        let i = self.uids.get();
        if i >= MAX_UIDS {
            return Err(limit());
        }
        let id = self
            .source
            .uids
            .get(i)
            .ok_or_else(|| unsupported("Trusted UID sequence exhausted"))?;
        self.uids.set(i + 1);
        if id.len() != 11
            || !id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(unsupported(
                "Only ordinary eleven-lowerhex source UIDs are supported",
            ));
        }
        b.work(id.len())?;
        Ok((*id).into())
    }
    fn now(&self, b: &Budget) -> Result<SourceValue> {
        let i = self.now.get();
        let n = *self
            .source
            .now_millis
            .get(i)
            .ok_or_else(|| unsupported("Trusted clock sequence exhausted"))?;
        self.now.set(i + 1);
        b.number(n)
    }
}
#[derive(Debug)]
pub struct Built {
    pub value: SourceValue,
    pub uid_calls: usize,
    pub now_calls: usize,
    pub date_calls: usize,
    pub work_bytes: usize,
    pub operations: usize,
}
fn input(values: &[&SourceValue]) -> Result<()> {
    let (mut bytes, mut nodes) = (0, 0);
    for v in values {
        v.validate(0, &mut bytes, &mut nodes)?;
    }
    Ok(())
}
fn cursor(source: Environment<'_>) -> Result<Cursor<'_>> {
    if source.uids.len() > MAX_UIDS || source.now_millis.len() > 16 {
        return Err(limit());
    }
    Ok(Cursor {
        source,
        uids: Cell::new(0),
        now: Cell::new(0),
    })
}
fn finish(value: SourceValue, c: Cursor<'_>, b: Budget, date_calls: usize) -> Built {
    Built {
        value,
        uid_calls: c.uids.get(),
        now_calls: c.now.get(),
        date_calls,
        work_bytes: b.work.get(),
        operations: b.operations.get(),
    }
}
fn get<'a>(object: &'a SourceValue, key: &str) -> Result<&'a SourceValue> {
    match object {
        SourceValue::Object(fields) => Ok(fields
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&SourceValue::Undefined, |(_, v)| v)),
        _ => Err(unsupported("Builder records must be ordinary objects")),
    }
}
fn timestamps(b: &Budget, created: SourceValue, modified: SourceValue) -> Result<SourceValue> {
    b.object(vec![
        ("created", created),
        ("modified", modified),
        ("used", b.null()?),
    ])
}
fn settings(b: &Budget, id: &str) -> Result<SourceValue> {
    b.object(vec![
        ("activeLayout", b.string(id)?),
        ("template", b.null()?),
    ])
}
fn result(
    b: &Budget,
    id: SourceValue,
    show: SourceValue,
    seconds: &SourceValue,
) -> Result<SourceValue> {
    b.object(vec![
        ("showId", id),
        ("show", show),
        ("seconds", b.fallback(seconds, b.number(0.0)?)?),
    ])
}
fn layouts(
    b: &Budget,
    id: &str,
    notes: &SourceValue,
    slides: Vec<SourceValue>,
) -> Result<SourceValue> {
    b.object(vec![(
        id,
        b.object(vec![
            ("name", b.string("Default")?),
            ("notes", b.fallback(notes, b.string("")?)?),
            ("slides", b.array(slides)?),
        ])?,
    )])
}
const ITEM_STYLE: &str = "left:50px;top:120px;width:1820px;height:840px;";
/// Exact parseLyrics branches for supported strings and JS-falsy values.
pub fn parse_lyrics(value: &SourceValue) -> Result<Vec<Section>> {
    input(&[value])?;
    let b = Budget::default();
    let sections = lyrics::parse(value, &b)?;
    let mut bytes = 0usize;
    for s in &sections {
        bytes = bytes
            .checked_add(s.label.len() * 6 + 64)
            .ok_or_else(limit)?;
        for text in &s.lyrics {
            bytes = bytes.checked_add(text.len() * 6 + 4).ok_or_else(limit)?;
        }
    }
    if bytes > MAX_OUTPUT_BYTES {
        return Err(limit());
    }
    Ok(sections)
}
pub fn create_empty_show(
    id: &SourceValue,
    title: &SourceValue,
    description: &SourceValue,
    seconds: &SourceValue,
    environment: Environment<'_>,
) -> Result<Built> {
    input(&[id, title, description, seconds])?;
    let b = Budget::default();
    let c = cursor(environment)?;
    let layout_id = c.uid(&b)?;
    let show = b.object(vec![
        ("name", b.fallback(title, b.string("")?)?),
        ("category", b.string("churchapps")?),
        ("timestamps", timestamps(&b, c.now(&b)?, b.null()?)?),
        (
            "meta",
            b.object(vec![
                ("title", b.copy(title)?),
                ("author", b.string("")?),
                ("publisher", b.string("")?),
                ("copyright", b.string("")?),
                ("CCLI", b.string("")?),
                ("key", b.string("")?),
            ])?,
        ),
        ("settings", settings(&b, &layout_id)?),
        ("layouts", layouts(&b, &layout_id, description, vec![])?),
        ("slides", b.object(vec![])?),
        ("media", b.object(vec![])?),
    ])?;
    let id = format!("chumsshow_{}", id.js_string(&b, 0)?);
    b.work(id.len())?;
    let value = result(&b, b.string(&id)?, show, seconds)?;
    Ok(finish(value, c, b, 0))
}
pub fn create_media_show(
    item: &SourceValue,
    files: &[SourceValue],
    environment: Environment<'_>,
) -> Result<Built> {
    if files.len() > MAX_FILES {
        return Err(limit());
    }
    let mut bytes = 0;
    let mut nodes = 0;
    for f in std::iter::once(item).chain(files) {
        f.validate(0, &mut bytes, &mut nodes)?;
    }
    let b = Budget::default();
    let c = cursor(environment)?;
    let layout_id = c.uid(&b)?;
    let (mut slides, mut media, mut order) = (IndexMap::new(), IndexMap::new(), Vec::new());
    for file in files {
        b.work(256)?;
        let slide_id = c.uid(&b)?;
        let media_id = c.uid(&b)?;
        let url = get(file, "url")?;
        let stream = get(file, "streamUrl")?;
        let empty = SourceValue::String(String::new());
        let path = if url.truthy() {
            url
        } else if stream.truthy() {
            stream
        } else {
            &empty
        };
        let file_type = get(file, "fileType")?;
        let video = if matches!(file_type,SourceValue::String(s) if s=="video") {
            true
        } else {
            let SourceValue::String(s) = path else {
                return Err(unsupported(
                    "Truthy media URL requires source string methods",
                ));
            };
            s.ends_with(".mp4") || s.contains("/externalVideos/download/") || stream.truthy()
        };
        let record = b.object(vec![
            ("name", b.fallback(get(file, "name")?, b.string("")?)?),
            ("path", b.copy(path)?),
            ("type", b.string(if video { "video" } else { "image" })?),
            (
                "loop",
                b.fallback(get(file, "loopVideo")?, b.boolean(false)?)?,
            ),
        ])?;
        media.insert(media_id.clone(), record);
        let slide = b.object(vec![
            ("group", b.copy(get(item, "label")?)?),
            ("color", b.null()?),
            ("settings", b.object(vec![])?),
            ("notes", b.string("")?),
            ("items", b.array(vec![])?),
        ])?;
        slides.insert(slide_id.clone(), slide);
        order.push(b.object(vec![
            ("id", b.string(&slide_id)?),
            ("background", b.string(&media_id)?),
        ])?);
    }
    let show = b.object(vec![
        ("name", b.fallback(get(item, "label")?, b.string("")?)?),
        ("category", b.string("churchapps")?),
        ("timestamps", timestamps(&b, c.now(&b)?, b.null()?)?),
        (
            "meta",
            b.object(vec![("title", b.copy(get(item, "label")?)?)])?,
        ),
        ("settings", settings(&b, &layout_id)?),
        (
            "layouts",
            layouts(&b, &layout_id, get(item, "description")?, order)?,
        ),
        ("slides", owned_object(&b, slides)?),
        ("media", owned_object(&b, media)?),
    ])?;
    let prefix = if matches!(get(item,"itemType")?,SourceValue::String(s) if s=="lessonSection") {
        "chumslesson"
    } else {
        "chumsaction"
    };
    let id = format!("{prefix}_{}", get(item, "id")?.js_string(&b, 0)?);
    b.work(id.len())?;
    let value = result(&b, b.string(&id)?, show, get(item, "seconds")?)?;
    Ok(finish(value, c, b, 0))
}
fn set(fields: &mut Vec<(String, SourceValue)>, key: String, value: SourceValue) {
    if let Some((_, old)) = fields.iter_mut().find(|(k, _)| k == &key) {
        *old = value;
    } else {
        fields.push((key, value));
    }
}
fn owned_object(b: &Budget, fields: IndexMap<String, SourceValue>) -> Result<SourceValue> {
    b.value(fields.iter().map(|(k, _)| k.len() * 6 + 4).sum::<usize>() + 2)?;
    Ok(SourceValue::Object(fields.into_iter().collect()))
}
pub fn create_song_show(
    key: &SourceValue,
    arrangement: &SourceValue,
    song: &SourceValue,
    details: &SourceValue,
    environment: Environment<'_>,
) -> Result<Built> {
    input(&[key, arrangement, song, details])?;
    let b = Budget::default();
    let c = cursor(environment)?;
    let sections = lyrics::parse(get(arrangement, "lyrics")?, &b)?;
    let (mut slides, mut order) = (IndexMap::new(), Vec::new());
    let mut dedup = HashMap::<String, String>::new();
    let mut slide_count = 0;
    for section in sections {
        let (mut children, mut parent) = (Vec::new(), None);
        for (i, text) in section.lyrics.iter().enumerate() {
            slide_count += 1;
            if slide_count > MAX_SLIDES {
                return Err(limit());
            }
            b.op(text.len() + section.label.len())?;
            let line_capacity = text.bytes().filter(|&c| c == b'\n').count() + 1;
            b.work(
                line_capacity * std::mem::size_of::<&str>()
                    + section.label.len()
                    + text.len() * 2
                    + 1,
            )?;
            let lines = text
                .split('\n')
                .filter(|line| !lyrics::trim(line).is_empty())
                .collect::<Vec<_>>();
            let dedup_key = format!("{}\n{}", section.label, lines.join("\n"));
            let id = if let Some(id) = dedup.get(&dedup_key) {
                b.work(id.len())?;
                id.clone()
            } else {
                let id = c.uid(&b)?;
                let mut output_lines = Vec::new();
                for line in lines {
                    b.work(64)?;
                    output_lines.push(b.object(vec![
                        ("align", b.string("")?),
                        (
                            "text",
                            b.array(vec![b.object(vec![
                                ("style", b.string("")?),
                                ("value", b.string(line)?),
                            ])?])?,
                        ),
                    ])?);
                }
                let lower = lyrics::lower(&section.label, &b)?;
                let slide = b.object(vec![
                    ("group", b.string(&section.label)?),
                    ("globalGroup", b.string(&lower)?),
                    ("color", b.null()?),
                    ("settings", b.object(vec![])?),
                    ("notes", b.string("")?),
                    (
                        "items",
                        b.array(vec![b.object(vec![
                            ("style", b.string(ITEM_STYLE)?),
                            ("lines", b.array(output_lines)?),
                        ])?])?,
                    ),
                ])?;
                b.work(128 + id.len())?;
                slides.insert(id.clone(), slide);
                dedup.insert(dedup_key, id.clone());
                id
            };
            if i == 0 {
                parent = Some(id.clone());
                order.push(b.object(vec![("id", b.string(&id)?)])?);
            } else {
                children.push(b.string(&id)?);
            }
        }
        if !children.is_empty() {
            if let Some(id) = parent {
                let value = b.array(children)?;
                let slide = slides
                    .get_mut(&id)
                    .ok_or_else(|| unsupported("UID collision removed a parent"))?;
                if let SourceValue::Object(fields) = slide {
                    if !fields.iter().any(|(k, _)| k == "children") {
                        b.work(64)?;
                    }
                    set(fields, "children".into(), value);
                }
            }
        }
    }
    let title = format!(
        "{} ({} - {})",
        get(details, "title")?.js_string(&b, 0)?,
        get(arrangement, "name")?.js_string(&b, 0)?,
        get(key, "keySignature")?.js_string(&b, 0)?
    );
    b.work(title.len())?;
    let layout_id = c.uid(&b)?;
    let date_input = get(song, "dateAdded")?;
    if let SourceValue::String(s) = date_input {
        b.op(s.len().checked_mul(2).ok_or_else(limit)?)?;
    }
    let milliseconds = date::parse(date_input)?;
    let created = if milliseconds != 0.0 && !milliseconds.is_nan() {
        b.number(milliseconds)?
    } else {
        c.now(&b)?
    };
    let modified = if milliseconds != 0.0 && !milliseconds.is_nan() {
        b.number(milliseconds)?
    } else {
        b.null()?
    };
    let show = b.object(vec![
        ("name", b.string(&title)?),
        ("category", b.string("churchapps")?),
        ("timestamps", timestamps(&b, created, modified)?),
        (
            "meta",
            b.object(vec![
                ("title", b.string(&title)?),
                (
                    "author",
                    b.fallback(get(details, "artist")?, b.string("")?)?,
                ),
                ("publisher", b.string("")?),
                ("copyright", b.string("")?),
                ("CCLI", b.string("")?),
                (
                    "key",
                    b.fallback(get(details, "keySignature")?, b.string("")?)?,
                ),
            ])?,
        ),
        ("settings", settings(&b, &layout_id)?),
        (
            "layouts",
            layouts(&b, &layout_id, get(song, "notes")?, order)?,
        ),
        ("slides", owned_object(&b, slides)?),
        ("media", b.object(vec![])?),
    ])?;
    let supplied = get(arrangement, "freeShowId")?;
    let show_id = if supplied.truthy() {
        b.copy(supplied)?
    } else {
        let id = format!("chumssong_{}", get(key, "id")?.js_string(&b, 0)?);
        b.work(id.len())?;
        b.string(&id)?
    };
    let value = result(&b, show_id, show, get(details, "seconds")?)?;
    Ok(finish(value, c, b, 2))
}

#[cfg(test)]
mod tests;
