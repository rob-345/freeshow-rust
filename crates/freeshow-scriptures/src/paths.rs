//! Native Node path.join spelling; later absolute-looking parts do not replace
//! earlier parts. Filesystem lookup remains native and does not case/Unicode fold.
#[cfg(any(windows, test))]
use crate::Error;
use crate::Result;
use std::path::Path;
pub(super) fn join(parent: &str, child: &str) -> String {
    #[cfg(windows)]
    return win32_join(parent, child);
    #[cfg(not(windows))]
    {
        let text = format!("{parent}/{child}");
        let mut parts = Vec::new();
        for part in text.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => parts.push(part),
            }
        }
        format!("/{}", parts.join("/"))
    }
}
#[cfg(any(windows, test))]
fn reserved(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³"
    ) || (name.len() == 4
        && (name.starts_with("COM") || name.starts_with("LPT"))
        && matches!(name.as_bytes()[3], b'1'..=b'9'))
}
#[cfg(any(windows, test))]
fn win32_join(parent: &str, child: &str) -> String {
    // Both arguments are bounded; callers supply an absolute trusted parent.
    let mut text = if child.is_empty() {
        parent.to_owned()
    } else {
        format!("{parent}\\{child}")
    };
    let separator = |b: &u8| matches!(b, b'/' | b'\\');
    let first = parent.as_bytes();
    let unc =
        first.len() > 2 && separator(&first[0]) && separator(&first[1]) && !separator(&first[2]);
    if !unc {
        let count = text.as_bytes().iter().take_while(|b| separator(b)).count();
        if count >= 2 {
            text = format!("\\{}", &text[count..]);
        }
    }
    // Node 22.21.1 join bypasses normalization for reserved colon names.
    if text
        .split('\\')
        .any(|part| part.find(':').is_some_and(|i| reserved(&part[..i])))
    {
        return text.replace('/', "\\");
    }
    let text = text.replace('/', "\\");
    let bytes = text.as_bytes();
    let mut root_end = 0;
    let mut device = String::new();
    let absolute = bytes.first() == Some(&b'\\')
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\');
    if bytes.first() == Some(&b'\\') {
        if bytes.get(1) != Some(&b'\\') {
            root_end = 1;
        } else {
            let host_end = text[2..].find('\\').map(|n| n + 2);
            if let Some(host_end) = host_end.filter(|n| *n > 2) {
                let share_start = host_end
                    + bytes[host_end..]
                        .iter()
                        .take_while(|b| **b == b'\\')
                        .count();
                if share_start < bytes.len() {
                    let share_end = text[share_start..]
                        .find('\\')
                        .map_or(bytes.len(), |n| n + share_start);
                    let host = &text[2..host_end];
                    if host == "?" || host == "." {
                        device = format!("\\\\{host}");
                        root_end = 4;
                    } else {
                        device = format!("\\\\{host}\\{}", &text[share_start..share_end]);
                        root_end = share_end;
                    }
                }
            }
        }
    } else if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        device = text[..2].into();
        root_end = if absolute { 3 } else { 2 };
    }
    let mut parts = Vec::new();
    for part in text[root_end..].split('\\') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if !absolute => parts.push(part),
            ".." => {}
            _ => parts.push(part),
        }
    }
    let mut tail = parts.join("\\");
    if tail.is_empty() && !absolute {
        tail.push('.');
    }
    if !tail.is_empty() && text.ends_with('\\') {
        tail.push('\\');
    }
    if absolute {
        format!("{device}\\{tail}")
    } else {
        format!("{device}{tail}")
    }
}
/// Avoid device/pipe reads that can consume a fixed worker indefinitely. This
/// rejects input explicitly; it does not turn unsupported devices into missing.
pub(super) fn check_io_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        check_windows_path(
            path.to_str()
                .ok_or_else(|| Error::unsupported("Non-Unicode scripture path"))?,
        )?;
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}
#[cfg(any(windows, test))]
fn check_windows_path(text: &str) -> Result<()> {
    let text = text.replace('/', "\\");
    if text.starts_with("\\\\.\\") {
        return Err(Error::unsupported("Windows device/pipe namespace"));
    }
    if let Some(rest) = text.strip_prefix("\\\\?\\") {
        let bytes = rest.as_bytes();
        let drive = bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\';
        if !drive
            && !rest
                .get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case("UNC\\"))
        {
            return Err(Error::unsupported("Windows extended device/pipe namespace"));
        }
    }
    for part in text.split('\\') {
        let base = part
            .split(['.', ':'])
            .next()
            .unwrap_or("")
            .trim_end_matches(' ');
        if reserved(base) {
            return Err(Error::unsupported("Windows reserved device filename"));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn node_win32_join_spelling_for_absolute_parents() {
        for (parent, child, expected) in [
            (
                r"C:\selected",
                r"Bibles\nested/../same.fsb",
                r"C:\selected\Bibles\same.fsb",
            ),
            (
                r"C:\selected\Bibles",
                r"/leading.fsb",
                r"C:\selected\Bibles\leading.fsb",
            ),
            (
                r"C:\selected\Bibles",
                r"D:\other.fsb",
                r"C:\selected\Bibles\D:\other.fsb",
            ),
            (
                r"C:\selected\Bibles",
                r"\\server\share\x.fsb",
                r"C:\selected\Bibles\server\share\x.fsb",
            ),
            (
                r"\\server\share\selected",
                r"../../file.fsb",
                r"\\server\share\file.fsb",
            ),
            (
                r"\\?\C:\selected",
                r"Bibles\../file.fsb",
                r"\\?\C:\selected\file.fsb",
            ),
            (
                r"//?/UNC/server/share/selected",
                "Bibles",
                r"\\?\UNC\server\share\selected\Bibles",
            ),
            (r"C:/selected//", "Bibles", r"C:\selected\Bibles"),
            (
                r"C:\selected\Bibles",
                r"a\NUL:\..\b.fsb",
                r"C:\selected\Bibles\a\NUL:\..\b.fsb",
            ),
            (r"\\\selected", "Bibles", r"\selected\Bibles"),
        ] {
            assert_eq!(
                win32_join(parent, child),
                expected,
                "{parent:?} + {child:?}"
            );
        }
    }
    #[test]
    fn windows_namespace_rejection_is_explicit_and_does_not_reject_normal_extended_roots() {
        for path in [
            r"\\.\pipe\test.fsb",
            r"\\?\GLOBALROOT\Device\Harddisk0",
            r"\\?\pipe\test",
            r"C:\Bibles\NUL.fsb",
            r"C:\Bibles\com¹.fsb",
            r"C:\Bibles\NUL:stream.fsb",
        ] {
            assert_eq!(
                check_windows_path(path).unwrap_err().code,
                "scripture_unsupported"
            );
        }
        for path in [
            r"C:\Bibles\ordinary.fsb",
            r"\\?\C:\Bibles\ordinary.fsb",
            r"\\?\UNC\localhost\share\file.fsb",
            r"C:\Bibles\COM10.fsb",
            r"C:\Bibles\file:stream.fsb",
        ] {
            check_windows_path(path).unwrap();
        }
    }
}
