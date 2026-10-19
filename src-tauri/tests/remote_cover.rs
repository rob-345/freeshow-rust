#[allow(dead_code)]
#[path = "../src/now_playing_cover_file.rs"]
mod now_playing_cover_file;
#[path = "../src/remote_cover.rs"]
mod remote_cover;
#[allow(dead_code)]
#[path = "../src/remote_cover_publications.rs"]
mod remote_cover_publications;

#[cfg(target_os = "linux")]
use freeshow_core::protocol::Request;
use freeshow_core::{protocol::Event, runtime::Runtime, service::LibraryService};
use freeshow_remotes::{CoverControl, CoverErrorRendering, CoverSource, CoverState};
use now_playing_cover_file::CoverFiles;
use remote_cover::Source;
use remote_cover_publications::Publications;
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use std::{future::poll_fn, task::Poll};
use tokio_util::sync::CancellationToken;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
    _temp: tempfile::TempDir,
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Only Linux context-interleaving cases read the fixture library directly"
        )
    )]
    library: LibraryService,
    files: CoverFiles,
    publications: Publications,
    source: Source,
    events: tokio::sync::mpsc::Receiver<Event>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("selected")).unwrap(),
        )
        .unwrap();
        let files = CoverFiles::default();
        let publications = Publications::new();
        publications.ready(library.context().unwrap()).unwrap();
        let (send, events) = tokio::sync::mpsc::channel(4);
        let source = Source::new(
            library.context().unwrap(),
            library.clone(),
            files.clone(),
            publications.clone(),
            std::sync::Arc::new(move |event| send.try_send(event).map_err(|e| e.to_string())),
            CoverErrorRendering::Production,
        );
        Self {
            _temp: temp,
            library,
            files,
            publications,
            source,
            events,
        }
    }
    fn control(&self) -> CoverControl {
        CoverControl {
            cancel: CancellationToken::new(),
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
    #[cfg(target_os = "linux")]
    fn install(&self, bytes: &[u8]) -> std::path::PathBuf {
        let audio = self.library.context().unwrap().data_path.join("Audio");
        std::fs::create_dir_all(&audio).unwrap();
        let path = audio.join("NowPlayingCover.png");
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

#[tokio::test]
async fn native_source_reports_actual_platform_capability_and_settles_file_ownership() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    let mut prepared = f.source.prepare(f.control()).await.unwrap();
    #[cfg(target_os = "linux")]
    {
        assert!(matches!(prepared.state(), CoverState::Missing));
        prepared.before_headers().await.unwrap();
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert!(matches!(prepared.state(), CoverState::Failed));
        assert_eq!(
            prepared.before_headers().await.unwrap_err().code,
            "now_playing_cover_unsupported"
        );
    }
    prepared.finish().await.unwrap();
    assert_eq!((f.files.retained(), f.publications.retained()), (0, 0));
    assert!(f.events.try_recv().is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn held_opaque_bytes_and_posix_stat_survive_until_actual_header_validation_and_disposal() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    let path = f.install(b"opaque\0not-an-image\xff");
    let control = f.control();
    let mut prepared = f.source.prepare(control.clone()).await.unwrap();
    let CoverState::File(stat) = prepared.state() else {
        panic!("Actual held file required")
    };
    assert_eq!(stat.length(), std::fs::metadata(path).unwrap().len());
    assert!(stat.etag().starts_with("W/\""));
    prepared.before_headers().await.unwrap();
    let mut bytes = [0; 7];
    assert_eq!(prepared.read_at(0, &mut bytes).unwrap(), 7);
    assert_eq!(&bytes, b"opaque\0");
    assert_eq!(f.files.retained(), 1);
    control.cancel.cancel(); // Normal body disposal is independent of UI receipts.
    prepared.finish().await.unwrap();
    assert_eq!((f.files.retained(), f.publications.retained()), (0, 0));
    assert!(f.events.try_recv().is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn actual_folder_alert_precedes_missing_http_continuation_but_completion_owns_its_private_lease(
) {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    let context = f.library.context().unwrap();
    let audio = context.data_path.join("Audio");
    let blocker = b"independently checked regular Audio blocker";
    std::fs::write(&audio, blocker).unwrap();
    let control = f.control();
    let mut prepared = f.source.prepare(control.clone()).await.unwrap();
    let event = f.events.try_recv().unwrap();
    assert_eq!(event.channel, "MAIN");
    assert_eq!(event.context.as_ref(), Some(&context));
    assert!(event.listener_id.is_none());
    assert_eq!(
        serde_json::to_value(&event.message).unwrap(),
        serde_json::json!({"channel":"ALERT","data":format!("Error: Could not create folder at: {}!", audio.display())})
    );
    let private = event.native_cover.unwrap();
    let id = private["id"].as_str().unwrap();
    assert!(id.starts_with("native-cover-"));
    assert!(matches!(prepared.state(), CoverState::Missing));
    prepared.before_headers().await.unwrap(); // No UI ACK has happened.
    assert_eq!((f.files.retained(), f.publications.retained()), (1, 1));
    control.cancel.cancel();
    let mut finish = prepared.finish();
    poll_fn(|cx| {
        assert!(finish.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(
        (f.files.retained(), f.publications.retained()),
        (1, 1),
        "HTTP cancellation cannot forge private delivery"
    );
    f.publications.complete(&context, id, true).unwrap();
    finish.await.unwrap();
    assert_eq!((f.files.retained(), f.publications.retained()), (0, 0));
    assert_eq!(std::fs::read(&audio).unwrap(), blocker);
    assert!(f.events.try_recv().is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn replacement_between_prepare_and_headers_is_rejected_on_actual_ordered_revalidation() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let path = f.install(b"first");
    let mut prepared = f.source.prepare(f.control()).await.unwrap();
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    std::fs::write(&path, b"other").unwrap();
    assert_eq!(
        prepared.before_headers().await.unwrap_err().code,
        "now_playing_cover_changed"
    );
    prepared.finish().await.unwrap();
    assert_eq!((f.files.retained(), f.publications.retained()), (0, 0));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn library_change_rejects_old_authority_and_retirement_never_frees_unknown_ui_delivery() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    let first = f.library.context().unwrap();
    std::fs::write(first.data_path.join("Audio"), b"real blocker").unwrap();
    let mut prepared = f.source.prepare(f.control()).await.unwrap();
    let private = f.events.try_recv().unwrap().native_cover.unwrap();
    let new_path = f._temp.path().join("new-library");
    std::fs::create_dir(&new_path).unwrap();
    f.library.dispatch(Request { channel: "MAIN".into(), data: serde_json::json!({"channel":"UPDATE_DATA_PATH","data":{"oldPath":first.data_path,"newPath":new_path}}), listener_id: None, context: Some(first.clone()) }, "main").await.unwrap();
    f.publications.retire(&first, false);
    assert_eq!(
        prepared.before_headers().await.unwrap_err().code,
        "cover_context"
    );
    assert_eq!(
        f.source.prepare(f.control()).await.err().unwrap().code,
        "cover_context"
    );
    prepared.finish().await.unwrap();
    assert_eq!((f.files.retained(), f.publications.retained()), (0, 1));
    f.publications.ready(f.library.context().unwrap()).unwrap();
    assert_eq!(f.publications.retained(), 1);
    f.publications
        .complete(&first, private["id"].as_str().unwrap(), false)
        .unwrap();
    assert_eq!(f.publications.retained(), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn http_cancellation_after_actual_mkdir_failure_does_not_erase_its_independent_alert() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    let context = f.library.context().unwrap();
    std::fs::write(context.data_path.join("Audio"), b"real blocker").unwrap();
    let (entered, release) = f
        .files
        .test_pause(now_playing_cover_file::TestStage::AudioAnchored);
    let control = f.control();
    let (prepared, ()) = tokio::join!(f.source.prepare(control.clone()), async {
        entered.await.unwrap();
        control.cancel.cancel();
        release.send(()).unwrap();
    });
    let prepared = prepared.unwrap();
    assert!(
        matches!(prepared.state(), CoverState::Failed),
        "the actual IO cancellation stays explicit"
    );
    let alert = f.events.try_recv().unwrap();
    assert_eq!(alert.message.channel, "ALERT");
    let private = alert.native_cover.unwrap();
    f.publications
        .complete(&context, private["id"].as_str().unwrap(), true)
        .unwrap();
    prepared.finish().await.unwrap();
    assert_eq!((f.files.retained(), f.publications.retained()), (0, 0));
    assert!(f.events.try_recv().is_err());
}
