use crate::*;
#[derive(Debug, Clone)]
pub enum Selector {
    Undefined,
    Json(Value),
    List(Vec<Selector>),
}
impl Selector {
    pub fn from_field(value: Option<&Value>) -> Result<Self> {
        if let Some(v) = value {
            bounded_value(v)?;
            Ok(Self::Json(v.clone()))
        } else {
            Ok(Self::Undefined)
        }
    }
    fn parts(&self) -> Result<Vec<&Selector>> {
        match self {
            Self::List(v) => {
                if v.len() > 32 || v.iter().any(|v| matches!(v, Self::List(_))) {
                    return Err(Error::limit());
                }
                Ok(v.iter().collect())
            }
            _ => Ok(vec![self]),
        }
    }
}
/// JZZ 1.9.5 `_filterList`, including its index-after-splice semantics. Function
/// and RegExp selectors are not representable by this trusted JSON boundary.
pub fn select_devices(selector: &Selector, devices: &[Device]) -> Result<Vec<usize>> {
    match selector {
        Selector::Json(value) => bounded_value(value)?,
        Selector::List(parts) => {
            if parts.len() > 32 {
                return Err(Error::limit());
            }
            for part in parts {
                match part {
                    Selector::Json(value) => bounded_value(value)?,
                    Selector::Undefined => (),
                    Selector::List(_) => return Err(Error::limit()),
                }
            }
        }
        Selector::Undefined => (),
    }
    if devices.len() > DEVICE_LIMIT {
        return Err(Error::limit());
    }
    let json_array;
    let owned;
    let parts = match selector {
        Selector::Json(Value::Array(v)) => {
            if v.len() > 32 {
                return Err(Error::limit());
            }
            json_array = v.iter().cloned().map(Selector::Json).collect::<Vec<_>>();
            json_array.iter().collect::<Vec<_>>()
        }
        _ => {
            owned = selector.parts()?;
            owned
        }
    };
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut rest = (0..devices.len()).collect::<Vec<_>>();
    let mut fallback = false;
    for part in parts {
        let value = match part {
            Selector::Undefined => {
                fallback = true;
                continue;
            }
            Selector::Json(v) => v,
            Selector::List(_) => return Err(Error::limit()),
        };
        bounded_value(value)?;
        // Reuse the foundation's exact JavaScript property-key coercion without
        // exposing/changing its private scalar helpers.
        let mut payload = serde_json::json!({"output":value});
        let text = freeshow_midi::prepare_send(&mut payload)?.output_key()?;
        if text.len() > 4096 {
            return Err(Error::limit());
        }
        let mut index = 0;
        while index < rest.len() {
            let device = &devices[rest[index]];
            let matches = text == index.to_string()
                || value.as_str() == Some(device.name.as_str())
                || value.get("name").and_then(Value::as_str) == Some(device.name.as_str());
            if matches {
                let original = rest.remove(index);
                if fallback {
                    after.push(original)
                } else {
                    before.push(original)
                }
            } else {
                index += 1;
            }
        }
    }
    if fallback {
        before.extend(rest);
        before.extend(after);
    }
    Ok(before)
}
