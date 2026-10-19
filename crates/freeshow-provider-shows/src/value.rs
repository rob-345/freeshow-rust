use crate::{limit, unsupported, Budget, Result, MAX_DEPTH, MAX_INPUT_BYTES, MAX_NODES};
use std::collections::BTreeSet;
/// Own undefined and all binary64 values remain typed, before any JSON projection.
/// Objects represent ordinary own enumerable properties, not JS prototypes/getters.
#[derive(Clone, Debug)]
pub enum SourceValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}
fn index(s: &str) -> Option<u32> {
    let n = s.parse::<u32>().ok()?;
    (n != u32::MAX && n.to_string() == s).then_some(n)
}
impl SourceValue {
    pub fn from_json(value: &serde_json::Value) -> Result<Self> {
        fn convert(
            v: &serde_json::Value,
            depth: usize,
            bytes: &mut usize,
            nodes: &mut usize,
        ) -> Result<SourceValue> {
            if depth > MAX_DEPTH || *nodes >= MAX_NODES {
                return Err(limit());
            }
            *nodes += 1;
            let add = match v {
                serde_json::Value::String(s) => s.len(),
                serde_json::Value::Object(o) => o.keys().map(String::len).sum(),
                _ => 0,
            };
            *bytes = bytes.checked_add(add).ok_or_else(limit)?;
            if *bytes > MAX_INPUT_BYTES {
                return Err(limit());
            }
            Ok(match v {
                serde_json::Value::Null => SourceValue::Null,
                serde_json::Value::Bool(b) => SourceValue::Bool(*b),
                serde_json::Value::Number(n) => SourceValue::Number(n.as_f64().ok_or_else(limit)?),
                serde_json::Value::String(s) => SourceValue::String(s.clone()),
                serde_json::Value::Array(a) => SourceValue::Array(
                    a.iter()
                        .map(|v| convert(v, depth + 1, bytes, nodes))
                        .collect::<Result<_>>()?,
                ),
                serde_json::Value::Object(o) => {
                    let fields = o
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), convert(v, depth + 1, bytes, nodes)?)))
                        .collect::<Result<_>>()?;
                    let mut out = SourceValue::Object(fields);
                    out.sort_keys();
                    out
                }
            })
        }
        convert(value, 0, &mut 0, &mut 0)
    }
    pub(crate) fn sort_keys(&mut self) {
        if let Self::Object(o) = self {
            o.sort_by(|(a, _), (b, _)| match (index(a), index(b)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            });
        }
    }
    pub(crate) fn validate(
        &self,
        depth: usize,
        bytes: &mut usize,
        nodes: &mut usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH || *nodes >= MAX_NODES {
            return Err(limit());
        }
        *nodes += 1;
        match self {
            Self::String(s) => *bytes = bytes.checked_add(s.len()).ok_or_else(limit)?,
            Self::Array(a) => {
                for v in a {
                    v.validate(depth + 1, bytes, nodes)?
                }
            }
            Self::Object(o) => {
                let mut keys = BTreeSet::new();
                for (k, v) in o {
                    if !keys.insert(k) {
                        return Err(unsupported(
                            "Duplicate own keys do not represent an ordinary source object",
                        ));
                    }
                    *bytes = bytes.checked_add(k.len()).ok_or_else(limit)?;
                    if *bytes > MAX_INPUT_BYTES {
                        return Err(limit());
                    }
                    v.validate(depth + 1, bytes, nodes)?;
                }
            }
            _ => {}
        }
        if *bytes > MAX_INPUT_BYTES {
            return Err(limit());
        }
        Ok(())
    }
    pub fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::String(s) => !s.is_empty(),
            Self::Array(_) | Self::Object(_) => true,
        }
    }
    pub(crate) fn js_string(&self, b: &Budget, depth: usize) -> Result<String> {
        if depth > MAX_DEPTH {
            return Err(limit());
        }
        let text = match self {
            Self::Undefined => "undefined".into(),
            Self::Null => "null".into(),
            Self::Bool(v) => v.to_string(),
            Self::Number(n) => ryu_js::Buffer::new().format(*n).to_owned(),
            Self::String(s) => {
                b.work(s.len())?;
                return Ok(s.clone());
            }
            Self::Object(o) => {
                if o.iter().any(|(k, _)| k == "toString" || k == "valueOf") {
                    return Err(unsupported("Custom primitive coercion is not supported"));
                }
                "[object Object]".into()
            }
            Self::Array(a) => {
                let mut text = String::new();
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        b.work(1)?;
                        text.push(',');
                    }
                    if !matches!(v, Self::Null | Self::Undefined) {
                        let s = v.js_string(b, depth + 1)?;
                        b.work(s.len())?;
                        text.push_str(&s);
                    }
                }
                return Ok(text);
            }
        };
        b.work(text.len())?;
        Ok(text)
    }
    /// Lossy JSON projection is separate from the source-shaped typed result.
    /// Own undefined is omitted for objects and null in arrays, like JSON.stringify.
    pub fn json_projection(&self) -> Result<serde_json::Value> {
        self.validate(0, &mut 0, &mut 0)?;
        fn project(v: &SourceValue, b: &Budget) -> Result<serde_json::Value> {
            Ok(match v {
                SourceValue::Undefined | SourceValue::Null => {
                    b.value(4)?;
                    serde_json::Value::Null
                }
                SourceValue::Bool(v) => {
                    b.value(5)?;
                    (*v).into()
                }
                SourceValue::Number(n) => {
                    b.value(32)?;
                    serde_json::Number::from_f64(if *n == 0.0 { 0.0 } else { *n })
                        .map_or(serde_json::Value::Null, serde_json::Value::Number)
                }
                SourceValue::String(s) => {
                    b.value(
                        s.len()
                            .checked_mul(6)
                            .and_then(|n| n.checked_add(2))
                            .ok_or_else(limit)?,
                    )?;
                    s.clone().into()
                }
                SourceValue::Array(a) => {
                    b.value(a.len() + 2)?;
                    b.work(a.len() * 32)?;
                    serde_json::Value::Array(
                        a.iter().map(|v| project(v, b)).collect::<Result<_>>()?,
                    )
                }
                SourceValue::Object(o) => {
                    b.value(o.iter().map(|(k, _)| k.len() * 6 + 4).sum::<usize>() + 2)?;
                    b.work(o.len() * 16)?;
                    let mut entries = o.iter().collect::<Vec<_>>();
                    entries.sort_by(|(a, _), (b, _)| match (index(a), index(b)) {
                        (Some(a), Some(b)) => a.cmp(&b),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        _ => std::cmp::Ordering::Equal,
                    });
                    let mut out = serde_json::Map::new();
                    for (k, v) in entries {
                        if !matches!(v, SourceValue::Undefined) {
                            out.insert(k.clone(), project(v, b)?);
                        }
                    }
                    serde_json::Value::Object(out)
                }
            })
        }
        project(self, &Budget::default())
    }
}
