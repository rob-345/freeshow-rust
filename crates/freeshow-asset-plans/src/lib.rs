//! Pure preparation only: no filesystem, threads, runtime, or store mutation.
//! Source wrappers intentionally have no Deserialize implementation.
mod collect;
pub use freeshow_assets::{Context, NativeGrant, Protection, Provenance};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

/// A bounded, nonblocking lookup into the host's captured protection metadata.
/// None means unclassified, not plaintext. The planner bounds invocations.
pub type Classifier<'a> = &'a dyn Fn(&str) -> Option<Protection>;

/// ```compile_fail
/// fn wire<T: serde::de::DeserializeOwned>() {}
/// wire::<freeshow_asset_plans::Committed<'static>>();
/// ```
pub struct Committed<'a> {
    context: &'a Context,
    input: CommittedInput<'a>,
    classify: Classifier<'a>,
}
enum CommittedInput<'a> {
    Show(&'a Value),
    ShowReply(&'a Value),
    Save(&'a Value),
}
impl<'a> Committed<'a> {
    pub fn show(context: &'a Context, show: &'a Value, classify: Classifier<'a>) -> Self {
        Self {
            context,
            input: CommittedInput::Show(show),
            classify,
        }
    }
    pub fn show_reply(context: &'a Context, reply: &'a Value, classify: Classifier<'a>) -> Self {
        Self {
            context,
            input: CommittedInput::ShowReply(reply),
            classify,
        }
    }
    /// SAVE.showsCache contains full shows. SAVE.SHOWS is deliberately ignored.
    pub fn save_snapshot(
        context: &'a Context,
        snapshot: &'a Value,
        classify: Classifier<'a>,
    ) -> Self {
        Self {
            context,
            input: CommittedInput::Save(snapshot),
            classify,
        }
    }
}

/// Only construct from a native result that the host has already accepted.
/// The source spelling and native target are separate: selected native percent
/// signs are literal, while committed file URLs need source normalization.
#[derive(Clone, Copy)]
pub struct AcceptedPath<'a> {
    source: &'a str,
    native: &'a Path,
    protection: Option<Protection>,
}
impl<'a> AcceptedPath<'a> {
    pub fn new(
        source_path: &'a str,
        native_path: &'a Path,
        protection: Option<Protection>,
    ) -> Self {
        Self {
            source: source_path,
            native: native_path,
            protection,
        }
    }
}
#[derive(Clone, Copy)]
pub struct DroppedFile<'a> {
    pub path: AcceptedPath<'a>,
    pub is_directory: bool,
}
#[derive(Clone, Copy)]
pub struct BrowsedFile<'a> {
    pub path: AcceptedPath<'a>,
    pub is_folder: bool,
    pub thumbnail: Option<AcceptedPath<'a>>,
}
/// ```compile_fail
/// fn wire<T: serde::de::DeserializeOwned>() {}
/// wire::<freeshow_asset_plans::AcceptedNative<'static>>();
/// ```
pub struct AcceptedNative<'a> {
    context: &'a Context,
    input: NativeInput<'a>,
}
enum NativeInput<'a> {
    Selection(&'a [AcceptedPath<'a>]),
    Drop(&'a [DroppedFile<'a>]),
    Located(Option<AcceptedPath<'a>>),
    Browse(&'a [BrowsedFile<'a>]),
    Imports(&'a [AcceptedPath<'a>]),
    Thumbnail(AcceptedPath<'a>),
}
impl<'a> AcceptedNative<'a> {
    pub fn selection(context: &'a Context, paths: &'a [AcceptedPath<'a>]) -> Self {
        Self {
            context,
            input: NativeInput::Selection(paths),
        }
    }
    /// Only a completed, metadata/gesture/context-validated native Drop.
    pub fn drop_files(context: &'a Context, files: &'a [DroppedFile<'a>]) -> Self {
        Self {
            context,
            input: NativeInput::Drop(files),
        }
    }
    /// Null/error LOCATE replies provide None and issue no grant.
    pub fn located(context: &'a Context, path: Option<AcceptedPath<'a>>) -> Self {
        Self {
            context,
            input: NativeInput::Located(path),
        }
    }
    /// Folder records do not imply grants for their recursive member strings.
    pub fn browse(context: &'a Context, files: &'a [BrowsedFile<'a>]) -> Self {
        Self {
            context,
            input: NativeInput::Browse(files),
        }
    }
    /// Final destinations only, after the ordered import effects commit.
    pub fn committed_imports(context: &'a Context, paths: &'a [AcceptedPath<'a>]) -> Self {
        Self {
            context,
            input: NativeInput::Imports(paths),
        }
    }
    pub fn generated_thumbnail(context: &'a Context, path: AcceptedPath<'a>) -> Self {
        Self {
            context,
            input: NativeInput::Thumbnail(path),
        }
    }
}
pub enum Source<'a> {
    Committed(Committed<'a>),
    Accepted(AcceptedNative<'a>),
}
impl<'a> From<Committed<'a>> for Source<'a> {
    fn from(source: Committed<'a>) -> Self {
        Self::Committed(source)
    }
}
impl<'a> From<AcceptedNative<'a>> for Source<'a> {
    fn from(source: AcceptedNative<'a>) -> Self {
        Self::Accepted(source)
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub shows: usize,
    pub container_entries: usize,
    pub containers: usize,
    pub operations: usize,
    pub references: usize,
    pub path_bytes: usize,
    pub input_path_bytes: usize,
    pub candidates: usize,
    pub candidate_bytes: usize,
    pub diagnostics: usize,
    pub diagnostic_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            shows: 128,
            container_entries: 4096,
            containers: 8192,
            operations: 100_000,
            references: 4096,
            path_bytes: 32768,
            input_path_bytes: 4 * 1024 * 1024,
            candidates: 1024,
            candidate_bytes: 8 * 1024 * 1024,
            diagnostics: 64,
            diagnostic_bytes: 64 * 1024,
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub field: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} ({})", self.code, self.message, self.field)
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: &'static str,
    pub field: &'static str,
    pub source_path: String,
}
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub shows: usize,
    pub containers: usize,
    pub operations: usize,
    pub references: usize,
    pub input_path_bytes: usize,
    pub candidate_bytes: usize,
    pub diagnostic_bytes: usize,
    pub classifier_calls: usize,
    pub legacy_bytes: usize,
    pub duplicates: usize,
    pub remote: usize,
    pub bundled: usize,
    pub protected: usize,
    pub unclassified: usize,
    pub invalid: usize,
    pub diagnostics_omitted: usize,
}
#[derive(Debug)]
pub struct Plan {
    pub context: Context,
    pub grants: Vec<NativeGrant>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: Stats,
}
pub struct Planner {
    limits: Limits,
}
impl Planner {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        for (value, hard) in [
            (limits.shows, 1024),
            (limits.container_entries, 16384),
            (limits.containers, 65536),
            (limits.operations, 2_000_000),
            (limits.references, 32768),
            (limits.path_bytes, 32768),
            (limits.input_path_bytes, 64 * 1024 * 1024),
            (limits.candidates, 16384),
            (limits.candidate_bytes, 64 * 1024 * 1024),
        ] {
            if value == 0 || value > hard {
                return Err(Error {
                    code: "asset_plan_limit",
                    field: "limits",
                    message: "Planner configuration exceeds hard bounds",
                });
            }
        }
        if limits.diagnostics > 1024 || limits.diagnostic_bytes > 1024 * 1024 {
            return Err(Error {
                code: "asset_plan_limit",
                field: "limits",
                message: "Diagnostic configuration exceeds hard bounds",
            });
        }
        Ok(Self { limits })
    }
    pub fn plan<'a>(&self, source: impl Into<Source<'a>>) -> Result<Plan, Error> {
        collect::plan(&self.limits, source.into())
    }
}
impl Default for Planner {
    fn default() -> Self {
        Self::new(Limits::default()).expect("checked default limits")
    }
}

#[cfg(test)]
mod tests;
