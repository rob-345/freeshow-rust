//! Private test driver, never an application command or authorization boundary.
use base64::{engine::general_purpose::STANDARD, Engine};
use freeshow_cover_images::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};
fn encode(data: &[u8]) -> Value {
    json!({"base64":STANDARD.encode(data),"length":data.len(),"sha256":format!("{:x}",Sha256::digest(data))})
}
fn project(reply: &Reply) -> Value {
    match reply {
        Reply::Covers(pictures) => {
            json!({"covers":pictures.iter().map(|p|json!({"fields":p.fields,"data":encode(&p.data),"undefinedFields":p.undefined_fields})).collect::<Vec<_>>()})
        }
        Reply::Image { width, height } => json!({"width":width,"height":height}),
        Reply::Bytes { data, diagnostics } => {
            json!({"bytes":encode(data),"diagnostics":diagnostics})
        }
        Reply::Released => json!({"released":true}),
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut input = String::new();
    std::io::stdin()
        .take(8 * 1024 * 1024)
        .read_to_string(&mut input)
        .unwrap();
    let data: Value = serde_json::from_str(&input).unwrap();
    let service = CoverImageService::new().unwrap();
    let ctx = Context::from_host("parity", 1, "/library").unwrap();
    let session = service.begin_session(&ctx).unwrap();
    assert!(data["ops"].as_array().unwrap().len() <= MAX_OPERATIONS);
    let mut results = vec![];
    for op in data["ops"].as_array().unwrap() {
        let ticket = match op["op"].as_str().unwrap() {
            "covers" => service.covers(
                &session,
                TrustedMediaFile::from_host(&ctx, op["path"].as_str().unwrap()).unwrap(),
                Cancel::default(),
            ),
            "create" => service.create(
                &session,
                STANDARD.decode(op["base64"].as_str().unwrap()).unwrap(),
                if op["alternate"] == true {
                    ImagePolicy::AllowNativeResizeDifference
                } else {
                    ImagePolicy::VerifiedPngPixels
                },
                Cancel::default(),
            ),
            "png" => service.png(&session, Cancel::default()),
            "size" => service.size(&session, Cancel::default()),
            "jpeg" => service.resize_jpeg90(&session, Cancel::default()),
            "release" => service.release(&session),
            _ => panic!("Unknown private test operation"),
        };
        let result = match ticket {
            Ok(t) => match t.finish().await {
                Ok(o) => o.inspect(project),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        results.push(match result {
            Ok(v) => v,
            Err(e) => json!({"error":e}),
        })
    }
    service
        .retire(&session, Duration::from_secs(2))
        .await
        .unwrap();
    service.shutdown(Duration::from_secs(2)).await.unwrap();
    println!("{}", json!({"results":results,"status":service.status()}));
}
