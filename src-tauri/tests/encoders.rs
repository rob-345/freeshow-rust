#[path = "../src/encoder_inputs.rs"]
mod encoder_inputs;

use encoder_inputs::EncoderInputs;
use freeshow_encoders::{EncoderConfig, EncoderService};
use serde_json::Value;
use std::{fs, io::Write, path::Path};

fn executable_copy(root: &Path) -> std::path::PathBuf {
    let target = root.join(if cfg!(windows) {
        "controlled-encoder.exe"
    } else {
        "controlled-encoder"
    });
    fs::copy(std::env::current_exe().unwrap(), &target).unwrap();
    target
}

#[tokio::test]
async fn trusted_input_preserves_literal_key_metadata_cache_bytes_and_missing_tool_shape() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    fs::create_dir(&root).unwrap();
    let binary = executable_copy(temp.path());
    let source = binary.to_str().unwrap().to_string();
    let cache = b"{\"unknown\":true,\"detection\":{\"future\":\"Japan \\u65e5\"}}";
    fs::write(root.join("encoder-detection.json"), cache).unwrap();
    let inputs = EncoderInputs::new(root.clone()).unwrap();
    let prepared = inputs.freeze(Some(source.clone()), 7, false).await.unwrap();
    assert_eq!(prepared.input().generation, 7);
    assert_eq!(prepared.input().cache.as_deref(), Some(cache.as_slice()));
    let executable = prepared.input().executable.as_ref().unwrap();
    assert_eq!(executable.path, fs::canonicalize(&binary).unwrap());
    assert_eq!(executable.source_path, source);
    assert!(executable.mtime_ms.is_finite() && executable.mtime_ms > 0.0);
    let source_stat=std::process::Command::new(std::env::var_os("FREESHOW_TEST_NODE").unwrap_or_else(||"node".into()))
        .args(["-e","const b=Buffer.alloc(8);b.writeDoubleLE(require('node:fs').statSync(process.argv[1]).mtimeMs);process.stdout.write(b.toString('hex'))"])
        .arg(&binary).output().unwrap();
    assert!(
        source_stat.status.success(),
        "{}",
        String::from_utf8_lossy(&source_stat.stderr)
    );
    let source_bits = String::from_utf8(source_stat.stdout).unwrap();
    let native_bits: String = executable
        .mtime_ms
        .to_le_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(native_bits,source_bits,"cache key retains actual Node fractional fs.Stats double bits without JSON float re-parsing");
    assert!(prepared
        .input()
        .cache_key()
        .unwrap()
        .unwrap()
        .starts_with(&format!("1|{source}|")));
    let missing = inputs.freeze(None, 8, false).await.unwrap();
    assert!(missing.input().executable.is_none() && missing.input().cache.is_none());
    inputs.shutdown().await.unwrap();
    inputs.shutdown().await.unwrap();
    assert!(inputs.freeze(None, 9, false).await.is_err());
}

#[tokio::test]
async fn cache_commit_uses_sealed_intent_exact_bytes_and_rejects_changed_generation_binary_and_root(
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    fs::create_dir(&root).unwrap();
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    let binary = executable_copy(temp.path());
    let inputs = EncoderInputs::new(root.clone()).unwrap();
    let prepared = inputs
        .freeze(Some(binary.to_str().unwrap().to_owned()), 2, true)
        .await
        .unwrap();
    let encoders = EncoderService::new(EncoderConfig::default()).unwrap();
    // This controlled executable is a test harness, not FFmpeg: it rejects the
    // listing flags. No encoder availability is fabricated by the fixture.
    let detection = encoders
        .detect(prepared.input().clone(), true)
        .await
        .unwrap();
    assert!(detection.detection["encoders"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["available"] == false));
    let intent = detection.cache_write.as_ref().unwrap();
    assert!(prepared.commit_cache(intent, &other, 2).is_err());
    assert!(prepared.commit_cache(intent, &root, 3).is_err());
    assert!(!root.join("encoder-detection.json").exists());
    prepared.commit_cache(intent, &root, 2).unwrap();
    assert_eq!(
        fs::read(root.join("encoder-detection.json")).unwrap(),
        intent.bytes()
    );
    let actual: Value = serde_json::from_slice(intent.bytes()).unwrap();
    assert_eq!(actual["detection"], detection.detection);
    fs::OpenOptions::new()
        .append(true)
        .open(&binary)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert!(prepared.commit_cache(intent, &root, 2).is_err());
    assert_eq!(
        fs::read(root.join("encoder-detection.json")).unwrap(),
        intent.bytes()
    );
    encoders.shutdown().await.unwrap();
    inputs.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_cache_is_bounded_force_skips_it_and_special_cache_files_never_hold_a_worker() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    fs::create_dir(&root).unwrap();
    let binary = executable_copy(temp.path());
    let cache = root.join("encoder-detection.json");
    fs::File::create(&cache)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let inputs = EncoderInputs::new(root.clone()).unwrap();
    let source = Some(binary.to_str().unwrap().to_string());
    assert!(
        matches!(inputs.freeze(source.clone(),1,false).await,Err(error) if error.code=="encoder_cache_limit")
    );
    assert!(inputs
        .freeze(source.clone(), 1, true)
        .await
        .unwrap()
        .input()
        .cache
        .is_none());
    fs::remove_file(&cache).unwrap();
    fs::create_dir(&cache).unwrap();
    assert!(inputs
        .freeze(source.clone(), 1, false)
        .await
        .unwrap()
        .input()
        .cache
        .is_none());
    fs::remove_dir(&cache).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(cache.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            inputs.freeze(source.clone(), 1, false),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.input().cache.is_none());
        fs::remove_file(&cache).unwrap();
        std::os::unix::fs::symlink(&binary, &cache).unwrap();
        assert!(inputs
            .freeze(source, 1, false)
            .await
            .unwrap()
            .input()
            .cache
            .is_none());
    }
    inputs.shutdown().await.unwrap();
}

#[tokio::test]
async fn replaced_cache_directory_is_rejected_before_reading_replacement_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    fs::create_dir(&root).unwrap();
    let binary = executable_copy(temp.path());
    let inputs = EncoderInputs::new(root.clone()).unwrap();
    fs::rename(&root, temp.path().join("original-app")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("encoder-detection.json"), b"replacement").unwrap();
    let error = inputs
        .freeze(Some(binary.to_str().unwrap().to_owned()), 1, false)
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("directory identity changed"));
    inputs.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn equal_mtime_symlink_retarget_cannot_commit_another_executable_cache() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    fs::create_dir(&root).unwrap();
    let first = executable_copy(temp.path());
    let second = temp.path().join("other-encoder");
    fs::copy(&first, &second).unwrap();
    fs::File::options()
        .write(true)
        .open(&second)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(fs::metadata(&first).unwrap().modified().unwrap()),
        )
        .unwrap();
    let alias = temp.path().join("encoder-alias");
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let inputs = EncoderInputs::new(root.clone()).unwrap();
    let prepared = inputs
        .freeze(Some(alias.to_str().unwrap().to_owned()), 2, true)
        .await
        .unwrap();
    let service = EncoderService::new(EncoderConfig::default()).unwrap();
    let detection = service
        .detect(prepared.input().clone(), true)
        .await
        .unwrap();
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&second, &alias).unwrap();
    assert_eq!(
        fs::metadata(&first).unwrap().modified().unwrap(),
        fs::metadata(&alias).unwrap().modified().unwrap()
    );
    let error = prepared
        .commit_cache(detection.cache_write.as_ref().unwrap(), &root, 2)
        .unwrap_err();
    assert!(error.message.contains("resolution changed"));
    assert!(!root.join("encoder-detection.json").exists());
    service.shutdown().await.unwrap();
    inputs.shutdown().await.unwrap();
}

#[path = "../src/encoders.rs"]
mod encoders;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_media::{MediaConfig, MediaService};
use serde_json::json;
use std::{path::PathBuf, sync::LazyLock, time::Duration};
use tokio::{sync::oneshot, time::timeout};

static HELPER: LazyLock<(tempfile::TempDir, PathBuf)> = LazyLock::new(|| {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join(if cfg!(windows) {
        "encoder-process.exe"
    } else {
        "encoder-process"
    });
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encoder_process.rs");
    let output = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg("-Cdebuginfo=0")
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (dir, binary)
});
fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":context,"listenerId":"encoder-host-correlation"})).unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    library: LibraryService,
    media: MediaService,
    native: encoders::NativeEncoders,
}
impl Fixture {
    fn new(mode: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let candidates = mode
            .map(|mode| {
                let path = dir.path().join(format!(
                    "encoder-fixture-{mode}{}",
                    if cfg!(windows) { ".exe" } else { "" }
                ));
                fs::copy(&HELPER.1, &path).unwrap();
                vec![path]
            })
            .unwrap_or_default();
        let library = LibraryService::start(
            Runtime::new(dir.path().join("app"), dir.path().join("data")).unwrap(),
        )
        .unwrap();
        let media = MediaService::new(MediaConfig {
            ffmpeg_candidates: candidates,
            discover_system: false,
            ..MediaConfig::default()
        })
        .unwrap();
        let native =
            encoders::NativeEncoders::new(library.clone(), media.clone(), dir.path().join("app"))
                .unwrap();
        Self {
            dir,
            library,
            media,
            native,
        }
    }
    fn cache(&self) -> PathBuf {
        self.dir.path().join("app/encoder-detection.json")
    }
    fn witness(&self) -> PathBuf {
        self.dir.path().join("witness")
    }
    async fn warm_resolution(&self) {
        assert_eq!(
            self.media
                .dispatch("FFMPEG_CHECK", Value::Null)
                .await
                .unwrap()
                .data["installed"],
            true
        );
    }
    async fn close(&self) {
        self.native.shutdown().await.unwrap();
        self.media.shutdown().await;
        self.library.finish_writes().await.unwrap();
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn listing_pids(root: &Path) -> Vec<u32> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix("listing-")?
                .parse()
                .ok()
        })
        .collect()
}
#[cfg(unix)]
fn assert_reaped(pids: &[u32]) {
    assert!(!pids.is_empty());
    for pid in pids {
        assert_eq!(
            unsafe { libc::kill(*pid as i32, 0) },
            -1,
            "controlled child {pid} still exists"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
#[tokio::test]
async fn missing_tool_reply_context_correlation_admission_and_terminal_shutdown_are_explicit() {
    assert_eq!(encoders::COMMANDS, ["ENCODER_DETECT"]);
    let f = Fixture::new(None);
    let context = f.library.context().unwrap();
    let reply = f
        .native
        .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
        .await
        .unwrap();
    assert_eq!(reply.len(), 1);
    let value = serde_json::to_value(&reply[0]).unwrap();
    assert_eq!(value["message"]["channel"], "ENCODER_DETECT");
    assert_eq!(value["listenerId"], "encoder-host-correlation");
    assert_eq!(value["context"], json!(context));
    assert_eq!(value["message"]["data"], freeshow_encoders::unavailable());
    assert!(!f.cache().exists());
    assert_eq!(
        f.native
            .dispatch(request("SET_RTMP_ENCODER", Value::Null, &context), "main")
            .await
            .unwrap_err()
            .code,
        "unsupported_command"
    );
    assert_eq!(
        f.native
            .dispatch(
                request("ENCODER_DETECT", json!({"x":"a".repeat(65536)}), &context),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "encoder_input_limit"
    );
    f.native.shutdown().await.unwrap();
    assert_eq!(
        f.native
            .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
            .await
            .unwrap_err()
            .code,
        "encoders_shutdown"
    );
    assert_eq!(f.native.status()["admittedRequests"], 0);
    f.close().await;
}
#[tokio::test]
async fn whole_host_leases_survive_caller_drop_and_owned_cache_wait_without_holding_probe_runtime()
{
    let f = Fixture::new(Some("gate"));
    f.warm_resolution().await;
    let context = f.library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, held) = std::sync::mpsc::sync_channel(1);
    let library = f.library.clone();
    let captured = context.clone();
    let owner = tokio::spawn(async move {
        library
            .commit(
                request("HOST_HOLD", Value::Null, &captured),
                "main",
                move |_| {
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    Ok(vec![])
                },
            )
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let mut callers = Vec::new();
    for index in 0..4 {
        let native = f.native.clone();
        let context = context.clone();
        callers.push(tokio::spawn(async move {
            native
                .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
                .await
        }));
        // Admit through the independently bounded media resolver before the
        // next caller; all four then overlap on the same gated detection.
        until(|| f.native.status()["binding"]["service"]["admitted_callers"] == index + 1).await;
    }
    assert_eq!(f.native.status()["admittedRequests"], 4);
    assert_eq!(
        f.native
            .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
            .await
            .unwrap_err()
            .code,
        "encoders_busy"
    );
    assert_eq!(
        listing_pids(&f.witness()).len(),
        1,
        "non-forced concurrent calls share one real controlled child"
    );
    callers.pop().unwrap().abort();
    fs::write(f.witness().join("release"), b"release").unwrap();
    until(|| f.library.status().unwrap().queued_jobs == 4).await;
    assert_eq!(
        f.native.status()["admittedRequests"],
        4,
        "dropped IPC receiver does not release accepted cache ownership"
    );
    assert_eq!(
        f.native.status()["binding"]["service"]["active_children"],
        0
    );
    assert!(!f.cache().exists());
    release.send(()).unwrap();
    owner.await.unwrap();
    for caller in callers {
        let events = timeout(Duration::from_secs(4), caller)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(events[0].message.channel, "ENCODER_DETECT");
    }
    until(|| f.native.status()["admittedRequests"] == 0).await;
    let cache: Value = serde_json::from_slice(&fs::read(f.cache()).unwrap()).unwrap();
    assert!(cache["detection"]["encoders"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["available"] == false));
    assert_eq!(f.native.status()["cache"]["persisted"], true);
    let before = listing_pids(&f.witness()).len();
    let events = f
        .native
        .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
        .await
        .unwrap();
    assert_eq!(events[0].message.data.as_ref(), Some(&cache["detection"]));
    assert_eq!(
        listing_pids(&f.witness()).len(),
        before,
        "cache hit launches no new child"
    );
    f.close().await;
}
#[tokio::test]
async fn failed_cache_persistence_preserves_valid_detection_reply() {
    let f = Fixture::new(Some("fail"));
    fs::create_dir(f.cache()).unwrap();
    let context = f.library.context().unwrap();
    let reply = f
        .native
        .dispatch(
            request("ENCODER_DETECT", json!({"force":true}), &context),
            "main",
        )
        .await
        .unwrap();
    assert_eq!(reply[0].message.channel, "ENCODER_DETECT");
    assert!(reply[0].message.data.as_ref().unwrap()["encoders"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["available"] == false));
    assert!(f.cache().is_dir());
    assert_eq!(f.native.status()["cache"]["complete"], true);
    assert_eq!(f.native.status()["cache"]["persisted"], false);
    f.close().await;
}
#[tokio::test]
async fn context_retirement_reaps_active_probe_rejects_old_reply_and_binds_fresh_owner() {
    let f = Fixture::new(Some("gate"));
    let old = f.library.context().unwrap();
    let old_run = f.native.status()["binding"]["run"].clone();
    let native = f.native.clone();
    let captured = old.clone();
    let caller = tokio::spawn(async move {
        native
            .dispatch(request("ENCODER_DETECT", Value::Null, &captured), "main")
            .await
    });
    until(|| f.native.status()["binding"]["service"]["active_children"] == 1).await;
    until(|| !listing_pids(&f.witness()).is_empty()).await;
    let pids = listing_pids(&f.witness());
    let other = f.dir.path().join("other");
    fs::create_dir(&other).unwrap();
    f.library
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":old.data_path,"newPath":other}),
                &old,
            ),
            "main",
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(3), f.native.context_changed())
        .await
        .unwrap()
        .unwrap();
    assert!(caller.await.unwrap().is_err());
    assert_ne!(old_run, f.native.status()["binding"]["run"]);
    #[cfg(unix)]
    assert_reaped(&pids);
    #[cfg(not(unix))]
    assert!(!pids.is_empty());
    assert_eq!(
        f.native
            .dispatch(request("ENCODER_DETECT", Value::Null, &old), "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    fs::write(f.witness().join("release"), b"release").unwrap();
    let current = f.library.context().unwrap();
    let reply = f
        .native
        .dispatch(request("ENCODER_DETECT", Value::Null, &current), "main")
        .await
        .unwrap();
    assert_eq!(reply[0].context.as_ref(), Some(&current));
    f.close().await;
}
#[tokio::test]
async fn terminal_shutdown_reaps_probe_and_drains_owned_request_after_caller_abort() {
    let f = Fixture::new(Some("gate"));
    let native = f.native.clone();
    let context = f.library.context().unwrap();
    let caller = tokio::spawn(async move {
        native
            .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
            .await
    });
    until(|| f.native.status()["binding"]["service"]["active_children"] == 1).await;
    until(|| !listing_pids(&f.witness()).is_empty()).await;
    let pids = listing_pids(&f.witness());
    caller.abort();
    timeout(Duration::from_secs(3), f.native.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert_eq!(f.native.status()["closing"], true);
    #[cfg(unix)]
    assert_reaped(&pids);
    #[cfg(not(unix))]
    assert!(!pids.is_empty());
    f.close().await;
}

#[tokio::test]
async fn terminal_shutdown_waits_for_accepted_cache_owner_after_ipc_receiver_disappears() {
    let f = Fixture::new(Some("fail"));
    f.warm_resolution().await;
    let context = f.library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, held) = std::sync::mpsc::sync_channel(1);
    let library = f.library.clone();
    let captured = context.clone();
    let owner = tokio::spawn(async move {
        library
            .commit(
                request("HOST_HOLD", Value::Null, &captured),
                "main",
                move |_| {
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    Ok(vec![])
                },
            )
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let native = f.native.clone();
    let caller = tokio::spawn(async move {
        native
            .dispatch(request("ENCODER_DETECT", Value::Null, &context), "main")
            .await
    });
    until(|| f.library.status().unwrap().queued_jobs == 1).await;
    caller.abort();
    let native = f.native.clone();
    let shutdown = tokio::spawn(async move { native.shutdown().await });
    until(|| f.native.status()["closing"] == true).await;
    assert_eq!(f.native.status()["admittedRequests"], 1);
    assert!(
        !shutdown.is_finished(),
        "accepted cache closure retains its host lease until the ordered owner handles it"
    );
    release.send(()).unwrap();
    owner.await.unwrap();
    timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert!(
        !f.cache().exists(),
        "terminal retirement rejects an obsolete derived cache effect"
    );
    f.close().await;
}
