use crate::controlled::Controlled;
use crate::*;
use serde_json::json;
fn owner() -> Owner {
    Owner::new("runtime", 1, "/library", 1).unwrap()
}
fn devices(names: &[&str]) -> Vec<Device> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| Device {
            id: i.to_string(),
            name: (*n).into(),
        })
        .collect()
}
fn fixture(inputs: &[&str], outputs: &[&str]) -> (MidiService, Controlled) {
    let control = Controlled::new(devices(inputs), devices(outputs));
    let service = MidiService::with_backend(
        owner(),
        CancellationToken::new(),
        Config {
            shutdown_timeout: Duration::from_millis(20),
            ..Config::default()
        },
        Box::new(control.clone()),
    )
    .unwrap();
    (service, control)
}
async fn call(service: &MidiService, command: Command) -> Result<Reply> {
    service
        .begin(
            &owner(),
            command,
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )?
        .finish()
        .await
}
async fn until(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    while !test() {
        assert!(
            Instant::now() < end,
            "owned MIDI work did not reach controlled barrier"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
fn send(output: &str) -> Command {
    Command::Send(json!({"output":output,"values":{"channel":1,"note":60,"velocity":100}}))
}
fn listen(id: &str, input: &str) -> Command {
    Command::Listen(json!({"id":id,"input":input}))
}
#[test]
fn selection_keeps_jzz_splice_fallback_duplicates_and_native_caps() {
    let d = devices(&["A", "B", "A", "敬拜"]);
    assert_eq!(
        select_devices(&Selector::Json(json!(0)), &d).unwrap(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        select_devices(&Selector::Json(json!(1)), &d).unwrap(),
        vec![1, 2, 3]
    );
    assert_eq!(
        select_devices(&Selector::Json(json!("A")), &d).unwrap(),
        vec![0, 2]
    );
    assert_eq!(
        select_devices(
            &Selector::List(vec![
                Selector::Json(json!("敬拜")),
                Selector::Undefined,
                Selector::Json(json!("A"))
            ]),
            &d
        )
        .unwrap(),
        vec![3, 1, 0, 2]
    );
    assert!(select_devices(&Selector::Json(json!(vec![0; 33])), &d).is_err());
    assert!(Owner::new("runtime", 1, &"界".repeat(12000), 1).is_err());
}
#[tokio::test]
async fn real_owner_caches_outputs_after_open_before_packet_validation_and_keeps_them_until_stop() {
    let (service, control) = fixture(&[], &["X"]);
    assert_eq!(
        call(
            &service,
            Command::Send(json!({"output":"X","values":{"channel":17,"note":60,"velocity":100}}))
        )
        .await
        .err()
        .unwrap()
        .code,
        "midi_value"
    );
    call(&service, send("X")).await.unwrap();
    call(&service, send("X")).await.unwrap();
    assert_eq!(
        control.trace(),
        vec![
            json!(["open", "out", "X"]),
            json!(["send", "X", [144, 60, 100]]),
            json!(["send", "X", [144, 60, 100]])
        ]
    );
    assert_eq!(service.status().outputs, 1);
    call(&service, Command::Stop).await.unwrap();
    call(&service, send("X")).await.unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(control.trace().iter().filter(|v| v[0] == "open").count(), 2);
    assert_eq!(
        control.trace().iter().filter(|v| v[0] == "close").count(),
        2
    );
}
#[tokio::test]
async fn shared_input_fanout_duplicate_ids_close_failure_retry_and_stale_callback_are_real() {
    let (service, control) = fixture(&["A"], &[]);
    let mut events = service.events().unwrap();
    call(&service, listen("2", "A")).await.unwrap();
    call(&service, listen("1", "A")).await.unwrap();
    call(&service, listen("1", "missing")).await.unwrap();
    call(&service, listen("sentinel", "input")).await.unwrap();
    assert_eq!(service.status().inputs, 1);
    control.emit("0", &[0x90, 60, 0]);
    let a = events.recv().await.unwrap();
    let b = events.recv().await.unwrap();
    let values = [a, b]
        .into_iter()
        .map(|e| match e {
            Event::Midi { data, .. } => data.data,
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![
            json!({"id":"2","values":{"note":60,"velocity":0,"channel":1},"type":"noteoff"}),
            json!({"id":"1","values":{"note":60,"velocity":0,"channel":1},"type":"noteoff"})
        ]
    );
    let old = control.callback("0").unwrap();
    call(&service, Command::CloseInputs("2".into()))
        .await
        .unwrap();
    assert!(!control.trace().iter().any(|v| v[0] == "close"));
    control.fail_close("A");
    let reply = call(&service, Command::CloseInputs("1".into()))
        .await
        .unwrap();
    assert!(matches!(reply,Reply::Diagnostics(v)if v.len()==1));
    assert_eq!(service.status().inputs, 1);
    call(&service, Command::CloseInputs("1".into()))
        .await
        .unwrap();
    old(0, &[0x90, 1, 100]);
    tokio::task::yield_now().await;
    assert_eq!(events.pending(), 0);
    assert_eq!(service.status().inputs, 0);
    call(&service, listen("next", "A")).await.unwrap();
    control.emit("0", &[0xb0, 7, 101]);
    let event = events.recv().await.unwrap();
    assert!(matches!(event,Event::Midi{data,..}if data.data["id"]=="next"));
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn mtc_uses_real_shared_ordered_output_and_independent_listener_handle() {
    let (service, control) = fixture(&["A"], &["X", "Y"]);
    let mut events = service.events().unwrap();
    call(&service, send("X")).await.unwrap();
    call(
        &service,
        Command::MtcSend {
            selector: Selector::Json(json!("X")),
            time_ms: 12345.0,
            framerate: 24.0,
        },
    )
    .await
    .unwrap();
    call(
        &service,
        Command::MtcSend {
            selector: Selector::Json(json!("Y")),
            time_ms: 12346.0,
            framerate: 29.97,
        },
    )
    .await
    .unwrap();
    assert_eq!(service.status().outputs, 1);
    call(&service, Command::Stop).await.unwrap();
    assert_eq!(
        service.status().outputs,
        1,
        "ordinary stop cannot close retained MTC alias"
    );
    call(&service, Command::MtcStop).await.unwrap();
    assert_eq!(service.status().outputs, 0);
    call(&service, listen("midi", "A")).await.unwrap();
    let Reply::MtcListener(handle) = call(&service, Command::MtcListen(Selector::Json(json!("A"))))
        .await
        .unwrap()
    else {
        panic!()
    };
    control.emit("0", &[0xf1, 0x70]);
    let event = events.recv().await.unwrap();
    assert!(matches!(event,Event::Mtc{listener,bytes,..}if listener==handle&&bytes==[0xf1,0x70]));
    assert!(call(&service, Command::MtcClose(handle + 1)).await.is_err());
    call(&service, Command::MtcClose(handle)).await.unwrap();
    assert_eq!(service.status().inputs, 1);
    service.shutdown().await.unwrap();
    assert_eq!(control.trace().iter().filter(|v| v[0] == "open").count(), 2);
    assert_eq!(
        control.trace().iter().filter(|v| v[0] == "send").count(),
        17
    );
}
#[tokio::test]
async fn deferred_backend_owns_accepted_io_after_caller_drop_and_terminal_close_reports_timeout() {
    let (live_service, live_control) = fixture(&[], &["X"]);
    live_control.block("open");
    let abandoned = live_service
        .begin(
            &owner(),
            send("X"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| live_control.entered()).await;
    drop(abandoned);
    live_control.release();
    until(|| live_service.status().admitted == 0).await;
    assert_eq!(
        live_control.trace(),
        vec![
            json!(["open", "out", "X"]),
            json!(["send", "X", [144, 60, 100]])
        ],
        "accepted IO finishes on its actual owner after the requesting future is dropped"
    );
    live_service.shutdown().await.unwrap();
    let (service, control) = fixture(&[], &["X"]);
    control.block("open");
    let first = service
        .begin(
            &owner(),
            send("X"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    drop(first);
    let mut queued = vec![];
    for _ in 1..COMMAND_LIMIT {
        queued.push(
            service
                .begin(
                    &owner(),
                    send("X"),
                    CancellationToken::new(),
                    Instant::now() + Duration::from_secs(2),
                )
                .unwrap(),
        );
    }
    assert_eq!(
        service
            .begin(
                &owner(),
                send("X"),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(2)
            )
            .err()
            .unwrap()
            .code,
        "midi_busy"
    );
    assert_eq!(
        service.shutdown().await.err().unwrap().code,
        "midi_shutdown_timeout"
    );
    assert_eq!(service.status().admitted, COMMAND_LIMIT);
    assert!(!service.status().worker_finished);
    control.release();
    until(|| service.status().worker_finished).await;
    service.shutdown().await.unwrap();
    assert_eq!(service.status().admitted, 0);
    assert_eq!(
        control.trace(),
        vec![json!(["open", "out", "X"]), json!(["close", "out", "X"])]
    );
    for ticket in queued {
        assert_eq!(ticket.finish().await.err().unwrap().code, "midi_shutdown");
    }
}
#[tokio::test]
async fn stale_cancelled_and_expired_queued_commands_never_open_or_send() {
    let (service, control) = fixture(&[], &["X"]);
    assert_eq!(
        call(&service, Command::Listen(Value::Null))
            .await
            .err()
            .unwrap()
            .code,
        "midi_invalid"
    );
    assert!(control.trace().is_empty());
    let other = Owner::new("runtime", 2, "/library", 2).unwrap();
    assert_eq!(
        service
            .begin(
                &other,
                send("X"),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
        "stale_generation"
    );
    control.block("open");
    let accepted = service
        .begin(
            &owner(),
            send("X"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    let cancel = CancellationToken::new();
    let cancelled = service
        .begin(
            &owner(),
            send("X"),
            cancel.clone(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    cancel.cancel();
    let expired = service
        .begin(
            &owner(),
            send("X"),
            CancellationToken::new(),
            Instant::now() + Duration::from_millis(1),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(3)).await;
    control.release();
    accepted.finish().await.unwrap();
    assert_eq!(
        cancelled.finish().await.err().unwrap().code,
        "midi_cancelled"
    );
    assert_eq!(expired.finish().await.err().unwrap().code, "midi_deadline");
    assert_eq!(control.trace().iter().filter(|v| v[0] == "send").count(), 1);
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn callback_and_consumer_overflow_fail_closed_and_drain_before_explicit_new_owner() {
    let (service, control) = fixture(&["A"], &["X"]);
    let mut events = service.events().unwrap();
    call(&service, listen("id", "A")).await.unwrap();
    let old = control.callback("0").unwrap();
    control.block("open");
    let pending = service
        .begin(
            &owner(),
            send("X"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    for _ in 0..=EVENT_LIMIT {
        control.emit("0", &[0x90, 60, 100]);
    }
    assert!(service.status().closing);
    assert_eq!(events.recv().await.err().unwrap().code, "midi_shutdown");
    control.release();
    assert!(pending.finish().await.is_err());
    until(|| service.status().worker_finished).await;
    assert_eq!(
        service.shutdown().await.err().unwrap().code,
        "midi_input_overflow"
    );
    old(0, &[0x90, 60, 100]);
    assert_eq!(service.status().inputs, 0);
    drop(events);
    drop(service);
    let (service, control) = fixture(&["A"], &[]);
    let events = service.events().unwrap();
    call(&service, listen("new", "A")).await.unwrap();
    for count in 1..=EVENT_LIMIT {
        control.emit("0", &[0x90, 60, 100]);
        until(|| events.pending() == count).await;
    }
    control.emit("0", &[0x90, 60, 100]);
    until(|| service.status().worker_finished).await;
    assert_eq!(
        service.shutdown().await.err().unwrap().code,
        "midi_event_overflow"
    );
    assert_eq!(service.status().inputs, 0);
}
#[tokio::test]
async fn native_device_disappearance_is_observable_and_owner_cancellation_blocks_late_events() {
    let (service, control) = fixture(&["A"], &[]);
    call(&service, listen("id", "A")).await.unwrap();
    control.remove_inputs();
    until(|| service.status().worker_finished).await;
    assert_eq!(
        service.shutdown().await.err().unwrap().code,
        "midi_disconnected"
    );
    assert_eq!(service.status().inputs, 0);
    let cancel = CancellationToken::new();
    let control = Controlled::new(devices(&["A"]), vec![]);
    let service = MidiService::with_backend(
        owner(),
        cancel.clone(),
        Config::default(),
        Box::new(control.clone()),
    )
    .unwrap();
    let mut events = service.events().unwrap();
    call(&service, listen("id", "A")).await.unwrap();
    cancel.cancel();
    control.emit("0", &[0x90, 60, 100]);
    assert_eq!(events.recv().await.err().unwrap().code, "midi_shutdown");
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn confirmed_close_revokes_already_queued_events_and_deferred_close_retains_actual_handle() {
    let (service, control) = fixture(&["A"], &[]);
    let mut events = service.events().unwrap();
    call(&service, listen("old", "A")).await.unwrap();
    control.emit("0", &[0x90, 60, 100]);
    until(|| events.pending() == 1).await;
    call(&service, Command::CloseInputs("old".into()))
        .await
        .unwrap();
    call(&service, listen("new", "A")).await.unwrap();
    control.emit("0", &[0x90, 61, 100]);
    let event = events.recv().await.unwrap();
    assert!(matches!(event, Event::Midi { data, .. } if data.data["id"] == "new"));
    control.block("close");
    let close = service
        .begin(
            &owner(),
            Command::CloseInputs("new".into()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    drop(close);
    assert_eq!(
        service.shutdown().await.err().unwrap().code,
        "midi_shutdown_timeout"
    );
    assert_eq!(
        service.status().inputs,
        1,
        "a blocked actual close retains the owned handle"
    );
    control.release();
    until(|| service.status().worker_finished).await;
    service.shutdown().await.unwrap();
    assert_eq!(service.status().inputs, 0);
}

#[tokio::test]
async fn close_all_failure_keeps_untracked_handles_on_terminal_owner_until_cleanup() {
    let (service, control) = fixture(&["A"], &[]);
    call(&service, listen("id", "A")).await.unwrap();
    control.fail_close("A");
    let reply = call(&service, Command::CloseInputs("unknown".into()))
        .await
        .unwrap();
    assert!(matches!(reply, Reply::Diagnostics(errors) if errors.len() == 1));
    until(|| service.status().worker_finished).await;
    assert_eq!(
        service.shutdown().await.err().unwrap().code,
        "controlled_close"
    );
    assert_eq!(service.status().inputs, 0);
    assert_eq!(
        control.trace().iter().filter(|v| v[0] == "close").count(),
        2,
        "failed source close-all does not abandon the actual remaining handle"
    );
}

#[tokio::test]
async fn logical_admission_bound_precedes_physical_open_and_stop_releases_every_handle() {
    let (service, control) = fixture(&["A"], &["X", "Y"]);
    for index in 0..freeshow_midi::MAX_PORTS {
        call(&service, Command::Send(json!({"output":["X",index]})))
            .await
            .unwrap();
    }
    assert_eq!(service.status().outputs, 1);
    assert_eq!(
        call(&service, listen("id", "A")).await.err().unwrap().code,
        "midi_limit"
    );
    assert_eq!(
        call(&service, send("Y")).await.err().unwrap().code,
        "midi_limit"
    );
    assert_eq!(
        call(
            &service,
            Command::MtcSend {
                selector: Selector::Json(json!("Y")),
                time_ms: 0.0,
                framerate: 25.0
            }
        )
        .await
        .err()
        .unwrap()
        .code,
        "midi_limit"
    );
    assert_eq!(control.trace().iter().filter(|v| v[0] == "open").count(), 1);
    call(&service, Command::Stop).await.unwrap();
    assert_eq!(service.status().inputs, 0);
    assert_eq!(service.status().outputs, 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn numeric_close_ids_keep_source_truthiness_and_truthy_empty_keys_are_explicit() {
    let (service, control) = fixture(&["A", "B"], &[]);
    call(&service, Command::Listen(json!({"id":12,"input":"A"})))
        .await
        .unwrap();
    call(&service, listen("other", "B")).await.unwrap();
    call(
        &service,
        Command::close_inputs_field(Some(&json!(12))).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(service.status().inputs, 1);
    call(
        &service,
        Command::close_inputs_field(Some(&json!(0))).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(service.status().inputs, 0);
    assert_eq!(
        Command::close_inputs_field(Some(&json!([])))
            .err()
            .unwrap()
            .code,
        "midi_unsupported"
    );
    service.shutdown().await.unwrap();
    assert_eq!(
        control.trace().iter().filter(|v| v[0] == "close").count(),
        2
    );
}

#[tokio::test]
async fn callback_registration_fences_prevent_early_open_and_pre_listener_packets_from_leaking() {
    let (service, control) = fixture(&["A"], &["X", "Y"]);
    let mut events = service.events().unwrap();
    control.block("open");
    let opening = service
        .begin(
            &owner(),
            listen("old", "A"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    control.emit("0", &[0x90, 1, 100]);
    control.release();
    opening.finish().await.unwrap();
    control.emit("0", &[0x90, 2, 100]);
    let event = events.recv().await.unwrap();
    assert!(
        matches!(event,Event::Midi{data,..}if data.data["values"]["note"]==2),
        "callbacks before a successful input registration are dropped"
    );
    control.block("open");
    let opening = service
        .begin(
            &owner(),
            send("X"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    control.emit("0", &[0x90, 3, 100]);
    let listener = service
        .begin(
            &owner(),
            listen("new", "A"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    control.release();
    opening.finish().await.unwrap();
    listener.finish().await.unwrap();
    let event = events.recv().await.unwrap();
    assert!(
        matches!(event,Event::Midi{data,..}if data.data["id"]=="old"&&data.data["values"]["note"]==3)
    );
    control.emit("0", &[0x90, 4, 100]);
    for id in ["old", "new"] {
        let event = events.recv().await.unwrap();
        assert!(
            matches!(event,Event::Midi{data,..}if data.data["id"]==id&&data.data["values"]["note"]==4),
            "a newly registered listener cannot receive an older queued packet"
        );
    }
    control.block("open");
    let opening = service
        .begin(
            &owner(),
            send("Y"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| control.entered()).await;
    control.emit("0", &[0xf1, 0x70]);
    let listener = service
        .begin(
            &owner(),
            Command::MtcListen(Selector::Json(json!("A"))),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    control.release();
    opening.finish().await.unwrap();
    listener.finish().await.unwrap();
    control.emit("0", &[0xf1, 0x71]);
    let event = events.recv().await.unwrap();
    assert!(
        matches!(event,Event::Mtc{bytes,..}if bytes==[0xf1,0x71]),
        "MTC registration obeys the same callback sequence fence"
    );
    service.shutdown().await.unwrap();
}
