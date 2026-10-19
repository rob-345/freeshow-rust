//! Controlled JSON fixture driver. Its native path is host environment only.
use freeshow_ltc::*;
use serde_json::{json, Value};
use std::{
    io::{self, Read},
    path::PathBuf,
    time::{Duration, Instant},
};
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut text = String::new();
    io::stdin()
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .unwrap();
    assert!(text.len() <= 4 * 1024 * 1024);
    let cases: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert!(cases.len() <= 64);
    let path = std::env::var_os("FREESHOW_LTC_LIBRARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap()
                .join("node_modules/libltc-wrapper/libltc/bin/libltc.so.11")
        });
    let mut out = vec![];
    let mut output_bytes = 0;
    for case in cases {
        let owner = Owner::new(
            "fixture",
            0,
            if cfg!(windows) {
                "C:\\fixture"
            } else {
                "/fixture"
            },
        )
        .unwrap();
        let service = LtcService::start(
            owner.clone(),
            TrustedLibrary::from_host(path.clone()).unwrap(),
            Config::default(),
        )
        .unwrap();
        let mut replies = vec![];
        let actions = case.as_array().unwrap();
        assert!(actions.len() <= 512);
        for action in actions {
            let number = |key: &str| action[key].as_f64().unwrap_or(f64::NAN);
            let op = match action["op"].as_str().unwrap() {
                "capability" => Operation::Capability,
                "encoder" => Operation::CreateEncoder { fps: number("fps") },
                "decoder" => Operation::CreateDecoder { fps: number("fps") },
                "encode" => Operation::Encode {
                    hours: number("hours"),
                    minutes: number("minutes"),
                    seconds: number("seconds"),
                    frames: number("frames"),
                },
                "decode" => Operation::Decode {
                    bytes: action["bytes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().filter(|n| *n < 256).unwrap() as u8)
                        .collect(),
                },
                "stopEncoder" => Operation::StopEncoder,
                "stopDecoder" => Operation::StopDecoder,
                _ => panic!("unsupported fixture"),
            };
            let reply = match service.begin(&owner, op, Instant::now() + Duration::from_secs(5)) {
                Ok(ticket) => match ticket.finish().await.unwrap().into_reply() {
                    Ok(value) => serde_json::to_value(value).unwrap(),
                    Err(error) => json!({"error":error}),
                },
                Err(error) => json!({"error":error}),
            };
            output_bytes += serde_json::to_vec(&reply).unwrap().len();
            assert!(output_bytes <= 16 * 1024 * 1024);
            replies.push(reply);
        }
        service.shutdown().await.unwrap();
        out.push(json!({"replies":replies,"closed":service.status()}));
    }
    println!("{}", serde_json::to_string(&out).unwrap());
}
