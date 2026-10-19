//! Bounded source-specific APIC / FLAC PICTURE projection, not a general metadata parser.
use crate::{service::Check, *};
use serde_json::json;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
const TYPES: [&str; 21] = [
    "Other",
    "32x32 pixels 'file icon' (PNG only)",
    "Other file icon",
    "Cover (front)",
    "Cover (back)",
    "Leaflet page",
    "Media (e.g. label side of CD)",
    "Lead artist/lead performer/soloist",
    "Artist/performer",
    "Conductor",
    "Band/Orchestra",
    "Composer",
    "Lyricist/text writer",
    "Recording Location",
    "During recording",
    "During performance",
    "Movie/video screen capture",
    "A bright coloured fish",
    "Illustration",
    "Band/artist logotype",
    "Publisher/Studio logotype",
];
fn io(e: std::io::Error) -> Error {
    Error::new("cover_io", e.to_string())
}
fn read_exact(file: &mut File, n: usize, check: &Check) -> Result<Vec<u8>> {
    if n > MAX_METADATA {
        return Err(resource("Metadata allocation exceeds 8MiB"));
    }
    let mut result = vec![0; n];
    for chunk in result.chunks_mut(65536) {
        check.check()?;
        file.read_exact(chunk).map_err(io)?
    }
    Ok(result)
}
fn syncsafe(b: &[u8]) -> Result<usize> {
    if b.len() != 4 || b.iter().any(|n| n & 128 != 0) {
        return Err(invalid("Non-syncsafe ID3 size"));
    }
    Ok(b.iter().fold(0, |n, b| (n << 7) | *b as usize))
}
fn u32be(b: &[u8]) -> Result<usize> {
    let b: [u8; 4] = b.try_into().map_err(|_| invalid("Truncated integer"))?;
    Ok(u32::from_be_bytes(b) as usize)
}
fn picture_type(fields: &mut Value, t: usize) {
    if let Some(t) = TYPES.get(t) {
        fields["type"] = json!(t)
    }
}
fn fixed_mime(mime: &str, data: &[u8], id3: bool) -> Result<Option<String>> {
    if !mime.is_ascii() {
        return Err(unsupported("Non-ASCII MIME locale case mapping"));
    }
    let mut mime = mime.to_ascii_lowercase();
    if id3 {
        mime = match mime.as_str() {
            "jpg" => "image/jpeg".into(),
            "png" => "image/png".into(),
            _ => mime,
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    if mime.is_empty() {
        return Err(unsupported(
            "Empty MIME requires asynchronous file-type sniff/order; not certified",
        ));
    }
    if mime == "image/jpg" {
        mime = "image/jpeg".into()
    }
    Ok(Some(mime))
}
fn append(
    out: &mut Vec<Picture>,
    fields: Value,
    data: &[u8],
    total: &mut usize,
    field_order: &'static [&'static str],
) -> Result<()> {
    if out.len() >= 8 {
        return Err(resource("More than eight pictures"));
    }
    *total = total
        .checked_add(data.len())
        .ok_or_else(|| resource("Picture byte overflow"))?;
    if *total > MAX_ENCODED {
        return Err(resource("Aggregate picture bytes exceed 4MiB"));
    }
    let undefined_fields = if fields.get("type").is_none() {
        vec!["type"]
    } else {
        vec![]
    };
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > 65536 {
                return Err(std::io::Error::other("Picture fields exceed 64KiB"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(0), &fields)
        .map_err(|_| resource("Picture fields exceed 64KiB"))?;
    out.push(Picture {
        fields,
        data: data.to_vec(),
        undefined_fields,
        field_order,
    });
    Ok(())
}
fn description(b: &[u8], encoding: u8) -> Result<String> {
    if matches!(encoding, 0 | 3) && b.starts_with(&[255, 254]) {
        return Err(unsupported("Cross-encoding BOM workaround not certified"));
    }
    if b.len() > 65536 {
        return Err(resource("Picture description exceeds 64KiB"));
    }
    match encoding {
        0 => Ok(b.iter().map(|c| char::from(*c)).collect()),
        3 => String::from_utf8(b.to_vec())
            .map_err(|_| unsupported("Invalid UTF-8 description replacement semantics")),
        1 | 2 => {
            if !b.len().is_multiple_of(2) {
                return Err(invalid("Odd UTF-16 description"));
            }
            // Pinned TextEncodingToken maps BOTH 1 and 2 to utf16le.
            // Encoding 2 without BOM is therefore deliberately interpreted LE.
            let (little, b) = if b.starts_with(&[255, 254]) {
                (true, &b[2..])
            } else if b.starts_with(&[254, 255]) {
                (false, &b[2..])
            } else {
                (true, b)
            };
            if b.starts_with(&[255, 254]) || b.starts_with(&[254, 255]) {
                return Err(unsupported("Repeated UTF-16 BOM workaround not certified"));
            }
            let units = b.as_chunks::<2>().0.iter().map(|s| {
                if little {
                    u16::from_le_bytes([s[0], s[1]])
                } else {
                    u16::from_be_bytes([s[0], s[1]])
                }
            });
            std::char::decode_utf16(units)
                .collect::<std::result::Result<String, _>>()
                .map_err(|_| unsupported("Unpaired UTF-16 description"))
        }
        _ => Err(unsupported("Unknown APIC description encoding")),
    }
}
fn apic(body: &[u8], out: &mut Vec<Picture>, total: &mut usize) -> Result<()> {
    let encoding = *body.first().ok_or_else(|| invalid("Empty APIC"))?;
    let end = body[1..]
        .iter()
        .position(|b| *b == 0)
        .map(|p| p + 1)
        .ok_or_else(|| invalid("Unterminated MIME"))?;
    if end > 1024 {
        return Err(resource("Picture MIME exceeds 1KiB"));
    }
    let mime = std::str::from_utf8(&body[1..end]).map_err(|_| unsupported("Non-ASCII MIME"))?;
    let kind = *body
        .get(end + 1)
        .ok_or_else(|| invalid("Missing picture type"))? as usize;
    let start = end + 2;
    if start > body.len() {
        return Err(invalid("Missing description"));
    }
    let step = if encoding == 1 || encoding == 2 { 2 } else { 1 };
    let desc_end = if step == 1 {
        body[start..]
            .iter()
            .position(|b| *b == 0)
            .map(|p| start + p)
    } else {
        body[start..]
            .as_chunks::<2>()
            .0
            .iter()
            .position(|b| *b == [0, 0])
            .map(|p| start + p * 2)
    }
    .ok_or_else(|| invalid("Unterminated picture description"))?;
    let data = &body[desc_end + step..];
    let Some(mime) = fixed_mime(mime, data, true)? else {
        return Ok(());
    };
    let desc = description(&body[start..desc_end], encoding)?;
    let mut fields = json!({"format":mime,"description":desc});
    picture_type(&mut fields, kind);
    append(
        out,
        fields,
        data,
        total,
        &["format", "type", "description", "data"],
    )
}
fn flac_picture(body: &[u8], out: &mut Vec<Picture>, total: &mut usize) -> Result<()> {
    let mut pos = 0;
    fn number(body: &[u8], pos: &mut usize) -> Result<usize> {
        let n = u32be(
            body.get(*pos..*pos + 4)
                .ok_or_else(|| invalid("Truncated FLAC picture"))?,
        )?;
        *pos += 4;
        Ok(n)
    }
    fn text(body: &[u8], pos: &mut usize) -> Result<String> {
        let len = number(body, pos)?;
        if len > 65536 {
            return Err(resource("FLAC picture field exceeds 64KiB"));
        }
        let value = body
            .get(*pos..*pos + len)
            .ok_or_else(|| invalid("Truncated FLAC text"))?;
        *pos += len;
        String::from_utf8(value.to_vec()).map_err(|_| unsupported("Invalid FLAC UTF-8"))
    }
    let kind = number(body, &mut pos)?;
    let mime = text(body, &mut pos)?;
    let desc = text(body, &mut pos)?;
    let width = number(body, &mut pos)?;
    let height = number(body, &mut pos)?;
    let depth = number(body, &mut pos)?;
    let indexed = number(body, &mut pos)?;
    let len = number(body, &mut pos)?;
    let data = body
        .get(
            pos..pos
                .checked_add(len)
                .ok_or_else(|| resource("Picture offset"))?,
        )
        .ok_or_else(|| invalid("Truncated FLAC picture data"))?;
    if pos + len != body.len() {
        return Err(unsupported("Trailing FLAC picture fields"));
    }
    let Some(mime) = fixed_mime(&mime, data, false)? else {
        return Ok(());
    };
    let mut fields = json!({"format":mime,"description":desc,"width":width,"height":height,"colour_depth":depth,"indexed_color":indexed});
    picture_type(&mut fields, kind);
    append(
        out,
        fields,
        data,
        total,
        &[
            "type",
            "format",
            "description",
            "width",
            "height",
            "colour_depth",
            "indexed_color",
            "data",
        ],
    )
}
pub(crate) fn read(trusted: TrustedMediaFile, check: &Check) -> Result<Vec<Picture>> {
    check.check()?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match trusted.opened {
        Some(file) => file,
        None => options.open(trusted.path).map_err(io)?,
    };
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() {
        return Err(unsupported("Only actual regular files are accepted"));
    }
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let size = metadata.len();
    if size < 4 {
        return Err(invalid("Truncated media signature"));
    }
    // AbstractID3Parser also reads trailing APE/ID3v1 for FLAC. Reject
    // all such priority variants before certifying absence in any container.
    if size >= 128 {
        file.seek(SeekFrom::End(-128)).map_err(io)?;
        if read_exact(&mut file, 3, check)? == b"TAG" {
            return Err(unsupported("ID3v1/mixed tail tag priority"));
        }
    }
    if size >= 32 {
        file.seek(SeekFrom::End(-32)).map_err(io)?;
        if read_exact(&mut file, 8, check)? == b"APETAGEX" {
            return Err(unsupported("APE tail cover priority"));
        }
    }
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let head = read_exact(&mut file, 4, check)?;
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut out = vec![];
    let mut total = 0;
    if &head[..3] == b"ID3" {
        let head = read_exact(&mut file, 10, check)?;
        if !matches!(head[3], 3 | 4) || head[4] != 0 || head[5] != 0 {
            return Err(unsupported(
                "Only ID3v2.3/2.4 without unsync/extended/footer flags",
            ));
        }
        let len = syncsafe(&head[6..10])?;
        if len > MAX_METADATA || 10 + len as u64 > size {
            return Err(resource("Declared ID3 tag exceeds bound/file"));
        }
        file.seek(SeekFrom::Start(10)).map_err(io)?;
        let bytes = read_exact(&mut file, len, check)?;
        let mut p = 0;
        let mut count = 0;
        while p < bytes.len() {
            check.check()?;
            if bytes[p..].iter().all(|b| *b == 0) {
                break;
            }
            count += 1;
            if count > 256 {
                return Err(resource("ID3 frame count"));
            }
            let h = bytes
                .get(p..p + 10)
                .ok_or_else(|| invalid("Truncated ID3 frame"))?;
            if !h[..4]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                || h[8] != 0
                || h[9] != 0
            {
                return Err(unsupported("Unknown ID3 frame flags/identifier"));
            }
            let n = if head[3] == 4 {
                syncsafe(&h[4..8])?
            } else {
                u32be(&h[4..8])?
            };
            p += 10;
            let body = bytes
                .get(
                    p..p.checked_add(n)
                        .ok_or_else(|| resource("ID3 frame offset"))?,
                )
                .ok_or_else(|| invalid("Truncated ID3 frame body"))?;
            if &h[..4] == b"APIC" {
                apic(body, &mut out, &mut total)?
            }
            p += n;
        }
        file.seek(SeekFrom::Start(10 + len as u64)).map_err(io)?;
        let next = read_exact(&mut file, 3, check)?;
        if next == b"ID3" || next == b"fLa" {
            return Err(unsupported("Consecutive/mixed tag containers"));
        }
        if next[0] != 255 || next[1] & 0xe0 != 0xe0 {
            return Err(unsupported("ID3 tag not followed by MPEG audio"));
        }
    } else if head[0] == 255 && head[1] & 0xe0 == 0xe0 {
        // No leading picture tag and no trailing picture-capable tag after
        // the common preflight; the source exposes no cover from MPEG frames.
    } else if &head == b"fLaC" {
        file.seek(SeekFrom::Start(4)).map_err(io)?;
        let mut seen = 0usize;
        let mut ended = false;
        for index in 0..256 {
            check.check()?;
            let h = read_exact(&mut file, 4, check)?;
            let kind = h[0] & 127;
            let len = ((h[1] as usize) << 16) | ((h[2] as usize) << 8) | h[3] as usize;
            seen = seen
                .checked_add(len + 4)
                .ok_or_else(|| resource("FLAC byte overflow"))?;
            if seen > MAX_METADATA {
                return Err(resource("FLAC metadata exceeds 8MiB"));
            }
            if index == 0 && (kind != 0 || len != 34) {
                return Err(invalid("FLAC must begin with STREAMINFO"));
            }
            let bytes = read_exact(&mut file, len, check)?;
            match kind {
                6 => flac_picture(&bytes, &mut out, &mut total)?,
                4 => vorbis_no_pictures(&bytes)?,
                0..=3 | 5 => {}
                _ => return Err(unsupported("Unknown FLAC metadata block")),
            }
            if h[0] & 128 != 0 {
                ended = true;
                break;
            }
        }
        if !ended {
            return Err(resource("FLAC metadata block count"));
        }
    } else if &head == b"RIFF" {
        let h = read_exact(&mut file, 12, check)?;
        if &h[8..] != b"WAVE" {
            return Err(unsupported("Non-WAVE RIFF"));
        }
        let end = 8 + u32::from_le_bytes(h[4..8].try_into().unwrap()) as u64;
        if end > size {
            return Err(invalid("Truncated RIFF"));
        }
        let mut p = 12u64;
        let mut count = 0;
        while p + 8 <= end {
            check.check()?;
            count += 1;
            if count > 256 {
                return Err(resource("WAV chunk count"));
            }
            file.seek(SeekFrom::Start(p)).map_err(io)?;
            let h = read_exact(&mut file, 8, check)?;
            let n = u32::from_le_bytes(h[4..].try_into().unwrap()) as u64;
            p = p
                .checked_add(8 + n + (n & 1))
                .ok_or_else(|| resource("WAV chunk offset"))?;
            if p > end {
                return Err(invalid("Truncated WAV chunk"));
            }
            if !matches!(&h[..4], b"fmt " | b"data" | b"JUNK" | b"LIST") {
                return Err(unsupported("WAV embedded/unknown picture-capable chunk"));
            }
        }
        if p != end {
            return Err(invalid("WAV trailing chunk bytes"));
        }
    } else {
        return Err(unsupported(
            "Only ID3v2 MPEG, direct-picture FLAC and ordinary WAV are certified",
        ));
    }
    check.check()?;
    Ok(out)
}

fn vorbis_no_pictures(bytes: &[u8]) -> Result<()> {
    fn n(bytes: &[u8], p: &mut usize) -> Result<usize> {
        let n = u32::from_le_bytes(
            bytes
                .get(*p..*p + 4)
                .ok_or_else(|| invalid("Truncated Vorbis comment"))?
                .try_into()
                .unwrap(),
        ) as usize;
        *p += 4;
        Ok(n)
    }
    let mut p = 0;
    let vendor = n(bytes, &mut p)?;
    p = p
        .checked_add(vendor)
        .ok_or_else(|| resource("Vendor offset"))?;
    if p > bytes.len() {
        return Err(invalid("Truncated Vorbis vendor"));
    }
    let count = n(bytes, &mut p)?;
    if count > 256 {
        return Err(resource("Vorbis comment count"));
    }
    for _ in 0..count {
        let len = n(bytes, &mut p)?;
        if len > 65536 {
            return Err(resource("Vorbis comment field"));
        }
        let value = bytes
            .get(p..p + len)
            .ok_or_else(|| invalid("Truncated Vorbis comment"))?;
        p += len;
        let key = value.split(|b| *b == b'=').next().unwrap_or_default();
        if key.eq_ignore_ascii_case(b"METADATA_BLOCK_PICTURE")
            || key.eq_ignore_ascii_case(b"COVERART")
            || key.eq_ignore_ascii_case(b"COVERARTMIME")
        {
            return Err(unsupported(
                "Vorbis-comment embedded/base64 picture priority not yet certified",
            ));
        }
    }
    if p != bytes.len() {
        return Err(invalid("Trailing Vorbis comment bytes"));
    }
    Ok(())
}
