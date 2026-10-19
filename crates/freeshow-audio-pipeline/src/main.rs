//! Differential harness only. No runtime sidecar or output backend.
use freeshow_audio_pipeline::*;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use tokio_util::sync::CancellationToken;
fn bytes(v: &Value) -> Vec<u8> {
    let text = v.as_str().unwrap();
    assert!(text.len() <= 1024 * 1024);
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn scalar(v: Option<&Value>) -> JsInput {
    match v {
        None => JsInput::Undefined,
        Some(v) if v.get("numberBits").is_some() => JsInput::Number(f64::from_bits(
            u64::from_str_radix(v["numberBits"].as_str().unwrap(), 16).unwrap(),
        )),
        Some(v) => JsInput::Json(v.clone()),
    }
}
fn sinks(v: &Value) -> SinkSnapshot {
    let keys = |n| {
        v[n].as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default()
    };
    SinkSnapshot::new(
        ContextId(1, 1),
        keys("ndi"),
        keys("omt"),
        keys("blackmagic"),
        v["output"].as_bool().unwrap_or(false),
        v["webrtc"].as_bool().unwrap_or(false),
        v["rtmp"].as_bool().unwrap_or(false),
    )
    .unwrap()
}
fn intents(v: &[SinkIntent]) -> Value {
    json!(v.iter().map(|i|json!({"kind":i.kind(),"hex":hex(i.buffer()),"sampleRateBits":format!("{:016x}",i.sample_rate().to_bits()),"target":i.target_id(),"channels":i.channel_count()})).collect::<Vec<_>>())
}
fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = String::new();
        if (&mut input)
            .take(8 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .unwrap()
            == 0
            || line.len() > 8 * 1024 * 1024
        {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        let config = PipelineConfig::default();
        let value = if request["mode"] == "convert" {
            let input = bytes(&request["hex"]);
            match convert_stereo(&input, &config, &CancellationToken::new()) {
                Ok(v) => json!({"hex":hex(&v)}),
                Err(e) => json!({"error":e}),
            }
        } else if request["mode"] == "resample" {
            let mut resampler = StereoResampler::default();
            json!(request["packets"].as_array().unwrap().iter().map(|p|{let rate=match scalar(p.get("rate")){JsInput::Number(v)=>v,JsInput::Json(v)=>v.as_f64().unwrap(),_=>48000.0};match resampler.process(&bytes(&p["hex"]),rate,&config,&CancellationToken::new()){Ok(v)=>json!({"hex":hex(&v),"state":resampler.state_bits().map(|(l,r,phase,rate)|json!([l,r,format!("{phase:016x}"),format!("{rate:016x}")]))}),Err(e)=>json!({"error":e})}}).collect::<Vec<_>>())
        } else {
            let service =
                AudioPipelineService::new(config, request["opus"].as_bool().unwrap_or(true))
                    .unwrap();
            let result=rt.block_on(async{let mut results=vec![];for p in request["packets"].as_array().unwrap(){let meta=AudioMetadata{sample_rate:scalar(p.get("sampleRate")),target_id:scalar(p.get("id")),icecast:p.get("icecast").cloned()};let snapshot=sinks(&p["sinks"]);match service.prepare(&bytes(&p["hex"]),&meta,&snapshot,CancellationToken::new()).await{Ok(plan)=>{let pre=intents(plan.planar_intents());let frames:Vec<_>=plan.opus_frames().iter().map(|f|hex(f.bytes())).collect();let icecast=plan.icecast_config().cloned();let mut accepted=plan.accept(ContextId(1,1)).unwrap();let outcome=if p["fail"].is_object(){EncodingOutcome::Failed{index:p["fail"]["index"].as_u64().unwrap()as usize,stage:if p["fail"]["stage"]=="send"{FailureStage::Send}else{FailureStage::Encode},message:"controlled failure".into()}}else{EncodingOutcome::AllSucceeded};match service.finish(&mut accepted,ContextId(1,1),outcome).await{Ok(v)=>results.push(json!({"pre":pre,"frames":frames,"icecast":icecast,"legacy":intents(v.legacy_intents()),"diagnostics":v.diagnostics(),"accumulator":hex(v.debug_accumulator()),"state":v.debug_resampler_bits().map(|(l,r,phase,rate)|json!([l,r,format!("{phase:016x}"),format!("{rate:016x}")]))})),Err(e)=>results.push(json!({"error":e}))}},Err(e)=>results.push(json!({"error":e}))}}service.shutdown().await.unwrap();results});
            json!(result)
        };
        serde_json::to_writer(&mut output, &value).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
    }
}
