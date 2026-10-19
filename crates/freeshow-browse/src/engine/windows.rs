//! Ordinary fixed-local-disk Windows folder/similarity logic on existing workers.
//! Native listing order is deliberately retained; no Unix scandir sort is used.
use super::{distance, units, IMAGE, VIDEO};
use crate::{paths::windows as paths, stats, *};
use serde_json::{json, Map};
use std::collections::HashSet;
#[path = "windows_scandir.rs"]
mod scandir;
fn unsupported(message: impl Into<String>) -> BrowseError {
    BrowseError::new("unsupported_windows_listing", message)
}
fn lexical<T>(value: std::result::Result<T, paths::PathError>) -> Result<T> {
    value.map_err(|error| BrowseError::new(error.code, error.message))
}
fn ordinary(path: &str, budget: &mut Budget) -> Result<()> {
    if lexical(paths::parse(path))?.root.len() != 3 {
        return Err(unsupported(
            "Only ordinary absolute local drive-letter listing paths are certified",
        ));
    }
    let body = path[3..].strip_suffix(['/', '\\']).unwrap_or(&path[3..]);
    for name in body.split(['/', '\\']) {
        budget.check()?;
        if name.is_empty() && body.is_empty() {
            continue;
        }
        if name.is_empty()
            || name == "."
            || name == ".."
            || reserved(name)
            || name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c <= '\u{1f}' || ":\"<>|?*".contains(c))
            || name.encode_utf16().take(256).count() > 255
        {
            return Err(unsupported(
                "Ambiguous/reserved/unnormalized Windows listing component",
            ));
        }
    }
    Ok(())
}
fn reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
fn media(path: &str) -> Result<bool> {
    let extension = lexical(paths::extension(path))?
        .trim_start_matches('.')
        .to_lowercase();
    Ok(IMAGE.contains(&extension.as_str()) || VIDEO.contains(&extension.as_str()))
}
fn record(folder: &str, files: &[String], no_media: bool) -> Result<Value> {
    Ok(
        json!({"isFolder":true,"path":folder,"name":lexical(paths::basename(folder))?,"files":files,"noMedia":if no_media{Value::Bool(true)}else{Value::Null}}),
    )
}
struct Listing<'a> {
    depth: f64,
    generate: bool,
    capture: bool,
    visited: HashSet<String>,
    output: Map<String, Value>,
    intents: Vec<ThumbnailIntent>,
    budget: &'a mut Budget,
}
impl Listing<'_> {
    fn folder(&mut self, folder: &str, current: usize) -> Result<()> {
        self.budget.check()?;
        self.budget.string(folder)?;
        if !self.visited.insert(folder.into()) {
            return Ok(());
        }
        let exceeded = current as f64 > self.depth;
        if self.capture && current >= 2 && exceeded {
            self.output.insert(
                folder.into(),
                json!({"isFolder":true,"path":folder,"name":lexical(paths::basename(folder))?,"files":[]}),
            );
            return Ok(());
        }
        let files = scandir::readdir(folder, self.budget)?;
        let mut no_media = false;
        if self.capture {
            no_media = true;
            for path in &files {
                let stat = stats::stat(path, self.budget)?;
                if stat.is_some_and(|stat| stat.directory) || media(path)? {
                    no_media = false;
                    break;
                }
            }
        }
        // Preserve the source ternary's actual precedence.
        if !(self.capture && current < 2) && exceeded {
            self.output
                .insert(folder.into(), record(folder, &files, no_media)?);
            return Ok(());
        }
        let first_media: Vec<_> = if self.capture && current == 1 {
            let mut selected = Vec::new();
            for path in &files {
                if media(path)? {
                    selected.push(path.clone());
                    if selected.len() == 4 {
                        break;
                    }
                }
            }
            selected
        } else {
            vec![]
        };
        let selected = if self.capture && exceeded {
            &first_media
        } else {
            &files
        };
        for path in selected {
            let Some(stat) = stats::stat(path, self.budget)? else {
                continue;
            };
            if stat.directory {
                self.folder(path, current + 1)?;
            } else {
                if first_media.contains(path) || (self.generate && current == 0 && media(path)?) {
                    self.intents.push(ThumbnailIntent {
                        input: path.clone(),
                        size: 250,
                        reason: if first_media.contains(path) {
                            ThumbnailReason::FolderCapture
                        } else {
                            ThumbnailReason::RootMedia
                        },
                    });
                }
                self.output.insert(path.clone(),json!({"isFolder":false,"path":path,"name":lexical(paths::basename(path))?,"thumbnailPath":"","stats":stat.json}));
            }
        }
        self.output
            .insert(folder.into(), record(folder, &files, no_media)?);
        Ok(())
    }
}
fn similar(original: &[String], budget: &mut Budget) -> Result<Value> {
    let originals: HashSet<&str> = original.iter().map(String::as_str).collect();
    let mut candidates = vec![];
    let mut seen = HashSet::new();
    for path in original {
        ordinary(path, budget)?;
        for candidate in scandir::readdir(lexical(paths::parent(path))?, budget)? {
            if seen.insert(candidate.clone()) && !originals.contains(candidate.as_str()) {
                let name = lexical(paths::stem(&candidate))?.to_owned();
                let code_units = units(&name, budget)?;
                candidates.push((candidate, name, code_units));
            }
        }
    }
    let mut accepted: Vec<(String, String, f64)> = vec![];
    for path in original {
        let source = units(lexical(paths::stem(path))?, budget)?;
        for (candidate, name, candidate_units) in &candidates {
            budget.check()?;
            let mut duplicate = false;
            for (_, existing, _) in &accepted {
                budget.substrings = budget
                    .substrings
                    .saturating_add((existing.len() + name.len()) as u64);
                if budget.substrings > budget.config.substring_units {
                    return Err(BrowseError::resource("Similarity substring search budget"));
                }
                if existing.contains(name) {
                    duplicate = true;
                    break;
                }
            }
            if duplicate {
                continue;
            }
            let max = source.len().max(candidate_units.len());
            let score = 1.0 - distance(&source, candidate_units, budget)? as f64 / max as f64;
            if score < 0.5 {
                continue;
            }
            accepted.push((candidate.clone(), name.clone(), score));
        }
    }
    accepted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Value::Array(
        accepted
            .into_iter()
            .take(10)
            .map(|(path, name, _)| json!({"path":path,"name":name}))
            .collect(),
    ))
}
pub(super) fn process(request: Request, budget: &mut Budget) -> Result<BrowseReply> {
    budget.check()?;
    let mut thumbnail_intents = Vec::new();
    let data = match request {
        Request::FileInfo(_) => {
            return Err(unsupported("FILE_INFO uses the separate stat backend"))
        }
        Request::Similar(paths) => similar(&paths, budget)?,
        Request::Folder {
            roots,
            depth,
            generate,
            capture,
        } => {
            let mut listing = Listing {
                depth,
                generate,
                capture,
                visited: HashSet::new(),
                output: Map::new(),
                intents: vec![],
                budget,
            };
            for path in roots {
                ordinary(&path, listing.budget)?;
                if stats::stat(&path, listing.budget)?.is_some_and(|stat| stat.directory) {
                    listing.folder(&path, 0)?;
                }
            }
            thumbnail_intents = listing.intents;
            Value::Object(listing.output)
        }
    };
    budget.check()?;
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > self.limit {
                return Err(std::io::Error::other("Browse serialized output limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(
        Counter {
            bytes: 0,
            limit: budget.config.output_bytes,
        },
        &data,
    )
    .map_err(|_| BrowseError::resource("Browse serialized output limit"))?;
    budget.check()?;
    Ok(BrowseReply {
        data,
        thumbnail_intents,
        diagnostics: vec![],
    })
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
