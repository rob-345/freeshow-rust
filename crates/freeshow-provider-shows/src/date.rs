use crate::{unsupported, Result, SourceValue};
const CLIP: f64 = 8_640_000_000_000_000.0;
fn clip(n: f64) -> f64 {
    if !n.is_finite() || n.abs() > CLIP {
        f64::NAN
    } else {
        let integer = n.trunc();
        if integer == 0.0 {
            0.0
        } else {
            integer
        }
    }
}
// Gregorian civil-day arithmetic, including ISO extended years. Intermediate
// bounds are fixed by the supported six-digit grammar; no timezone database.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    era * 146097 + (yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + day - 1) - 719468
}
fn digits(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}
pub(crate) fn parse(value: &SourceValue) -> Result<f64> {
    Ok(match value {
        SourceValue::Undefined => f64::NAN,
        SourceValue::Null => 0.0,
        SourceValue::Bool(v) => f64::from(*v),
        SourceValue::Number(n) => clip(*n),
        SourceValue::String(s) => iso(s)?,
        _ => {
            return Err(unsupported(
                "Date object/array coercion is outside the supported ISO/primitive scope",
            ))
        }
    })
}
fn iso(s: &str) -> Result<f64> {
    if s.is_empty() || s == "invalid" || s == "not a date" {
        return Ok(f64::NAN);
    }
    if !s.is_ascii() {
        return Err(unsupported(
            "Legacy or local-zone Date syntax is unsupported",
        ));
    }
    let (year, rest) = if s.starts_with(['+', '-']) {
        if s.len() < 8 {
            return Err(unsupported("Legacy Date syntax is unsupported"));
        }
        let Some(y) = digits(&s[1..7]) else {
            return Err(unsupported("Legacy Date syntax is unsupported"));
        };
        if s.as_bytes()[7] != b'-' || s.starts_with("-000000") {
            return Err(unsupported(
                "Legacy extended-year Date fallback is unsupported",
            ));
        }
        (if s.starts_with('-') { -y } else { y }, &s[8..])
    } else {
        if s.len() < 5 || s.as_bytes()[4] != b'-' {
            return Err(unsupported("Legacy Date syntax is unsupported"));
        }
        let Some(y) = digits(&s[..4]) else {
            return Err(unsupported("Legacy Date syntax is unsupported"));
        };
        (y, &s[5..])
    };
    if rest.len() < 5 || rest.as_bytes()[2] != b'-' {
        return Err(unsupported("Legacy Date syntax is unsupported"));
    }
    let (Some(month), Some(day)) = (digits(&rest[..2]), digits(&rest[3..5])) else {
        return Err(unsupported("Legacy Date syntax is unsupported"));
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Ok(f64::NAN);
    }
    let tail = &rest[5..];
    let (mut hour, mut minute, mut second, mut millis, mut offset) = (0i64, 0i64, 0i64, 0i64, 0i64);
    if !tail.is_empty() {
        if !tail.starts_with('T') {
            return Err(unsupported("Legacy Date syntax is unsupported"));
        }
        let time = &tail[1..];
        let (local, zone) = if let Some(t) = time.strip_suffix('Z') {
            (t, 0)
        } else {
            let Some(i) = time.find(['+', '-']) else {
                return Err(unsupported(
                    "Timezone-naive Date parsing requires the native local timezone",
                ));
            };
            let z = &time[i..];
            if z.len() != 6 || z.as_bytes()[3] != b':' {
                return Err(unsupported("Unsupported ISO timezone spelling"));
            }
            let (Some(h), Some(m)) = (digits(&z[1..3]), digits(&z[4..])) else {
                return Ok(f64::NAN);
            };
            if h > 23 || m > 59 {
                return Ok(f64::NAN);
            }
            (
                &time[..i],
                (h * 60 + m) * if z.starts_with('-') { -1 } else { 1 },
            )
        };
        offset = zone;
        let parts = local.split(':').collect::<Vec<_>>();
        if !(2..=3).contains(&parts.len()) || parts[0].len() != 2 || parts[1].len() != 2 {
            return Err(unsupported("Unsupported ISO clock spelling"));
        }
        let (Some(h), Some(m)) = (digits(parts[0]), digits(parts[1])) else {
            return Ok(f64::NAN);
        };
        hour = h;
        minute = m;
        if parts.len() == 3 {
            let mut sec = parts[2].split('.');
            let part = sec.next().unwrap_or("");
            if part.len() != 2 {
                return Err(unsupported("Unsupported ISO seconds spelling"));
            }
            let Some(n) = digits(part) else {
                return Ok(f64::NAN);
            };
            second = n;
            if let Some(fraction) = sec.next() {
                if fraction.is_empty()
                    || !fraction.bytes().all(|b| b.is_ascii_digit())
                    || sec.next().is_some()
                {
                    return Ok(f64::NAN);
                }
                let mut multiplier = 100;
                for ch in fraction.bytes().take(3) {
                    millis += i64::from(ch - b'0') * multiplier;
                    multiplier /= 10;
                }
            }
        }
        if hour > 24
            || minute > 59
            || second > 59
            || hour == 24 && (minute != 0 || second != 0 || millis != 0)
        {
            return Ok(f64::NAN);
        }
    }
    let result = days(year, month, day) * 86_400_000
        + hour * 3_600_000
        + minute * 60_000
        + second * 1000
        + millis
        - offset * 60_000;
    Ok(clip(result as f64))
}
