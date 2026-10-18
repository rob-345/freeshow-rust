use super::*;
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0 && !v.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
pub(super) fn input_truthy(value: &JsInput) -> bool {
    match value {
        JsInput::Undefined => false,
        JsInput::Number(n) => *n != 0.0 && !n.is_nan(),
        JsInput::Json(v) => truthy(v),
    }
}
pub(super) fn string(value: &JsInput) -> Result<String> {
    match value {
        JsInput::Undefined => Ok("undefined".into()),
        JsInput::Number(n) => Ok(ryu_js::Buffer::new().format(*n).into()),
        JsInput::Json(v) => json_string(v, 0),
    }
}
fn json_string(value: &Value, depth: usize) -> Result<String> {
    if depth > 8 {
        return Err(Error::resource("Metadata coercion depth"));
    }
    Ok(match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => ryu_js::Buffer::new()
            .format(
                n.as_f64()
                    .ok_or_else(|| Error::new("unsupported_metadata", "Non IEEE JSON number"))?,
            )
            .into(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let mut result = String::new();
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    result.push(',');
                }
                if !v.is_null() {
                    result.push_str(&json_string(v, depth + 1)?);
                }
            }
            result
        }
        Value::Object(o) => {
            if o.contains_key("toString") || o.contains_key("valueOf") {
                return Err(Error::new(
                    "unsupported_metadata",
                    "Own coercion properties are unsupported",
                ));
            }
            "[object Object]".into()
        }
    })
}
fn whitespace(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn parse_number(text: &str) -> Result<f64> {
    let s = text.trim_matches(whitespace);
    if s.is_empty() {
        return Ok(0.0);
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return Ok(f64::INFINITY);
    }
    if s == "-Infinity" {
        return Ok(f64::NEG_INFINITY);
    }
    if s.starts_with("0x")
        || s.starts_with("0X")
        || s.starts_with("0b")
        || s.starts_with("0B")
        || s.starts_with("0o")
        || s.starts_with("0O")
    {
        let radix = match s.as_bytes()[1] {
            b'x' | b'X' => 16,
            b'b' | b'B' => 2,
            _ => 8,
        };
        let digits = &s[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii() && c.is_digit(radix)) {
            return Ok(f64::NAN);
        }
        return u128::from_str_radix(digits, radix)
            .map(|v| v as f64)
            .map_err(|_| Error::new("unsupported_metadata", "Radix integer exceeds128 bits"));
    }
    let mut bytes = s.bytes().peekable();
    if matches!(bytes.peek(), Some(b'+' | b'-')) {
        bytes.next();
    }
    let mut digits = 0;
    while bytes.peek().is_some_and(u8::is_ascii_digit) {
        bytes.next();
        digits += 1;
    }
    if bytes.peek() == Some(&b'.') {
        bytes.next();
        while bytes.peek().is_some_and(u8::is_ascii_digit) {
            bytes.next();
            digits += 1;
        }
    }
    if digits == 0 {
        return Ok(f64::NAN);
    }
    if matches!(bytes.peek(), Some(b'e' | b'E')) {
        bytes.next();
        if matches!(bytes.peek(), Some(b'+' | b'-')) {
            bytes.next();
        }
        let mut exponent = 0;
        while bytes.peek().is_some_and(u8::is_ascii_digit) {
            bytes.next();
            exponent += 1;
        }
        if exponent == 0 {
            return Ok(f64::NAN);
        }
    }
    if bytes.next().is_some() {
        return Ok(f64::NAN);
    }
    Ok(s.parse::<f64>().unwrap_or(f64::NAN))
}
pub(super) fn number(value: &JsInput) -> Result<f64> {
    match value {
        JsInput::Undefined => Ok(f64::NAN),
        JsInput::Number(n) => Ok(*n),
        JsInput::Json(Value::Null) => Ok(0.0),
        JsInput::Json(Value::Bool(b)) => Ok(if *b { 1.0 } else { 0.0 }),
        JsInput::Json(Value::Number(n)) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        JsInput::Json(v) => parse_number(&json_string(v, 0)?),
    }
}
pub(super) fn metadata(meta: &AudioMetadata, config: &PipelineConfig) -> Result<()> {
    fn visit(v: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > 8 || *nodes > 256 {
            return Err(Error::resource("Metadata node/depth limit"));
        }
        match v {
            Value::String(s) => *bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                for v in a {
                    visit(v, depth + 1, nodes, bytes)?;
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    *bytes = bytes.saturating_add(k.len());
                    visit(v, depth + 1, nodes, bytes)?;
                }
            }
            _ => *bytes += 16,
        }
        Ok(())
    }
    let mut nodes = 0;
    let mut bytes = 0;
    for v in [&meta.sample_rate, &meta.target_id] {
        if let JsInput::Json(v) = v {
            visit(v, 0, &mut nodes, &mut bytes)?;
        }
    }
    if let Some(v) = &meta.icecast {
        visit(v, 0, &mut nodes, &mut bytes)?;
    }
    if bytes > config.metadata_bytes {
        return Err(Error::resource("Metadata byte limit"));
    }
    Ok(())
}
