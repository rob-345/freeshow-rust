#[path = "../src/timecode_input.rs"]
mod timecode_input;
use freeshow_midi::Number;
use freeshow_timecode::{Command, Data, Scalar};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    time::{Duration, Instant},
};
use timecode_input::{parse_command, parse_ltc};

fn context() -> Value {
    json!({"runtimeId":"native-時間","libraryGeneration":7,"dataPath":"/private/日本語/🎹"})
}
fn command(value: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"context":context(),"command":value})).unwrap()
}
fn frame(prefix: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut frame = (prefix.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(prefix);
    frame.extend_from_slice(bytes);
    frame
}
fn ltc(length: usize) -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"context":context(),"mode":"LTC","byteLength":length}))
        .unwrap()
}
fn json_data(data: &Data) -> &Value {
    match data {
        Data::Json(value) => value,
        _ => panic!("JSON data expected"),
    }
}

#[test]
fn exact_four_commands_and_all_source_scalar_number_tags() {
    let stop = parse_command(&command(json!({"kind":"stop"}))).unwrap();
    assert!(matches!(stop.command, Command::Stop));
    assert_eq!(stop.context.runtime_id, "native-時間");
    assert_eq!(stop.context.library_generation, 7);
    assert_eq!(stop.context.data_path, PathBuf::from("/private/日本語/🎹"));
    for (tag, expected) in [
        (json!({"kind":"undefined"}), Scalar::Undefined),
        (json!({"kind":"null"}), Scalar::Null),
        (json!({"kind":"bool","value":false}), Scalar::Bool(false)),
        (
            json!({"kind":"text","value":"29.97"}),
            Scalar::Text("29.97".into()),
        ),
        (
            json!({"kind":"number","value":{"kind":"NegativeZero"}}),
            Scalar::Number(Number::NegativeZero),
        ),
    ] {
        let parsed = parse_command(&command(json!({"kind":"start","type":"receive","mode":"MTC","framerate":tag,"data":{"kind":"undefined"}}))).unwrap();
        match parsed.command {
            Command::Start {
                kind,
                mode,
                framerate,
                data,
            } => {
                assert_eq!(kind, "receive");
                assert_eq!(mode, "MTC");
                assert_eq!(framerate, expected);
                assert_eq!(data, Data::Undefined);
            }
            _ => panic!("START"),
        }
    }
    for (tag, bits) in [
        (json!({"kind":"Finite","value":25.0}), 25.0f64.to_bits()),
        (json!({"kind":"NaN"}), f64::NAN.to_bits()),
        (json!({"kind":"PositiveInfinity"}), f64::INFINITY.to_bits()),
        (
            json!({"kind":"NegativeInfinity"}),
            f64::NEG_INFINITY.to_bits(),
        ),
        (json!({"kind":"NegativeZero"}), (-0.0f64).to_bits()),
    ] {
        match parse_command(&command(json!({"kind":"value","value":tag})))
            .unwrap()
            .command
        {
            Command::Value(value) => assert_eq!(value.value().to_bits(), bits),
            _ => panic!("VALUE"),
        }
    }
    let undefined = parse_command(&command(
        json!({"kind":"status","data":{"kind":"undefined"}}),
    ))
    .unwrap();
    assert!(matches!(
        undefined.command,
        Command::Status(Data::Undefined)
    ));
    let null = parse_command(&command(
        json!({"kind":"status","data":{"kind":"json","value":null}}),
    ))
    .unwrap();
    assert!(matches!(
        null.command,
        Command::Status(Data::Json(Value::Null))
    ));
}
#[test]
fn exact_binary64_raw_tokens_negative_zero_and_unknown_json_key_order() {
    let body = String::from_utf8(command(json!({"kind":"status","data":{"kind":"json","value":null}}))).unwrap().replace("\"value\":null",r#""value":{"z":-0.0,"a/b":8.3163996793004788882e-275,"~":5e-324,"__proto__":9007199254740993,"array":[1e-4000,false,null]}"#);
    let parsed = parse_command(body.as_bytes()).unwrap();
    let Command::Status(data) = parsed.command else {
        panic!("STATUS")
    };
    let data = json_data(&data);
    assert_eq!(
        data.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["z", "a/b", "~", "__proto__", "array"]
    );
    assert_eq!(data["z"].as_f64().unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(
        data["a/b"].as_f64().unwrap().to_bits(),
        "8.3163996793004788882e-275"
            .parse::<f64>()
            .unwrap()
            .to_bits()
    );
    assert_eq!(data["~"].as_f64().unwrap().to_bits(), 1);
    assert_eq!(data["__proto__"].as_f64(), Some(9007199254740992.0));
    assert_eq!(data["array"][0].as_f64(), Some(0.0));
    let finite = String::from_utf8(command(
        json!({"kind":"value","value":{"kind":"Finite","value":1}}),
    ))
    .unwrap()
    .replace("\"value\":1", "\"value\":8.3163996793004788882e-275");
    let Command::Value(value) = parse_command(finite.as_bytes()).unwrap().command else {
        panic!("VALUE")
    };
    assert_eq!(
        value.value().to_bits(),
        "8.3163996793004788882e-275"
            .parse::<f64>()
            .unwrap()
            .to_bits()
    );
}
#[test]
fn strict_variant_shapes_tags_and_no_source_ack_or_audio_command() {
    for value in [
        json!({"kind":"stop","data":null}),
        json!({"kind":"stop","value":null}),
        json!({"kind":"value"}),
        json!({"kind":"status"}),
        json!({"kind":"status","data":{"kind":"undefined","value":null}}),
        json!({"kind":"status","data":{"kind":"json"}}),
        json!({"kind":"status","data":{"kind":"undefined","unexpected":true}}),
        json!({"kind":"value","value":{"kind":"NaN","value":null}}),
        json!({"kind":"value","value":{"kind":"Finite","value":"1"}}),
        json!({"kind":"value","value":{"kind":"Finite","value":-0.0}}),
        json!({"kind":"value","value":{"kind":"negativeZero"}}),
        json!({"kind":"start","type":"send","mode":"MTC","framerate":{"kind":"undefined"}}),
        json!({"kind":"start","type":"send","mode":"MTC","framerate":{"kind":"bool","value":1},"data":{"kind":"undefined"}}),
        json!({"kind":"status","data":{"kind":"Undefined"}}),
    ] {
        assert!(
            parse_command(&command(value.clone())).is_err(),
            "must reject {value}"
        );
    }
    for kind in ["TIMECODE_START", "audio", "ack", "success", "complete"] {
        assert_eq!(
            parse_command(&command(json!({"kind":kind})))
                .unwrap_err()
                .code,
            "timecode_input_unsupported"
        );
    }
    let mut envelope: Value = serde_json::from_slice(&command(json!({"kind":"stop"}))).unwrap();
    envelope["success"] = json!(true);
    assert_eq!(
        parse_command(&serde_json::to_vec(&envelope).unwrap())
            .unwrap_err()
            .code,
        "timecode_input_schema"
    );
}
#[test]
fn duplicates_reject_at_every_private_schema_and_source_json_layer() {
    let original = String::from_utf8(command(
        json!({"kind":"status","data":{"kind":"json","value":{"key":1}}}),
    ))
    .unwrap();
    for body in [
        original.replace("\"version\":1", "\"version\":1,\"version\":1"),
        original.replace(
            "\"context\":",
            &format!("\"context\":{},\"context\":", context()),
        ),
        original.replace("\"runtimeId\":", "\"runtimeId\":\"first\",\"runtimeId\":"),
        original.replace(
            "\"kind\":\"status\"",
            "\"kind\":\"status\",\"kind\":\"status\"",
        ),
        original.replace("\"data\":", "\"data\":{\"kind\":\"undefined\"},\"data\":"),
        original.replace("\"kind\":\"json\"", "\"kind\":\"json\",\"kind\":\"json\""),
        original.replace("\"key\":1", "\"key\":1,\"\\u006bey\":2"),
    ] {
        assert_eq!(
            parse_command(body.as_bytes()).unwrap_err().code,
            "timecode_input_schema",
            "{body}"
        );
    }
    let finite = String::from_utf8(command(
        json!({"kind":"value","value":{"kind":"Finite","value":1}}),
    ))
    .unwrap();
    for body in [
        finite.replace(
            "\"kind\":\"Finite\"",
            "\"kind\":\"Finite\",\"kind\":\"Finite\"",
        ),
        finite.replace("\"value\":1", "\"value\":1,\"value\":2"),
    ] {
        assert!(parse_command(body.as_bytes()).is_err());
    }
}
#[test]
fn context_schema_utf8_framing_and_host_limits_are_enforced() {
    for (key, value) in [
        ("runtimeId", json!("")),
        ("runtimeId", json!("a\0b")),
        ("runtimeId", json!("🙌".repeat(65))),
        ("dataPath", json!("")),
        ("dataPath", json!("a\0b")),
        ("dataPath", json!("a".repeat(32769))),
        ("libraryGeneration", json!(9007199254740992u64)),
    ] {
        let mut envelope: Value = serde_json::from_slice(&command(json!({"kind":"stop"}))).unwrap();
        envelope["context"][key] = value;
        assert_eq!(
            parse_command(&serde_json::to_vec(&envelope).unwrap())
                .unwrap_err()
                .code,
            "timecode_input_context"
        );
    }
    for body in [&[][..], &[0xff][..], b"{}", b"[]", b"null"] {
        assert!(parse_command(body).is_err());
    }
    let mut too_big = vec![b' '; timecode_input::COMMAND_LIMIT + 1];
    too_big[0] = b'{';
    assert_eq!(
        parse_command(&too_big).unwrap_err().code,
        "timecode_input_limit"
    );
    let mut nested = json!(null);
    for _ in 0..9 {
        nested = json!([nested]);
    }
    for value in [
        json!("a".repeat(65537)),
        nested,
        Value::Array(vec![Value::Null; 1024]),
    ] {
        assert_eq!(
            parse_command(&command(
                json!({"kind":"status","data":{"kind":"json","value":value}})
            ))
            .unwrap_err()
            .code,
            "timecode_input_limit"
        );
    }
    let overflow = String::from_utf8(command(
        json!({"kind":"value","value":{"kind":"Finite","value":1}}),
    ))
    .unwrap()
    .replace("\"value\":1", "\"value\":1e309");
    assert!(parse_command(overflow.as_bytes()).is_err());
    let lone = String::from_utf8(command(
        json!({"kind":"status","data":{"kind":"json","value":"x"}}),
    ))
    .unwrap()
    .replace("\"x\"", r#""\ud800""#);
    assert!(parse_command(lone.as_bytes()).is_err());
}
#[test]
fn ltc_suffix_is_borrowed_exact_and_independent_of_audio_pcm_limit() {
    for length in [0, 1, 32769, 65536] {
        let bytes = (0..length).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        let framed = frame(&ltc(length), &bytes);
        let mut outer = vec![4, 5, 6];
        outer.extend_from_slice(&framed);
        outer.push(7);
        let result = parse_ltc(&outer[3..outer.len() - 1]).unwrap();
        assert_eq!(result.mode, "LTC");
        assert_eq!(result.bytes, bytes);
        assert_eq!(result.context.runtime_id, "native-時間");
        assert_eq!(
            result.bytes.as_ptr(),
            outer[outer.len() - 1 - length..].as_ptr()
        );
    }
    for body in [
        vec![],
        vec![1, 2, 3],
        frame(&ltc(3), &[1, 2]),
        frame(&ltc(2), &[1, 2, 3]),
        frame(&ltc(65537), &vec![0; 65537]),
        frame(b"", &[]),
        frame(&vec![b' '; 49153], &[]),
    ] {
        assert!(parse_ltc(&body).is_err());
    }
    let original = String::from_utf8(ltc(1)).unwrap();
    for prefix in [
        original.replace("\"mode\":\"LTC\"", "\"mode\":\"MTC\""),
        original.replace("\"byteLength\":1", "\"byteLength\":1,\"byteLength\":1"),
        original.replace("\"version\":1", "\"version\":2"),
        original.replace("\"mode\":", "\"success\":true,\"mode\":"),
        original.replace("\"byteLength\":1", "\"byteLength\":1.5"),
    ] {
        assert!(parse_ltc(&frame(prefix.as_bytes(), &[1])).is_err());
    }
}

#[test]
fn deeply_declared_raw_json_is_rejected_without_recursive_skip_or_large_value_tree() {
    let template = String::from_utf8(command(
        json!({"kind":"status","data":{"kind":"json","value":null}}),
    ))
    .unwrap();
    let deep = format!("{}null{}", "[".repeat(20000), "]".repeat(20000));
    let body = template.replace("\"value\":null", &format!("\"value\":{deep}"));
    assert_eq!(
        parse_command(body.as_bytes()).unwrap_err().code,
        "timecode_input_limit"
    );
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect()
}
#[test]
fn actual_node22_frontend_encoder_roundtrips_into_native_commands_and_borrowed_ltc() {
    let cwd = std::env::current_dir().unwrap();
    let root = [
        Path::new(env!("CARGO_MANIFEST_DIR")),
        Path::new(file!()),
        cwd.as_path(),
    ]
    .into_iter()
    .flat_map(Path::ancestors)
    .find(|root| root.join("src/frontend/rust/timecodeWire.ts").is_file())
    .expect("repository source root for actual frontend encoder")
    .to_path_buf();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("wire.json");
    let stderr = dir.path().join("stderr");
    let script = r#"
      const fs=require('node:fs'),path=require('node:path'),ts=require('typescript');
      const code=ts.transpileModule(fs.readFileSync(path.join(process.argv[1],'src/frontend/rust/timecodeWire.ts'),'utf8'),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
      (async()=>{const {encodeTimecodeCommand:C,encodeTimecodeLtc:L}=await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
      const context={runtimeId:'native-時間',libraryGeneration:7,dataPath:'/private/日本語/🎹'};
      const numbers=[0,-0,NaN,Infinity,-Infinity,5e-324,Number.MAX_VALUE,8.3163996793004788882e-275,9007199254740992];
      const bits=n=>{const b=Buffer.alloc(8);b.writeDoubleLE(n);return b.toString('hex')};
      const values=numbers.map(n=>({wire:Buffer.from(C({channel:'TIMECODE_VALUE',data:n},context)).toString('hex'),bits:bits(n)}));
      const rates=[undefined,null,false,true,'29.97',...numbers];
      const starts=rates.map(n=>({wire:Buffer.from(C({channel:'TIMECODE_START',data:{type:'receive',mode:'MTC',framerate:n,data:undefined}},context)).toString('hex'),scalar:typeof n==='number'?{type:'number',bits:bits(n)}:{type:n===undefined?'undefined':n===null?'null':typeof n,value:n}}));
      const status=[{channel:'TIMECODE_STATUS'},{channel:'TIMECODE_STATUS',data:undefined},{channel:'TIMECODE_STATUS',data:null},{channel:'TIMECODE_STATUS',data:{z:-0,'a/b':8.3163996793004788882e-275,'~':5e-324,'__proto__':null,array:[false,null,'🎹']}}].map(m=>Buffer.from(C(m,context)).toString('hex'));
      const backing=Uint8Array.from({length:65538},(_,i)=>i%256);const raw=L(backing.subarray(1,65537),'LTC',context);backing.fill(22);
      const stop=Buffer.from(C({channel:'TIMECODE_STOP',data:{ignored:'source stop ignores data'}},context)).toString('hex');
      const jsonStart=Buffer.from(C({channel:'TIMECODE_START',data:{type:'send',mode:'MTC',data:JSON.parse('{"midiOutput":0,"unknown":"🎹","__proto__":"data only"}')}},context)).toString('hex');
      fs.writeFileSync(process.argv[2],JSON.stringify({values,starts,status,stop,jsonStart,ltc:Buffer.from(raw).toString('hex')}));})().catch(e=>{console.error(e);process.exitCode=1});
    "#;
    let node = std::env::var_os("FREESHOW_TIMECODE_NODE").unwrap_or_else(|| "node".into());
    let mut child = Process::new(node)
        .args(["-e", script])
        .arg(&root)
        .arg(&output)
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .expect("Node+TypeScript is mandatory cross-language test prerequisite");
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("bounded Node encoder oracle timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut diagnostic = String::new();
    std::fs::File::open(stderr)
        .unwrap()
        .take(16384)
        .read_to_string(&mut diagnostic)
        .unwrap();
    assert!(status.success(), "{diagnostic}");
    assert!(std::fs::metadata(&output).unwrap().len() < 1024 * 1024);
    let output: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    for case in output["values"].as_array().unwrap() {
        let parsed = parse_command(&hex(case["wire"].as_str().unwrap())).unwrap();
        let Command::Value(number) = parsed.command else {
            panic!("VALUE")
        };
        assert_eq!(
            number.value().to_le_bytes().to_vec(),
            hex(case["bits"].as_str().unwrap())
        );
    }
    for case in output["starts"].as_array().unwrap() {
        let parsed = parse_command(&hex(case["wire"].as_str().unwrap())).unwrap();
        let Command::Start {
            framerate, data, ..
        } = parsed.command
        else {
            panic!("START")
        };
        assert_eq!(data, Data::Undefined);
        match (case["scalar"]["type"].as_str().unwrap(), framerate) {
            ("undefined", Scalar::Undefined) | ("null", Scalar::Null) => {}
            ("boolean", Scalar::Bool(value)) => {
                assert_eq!(value, case["scalar"]["value"].as_bool().unwrap())
            }
            ("string", Scalar::Text(value)) => {
                assert_eq!(value, case["scalar"]["value"].as_str().unwrap())
            }
            ("number", Scalar::Number(number)) => assert_eq!(
                number.value().to_le_bytes().to_vec(),
                hex(case["scalar"]["bits"].as_str().unwrap())
            ),
            other => panic!("scalar mismatch {other:?}"),
        }
    }
    for (index, case) in output["status"].as_array().unwrap().iter().enumerate() {
        let Command::Status(data) = parse_command(&hex(case.as_str().unwrap())).unwrap().command
        else {
            panic!("STATUS")
        };
        match index {
            0 | 1 => assert_eq!(data, Data::Undefined),
            2 => assert_eq!(data, Data::Json(Value::Null)),
            _ => {
                let value = json_data(&data);
                assert!(value["z"].as_f64().unwrap().is_sign_negative());
                assert_eq!(value["~"].as_f64().unwrap().to_bits(), 1);
                assert_eq!(value["array"][2], "🎹");
            }
        }
    }
    assert!(matches!(
        parse_command(&hex(output["stop"].as_str().unwrap()))
            .unwrap()
            .command,
        Command::Stop
    ));
    let Command::Start { data, .. } = parse_command(&hex(output["jsonStart"].as_str().unwrap()))
        .unwrap()
        .command
    else {
        panic!("START");
    };
    let data = json_data(&data);
    assert_eq!(
        data.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["midiOutput", "unknown", "__proto__"]
    );
    assert_eq!(data["midiOutput"].as_f64(), Some(0.0));
    assert_eq!(data["unknown"], "🎹");
    assert_eq!(data["__proto__"], "data only");
    let framed = hex(output["ltc"].as_str().unwrap());
    let raw = parse_ltc(&framed).unwrap();
    assert_eq!(raw.bytes.len(), 65536);
    for (index, byte) in raw.bytes.iter().enumerate() {
        assert_eq!(*byte, ((index + 1) % 256) as u8);
    }
}
