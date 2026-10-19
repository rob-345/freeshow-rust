//! Bounded controlled fixture driver. No renderer path or raw-value command.
use freeshow_audio::{AudioService, CommonValue as C, FormatHint};
use freeshow_now_playing::*;
use serde_json::{json, Value};
use std::{fs::File, io::Read};

fn decode(value: &Value, depth: usize) -> std::result::Result<C, Box<dyn std::error::Error>> {
    if depth > 10 {
        return Err("raw fixture depth".into());
    }
    let parts = value.as_array().ok_or("raw fixture tuple")?;
    Ok(
        match parts
            .first()
            .and_then(Value::as_str)
            .ok_or("raw fixture tag")?
        {
            "undefined" => C::Undefined,
            "null" => C::Null,
            "boolean" => C::Bool(parts[1].as_bool().ok_or("raw boolean")?),
            "string" => C::String(parts[1].as_str().ok_or("raw string")?.into()),
            "number" => C::Number(if let Some(n) = parts[1].as_f64() {
                n
            } else {
                match parts[1].as_str().ok_or("raw number")? {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    "-0" => -0.0,
                    _ => return Err("raw number tag".into()),
                }
            }),
            "array" => C::Array(
                parts[1]
                    .as_array()
                    .ok_or("raw array")?
                    .iter()
                    .map(|v| decode(v, depth + 1))
                    .collect::<std::result::Result<_, _>>()?,
            ),
            "object" => C::Object(
                parts[1]
                    .as_array()
                    .ok_or("raw object")?
                    .iter()
                    .map(|pair| {
                        Ok((
                            pair[0].as_str().ok_or("raw key")?.into(),
                            decode(&pair[1], depth + 1)?,
                        ))
                    })
                    .collect::<std::result::Result<_, Box<dyn std::error::Error>>>()?,
            ),
            _ => return Err("unknown raw fixture tag".into()),
        },
    )
}
fn raw(value: &C) -> Value {
    match value {
        C::Undefined => json!(["undefined"]),
        C::Null => json!(["null"]),
        C::Bool(value) => json!(["boolean", value]),
        C::String(value) => json!(["string", value]),
        C::Number(value) if value.is_nan() => json!(["number", "NaN"]),
        C::Number(value) if *value == f64::INFINITY => json!(["number", "Infinity"]),
        C::Number(value) if *value == f64::NEG_INFINITY => json!(["number", "-Infinity"]),
        C::Number(value) if *value == 0.0 && value.is_sign_negative() => json!(["number", "-0"]),
        C::Number(value) => json!(["number", value]),
        C::Array(values) => json!(["array", values.iter().map(raw).collect::<Vec<_>>()]),
        C::Object(fields) => json!([
            "object",
            fields
                .iter()
                .map(|(key, value)| json!([key, raw(value)]))
                .collect::<Vec<_>>()
        ]),
    }
}
fn pictures(case: &Value) -> std::result::Result<Vec<NativePicture>, Box<dyn std::error::Error>> {
    case["pictures"].as_array().map_or(Ok(vec![]), |pictures| {
        pictures
            .iter()
            .map(|picture| {
                let bytes = picture["bytes"]
                    .as_array()
                    .ok_or("picture bytes")?
                    .iter()
                    .map(|b| {
                        b.as_u64()
                            .filter(|b| *b < 256)
                            .map(|b| b as u8)
                            .ok_or("picture byte")
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(NativePicture::from_host(
                    decode(&picture["fields"], 0)?,
                    bytes,
                )?)
            })
            .collect()
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    if input.len() > 8 * 1024 * 1024 {
        return Err("fixture input limit".into());
    }
    let cases: Vec<Value> = serde_json::from_str(&input)?;
    if cases.len() > 128 {
        return Err("fixture case limit".into());
    }
    let audio = AudioService::new(Default::default())?;
    let mut output = Vec::new();
    for case in cases {
        let context = Context::from_host("fixture", 1, "/fixture")?;
        let mut router = Router::new(
            context.clone(),
            TrustedPaths::from_host(&context, Platform::Posix, "/fixture/Audio")?,
        )?;
        let path = case["data"]["filePath"]
            .as_str()
            .ok_or("fixture source path")?;
        let mut source = Some(if case["opened"] == true {
            let read = audio
                .reserve_opened()?
                .submit(
                    File::open(path)?,
                    path,
                    FormatHint::FromSourceName,
                    Default::default(),
                )?
                .finish()
                .await?;
            let (common, descriptor, observations) = read.into_parts();
            if observations.unchanged() != Some(true) {
                return Err("fixture changed during actual parse".into());
            }
            drop(descriptor);
            Metadata::from_native_common(common?, Covers::Unknown)?
        } else {
            Metadata::from_common_value(decode(&case["common"], 0)?, Covers::Unknown)?
        });
        let started = router.begin_set(
            SourceData::from_payload(case["data"].clone())?,
            TrustedMediaPath::from_host(&context, path)?,
        )?;
        let mut pending = started.effect;
        let mut trace = Vec::new();
        let mut cleanup = Vec::new();
        let mut cover_reads = 0;
        let mut parsed = false;
        let mut calls = 0;
        let mut projection = Value::Null;
        while let Some(issued) = pending {
            calls += 1;
            if calls > 128 {
                return Err("fixture step limit".into());
            }
            if let Some(publication) = issued.effect.native_metadata() {
                parsed = publication.parsed_common().is_some();
                projection = serde_json::to_value(&issued.effect)?;
                if projection.get("native_receipt").is_some() {
                    return Err("raw receipt leaked to JSON".into());
                }
            }
            let accepted = router.accept(issued.token)?;
            let receipt = match issued.effect {
                Effect::ParseMetadata { path } => {
                    trace.push(json!(["parse", path]));
                    Receipt::Metadata(source.take())
                }
                Effect::ReadCovers { .. } => {
                    cover_reads += 1;
                    if case["fail"] == "covers" {
                        Receipt::Failed(Error::native(
                            "fixture_covers_unavailable",
                            "Explicit controlled unavailable covers",
                        ))
                    } else {
                        Receipt::NativeCovers(pictures(&case)?)
                    }
                }
                effect @ Effect::PublishMetadata { .. } => {
                    let publication = effect.native_metadata().ok_or("missing raw publication")?;
                    trace.push(json!(["metadata", {"filePath":publication.file_path(), "common":raw(publication.common()), "pictures":publication.pictures().iter().map(|p| json!({"fields":raw(p.fields()),"bytes":p.bytes()})).collect::<Vec<_>>()}]));
                    Receipt::Done { success: true }
                }
                Effect::IcecastMetadata { title } => {
                    trace.push(json!(["icecast", title]));
                    Receipt::Done { success: true }
                }
                Effect::EnsureAudioFolder { .. } => {
                    trace.push(json!(["folder", "audio"]));
                    Receipt::Done { success: true }
                }
                Effect::Exists { path } => {
                    trace.push(json!(["exists", path]));
                    Receipt::Exists(false)
                }
                Effect::Delete { path } => {
                    trace.push(json!(["delete", path]));
                    Receipt::Done { success: true }
                }
                Effect::ImageCreate { bytes } => {
                    trace.push(json!(["image.create", bytes]));
                    if case["fail"] == "image" {
                        Receipt::Failed(Error::native(
                            "fixture_image_unavailable",
                            "Explicit controlled image failure",
                        ))
                    } else {
                        Receipt::Image(1)
                    }
                }
                Effect::ImagePng { .. } => {
                    trace.push(json!(["image.png"]));
                    Receipt::Bytes(vec![10, 20])
                }
                Effect::WriteCover { path, bytes, .. } => {
                    trace.push(json!(["write", path, bytes]));
                    Receipt::Done { success: true }
                }
                Effect::ImageSize { .. } => {
                    trace.push(json!(["image.size"]));
                    Receipt::Size {
                        width: 2,
                        height: 1,
                    }
                }
                Effect::ImageResizeJpeg { .. } => return Err("fixture unexpected resize".into()),
                Effect::WriteText { path, text, .. } => {
                    trace.push(json!(["write", path, text]));
                    Receipt::Done { success: true }
                }
                Effect::ReleaseImage { image } => {
                    cleanup.push(image);
                    Receipt::Done { success: true }
                }
                Effect::Reveal { .. } | Effect::Alert { .. } => {
                    return Err("fixture unexpected command effect".into())
                }
            };
            pending = router.complete(&accepted, receipt)?;
        }
        let result = router.take_result(started.operation)?;
        output.push(json!({"trace":trace,"format":result.mutated_input.as_ref().and_then(|v|v.get("format")),"threw":result.error.is_some(),"error":result.error,"cleanup":cleanup,"coverReads":cover_reads,"parsed":parsed,"projection":projection}));
    }
    audio.shutdown().await;
    let output = serde_json::to_string(&output)?;
    if output.len() > 24 * 1024 * 1024 {
        return Err("fixture output limit".into());
    }
    println!("{output}");
    Ok(())
}
