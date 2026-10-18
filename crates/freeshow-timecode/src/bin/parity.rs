//! Bounded data-only fixture driver, never an OS codec/device implementation.
use freeshow_midi::{Number, Status};
use freeshow_timecode::*;
use serde_json::{json, Value};
use std::io::{self, Read};
fn number(v: &Value) -> Number {
    Number::new(match v.get("$number").and_then(Value::as_str) {
        Some("NaN") => f64::NAN,
        Some("Infinity") => f64::INFINITY,
        Some("-Infinity") => f64::NEG_INFINITY,
        Some("-0") => -0.0,
        _ => v.as_f64().unwrap_or(f64::NAN),
    })
}
fn scalar(v: Option<&Value>) -> Scalar {
    match v {
        None => Scalar::Undefined,
        Some(Value::Null) => Scalar::Null,
        Some(Value::Bool(b)) => Scalar::Bool(*b),
        Some(Value::String(s)) => Scalar::Text(s.clone()),
        Some(v) if v.is_number() || v.get("$number").is_some() => Scalar::Number(number(v)),
        _ => panic!("unsupported complex scalar fixture"),
    }
}
fn data(v: Option<&Value>) -> Data {
    v.map_or(Data::Undefined, |v| Data::Json(v.clone()))
}
fn unnumber(n: Number) -> Value {
    match Number::new(n.value()) {
        Number::Finite(n) => json!(n),
        Number::NegativeZero => json!({"$number":"-0"}),
        Number::NaN => json!({"$number":"NaN"}),
        Number::PositiveInfinity => json!({"$number":"Infinity"}),
        Number::NegativeInfinity => json!({"$number":"-Infinity"}),
    }
}
fn unscalar(v: &Scalar) -> Value {
    match v {
        Scalar::Undefined => json!({"$undefined":true}),
        Scalar::Null => Value::Null,
        Scalar::Bool(b) => json!(b),
        Scalar::Text(s) => json!(s),
        Scalar::Number(n) => unnumber(*n),
    }
}
fn undata(v: &Data) -> Value {
    match v {
        Data::Undefined => json!({"$undefined":true}),
        Data::Json(v) => v.clone(),
    }
}
fn frame(v: &Value) -> LtcFrame {
    LtcFrame {
        hours: number(&v["hours"]),
        minutes: number(&v["minutes"]),
        seconds: number(&v["seconds"]),
        frames: number(&v["frames"]),
    }
}
fn state(router: &Router) -> Value {
    let s = router.snapshot();
    json!({"framerate":unscalar(&s.framerate),"currentMode":s.current_mode,"sendData":undata(&s.send_data),"receivers":s.receivers,"decoder":s.decoder,"decoderFailed":s.decoder_failed,"decoderFramerate":unscalar(&s.decoder_framerate),"encoder":s.encoder,"encoderFailed":s.encoder_failed,"encoderFramerate":s.encoder_framerate.map(|v|unscalar(&v)),"lastTime":unnumber(s.last_time)})
}
struct Driver {
    router: Router,
    config: Value,
    trace: Vec<Value>,
    errors: Vec<String>,
    opens: Vec<EffectToken>,
    listeners: Vec<ListenerToken>,
    decoder_count: usize,
    encoder_count: usize,
    read: Option<LtcFrame>,
    work_bytes: usize,
}
impl Driver {
    fn record(&mut self, value: Value) {
        self.work_bytes = self
            .work_bytes
            .checked_add(serde_json::to_vec(&value).unwrap().len())
            .unwrap();
        assert!(self.work_bytes <= 8 * 1024 * 1024, "fixture output budget");
        self.trace.push(value);
    }
    fn step(&mut self, step: Step) {
        let mut effects = std::collections::VecDeque::from(step.effects);
        self.errors
            .extend(step.errors.into_iter().map(|e| e.message));
        let mut work = 0;
        while let Some(intent) = effects.pop_front() {
            work += 1;
            assert!(work <= 64);
            let outcome = match intent.effect {
                Effect::OpenMtc {
                    framerate,
                    data,
                    listener,
                } => {
                    self.record(json!(["mtc.open", unscalar(&framerate), undata(&data)]));
                    self.opens.push(intent.token.unwrap());
                    self.listeners.push(listener);
                    continue;
                }
                Effect::SendMtc {
                    time,
                    framerate,
                    data,
                } => {
                    self.record(json!([
                        "mtc.send",
                        unnumber(time),
                        unscalar(&framerate),
                        undata(&data)
                    ]));
                    Completion::Done
                }
                Effect::StopMtc => {
                    self.record(json!(["mtc.stop"]));
                    if self.config["failStopMtc"] == true {
                        Completion::Failed(Error::native("stop failure"))
                    } else {
                        Completion::Done
                    }
                }
                Effect::CreateDecoder {
                    sample_rate,
                    framerate,
                    format,
                } => {
                    self.decoder_count += 1;
                    self.record(json!([
                        "decoder.new",
                        sample_rate,
                        unscalar(&framerate),
                        format
                    ]));
                    if self.config["failDecoder"].as_u64() == Some(self.decoder_count as u64) {
                        Completion::Failed(Error::native("decoder failure"))
                    } else {
                        Completion::Opened
                    }
                }
                Effect::CreateEncoder {
                    sample_rate,
                    framerate,
                    ..
                } => {
                    self.encoder_count += 1;
                    self.record(json!(["encoder.new", sample_rate, unscalar(&framerate)]));
                    if self.config["failEncoder"].as_u64() == Some(self.encoder_count as u64) {
                        Completion::Failed(Error::native("encoder failure"))
                    } else {
                        Completion::Opened
                    }
                }
                Effect::Decode { bytes } => {
                    self.record(json!(["decoder.write", bytes]));
                    if self.config["failWrite"] == true {
                        Completion::Failed(Error::native("write failure"))
                    } else {
                        self.record(json!(["decoder.read"]));
                        if self.config["failRead"] == true {
                            Completion::Failed(Error::native("read failure"))
                        } else {
                            Completion::Decoded(self.read.take())
                        }
                    }
                }
                Effect::Encode { frame } => {
                    self.record(json!(["encoder.set",{"hours":unnumber(frame.hours),"minutes":unnumber(frame.minutes),"seconds":unnumber(frame.seconds),"frames":unnumber(frame.frames)}]));
                    if self.config["failSet"] == true {
                        Completion::Failed(Error::native("set failure"))
                    } else {
                        self.record(json!(["encoder.encode"]));
                        if self.config["failEncode"] == true {
                            Completion::Failed(Error::native("encode failure"))
                        } else {
                            self.record(json!(["encoder.buffer"]));
                            if self.config["failBuffer"] == true {
                                Completion::Failed(Error::native("buffer failure"))
                            } else {
                                Completion::Encoded(
                                    self.config["bytes"]
                                        .as_array()
                                        .unwrap_or(&vec![json!(0), json!(128), json!(255)])
                                        .iter()
                                        .map(|v| v.as_u64().unwrap() as u8)
                                        .collect(),
                                )
                            }
                        }
                    }
                }
                Effect::StopEncoder => Completion::Done,
                Effect::CloseListener { listener, mode } => {
                    if mode == "MTC" {
                        let i = self
                            .listeners
                            .iter()
                            .position(|t| t.serial() == listener.serial())
                            .unwrap();
                        self.record(json!(["mtc.close", i]));
                        if self.config["failClose"]
                            .as_array()
                            .is_some_and(|a| a.contains(&json!(i)))
                        {
                            Completion::Failed(Error::native("close failure"))
                        } else {
                            Completion::Done
                        }
                    } else {
                        Completion::Done
                    }
                }
                Effect::Value { value } => {
                    self.record(json!(["main", "TIMECODE_VALUE", unnumber(value)]));
                    continue;
                }
                Effect::Status { status } => {
                    self.record(json!(["main", "TIMECODE_STATUS", status]));
                    continue;
                }
                Effect::Audio { bytes } => {
                    self.record(json!(["main", "TIMECODE_AUDIO_DATA", bytes]));
                    continue;
                }
                Effect::Alert { message } => {
                    self.record(json!(["toMain", "ALERT", message]));
                    continue;
                }
                Effect::LogStatus { data } => {
                    self.record(json!(["log", undata(&data)]));
                    continue;
                }
                Effect::Cancel { .. } => continue,
            };
            let next = self
                .router
                .complete(&intent.token.unwrap(), outcome)
                .unwrap();
            self.errors
                .extend(next.errors.into_iter().map(|e| e.message));
            effects.extend(next.effects);
        }
    }
    fn action(&mut self, a: &Value) {
        let op = a["op"].as_str().unwrap();
        let result = match op {
            "start" => self.router.begin(Command::Start {
                kind: a["type"].as_str().unwrap_or("").into(),
                mode: a["mode"].as_str().unwrap_or("").into(),
                framerate: scalar(a.get("fps")),
                data: data(a.get("data")),
            }),
            "stop" => self.router.begin(Command::Stop),
            "value" => self.router.begin(Command::Value(number(&a["value"]))),
            "status" => self.router.begin(Command::Status(data(a.get("value")))),
            "audio" => {
                self.read = a.get("frame").map(frame);
                self.router.begin(Command::Audio {
                    mode: a["mode"].as_str().unwrap_or("").into(),
                    bytes: a["bytes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u8)
                        .collect(),
                })
            }
            "resolve" | "reject" => {
                let token = self.opens[a["index"].as_u64().unwrap() as usize].clone();
                self.router.complete(
                    &token,
                    if op == "resolve" {
                        Completion::Opened
                    } else {
                        Completion::Failed(Error::native("open failure"))
                    },
                )
            }
            "accept" => {
                let token = &self.opens[a["index"].as_u64().unwrap() as usize];
                self.router.accept_listener(token).map(|_| Step::default())
            }
            "mtc" => self.router.mtc_message(
                &self.listeners[a["index"].as_u64().unwrap() as usize],
                &a["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8)
                    .collect::<Vec<_>>(),
                number(&a["now"]).value(),
            ),
            "frame" => Ok(self.router.process_frame(number(&a["value"]))),
            "updateStatus" => Ok(self
                .router
                .update_status(match a["value"].as_str().unwrap() {
                    "play" => Status::Play,
                    "pause" => Status::Pause,
                    _ => Status::Stop,
                })),
            _ => panic!("unsupported fixture action"),
        };
        match result {
            Ok(step) => self.step(step),
            Err(error) => self.errors.push(error.message),
        }
    }
}
fn main() {
    let mut input = String::new();
    io::stdin()
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut input)
        .unwrap();
    assert!(input.len() <= 4 * 1024 * 1024);
    let cases: Vec<Value> = serde_json::from_str(&input).unwrap();
    assert!(cases.len() <= 512);
    let mut output = vec![];
    let mut output_bytes = 0usize;
    for case in cases {
        let mut driver = Driver {
            router: Router::new(case["windows"] == true),
            config: case.clone(),
            trace: vec![],
            errors: vec![],
            opens: vec![],
            listeners: vec![],
            decoder_count: 0,
            encoder_count: 0,
            read: None,
            work_bytes: 0,
        };
        let mut states = vec![];
        let actions = case["actions"].as_array().unwrap();
        assert!(actions.len() <= 256);
        for action in actions {
            driver.action(action);
            let snapshot = state(&driver.router);
            driver.work_bytes += serde_json::to_vec(&snapshot).unwrap().len();
            assert!(
                driver.work_bytes <= 8 * 1024 * 1024,
                "fixture output budget"
            );
            states.push(snapshot);
        }
        let result = json!({"trace":driver.trace,"errors":driver.errors,"states":states});
        output_bytes += serde_json::to_vec(&result).unwrap().len();
        assert!(output_bytes <= 16 * 1024 * 1024, "fixture output budget");
        output.push(result);
    }
    println!("{}", serde_json::to_string(&output).unwrap());
}
