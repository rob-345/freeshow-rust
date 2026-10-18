//! Original Linux path + upath2.0.1 transformations, including UTF-16 hashes.
pub(crate) fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|s| *s != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let mut out = if absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    };
    if out.is_empty() {
        out.push('.');
    }
    if trailing && out != "/" {
        out.push('/');
    }
    out
}
pub(crate) fn join(parent: &str, name: &str) -> String {
    normalize(&format!("{parent}/{name}"))
}
pub(crate) fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}
fn dirname(path: &str) -> &str {
    let end = path.trim_end_matches('/');
    if end.is_empty() {
        return if path.starts_with('/') { "/" } else { "." };
    }
    match end.rfind('/') {
        None => ".",
        Some(0) => "/",
        Some(1) if path.starts_with("//") => "//",
        Some(index) => &path[..index],
    }
}
fn to_unix(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        let c = if c == '\\' { '/' } else { c };
        if c == '/' && out.ends_with('/') && out.len() != 1 {
            continue;
        }
        out.push(c);
    }
    out
}
pub fn parent_folder_id(path: &str) -> String {
    let unix = to_unix(path);
    let parent = to_unix(dirname(&unix));
    let name = to_unix(basename(&parent));
    format!("{name}_{}", file_path_hash(&parent))
}
pub fn file_path_hash(path: &str) -> String {
    let hash = path
        .encode_utf16()
        .fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(u as i32));
    if hash < 0 {
        format!("i{}", hash.unsigned_abs())
    } else {
        format!("a{hash}")
    }
}
