use crate::*;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata, OpenOptions},
    io::{Read, Write},
};

fn io_error() -> Error {
    Error::new(
        "ai_status_io",
        "Nemotron status filesystem operation failed",
    )
}

// Nonblocking Unix open prevents a raced FIFO from trapping an owned worker.
// Source symlinks are followed: these are integrity probes, not path grants.
fn open(path: &Path, write: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(!write).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    if write {
        options.create(true);
    }
    options.open(path)
}

#[cfg(windows)]
pub(crate) fn namespace(job: &mut Job, path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    // No unverified device/reparse namespace. This is not an anchored grant.
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        // A bare C: prefix addresses the drive's current directory. Probe only
        // after the RootDir component makes this the actual absolute ancestor.
        if !current.is_absolute() {
            continue;
        }
        job.step()?;
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.file_attributes() & 0x400 != 0 => return Err(Error::unsupported()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => {}
        }
    }
    Ok(())
}
#[cfg(not(windows))]
pub(crate) fn namespace(_job: &mut Job, _path: &Path) -> Result<()> {
    Ok(())
}

pub(crate) fn values(metadata: &Metadata) -> Result<(f64, f64)> {
    #[cfg(unix)]
    let millis = {
        use std::os::unix::fs::MetadataExt;
        // Exactly Node's separate binary64 casts/multiply/divide/add, including
        // signed Darwin times. Never aggregate nanoseconds into one integer.
        posix_millis(metadata.mtime(), metadata.mtime_nsec())
    };
    #[cfg(windows)]
    let millis = {
        use std::os::windows::fs::MetadataExt;
        let ticks = i128::from(metadata.last_write_time()) - 116_444_736_000_000_000;
        let seconds = ticks.div_euclid(10_000_000);
        if !(0..=i128::from(u32::MAX)).contains(&seconds) {
            return Err(Error::unsupported());
        }
        let nanos = ticks.rem_euclid(10_000_000) * 100;
        seconds as f64 * 1000.0 + nanos as f64 / 1_000_000.0
    };
    #[cfg(not(any(unix, windows)))]
    return Err(Error::unsupported());
    #[cfg(any(unix, windows))]
    Ok((metadata.len() as f64, millis))
}
#[cfg(unix)]
pub(crate) fn posix_millis(seconds: i64, nanos: i64) -> f64 {
    seconds as f64 * 1000.0 + nanos as f64 / 1_000_000.0
}

fn stat(job: &mut Job, name: &'static str) -> Result<(f64, f64)> {
    job.step()?;
    let path = job.path(Some(name))?;
    namespace(job, &path)?;
    let metadata = std::fs::metadata(path).map_err(|_| io_error())?;
    let (size, mtime) = values(&metadata)?;
    job.evidence.stats.push(StatObservation {
        file: name,
        size_bits: format!("{:016x}", size.to_bits()),
        mtime_bits: format!("{:016x}", mtime.to_bits()),
    });
    Ok((size, mtime))
}

fn stamp_valid(job: &mut Job) -> Result<bool> {
    job.step()?;
    let path = job.path(Some("model.json"))?;
    namespace(job, &path)?;
    let mut file = match open(&path, false) {
        Ok(f) => f,
        Err(_) => return Ok(false),
    };
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    if !metadata.is_file() {
        if metadata.is_dir() {
            return Ok(false);
        }
        return Err(Error::unsupported());
    }
    if metadata.len() > STAMP_BYTES as u64 {
        return Err(Error::limit());
    }
    // Bound even a file that grows after fstat; no capacity from untrusted len.
    let mut bytes = Vec::with_capacity(STAMP_BYTES + 1);
    let mut chunk = [0; 4096];
    loop {
        job.step()?;
        let want = chunk.len().min(STAMP_BYTES + 1 - bytes.len());
        let n = match file.read(&mut chunk[..want]) {
            Ok(n) => n,
            Err(_) => return Ok(false),
        };
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > STAMP_BYTES {
            return Err(Error::limit());
        }
        if n == 0 {
            break;
        }
    }
    let stamp = match stamp::parse(&bytes)? {
        Some(s) => s,
        None => return Ok(false),
    };
    for (name, sha) in FILES {
        let Some(entry) = stamp.entry(name, sha) else {
            return Ok(false);
        };
        // Source catches stat errors only inside isStampValid; later ordinary
        // stat/hash errors remain observable failures rather than Missing.
        let observed = match stat(job, name) {
            Ok(v) => v,
            Err(e) if e.code == "ai_status_io" => return Ok(false),
            Err(e) => return Err(e),
        };
        // The original stats before comparing size/time types. Preserve this
        // IO/error order even when a stamp field is a string/null/missing.
        if entry.numbers() != Some(observed) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn hash(job: &mut Job, name: &'static str) -> Result<String> {
    job.step()?;
    let path = job.path(Some(name))?;
    namespace(job, &path)?;
    let mut file = open(&path, false).map_err(|_| io_error())?;
    if !file.metadata().map_err(|_| io_error())?.is_file() {
        return Err(Error::unsupported());
    }
    let mut hash = Sha256::new();
    let mut chunk = [0; 65536];
    loop {
        job.step()?;
        let remaining = job.config.limits.hash_bytes - job.hash_bytes;
        if remaining == 0 {
            return Err(Error::limit());
        }
        let want = chunk
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let n = file.read(&mut chunk[..want]).map_err(|_| io_error())?;
        job.hash_bytes = job
            .hash_bytes
            .checked_add(n as u64)
            .ok_or_else(Error::limit)?;
        if job.hash_bytes > job.config.limits.hash_bytes {
            return Err(Error::limit());
        }
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
        #[cfg(test)]
        if let Some(after) = job.after_chunk.take() {
            after(job);
        }
    }
    job.evidence.hashed.push(name);
    Ok(format!("{:x}", hash.finalize()))
}

// Digest injection is private and used ONLY by crate tests to reach the real
// write branch without downloading the genuine 682 MB pinned assets.
pub(crate) fn prepare(job: Job, digest_boundary: Option<[&str; 5]>) -> Result<Prepared> {
    prepare_classified(job, true, |index, actual| {
        digest_boundary.map_or(actual, |values| values[index].to_owned())
    })
}

#[cfg(feature = "test-harness")]
const FIXED_TINY_DIGESTS: [&str; 5] = [
    "9a20630b95d3fe0fca62595087dc0282aa8d0779dd2a13dfcc65fa59caa90b96",
    "ee10d2bb896a9036c5462f6eccff43923f4fca1427ce5d5bc91ac0c1efd2f809",
    "2356d8addc5a593f373b9f4620075d39a9b2c2f075107a93fd8f990b2c5b553a",
    "e16a20444ac6bfccced4f63ba2528e20173f9e0fb3b2cdb4ed2a04e1ce60e8be",
    "003b7e9089abbe8874fcf04a6cd8726121cb3acc96dda7cfc698f7ce7c2beb61",
];
#[cfg(feature = "test-harness")]
pub(crate) fn prepare_fixed_tiny_fixture(job: Job) -> Result<Prepared> {
    // Deliberately bypass test-created valid stamps: changed bytes must pass
    // the actual fixed-digest gate again. Normal prepare retains source reuse.
    prepare_classified(job, false, |index, actual| {
        if actual == FIXED_TINY_DIGESTS[index] {
            FILES[index].1.to_owned()
        } else {
            actual
        }
    })
}
fn prepare_classified(
    mut job: Job,
    stamp_fast_path: bool,
    classify: impl Fn(usize, String) -> String,
) -> Result<Prepared> {
    job.check()?;
    if matches!(job.platform, Platform::MacIntel) {
        return Ok(Prepared::Ready(job.ready(EngineStatus::failed(
            "Nemotron transcription is not supported on Intel Mac",
        ))?));
    }
    if matches!(job.platform, Platform::Unsupported) {
        return Err(Error::unsupported());
    }
    if stamp_fast_path && stamp_valid(&mut job)? {
        job.evidence.fast_path = true;
        let path = job.path(None)?;
        return Ok(Prepared::Ready(job.ready(EngineStatus::ok(&path))?));
    }
    let mut records = Vec::with_capacity(5);
    let mut outdated = false;
    for (index, (name, expected)) in FILES.into_iter().enumerate() {
        job.step()?;
        let path = job.path(Some(name))?;
        namespace(&mut job, &path)?;
        // Node existsSync returns false for probe errors. A later failed open
        // or stat is NOT reclassified Missing.
        #[cfg(windows)]
        let exists = path.try_exists().map_err(|_| Error::unsupported())?;
        #[cfg(not(windows))]
        let exists = path.try_exists().unwrap_or(false);
        if !exists {
            return Ok(Prepared::Ready(
                job.ready(EngineStatus::failed("The local model is missing"))?,
            ));
        }
        let (size, mtime) = stat(&mut job, name)?;
        let actual_digest = hash(&mut job, name)?;
        let digest = classify(index, actual_digest);
        if digest != expected {
            outdated = true
        }
        records.push((name, digest, size, mtime));
    }
    if outdated {
        return Ok(Prepared::Ready(
            job.ready(EngineStatus::failed("The local model is outdated"))?,
        ));
    }
    let body = stamp::body(&records);
    job.check()?;
    Ok(Prepared::Stamp(StampIntent { job, body }))
}

pub(crate) fn write_stamp(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut file = open(path, true)?;
    // Refuse raced special objects before truncation. No atomic-rename/fync:
    // this is the original direct write policy with an explicit safe subset.
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("unsupported stamp target"));
    }
    file.set_len(0)?;
    file.write_all(body)
}
