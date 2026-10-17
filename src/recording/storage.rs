use super::{failure, RecordingCodec, RecordingErrorCode};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) const RESERVE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Serialize)]
pub struct StorageStatus {
    pub free_bytes: u64,
    pub reserve_bytes: u64,
}

pub(super) fn space(dir: &Path) -> Result<StorageStatus> {
    let mut parent = dir;
    while !parent.try_exists()? {
        parent = parent
            .parent()
            .ok_or_else(|| anyhow::anyhow!("No existing storage parent"))?;
    }
    let stat = nix::sys::statvfs::statvfs(parent)?;
    Ok(StorageStatus {
        free_bytes: stat.blocks_available().saturating_mul(stat.fragment_size()),
        reserve_bytes: RESERVE_BYTES,
    })
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Metadata {
    pub started_at: Option<u64>,
    pub codec: Option<RecordingCodec>,
    pub duration_seconds: Option<f64>,
    pub stop_reason: Option<RecordingErrorCode>,
}

fn sidecar_path(dir: &Path, name: &str) -> Result<PathBuf> {
    path(dir, name)?;
    Ok(dir.join(format!(
        "{}.json",
        name.trim_end_matches(".part").trim_end_matches(".mkv")
    )))
}

pub(super) fn write_metadata(dir: &Path, name: &str, metadata: &Metadata) -> Result<()> {
    let destination = sidecar_path(dir, name)?;
    let temporary = dir.join(format!("{}.json.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(metadata)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub(super) fn read_metadata(dir: &Path, name: &str) -> Metadata {
    let read = || -> Result<Metadata> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(sidecar_path(dir, name)?)?;
        let stat = file.metadata()?;
        if !stat.is_file() || stat.nlink() != 1 || stat.len() > 4096 {
            return Err(failure(
                RecordingErrorCode::StorageError,
                "Invalid recording metadata",
            ));
        }
        Ok(serde_json::from_reader(file.take(4097))?)
    };
    read().unwrap_or_default()
}

pub(super) fn delete(dir: &Path, name: &str) -> Result<()> {
    let _file = open(dir, name)?;
    std::fs::remove_file(path(dir, name)?)?;
    match std::fs::remove_file(sidecar_path(dir, name)?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

pub(super) fn download_name(started_at: Option<u64>, modified: u64) -> String {
    let timestamp = libc::time_t::try_from(started_at.unwrap_or(modified)).unwrap_or(0);
    // time's local offset lookup refuses to run in multithreaded processes; localtime_r is thread-safe.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut tm) }.is_null() {
        unsafe { libc::gmtime_r(&timestamp, &mut tm) };
    }
    format!(
        "one-kvm-{:04}{:02}{:02}-{:02}{:02}{:02}.mkv",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[derive(Serialize)]
pub struct RecordingFile {
    pub name: String,
    pub bytes: u64,
    pub modified: u64,
    pub complete: bool,
    pub started_at: Option<u64>,
    pub duration_seconds: Option<f64>,
    pub codec: Option<RecordingCodec>,
    pub stop_reason: Option<RecordingErrorCode>,
    pub download_name: String,
}

pub(super) fn prepare(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(failure(
            RecordingErrorCode::StorageError,
            "Recording directory is not a regular directory",
        ));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    check_space(dir)
}

pub(super) fn check_space(dir: &Path) -> Result<()> {
    let available = space(dir).map_err(|e| failure(RecordingErrorCode::StorageError, e))?;
    if available.free_bytes < RESERVE_BYTES {
        return Err(failure(
            RecordingErrorCode::LowStorage,
            "Recording stopped: less than 256 MiB free storage",
        ));
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
        return Err(failure(
            RecordingErrorCode::StorageError,
            "Invalid recording filename",
        ));
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
        return Err(failure(
            RecordingErrorCode::StorageError,
            "Not a private recording file",
        ));
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
        let sidecar = read_metadata(dir, &name);
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        files.push(RecordingFile {
            complete: name.ends_with(".mkv"),
            name,
            bytes: metadata.len(),
            modified,
            download_name: download_name(sidecar.started_at, modified),
            started_at: sidecar.started_at,
            duration_seconds: sidecar.duration_seconds,
            codec: sidecar.codec,
            stop_reason: sidecar.stop_reason,
        });
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.modified));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_lifecycle_and_media_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let (name, file) = create(dir.path()).unwrap();
        drop(file);
        let mut metadata = Metadata {
            started_at: Some(1_700_000_000),
            codec: Some(RecordingCodec::H265),
            ..Default::default()
        };
        write_metadata(dir.path(), &name, &metadata).unwrap();
        let sidecar = sidecar_path(dir.path(), &name).unwrap();
        assert_eq!(std::fs::metadata(&sidecar).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            read_metadata(dir.path(), &name).started_at,
            metadata.started_at
        );
        let json_name = sidecar.file_name().unwrap().to_str().unwrap();
        assert!(open(dir.path(), json_name).is_err());
        assert!(delete(dir.path(), json_name).is_err());
        metadata.duration_seconds = Some(12.345);
        metadata.stop_reason = Some(RecordingErrorCode::CodecChanged);
        write_metadata(dir.path(), &name, &metadata).unwrap();
        let complete = name.trim_end_matches(".part");
        std::fs::rename(dir.path().join(&name), dir.path().join(complete)).unwrap();
        let files = list(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].complete);
        assert_eq!(files[0].duration_seconds, Some(12.345));
        assert_eq!(files[0].stop_reason, Some(RecordingErrorCode::CodecChanged));
        delete(dir.path(), complete).unwrap();
        assert!(!sidecar.exists());
        assert!(list(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn malformed_missing_and_linked_metadata_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (name, _) = create(dir.path()).unwrap();
        let sidecar = sidecar_path(dir.path(), &name).unwrap();
        assert!(list(dir.path()).unwrap()[0].started_at.is_none());
        std::fs::write(&sidecar, b"{broken").unwrap();
        assert!(list(dir.path()).unwrap()[0].started_at.is_none());
        let target = dir.path().join("private");
        std::fs::write(&target, br#"{"started_at":1700000000,"codec":"h264"}"#).unwrap();
        std::fs::remove_file(&sidecar).unwrap();
        std::os::unix::fs::symlink(&target, &sidecar).unwrap();
        assert!(list(dir.path()).unwrap()[0].started_at.is_none());
        // Atomic replacement must replace the link, never its target.
        let before = std::fs::read(&target).unwrap();
        write_metadata(dir.path(), &name, &Metadata::default()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), before);
        std::fs::remove_file(&sidecar).unwrap();
        std::fs::hard_link(&target, &sidecar).unwrap();
        assert!(list(dir.path()).unwrap()[0].started_at.is_none());
        delete(dir.path(), &name).unwrap();
        assert!(target.exists());
    }

    #[test]
    fn list_keeps_modified_order_and_formats_download_dates() {
        let dir = tempfile::tempdir().unwrap();
        let (older, old_file) = create(dir.path()).unwrap();
        let (newer, new_file) = create(dir.path()).unwrap();
        let epoch = std::time::UNIX_EPOCH;
        old_file
            .set_modified(epoch + std::time::Duration::from_secs(1_700_000_000))
            .unwrap();
        new_file
            .set_modified(epoch + std::time::Duration::from_secs(1_700_000_100))
            .unwrap();
        write_metadata(
            dir.path(),
            &older,
            &Metadata {
                started_at: Some(1_800_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        let files = list(dir.path()).unwrap();
        assert_eq!(
            files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec![newer.as_str(), older.as_str()]
        );
        assert_eq!(
            files[0].download_name,
            download_name(Some(1_700_000_100), 0)
        );
        assert_eq!(
            files[1].download_name,
            download_name(Some(1_800_000_000), 0)
        );
        assert!(download_name(None, 1_700_000_000)
            .strip_prefix("one-kvm-2023111")
            .and_then(|rest| rest.strip_suffix(".mkv"))
            .is_some_and(|rest| rest.len() == 8 && rest.as_bytes()[1] == b'-'));
    }

    #[test]
    fn storage_probe_uses_existing_parent_without_creating_directories() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not/created");
        assert_eq!(space(&missing).unwrap().reserve_bytes, RESERVE_BYTES);
        assert!(!missing.exists());
    }
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
