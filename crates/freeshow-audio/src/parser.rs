use crate::{
    common::{self, Collector},
    js::{self, Js},
    AudioConfig, AudioError, AudioFormat, FormatHint,
};
use lofty::{
    config::{apply_global_options, GlobalOptions, ParseOptions},
    file::{AudioFile, FileType},
    id3::v2::{Frame, Id3v2Tag, Id3v2Version},
    mp4::{AtomData, AtomIdent},
    probe::Probe,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Budget<R> {
    pub reader: R,
    pub bytes: usize,
    pub operations: usize,
    pub cancelled: bool,
    pub exceeded: bool,
    limit: usize,
    ops_limit: usize,
    deadline: Instant,
    cancel: CancellationToken,
    external_cancel: Option<CancellationToken>,
}
impl<R> Budget<R> {
    pub fn new(reader: R, config: &AudioConfig, cancel: CancellationToken) -> Self {
        Self {
            reader,
            bytes: 0,
            operations: 0,
            cancelled: false,
            exceeded: false,
            limit: config.read_limit,
            ops_limit: config.operation_limit,
            deadline: Instant::now() + config.parse_timeout,
            cancel,
            external_cancel: None,
        }
    }
    fn check(&mut self) -> io::Result<()> {
        if self.is_cancelled() {
            self.cancelled = true;
            return Err(io::Error::other("audio cancelled"));
        }
        self.operations += 1;
        if self.operations > self.ops_limit || Instant::now() >= self.deadline {
            self.exceeded = true;
            return Err(io::Error::other("audio operation/deadline limit"));
        }
        Ok(())
    }
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
            || self
                .external_cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
    }
}
impl<R: Read> Read for Budget<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let remaining = self.limit.saturating_sub(self.bytes);
        if remaining == 0 {
            self.exceeded = true;
            return Err(io::Error::other("audio read limit"));
        }
        let allowed = buffer.len().min(remaining);
        let count = self.reader.read(&mut buffer[..allowed])?;
        self.bytes += count;
        Ok(count)
    }
}
impl<R: Seek> Seek for Budget<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.reader.seek(position)
    }
}

pub(crate) fn parse(
    path: &str,
    config: &AudioConfig,
    cancel: CancellationToken,
) -> Result<Js, AudioError> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(config.allocation_limit)
            .use_custom_resolvers(false),
    );
    if !std::fs::metadata(path)
        .map_err(|_| AudioError::Input)?
        .is_file()
    {
        return Err(AudioError::Input);
    }
    let file = File::open(path).map_err(|_| AudioError::Input)?;
    if !file.metadata().map_err(|_| AudioError::Input)?.is_file() {
        return Err(AudioError::Input);
    }
    let mut reader = Budget::new(file, config, cancel);
    let result = parse_reader(&mut reader, config);
    finish_parse(reader, result, config)
}
pub(crate) fn parse_opened(
    file: &mut File,
    source_name: &str,
    hint: FormatHint,
    config: &AudioConfig,
    shutdown: CancellationToken,
    cancel: CancellationToken,
) -> Result<Js, AudioError> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(config.allocation_limit)
            .use_custom_resolvers(false),
    );
    let expected = hint.expected(source_name)?;
    let mut reader = Budget::new(file, config, shutdown);
    reader.external_cancel = Some(cancel);
    let result = reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| AudioError::Input)
        .and_then(|_| parse_reader_hint(&mut reader, config, expected));
    finish_parse(reader, result, config)
}
fn finish_parse<R>(
    reader: Budget<R>,
    result: Result<Js, AudioError>,
    config: &AudioConfig,
) -> Result<Js, AudioError> {
    if reader.cancelled || reader.is_cancelled() {
        return Err(AudioError::Cancelled);
    }
    if reader.exceeded || Instant::now() >= reader.deadline {
        return Err(AudioError::Resource("read/seek/deadline budget".into()));
    }
    let common = result?;
    if let Js::Object(fields) = &common {
        if fields.len() > config.tag_limit {
            return Err(AudioError::Resource("tag count".into()));
        }
    }
    Ok(common)
}
fn parse_reader<R: Read + Seek>(reader: &mut R, config: &AudioConfig) -> Result<Js, AudioError> {
    parse_reader_hint(reader, config, None)
}
fn parse_reader_hint<R: Read + Seek>(
    reader: &mut R,
    config: &AudioConfig,
    expected: Option<AudioFormat>,
) -> Result<Js, AudioError> {
    let text_plan = crate::preflight::prefix(reader, config)?;
    let mut view = text_plan.view(reader);
    let reader = &mut view;
    let file_type = Probe::new(&mut *reader)
        .guess_file_type()
        .map_err(parser_error)?
        .file_type()
        .ok_or(AudioError::Input)?;
    let actual = match file_type {
        FileType::Mpeg => Some(AudioFormat::Mpeg),
        FileType::Flac => Some(AudioFormat::Flac),
        FileType::Vorbis | FileType::Opus => Some(AudioFormat::Ogg),
        FileType::Mp4 => Some(AudioFormat::Mp4),
        FileType::Wav => Some(AudioFormat::Wav),
        _ => None,
    };
    if expected.is_some() && expected != actual {
        return Err(AudioError::UnsupportedVariant(
            "source format hint/content mismatch".into(),
        ));
    }
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| AudioError::Input)?;
    let flac_comments = crate::preflight::format(reader, file_type, config)?;
    let options = ParseOptions::new()
        .read_properties(false)
        .read_cover_art(false)
        .implicit_conversions(false);
    let mut collector = Collector::new(config.tag_limit);
    match file_type {
        FileType::Mpeg => {
            let file = lofty::mpeg::MpegFile::read_from(reader, options).map_err(parser_error)?;
            if file.ape().is_some() || file.id3v1().is_some() {
                return Err(AudioError::UnsupportedVariant(
                    "MP3 APE/ID3v1 tag priority".into(),
                ));
            }
            if let Some(tag) = file.id3v2() {
                id3(&mut collector, tag, &text_plan)?;
            }
        }
        FileType::Flac => {
            let file = lofty::flac::FlacFile::read_from(reader, options).map_err(parser_error)?;
            if let Some(tag) = file.id3v2() {
                id3(&mut collector, tag, &text_plan)?;
            }
            // Lofty exposes only the last comment block. The source accumulates
            // every physical block/item after leading ID3, at Vorbis priority.
            flac_comments.collect(&mut collector);
        }
        FileType::Vorbis => {
            let file = lofty::ogg::VorbisFile::read_from(reader, options).map_err(parser_error)?;
            for (key, value) in file.vorbis_comments().items() {
                collector.add("vorbis", &key.to_uppercase(), value.into());
            }
        }
        FileType::Opus => {
            let file = lofty::ogg::OpusFile::read_from(reader, options).map_err(parser_error)?;
            for (key, value) in file.vorbis_comments().items() {
                collector.add("vorbis", &key.to_uppercase(), value.into());
            }
        }
        FileType::Mp4 => {
            let file = lofty::mp4::Mp4File::read_from(reader, options).map_err(parser_error)?;
            if let Some(tag) = file.ilst() {
                for atom in tag {
                    let id = match atom.ident() {
                        AtomIdent::Fourcc(bytes) => {
                            bytes.iter().map(|byte| char::from(*byte)).collect()
                        }
                        AtomIdent::Freeform { mean, name } => format!("----:{mean}:{name}"),
                    };
                    for value in atom.data() {
                        let value = match value {
                            AtomData::UTF8(text) | AtomData::UTF16(text) => {
                                Js::String(text.clone())
                            }
                            AtomData::SignedInteger(n) => Js::Number(*n as f64),
                            AtomData::UnsignedInteger(n) => Js::Number(*n as f64),
                            AtomData::Bool(b) => Js::Number(if *b { 1.0 } else { 0.0 }),
                            AtomData::Picture(_) => continue,
                            AtomData::Unknown { data, .. }
                                if matches!(id.as_str(), "trkn" | "disk") && data.len() >= 6 =>
                            {
                                Js::String(format!(
                                    "{}/{}",
                                    u16::from_be_bytes([data[2], data[3]]),
                                    u16::from_be_bytes([data[4], data[5]])
                                ))
                            }
                            AtomData::Unknown { data, .. } if id == "gnre" && data.len() >= 2 => {
                                let index = u16::from_be_bytes([data[0], data[1]]) as usize;
                                common::schema()["genres"]
                                    .get(index.saturating_sub(1))
                                    .and_then(|v| v.as_str())
                                    .map(Js::from)
                                    .unwrap_or(Js::Undefined)
                            }
                            _ => {
                                if common::schema()["maps"]["mp4"]
                                    .get(id.to_uppercase())
                                    .is_some()
                                {
                                    return Err(AudioError::UnsupportedVariant(format!(
                                        "MP4 atom {id} data type"
                                    )));
                                }
                                continue;
                            }
                        };
                        collector.add("mp4", &id, value);
                    }
                }
            }
        }
        FileType::Wav => {
            let file =
                lofty::iff::wav::WavFile::read_from(reader, options).map_err(parser_error)?;
            if let Some(tag) = file.riff_info() {
                for (key, value) in tag {
                    let ascii: String = value
                        .as_bytes()
                        .iter()
                        .map(|byte| char::from(byte & 0x7f))
                        .collect();
                    collector.add("riff", key, Js::String(ascii));
                }
            }
            if let Some(tag) = file.id3v2() {
                id3(&mut collector, tag, &text_plan)?;
            }
        }
        _ => {
            return Err(AudioError::UnsupportedVariant(format!(
                "file format {file_type:?}"
            )))
        }
    }
    if collector.exceeded {
        return Err(AudioError::Resource("tag count".into()));
    }
    Ok(collector.common)
}
fn parser_error(error: impl std::fmt::Display) -> AudioError {
    let message = error.to_string();
    if message.to_lowercase().contains("allocat") {
        AudioError::Resource("declared tag allocation".into())
    } else if message.to_lowercase().contains("compress") {
        AudioError::UnsupportedVariant("compressed ID3 frames".into())
    } else {
        AudioError::Input
    }
}
fn id3(
    collector: &mut Collector,
    tag: &Id3v2Tag,
    text_plan: &crate::id3_text::Plan,
) -> Result<(), AudioError> {
    if tag.original_version() == Id3v2Version::V2 {
        return Err(AudioError::UnsupportedVariant("ID3v2.2".into()));
    }
    for frame in tag {
        let restored = text_plan.original(frame.id_str());
        let recovered = restored != frame.id_str();
        let mut id = restored.to_owned();
        if id == "PCST" {
            return Err(AudioError::UnsupportedVariant(
                "ID3 podcast flag frame semantics".into(),
            ));
        }
        let values = match frame {
            Frame::Text(text) => {
                let text = text.value.trim_end_matches('\0');
                let split = if matches!(id.as_str(), "TRCK" | "TPOS")
                    || (recovered && tag.original_version() == Id3v2Version::V3)
                {
                    vec![text]
                } else if tag.original_version() == Id3v2Version::V3
                    && matches!(
                        id.as_str(),
                        "TCOM" | "TEXT" | "TOLY" | "TOPE" | "TPE1" | "TSRC" | "TCON"
                    )
                    && !text.contains('\0')
                {
                    text.split('/').collect()
                } else {
                    text.split('\0').collect()
                };
                split
                    .into_iter()
                    .flat_map(|value| {
                        let value = if recovered && tag.original_version() == Id3v2Version::V3 {
                            value
                        } else {
                            value.trim_matches(js::whitespace)
                        };
                        if id == "TCON" {
                            common::genre(value)
                        } else {
                            vec![value.into()]
                        }
                    })
                    .collect::<Vec<_>>()
            }
            Frame::UserText(text) => {
                id = format!("TXXX:{}", text.description);
                text.content
                    .trim_end_matches('\0')
                    .split('\0')
                    .map(|value| Js::String(value.trim_matches(js::whitespace).into()))
                    .collect()
            }
            Frame::Timestamp(timestamp) => vec![Js::String(timestamp.timestamp.to_string())],
            Frame::Comment(comment) => vec![Js::String(comment.content.clone().into_owned())],
            Frame::UnsynchronizedText(text) => {
                id = "USLT:description".into();
                vec![Js::String(text.content.clone().into_owned())]
            }
            Frame::Url(url) => vec![Js::String(url.url().to_owned())],
            Frame::Picture(_) => continue,
            _ => {
                if common::schema()["maps"]["id3"]
                    .as_object()
                    .is_some_and(|map| {
                        map.keys().any(|key| {
                            key == &id.to_uppercase()
                                || key.starts_with(&format!("{}:", id.to_uppercase()))
                        })
                    })
                {
                    return Err(AudioError::UnsupportedVariant(format!("ID3 frame {id}")));
                }
                continue;
            }
        };
        for value in values {
            collector.add("id3", &id, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn reads_seeks_deadlines_and_cancellation_are_cooperatively_bounded() {
        let config = AudioConfig {
            read_limit: 3,
            operation_limit: 2,
            ..AudioConfig::default()
        };
        let token = CancellationToken::new();
        let mut reader = Budget::new(Cursor::new(vec![0; 20]), &config, token.clone());
        let mut bytes = [0; 8];
        assert_eq!(reader.read(&mut bytes).unwrap(), 3);
        assert!(reader.read(&mut bytes).is_err());
        assert!(reader.exceeded);
        let mut reader = Budget::new(Cursor::new(vec![0; 20]), &config, token.clone());
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert!(reader.seek(SeekFrom::Start(0)).is_err());
        let mut reader = Budget::new(Cursor::new(vec![0; 20]), &config, token.clone());
        token.cancel();
        assert!(reader.read(&mut bytes).is_err());
        assert!(reader.cancelled);
        let mut reader = Budget::new(Cursor::new(vec![0; 20]), &config, CancellationToken::new());
        reader.deadline = Instant::now() - std::time::Duration::from_millis(1);
        assert!(reader.read(&mut bytes).is_err());
        assert!(reader.exceeded);
    }
    #[test]
    fn cancellation_can_stop_a_native_parse_at_the_next_read() {
        struct SlowRead {
            cursor: Cursor<Vec<u8>>,
            ready: std::sync::mpsc::Sender<()>,
        }
        impl Read for SlowRead {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let _ = self.ready.send(());
                std::thread::sleep(std::time::Duration::from_millis(30));
                self.cursor.read(bytes)
            }
        }
        impl Seek for SlowRead {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.cursor.seek(position)
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            let config = AudioConfig::default();
            let mut reader = Budget::new(
                SlowRead {
                    cursor: Cursor::new(vec![0; 100]),
                    ready: tx,
                },
                &config,
                worker_token,
            );
            let _ = parse_reader(&mut reader, &config);
            reader.cancelled
        });
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        token.cancel();
        assert!(worker.join().unwrap());
    }
}
