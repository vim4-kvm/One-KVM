use anyhow::{bail, Result};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) const RESERVE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize)]
pub struct RecordingFile {
    pub name: String,
    pub bytes: u64,
    pub modified: u64,
    pub complete: bool,
}

pub(super) fn prepare(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        bail!("Recording directory is not a regular directory");
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    check_space(dir)
}

pub(super) fn check_space(dir: &Path) -> Result<()> {
    let stat = nix::sys::statvfs::statvfs(dir)?;
    if stat.blocks_available().saturating_mul(stat.fragment_size()) < RESERVE_BYTES {
        bail!("Recording stopped: less than 256 MiB free storage");
    }
    Ok(())
}

pub(super) fn create(dir: &Path) -> Result<(String, File)> {
    let name = format!("{}.mkv.part", uuid::Uuid::new_v4());
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(&name))?;
    Ok((name, file))
}

pub(super) fn path(dir: &Path, name: &str) -> Result<PathBuf> {
    let id = name
        .strip_suffix(".mkv.part")
        .or_else(|| name.strip_suffix(".mkv"));
    if !id.is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)) {
        bail!("Invalid recording filename");
    }
    Ok(dir.join(name))
}

pub(super) fn open(dir: &Path, name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path(dir, name)?)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!("Not a private recording file");
    }
    Ok(file)
}

pub(super) fn list(dir: &Path) -> Result<Vec<RecordingFile>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let Ok(file) = open(dir, &name) else {
            continue;
        };
        let metadata = file.metadata()?;
        files.push(RecordingFile {
            complete: name.ends_with(".mkv"),
            name,
            bytes: metadata.len(),
            modified: metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        });
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.modified));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recordings_cannot_escape_storage_or_follow_links() {
        let dir = tempfile::tempdir().unwrap();
        assert!(path(dir.path(), "../../secret.mkv").is_err());
        let (name, file) = create(dir.path()).unwrap();
        drop(file);
        assert!(open(dir.path(), &name).is_ok());
        let other = dir.path().join("private");
        std::fs::hard_link(dir.path().join(&name), &other).unwrap();
        assert!(open(dir.path(), &name).is_err());
        std::fs::remove_file(dir.path().join(&name)).unwrap();
        std::os::unix::fs::symlink(&other, dir.path().join(&name)).unwrap();
        assert!(open(dir.path(), &name).is_err());
    }
}
