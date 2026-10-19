//! Controlled loopback JSON-lines test driver; no application IPC integration.
use freeshow_remotes::{prepare_start, RemoteConfig, RemoteGeneration, RemoteService, ServerKind};
use serde_json::{json, Value};
use std::{
    net::Ipv4Addr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let limits: Value = std::env::var("FREESHOW_REMOTES_TEST_LIMITS")
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or(json!({}));
    let clock = Arc::new(AtomicU64::new(1000));
    let time = clock.clone();
    let mut config = RemoteConfig {
        bind_ip: Ipv4Addr::LOCALHOST.into(),
        clock: Arc::new(move || time.load(Ordering::Relaxed)),
        ..Default::default()
    };
    for (key, target) in [
        ("tcp", &mut config.max_tcp_connections),
        ("sessions", &mut config.max_engine_sessions),
        ("clients", &mut config.max_clients),
        ("events", &mut config.data_events),
        ("commands", &mut config.command_capacity),
        ("payload", &mut config.max_socket_payload),
        ("socket_queue", &mut config.socket_buffer_packets),
        ("response_ids", &mut config.max_response_ids),
        ("file_workers", &mut config.static_workers),
        ("file_queue", &mut config.static_queue),
    ] {
        if let Some(n) = limits[key].as_u64() {
            *target = n as usize;
        }
    }
    if let Some(n) = limits["handshake_ms"].as_u64() {
        config.handshake_timeout = Duration::from_millis(n);
    }
    if let Some(n) = limits["body_ms"].as_u64() {
        config.body_timeout = Duration::from_millis(n);
    }
    if let Some(n) = limits["static_bytes"].as_u64() {
        config.max_static_bytes = n;
    }
    for kind in ServerKind::ALL {
        if let Some(root) = limits["roots"][kind.key()].as_str() {
            config.static_roots.insert(kind, PathBuf::from(root));
        }
    }
    let (service, mut events) = RemoteService::new(config)?;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut drain = true;
    loop {
        tokio::select! {
            event=events.recv(),if drain=>{if let Some(event)=event{stdout.write_all(format!("{}\n",json!({"event":event})).as_bytes()).await?;stdout.flush().await?;}},
            line=lines.next_line()=>{let Some(line)=line?else{break};let input:Value=serde_json::from_str(&line)?;
                let kind=serde_json::from_value::<ServerKind>(input["server"].clone()).unwrap_or(ServerKind::REMOTE);
                let result=match input["op"].as_str().unwrap_or_default(){
                    "start"=>service.start(&input["data"],input["context"].as_u64().unwrap_or(1)).await.map(|v|json!(v)),
                    "stop"=>service.stop().await.map(|_|Value::Null),
                    "stats"=>Ok(json!(service.stats())),
                    "update"=>service.update_server_data(&input["data"]).map(|_|Value::Null),
                    "get_data"=>Ok(service.get_server_data(kind)),
                    "prepare"=>prepare_start(&input["data"]).map(|v|json!(v)),
                    "os"=>Ok(json!(freeshow_remotes::get_os(input["data"].as_str().unwrap_or_default()))),
                    "clock"=>{clock.store(input["millis"].as_u64().unwrap_or_default(),Ordering::Relaxed);Ok(Value::Null)},
                    "drain"=>{drain=input["enabled"].as_bool().unwrap_or(true);Ok(Value::Null)},
                    "subscribers"=>{let generation=serde_json::from_value::<RemoteGeneration>(input["generation"].clone())?;service.stage_subscribers(&generation).map(|v|json!(v))},
                    op @("renderer"|"backend"|"stage")=>{let generation=serde_json::from_value::<RemoteGeneration>(input["generation"].clone())?;match op{"renderer"=>service.renderer_send(&generation,kind,&input["data"]).await,"backend"=>service.broadcast_backend(&generation,kind,&input["data"]).await,_=>service.send_stage_subscribers(&generation,&input["data"]).await}.map(|_|Value::Null)},
                    _=>Err(freeshow_remotes::RemoteError{code:"unsupported_operation",message:"Unknown harness operation".into()}),
                };
                let response=match result{Ok(value)=>json!({"id":input["id"],"ok":true,"value":value}),Err(error)=>json!({"id":input["id"],"ok":false,"error":error})};stdout.write_all(format!("{response}\n").as_bytes()).await?;stdout.flush().await?;
            }
        }
    }
    service.shutdown().await?;
    Ok(())
}
