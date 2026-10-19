//! Guards declaration-driven allocations/recursion before passing bytes to Lofty.
use crate::{AudioConfig, AudioError};
use lofty::file::FileType;
use std::io::{Read, Seek, SeekFrom};
fn resource(what: &str) -> AudioError {
    AudioError::Resource(what.into())
}
fn read<R: Read>(reader: &mut R, bytes: &mut [u8]) -> Result<(), AudioError> {
    reader.read_exact(bytes).map_err(|_| AudioError::Input)
}
fn seek<R: Seek>(reader: &mut R, position: SeekFrom) -> Result<u64, AudioError> {
    reader.seek(position).map_err(|_| AudioError::Input)
}
fn sync(bytes: &[u8]) -> Result<usize, AudioError> {
    if bytes.iter().any(|byte| byte & 128 != 0) {
        return Err(AudioError::Input);
    }
    Ok(bytes
        .iter()
        .fold(0, |value, byte| (value << 7) | usize::from(*byte)))
}
fn block<R: Read>(
    reader: &mut R,
    size: usize,
    config: &AudioConfig,
) -> Result<Vec<u8>, AudioError> {
    if size > config.allocation_limit {
        return Err(resource("declared metadata block size"));
    }
    let mut bytes = vec![0; size];
    read(reader, &mut bytes)?;
    Ok(bytes)
}

pub(crate) fn prefix<R: Read + Seek>(
    reader: &mut R,
    config: &AudioConfig,
) -> Result<crate::id3_text::Plan, AudioError> {
    let mut header = [0; 10];
    read(reader, &mut header)?;
    if header.starts_with(&[0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11])
        || header.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
    {
        return Err(AudioError::UnsupportedVariant(
            "ASF/Matroska metadata".into(),
        ));
    }
    let mut plan = crate::id3_text::Plan::default();
    if &header[..3] == b"ID3" {
        let size = sync(&header[6..10])?;
        let bytes = block(reader, size, config)?;
        id3(&header, &bytes, config)?;
        plan = crate::id3_text::Plan::leading(&header, &bytes, config)?;
        if plan.active() {
            if header[3] == 4 && header[5] & 0x10 != 0 {
                seek(reader, SeekFrom::Current(10))?;
            }
            let mut next = [0; 3];
            let mut count = 0;
            while count < next.len() {
                let n = reader
                    .read(&mut next[count..])
                    .map_err(|_| AudioError::Input)?;
                if n == 0 {
                    break;
                }
                count += n;
            }
            if count == 3 && &next == b"ID3" {
                return Err(AudioError::UnsupportedVariant(
                    "multiple leading ID3 tags with timestamp text recovery".into(),
                ));
            }
        }
    }
    seek(reader, SeekFrom::Start(0))?;
    Ok(plan)
}
fn id3(header: &[u8], bytes: &[u8], config: &AudioConfig) -> Result<(), AudioError> {
    if !matches!(header[3], 3 | 4) || header[5] & 0xc0 != 0 {
        return Err(AudioError::UnsupportedVariant(
            "ID3v2.2, unsynchronized or extended tag header".into(),
        ));
    }
    let declared = sync(&header[6..10])?;
    if declared > config.allocation_limit {
        return Err(resource("declared embedded ID3 tag size"));
    }
    if declared > bytes.len() {
        return Err(AudioError::Input);
    }
    let bytes = &bytes[..declared];
    let mut position = 0;
    let mut count = 0;
    while position < bytes.len() {
        if bytes[position] == 0 {
            break;
        }
        if bytes.len() - position < 10 {
            return Err(AudioError::Input);
        }
        let frame = &bytes[position..position + 10];
        count += 1;
        if count > config.tag_limit {
            return Err(resource("declared ID3 frame count"));
        }
        let size = if header[3] == 4 {
            sync(&frame[4..8])?
        } else {
            u32::from_be_bytes(frame[4..8].try_into().unwrap()) as usize
        };
        if size > config.allocation_limit {
            return Err(resource("declared ID3 frame size"));
        }
        if matches!(&frame[..4], b"CHAP" | b"CTOC")
            || (header[3] == 3 && frame[9] & 0xc0 != 0)
            || (header[3] == 4 && frame[9] & 0x0c != 0)
        {
            return Err(AudioError::UnsupportedVariant(
                "ID3 chapters/compressed/encrypted frames".into(),
            ));
        }
        position = position
            .checked_add(10 + size)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::Input)?;
    }
    Ok(())
}
fn comments(bytes: &[u8], config: &AudioConfig) -> Result<(usize, usize), AudioError> {
    let get = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
            .ok_or(AudioError::Input)
    };
    let vendor = get(0)?;
    if vendor > config.allocation_limit {
        return Err(resource("declared Vorbis vendor length"));
    }
    let mut position = 4usize.checked_add(vendor).ok_or(AudioError::Input)?;
    let count = get(position)?;
    position += 4;
    let start = position;
    if count > config.tag_limit {
        return Err(resource("declared Vorbis comment count"));
    }
    for _ in 0..count {
        let length = get(position)?;
        if length > config.allocation_limit {
            return Err(resource("declared Vorbis field length"));
        }
        position = position
            .checked_add(4 + length)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::Input)?;
    }
    Ok((count, start))
}
pub(crate) fn format<R: Read + Seek>(
    reader: &mut R,
    format: FileType,
    config: &AudioConfig,
) -> Result<crate::flac_comments::Plan, AudioError> {
    let mut flac_comments = crate::flac_comments::Plan::default();
    seek(reader, SeekFrom::Start(0))?;
    match format {
        FileType::Flac => {
            let mut signature = [0; 4];
            read(reader, &mut signature)?;
            if &signature[..3] == b"ID3" {
                let mut rest = [0; 6];
                read(reader, &mut rest)?;
                seek(reader, SeekFrom::Current(sync(&rest[2..6])? as i64))?;
                read(reader, &mut signature)?;
            }
            if &signature != b"fLaC" {
                return Err(AudioError::Input);
            }
            loop {
                let mut header = [0; 4];
                read(reader, &mut header)?;
                let length = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
                if header[0] & 127 == 4 {
                    let bytes = block(reader, length, config)?;
                    let (count, start) = comments(&bytes, config)?;
                    flac_comments.append(&bytes, count, start, config)?;
                } else {
                    seek(reader, SeekFrom::Current(length as i64))?;
                }
                if header[0] & 128 != 0 {
                    break;
                }
            }
        }
        FileType::Vorbis | FileType::Opus => {
            let mut packet = vec![];
            let mut found = false;
            for _ in 0..config.operation_limit {
                let mut header = [0; 27];
                read(reader, &mut header)?;
                if &header[..4] != b"OggS" {
                    return Err(AudioError::Input);
                }
                let mut lacing = vec![0; header[26] as usize];
                read(reader, &mut lacing)?;
                for length in lacing {
                    if packet.len() + length as usize > config.allocation_limit {
                        return Err(resource("OGG header packet length"));
                    }
                    let start = packet.len();
                    packet.resize(start + length as usize, 0);
                    read(reader, &mut packet[start..])?;
                    if length < 255 {
                        let offset = if packet.starts_with(b"OpusTags") {
                            Some(8)
                        } else if packet.starts_with(b"\x03vorbis") {
                            Some(7)
                        } else {
                            None
                        };
                        if let Some(offset) = offset {
                            comments(&packet[offset..], config)?;
                            found = true;
                            break;
                        }
                        packet.clear();
                    }
                }
                if found {
                    break;
                }
            }
            if !found {
                return Err(resource("OGG header search"));
            }
        }
        FileType::Wav => {
            let mut header = [0; 12];
            read(reader, &mut header)?;
            if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
                return Err(AudioError::UnsupportedVariant("RF64/RIFX WAV".into()));
            }
            let end = seek(reader, SeekFrom::End(0))?;
            seek(reader, SeekFrom::Start(12))?;
            while seek(reader, SeekFrom::Current(0))? + 8 <= end {
                let mut chunk = [0; 8];
                read(reader, &mut chunk)?;
                let size = u32::from_le_bytes(chunk[4..].try_into().unwrap()) as usize;
                if matches!(&chunk[..4], b"LIST" | b"id3 " | b"ID3 ") {
                    let bytes = block(reader, size, config)?;
                    if &chunk[..4] == b"LIST" && bytes.starts_with(b"INFO") {
                        let mut position = 4;
                        let mut count = 0;
                        while position + 8 <= bytes.len() {
                            count += 1;
                            if count > config.tag_limit {
                                return Err(resource("WAV INFO field count"));
                            }
                            let length = u32::from_le_bytes(
                                bytes[position + 4..position + 8].try_into().unwrap(),
                            ) as usize;
                            if length > config.allocation_limit {
                                return Err(resource("WAV INFO field length"));
                            }
                            position = position
                                .checked_add(8 + length + (length & 1))
                                .filter(|end| *end <= bytes.len())
                                .ok_or(AudioError::Input)?;
                        }
                    } else if bytes.starts_with(b"ID3") && bytes.len() >= 10 {
                        id3(&bytes[..10], &bytes[10..], config)?;
                    }
                } else {
                    seek(reader, SeekFrom::Current(size as i64))?;
                }
                if size & 1 != 0 {
                    seek(reader, SeekFrom::Current(1))?;
                }
            }
        }
        FileType::Mp4 => {
            let mut ftyp = [0; 12];
            read(reader, &mut ftyp)?;
            if &ftyp[4..8] == b"ftyp" && &ftyp[8..12] == b"qt  " {
                return Err(AudioError::Input);
            }
            mp4(reader, config)?;
        }
        FileType::Mpeg => {
            let end = seek(reader, SeekFrom::End(0))?;
            let length = end.min(160) as usize;
            seek(reader, SeekFrom::End(-(length as i64)))?;
            let bytes = block(reader, length, config)?;
            if (length >= 128 && &bytes[length - 128..length - 125] == b"TAG")
                || bytes.windows(8).any(|b| b == b"APETAGEX")
            {
                return Err(AudioError::UnsupportedVariant(
                    "MP3 APE/ID3v1 tag priority".into(),
                ));
            }
        }
        _ => {}
    }
    seek(reader, SeekFrom::Start(0))?;
    Ok(flac_comments)
}

// Iterative because AtomInfo::read -> parse_freeform -> freeform_chunk ->
// AtomInfo::read in Lofty recurses on nested `----` even before mean/name checks.
fn mp4<R: Read + Seek>(reader: &mut R, config: &AudioConfig) -> Result<(), AudioError> {
    let end = seek(reader, SeekFrom::End(0))?;
    // Context 1 = direct ilst entries; context 0 = ordinary box children.
    let mut ranges = vec![(0u64, end, 0usize, 0u8)];
    let mut count = 0;
    while let Some((mut position, end, depth, context)) = ranges.pop() {
        while position.checked_add(8).is_some_and(|next| next <= end) {
            count += 1;
            if count > config.tag_limit {
                return Err(resource("MP4 atom count"));
            }
            seek(reader, SeekFrom::Start(position))?;
            let mut header = [0; 8];
            read(reader, &mut header)?;
            let size = u32::from_be_bytes(header[..4].try_into().unwrap());
            let mut header_size = 8u64;
            let size = if size == 0 {
                end - position
            } else if size == 1 {
                let mut extended = [0; 8];
                read(reader, &mut extended)?;
                header_size = 16;
                u64::from_be_bytes(extended)
            } else {
                u64::from(size)
            };
            if size < header_size {
                return Err(AudioError::Input);
            }
            let box_end = position
                .checked_add(size)
                .filter(|next| *next <= end)
                .ok_or(AudioError::Input)?;
            let name = &header[4..];
            let payload = position + header_size;
            if matches!(name, b"ilst" | b"mean" | b"name")
                && size - header_size > config.allocation_limit as u64
            {
                return Err(resource("MP4 declared metadata allocation"));
            }
            let known = matches!(
                name,
                b"moov"
                    | b"trak"
                    | b"mdia"
                    | b"minf"
                    | b"stbl"
                    | b"udta"
                    | b"meta"
                    | b"ilst"
                    | b"moof"
                    | b"traf"
                    | b"----"
            );
            let entry = context == 1 && !matches!(name, b"free" | b"skip");
            if known || entry {
                if depth >= 32 {
                    return Err(resource("MP4 metadata nesting depth"));
                }
                let mut child_start = payload;
                if name == b"meta" && box_end - payload >= 8 {
                    seek(reader, SeekFrom::Start(payload))?;
                    let mut possible = [0; 8];
                    read(reader, &mut possible)?;
                    if !matches!(
                        &possible[4..],
                        b"hdlr" | b"ilst" | b"mhdr" | b"ctry" | b"lang"
                    ) {
                        child_start += 4;
                    }
                }
                ranges.push((
                    child_start,
                    box_end,
                    depth + 1,
                    if name == b"ilst" { 1 } else { 0 },
                ));
            }
            position = box_end;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vorbis_declarations_cannot_reserve_unbounded_vectors() {
        let config = AudioConfig::default();
        assert!(matches!(
            comments(&[0, 0, 0, 0, 255, 255, 255, 255], &config),
            Err(AudioError::Resource(_))
        ));
        assert!(matches!(
            comments(&[255, 255, 255, 255], &config),
            Err(AudioError::Resource(_))
        ));
    }
    #[test]
    fn id3_declared_size_is_rejected_before_reading_body() {
        let mut reader = std::io::Cursor::new([73, 68, 51, 4, 0, 0, 127, 127, 127, 127]);
        assert!(matches!(
            prefix(&mut reader, &AudioConfig::default()),
            Err(AudioError::Resource(_))
        ));
        assert_eq!(reader.position(), 10);
    }
    #[test]
    fn compressed_and_recursive_frames_are_explicit_gaps() {
        let header = [73, 68, 51, 4, 0, 0, 0, 0, 0, 10];
        for bytes in [*b"CHAP\0\0\0\0\0\0", *b"TIT2\0\0\0\0\0\x08"] {
            assert!(matches!(
                id3(&header, &bytes, &AudioConfig::default()),
                Err(AudioError::UnsupportedVariant(_))
            ));
        }
    }
}
