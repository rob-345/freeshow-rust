//! Encoder-local binary64 conversion. `raw_value` does not change serde_json's
//! parser behavior in other crates; float_roundtrip/arbitrary_precision would.
use crate::EncoderError;
use serde::{
    de::{MapAccess, Visitor},
    Deserializer,
};
use serde_json::{value::RawValue, Map, Value};
use std::fmt;

fn invalid(error: impl fmt::Display) -> EncoderError {
    let message = error.to_string();
    let code = if message.contains("surrogate") || message.contains("unexpected end of hex escape")
    {
        "unsupported_utf16"
    } else {
        "invalid_json"
    };
    EncoderError::new(code, message)
}
pub(crate) fn parse(text: &str) -> Result<Value, EncoderError> {
    let raw: &RawValue = serde_json::from_str(text).map_err(invalid)?;
    convert(raw, 0)
}
fn convert(raw: &RawValue, depth: usize) -> Result<Value, EncoderError> {
    if depth >= 128 {
        return Err(EncoderError::new(
            "json_depth_limit",
            "Encoder JSON nesting exceeds 128 levels",
        ));
    }
    let text = raw.get();
    match text.as_bytes()[0] {
        b'{' => {
            // MapAccess preserves duplicate field order and lets each value be
            // converted independently; replacing an existing key retains its
            // original insertion position, just like JSON.parse.
            struct Object(usize);
            impl<'de> Visitor<'de> for Object {
                type Value = Result<Value, EncoderError>;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("JSON object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                    let mut values = Map::new();
                    while let Some((key, raw)) = map.next_entry::<String, &RawValue>()? {
                        let value = match convert(raw, self.0 + 1) {
                            Ok(value) => value,
                            Err(error) => return Ok(Err(error)),
                        };
                        values.insert(key, value);
                    }
                    Ok(Ok(Value::Object(values)))
                }
            }
            let mut decoder = serde_json::Deserializer::from_str(text);
            decoder.deserialize_map(Object(depth)).map_err(invalid)?
        }
        b'[' => {
            let values: Vec<&RawValue> = serde_json::from_str(text).map_err(invalid)?;
            values
                .into_iter()
                .map(|raw| convert(raw, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        b'"' => serde_json::from_str::<String>(text)
            .map(Value::String)
            .map_err(invalid),
        b'n' => Ok(Value::Null),
        b't' => Ok(Value::Bool(true)),
        b'f' => Ok(Value::Bool(false)),
        _ => {
            let number = text.parse::<f64>().map_err(invalid)?;
            serde_json::Number::from_f64(number)
                .map(Value::Number)
                .ok_or_else(|| {
                    EncoderError::new(
                        "json_number_overflow",
                        "Encoder JSON number exceeds finite binary64 range",
                    )
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_duplicate_positions_strings_and_exact_binary64() {
        let text = r#"{"b":1,"s":"123 \" 1e309","a":[1.0000000000000002,{"value":8.3163996793004788882e-275}],"b":9007199254740993,"z":-0}"#;
        let value = parse(text).unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["b", "s", "a", "z"]
        );
        assert_eq!(value["b"].as_f64(), Some(9007199254740992.0));
        assert_eq!(
            value["a"][1]["value"].as_f64(),
            Some("8.3163996793004788882e-275".parse().unwrap())
        );
        assert!(value["z"].as_f64().unwrap().is_sign_negative());
        assert_eq!(value["s"], "123 \" 1e309");
        assert_eq!(parse("1e309").unwrap_err().code, "json_number_overflow");
        assert_eq!(parse("[1,]").unwrap_err().code, "invalid_json");
    }
}
