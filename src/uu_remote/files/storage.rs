//! File access is relative to held directory descriptors; remote paths never name host paths.
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub const FREE_RESERVE: u64 = 128 * 1024 * 1024;
pub type Media = Arc<RwLock<Option<crate::msd::MsdController>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Files,
    Images,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub area: Area,
    pub parts: Vec<String>,
}

impl Location {
    pub fn parse(raw: &str) -> io::Result<Option<Self>> {
        if raw.len() > 4096 || raw.contains('\0') {
            return Err(invalid());
        }
        let text = raw.replace('\\', "/");
        let text = text.strip_prefix('~').unwrap_or(&text);
        let text =
            if text.as_bytes().get(1) == Some(&b':') && text.as_bytes()[0].is_ascii_alphabetic() {
                &text[2..]
            } else {
                text.strip_prefix(':').unwrap_or(text)
            };
        if text.is_empty() || text == "/" {
            return Ok(None);
        }
        if !text.starts_with('/') {
            return Err(invalid());
        }
        let mut parts = text.trim_end_matches('/').split('/').skip(1);
        let area = match parts.next() {
            Some("files") => Area::Files,
            Some("images") => Area::Images,
            _ => return Err(denied()),
        };
        let parts: Vec<String> = parts.map(str::to_owned).collect();
        validate_parts(&parts)?;
        Ok(Some(Self { area, parts }))
    }

    pub fn child(&self, relative: &str) -> io::Result<Self> {
        if relative.starts_with(['/', '\\']) || relative.contains(':') {
            return Err(invalid());
        }
        let relative = relative.replace('\\', "/");
        let mut parts = self.parts.clone();
        parts.extend(relative.split('/').map(str::to_owned));
        validate_parts(&parts)?;
        Ok(Self {
            area: self.area,
            parts,
        })
    }

    pub fn display(&self) -> String {
        let prefix = if self.area == Area::Files {
            "/files"
        } else {
            "/images"
        };
        if self.parts.is_empty() {
            prefix.into()
        } else {
            format!("{prefix}/{}", self.parts.join("/"))
        }
    }
}

fn validate_parts(parts: &[String]) -> io::Result<()> {
    if parts.len() > 16
        || parts.iter().any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || p.len() > 255
                || p.contains(['\0', ':', '/', '\\'])
        })
    {
        Err(invalid())
    } else {
        Ok(())
    }
}

pub fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidInput)
}
pub fn denied() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn open_at(parent: &File, name: &str, flags: i32) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub struct Storage {
    files: PathBuf,
    media: parking_lot::RwLock<Option<Media>>,
    reserved: AtomicU64,
}

/// Held until a transfer's file is closed, excluding gadget mount/delete and controller replacement.
pub struct Access {
    root: File,
    _controller: Option<OwnedRwLockReadGuard<Option<crate::msd::MsdController>>>,
    _operation: Option<OwnedRwLockReadGuard<()>>,
    blocked: HashSet<(u64, u64)>,
}

impl Storage {
    pub fn new(files: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            files,
            media: parking_lot::RwLock::new(None),
            reserved: AtomicU64::new(0),
        })
    }

    pub fn bind_media(&self, media: Media) {
        *self.media.write() = Some(media);
    }

    pub fn reserve(self: &Arc<Self>, access: &Access, bytes: u64) -> io::Result<Reservation> {
        if bytes > MAX_FILE_BYTES {
            return Err(invalid());
        }
        let available = access.space()?.saturating_sub(FREE_RESERVE);
        self.reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= available)
            })
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOSPC))?;
        Ok(Reservation {
            storage: self.clone(),
            remaining: bytes,
        })
    }

    pub async fn access(&self, location: &Location) -> io::Result<Access> {
        match location.area {
            Area::Files => {
                std::fs::create_dir_all(&self.files)?;
                Ok(Access {
                    root: directory(&self.files)?,
                    _controller: None,
                    _operation: None,
                    blocked: HashSet::new(),
                })
            }
            Area::Images => {
                let media = self.media.read().clone().ok_or_else(denied)?;
                let controller = media.try_read_owned().map_err(|_| denied())?;
                let msd = controller.as_ref().ok_or_else(denied)?;
                let (operation, mounted) =
                    msd.reserve_image_transfer().await.map_err(|_| denied())?;
                let mut blocked = HashSet::new();
                for path in mounted {
                    let meta = std::fs::metadata(path)?;
                    blocked.insert((meta.dev(), meta.ino()));
                }
                let root = directory(msd.image_transfer_root())?;
                Ok(Access {
                    root,
                    _controller: Some(controller),
                    _operation: Some(operation),
                    blocked,
                })
            }
        }
    }
}

pub struct Reservation {
    storage: Arc<Storage>,
    remaining: u64,
}
impl Reservation {
    pub fn consume(&mut self, bytes: u64) {
        assert!(bytes <= self.remaining);
        self.remaining -= bytes;
        self.storage.reserved.fetch_sub(bytes, Ordering::AcqRel);
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.storage
            .reserved
            .fetch_sub(self.remaining, Ordering::AcqRel);
    }
}

pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
    pub modified: u64,
}

impl Access {
    pub fn entries(&self, location: &Location) -> io::Result<Vec<Entry>> {
        let directory = self.open_directory(&location.parts)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
            if entries.len() >= 1024 {
                return Err(invalid());
            }
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| invalid())?;
            let file = match open_at(&directory, &name, libc::O_RDONLY) {
                Ok(file) => file,
                Err(e) if e.raw_os_error() == Some(libc::ELOOP) => continue,
                Err(e) => return Err(e),
            };
            let meta = file.metadata()?;
            if !meta.is_dir() && (!meta.is_file() || meta.nlink() != 1) {
                continue;
            }
            if location.area == Area::Images && meta.is_file() && !is_image(&name) {
                continue;
            }
            entries.push(Entry {
                name,
                directory: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            });
        }
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }
    fn parent(&self, parts: &[String], create: bool) -> io::Result<(File, String)> {
        let (name, parents) = parts.split_last().ok_or_else(denied)?;
        let mut current = self.root.try_clone()?;
        for part in parents {
            if create {
                let c = CString::new(part.as_str()).map_err(|_| invalid())?;
                let result = unsafe { libc::mkdirat(current.as_raw_fd(), c.as_ptr(), 0o700) };
                if result != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(io::Error::last_os_error());
                }
            }
            current = open_at(&current, part, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        Ok((current, name.clone()))
    }

    pub fn open_file(&self, location: &Location, write: bool) -> io::Result<File> {
        self.open(location, write, false)
    }

    pub fn create_exclusive(&self, location: &Location) -> io::Result<File> {
        self.open(location, true, true)
    }

    fn open(&self, location: &Location, write: bool, exclusive: bool) -> io::Result<File> {
        if location.area == Area::Images && !is_image(location.parts.last().ok_or_else(denied)?) {
            return Err(denied());
        }
        let (parent, name) = self.parent(&location.parts, write)?;
        let flags = if write {
            libc::O_RDWR | libc::O_CREAT | if exclusive { libc::O_EXCL } else { 0 }
        } else {
            libc::O_RDONLY
        };
        let file = open_at(&parent, &name, flags)?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.len() > MAX_FILE_BYTES
            || self.blocked.contains(&(meta.dev(), meta.ino()))
        {
            return Err(denied());
        }
        let mode = if write { libc::LOCK_EX } else { libc::LOCK_SH };
        if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
            return Err(denied());
        }
        Ok(file)
    }

    pub async fn check_unmounted(&self, location: &Location, file: &File) -> io::Result<()> {
        if location.area != Area::Images {
            return Ok(());
        }
        let path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        let mut command = tokio::process::Command::new("sudo");
        command
            .args(["-n", "/usr/libexec/vim4-onekvm", "uu-image-check"])
            .arg(path)
            .kill_on_drop(true);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
            .await
            .map_err(|_| denied())??;
        if result.status.success() {
            Ok(())
        } else {
            Err(denied())
        }
    }

    pub fn open_directory(&self, parts: &[String]) -> io::Result<File> {
        let mut current = self.root.try_clone()?;
        for part in parts {
            current = open_at(&current, part, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        Ok(current)
    }

    pub fn mkdir(&self, parts: &[String]) -> io::Result<()> {
        let (parent, name) = self.parent(parts, true)?;
        let c = CString::new(name).map_err(|_| invalid())?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) } == 0 {
            Ok(())
        } else if io::Error::last_os_error().kind() == io::ErrorKind::AlreadyExists {
            self.open_directory(parts).map(|_| ())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn space(&self) -> io::Result<u64> {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(self.root.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let stats = unsafe { stats.assume_init() };
        Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
    }
}

fn is_image(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|x| x.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("img") || ext.eq_ignore_ascii_case("iso"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn traversal_and_symlink_components_cannot_escape_the_share() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("share");
        let storage = Storage::new(root.clone());
        let location = Location::parse("C:\\files\\safe.bin").unwrap().unwrap();
        let access = storage.access(&location).await.unwrap();
        for path in [
            "/files/../credentials.json",
            "/files/a/../../x",
            "/etc/passwd",
            "/images/../../etc",
            "/files/a//b",
            "/files/a\0b",
        ] {
            assert!(Location::parse(path).is_err(), "accepted {path:?}");
        }
        std::os::unix::fs::symlink(temp.path(), root.join("outside")).unwrap();
        let escaped = Location::parse("/files/outside/secret").unwrap().unwrap();
        assert!(access.open_file(&escaped, true).is_err());
        std::os::unix::fs::symlink(temp.path().join("secret"), root.join("linked")).unwrap();
        let linked = Location::parse("/files/linked").unwrap().unwrap();
        assert!(access.open_file(&linked, true).is_err());
        assert!(!temp.path().join("secret").exists());
        let nested = Location::parse("/files/new/folder/test.bin")
            .unwrap()
            .unwrap();
        access.open_file(&nested, true).unwrap();
        assert!(root.join("new/folder/test.bin").is_file());
        assert!(access.space().unwrap() > FREE_RESERVE);
    }

    #[tokio::test]
    async fn reservations_refuse_overcommit_and_are_released_when_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::new(temp.path().join("files"));
        let location = Location::parse("/files").unwrap().unwrap();
        let access = storage.access(&location).await.unwrap();
        let first = storage.reserve(&access, 1024).unwrap();
        assert_eq!(storage.reserved.load(Ordering::Acquire), 1024);
        assert!(storage.reserve(&access, MAX_FILE_BYTES + 1).is_err());
        let available = access.space().unwrap().saturating_sub(FREE_RESERVE);
        storage.reserved.store(available, Ordering::Release);
        assert_eq!(
            storage.reserve(&access, 1).err().unwrap().raw_os_error(),
            Some(libc::ENOSPC)
        );
        storage.reserved.store(1024, Ordering::Release);
        drop(first);
        assert_eq!(storage.reserved.load(Ordering::Acquire), 0);
        storage.reserve(&access, 1024).unwrap();
    }

    #[test]
    fn mounted_images_and_hardlinks_are_rejected_before_the_caller_can_write() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("mounted.img");
        std::fs::write(&image, vec![7; 512]).unwrap();
        let meta = image.metadata().unwrap();
        let access = Access {
            root: directory(temp.path()).unwrap(),
            _controller: None,
            _operation: None,
            blocked: HashSet::from([(meta.dev(), meta.ino())]),
        };
        let location = Location::parse("/images/mounted.img").unwrap().unwrap();
        assert!(access.open_file(&location, true).is_err());
        std::fs::hard_link(&image, temp.path().join("hard.img")).unwrap();
        let hard = Location::parse("/images/hard.img").unwrap().unwrap();
        assert!(access.open_file(&hard, true).is_err());
        assert_eq!(std::fs::read(image).unwrap(), vec![7; 512]);
    }
}
