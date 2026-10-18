use crate::{
    missing, tags, Context, Control, Error, ExifOutcome, Hydration, Limits, NumberHydration,
    NumberKind, Stats,
};
use indexmap::IndexMap;
use serde_json::{Map, Number, Value};
use std::{io::Write, sync::atomic::AtomicBool};

#[derive(Debug)]
enum Atom {
    Undefined,
    Number(f64),
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<Atom>),
}
type Group = IndexMap<&'static str, Atom>;
const NAMES: [&str; 6] = [
    "image",
    "thumbnail",
    "exif",
    "gps",
    "interoperability",
    "makernote",
];
struct Parser<'a> {
    data: &'a [u8],
    big: bool,
    tiff: f64,
    limits: &'a Limits,
    control: &'a Control,
    closing: &'a AtomicBool,
    stats: Stats,
}
impl Parser<'_> {
    fn tick(&mut self, amount: usize) -> Result<(), Error> {
        self.stats.operations = self
            .stats
            .operations
            .checked_add(amount)
            .ok_or_else(limit)?;
        if self.stats.operations > self.limits.operations {
            return Err(limit());
        }
        self.control.check(self.closing)
    }
    fn alloc(&mut self, amount: usize) -> Result<(), Error> {
        self.stats.allocated_bytes = self
            .stats
            .allocated_bytes
            .checked_add(amount)
            .ok_or_else(limit)?;
        if self.stats.allocated_bytes > self.limits.allocation_bytes {
            return Err(limit());
        }
        self.tick(1)
    }
    fn byte(&self, offset: f64) -> Option<u8> {
        if offset.is_finite() && offset >= 0.0 && offset.fract() == 0.0 {
            self.data.get(offset as usize).copied()
        } else {
            None
        }
    }
    // These intentionally reproduce exif 0.6.0 Buffer.js's bitwise/addition behavior,
    // including NaN and partially out-of-range values. This is not a TIFF validator.
    fn short(&self, offset: f64, signed: bool) -> f64 {
        let (hi, lo) = if self.big {
            (offset, offset + 1.0)
        } else {
            (offset + 1.0, offset)
        };
        let value = ((self.byte(hi).unwrap_or(0) as i32) << 8) as f64
            + self.byte(lo).map_or(f64::NAN, f64::from);
        if signed && value > 32767.0 {
            value - 65536.0
        } else {
            value
        }
    }
    fn long(&self, offset: f64, signed: bool) -> f64 {
        let mut value = 0.0;
        for i in 0..4 {
            let index = if self.big { i } else { 3 - i };
            value = ((value as i32).wrapping_shl(8)) as f64
                + self.byte(offset + index as f64).map_or(f64::NAN, f64::from);
        }
        if !signed && value < 0.0 {
            value + 4294967296.0
        } else {
            value
        }
    }
    fn directory(
        &mut self,
        offset: f64,
        cap: Option<usize>,
        gps: bool,
        stop_unknown: bool,
        depth: usize,
    ) -> Result<(Group, usize), Error> {
        self.tick(1)?;
        if depth > self.limits.depth {
            return Err(limit());
        }
        self.stats.directories += 1;
        let raw = self.short(offset, false);
        let count = if raw.is_nan() {
            0
        } else {
            cap.map_or(raw as usize, |cap| (raw as usize).min(cap))
        };
        if count > self.limits.tags.saturating_sub(self.stats.tags) {
            return Err(limit());
        }
        self.alloc(count.saturating_mul(256))?;
        let mut group = Group::new();
        for index in 0..count {
            self.stats.tags += 1;
            self.tick(1)?;
            let entry = offset + 2.0 + index as f64 * 12.0;
            let tag = self.short(entry, false);
            let table = if gps { tags::GPS } else { tags::EXIF };
            let name = if tag.is_nan() {
                None
            } else {
                table
                    .binary_search_by_key(&(tag as u16), |x| x.0)
                    .ok()
                    .map(|i| table[i].1)
            };
            let Some(name) = name else {
                if stop_unknown {
                    break;
                } else {
                    continue;
                }
            };
            let format = self.short(entry + 2.0, false);
            if !(1.0..=10.0).contains(&format) {
                if stop_unknown {
                    break;
                } else {
                    continue;
                }
            }
            let components = self.long(entry + 4.0, false);
            if components > self.limits.components as f64 {
                return Err(limit());
            }
            let count = if components.is_nan() {
                0
            } else {
                components as usize
            };
            self.alloc(count.saturating_mul(160).saturating_add(128))?;
            let inline = match format as u8 {
                1 | 2 | 6 | 7 => components <= 4.0,
                3 | 8 => components <= 2.0,
                4 | 9 => components == 1.0,
                _ => false,
            };
            let value_offset = if inline {
                entry + 8.0
            } else {
                self.long(entry + 8.0, false) + self.tiff
            };
            let value = match format as u8 {
                2 => {
                    let mut string = String::new();
                    if value_offset.is_finite() {
                        for i in 0..count {
                            self.tick(1)?;
                            string
                                .push(char::from(self.byte(value_offset + i as f64).unwrap_or(0)));
                        }
                    }
                    if string.ends_with('\0') {
                        string.pop();
                    }
                    Atom::Text(string)
                }
                7 => {
                    let start = slice_index(value_offset, self.data.len());
                    let end = slice_index(value_offset + components, self.data.len()).max(start);
                    self.tick(end - start + 1)?;
                    Atom::Bytes(self.data[start..end].to_vec())
                }
                kind => {
                    let mut values = Vec::with_capacity(count);
                    for i in 0..count {
                        self.tick(1)?;
                        let i = i as f64;
                        let value = match kind {
                            1 | 6 => self.byte(value_offset + i).map_or(Atom::Undefined, |x| {
                                Atom::Number(if kind == 6 && x > 127 {
                                    f64::from(x) - 256.0
                                } else {
                                    f64::from(x)
                                })
                            }),
                            3 | 8 => Atom::Number(self.short(value_offset + i * 2.0, kind == 8)),
                            4 | 9 => Atom::Number(self.long(value_offset + i * 4.0, kind == 9)),
                            5 | 10 => Atom::Number(
                                self.long(value_offset + i * 8.0, kind == 10)
                                    / self.long(value_offset + i * 8.0 + 4.0, kind == 10),
                            ),
                            _ => unreachable!(),
                        };
                        values.push(value);
                    }
                    if count == 1 {
                        values.pop().unwrap()
                    } else {
                        Atom::Array(values)
                    }
                }
            };
            if tag != 0xEA1C as f64 {
                group.insert(name, value);
            }
        }
        Ok((group, count))
    }
    fn wire(&mut self, value: Atom, path: &str, hydration: &mut Hydration) -> Result<Value, Error> {
        self.tick(1)?;
        self.alloc(path.len().saturating_mul(4).saturating_add(96))?;
        Ok(match value {
            Atom::Undefined => {
                hydration.undefined_paths.push(path.into());
                Value::Null
            }
            Atom::Number(value) => {
                let kind = if value.is_nan() {
                    Some(NumberKind::NaN)
                } else if value == f64::INFINITY {
                    Some(NumberKind::PositiveInfinity)
                } else if value == f64::NEG_INFINITY {
                    Some(NumberKind::NegativeInfinity)
                } else if value == 0.0 && value.is_sign_negative() {
                    Some(NumberKind::NegativeZero)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    hydration.special_numbers.push(NumberHydration {
                        path: path.into(),
                        kind,
                    });
                }
                Number::from_f64(if value == 0.0 { 0.0 } else { value })
                    .map_or(Value::Null, Value::Number)
            }
            Atom::Text(text) => Value::String(text),
            Atom::Bytes(bytes) => {
                self.alloc(bytes.len().saturating_mul(64))?;
                hydration.byte_buffers.push(path.into());
                Value::Array(bytes.into_iter().map(|b| Value::from(b as u64)).collect())
            }
            Atom::Array(values) => {
                self.alloc(values.len().saturating_mul(64))?;
                let mut result = Vec::with_capacity(values.len());
                for (index, value) in values.into_iter().enumerate() {
                    result.push(self.wire(value, &format!("{path}/{index}"), hydration)?);
                }
                Value::Array(result)
            }
        })
    }
}
fn slice_index(value: f64, length: usize) -> usize {
    if value.is_nan() {
        0
    } else if value < 0.0 {
        (length as f64 + value.trunc()).clamp(0.0, length as f64) as usize
    } else {
        value.min(length as f64) as usize
    }
}
fn limit() -> Error {
    Error::new(
        "exif_limit",
        "EXIF input, tags, depth, allocation, output or work limit exceeded",
    )
}
fn pointer(group: &Group, name: &str) -> Result<Option<f64>, Error> {
    match group.get(name) {
        None | Some(Atom::Undefined) => Ok(None),
        Some(Atom::Number(n)) => Ok(if *n == 0.0 || n.is_nan() {
            None
        } else {
            Some(*n)
        }),
        // Arrays/strings invoke JS's + coercion. Do not pretend numeric TIFF pointers
        // are equivalent to arbitrary coercions of malformed pointer values.
        _ => Err(Error::new(
            "exif_unsupported",
            "Non-numeric EXIF directory pointer is unsupported",
        )),
    }
}
struct CountWriter<'a> {
    count: usize,
    limit: usize,
    operations: usize,
    max_operations: usize,
    control: &'a Control,
    closing: &'a AtomicBool,
    failure: Option<Error>,
}
impl Write for CountWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let error = self.control.check(self.closing).err().or_else(|| {
            self.operations = self.operations.saturating_add(bytes.len() / 64 + 1);
            if bytes.len() > self.limit.saturating_sub(self.count)
                || self.operations > self.max_operations
            {
                Some(limit())
            } else {
                None
            }
        });
        if let Some(error) = error {
            self.failure = Some(error);
            return Err(std::io::Error::other("EXIF bounded serialization stopped"));
        }
        self.count += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn parse(
    id: String,
    context: Context,
    data: &[u8],
    limits: &Limits,
    control: &Control,
    closing: &AtomicBool,
) -> Result<ExifOutcome, Error> {
    control.check(closing)?;
    if data.len() > limits.input_bytes {
        return Err(limit());
    }
    let mut p = Parser {
        data,
        big: true,
        tiff: 0.0,
        limits,
        control,
        closing,
        stats: Stats {
            input_bytes: data.len(),
            ..Stats::default()
        },
    };
    p.alloc(
        data.len()
            .saturating_mul(2)
            .saturating_add(crate::service::base_allocation(&id, &context)),
    )?;
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return Ok(missing(
            context,
            id,
            "exif_format",
            "Original parser accepts JPEG only",
            true,
            p.stats,
        ));
    }
    let mut offset = 2.0;
    let mut start = None;
    while offset < data.len() as f64 {
        p.tick(1)?;
        if p.byte(offset) != Some(0xFF) {
            break;
        }
        let marker = p.byte(offset + 1.0);
        offset += 2.0;
        if marker == Some(0xE1) {
            start = Some(offset + 2.0);
            break;
        }
        offset += p.short(offset, false);
    }
    let Some(start) = start else {
        return Ok(missing(
            context,
            id,
            "exif_missing",
            "No first APP1 EXIF segment",
            true,
            p.stats,
        ));
    };
    if !(0..6).all(|i| p.byte(start + i as f64) == Some(b"Exif\0\0"[i])) {
        return Ok(missing(
            context,
            id,
            "exif_parse",
            "First APP1 segment is not EXIF",
            true,
            p.stats,
        ));
    }
    p.tiff = start + 6.0;
    match (p.byte(p.tiff), p.byte(p.tiff + 1.0)) {
        (Some(b'I'), Some(b'I')) => p.big = false,
        (Some(b'M'), Some(b'M')) => p.big = true,
        _ => {
            return Ok(missing(
                context,
                id,
                "exif_parse",
                "Invalid EXIF byte order",
                true,
                p.stats,
            ))
        }
    }
    if p.short(p.tiff + 2.0, false) != 42.0 {
        return Ok(missing(
            context,
            id,
            "exif_parse",
            "Invalid EXIF TIFF magic",
            true,
            p.stats,
        ));
    }
    let ifd = p.tiff + p.long(p.tiff + 4.0, false);
    let (image, count) = p.directory(ifd, Some(128), false, false, 1)?;
    let next = p.long(ifd + 2.0 + count as f64 * 12.0, false);
    let thumbnail = if next != 0.0 && !next.is_nan() {
        p.directory(p.tiff + next, Some(128), false, false, 1)?.0
    } else {
        Group::new()
    };
    let exif = match pointer(&image, "ExifOffset") {
        Ok(Some(n)) => p.directory(p.tiff + n, None, false, false, 2)?.0,
        Ok(None) => Group::new(),
        Err(_) => {
            return Ok(missing(
                context,
                id,
                "exif_unsupported",
                "Non-numeric EXIF directory pointer",
                false,
                p.stats,
            ))
        }
    };
    let gps = match pointer(&image, "GPSInfo") {
        Ok(Some(n)) => p.directory(p.tiff + n, Some(128), true, false, 2)?.0,
        Ok(None) => Group::new(),
        Err(_) => {
            return Ok(missing(
                context,
                id,
                "exif_unsupported",
                "Non-numeric GPS directory pointer",
                false,
                p.stats,
            ))
        }
    };
    let interoperability = match pointer(&exif, "InteropOffset") {
        Ok(Some(n)) => p.directory(p.tiff + n, Some(128), false, true, 3)?.0,
        Ok(None) => Group::new(),
        Err(_) => {
            return Ok(missing(
                context,
                id,
                "exif_unsupported",
                "Non-numeric interoperability pointer",
                false,
                p.stats,
            ))
        }
    };
    let mut makernote = Group::new();
    if let Some(note) = exif.get("MakerNote") {
        match note {
            Atom::Undefined => (),
            Atom::Bytes(bytes) => {
                let known = [
                    b"OLYMP\0\x01".as_slice(),
                    b"OLYMP\0\x02",
                    b"AGFA \0\x01",
                    b"EPSON\0\x01\0",
                    b"FUJIFILM",
                    b"Panasonic",
                    b"SANYO",
                ]
                .iter()
                .any(|prefix| {
                    prefix
                        .iter()
                        .enumerate()
                        .all(|(i, byte)| bytes.get(i).copied().unwrap_or(0) == *byte)
                });
                // The pinned parser passes this.makernoteOffset (unset), rather than
                // this.offsets.makernoteOffset, to its vendor plugins: all six are empty.
                if !known {
                    makernote.insert("error",Atom::Text("Unable to extract Makernote information as it is in an unsupported or unrecognized format.".into()));
                }
            }
            _ => {
                return Ok(missing(
                    context,
                    id,
                    "exif_parse",
                    "Original MakerNote parser requires bytes",
                    true,
                    p.stats,
                ))
            }
        }
    }
    let mut groups = Map::new();
    let mut hydration = Hydration::default();
    for (name, group) in
        NAMES
            .into_iter()
            .zip([image, thumbnail, exif, gps, interoperability, makernote])
    {
        let mut object = Map::new();
        for (tag, atom) in group {
            let undefined = matches!(atom, Atom::Undefined);
            let value = p.wire(atom, &format!("/exif/{name}/{tag}"), &mut hydration)?;
            if !undefined {
                object.insert(tag.into(), value);
            }
        }
        groups.insert(name.into(), Value::Object(object));
    }
    let reply = serde_json::json!({"id":id,"exif":groups});
    Ok(ExifOutcome {
        context,
        reply,
        hydration,
        diagnostics: Vec::new(),
        source_compatible: true,
        stats: p.stats,
    })
}

/// Bounds the complete data-only envelope, including omitted-field metadata and
/// diagnostics. Count without allocating a second serialized payload.
pub(crate) fn finish(
    mut outcome: ExifOutcome,
    limits: &Limits,
    control: &Control,
    closing: &AtomicBool,
) -> Result<ExifOutcome, Error> {
    control.check(closing)?;
    for _ in 0..4 {
        let mut writer = CountWriter {
            count: 0,
            limit: limits.output_bytes,
            operations: outcome.stats.operations,
            max_operations: limits.operations,
            control,
            closing,
            failure: None,
        };
        if serde_json::to_writer(&mut writer, &outcome).is_err() {
            return Err(writer.failure.unwrap_or_else(limit));
        }
        outcome.stats.operations = writer.operations;
        if writer.count == outcome.stats.output_bytes {
            break;
        }
        outcome.stats.output_bytes = writer.count;
    }
    control.check(closing)?;
    Ok(outcome)
}
