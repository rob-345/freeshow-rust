use crate::*;
use std::{
    collections::HashSet,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
};
use zip::ZipArchive;

pub(crate) struct Archive {
    zip: ZipArchive<CheckedCursor>,
    pub names: Vec<String>,
}
struct CheckedCursor {
    cursor: Cursor<Vec<u8>>,
    guard: IoGuard,
}
impl Read for CheckedCursor {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.guard.check()?;
        self.cursor.read(bytes)
    }
}
impl Seek for CheckedCursor {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.guard.check()?;
        self.cursor.seek(position)
    }
}
fn invalid(message: impl Into<String>) -> ImportError {
    ImportError::new("import_zip_error", message)
}
fn u16le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn u32le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn checked_end(start: usize, length: usize, limit: usize) -> Result<usize> {
    start
        .checked_add(length)
        .filter(|end| *end <= limit)
        .ok_or_else(|| invalid("Truncated ZIP headers"))
}
fn preflight(bytes: &[u8], budget: &mut Budget) -> Result<Vec<String>> {
    let end = (bytes.len().saturating_sub(65557)..bytes.len().saturating_sub(21))
        .rev()
        .find(|offset| {
            bytes.get(*offset..*offset + 4) == Some(b"PK\x05\x06")
                && *offset + 22 + u16le(bytes, *offset + 20) as usize == bytes.len()
        })
        .ok_or_else(|| invalid("Missing ZIP end record"))?;
    let count = u16le(bytes, end + 10) as usize;
    let size = u32le(bytes, end + 12) as usize;
    let start = u32le(bytes, end + 16) as usize;
    if u16le(bytes, end + 4) != 0
        || u16le(bytes, end + 6) != 0
        || u16le(bytes, end + 8) as usize != count
    {
        return Err(ImportError::new(
            "unsupported_archive",
            "Multi-disk ZIP is unsupported",
        ));
    }
    if count == 65535 || size == u32::MAX as usize || start == u32::MAX as usize {
        return Err(ImportError::new(
            "unsupported_archive",
            "ZIP64 is unsupported",
        ));
    }
    budget.entries = budget
        .entries
        .checked_add(count)
        .ok_or_else(|| ImportError::resource("ZIP entry count overflow"))?;
    if budget.entries > budget.config.max_entries || size > budget.config.max_entries * 131072 {
        return Err(ImportError::resource("ZIP central directory/entry limit"));
    }
    if checked_end(start, size, end)? != end {
        return Err(invalid("Unsupported ZIP central directory layout"));
    }
    let mut offset = start;
    let mut names = vec![];
    let mut seen = HashSet::new();
    let mut declared = 0u64;
    for _ in 0..count {
        budget.check()?;
        checked_end(offset, 46, end)?;
        if &bytes[offset..offset + 4] != b"PK\x01\x02" {
            return Err(invalid("Invalid ZIP central header"));
        }
        let flags = u16le(bytes, offset + 8);
        let method = u16le(bytes, offset + 10);
        let compressed = u32le(bytes, offset + 20) as u64;
        let expanded = u32le(bytes, offset + 24) as u64;
        let name_len = u16le(bytes, offset + 28) as usize;
        let extra_len = u16le(bytes, offset + 30) as usize;
        let comment_len = u16le(bytes, offset + 32) as usize;
        let local = u32le(bytes, offset + 42) as usize;
        if flags & 1 != 0 {
            return Err(ImportError::new(
                "import_encrypted_archive",
                "Encrypted ZIP is unsupported",
            ));
        }
        if !matches!(method, 0 | 8) {
            return Err(ImportError::new(
                "unsupported_archive",
                "ZIP compression method is unsupported",
            ));
        }
        if expanded == u32::MAX as u64
            || compressed == u32::MAX as u64
            || local == u32::MAX as usize
        {
            return Err(ImportError::new(
                "unsupported_archive",
                "ZIP64 entry is unsupported",
            ));
        }
        if name_len == 0 || name_len > 32768 || extra_len > 4096 || comment_len > 4096 {
            return Err(ImportError::resource("ZIP header field length limit"));
        }
        let next = checked_end(offset, 46 + name_len + extra_len + comment_len, end)?;
        let name =
            String::from_utf8_lossy(&bytes[offset + 46..offset + 46 + name_len]).into_owned();
        let normalized = name.replace('\\', "/");
        let components: Vec<_> = normalized.trim_end_matches('/').split('/').collect();
        if normalized.starts_with('/')
            || normalized.contains(['\0', ':'])
            || components
                .iter()
                .any(|part| part.is_empty() || matches!(*part, "." | ".."))
            || components.len() > budget.config.max_depth
        {
            return Err(ImportError::new(
                "import_invalid_path",
                "Unsafe ZIP entry path/depth",
            ));
        }
        if !seen.insert(normalized.trim_end_matches('/').to_owned()) {
            return Err(ImportError::new(
                "import_invalid_path",
                "Duplicate ZIP entry destination",
            ));
        }
        let mode = u32le(bytes, offset + 38) >> 16;
        let kind = mode & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            return Err(ImportError::new(
                "import_invalid_path",
                "Non-regular ZIP entry",
            ));
        }
        declared = declared
            .checked_add(expanded)
            .ok_or_else(|| ImportError::resource("ZIP declared output overflow"))?;
        if expanded > budget.config.entry_bytes
            || declared > budget.config.expanded_bytes.saturating_sub(budget.expanded)
            || expanded
                > compressed
                    .max(1)
                    .saturating_mul(budget.config.expansion_ratio)
        {
            return Err(ImportError::resource("ZIP size/expansion limit"));
        }
        let local_end = checked_end(local, 30, start)?;
        if &bytes[local..local + 4] != b"PK\x03\x04" {
            return Err(invalid("Invalid ZIP local header"));
        }
        let local_name = u16le(bytes, local + 26) as usize;
        let local_extra = u16le(bytes, local + 28) as usize;
        if local_name != name_len
            || local_extra > 4096
            || u16le(bytes, local + 6) != flags
            || u16le(bytes, local + 8) != method
        {
            return Err(invalid("ZIP local/central header mismatch"));
        }
        let data_start = checked_end(local_end, local_name + local_extra, start)?;
        checked_end(data_start, compressed as usize, start)?;
        if bytes[local + 30..local + 30 + local_name] != bytes[offset + 46..offset + 46 + name_len]
        {
            return Err(invalid("ZIP local/central name mismatch"));
        }
        // zip allocates/parses extras before entry data. Bound each field and reject
        // alternate names/ZIP64 to keep the raw yauzl UTF-8 name contract.
        for (mut extra, limit) in [
            (offset + 46 + name_len, offset + 46 + name_len + extra_len),
            (local + 30 + local_name, data_start),
        ] {
            while extra < limit {
                checked_end(extra, 4, limit)?;
                let tag = u16le(bytes, extra);
                let length = u16le(bytes, extra + 2) as usize;
                extra = checked_end(extra, 4 + length, limit)?;
                if matches!(tag, 1 | 0x7075) {
                    return Err(ImportError::new(
                        "unsupported_archive",
                        "ZIP64/alternate Unicode path extras are unsupported",
                    ));
                }
            }
        }
        names.push(normalized);
        offset = next;
    }
    if offset != end {
        return Err(invalid("ZIP directory size mismatch"));
    }
    Ok(names)
}
impl Archive {
    pub fn open(path: &Path, budget: &mut Budget) -> Result<Self> {
        budget.archives += 1;
        if budget.archives > budget.config.max_archives {
            return Err(ImportError::resource("Too many import archives"));
        }
        let bytes = super::prepare::read_bytes(path, budget.config.archive_bytes, budget, false)?
            .ok_or_else(|| invalid("Cannot read ZIP file"))?;
        let names = preflight(&bytes, budget)?;
        budget.check()?;
        let parsed = ZipArchive::new(CheckedCursor {
            cursor: Cursor::new(bytes),
            guard: budget.io_guard(),
        });
        budget.check()?;
        let zip = parsed.map_err(|error| invalid(error.to_string()))?;
        if zip.len() != names.len() {
            return Err(invalid("ZIP entry count mismatch"));
        }
        Ok(Self { zip, names })
    }
    pub fn read(
        &mut self,
        index: usize,
        budget: &mut Budget,
        limit: u64,
        mut output: Option<&mut dyn Write>,
    ) -> Result<Vec<u8>> {
        budget.check()?;
        let entry = self.zip.by_index(index);
        budget.check()?;
        let mut entry = entry.map_err(|error| invalid(error.to_string()))?;
        if entry.encrypted() {
            return Err(ImportError::new(
                "import_encrypted_archive",
                "Encrypted ZIP is unsupported",
            ));
        }
        let mut buffer = [0u8; 32768];
        let mut data = vec![];
        let mut count = 0u64;
        loop {
            budget.check()?;
            let read = entry.read(&mut buffer);
            budget.check()?;
            let read = read.map_err(|error| invalid(error.to_string()))?;
            if read == 0 {
                break;
            }
            count += read as u64;
            budget.charge_expanded(read as u64)?;
            if count > limit {
                return Err(ImportError::resource("ZIP entry actual byte limit"));
            }
            if let Some(ref mut writer) = output {
                writer.write_all(&buffer[..read]).map_err(ImportError::io)?;
            } else {
                data.extend_from_slice(&buffer[..read]);
            }
        }
        if count != entry.size() {
            return Err(invalid("ZIP actual size mismatch"));
        }
        Ok(data)
    }
    pub fn discard(&mut self, index: usize, budget: &mut Budget) -> Result<()> {
        self.read(
            index,
            budget,
            budget.config.entry_bytes,
            Some(&mut std::io::sink()),
        )
        .map(|_| ())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_zip_is_rejected_without_parser() {
        let mut budget = Budget::new(ImportConfig::default(), CancellationToken::new());
        assert_eq!(
            preflight(b"PK\x03\x04", &mut budget).unwrap_err().code,
            "import_zip_error"
        );
    }
}
