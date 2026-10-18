use super::*;
use serde_json::json;
fn budget(config: ThumbnailConfig) -> Budget {
    Budget {
        config,
        cancel: CancellationToken::new(),
        request_cancel: CancellationToken::new(),
        started: Instant::now(),
    }
}
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut output = vec![];
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&vec![128; width as usize * height as usize * 4])
            .unwrap();
    }
    output
}
#[test]
fn hash_and_config() {
    assert_eq!(file_path_hash_code(""), "a0");
    assert_eq!(file_path_hash_code("polygenelubricants"), "i2147483648");
    for workers in [0, 9] {
        assert!(ThumbnailService::new(ThumbnailConfig {
            workers,
            ..ThumbnailConfig::default()
        })
        .is_err());
    }
}
#[test]
fn png_preflight_and_row_limits() {
    let b = budget(ThumbnailConfig::default());
    let bytes = png(3, 2);
    let decoded = png_data::validate(&bytes, &b).unwrap();
    assert_eq!((decoded.width, decoded.height), (3, 2));
    let mut hostile = bytes.clone();
    hostile[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        png_data::validate(&hostile, &b).err().unwrap().code,
        "thumbnail_resource_limit"
    );
    let mut hostile = bytes.clone();
    hostile[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        png_data::validate(&hostile, &b).err().unwrap().code,
        "invalid_canvas_png"
    );
    let mut inflation = bytes.clone();
    inflation[16..20].copy_from_slice(&1_u32.to_be_bytes());
    inflation[20..24].copy_from_slice(&1_u32.to_be_bytes());
    assert_eq!(
        png_data::validate(&inflation, &b).err().unwrap().code,
        "thumbnail_resource_limit"
    );
    for (offset, value) in [(24, 16), (25, 3), (28, 1)] {
        let mut variant = bytes.clone();
        variant[offset] = value;
        assert_eq!(
            png_data::validate(&variant, &b).err().unwrap().code,
            "unsupported_canvas_png"
        );
    }
    let mut bad_crc = bytes.clone();
    bad_crc[29] ^= 1;
    assert!(png_data::validate(&bad_crc, &b).is_err());
    let b = budget(ThumbnailConfig {
        chunks: 1,
        ..ThumbnailConfig::default()
    });
    assert_eq!(
        png_data::validate(&bytes, &b).err().unwrap().code,
        "thumbnail_resource_limit"
    );
}
#[tokio::test]
async fn validation_capacity_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let roots = ThumbnailRoots {
        cache_root: temp.path().into(),
    };
    let service = ThumbnailService::new(ThumbnailConfig {
        max_prepared: 1,
        ..ThumbnailConfig::default()
    })
    .unwrap();
    for (command, data, code) in [
        ("UNKNOWN", json!({}), "unsupported_command"),
        (
            "GET_THUMBNAIL",
            json!({"input":"x","size":-1}),
            "unsupported_size",
        ),
        (
            "GET_THUMBNAIL",
            json!({"input":"x","size":4097}),
            "thumbnail_resource_limit",
        ),
        (
            "DOES_MEDIA_EXIST",
            json!({"path":"bad\0path"}),
            "thumbnail_resource_limit",
        ),
    ] {
        assert_eq!(
            service
                .prepare(command, &data, &roots, CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code,
            code
        );
    }
    if !cfg!(target_os = "linux") {
        service.shutdown().await.unwrap();
        return;
    }
    let plan = service
        .prepare(
            "GET_THUMBNAIL",
            &json!({"input":"missing"}),
            &roots,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .prepare(
                "GET_THUMBNAIL",
                &json!({"input":"missing"}),
                &roots,
                CancellationToken::new()
            )
            .await
            .err()
            .unwrap()
            .code,
        "thumbnail_busy"
    );
    service.shutdown().await.unwrap();
    assert!(plan.ensure_current().is_err());
    drop(plan);
    assert_eq!(
        service
            .prepare(
                "GET_THUMBNAIL",
                &json!({"input":"missing"}),
                &roots,
                CancellationToken::new()
            )
            .await
            .err()
            .unwrap()
            .code,
        "thumbnail_shutdown"
    );
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stage_completion_identity_cancel_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.png");
    std::fs::write(&input, b"input").unwrap();
    let roots = ThumbnailRoots {
        cache_root: temp.path().join("cache"),
    };
    let service = ThumbnailService::new(ThumbnailConfig {
        staging_root: Some(temp.path().into()),
        ..ThumbnailConfig::default()
    })
    .unwrap();
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    let id = plan.capture.as_ref().unwrap().id.clone();
    let stage = plan.stage.as_ref().unwrap().path().to_owned();
    let plan = service
        .complete(plan, &id, CaptureCompletion::Png(&png(3, 2)))
        .await
        .unwrap();
    assert_eq!(plan.outcome, ThumbnailOutcome::GeneratedPng);
    assert_eq!(plan.effects.len(), 1);
    assert!(!roots.cache_root.exists());
    drop(plan);
    service.inner.cleanup.fence().await.unwrap();
    assert!(!stage.exists());
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        service
            .complete(plan, "wrong", CaptureCompletion::Failed)
            .await
            .err()
            .unwrap()
            .code,
        "invalid_capture_completion"
    );
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    let id = plan.capture.as_ref().unwrap().id.clone();
    std::fs::write(&input, b"changed input").unwrap();
    assert_eq!(
        service
            .complete(plan, &id, CaptureCompletion::Png(&png(3, 2)))
            .await
            .err()
            .unwrap()
            .code,
        "source_changed"
    );
    let cancel = CancellationToken::new();
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, cancel.clone())
        .await
        .unwrap();
    let stage = plan.stage.as_ref().unwrap().path().to_owned();
    cancel.cancel();
    assert_eq!(
        service
            .complete(plan, "irrelevant", CaptureCompletion::Failed)
            .await
            .err()
            .unwrap()
            .code,
        "cancelled"
    );
    service.shutdown().await.unwrap();
    assert!(!stage.exists());
}
#[tokio::test(flavor = "current_thread")]
async fn shutdown_admission_races() {
    for after in [false, true] {
        let service = ThumbnailService::new(ThumbnailConfig::default()).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let roots = ThumbnailRoots {
            cache_root: temp.path().into(),
        };
        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
        let e = entered.clone();
        let r = resume.clone();
        let hook = Box::new(move || {
            e.wait();
            r.wait();
        }) as Box<dyn FnOnce() + Send>;
        if after {
            *service.inner.after.lock().unwrap() = Some(hook);
        } else {
            *service.inner.before.lock().unwrap() = Some(hook);
        }
        let worker = service.clone();
        let dispatch = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(worker.prepare(
                    "GET_THUMBNAIL",
                    &json!({"input":"missing"}),
                    &roots,
                    CancellationToken::new(),
                ))
        });
        entered.wait();
        let shutdown = service.shutdown();
        tokio::pin!(shutdown);
        if after {
            tokio::select! {_=&mut shutdown=>panic!("held permit ignored"),_=tokio::task::yield_now()=>{}}
            resume.wait();
            shutdown.await.unwrap();
        } else {
            shutdown.await.unwrap();
            resume.wait();
        }
        assert_eq!(
            dispatch.join().unwrap().err().unwrap().code,
            "thumbnail_shutdown"
        );
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn retained_capture_shutdown_foreign_and_expired_completions() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.png");
    std::fs::write(&input, b"fixture").unwrap();
    let roots = ThumbnailRoots {
        cache_root: dir.path().join("cache"),
    };
    let service = ThumbnailService::new(ThumbnailConfig {
        staging_root: Some(dir.path().into()),
        ..ThumbnailConfig::default()
    })
    .unwrap();
    let other = ThumbnailService::new(ThumbnailConfig::default()).unwrap();
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    let id = plan.capture.as_ref().unwrap().id.clone();
    assert_eq!(
        other
            .complete(plan, &id, CaptureCompletion::Png(&png(3, 2)))
            .await
            .err()
            .unwrap()
            .code,
        "invalid_capture_completion"
    );
    other.shutdown().await.unwrap();
    let mut plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    let id = plan.capture.as_ref().unwrap().id.clone();
    plan.created = Instant::now() - Duration::from_secs(16);
    assert_eq!(
        service
            .complete(plan, &id, CaptureCompletion::Png(&png(3, 2)))
            .await
            .err()
            .unwrap()
            .code,
        "capture_expired"
    );
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    let stage = plan.stage.as_ref().unwrap().path().to_owned();
    tokio::time::timeout(Duration::from_secs(1), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(plan.ensure_current().is_err());
    assert!(stage.exists());
    drop(plan);
    service.shutdown().await.unwrap();
    assert!(!stage.exists());
}
#[test]
fn deadline_cancel_and_deep_json_limits() {
    let mut b = budget(ThumbnailConfig::default());
    b.cancel.cancel();
    assert_eq!(
        png_data::validate(&png(2, 2), &b).err().unwrap().code,
        "cancelled"
    );
    b = budget(ThumbnailConfig::default());
    b.started = Instant::now() - Duration::from_secs(20);
    assert_eq!(
        png_data::validate(&png(2, 2), &b).err().unwrap().code,
        "thumbnail_resource_limit"
    );
    let dir = tempfile::tempdir().unwrap();
    let roots = ThumbnailRoots {
        cache_root: dir.path().into(),
    };
    let mut nested = json!(null);
    for _ in 0..66 {
        nested = json!([nested]);
    }
    assert_eq!(
        prepare::validate(
            "GET_THUMBNAIL",
            &json!({"input":"x","future":nested}),
            &roots,
            &ThumbnailConfig::default()
        )
        .err()
        .unwrap()
        .code,
        "thumbnail_resource_limit"
    );
}
#[test]
fn shutdown_without_runtime_does_not_panic() {
    use std::{
        future::Future,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let service = ThumbnailService::new(ThumbnailConfig::default()).unwrap();
    let mut future = Box::pin(service.shutdown());
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    for _ in 0..1000 {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => {
                if let Err(error) = result {
                    assert_eq!(error.code, "runtime_required");
                }
                return;
            }
            Poll::Pending => std::thread::park_timeout(Duration::from_millis(1)),
        }
    }
    panic!("Shutdown failed to finish");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn capture_input_and_completion_dimension_caps() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.png");
    std::fs::write(&input, b"1234").unwrap();
    let roots = ThumbnailRoots {
        cache_root: dir.path().join("cache"),
    };
    let service = ThumbnailService::new(ThumbnailConfig {
        input_bytes: 3,
        ..ThumbnailConfig::default()
    })
    .unwrap();
    assert_eq!(
        service
            .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "thumbnail_resource_limit"
    );
    service.shutdown().await.unwrap();
    let service = ThumbnailService::new(ThumbnailConfig::default()).unwrap();
    let plan = service
        .prepare_capture(input.to_str().unwrap(), 3, &roots, CancellationToken::new())
        .await
        .unwrap();
    let id = plan.capture.as_ref().unwrap().id.clone();
    assert_eq!(
        service
            .complete(plan, &id, CaptureCompletion::Png(&png(2, 2)))
            .await
            .err()
            .unwrap()
            .code,
        "invalid_capture_completion"
    );
    service.shutdown().await.unwrap();
}
