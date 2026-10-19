use freeshow_now_playing::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
fn metadata(value: &Value) -> Result<Option<Metadata>> {
    if value.is_null() {
        return Ok(None);
    }
    let mut common = value.clone();
    let pictures = common
        .as_object_mut()
        .unwrap()
        .remove("picture")
        .map_or(vec![], |v| v.as_array().unwrap().clone());
    let mut cover = vec![];
    for mut p in pictures {
        let raw = p.as_object_mut().unwrap().remove("data").unwrap();
        let bytes = raw
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        cover.push(Picture::from_host(p, bytes)?)
    }
    Metadata::from_host(common, Covers::Complete(cover)).map(Some)
}
fn bytes(v: &Value) -> Vec<u8> {
    v.as_array().map_or(vec![], |a| {
        a.iter().map(|v| v.as_u64().unwrap() as u8).collect()
    })
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    assert!(input.len() <= 8 * 1024 * 1024);
    let cases: Vec<Value> = serde_json::from_str(&input)?;
    assert!(cases.len() <= 128);
    let audio = freeshow_audio::AudioService::new(Default::default())?;
    let mut output = vec![];
    for case in cases {
        let context = Context::from_host("fixture", 1, "/fixture")?;
        let platform = if case["platform"] == "win32" {
            Platform::Windows
        } else {
            Platform::Posix
        };
        let folder = case["folder"].as_str().unwrap_or("/fixture/Audio");
        let paths = TrustedPaths::from_host(&context, platform, folder)?;
        let mut router = Router::new(context.clone(), paths)?;
        let command = case["command"].as_str().unwrap_or("set");
        let started = match command {
            "set" => {
                let mut data = SourceData::from_payload(case["data"].clone())?;
                if let Some(n) = case["specialDuration"].as_str() {
                    data.set_duration_number(match n {
                        "NaN" => f64::NAN,
                        "Infinity" => f64::INFINITY,
                        "-Infinity" => f64::NEG_INFINITY,
                        "-0" => -0.0,
                        _ => panic!(),
                    })
                }
                let path = TrustedMediaPath::from_host(
                    &context,
                    case["data"]["filePath"].as_str().unwrap(),
                )?;
                router.begin_set(data, path)?
            }
            "unset" => router.begin_unset()?,
            "open" => router.begin_open()?,
            "path" => router.begin_media_path()?,
            _ => panic!(),
        };
        let mut pending = started.effect;
        let mut trace = vec![];
        let mut cleanup = vec![];
        let mut calls = 0;
        while let Some(issued) = pending {
            calls += 1;
            assert!(calls <= 128);
            let effect = issued.effect;
            let accepted = router.accept(issued.token)?;
            let fail = case["fail"].as_str().unwrap_or("");
            let receipt = match effect {
                Effect::ParseMetadata { path } => {
                    trace.push(json!(["parse", path]));
                    if case["useAudio"] == true {
                        let reply = audio
                            .dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))
                            .await?;
                        match Metadata::from_audio_reply(&reply, Covers::Complete(vec![])) {
                            Ok(m) => Receipt::Metadata(m),
                            Err(e) => Receipt::Failed(e),
                        }
                    } else {
                        Receipt::Metadata(metadata(&case["metadata"])?)
                    }
                }
                Effect::ReadCovers { .. } => Receipt::Failed(Error::native(
                    "now_playing_covers_unknown",
                    "No complete native cover parser receipt",
                )),
                Effect::PublishMetadata {
                    file_path,
                    mut common,
                    pictures,
                    ..
                } => {
                    if !pictures.is_empty() {
                        common["picture"] = Value::Array(
                            pictures
                                .into_iter()
                                .map(|p| {
                                    let mut f = p.fields;
                                    f["data"] = json!(p.bytes);
                                    f
                                })
                                .collect(),
                        )
                    }
                    trace.push(json!(["metadata",{"filePath":file_path,"metadata":common}]));
                    Receipt::Done { success: true }
                }
                Effect::IcecastMetadata { title } => {
                    trace.push(json!(["icecast", title]));
                    Receipt::Done { success: true }
                }
                Effect::EnsureAudioFolder { .. } => {
                    trace.push(json!(["folder", "audio"]));
                    Receipt::Done {
                        success: case["fsFalse"] != true,
                    }
                }
                Effect::Exists { path } => {
                    trace.push(json!(["exists", path]));
                    Receipt::Exists(case["exists"] == true)
                }
                Effect::Delete { path } => {
                    trace.push(json!(["delete", path]));
                    Receipt::Done {
                        success: case["fsFalse"] != true,
                    }
                }
                Effect::WriteCover { path, bytes, .. } => {
                    trace.push(json!(["write", path, bytes]));
                    Receipt::Done {
                        success: case["fsFalse"] != true,
                    }
                }
                Effect::WriteText { path, text, .. } => {
                    trace.push(json!(["write", path, text]));
                    Receipt::Done {
                        success: case["fsFalse"] != true,
                    }
                }
                Effect::Reveal { path } => {
                    trace.push(json!(["reveal", path]));
                    Receipt::Done { success: true }
                }
                Effect::Alert { message } => {
                    trace.push(json!(["alert", message]));
                    Receipt::Done { success: true }
                }
                Effect::ImageCreate { bytes } => {
                    trace.push(json!(["image.create", bytes]));
                    if fail == "create" {
                        Receipt::Failed(Error::native(
                            "native_image_unavailable",
                            "Controlled missing image capability",
                        ))
                    } else {
                        Receipt::Image(1)
                    }
                }
                Effect::ImagePng { .. } => {
                    trace.push(json!(["image.png"]));
                    if fail == "png" {
                        Receipt::Failed(Error::native("image_error", "Controlled toPNG failure"))
                    } else {
                        Receipt::Bytes(bytes(&case["image"]["png"]))
                    }
                }
                Effect::ImageSize { .. } => {
                    trace.push(json!(["image.size"]));
                    Receipt::Size {
                        width: case["image"]["width"].as_u64().unwrap_or(0) as u32,
                        height: case["image"]["height"].as_u64().unwrap_or(0) as u32,
                    }
                }
                Effect::ImageResizeJpeg {
                    width,
                    height,
                    quality,
                    ..
                } => {
                    trace.push(json!(["image.resize",{"width":width,"height":height}]));
                    trace.push(json!(["image.jpeg", quality]));
                    if fail == "jpeg" {
                        Receipt::Failed(Error::native("image_error", "Controlled JPEG failure"))
                    } else {
                        Receipt::Bytes(bytes(&case["image"]["jpeg"]))
                    }
                }
                Effect::ReleaseImage { image } => {
                    cleanup.push(image);
                    Receipt::Done { success: true }
                }
            };
            pending = router.complete(&accepted, receipt)?;
        }
        let result = router.take_result(started.operation)?;
        output.push(json!({"trace":trace,"format":result.mutated_input.as_ref().and_then(|v|v.get("format")),"formatOwn":result.mutated_input.as_ref().is_some_and(|v|v.get("format").is_some()),"mediaPath":result.media_path,"threw":result.error.is_some(),"error":result.error,"cleanup":cleanup}));
    }
    audio.shutdown().await;
    let bytes = serde_json::to_vec(&output)?;
    assert!(bytes.len() <= 24 * 1024 * 1024);
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
