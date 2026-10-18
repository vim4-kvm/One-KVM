//! NAS discovery never implies that its mountpoint is safe to write.
use super::storage;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

pub const IO_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    #[error("not_mounted")]
    NotMounted,
    #[error("unresponsive")]
    Unresponsive,
    #[error("not_writable")]
    NotWritable,
}

#[derive(Clone, Default, Serialize)]
pub struct Location {
    pub id: String,
    pub name: String,
    pub folder: String,
}

#[derive(Clone, Serialize)]
pub struct Availability {
    #[serde(flatten)]
    pub location: Location,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Unavailable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
    pub reserve_bytes: u64,
}

#[derive(Deserialize)]
struct Nas {
    id: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

pub struct Locations {
    pub local: PathBuf,
    pub config_dir: PathBuf,
    pub mount_root: PathBuf,
    pub mountinfo: PathBuf,
    gates: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
}

/// A detached OS thread cannot hold Tokio runtime shutdown hostage. The permit
/// stays with that thread after timeout, so repeated polls cannot accumulate workers.
pub async fn bounded<T: Send + 'static>(
    gate: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Unavailable> {
    let deadline = tokio::time::Instant::now() + IO_TIMEOUT;
    // Concurrent fast probes share the same budget rather than reporting a
    // healthy filesystem as unresponsive just because another request owns it.
    let permit = tokio::time::timeout_at(deadline, gate.acquire_owned())
        .await
        .map_err(|_| Unavailable::Unresponsive)?
        .map_err(|_| Unavailable::Unresponsive)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = work();
        let _ = tx.send(result);
        // If the caller timed out, dropping a returned file can itself perform
        // network I/O. Keep the permit until that cleanup also completes.
        drop(permit);
    });
    tokio::time::timeout_at(deadline, rx)
        .await
        .map_err(|_| Unavailable::Unresponsive)?
        .map_err(|_| Unavailable::Unresponsive)
}

impl Locations {
    pub fn new(local: PathBuf) -> Self {
        Self {
            local,
            config_dir: "/etc/vim4-kvm/nas".into(),
            mount_root: "/var/lib/vim4-kvm/nas".into(),
            mountinfo: "/proc/self/mountinfo".into(),
            gates: Mutex::new(HashMap::new()),
        }
    }
    pub fn gate(&self, id: &str) -> Arc<tokio::sync::Semaphore> {
        self.gates
            .lock()
            .unwrap()
            .entry(id.into())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    }
    pub fn discover(&self, config: &crate::config::RecordingConfig) -> Vec<Availability> {
        let mut result = vec![Availability {
            location: Location {
                id: "local".into(),
                name: "本机".into(),
                folder: String::new(),
            },
            kind: None,
            available: false,
            reason: None,
            free_bytes: None,
            reserve_bytes: storage::RESERVE_BYTES,
        }];
        if let Ok(entries) = std::fs::read_dir(&self.config_dir) {
            for entry in entries.flatten() {
                let read = || -> anyhow::Result<Nas> {
                    use std::io::Read;
                    let file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(entry.path())?;
                    anyhow::ensure!(file.metadata()?.is_file(), "Invalid NAS config");
                    Ok(serde_json::from_reader(file.take(65536))?)
                };
                if let Ok(nas) = read() {
                    if !valid_id(&nas.id)
                        || entry.file_name() != format!("{}.json", nas.id).as_str()
                        || !matches!(nas.kind.as_str(), "smb" | "nfs")
                    {
                        continue;
                    }
                    let id = format!("nas:{}", nas.id);
                    result.push(Availability {
                        location: Location {
                            folder: config
                                .folders
                                .get(&id)
                                .cloned()
                                .unwrap_or_else(|| "One-KVM".into()),
                            id,
                            name: nas.name,
                        },
                        kind: Some(nas.kind),
                        available: false,
                        reason: None,
                        free_bytes: None,
                        reserve_bytes: storage::RESERVE_BYTES,
                    });
                }
            }
        }
        result
    }

    pub fn directory(&self, location: &Location, create: bool) -> Result<Directory, Unavailable> {
        self.open_directory(location, create)?
            .ok_or(Unavailable::NotWritable)
    }

    /// Missing historical folders are omitted without recreating them.
    pub fn directory_for_listing(
        &self,
        location: &Location,
    ) -> Result<Option<Directory>, Unavailable> {
        self.open_directory(location, false)
    }

    fn open_directory(
        &self,
        location: &Location,
        create: bool,
    ) -> Result<Option<Directory>, Unavailable> {
        if location.id == "local" {
            if create {
                std::fs::create_dir_all(&self.local).map_err(|_| Unavailable::NotWritable)?;
            }
            let dir = match open_dir(&self.local) {
                Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None)
                }
                result => result.map_err(|_| Unavailable::NotWritable)?,
            };
            if create {
                use std::os::unix::fs::PermissionsExt;
                dir.set_permissions(std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| Unavailable::NotWritable)?;
            }
            return Ok(Some(Directory(dir)));
        }
        let id = location
            .id
            .strip_prefix("nas:")
            .filter(|id| valid_id(id))
            .ok_or(Unavailable::NotMounted)?;
        validate_folder(&location.folder).map_err(|_| Unavailable::NotWritable)?;
        let mount = self.mount_root.join(id);
        let expected = mount_id(&self.mountinfo, &mount).ok_or(Unavailable::NotMounted)?;
        let mut dir = open_dir(&mount).map_err(|_| Unavailable::NotWritable)?;
        // Compare the opened descriptor's mount ID, not a path stat: unmount and
        // replacement between mountinfo and open must never select the root disk.
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", dir.as_raw_fd()))
            .map_err(|_| Unavailable::NotMounted)?;
        let actual = info.lines().find_map(|l| {
            l.strip_prefix("mnt_id:")
                .and_then(|v| v.trim().parse::<u64>().ok())
        });
        if actual != Some(expected) {
            return Err(Unavailable::NotMounted);
        }
        for component in location.folder.split('/') {
            let name = std::ffi::CString::new(component).map_err(|_| Unavailable::NotWritable)?;
            if create {
                let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
                if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                    return Err(Unavailable::NotWritable);
                }
            }
            let fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                if !create && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
                    return Ok(None);
                }
                return Err(Unavailable::NotWritable);
            }
            dir = unsafe { File::from_raw_fd(fd) };
        }
        Ok(Some(Directory(dir)))
    }
}

pub struct Directory(File);
impl Directory {
    pub fn same_mount(&self, other: &Self) -> bool {
        fn id(file: &File) -> Option<u64> {
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))
                .ok()?
                .lines()
                .find_map(|line| {
                    line.strip_prefix("mnt_id:")
                        .and_then(|value| value.trim().parse().ok())
                })
        }
        let mounted = id(&self.0);
        mounted.is_some() && mounted == id(&other.0)
    }
    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
    }
    pub fn probe(&self) -> anyhow::Result<storage::StorageStatus> {
        let path = self.path();
        let space = storage::space(&path)?;
        // Probes must never appear as recordings, even if a timed-out unlink
        // finishes later than a concurrent library scan.
        let probe = path.join(format!(".{}.probe", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&probe)?;
        std::fs::remove_file(probe)?;
        drop(file);
        Ok(space)
    }
}
fn open_dir(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && id.as_bytes()[0] != b'-'
}
pub fn validate_folder(folder: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !folder.is_empty()
            && folder.len() <= 4096
            && !folder.chars().any(char::is_control)
            && folder
                .split('/')
                .all(|s| !s.is_empty() && s != "." && s != ".."),
        "Invalid relative recording folder"
    );
    Ok(())
}
fn mount_id(info: &Path, mount: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(info).ok()?;
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let encoded = *fields.get(4)?;
            let decoded = encoded
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            (Path::new(&decoded) == mount)
                .then(|| fields[0].parse().ok())
                .flatten()
        })
        .last()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub fn fixture(root: &Path) -> Locations {
        let mut locations = Locations::new(root.join("local"));
        locations.config_dir = root.join("config");
        locations.mount_root = root.join("mounts");
        locations.mountinfo = root.join("mountinfo");
        std::fs::create_dir_all(&locations.config_dir).unwrap();
        let mount = locations.mount_root.join("test");
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(
            locations.config_dir.join("test.json"),
            r#"{"id":"test","name":"Test NAS","type":"smb"}"#,
        )
        .unwrap();
        let dir = open_dir(&mount).unwrap();
        let info =
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", dir.as_raw_fd())).unwrap();
        let id = info
            .lines()
            .find_map(|l| l.strip_prefix("mnt_id:"))
            .unwrap()
            .trim();
        std::fs::write(
            &locations.mountinfo,
            format!("{id} 1 0:1 / {} rw - cifs server rw\n", mount.display()),
        )
        .unwrap();
        locations
    }

    #[test]
    fn mountinfo_is_required_and_opened_mount_must_match() {
        let root = tempfile::tempdir().unwrap();
        let locations = fixture(root.path());
        let target = Location {
            id: "nas:test".into(),
            name: "Test".into(),
            folder: "nested/recordings".into(),
        };
        assert!(locations.directory(&target, true).is_ok());
        std::fs::write(&locations.mountinfo, "").unwrap();
        assert!(matches!(
            locations.directory(&target, true),
            Err(Unavailable::NotMounted)
        ));
        std::fs::write(
            &locations.mountinfo,
            format!(
                "999999 1 0:1 / {} rw - cifs server rw",
                locations.mount_root.join("test").display()
            ),
        )
        .unwrap();
        assert!(matches!(
            locations.directory(&target, true),
            Err(Unavailable::NotMounted)
        ));
        assert!(!locations.local.exists());
    }

    #[test]
    fn folders_reject_escape_and_pinned_directory_survives_replacement() {
        for folder in ["", "/tmp", "..", "a/../b", "a//b", "a/./b", "a\nb", "a\0b"] {
            assert!(validate_folder(folder).is_err(), "{folder:?}");
        }
        let root = tempfile::tempdir().unwrap();
        let locations = fixture(root.path());
        let mount = locations.mount_root.join("test");
        std::os::unix::fs::symlink(root.path(), mount.join("escape")).unwrap();
        let mut target = Location {
            id: "nas:test".into(),
            name: "Test".into(),
            folder: "escape/files".into(),
        };
        assert!(locations.directory(&target, true).is_err());
        assert!(!root.path().join("files").exists());
        target.folder = "safe/nested".into();
        let directory = locations.directory(&target, true).unwrap();
        std::fs::rename(mount.join("safe"), mount.join("old")).unwrap();
        std::os::unix::fs::symlink(root.path(), mount.join("safe")).unwrap();
        let (name, _) = storage::create(&directory.path()).unwrap();
        assert!(mount.join("old/nested").join(&name).exists());
        assert!(!root.path().join(&name).exists());
    }

    #[test]
    fn mountinfo_decodes_escaped_mount_names() {
        let root = tempfile::tempdir().unwrap();
        let info = root.path().join("info");
        std::fs::write(&info, "42 1 0:1 / /nas/a\\040b rw - nfs server rw\n").unwrap();
        assert_eq!(mount_id(&info, Path::new("/nas/a b")), Some(42));
        assert_eq!(mount_id(&info, Path::new("/nas/a")), None);
    }

    #[tokio::test]
    async fn timeout_retains_worker_permit_until_io_completes() {
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let (release, wait) = std::sync::mpsc::channel();
        assert_eq!(
            bounded(gate.clone(), move || wait.recv().unwrap()).await,
            Err(Unavailable::Unresponsive)
        );
        assert_eq!(
            bounded(gate.clone(), || ()).await,
            Err(Unavailable::Unresponsive)
        );
        release.send(()).unwrap();
        let permit = tokio::time::timeout(IO_TIMEOUT, gate.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert!(bounded(gate, || ()).await.is_ok());
    }
}
