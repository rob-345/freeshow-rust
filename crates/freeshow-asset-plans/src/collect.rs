use crate::*;
use serde_json::Map;
use std::{collections::HashMap, path::PathBuf};

enum Seen {
    Grant(usize),
    Denied(&'static str),
}
struct Collector<'a> {
    limits: &'a Limits,
    plan: Plan,
    seen: HashMap<&'a str, Seen>,
}
fn error(code: &'static str, field: &'static str, message: &'static str) -> Error {
    Error {
        code,
        field,
        message,
    }
}
fn limit(field: &'static str) -> Error {
    error(
        "asset_plan_limit",
        field,
        "Grant preparation exceeded its work or payload budget",
    )
}
fn shape(field: &'static str) -> Error {
    error(
        "asset_plan_shape",
        field,
        "Expected the typed native/committed source shape",
    )
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}
fn index(key: &str) -> Option<u32> {
    if key.is_empty()
        || key.len() > 10
        || !key.bytes().all(|byte| byte.is_ascii_digit())
        || (key.len() > 1 && key.starts_with('0'))
    {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}
impl<'a> Collector<'a> {
    fn operations(&mut self, count: usize, field: &'static str) -> Result<(), Error> {
        self.plan.stats.operations = self.plan.stats.operations.saturating_add(count);
        if self.plan.stats.operations > self.limits.operations {
            return Err(limit(field));
        }
        Ok(())
    }
    fn container(&mut self, length: usize, field: &'static str) -> Result<(), Error> {
        if length > self.limits.container_entries {
            return Err(limit(field));
        }
        self.plan.stats.containers += 1;
        if self.plan.stats.containers > self.limits.containers {
            return Err(limit(field));
        }
        self.operations(1, field)
    }
    fn object(
        &mut self,
        value: &'a Value,
        field: &'static str,
    ) -> Result<&'a Map<String, Value>, Error> {
        let object = value.as_object().ok_or_else(|| shape(field))?;
        self.container(object.len(), field)?;
        Ok(object)
    }
    fn optional_object(
        &mut self,
        value: Option<&'a Value>,
        field: &'static str,
    ) -> Result<Option<&'a Map<String, Value>>, Error> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(value) => self.object(value, field).map(Some),
        }
    }
    fn array(&mut self, value: &'a Value, field: &'static str) -> Result<&'a [Value], Error> {
        let array = value.as_array().ok_or_else(|| shape(field))?;
        self.container(array.len(), field)?;
        Ok(array)
    }
    fn values(
        &mut self,
        object: &'a Map<String, Value>,
        field: &'static str,
    ) -> Result<Vec<&'a Value>, Error> {
        // JS visits canonical u32 index keys first, then other insertion keys.
        // Charge n(log2(n)+1) sort work units before allocating/sorting.
        let log = usize::BITS as usize - object.len().saturating_sub(1).leading_zeros() as usize;
        self.operations(object.len().saturating_mul(log + 1), field)?;
        let mut values: Vec<_> = object.iter().collect();
        values.sort_by(|(a, _), (b, _)| match (index(a), index(b)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        });
        Ok(values.into_iter().map(|(_, value)| value).collect())
    }
    fn diagnostic(&mut self, code: &'static str, field: &'static str, source: &str) {
        let mut end = source.len().min(512);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = code.len() + field.len() + end;
        if self.plan.diagnostics.len() >= self.limits.diagnostics
            || self.plan.stats.diagnostic_bytes.saturating_add(bytes) > self.limits.diagnostic_bytes
        {
            self.plan.stats.diagnostics_omitted += 1;
            return;
        }
        self.plan.stats.diagnostic_bytes += bytes;
        self.plan.diagnostics.push(Diagnostic {
            code,
            field,
            source_path: source[..end].to_string(),
        });
    }
    fn denied(
        &mut self,
        source: &'a str,
        field: &'static str,
        code: &'static str,
    ) -> Result<(), Error> {
        if let Some(seen) = self.seen.get(source) {
            self.plan.stats.duplicates += 1;
            return match seen {
                Seen::Denied(previous) if previous == &code => Ok(()),
                _ => Err(error(
                    "asset_plan_conflict",
                    field,
                    "The same source key has conflicting target/protection facts",
                )),
            };
        }
        self.seen.insert(source, Seen::Denied(code));
        self.diagnostic(code, field, source);
        Ok(())
    }
    fn path(
        &mut self,
        source: &'a str,
        native: Option<&Path>,
        classify: Option<Classifier<'a>>,
        protection: Option<Protection>,
        provenance: Provenance,
        field: &'static str,
    ) -> Result<(), Error> {
        self.operations(1, field)?;
        self.plan.stats.references += 1;
        if self.plan.stats.references > self.limits.references {
            return Err(limit(field));
        }
        if source.is_empty() {
            return Ok(());
        }
        if source.starts_with("http://")
            || source.starts_with("https://")
            || source.starts_with("data:")
            || source.starts_with("blob:")
        {
            self.plan.stats.remote += 1;
            return Ok(());
        }
        if source.len() > self.limits.path_bytes {
            return Err(limit(field));
        }
        self.plan.stats.input_path_bytes = self
            .plan
            .stats
            .input_path_bytes
            .saturating_add(source.len());
        if self.plan.stats.input_path_bytes > self.limits.input_path_bytes {
            return Err(limit(field));
        }
        if source.contains('\0') {
            self.plan.stats.invalid += 1;
            return self.denied(source, field, "asset_plan_invalid_path");
        }
        if source.starts_with("freeshow-protected://") {
            self.plan.stats.protected += 1;
            return self.denied(source, field, "asset_plan_protected");
        }
        // Retained relative bundled resources stay with the original encoder.
        if !source.starts_with("file://") && !Path::new(source).is_absolute() {
            self.plan.stats.bundled += 1;
            return Ok(());
        }
        let protection = match classify {
            Some(classify) => {
                self.plan.stats.classifier_calls += 1;
                classify(source)
            }
            None => protection,
        };
        match protection {
            Some(Protection::Protected) => {
                self.plan.stats.protected += 1;
                return self.denied(source, field, "asset_plan_protected");
            }
            None => {
                self.plan.stats.unclassified += 1;
                return self.denied(source, field, "asset_plan_unclassified");
            }
            Some(Protection::Plain) => {}
        }
        let normalized;
        let native = match native {
            Some(path) => path,
            None => {
                normalized = PathBuf::from(freeshow_files::paths::normalize_local_path(source));
                &normalized
            }
        };
        let Some(text) = native.to_str() else {
            self.plan.stats.invalid += 1;
            return self.denied(source, field, "asset_plan_encoding");
        };
        if text.len() > self.limits.path_bytes {
            return Err(limit(field));
        }
        self.plan.stats.input_path_bytes =
            self.plan.stats.input_path_bytes.saturating_add(text.len());
        if self.plan.stats.input_path_bytes > self.limits.input_path_bytes {
            return Err(limit(field));
        }
        if !native.is_absolute() || text.contains('\0') {
            self.plan.stats.invalid += 1;
            return self.denied(source, field, "asset_plan_invalid_path");
        }
        if let Some(seen) = self.seen.get(source) {
            self.plan.stats.duplicates += 1;
            return match seen {
                Seen::Grant(index) if self.plan.grants[*index].path == native => Ok(()),
                _ => Err(error(
                    "asset_plan_conflict",
                    field,
                    "The same source key has conflicting target/protection facts",
                )),
            };
        }
        let bytes = source.len().saturating_add(text.len()).saturating_add(128);
        if self.plan.grants.len() >= self.limits.candidates
            || self.plan.stats.candidate_bytes.saturating_add(bytes) > self.limits.candidate_bytes
        {
            return Err(limit(field));
        }
        self.plan.stats.candidate_bytes += bytes;
        self.seen
            .insert(source, Seen::Grant(self.plan.grants.len()));
        self.plan.grants.push(NativeGrant {
            source_path: source.to_string(),
            path: native.to_owned(),
            provenance,
            protection: Protection::Plain,
        });
        Ok(())
    }
    fn field(
        &mut self,
        value: Option<&'a Value>,
        classify: Classifier<'a>,
        field: &'static str,
    ) -> Result<(), Error> {
        match value {
            None | Some(Value::Null) => Ok(()),
            Some(Value::String(value)) => self.path(
                value,
                None,
                Some(classify),
                None,
                Provenance::CommittedLibrary,
                field,
            ),
            Some(_) => {
                self.operations(1, field)?;
                self.plan.stats.invalid += 1;
                self.diagnostic("asset_plan_shape", field, "");
                Ok(())
            }
        }
    }
    fn timeline(
        &mut self,
        value: Option<&'a Value>,
        classify: Classifier<'a>,
    ) -> Result<(), Error> {
        let Some(timeline) = self.optional_object(value, "timeline")? else {
            return Ok(());
        };
        let Some(actions) = timeline.get("actions").filter(|value| !value.is_null()) else {
            return Ok(());
        };
        for action in self.array(actions, "timeline.actions")? {
            let action = self.object(action, "timeline.action")?;
            if !matches!(
                action.get("type").and_then(Value::as_str),
                Some("audio" | "video")
            ) {
                continue;
            }
            if let Some(data) = self.optional_object(action.get("data"), "timeline.action.data")? {
                self.field(data.get("path"), classify, "timeline.action.data.path")?;
            }
        }
        Ok(())
    }
    fn show(&mut self, value: &'a Value, classify: Classifier<'a>) -> Result<(), Error> {
        self.plan.stats.shows += 1;
        if self.plan.stats.shows > self.limits.shows {
            return Err(limit("shows"));
        }
        let show = self.object(value, "show")?;
        if let Some(media) = self.optional_object(show.get("media"), "show.media")? {
            for value in self.values(media, "show.media")? {
                let entry = self.object(value, "show.media.entry")?;
                // Exact source media.path || media.id fallback, including JS
                // truthiness. A truthy non-string does not fall through to id.
                self.field(
                    entry
                        .get("path")
                        .filter(|value| truthy(value))
                        .or_else(|| entry.get("id")),
                    classify,
                    "show.media.path_or_id",
                )?;
            }
        }
        if let Some(slides) = self.optional_object(show.get("slides"), "show.slides")? {
            for value in self.values(slides, "show.slides")? {
                let slide = self.object(value, "show.slide")?;
                if let Some(items) = slide.get("items").filter(|value| !value.is_null()) {
                    for item in self.array(items, "show.slide.items")? {
                        let item = self.object(item, "show.slide.item")?;
                        if item.get("type").and_then(Value::as_str) == Some("media") {
                            self.field(item.get("src"), classify, "show.slide.item.src")?;
                        }
                    }
                }
                if let Some(settings) =
                    self.optional_object(slide.get("settings"), "show.slide.settings")?
                {
                    self.field(
                        settings.get("backgroundImage"),
                        classify,
                        "show.slide.settings.backgroundImage",
                    )?;
                }
                self.timeline(slide.get("timeline"), classify)?;
            }
        }
        if let Some(layouts) = self.optional_object(show.get("layouts"), "show.layouts")? {
            for value in self.values(layouts, "show.layouts")? {
                let layout = self.object(value, "show.layout")?;
                self.timeline(layout.get("timeline"), classify)?;
            }
        }
        Ok(())
    }
    fn accepted(
        &mut self,
        path: AcceptedPath<'a>,
        provenance: Provenance,
        field: &'static str,
    ) -> Result<(), Error> {
        self.path(
            path.source,
            Some(path.native),
            None,
            path.protection,
            provenance,
            field,
        )
    }
}

pub(super) fn plan<'a>(limits: &'a Limits, source: Source<'a>) -> Result<Plan, Error> {
    let context = match &source {
        Source::Committed(source) => source.context,
        Source::Accepted(source) => source.context,
    };
    if context.runtime_id.is_empty()
        || context.runtime_id.len() > 128
        || context.runtime_id.contains('\0')
        || context.library_generation > (1u64 << 53) - 1
        || context.data_path.len() > 32768
        || context.data_path.contains('\0')
    {
        return Err(shape("context"));
    }
    let mut collector = Collector {
        limits,
        plan: Plan {
            context: context.clone(),
            grants: Vec::new(),
            diagnostics: Vec::new(),
            stats: Stats::default(),
        },
        seen: HashMap::new(),
    };
    match source {
        Source::Committed(source) => match source.input {
            CommittedInput::Show(show) => collector.show(show, source.classify)?,
            CommittedInput::ShowReply(reply) => {
                let reply = collector.object(reply, "show_reply")?;
                if reply.get("error").is_some_and(truthy) {
                    return Err(shape("show_reply.error"));
                }
                let content = collector.array(
                    reply
                        .get("content")
                        .ok_or_else(|| shape("show_reply.content"))?,
                    "show_reply.content",
                )?;
                if content.len() != 2 {
                    return Err(shape("show_reply.content"));
                }
                if let Some(serialized) = content[1].as_str() {
                    // Retained loadSingleShow accepts one legacy embedded
                    // [id,Show] string. Parse only this documented field, with
                    // a separate hard byte cap; never scan arbitrary strings.
                    if serialized.len() > 1024 * 1024 {
                        return Err(limit("show_reply.legacy_bytes"));
                    }
                    let parsed: Value =
                        serde_json::from_str(serialized).map_err(|_| shape("show_reply.legacy"))?;
                    let show = parsed
                        .get(1)
                        .filter(|show| show.get("name").is_some_and(truthy))
                        .ok_or_else(|| shape("show_reply.legacy"))?;
                    let mut remaining = limits.clone();
                    remaining.operations = remaining
                        .operations
                        .checked_sub(collector.plan.stats.operations)
                        .filter(|left| *left > 0)
                        .ok_or_else(|| limit("operations"))?;
                    remaining.containers = remaining
                        .containers
                        .checked_sub(collector.plan.stats.containers)
                        .filter(|left| *left > 0)
                        .ok_or_else(|| limit("containers"))?;
                    let mut nested = plan(
                        &remaining,
                        Source::Committed(Committed::show(context, show, source.classify)),
                    )?;
                    nested.stats.operations += collector.plan.stats.operations;
                    nested.stats.containers += collector.plan.stats.containers;
                    nested.stats.legacy_bytes = serialized.len();
                    return Ok(nested);
                }
                collector.show(&content[1], source.classify)?;
            }
            CommittedInput::Save(snapshot) => {
                let snapshot = collector.object(snapshot, "save")?;
                if let Some(shows) =
                    collector.optional_object(snapshot.get("showsCache"), "save.showsCache")?
                {
                    if shows.len() > limits.shows {
                        return Err(limit("shows"));
                    }
                    for show in collector.values(shows, "save.showsCache")? {
                        collector.show(show, source.classify)?;
                    }
                }
            }
        },
        Source::Accepted(source) => match source.input {
            NativeInput::Selection(paths) | NativeInput::Imports(paths) => {
                let provenance = if matches!(source.input, NativeInput::Imports(_)) {
                    Provenance::CommittedLibrary
                } else {
                    Provenance::NativeSelection
                };
                collector.container(paths.len(), "accepted.paths")?;
                for &path in paths {
                    collector.accepted(path, provenance, "accepted.path")?;
                }
            }
            NativeInput::Drop(files) => {
                collector.container(files.len(), "accepted.drop")?;
                for file in files {
                    collector.operations(1, "accepted.drop")?;
                    if !file.is_directory {
                        collector.accepted(
                            file.path,
                            Provenance::NativeDrop,
                            "accepted.drop.path",
                        )?;
                    }
                }
            }
            NativeInput::Located(path) => {
                if let Some(path) = path {
                    collector.accepted(path, Provenance::LocatedMedia, "accepted.located.path")?;
                }
            }
            NativeInput::Browse(files) => {
                collector.container(files.len(), "accepted.browse")?;
                for file in files {
                    collector.operations(1, "accepted.browse")?;
                    if !file.is_folder {
                        collector.accepted(
                            file.path,
                            Provenance::NativeSelection,
                            "accepted.browse.path",
                        )?;
                        if let Some(path) = file.thumbnail {
                            collector.accepted(
                                path,
                                Provenance::GeneratedThumbnail,
                                "accepted.browse.thumbnail",
                            )?;
                        }
                    }
                }
            }
            NativeInput::Thumbnail(path) => {
                collector.accepted(path, Provenance::GeneratedThumbnail, "accepted.thumbnail")?
            }
        },
    }
    Ok(collector.plan)
}
