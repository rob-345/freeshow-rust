use crate::{Error, Result};
pub const OPUS_HEAD: &[u8] = &[
    0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 1, 2, 0x38, 1, 0x80, 0xbb, 0, 0, 0, 0, 0,
];
pub const SILENCE: &[u8] = &[0xf8, 0xff, 0xfe];
const fn table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x80000000 != 0 {
                (r << 1) ^ 0x04c11db7
            } else {
                r << 1
            };
            j += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
}
const CRC: [u32; 256] = table();
pub fn crc32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |crc, b| {
        (crc << 8) ^ CRC[((crc >> 24) as u8 ^ b) as usize]
    })
}
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
pub fn opus_tags(song: &str) -> Result<Vec<u8>> {
    if song.len() > 32768 {
        return Err(Error::new(
            "resource_limit",
            "Song title exceeds32768 bytes",
        ));
    }
    let (artist, title) = song
        .split_once(" - ")
        .map_or(("", song), |(a, t)| (js_trim(a), js_trim(t)));
    let mut comments = Vec::new();
    if !title.is_empty() {
        comments.push(format!("TITLE={title}"));
    }
    if !artist.is_empty() {
        comments.push(format!("ARTIST={artist}"));
    }
    let mut out = b"OpusTags".to_vec();
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(b"FreeShow");
    out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for comment in comments {
        out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        out.extend_from_slice(comment.as_bytes());
    }
    Ok(out)
}
#[derive(Debug)]
pub struct OggState {
    serial: u32,
    sequence: u32,
    granule: u64,
}
impl OggState {
    pub fn new(serial: u32) -> Self {
        Self {
            serial,
            sequence: 0,
            granule: 0,
        }
    }
    pub fn serial(&self) -> u32 {
        self.serial
    }
    pub fn sequence(&self) -> u32 {
        self.sequence
    }
    pub fn granule(&self) -> u64 {
        self.granule
    }
    pub fn page(&mut self, packet: &[u8], flags: u8, granule: u64) -> Result<Vec<u8>> {
        if packet.len() > 65024 || self.sequence > i32::MAX as u32 {
            return Err(Error::new(
                "resource_limit",
                "Source Ogg single-page/sequence limit",
            ));
        }
        let segments = packet.len() / 255 + 1;
        let mut out = vec![0u8; 27 + segments + packet.len()];
        out[..4].copy_from_slice(b"OggS");
        out[5] = flags;
        out[6..14].copy_from_slice(&granule.to_le_bytes());
        out[14..18].copy_from_slice(&self.serial.to_le_bytes());
        out[18..22].copy_from_slice(&self.sequence.to_le_bytes());
        self.sequence += 1;
        out[26] = segments as u8;
        out[27..27 + segments - 1].fill(255);
        out[26 + segments] = (packet.len() % 255) as u8;
        out[27 + segments..].copy_from_slice(packet);
        let crc = crc32(&out);
        out[22..26].copy_from_slice(&crc.to_le_bytes());
        Ok(out)
    }
    pub fn audio(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        self.granule = self
            .granule
            .checked_add(960)
            .ok_or_else(|| Error::new("resource_limit", "Ogg granule overflow"))?;
        self.page(packet, 0, self.granule)
    }
    pub fn headers(&mut self, song: &str) -> Result<Vec<u8>> {
        let mut bytes = self.page(OPUS_HEAD, 2, 0)?;
        bytes.extend(self.page(&opus_tags(song)?, 0, 0)?);
        Ok(bytes)
    }
    pub fn eos(&mut self) -> Result<Vec<u8>> {
        self.page(&[], 4, self.granule)
    }
}
