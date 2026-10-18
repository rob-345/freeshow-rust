//! Bounded evidence harness; feature-gated out of the desktop library.
use freeshow_opus::*;
use serde_json::{json, Value};
use std::future::Future;
use std::{io::Read, path::Path};
use tokio_util::sync::CancellationToken;
fn error(text: &str) -> Error {
    Error {
        code: "harness",
        message: text.into(),
        native_code: None,
    }
}
fn unhex(v: &Value) -> Result<Vec<u8>> {
    let s = v.as_str().ok_or_else(|| error("Require hex"))?;
    if s.len() > PCM_BYTES * 4 || s.len() % 2 != 0 {
        return Err(error("Hex size"));
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| {
            let s = std::str::from_utf8(s).map_err(|_| error("Hex chars"))?;
            u8::from_str_radix(s, 16).map_err(|_| error("Hex chars"))
        })
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
async fn execute(v: Value) -> Result<Value> {
    let source = match v["library"].as_str() {
        // Test-operator supplied native executable; this harness is never desktop IPC.
        Some(p) => unsafe { LibrarySource::trusted_path(Path::new(p)) }?,
        None => LibrarySource::system(),
    };
    if v["mode"] == "decode" {
        let mut decoder = testing::Decoder::open(source)?;
        let version = decoder.version().to_owned();
        let mut output = Vec::new();
        let packets = v["packets"]
            .as_array()
            .ok_or_else(|| error("Packets required"))?;
        if packets.len() > 128 {
            return Err(error("Packet count"));
        }
        for packet in packets {
            let pcm = decoder.decode(&unhex(packet)?)?;
            let mut bytes = Vec::with_capacity(pcm.len() * 2);
            for n in pcm {
                bytes.extend_from_slice(&n.to_le_bytes());
            }
            output.push(hex(&bytes));
        }
        return Ok(json!({"version":version,"decoded":output}));
    }
    let context = ContextId(1, 1);
    let service = OpusService::start(context, source, Limits::default())?;
    let capability = service.capability().await;
    let mut packets = Vec::new();
    if let Some(frames) = v["frames"].as_array() {
        if frames.len() > 128 {
            return Err(error("Frame count"));
        }
        for (index, frame) in frames.iter().enumerate() {
            let frame = unhex(frame)?;
            if index == 0 && v["discard_first"] == true {
                let mut pending =
                    Box::pin(service.encode(context, &frame, CancellationToken::new()));
                std::future::poll_fn(|cx| {
                    let _ = pending.as_mut().poll(cx);
                    std::task::Poll::Ready(())
                })
                .await;
                drop(pending);
                packets.push(json!({"discarded":true}));
                continue;
            }
            packets.push(
                match service
                    .encode(context, &frame, CancellationToken::new())
                    .await
                {
                    Ok(packet) => {
                        json!({"sequence":packet.sequence(),"hex":hex(packet.bytes(context)?)})
                    }
                    Err(e) => json!({"error":e}),
                },
            );
        }
    }
    let shutdown = service.shutdown().await;
    Ok(json!({"capability":capability,"packets":packets,"shutdown":shutdown.err()}))
}
fn main() {
    let mut input = Vec::new();
    std::io::stdin()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut input)
        .unwrap();
    if input.len() > 4 * 1024 * 1024 {
        std::process::exit(2);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = runtime.block_on(async {
        match serde_json::from_slice(input.as_slice()) {
            Ok(v) => execute(v).await,
            Err(_) => Err(error("Invalid JSON")),
        }
    });
    println!(
        "{}",
        match output {
            Ok(v) => json!({"ok":v}),
            Err(e) => json!({"error":e}),
        }
    );
}
