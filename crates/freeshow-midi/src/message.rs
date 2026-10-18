use crate::{
    bounded_json, bounded_name, Error, Hydration, MidiValue, Number, NumberField, WireData,
    MAX_MESSAGE_BYTES,
};
use serde::Serialize;
use serde_json::{json, Value};
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Control,
    NoteOn,
    NoteOff,
}
/// Source preparation is separate from packet validation: the original opens or
/// reuses its output before JZZ validates packet fields. Host must keep that order.
#[derive(Debug, Clone)]
pub struct SendPlan {
    pub output: Option<Value>,
    pub kind: MessageKind,
    pub channel: MidiValue,
    pub key: MidiValue,
    pub value: MidiValue,
    pub hydration: Hydration,
}
impl SendPlan {
    pub fn output_key(&self) -> Result<String, Error> {
        Ok(MidiValue::from_json(self.output.as_ref())?.text())
    }
    pub fn packet(&self) -> Result<[u8; 3], Error> {
        for value in [&self.channel, &self.key, &self.value] {
            if matches!(value,MidiValue::Text(text)|MidiValue::Coercible(text) if text.len()>crate::MAX_NAME_BYTES)
            {
                return Err(Error::resource());
            }
        }
        let channel = self.channel.integer(15, true)?;
        let key = if self.kind == MessageKind::Control {
            self.key.integer(127, false)?
        } else {
            self.key.note()?
        };
        let value = match (&self.value, self.kind) {
            (MidiValue::Undefined, MessageKind::NoteOn) => 127,
            (MidiValue::Undefined, MessageKind::NoteOff) => 64,
            _ => self.value.integer(127, false)?,
        };
        Ok([
            match self.kind {
                MessageKind::Control => 0xb0,
                MessageKind::NoteOn => 0x90,
                MessageKind::NoteOff => 0x80,
            } + channel,
            key,
            value,
        ])
    }
}
/// Mutates only the fields the original sendMidi mutates. Hydration describes
/// special values in that private mutated input, not a renderer acknowledgement.
pub fn prepare_send(data: &mut Value) -> Result<SendPlan, Error> {
    bounded_json(data)?;
    if !data.is_object() {
        return Err(Error::invalid("MIDI send payload must be an object"));
    }
    if !crate::truthy_json(data.get("type")) {
        data["type"] = json!("noteon");
    }
    if !crate::truthy_json(data.get("values")) {
        data["values"] = if data["type"] == "control" {
            json!({"controller":0,"value":0,"channel":1})
        } else {
            json!({"note":0,"velocity":0,"channel":1})
        };
    }
    let kind = if data["type"] == "control" {
        MessageKind::Control
    } else if data["type"] == "noteon" {
        MessageKind::NoteOn
    } else {
        MessageKind::NoteOff
    };
    let mut channel = MidiValue::from_json(data["values"].get("channel"))?;
    if channel.truthy() {
        channel = MidiValue::Number(channel.number() - 1.0);
        let value = channel.number();
        data["values"]["channel"] = if value.is_finite() {
            json!(if value == 0.0 { 0.0 } else { value })
        } else {
            Value::Null
        };
    }
    let mut hydration = Hydration::default();
    if let MidiValue::Number(value) = channel {
        let number = Number::new(value);
        if !matches!(number, Number::Finite(_)) {
            data["values"]["channel"] = if value == 0.0 { json!(0) } else { Value::Null };
            hydration.numbers.push(NumberField {
                path: "/values/channel".into(),
                number,
            });
        }
    }
    Ok(SendPlan {
        output: data.get("output").cloned(),
        kind,
        channel,
        key: MidiValue::from_json(data["values"].get(if kind == MessageKind::Control {
            "controller"
        } else {
            "note"
        }))?,
        value: MidiValue::from_json(data["values"].get(if kind == MessageKind::Control {
            "value"
        } else {
            "velocity"
        }))?,
        hydration,
    })
}
/// Preserve source ordering, duplicate names, Unicode and omission of extras.
/// Missing names retain an own undefined property via data-only hydration.
pub fn device_names(devices: &[Value]) -> Result<WireData, Error> {
    if devices.len() > 256 {
        return Err(Error::resource());
    }
    let mut data = Vec::with_capacity(devices.len());
    let mut hydration = Hydration::default();
    let mut bytes = 0usize;
    for (index, device) in devices.iter().enumerate() {
        if !device.is_object() {
            return Err(Error::invalid("MIDI device metadata must be an object"));
        }
        match device.get("name") {
            Some(name) => {
                bounded_json(name)?;
                if let Some(name) = name.as_str() {
                    bounded_name(name)?;
                    bytes = bytes.saturating_add(name.len());
                } else if name.is_array() || name.is_object() {
                    return Err(Error::invalid("MIDI device name must be a scalar"));
                } else {
                    bytes = bytes.saturating_add(8);
                }
                if bytes > crate::MAX_JSON_BYTES {
                    return Err(Error::resource());
                }
                data.push(json!({"name":name}));
            }
            None => {
                hydration.undefined_paths.push(format!("/{index}/name"));
                data.push(json!({}));
            }
        }
    }
    Ok(WireData {
        data: Value::Array(data),
        hydration,
    })
}
/// `description` is the actual backend/JZZ message string, including any label.
/// It is semantically significant: "Note"/"Off"/"Control" override raw status.
/// Native bytes alone are insufficient to reproduce arbitrary JZZ labels/SysEx.
pub fn receive(
    id: Option<&Value>,
    bytes: &[u8],
    description: &str,
) -> Result<Option<WireData>, Error> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::resource());
    }
    bounded_name(description)?;
    if let Some(id) = id {
        bounded_json(id)?;
    }
    let status = bytes.first().copied().unwrap_or(0);
    let channel = (status & 15) + 1;
    let type_code = status & 0xf0;
    let kind = if description.contains("Note") || matches!(type_code, 0x90 | 0x80) {
        if description.contains("Off") {
            MessageKind::NoteOff
        } else {
            MessageKind::NoteOn
        }
    } else if type_code == 0xb0 || description.contains("Control") {
        MessageKind::Control
    } else {
        return Ok(None);
    };
    let mut hydration = Hydration::default();
    let mut values = serde_json::Map::new();
    let (first, second) = if kind == MessageKind::Control {
        ("controller", "value")
    } else {
        ("note", "velocity")
    };
    for (index, key) in [(1, first), (2, second)] {
        if let Some(byte) = bytes.get(index) {
            values.insert(key.into(), json!(byte));
        } else {
            hydration.undefined_paths.push(format!("/values/{key}"));
        }
    }
    values.insert("channel".into(), json!(channel));
    let mut data = json!({"values":values,"type":kind});
    if let Some(id) = id {
        data["id"] = id.clone();
    } else {
        hydration.undefined_paths.insert(0, "/id".into());
    }
    Ok(Some(WireData { data, hydration }))
}
