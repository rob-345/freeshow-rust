use crate::{Error, Limits, Result};
use base64::Engine;
use serde::Serialize;
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Config {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub mount: String,
    pub password: String,
    #[serde(skip)]
    password_key: String,
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn string(v: &Value, depth: usize) -> Result<String> {
    if depth > 8 {
        return Err(Error::new("resource_limit", "Config coercion depth"));
    }
    Ok(match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => ryu_js::Buffer::new()
            .format(n.as_f64().unwrap_or(f64::NAN))
            .into(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let mut s = String::new();
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                if !v.is_null() {
                    s.push_str(&string(v, depth + 1)?);
                }
            }
            s
        }
        Value::Object(o) => {
            if o.contains_key("toString") || o.contains_key("valueOf") {
                return Err(Error::new("unsupported_config", "Own coercion properties"));
            }
            "[object Object]".into()
        }
    })
}
fn number(v: Option<&Value>) -> Result<f64> {
    let Some(v) = v else { return Ok(f64::NAN) };
    if v.is_null() {
        return Ok(0.0);
    }
    if let Value::Bool(b) = v {
        return Ok(if *b { 1.0 } else { 0.0 });
    }
    let s = string(v, 0)?;
    let s = crate::ogg::js_trim(&s);
    if s.is_empty() {
        return Ok(0.0);
    }
    // Number accepts unsigned radix strings; signed radix strings become NaN.
    for (prefix, radix) in [("0x", 16), ("0b", 2), ("0o", 8)] {
        if s.to_ascii_lowercase().starts_with(prefix) {
            let digits = &s[2..];
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii() && c.is_digit(radix)) {
                return Ok(f64::NAN);
            }
            return u128::from_str_radix(digits, radix)
                .map(|n| n as f64)
                .map_err(|_| Error::new("unsupported_config", "Radix integer exceeds128 bits"));
        }
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return Ok(f64::INFINITY);
    }
    if s == "-Infinity" {
        return Ok(f64::NEG_INFINITY);
    }
    let mut b = s.bytes().peekable();
    if matches!(b.peek(), Some(b'+' | b'-')) {
        b.next();
    }
    let mut digits = 0;
    while b.peek().is_some_and(u8::is_ascii_digit) {
        b.next();
        digits += 1;
    }
    if b.peek() == Some(&b'.') {
        b.next();
        while b.peek().is_some_and(u8::is_ascii_digit) {
            b.next();
            digits += 1;
        }
    }
    if digits == 0 {
        return Ok(f64::NAN);
    }
    if matches!(b.peek(), Some(b'e' | b'E')) {
        b.next();
        if matches!(b.peek(), Some(b'+' | b'-')) {
            b.next();
        }
        let mut n = 0;
        while b.peek().is_some_and(u8::is_ascii_digit) {
            b.next();
            n += 1;
        }
        if n == 0 {
            return Ok(f64::NAN);
        }
    }
    if b.next().is_some() {
        return Ok(f64::NAN);
    }
    Ok(s.parse().unwrap_or(f64::NAN))
}
fn bound(v: &Value, max: usize) -> Result<()> {
    fn visit(v: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<()> {
        *nodes += 1;
        if *nodes > 256 || depth > 8 {
            return Err(Error::new("resource_limit", "Config nodes/depth"));
        }
        match v {
            Value::String(s) => *bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                for v in a {
                    visit(v, depth + 1, nodes, bytes)?
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    *bytes = bytes.saturating_add(k.len());
                    visit(v, depth + 1, nodes, bytes)?
                }
            }
            _ => *bytes += 16,
        }
        Ok(())
    }
    let mut nodes = 0;
    let mut bytes = 0;
    visit(v, 0, &mut nodes, &mut bytes)?;
    if bytes > max {
        Err(Error::new("resource_limit", "Config byte limit"))
    } else {
        Ok(())
    }
}
impl Config {
    pub fn parse(input: Option<&Value>, limits: &Limits) -> Result<Option<Self>> {
        let Some(v) = input.filter(|v| truthy(v)) else {
            return Ok(None);
        };
        bound(v, limits.metadata_bytes)?;
        let field = |key| v.get(key).filter(|v| truthy(v));
        let host = match field("host") {
            None => "localhost",
            Some(Value::String(s)) => s,
            Some(_) => return Err(Error::new("unsupported_config", "Truthy non-string host")),
        }
        .to_owned();
        let mount = match field("mount") {
            None => "/stream.opus",
            Some(Value::String(s)) => s,
            Some(_) => return Err(Error::new("unsupported_config", "Truthy non-string mount")),
        }
        .to_owned();
        let mut port = number(v.get("port"))?;
        if port == 0.0 || port.is_nan() {
            port = 8000.0;
        }
        if !port.is_finite() || port.fract() != 0.0 || !(1.0..=65535.0).contains(&port) {
            return Err(Error::new(
                "unsupported_config",
                "Port must resolve to1..65535",
            ));
        }
        let raw_password = field("password");
        if raw_password.is_some_and(|v| v.is_object() || v.is_array()) {
            return Err(Error::new(
                "unsupported_config",
                "Object password identity/coercion",
            ));
        }
        let password = raw_password
            .map(|v| string(v, 0))
            .transpose()?
            .unwrap_or_default();
        let password_key = match raw_password {
            Some(Value::Bool(_)) => format!("b:{password}"),
            Some(Value::Number(_)) => format!("n:{password}"),
            _ => format!("s:{password}"),
        };
        let config = Self {
            enabled: field("enabled").is_some(),
            host,
            port: port as u16,
            mount,
            password,
            password_key,
        };
        if config.host.len() > 1024
            || config
                .host
                .bytes()
                .any(|b| b == 0 || b == b'\r' || b == b'\n')
            || config
                .mount
                .bytes()
                .any(|b| b == 0 || b == b'\r' || b == b'\n')
        {
            return Err(Error::new(
                "unsupported_config",
                "Host/mount control characters or size",
            ));
        }
        config.request_header("", limits)?;
        Ok(Some(config))
    }
    pub fn request_header(&self, song: &str, limits: &Limits) -> Result<Vec<u8>> {
        if song.len() > limits.metadata_bytes
            || song.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
        {
            return Err(Error::new(
                "unsupported_metadata",
                "Song title size/control characters",
            ));
        }
        let mount = if self.mount.starts_with('/') {
            self.mount.clone()
        } else {
            format!("/{}", self.mount)
        };
        let auth =
            base64::engine::general_purpose::STANDARD.encode(format!("source:{}", self.password));
        let mut header=format!("PUT {mount} HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Basic {auth}\r\nContent-Type: audio/ogg; codecs=opus\r\nIce-Name: FreeShow Audio Stream\r\nIce-Description: Live Audio Stream from FreeShow\r\nIce-Genre: Live\r\nIce-Public: 0\r\nExpect: 100-continue\r\nUser-Agent: FreeShow/1.0\r\n",self.host,self.port);
        if !song.is_empty() {
            header.push_str(&format!("Ice-Title: {song}\r\n"));
        }
        header.push_str("\r\n");
        if header.len() > limits.header_bytes {
            return Err(Error::new("resource_limit", "Request header limit"));
        }
        Ok(header.into_bytes())
    }
    pub(crate) fn storage(&self) -> usize {
        self.host.len() + self.mount.len() + self.password.len() + self.password_key.len() + 32
    }
}
