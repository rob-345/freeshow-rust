//! Test-only JSON-lines control plane. Listeners are always loopback.
use freeshow_api::{ApiConfig, ApiGeneration, ApiService};
use serde_json::{json, Value};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddrV4},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits: Value = std::env::var("FREESHOW_API_TEST_LIMITS")
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or(json!({}));
    let mut config = ApiConfig {
        // Only one explicitly authorized parity case opts into production
        // wildcard binding; ordinary network fixtures remain loopback.
        bind_ip: if limits["wildcard"].as_bool() == Some(true) {
            Ipv6Addr::UNSPECIFIED.into()
        } else {
            Ipv4Addr::LOCALHOST.into()
        },
        ..Default::default()
    };
    for (key, target) in [
        ("sessions", &mut config.max_engine_sessions),
        ("pending", &mut config.max_pending_replies),
        ("inflight", &mut config.max_inflight),
        ("client_inflight", &mut config.max_client_inflight),
        ("events", &mut config.event_capacity),
        ("tcp", &mut config.max_tcp_connections),
        ("payload", &mut config.max_socket_payload),
    ] {
        if let Some(value) = limits[key].as_u64() {
            *target = value as usize;
        }
    }
    for (key, target) in [
        ("reply_ms", &mut config.request_timeout),
        ("dynamic_ms", &mut config.dynamic_timeout),
        ("handshake_ms", &mut config.handshake_timeout),
        ("body_ms", &mut config.body_timeout),
    ] {
        if let Some(value) = limits[key].as_u64() {
            *target = Duration::from_millis(value);
        }
    }
    if let Some(port) = limits["osc_response_port"].as_u64() {
        config.osc_response_target = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port as u16);
    }
    let (service, mut events) = ApiService::new(config)?;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    loop {
        tokio::select! {
            Some(event)=events.recv()=>{stdout.write_all(format!("{}\n",json!({"event":event})).as_bytes()).await?;stdout.flush().await?;}
            line=lines.next_line()=>{let Some(line)=line? else{break};let input:Value=serde_json::from_str(&line)?;
                let result:Result<Value,Value>=match input["op"].as_str().unwrap_or_default(){
                    "start"=>service.start(&input["data"],input["context"].as_u64().unwrap_or(1)).await.map(|v|json!(v)).map_err(|e|json!(e)),
                    "stop"=>service.stop().await.map(|_|Value::Null).map_err(|e|json!(e)),
                    "stats"=>Ok(json!(service.stats().await)),
                    "cancel"=>{service.cancel_pending();Ok(Value::Null)},
                    "complete"=>{let generation=serde_json::from_value::<ApiGeneration>(input["generation"].clone())?;service.api_trigger(&generation,&input["data"]).map(|_|Value::Null).map_err(|e|json!(e))},
                    "dynamic"=>{let generation=serde_json::from_value::<ApiGeneration>(input["generation"].clone())?;service.dynamic_values_reply(&generation,input["listenerId"].as_str().unwrap_or_default(),input["data"].clone()).map(|_|Value::Null).map_err(|e|json!(e))},
                    "auth"=>Ok(json!(freeshow_api::auth_token(input["password"].as_str().unwrap_or_default(),input["sources"].as_array().map(Vec::as_slice).unwrap_or_default()))),
                    "query"=>Ok(freeshow_api::parse_query(input["raw"].as_str().unwrap_or_default())),
                    "rest_data"=>freeshow_api::rest_data(input["body"].clone(),&input["query"]).map_err(|e|json!(e)),
                    _=>Err(json!({"code":"unsupported_operation"})),
                };
                let response=match result{Ok(value)=>json!({"id":input["id"],"ok":true,"value":value}),Err(error)=>json!({"id":input["id"],"ok":false,"error":error})};stdout.write_all(format!("{response}\n").as_bytes()).await?;stdout.flush().await?;
            }
        }
    }
    service.shutdown().await?;
    Ok(())
}
