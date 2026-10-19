//! Bounded lexical Node-win32 helpers. No filesystem or current-drive lookup.
pub const MAX_PATH_BYTES: usize = 32768;
pub const MAX_COMPONENTS: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathError {
    pub code: &'static str,
    pub message: &'static str,
}
type Result<T> = std::result::Result<T, PathError>;
fn unsupported() -> PathError {
    PathError {
        code: "unsupported_windows_path",
        message:
            "Drive-relative, rooted-current-drive, UNC/device and colon namespaces are unsupported",
    }
}
fn resource() -> PathError {
    PathError {
        code: "browse_resource_limit",
        message: "Windows lexical path byte/component limit",
    }
}
fn separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'\\')
}
fn root_length(path: &str) -> Result<usize> {
    if path.len() > MAX_PATH_BYTES {
        return Err(resource());
    }
    if path.contains('\0') {
        return Err(PathError {
            code: "invalid_request",
            message: "NUL is outside the native browse path contract",
        });
    }
    let bytes = path.as_bytes();
    let root = if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && separator(bytes[2])
    {
        3
    } else {
        0
    };
    if (root == 0 && bytes.first().is_some_and(|byte| separator(*byte)))
        || path[root..].contains(':')
        || (root == 0 && path.contains(':'))
    {
        return Err(unsupported());
    }
    if path[root..].split(['/', '\\']).count() > MAX_COMPONENTS {
        return Err(resource());
    }
    Ok(root)
}

/// Raw slices match path.win32.parse, including repeated/trailing separators.
/// In particular dir is parse.dir, not dirname's relative-path fallback ".".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts<'a> {
    pub root: &'a str,
    pub dir: &'a str,
    pub base: &'a str,
    pub ext: &'a str,
    pub name: &'a str,
}
pub fn parse(path: &str) -> Result<Parts<'_>> {
    let root = root_length(path)?;
    let bytes = path.as_bytes();
    let mut end = path.len();
    while end > root && separator(bytes[end - 1]) {
        end -= 1;
    }
    let start = path[root..end]
        .bytes()
        .rposition(separator)
        .map_or(root, |index| root + index + 1);
    let base = &path[start..end];
    let ext = match base.rfind('.') {
        Some(index) if index > 0 && base != ".." => &base[index..],
        _ => "",
    };
    Ok(Parts {
        root: &path[..root],
        dir: &path[..if start > root { start - 1 } else { root }],
        base,
        ext,
        name: &base[..base.len() - ext.len()],
    })
}
pub fn basename(path: &str) -> Result<&str> {
    Ok(parse(path)?.base)
}
pub fn extension(path: &str) -> Result<&str> {
    Ok(parse(path)?.ext)
}
pub fn stem(path: &str) -> Result<&str> {
    Ok(parse(path)?.name)
}
pub fn parent(path: &str) -> Result<&str> {
    Ok(parse(path)?.dir)
}

/// Source readdir joins use a parent and unrooted child spelling. This preserves
/// lexical Node normalization; it does not authorize the result or resolve it.
pub fn join(parent: &str, name: &str) -> Result<String> {
    let root = parse(parent)?.root.len();
    if root_length(name)? != 0 {
        return Err(unsupported());
    }
    if parent.len().saturating_add(name.len()) > MAX_PATH_BYTES {
        return Err(resource());
    }
    let mut raw = String::with_capacity(parent.len() + name.len() + 1);
    raw.push_str(parent);
    if !parent.is_empty() && !name.is_empty() {
        raw.push('\\');
    }
    raw.push_str(name);
    let suffix = &raw[root..];
    if suffix.split(['/', '\\']).count() > MAX_COMPONENTS {
        return Err(resource());
    }
    let trailing = raw.as_bytes().last().is_some_and(|byte| separator(*byte));
    let mut parts = Vec::new();
    for part in suffix.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|part| *part != "..") => {
                parts.pop();
            }
            ".." if root == 0 => parts.push(part),
            ".." => {}
            _ => parts.push(part),
        }
    }
    let mut output = String::with_capacity(raw.len());
    if root != 0 {
        output.push_str(&raw[..2]);
        output.push('\\');
    }
    for part in parts {
        if !output.is_empty() && !output.ends_with('\\') {
            output.push('\\');
        }
        output.push_str(part);
    }
    if output.is_empty() {
        output.push('.');
    }
    if trailing && !output.ends_with('\\') {
        output.push('\\');
    }
    if output.len() > MAX_PATH_BYTES {
        return Err(resource());
    }
    Ok(output)
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
