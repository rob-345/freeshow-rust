use super::*;
fn start(router: &mut Router, kind: &str, mode: &str, fps: f64) -> Step {
    router
        .begin(Command::Start {
            kind: kind.into(),
            mode: mode.into(),
            framerate: Scalar::number(fps),
            data: Data::Undefined,
        })
        .unwrap()
}
fn token(step: &Step) -> EffectToken {
    step.effects[0].token.clone().unwrap()
}
#[test]
fn persistence_truthiness_status_and_no_ack() {
    let mut r = Router::new(false);
    assert!(start(&mut r, "send", "MTC", 30.0).effects.is_empty());
    start(&mut r, "ignored", "ableton", 0.0);
    assert_eq!(r.snapshot().framerate, Scalar::number(30.0));
    let s = r.begin(Command::Stop).unwrap();
    r.complete(&token(&s), Completion::Done).unwrap();
    assert_eq!(r.snapshot().current_mode.as_deref(), Some("MTC"));
    let value = r.begin(Command::Value(Number::new(5.0))).unwrap();
    assert!(matches!(value.effects[0].effect, Effect::SendMtc { .. }));
    r.complete(&token(&value), Completion::Done).unwrap();
    assert_eq!(r.update_status(Status::Play).effects.len(), 1);
    assert!(r.update_status(Status::Play).effects.is_empty());
    assert_eq!(r.process_frame(Number::NaN).effects.len(), 1);
    assert_eq!(r.process_frame(Number::NaN).effects.len(), 1);
}
#[test]
fn late_duplicate_opens_are_owned_across_stop_overwrite_and_retirement() {
    let mut r = Router::new(false);
    let a = start(&mut r, "receive", "MTC", 25.0);
    let b = start(&mut r, "receive", "MTC", 30.0);
    let accepted = r.accept_listener(&token(&a)).unwrap();
    assert!(r.accept_listener(&token(&a)).is_err());
    assert_eq!(
        r.mtc_message(&accepted, &[0xfa], 100.0)
            .unwrap()
            .effects
            .len(),
        2
    );
    assert!(r.begin(Command::Stop).unwrap().effects.is_empty());
    r.complete(&token(&b), Completion::Opened).unwrap();
    r.complete(&token(&a), Completion::Opened).unwrap();
    assert_eq!(r.snapshot().receivers, vec!["MTC"]);
    assert_eq!(r.snapshot().owned_listeners, 2);
    assert!(start(&mut r, "receive", "MTC", 60.0).effects.is_empty());
    let mut foreign = Router::new(false);
    assert!(foreign.complete(&token(&a), Completion::Opened).is_err());
    assert!(r.complete(&token(&a), Completion::Opened).is_err());
    let retiring = r.retire().unwrap();
    assert_eq!(retiring.effects.len(), 2);
    for e in retiring.effects {
        r.complete(&e.token.unwrap(), Completion::Done).unwrap();
    }
    assert!(r.drained());
}
#[test]
fn bounded_async_work_retirement_at_full_capacity_never_drops_handles() {
    let mut r = Router::new(false);
    let mut opens = vec![];
    for _ in 0..MAX_LISTENERS {
        opens.push(start(&mut r, "receive", "MTC", 25.0));
    }
    assert!(r
        .begin(Command::Start {
            kind: "receive".into(),
            mode: "MTC".into(),
            framerate: Scalar::Undefined,
            data: Data::Undefined
        })
        .is_err());
    let cancel = r.retire().unwrap();
    assert_eq!(cancel.effects.len(), MAX_PENDING);
    for open in opens {
        let close = r.complete(&token(&open), Completion::Opened).unwrap();
        assert_eq!(close.effects.len(), 1);
        r.complete(&token(&close), Completion::Done).unwrap();
    }
    assert!(r.drained());
}
#[test]
fn sticky_constructor_failure_and_exact_alert_do_not_claim_codec() {
    for windows in [false, true] {
        let mut r = Router::new(windows);
        let step = start(&mut r, "receive", "LTC", 24.0);
        let fail = r
            .complete(
                &token(&step),
                Completion::Failed(Error::native("unavailable")),
            )
            .unwrap();
        let Effect::Alert { message } = &fail.effects[0].effect else {
            panic!()
        };
        assert_eq!(message.contains("vc_redist.x64.exe"), windows);
        assert!(!r.snapshot().decoder);
        assert!(r.snapshot().decoder_failed);
        assert!(start(&mut r, "receive", "LTC", 60.0).effects.is_empty());
        assert_eq!(r.snapshot().decoder_framerate, Scalar::number(24.0));
        assert!(r
            .begin(Command::Audio {
                mode: "LTC".into(),
                bytes: vec![128]
            })
            .unwrap()
            .effects
            .is_empty());
    }
}
#[test]
fn ltc_encoder_resets_exactly_and_real_buffer_receipt_is_required() {
    let mut r = Router::new(false);
    start(&mut r, "send", "LTC", 25.0);
    let create = r.begin(Command::Value(Number::new(1000.0))).unwrap();
    assert!(r.begin(Command::Value(Number::new(1001.0))).is_err());
    let encode = r.complete(&token(&create), Completion::Opened).unwrap();
    assert!(r.complete(&token(&encode), Completion::Done).is_err());
    assert!(r
        .complete(
            &token(&encode),
            Completion::Encoded(vec![0; MAX_AUDIO_BYTES + 1])
        )
        .is_err());
    let bytes = r
        .complete(&token(&encode), Completion::Encoded(vec![0, 128, 255]))
        .unwrap();
    assert!(matches!(&bytes.effects[0].effect,Effect::Audio{bytes} if bytes==&[0,128,255]));
    let encode = r.begin(Command::Value(Number::new(1000.0))).unwrap();
    assert!(matches!(encode.effects[0].effect, Effect::Encode { .. }));
    r.complete(&token(&encode), Completion::Encoded(vec![]))
        .unwrap();
    let reset = r.begin(Command::Value(Number::new(999.0))).unwrap();
    assert!(matches!(
        reset.effects[0].effect,
        Effect::CreateEncoder {
            replacing: true,
            ..
        }
    ));
    r.complete(
        &token(&reset),
        Completion::Failed(Error::native("reset failed")),
    )
    .unwrap();
    assert!(r.snapshot().encoder);
    assert!(r.snapshot().encoder_failed);
    assert_eq!(r.snapshot().last_time, Number::new(1000.0));
    let stop = r.begin(Command::Stop).unwrap();
    r.complete(&token(&stop), Completion::Done).unwrap();
    assert!(r.snapshot().encoder_failed);
    assert!(!r.snapshot().encoder);
    assert!(r
        .begin(Command::Value(Number::new(5000.0)))
        .unwrap()
        .effects
        .is_empty());
}
#[test]
fn stop_throw_preserves_map_tombstones_and_short_circuits() {
    let mut r = Router::new(false);
    let ltc = start(&mut r, "receive", "LTC", 25.0);
    r.complete(&token(&ltc), Completion::Opened).unwrap();
    let mtc = start(&mut r, "receive", "MTC", 25.0);
    r.complete(&token(&mtc), Completion::Opened).unwrap();
    let close = r.begin(Command::Stop).unwrap();
    let next = r.complete(&token(&close), Completion::Done).unwrap();
    r.complete(
        &token(&next),
        Completion::Failed(Error::native("close failed")),
    )
    .unwrap();
    assert_eq!(r.snapshot().receivers, vec!["LTC", "MTC"]);
    assert!(!r.snapshot().decoder);
    assert!(start(&mut r, "receive", "LTC", 25.0).effects.is_empty());
    let retirement = r.retire().unwrap();
    assert_eq!(retirement.effects.len(), 1);
    r.complete(&token(&retirement), Completion::Done).unwrap();
    assert!(r.drained());
}
#[test]
fn accepted_open_failure_and_failed_close_retain_authority_until_receipt() {
    let mut r = Router::new(false);
    let open = start(&mut r, "receive", "MTC", 25.0);
    let listener = r.accept_listener(&token(&open)).unwrap();
    r.complete(
        &token(&open),
        Completion::Failed(Error::native("connect failed")),
    )
    .unwrap();
    assert_eq!(r.snapshot().owned_listeners, 1);
    let close = r.retire().unwrap();
    let retry = r
        .complete(
            &token(&close),
            Completion::Failed(Error::native("close failed")),
        )
        .unwrap();
    assert!(!r.drained());
    assert!(retry.effects.is_empty());
    let retry = r.retire().unwrap();
    assert_eq!(retry.effects.len(), 1);
    r.complete(&token(&retry), Completion::Done).unwrap();
    assert!(r.mtc_message(&listener, &[0xfa], 0.0).is_err());
    assert!(r.drained());
}
#[test]
fn late_ltc_constructor_and_buffer_cannot_escape_retirement() {
    let mut r = Router::new(false);
    start(&mut r, "send", "LTC", 25.0);
    let create = r.begin(Command::Value(Number::new(0.0))).unwrap();
    assert!(matches!(
        r.retire().unwrap().effects[0].effect,
        Effect::Cancel { .. }
    ));
    let stop = r.complete(&token(&create), Completion::Opened).unwrap();
    assert!(matches!(stop.effects[0].effect, Effect::StopEncoder));
    r.complete(&token(&stop), Completion::Done).unwrap();
    assert!(r.drained());
}

#[test]
fn async_completions_and_callbacks_cannot_interleave_sync_source_continuations() {
    let mut r = Router::new(false);
    let open = start(&mut r, "receive", "MTC", 25.0);
    let listener = r.accept_listener(&token(&open)).unwrap();
    let decoder = start(&mut r, "receive", "LTC", 30.0);
    assert!(r.complete(&token(&open), Completion::Opened).is_err());
    assert!(r.mtc_message(&listener, &[0xfa], 0.0).is_err());
    assert!(!r.process_frame(Number::new(1.0)).errors.is_empty());
    assert!(!r.update_status(Status::Play).errors.is_empty());
    assert!(r.snapshot().receivers.is_empty());
    r.complete(&token(&decoder), Completion::Opened).unwrap();
    r.complete(&token(&open), Completion::Opened).unwrap();
    assert_eq!(r.snapshot().receivers, vec!["LTC", "MTC"]);
    assert_eq!(
        r.mtc_message(&listener, &[0xfa], 100.0)
            .unwrap()
            .effects
            .len(),
        2
    );
}
#[test]
fn resource_rejections_preserve_pending_authority_and_bound_diagnostics() {
    let mut r = Router::new(false);
    assert!(r
        .begin(Command::Start {
            kind: "send".into(),
            mode: "MTC".into(),
            framerate: Scalar::Undefined,
            data: Data::Json(serde_json::json!({"huge":"a".repeat(65537)}))
        })
        .is_err());
    assert_eq!(r.snapshot().current_mode, None);
    assert!(r
        .begin(Command::Audio {
            mode: "LTC".into(),
            bytes: vec![0; MAX_AUDIO_BYTES + 1]
        })
        .is_err());
    let decoder = start(&mut r, "receive", "LTC", 25.0);
    let step = r
        .complete(
            &token(&decoder),
            Completion::Failed(Error {
                code: "host_error",
                message: "敬".repeat(2000),
            }),
        )
        .unwrap();
    assert!(step.errors[0].message.len() <= 1024);
    assert!(step.errors[0].message.ends_with('敬'));
    assert_eq!(step.errors[0].code, "host_error");
    assert!(r.complete(&token(&decoder), Completion::Opened).is_err());
}
#[test]
fn retirement_cancels_all_pending_owners_and_suppresses_late_constructor_alert() {
    let mut r = Router::new(true);
    let mtc = start(&mut r, "receive", "MTC", 25.0);
    let decoder = start(&mut r, "receive", "LTC", 25.0);
    assert_eq!(r.retire().unwrap().effects.len(), 2);
    let finish = r
        .complete(
            &token(&decoder),
            Completion::Failed(Error::native("unavailable")),
        )
        .unwrap();
    assert!(finish
        .effects
        .iter()
        .all(|e| !matches!(e.effect, Effect::Alert { .. })));
    r.complete(&token(&mtc), Completion::Failed(Error::native("cancelled")))
        .unwrap();
    assert!(r.drained());
}

#[test]
fn retirement_defers_closes_at_full_async_capacity_then_drains_every_listener_and_encoder() {
    let mut r = Router::new(false);
    let opens = (0..MAX_LISTENERS)
        .map(|_| start(&mut r, "receive", "MTC", 25.0))
        .collect::<Vec<_>>();
    for open in opens {
        r.complete(&token(&open), Completion::Opened).unwrap();
    }
    start(&mut r, "send", "MTC", 25.0);
    let sends = (0..MAX_PENDING)
        .map(|_| r.begin(Command::Value(Number::new(1.0))).unwrap())
        .collect::<Vec<_>>();
    start(&mut r, "send", "LTC", 25.0);
    let create = r.begin(Command::Value(Number::new(0.0))).unwrap();
    let encode = r.complete(&token(&create), Completion::Opened).unwrap();
    r.complete(&token(&encode), Completion::Encoded(vec![128]))
        .unwrap();
    assert_eq!(r.retire().unwrap().effects.len(), MAX_PENDING);
    let mut cleanup = std::collections::VecDeque::new();
    for send in sends {
        for effect in r.complete(&token(&send), Completion::Done).unwrap().effects {
            if effect.token.is_some() {
                cleanup.push_back(effect);
            }
        }
    }
    let mut completions = 0;
    while let Some(effect) = cleanup.pop_front() {
        completions += 1;
        assert!(completions <= MAX_LISTENERS + 2);
        let next = r
            .complete(&effect.token.unwrap(), Completion::Done)
            .unwrap();
        cleanup.extend(next.effects.into_iter().filter(|e| e.token.is_some()));
    }
    assert_eq!(completions, MAX_LISTENERS + 2);
    assert!(r.drained());
}

#[test]
fn retained_mtc_sender_and_late_failed_send_are_owned_even_after_mode_switch_and_stop() {
    let mut r = Router::new(false);
    start(&mut r, "send", "MTC", 25.0);
    let send = r.begin(Command::Value(Number::new(0.0))).unwrap();
    r.accept_sender(&token(&send)).unwrap();
    assert!(r.accept_sender(&token(&send)).is_err());
    let stop = r.begin(Command::Stop).unwrap();
    r.complete(&token(&stop), Completion::Done).unwrap();
    assert!(!r.snapshot().mtc_sender);
    r.complete(
        &token(&send),
        Completion::Failed(Error::native("late send failure")),
    )
    .unwrap();
    assert!(r.snapshot().mtc_sender);
    start(&mut r, "send", "ableton", 25.0);
    assert!(r.begin(Command::Stop).unwrap().effects.is_empty());
    let retiring = r.retire().unwrap();
    assert_eq!(retiring.effects.len(), 1);
    assert!(matches!(retiring.effects[0].effect, Effect::StopMtc));
    let failure = r
        .complete(
            &token(&retiring),
            Completion::Failed(Error::native("close failed")),
        )
        .unwrap();
    assert!(failure.effects.is_empty());
    assert!(!r.drained());
    let retry = r.retire().unwrap();
    r.complete(&token(&retry), Completion::Done).unwrap();
    assert!(r.drained());
}

#[test]
fn renewal_requires_drain_reseals_tokens_and_preserves_source_globals() {
    let mut r = Router::new(false);
    assert!(r.renew_after_drain().is_err());
    let opened = start(&mut r, "receive", "MTC", 25.0);
    let old_token = token(&opened);
    let old_listener = r.accept_listener(&old_token).unwrap();
    r.complete(&old_token, Completion::Opened).unwrap();
    let fail = start(&mut r, "receive", "LTC", 24.0);
    r.complete(
        &token(&fail),
        Completion::Failed(Error::native("actual unavailable")),
    )
    .unwrap();
    r.begin(Command::Start {
        kind: "send".into(),
        mode: "LTC".into(),
        framerate: Scalar::Text("30".into()),
        data: Data::Json(serde_json::json!({"unknown":"日本語"})),
    })
    .unwrap();
    let fail = r.begin(Command::Value(Number::new(123.0))).unwrap();
    r.complete(
        &token(&fail),
        Completion::Failed(Error::native("actual unavailable")),
    )
    .unwrap();
    r.update_status(Status::Play);
    let retire = r.retire().unwrap();
    assert!(!r.drained());
    assert!(r.renew_after_drain().is_err());
    for effect in retire.effects {
        r.complete(&effect.token.unwrap(), Completion::Done)
            .unwrap();
    }
    assert!(r.drained());
    r.renew_after_drain().unwrap();
    assert_eq!(r.snapshot().framerate, Scalar::Text("30".into()));
    assert_eq!(r.snapshot().current_mode.as_deref(), Some("LTC"));
    assert_eq!(
        r.snapshot().send_data,
        Data::Json(serde_json::json!({"unknown":"日本語"}))
    );
    assert!(r.snapshot().decoder_failed);
    assert!(r.snapshot().encoder_failed);
    assert!(r.update_status(Status::Play).effects.is_empty());
    assert!(r
        .begin(Command::Value(Number::new(456.0)))
        .unwrap()
        .effects
        .is_empty());
    assert!(r.complete(&old_token, Completion::Opened).is_err());
    assert!(r.mtc_message(&old_listener, &[0xfa], 10.0).is_err());
    let fresh = start(&mut r, "receive", "MTC", 0.0);
    let fresh_token = token(&fresh);
    assert_eq!(fresh_token.serial(), 2);
    assert!(r.accept_listener(&old_token).is_err());
    r.accept_listener(&fresh_token).unwrap();
    r.complete(&fresh_token, Completion::Opened).unwrap();
    assert_eq!(r.snapshot().owned_listeners, 1);
}
