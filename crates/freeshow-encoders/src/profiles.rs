use crate::{EncoderError, Result};
use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: u32 = 48000;
pub const AUDIO_CHANNELS: u32 = 2;
pub const DEFAULT_VAAPI_DEVICE: &str = "/dev/dri/renderD128";
pub const ENCODER_IDS: [&str; 6] = ["x264", "videotoolbox", "nvenc", "qsv", "amf", "vaapi"];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderProfile {
    pub id: &'static str,
    pub codec: &'static str,
    pub label: &'static str,
    pub hardware: bool,
    pub platforms: &'static [&'static str],
    pub pixel_format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_suffix: Option<&'static str>,
}
pub fn profile(id: &str, device: &str) -> EncoderProfile {
    let (id, codec, label, platforms): (_, _, _, &'static [&'static str]) = match id {
        "videotoolbox" => (
            "videotoolbox",
            "h264_videotoolbox",
            "VideoToolbox (Apple)",
            &["darwin"],
        ),
        "nvenc" => ("nvenc", "h264_nvenc", "NVENC (NVIDIA)", &["win32", "linux"]),
        "qsv" => ("qsv", "h264_qsv", "Quick Sync (Intel)", &["win32", "linux"]),
        "amf" => ("amf", "h264_amf", "AMF (AMD)", &["win32"]),
        "vaapi" => ("vaapi", "h264_vaapi", "VAAPI (Linux)", &["linux"]),
        _ => (
            "x264",
            "libx264",
            "Software (x264)",
            &["darwin", "win32", "linux"],
        ),
    };
    EncoderProfile {
        id,
        codec,
        label,
        platforms,
        hardware: id != "x264",
        pixel_format: if id == "x264" { "yuv420p" } else { "nv12" },
        pre_input: (id == "vaapi").then(|| {
            vec![
                "-init_hw_device".into(),
                format!(
                    "vaapi=va:{}",
                    if device.is_empty() {
                        DEFAULT_VAAPI_DEVICE
                    } else {
                        device
                    }
                ),
                "-filter_hw_device".into(),
                "va".into(),
            ]
        }),
        filter_suffix: (id == "vaapi").then_some("hwupload"),
    }
}
pub fn profiles(device: &str) -> Vec<EncoderProfile> {
    ENCODER_IDS.iter().map(|id| profile(id, device)).collect()
}
pub fn supported(id: &str, platform: &str) -> bool {
    profile(id, DEFAULT_VAAPI_DEVICE)
        .platforms
        .contains(&platform)
}
pub(crate) fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
pub fn parse_available_encoders(stdout: &str) -> Vec<&'static str> {
    let mut found = [false; 6];
    let mut prefix_whitespace = true;
    for (index, c) in stdout.char_indices() {
        if c == 'V' && prefix_whitespace {
            let tail = &stdout[index..];
            if let Some(end) = tail.find(whitespace) {
                let tail = tail[end..].trim_start_matches(whitespace);
                for (slot, id) in ENCODER_IDS.iter().enumerate() {
                    let codec = profile(id, "").codec;
                    if tail
                        .strip_prefix(codec)
                        .is_some_and(|after| after.chars().next().is_some_and(whitespace))
                    {
                        found[slot] = true;
                    }
                }
            }
        }
        if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            prefix_whitespace = true;
        } else if !whitespace(c) {
            prefix_whitespace = false;
        }
    }
    ENCODER_IDS
        .into_iter()
        .enumerate()
        .filter_map(|(slot, id)| found[slot].then_some(id))
        .collect()
}

pub(crate) fn number(value: f64) -> String {
    ryu_js::Buffer::new().format(value).into()
}
fn round(value: f64) -> f64 {
    if value.abs() >= 4_503_599_627_370_496.0 {
        return value;
    }
    let floor = value.floor();
    if value - floor < 0.5 {
        floor
    } else {
        floor + 1.0
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}
pub fn encoder_args(id: &str, bitrate: f64, gop: f64, mode: Option<&str>) -> Result<Vec<String>> {
    validate_profile_key(id)?;
    if !bitrate.is_finite()
        || !gop.is_finite()
        || mode == Some("vbr") && !(bitrate * 1.5).is_finite()
    {
        return Err(EncoderError::new(
            "nonfinite_number",
            "Encoder numbers must remain finite binary64",
        ));
    }
    let id = profile(id, "").id;
    let mut args = match id {
        "videotoolbox" => strings(&[
            "-prio_speed",
            "1",
            "-allow_sw",
            "1",
            "-bf",
            "0",
            "-profile:v",
            "high",
        ]),
        "nvenc" => strings(&[
            "-preset",
            "p4",
            "-tune",
            "ll",
            "-rc",
            if mode == Some("vbr") { "vbr" } else { "cbr" },
            "-profile:v",
            "high",
        ]),
        "qsv" => strings(&[
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
            "-forced_idr",
            "1",
            "-low_delay_brc",
            "1",
            "-bf",
            "0",
        ]),
        "amf" => strings(&[
            "-usage",
            "lowlatency",
            "-quality",
            "speed",
            "-rc",
            if mode == Some("vbr") {
                "vbr_peak"
            } else {
                "cbr"
            },
            "-profile:v",
            "high",
        ]),
        "vaapi" => strings(&[
            "-rc_mode",
            if mode == Some("vbr") { "VBR" } else { "CBR" },
            "-profile:v",
            "high",
            "-bf",
            "0",
        ]),
        _ => strings(&[
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "high",
        ]),
    };
    let max = if mode == Some("vbr") {
        round(bitrate * 1.5)
    } else {
        bitrate
    };
    args.extend([
        "-b:v".into(),
        format!("{}k", number(bitrate)),
        "-maxrate".into(),
        format!("{}k", number(max)),
        "-bufsize".into(),
        format!("{}k", number(max)),
        "-g".into(),
        number(gop),
    ]);
    Ok(args)
}
pub fn video_filter(id: &str, scale: Option<(f64, f64)>) -> Result<String> {
    validate_profile_key(id)?;
    let profile = profile(id, "");
    let mut parts = vec![];
    if let Some((width, height)) = scale {
        if !width.is_finite() || !height.is_finite() {
            return Err(EncoderError::new(
                "nonfinite_number",
                "Scale dimensions must be finite",
            ));
        }
        parts.push(format!(
            "scale={}:{}:flags=bicubic",
            number(width),
            number(height)
        ));
    }
    parts.push(format!("format={}", profile.pixel_format));
    if let Some(suffix) = profile.filter_suffix {
        parts.push(suffix.into());
    }
    Ok(parts.join(","))
}
pub fn test_encode_command(id: &str, device: &str) -> Result<Vec<String>> {
    validate_profile_key(id)?;
    let profile = profile(id, device);
    let mut args = strings(&["-hide_banner", "-loglevel", "error"]);
    args.extend(profile.pre_input.unwrap_or_default());
    args.extend(strings(&[
        "-f",
        "lavfi",
        "-i",
        "color=black:s=256x144:r=30",
        "-frames:v",
        "3",
        "-vf",
    ]));
    args.push(video_filter(id, None)?);
    args.extend(["-c:v".into(), profile.codec.into()]);
    args.extend(encoder_args(id, 500.0, 60.0, None)?);
    args.extend(strings(&["-f", "null", "-"]));
    Ok(args)
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderCommandOptions {
    pub encoder_id: String,
    pub input_width: f64,
    pub input_height: f64,
    pub output_width: f64,
    pub output_height: f64,
    pub fps: f64,
    pub bitrate: f64,
    pub rate_control: Option<String>,
    pub enable_audio: bool,
    pub sample_rate: Option<f64>,
}
pub fn encoder_command(opts: &EncoderCommandOptions, device: &str) -> Result<Vec<String>> {
    if [
        opts.input_width,
        opts.input_height,
        opts.output_width,
        opts.output_height,
        opts.fps,
        opts.bitrate,
        opts.sample_rate.unwrap_or(0.0),
    ]
    .iter()
    .any(|n| !n.is_finite())
    {
        return Err(EncoderError::new(
            "nonfinite_number",
            "Encoder options must be finite binary64",
        ));
    }
    validate_profile_key(&opts.encoder_id)?;
    let profile = profile(&opts.encoder_id, device);
    let mut args = strings(&["-hide_banner", "-loglevel", "warning"]);
    args.extend(profile.pre_input.unwrap_or_default());
    args.extend(strings(&[
        "-thread_queue_size",
        "512",
        "-use_wallclock_as_timestamps",
        "1",
        "-fflags",
        "+nobuffer",
        "-f",
        "rawvideo",
        "-pixel_format",
        "bgra",
        "-video_size",
    ]));
    args.extend([
        format!("{}x{}", number(opts.input_width), number(opts.input_height)),
        "-framerate".into(),
        number(opts.fps),
        "-i".into(),
        "pipe:0".into(),
    ]);
    if opts.enable_audio {
        let sample = opts
            .sample_rate
            .filter(|n| *n != 0.0)
            .unwrap_or(SAMPLE_RATE as f64);
        args.extend(strings(&[
            "-thread_queue_size",
            "512",
            "-fflags",
            "+nobuffer",
            "-f",
            "s16le",
            "-ar",
        ]));
        args.push(number(sample));
        args.extend(strings(&[
            "-ac",
            "2",
            "-channel_layout",
            "stereo",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-i",
            "pipe:3",
        ]));
    } else {
        args.extend(strings(&[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
        ]));
    }
    let scale = (opts.input_width != opts.output_width || opts.input_height != opts.output_height)
        .then_some((opts.output_width, opts.output_height));
    args.extend([
        "-vf".into(),
        format!(
            "{},fps={}",
            video_filter(profile.id, scale)?,
            number(opts.fps)
        ),
    ]);
    args.extend(strings(&[
        "-fps_mode",
        "passthrough",
        "-fflags",
        "+genpts",
        "-c:v",
    ]));
    args.push(profile.codec.into());
    args.extend(encoder_args(
        profile.id,
        opts.bitrate,
        opts.fps * 2.0,
        opts.rate_control.as_deref(),
    )?);
    if opts.enable_audio {
        args.extend(strings(&[
            "-af",
            "aresample=async=1:max_soft_comp=10000:first_pts=0",
        ]));
    }
    args.extend(strings(&[
        "-c:a",
        "aac",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-b:a",
        "128k",
        "-flags",
        "+global_header",
        "-max_muxing_queue_size",
        "4096",
        "-muxdelay",
        "0",
        "-muxpreload",
        "0",
        "-f",
        "flv",
        "-flvflags",
        "no_sequence_end",
        "pipe:1",
    ]));
    Ok(args)
}
pub fn relay_command(url: &str) -> Vec<String> {
    let mut args = strings(&[
        "-hide_banner",
        "-loglevel",
        "warning",
        "-f",
        "flv",
        "-i",
        "pipe:0",
        "-c",
        "copy",
        "-f",
        "flv",
        "-flvflags",
        "no_sequence_end",
    ]);
    args.push(url.into());
    args
}

fn validate_profile_key(id: &str) -> Result<()> {
    if [
        "__proto__",
        "constructor",
        "toString",
        "valueOf",
        "hasOwnProperty",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "toLocaleString",
        "__defineGetter__",
        "__defineSetter__",
        "__lookupGetter__",
        "__lookupSetter__",
    ]
    .contains(&id)
    {
        return Err(EncoderError::new(
            "unsupported_profile_key",
            "Source prototype property is not an encoder profile",
        ));
    }
    Ok(())
}
