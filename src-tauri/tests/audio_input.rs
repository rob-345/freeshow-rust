#[path = "../src/audio_input.rs"]
mod audio_input;
use audio_input::{parse, AudioOrigin};
use freeshow_audio_pipeline::JsInput;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn prefix(metadata: Value) -> Value {
    json!({"version":1,"context":{"runtimeId":"native-音楽","libraryGeneration":7,"dataPath":"/private/日本語/🎹"},"origin":"port","metadata":metadata})
}
fn raw(json: &[u8], pcm: &[u8]) -> Vec<u8> {
    let mut frame = (json.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(json);
    frame.extend_from_slice(pcm);
    frame
}
fn frame(prefix: &Value, pcm: &[u8]) -> Vec<u8> {
    raw(&serde_json::to_vec(prefix).unwrap(), pcm)
}
fn value(input: &JsInput) -> &Value {
    match input {
        JsInput::Json(value) => value,
        other => panic!("expected unchanged JSON, got {other:?}"),
    }
}
fn code(prefix: &Value) -> &'static str {
    parse(&frame(prefix, &[1])).unwrap_err().code
}

#[test]
fn exact_borrowed_pcm_range_preserves_nonzero_outer_offsets_and_unicode_context() {
    let prefix = prefix(json!({"sampleRate":"44100","id":["raw",1],"icecast":{"future":"日本語"}}));
    let framed = frame(&prefix, &[0, 255, 17, 4, 0]);
    let mut storage = vec![90, 91, 92];
    storage.extend_from_slice(&framed);
    storage.push(93);
    let result = parse(&storage[3..storage.len() - 1]).unwrap();
    assert_eq!(result.pcm, [0, 255, 17, 4, 0]);
    assert_eq!(result.pcm.as_ptr(), storage[storage.len() - 6..].as_ptr());
    assert_eq!(result.context.runtime_id, "native-音楽");
    assert_eq!(result.context.library_generation, 7);
    assert_eq!(
        result.context.data_path.to_str(),
        Some("/private/日本語/🎹")
    );
    assert_eq!(result.origin, AudioOrigin::Port);
    assert_eq!(value(&result.metadata.sample_rate), "44100");
    assert_eq!(value(&result.metadata.target_id)[0], "raw");
    assert_eq!(value(&result.metadata.target_id)[1].as_f64(), Some(1.0));
    assert_eq!(result.metadata.icecast.unwrap()["future"], "日本語");
}
#[test]
fn missing_and_null_fields_remain_distinct_and_no_source_coercion_or_latest_latch_is_added() {
    let empty = frame(&prefix(json!({})), &[]);
    let missing = parse(&empty).unwrap();
    assert!(missing.pcm.is_empty());
    assert!(matches!(missing.metadata.sample_rate, JsInput::Undefined));
    assert!(matches!(missing.metadata.target_id, JsInput::Undefined));
    assert!(missing.metadata.icecast.is_none());
    let null = frame(
        &prefix(json!({"sampleRate":null,"id":null,"icecast":null})),
        &[1],
    );
    let null = parse(&null).unwrap();
    assert_eq!(value(&null.metadata.sample_rate), &Value::Null);
    assert_eq!(value(&null.metadata.target_id), &Value::Null);
    assert_eq!(null.metadata.icecast, Some(Value::Null));
    let first = frame(
        &prefix(json!({"sampleRate":false,"id":0,"icecast":{"enabled":true}})),
        &[1],
    );
    assert_eq!(value(&parse(&first).unwrap().metadata.sample_rate), false);
    let last = parse(&empty).unwrap();
    assert!(last.metadata.icecast.is_none());
    assert!(matches!(last.metadata.sample_rate, JsInput::Undefined));
}
#[test]
fn negative_zero_paths_restore_nested_objects_arrays_escaped_and_prototype_named_data() {
    let mut prefix = prefix(
        json!({"sampleRate":0,"id":[0,1],"icecast":{"a/b":{"~":0},"":0,"__proto__":0,"constructor":{"prototype":0},"01":0}}),
    );
    prefix["negativeZeroPaths"] = json!([
        "/sampleRate",
        "/id/0",
        "/icecast/a~1b/~0",
        "/icecast/",
        "/icecast/__proto__",
        "/icecast/constructor/prototype",
        "/icecast/01"
    ]);
    let frame = frame(&prefix, &[5, 6]);
    let result = parse(&frame).unwrap();
    assert!(value(&result.metadata.sample_rate)
        .as_f64()
        .unwrap()
        .is_sign_negative());
    let id = value(&result.metadata.target_id);
    assert!(id[0].as_f64().unwrap().is_sign_negative());
    assert_eq!(id[1].as_f64(), Some(1.0));
    let icecast = result.metadata.icecast.unwrap();
    for field in [
        &icecast["a/b"]["~"],
        &icecast[""],
        &icecast["__proto__"],
        &icecast["constructor"]["prototype"],
        &icecast["01"],
    ] {
        assert!(field.as_f64().unwrap().is_sign_negative());
    }
    assert_eq!(
        icecast
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a/b", "", "__proto__", "constructor", "01"]
    );
}
#[test]
fn binary64_conversion_preserves_the_one_ulp_case_rounding_subnormals_and_raw_negative_zero() {
    let prefix = br#"{"version":1,"context":{"runtimeId":"native","libraryGeneration":1,"dataPath":"/p"},"origin":"PCM","metadata":{"sampleRate":8.3163996793004788882e-275,"id":9007199254740993,"icecast":{"subnormal":5e-324,"underflow":1e-4000,"zero":-0}}}"#;
    let frame = raw(prefix, &[1]);
    let result = parse(&frame).unwrap();
    assert_eq!(result.origin, AudioOrigin::Pcm);
    assert_eq!(
        value(&result.metadata.sample_rate).as_f64(),
        Some("8.3163996793004788882e-275".parse().unwrap())
    );
    assert_eq!(
        value(&result.metadata.target_id).as_f64(),
        Some(9007199254740992.0)
    );
    let icecast = result.metadata.icecast.unwrap();
    assert_eq!(icecast["subnormal"].as_f64().unwrap().to_bits(), 1);
    assert_eq!(icecast["underflow"].as_f64(), Some(0.0));
    assert!(icecast["zero"].as_f64().unwrap().is_sign_negative());
    let overflow = String::from_utf8(prefix.to_vec())
        .unwrap()
        .replace("8.3163996793004788882e-275", "1e309");
    assert_eq!(
        parse(&raw(overflow.as_bytes(), &[1])).unwrap_err().code,
        "audio_input_number"
    );
}
#[test]
fn exact_envelope_schema_rejects_unknown_missing_duplicate_and_wrong_typed_fields() {
    for key in ["version", "context", "origin", "metadata"] {
        let mut bad = prefix(json!({}));
        bad.as_object_mut().unwrap().remove(key);
        assert_eq!(code(&bad), "audio_input_json");
    }
    for (key, replacement) in [
        ("sink", json!({"ndi":true})),
        ("negativeZeroPaths", Value::Null),
        ("origin", json!("pcm")),
        ("origin", json!("AUDIO")),
        ("origin", json!(1)),
        ("version", json!(null)),
    ] {
        let mut bad = prefix(json!({}));
        bad[key] = replacement;
        assert_eq!(code(&bad), "audio_input_json");
    }
    let mut bad = prefix(json!({}));
    bad["version"] = json!(2);
    assert_eq!(code(&bad), "audio_input_version");
    let mut bad = prefix(json!({}));
    bad["metadata"] = json!([]);
    assert_eq!(code(&bad), "audio_input_metadata");
    let mut bad = prefix(json!({"sink":"renderer-authority"}));
    assert_eq!(code(&bad), "audio_input_metadata");
    bad = prefix(json!({}));
    bad["context"]["extra"] = json!(false);
    assert_eq!(code(&bad), "audio_input_json");
    let base = serde_json::to_string(&prefix(json!({}))).unwrap();
    for duplicate in [
        base.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
        base.replacen("\"runtimeId\":", "\"runtimeId\":\"x\",\"runtimeId\":", 1),
        base.replace("\"metadata\":{}", "\"metadata\":{\"id\":0,\"id\":0}"),
        base.replace(
            "\"metadata\":{}",
            "\"metadata\":{\"icecast\":{\"x\":0,\"x\":0}}",
        ),
    ] {
        assert_eq!(
            parse(&raw(duplicate.as_bytes(), &[])).unwrap_err().code,
            "audio_input_json"
        );
    }
}
#[test]
fn prefix_pcm_and_total_body_boundaries_are_checked_without_scanning_pcm_as_json() {
    for body in [&[][..], &[0][..], &[1, 2, 3][..]] {
        assert_eq!(parse(body).unwrap_err().code, "audio_input_framing");
    }
    for length in [0u32, 49153, u32::MAX] {
        assert_eq!(
            parse(&length.to_le_bytes()).unwrap_err().code,
            "audio_input_limit"
        );
    }
    assert_eq!(
        parse(&[2, 0, 0, 0, b'{']).unwrap_err().code,
        "audio_input_framing"
    );
    assert_eq!(
        parse(&raw(&[0xff], &[])).unwrap_err().code,
        "audio_input_json"
    );
    let mut json = serde_json::to_vec(&prefix(json!({}))).unwrap();
    json.resize(49152, b' ');
    let boundary = raw(&json, &vec![0xff; 32768]);
    assert_eq!(parse(&boundary).unwrap().pcm.len(), 32768);
    let extra_pcm = raw(
        &serde_json::to_vec(&prefix(json!({}))).unwrap(),
        &vec![0; 32769],
    );
    assert_eq!(parse(&extra_pcm).unwrap_err().code, "audio_input_limit");
    assert_eq!(
        parse(&vec![0; 4 + 49152 + 32769]).unwrap_err().code,
        "audio_input_limit"
    );
    let mut trailing = serde_json::to_vec(&prefix(json!({}))).unwrap();
    trailing.push(b'x');
    assert_eq!(
        parse(&raw(&trailing, &[])).unwrap_err().code,
        "audio_input_json"
    );
}
#[test]
fn native_utf8_context_bytes_and_safe_generation_have_exact_bounds() {
    let mut prefix = prefix(json!({}));
    prefix["context"]["runtimeId"] = json!("é".repeat(128));
    prefix["context"]["dataPath"] = json!(format!("/{}x", "é".repeat(16383)));
    prefix["context"]["libraryGeneration"] = json!(9007199254740991u64);
    assert!(parse(&frame(&prefix, &[])).is_ok());
    for (key, value) in [
        ("runtimeId", json!("é".repeat(129))),
        ("dataPath", json!("é".repeat(16385))),
        ("runtimeId", json!("")),
        ("dataPath", json!("")),
        ("runtimeId", json!("bad\0id")),
        ("dataPath", json!("bad\0path")),
        ("libraryGeneration", json!(9007199254740992u64)),
    ] {
        let mut bad = prefix.clone();
        bad["context"][key] = value;
        assert_eq!(code(&bad), "audio_input_context");
    }
}
#[test]
fn metadata_encoded_bytes_nodes_and_depth_are_limited_before_reconstruction() {
    assert!(parse(&frame(&prefix(json!({"id":"x".repeat(8183)})), &[])).is_ok());
    assert_eq!(
        code(&prefix(json!({"id":"x".repeat(8184)}))),
        "audio_input_limit"
    );
    assert!(parse(&frame(&prefix(json!({"icecast":vec![0;254]})), &[])).is_ok());
    assert_eq!(
        code(&prefix(json!({"icecast":vec![0;255]}))),
        "audio_input_limit"
    );
    let mut nested = json!(0);
    for _ in 0..7 {
        nested = json!([nested]);
    }
    assert!(parse(&frame(&prefix(json!({"icecast":nested.clone()})), &[])).is_ok());
    assert_eq!(
        code(&prefix(json!({"icecast":[nested]}))),
        "audio_input_limit"
    );
}
#[test]
fn negative_zero_paths_reject_nonzero_nonnumber_aliases_escapes_duplicates_and_overlap() {
    let mut prefix = prefix(
        json!({"sampleRate":0,"id":[0],"icecast":{"zero":0,"text":"0","null":null,"bool":false,"one":1}}),
    );
    for path in [
        "",
        "#/sampleRate",
        "sampleRate",
        "/missing",
        "/id/00",
        "/id/+0",
        "/id/-",
        "/id/1",
        "/icecast/text",
        "/icecast/null",
        "/icecast/bool",
        "/icecast/one",
        "/icecast/zero~",
        "/icecast/zero~2",
    ] {
        prefix["negativeZeroPaths"] = json!([path]);
        assert_eq!(code(&prefix), "audio_input_path", "{path}");
    }
    prefix["negativeZeroPaths"] = json!(["/sampleRate", "/sampleRate"]);
    assert_eq!(code(&prefix), "audio_input_path");
    prefix["icecast"] = json!(0); // Unknown envelope fields cannot become a target.
    assert_eq!(code(&prefix), "audio_input_json");
    prefix.as_object_mut().unwrap().remove("icecast");
    prefix["metadata"]["icecast"] = json!(0);
    prefix["negativeZeroPaths"] = json!(["/icecast", "/icecast/nested"]);
    let bytes = frame(&prefix, &[1]);
    let original = bytes.clone();
    assert_eq!(parse(&bytes).unwrap_err().code, "audio_input_path");
    assert_eq!(
        bytes, original,
        "failed parsing/restoration never modifies raw input"
    );
}
#[test]
fn negative_zero_path_count_utf8_bytes_and_depth_are_bounded() {
    let key = "x".repeat(1015);
    let mut prefix = prefix(json!({"icecast":{key.clone():0}}));
    prefix["negativeZeroPaths"] = json!([format!("/icecast/{key}")]);
    assert!(parse(&frame(&prefix, &[])).is_ok());
    prefix["negativeZeroPaths"] = json!([format!("/icecast/{key}x")]);
    assert_eq!(code(&prefix), "audio_input_limit");
    prefix["negativeZeroPaths"] = json!(vec!["/icecast"; 257]);
    assert_eq!(code(&prefix), "audio_input_limit");
    prefix["negativeZeroPaths"] = json!(["/a/b/c/d/e/f/g/h/i"]);
    assert_eq!(code(&prefix), "audio_input_limit");
    prefix["negativeZeroPaths"] = json!([123]);
    assert_eq!(code(&prefix), "audio_input_json");
}
#[test]
fn all_provenances_are_exact_and_raw_suffix_has_no_fabricated_audio_reply() {
    for (origin, expected) in [
        ("port", AudioOrigin::Port),
        ("PCM", AudioOrigin::Pcm),
        ("CAPTURE", AudioOrigin::Capture),
    ] {
        let mut prefix = prefix(json!({}));
        prefix["origin"] = json!(origin);
        let body = frame(&prefix, &[0xff, 0xfe, b'{', b'\0']);
        let parsed = parse(&body).unwrap();
        assert_eq!(parsed.origin, expected);
        assert_eq!(parsed.pcm, [0xff, 0xfe, b'{', b'\0']);
    }
}

// Test-only base64 decoding of Node's bounded actual-encoder receipt; this is
// not another implementation of the raw frame or numeric restoration logic.
fn unbase64(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 4, 0);
    let mut bytes = Vec::new();
    for block in input.as_bytes().as_chunks::<4>().0 {
        let mut number = 0u32;
        for &byte in block {
            number = (number << 6)
                | match byte {
                    b'A'..=b'Z' => u32::from(byte - b'A'),
                    b'a'..=b'z' => u32::from(byte - b'a') + 26,
                    b'0'..=b'9' => u32::from(byte - b'0') + 52,
                    b'+' => 62,
                    b'/' => 63,
                    b'=' => 0,
                    _ => panic!("non-base64 actual encoder output"),
                };
        }
        bytes.push((number >> 16) as u8);
        if block[2] != b'=' {
            bytes.push((number >> 8) as u8);
        }
        if block[3] != b'=' {
            bytes.push(number as u8);
        }
    }
    bytes
}
#[test]
fn actual_frontend_encoder_to_native_parser_preserves_offsets_unicode_null_missing_and_negative_zero(
) {
    let root = std::env::var_os("FREESHOW_AUDIO_INPUT_ORACLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        });
    let node = std::env::var_os("FREESHOW_ORACLE_NODE").unwrap_or_else(|| "node".into());
    let script = r#"
import fs from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
const source = fs.readFileSync(path.join(process.argv[1], 'src/frontend/rust/audioRaw.ts'), 'utf8');
const js = ts.transpileModule(source, {compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
const {encodeAudioFrame} = await import('data:text/javascript;base64,' + Buffer.from(js).toString('base64'));
const context = {runtimeId:'native-音楽',libraryGeneration:7,dataPath:'/private/日本語/🎹'};
const storage = new Uint8Array([99,0,1,254,0,88]);
const config = JSON.parse('{"a/b":{"~é":0},"__proto__":0,"future":"保留"}');
config['a/b']['~é'] = -0; config.__proto__ = -0;
const frames = [
  encodeAudioFrame(storage.subarray(1,5), {sampleRate:-0,id:undefined,icecast:config}, context, 'port'),
  encodeAudioFrame(new Uint8Array([5]), {sampleRate:null,id:null,icecast:null}, context, 'CAPTURE'),
  encodeAudioFrame(new Uint8Array(), {}, context, 'PCM')
];
process.stdout.write(JSON.stringify({node:process.versions.node,frames:frames.map(frame=>Buffer.from(frame).toString('base64'))}));
"#;
    let mut child = Command::new(node)
        .current_dir(&root)
        .args(["--input-type=module", "-e", script])
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("pinned Node source oracle must be available");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("actual frontend encoder oracle exceeded ten seconds (killed/reaped)");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= 8192);
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        receipt["node"], "22.21.1",
        "use the pinned source-oracle Node runtime"
    );
    let frames = receipt["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| unbase64(value.as_str().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 3);
    let first = parse(&frames[0]).unwrap();
    assert_eq!(first.pcm, [0, 1, 254, 0]);
    let prefix_length = u32::from_le_bytes(frames[0][..4].try_into().unwrap()) as usize;
    assert_eq!(first.pcm.as_ptr(), frames[0][4 + prefix_length..].as_ptr());
    assert_eq!(first.context.runtime_id, "native-音楽");
    assert_eq!(first.context.data_path.to_str(), Some("/private/日本語/🎹"));
    assert!(matches!(first.metadata.target_id, JsInput::Undefined));
    assert!(value(&first.metadata.sample_rate)
        .as_f64()
        .unwrap()
        .is_sign_negative());
    let config = first.metadata.icecast.unwrap();
    assert!(config["a/b"]["~é"].as_f64().unwrap().is_sign_negative());
    assert!(config["__proto__"].as_f64().unwrap().is_sign_negative());
    assert_eq!(config["future"], "保留");
    let null = parse(&frames[1]).unwrap();
    assert_eq!(null.origin, AudioOrigin::Capture);
    assert_eq!(value(&null.metadata.sample_rate), &Value::Null);
    assert_eq!(value(&null.metadata.target_id), &Value::Null);
    assert_eq!(null.metadata.icecast, Some(Value::Null));
    let missing = parse(&frames[2]).unwrap();
    assert!(matches!(missing.metadata.sample_rate, JsInput::Undefined));
    assert!(matches!(missing.metadata.target_id, JsInput::Undefined));
    assert!(missing.metadata.icecast.is_none());
}
