//! Trusted, synchronous final effects for sealed media-copy preparations.
//! The host supplies frozen roots and an accepted ordered-owner pin. This crate
//! creates no threads, runtime, renderer commands, or library-store mutations.
#[cfg(target_os = "linux")]
mod linux;
use freeshow_media_copy::{CommitLease, CopyDiagnostic, PreparedMediaCopy};
use serde::Serialize;
use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub struct TrustedCopyRoots<'a> {
    pub data_root: &'a Path,
    pub media_root: &'a Path,
}
#[derive(Clone, Debug)]
pub struct EffectLimits {
    pub max_effects: usize,
    pub max_read_bytes: u64,
    pub max_install_bytes: u64,
    pub max_chunks: usize,
    pub max_operations: usize,
    pub max_output_bytes: usize,
    pub timeout: Duration,
}
impl Default for EffectLimits {
    fn default() -> Self {
        Self {
            max_effects: 1024,
            max_read_bytes: 128 * 1024 * 1024,
            max_install_bytes: 64 * 1024 * 1024,
            max_chunks: 8192,
            max_operations: 50000,
            max_output_bytes: 4 * 1024 * 1024,
            timeout: Duration::from_secs(15),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledCopy {
    pub destination: PathBuf,
    pub length: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyFailure {
    pub source: String,
    pub destination: PathBuf,
    pub os_code: Option<i32>,
    pub phase: FailurePhase,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FailurePhase {
    PreparedSourceRead,
    ParentUnavailable,
    FinalCopy,
    DirectoryDestinationMissing,
}
/// Shell invocation remains host-owned. This decision was made after default
/// root creation and before per-candidate directory/copy effects, as in source.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OpenFolderIntent {
    OpenDirectory { path: PathBuf },
    Missing { path: PathBuf },
}
/// Sealed result. Some(false/true) is the original MEDIA_FOLDER_COPY response;
/// None means BUNDLE_MEDIA_FILES has no successful IPC response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    reply: Option<bool>,
    installed: Vec<InstalledCopy>,
    failures: Vec<CopyFailure>,
    skipped_existing: usize,
    duplicate_destinations: usize,
    alerts: Vec<String>,
    open_folder: Option<OpenFolderIntent>,
    diagnostics: Vec<String>,
    diagnostics_omitted: usize,
    preparation_diagnostics: Vec<CopyDiagnostic>,
    preparation_diagnostics_omitted: usize,
}
impl Receipt {
    pub fn reply(&self) -> Option<bool> {
        self.reply
    }
    pub fn installed(&self) -> &[InstalledCopy] {
        &self.installed
    }
    pub fn failures(&self) -> &[CopyFailure] {
        &self.failures
    }
    pub fn skipped_existing(&self) -> usize {
        self.skipped_existing
    }
    pub fn duplicate_destinations(&self) -> usize {
        self.duplicate_destinations
    }
    pub fn alerts(&self) -> &[String] {
        &self.alerts
    }
    pub fn open_folder(&self) -> Option<&OpenFolderIntent> {
        self.open_folder.as_ref()
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn diagnostics_omitted(&self) -> usize {
        self.diagnostics_omitted
    }
    pub fn preparation_diagnostics(&self) -> &[CopyDiagnostic] {
        &self.preparation_diagnostics
    }
    pub fn preparation_diagnostics_omitted(&self) -> usize {
        self.preparation_diagnostics_omitted
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectError {
    pub code: &'static str,
    pub message: String,
    pub os_code: Option<i32>,
    pub rollback_incomplete: bool,
    #[serde(flatten)]
    pub observed: Box<FailureDetails>,
}
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FailureDetails {
    /// Already observed source ALERT events survive a later hard failure. No
    /// MEDIA_FOLDER_COPY bool is available on a hard failure.
    pub alerts: Vec<String>,
    pub failures: Vec<CopyFailure>,
    pub open_folder: Option<OpenFolderIntent>,
    pub preparation_diagnostics: Vec<CopyDiagnostic>,
    pub preparation_diagnostics_omitted: usize,
    pub diagnostics: Vec<String>,
    pub diagnostics_omitted: usize,
}
pub type Result<T> = std::result::Result<T, EffectError>;
fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max.saturating_sub(12);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [truncated]", &text[..end])
}
impl EffectError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded(&message.into(), 2048),
            os_code: None,
            rollback_incomplete: false,
            observed: Box::default(),
        }
    }
    #[cfg(target_os = "linux")]
    fn io(action: &str, error: std::io::Error) -> Self {
        let mut e = Self::new("copy_effect_io", format!("{action}: {error}"));
        e.os_code = error.raw_os_error();
        e
    }
    #[cfg(any(target_os = "linux", test))]
    fn detail(&mut self, text: impl AsRef<str>) {
        let used = self
            .observed
            .diagnostics
            .iter()
            .map(String::len)
            .sum::<usize>();
        if self.observed.diagnostics.len() >= 16 || 16384usize.saturating_sub(used) < 16 {
            self.observed.diagnostics_omitted = self.observed.diagnostics_omitted.saturating_add(1);
        } else {
            self.observed
                .diagnostics
                .push(bounded(text.as_ref(), (16384 - used).min(2048)));
        }
    }
}
impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for EffectError {}
fn validate(
    roots: TrustedCopyRoots<'_>,
    plan: &PreparedMediaCopy,
    lease: &CommitLease,
    limits: &EffectLimits,
) -> Result<()> {
    if !(1..=4096).contains(&limits.max_effects)
        || limits.max_read_bytes == 0
        || limits.max_read_bytes > 512 * 1024 * 1024
        || limits.max_install_bytes == 0
        || limits.max_install_bytes > 128 * 1024 * 1024
        || limits.max_chunks == 0
        || limits.max_chunks > 32768
        || limits.max_operations == 0
        || limits.max_operations > 1_000_000
        || !(1..=16 * 1024 * 1024).contains(&limits.max_output_bytes)
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(60)
    {
        return Err(EffectError::new(
            "invalid_configuration",
            "Invalid media copy effect bounds",
        ));
    }
    plan.validate_commit_lease(lease)
        .map_err(|e| EffectError::new(e.code, e.message))?;
    if roots.data_root.as_os_str() != plan.captured_data_root().as_os_str()
        || roots.media_root.as_os_str() != plan.destination_root().as_os_str()
    {
        return Err(EffectError::new(
            "copy_effect_root_mismatch",
            "Trusted roots differ from captured roots",
        ));
    }
    if plan.effects().len() > limits.max_effects || plan.candidates().len() > limits.max_effects {
        return Err(EffectError::new(
            "copy_effect_limit",
            "Media effect/candidate count limit",
        ));
    }
    Ok(())
}
pub fn apply(
    roots: TrustedCopyRoots<'_>,
    plan: &PreparedMediaCopy,
    lease: &CommitLease,
    owner_cancel: &CancellationToken,
) -> Result<Receipt> {
    apply_with_limits(roots, plan, lease, EffectLimits::default(), owner_cancel)
}
pub fn apply_with_limits(
    roots: TrustedCopyRoots<'_>,
    plan: &PreparedMediaCopy,
    lease: &CommitLease,
    limits: EffectLimits,
    owner_cancel: &CancellationToken,
) -> Result<Receipt> {
    validate(roots, plan, lease, &limits)?;
    #[cfg(target_os = "linux")]
    {
        linux::apply(plan, lease, limits, owner_cancel, &mut |_| {}).map_err(|mut e| {
            e.observed.preparation_diagnostics = plan.diagnostics().to_vec();
            e.observed.preparation_diagnostics_omitted = plan.omitted_diagnostics();
            e
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (limits, owner_cancel);
        Err(EffectError::new(
            "unsupported_platform",
            "Anchored media copy effects are verified on Linux only",
        ))
    }
}
#[cfg(test)]
mod tests;
