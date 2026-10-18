//! Output routing and display placement without filesystem or GUI dependencies.
use crate::{show::truthy, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub follower: bool,
    #[serde(default = "yes")]
    pub available: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub target: String,
    pub message: Value,
}

/// This is OutputHelper's MAIN routing and OutputSend's fanout behavior. Native
/// lifecycle commands must be intercepted before calling this function.
pub fn route(message: &Value, endpoints: &[Endpoint]) -> Result<Vec<Delivery>> {
    let channel = message["channel"]
        .as_str()
        .ok_or_else(|| Error::Invalid("output message channel must be a string".into()))?;
    if channel.contains("MAIN") {
        return Ok(vec![Delivery {
            target: "main".into(),
            message: message.clone(),
        }]);
    }
    let mut deliveries = Vec::new();
    let mut ordered = endpoints.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|output| {
        output
            .id
            .parse::<u32>()
            .ok()
            .filter(|index| *index != u32::MAX && index.to_string() == output.id)
            .unwrap_or(u32::MAX)
    });
    for output in ordered {
        if output.follower || !output.available {
            continue;
        }
        if message["data"]
            .get("id")
            .filter(|id| truthy(id))
            .is_some_and(|id| id != &json!(output.id))
        {
            continue;
        }
        let mut copy = message.clone();
        if channel == "OUTPUTS" {
            if let Some(value) = property(&message["data"], &output.id)?.filter(truthy) {
                copy["data"] = json!({ &output.id: value });
            }
        }
        deliveries.push(Delivery {
            target: output.label.clone(),
            message: copy,
        });
    }
    Ok(deliveries)
}

fn property(value: &Value, key: &str) -> Result<Option<Value>> {
    if let Some(value) = value.get(key) {
        return Ok(Some(value.clone()));
    }
    if key == "length" {
        return Ok(match value {
            Value::Array(values) => Some(json!(values.len())),
            Value::String(text) => Some(json!(text.encode_utf16().count())),
            _ => None,
        });
    }
    let Some(index) = key
        .parse::<usize>()
        .ok()
        .filter(|index| index.to_string() == key)
    else {
        return Ok(None);
    };
    Ok(match value {
        Value::Array(values) => values.get(index).cloned(),
        Value::String(text) => text
            .encode_utf16()
            .nth(index)
            .map(|unit| {
                char::from_u32(unit as u32)
                    .map(|character| json!(character.to_string()))
                    .ok_or_else(|| {
                        Error::Invalid(
                            "output property contains an isolated UTF-16 surrogate".into(),
                        )
                    })
            })
            .transpose()?,
        _ => None,
    })
}

pub fn new_label() -> String {
    format!("output-{}", uuid::Uuid::new_v4())
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn validate(&self) -> Result<()> {
        if ![self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(Error::Invalid(
                "output bounds must have positive finite dimensions".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Display {
    pub id: String,
    pub bounds: Rect,
}

pub fn amount_covered(display: Rect, window: Rect) -> f64 {
    let x = (display.x + display.width).min(window.x + window.width) - display.x.max(window.x);
    let y = (display.y + display.height).min(window.y + window.height) - display.y.max(window.y);
    x.max(0.0) * y.max(0.0) / (display.width * display.height)
}

pub fn resolve_bounds(
    output: &Value,
    displays: &[Display],
    main: Rect,
    auto_position: bool,
    is_mac: bool,
) -> Result<Rect> {
    let primary = displays
        .first()
        .map(|display| display.bounds)
        .unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        });
    let has_bounds = truthy(&output["bounds"]["width"]) && truthy(&output["bounds"]["height"]);
    let bounds: Rect = if has_bounds {
        serde_json::from_value(output["bounds"].clone())?
    } else {
        primary
    };
    if output["boundsLocked"] == true {
        return Ok(bounds);
    }
    if has_bounds
        && displays.iter().any(|display| {
            let center_x = bounds.x + bounds.width / 2.0;
            let center_y = bounds.y + bounds.height / 2.0;
            center_x >= display.bounds.x
                && center_x < display.bounds.x + display.bounds.width
                && center_y >= display.bounds.y
                && center_y < display.bounds.y + display.bounds.height
        })
    {
        return Ok(bounds);
    }
    if truthy(&output["screen"]) && amount_covered(bounds, main) <= 0.5 {
        if let Some(display) = displays
            .iter()
            .find(|display| output["screen"] == display.id)
        {
            return Ok(display.bounds);
        }
    }
    if (auto_position || !has_bounds) && displays.len() == 2 && !is_mac {
        return Ok(if amount_covered(displays[1].bounds, main) > 0.5 {
            displays[0].bounds
        } else {
            displays[1].bounds
        });
    }
    Ok(bounds)
}
