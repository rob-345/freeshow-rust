use freeshow_midi::{
    device_names, mtc_packets, prepare_send, receive, InputDecision, MidiValue, MtcReceiver,
    Number, PortBindings, PortToken, Status, StatusGate, TimecodeEffect,
};
use serde_json::{json, Value};
use std::io::{self, BufRead};
fn number(value: &Value) -> f64 {
    match value.as_str() {
        Some("NaN") => f64::NAN,
        Some("Infinity") => f64::INFINITY,
        Some("-Infinity") => f64::NEG_INFINITY,
        Some("-0") => -0.0,
        _ => value.as_f64().unwrap_or(f64::NAN),
    }
}
fn scalar(value: &Value) -> MidiValue {
    match value.get("special").and_then(Value::as_str) {
        Some("undefined") => MidiValue::Undefined,
        Some(n) => MidiValue::Number(number(&json!(n))),
        None => MidiValue::from_json(Some(value)).unwrap(),
    }
}
fn ports(input: &Value) -> Result<Value, freeshow_midi::Error> {
    let mut ports = PortBindings::default();
    let mut labels: Vec<(PortToken, String, Value, usize)> = vec![];
    let mut trace = vec![];
    for action in input["actions"].as_array().unwrap() {
        let mut close = |token: &PortToken| {
            let (_, direction, name, id) = labels
                .iter()
                .find(|(label, _, _, _)| label == token)
                .unwrap();
            trace.push(json!(["close", direction, name, id]));
            if action["failClose"]
                .as_array()
                .is_some_and(|failed| failed.contains(&json!(id)))
            {
                Err(freeshow_midi::Error {
                    code: "backend",
                    name: "Error",
                    message: "close failed".into(),
                })
            } else {
                Ok(())
            }
        };
        match action["kind"].as_str().unwrap() {
            "send" => {
                let mut data = action["data"].clone();
                let plan = prepare_send(&mut data)?;
                if ports.output(plan.output.as_ref())?.is_none() {
                    trace.push(json!(["open", "out", plan.output]));
                    if action["failOpen"] == true {
                        continue;
                    }
                    let token = ports.opened_output(plan.output.as_ref())?;
                    labels.push((
                        token,
                        "out".into(),
                        plan.output.clone().unwrap_or(Value::Null),
                        labels.len() + 1,
                    ));
                }
                match plan.packet() {
                    Ok(bytes) => trace.push(json!(["packet", bytes])),
                    Err(error) => trace.push(json!(["error", error.name, error.message])),
                }
            }
            "listen" => {
                if ports.listen(action.get("id"), action.get("input"))? == InputDecision::Open {
                    trace.push(json!(["open", "in", action["input"]]));
                    if action["failOpen"] == true {
                        continue;
                    }
                    let token = ports.opened_input(action.get("id"), action.get("input"), true)?;
                    labels.push((
                        token,
                        "in".into(),
                        action["input"].clone(),
                        labels.len() + 1,
                    ));
                }
            }
            "close" => {
                ports.close_inputs(action["id"].as_str().unwrap_or(""), &mut close)?;
            }
            "stop" => {
                ports.stop_midi(&mut close)?;
            }
            _ => panic!("Unknown port operation"),
        }
    }
    Ok(json!({"trace":trace,"counts":ports.counts()}))
}
fn run(input: Value) -> Result<Value, freeshow_midi::Error> {
    match input["op"].as_str().unwrap_or("") {
        "ports" => ports(&input),
        "send" => {
            let mut data = input["data"].clone();
            let plan = prepare_send(&mut data)?;
            let packet = match plan.packet() {
                Ok(bytes) => json!({"ok":true,"bytes":bytes}),
                Err(error) => json!({"ok":false,"error":error}),
            };
            Ok(
                json!({"data":data,"hydration":plan.hydration,"kind":plan.kind,"outputKey":plan.output_key()?,"packet":packet}),
            )
        }
        "packet" => {
            let mut data = json!({"type":input["type"],"values":{"channel":1}});
            let mut plan = prepare_send(&mut data)?;
            plan.channel = scalar(&input["channel"]);
            plan.key = scalar(&input["key"]);
            plan.value = scalar(&input["value"]);
            Ok(json!(plan.packet()?))
        }
        "devices" => Ok(json!(device_names(input["data"].as_array().unwrap())?)),
        "receive" => {
            let bytes: Vec<u8> = serde_json::from_value(input["bytes"].clone()).unwrap();
            Ok(json!(receive(
                input.get("id"),
                &bytes,
                input["description"].as_str().unwrap()
            )?))
        }
        "mtcSend" => Ok(json!(mtc_packets(
            number(&input["time"]),
            number(&input["fps"])
        ))),
        "mtcReceive" => {
            let mut receiver = MtcReceiver::default();
            let mut gate = StatusGate::default();
            let mut all = vec![];
            for item in input["messages"].as_array().unwrap() {
                let bytes: Vec<u8> = serde_json::from_value(item["bytes"].clone()).unwrap();
                let mut effects = receiver.feed(&bytes, number(&item["now"]))?;
                if input["dedup"] == true {
                    effects = effects
                        .into_iter()
                        .filter_map(|effect| match effect {
                            TimecodeEffect::Status(status) => gate.update(status),
                            effect => Some(effect),
                        })
                        .collect();
                }
                all.push(effects);
            }
            Ok(json!({"effects":all,"snapshot":receiver.snapshot()}))
        }
        "status" => {
            let mut gate = StatusGate::default();
            Ok(json!(input["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| gate.update(match value.as_str().unwrap() {
                    "play" => Status::Play,
                    "pause" => Status::Pause,
                    _ => Status::Stop,
                }))
                .collect::<Vec<_>>()))
        }
        "number" => Ok(json!(Number::new(number(&input["value"])))),
        _ => panic!("Unknown parity operation"),
    }
}
fn main() {
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        assert!(line.len() <= 1024 * 1024);
        let result = run(serde_json::from_str(&line).unwrap());
        println!(
            "{}",
            match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => json!({"ok":false,"error":error}),
            }
        );
    }
}
