use crate::*;
use serde_json::{json, Map};
use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io::Write};
pub(crate) struct Input {
    pub command: String,
    pub payload: Value,
    pub roots: ThumbnailRoots,
}
pub(crate) fn validate(
    command: &str,
    payload: &Value,
    roots: &ThumbnailRoots,
    config: &ThumbnailConfig,
) -> Result<Input> {
    if !COMMANDS.contains(&command) {
        return Err(ThumbnailError::new(
            "unsupported_command",
            "Unsupported thumbnail command",
        ));
    }
    let object = payload.as_object().ok_or_else(|| {
        ThumbnailError::new("invalid_request", "Thumbnail data must be an object")
    })?;
    let key = if command == "GET_THUMBNAIL" {
        "input"
    } else {
        "path"
    };
    let path = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ThumbnailError::new("invalid_request", "Thumbnail path must be a string"))?;
    if path.len() > config.path_bytes || path.contains('\0') {
        return Err(ThumbnailError::resource("Thumbnail path length/NUL limit"));
    }
    if !roots.cache_root.is_absolute() || roots.cache_root.as_os_str().len() > config.path_bytes {
        return Err(ThumbnailError::new(
            "invalid_request",
            "Host cache root must be an absolute bounded path",
        ));
    }
    if command == "GET_THUMBNAIL" {
        match payload.get("size") {
            None | Some(Value::Null) => {}
            Some(value) => {
                let size = value.as_u64().ok_or_else(|| {
                    ThumbnailError::new(
                        "unsupported_size",
                        "Thumbnail size must be a nonnegative integer",
                    )
                })?;
                if size > config.max_size as u64 {
                    return Err(ThumbnailError::resource("Thumbnail size limit"));
                }
            }
        }
        let effective = payload["size"].as_u64().filter(|x| *x != 0).unwrap_or(500);
        if effective > config.max_size as u64 {
            return Err(ThumbnailError::resource("Effective thumbnail size limit"));
        }
    } else {
        if payload
            .get("creationTime")
            .is_some_and(|x| !x.is_null() && !x.is_number())
            || payload
                .get("noCache")
                .is_some_and(|x| !x.is_null() && !x.is_boolean())
        {
            return Err(ThumbnailError::new(
                "invalid_request",
                "Invalid media existence options",
            ));
        }
    }
    // Validate before recursive serde serialization/cloning, including direct API
    // callers that can construct deeper Values than the transport JSON parser.
    let mut pending = vec![(payload, 0_usize)];
    let mut nodes = 0;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 64 || nodes > 4096 {
            return Err(ThumbnailError::resource("Thumbnail JSON depth/node limit"));
        }
        match value {
            Value::Array(items) => {
                if items.len() > 4096 {
                    return Err(ThumbnailError::resource("Thumbnail JSON node limit"));
                }
                pending.extend(items.iter().map(|x| (x, depth + 1)));
            }
            Value::Object(items) => {
                if items.len() > 4096 {
                    return Err(ThumbnailError::resource("Thumbnail JSON node limit"));
                }
                pending.extend(items.values().map(|x| (x, depth + 1)));
            }
            _ => {}
        }
    }
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > 64 * 1024 {
                return Err(std::io::Error::other("payload byte limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), payload)
        .map_err(|_| ThumbnailError::resource("Thumbnail payload byte limit"))?;
    Ok(Input {
        command: command.into(),
        payload: payload.clone(),
        roots: roots.clone(),
    })
}
fn relative(input: &str, size: u32) -> CacheRelativePath {
    CacheRelativePath(format!("{}-{size}.png", file_path_hash_code(input)))
}
fn extension(input: &str) -> String {
    let base = input.rsplit('/').next().unwrap_or("");
    let Some((stem, ext)) = base.rsplit_once('.') else {
        return String::new();
    };
    if stem.is_empty() || base == ".." {
        String::new()
    } else {
        ext.to_lowercase()
    }
}
fn cache_stat(
    root: &Path,
    destination: &CacheRelativePath,
    budget: &Budget,
) -> Result<Option<stat::Stat>> {
    let path = root.join(destination.as_path());
    if !stat::access(&path, budget)? {
        return Ok(None);
    }
    let result = stat::read(&path, true, budget)?;
    if result.as_ref().is_some_and(|x| !stat::regular(x)) {
        return Err(ThumbnailError::new(
            "unsupported_cache_variant",
            "Cache entry is not a regular non-symlink file",
        ));
    }
    Ok(result)
}
fn invalidations(input: &str, root: &Path, budget: &Budget) -> Result<Vec<CacheEffect>> {
    let mut effects = vec![];
    for size in [900, 500, 250, 100] {
        let destination = relative(input, size);
        if let Some(entry) = cache_stat(root, &destination, budget)? {
            effects.push(CacheEffect::Invalidate {
                destination,
                expected: entry.identity,
            });
        }
    }
    Ok(effects)
}
pub(crate) fn run(
    input: Input,
    mut plan: PreparedThumbnail,
    budget: &Budget,
) -> Result<PreparedThumbnail> {
    budget.check()?;
    let file = input.payload[if input.command == "GET_THUMBNAIL" {
        "input"
    } else {
        "path"
    }]
    .as_str()
    .unwrap();
    plan.cache_root = input.roots.cache_root;
    if input.command == "DOES_MEDIA_EXIST" {
        if !stat::access(Path::new(file), budget)? {
            plan.reply = json!({"path":file,"exists":false});
            plan.outcome = ThumbnailOutcome::Missing;
            plan.effects = invalidations(file, &plan.cache_root, budget)?;
            return Ok(plan);
        }
        if input.payload["noCache"].as_bool() == Some(true) {
            plan.reply = json!({"path":file,"exists":true});
            plan.outcome = ThumbnailOutcome::MediaExists;
            return Ok(plan);
        }
        let ctime = stat::read(Path::new(file), false, budget)?
            .map_or(0.0, |x| x.ctime_ms)
            .floor();
        let previous = input.payload["creationTime"].as_f64().unwrap_or(0.0);
        if previous != 0.0 && previous != ctime {
            plan.reply = json!({"path":file,"exists":false});
            plan.outcome = ThumbnailOutcome::ChangeTimeMismatch;
            plan.effects = invalidations(file, &plan.cache_root, budget)?;
        } else {
            plan.reply = json!({"path":file,"exists":true,"creationTime":ctime});
            plan.outcome = ThumbnailOutcome::MediaExists;
        }
        return Ok(plan);
    }
    let mut reply: Map<String, Value> = input.payload.as_object().unwrap().clone();
    let Some(source) = stat::read(Path::new(file), false, budget)? else {
        reply.insert("output".into(), json!(""));
        plan.reply = Value::Object(reply);
        plan.outcome = ThumbnailOutcome::Missing;
        if file.starts_with("freeshow-protected://") {
            plan.diagnostics.push(ThumbnailDiagnostic{code:"protected_capture_unavailable".into(),message:"Original stat gate rejects protected URLs; native protected capture is unavailable".into()});
        }
        return Ok(plan);
    };
    let size = input.payload["size"]
        .as_u64()
        .filter(|x| *x != 0)
        .unwrap_or(500) as u32;
    let destination = relative(file, size);
    let output = plan.cache_root.join(destination.as_path());
    reply.insert("output".into(), json!(output));
    plan.reply = Value::Object(reply);
    let cached = cache_stat(&plan.cache_root, &destination, budget)?;
    if let Some(cached) = &cached {
        if source.mtime_ms <= cached.mtime_ms {
            plan.outcome = ThumbnailOutcome::CacheHitUnverified;
            plan.diagnostics.push(ThumbnailDiagnostic {
                code: "cache_hit_unverified".into(),
                message: "Original cache hit checks stat/mtime only; PNG bytes were not decoded"
                    .into(),
            });
            return Ok(plan);
        }
    }
    if !stat::regular(&source) {
        return Err(ThumbnailError::new(
            "unsupported_input_variant",
            "Capture input must be a regular file",
        ));
    }
    if source.identity.length > budget.config.input_bytes {
        return Err(ThumbnailError::resource(
            "Thumbnail capture input byte limit",
        ));
    }
    let extension = extension(file);
    let images = [
        "jpg", "jpeg", "png", "gif", "bmp", "svg", "webp", "tiff", "tif", "jfif", "avif", "heic",
    ];
    let videos = [
        "mp4", "webm", "ogv", "mov", "m4v", "3gp", "3g2", "avi", "mkv", "flv", "mpeg", "mpg", "wmv",
    ];
    if !images.contains(&extension.as_str()) && !videos.contains(&extension.as_str()) {
        return Err(ThumbnailError::new(
            "unsupported_capture_format",
            "Original queue skips this extension without generating output",
        ));
    }
    // The original fs.stat gate rejects URLs/protected protocol strings before capture.
    if file.starts_with("freeshow-protected://")
        || file.starts_with("http://")
        || file.starts_with("https://")
        || file.starts_with("data:")
        || file.starts_with("blob:")
    {
        return Err(ThumbnailError::new(
            "unsupported_capture_input",
            "Protected/network capture is unavailable",
        ));
    }
    let stage = match &budget.config.staging_root {
        Some(root) => tempfile::Builder::new()
            .prefix("freeshow-thumbnail-")
            .tempdir_in(root),
        None => tempfile::Builder::new()
            .prefix("freeshow-thumbnail-")
            .tempdir(),
    }
    .map_err(ThumbnailError::io)?;
    plan.failure_matches_request = input.payload["size"].as_u64() == Some(size as u64);
    plan.guard = cached.as_ref().map_or(CacheGuard::Absent, |x| {
        CacheGuard::Existing(x.identity.clone())
    });
    if let Some(entry) = cached {
        plan.effects.push(CacheEffect::Invalidate {
            destination: destination.clone(),
            expected: entry.identity,
        });
    }
    plan.source = Some(SourceGuard {
        path: file.into(),
        expected: source.identity,
    });
    plan.destination = Some(destination);
    plan.capture = Some(CaptureCanvas {
        input: file.into(),
        output: stage.path().join("canvas.png"),
        size: CaptureSize { width: size },
        extension,
        config: CaptureConfig { seek: 0.5 },
        id: format!("{file}-{size}"),
    });
    plan.stage = Some(stage);
    plan.outcome = ThumbnailOutcome::CapturePending;
    plan.diagnostics.push(ThumbnailDiagnostic {
        code: "capture_pending".into(),
        message:
            "Retained frontend canvas capture required; predicted reply is not a verified artifact"
                .into(),
    });
    budget.check()?;
    Ok(plan)
}
pub(crate) enum Completion {
    Png { bytes: Vec<u8>, id: String },
    Failed { id: String },
    TimedOut { id: String },
}
pub(crate) fn complete(
    mut plan: PreparedThumbnail,
    completion: Completion,
    budget: &Budget,
) -> Result<PreparedThumbnail> {
    budget.check()?;
    plan.ensure_current()?;
    if plan.outcome != ThumbnailOutcome::CapturePending {
        return Err(ThumbnailError::new(
            "invalid_capture_completion",
            "Plan has no live capture",
        ));
    }
    let id = match &completion {
        Completion::Png { id, .. } | Completion::Failed { id } | Completion::TimedOut { id } => id,
    };
    if Some(id.as_str()) != plan.capture.as_ref().map(|x| x.id.as_str()) {
        return Err(ThumbnailError::new(
            "invalid_capture_completion",
            "Capture ID does not belong to this private plan",
        ));
    }
    match completion {
        Completion::Failed { .. } => {
            if plan.failure_matches_request {
                plan.reply["output"] = json!("");
            } else {
                plan.diagnostics.push(ThumbnailDiagnostic{code:"failure_key_mismatch".into(),message:"Original raw/effective-size mismatch returns predicted path after capture failure; artifact absent".into()});
            }
            plan.outcome = ThumbnailOutcome::CaptureFailed;
        }
        Completion::TimedOut { .. } => {
            plan.outcome = ThumbnailOutcome::TimedOutPrediction;
            plan.diagnostics.push(ThumbnailDiagnostic {
                code: "predicted_path_unverified".into(),
                message:
                    "Original wait timeout returns a predicted path; no PNG generation is verified"
                        .into(),
            });
        }
        Completion::Png { bytes, .. } => {
            if plan.created.elapsed() > Duration::from_secs(15) {
                return Err(ThumbnailError::new(
                    "capture_expired",
                    "Capture completion arrived after original capture deadline",
                ));
            }
            let info = png_data::validate(&bytes, budget)?;
            let source = plan.source.as_ref().unwrap();
            if stat::read(Path::new(&source.path), false, budget)?
                .as_ref()
                .map(|x| &x.identity)
                != Some(&source.expected)
            {
                return Err(ThumbnailError::new(
                    "source_changed",
                    "Thumbnail source changed during capture",
                ));
            }
            let capture = plan.capture.as_ref().unwrap();
            if info.width != capture.size.width {
                return Err(ThumbnailError::new(
                    "invalid_capture_completion",
                    "Canvas width differs from requested source width",
                ));
            }
            budget.check()?;
            let staged = plan.stage.as_ref().unwrap().path().join("validated.png");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)
                .map_err(ThumbnailError::io)?;
            file.write_all(&bytes).map_err(ThumbnailError::io)?;
            file.sync_all().map_err(ThumbnailError::io)?;
            drop(file);
            budget.check()?;
            let identity = stat::read(&staged, true, budget)?
                .ok_or_else(|| ThumbnailError::new("thumbnail_io_error", "Staged PNG disappeared"))?
                .identity;
            plan.effects.clear();
            plan.effects.push(CacheEffect::Install {
                destination: plan.destination.clone().unwrap(),
                expected_destination: plan.guard.clone(),
                staged_path: staged,
                expected_staged: identity,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                source: Box::new(source.clone()),
                width: info.width,
                height: info.height,
                pixel_sha256: info.pixel_sha256,
            });
            plan.outcome = ThumbnailOutcome::GeneratedPng;
        }
    }
    plan.capture = None;
    plan.diagnostics.retain(|x| x.code != "capture_pending");
    budget.check()?;
    Ok(plan)
}
