#[path = "../src/now_playing_input.rs"]
mod input;
use input::{parse, Command};
use serde_json::{json, Value};
fn context() -> Value {
    json!({"runtimeId":"native-音","libraryGeneration":3,"dataPath":"/selected/日本語/🎹"})
}
fn frame(command: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"context":context(),"command":command})).unwrap()
}
fn set(duration: Value) -> Vec<u8> {
    frame(
        json!({"kind":"set","data":{"filePath":"/audio.wav","duration":null},"duration":duration}),
    )
}
#[test]
fn exact_commands_and_all_ieee_duration_tags() {
    assert!(matches!(
        parse(&frame(json!({"kind":"unset"}))).unwrap().command,
        Command::Unset
    ));
    assert!(matches!(
        parse(&frame(json!({"kind":"open"}))).unwrap().command,
        Command::Open
    ));
    for (tag, number) in [
        (json!({"kind":"Finite","value":61.25}), 61.25),
        (json!({"kind":"NaN"}), f64::NAN),
        (json!({"kind":"PositiveInfinity"}), f64::INFINITY),
        (json!({"kind":"NegativeInfinity"}), f64::NEG_INFINITY),
        (json!({"kind":"NegativeZero"}), -0.0),
    ] {
        let result = parse(&set(tag)).unwrap();
        assert_eq!(result.context.runtime_id, "native-音");
        assert_eq!(result.context.library_generation, 3);
        match result.command {
            Command::Set {
                payload,
                duration_number: Some(value),
            } => {
                assert_eq!(payload["duration"], Value::Null);
                assert_eq!(value.to_bits(), number.to_bits());
            }
            _ => panic!("SET"),
        }
    }
}
#[test]
fn absent_null_and_untyped_source_duration_are_not_guessed_as_nonfinite() {
    for duration in [
        None,
        Some(Value::Null),
        Some(json!("61")),
        Some(json!(false)),
        Some(json!([1, 2])),
    ] {
        let mut payload = json!({"filePath":""});
        if let Some(value) = duration.clone() {
            payload["duration"] = value;
        }
        match parse(&frame(json!({"kind":"set","data":payload})))
            .unwrap()
            .command
        {
            Command::Set {
                payload,
                duration_number: None,
            } => {
                if let Some(Value::Array(expected)) = &duration {
                    let actual = payload["duration"].as_array().unwrap();
                    assert_eq!(actual.len(), expected.len());
                    for (actual, expected) in actual.iter().zip(expected) {
                        assert_eq!(actual.as_f64(), expected.as_f64());
                    }
                } else {
                    assert_eq!(payload.get("duration"), duration.as_ref());
                }
            }
            _ => panic!("source scalar"),
        }
    }
}
#[test]
fn ambiguous_duplicate_unknown_or_inexact_private_tags_reject() {
    for command in [
        json!({"kind":"open","data":null}),
        json!({"kind":"unset","duration":null}),
        json!({"kind":"ACK"}),
        json!({"kind":"set","data":null}),
        json!({"kind":"set","data":{"filePath":42}}),
        json!({"kind":"set","data":{"filePath":"x","duration":1}}),
        json!({"kind":"set","data":{"filePath":"x"},"duration":{"kind":"NaN"}}),
    ] {
        assert!(parse(&frame(command)).is_err());
    }
    for tag in [
        json!(null),
        json!({"kind":"Finite","value":"1"}),
        json!({"kind":"Finite","value":-0.0}),
        json!({"kind":"NaN","value":0}),
        json!({"kind":"unknown"}),
    ] {
        assert!(parse(&set(tag)).is_err());
    }
    let good = String::from_utf8(set(json!({"kind":"Finite","value":1}))).unwrap();
    for bad in [
        good.replace("\"kind\":\"set\"", "\"kind\":\"set\",\"kind\":\"unset\""),
        good.replace(
            "\"runtimeId\":\"native-音\"",
            "\"runtimeId\":\"native-音\",\"runtimeId\":\"foreign\"",
        ),
        good.replace(
            "\"filePath\":\"/audio.wav\"",
            "\"filePath\":\"/audio.wav\",\"filePath\":\"other\"",
        ),
        good.replace("\"value\":1", "\"value\":1e4000"),
    ] {
        assert!(parse(bad.as_bytes()).is_err());
    }
}
#[test]
fn precise_payload_numbers_and_bounded_work_do_not_flatten_into_null() {
    let raw = String::from_utf8(frame(
        json!({"kind":"set","data":{"filePath":"x","extra":null}}),
    ))
    .unwrap()
    .replace(
        "\"extra\":null",
        r#""extra":{"z":-0.0,"tiny":8.3163996793004788882e-275,"min":5e-324,"__proto__":true}"#,
    );
    match parse(raw.as_bytes()).unwrap().command {
        Command::Set { payload, .. } => {
            assert_eq!(
                payload["extra"]["z"].as_f64().unwrap().to_bits(),
                (-0.0f64).to_bits()
            );
            let tiny = "8.3163996793004788882e-275".parse::<f64>().unwrap();
            assert_eq!(
                payload["extra"]["tiny"].as_f64().unwrap().to_bits(),
                tiny.to_bits()
            );
            assert_eq!(payload["extra"]["min"].as_f64().unwrap(), f64::from_bits(1));
            assert_eq!(
                payload["extra"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>(),
                ["z", "tiny", "min", "__proto__"]
            );
        }
        _ => panic!("SET"),
    }
    assert!(parse(&vec![b' '; input::COMMAND_LIMIT + 1]).is_err());
    assert!(parse(b"").is_err());
    let mut deep = Value::Null;
    for _ in 0..9 {
        deep = json!([deep]);
    }
    for extra in [
        deep,
        json!(vec![Value::Null; 1024]),
        json!("x".repeat(65536)),
        json!("\u{1}".repeat(12000)),
    ] {
        assert!(parse(&frame(
            json!({"kind":"set","data":{"filePath":"x","extra":extra}})
        ))
        .is_err());
    }
}
#[test]
fn actual_retained_encoder_frames_parse_with_exact_binary64_and_send_time_snapshot() {
    let script = r#"
import fs from 'node:fs';import ts from 'typescript';
const source=fs.readFileSync('src/frontend/rust/nowPlayingWire.ts','utf8');
const code=ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
const {encodeNowPlayingCommand:encode}=await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
const context={runtimeId:'native-音',libraryGeneration:3,dataPath:'/selected/日本語/🎹'};
const out=[];
for(const duration of [61.25,-0,NaN,Infinity,-Infinity,5e-324]){
 const data={filePath:'/audio/音.wav',name:'Before',unknownLang:['A','T','B'],format:'{duration_s}',duration,extra:{z:-0,tiny:8.3163996793004788882e-275}};
 const bytes=encode({channel:'NOW_PLAYING',data},context);data.name='After';data.unknownLang.fill('After');
 out.push({hex:Buffer.from(bytes).toString('hex'),bits:Buffer.from(new Float64Array([duration]).buffer).toString('hex')});
}
console.log(JSON.stringify(out));
"#;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e", script])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cases.len(), 6);
    for case in cases {
        fn bytes(text: &str) -> Vec<u8> {
            text.as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
                .collect()
        }
        let frame = bytes(case["hex"].as_str().unwrap());
        let bits = bytes(case["bits"].as_str().unwrap());
        match parse(&frame).unwrap().command {
            Command::Set {
                payload,
                duration_number: Some(value),
            } => {
                assert_eq!(
                    value.to_bits(),
                    u64::from_ne_bytes(bits.try_into().unwrap())
                );
                assert_eq!(payload["name"], "Before");
                assert_eq!(payload["unknownLang"][0], "A");
                assert_eq!(
                    payload["extra"]["z"].as_f64().unwrap().to_bits(),
                    (-0.0f64).to_bits()
                );
            }
            _ => panic!("SET"),
        }
    }
}
