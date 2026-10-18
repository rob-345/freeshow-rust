use super::*;
use serde_json::json;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Wake, Waker},
};

// This executor has no Tokio runtime, proving service shutdown does not rely on one.
fn block_on<F: Future>(future: F) -> F::Output {
    struct WakeThread(std::thread::Thread);
    impl Wake for WakeThread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(WakeThread(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park_timeout(Duration::from_secs(5)),
        }
    }
}
#[test]
fn service_defaults_and_shutdown_work_without_a_tokio_runtime() {
    let service = AudioService::new(AudioConfig::default()).unwrap();
    for path in [Value::Null, json!(false), json!("")] {
        let reply =
            block_on(service.dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))).unwrap();
        assert_eq!(reply.data, Value::Null);
        assert!(reply.diagnostics.is_empty());
    }
    assert!(matches!(
        block_on(service.dispatch("UNKNOWN", json!({}))),
        Err(AudioError::Unsupported(_))
    ));
    assert!(matches!(
        block_on(service.dispatch("READ_AUDIO_METADATA", Value::Null)),
        Err(AudioError::Invalid)
    ));
    for path in ["x".repeat(32769), "a\0b".into()] {
        assert!(matches!(
            block_on(service.dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))),
            Err(AudioError::Invalid)
        ));
    }
    block_on(service.shutdown());
    assert_eq!(
        service.inner.completion.remaining.load(Ordering::Acquire),
        0
    );
    assert!(matches!(
        block_on(service.dispatch("READ_AUDIO_METADATA", json!({}))),
        Err(AudioError::Shutdown)
    ));
}
#[tokio::test]
async fn late_admission_cannot_overtake_completed_shutdown() {
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let (check_tx, check_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let late = async {
        assert!(!service.inner.cancel.is_cancelled());
        check_tx.send(()).unwrap();
        resume_rx.await.unwrap();
        service.inner.acquire_job()
    };
    let stop = async {
        check_rx.await.unwrap();
        service.shutdown().await;
        resume_tx.send(()).unwrap();
    };
    let (admission, ()) = tokio::join!(late, stop);
    assert!(matches!(admission, Err(AudioError::Shutdown)));
    assert_eq!(service.inner.permits.available_permits(), 2);
}
#[tokio::test]
async fn shutdown_waits_for_admitted_native_work_and_excess_admission_is_busy() {
    let service = AudioService::new(AudioConfig {
        max_jobs: 1,
        ..AudioConfig::default()
    })
    .unwrap();
    let admitted = service.inner.acquire_job().unwrap();
    assert!(matches!(
        service
            .dispatch("READ_AUDIO_METADATA", json!({"filePath":"missing"}))
            .await,
        Err(AudioError::Busy)
    ));
    let mut shutdown = Box::pin(service.shutdown());
    tokio::select! {biased;_= &mut shutdown=>panic!("shutdown returned while native work still held a permit"),_=tokio::task::yield_now()=>{}}
    assert!(service.inner.cancel.is_cancelled());
    assert!(matches!(
        service.inner.acquire_job(),
        Err(AudioError::Shutdown)
    ));
    drop(admitted);
    tokio::time::timeout(Duration::from_secs(1), shutdown)
        .await
        .unwrap();
}
#[test]
fn output_and_configuration_limits_are_enforced() {
    assert!(within_limit(&json!({"x":1}), 7));
    assert!(!within_limit(&json!({"x":1}), 6));
    for max_jobs in [0, 9] {
        assert!(matches!(
            AudioService::new(AudioConfig {
                max_jobs,
                ..AudioConfig::default()
            }),
            Err(AudioError::Invalid)
        ));
    }
    assert!(matches!(
        AudioService::new(AudioConfig {
            read_limit: 0,
            ..AudioConfig::default()
        }),
        Err(AudioError::Invalid)
    ));
}

#[test]
fn real_file_parser_defaults_and_malformed_declarations_need_no_ffmpeg() {
    let directory = std::env::temp_dir().join(format!(
        "freeshow-audio-unit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(8000u32.to_le_bytes());
    wav.extend(16000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data\x02\0\0\0\0\0LIST\x16\0\0\0INFOINAM\x09\0\0\0Portable\0\0");
    let size = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&size.to_le_bytes());
    let path = directory.join("portable.wav");
    std::fs::write(&path, wav).unwrap();
    let reply =
        block_on(service.dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))).unwrap();
    assert!(reply.diagnostics.is_empty());
    assert_eq!(
        reply.data,
        json!({"common":{"track":{"no":null,"of":null},"disk":{"no":null,"of":null},"movementIndex":{},"title":"Portable"}})
    );
    let id3 = vec![73, 68, 51, 4, 0, 0, 127, 127, 127, 127];
    let mut flac = b"fLaC\0\0\0\x22".to_vec();
    flac.extend([0; 34]);
    flac.extend([0x84, 0, 0, 8, 0, 0, 0, 0, 255, 255, 255, 255]);
    let oversized_wav = b"RIFF\xff\xff\xff\x7fWAVELIST\xff\xff\xff\x7fINFO".to_vec();
    fn page(packet: &[u8], sequence: u32) -> Vec<u8> {
        let mut header = vec![0; 28];
        header[..4].copy_from_slice(b"OggS");
        header[5] = if sequence == 0 { 2 } else { 0 };
        header[14..18].copy_from_slice(&1u32.to_le_bytes());
        header[18..22].copy_from_slice(&sequence.to_le_bytes());
        header[26] = 1;
        header[27] = packet.len() as u8;
        header.extend(packet);
        header
    }
    let mut opus = page(b"OpusHead\x01\x01\0\0\x80\xbb\0\0\0\0\0", 0);
    opus.extend(page(b"OpusTags\0\0\0\0\xff\xff\xff\xff", 1));
    for (name, bytes) in [
        ("declared.mp3", id3),
        ("declared.flac", flac),
        ("declared.wav", oversized_wav),
        ("declared.opus", opus),
    ] {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        let reply =
            block_on(service.dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))).unwrap();
        assert_eq!(reply.data, Value::Null, "{name}");
        assert_eq!(
            reply.diagnostics.first().map(|d| d.code.as_str()),
            Some("audio_resource_limit"),
            "{name}: {reply:?}"
        );
    }
    block_on(service.shutdown());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn mp4_recursive_freeform_and_wav_embedded_id3_declarations_are_rejected() {
    fn atom(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend(id);
        bytes.extend(body);
        bytes
    }
    let mut nested = atom(b"mean", b"\0\0\0\0name");
    for _ in 0..128 {
        nested = atom(b"----", &nested);
    }
    let ilst = atom(b"ilst", &nested);
    let meta = atom(b"meta", &[&[0; 4][..], &ilst].concat());
    let moov = atom(b"moov", &atom(b"udta", &meta));
    let mp4 = [atom(b"ftyp", b"M4A \0\0\0\0isom"), moov].concat();
    let declared_id3 = [73, 68, 51, 4, 0, 0, 127, 127, 127, 127];
    let mut wav = b"RIFF\x16\0\0\0WAVEid3 \x0a\0\0\0".to_vec();
    wav.extend(declared_id3);
    let directory =
        std::env::temp_dir().join(format!("freeshow-audio-nested-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    for (name, bytes, detail) in [
        ("nested.m4a", mp4, "nesting depth"),
        ("embedded.wav", wav, "embedded ID3 tag size"),
    ] {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        let reply =
            block_on(service.dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))).unwrap();
        assert_eq!(reply.data, Value::Null);
        assert_eq!(reply.diagnostics[0].code, "audio_resource_limit");
        assert!(reply.diagnostics[0].message.contains(detail), "{reply:?}");
    }
    block_on(service.shutdown());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn flac_cover_block_is_seeked_without_allocation_or_reading_picture_bytes() {
    let path = std::env::temp_dir().join(format!(
        "freeshow-audio-picture-{}.flac",
        std::process::id()
    ));
    let mut bytes = b"fLaC\0\0\0\x22".to_vec();
    bytes.extend([0; 34]);
    bytes.extend([0x86, 0x40, 0, 0]);
    let mut file = std::fs::File::create(&path).unwrap();
    std::io::Write::write_all(&mut file, &bytes).unwrap();
    file.set_len((bytes.len() + 4 * 1024 * 1024) as u64)
        .unwrap();
    drop(file);
    let service = AudioService::new(AudioConfig {
        read_limit: 512,
        allocation_limit: 128,
        ..AudioConfig::default()
    })
    .unwrap();
    let reply =
        block_on(service.dispatch("READ_AUDIO_METADATA", json!({"filePath":path}))).unwrap();
    assert!(reply.diagnostics.is_empty(), "{reply:?}");
    assert_eq!(
        reply.data,
        json!({"common":{"track":{"no":null,"of":null},"disk":{"no":null,"of":null},"movementIndex":{}}})
    );
    block_on(service.shutdown());
    std::fs::remove_file(path).unwrap();
}
