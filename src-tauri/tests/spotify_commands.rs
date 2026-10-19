#[path = "../src/spotify.rs"]
mod spotify;
use freeshow_core::{protocol::Request, runtime::Runtime, service::LibraryService};
#[cfg(target_os = "linux")]
use freeshow_core::{
    protocol::{Event, RuntimeContext},
    WireError,
};
use serde_json::{json, Value};
use spotify::NativeSpotify;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
const LIMIT: Duration = Duration::from_secs(10);
fn request(library: &LibraryService, command: &str, value: Option<Value>) -> Request {
    let mut payload = json!({"command":command,"ignored":{"path":"/not-native-authority","bus":"not-native-authority"}});
    if let Some(value) = value {
        payload["value"] = value;
    }
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"SPOTIFY_COMMAND","data":payload},"listenerId":"command-音楽","context":library.context().unwrap()})).unwrap()
}
fn fixture() -> (tempfile::TempDir, LibraryService, NativeSpotify) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let spotify = NativeSpotify::new(&library.context().unwrap());
    (temp, library, spotify)
}

#[tokio::test]
async fn command_validation_precedes_lazy_admission() {
    #[cfg(target_os = "linux")]
    if let Ok(case) = std::env::var("FREESHOW_SPOTIFY_COMMAND_HOST_CASE") {
        linux::run(&case).await;
        return;
    }
    fn send_sync<T: Send + Sync>() {}
    send_sync::<freeshow_spotify::CommandReceipt>();
    let (_temp, library, spotify) = fixture();
    spotify.context_changed(&library.context().unwrap());
    for (command, value) in [
        ("unknown", None),
        ("seek", None),
        ("setVolume", None),
        ("setVolume", Some(Value::Null)),
        ("seek", Some(json!("0.5"))),
        ("seek", Some(json!(true))),
        ("seek", Some(json!({}))),
        ("seek", Some(json!(1e20))),
    ] {
        assert_eq!(
            spotify
                .dispatch(&library, request(&library, command, value), "main")
                .await
                .unwrap_err()
                .code,
            "spotify_command_unsupported"
        );
    }
    for (command, value, number) in [
        ("seek", Some(json!(0)), f64::INFINITY),
        ("setVolume", Some(json!(0)), f64::NAN),
        ("setVolume", None, 0.5),
        ("seek", Some(Value::Null), 0.5),
        ("setVolume", Some(json!(false)), 0.5),
        ("pause", Some(json!(0)), 0.5),
    ] {
        assert_eq!(
            spotify
                .dispatch_with_number(&library, request(&library, command, value), "main", number)
                .await
                .unwrap_err()
                .code,
            "spotify_command_unsupported"
        );
    }
    let mut too_large = request(&library, "pause", None);
    too_large.data["data"]["ignored"] = json!("x".repeat(64 * 1024));
    assert_eq!(
        spotify
            .dispatch(&library, too_large, "main")
            .await
            .unwrap_err()
            .code,
        "spotify_resource_limit"
    );
    let mut deep = json!(null);
    for _ in 0..17 {
        deep = json!([deep]);
    }
    let mut too_deep = request(&library, "pause", None);
    too_deep.data["data"]["ignored"] = deep;
    assert_eq!(
        spotify
            .dispatch(&library, too_deep, "main")
            .await
            .unwrap_err()
            .code,
        "spotify_resource_limit"
    );
    for component in [
        "missing",
        "runtime",
        "generation",
        "path",
        "listener",
        "role",
    ] {
        let mut invalid = request(&library, "pause", None);
        match component {
            "missing" => invalid.context = None,
            "runtime" => invalid.context.as_mut().unwrap().runtime_id.push('x'),
            "generation" => invalid.context.as_mut().unwrap().library_generation += 1,
            "path" => invalid.context.as_mut().unwrap().data_path.push("other"),
            "listener" => invalid.listener_id = Some("x".repeat(8193)),
            _ => {}
        }
        let expected = match component {
            "role" => "spotify_role",
            "listener" => "spotify_resource_limit",
            _ => "stale_library",
        };
        assert_eq!(
            spotify
                .dispatch(
                    &library,
                    invalid,
                    if component == "role" {
                        "output-1"
                    } else {
                        "main"
                    }
                )
                .await
                .unwrap_err()
                .code,
            expected
        );
    }
    assert!(spotify.status().is_none());
    assert_eq!(spotify.test_registered_commands(), 0);
    spotify.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn real_command_host_and_source_private_bus_evidence() {
    use std::{
        io::Read,
        process::{Command, Stdio},
        thread,
        time::Instant,
    };
    let source = std::path::Path::new(file!());
    let source = if source.is_absolute() {
        source.to_owned()
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(source)
    };
    let root = source.ancestors().nth(3).unwrap();
    let mut child = Command::new(
        std::env::var_os("FREESHOW_SPOTIFY_HOST_NODE").unwrap_or_else(|| "node".into()),
    )
    .args(["--input-type=module", "-e", linux::SUPERVISOR])
    .arg(std::env::current_exe().unwrap())
    .arg(root)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    fn read(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = vec![];
            pipe.by_ref()
                .take(128 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        })
    }
    let out = read(child.stdout.take().unwrap());
    let err = read(child.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let waited = child.wait().unwrap();
            panic!(
                "Owned supervisor timeout, waited {waited}: {:?} {:?}",
                out.join().unwrap(),
                err.join().unwrap()
            )
        }
        thread::sleep(Duration::from_millis(5));
    };
    let out = out.join().unwrap();
    let err = err.join().unwrap();
    assert!(out.len() <= 128 * 1024 && err.len() <= 128 * 1024);
    assert!(
        status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let receipt: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        receipt["cases"].as_array().unwrap().len(),
        if std::env::var_os("FREESHOW_SPOTIFY_COMMAND_ONLY").is_some() {
            1
        } else {
            8
        }
    );
    assert_eq!(receipt["ownedFixtureProcessesRemaining"], 0);
    println!(
        "Spotify command host evidence: {}",
        String::from_utf8(out).unwrap()
    );
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use futures_util::FutureExt;
    use std::{fs, panic::AssertUnwindSafe, path::Path};
    use tokio::task::JoinHandle;
    struct Rig {
        temp: tempfile::TempDir,
        library: LibraryService,
        spotify: NativeSpotify,
        replacements: Vec<NativeSpotify>,
        observer: Option<JoinHandle<Result<Vec<Event>, WireError>>>,
        release: Vec<tokio::sync::oneshot::Sender<()>>,
        specs: Vec<Value>,
        events: Vec<Value>,
    }
    impl Rig {
        fn new() -> Self {
            let (temp, library, spotify) = fixture();
            Self {
                temp,
                library,
                spotify,
                replacements: vec![],
                observer: None,
                release: vec![],
                specs: vec![],
                events: vec![],
            }
        }
        async fn send(
            &mut self,
            command: &str,
            value: Option<Value>,
            exact: Option<f64>,
        ) -> Vec<Event> {
            let request = request(&self.library, command, value.clone());
            let events = if let Some(number) = exact {
                self.spotify
                    .dispatch_with_number(&self.library, request, "main", number)
                    .await
            } else {
                self.spotify.dispatch(&self.library, request, "main").await
            }
            .unwrap();
            self.events.push(event(&events, &self.library));
            let mut spec = json!({"command":command});
            if let Some(number) = exact {
                spec["valueBits"] = json!(format!("{:016x}", number.to_bits()));
            } else if let Some(value) = value {
                spec["value"] = value;
            }
            self.specs.push(spec);
            events
        }
        async fn switch(&self) -> RuntimeContext {
            let old = self.library.context().unwrap();
            let next = self.temp.path().join("next");
            fs::create_dir(&next).unwrap();
            let change=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
            self.library.dispatch(change, "main").await.unwrap();
            self.library.context().unwrap()
        }
        fn query(&mut self) {
            let library = self.library.clone();
            let spotify = self.spotify.clone();
            let mut source = request(&library, "seek", Some(json!(3.0000005)));
            let request = Request {
                channel: source.channel.clone(),
                data: source.data.clone(),
                listener_id: source.listener_id.clone(),
                context: source.context.clone(),
            };
            self.observer = Some(tokio::spawn(async move {
                spotify.dispatch(&library, request, "main").await
            }));
            source.data["data"]["command"] = json!("pause");
            source.data["data"]["value"] = json!(99);
            source.context.as_mut().unwrap().library_generation += 1;
        }
        async fn take(&mut self) -> Result<Vec<Event>, WireError> {
            tokio::time::timeout(LIMIT, self.observer.take().unwrap())
                .await
                .unwrap()
                .unwrap()
        }
        async fn cleanup(&mut self) {
            self.spotify.seal();
            for host in &self.replacements {
                host.seal();
            }
            self.release.clear();
            if let Some(task) = self.observer.take() {
                task.abort();
                let _ = task.await;
            }
            let first = tokio::time::timeout(LIMIT, self.spotify.shutdown()).await;
            let mut others = vec![];
            for host in &self.replacements {
                others.push(tokio::time::timeout(LIMIT, host.shutdown()).await);
            }
            let writes = tokio::time::timeout(LIMIT, self.library.finish_writes()).await;
            first.unwrap().unwrap();
            for result in others {
                result.unwrap().unwrap();
            }
            writes.unwrap().unwrap();
            if let Some(s) = self.spotify.status() {
                assert!(s.joined && s.drained);
                assert_eq!(s.spawned, s.reaped);
                assert!(s.command_pids.is_empty());
                assert_eq!(s.retained, 0);
            }
            assert_eq!(self.spotify.test_registered_commands(), 0);
        }
    }
    fn event(events: &[Event], library: &LibraryService) -> Value {
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.channel, "MAIN");
        assert_eq!(event.message.channel, "SPOTIFY_COMMAND");
        assert_eq!(event.message.data, Some(json!(true)));
        assert_eq!(event.listener_id.as_deref(), Some("command-音楽"));
        assert_eq!(event.context.as_ref(), Some(&library.context().unwrap()));
        assert!(event.native_lifetime.is_some());
        assert!(event.native_spotify.is_none());
        let value = serde_json::to_value(event).unwrap();
        assert!(value.get("nativeLifetime").is_none());
        assert!(value.get("nativeSpotify").is_none());
        assert_eq!(
            value["message"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["channel", "data"]
        );
        value
    }
    async fn observed(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(LIMIT, async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    fn peer() -> Vec<Value> {
        fs::read(std::env::var_os("FREESHOW_SPOTIFY_COMMAND_PEER").unwrap())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    fn count(name: &str) -> usize {
        peer().iter().filter(|v| v["name"] == name).count()
    }
    async fn held(rig: &Rig) -> u32 {
        observed(|| {
            count("Get") == 1
                && rig
                    .spotify
                    .status()
                    .is_some_and(|s| s.command_pids.len() == 1)
        })
        .await;
        let s = rig.spotify.status().unwrap();
        assert_eq!(s.retained, 1);
        assert_eq!(rig.spotify.test_registered_commands(), 1);
        assert!(Path::new(&format!("/proc/{}", s.command_pids[0])).exists());
        s.command_pids[0]
    }
    async fn settled(rig: &Rig, commands: u64) {
        observed(|| {
            rig.spotify.status().is_some_and(|s| {
                s.commands_completed == commands
                    && s.spawned == s.reaped
                    && s.command_pids.is_empty()
            })
        })
        .await;
    }
    async fn scenario(rig: &mut Rig, case: &str) {
        match case {
            "concurrent" => {
                let seek = rig.send("seek", Some(json!(3.0000005)), None).await;
                let pid = held(rig).await;
                let pause = rig
                    .send("pause", Some(json!({"ignored":[null,"value"]})), None)
                    .await;
                observed(|| count("Pause") == 1).await;
                drop(pause);
                let volume = rig
                    .send(
                        "setVolume",
                        Some(json!(0)),
                        Some(f64::from_bits(0x3fe0000000000001)),
                    )
                    .await;
                observed(|| count("Set") == 1).await;
                drop(volume);
                observed(|| rig.spotify.status().unwrap().commands_completed == 2).await;
                assert_eq!(
                    peer()
                        .iter()
                        .map(|v| v["name"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["Get", "Pause", "Set"]
                );
                drop(seek);
                assert!(Path::new(&format!("/proc/{pid}")).exists());
                rig.spotify.context_changed(&rig.library.context().unwrap());
                assert_eq!(rig.spotify.test_registered_commands(), 1);
                println!("HOST_RELEASE");
                observed(|| count("SetPosition") == 1).await;
                settled(rig, 3).await;
                assert_eq!(rig.spotify.status().unwrap().retained, 0);
                assert!(!Path::new(&format!("/proc/{pid}")).exists());
            }
            "callerDrop" => {
                let (entered, release) = rig.spotify.test_hold_after_command_begin();
                rig.release.push(release);
                rig.query();
                entered.await.unwrap();
                let pid = held(rig).await;
                let task = rig.observer.take().unwrap();
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                assert!(Path::new(&format!("/proc/{pid}")).exists());
                assert_eq!(rig.spotify.status().unwrap().retained, 1);
                assert_eq!(rig.spotify.test_registered_commands(), 1);
                println!("HOST_RELEASE");
                observed(|| count("SetPosition") == 1).await;
                settled(rig, 1).await;
                assert_eq!(rig.spotify.status().unwrap().retained, 0);
            }
            "stale" => {
                drop(rig.send("seek", Some(json!(3.0000005)), None).await);
                let pid = held(rig).await;
                let process_owner = rig.spotify.status().unwrap().context;
                let current = rig.switch().await;
                rig.spotify.context_changed(&current);
                println!("HOST_RELEASE");
                settled(rig, 1).await;
                assert_eq!(
                    rig.spotify
                        .status()
                        .unwrap()
                        .last_command_error
                        .unwrap()
                        .code,
                    "spotify_cancelled"
                );
                assert!(!Path::new(&format!("/proc/{pid}")).exists());
                assert_eq!(count("SetPosition"), 0);
                rig.spotify.context_changed(&current);
                assert_eq!(rig.spotify.test_registered_commands(), 0);
                drop(rig.send("pause", None, None).await);
                observed(|| count("Pause") == 1).await;
                settled(rig, 2).await;
                let status = rig.spotify.status().unwrap();
                assert_eq!(
                    status.context, process_owner,
                    "request context does not replace process owner identity"
                );
                assert_eq!(
                    status.spawned, 3,
                    "same owner: help, canceled Metadata, new-context Pause"
                );
            }
            "finalContext" => {
                let (entered, release) = rig.spotify.test_hold_before_context();
                rig.release.push(release);
                rig.query();
                entered.await.unwrap();
                let pid = held(rig).await;
                rig.switch().await;
                rig.release.pop().unwrap().send(()).unwrap();
                assert_eq!(rig.take().await.unwrap_err().code, "stale_library");
                println!("HOST_RELEASE");
                settled(rig, 1).await;
                assert_eq!(
                    rig.spotify
                        .status()
                        .unwrap()
                        .last_command_error
                        .unwrap()
                        .code,
                    "spotify_cancelled"
                );
                assert!(!Path::new(&format!("/proc/{pid}")).exists());
                assert_eq!(count("SetPosition"), 0);
            }
            "closeActive" => {
                let events = rig.send("seek", Some(json!(3.0000005)), None).await;
                let pid = held(rig).await;
                let copied = events.clone();
                drop(events);
                rig.spotify.seal();
                rig.spotify.shutdown().await.unwrap();
                let status = rig.spotify.status().unwrap();
                assert!(status.drained);
                assert!(!Path::new(&format!("/proc/{pid}")).exists());
                assert_eq!(count("SetPosition"), 0);
                assert_eq!(rig.spotify.test_registered_commands(), 0);
                assert_eq!(
                    status.retained, 1,
                    "joined IO does not dispose the cloned early event"
                );
                drop(copied);
                assert_eq!(rig.spotify.status().unwrap().retained, 0);
            }
            "quotaClose" => {
                let mut events = vec![];
                for _ in 0..4 {
                    events.push(rig.send("pause", None, None).await);
                }
                observed(|| count("Pause") == 4).await;
                settled(rig, 4).await;
                assert_eq!(rig.spotify.status().unwrap().retained, 4);
                let copied = events[0].clone();
                let bytes = serde_json::to_vec(&copied).unwrap();
                let decoded: Vec<Event> = serde_json::from_slice(&bytes).unwrap();
                assert!(decoded[0].native_lifetime.is_none());
                events.remove(0);
                assert_eq!(rig.spotify.status().unwrap().retained, 4);
                assert_eq!(
                    rig.spotify
                        .dispatch(&rig.library, request(&rig.library, "pause", None), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_busy"
                );
                rig.spotify.shutdown().await.unwrap();
                assert!(rig.spotify.status().unwrap().drained);
                assert_eq!(rig.spotify.status().unwrap().retained, 4);
                let next = NativeSpotify::new(&rig.library.context().unwrap());
                rig.replacements.push(next.clone());
                assert_eq!(
                    next.dispatch(&rig.library, request(&rig.library, "pause", None), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_busy"
                );
                drop(copied);
                let refill = next
                    .dispatch(&rig.library, request(&rig.library, "pause", None), "main")
                    .await
                    .unwrap();
                event(&refill, &rig.library);
                next.shutdown().await.unwrap();
                assert!(next.status().unwrap().drained);
                assert_eq!(next.status().unwrap().retained, 4);
                drop((refill, events));
                assert_eq!(next.status().unwrap().retained, 0);
            }
            "saturation" => {
                let mut events = vec![];
                for _ in 0..4 {
                    events.push(rig.send("seek", Some(json!(1)), None).await);
                }
                observed(|| {
                    count("Get") == 4 && rig.spotify.status().unwrap().command_pids.len() == 4
                })
                .await;
                assert_eq!(rig.spotify.test_registered_commands(), 4);
                let pids = rig.spotify.status().unwrap().command_pids;
                drop(events);
                assert_eq!(
                    rig.spotify.status().unwrap().retained,
                    4,
                    "dropped early events do not cancel or release actual held work"
                );
                assert_eq!(
                    rig.spotify
                        .dispatch(&rig.library, request(&rig.library, "pause", None), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_busy"
                );
                let mut state = request(&rig.library, "pause", None);
                state.data = json!({"channel":"SPOTIFY_GET_STATE"});
                assert_eq!(
                    rig.spotify
                        .dispatch(&rig.library, state, "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_busy"
                );
                let current = rig.switch().await;
                rig.spotify.context_changed(&current);
                println!("HOST_RELEASE");
                settled(rig, 4).await;
                let status = rig.spotify.status().unwrap();
                assert_eq!(status.command_failures, 4);
                assert_eq!(status.last_command_error.unwrap().code, "spotify_cancelled");
                assert_eq!(status.retained, 0);
                assert_eq!(count("SetPosition"), 0);
                for pid in pids {
                    assert!(!Path::new(&format!("/proc/{pid}")).exists());
                }
            }
            "numeric" => {
                let mut completed = 0;
                for (command, value, method) in [
                    ("playpause", Value::Null, "PlayPause"),
                    ("next", json!([]), "Next"),
                    ("prev", json!({"ignored":true}), "Previous"),
                    ("pause", json!(false), "Pause"),
                ] {
                    let events = rig.send(command, Some(value), None).await;
                    observed(|| count(method) == 1).await;
                    completed += 1;
                    settled(rig, completed).await;
                    drop(events);
                }
                for number in [-0.0, 5e-324, 1e-7, 1e21, f64::from_bits(0x3fe0000000000001)] {
                    let before = count("Set");
                    let failures = rig.spotify.status().map_or(0, |s| s.command_failures);
                    let events = rig.send("setVolume", Some(json!(0)), Some(number)).await;
                    completed += 1;
                    settled(rig, completed).await;
                    if rig.spotify.status().unwrap().command_failures == failures {
                        observed(|| count("Set") == before + 1).await;
                    } else {
                        assert_eq!(
                            count("Set"),
                            before,
                            "failed tool process must not be invented as a peer effect"
                        );
                    }
                    drop(events);
                }
                for number in [-0.0000005, 0.0000005, -0.0000015, 0.0000015] {
                    let before = count("SetPosition");
                    let events = rig.send("seek", Some(json!(0)), Some(number)).await;
                    observed(|| count("SetPosition") == before + 1).await;
                    completed += 1;
                    settled(rig, completed).await;
                    drop(events);
                }
                settled(rig, 13).await;
            }
            _ => panic!("unknown command host case"),
        }
    }
    pub async fn run(case: &str) {
        assert_eq!(
            std::env::var("FREESHOW_SPOTIFY_COMMAND_HOST_CHILD").unwrap(),
            "private-v1"
        );
        let mut rig = Rig::new();
        let result = AssertUnwindSafe(scenario(&mut rig, case))
            .catch_unwind()
            .await;
        rig.cleanup().await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        let receipt = json!({"case":case,"events":rig.events,"specs":rig.specs,"status":rig.spotify.status(),"registeredCommands":rig.spotify.test_registered_commands()});
        let bytes = serde_json::to_vec(&receipt).unwrap();
        assert!(bytes.len() <= 65536);
        fs::write(
            std::env::var_os("FREESHOW_SPOTIFY_COMMAND_HOST_RECEIPT").unwrap(),
            bytes,
        )
        .unwrap();
    }
    pub const SUPERVISOR: &str = r#"
import assert from 'node:assert/strict';import fs from 'node:fs';import os from 'node:os';import path from 'node:path';import{spawn}from'node:child_process';import{pathToFileURL}from'node:url';
const[exe,root]=process.argv.slice(1);assert.equal(process.versions.node,'22.21.1');
const{commandBus}=await import(pathToFileURL(path.join(root,'tests/parity/spotify-command-oracle.mjs')));
const{Lines,cleanup}=await import(pathToFileURL(path.join(root,'tests/parity/spotify-oracle.mjs')));
const cases=process.env.FREESHOW_SPOTIFY_COMMAND_ONLY?[process.env.FREESHOW_SPOTIFY_COMMAND_ONLY]:['concurrent','callerDrop','stale','finalContext','closeActive','quotaClose','saturation','numeric'];const evidence=[];
for(const name of cases){
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'freeshow-spotify-command-host-'));const receipt=path.join(dir,'receipt.json'),witness=path.join(dir,'peer.json');let bus,child,timer,release;
 try{
  bus=await commandBus(name==='numeric'||name==='quotaClose'?'ordinary':'hold');
  bus.peer.child.stdout.on('data',()=>fs.writeFileSync(witness,JSON.stringify(bus.events())));
  child=new Lines(spawn(exe,['--exact','command_validation_precedes_lazy_admission','--nocapture','--test-threads=1'],{env:{...bus.env,FREESHOW_SPOTIFY_COMMAND_HOST_CASE:name,FREESHOW_SPOTIFY_COMMAND_HOST_CHILD:'private-v1',FREESHOW_SPOTIFY_COMMAND_HOST_RECEIPT:receipt,FREESHOW_SPOTIFY_COMMAND_PEER:witness},stdio:['ignore','pipe','pipe']}));
  if(['concurrent','callerDrop','stale','finalContext','saturation'].includes(name)){release=child.wait(line=>line.endsWith('HOST_RELEASE')).then(()=>bus.peer.child.stdin.write('release\n'));release.catch(()=>{});}
  const exit=await Promise.race([child.exit,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('actual native command host timeout '+name)),20000)})]);
  assert.equal(exit.code,0,name+'\n'+child.lines.join('\n')+'\n'+child.stderr);if(release)await release;
  const actual=JSON.parse(fs.readFileSync(receipt,'utf8'));assert.equal(actual.case,name);assert.equal(actual.status.joined,true);assert.equal(actual.status.drained,true);assert.equal(actual.status.retained,0);assert.equal(actual.registeredCommands,0);assert.equal(actual.status.spawned,actual.status.reaped);assert.deepEqual(actual.status.commandPids,[]);
  const nativeEffects=bus.events();if(['finalContext','closeActive'].includes(name))assert.deepEqual(nativeEffects.map(e=>e.name),['Get']);if(name==='stale')assert.deepEqual(nativeEffects.map(e=>e.name),['Get','Pause']);if(name==='saturation')assert.deepEqual(nativeEffects.map(e=>e.name),['Get','Get','Get','Get']);if(name==='callerDrop')assert.deepEqual(nativeEffects.at(-1),{name:'SetPosition',args:['/fixture/track_01',Math.round(3.0000005*1e6)]});
  for(const e of actual.events){assert.deepEqual(e.message,{channel:'SPOTIFY_COMMAND',data:true});assert.equal(e.listenerId,'command-音楽');assert.equal(e.channel,'MAIN');assert.ok(e.context.runtimeId&&Number.isInteger(e.context.libraryGeneration)&&e.context.dataPath);assert.equal(Object.hasOwn(e,'nativeLifetime'),false);assert.equal(Object.hasOwn(e,'nativeSpotify'),false);}
  await bus.close();bus=undefined;
  if(['concurrent','numeric'].includes(name)){
   bus=await commandBus(name==='concurrent'?'hold':'ordinary');
   for(const spec of actual.specs){const value=spec.valueBits?Buffer.from(spec.valueBits,'hex').readDoubleBE():spec.value;const before=bus.events().length,errors=bus.original.calls.errors;const payload={command:spec.command,value};const sent=bus.original.send(payload,'command-音楽');payload.command='unknown';payload.value=99;const result=await sent;assert.equal(result,undefined,'actual receiveMain has no return ACK');assert.deepEqual(bus.original.calls.replies.at(-1),['MAIN',{channel:'SPOTIFY_COMMAND',data:true},'command-音楽']);
    if(name==='numeric'){await bus.settle();if(bus.original.calls.errors===errors)await bus.peer.wait(()=>bus.events().length>=before+(spec.command==='seek'?2:1));else assert.equal(bus.events().length,before);}
    else {await bus.peer.wait(()=>bus.events().length>before);}
   }
   if(name==='concurrent'){assert.deepEqual(bus.events().map(e=>e.name),['Get','Pause','Set']);bus.peer.child.stdin.write('release\n');await bus.peer.wait(()=>bus.events().some(e=>e.name==='SetPosition'));await bus.settle();}
   assert.deepEqual(nativeEffects,bus.events(),name+' actual peer effects must match actual immutable source');
   assert.equal(actual.status.commandFailures,bus.original.calls.errors,'source tool failures remain native diagnostics, not peer effects');
  }
  evidence.push({case:name,nativePeerEffects:nativeEffects.length,sourceCompared:['concurrent','numeric'].includes(name),spawned:actual.status.spawned,reaped:actual.status.reaped});
 }finally{clearTimeout(timer);await cleanup(child?child.stop():Promise.resolve(),bus?bus.close():Promise.resolve());fs.rmSync(dir,{recursive:true,force:true});}
}
console.log(JSON.stringify({cases:evidence,ownedFixtureProcessesRemaining:0}));
"#;
}
