use crate::{service::Check, *};
use std::io::{Cursor, Read, Write};
pub(crate) struct Raster {
    pub width: u32,
    pub height: u32,
    rgba: Vec<u8>,
    policy: ImagePolicy,
}
struct Output(Vec<u8>);
impl Write for Output {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(b.len()) > MAX_ENCODED {
            return Err(std::io::Error::other("Encoded output exceeds 4MiB"));
        }
        self.0.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn dimensions(w: u32, h: u32) -> Result<usize> {
    let pixels = (w as usize)
        .checked_mul(h as usize)
        .ok_or_else(|| resource("Pixel product overflow"))?;
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || pixels > MAX_PIXELS {
        return Err(resource("Image exceeds 4096 side/4M pixel bound"));
    }
    Ok(pixels)
}
pub(crate) fn create(bytes: Vec<u8>, policy: ImagePolicy, check: &Check) -> Result<Raster> {
    check.check()?;
    if bytes.starts_with(&[255, 216]) {
        let (width, height, rgba) = crate::jpeg::decode(&bytes, check)?;
        return Ok(Raster {
            width,
            height,
            rgba,
            policy,
        });
    }
    if !bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        if bytes.is_empty() {
            return Ok(Raster {
                width: 0,
                height: 0,
                rgba: vec![],
                policy,
            });
        }
        return Err(unsupported("Only bounded certified PNG and Linux JPEG subsets are available; GIF/WebP/etc are unavailable"));
    }
    let mut p = 8usize;
    let mut count = 0;
    let mut ended = false;
    let mut wh = None;
    let mut channels = 0usize;
    let mut compressed = Vec::with_capacity(MAX_ENCODED);
    while p < bytes.len() {
        check.check()?;
        count += 1;
        if count > 256 {
            return Err(resource("PNG chunk count"));
        }
        let header = bytes
            .get(p..p + 8)
            .ok_or_else(|| invalid("Truncated PNG chunk"))?;
        let n = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = p
            .checked_add(n)
            .and_then(|p| p.checked_add(12))
            .ok_or_else(|| resource("PNG offset"))?;
        let data = bytes
            .get(p + 8..end - 4)
            .ok_or_else(|| invalid("Truncated PNG chunk data"))?;
        match &header[4..] {
            b"IHDR" => {
                if wh.is_some() || p != 8 || n != 13 {
                    return Err(invalid("Invalid PNG header"));
                }
                let w = u32::from_be_bytes(data[..4].try_into().unwrap());
                let h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                dimensions(w, h)?;
                if data[8] != 8 || !matches!(data[9], 2 | 6) || data[10..] != [0, 0, 0] {
                    return Err(unsupported("PNG requires 8-bit RGB/RGBA, no interlace"));
                }
                wh = Some((w, h));
                channels = if data[9] == 2 { 3 } else { 4 };
            }
            b"IDAT" => compressed.extend_from_slice(data),
            b"IEND" => {
                if n != 0 {
                    return Err(invalid("Nonempty PNG IEND"));
                }
                ended = true;
            }
            b"sRGB" => {
                if data != [0] {
                    return Err(unsupported("PNG non-perceptual color intent"));
                }
            }
            b"sBIT" => {
                if !data.iter().all(|b| *b == 8) {
                    return Err(unsupported("PNG reduced significant bits"));
                }
            }
            _ => {
                return Err(unsupported(
                    "PNG ancillary/color/animation chunks are not certified",
                ))
            }
        }
        p = end;
        if ended {
            break;
        }
    }
    if !ended || p != bytes.len() {
        return Err(invalid("PNG missing end/trailing bytes"));
    }
    let (w, h) = wh.ok_or_else(|| invalid("Missing PNG IHDR"))?;
    let pixels = dimensions(w, h)?;
    // png intentionally tolerates extra inflate output like libpng. Our strict
    // subset validates exact scanline byte count before giving it a raster.
    let expected = pixels * channels + h as usize;
    let mut inflate = flate2::read::ZlibDecoder::new(Cursor::new(&compressed));
    let mut scratch = [0u8; 65536];
    let mut produced = 0usize;
    loop {
        check.check()?;
        let want = scratch.len().min(expected + 1 - produced);
        let n = inflate
            .read(&mut scratch[..want])
            .map_err(|e| invalid(e.to_string()))?;
        produced += n;
        if produced > expected {
            return Err(invalid("PNG excess inflate output"));
        }
        if n == 0 {
            break;
        }
    }
    if produced != expected || inflate.total_in() != compressed.len() as u64 {
        return Err(invalid("PNG truncated/trailing deflate stream"));
    }
    drop(inflate);
    drop(compressed);
    let decoder = png::Decoder::new_with_limits(
        Cursor::new(&bytes),
        png::Limits {
            bytes: 16 * 1024 * 1024,
        },
    );
    let mut reader = decoder.read_info().map_err(|e| invalid(e.to_string()))?;
    let len = reader
        .output_buffer_size()
        .ok_or_else(|| resource("PNG output overflow"))?;
    if len > pixels * 4 {
        return Err(resource("Unexpected PNG output size"));
    }
    let mut decoded = vec![0; len];
    let info = reader
        .next_frame(&mut decoded)
        .map_err(|e| invalid(e.to_string()))?;
    reader.finish().map_err(|e| invalid(e.to_string()))?;
    if info.width != w || info.height != h {
        return Err(invalid("PNG dimensions changed"));
    }
    let rgba = if info.color_type == png::ColorType::Rgb {
        let mut rgba = Vec::with_capacity(pixels * 4);
        for row in decoded.chunks(65535) {
            check.check()?;
            for p in row.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255])
            }
        }
        rgba
    } else {
        decoded
    };
    let mut rgba = rgba;
    if rgba.len() != pixels * 4 {
        return Err(invalid("Unexpected decoded pixel count"));
    }
    for row in rgba.chunks_mut((w as usize) * 4) {
        check.check()?;
        for p in row.as_chunks_mut::<4>().0 {
            if !matches!(p[3], 0 | 255) {
                return Err(unsupported(
                    "Partial-alpha Skia premultiplication is not certified",
                ));
            }
            if p[3] == 0 {
                p[..3].fill(0)
            }
        }
    }
    Ok(Raster {
        width: w,
        height: h,
        rgba,
        policy,
    })
}
pub(crate) fn png(image: &Raster, check: &Check) -> Result<Vec<u8>> {
    if image.width == 0 {
        return Ok(vec![]);
    }
    check.check()?;
    let mut output = Output(Vec::with_capacity(MAX_ENCODED));
    {
        let mut encoder = png::Encoder::new(&mut output, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| invalid(e.to_string()))?;
        writer
            .write_image_data(&image.rgba)
            .map_err(|e| resource(e.to_string()))?;
        writer.finish().map_err(|e| invalid(e.to_string()))?
    }
    check.check()?;
    Ok(output.0)
}
pub(crate) fn resize_jpeg90(image: &Raster, check: &Check) -> Result<Vec<u8>> {
    if image.width == 0 {
        return Ok(vec![]);
    }
    let scale = (640.0 / image.width as f64).min(480.0 / image.height as f64);
    let w = (image.width as f64 * scale).round() as u32;
    let h = (image.height as f64 * scale).round() as u32;
    if w == 0 || h == 0 {
        return Ok(vec![]);
    }
    if image.width <= 640 && image.height <= 480 {
        return Err(invalid(
            "Source preserves original cover bytes at or below 640x480; no JPEG conversion",
        ));
    }
    if image.policy != ImagePolicy::AllowNativeResizeDifference {
        return Err(unsupported("Native Lanczos3/JPEG90 is an explicit alternate codec, not Electron Skia byte/pixel equivalence"));
    }
    check.check()?;
    // image0.25.10 is separable vertical then horizontal Lanczos3. Bound
    // its declared support loops before any clone/scratch allocation.
    let taps = |input: u32, output: u32| {
        ((6.0 * input as f64 / output as f64).ceil() as u64 + 2).min(input as u64)
    };
    let work = image.width as u64 * h as u64 * taps(image.height, h)
        + w as u64 * h as u64 * taps(image.width, w);
    if work > 64_000_000 {
        return Err(resource("Resample tap budget"));
    }
    let src = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
        .ok_or_else(|| invalid("Invalid RGBA raster"))?;
    let resized = image::imageops::resize(&src, w, h, image::imageops::FilterType::Lanczos3);
    check.check()?;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for row in resized.rows() {
        check.check()?;
        for p in row {
            rgb.extend_from_slice(&p.0[..3])
        }
    }
    let mut output = Output(Vec::with_capacity(MAX_ENCODED));
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 90)
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| resource(e.to_string()))?;
    check.check()?;
    Ok(output.0)
}
