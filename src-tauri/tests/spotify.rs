#[path = "../src/spotify.rs"]
mod spotify;
use freeshow_core::{
    protocol::{Event, NativeLifetime, Request},
    runtime::Runtime,
    service::LibraryService,
    WireError,
};
use serde_json::json;
#[cfg(target_os = "linux")]
use serde_json::Value;
use spotify::NativeSpotify;
use std::time::Duration;
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(10);
fn fixture() -> (tempfile::TempDir, LibraryService, NativeSpotify) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let spotify = NativeSpotify::new(&library.context().unwrap());
    (temp, library, spotify)
}
fn request(library: &LibraryService) -> Request {
    serde_json::from_value(json!({
        "channel":"MAIN",
        "data":{"channel":"SPOTIFY_GET_STATE","data":{"path":"ignored renderer executable","bus":"ignored renderer address"}},
        "context":library.context().unwrap(),"listenerId":"spotify-敬拜"
    })).unwrap()
}
async fn drain(library: &LibraryService, spotify: &NativeSpotify) {
    let (native, writes) = tokio::join!(
        timeout(DEADLINE, spotify.shutdown()),
        timeout(DEADLINE, library.finish_writes())
    );
    native.unwrap().unwrap();
    writes.unwrap().unwrap();
}

#[tokio::test]
async fn main_role_full_context_and_listener_validate_before_lazy_start() {
    #[cfg(target_os = "linux")]
    if let Ok(case) = std::env::var("FREESHOW_SPOTIFY_HOST_CASE") {
        linux::run_child(&case).await;
        return;
    }
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Event>();
    send_sync::<NativeLifetime>();
    send_sync::<freeshow_spotify::Receipt>();
    let (_temp, library, spotify) = fixture();
    assert!(spotify.status().is_none());
    spotify.context_changed(&library.context().unwrap());
    assert!(
        spotify.status().is_none(),
        "Context notification is lazy too"
    );
    for window in ["output-1", "stage", "main-other"] {
        assert_eq!(
            spotify
                .dispatch(&library, request(&library), window)
                .await
                .unwrap_err()
                .code,
            "spotify_role"
        );
    }
    for component in ["missing", "runtime", "generation", "path"] {
        let mut invalid = request(&library);
        match component {
            "missing" => invalid.context = None,
            "runtime" => invalid.context.as_mut().unwrap().runtime_id.push('x'),
            "generation" => invalid.context.as_mut().unwrap().library_generation += 1,
            "path" => invalid.context.as_mut().unwrap().data_path.push("foreign"),
            _ => unreachable!(),
        }
        assert_eq!(
            spotify
                .dispatch(&library, invalid, "main")
                .await
                .unwrap_err()
                .code,
            "stale_library",
            "{component}"
        );
    }
    for listener in ["x".repeat(8193), "界".repeat(2731)] {
        let mut invalid = request(&library);
        invalid.listener_id = Some(listener);
        assert_eq!(
            spotify
                .dispatch(&library, invalid, "main")
                .await
                .unwrap_err()
                .code,
            "spotify_resource_limit"
        );
    }
    for (outer, inner) in [("OUTPUT", "SPOTIFY_GET_STATE"), ("MAIN", "SPOTIFY_COMMAND")] {
        let mut invalid = request(&library);
        invalid.channel = outer.into();
        invalid.data["channel"] = json!(inner);
        assert_eq!(
            spotify
                .dispatch(&library, invalid, "main")
                .await
                .unwrap_err()
                .code,
            "unsupported_command"
        );
    }
    assert!(
        spotify.status().is_none(),
        "No rejected request may start/probe a backend"
    );
    assert_eq!(spotify.test_registered(), 0);
    spotify.seal();
    assert_eq!(
        spotify
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "spotify_shutdown"
    );
    drain(&library, &spotify).await;
    assert!(spotify.status().is_none());
}

#[test]
fn missing_async_runtime_rejects_before_native_start() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let (_temp, library, spotify) = fixture();
    let mut context = Context::from_waker(Waker::noop());
    let request = request(&library);
    assert!(matches!(
        std::pin::pin!(spotify.dispatch(&library, request, "main")).as_mut().poll(&mut context),
        Poll::Ready(Err(WireError { code, .. })) if code == "spotify_runtime"
    ));
    assert!(spotify.status().is_none());
    assert_eq!(spotify.test_registered(), 0);
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(drain(&library, &spotify));
}

#[cfg(target_os = "linux")]
#[test]
fn actual_private_bus_native_host_and_immutable_source_scenarios() {
    use std::{
        io::Read,
        process::{Command, Stdio},
        thread,
        time::Instant,
    };
    let path = std::path::Path::new(file!());
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    };
    let root = absolute.ancestors().nth(3).unwrap();
    let node = std::env::var_os("FREESHOW_SPOTIFY_HOST_NODE").unwrap_or_else(|| "node".into());
    let mut child = Command::new(node)
        .args(["--input-type=module", "-e", linux::SUPERVISOR])
        .arg(std::env::current_exe().unwrap())
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Node22/private D-Bus prerequisites are required on Linux");
    fn capture(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = vec![];
            // Test supervisor output is bounded too; no unbounded wait_with_output.
            pipe.by_ref()
                .take(128 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        })
    }
    let stdout = capture(child.stdout.take().unwrap());
    let stderr = capture(child.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let waited = child.wait().unwrap();
            let out = stdout.join().unwrap();
            let err = stderr.join().unwrap();
            panic!(
                "private supervisor timeout (actual wait {waited}): {} {}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            );
        }
        thread::sleep(Duration::from_millis(5));
    };
    let out = stdout.join().unwrap();
    let err = stderr.join().unwrap();
    assert!(out.len() <= 128 * 1024 && err.len() <= 128 * 1024);
    assert!(
        status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let evidence: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(evidence["cases"].as_array().unwrap().len(), 7);
    assert_eq!(evidence["sourceComparisons"], 7);
    assert_eq!(evidence["ownedFixtureProcessesRemaining"], 0);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use freeshow_core::protocol::RuntimeContext;
    use futures_util::FutureExt;
    use std::{fs, panic::AssertUnwindSafe, path::Path, sync::mpsc};
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    type Query = JoinHandle<Result<Vec<Event>, WireError>>;
    type Commit = JoinHandle<freeshow_core::Result<Vec<Event>>>;
    struct Rig {
        temp: tempfile::TempDir,
        library: LibraryService,
        spotify: NativeSpotify,
        replacements: Vec<NativeSpotify>,
        observer: Option<Query>,
        commit: Option<Commit>,
        release_commit: Option<mpsc::SyncSender<()>>,
        release: Vec<tokio::sync::oneshot::Sender<()>>,
        initial: RuntimeContext,
    }
    impl Rig {
        fn new() -> Self {
            let (temp, library, spotify) = fixture();
            let initial = library.context().unwrap();
            Self {
                temp,
                library,
                spotify,
                replacements: vec![],
                observer: None,
                commit: None,
                release_commit: None,
                release: vec![],
                initial,
            }
        }
        fn query(&mut self) {
            assert!(self.observer.is_none());
            let library = self.library.clone();
            let spotify = self.spotify.clone();
            let request = request(&library);
            self.observer = Some(tokio::spawn(async move {
                spotify.dispatch(&library, request, "main").await
            }));
        }
        async fn take_observer(&mut self) -> Result<Vec<Event>, WireError> {
            joined(self.observer.take().unwrap()).await.unwrap()
        }
        async fn switch_library(&self) -> RuntimeContext {
            let previous = self.library.context().unwrap();
            let next = self.temp.path().join("next-library");
            fs::create_dir(&next).unwrap();
            let change = serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":previous.data_path,"newPath":next}},"context":previous})).unwrap();
            timeout(DEADLINE, self.library.dispatch(change, "main"))
                .await
                .unwrap()
                .unwrap();
            let current = self.library.context().unwrap();
            assert_eq!(current.runtime_id, previous.runtime_id);
            assert_eq!(current.library_generation, previous.library_generation + 1);
            assert_ne!(current.data_path, previous.data_path);
            current
        }
        async fn cleanup(&mut self) {
            self.spotify.seal();
            for replacement in &self.replacements {
                replacement.seal();
            }
            self.release.clear();
            self.release_commit.take();
            if let Some(observer) = self.observer.take() {
                observer.abort();
                let _ = joined(observer).await;
            }
            let committed = if let Some(commit) = self.commit.take() {
                Some(joined(commit).await)
            } else {
                None
            };
            // Attempt every independent owner before returning any cleanup error.
            let mut native = vec![timeout(DEADLINE, self.spotify.shutdown()).await];
            for replacement in &self.replacements {
                native.push(timeout(DEADLINE, replacement.shutdown()).await);
            }
            let writes = timeout(DEADLINE, self.library.finish_writes()).await;
            for result in native {
                result.unwrap().unwrap();
            }
            writes.unwrap().unwrap();
            if let Some(result) = committed {
                result.unwrap().unwrap();
            }
            if let Some(status) = self.spotify.status() {
                assert!(status.drained && status.joined);
                assert_eq!(status.active_pid, 0);
                assert_eq!(status.spawned, status.reaped);
                assert_eq!(status.retained, 0);
            }
            assert_eq!(self.spotify.test_registered(), 0);
        }
    }
    async fn joined<T>(mut task: JoinHandle<T>) -> Result<T, tokio::task::JoinError> {
        match timeout(DEADLINE, &mut task).await {
            Ok(result) => result,
            Err(error) => {
                task.abort();
                let _ = task.await;
                panic!("bounded test task observer timed out; native owner still requires drain: {error}")
            }
        }
    }
    async fn observed(mut predicate: impl FnMut() -> bool) {
        timeout(DEADLINE, async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    fn event(events: &[Event], library: &LibraryService, listener: Option<&str>) -> Value {
        assert_eq!(
            events.len(),
            1,
            "Only the original source reply, no success ACK"
        );
        let event = &events[0];
        assert_eq!(event.channel, "MAIN");
        assert_eq!(event.message.channel, "SPOTIFY_GET_STATE");
        assert_eq!(event.listener_id.as_deref(), listener);
        assert_eq!(event.context.as_ref(), Some(&library.context().unwrap()));
        assert!(event.native_lifetime.is_some());
        assert_eq!(
            serde_json::to_value(&event.message)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["channel", "data"]
        );
        let encoded = serde_json::to_value(event).unwrap();
        assert!(encoded.get("nativeLifetime").is_none());
        assert_eq!(
            encoded
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            if listener.is_some() {
                vec![
                    "channel",
                    "message",
                    "listenerId",
                    "context",
                    "nativeSpotify",
                ]
            } else {
                vec!["channel", "message", "context", "nativeSpotify"]
            }
        );
        encoded
    }
    async fn normal(rig: &Rig) -> Vec<Event> {
        timeout(
            DEADLINE,
            rig.spotify
                .dispatch(&rig.library, request(&rig.library), "main"),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn held_position(rig: &Rig) -> u32 {
        observed(|| {
            rig.spotify
                .status()
                .is_some_and(|s| s.spawned == 4 && s.reaped == 3 && s.active_pid != 0)
        })
        .await;
        let witness = std::env::var_os("FREESHOW_SPOTIFY_HOST_POSITION_WITNESS").unwrap();
        observed(|| fs::read(&witness).is_ok_and(|bytes| bytes == b"GET Position\n")).await;
        let status = rig.spotify.status().unwrap();
        assert_eq!(status.retained, 1);
        assert_eq!(status.completed, 0);
        assert!(Path::new(&format!("/proc/{}", status.active_pid)).exists());
        status.active_pid
    }
    async fn cleaned_query(rig: &Rig, pid: u32) {
        observed(|| {
            rig.spotify.status().is_some_and(|s| {
                s.completed == 1 && s.retained == 0 && s.active_pid == 0 && s.spawned == s.reaped
            })
        })
        .await;
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "actual native client was waited"
        );
        assert_eq!(rig.spotify.test_registered(), 0);
    }
    async fn accepted(rig: &mut Rig) -> CancellationToken {
        let (entered, release) = rig.spotify.test_hold_after_begin();
        rig.release.push(release);
        rig.query();
        timeout(DEADLINE, entered).await.unwrap().unwrap()
    }
    async fn scenario(rig: &mut Rig, case: &str) -> Value {
        match case {
            "lifetime" => {
                let mut held = vec![];
                for _ in 0..4 {
                    held.push(normal(rig).await);
                }
                assert_eq!(rig.spotify.status().unwrap().retained, 4);
                let copied = held[0].clone();
                let bytes = serde_json::to_vec(&copied).unwrap();
                let reconstructed: Vec<Event> = serde_json::from_slice(&bytes).unwrap();
                assert!(
                    reconstructed[0].native_lifetime.is_none(),
                    "JSON cannot recreate native authority"
                );
                let source_event = event(&copied, &rig.library, Some("spotify-敬拜"));
                held.remove(0);
                assert_eq!(rig.spotify.status().unwrap().retained, 4);
                assert_eq!(
                    rig.spotify
                        .dispatch(&rig.library, request(&rig.library), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_busy"
                );
                rig.spotify.seal();
                assert_eq!(
                    rig.spotify
                        .dispatch(&rig.library, request(&rig.library), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_shutdown"
                );
                rig.spotify.shutdown().await.unwrap();
                assert!(rig.spotify.status().unwrap().drained);
                assert_eq!(
                    rig.spotify.status().unwrap().retained,
                    4,
                    "actual IO join does not dispose old publications"
                );
                let next = NativeSpotify::new(&rig.library.context().unwrap());
                rig.replacements.push(next.clone());
                assert_eq!(
                    next.dispatch(&rig.library, request(&rig.library), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_busy"
                );
                drop(copied);
                assert_eq!(next.status().unwrap().retained, 3);
                let refill = next
                    .dispatch(&rig.library, request(&rig.library), "main")
                    .await
                    .unwrap();
                event(&refill, &rig.library, Some("spotify-敬拜"));
                assert_eq!(next.status().unwrap().retained, 4);
                next.shutdown().await.unwrap();
                assert!(next.status().unwrap().drained);
                assert_eq!(next.status().unwrap().retained, 4);
                drop(held);
                drop(refill);
                assert_eq!(
                    next.status().unwrap().retained,
                    0,
                    "encoded/untrusted copies do not retain or replace a real guard"
                );
                assert!(!bytes.is_empty() && !reconstructed.is_empty());
                rig.spotify.shutdown().await.unwrap();
                next.shutdown().await.unwrap();
                source_event
            }
            "callerDrop" => {
                let caller = accepted(rig).await;
                let pid = held_position(rig).await;
                rig.observer.as_ref().unwrap().abort();
                assert!(joined(rig.observer.take().unwrap())
                    .await
                    .unwrap_err()
                    .is_cancelled());
                assert!(
                    caller.is_cancelled(),
                    "actual Caller Drop cancels the registered parent"
                );
                cleaned_query(rig, pid).await;
                let events = normal(rig).await;
                let source_event = event(&events, &rig.library, Some("spotify-敬拜"));
                let status = rig.spotify.status().unwrap();
                assert_eq!(status.context.runtime_id, rig.initial.runtime_id);
                assert_eq!(
                    status.spawned, 8,
                    "same owner: help +3 canceled reads +4 new reads"
                );
                assert_eq!(status.queries, 2);
                drop(events);
                source_event
            }
            "contextChanged" => {
                let caller = accepted(rig).await;
                let pid = held_position(rig).await;
                rig.spotify.context_changed(&rig.initial);
                assert!(!caller.is_cancelled());
                let current = rig.switch_library().await;
                rig.spotify.context_changed(&current);
                assert!(caller.is_cancelled());
                assert_eq!(rig.spotify.test_registered(), 1);
                assert_eq!(
                    rig.spotify.status().unwrap().retained,
                    1,
                    "held accepted Ticket remains charged after context cancellation"
                );
                rig.release.pop().unwrap().send(()).unwrap();
                assert_eq!(
                    rig.take_observer().await.unwrap_err().code,
                    "spotify_cancelled"
                );
                cleaned_query(rig, pid).await;
                let events = normal(rig).await;
                let source_event = event(&events, &rig.library, Some("spotify-敬拜"));
                let status = rig.spotify.status().unwrap();
                assert_eq!(status.context.runtime_id, rig.initial.runtime_id);
                assert_eq!(
                    status.context.library_generation,
                    rig.initial.library_generation
                );
                assert_eq!(status.context.data_path, rig.initial.data_path);
                assert_eq!(
                    status.spawned, 8,
                    "library switch never starts another help/owner"
                );
                assert_eq!((status.queries, status.completed), (2, 2));
                drop(events);
                source_event
            }
            "finalContextFence" => {
                let (entered, release) = rig.spotify.test_hold_before_context();
                rig.release.push(release);
                rig.query();
                let caller = timeout(DEADLINE, entered).await.unwrap().unwrap();
                assert_eq!(rig.spotify.status().unwrap().retained, 1);
                assert_eq!(rig.spotify.status().unwrap().completed, 1);
                let current = rig.switch_library().await;
                // Deliberately omit the notification: the actual final library
                // snapshot must independently reject the already-built old event.
                assert!(!caller.is_cancelled());
                rig.release.pop().unwrap().send(()).unwrap();
                assert_eq!(rig.take_observer().await.unwrap_err().code, "stale_library");
                assert_eq!(rig.spotify.status().unwrap().retained, 0);
                rig.spotify.context_changed(&current);
                let events = normal(rig).await;
                let source_event = event(&events, &rig.library, Some("spotify-敬拜"));
                assert_eq!(
                    rig.spotify.status().unwrap().spawned,
                    9,
                    "one help and two complete queries"
                );
                drop(events);
                source_event
            }
            "heldCommit" => {
                let (entered, owner) = tokio::sync::oneshot::channel();
                let (release, held) = mpsc::sync_channel::<()>(1);
                rig.release_commit = Some(release);
                let library = rig.library.clone();
                let context = library.context().unwrap();
                let request = serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"SPOTIFY_TEST_HELD_COMMIT"},"context":context})).unwrap();
                rig.commit = Some(tokio::spawn(async move {
                    library
                        .commit(request, "main", move |_| {
                            let _ = entered
                                .send(std::thread::current().name().unwrap_or("").to_string());
                            let _ = held.recv();
                            Ok(vec![])
                        })
                        .await
                }));
                assert_eq!(
                    timeout(DEADLINE, owner).await.unwrap().unwrap(),
                    "freeshow-library"
                );
                let events = normal(rig).await;
                let source_event = event(&events, &rig.library, Some("spotify-敬拜"));
                assert!(!rig.commit.as_ref().unwrap().is_finished());
                assert_eq!(
                    rig.library.status().unwrap().active_job.unwrap().command,
                    "SPOTIFY_TEST_HELD_COMMIT"
                );
                assert_eq!(rig.spotify.status().unwrap().retained, 1);
                drop(events);
                source_event
            }
            "sealActive" => {
                let caller = accepted(rig).await;
                let pid = held_position(rig).await;
                rig.spotify.seal();
                assert!(caller.is_cancelled());
                assert_eq!(
                    rig.spotify
                        .dispatch(&rig.library, request(&rig.library), "main")
                        .await
                        .unwrap_err()
                        .code,
                    "spotify_shutdown"
                );
                rig.release.pop().unwrap().send(()).unwrap();
                assert_eq!(
                    rig.take_observer().await.unwrap_err().code,
                    "spotify_cancelled"
                );
                rig.spotify.shutdown().await.unwrap();
                rig.spotify.shutdown().await.unwrap();
                cleaned_query(rig, pid).await;
                assert!(rig.spotify.status().unwrap().drained);
                let next = NativeSpotify::new(&rig.library.context().unwrap());
                rig.replacements.push(next.clone());
                let events = next
                    .dispatch(&rig.library, request(&rig.library), "main")
                    .await
                    .unwrap();
                let source_event = event(&events, &rig.library, Some("spotify-敬拜"));
                drop(events);
                source_event
            }
            "missingService" => {
                let mut request = request(&rig.library);
                request.listener_id = None;
                let events = rig
                    .spotify
                    .dispatch(&rig.library, request, "main")
                    .await
                    .unwrap();
                let source_event = event(&events, &rig.library, None);
                assert_eq!(events[0].message.data, Some(Value::Null));
                let status = rig.spotify.status().unwrap();
                assert_eq!(
                    (
                        status.spawned,
                        status.reaped,
                        status.queries,
                        status.completed
                    ),
                    (2, 2, 1, 1)
                );
                drop(events);
                source_event
            }
            _ => panic!("Unknown private native scenario"),
        }
    }
    pub async fn run_child(case: &str) {
        assert_eq!(
            std::env::var("FREESHOW_SPOTIFY_HOST_CHILD").unwrap(),
            "private-v1"
        );
        let mut rig = Rig::new();
        let result = AssertUnwindSafe(scenario(&mut rig, case))
            .catch_unwind()
            .await;
        rig.cleanup().await;
        let source_event = match result {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        let bytes = serde_json::to_vec(
            &json!({"case":case,"sourceEvent":source_event,"status":rig.spotify.status()}),
        )
        .unwrap();
        assert!(bytes.len() <= 65536);
        fs::write(
            std::env::var_os("FREESHOW_SPOTIFY_HOST_RECEIPT").unwrap(),
            bytes,
        )
        .unwrap();
    }

    pub const SUPERVISOR: &str = r#"
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawn,execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [exe,root]=process.argv.slice(1);
assert.equal(process.versions.node,'22.21.1');
const {privateBus,Lines,cleanup,restore}=await import(pathToFileURL(path.join(root,'tests/parity/spotify-oracle.mjs')));
const ts=(await import(pathToFileURL(path.join(root,'node_modules/typescript/lib/typescript.js')))).default;
const original=execFileSync('git',['show','2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/IPC/responsesMain.ts'],{cwd:root,encoding:'utf8',timeout:5000,maxBuffer:1048576}).replaceAll('\r\n','\n');
const ast=ts.createSourceFile('responsesMain.ts',original,ts.ScriptTarget.Latest,true);
let handler; function visit(n){if(ts.isPropertyAssignment(n)&&n.name.getText(ast)==='[Main.SPOTIFY_GET_STATE]')handler=n.getText(ast);ts.forEachChild(n,visit)}visit(ast);
assert.equal(createHash('sha256').update(handler).digest('hex'),'8d655e635c64c162bbf128b08089ec4f48a0f9a13af8979c4874e5f5f6069944');
const cases=[['lifetime','missingArt'],['callerDrop','holdPosition'],['contextChanged','holdPosition'],['finalContextFence','ordinary'],['heldCommit','ordinary'],['sealActive','holdPosition'],['missingService',undefined]];
const evidence=[];
for(const [name,profile] of cases){
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'freeshow-spotify-host-'));
 const receipt=path.join(dir,'receipt.json');
 let bus,child,timer,witness;
 try {
  bus=await privateBus(profile);
  const entered=path.join(dir,'position-entered');
  if(profile==='holdPosition'){
   witness=bus.provider.wait(line=>line==='GET Position').then(()=>fs.writeFileSync(entered,'GET Position\n'));
   witness.catch(()=>{}); // Keep the original observation for actual cleanup below.
  }
  child=new Lines(spawn(exe,['--exact','main_role_full_context_and_listener_validate_before_lazy_start','--nocapture','--test-threads=1'],{env:{...bus.env,FREESHOW_SPOTIFY_HOST_CHILD:'private-v1',FREESHOW_SPOTIFY_HOST_CASE:name,FREESHOW_SPOTIFY_HOST_RECEIPT:receipt,FREESHOW_SPOTIFY_HOST_POSITION_WITNESS:entered},stdio:['ignore','pipe','pipe']}));
  const exit=await Promise.race([child.exit,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Private native scenario timeout: '+name)),15000)})]);
  assert.equal(exit.code,0,name+'\n'+child.lines.join('\n')+'\n'+child.stderr);
  assert.ok(fs.statSync(receipt).size<=65536);
  const actual=JSON.parse(fs.readFileSync(receipt,'utf8'));
  assert.equal(actual.case,name);
  const Main={SPOTIFY_GET_STATE:'SPOTIFY_GET_STATE'};
  const source=await new Function('Main','getSpotifyState','return ({'+handler+'})')(Main,bus.original.get)[Main.SPOTIFY_GET_STATE]();
  const event=actual.sourceEvent;
  const restored=restore({data:event.message.data,...event.nativeSpotify});
  if(source===null)assert.equal(restored,null);
  else {
   assert.deepEqual(Object.keys(restored),Object.keys(source));
   for(const key of Object.keys(source))assert.ok(Object.is(restored[key],source[key]),name+':'+key);
   assert.equal(Object.hasOwn(restored,'albumArt'),true);
  }
  assert.equal(actual.status.joined,true);assert.equal(actual.status.drained,true);
  assert.equal(actual.status.retained,0);assert.equal(actual.status.activePid,0);
  assert.equal(actual.status.spawned,actual.status.reaped);
  evidence.push({case:name,sourceKeys:source===null?[]:Object.keys(source),undefinedFields:event.nativeSpotify.undefinedFields,spawned:actual.status.spawned,reaped:actual.status.reaped});
 } finally {
  clearTimeout(timer);
  await cleanup(child?child.stop():Promise.resolve(),bus?bus.close():Promise.resolve(),witness??Promise.resolve());
  fs.rmSync(dir,{recursive:true,force:true});
 }
}
console.log(JSON.stringify({cases:evidence,sourceComparisons:evidence.length,ownedFixtureProcessesRemaining:0}));
"#;
}
