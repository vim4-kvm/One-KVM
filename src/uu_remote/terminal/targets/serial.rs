use crate::config::TERMINAL_BAUDS;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use typeshare::typeshare;

#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct SerialDevice {
    pub id: String,
    pub label: String,
    pub present: bool,
}

pub fn device_label(path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let short = std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| name.to_string());
    if path.parent() == Some(Path::new("/dev/serial/by-id")) {
        let model = name
            .strip_prefix("usb-")
            .unwrap_or(&name)
            .split("-if")
            .next()
            .unwrap_or(&name);
        // udev's by-id suffix contains the device serial, not the model.
        let model = model
            .rsplit_once('_')
            .map_or(model, |(m, _)| m)
            .replace('_', " ");
        format!("{model} · {short}")
    } else {
        short
    }
}
pub fn devices(configured: &str) -> Vec<SerialDevice> {
    let mut devices = vec![SerialDevice {
        id: "usb-gadget".into(),
        label: "USB 串口（经 USB-C 线）".into(),
        present: Path::new("/dev/ttyGS0").exists(),
    }];
    devices.retain(|device| device.present || device.id == configured);
    let mut covered = std::collections::HashSet::new();
    let mut by_id: Vec<_> = std::fs::read_dir("/dev/serial/by-id")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    by_id.sort();
    for path in by_id {
        if let Ok(real) = std::fs::canonicalize(&path) {
            covered.insert(real);
            devices.push(SerialDevice {
                id: path.to_string_lossy().into_owned(),
                label: device_label(&path),
                present: true,
            });
        }
    }
    let mut extra: Vec<_> = std::fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            (name.starts_with("ttyUSB") || name.starts_with("ttyACM")) && !covered.contains(p)
        })
        .collect();
    extra.sort();
    for path in extra {
        devices.push(SerialDevice {
            id: path.to_string_lossy().into_owned(),
            label: device_label(&path),
            present: true,
        });
    }
    if !configured.is_empty() && !devices.iter().any(|d| d.id == configured) {
        devices.push(SerialDevice {
            id: configured.into(),
            label: device_label(Path::new(configured)),
            present: Path::new(configured).exists(),
        });
    }
    devices
}

#[derive(Default)]
pub enum Menu {
    #[default]
    Normal,
    Command,
    Baud,
}
pub enum MenuAction {
    Send(u8),
    Print(String),
    Break,
    Baud(u32),
    Disconnect,
    None,
}
impl Menu {
    pub fn key(&mut self, key: u8, baud: u32) -> MenuAction {
        match std::mem::take(self) {
            Self::Normal if key == 0x1d => {
                *self = Self::Command;
                MenuAction::Print(format!("\r\n\x1b[2;7m串口菜单  b 发送 Break · r 波特率 {baud} · d 断开 · ] 发送 Ctrl-] · 其他键返回\x1b[0m\r\n"))
            }
            Self::Normal => MenuAction::Send(key),
            Self::Command => match key {
                b'b' => MenuAction::Break,
                b'd' => MenuAction::Disconnect,
                b']' => MenuAction::Send(0x1d),
                b'r' => {
                    *self = Self::Baud;
                    MenuAction::Print("\r\n1 9600  2 19200  3 38400  4 57600  5 115200  6 230400  7 460800  8 921600  9 1500000\r\n".into())
                }
                _ => MenuAction::None,
            },
            Self::Baud => {
                if (b'1'..=b'9').contains(&key) {
                    MenuAction::Baud(TERMINAL_BAUDS[(key - b'1') as usize])
                } else {
                    MenuAction::None
                }
            }
        }
    }
}

pub struct Serial {
    pub path: PathBuf,
    pub baud: u32,
    pub dropped: u64,
    file: Option<File>,
    pub menu: Menu,
}
impl Serial {
    pub fn open(path: &Path, baud: u32) -> io::Result<Self> {
        Ok(Self {
            path: path.into(),
            baud,
            dropped: 0,
            file: Some(open_device(path, baud)?),
            menu: Menu::default(),
        })
    }
    pub fn connected(&self) -> bool {
        self.file.is_some()
    }
    pub fn fd(&self) -> Option<i32> {
        use std::os::fd::AsRawFd;
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }
    pub fn reconnect(&mut self) -> bool {
        if self.file.is_none() {
            self.file = open_device(&self.path, self.baud).ok();
        }
        self.connected()
    }
    pub fn check_presence(&mut self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if self.file.as_ref().is_some_and(|f| {
                match (f.metadata(), std::fs::metadata(&self.path)) {
                    (Ok(a), Ok(b)) => a.rdev() != b.rdev() || a.ino() != b.ino(),
                    _ => true,
                }
            }) {
                self.disconnect();
            }
        }
        self.connected()
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.file.as_mut().map_or(Ok(0), |f| f.read(bytes));
        if matches!(result, Ok(0)) {
            if let Some(fd) = self.fd() {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut poll, 1, 0) } > 0
                    && poll.revents & (libc::POLLHUP | libc::POLLERR) != 0
                {
                    self.disconnect();
                }
            }
        }
        // Raw tty reads may return zero with VMIN=0; POLLHUP/EIO or path loss signals unplug.
        if result.as_ref().is_err_and(|e| {
            !matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            )
        }) {
            self.disconnect();
        }
        result
    }
    pub fn write(&mut self, bytes: &[u8]) {
        let written = self
            .file
            .as_mut()
            .and_then(|f| f.write(bytes).ok())
            .unwrap_or(0);
        self.dropped += (bytes.len() - written) as u64;
    }
    pub fn set_baud(&mut self, baud: u32) -> io::Result<()> {
        if let Some(file) = &self.file {
            configure(file, baud)?;
        }
        self.baud = baud;
        Ok(())
    }
    pub fn send_break(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if let Some(file) = &self.file {
                if unsafe { libc::tcsendbreak(file.as_raw_fd(), 0) } == 0 {
                    return Ok(());
                }
            }
        }
        Err(io::Error::last_os_error())
    }
    fn disconnect(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if let Some(file) = self.file.take() {
                unsafe {
                    libc::ioctl(file.as_raw_fd(), libc::TIOCNXCL as _, 0);
                }
            }
        }
        #[cfg(not(unix))]
        {
            self.file = None;
        }
    }
}
impl Drop for Serial {
    fn drop(&mut self) {
        self.disconnect();
    }
}
#[cfg(target_os = "linux")]
fn open_device(path: &Path, baud: u32) -> io::Result<File> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if unsafe { libc::ioctl(file.as_raw_fd(), libc::TIOCEXCL as _, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if let Err(e) = configure(&file, baud) {
        unsafe {
            libc::ioctl(file.as_raw_fd(), libc::TIOCNXCL as _, 0);
        }
        return Err(e);
    }
    Ok(file)
}
#[cfg(target_os = "linux")]
fn configure(file: &File, baud: u32) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let speed = match baud {
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        230400 => libc::B230400,
        460800 => libc::B460800,
        921600 => libc::B921600,
        1500000 => libc::B1500000,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "baud")),
    };
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(file.as_raw_fd(), &mut term) < 0 {
            return Err(io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut term);
        term.c_cflag &= !(libc::CSIZE | libc::CSTOPB | libc::PARENB | libc::CRTSCTS);
        term.c_cflag |= libc::CS8 | libc::CLOCAL | libc::CREAD;
        term.c_cc[libc::VMIN] = 1;
        term.c_cc[libc::VTIME] = 0;
        libc::cfsetispeed(&mut term, speed);
        libc::cfsetospeed(&mut term, speed);
        if libc::tcsetattr(file.as_raw_fd(), libc::TCSANOW, &term) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn open_device(_: &Path, _: u32) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "serial requires Linux",
    ))
}
#[cfg(not(target_os = "linux"))]
fn configure(_: &File, _: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "serial requires Linux",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    #[test]
    fn hangup_disconnects_instead_of_spinning_on_readable_eof() {
        let pair = nix::pty::openpty(None, None).unwrap();
        let path = std::fs::read_link(format!("/proc/self/fd/{}", pair.slave.as_raw_fd())).unwrap();
        let mut serial = Serial::open(&path, 115200).unwrap();
        drop(pair.master);
        let _ = serial.read(&mut [0; 64]);
        assert!(!serial.connected());
        assert!(serial.fd().is_none());
    }
    #[test]
    fn raw_menu_and_disconnect_recover_same_baud() {
        let pty = nix::pty::openpty(None, None).unwrap();
        let path = std::fs::read_link(format!("/proc/self/fd/{}", pty.slave.as_raw_fd())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("device");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let mut serial = Serial::open(&link, 115200).unwrap();
        let mut master = File::from(pty.master);
        serial.write(b"hello");
        let mut buf = [0u8; 5];
        master.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        assert!(matches!(
            serial.menu.key(0x1d, serial.baud),
            MenuAction::Print(_)
        ));
        assert!(matches!(
            serial.menu.key(b']', serial.baud),
            MenuAction::Send(0x1d)
        ));
        serial.menu.key(0x1d, serial.baud);
        serial.menu.key(b'r', serial.baud);
        let MenuAction::Baud(baud) = serial.menu.key(b'8', serial.baud) else {
            panic!()
        };
        serial.set_baud(baud).unwrap();
        std::fs::remove_file(&link).unwrap();
        assert!(!serial.check_presence());
        assert!(!serial.reconnect());
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(serial.reconnect());
        assert_eq!(serial.baud, 921600);
        let mut term = unsafe { std::mem::zeroed() };
        unsafe {
            libc::tcgetattr(serial.file.as_ref().unwrap().as_raw_fd(), &mut term);
        }
        assert_eq!(term.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG), 0);
        serial.menu.key(0x1d, serial.baud);
        assert!(matches!(
            serial.menu.key(b'd', serial.baud),
            MenuAction::Disconnect
        ));
    }
}
