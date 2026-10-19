#[path = "../src/spotify_input.rs"]
mod input;
use input::{parse, FRAME_LIMIT};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};

fn context() -> Value {
    json!({"runtimeId":"native-音","libraryGeneration":3,"dataPath":"/selected/日本語/🎹"})
}
fn request(payload: Value) -> Value {
    json!({"channel":"MAIN","data":{"channel":"SPOTIFY_COMMAND","data":payload},"listenerId":"listener-音","context":context()})
}
fn frame(payload: Value) -> Vec<u8> {
    serde_json::to_vec(&request(payload)).unwrap()
}
fn rejected(value: Value) {
    assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
}
fn root() -> PathBuf {
    let file = std::path::Path::new(file!());
    let file = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file)
    };
    file.parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}
fn node(script: &str) -> Value {
    let mut child =
        Command::new(std::env::var("FREESHOW_ORACLE_NODE").unwrap_or_else(|_| "node".into()));
    if let Ok(loader) = std::env::var("FREESHOW_ORACLE_NODE_LOADER") {
        child.args(["--experimental-loader", &loader]);
    }
    let output = child
        .args(["--input-type=module", "-e", script])
        .current_dir(root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn six_commands_original_request_context_listener_and_ignored_values() {
    for command in ["playpause", "next", "prev", "pause", "seek", "setVolume"] {
        let input = parse(&frame(
            json!({"command":command,"value":0.5,"other":{"a":null}}),
        ))
        .unwrap();
        assert_eq!(input.request.channel, "MAIN");
        assert_eq!(input.request.data["channel"], "SPOTIFY_COMMAND");
        assert_eq!(input.request.data["data"]["command"], command);
        assert_eq!(input.request.listener_id.as_deref(), Some("listener-音"));
        let ctx = input.request.context.unwrap();
        assert_eq!(ctx.runtime_id, "native-音");
        assert_eq!(ctx.library_generation, 3);
        assert_eq!(ctx.data_path, PathBuf::from("/selected/日本語/🎹"));
        assert_eq!(
            input.number,
            matches!(command, "seek" | "setVolume").then_some(0.5)
        );
    }
    for command in ["playpause", "next", "prev", "pause"] {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(false)),
            Some(json!("0")),
            Some(json!([1,{"nested":true}])),
            Some(json!({"command":"seek","value":4})),
        ] {
            let mut payload = json!({"command":command});
            if let Some(value) = value {
                payload["value"] = value;
            }
            let input = parse(&frame(payload.clone())).unwrap();
            assert_eq!(input.number, None);
            assert_eq!(
                input.request.data["data"].get("value").is_some(),
                payload.get("value").is_some()
            );
        }
    }
    for listener in [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!("\0")),
        Some(json!("x".repeat(8192))),
    ] {
        let mut frame = request(json!({"command":"pause"}));
        if let Some(listener) = listener.clone() {
            frame["listenerId"] = listener;
        } else {
            frame.as_object_mut().unwrap().remove("listenerId");
        }
        let input = parse(&serde_json::to_vec(&frame).unwrap()).unwrap();
        assert_eq!(
            input.request.listener_id.as_deref(),
            listener.as_ref().and_then(Value::as_str)
        );
    }
    let ignored = node(
        r#"
import assert from 'node:assert/strict';
import {commandOracle} from './tests/parity/spotify-command-oracle.mjs';
const rows=[];
for(const [command,method] of [['playpause','PlayPause'],['next','Next'],['prev','Previous'],['pause','Pause']]){
 for(const value of [undefined,null,false,'0.5',[1,{nested:true}],{command:'seek',value:4}]){
  const original=commandOracle((_cmd,cb)=>cb(null,''));
  await original.send({command,value},'ignored');
  assert.equal(original.calls.errors,0);assert.equal(original.calls.commands.length,1);
  assert.ok(original.calls.commands[0].endsWith('org.mpris.MediaPlayer2.Player.'+method));
  assert.deepEqual(original.calls.replies,[['MAIN',{channel:'SPOTIFY_COMMAND',data:true},'ignored']]);
  rows.push(JSON.stringify({channel:'MAIN',data:{channel:'SPOTIFY_COMMAND',data:{command,value}},listenerId:'ignored',context:{runtimeId:'r',libraryGeneration:0,dataPath:'/data'}}));
 }
}
console.log(JSON.stringify(rows));
"#,
    );
    assert_eq!(ignored.as_array().unwrap().len(), 24);
    for raw in ignored.as_array().unwrap() {
        let parsed = parse(raw.as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(parsed.number, None);
        assert_eq!(parsed.request.listener_id.as_deref(), Some("ignored"));
    }
}

#[test]
fn actual_node_frames_and_pinned_source_operation_preserve_ieee_bits() {
    let cases = node(
        r#"
import assert from 'node:assert/strict';
import {commandOracle} from './tests/parity/spotify-command-oracle.mjs';
const bits=n=>{const b=Buffer.alloc(8);b.writeDoubleBE(n);return b.toString('hex')};
const ctx={runtimeId:'native-音',libraryGeneration:3,dataPath:'/selected/日本語/🎹'};
const req=data=>({channel:'MAIN',data:{channel:'SPOTIFY_COMMAND',data},listenerId:'snapshot',context:ctx});
const out=[];
async function record(command,raw,canonical){
 const original=commandOracle((cmd,cb)=>{cb(null,cmd.includes("string:'Metadata'")?'string "mpris:trackid" variant string "/private_track"':'');});
 const data=JSON.parse(raw).data.data;
 const ret=await original.send(data,'snapshot');
 assert.equal(ret,undefined);assert.equal(original.calls.errors,0);
 assert.deepEqual(original.calls.replies,[['MAIN',{channel:'SPOTIFY_COMMAND',data:true},'snapshot']]);
 const operations=original.calls.commands;
 assert.equal(operations.length,command==='seek'?2:1);
 assert.match(operations.at(-1),command==='seek'?/SetPosition objpath:\/private_track int64:-?\d+$/:/variant:double:/);
 out.push({raw,bits:bits(data.value),command,canonical,operations});
}
for(const value of [0,-0,5e-324,-5e-324,1e-7,1e21,Number.MAX_VALUE,0.5000000000000001,8.3163996793004788882e-275,-9.084938291167941e48,-1.7257423766123035e-279]){
 const snapshot=structuredClone(req({command:'setVolume',value,other:{command:'seek',value:99}}));
 const raw=JSON.stringify(snapshot);snapshot.data.data.value=44;await record('setVolume',raw,true);
}
for(const value of [0,-0,5e-324,0.0000005,-0.0000005,0.0000015,-0.0000015,9007199254.74099,-9007199254.74099]){
 await record('seek',JSON.stringify(req({command:'seek',value})),true);
}
for(const token of ['-0','-0.0','8.3163996793004788882e-275','5e-324','2.2250738585072014e-308','1.00000000000000011102230246251565404236316680908203125','1.00000000000000011102230246251565404236316680908203126','1.7976931348623157e308','1e-9999']){
 const raw=JSON.stringify(req({command:'setVolume',value:null,extra:'"value":0'})).replace('"value":null','"value":'+token);
 await record('setVolume',raw,false);
}
assert.equal(out.length,29);console.log(JSON.stringify({node:process.version,cases:out}));
"#,
    );
    assert!(cases["node"].as_str().unwrap().starts_with('v'));
    let cases = cases["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 29);
    for case in cases {
        let raw = case["raw"].as_str().unwrap();
        let parsed = parse(raw.as_bytes()).unwrap();
        let bits = u64::from_str_radix(case["bits"].as_str().unwrap(), 16).unwrap();
        assert_eq!(parsed.number.unwrap().to_bits(), bits, "{raw}");
        assert_eq!(
            parsed.request.data["data"]["value"]
                .as_f64()
                .unwrap()
                .to_bits(),
            bits,
            "retained payload {raw}"
        );
        assert_eq!(parsed.request.data["data"]["command"], case["command"]);
        assert_eq!(parsed.request.listener_id.as_deref(), Some("snapshot"));
        assert_eq!(parsed.request.context.unwrap().library_generation, 3);
    }
}

#[test]
fn numeric_kinds_range_and_irreversible_json_artifacts_are_explicit() {
    for command in ["seek", "setVolume"] {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(true)),
            Some(json!("0.5")),
            Some(json!({"valueOf":0.5})),
            Some(json!([0.5])),
        ] {
            let mut payload = json!({"command":command});
            if let Some(value) = value {
                payload["value"] = value;
            }
            assert!(parse(&frame(payload)).is_err());
        }
    }
    for token in [
        "1e400",
        "-1e400",
        "NaN",
        "Infinity",
        "-Infinity",
        "01",
        "+1",
        "1.",
        ".5",
    ] {
        let raw = String::from_utf8(frame(json!({"command":"setVolume","value":null})))
            .unwrap()
            .replace("\"value\":null", &format!("\"value\":{token}"));
        assert!(parse(raw.as_bytes()).is_err(), "{token}");
    }
    for number in [9007199254.740992, -9007199254.740992, f64::MAX] {
        assert!(parse(&frame(json!({"command":"seek","value":number}))).is_err());
        assert!(parse(&frame(json!({"command":"setVolume","value":number}))).is_ok());
    }
    let cases = node(
        r#"
const frame=value=>JSON.stringify({channel:'MAIN',data:{channel:'SPOTIFY_COMMAND',data:{command:'setVolume',value}},context:{runtimeId:'r',libraryGeneration:0,dataPath:'/data'}});
console.log(JSON.stringify([undefined,null,false,'0.5',{},new Number(0.5),NaN,Infinity,-Infinity,-0].map(value=>({kind:typeof value,raw:frame(value)}))));
"#,
    );
    let cases = cases.as_array().unwrap();
    for (index, case) in cases.iter().enumerate() {
        let actual = parse(case["raw"].as_str().unwrap().as_bytes());
        if index == 5 {
            assert_eq!(case["kind"], "object");
            assert_eq!(actual.unwrap().number, Some(0.5));
        } else if index == 9 {
            assert_eq!(actual.unwrap().number.unwrap().to_bits(), 0f64.to_bits());
        } else {
            assert!(actual.is_err(), "case{index}");
        }
    }
}

#[test]
fn strict_private_schema_duplicates_and_whole_frame_selection() {
    let good = String::from_utf8(frame(
        json!({"command":"setVolume","value":0.25,"ignored":{"a":1}}),
    ))
    .unwrap();
    for bad in [
        good.replace("\"MAIN\"", "\"OTHER\""),
        good.replace("\"SPOTIFY_COMMAND\"", "\"SPOTIFY_GET_STATE\""),
        good.replacen(
            "\"channel\":\"MAIN\"",
            "\"channel\":\"MAIN\",\"channel\":\"MAIN\"",
            1,
        ),
        good.replace(
            "\"channel\":\"SPOTIFY_COMMAND\"",
            "\"channel\":\"SPOTIFY_COMMAND\",\"channel\":\"SPOTIFY_COMMAND\"",
        ),
        good.replace(
            "\"runtimeId\":\"native-音\"",
            "\"runtimeId\":\"native-音\",\"runtimeId\":\"native-音\"",
        ),
        good.replace(
            "\"command\":\"setVolume\"",
            "\"command\":\"setVolume\",\"command\":\"pause\"",
        ),
        good.replace("\"value\":0.25", "\"value\":0.25,\"value\":0.5"),
        good.replace("\"a\":1", "\"a\":1,\"\\u0061\":2"),
        format!("{good} {good}"),
        format!("\u{feff}{good}"),
    ] {
        assert!(parse(bad.as_bytes()).is_err(), "{bad}");
    }
    for field in ["channel", "data", "context"] {
        let mut bad = request(json!({"command":"pause"}));
        bad.as_object_mut().unwrap().remove(field);
        rejected(bad);
    }
    let mut bad = request(json!({"command":"pause"}));
    bad["extra"] = Value::Null;
    rejected(bad);
    let mut bad = request(json!({"command":"pause"}));
    bad["data"]["extra"] = Value::Null;
    rejected(bad);
    let mut bad = request(json!({"command":"pause"}));
    bad["context"]["extra"] = Value::Null;
    rejected(bad);
    for bad in [
        json!(["MAIN", {"channel":"SPOTIFY_COMMAND","data":{"command":"pause"}}, null, context()]),
        json!({"channel":"MAIN","data":["SPOTIFY_COMMAND",{"command":"pause"}],"context":context()}),
        json!({"channel":"MAIN","data":{"channel":"SPOTIFY_COMMAND","data":["pause",null]},"context":context()}),
        json!({"channel":"MAIN","data":{"channel":"SPOTIFY_COMMAND","data":{"command":"pause"}},"context":["native-音",3,"/selected"]}),
    ] {
        rejected(bad);
    }
    for payload in [
        json!(null),
        json!([]),
        json!({}),
        json!({"command":null}),
        json!({"command":"SEEK","value":1}),
        json!({"command":"unknown"}),
        json!({"command":{"toString":"pause"}}),
    ] {
        assert!(parse(&frame(payload)).is_err());
    }
    let raw=br#"{"data":{"data":{"unused":{"command":"seek","value":99},"value":-0.0,"command":"setVolume","__proto__":true},"channel":"SPOTIFY_COMMAND"},"context":{"dataPath":"/data","libraryGeneration":0,"runtimeId":"r"},"channel":"MAIN"}"#;
    let actual = parse(raw).unwrap();
    assert_eq!(actual.number.unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(
        actual.request.data["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["unused", "value", "command", "__proto__"]
    );
}

#[test]
fn context_listener_string_and_payload_work_bounds() {
    for (field, values) in [
        (
            "runtimeId",
            vec![json!(""), json!("r".repeat(257)), json!("r\0x"), json!(5)],
        ),
        (
            "dataPath",
            vec![
                json!(""),
                json!("音".repeat(10923)),
                json!("/x\0y"),
                Value::Null,
            ],
        ),
        (
            "libraryGeneration",
            vec![
                json!(-1),
                json!(1.5),
                json!(9007199254740992u64),
                json!("3"),
                Value::Null,
            ],
        ),
    ] {
        for value in values {
            let mut bad = request(json!({"command":"pause"}));
            bad["context"][field] = value;
            rejected(bad);
        }
    }
    for value in [
        json!(8192),
        json!(false),
        json!([]),
        json!("x".repeat(8193)),
        json!("音".repeat(2731)),
    ] {
        let mut bad = request(json!({"command":"pause"}));
        bad["listenerId"] = value;
        rejected(bad);
    }
    let mut edge = request(json!({"command":"pause"}));
    edge["context"] = json!({"runtimeId":"r".repeat(256),"dataPath":"/".repeat(32768),"libraryGeneration":9007199254740991u64});
    assert!(parse(&serde_json::to_vec(&edge).unwrap()).is_ok());
    let surrogate = String::from_utf8(frame(json!({"command":"pause","extra":"replace"})))
        .unwrap()
        .replace("\"replace\"", "\"\\ud800\"");
    assert!(parse(surrogate.as_bytes()).is_err());
    // Only ~11KiB encoded and <4096 structural tokens, but actual host-style
    // charged Value allocation exceeds64KiB. No ignored-extra escape hatch.
    assert!(parse(&frame(
        json!({"command":"pause","extra":vec![Value::Null;2100]})
    ))
    .is_err());
    assert!(parse(&frame(json!({"command":"pause","extra":"x".repeat(65500)}))).is_err());
}

#[test]
fn iterative_preflight_limits_deep_inputs_before_recursive_owned_parsing() {
    let base = String::from_utf8(frame(json!({"command":"pause","extra":null}))).unwrap();
    for count in [14, 128, 20000] {
        let nested = format!("{}null{}", "[".repeat(count), "]".repeat(count));
        let raw = base.replace("\"extra\":null", &format!("\"extra\":{nested}"));
        let error = parse(raw.as_bytes()).unwrap_err();
        assert_eq!(error.code, "spotify_input_limit");
    }
    let nested = format!("{}null{}", "[".repeat(13), "]".repeat(13));
    assert!(parse(
        base.replace("\"extra\":null", &format!("\"extra\":{nested}"))
            .as_bytes()
    )
    .is_ok());
    let brackets = "[\\\"{}]".repeat(1000);
    assert!(parse(&frame(json!({"command":"pause","extra":brackets}))).is_ok());
    let mut padded = frame(json!({"command":"pause"}));
    padded.resize(FRAME_LIMIT, b' ');
    assert!(parse(&padded).is_ok());
    padded.push(b' ');
    assert_eq!(parse(&padded).unwrap_err().code, "spotify_input_limit");
    assert!(parse(b"").is_err());
    assert!(parse(b"\xff").is_err());
    for raw in [
        b"{]".as_slice(),
        b"{}\"unterminated",
        b"{}\"escaped\\",
        b"null",
        b"[]",
        b"{\"channel\":\"MAIN\",}",
    ] {
        assert!(parse(raw).is_err());
    }
    let raw = base.replace(
        "\"extra\":null",
        &format!("\"extra\":[{}]", vec!["null"; 4096].join(",")),
    );
    assert_eq!(
        parse(raw.as_bytes()).unwrap_err().code,
        "spotify_input_limit"
    );
}
