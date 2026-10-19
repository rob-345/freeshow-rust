use freeshow_midi_ports::*;
use serde_json::{json, Value};
use std::{
    io::{self, BufRead},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
#[path = "../controlled.rs"]
mod controlled;
fn owner() -> Owner {
    Owner::new("fixture", 1, "/controlled/library", 1).unwrap()
}
fn devices(value: &Value, prefix: &str) -> Vec<Device> {
    value.as_array().map_or(vec![], |v| {
        v.iter()
            .enumerate()
            .map(|(i, v)| Device {
                id: format!("{prefix}{i}"),
                name: v.as_str().unwrap().into(),
            })
            .collect()
    })
}
fn selector(input: &Value) -> Selector {
    if let Some(parts) = input.get("parts") {
        Selector::List(
            parts
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    if v["undefined"] == true {
                        Selector::Undefined
                    } else {
                        Selector::Json(v["value"].clone())
                    }
                })
                .collect(),
        )
    } else {
        Selector::from_field(input.get("selector")).unwrap()
    }
}
fn wire(result: Result<Reply>) -> Value {
    match result {
        Ok(reply) => json!({"ok":true,"value":reply}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}
async fn command(service: &MidiService, command: Command) -> Result<Reply> {
    service
        .begin(
            &owner(),
            command,
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )?
        .finish()
        .await
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        assert!(line.len() <= 1024 * 1024);
        let input: Value = serde_json::from_str(&line).unwrap();
        println!("{}", run(input).await);
    }
}
async fn run(input: Value) -> Value {
    if input["op"] == "select" {
        let devices = devices(&input["names"], "p");
        return match select_devices(&selector(&input), &devices) {
            Ok(indices) => json!({"ok":true,"indices":indices}),
            Err(error) => json!({"ok":false,"error":error}),
        };
    }
    #[cfg(feature = "native")]
    if input["op"] == "native" {
        let service =
            MidiService::start(owner(), CancellationToken::new(), Config::default()).unwrap();
        let inputs = wire(command(&service, Command::Devices(Direction::Input)).await);
        let outputs = wire(command(&service, Command::Devices(Direction::Output)).await);
        let shutdown = service.shutdown().await;
        return json!({"inputs":inputs,"outputs":outputs,"shutdown":shutdown.err(),"status":service.status()});
    }
    let control = controlled::Controlled::new(
        devices(&input["inputs"], "i"),
        devices(&input["outputs"], "o"),
    );
    let service = MidiService::with_backend(
        owner(),
        CancellationToken::new(),
        Config::default(),
        Box::new(control.clone()),
    )
    .unwrap();
    let mut events = service.events().unwrap();
    let mut results = vec![];
    let mut messages = vec![];
    let actions = input["actions"].as_array().unwrap();
    assert!(actions.len() <= 1024);
    for action in actions {
        let op = action["op"].as_str().unwrap();
        if op == "failClose" {
            control.fail_close(action["name"].as_str().unwrap());
            continue;
        }
        if op == "removeInputs" {
            control.remove_inputs();
            continue;
        }
        if op == "emit" {
            let bytes = action["bytes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect::<Vec<_>>();
            assert!(bytes.len() <= 1024);
            control.emit(action["device"].as_str().unwrap_or("i0"), &bytes);
            for _ in 0..action["expect"].as_u64().unwrap_or(0) {
                let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                    .await
                    .unwrap()
                    .unwrap();
                messages.push(serde_json::to_value(event).unwrap());
            }
            continue;
        }
        let cmd = match op {
            "inputs" => Command::Devices(Direction::Input),
            "outputs" => Command::Devices(Direction::Output),
            "send" => Command::Send(action["data"].clone()),
            "listen" => Command::Listen(action["data"].clone()),
            "close" => Command::close_inputs_field(action.get("id")).unwrap(),
            "stop" => Command::Stop,
            "mtcListen" => Command::MtcListen(selector(action)),
            "mtcClose" => Command::MtcClose(action["listener"].as_u64().unwrap()),
            "mtcSend" => Command::MtcSend {
                selector: selector(action),
                time_ms: action["time"].as_f64().unwrap(),
                framerate: action["fps"].as_f64().unwrap(),
            },
            "mtcStop" => Command::MtcStop,
            _ => panic!("unknown fixture action"),
        };
        if action["deferred"] == true {
            control.block("open");
            let ticket = service
                .begin(
                    &owner(),
                    cmd,
                    CancellationToken::new(),
                    Instant::now() + Duration::from_secs(2),
                )
                .unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while !control.entered() {
                assert!(Instant::now() < until);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert!(!control.trace().iter().any(|v| v[0] == "send"));
            control.release();
            results.push(wire(ticket.finish().await));
        } else {
            results.push(wire(command(&service, cmd).await));
        }
    }
    let before_shutdown = control.trace();
    let status = service.status();
    let shutdown = service.shutdown().await;
    json!({"results":results,"messages":messages,"trace":before_shutdown,"cleanupTrace":control.trace(),"statusBeforeShutdown":status,"shutdown":shutdown.err()})
}
