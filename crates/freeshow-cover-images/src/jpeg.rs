//! Narrow JPEG subset, validated before any native decoder allocation.
use crate::{service::Check, *};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Info {
    width: u32,
    height: u32,
    components: u8,
    subsampled: bool,
}
#[derive(Clone, Copy)]
struct Huffman {
    counts: [u8; 16],
    values: [u8; 162],
}
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    byte: u8,
    remaining: u8,
}
impl Bits<'_> {
    fn bit(&mut self) -> Result<u32> {
        if self.remaining == 0 {
            self.byte = *self
                .data
                .get(self.position)
                .ok_or_else(|| invalid("JPEG entropy ends before the declared raster"))?;
            self.position += 1;
            if self.byte == 255 {
                if self.data.get(self.position) != Some(&0) {
                    return Err(invalid("JPEG entropy byte stuffing"));
                }
                self.position += 1;
            }
            self.remaining = 8;
        }
        self.remaining -= 1;
        Ok(((self.byte >> self.remaining) & 1) as u32)
    }
    fn skip(&mut self, count: u8) -> Result<()> {
        for _ in 0..count {
            self.bit()?;
        }
        Ok(())
    }
}
impl Huffman {
    fn symbol(&self, bits: &mut Bits<'_>) -> Result<u8> {
        let (mut code, mut first, mut index) = (0u32, 0u32, 0usize);
        for count in self.counts {
            code = (code << 1) | bits.bit()?;
            if code >= first && code - first < count as u32 {
                return Ok(self.values[index + (code - first) as usize]);
            }
            first = (first + count as u32) << 1;
            index += count as usize;
        }
        Err(invalid("Invalid JPEG Huffman entropy code"))
    }
}
fn entropy(
    data: &[u8],
    info: Info,
    huffman: &[[Option<Huffman>; 2]; 2],
    check: &impl Fn() -> Result<()>,
) -> Result<()> {
    let side = if info.subsampled { 16 } else { 8 };
    let mcus = info.width.div_ceil(side) * info.height.div_ceil(side);
    let mut bits = Bits {
        data,
        position: 0,
        byte: 0,
        remaining: 0,
    };
    for mcu in 0..mcus {
        if mcu % 256 == 0 {
            check()?;
        }
        for component in 0..info.components {
            let table = usize::from(component != 0);
            let dc = huffman[0][table].as_ref().unwrap();
            let ac = huffman[1][table].as_ref().unwrap();
            let blocks = if component == 0 && info.subsampled {
                4
            } else {
                1
            };
            for _ in 0..blocks {
                let size = dc.symbol(&mut bits)?;
                bits.skip(size)?;
                let mut coefficient = 1usize;
                while coefficient < 64 {
                    let symbol = ac.symbol(&mut bits)?;
                    if symbol == 0 {
                        break;
                    }
                    if symbol == 0xf0 {
                        coefficient += 16;
                        if coefficient > 64 {
                            return Err(invalid("JPEG zero run exceeds block"));
                        }
                        continue;
                    }
                    coefficient += (symbol >> 4) as usize;
                    if coefficient >= 64 {
                        return Err(invalid("JPEG AC run exceeds block"));
                    }
                    bits.skip(symbol & 15)?;
                    coefficient += 1;
                }
            }
        }
    }
    let padding = ((1u16 << bits.remaining) - 1) as u8;
    if bits.position != data.len() || bits.byte & padding != padding {
        return Err(invalid("JPEG entropy trailing bytes/noncanonical padding"));
    }
    check()
}
const EXIF: &[u8; 32] = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\0\0\0\0\0\0\0\0";
fn preflight(bytes: &[u8], check: &impl Fn() -> Result<()>) -> Result<Info> {
    check()?;
    if bytes.len() > MAX_ENCODED {
        return Err(resource("JPEG encoded byte limit"));
    }
    if !bytes.starts_with(&[255, 216]) {
        return Err(invalid("Missing JPEG SOI"));
    }
    let (mut offset, mut count) = (2usize, 0usize);
    let mut frame = None;
    let (mut jfif, mut exif, mut icc) = (false, false, false);
    let mut quant = [false; 2];
    let mut huffman = [[None; 2]; 2];
    while offset < bytes.len() {
        check()?;
        count += 1;
        if count > 256 {
            return Err(resource("JPEG segment limit"));
        }
        let header = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("Truncated JPEG marker"))?;
        if header[0] != 255 {
            return Err(invalid("Invalid JPEG marker prefix"));
        }
        let length = u16::from_be_bytes([header[2], header[3]]) as usize;
        if length < 2 {
            return Err(invalid("Invalid JPEG marker length"));
        }
        let end = offset + 2 + length;
        let data = bytes
            .get(offset + 4..end)
            .ok_or_else(|| invalid("Truncated JPEG segment"))?;
        match header[1] {
            0xe0 => {
                if jfif
                    || !matches!(
                        data,
                        b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0" | b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"
                    )
                {
                    return Err(unsupported("JPEG requires the checked JFIF header"));
                }
                jfif = true;
            }
            0xe1 => {
                if exif || data.len() != EXIF.len() || !matches!(data[24], 2 | 6 | 8) {
                    return Err(unsupported("JPEG EXIF variant is not certified"));
                }
                let mut expected = *EXIF;
                expected[24] = data[24];
                if data != expected {
                    return Err(unsupported("JPEG EXIF variant is not certified"));
                }
                exif = true;
            }
            0xe2 => {
                if icc
                    || data.len() != 470
                    || format!("{:x}", Sha256::digest(data))
                        != "c3bb12de30d7357252ec3a5ec781bd2f8a6dd8c69dd7d3de97bbac262d9e1fd4"
                {
                    return Err(unsupported("JPEG ICC/color profile is not certified"));
                }
                icc = true;
            }
            0xc0 => {
                if frame.is_some() || data.len() < 6 {
                    return Err(invalid("Invalid or repeated JPEG frame"));
                }
                let mut info = Info {
                    width: u16::from_be_bytes([data[3], data[4]]) as u32,
                    height: u16::from_be_bytes([data[1], data[2]]) as u32,
                    components: data[5],
                    subsampled: false,
                };
                if info.width == 0
                    || info.height == 0
                    || info.width > MAX_SIDE
                    || info.height > MAX_SIDE
                    || info.width as usize * info.height as usize > MAX_PIXELS
                {
                    return Err(resource("JPEG exceeds 4096 side/4M pixel bound"));
                }
                if data[0] != 8
                    || !matches!(
                        &data[6..],
                        [1, 0x11, 0]
                            | [1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]
                            | [1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]
                    )
                    || !matches!((info.components, data.len()), (1, 9) | (3, 15))
                {
                    return Err(unsupported(
                        "JPEG requires checked 8-bit gray or JFIF YCbCr 4:4:4/4:2:0 components",
                    ));
                }
                info.subsampled = data[7] == 0x22;
                frame = Some(info);
            }
            0xdb => {
                let mut p = 0;
                while p < data.len() {
                    let table = data[p];
                    if table > 1 {
                        return Err(unsupported("JPEG requires 8-bit quantization tables 0/1"));
                    }
                    if quant[table as usize] {
                        return Err(unsupported("JPEG repeated quantization table"));
                    }
                    let values = data
                        .get(p + 1..p + 65)
                        .ok_or_else(|| invalid("Truncated JPEG quantization table"))?;
                    if values.contains(&0) {
                        return Err(invalid("Zero JPEG quantizer"));
                    }
                    quant[table as usize] = true;
                    p += 65;
                }
            }
            0xc4 => {
                let mut p = 0;
                while p < data.len() {
                    let table = data
                        .get(p..p + 17)
                        .ok_or_else(|| invalid("Truncated JPEG Huffman table"))?;
                    if !matches!(table[0], 0 | 1 | 0x10 | 0x11) {
                        return Err(unsupported("JPEG Huffman table variant is not certified"));
                    }
                    let (class, id) = ((table[0] >> 4) as usize, (table[0] & 15) as usize);
                    if huffman[class][id].is_some() {
                        return Err(unsupported("JPEG repeated Huffman table"));
                    }
                    let mut slots = 1u32;
                    let mut symbols = 0usize;
                    for count in &table[1..] {
                        slots *= 2;
                        if *count as u32 > slots {
                            return Err(invalid("Oversubscribed JPEG Huffman table"));
                        }
                        slots -= *count as u32;
                        symbols += *count as usize;
                    }
                    if slots == 0 || symbols == 0 || symbols > if class == 0 { 12 } else { 162 } {
                        return Err(invalid("Invalid JPEG Huffman symbol count"));
                    }
                    let values = data
                        .get(p + 17..p + 17 + symbols)
                        .ok_or_else(|| invalid("Truncated JPEG Huffman values"))?;
                    let mut seen = [false; 256];
                    for value in values {
                        if seen[*value as usize]
                            || (class == 0 && *value > 11)
                            || (class == 1
                                && !matches!(*value, 0 | 0xf0)
                                && !matches!(*value & 15, 1..=10))
                        {
                            return Err(invalid("Invalid JPEG Huffman symbol"));
                        }
                        seen[*value as usize] = true;
                    }
                    let mut decoded = Huffman {
                        counts: table[1..].try_into().unwrap(),
                        values: [0; 162],
                    };
                    decoded.values[..symbols].copy_from_slice(values);
                    huffman[class][id] = Some(decoded);
                    p += 17 + symbols;
                }
            }
            0xda => {
                let info = frame.ok_or_else(|| invalid("JPEG missing frame"))?;
                let expected: &[u8] = if info.components == 1 {
                    &[1, 1, 0, 0, 63, 0]
                } else {
                    &[3, 1, 0, 2, 0x11, 3, 0x11, 0, 63, 0]
                };
                if data != expected || !jfif {
                    return Err(unsupported(
                        "JPEG requires one checked interleaved baseline scan",
                    ));
                }
                if !quant[0]
                    || huffman[0][0].is_none()
                    || huffman[1][0].is_none()
                    || (info.components == 3
                        && (!quant[1] || huffman[0][1].is_none() || huffman[1][1].is_none()))
                {
                    return Err(invalid("JPEG scan references missing tables"));
                }
                let (mut p, mut next_check) = (end, end);
                while p < bytes.len() {
                    if p >= next_check {
                        check()?;
                        next_check = p + 65536;
                    }
                    if bytes[p] != 255 {
                        p += 1;
                        continue;
                    }
                    match bytes.get(p + 1) {
                        Some(0) => p += 2,
                        Some(0xd9) if p + 2 == bytes.len() && p > end => {
                            entropy(&bytes[end..p], info, &huffman, check)?;
                            return Ok(info);
                        }
                        Some(0xd9) => return Err(invalid("JPEG empty scan/trailing data")),
                        Some(_) => return Err(unsupported(
                            "JPEG multiple scans/restarts/entropy marker variant is not certified",
                        )),
                        None => break,
                    }
                }
                return Err(invalid("JPEG truncated entropy/EOI"));
            }
            _ => {
                return Err(unsupported(
                    "JPEG marker/metadata/codec variant is not certified",
                ))
            }
        }
        offset = end;
    }
    Err(invalid("JPEG missing scan"))
}

pub(crate) fn decode(bytes: &[u8], check: &Check) -> Result<(u32, u32, Vec<u8>)> {
    let info = preflight(bytes, &|| check.check())?;
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        native::verified(check)?;
        let rgba = native::read(bytes, info, check)?;
        Ok((info.width, info.height, rgba))
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = info;
        Err(unsupported(
            "JPEG native consumer is verified only on Linux x64",
        ))
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn hold_after_native_write(gate: Arc<crate::tests::Gate>) {
    native::hold_after_write(gate);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod native {
    use super::*;
    use gdk_pixbuf::{prelude::*, Colorspace, PixbufError, PixbufLoader};
    use std::cell::Cell;
    // Public immutable scalar API from 2.42.10 gdk-pixbuf-features.h:
    // GDK_PIXBUF_VAR const guint. gtk-rs omits these globals.
    unsafe extern "C" {
        static gdk_pixbuf_major_version: libc::c_uint;
        static gdk_pixbuf_minor_version: libc::c_uint;
        static gdk_pixbuf_micro_version: libc::c_uint;
    }
    thread_local! {
        // These TLS cells belong to the existing fixed owners, not UI threads.
        static VERIFIED: Cell<Option<bool>> = const { Cell::new(None) };
    }
    #[cfg(test)]
    static AFTER_WRITE: std::sync::Mutex<Option<Arc<crate::tests::Gate>>> =
        std::sync::Mutex::new(None);
    #[cfg(test)]
    pub(super) fn hold_after_write(gate: Arc<crate::tests::Gate>) {
        assert!(AFTER_WRITE.lock().unwrap().replace(gate).is_none());
    }
    pub(super) fn verified(check: &Check) -> Result<()> {
        // SAFETY: immutable exported guint globals from the linked C API.
        // No struct layouts, renderer symbols, pointers or lookup paths.
        let version = unsafe {
            (
                gdk_pixbuf_major_version,
                gdk_pixbuf_minor_version,
                gdk_pixbuf_micro_version,
            )
        };
        if version != (2, 42, 10) {
            return Err(unsupported(
                "Native JPEG requires verified GdkPixbuf 2.42.10",
            ));
        }
        match VERIFIED.get() {
            Some(true) => return Ok(()),
            Some(false) => {
                return Err(unsupported(
                    "Installed JPEG consumer failed source verification",
                ))
            }
            None => {}
        }
        let result = (|| {
            for (bytes, expected) in [
                (
                    include_bytes!("../fixtures/jpeg/texture-17x11-no-icc.jpg").as_slice(),
                    "16aaaea16ec707cdfd1a7a963e6bafe1737a81dfdbfd0992002f23ac9a3615ce",
                ),
                (
                    include_bytes!("../fixtures/jpeg/native-encoded-gray.jpg").as_slice(),
                    "8f328d193198764155deda97740d4fd870cb7af3f3906c61b4a62099f75236a1",
                ),
            ] {
                let info = preflight(bytes, &|| check.check())?;
                let rgba = read_inner(bytes, info, check, true)?;
                if format!("{:x}", Sha256::digest(&rgba)) != expected {
                    return Err(unsupported(
                        "Installed JPEG consumer pixels differ from the actual source",
                    ));
                }
            }
            check.check()
        })();
        match result {
            Ok(()) => {
                VERIFIED.set(Some(true));
                Ok(())
            }
            Err(error)
                if matches!(
                    error.code,
                    "cover_cancelled" | "cover_deadline" | "cover_resource_limit"
                ) =>
            {
                Err(error)
            }
            Err(error) => {
                VERIFIED.set(Some(false));
                Err(unsupported(format!(
                    "Installed JPEG consumer failed source verification: {error}"
                )))
            }
        }
    }
    fn failure(error: gdk_pixbuf::glib::Error) -> Error {
        if error.matches(PixbufError::InsufficientMemory) {
            resource(format!("Native JPEG allocation failed: {error}"))
        } else {
            invalid(format!("Native JPEG decode failed: {error}"))
        }
    }
    struct Loader {
        object: PixbufLoader,
        closed: bool,
    }
    impl Loader {
        fn close(&mut self) -> Result<()> {
            let result = self.object.close().map_err(failure);
            self.closed = true;
            result
        }
    }
    impl Drop for Loader {
        fn drop(&mut self) {
            if !self.closed {
                // The native close/destroy completes here on the same owner,
                // even if caller cancellation interrupted cooperative feeding.
                let _ = self.object.close();
            }
        }
    }
    pub(super) fn read(bytes: &[u8], info: Info, check: &Check) -> Result<Vec<u8>> {
        read_inner(bytes, info, check, false)
    }
    fn read_inner(bytes: &[u8], info: Info, check: &Check, _canary: bool) -> Result<Vec<u8>> {
        check.check()?;
        let mut loader = Loader {
            object: PixbufLoader::with_type("jpeg")
                .map_err(|e| unsupported(format!("Native JPEG consumer unavailable: {e}")))?,
            closed: false,
        };
        for chunk in bytes.chunks(32768) {
            check.check()?;
            loader.object.write(chunk).map_err(failure)?;
            #[cfg(test)]
            if !_canary {
                let gate = AFTER_WRITE.lock().unwrap().take();
                if let Some(gate) = gate {
                    gate.block();
                }
            }
        }
        loader.close()?;
        check.check()?;
        let image = loader
            .object
            .pixbuf()
            .ok_or_else(|| invalid("Native JPEG has no raster"))?;
        if image.width() != info.width as i32
            || image.height() != info.height as i32
            || image.n_channels() != 3
            || image.has_alpha()
            || image.bits_per_sample() != 8
            || image.colorspace() != Colorspace::Rgb
        {
            return Err(unsupported(
                "Native JPEG raster/color/dimensions differ from preflight",
            ));
        }
        let row_bytes = info.width as usize * 3;
        let stride =
            usize::try_from(image.rowstride()).map_err(|_| resource("Native JPEG stride"))?;
        if stride < row_bytes || stride > row_bytes + 3 {
            return Err(resource("Native JPEG row layout"));
        }
        let pixels = image.read_pixel_bytes();
        if pixels.len() < stride * (info.height as usize - 1) + row_bytes
            || pixels.len() > stride * info.height as usize
        {
            return Err(resource("Native JPEG buffer length"));
        }
        let mut rgba = Vec::with_capacity(info.width as usize * info.height as usize * 4);
        for y in 0..info.height as usize {
            check.check()?;
            for rgb in pixels[y * stride..y * stride + row_bytes]
                .as_chunks::<3>()
                .0
            {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        check.check()?;
        Ok(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        include_bytes!("../fixtures/jpeg/texture-17x11-no-icc.jpg").to_vec()
    }
    #[test]
    fn every_source_fixture_header_is_supported_and_work_checks_are_live() {
        let receipt: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/jpeg/electron-linux.json")).unwrap();
        use base64::Engine;
        for f in receipt["fixtures"].as_array().unwrap() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(f["input"]["base64"].as_str().unwrap())
                .unwrap();
            let info = preflight(&bytes, &|| Ok(())).unwrap();
            assert_eq!(
                info.width as u64,
                f["source"]["size"]["width"].as_u64().unwrap()
            );
            assert_eq!(
                info.height as u64,
                f["source"]["size"]["height"].as_u64().unwrap()
            );
        }
        let error = preflight(&fixture(), &|| {
            Err(Error::new("cover_cancelled", "controlled"))
        })
        .unwrap_err();
        assert_eq!(error.code, "cover_cancelled");
    }
    #[test]
    fn invalid_unsupported_and_resource_declarations_fail_in_pure_preflight() {
        let valid = fixture();
        for n in 0..valid.len() {
            assert!(preflight(&valid[..n], &|| Ok(())).is_err());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert_eq!(
            preflight(&trailing, &|| Ok(())).unwrap_err().code,
            "cover_invalid"
        );
        let frame = valid.windows(2).position(|b| b == [255, 0xc0]).unwrap();
        let mut huge = valid.clone();
        huge[frame + 5..frame + 7].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            preflight(&huge, &|| Ok(())).unwrap_err().code,
            "cover_resource_limit"
        );
        let mut progressive = valid.clone();
        progressive[frame + 1] = 0xc2;
        assert_eq!(
            preflight(&progressive, &|| Ok(())).unwrap_err().code,
            "cover_unsupported"
        );
        let mut unknown = vec![255, 216, 255, 0xe2, 0, 18];
        unknown.extend_from_slice(b"ICC_PROFILE\0\x01\xffxx");
        unknown.extend_from_slice(&valid[2..]);
        assert_eq!(
            preflight(&unknown, &|| Ok(())).unwrap_err().code,
            "cover_unsupported"
        );
        assert_eq!(
            preflight(&vec![0; MAX_ENCODED + 1], &|| Ok(()))
                .unwrap_err()
                .code,
            "cover_resource_limit"
        );
    }
}
