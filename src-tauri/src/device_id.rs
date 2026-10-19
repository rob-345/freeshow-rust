//! DEVICE_ID is a global CONFIG mutation, serialized on the existing library
//! owner. This host has no worker: its lease follows the actual accepted job.
use freeshow_core::{
    protocol::{Event, Request},
    service::LibraryService,
    Error, WireError,
};
use freeshow_system::{device_id as native, CancelToken, JobControl};
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const SLOTS: usize = 4;
const CONFIG_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, CancelToken>,
}

#[derive(Clone)]
pub struct DeviceIdentity {
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
}
struct Lease {
    id: u64,
    registry: Arc<Mutex<Registry>>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
    }
}
struct Caller(CancelToken);
impl Drop for Caller {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn failure(code: &'static str, message: &'static str) -> Error {
    Error::NativeOperation {
        code,
        message: message.into(),
    }
}
fn check(control: &JobControl) -> freeshow_core::Result<()> {
    if control.cancel.is_cancelled() {
        Err(failure(
            "device_id_cancelled",
            "Device identity request cancelled",
        ))
    } else if Instant::now() >= control.deadline {
        Err(failure(
            "device_id_deadline",
            "Device identity request deadline elapsed",
        ))
    } else {
        Ok(())
    }
}
fn resource() -> Error {
    failure(
        "device_id_resource_limit",
        "Global configuration exceeds device identity work limits",
    )
}
fn native_error(error: native::Error) -> Error {
    Error::NativeOperation {
        code: error.code,
        message: error.message,
    }
}

struct RawFields<'a>(Vec<(String, &'a serde_json::value::RawValue)>);
impl<'de> Deserialize<'de> for RawFields<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Fields;
        impl<'de> Visitor<'de> for Fields {
            type Value = RawFields<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("configuration object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields = Vec::new();
                while let Some(pair) = map.next_entry()? {
                    if fields.len() >= 32768 {
                        return Err(serde::de::Error::custom("configuration entry limit"));
                    }
                    fields.push(pair);
                }
                Ok(RawFields(fields))
            }
        }
        deserializer.deserialize_map(Fields)
    }
}
// Convert the bounded identity value's actual JSON numerals with Rust's
// correctly rounded binary64 parser. Default serde decimal parsing can differ
// by one ULP; a truthy numeric legacy ID must not silently change identity.
fn identity_value(
    raw: &serde_json::value::RawValue,
    depth: usize,
    nodes: &mut usize,
    control: &JobControl,
) -> freeshow_core::Result<Value> {
    check(control)?;
    *nodes += 1;
    let text = raw.get();
    if text.len() > 8192 || depth > 8 || *nodes > 256 {
        return Err(resource());
    }
    match text.as_bytes()[0] {
        b'{' => {
            let fields: RawFields<'_> = serde_json::from_str(text).map_err(|_| resource())?;
            let mut object = serde_json::Map::new();
            for (key, value) in fields.0 {
                *nodes += 1;
                object.insert(key, identity_value(value, depth + 1, nodes, control)?);
            }
            Ok(Value::Object(object))
        }
        b'[' => {
            let values: Vec<&serde_json::value::RawValue> =
                serde_json::from_str(text).map_err(|_| resource())?;
            if values.len() > 256 {
                return Err(resource());
            }
            values
                .into_iter()
                .map(|v| identity_value(v, depth + 1, nodes, control))
                .collect::<freeshow_core::Result<Vec<_>>>()
                .map(Value::Array)
        }
        b'-' | b'0'..=b'9' => {
            let number = text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .ok_or_else(|| {
                    failure(
                        "device_id_unsupported",
                        "Nonfinite configured device identity is not supported",
                    )
                })?;
            Ok(Value::Number(number))
        }
        _ => serde_json::from_str(text).map_err(|_| resource()),
    }
}

// serde_json's locked parser reports its finite recursion budget as a syntax
// category. That native work limit is not a genuine Conf/JSON.parse read failure.
fn depth_limit(error: &serde_json::Error) -> bool {
    error.to_string().starts_with("recursion limit exceeded")
}

// Open/read the actual owner-selected global file once, under the same serial
// owner used by SET_STORE_VALUE. Never precheck then reread an unbounded file.
fn config(
    path: &std::path::Path,
    control: &JobControl,
) -> freeshow_core::Result<Option<(Value, Option<Value>)>> {
    check(control)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some((serde_json::json!({}), None)))
        }
        Err(_) => return Ok(None),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return Ok(None),
    };
    if !metadata.is_file() {
        return Err(failure(
            "device_id_unsupported",
            "Global configuration is not a regular file",
        ));
    }
    if metadata.len() > CONFIG_BYTES as u64 {
        return Err(resource());
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(32768));
    loop {
        check(control)?;
        let mut chunk = [0; 32768];
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                if bytes.len() + count > CONFIG_BYTES {
                    return Err(resource());
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(None),
        }
    }
    check(control)?;
    // Conservative structural-work cap before allocating the general config
    // tree. Punctuation inside strings can also count toward this resource cap.
    if bytes
        .iter()
        .filter(|b| matches!(**b, b'{' | b'}' | b'[' | b']' | b',' | b':'))
        .count()
        > 32768
    {
        return Err(resource());
    }
    let fields: RawFields<'_> = match serde_json::from_slice(&bytes) {
        Ok(fields) => fields,
        Err(error) if depth_limit(&error) => return Err(resource()),
        Err(_) => return Ok(None),
    };
    let raw = fields
        .0
        .iter()
        .rev()
        .find(|(key, _)| key == "machineId")
        .map(|(_, value)| *value);
    let identity = raw
        .map(|raw| identity_value(raw, 0, &mut 0, control))
        .transpose()?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if value.is_object() => Ok(Some((value, identity))),
        Err(error) if depth_limit(&error) => Err(resource()),
        _ => Ok(None),
    }
}
struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > CONFIG_BYTES {
            return Err(std::io::Error::other("Device configuration limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl DeviceIdentity {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Mutex::new(Registry::default())),
            slots: Arc::new(Semaphore::new(SLOTS)),
        }
    }
    fn begin(&self, control: &JobControl) -> Result<Arc<Lease>, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "device_id_shutdown"
                } else {
                    "device_id_busy"
                },
                "Device identity admission is closed or at capacity",
            )
        })?;
        let mut registry = self.registry.lock().map_err(|_| {
            WireError::new("device_id_worker", "Device identity registry unavailable")
        })?;
        if registry.closed {
            return Err(WireError::new(
                "device_id_shutdown",
                "Device identity admission is closed",
            ));
        }
        let id = registry.next;
        registry.next = id.checked_add(1).ok_or_else(|| {
            WireError::new("device_id_worker", "Device identity sequence exhausted")
        })?;
        registry.active.insert(id, control.cancel.clone());
        Ok(Arc::new(Lease {
            id,
            registry: self.registry.clone(),
            _permit: permit,
        }))
    }
    /// Seal admission and request cooperative cancellation. Actual accepted
    /// jobs remain retained; the later LibraryService finish_writes fence must
    /// complete before native exit. This method is not a thread-join receipt.
    pub fn seal(&self) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.closed = true;
        self.slots.close();
        for token in registry.active.values() {
            token.cancel();
        }
    }
    pub async fn dispatch(
        &self,
        library: &LibraryService,
        mut request: Request,
        window: &str,
    ) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "device_id_role",
                "Only the main window may request device identity",
            ));
        }
        if request.channel != "MAIN" || request.data["channel"] != "DEVICE_ID" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown device identity command",
            ));
        }
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        if request
            .listener_id
            .as_ref()
            .is_some_and(|id| id.len() > 8192)
        {
            return Err(WireError::from(resource()));
        }
        // The source handler ignores its payload; do not retain it in CONFIG's
        // queue. Frozen context and original correlation remain unchanged.
        request.data = serde_json::json!({"channel":"DEVICE_ID"});
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(WireError::new(
                "device_id_runtime",
                "Device identity admission requires the native async runtime",
            ));
        }
        let control = JobControl {
            deadline: Instant::now() + Duration::from_secs(2),
            cancel: CancelToken::default(),
        };
        let deadline = control.deadline;
        let lease = self.begin(&control)?;
        let _observer_lease = lease.clone();
        let _caller = Caller(control.cancel.clone());
        let reply_request = Request {
            channel: request.channel.clone(),
            data: request.data.clone(),
            listener_id: request.listener_id.clone(),
            context: request.context.clone(),
        };
        let listener_id = request.listener_id.clone();
        let operation = library.commit_without_media_refresh(request, window, move |runtime| {
            let _lease = lease;
            check(&control)?;
            let mut configuration = config(&runtime.stores.path("config")?, &control)?;
            let snapshot = match configuration.as_ref() {
                Some((_, identity)) => native::ConfigSnapshot::Read {
                    value: identity.clone(),
                    can_set: true,
                },
                None => native::ConfigSnapshot::ReadFailed,
            };
            let prepared = native::prepare(snapshot, &control).map_err(native_error)?;
            let result = if let Some(intent) = prepared.write_intent() {
                check(&control)?;
                let (value, _) = configuration.as_mut().ok_or_else(|| {
                    failure(
                        "device_id_commit",
                        "Device identity lost its configuration snapshot",
                    )
                })?;
                value
                    .as_object_mut()
                    .unwrap()
                    .insert(intent.key().into(), Value::String(intent.value().into()));
                serde_json::to_writer_pretty(&mut Count(0), value).map_err(|_| resource())?;
                check(&control)?;
                let write = runtime.stores.set("config", value.clone());
                check(&control)?;
                if write.is_ok() {
                    native::WriteResult::Committed
                } else {
                    native::WriteResult::Failed
                }
            } else {
                native::WriteResult::NotRequired
            };
            let value = prepared.finish(result).map_err(native_error)?;
            check(&control)?;
            Ok(vec![Event::reply("MAIN", "DEVICE_ID", value, listener_id)])
        });
        let mut events = tokio::time::timeout_at(deadline.into(), operation)
            .await
            .map_err(|_| {
                WireError::new(
                    "device_id_deadline",
                    "Device identity observer deadline elapsed; accepted work remains retained",
                )
            })?
            .map_err(WireError::from)?;
        library
            .contextualize(&reply_request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    #[cfg(test)]
    #[allow(dead_code)] // Shared file also compiles in the binary unit target.
    pub fn retained(&self) -> usize {
        self.registry.lock().unwrap().active.len()
    }
}

impl Default for DeviceIdentity {
    fn default() -> Self {
        Self::new()
    }
}
