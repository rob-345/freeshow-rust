//! Test-only candidate measurement. This does not enable JPEG in the service.
use base64::{engine::general_purpose::STANDARD, Engine};
use image::ImageDecoder;
use serde_json::{json, Value};
use std::io::{Cursor, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(8 * 1024 * 1024)
        .read_to_string(&mut input)?;
    let input: Value = serde_json::from_str(&input)?;
    if input["op"] == "generate" {
        let (w, h) = (17u32, 11u32);
        let gray = input["gray"] == true;
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.push(((x * 17 + y * 3) % 256) as u8);
                if !gray {
                    data.push(((x * 3 + y * 11) % 256) as u8);
                    data.push(((x * 7 + y * 13) % 256) as u8);
                }
            }
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode(
            &data,
            w,
            h,
            if gray {
                image::ExtendedColorType::L8
            } else {
                image::ExtendedColorType::Rgb8
            },
        )?;
        println!("{}", json!({"base64":STANDARD.encode(jpeg)}));
        return Ok(());
    }
    let encoded = input["base64"].as_str().ok_or("base64 required")?;
    let bytes = STANDARD.decode(encoded)?;
    if bytes.len() > freeshow_cover_images::MAX_ENCODED {
        return Err("candidate input cap".into());
    }
    // The example accepts only the controlled baseline corpus. It is not an
    // allocation/cancellation audit or production JPEG validation boundary.
    let mut offset = 2usize;
    if !bytes.starts_with(&[255, 216]) || !bytes.ends_with(&[255, 217]) {
        return Err("candidate requires complete JPEG".into());
    }
    for index in 0..256 {
        let header = bytes.get(offset..offset + 4).ok_or("JPEG marker")?;
        if header[0] != 255 {
            return Err("JPEG marker prefix".into());
        }
        let length = u16::from_be_bytes([header[2], header[3]]) as usize;
        if length < 2 || offset + length + 2 > bytes.len() {
            return Err("JPEG marker length".into());
        }
        if matches!(header[1], 0xc1..=0xcf) && !matches!(header[1], 0xc4 | 0xc8 | 0xcc) {
            return Err("candidate only measures baseline JPEG".into());
        }
        if header[1] == 0xc0 {
            let frame = &bytes[offset + 4..offset + length + 2];
            if frame.len() < 6 || frame[0] != 8 || !matches!(frame[5], 1 | 3) {
                return Err("candidate requires 8-bit gray/three-component JPEG".into());
            }
            let height = u16::from_be_bytes([frame[1], frame[2]]) as usize;
            let width = u16::from_be_bytes([frame[3], frame[4]]) as usize;
            if width == 0
                || height == 0
                || width > 4096
                || height > 4096
                || width * height > freeshow_cover_images::MAX_PIXELS
            {
                return Err("candidate dimensions cap".into());
            }
        }
        offset += length + 2;
        if header[1] == 0xda {
            break;
        }
        if index == 255 {
            return Err("candidate header count cap".into());
        }
    }
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(&bytes))?;
    let (width, height) = decoder.dimensions();
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(32 * 1024 * 1024);
    decoder.set_limits(limits)?;
    if width as usize * height as usize > freeshow_cover_images::MAX_PIXELS {
        return Err("candidate pixel cap".into());
    }
    let color = decoder.color_type();
    let mut decoded = vec![0; usize::try_from(decoder.total_bytes())?];
    decoder.read_image(&mut decoded)?;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    match color {
        image::ColorType::L8 => {
            for gray in decoded {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        image::ColorType::Rgb8 => {
            for rgb in decoded.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        _ => return Err("candidate unsupported decoded color".into()),
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba)?;
        writer.finish()?;
    }
    println!(
        "{}",
        json!({"width":width,"height":height,"rgba":STANDARD.encode(rgba),"png":STANDARD.encode(png)}),
    );
    Ok(())
}
