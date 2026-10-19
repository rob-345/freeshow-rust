//! Synchronous bounded cache effects for a trusted context-validated ordered owner.
//! No runtime/IPC/worker is created and no source video content is read.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use freeshow_thumbnails::ThumbnailOutcome;
use freeshow_thumbnails::{CacheRelativePath, FileIdentity, PlanIdentity, PreparedThumbnail};
use serde::Serialize;
use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct EffectLimits {
    pub max_effects: usize,
    pub max_bytes: u64,
    pub max_chunks: usize,
    pub max_operations: usize,
    pub timeout: Duration,
}
impl Default for EffectLimits {
    fn default() -> Self {
        Self {
            max_effects: 4,
            max_bytes: 32 * 1024 * 1024,
            max_chunks: 4096,
            max_operations: 16384,
            timeout: Duration::from_secs(10),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct AppliedEffect {
    pub destination: PathBuf,
    pub installed: bool,
    pub bytes: u64,
    pub sha256: Option<String>,
}
/// Sealed native receipt. It has no Deserialize implementation/public constructor.
#[derive(Debug)]
pub struct Receipt {
    root: PathBuf,
    identity: PlanIdentity,
    effects: Vec<AppliedEffect>,
    invalidation: Option<(CacheRelativePath, FileIdentity)>,
    acknowledged: bool,
    diagnostics: Vec<String>,
    diagnostics_omitted: usize,
}
impl Receipt {
    pub fn effects(&self) -> &[AppliedEffect] {
        &self.effects
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn diagnostics_omitted(&self) -> usize {
        self.diagnostics_omitted
    }
    /// Successful preparation invalidation advances only its own live pending plan.
    pub fn acknowledge_preparation(&mut self, plan: &mut PreparedThumbnail) -> Result<()> {
        if self.acknowledged || self.identity != plan.identity() {
            return Err(EffectError::new(
                "invalid_preparation_acknowledgement",
                "Foreign/duplicate preparation receipt",
            ));
        }
        let (destination, expected) = self.invalidation.as_ref().ok_or_else(|| {
            EffectError::new(
                "invalid_preparation_acknowledgement",
                "Receipt is not a pre-capture invalidation",
            )
        })?;
        plan.acknowledge_pre_capture_invalidation(
            &self.identity,
            &self.root,
            destination,
            expected,
        )
        .map_err(|e| EffectError::new(e.code, e.message))?;
        self.acknowledged = true;
        Ok(())
    }
}
#[derive(Debug)]
pub struct EffectError {
    pub code: &'static str,
    pub message: String,
    pub rollback_incomplete: bool,
    pub diagnostics: Vec<String>,
    pub diagnostics_omitted: usize,
}
pub type Result<T> = std::result::Result<T, EffectError>;
#[cfg(any(target_os = "linux", test))]
const MAX_DETAILS: usize = 16;
#[cfg(any(target_os = "linux", test))]
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let marker = if max >= 12 { " [truncated]" } else { "" };
    let mut end = max.saturating_sub(marker.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}
impl EffectError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded(&message.into(), 2048),
            rollback_incomplete: false,
            diagnostics: vec![],
            diagnostics_omitted: 0,
        }
    }
    #[cfg(target_os = "linux")]
    fn io(action: &str, error: std::io::Error) -> Self {
        Self::new("thumbnail_effect_io", format!("{action}: {error}"))
    }
    #[cfg(any(target_os = "linux", test))]
    fn detail(&mut self, text: impl Into<String>) {
        let text = text.into();
        let used = self.diagnostics.iter().map(String::len).sum::<usize>();
        if self.diagnostics.len() >= MAX_DETAILS || MAX_DIAGNOSTIC_BYTES.saturating_sub(used) < 16 {
            self.diagnostics_omitted = self.diagnostics_omitted.saturating_add(1);
            return;
        }
        self.diagnostics
            .push(bounded(&text, (MAX_DIAGNOSTIC_BYTES - used).min(2048)));
    }
}
impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for EffectError {}
pub fn apply(trusted_cache_root: &Path, plan: &PreparedThumbnail) -> Result<Receipt> {
    apply_with_limits(trusted_cache_root, plan, EffectLimits::default())
}
pub fn apply_with_limits(
    root: &Path,
    plan: &PreparedThumbnail,
    limits: EffectLimits,
) -> Result<Receipt> {
    if !(1..=4).contains(&limits.max_effects)
        || limits.max_bytes == 0
        || limits.max_bytes > 128 * 1024 * 1024
        || limits.max_chunks == 0
        || limits.max_chunks > 16384
        || limits.max_operations == 0
        || limits.max_operations > 100_000
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(30)
    {
        return Err(EffectError::new(
            "invalid_configuration",
            "Invalid thumbnail effect bounds",
        ));
    }
    plan.ensure_current()
        .map_err(|e| EffectError::new(e.code, e.message))?;
    if !root.is_absolute()
        || root
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(EffectError::new(
            "thumbnail_effect_path",
            "Cache root must be an absolute path without parent traversal",
        ));
    }
    if root.as_os_str() != plan.captured_cache_root().as_os_str()
        || plan.cache_root.as_os_str() != plan.captured_cache_root().as_os_str()
    {
        return Err(EffectError::new(
            "thumbnail_effect_root_mismatch",
            "Cache root differs from captured trusted root",
        ));
    }
    if plan.outcome == freeshow_thumbnails::ThumbnailOutcome::GeneratedPng
        && (plan.effects.len() != 1
            || !matches!(
                plan.effects[0],
                freeshow_thumbnails::CacheEffect::Install { .. }
            ))
    {
        return Err(EffectError::new(
            "thumbnail_effect_invalid_plan",
            "Generated PNG requires its completed install effect",
        ));
    }
    if plan.effects.len() > limits.max_effects {
        return Err(EffectError::new(
            "thumbnail_effect_limit",
            "Thumbnail effect count limit",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        linux::apply(root, plan, limits, &mut |_| {})
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, plan, limits);
        Err(EffectError::new(
            "unsupported_platform",
            "Anchored thumbnail effects verified on Linux only",
        ))
    }
}
#[cfg(test)]
mod tests;
