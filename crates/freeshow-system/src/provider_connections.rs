//! The pinned registry's stored-access indication, without provider IO.
//!
//! The host supplies a successful, bounded ACCESS store snapshot from its
//! trusted app-data owner. This does not authenticate, refresh, revoke, open an
//! OAuth listener, or establish that a provider backend is available.
use serde_json::{Map, Value};
use std::fmt;

/// Bounds apply to the whole snapshot, including unused providers and secrets.
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
pub const MAX_SNAPSHOT_NODES: usize = 16_384;
/// The root is depth zero. Containers at this depth may have no deeper values.
pub const MAX_SNAPSHOT_DEPTH: usize = 16;

// Registration order and each provider's supportedScopes, not OAuth scope
// tokenization. In particular, APlay and Canva use one combined scope key.
const PROVIDERS: &[(&str, &[&str])] = &[
    ("churchApps", &["plans"]),
    (
        "planningcenter",
        &[
            "calendar",
            "check_ins",
            "giving",
            "groups",
            "people",
            "publishing",
            "services",
        ],
    ),
    ("amazinglife", &["openid profile email"]),
    ("onstage", &["presenter"]),
    (
        "canva",
        &["folder:read design:content:read design:meta:read"],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionError {
    InvalidSnapshot,
    ByteLimit,
    NodeLimit,
    DepthLimit,
}
impl ProjectionError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidSnapshot => "provider_connections_invalid",
            Self::ByteLimit | Self::NodeLimit | Self::DepthLimit => "provider_connections_limit",
        }
    }
    /// Static diagnostics deliberately contain no store keys, tokens or values.
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidSnapshot => "ACCESS snapshot must be a JSON object",
            Self::ByteLimit => "ACCESS snapshot exceeds the JSON byte limit",
            Self::NodeLimit => "ACCESS snapshot exceeds the value node limit",
            Self::DepthLimit => "ACCESS snapshot exceeds the depth limit",
        }
    }
}
impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}
impl std::error::Error for ProjectionError {}

/// Project a whole, already parsed ACCESS store snapshot.
///
/// The result contains only present `true` keys in original registration order.
/// `serde_json/preserve_order` is required for that Value representation. No
/// access-token field or expiry is inspected: even an empty object or array at
/// an exact supported scope is truthy in the original JavaScript.
///
/// Wrong-shaped nested containers behave like the source's property lookups;
/// raw non-object snapshots and native resource rejections are errors, never
/// successful empty results. Missing/read/parse errors belong to the trusted
/// reader and must not be passed here as a fabricated empty object.
pub fn project(access_snapshot: &Value) -> Result<Value, ProjectionError> {
    let access = access_snapshot
        .as_object()
        .ok_or(ProjectionError::InvalidSnapshot)?;
    Budget::default().value(access_snapshot, 0)?;
    let stored = access.get("contentProviders").and_then(Value::as_object);
    let mut connections = Map::new();
    for &(provider, scopes) in PROVIDERS {
        let provider_data = stored
            .and_then(|stored| stored.get(provider))
            .and_then(Value::as_object);
        if scopes.iter().any(|scope| {
            provider_data
                .and_then(|data| data.get(*scope))
                .is_some_and(truthy)
        }) {
            connections.insert(provider.into(), Value::Bool(true));
        }
    }
    Ok(Value::Object(connections))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    nodes: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), ProjectionError> {
        if bytes > MAX_SNAPSHOT_BYTES - self.bytes {
            return Err(ProjectionError::ByteLimit);
        }
        self.bytes += bytes;
        Ok(())
    }
    fn string(&mut self, text: &str) -> Result<(), ProjectionError> {
        // UTF-8 length is a lower bound, so oversized values fail before any
        // whole-string scan or escaping allocation. JSON is counted in place.
        self.charge(2)?;
        if text.len() > MAX_SNAPSHOT_BYTES - self.bytes {
            return Err(ProjectionError::ByteLimit);
        }
        for byte in text.bytes() {
            self.charge(match byte {
                b'"' | b'\\' | 8 | 9 | 10 | 12 | 13 => 2,
                0..=31 => 6,
                _ => 1,
            })?;
        }
        Ok(())
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), ProjectionError> {
        if depth > MAX_SNAPSHOT_DEPTH {
            return Err(ProjectionError::DepthLimit);
        }
        if self.nodes == MAX_SNAPSHOT_NODES {
            return Err(ProjectionError::NodeLimit);
        }
        self.nodes += 1;
        match value {
            Value::Null => self.charge(4),
            Value::Bool(value) => self.charge(if *value { 4 } else { 5 }),
            Value::Number(value) => self.charge(value.to_string().len()),
            Value::String(value) => self.string(value),
            Value::Array(values) => {
                self.charge(2)?;
                for (index, value) in values.iter().enumerate() {
                    self.charge(usize::from(index != 0))?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                self.charge(2)?;
                for (index, (key, value)) in values.iter().enumerate() {
                    self.charge(usize::from(index != 0))?;
                    self.string(key)?;
                    self.charge(1)?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
        }
    }
}
