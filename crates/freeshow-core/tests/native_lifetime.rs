use freeshow_core::protocol::{Event, NativeLifetime};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
struct Resource(Arc<AtomicUsize>);
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn native_resources_survive_event_clones_and_encoding_but_cannot_enter_from_json() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut event = Event::reply("MAIN", "AI_GET_BIN", json!([]), Some("lifetime".into()));
    event.native_lifetime = Some(NativeLifetime::new(Resource(drops.clone())));
    let copy = event.clone();
    let encoded = serde_json::to_value(&event).unwrap();
    assert!(encoded.get("nativeLifetime").is_none());
    assert_eq!(
        encoded,
        json!({"channel":"MAIN","message":{"channel":"AI_GET_BIN","data":[]},"listenerId":"lifetime"})
    );
    drop(event);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let mut injected = encoded;
    injected["nativeLifetime"] = json!({"untrusted":"ignored"});
    let decoded: Event = serde_json::from_value(injected).unwrap();
    assert!(decoded.native_lifetime.is_none());
    drop(copy);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
