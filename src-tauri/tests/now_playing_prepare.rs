#[path = "../src/assets.rs"]
#[allow(dead_code)]
mod assets;
#[path = "../src/now_playing_prepare.rs"]
mod now_playing_prepare;

use freeshow_audio::{AudioConfig, AudioService, CommonValue, CoverKnowledge};
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_cover_images::{Cancel, Context, CoverImageService, ImagePolicy, Reply};
use now_playing_prepare::{
    FailureKind, PreparationControl, PreparationServices, PrepareError, Prepared, TestHook,
    TestStage,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

struct Fixture {
    temporary: tempfile::TempDir,
    library: LibraryService,
    assets: assets::NativeAssets,
    audio: AudioService,
    covers: CoverImageService,
    current: RuntimeContext,
    cover_context: Context,
}
impl Fixture {
    async fn new() -> Arc<Self> {
        Self::configured(AudioConfig::default()).await
    }
    async fn configured(config: AudioConfig) -> Arc<Self> {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(
            temporary.path().join("app"),
            temporary.path().join("library"),
        )
        .unwrap();
        let library = LibraryService::start(runtime).unwrap();
        let current = library.context().unwrap();
        let assets = assets::NativeAssets::start(&current, &temporary.path().join("app"))
            .await
            .unwrap();
        let cover_context = Context::from_host(
            &current.runtime_id,
            current.library_generation,
            current.data_path.to_str().unwrap(),
        )
        .unwrap();
        Arc::new(Self {
            temporary,
            library,
            assets,
            audio: AudioService::new(config).unwrap(),
            covers: CoverImageService::new().unwrap(),
            current,
            cover_context,
        })
    }
    fn services(&self) -> PreparationServices<'_> {
        PreparationServices::new(&self.library, &self.assets, &self.audio, &self.covers)
    }
    async fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.temporary.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.assets
            .import_grants(&self.library, &self.current, std::slice::from_ref(&path))
            .await
            .unwrap();
        path
    }
    async fn prepare(&self, path: &std::path::Path) -> Result<Prepared, PrepareError> {
        self.services()
            .prepare(
                &self.current,
                &self.cover_context,
                path.to_str().unwrap(),
                &PreparationControl::default(),
            )
            .await
    }
    async fn switch(&self) {
        let next = self.temporary.path().join("new-library");
        fs::create_dir_all(&next).unwrap();
        let request: Request = serde_json::from_value(json!({
            "channel":"MAIN", "context":self.current,
            "data":{"channel":"UPDATE_DATA_PATH", "data":{"oldPath":self.current.data_path,"newPath":next}}
        }))
        .unwrap();
        self.library.dispatch(request, "main").await.unwrap();
        self.assets
            .context_changed(&self.library.context().unwrap())
            .unwrap();
    }
    async fn shutdown(&self) {
        self.audio.shutdown().await;
        self.covers.shutdown(Duration::from_secs(2)).await.unwrap();
        self.assets.shutdown().await.unwrap();
        assert_eq!(self.assets.status()["nativeReads"], 0);
        assert_eq!(self.assets.status()["fileWorkersJoined"], true);
        assert_eq!(self.covers.status().sessions, 0);
        assert_eq!(self.covers.status().finished_workers, 2);
        self.library.finish_writes().await.unwrap();
    }
}
fn error(result: Result<Prepared, PrepareError>) -> PrepareError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("Expected an explicit preparation error"),
    }
}
fn png() -> Vec<u8> {
    // The existing cover fixture generator's opaque red 1x1 RGBA PNG.
    "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082"
        .as_bytes().as_chunks::<2>().0.iter()
        .map(|s| u8::from_str_radix(std::str::from_utf8(s).unwrap(),16).unwrap()).collect()
}
fn wav(extra: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
    let mut out = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x02\0\0\0\0\0".to_vec();
    if let Some((key, value)) = extra {
        out.extend(key);
        out.extend((value.len() as u32).to_le_bytes());
        out.extend(value);
        if value.len() % 2 == 1 {
            out.push(0);
        }
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out
}
fn safe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 127) as u8,
        ((n >> 14) & 127) as u8,
        ((n >> 7) & 127) as u8,
        (n & 127) as u8,
    ]
}
fn mp3(mime: &str) -> Vec<u8> {
    let mut tag = Vec::new();
    let mut frames = vec![
        (
            b"TIT2",
            [vec![3], "Café 音楽 🎵".as_bytes().to_vec()].concat(),
        ),
        (b"TCON", b"\x039999".to_vec()),
    ];
    for (kind, description) in [(255, "undefined first"), (4, "Back")] {
        let body = [
            vec![3],
            mime.as_bytes().to_vec(),
            vec![0, kind],
            description.as_bytes().to_vec(),
            vec![0],
            png(),
        ]
        .concat();
        frames.push((b"APIC", body));
    }
    for (key, body) in frames {
        tag.extend(key);
        tag.extend(safe(body.len()));
        tag.extend([0, 0]);
        tag.extend(body);
    }
    let mut out = b"ID3\x04\0\0".to_vec();
    out.extend(safe(tag.len()));
    out.extend(tag);
    let mut frame = vec![0; 417];
    frame[..4].copy_from_slice(&[255, 251, 144, 100]);
    for _ in 0..8 {
        out.extend(&frame);
    }
    out
}
fn flac() -> Vec<u8> {
    let mut info = vec![0; 34];
    info[..4].copy_from_slice(&[0x10, 0, 0x10, 0]);
    info[10..18].copy_from_slice(&((44100u64 << 44) | (1 << 41) | (15 << 36) | 4410).to_be_bytes());
    let mut body = Vec::new();
    for kind in [3u32, 255] {
        let mut picture = kind.to_be_bytes().to_vec();
        for text in ["IMAGE/PNG", "Café 音楽 🎵"] {
            picture.extend((text.len() as u32).to_be_bytes());
            picture.extend(text.as_bytes());
        }
        for n in [123u32, 456, 24, 0, png().len() as u32] {
            picture.extend(n.to_be_bytes());
        }
        picture.extend(png());
        body.push((6, picture));
    }
    let blocks = [(0, info)].into_iter().chain(body).collect::<Vec<_>>();
    let mut out = b"fLaC".to_vec();
    for (index, (kind, bytes)) in blocks.iter().enumerate() {
        out.push(kind | if index + 1 == blocks.len() { 128 } else { 0 });
        out.extend(&((bytes.len() as u32).to_be_bytes())[1..]);
        out.extend(bytes);
    }
    out
}
fn hook(stage: TestStage) -> Arc<TestHook> {
    Arc::new(TestHook {
        stage,
        entered: Default::default(),
        resume: Default::default(),
    })
}
fn owned_prepare(
    fixture: Arc<Fixture>,
    path: PathBuf,
    hook: Arc<TestHook>,
    control: PreparationControl,
) -> tokio::task::JoinHandle<Result<Prepared, PrepareError>> {
    // Models the root's retained fixed operation owner, not a task spawned by
    // the helper and not a claim that dropping a helper future is safe.
    tokio::spawn(async move {
        let mut services = fixture.services();
        services.hook = Some(&hook);
        services
            .prepare(
                &fixture.current,
                &fixture.cover_context,
                path.to_str().unwrap(),
                &control,
            )
            .await
    })
}
async fn entered(hook: &TestHook) {
    tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn wav_empty_is_certified_by_actual_cover_read_and_result_keeps_both_native_leases() {
    let f = Fixture::new().await;
    let path = f.source("日本語 literal% source.wav", &wav(None)).await;
    let prepared = f.prepare(&path).await.unwrap();
    assert_eq!(prepared.source_path(), path.to_str().unwrap());
    assert_eq!(prepared.context(), &f.current);
    assert_eq!(prepared.common().covers(), CoverKnowledge::Unknown);
    assert!(prepared.all_pictures().is_empty());
    assert_eq!(prepared.audio_observations().unchanged(), Some(true));
    assert_eq!(
        prepared.authority().context(),
        &assets::context(&f.current).unwrap()
    );
    assert_eq!(f.assets.status()["nativeReads"], 1);
    assert_eq!(f.covers.status().sessions, 1);
    prepared
        .revalidate(&f.library, &PreparationControl::default())
        .await
        .unwrap();
    drop(prepared);
    f.shutdown().await;
}

#[tokio::test]
async fn same_sequential_descriptor_produces_complete_mp3_flac_pictures_and_idle_image_session() {
    let f = Fixture::new().await;
    for (name, bytes) in [("曲.mp3", mp3("image/png")), ("音楽.flac", flac())] {
        let path = f.source(name, &bytes).await;
        let prepared = f.prepare(&path).await.unwrap();
        assert_eq!(prepared.all_pictures().len(), 2);
        assert!(prepared.all_pictures().iter().all(|p| p.data == png()));
        if name.ends_with("mp3") {
            assert_eq!(prepared.common().json_projection()["title"], "Café 音楽 🎵");
            assert!(
                matches!(prepared.common().value().get("genre"), CommonValue::Array(values) if matches!(values.as_slice(),[CommonValue::Undefined]))
            );
            assert_eq!(prepared.all_pictures()[0].undefined_fields, ["type"]);
            assert_eq!(prepared.all_pictures()[1].fields["type"], "Cover (back)");
        } else {
            assert_eq!(prepared.all_pictures()[1].undefined_fields, ["type"]);
            let keys = prepared.all_pictures()[0]
                .fields
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(
                keys,
                [
                    "format",
                    "description",
                    "width",
                    "height",
                    "colour_depth",
                    "indexed_color",
                    "type"
                ]
            );
        }
        // A live Covers observation would keep this busy. Preparation has
        // disposed it, retaining the actual Session for genuine later pixels.
        let image = f
            .covers
            .create(
                prepared.session(),
                prepared.all_pictures()[0].data.clone(),
                ImagePolicy::VerifiedPngPixels,
                Cancel::default(),
            )
            .unwrap()
            .finish()
            .await
            .unwrap();
        image
            .inspect(|reply| {
                assert!(matches!(
                    reply,
                    Reply::Image {
                        width: 1,
                        height: 1
                    }
                ))
            })
            .unwrap();
        drop(image);
        let (common, pictures, authority, session, captured) = prepared.into_parts();
        assert_eq!(captured, f.current);
        assert_eq!(pictures.len(), 2);
        assert!(common.json_projection().is_object());
        assert_eq!(authority.source_path(), path.to_str().unwrap());
        f.covers
            .retire(&session, Duration::from_secs(2))
            .await
            .unwrap();
        drop((common, pictures, authority, session));
    }
    f.shutdown().await;
}

#[tokio::test]
async fn unsupported_covers_cannot_become_prepared_even_when_common_succeeds() {
    let f = Fixture::new().await;
    let path = f.source("unsupported.mp3", &mp3("")).await;
    let e = error(f.prepare(&path).await);
    assert_eq!(e.kind, FailureKind::Unsupported);
    assert_eq!(e.code, "cover_unsupported");
    assert_eq!(f.assets.status()["nativeReads"], 0);
    assert_eq!(f.covers.status().sessions, 0);
    f.shutdown().await;
}

#[tokio::test]
async fn parser_rejection_resource_and_absent_authority_stay_distinct() {
    let f = Fixture::new().await;
    let path = f.source("invalid.mp3", b"not an audio container").await;
    let e = error(f.prepare(&path).await);
    assert_eq!(e.kind, FailureKind::Input);
    assert_eq!(e.code, "audio_input_rejected");
    let missing = f.temporary.path().join("unregistered.wav");
    fs::write(&missing, wav(None)).unwrap();
    let e = error(f.prepare(&missing).await);
    assert_eq!(e.kind, FailureKind::Authorization);
    assert_eq!(e.code, "assets_unregistered");
    f.shutdown().await;
    let f = Fixture::configured(AudioConfig {
        read_limit: 128,
        ..Default::default()
    })
    .await;
    let path = f.source("bounded.mp3", &mp3("image/png")).await;
    let e = error(f.prepare(&path).await);
    assert_eq!(e.kind, FailureKind::Resource);
    assert_eq!(e.code, "audio_resource_limit");
    f.shutdown().await;
}

#[tokio::test]
async fn every_public_cover_context_field_is_checked_before_native_file_admission() {
    let f = Fixture::new().await;
    let path = f.source("context.wav", &wav(None)).await;
    for context in [
        Context::from_host(
            "foreign",
            f.current.library_generation,
            f.current.data_path.to_str().unwrap(),
        )
        .unwrap(),
        Context::from_host(
            &f.current.runtime_id,
            f.current.library_generation + 1,
            f.current.data_path.to_str().unwrap(),
        )
        .unwrap(),
        Context::from_host(
            &f.current.runtime_id,
            f.current.library_generation,
            "/foreign",
        )
        .unwrap(),
    ] {
        let e = error(
            f.services()
                .prepare(
                    &f.current,
                    &context,
                    path.to_str().unwrap(),
                    &PreparationControl::default(),
                )
                .await,
        );
        assert_eq!(e.kind, FailureKind::Context);
        assert_eq!(e.code, "cover_context");
        assert_eq!(f.assets.status()["nativeReads"], 0);
        assert_eq!(f.covers.status().sessions, 0);
    }
    f.shutdown().await;
}

#[tokio::test]
async fn inter_read_in_place_changes_after_common_and_covers_reject_actual_preparation() {
    let f = Fixture::new().await;
    for stage in [TestStage::Common, TestStage::Covers, TestStage::Ready] {
        let path = f.source("changed.mp3", &mp3("image/png")).await;
        let gate = hook(stage);
        let owner = owned_prepare(
            f.clone(),
            path.clone(),
            gate.clone(),
            PreparationControl::default(),
        );
        entered(&gate).await;
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"observed mutation")
            .unwrap();
        gate.resume.notify_one();
        let e = error(owner.await.unwrap());
        assert_eq!(e.kind, FailureKind::Authorization);
        assert_eq!(e.code, "assets_changed");
        assert_eq!(f.assets.status()["nativeReads"], 0);
        assert_eq!(f.covers.status().sessions, 0);
    }
    f.shutdown().await;
}

#[tokio::test]
async fn replaced_path_and_replaced_registry_grant_are_separately_rejected() {
    let f = Fixture::new().await;
    for regrant in [false, true] {
        let path = f.source("replaced.mp3", &mp3("image/png")).await;
        let gate = hook(TestStage::Common);
        let owner = owned_prepare(
            f.clone(),
            path.clone(),
            gate.clone(),
            PreparationControl::default(),
        );
        entered(&gate).await;
        fs::rename(&path, f.temporary.path().join(format!("old-{regrant}.mp3"))).unwrap();
        fs::write(&path, mp3("image/png")).unwrap();
        if regrant {
            f.assets
                .import_grants(&f.library, &f.current, std::slice::from_ref(&path))
                .await
                .unwrap();
        }
        gate.resume.notify_one();
        let e = error(owner.await.unwrap());
        assert_eq!(e.kind, FailureKind::Authorization);
        assert_eq!(
            e.code,
            if regrant {
                "assets_revoked"
            } else {
                "assets_changed"
            }
        );
    }
    f.shutdown().await;
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn symlink_swap_to_protected_cache_between_real_reads_never_authorizes_the_held_result() {
    let f = Fixture::new().await;
    let path = f.source("public.mp3", &mp3("image/png")).await;
    let protected = f.temporary.path().join("app/protected-cache/private.mp3");
    fs::create_dir_all(protected.parent().unwrap()).unwrap();
    fs::write(&protected, mp3("image/png")).unwrap();
    let gate = hook(TestStage::Common);
    let owner = owned_prepare(
        f.clone(),
        path.clone(),
        gate.clone(),
        PreparationControl::default(),
    );
    entered(&gate).await;
    fs::rename(&path, f.temporary.path().join("held.mp3")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&protected, &path).unwrap();
    #[cfg(windows)]
    if let Err(e) = std::os::windows::fs::symlink_file(&protected, &path) {
        if e.raw_os_error() == Some(1314) {
            // Capability-specific skip only; actual generic file tests still run.
            eprintln!("Symlink swap requires Windows symlink privilege (1314)");
            gate.resume.notify_one();
            let _ = owner.await.unwrap();
            f.shutdown().await;
            return;
        }
        panic!("Actual Windows symlink creation failed: {e}");
    }
    gate.resume.notify_one();
    let e = error(owner.await.unwrap());
    assert_eq!(e.kind, FailureKind::Authorization);
    assert_eq!(e.code, "assets_protected");
    assert_eq!(f.assets.status()["nativeReads"], 0);
    f.shutdown().await;
}

#[tokio::test]
async fn root_change_after_complete_cover_read_prevents_ready_value() {
    let f = Fixture::new().await;
    let path = f.source("retiring.mp3", &mp3("image/png")).await;
    let gate = hook(TestStage::Ready);
    let owner = owned_prepare(f.clone(), path, gate.clone(), PreparationControl::default());
    entered(&gate).await;
    f.switch().await;
    gate.resume.notify_one();
    let e = error(owner.await.unwrap());
    assert_eq!(e.kind, FailureKind::Context);
    assert_eq!(e.code, "stale_library");
    f.shutdown().await;
}

#[tokio::test]
async fn cancellation_reaches_pre_admission_and_each_real_inter_read_boundary() {
    let f = Fixture::new().await;
    let path = f.source("cancel.mp3", &mp3("image/png")).await;
    let control = PreparationControl::default();
    control.cancel();
    assert_eq!(
        error(
            f.services()
                .prepare(
                    &f.current,
                    &f.cover_context,
                    path.to_str().unwrap(),
                    &control
                )
                .await
        )
        .kind,
        FailureKind::Cancelled
    );
    for stage in [
        TestStage::Opened,
        TestStage::Common,
        TestStage::Covers,
        TestStage::Ready,
    ] {
        let control = PreparationControl::default();
        let gate = hook(stage);
        let owner = owned_prepare(f.clone(), path.clone(), gate.clone(), control.clone());
        entered(&gate).await;
        control.cancel();
        gate.resume.notify_one();
        assert_eq!(error(owner.await.unwrap()).kind, FailureKind::Cancelled);
        assert_eq!(f.assets.status()["nativeReads"], 0);
        assert_eq!(f.covers.status().sessions, 0);
    }
    let control = PreparationControl::new(std::time::Instant::now());
    assert_eq!(
        error(
            f.services()
                .prepare(
                    &f.current,
                    &f.cover_context,
                    path.to_str().unwrap(),
                    &control
                )
                .await
        )
        .code,
        "now_playing_deadline"
    );
    f.shutdown().await;
}

#[tokio::test]
async fn cancellation_of_accepted_audio_and_cover_jobs_waits_for_real_receipt_and_cleanup() {
    let f = Fixture::new().await;
    let path = f.source("accepted.mp3", &mp3("image/png")).await;
    for stage in [TestStage::AudioAccepted, TestStage::CoverAccepted] {
        let control = PreparationControl::default();
        let gate = hook(stage);
        let owner = owned_prepare(f.clone(), path.clone(), gate.clone(), control.clone());
        entered(&gate).await;
        assert_eq!(f.assets.status()["nativeReads"], 1);
        assert_eq!(f.covers.status().sessions, 1);
        if stage == TestStage::AudioAccepted {
            // Accepted or ready-uncollected audio still owns one shared slot.
            let spare = f.audio.reserve_opened().unwrap();
            assert!(matches!(
                f.audio.reserve_opened(),
                Err(freeshow_audio::AudioError::Busy)
            ));
            drop(spare);
        }
        control.cancel();
        gate.resume.notify_one();
        let e = error(owner.await.unwrap());
        assert_eq!(e.kind, FailureKind::Cancelled);
        assert_eq!(
            e.code,
            if stage == TestStage::CoverAccepted {
                "cover_cancelled"
            } else {
                "cancelled"
            }
        );
        assert_eq!(f.assets.status()["nativeReads"], 0);
        assert_eq!(f.covers.status().sessions, 0);
        assert_eq!(f.covers.status().active, 0);
        let first = f.audio.reserve_opened().unwrap();
        let second = f.audio.reserve_opened().unwrap();
        drop((first, second));
    }
    f.shutdown().await;
}

#[tokio::test]
async fn four_completed_results_retain_actual_quotas_until_consumed_or_dropped() {
    let f = Fixture::new().await;
    let path = f.source("quota.wav", &wav(None)).await;
    let mut retained = Vec::new();
    for _ in 0..4 {
        retained.push(f.prepare(&path).await.unwrap());
    }
    assert_eq!(f.assets.status()["nativeReads"], 4);
    assert_eq!(f.covers.status().sessions, 4);
    assert_eq!(error(f.prepare(&path).await).code, "cover_busy");
    assert_eq!(
        f.assets
            .open_registered_media(
                &f.library,
                &f.current,
                path.to_str().unwrap(),
                Default::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_busy"
    );
    retained.pop();
    retained.push(f.prepare(&path).await.unwrap());
    drop(retained);
    f.shutdown().await;
}

#[tokio::test]
async fn retained_owner_not_dropped_observer_finishes_real_work_and_releases_dependencies() {
    let f = Fixture::new().await;
    let path = f.source("owned.mp3", &mp3("image/png")).await;
    let gate = hook(TestStage::Common);
    let (send, receive) = tokio::sync::oneshot::channel();
    let owner = owned_prepare(f.clone(), path, gate.clone(), PreparationControl::default());
    entered(&gate).await;
    let observer = tokio::spawn(receive);
    observer.abort();
    let _ = observer.await;
    assert_eq!(f.assets.status()["nativeReads"], 1);
    gate.resume.notify_one();
    let prepared = owner.await.unwrap().unwrap();
    let rejected = send.send(prepared).err().unwrap();
    assert_eq!(rejected.all_pictures().len(), 2);
    drop(rejected);
    f.shutdown().await;
}

fn raw(common: &CommonValue) -> Value {
    match common {
        CommonValue::Undefined => json!(["undefined"]),
        CommonValue::Null => json!(["null"]),
        CommonValue::Bool(v) => json!(["boolean", v]),
        CommonValue::String(v) => json!(["string", v]),
        CommonValue::Number(v) if v.is_nan() => json!(["number", "NaN"]),
        CommonValue::Number(v) if v.is_infinite() => json!([
            "number",
            if v.is_sign_negative() {
                "-Infinity"
            } else {
                "Infinity"
            }
        ]),
        CommonValue::Number(v) if *v == 0.0 && v.is_sign_negative() => json!(["number", "-0"]),
        CommonValue::Number(v) => json!(["number", v]),
        CommonValue::Array(v) => json!(["array", v.iter().map(raw).collect::<Vec<_>>()]),
        CommonValue::Object(v) => json!([
            "object",
            v.iter()
                .map(|(k, v)| json!([k, raw(v)]))
                .collect::<Vec<_>>()
        ]),
    }
}
fn equivalent(actual: &Value, expected: &Value) {
    if let (Some(a), Some(b)) = (actual.as_f64(), expected.as_f64()) {
        assert!((a - b).abs() <= 2e-14 * b.abs().max(1.0), "{a} != {b}");
    } else if let (Some(a), Some(b)) = (actual.as_array(), expected.as_array()) {
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            equivalent(a, b);
        }
    } else {
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn real_prepared_common_and_every_picture_match_pinned_now_playing_source() {
    let f = Fixture::new().await;
    let root = std::env::var_os("FREESHOW_NOW_PLAYING_ORACLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        });
    for (name, bytes) in [
        ("source.wav", wav(None)),
        ("source.mp3", mp3("image/png")),
        ("source.flac", flac()),
    ] {
        let path = f.source(name, &bytes).await;
        let prepared = f.prepare(&path).await.unwrap();
        let output = std::process::Command::new("node")
            .current_dir(&root)
            .args(["--input-type=module","-e",r#"
import ts from 'typescript'; import {parseFile} from 'music-metadata'; import {execFileSync} from 'node:child_process';
const timer=setTimeout(()=>process.exit(124),10000);
try {
 const source=execFileSync('git',['show','2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/audio/nowPlaying.ts'],{encoding:'utf8',timeout:10000,maxBuffer:1024*1024});
 const ast=ts.createSourceFile('nowPlaying.ts',source,ts.ScriptTarget.Latest,true);
 const declaration=ast.statements.find(s=>ts.isFunctionDeclaration(s)&&s.name?.text==='getAudioMetadata').getText(ast);
 const js=ts.transpileModule(declaration+';return getAudioMetadata',{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
 const sent=[]; const fn=new Function('parseFile','sendToMain','ToMain','console',js)(parseFile,(channel,data)=>sent.push({channel,data}),{AUDIO_METADATA:'AUDIO_METADATA'},{error(){}});
 const common=await fn(process.argv[1]); if(!common||sent.length!==1||sent[0].data.metadata!==common)throw Error('Actual source did not publish complete common');
 function raw(v){if(v===undefined)return ['undefined'];if(v===null)return ['null'];if(typeof v==='number')return ['number',!Number.isFinite(v)?String(v):Object.is(v,-0)?'-0':v];if(Array.isArray(v))return ['array',v.map(raw)];if(typeof v==='object')return ['object',Object.keys(v).map(k=>[k,raw(v[k])])];return [typeof v,v];}
 const pictures=(common.picture||[]).map(({data,...fields})=>{const undefined_fields=Object.keys(fields).filter(k=>fields[k]===undefined);return {fields,undefined_fields,data:Array.from(data)};});
 delete common.picture;
 process.stdout.write(JSON.stringify({common:raw(common),pictures,sourcePath:sent[0].data.filePath}));
} finally {clearTimeout(timer);}
"#])
            .arg(&path)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let original: Value = serde_json::from_slice(&output.stdout).unwrap();
        equivalent(&raw(prepared.common().value()), &original["common"]);
        assert_eq!(
            serde_json::to_value(prepared.all_pictures()).unwrap(),
            original["pictures"]
        );
        assert_eq!(
            prepared.source_path(),
            original["sourcePath"].as_str().unwrap()
        );
        drop(prepared);
    }
    f.shutdown().await;
}
