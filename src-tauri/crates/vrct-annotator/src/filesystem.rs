use crate::Result;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub(crate) fn relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.starts_with('/')
        || path.split('/').any(|part| {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            part.is_empty()
                || matches!(part, "." | "..")
                || part.ends_with(['.', ' '])
                || part
                    .chars()
                    .any(|c| c.is_control() || "<>\"|?*".contains(c))
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
        || !Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
    {
        return Err(format!("unsafe relative path: {path:?}"));
    }
    Ok(())
}
pub(crate) fn root(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).map_err(|e| format!("directory unavailable: {e}"))?;
    if !path.is_dir() {
        return Err("expected a directory".into());
    }
    Ok(path)
}
/// Canonical Windows paths retain their extended prefix for filesystem I/O,
/// while external applications and human instructions receive ordinary paths.
pub(crate) fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(network) = path.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{network}")
    } else {
        path.strip_prefix("\\\\?\\").unwrap_or(&path).to_owned()
    }
}
pub(crate) fn safe(root: &Path, name: &str) -> Result<PathBuf> {
    relative(name)?;
    let mut path = root.to_owned();
    for part in name.split('/') {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink()
                    || fs::canonicalize(&path).map_err(|e| e.to_string())? != path
                {
                    return Err(format!("linked path is unsupported: {name}"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(path)
}
pub(crate) fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| format!("file unavailable: {e}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
pub(crate) fn json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|e| format!("file unavailable: {e}"))?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).map_err(|_| "invalid JSON file".into())
}
pub(crate) fn atomic(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = safe(root, name)?;
    let parent = path.parent().ok_or("missing parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let path = safe(root, name)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(|e| e.to_string())?;
    temporary.write_all(bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist(&path)
        .map_err(|e| format!("atomic publication failed: {}", e.error))?;
    Ok(())
}
pub(crate) fn write_json(root: &Path, name: &str, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| "cannot serialize JSON")?;
    bytes.push(b'\n');
    atomic(root, name, &bytes)
}
pub struct Lock {
    file: File,
}
impl Lock {
    pub(crate) fn new(root: &Path, name: &str) -> Result<Self> {
        let path = safe(root, name)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|_| "This job is already in use by another process")?;
        Ok(Self { file })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn displayed_windows_roots_preserve_unc_and_unicode() {
        assert_eq!(display_path(Path::new(r"\\?\D:\作業 set")), r"D:\作業 set");
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\作業")),
            r"\\server\share\作業"
        );
    }
}
