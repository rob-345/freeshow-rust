//! POSIX Node path semantics. The initial runtime contract is gated to Linux.
pub fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}
pub fn extension(path: &str) -> &str {
    let base = basename(path);
    match base.rfind('.') {
        Some(index) if index > 0 && base != ".." => &base[index..],
        _ => "",
    }
}
pub fn stem(path: &str) -> &str {
    let base = basename(path);
    &base[..base.len() - extension(path).len()]
}
pub fn parent(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.starts_with('/') { "/" } else { "" };
    }
    match trimmed.rfind('/') {
        Some(0) => "/",
        Some(index) => &path[..index],
        None => "",
    }
}
pub fn join(parent: &str, name: &str) -> String {
    let joined = format!("{parent}/{name}");
    let joined = if parent.is_empty() { name } else { &joined };
    let absolute = joined.starts_with('/');
    let mut parts = vec![];
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|part| *part != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let mut output = if absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    };
    if output.is_empty() {
        output.push('.');
    }
    output
}
