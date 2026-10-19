use crate::*;
use std::{fs, io::Write};

pub(crate) struct Budget {
    pub limits: Limits,
    pub control: Control,
    pub service_cancel: CancellationToken,
    pub directories: usize,
    pub entries: usize,
    pub steps: usize,
    pub path_bytes: usize,
}
impl Budget {
    pub fn check(&self) -> Result<()> {
        if self.service_cancel.is_cancelled() {
            return Err(Error::closed());
        }
        if self.control.cancel.is_cancelled() {
            return Err(Error::new(
                "ai_local_cancelled",
                "Local inventory request cancelled",
            ));
        }
        if Instant::now() >= self.control.deadline {
            return Err(Error::new(
                "ai_local_deadline",
                "Local inventory deadline elapsed",
            ));
        }
        Ok(())
    }
    fn charge_path(&mut self, path: &Path) -> Result<()> {
        let bytes = path_text(path)?.len();
        self.path_bytes = self
            .path_bytes
            .checked_add(bytes)
            .ok_or_else(Error::resource)?;
        if self.path_bytes > self.limits.path_bytes {
            return Err(Error::resource());
        }
        Ok(())
    }
    fn step(&mut self) -> Result<()> {
        self.check()?;
        self.steps += 1;
        if self.steps > self.limits.steps {
            Err(Error::resource())
        } else {
            Ok(())
        }
    }
}
#[cfg(windows)]
fn reparse(path: &Path, budget: &mut Budget) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    budget.step()?;
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_attributes() & 0x400 != 0) {
        return Err(Error::unsupported());
    }
    Ok(())
}
fn add(total: u64, size: u64) -> Result<u64> {
    if size > MAX_EXACT_SIZE {
        return Err(Error::unsupported());
    }
    total
        .checked_add(size)
        .filter(|v| *v <= MAX_EXACT_SIZE)
        .ok_or_else(Error::unsupported)
}
fn folder(path: &Path, depth: usize, budget: &mut Budget) -> Result<u64> {
    path_text(path)?;
    if depth > budget.limits.depth {
        return Err(Error::resource());
    }
    budget.directories += 1;
    if budget.directories > budget.limits.directories {
        return Err(Error::resource());
    }
    #[cfg(windows)]
    reparse(path, budget)?;
    budget.step()?;
    let Ok(directory) = fs::read_dir(path) else {
        return Ok(0);
    };
    let mut entries = Vec::new();
    // Source readdirSync constructs the complete Dirent array before its map.
    // If enumeration/type retrieval fails, discard this whole folder, not just
    // its as-yet-unseen suffix. Native resource/cancel errors are never zero.
    for entry in directory {
        budget.step()?;
        let Ok(entry) = entry else { return Ok(0) };
        budget.entries += 1;
        if budget.entries > budget.limits.entries {
            return Err(Error::resource());
        }
        let child = entry.path();
        budget.charge_path(&child)?;
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => return Ok(0),
        };
        #[cfg(windows)]
        reparse(&child, budget)?;
        entries.push((child, kind.is_dir()));
    }
    let mut total = 0;
    for (child, is_dir) in entries {
        let size = if is_dir {
            folder(&child, depth + 1, budget)?
        } else {
            budget.step()?;
            match fs::metadata(&child) {
                Ok(stat) => stat.len(),
                Err(_) => 0,
            }
        };
        total = add(total, size)?;
    }
    budget.check()?;
    Ok(total)
}
struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > MAX_RESULT {
            return Err(std::io::Error::other("Local inventory result bound"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn query(root: &Path, budget: &mut Budget) -> Result<Vec<BinFile>> {
    budget.check()?;
    #[cfg(windows)]
    {
        reparse(root, budget)?;
        reparse(&root.join("bin"), budget)?;
    }
    let path = root.join("bin").join("nemotron");
    let text = path_text(&path)?;
    budget.charge_path(&path)?;
    let size = folder(&path, 0, budget)?;
    let result = if size > 0 {
        vec![BinFile {
            path: text.into(),
            name: "nemotron",
            size,
        }]
    } else {
        vec![]
    };
    serde_json::to_writer(&mut Count(0), &result).map_err(|_| Error::resource())?;
    budget.check()?;
    Ok(result)
}
#[cfg(test)]
pub(crate) fn sum_for_test(a: u64, b: u64) -> Result<u64> {
    add(a, b)
}
