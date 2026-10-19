//! Pure, bounded representation planning for the original fixed PNG route.
//!
//! Inputs are native observations, never authority to open a file. The caller
//! supplies normalized Node HTTP request headers and owns the descriptor, stat
//! validity, response rendering and actual transfer. There is no IO or task here.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use std::{collections::BTreeMap, fmt};

pub const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_HEADERS: usize = 32;
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_VALUE_BYTES: usize = 8192;
pub const MAX_RANGES: usize = 64;
pub const MAX_RAW_TARGET_BYTES: usize = 8192;
pub const MAX_METHOD_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
fn error(code: &'static str, message: &'static str) -> Error {
    Error { code, message }
}

/// Node fs.Stats projection, including the Date's rounded millisecond value.
/// Native only: this value grants no filesystem or response-body authority.
#[derive(Debug, Clone, Copy)]
pub struct SourceFileStat {
    length: u64,
    mtime: DateTime<Utc>,
}

impl SourceFileStat {
    pub fn new(length: u64, mtime_ms: f64) -> Result<Self, Error> {
        if length > MAX_BODY_BYTES {
            return Err(error("cover_body_limit", "source file exceeds 4 MiB"));
        }
        if !mtime_ms.is_finite() {
            return Err(error("cover_mtime", "mtime must be finite"));
        }
        // JS Math.round: ties toward +infinity, including negative half values.
        // Avoid (x + 0.5).floor(), which can round large integers incorrectly.
        let floor = mtime_ms.floor();
        let rounded = if mtime_ms - floor >= 0.5 {
            floor + 1.0
        } else {
            floor
        };
        if !(-11_644_473_600_000.0..=253_402_300_799_999.0).contains(&rounded) {
            return Err(error("cover_mtime", "supported mtime years are 1601..9999"));
        }
        let mtime = DateTime::from_timestamp_millis(rounded as i64)
            .ok_or_else(|| error("cover_mtime", "invalid native mtime"))?;
        Ok(Self { length, mtime })
    }
    pub fn length(&self) -> u64 {
        self.length
    }
    pub fn etag(&self) -> String {
        let ms = self.mtime.timestamp_millis();
        let time = if ms < 0 {
            format!("-{:x}", ms.unsigned_abs())
        } else {
            format!("{ms:x}")
        };
        format!("W/\"{:x}-{time}\"", self.length)
    }
    pub fn last_modified(&self) -> String {
        self.mtime.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SourceState {
    /// Only the original existence guard's false outcome, not a sendFile error.
    Missing,
    File(SourceFileStat),
    /// A stat/read/authorization/nonregular-file failure needs actual host policy.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Head,
}

/// Recognition only: fallthrough retains authority with the original remaining
/// index/static/router chain. OPTIONS must NOT invoke any fixed-file lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Route {
    Read { method: Method },
    AutomaticOptions,
    Fallthrough,
}

/// Original case-insensitive, non-strict route on an untouched origin-form
/// request target. No percent decoding, slash normalization or file selection.
/// The native HTTP parser must already have validated the uppercase method.
/// Legacy URL-parser inputs are explicit unsupported capabilities.
pub fn match_route(method: &str, raw_target: &str) -> Result<Route, Error> {
    if method.len() > MAX_METHOD_BYTES || raw_target.len() > MAX_RAW_TARGET_BYTES {
        return Err(error(
            "cover_route_limit",
            "method or raw request target exceeds limit",
        ));
    }
    if method.is_empty()
        || !method.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
    {
        return Err(error(
            "cover_route_method",
            "requires native HTTP-parsed uppercase method token",
        ));
    }
    if !raw_target.starts_with('/')
        || !raw_target
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'#')
    {
        return Err(error(
            "cover_route_unsupported",
            "requires visible ASCII origin-form target without fragment or legacy URL parsing",
        ));
    }
    let pathname = raw_target
        .split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(raw_target);
    if !(pathname.eq_ignore_ascii_case("/nowplayingcover")
        || pathname.eq_ignore_ascii_case("/nowplayingcover/"))
    {
        return Ok(Route::Fallthrough);
    }
    Ok(match method {
        "GET" => Route::Read {
            method: Method::Get,
        },
        "HEAD" => Route::Read {
            method: Method::Head,
        },
        "OPTIONS" => Route::AutomaticOptions,
        _ => Route::Fallthrough,
    })
}

/// Original automatic OPTIONS for this route, after later index/static
/// middleware falls through. Independent of file existence, range/conditions,
/// path helper side effects and PNG descriptor preparation.
pub fn options_plan() -> Plan {
    Plan {
        status: 200,
        headers: BTreeMap::from([
            ("x-powered-by".into(), "Express".into()),
            ("allow".into(), "GET,HEAD".into()),
            ("content-type".into(), "text/html; charset=utf-8".into()),
            ("content-length".into(), "8".into()),
            // Default Express weak etag1.8.1 of the fixed UTF8 GET,HEAD body.
            ("etag".into(), "W/\"8-ZRAf8oNBS3Bjb/SU2GYZCmbtmXg\"".into()),
        ]),
        body: Body::Options,
    }
}

/// Already normalized incoming headers (lowercase names, Node duplicate joining
/// and HTTP OWS trimming performed by the host). Duplicate names are rejected.
#[derive(Debug, Default)]
pub struct Headers(BTreeMap<String, String>);
impl Headers {
    pub fn new<I, K, V>(pairs: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut map = BTreeMap::new();
        let mut bytes = 0usize;
        for (index, (name, value)) in pairs.into_iter().enumerate() {
            if index >= MAX_HEADERS {
                return Err(error("cover_header_limit", "too many headers"));
            }
            let name = name.as_ref();
            let value = value.as_ref();
            // Check byte lengths BEFORE scanning tokens/characters or copying.
            bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
            if value.len() > MAX_VALUE_BYTES || bytes > MAX_HEADER_BYTES {
                return Err(error("cover_header_limit", "header bytes exceed limits"));
            }
            if name.is_empty()
                || !name.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
                })
            {
                return Err(error(
                    "cover_header",
                    "header names must be normalized lowercase tokens",
                ));
            }
            // Node HTTP/1 Latin-1 projection; no invalid controls or CR/LF.
            if value
                .chars()
                .any(|c| (c < ' ' && c != '\t') || c == '\u{7f}' || c as u32 > 255)
            {
                return Err(error("cover_header", "invalid Node HTTP header value"));
            }
            if map.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err(error(
                    "cover_header",
                    "host must project Node duplicate header semantics",
                ));
            }
        }
        Ok(Self(map))
    }
    fn get(&self, name: &str) -> &str {
        self.0.get(name).map(String::as_str).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ErrorRendering {
    /// Original default development finalhandler includes environment-dependent
    /// stacks. Caller MUST render before responding; known headers are partial.
    #[default]
    Deferred,
    /// Explicit Express NODE_ENV=production policy, never inferred from release.
    Production,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    Empty,
    /// Exactly the eight ASCII bytes GET,HEAD (not a source command ACK).
    Options,
    File {
        offset: u64,
        length: u64,
    },
    ProductionError {
        html: String,
    },
    /// Not a complete HTTP response. Content-Length/body need the host renderer.
    DeferredError {
        status: u16,
        head_only: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub status: u16,
    /// All stable source headers, lowercase. Transport Date/connection excluded.
    pub headers: BTreeMap<String, String>,
    pub body: Body,
}

pub fn plan(
    source: SourceState,
    method: Method,
    request: &Headers,
    errors: ErrorRendering,
) -> Result<Plan, Error> {
    let mut headers = BTreeMap::from([("x-powered-by".to_owned(), "Express".to_owned())]);
    let stat = match source {
        SourceState::Missing => {
            // Node's res.status(404).end(): automatic zero Content-Length for
            // GET, but not HEAD (as observed with original handler + Node22).
            if method == Method::Get {
                headers.insert("content-length".into(), "0".into());
            }
            return Ok(Plan {
                status: 404,
                headers,
                body: Body::Empty,
            });
        }
        SourceState::Failed => {
            return Err(error(
                "cover_source_failure",
                "source failure is distinct from the absent-file guard",
            ))
        }
        SourceState::File(stat) => stat,
    };
    let etag = stat.etag();
    headers.extend([
        ("accept-ranges".into(), "bytes".into()),
        ("cache-control".into(), "public, max-age=0".into()),
        ("last-modified".into(), stat.last_modified()),
        ("etag".into(), etag.clone()),
    ]);
    let modified_second = stat.mtime.timestamp().saturating_mul(1000);
    let matches = request.get("if-match");
    let unmodified = request.get("if-unmodified-since");
    let none = request.get("if-none-match");
    let modified = request.get("if-modified-since");
    let failed = if !matches.is_empty() {
        matches != "*" && !etag_matches(matches, &etag)
    } else if !unmodified.is_empty() {
        parse_date(unmodified)?.is_some_and(|date| modified_second > date)
    } else {
        false
    };
    if failed {
        return Ok(error_plan(412, headers, method, errors));
    }
    if (!none.is_empty() || !modified.is_empty()) && !no_cache(request.get("cache-control")) {
        let mut fresh = true;
        if !none.is_empty() && none != "*" && !etag_matches(none, &etag) {
            fresh = false;
        }
        if fresh && !modified.is_empty() {
            fresh = parse_date(modified)?.is_some_and(|date| modified_second <= date);
        }
        if fresh {
            return Ok(Plan {
                status: 304,
                headers,
                body: Body::Empty,
            });
        }
    }
    let mut status = 200;
    let mut offset = 0;
    let mut length = stat.length;
    let range = request.get("range").trim_start_matches(' ');
    if let Some(value) = range.strip_prefix("bytes=") {
        let ranges = parse_ranges(stat.length, value)?;
        let if_range = request.get("if-range");
        let range_fresh = if if_range.is_empty() {
            true
        } else if if_range.contains('"') {
            if_range.contains(&etag)
        } else {
            parse_date(if_range)?.is_some_and(|date| modified_second <= date)
        };
        if range_fresh {
            if ranges.is_empty() {
                headers.insert("content-range".into(), format!("bytes */{}", stat.length));
                return Ok(error_plan(416, headers, method, errors));
            }
            if ranges.len() == 1 {
                let (start, end) = ranges[0];
                status = 206;
                offset = start;
                length = end - start + 1;
                headers.insert(
                    "content-range".into(),
                    format!("bytes {start}-{end}/{}", stat.length),
                );
            }
        }
    }
    headers.insert("content-type".into(), "image/png".into());
    headers.insert("content-length".into(), length.to_string());
    let body = if method == Method::Head || length == 0 {
        Body::Empty
    } else {
        Body::File { offset, length }
    };
    Ok(Plan {
        status,
        headers,
        body,
    })
}

fn error_plan(
    status: u16,
    mut headers: BTreeMap<String, String>,
    method: Method,
    mode: ErrorRendering,
) -> Plan {
    // finalhandler clears then re-applies 416's err.headers Content-Range.
    headers.insert(
        "content-security-policy".into(),
        "default-src 'none'".into(),
    );
    headers.insert("x-content-type-options".into(), "nosniff".into());
    headers.insert("content-type".into(), "text/html; charset=utf-8".into());
    let body = match mode {
        ErrorRendering::Deferred => Body::DeferredError {
            status,
            head_only: method == Method::Head,
        },
        ErrorRendering::Production => {
            let message = if status == 412 {
                "Precondition Failed"
            } else {
                "Range Not Satisfiable"
            };
            let html = format!("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>{message}</pre>\n</body>\n</html>\n");
            headers.insert("content-length".into(), html.len().to_string());
            if method == Method::Head {
                Body::Empty
            } else {
                Body::ProductionError { html }
            }
        }
    };
    Plan {
        status,
        headers,
        body,
    }
}

fn etag_matches(value: &str, etag: &str) -> bool {
    value
        .split(',')
        .map(|token| token.trim_matches(' '))
        .any(|token| {
            token == etag
                || token.strip_prefix("W/") == Some(etag)
                || etag.strip_prefix("W/") == Some(token)
        })
}

fn js_space(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn no_cache(value: &str) -> bool {
    value
        .split(',')
        .any(|part| part.trim_matches(js_space) == "no-cache")
}

fn parse_int(value: &str) -> f64 {
    let value = value.trim_start_matches(js_space);
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    let length = unsigned.bytes().take_while(u8::is_ascii_digit).count();
    if length == 0 {
        return f64::NAN;
    }
    let signed_length = length + value.len() - unsigned.len();
    value[..signed_length].parse::<f64>().unwrap_or(f64::NAN)
}

fn parse_ranges(size: u64, value: &str) -> Result<Vec<(u64, u64)>, Error> {
    let mut ranges = Vec::new();
    for (index, member) in value.split(',').enumerate() {
        if index >= MAX_RANGES {
            return Err(error("cover_range_limit", "too many range members"));
        }
        let mut parts = member.split('-');
        let mut start = parse_int(parts.next().unwrap_or(""));
        let mut end = parse_int(parts.next().unwrap_or(""));
        if start.is_nan() {
            start = size as f64 - end;
            end = size as f64 - 1.0;
        } else if end.is_nan() {
            end = size as f64 - 1.0;
        }
        if end > size as f64 - 1.0 {
            end = size as f64 - 1.0;
        }
        if start.is_nan() || end.is_nan() || start > end || start < 0.0 {
            continue;
        }
        ranges.push((start as u64, end as u64));
    }
    ranges.sort_unstable();
    let mut combined: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some(last) = combined.last_mut().filter(|last| start <= last.1 + 1) {
            last.1 = last.1.max(end);
        } else {
            combined.push((start, end));
        }
    }
    // Source restores input order, but only the singleton vs multiple distinction
    // is consumed by send. Multiple disjoint ranges produce the full response.
    Ok(combined)
}

/// Bounded, deliberately narrow V8 Date.parse capability. IMF-fixdate with
/// explicit GMT/UTC and years 1601..9999 is supported, including mismatched
/// weekday and normalized day 29..31. Other digit-bearing spellings return an
/// explicit capability error rather than silently changing source decisions.
fn parse_date(value: &str) -> Result<Option<i64>, Error> {
    if value.is_ascii() && !value.bytes().any(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let unsupported = || {
        error(
            "cover_date_unsupported",
            "conditional date requires IMF-fixdate with explicit GMT/UTC (1601..9999)",
        )
    };
    let parts: Vec<_> = value.split(' ').collect();
    if parts.len() != 6
        || parts[0].len() != 4
        || !parts[0].ends_with(',')
        || parts[1].len() != 2
        || parts[2].len() != 3
        || parts[3].len() != 4
        || parts[4].len() != 8
        || !(parts[5].eq_ignore_ascii_case("GMT") || parts[5].eq_ignore_ascii_case("UTC"))
    {
        return Err(unsupported());
    }
    if !["Mon,", "Tue,", "Wed,", "Thu,", "Fri,", "Sat,", "Sun,"]
        .iter()
        .any(|day| parts[0].eq_ignore_ascii_case(day))
    {
        return Err(unsupported());
    }
    if !parts[1].bytes().all(|b| b.is_ascii_digit())
        || !parts[3].bytes().all(|b| b.is_ascii_digit())
        || parts[4].as_bytes()[2] != b':'
        || parts[4].as_bytes()[5] != b':'
        || !parts[4]
            .bytes()
            .enumerate()
            .all(|(index, b)| index == 2 || index == 5 || b.is_ascii_digit())
    {
        return Err(unsupported());
    }
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = months
        .iter()
        .position(|month| parts[2].eq_ignore_ascii_case(month))
        .map(|n| n as u32 + 1);
    let year = parts[3].parse::<i32>().ok();
    let day = parts[1].parse::<u32>().ok();
    let time: Vec<_> = parts[4].split(':').map(str::parse::<u32>).collect();
    let (Some(month), Some(year), Some(day), [Ok(hour), Ok(minute), Ok(second)]) =
        (month, year, day, time.as_slice())
    else {
        return Ok(None);
    };
    if !(1601..=9999).contains(&year) {
        return Err(unsupported());
    }
    if !(1..=31).contains(&day) || *hour > 23 || *minute > 59 || *second > 59 {
        return Ok(None);
    }
    let date = NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|date| date.checked_add_signed(Duration::days((day - 1) as i64)))
        .and_then(|date| date.and_hms_opt(*hour, *minute, *second));
    Ok(date.map(|date| date.and_utc().timestamp_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file() -> SourceState {
        SourceState::File(SourceFileStat::new(20, 1_700_000_000_123.5).unwrap())
    }
    fn request(pairs: &[(&str, &str)]) -> Result<Plan, Error> {
        plan(
            file(),
            Method::Get,
            &Headers::new(pairs.iter().copied())?,
            ErrorRendering::Deferred,
        )
    }
    #[test]
    fn native_stat_math_round_and_pre_epoch() {
        for (ms, expected) in [(-1.5, "-1"), (-0.5, "0"), (0.5, "1"), (1.5, "2")] {
            let stat = SourceFileStat::new(20, ms).unwrap();
            assert_eq!(stat.etag(), format!("W/\"14-{expected}\""));
        }
        assert_eq!(
            SourceFileStat::new(1, -1.0).unwrap().last_modified(),
            "Wed, 31 Dec 1969 23:59:59 GMT"
        );
        assert!(SourceFileStat::new(MAX_BODY_BYTES + 1, 0.0).is_err());
        assert!(SourceFileStat::new(1, f64::INFINITY).is_err());
    }
    #[test]
    fn source_range_quirks_and_combine() {
        for range in ["bytes=-999", "bytes=-0", "bytes=", "bytes=garbage"] {
            assert_eq!(request(&[("range", range)]).unwrap().status, 416, "{range}");
        }
        for range in ["bytes=0-2,3-4", "bytes=3-4,0-2", "bytes=0-3,2-4"] {
            assert_eq!(
                request(&[("range", range)]).unwrap().body,
                Body::File {
                    offset: 0,
                    length: 5
                }
            );
        }
        assert_eq!(
            request(&[("range", "bytes=bad,1.5-3.8")]).unwrap().body,
            Body::File {
                offset: 1,
                length: 3
            }
        );
        assert_eq!(request(&[("range", "bytes=0-1,8-9")]).unwrap().status, 200);
        assert_eq!(
            request(&[("range", "bytes=-999"), ("if-range", "invalid")])
                .unwrap()
                .status,
            200
        );
    }
    #[test]
    fn precedence_skips_unsupported_unused_dates() {
        let etag = SourceFileStat::new(20, 1_700_000_000_123.5).unwrap().etag();
        assert_eq!(
            request(&[("if-match", &etag), ("if-unmodified-since", "2020-01-01")])
                .unwrap()
                .status,
            200
        );
        assert_eq!(
            request(&[
                ("if-none-match", "wrong"),
                ("if-modified-since", "2020-01-01")
            ])
            .unwrap()
            .status,
            200
        );
        assert_eq!(
            request(&[
                ("if-modified-since", "2020-01-01"),
                ("cache-control", "no-cache")
            ])
            .unwrap()
            .status,
            200
        );
        assert_eq!(
            request(&[("if-modified-since", "2020-01-01")])
                .unwrap_err()
                .code,
            "cover_date_unsupported"
        );
    }
    #[test]
    fn strict_budgets_and_header_projection() {
        assert_eq!(
            Headers::new([("Range", "bytes=1-2")]).unwrap_err().code,
            "cover_header"
        );
        assert!(Headers::new([("range", "a"), ("range", "b")]).is_err());
        assert!(Headers::new([("range", "\r\nfoo")]).is_err());
        assert!(Headers::new([("range", "漢字")]).is_err());
        assert_eq!(
            Headers::new([("range", "x".repeat(MAX_VALUE_BYTES + 1))])
                .unwrap_err()
                .code,
            "cover_header_limit"
        );
        assert_eq!(
            Headers::new([("x".repeat(MAX_HEADER_BYTES + 1), "x")])
                .unwrap_err()
                .code,
            "cover_header_limit"
        );
        assert_eq!(
            Headers::new([("a", "x".repeat(8192)), ("b", "x".repeat(8192))])
                .unwrap_err()
                .code,
            "cover_header_limit"
        );
        assert_eq!(
            request(&[(
                "range",
                &format!("bytes={}", vec!["0-1"; MAX_RANGES + 1].join(","))
            )])
            .unwrap_err()
            .code,
            "cover_range_limit"
        );
        assert_eq!(
            plan(
                SourceState::Failed,
                Method::Get,
                &Headers::default(),
                ErrorRendering::Deferred
            )
            .unwrap_err()
            .code,
            "cover_source_failure"
        );
    }
    #[test]
    fn deferred_error_is_not_a_complete_development_response() {
        let p = request(&[("range", "bytes=999-")]).unwrap();
        assert_eq!(
            p.body,
            Body::DeferredError {
                status: 416,
                head_only: false
            }
        );
        assert!(!p.headers.contains_key("content-length"));
        assert_eq!(p.headers["content-range"], "bytes */20");
    }
    #[test]
    fn date_normalization_and_freshness() {
        assert_eq!(
            parse_date("Fri, 31 Feb 2023 12:30:00 GMT").unwrap(),
            Some(1_677_846_600_000)
        );
        assert_eq!(parse_date("Tue, 32 Feb 2023 12:30:00 GMT").unwrap(), None);
        let modified = SourceFileStat::new(20, 1_700_000_000_123.5)
            .unwrap()
            .last_modified();
        assert_eq!(
            request(&[("if-modified-since", &modified)]).unwrap().status,
            304
        );
        assert!(no_cache("max-age=0,\u{a0}no-cache\t"));
        assert!(!no_cache("No-Cache"));
        assert!(!no_cache("no-cache=\"x\""));
    }

    #[test]
    fn raw_route_boundaries_do_not_decode_or_normalize() {
        for target in [
            "/nowplayingcover",
            "/NOWPLAYINGCOVER/",
            "/nowplayingcover?path=/foreign",
            "/nowplayingcover/?x=%2f",
        ] {
            assert_eq!(
                match_route("GET", target).unwrap(),
                Route::Read {
                    method: Method::Get
                }
            );
            assert_eq!(
                match_route("HEAD", target).unwrap(),
                Route::Read {
                    method: Method::Head
                }
            );
            assert_eq!(
                match_route("OPTIONS", target).unwrap(),
                Route::AutomaticOptions
            );
            assert_eq!(match_route("POST", target).unwrap(), Route::Fallthrough);
        }
        for target in [
            "/%6eowplayingcover",
            "/nowplayingcover%2f",
            "//nowplayingcover",
            "/nowplayingcover//",
            "/x/../nowplayingcover",
            "/nowplayingcover.png",
            "/nowplayingcover\\",
            "/?path=/nowplayingcover",
        ] {
            assert_eq!(
                match_route("GET", target).unwrap(),
                Route::Fallthrough,
                "{target}"
            );
        }
        for target in [
            "http://example.invalid/nowplayingcover",
            "/nowplayingcover#fragment",
            "/nowplayingcover ",
            "/nowplayingcover?x=\u{a0}",
        ] {
            assert_eq!(
                match_route("GET", target).unwrap_err().code,
                "cover_route_unsupported"
            );
        }
        assert_eq!(
            match_route("get", "/nowplayingcover").unwrap_err().code,
            "cover_route_method"
        );
        assert_eq!(
            match_route("GET\r\n", "/nowplayingcover").unwrap_err().code,
            "cover_route_method"
        );
        assert_eq!(
            match_route(&"A".repeat(MAX_METHOD_BYTES + 1), "/nowplayingcover")
                .unwrap_err()
                .code,
            "cover_route_limit"
        );
        assert_eq!(
            match_route("GET", &format!("/{}", "a".repeat(MAX_RAW_TARGET_BYTES)))
                .unwrap_err()
                .code,
            "cover_route_limit"
        );
        assert!(matches!(
            match_route("GET", &format!("/{}", "a".repeat(MAX_RAW_TARGET_BYTES - 1))).unwrap(),
            Route::Fallthrough
        ));
        let options = options_plan();
        assert_eq!(options.status, 200);
        assert_eq!(options.body, Body::Options);
        assert_eq!(options.headers["allow"], "GET,HEAD");
        assert!(!options.headers.contains_key("last-modified"));
    }
}
