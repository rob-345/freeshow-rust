use super::*;
use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};
pub(crate) struct Gate {
    state: Mutex<(bool, bool)>,
    cv: Condvar,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((false, false)),
            cv: Condvar::new(),
        })
    }
    pub(crate) fn block(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.cv.notify_all();
        while !state.1 {
            state = self.cv.wait(state).unwrap()
        }
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.cv.notify_all()
    }
    async fn started(&self) {
        for _ in 0..200 {
            if self.state.lock().unwrap().0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await
        }
        panic!("Fixed worker did not start")
    }
}
fn context() -> Context {
    Context::from_host("native", 1, "/library").unwrap()
}
fn code<T>(r: Result<T>) -> &'static str {
    match r {
        Err(e) => e.code,
        Ok(_) => panic!("Expected explicit error"),
    }
}
fn png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
    let mut out = vec![];
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer
            .write_image_data(&[255, 30, 0, alpha].repeat((w * h) as usize))
            .unwrap()
    }
    out
}
async fn image(service: &CoverImageService, s: &Session) {
    let o = service
        .create(
            s,
            png(2, 1, 255),
            ImagePolicy::VerifiedPngPixels,
            Cancel::default(),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    o.inspect(|r| {
        assert!(matches!(
            r,
            Reply::Image {
                width: 2,
                height: 1
            }
        ))
    })
    .unwrap()
}
#[tokio::test]
async fn whole_session_and_ready_observation_quotas_cover_actual_images() {
    let service = CoverImageService::new().unwrap();
    let ctx = context();
    let sessions = (0..4)
        .map(|_| service.begin_session(&ctx).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(code(service.begin_session(&ctx)), "cover_busy");
    assert_eq!(service.status().reserved_bytes, 512 * 1024 * 1024);
    image(&service, &sessions[0]).await;
    let result = service
        .png(&sessions[0], Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(
        code(service.size(&sessions[0], Cancel::default())),
        "cover_session_busy"
    );
    result
        .inspect(|r| assert!(matches!(r,Reply::Bytes{data,..}if!data.is_empty())))
        .unwrap();
    drop(result);
    let out = service
        .release(&sessions[0])
        .unwrap()
        .finish()
        .await
        .unwrap();
    out.inspect(|r| assert!(matches!(r, Reply::Released)))
        .unwrap();
    drop(out);
    service
        .retire(&sessions[0], Duration::from_secs(1))
        .await
        .unwrap();
    assert!(service.begin_session(&ctx).is_ok());
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    assert_eq!(service.status().finished_workers, 2);
    assert_eq!(service.status().reserved_bytes, 0)
}
#[tokio::test]
async fn accepted_caller_drop_keeps_actual_worker_owned_and_terminal_timeout_retries() {
    let service = CoverImageService::new().unwrap();
    let s = service.begin_session(&context()).unwrap();
    let gate = Gate::new();
    let ticket = service.gate(&s, gate.clone()).unwrap();
    gate.started().await;
    drop(ticket);
    let future = service.shutdown(Duration::from_millis(1));
    assert_eq!(code(service.size(&s, Cancel::default())), "cover_closed");
    assert_eq!(code(future.await), "cover_drain_timeout");
    assert_eq!(service.status().active, 1);
    assert_ne!(service.status().finished_workers, 2);
    gate.release();
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    assert_eq!(service.status().active, 0);
    assert_eq!(service.status().finished_workers, 2)
}
#[tokio::test]
async fn retired_queued_job_is_really_removed_and_disposed_independent_of_blocked_workers() {
    let service = CoverImageService::new().unwrap();
    let c = context();
    let s1 = service.begin_session(&c).unwrap();
    let s2 = service.begin_session(&c).unwrap();
    let g1 = Gate::new();
    let g2 = Gate::new();
    let t1 = service.gate(&s1, g1.clone()).unwrap();
    let t2 = service.gate(&s2, g2.clone()).unwrap();
    g1.started().await;
    g2.started().await;
    let queued = service.begin_session(&c).unwrap();
    let ticket = service
        .create(
            &queued,
            png(640, 480, 255),
            ImagePolicy::VerifiedPngPixels,
            Cancel::default(),
        )
        .unwrap();
    assert_eq!(service.status().queued, 1);
    let retire = service.retire(&queued, Duration::from_millis(100));
    assert_eq!(service.status().queued, 0);
    retire.await.unwrap();
    assert_eq!(code(ticket.finish().await), "cover_cancelled");
    assert_eq!(
        code(service.size(&queued, Cancel::default())),
        "cover_closed"
    );
    g1.release();
    g2.release();
    drop((t1, t2));
    service.shutdown(Duration::from_secs(1)).await.unwrap()
}
#[tokio::test]
async fn cancellation_after_ready_destroys_private_result_and_live_image() {
    let service = CoverImageService::new().unwrap();
    let s = service.begin_session(&context()).unwrap();
    image(&service, &s).await;
    let observation = service
        .png(&s, Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    service.retire(&s, Duration::from_secs(1)).await.unwrap();
    assert_eq!(code(observation.inspect(|_| ())), "cover_cancelled");
    assert_eq!(service.status().reserved_bytes, 0);
    service.shutdown(Duration::from_secs(1)).await.unwrap()
}
#[tokio::test]
async fn foreign_runs_services_and_missing_files_are_explicit() {
    let service = CoverImageService::new().unwrap();
    let c = context();
    let s = service.begin_session(&c).unwrap();
    let foreign = context();
    let path =
        TrustedMediaFile::from_host(&foreign, std::env::temp_dir().join("missing-cover")).unwrap();
    assert_eq!(
        code(service.covers(&s, path, Cancel::default())),
        "cover_context"
    );
    let other = CoverImageService::new().unwrap();
    assert_eq!(code(other.size(&s, Cancel::default())), "cover_authority");
    let path = TrustedMediaFile::from_host(
        &c,
        std::env::temp_dir().join("freeshow-definitely-missing-cover-file"),
    )
    .unwrap();
    let out = service
        .covers(&s, path, Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(code(out.inspect(|_| ())), "cover_io");
    drop(out);
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    other.shutdown(Duration::from_secs(1)).await.unwrap()
}
#[tokio::test]
async fn malformed_and_allocation_bombs_fail_before_decode_and_cancellation_is_not_empty_success() {
    let service = CoverImageService::new().unwrap();
    let s = service.begin_session(&context()).unwrap();
    let mut bad = png(1, 1, 255);
    bad[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    let out = service
        .create(&s, bad, ImagePolicy::VerifiedPngPixels, Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(code(out.inspect(|_| ())), "cover_resource_limit");
    drop(out);
    let out = service
        .create(
            &s,
            png(1, 1, 128),
            ImagePolicy::VerifiedPngPixels,
            Cancel::default(),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(code(out.inspect(|_| ())), "cover_unsupported");
    drop(out);
    let cancel = Cancel::default();
    cancel.cancel();
    let out = service
        .create(&s, png(1, 1, 255), ImagePolicy::VerifiedPngPixels, cancel)
        .unwrap();
    assert_eq!(code(out.finish().await), "cover_cancelled");
    service.shutdown(Duration::from_secs(1)).await.unwrap()
}
#[tokio::test]
async fn native_resize_is_opt_in_and_zero_height_matches_source_empty_image() {
    let service = CoverImageService::new().unwrap();
    let s = service.begin_session(&context()).unwrap();
    drop(
        service
            .create(
                &s,
                png(641, 1, 255),
                ImagePolicy::VerifiedPngPixels,
                Cancel::default(),
            )
            .unwrap()
            .finish()
            .await
            .unwrap(),
    );
    let out = service
        .resize_jpeg90(&s, Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(code(out.inspect(|_| ())), "cover_unsupported");
    drop(out);
    drop(
        service
            .create(
                &s,
                png(2000, 1, 255),
                ImagePolicy::VerifiedPngPixels,
                Cancel::default(),
            )
            .unwrap()
            .finish()
            .await
            .unwrap(),
    );
    let out = service
        .resize_jpeg90(&s, Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    out.inspect(|r| assert!(matches!(r,Reply::Bytes{data,..}if data.is_empty())))
        .unwrap();
    drop(out);
    service.shutdown(Duration::from_secs(1)).await.unwrap()
}
#[tokio::test]
async fn operation_budget_bounds_even_empty_native_commands() {
    let service = CoverImageService::new().unwrap();
    let s = service.begin_session(&context()).unwrap();
    for _ in 0..MAX_OPERATIONS {
        drop(service.release(&s).unwrap().finish().await.unwrap())
    }
    assert_eq!(code(service.release(&s)), "cover_resource_limit");
    service.shutdown(Duration::from_secs(1)).await.unwrap()
}

#[tokio::test]
async fn actual_clock_deadline_rejects_without_empty_success() {
    let service = CoverImageService::with_operation_timeout(Duration::from_nanos(1)).unwrap();
    let session = service.begin_session(&context()).unwrap();
    let out = service
        .create(
            &session,
            png(2, 1, 255),
            ImagePolicy::VerifiedPngPixels,
            Cancel::default(),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(code(out.inspect(|_| ())), "cover_deadline");
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn actual_fifo_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let dir = std::env::temp_dir().join(format!("freeshow-cover-fifo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("audio.mp3");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // Test fixture only; production uses safe OpenOptions O_NONBLOCK, then fstat.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let service = CoverImageService::new().unwrap();
    let ctx = context();
    let s = service.begin_session(&ctx).unwrap();
    let out = service
        .covers(
            &s,
            TrustedMediaFile::from_host(&ctx, &path).unwrap(),
            Cancel::default(),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(code(out.inspect(|_| ())), "cover_unsupported");
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn worker_panic_is_sticky_but_both_actual_owners_still_join_and_cleanup() {
    let service = CoverImageService::new().unwrap();
    let session = service.begin_session(&context()).unwrap();
    image(&service, &session).await;
    let out = service.panic_command(&session).unwrap();
    assert_eq!(code(out.finish().await), "cover_cancelled");
    assert_eq!(
        code(service.shutdown(Duration::from_secs(1)).await),
        "cover_worker_failed"
    );
    assert_eq!(service.status().finished_workers, 2);
    assert_eq!(service.status().reserved_bytes, 0);
    assert_eq!(service.status().failures.len(), 1);
    assert_eq!(
        code(service.shutdown(Duration::from_secs(1)).await),
        "cover_worker_failed"
    );
}

#[tokio::test]
async fn late_cancel_rejects_ready_publication_and_foreign_retire_cannot_close_owner() {
    let service = CoverImageService::new().unwrap();
    let other = CoverImageService::new().unwrap();
    let session = service.begin_session(&context()).unwrap();
    assert_eq!(
        code(other.retire(&session, Duration::from_secs(1)).await),
        "cover_authority"
    );
    let cancel = Cancel::default();
    let out = service
        .create(
            &session,
            png(2, 1, 255),
            ImagePolicy::VerifiedPngPixels,
            cancel.clone(),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    cancel.cancel();
    assert_eq!(code(out.inspect(|_| ())), "cover_cancelled");
    drop(out);
    drop(service.release(&session).unwrap().finish().await.unwrap());
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    other.shutdown(Duration::from_secs(1)).await.unwrap();
}
#[tokio::test]
async fn held_authorized_file_preserves_identity_and_resets_exclusive_cursor() {
    use std::io::{Seek, SeekFrom};
    let folder = std::env::temp_dir().join(format!("freeshow-held-cover-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("media音楽.wav");
    let mut wav = Vec::from(&b"RIFF"[..]);
    wav.extend_from_slice(&36u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[1, 0, 1, 0, 64, 31, 0, 0, 128, 62, 0, 0, 2, 0, 16, 0]);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, wav).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    let ctx = context();
    let trusted = TrustedMediaFile::from_opened_host(&ctx, &path, file).unwrap();
    std::fs::rename(&path, folder.join("original.wav")).unwrap();
    std::fs::write(&path, b"not original media").unwrap();
    let service = CoverImageService::new().unwrap();
    let session = service.begin_session(&ctx).unwrap();
    let observation = service
        .covers(&session, trusted, Cancel::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    observation
        .inspect(|r| assert!(matches!(r,Reply::Covers(p)if p.is_empty())))
        .unwrap();
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn unexpected_owner_panic_requires_actual_join_then_releases_real_image() {
    let service = CoverImageService::new().unwrap();
    let session = service.begin_session(&context()).unwrap();
    image(&service, &session).await;
    let ticket = service.panic_outside(&session).unwrap();
    assert_eq!(code(ticket.finish().await), "cover_worker_failed");
    assert_eq!(
        code(service.shutdown(Duration::from_secs(1)).await),
        "cover_worker_failed"
    );
    assert_eq!(service.status().finished_workers, 2);
    assert_eq!(service.status().active, 0);
    assert_eq!(service.status().reserved_bytes, 0);
}

#[tokio::test]
async fn concurrent_submission_and_retirement_leave_no_queued_payload_or_released_live_lease() {
    let service = CoverImageService::new().unwrap();
    let ctx = context();
    let a = service.begin_session(&ctx).unwrap();
    let b = service.begin_session(&ctx).unwrap();
    let g1 = Gate::new();
    let g2 = Gate::new();
    let t1 = service.gate(&a, g1.clone()).unwrap();
    let t2 = service.gate(&b, g2.clone()).unwrap();
    g1.started().await;
    g2.started().await;
    let session = service.begin_session(&ctx).unwrap();
    let bytes = png(640, 480, 255);
    let (submitted, retired) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            service.create(
                &session,
                bytes,
                ImagePolicy::VerifiedPngPixels,
                Cancel::default(),
            )
        });
        let second = scope.spawn(|| service.retire(&session, Duration::from_secs(1)));
        (first.join().unwrap(), second.join().unwrap())
    });
    retired.await.unwrap();
    match submitted {
        Ok(ticket) => assert_eq!(code(ticket.finish().await), "cover_cancelled"),
        Err(e) => assert_eq!(e.code, "cover_closed"),
    }
    assert_eq!(service.status().queued, 0);
    assert_eq!(service.status().sessions, 2);
    g1.release();
    g2.release();
    drop((t1, t2));
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}
