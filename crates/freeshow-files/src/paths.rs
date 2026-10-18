use crate::case_table;
use unicode_normalization::UnicodeNormalization;

pub fn file_path_hash(value: &str) -> String {
    let hash = value.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(unit as i32)
    });
    if hash < 0 {
        format!("i{}", -(hash as i64))
    } else {
        format!("a{hash}")
    }
}

fn in_ranges(character: char, ranges: &[(u32, u32)]) -> bool {
    let cp = character as u32;
    let index = ranges.partition_point(|(start, _)| *start <= cp);
    index > 0 && cp <= ranges[index - 1].1
}

pub fn normalized_file_name(value: &str) -> String {
    let characters: Vec<char> = value.nfc().collect();
    let mut output = String::new();
    for (index, &character) in characters.iter().enumerate() {
        if character == '\u{3a3}' {
            let before = characters[..index]
                .iter()
                .rev()
                .find(|&&c| !in_ranges(c, case_table::CASE_IGNORABLE));
            let after = characters[index + 1..]
                .iter()
                .find(|&&c| !in_ranges(c, case_table::CASE_IGNORABLE));
            if before.is_some_and(|&c| in_ranges(c, case_table::CASED))
                && !after.is_some_and(|&c| in_ranges(c, case_table::CASED))
            {
                output.push('\u{3c2}');
                continue;
            }
        }
        match case_table::LOWERCASE.binary_search_by_key(&(character as u32), |(cp, _)| *cp) {
            Ok(index) => output.push_str(case_table::LOWERCASE[index].1),
            Err(_) => output.push(character),
        }
    }
    output
}

pub fn file_names_match(a: &str, b: &str) -> bool {
    normalized_file_name(a) == normalized_file_name(b)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_percent(value: &str, preserve_reserved: bool) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let byte = (hex(*bytes.get(index + 1)?)? << 4) | hex(*bytes.get(index + 2)?)?;
        if preserve_reserved && b";/?:@&=+$,#".contains(&byte) {
            decoded.extend_from_slice(&bytes[index..index + 3]);
        } else {
            decoded.push(byte);
        }
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

pub fn normalize_local_path(value: &str) -> String {
    normalize_local_path_for(value, cfg!(windows))
}

pub fn normalize_local_path_for(value: &str, windows: bool) -> String {
    let mut path = value.to_string();
    if path.starts_with("file://") {
        path = url::Url::parse(&path)
            .ok()
            .filter(|url| {
                !url.path().as_bytes().windows(3).any(|bytes| {
                    bytes.eq_ignore_ascii_case(b"%2f")
                        || (windows && bytes.eq_ignore_ascii_case(b"%5c"))
                })
            })
            .and_then(|url| {
                let pathname = decode_percent(url.path(), false)?;
                let host = url.host_str().unwrap_or("");
                if windows {
                    let pathname = pathname.replace('/', "\\");
                    if !host.is_empty() {
                        return Some(format!("\\\\{}{pathname}", idna::domain_to_unicode(host).0));
                    }
                    let bytes = pathname.as_bytes();
                    (bytes.len() >= 3 && bytes[1].is_ascii_alphabetic() && bytes[2] == b':')
                        .then(|| pathname[1..].to_string())
                } else {
                    host.is_empty().then_some(pathname)
                }
            })
            .unwrap_or_else(|| path[7..].to_string());
    }
    decode_percent(&path, true).unwrap_or(path)
}

/// upath 2.0.1 replaces backslashes, then /(?<!^)\/+/g, on both input/output.
pub fn to_unix(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars().map(|c| if c == '\\' { '/' } else { c }) {
        if character == '/' && output.ends_with('/') && output.len() > 1 {
            continue;
        }
        output.push(character);
    }
    output
}

pub fn basename(value: &str) -> String {
    basename_for(value, cfg!(windows))
}

pub fn basename_for(value: &str, windows: bool) -> String {
    let value = to_unix(value);
    let skip = if windows
        && value.as_bytes().get(1) == Some(&b':')
        && value.as_bytes()[0].is_ascii_alphabetic()
    {
        2
    } else {
        0
    };
    value[skip..]
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

pub fn dirname(value: &str) -> String {
    dirname_for(value, cfg!(windows))
}

fn windows_root(value: &str) -> (usize, bool, bool) {
    let bytes = value.as_bytes();
    if bytes.first() == Some(&b'/') {
        if bytes.get(1) == Some(&b'/') && bytes.get(2).is_some_and(|byte| *byte != b'/') {
            if let Some(server_end) = value[2..].find('/').map(|index| index + 2) {
                let mut share_start = server_end + 1;
                while value.as_bytes().get(share_start) == Some(&b'/') {
                    share_start += 1;
                }
                if share_start < value.len() {
                    let share_end = value[share_start..]
                        .find('/')
                        .map(|index| share_start + index)
                        .unwrap_or(value.len());
                    if share_end > share_start {
                        return (share_end + usize::from(share_end < value.len()), true, true);
                    }
                }
            }
        }
        (1, true, false)
    } else if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.get(2) == Some(&b'/') {
            (3, true, false)
        } else {
            (2, false, false)
        }
    } else {
        (0, false, false)
    }
}

pub fn dirname_for(value: &str, windows: bool) -> String {
    let value = to_unix(value);
    let trimmed = value.trim_end_matches('/');
    if windows {
        let (root_end, _, _) = windows_root(&value);
        if trimmed.len() <= root_end {
            return if root_end == 0 {
                ".".into()
            } else {
                value[..root_end].into()
            };
        }
        return match trimmed.rfind('/').filter(|index| *index >= root_end) {
            Some(index) => trimmed[..index].into(),
            None if root_end > 0 => value[..root_end].into(),
            None => ".".into(),
        };
    }
    if trimmed.is_empty() {
        return if value.starts_with('/') {
            "/".into()
        } else {
            ".".into()
        };
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(0) => "/".into(),
        Some(1) if trimmed.starts_with("//") => "//".into(),
        Some(index) => trimmed[..index].to_string(),
    }
}

pub fn parent_folder_id(path: &str) -> String {
    let folder = dirname(path);
    format!("{}_{}", basename(&folder), file_path_hash(&folder))
}

/// Node path.join normalizes dots/separators rather than treating later
/// absolute components as replacements (unlike PathBuf::push).
pub fn join(parts: &[&str]) -> String {
    join_for(parts, cfg!(windows))
}

pub fn join_for(parts: &[&str], windows: bool) -> String {
    let selected = parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>();
    let combined = selected.join(if windows { "\\" } else { "/" });
    if combined.is_empty() {
        return ".".into();
    }
    let combined = if windows {
        let first = selected[0].replace('\\', "/");
        let mut joined = combined;
        let intended_unc =
            first.starts_with("//") && first.as_bytes().get(2).is_some_and(|byte| *byte != b'/');
        if !intended_unc
            && joined
                .chars()
                .take_while(|c| *c == '/' || *c == '\\')
                .count()
                >= 2
        {
            joined = format!("\\{}", joined.trim_start_matches(['/', '\\']));
        }
        let reserved = joined.split('\\').any(|part| {
            part.find(':').is_some_and(|index| {
                let name = part[..index].to_uppercase();
                matches!(
                    name.as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM¹"
                        | "COM²"
                        | "COM³"
                        | "LPT¹"
                        | "LPT²"
                        | "LPT³"
                ) || (name.len() == 4
                    && (name.starts_with("COM") || name.starts_with("LPT"))
                    && matches!(name.as_bytes()[3], b'1'..=b'9'))
            })
        });
        if reserved {
            return joined.replace('/', "\\");
        }
        joined.replace('\\', "/")
    } else {
        combined
    };
    let (root_end, absolute, unc) = if windows {
        if combined.starts_with("//?/") || combined.starts_with("//./") {
            (4, true, true)
        } else {
            windows_root(&combined)
        }
    } else {
        (
            usize::from(combined.starts_with('/')),
            combined.starts_with('/'),
            false,
        )
    };
    let prefix = &combined[..root_end];
    let trailing = combined.ends_with('/');
    let mut stack = Vec::new();
    for part in combined[root_end..].split('/') {
        match part {
            "" | "." => {}
            ".." if stack.last().is_some_and(|last| *last != "..") => {
                stack.pop();
            }
            ".." if !absolute => stack.push(part),
            ".." => {}
            part => stack.push(part),
        }
    }
    let mut prefix = prefix.to_string();
    if unc {
        prefix = format!(
            "//{}",
            prefix
                .split('/')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        );
    }
    if unc && !prefix.ends_with('/') {
        prefix.push('/');
    }
    let mut output = format!("{prefix}{}", stack.join("/"));
    if output.is_empty() {
        output = ".".into();
    }
    if windows && root_end == 2 && stack.is_empty() {
        output.push('.');
    }
    if trailing && !output.ends_with('/') {
        output.push('/');
    }
    if windows && !absolute && root_end == 0 && combined.contains(':') {
        let bytes = output.as_bytes();
        let drive_tail = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        let ambiguous_colon = combined.as_bytes().iter().enumerate().any(|(index, byte)| {
            *byte == b':' && (index + 1 == combined.len() || combined.as_bytes()[index + 1] == b'/')
        });
        if drive_tail || ambiguous_colon {
            output.insert_str(0, "./");
        }
    }
    if windows {
        output = output.replace('/', "\\");
    }
    output
}
