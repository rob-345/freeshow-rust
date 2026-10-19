//! Private loopback/source test adapter. Not an application runtime/sidecar.
use freeshow_discovery::*;
use serde_json::{json, Value};
use std::{net::SocketAddrV4, path::PathBuf, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
async fn output(value: Value) {
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    stdout.flush().await.unwrap();
}
fn generation(input: &Value) -> DiscoveryGeneration {
    serde_json::from_value(input["generation"].clone()).unwrap_or(DiscoveryGeneration {
        context: 1,
        run: "0123456789abcdef0123456789abcdef".into(),
    })
}
fn helper() -> bool {
    if std::env::args().nth(1).as_deref() != Some("-R") {
        return false;
    }
    let mode = std::env::var("FREESHOW_DISCOVERY_HELPER_MODE").unwrap_or_default();
    if mode == "ignore_term" {
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    if let Ok(path) = std::env::var("FREESHOW_DISCOVERY_HELPER_LOG") {
        std::fs::write(
            path,
            json!({"pid":std::process::id(),"args":std::env::args().skip(1).collect::<Vec<_>>()})
                .to_string(),
        )
        .unwrap();
    }
    if mode == "exit" {
        std::process::exit(7);
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    helper();
    let values: Value = serde_json::from_str(
        &std::env::var("FREESHOW_DISCOVERY_TEST_CONFIG").unwrap_or_else(|_| "{}".into()),
    )
    .unwrap();
    let mut config = DiscoveryConfig::new([0xab, 0xcd, 0xef]);
    config.platform = DiscoveryPlatform::Other;
    config.transport = DiscoveryTransport::Loopback {
        bind: values["bind"]
            .as_str()
            .unwrap_or("127.0.0.1:0")
            .parse::<SocketAddrV4>()
            .unwrap(),
        target: values["target"]
            .as_str()
            .unwrap_or("127.0.0.1:53535")
            .parse()
            .unwrap(),
    };
    config.probe_jitter = Duration::from_millis(values["jitter_ms"].as_u64().unwrap_or(0));
    if let Some(path) = values["dns_sd"].as_str() {
        config.platform = DiscoveryPlatform::Darwin {
            dns_sd: Some(PathBuf::from(path)),
        };
    }
    if let Some(value) = values["responses_per_second"].as_u64() {
        config.responses_per_second = value as usize;
    }
    let (service, mut events) = DiscoveryService::new(config).unwrap();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            value=lines.next_line()=>{let Some(line)=value.unwrap()else{break;};let input:Value=serde_json::from_str(&line).unwrap();let generation=generation(&input);let name=serde_json::from_value(input["name"].clone()).unwrap_or(DiscoveryName::Remote);let snapshot=||serde_json::from_value::<NetworkSnapshot>(input["snapshot"].clone()).unwrap();let result:Result<Value>=match input["op"].as_str().unwrap_or(""){
                "begin"=>service.begin(generation,snapshot()).await.map(|v|json!(v)),
                "publish"=>service.publish(&generation,name,input["port"].as_u64().unwrap_or(5510)as u16).await.map(|v|json!(v)),
                "stop"=>service.stop(&generation).await.map(|_|Value::Null),
                "stats"=>Ok(json!(service.stats())),
                "records"=>service_records(&snapshot(),name,input["port"].as_u64().unwrap_or(5510)as u16,[0xab,0xcd,0xef]).and_then(|records|encode(&Packet{response:true,answers:records,..Packet::default()})).map(|v|json!(v)),
                "ips"=>get_local_ips(&snapshot()).map(|v|json!(v)),
                "args"=>dns_sd_args(&snapshot(),name,input["port"].as_u64().unwrap_or(5510)as u16,[0xab,0xcd,0xef]).map(|v|json!(v)),
                "decode"=>decode(&serde_json::from_value::<Vec<u8>>(input["bytes"].clone()).unwrap()).map(|v|json!(v)),
                _=>Err(DiscoveryError{code:"unknown_test_operation",message:"Unknown test operation".into()}),
            };output(match result{Ok(value)=>json!({"id":input["id"],"ok":true,"value":value}),Err(error)=>json!({"id":input["id"],"ok":false,"error":error})}).await;},
            Some(event)=events.recv()=>output(json!({"event":event})).await,
        }
    }
    let result = service.shutdown().await;
    while let Ok(event) = events.try_recv() {
        output(json!({"event":event})).await;
    }
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
