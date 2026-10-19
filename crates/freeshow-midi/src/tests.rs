use super::*;
use serde_json::json;
#[test]
fn send_defaults_mutation_error_order_and_note_names() {
    let mut data = json!({"output":"port","future":"retained"});
    let plan = prepare_send(&mut data).unwrap();
    assert_eq!(plan.packet().unwrap(), [0x90, 0, 0]);
    assert_eq!(data["values"]["channel"], 0.0);
    assert_eq!(data["future"], "retained");
    let mut data = json!({"type":"other","values":{"channel":16,"note":"D♭5"}});
    assert_eq!(
        prepare_send(&mut data).unwrap().packet().unwrap(),
        [0x8f, 61, 64]
    );
    let mut data =
        json!({"type":"control","values":{"channel":0,"controller":"0x10","value":"12"}});
    assert_eq!(
        prepare_send(&mut data).unwrap().packet().unwrap(),
        [0xb0, 16, 12]
    );
    let mut data =
        json!({"type":"noteon","values":{"channel":"garbage","note":null,"velocity":128}});
    let plan = prepare_send(&mut data).unwrap();
    assert_eq!(plan.packet().unwrap_err().name, "RangeError");
    assert!(plan
        .packet()
        .unwrap_err()
        .message
        .starts_with("Bad channel value"));
    assert_eq!(data["values"]["channel"], Value::Null);
    assert_eq!(plan.hydration.numbers[0].number, Number::NaN);
}
#[test]
fn receive_missing_values_labels_and_raw_channel_preserve_contract() {
    let data = receive(Some(&json!("a")), &[0x90], "90 -- Note Off")
        .unwrap()
        .unwrap();
    assert_eq!(
        data.data,
        json!({"id":"a","type":"noteoff","values":{"channel":1}})
    );
    assert_eq!(
        data.hydration.undefined_paths,
        vec!["/values/note", "/values/velocity"]
    );
    let data = receive(None, &[0xa2, 60, 5], "a2 3c 05 -- Aftertouch (Note Off)")
        .unwrap()
        .unwrap();
    assert_eq!(data.data["type"], "noteoff");
    assert_eq!(data.data["values"]["channel"], 3);
    assert_eq!(data.hydration.undefined_paths, vec!["/id"]);
    assert!(receive(None, &[0xf8], "Timing clock").unwrap().is_none());
}
#[test]
fn mtc_source_clock_order_and_nonfinite_values_are_explicit() {
    let mut receiver = MtcReceiver::default();
    assert_eq!(
        receiver.feed(&[0xf2, 8, 0], 0.0).unwrap(),
        vec![TimecodeEffect::Frame(Number::Finite(1000.0))]
    );
    assert_eq!(
        receiver.feed(&[0xfa], 100.0).unwrap(),
        vec![
            TimecodeEffect::Frame(Number::Finite(1000.0)),
            TimecodeEffect::Status(Status::Play)
        ]
    );
    assert_eq!(
        receiver.feed(&[0xf8], 50.0).unwrap()[0],
        TimecodeEffect::Frame(Number::Finite(950.0))
    );
    assert_eq!(
        receiver.feed(&[0xf8], 550.0).unwrap(),
        vec![TimecodeEffect::Status(Status::Play)]
    );
    assert_eq!(
        receiver.feed(&[0xf0, 0x7f, 0, 1, 1], 0.0).unwrap(),
        vec![TimecodeEffect::Frame(Number::NaN)]
    );
    assert_eq!(receiver.snapshot().nibbles, [0; 8]);
    assert_eq!(receiver.snapshot().midi_clock_time, Number::NaN);
    assert_eq!(
        receiver.feed(&[0xfa], 1.0).unwrap()[0],
        TimecodeEffect::Frame(Number::NaN)
    );
    let packets = mtc_packets(0.0, 24.0);
    assert_eq!(packets[7], [0xf1, 0x72]);
    assert_eq!(mtc_packets(0.0, 29.97)[7], [0xf1, 0x74]);
    let mut gate = StatusGate::default();
    assert!(gate.update(Status::Play).is_some());
    assert!(gate.update(Status::Play).is_none());
    assert!(gate.update(Status::Stop).is_some());
    assert!(gate.update(Status::Stop).is_none());
}
#[test]
fn port_cache_close_order_failures_and_obsolete_callback_ownership() {
    let mut ports = PortBindings::default();
    let mut opened = vec![];
    for name in ["z", "10", "2", "a"] {
        let id = json!(name);
        assert_eq!(
            ports.listen(Some(&id), Some(&json!("device"))).unwrap(),
            InputDecision::Open
        );
        let token = ports
            .opened_input(Some(&id), Some(&json!("device")), true)
            .unwrap();
        opened.push((name, token));
    }
    assert_eq!(
        ports
            .listen(Some(&json!("z")), Some(&json!("another")))
            .unwrap(),
        InputDecision::AlreadyOpen
    );
    assert_eq!(
        ports.listen(None, Some(&json!("input"))).unwrap(),
        InputDecision::Ignored
    );
    let retained = opened[0].1.clone();
    let errors = ports
        .close_inputs("z", |_| {
            Err(Error::new("backend", "IoError", "close failed"))
        })
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert!(ports
        .input_message(&retained, &[0x90, 60, 1], "Note On")
        .unwrap()
        .is_some());
    let mut order = vec![];
    ports
        .close_inputs("unknown", |token| {
            order.push(opened.iter().find(|(_, id)| id == token).unwrap().0);
            Ok(())
        })
        .unwrap();
    assert_eq!(order, vec!["2", "10", "z", "a"]);
    assert_eq!(
        ports
            .input_message(&retained, &[0x90, 60, 1], "Note On")
            .unwrap_err()
            .code,
        "midi_closed"
    );
    let first = ports.opened_output(Some(&json!("out"))).unwrap();
    assert_eq!(
        ports.output(Some(&json!("out"))).unwrap(),
        Some(first.clone())
    );
    assert_eq!(ports.opened_output(Some(&json!("out"))).unwrap(), first);
    let mtc = ports.opened_mtc_output().unwrap();
    assert_eq!(ports.opened_mtc_output().unwrap(), mtc);
    assert!(ports
        .stop_mtc(|_| Err(Error::new("backend", "IoError", "failed")))
        .is_err());
    assert_eq!(ports.mtc_output().unwrap(), Some(mtc.clone()));
    ports.stop_mtc(|_| Ok(())).unwrap();
    assert_ne!(ports.opened_mtc_output().unwrap(), mtc);
    ports.stop_midi(|_| Ok(())).unwrap();
    assert_eq!(ports.counts(), (0, 0, true));
    assert!(ports.output(Some(&json!("out"))).unwrap().is_none());
    assert!(ports.shutdown(|_| Ok(())).is_empty());
    assert_eq!(ports.counts(), (0, 0, false));
    assert_eq!(ports.opened_mtc_output().unwrap_err().code, "midi_shutdown");
    assert!(ports.shutdown(|_| panic!("already closed")).is_empty());
}
#[test]
fn bounded_work_names_ports_messages_and_diagnostics() {
    assert_eq!(
        device_names(&vec![json!({"name":"p"}); 257])
            .unwrap_err()
            .code,
        "midi_resource_limit"
    );
    assert_eq!(
        device_names(&[json!({"name":"p".repeat(MAX_NAME_BYTES+1)})])
            .unwrap_err()
            .code,
        "midi_resource_limit"
    );
    assert_eq!(
        receive(None, &vec![0; MAX_MESSAGE_BYTES + 1], "")
            .unwrap_err()
            .code,
        "midi_resource_limit"
    );
    assert_eq!(
        MtcReceiver::default()
            .feed(&vec![0; MAX_MESSAGE_BYTES + 1], 0.0)
            .unwrap_err()
            .code,
        "midi_resource_limit"
    );
    let mut data = json!({"values":{"note":1},"unknown":vec![0;1024]});
    assert_eq!(
        prepare_send(&mut data).unwrap_err().code,
        "midi_resource_limit"
    );
    let mut data = json!({});
    for _ in 0..9 {
        data = json!({"nested":data});
    }
    assert_eq!(
        prepare_send(&mut data).unwrap_err().code,
        "midi_resource_limit"
    );
    let mut ports = PortBindings::default();
    for i in 0..MAX_PORTS {
        ports.opened_output(Some(&json!(i))).unwrap();
    }
    assert_eq!(
        ports.opened_mtc_output().unwrap_err().code,
        "midi_resource_limit"
    );
    let diagnostics = ports
        .stop_midi(|_| Err(Error::new("backend", "IoError", "日本語".repeat(1000))))
        .unwrap();
    assert_eq!(diagnostics.len(), MAX_PORTS);
    assert!(diagnostics.iter().all(|d| d.message.len() <= 512));
    assert_eq!(ports.counts(), (0, 0, false));
}

#[test]
fn special_number_serialization_never_loses_nonfinite_or_signed_zero_identity() {
    assert_eq!(json!(Number::Finite(f64::NAN)), json!({"kind":"NaN"}));
    assert_eq!(
        json!(Number::Finite(f64::INFINITY)),
        json!({"kind":"PositiveInfinity"})
    );
    assert_eq!(json!(Number::Finite(-0.0)), json!({"kind":"NegativeZero"}));
    let mut data = json!({"values":{"channel":1,"note":{"toString":null},"velocity":1}});
    assert_eq!(
        prepare_send(&mut data).unwrap_err().code,
        "midi_unsupported"
    );
    let mut data = json!({"values":{"channel":1,"note":60,"velocity":1,"toString":null}});
    assert_eq!(
        prepare_send(&mut data).unwrap().packet().unwrap(),
        [0x90, 60, 1]
    );
}
