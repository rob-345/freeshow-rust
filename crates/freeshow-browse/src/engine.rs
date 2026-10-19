use crate::{paths, stats, *};
use serde_json::{json, Map};
use std::collections::HashSet;
#[cfg(target_os = "windows")]
mod windows;
const IMAGE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "svg", "webp", "tiff", "tif", "jfif", "avif", "heic",
];
const VIDEO: &[&str] = &[
    "mp4", "webm", "ogv", "mov", "m4v", "3gp", "3g2", "avi", "mkv", "flv", "mpeg", "mpg", "wmv",
];
fn media(path: &str) -> bool {
    let extension = paths::extension(path)
        .trim_start_matches('.')
        .to_lowercase();
    IMAGE.contains(&extension.as_str()) || VIDEO.contains(&extension.as_str())
}
fn readdir(path: &str, budget: &mut Budget) -> Result<Vec<String>> {
    budget.check()?;
    let listing = std::fs::read_dir(path);
    budget.check()?;
    let Ok(listing) = listing else {
        return Ok(vec![]);
    };
    let mut names = vec![];
    for entry in listing {
        budget.entry()?;
        let Ok(entry) = entry else { return Ok(vec![]) };
        let name = entry.file_name();
        budget.string(&name.to_string_lossy())?;
        names.push(name);
    }
    // libuv scandir sorts strcmp byte names before Node decodes UTF-8.
    names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
    names
        .into_iter()
        .map(|name| {
            let joined = paths::join(path, &name.to_string_lossy());
            budget.string(&joined)?;
            Ok(joined)
        })
        .collect()
}
fn record(folder: &str, files: &[String], no_media: bool) -> Value {
    json!({"isFolder":true,"path":folder,"name":paths::basename(folder),"files":files,"noMedia":if no_media{Value::Bool(true)}else{Value::Null}})
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
        if !self.visited.insert(folder.into()) {
            return Ok(());
        }
        self.budget.string(folder)?;
        let exceeded = current as f64 > self.depth;
        if self.capture && current >= 2 && exceeded {
            self.output.insert(
                folder.into(),
                json!({"isFolder":true,"path":folder,"name":paths::basename(folder),"files":[]}),
            );
            return Ok(());
        }
        let files = readdir(folder, self.budget)?;
        let mut no_media = false;
        if self.capture {
            no_media = true;
            for path in &files {
                let stat = stats::stat(path, self.budget)?;
                if stat.is_some_and(|stat| stat.directory) || media(path) {
                    no_media = false;
                    break;
                }
            }
        }
        // Preserve the source ternary's actual precedence.
        if !(self.capture && current < 2) && exceeded {
            self.output
                .insert(folder.into(), record(folder, &files, no_media));
            return Ok(());
        }
        let first_media: Vec<_> = if self.capture && current == 1 {
            files
                .iter()
                .filter(|path| media(path))
                .take(4)
                .cloned()
                .collect()
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
                if first_media.contains(path) || (self.generate && current == 0 && media(path)) {
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
                self.output.insert(path.clone(),json!({"isFolder":false,"path":path,"name":paths::basename(path),"thumbnailPath":"","stats":stat.json}));
            }
        }
        self.output
            .insert(folder.into(), record(folder, &files, no_media));
        Ok(())
    }
}
fn distance(left: &[u16], right: &[u16], budget: &mut Budget) -> Result<usize> {
    let (left, right) = if left.len() < right.len() {
        (right, left)
    } else {
        (left, right)
    };
    let cells = (left.len() as u64)
        .saturating_add(1)
        .saturating_mul((right.len() as u64).saturating_add(1));
    budget.cells = budget.cells.saturating_add(cells);
    if budget.cells > budget.config.levenshtein_cells {
        return Err(BrowseError::resource("UTF-16 Levenshtein cell limit"));
    }
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        budget.check()?;
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = if a == b {
                diagonal
            } else {
                1 + diagonal.min(row[j]).min(old)
            };
            diagonal = old;
        }
    }
    Ok(row[right.len()])
}
fn units(name: &str, budget: &Budget) -> Result<Vec<u16>> {
    let units: Vec<_> = name
        .encode_utf16()
        .take(budget.config.name_units + 1)
        .collect();
    if units.len() > budget.config.name_units {
        return Err(BrowseError::resource(
            "Similarity filename UTF-16 length limit",
        ));
    }
    Ok(units)
}
fn similar(original: &[String], budget: &mut Budget) -> Result<Value> {
    let originals: HashSet<&str> = original.iter().map(String::as_str).collect();
    let mut candidates = vec![];
    let mut seen = HashSet::new();
    for path in original {
        for candidate in readdir(paths::parent(path), budget)? {
            if seen.insert(candidate.clone()) && !originals.contains(candidate.as_str()) {
                let name = paths::stem(&candidate).to_owned();
                let code_units = units(&name, budget)?;
                candidates.push((candidate, name, code_units));
            }
        }
    }
    let mut accepted: Vec<(String, String, f64)> = vec![];
    for path in original {
        let source = units(paths::stem(path), budget)?;
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
pub(crate) fn process(request: Request, budget: &mut Budget) -> Result<BrowseReply> {
    #[cfg(target_os = "windows")]
    if matches!(&request, Request::Folder { .. } | Request::Similar(_)) {
        return windows::process(request, budget);
    }
    if !cfg!(any(target_os = "linux", target_os = "macos"))
        && !(cfg!(target_os = "windows") && matches!(&request, Request::FileInfo(_)))
    {
        return Err(BrowseError::new(
            "unsupported_platform",
            "Requested native browse workflow is unavailable on this platform",
        ));
    }
    budget.check()?;
    let mut thumbnail_intents = Vec::new();
    let data = match request {
        Request::FileInfo(path) => match stats::stat(&path, budget)? {
            Some(stat) => {
                // The Windows stat backend also certifies verbatim drive paths.
                // Keep their source basename extension without routing them
                // through the stricter future lexical/folder path capability.
                let base = if cfg!(target_os = "windows") {
                    path.trim_end_matches(['/', '\\'])
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("")
                } else {
                    &path
                };
                let extension = paths::extension(base)
                    .trim_start_matches('.')
                    .to_lowercase();
                json!({"path":path,"stat":stat.json,"extension":extension,"folder":stat.directory})
            }
            None => Value::Null,
        },
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
pub(crate) fn score(left: &str, right: &str, budget: &mut Budget) -> Result<f64> {
    let left = units(left, budget)?;
    let right = units(right, budget)?;
    Ok(1.0 - distance(&left, &right, budget)? as f64 / left.len().max(right.len()) as f64)
}
