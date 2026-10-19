use crate::*;
use flate2::{Decompress, FlushDecompress, Status};
use std::io::Write;
struct Input<'a> {
    source: tokio::sync::mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    at: usize,
    control: &'a Control,
    config: &'a Config,
    steps: u64,
}
impl Input<'_> {
    fn step(&mut self) -> Result<()> {
        self.control.check()?;
        self.steps += 1;
        if self.steps > self.config.gzip_steps {
            return Err(Error::limit());
        }
        Ok(())
    }
    fn fill(&mut self) -> Result<bool> {
        self.step()?;
        while self.at == self.current.len() {
            let next = self.source.blocking_recv();
            self.control.check()?;
            let Some(bytes) = next else {
                return Ok(false);
            };
            if bytes.len() > CHUNK {
                return Err(Error::limit());
            }
            self.current = bytes;
            self.at = 0;
        }
        Ok(true)
    }
    fn byte(&mut self) -> Result<u8> {
        self.step()?;
        if !self.fill()? {
            return Err(Error::source("unexpected end of file"));
        }
        let byte = self.current[self.at];
        self.at += 1;
        Ok(byte)
    }
    fn header(&mut self, count: &mut usize, crc: &mut crc32fast::Hasher) -> Result<u8> {
        if *count == self.config.gzip_header_bytes {
            return Err(Error::limit());
        }
        *count += 1;
        let byte = self.byte()?;
        crc.update(&[byte]);
        Ok(byte)
    }
    fn tail_u32(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        for byte in &mut bytes {
            *byte = self.byte()?;
        }
        Ok(u32::from_le_bytes(bytes))
    }
}
pub(crate) fn decode(
    source: tokio::sync::mpsc::Receiver<Vec<u8>>,
    output: &mut impl Write,
    control: &Control,
    config: &Config,
) -> Result<TransferStats> {
    let mut input = Input {
        source,
        current: Vec::new(),
        at: 0,
        control,
        config,
        steps: 0,
    };
    let mut stats = TransferStats::default();
    let mut first = true;
    loop {
        input.step()?;
        if !first {
            if !input.fill()? {
                break;
            }
            // Node's nonzero data after zero padding behaves differently across
            // chunk boundaries. Certify zero padding only, with no guessed tail.
            if input.current[input.at] == 0 {
                while input.fill()? {
                    if input.current[input.at..].iter().any(|byte| *byte != 0) {
                        return Err(Error::new(
                            "download_gzip_padding",
                            "Nonzero data after gzip zero padding is unsupported",
                        ));
                    }
                    input.at = input.current.len();
                }
                break;
            }
        }
        first = false;
        if stats.members == config.gzip_members {
            return Err(Error::limit());
        }
        stats.members += 1;
        let mut header = crc32fast::Hasher::new();
        let mut count = 0;
        let one = input.header(&mut count, &mut header)?;
        let two = input.header(&mut count, &mut header)?;
        if [one, two] != [0x1f, 0x8b] {
            return Err(Error::source("incorrect header check"));
        }
        if input.header(&mut count, &mut header)? != 8 {
            return Err(Error::source("unknown compression method"));
        }
        let flags = input.header(&mut count, &mut header)?;
        if flags & 0xe0 != 0 {
            return Err(Error::source("unknown header flags set"));
        }
        for _ in 0..6 {
            input.header(&mut count, &mut header)?;
        }
        if flags & 4 != 0 {
            let low = input.header(&mut count, &mut header)?;
            let high = input.header(&mut count, &mut header)?;
            for _ in 0..u16::from_le_bytes([low, high]) {
                input.header(&mut count, &mut header)?;
            }
        }
        for flag in [8, 16] {
            if flags & flag != 0 {
                while input.header(&mut count, &mut header)? != 0 {}
            }
        }
        if flags & 2 != 0 {
            if count + 2 > config.gzip_header_bytes {
                return Err(Error::limit());
            }
            let low = input.byte()?;
            let high = input.byte()?;
            if u16::from_le_bytes([low, high]) != header.finalize() as u16 {
                return Err(Error::source("header crc mismatch"));
            }
        }
        let mut decoder = Decompress::new(false);
        let mut crc = crc32fast::Hasher::new();
        let mut member_bytes = 0u64;
        let mut chunk = [0u8; CHUNK];
        loop {
            input.step()?;
            if !input.fill()? {
                return Err(Error::source("unexpected end of file"));
            }
            let before_in = decoder.total_in();
            let before_out = decoder.total_out();
            let status = decoder
                .decompress(
                    &input.current[input.at..],
                    &mut chunk,
                    FlushDecompress::None,
                )
                .map_err(|_| Error::source("invalid compressed data"))?;
            let used = (decoder.total_in() - before_in) as usize;
            let produced = (decoder.total_out() - before_out) as usize;
            input.at += used;
            if produced > 0 {
                if produced as u64
                    > config
                        .uncompressed_bytes
                        .saturating_sub(stats.uncompressed_bytes)
                {
                    return Err(Error::limit());
                }
                control.check()?;
                output.write_all(&chunk[..produced]).map_err(|_| {
                    Error::new(
                        "download_filesystem",
                        "Writing private download artifact failed",
                    )
                })?;
                control.check()?;
                crc.update(&chunk[..produced]);
                member_bytes += produced as u64;
                stats.uncompressed_bytes += produced as u64;
            }
            if status == Status::StreamEnd {
                break;
            }
            if used == 0 && produced == 0 {
                return Err(Error::source("invalid compressed data"));
            }
        }
        if input.tail_u32()? != crc.finalize() {
            return Err(Error::source("incorrect data check"));
        }
        if input.tail_u32()? != member_bytes as u32 {
            return Err(Error::source("incorrect length check"));
        }
    }
    control.check()?;
    Ok(stats)
}
