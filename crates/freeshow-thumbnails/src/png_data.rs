use crate::*;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
pub(crate) struct PngInfo {
    pub width: u32,
    pub height: u32,
    pub pixel_sha256: String,
}
pub(crate) fn validate(bytes: &[u8], budget: &Budget) -> Result<PngInfo> {
    budget.check()?;
    if bytes.len() > budget.config.png_bytes {
        return Err(ThumbnailError::resource("Canvas PNG encoded byte limit"));
    }
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err(ThumbnailError::new(
            "invalid_canvas_png",
            "Expected a PNG completion",
        ));
    }
    let mut offset = 8_usize;
    let mut count = 0;
    let mut dimensions = None;
    let mut idat = false;
    let mut srgb = false;
    let mut significant_bits = false;
    let mut end = false;
    let mut compressed = vec![];
    while offset < bytes.len() {
        budget.check()?;
        count += 1;
        if count > budget.config.chunks {
            return Err(ThumbnailError::resource("Canvas PNG chunk count limit"));
        }
        let header = bytes
            .get(offset..offset + 8)
            .ok_or_else(|| ThumbnailError::new("invalid_canvas_png", "Truncated PNG chunk"))?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let finish = offset
            .checked_add(12)
            .and_then(|x| x.checked_add(length))
            .ok_or_else(|| ThumbnailError::resource("PNG chunk length overflow"))?;
        if finish > bytes.len() {
            return Err(ThumbnailError::new(
                "invalid_canvas_png",
                "PNG chunk exceeds supplied bytes",
            ));
        }
        let body = &bytes[offset + 8..finish - 4];
        match &header[4..8] {
            b"IHDR" if count == 1 && length == 13 => {
                let width = u32::from_be_bytes(body[..4].try_into().unwrap());
                let height = u32::from_be_bytes(body[4..8].try_into().unwrap());
                if width == 0
                    || height == 0
                    || width > budget.config.max_size
                    || height > budget.config.max_size
                    || width as u64 * height as u64 > budget.config.max_pixels
                {
                    return Err(ThumbnailError::resource(
                        "Canvas PNG dimensions/pixels limit",
                    ));
                }
                if body[8] != 8 || ![2, 6].contains(&body[9]) || body[10..13] != [0, 0, 0] {
                    return Err(ThumbnailError::new(
                        "unsupported_canvas_png",
                        "Only8-bit noninterlaced RGB/RGBA canvas PNG is verified",
                    ));
                }
                dimensions = Some((width, height, body[9]));
            }
            b"sRGB" if !srgb && !idat && dimensions.is_some() && length == 1 && body[0] == 0 => {
                srgb = true;
            }
            b"sBIT" if !significant_bits && !idat && dimensions.is_some() => {
                // WebKit's canvas encoder declares full eight-bit precision;
                // accepting that declaration does not transform any pixels.
                let channels = if dimensions.unwrap().2 == 6 { 4 } else { 3 };
                if length != channels || body.iter().any(|value| *value != 8) {
                    return Err(ThumbnailError::new(
                        "unsupported_canvas_png",
                        "Unverified canvas significant-bit precision",
                    ));
                }
                significant_bits = true;
            }
            b"IDAT" if dimensions.is_some() && !end => {
                idat = true;
                compressed.extend_from_slice(body);
            }
            b"IEND" if idat && length == 0 && finish == bytes.len() => {
                end = true;
            }
            _ => {
                return Err(ThumbnailError::new(
                    "unsupported_canvas_png",
                    "Unverified PNG chunk/order/animation/color metadata",
                ))
            }
        }
        offset = finish;
    }
    if !end || !srgb {
        return Err(ThumbnailError::new(
            "invalid_canvas_png",
            "PNG requires final IEND and verified sRGB intent",
        ));
    }
    let (width, height, color) = dimensions.unwrap();
    // png intentionally tolerates excess/missing zlib bytes after enough rows.
    // A completion is stricter: cap aggregate inflation and require exactly one
    // complete stream containing the declared scanlines, before staging bytes.
    let compressed_length = compressed.len() as u64;
    let mut zlib = flate2::bufread::ZlibDecoder::new(Cursor::new(compressed));
    let expected_raw = (width as u64 * if color == 6 { 4 } else { 3 } + 1) * height as u64;
    let mut inflated = 0_u64;
    let mut block = [0; 8192];
    loop {
        budget.check()?;
        let read = zlib
            .read(&mut block)
            .map_err(|x| ThumbnailError::new("invalid_canvas_png", x.to_string()))?;
        if read == 0 {
            break;
        }
        inflated = inflated.saturating_add(read as u64);
        if inflated > expected_raw {
            return Err(ThumbnailError::resource(
                "Canvas PNG aggregate inflation limit",
            ));
        }
    }
    if inflated != expected_raw || zlib.total_in() != compressed_length {
        return Err(ThumbnailError::new(
            "invalid_canvas_png",
            "Canvas PNG zlib stream size mismatch",
        ));
    }
    // All declarations are preflighted before png can allocate. Its own Limits cap
    // decoder-owned buffers; our decoded data is consumed one bounded row at a time.
    let mut options = png::DecodeOptions::default();
    options.set_skip_ancillary_crc_failures(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(png::Limits {
        bytes: budget.config.png_bytes,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|x| ThumbnailError::new("invalid_canvas_png", x.to_string()))?;
    let mut digest = Sha256::new();
    let mut rows = 0_u32;
    let expected = width as usize * if color == 6 { 4 } else { 3 };
    while let Some(row) = reader
        .next_row()
        .map_err(|x| ThumbnailError::new("invalid_canvas_png", x.to_string()))?
    {
        budget.check()?;
        rows += 1;
        if rows > height || row.data().len() != expected {
            return Err(ThumbnailError::resource("Canvas decoded row limit"));
        }
        if color == 6 {
            digest.update(row.data());
        } else {
            for pixel in row.data().as_chunks::<3>().0 {
                digest.update(pixel);
                digest.update([255]);
            }
        }
    }
    reader
        .finish()
        .map_err(|x| ThumbnailError::new("invalid_canvas_png", x.to_string()))?;
    budget.check()?;
    if rows != height {
        return Err(ThumbnailError::new(
            "invalid_canvas_png",
            "Canvas PNG decoded height mismatch",
        ));
    }
    Ok(PngInfo {
        width,
        height,
        pixel_sha256: format!("{:x}", digest.finalize()),
    })
}
