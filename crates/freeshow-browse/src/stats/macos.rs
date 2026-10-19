//! Darwin stat -> libuv uint64 fields -> Node numeric Stats projection.
//! The existing fixed browse owners supply admission, paths and operation limits.
#![cfg(target_os = "macos")]
use super::Stat;
use crate::{BrowseError, Budget, Result};
use serde_json::{Map, Value};
use std::{ffi::CString, mem::MaybeUninit};

// uv__to_stat assigns these native fields to uint64_t before Node casts to
// double. In particular, signed Darwin dev_t/blksize_t must not cast directly
// to f64. Preserve unsigned wrapping and then the actual binary64 rounding.
fn numeric(value: i128) -> Value {
    Value::from(value as u64 as f64)
}
fn milliseconds(seconds: i64, nanoseconds: i64) -> Value {
    // Actual Node msFromTimeSpec operation order. Darwin can expose signed
    // subsecond fields; do not normalize or aggregate integer nanoseconds first.
    Value::from(seconds as f64 * 1000.0 + nanoseconds as f64 / 1_000_000.0)
}
fn project(value: &libc::stat) -> Stat {
    let mut json = Map::new();
    for (key, value) in [
        ("dev", i128::from(value.st_dev)),
        ("mode", i128::from(value.st_mode)),
        ("nlink", i128::from(value.st_nlink)),
        ("uid", i128::from(value.st_uid)),
        ("gid", i128::from(value.st_gid)),
        ("rdev", i128::from(value.st_rdev)),
        ("blksize", i128::from(value.st_blksize)),
        ("ino", i128::from(value.st_ino)),
        ("size", i128::from(value.st_size)),
        ("blocks", i128::from(value.st_blocks)),
    ] {
        json.insert(key.into(), numeric(value));
    }
    for (key, seconds, nanos) in [
        ("atimeMs", value.st_atime, value.st_atime_nsec),
        ("mtimeMs", value.st_mtime, value.st_mtime_nsec),
        ("ctimeMs", value.st_ctime, value.st_ctime_nsec),
        ("birthtimeMs", value.st_birthtime, value.st_birthtime_nsec),
    ] {
        json.insert(key.into(), milliseconds(seconds, nanos));
    }
    Stat {
        directory: value.st_mode & libc::S_IFMT == libc::S_IFDIR,
        json: Value::Object(json),
    }
}
pub(crate) fn stat(path: &str, budget: &mut Budget) -> Result<Option<Stat>> {
    budget.check()?;
    if path.len() > budget.config.path_bytes {
        return Err(BrowseError::resource("Stat path length limit"));
    }
    let path = CString::new(path)
        .map_err(|_| BrowseError::new("invalid_request", "Stat path contains NUL"))?;
    loop {
        let mut value = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: bounded NUL-terminated path, exact public Darwin libc stat ABI,
        // and correctly sized/aligned writable output. stat follows symlinks,
        // matching fs.statSync/fs.stat; it never opens or reads the file body.
        let status = unsafe { libc::stat(path.as_ptr(), value.as_mut_ptr()) };
        let error = if status == 0 {
            None
        } else {
            std::io::Error::last_os_error().raw_os_error()
        };
        budget.check()?;
        if status == 0 {
            // SAFETY: successful stat initialized this public layout.
            return Ok(Some(project(&unsafe { value.assume_init() })));
        }
        match error {
            // libuv retries interrupted stat calls; each retry here consumes
            // the same finite budget and observes cancellation/deadline again.
            Some(libc::EINTR) => continue,
            Some(libc::ENOSYS) => {
                return Err(BrowseError::new(
                    "unsupported_stat_backend",
                    "Darwin stat API is unavailable; no metadata fallback",
                ))
            }
            // Actual original helpers return null on failed stat, including
            // missing, permission, ENOTDIR and ELOOP. This is no guessed stat.
            _ => return Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/support/macos.rs"]
mod evidence;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrowseConfig;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;

    fn budget() -> Budget {
        Budget {
            config: BrowseConfig::default(),
            cancel: CancellationToken::new(),
            request_cancel: CancellationToken::new(),
            started: Instant::now(),
            operations: 0,
            entries: 0,
            strings: 0,
            cells: 0,
            substrings: 0,
        }
    }
    fn row(v: &libc::stat) -> Value {
        Value::Array(
            [
                i128::from(v.st_dev),
                i128::from(v.st_mode),
                i128::from(v.st_nlink),
                i128::from(v.st_uid),
                i128::from(v.st_gid),
                i128::from(v.st_rdev),
                i128::from(v.st_blksize),
                i128::from(v.st_ino),
                i128::from(v.st_size),
                i128::from(v.st_blocks),
                i128::from(v.st_atime),
                i128::from(v.st_atime_nsec),
                i128::from(v.st_mtime),
                i128::from(v.st_mtime_nsec),
                i128::from(v.st_ctime),
                i128::from(v.st_ctime_nsec),
                i128::from(v.st_birthtime),
                i128::from(v.st_birthtime_nsec),
            ]
            .into_iter()
            .map(|number| Value::String(number.to_string()))
            .collect(),
        )
    }
    #[test]
    fn pure_darwin_fields_match_actual_node_projection_including_precision_and_signed_subseconds() {
        // Synthetic native fields are arithmetic evidence, not FS observations.
        // SAFETY: Darwin stat contains only integer fields/arrays; zero is valid.
        let mut a: libc::stat = unsafe { std::mem::zeroed() };
        a.st_mode = libc::S_IFREG | 0o640;
        a.st_dev = -1;
        a.st_rdev = -2;
        a.st_blksize = -4096;
        a.st_nlink = u16::MAX;
        a.st_uid = u32::MAX;
        a.st_gid = 42;
        a.st_ino = (1_u64 << 53) + 1;
        a.st_size = i64::MAX;
        a.st_blocks = i64::MAX;
        a.st_atime = -1;
        a.st_atime_nsec = -250_000_000;
        a.st_mtime = 1_700_000_000;
        a.st_mtime_nsec = 12_348;
        a.st_ctime = i64::MAX;
        a.st_ctime_nsec = 999_999_999;
        a.st_birthtime = -9_007_199_254_740_993;
        a.st_birthtime_nsec = 500_001;
        let mut b = a;
        b.st_mode = libc::S_IFDIR | 0o755;
        b.st_size = -1;
        b.st_blocks = -1;
        b.st_atime = 0;
        b.st_atime_nsec = 0;
        b.st_mtime = -51;
        b.st_mtime_nsec = 346_000_000;
        b.st_ctime = 123;
        b.st_ctime_nsec = 123_456_789;
        b.st_birthtime = 456;
        b.st_birthtime_nsec = 654_321_000;
        let expected = evidence::oracle("--project", &serde_json::json!([row(&a), row(&b)]));
        for (index, native) in [a, b].iter().enumerate() {
            let projected = project(native);
            evidence::assert_value(&projected.json, &expected["results"][index]["data"]);
            assert_eq!(projected.directory, index == 1);
        }
    }
    #[test]
    fn stat_consumes_actual_budget_and_preserves_explicit_cancel_path_and_backend_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut one = budget();
        one.config.operations = 1;
        assert_eq!(
            stat(path, &mut one).err().unwrap().code,
            "browse_resource_limit"
        );
        let mut cancelled = budget();
        cancelled.cancel.cancel();
        assert_eq!(stat(path, &mut cancelled).err().unwrap().code, "cancelled");
        let mut nul = budget();
        assert_eq!(
            stat("bad\0path", &mut nul).err().unwrap().code,
            "invalid_request"
        );
    }
}
