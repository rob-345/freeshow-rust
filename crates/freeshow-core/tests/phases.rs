use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::{
        LibraryService, PHASE_CAPACITY, PHASE_FINISH_CAPACITY, PHASE_JOB_CAPACITY, PHASE_JOB_LIMIT,
        PHASE_REPLY_BYTES, PHASE_REQUEST_BYTES,
    },
    Error,
};
use serde_json::{json, Value};
use std::{
    fs,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};
use tokio::{sync::oneshot, time::timeout};

const DEADLINE: Duration = Duration::from_secs(3);
const HELD: Duration = Duration::from_millis(20);

fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    Request {
        channel: "MAIN".into(),
        data: json!({"channel":command,"data":data}),
        listener_id: Some("phase-test".into()),
        context: Some(context.clone()),
    }
}

fn service() -> (tempfile::TempDir, LibraryService, RuntimeContext) {
    let temp = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(temp.path().join("app"), temp.path().join("A")).unwrap();
    let service = LibraryService::start(runtime).unwrap();
    let context = service.context().unwrap();
    (temp, service, context)
}

async fn pending(future: impl Future) {
    assert!(timeout(HELD, future).await.is_err());
}

fn resource(error: Error) {
    assert!(matches!(
        error,
        Error::NativeOperation {
            code: "library_phase_resource",
            ..
        }
    ));
}

fn closed(error: Error) {
    assert!(matches!(
        error,
        Error::NativeOperation {
            code: "library_phase_closed",
            ..
        }
    ));
}

#[tokio::test]
async fn earlier_phase_and_later_mutations_share_one_actual_owner_and_order() {
    let (_temp, service, context) = service();
    let order = Arc::new(Mutex::new(Vec::new()));
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let log = order.clone();
    let earlier = service.commit(
        request("NATIVE_EARLIER", Value::Null, &context),
        "main",
        move |_| {
            log.lock()
                .unwrap()
                .push(("earlier", std::thread::current().id()));
            let _ = entered.send(());
            wait.recv().unwrap();
            Ok(vec![])
        },
    );
    tokio::pin!(earlier);
    tokio::select! {
        result = &mut earlier => panic!("earlier returned before release: {result:?}"),
        result = received => result.unwrap(),
    }
    let mut begin =
        Box::pin(service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main"));
    pending(&mut begin).await;
    let log = order.clone();
    let mut later = Box::pin(service.commit(
        request("NATIVE_LATER", Value::Null, &context),
        "main",
        move |_| {
            log.lock()
                .unwrap()
                .push(("later", std::thread::current().id()));
            Ok(vec![])
        },
    ));
    pending(&mut later).await;
    release.send(()).unwrap();
    timeout(DEADLINE, &mut earlier).await.unwrap().unwrap();
    let phase = timeout(DEADLINE, begin).await.unwrap().unwrap();
    assert_eq!(phase.context(), &context);
    let log = order.clone();
    let first = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            log.lock()
                .unwrap()
                .push(("phase-one", std::thread::current().id()));
            Ok(vec![Event::reply(
                "MAIN",
                "AUDIO_METADATA",
                json!({"title":"one"}),
                Some("source".into()),
            )])
        })
        .unwrap();
    let events = timeout(DEADLINE, first.finish()).await.unwrap().unwrap();
    assert_eq!(events[0].context.as_ref(), Some(&context));
    assert_eq!(events[0].listener_id.as_deref(), Some("source"));
    pending(&mut later).await;
    let log = order.clone();
    phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            log.lock()
                .unwrap()
                .push(("phase-two", std::thread::current().id()));
            Ok(vec![])
        })
        .unwrap()
        .finish()
        .await
        .unwrap();
    phase.finish().await.unwrap();
    timeout(DEADLINE, later).await.unwrap().unwrap();
    let log = order.lock().unwrap();
    assert_eq!(
        log.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        ["earlier", "phase-one", "phase-two", "later"]
    );
    assert!(log.iter().all(|entry| entry.1 == log[0].1));
    assert_ne!(log[0].1, std::thread::current().id());
}

#[tokio::test]
async fn idle_phase_keeps_snapshots_and_show_pool_responsive_but_reads_fence_accepted_io() {
    let (temp, service, context) = service();
    fs::create_dir_all(temp.path().join("A/Shows")).unwrap();
    fs::write(
        temp.path().join("A/Shows/Live.show"),
        b"[\"song\",{\"name\":\"Live\",\"text\":\"before\"}]",
    )
    .unwrap();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let read = || {
        service.dispatch(
            request("SHOW", json!({"id":"song","name":"Live"}), &context),
            "main",
        )
    };
    let before = timeout(DEADLINE, read()).await.unwrap().unwrap();
    assert_eq!(
        before[0].message.data.as_ref().unwrap()["content"][1]["text"],
        "before"
    );
    assert_eq!(service.context().unwrap(), context);
    assert_eq!(
        service
            .media_settings(&request("NOW_PLAYING", Value::Null, &context))
            .unwrap()
            .context,
        context
    );
    assert_eq!(
        service.status().unwrap().active_job.unwrap().phase,
        "nativePhaseIdle"
    );
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let path = temp.path().join("A/Shows/Live.show");
    let ticket = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            let _ = entered.send(());
            wait.recv().unwrap();
            fs::write(path, b"[\"song\",{\"name\":\"Live\",\"text\":\"after\"}]")?;
            Ok(vec![])
        })
        .unwrap();
    timeout(DEADLINE, received).await.unwrap().unwrap();
    let mut fenced = Box::pin(read());
    pending(&mut fenced).await;
    assert_eq!(service.context().unwrap(), context);
    assert_eq!(
        service.status().unwrap().active_job.unwrap().phase,
        "nativePhase"
    );
    release.send(()).unwrap();
    let after = timeout(DEADLINE, fenced).await.unwrap().unwrap();
    assert_eq!(
        after[0].message.data.as_ref().unwrap()["content"][1]["text"],
        "after"
    );
    ticket.finish().await.unwrap();
    // Keep the phase open and prove its completed subjob does not hold SHOW.
    timeout(DEADLINE, read()).await.unwrap().unwrap();
    phase.finish().await.unwrap();
}

#[tokio::test]
async fn accepted_jobs_outlive_dropped_phase_ticket_and_finish_waiters() {
    let (temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let path = temp.path().join("actual-completion");
    let target = path.clone();
    let ticket = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            let _ = entered.send(());
            wait.recv().unwrap();
            fs::write(target, b"accepted")?;
            Ok(vec![])
        })
        .unwrap();
    timeout(DEADLINE, received).await.unwrap().unwrap();
    drop(ticket);
    pending(phase.finish()).await;
    assert_eq!(service.status().unwrap().retained_phase_jobs, 1);
    assert!(!path.exists());
    closed(
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                panic!("sealed")
            })
            .err()
            .unwrap(),
    );
    drop(phase);
    let mut later = Box::pin(service.commit(
        request("AFTER", Value::Null, &context),
        "main",
        |_| Ok(vec![]),
    ));
    pending(&mut later).await;
    assert_eq!(service.status().unwrap().retained_phases, 1);
    release.send(()).unwrap();
    timeout(DEADLINE, later).await.unwrap().unwrap();
    assert_eq!(fs::read(path).unwrap(), b"accepted");
    assert_eq!(service.status().unwrap().retained_phases, 0);
    assert_eq!(service.status().unwrap().retained_phase_jobs, 0);
}

#[tokio::test]
async fn retained_results_bound_jobs_but_do_not_hold_admission_after_real_completion() {
    let (_temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let mut tickets = Vec::new();
    for index in 0..PHASE_JOB_CAPACITY {
        tickets.push(
            phase
                .submit(request("NOW_PLAYING", json!(index), &context), |_| {
                    Ok(vec![])
                })
                .unwrap(),
        );
    }
    resource(
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                panic!("full")
            })
            .err()
            .unwrap(),
    );
    timeout(DEADLINE, phase.finish()).await.unwrap().unwrap();
    assert_eq!(
        service.status().unwrap().retained_phase_jobs,
        PHASE_JOB_CAPACITY
    );
    timeout(
        DEADLINE,
        service.commit(request("AFTER", Value::Null, &context), "main", |_| {
            Ok(vec![])
        }),
    )
    .await
    .unwrap()
    .unwrap();
    drop(phase);
    assert_eq!(service.status().unwrap().retained_phases, 1);
    let first = tickets.pop().unwrap();
    first.finish().await.unwrap();
    assert_eq!(
        service.status().unwrap().retained_phase_jobs,
        PHASE_JOB_CAPACITY - 1
    );
    drop(tickets);
    assert_eq!(service.status().unwrap().retained_phases, 0);
}

#[tokio::test]
async fn closed_retained_phases_still_consume_whole_phase_quota() {
    let (_temp, service, context) = service();
    let mut retained = Vec::new();
    for _ in 0..PHASE_CAPACITY {
        let phase = service
            .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
            .await
            .unwrap();
        phase.finish().await.unwrap();
        retained.push(phase);
    }
    resource(
        service
            .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
            .await
            .err()
            .unwrap(),
    );
    assert_eq!(service.status().unwrap().retained_phases, PHASE_CAPACITY);
    drop(retained.pop());
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    phase.finish().await.unwrap();
}

#[tokio::test]
async fn lifetime_job_limit_is_independent_of_result_capacity_and_close() {
    let (_temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    for _ in 0..PHASE_JOB_LIMIT {
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                Ok(vec![])
            })
            .unwrap()
            .finish()
            .await
            .unwrap();
    }
    resource(
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                panic!("lifetime full")
            })
            .err()
            .unwrap(),
    );
    timeout(DEADLINE, phase.finish()).await.unwrap().unwrap();
    timeout(
        DEADLINE,
        service.commit(request("AFTER", Value::Null, &context), "main", |_| {
            Ok(vec![])
        }),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn dropped_pending_and_already_enqueued_begin_cannot_leak_admission() {
    let (_temp, service, context) = service();
    let active = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let mut pending_begin =
        Box::pin(service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main"));
    pending(&mut pending_begin).await;
    assert_eq!(service.status().unwrap().retained_phases, 2);
    drop(pending_begin);
    assert_eq!(service.status().unwrap().retained_phases, 1);
    active.finish().await.unwrap();
    drop(active);
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut earlier = Box::pin(service.commit(
        request("BEFORE", Value::Null, &context),
        "main",
        move |_| {
            let _ = entered.send(());
            wait.recv().unwrap();
            Ok(vec![])
        },
    ));
    tokio::select! {
        result = &mut earlier => panic!("blocked job returned: {result:?}"),
        result = received => result.unwrap(),
    }
    let mut enqueued =
        Box::pin(service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main"));
    pending(&mut enqueued).await;
    assert_eq!(service.status().unwrap().queued_jobs, 1);
    drop(enqueued);
    release.send(()).unwrap();
    timeout(DEADLINE, earlier).await.unwrap().unwrap();
    timeout(
        DEADLINE,
        service.commit(request("AFTER", Value::Null, &context), "main", |_| {
            Ok(vec![])
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(service.status().unwrap().retained_phases, 0);
}

#[tokio::test]
async fn terminal_control_seals_saturated_phases_and_results_then_retries_real_drain() {
    let (temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut tickets = vec![phase
        .submit(
            request("NOW_PLAYING", Value::Null, &context),
            move |runtime| {
                let _ = entered.send(());
                wait.recv().unwrap();
                runtime.stores.set_key("CACHE_SYNC", "phase", json!(true))?;
                Ok(vec![])
            },
        )
        .unwrap()];
    timeout(DEADLINE, received).await.unwrap().unwrap();
    for index in 1..PHASE_JOB_CAPACITY {
        tickets.push(
            phase
                .submit(
                    request("NOW_PLAYING", Value::Null, &context),
                    move |runtime| {
                        runtime.stores.set_key(
                            "CACHE_SYNC",
                            &format!("phase{index}"),
                            json!(true),
                        )?;
                        runtime.save(&json!({}))?;
                        Ok(vec![])
                    },
                )
                .unwrap(),
        );
    }
    let mut pending_begins = Vec::new();
    for _ in 1..PHASE_CAPACITY {
        let mut begin =
            Box::pin(service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main"));
        pending(&mut begin).await;
        pending_begins.push(begin);
    }
    resource(
        service
            .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
            .await
            .err()
            .unwrap(),
    );
    // Dropping a terminal waiter only drops an observation; accepted IO stays.
    pending(service.finish_writes()).await;
    assert!(service.status().unwrap().phase_admission_closed);
    closed(
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                panic!("closed")
            })
            .err()
            .unwrap(),
    );
    for begin in pending_begins {
        // Still waiting for the retained admission guard until actual IO ends.
        drop(begin);
    }
    assert_eq!(
        service.status().unwrap().retained_phase_jobs,
        PHASE_JOB_CAPACITY
    );
    let mut retry = Box::pin(service.finish_writes());
    pending(&mut retry).await;
    release.send(()).unwrap();
    timeout(DEADLINE, retry).await.unwrap().unwrap();
    timeout(DEADLINE, phase.finish()).await.unwrap().unwrap();
    let saved: Value =
        serde_json::from_slice(&fs::read(temp.path().join("app/cache_sync.json")).unwrap())
            .unwrap();
    assert_eq!(saved["phase"], true);
    assert_eq!(saved["phase3"], true);
    assert!(!service.status().unwrap().accepting_requests);
    assert_eq!(
        service.status().unwrap().retained_phase_jobs,
        PHASE_JOB_CAPACITY
    );
    for ticket in tickets {
        ticket.finish().await.unwrap();
    }
    service.finish_writes().await.unwrap();
    closed(
        service
            .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
            .await
            .err()
            .unwrap(),
    );
}

#[tokio::test]
async fn source_context_cannot_be_forged_or_silently_renewed_inside_a_phase() {
    let (temp, service, context) = service();
    let (_other, other, foreign) = self::service();
    assert_ne!(foreign.runtime_id, context.runtime_id);
    assert!(matches!(
        service
            .begin_phase(request("NOW_PLAYING", Value::Null, &foreign), "main")
            .await,
        Err(Error::StaleLibrary { .. })
    ));
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    assert!(matches!(
        phase.submit(request("NOW_PLAYING", Value::Null, &foreign), |_| panic!(
            "foreign"
        )),
        Err(Error::StaleLibrary { .. })
    ));
    let destination = temp.path().join("B");
    fs::create_dir(&destination).unwrap();
    let changed = request(
        "UPDATE_DATA_PATH",
        json!({"oldPath":context.data_path,"newPath":destination}),
        &context,
    );
    phase
        .submit(
            request("NOW_PLAYING", Value::Null, &context),
            move |runtime| runtime.dispatch(changed, "main"),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    let current = service.context().unwrap();
    assert_ne!(current, context);
    let called = Arc::new(AtomicBool::new(false));
    let attempted = called.clone();
    let stale = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            attempted.store(true, Ordering::Release);
            Ok(vec![])
        })
        .unwrap()
        .finish()
        .await;
    assert!(matches!(stale, Err(Error::StaleLibrary { .. })));
    assert!(!called.load(Ordering::Acquire));
    assert!(matches!(
        phase.submit(request("NOW_PLAYING", Value::Null, &current), |_| panic!(
            "new context inside old phase"
        )),
        Err(Error::StaleLibrary { .. })
    ));
    phase.finish().await.unwrap();
    let renewed = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &current), "main")
        .await
        .unwrap();
    renewed.finish().await.unwrap();
    other.finish_writes().await.unwrap();
}

#[tokio::test]
async fn queued_library_change_is_validated_before_old_context_phase_begins() {
    let (temp, service, context) = service();
    let active = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let destination = temp.path().join("B");
    fs::create_dir(&destination).unwrap();
    let mut changed = Box::pin(service.dispatch(
        request(
            "UPDATE_DATA_PATH",
            json!({"oldPath":context.data_path,"newPath":destination}),
            &context,
        ),
        "main",
    ));
    pending(&mut changed).await;
    let mut old =
        Box::pin(service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main"));
    pending(&mut old).await;
    active.finish().await.unwrap();
    timeout(DEADLINE, changed).await.unwrap().unwrap();
    assert!(matches!(
        timeout(DEADLINE, old).await.unwrap(),
        Err(Error::StaleLibrary { .. })
    ));
    // An errored begin is not a completion fence; let the owner retire that
    // failed marker before asserting exact retained resource counts.
    service
        .commit(
            request("AFTER", Value::Null, &service.context().unwrap()),
            "main",
            |_| Ok(vec![]),
        )
        .await
        .unwrap();
    assert_eq!(service.status().unwrap().retained_phases, 1); // only the closed active handle
}

#[tokio::test]
async fn phase_json_limits_are_explicit_and_post_effect_failures_do_not_stall_the_owner() {
    let (_temp, service, context) = service();
    resource(
        service
            .begin_phase(
                request(
                    "NOW_PLAYING",
                    json!("x".repeat(PHASE_REQUEST_BYTES)),
                    &context,
                ),
                "main",
            )
            .await
            .err()
            .unwrap(),
    );
    let mut deep = Value::Null;
    for _ in 0..34 {
        deep = json!([deep]);
    }
    resource(
        service
            .begin_phase(request("NOW_PLAYING", deep, &context), "main")
            .await
            .err()
            .unwrap(),
    );
    resource(
        service
            .begin_phase(
                request(
                    "NOW_PLAYING",
                    Value::Array(vec![json!(0); 16_384]),
                    &context,
                ),
                "main",
            )
            .await
            .err()
            .unwrap(),
    );
    // Raw UTF-8 fits, but JSON escaping does not: the exact writer budget is
    // independent of the text preflight's bounded scan.
    resource(
        service
            .begin_phase(
                request(
                    "NOW_PLAYING",
                    json!("\u{0001}".repeat(PHASE_REQUEST_BYTES / 2)),
                    &context,
                ),
                "main",
            )
            .await
            .err()
            .unwrap(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut non_unicode = context.clone();
        non_unicode
            .data_path
            .push(std::ffi::OsString::from_vec(vec![0xff]));
        resource(
            service
                .begin_phase(request("NOW_PLAYING", Value::Null, &non_unicode), "main")
                .await
                .err()
                .unwrap(),
        );
    }
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    resource(
        phase
            .submit(
                request(
                    "NOW_PLAYING",
                    json!("x".repeat(PHASE_REQUEST_BYTES)),
                    &context,
                ),
                |_| panic!("oversized input"),
            )
            .err()
            .unwrap(),
    );
    let called = Arc::new(AtomicBool::new(false));
    let applied = called.clone();
    let oversized = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            applied.store(true, Ordering::Release);
            Ok(vec![Event::reply(
                "MAIN",
                "TEST",
                json!("x".repeat(PHASE_REPLY_BYTES)),
                None,
            )])
        })
        .unwrap()
        .finish()
        .await
        .err()
        .unwrap();
    resource(oversized);
    assert!(called.load(Ordering::Acquire));
    resource(
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                Ok((0..33)
                    .map(|_| Event::reply("MAIN", "TEST", Value::Null, None))
                    .collect())
            })
            .unwrap()
            .finish()
            .await
            .err()
            .unwrap(),
    );
    resource(
        phase
            .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
                let mut event = Event::reply("MAIN", "TEST", Value::Null, None);
                let mut deep = Value::Null;
                for _ in 0..34 {
                    deep = json!([deep]);
                }
                event.native_exif = Some(deep);
                Ok(vec![event])
            })
            .unwrap()
            .finish()
            .await
            .err()
            .unwrap(),
    );
    // An ordinary native failure is still a completed operation, not an early
    // phase release. Later subjobs and close remain usable.
    let failure = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
            Err(Error::Invalid("controlled".into()))
        })
        .unwrap()
        .finish()
        .await;
    assert!(matches!(failure, Err(Error::Invalid(_))));
    phase
        .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
            Ok(vec![])
        })
        .unwrap()
        .finish()
        .await
        .unwrap();
    phase.finish().await.unwrap();
    timeout(
        DEADLINE,
        service.commit(request("AFTER", Value::Null, &context), "main", |_| {
            Ok(vec![])
        }),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn actual_worker_failure_never_becomes_a_successful_phase_fence() {
    let (_temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let ticket = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), |_| {
            panic!("controlled IO owner unwind")
        })
        .unwrap();
    assert!(matches!(
        timeout(DEADLINE, ticket.finish()).await.unwrap(),
        Err(Error::WorkerUnavailable(_))
    ));
    assert!(matches!(
        timeout(DEADLINE, phase.finish()).await.unwrap(),
        Err(Error::WorkerUnavailable(_))
    ));
    assert!(matches!(
        timeout(DEADLINE, service.finish_writes()).await.unwrap(),
        Err(Error::WorkerUnavailable(_))
    ));
    // A failed phase receipt does not itself certify whole-thread exit. The
    // existing terminal queue closure observes lifecycle teardown separately.
    assert!(!service.status().unwrap().worker_running);
}

#[tokio::test]
async fn finish_observers_are_bounded_and_drop_only_releases_the_observer() {
    let (_temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let ticket = phase
        .submit(request("NOW_PLAYING", Value::Null, &context), move |_| {
            let _ = entered.send(());
            wait.recv().unwrap();
            Ok(vec![])
        })
        .unwrap();
    timeout(DEADLINE, received).await.unwrap().unwrap();
    let mut observers = Vec::new();
    for _ in 0..PHASE_FINISH_CAPACITY {
        let mut observer = Box::pin(phase.finish());
        pending(&mut observer).await;
        observers.push(observer);
    }
    assert_eq!(
        service.status().unwrap().retained_phase_observers,
        PHASE_FINISH_CAPACITY
    );
    resource(
        timeout(DEADLINE, phase.finish())
            .await
            .unwrap()
            .unwrap_err(),
    );
    phase.close(); // Always seals, even when all observation slots are held.
    drop(observers.pop());
    assert_eq!(
        service.status().unwrap().retained_phase_observers,
        PHASE_FINISH_CAPACITY - 1
    );
    let mut retry = Box::pin(phase.finish());
    pending(&mut retry).await;
    let mut later = Box::pin(service.commit(
        request("AFTER", Value::Null, &context),
        "main",
        |_| Ok(vec![]),
    ));
    pending(&mut later).await;
    assert_eq!(service.status().unwrap().retained_phase_jobs, 1);
    release.send(()).unwrap();
    timeout(DEADLINE, retry).await.unwrap().unwrap();
    for observer in observers {
        timeout(DEADLINE, observer).await.unwrap().unwrap();
    }
    timeout(DEADLINE, later).await.unwrap().unwrap();
    assert_eq!(service.status().unwrap().retained_phase_observers, 0);
    ticket.finish().await.unwrap();
}

#[tokio::test]
async fn ordered_final_save_and_terminal_fence_wait_for_real_phase_io() {
    let (temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let (entered, received) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let accepted = phase
        .submit(
            request("NOW_PLAYING", Value::Null, &context),
            move |runtime| {
                let _ = entered.send(());
                wait.recv().unwrap();
                runtime
                    .stores
                    .set_key("CACHE_SYNC", "phase", json!("committed"))?;
                Ok(vec![])
            },
        )
        .unwrap();
    timeout(DEADLINE, received).await.unwrap().unwrap();
    let mut save = Box::pin(service.dispatch(request("SAVE", json!({"PROJECTS":{"afterPhase":true},"runtimeSaveToken":"final","runtimeContext":context}), &context), "main"));
    pending(&mut save).await;
    let mut terminal = Box::pin(service.finish_writes());
    pending(&mut terminal).await;
    assert!(!temp.path().join("app/cache_sync.json").exists());
    release.send(()).unwrap();
    // SAVE already owned admission's FIFO position when the terminal fence
    // closed the phase. It must run after accepted IO and before the fence.
    let events = timeout(DEADLINE, save).await.unwrap().unwrap();
    assert_eq!(
        events[0].message.data.as_ref().unwrap()["runtimeSaveToken"],
        "final"
    );
    timeout(DEADLINE, terminal).await.unwrap().unwrap();
    accepted.finish().await.unwrap();
    phase.finish().await.unwrap();
    let cache: Value =
        serde_json::from_slice(&fs::read(temp.path().join("app/cache_sync.json")).unwrap())
            .unwrap();
    let projects: Value =
        serde_json::from_slice(&fs::read(temp.path().join("A/Config/projects.json")).unwrap())
            .unwrap();
    assert_eq!(cache["phase"], "committed");
    assert_eq!(projects["afterPhase"], true);
}

#[tokio::test]
async fn terminal_seals_pending_begins_without_work_or_result_quota() {
    let (_temp, service, context) = service();
    let active = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let mut begins = Vec::new();
    for _ in 1..PHASE_CAPACITY {
        let mut begin =
            Box::pin(service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main"));
        pending(&mut begin).await;
        begins.push(begin);
    }
    let mut terminal = Box::pin(service.finish_writes());
    pending(&mut terminal).await;
    // All four leases are retained, but close is independent. Polling each
    // already-waiting begin observes its seal and relinquishes FIFO admission.
    for begin in begins {
        closed(timeout(DEADLINE, begin).await.unwrap().err().unwrap());
    }
    timeout(DEADLINE, terminal).await.unwrap().unwrap();
    active.finish().await.unwrap();
    assert_eq!(service.status().unwrap().retained_phases, 1);
}

#[tokio::test]
async fn phase_subjobs_retain_embedded_save_snapshot_context_validation() {
    let (temp, service, context) = service();
    let phase = service
        .begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
        .await
        .unwrap();
    let mut old = context.clone();
    old.library_generation += 1;
    let called = Arc::new(AtomicBool::new(false));
    let attempted = called.clone();
    let result = phase
        .submit(
            request("SAVE", json!({"runtimeContext":old}), &context),
            move |_| {
                attempted.store(true, Ordering::Release);
                Ok(vec![])
            },
        )
        .unwrap()
        .finish()
        .await;
    let Err(Error::StaleLibrary {
        recovery_path: Some(recovery),
        ..
    }) = result
    else {
        panic!("embedded stale SAVE must retain original recovery behavior");
    };
    assert!(recovery.starts_with(temp.path().join("app/Recovery")));
    assert!(recovery.is_file());
    assert!(!called.load(Ordering::Acquire));
    phase.finish().await.unwrap();
}
