use crate::{storage, Error, Result};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub struct Entry {
    pub name: String,
    pub content: Vec<u8>,
}

pub struct Source {
    pub name: String,
    pub path: PathBuf,
}

pub fn validate_name(name: &str) -> Result<()> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty()
        || normalized.contains('\0')
        || normalized.starts_with('/')
        || (normalized.as_bytes().get(1) == Some(&b':')
            && normalized.as_bytes()[0].is_ascii_alphabetic())
        || normalized.split('/').any(|part| part == "..")
        || Path::new(&normalized)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Error::Invalid(format!("unsafe archive entry: {name}")));
    }
    Ok(())
}

/// Stream source files into an atomic archive. Missing entries are skipped as in
/// upstream yazl; a read error after opening an entry fails the whole archive.
pub fn write_zip(entries: &[Source], destination: &Path) -> Result<()> {
    if !destination.parent().is_some_and(Path::is_dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "archive parent directory does not exist",
        )
        .into());
    }
    for entry in entries {
        validate_name(&entry.name)?;
    }
    storage::atomic_replace(destination, |file| {
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));
        for entry in entries {
            let metadata = match fs::metadata(&entry.path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut input = fs::File::open(&entry.path)?;
            writer.start_file(
                &entry.name,
                options.large_file(metadata.len() >= u32::MAX as u64),
            )?;
            std::io::copy(&mut input, &mut writer)?;
        }
        writer.finish()?.flush()?;
        Ok(())
    })
}

pub fn read_zip(path: &Path) -> Result<Vec<Entry>> {
    let mut archive = ZipArchive::new(fs::File::open(path)?)?;
    if archive.len() > 100_000 {
        return Err(Error::Invalid("archive contains too many entries".into()));
    }
    let mut result = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.replace('\\', "/");
        validate_name(&name)?;
        if entry.encrypted() {
            return Err(Error::Invalid(
                "password-protected archives are not supported".into(),
            ));
        }
        // Match upstream's 50 MiB string conversion limit. Enforce the actual
        // streamed length too, rather than trusting ZIP headers alone.
        if entry.size() > 50 * 1024 * 1024 {
            return Err(Error::Invalid(format!(
                "archive entry is too large: {name}"
            )));
        }
        let mut content = Vec::new();
        entry
            .by_ref()
            .take(50 * 1024 * 1024 + 1)
            .read_to_end(&mut content)?;
        if content.len() > 50 * 1024 * 1024 {
            return Err(Error::Invalid(format!(
                "archive entry is too large: {name}"
            )));
        }
        total += content.len() as u64;
        if total > 1024 * 1024 * 1024 {
            return Err(Error::Invalid("uncompressed backup exceeds 1 GiB".into()));
        }
        result.push(Entry { name, content });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_names_allow_nested_unicode_and_colons_inside_linux_filenames() {
        assert!(validate_name("SHOWS/日本語: 🙌.show").is_ok());
        for name in [
            "/absolute.show",
            "C:/absolute.show",
            "SHOWS/../../escape.show",
            "\\\\server\\share",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_archive_write_preserves_the_previous_zip() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("entry.json");
        let output = root.path().join("backup.zip");
        fs::write(&source, "{\"original\":true}").unwrap();
        let entries = [Source {
            name: "SETTINGS.json".into(),
            path: source.clone(),
        }];
        write_zip(&entries, &output).unwrap();
        let previous = fs::read(&output).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o0)).unwrap();
        let result = write_zip(&entries, &output);
        fs::set_permissions(source, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(output).unwrap(), previous);
    }
}
