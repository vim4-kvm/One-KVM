//! PTY ownership stays on a session worker, including process reaping and HUP→KILL.
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Pty {
    master: File,
    child: Child,
    size: (u16, u16),
    redraw_at: Option<Instant>,
    reaped: bool,
}
impl Pty {
    pub fn spawn(mut command: Command, size: (u16, u16)) -> io::Result<Self> {
        let win = libc::winsize {
            ws_col: size.0,
            ws_row: size.1,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pair = nix::pty::openpty(Some(&win), None)?;
        command
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env(
                "LANG",
                std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()),
            )
            .current_dir(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
            .stdin(Stdio::from(pair.slave.try_clone()?))
            .stdout(Stdio::from(pair.slave.try_clone()?))
            .stderr(Stdio::from(pair.slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let fd = pair.master.as_raw_fd();
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let child = command.spawn()?;
        Ok(Self {
            master: File::from(pair.master),
            child,
            size,
            redraw_at: None,
            reaped: false,
        })
    }
    pub fn board(size: (u16, u16)) -> io::Result<Self> {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| s.starts_with('/') && std::path::Path::new(s).is_file())
            .unwrap_or_else(|| "/bin/bash".into());
        let mut cmd = Command::new(shell);
        cmd.arg("-l");
        Self::spawn(cmd, size)
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.master.read(bytes)
    }
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.master.write(bytes)
    }
    pub fn signal(&self, signal: i32) {
        if !self.reaped {
            unsafe {
                libc::kill(-(self.child.id() as i32), signal);
            }
        }
    }
    fn winsize(&self, size: (u16, u16)) {
        let win = libc::winsize {
            ws_col: size.0,
            ws_row: size.1,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &win);
        }
    }
    pub fn resize(&mut self, size: (u16, u16)) {
        self.size = size;
        self.winsize(size);
    }
    pub fn redraw(&mut self) {
        if self.size.1 > 1 {
            self.winsize((self.size.0, self.size.1 - 1));
            self.redraw_at = Some(Instant::now() + Duration::from_millis(150));
        }
    }
    pub fn tick(&mut self) -> Option<i32> {
        if self.redraw_at.is_some_and(|t| Instant::now() >= t) {
            self.redraw_at = None;
            self.winsize(self.size);
        }
        // Observe without reaping: retaining the leader's PID until Drop signals
        // the process group also covers background children after the shell exits.
        unsafe {
            let mut status: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut status,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) == 0
                && status.si_pid() != 0
            {
                Some(if status.si_code == libc::CLD_EXITED {
                    status.si_status()
                } else {
                    -1
                })
            } else {
                None
            }
        }
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        if !self.reaped {
            // Interactive bash puts foreground jobs in separate process groups.
            // Pin descendants with pidfds before HUP reparents them; retaining
            // these handles prevents signalling a reused PID during escalation.
            let descendants = descendant_handles(self.child.id());
            signal_handles(&descendants, libc::SIGHUP);
            self.signal(libc::SIGHUP);
            // Keep the leader unreaped until after signalling the group, avoiding PID reuse.
            std::thread::sleep(Duration::from_millis(300));
            signal_handles(&descendant_handles(self.child.id()), libc::SIGKILL);
            signal_handles(&descendants, libc::SIGKILL);
            self.signal(libc::SIGKILL);
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

fn descendant_handles(leader: u32) -> Vec<OwnedFd> {
    let processes: Vec<_> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
            Some((
                pid,
                fields.get(1)?.parse::<u32>().ok()?,
                fields.get(3)?.parse::<u32>().ok()?,
                fields.get(19)?.parse::<u64>().ok()?,
            ))
        })
        .collect();
    let mut owned = std::collections::HashSet::from([leader]);
    loop {
        let before = owned.len();
        for &(pid, parent, session, _) in &processes {
            if session == leader || owned.contains(&parent) {
                owned.insert(pid);
            }
        }
        if before == owned.len() {
            break;
        }
    }
    owned
        .into_iter()
        .filter_map(|pid| {
            let start = processes.iter().find(|p| p.0 == pid)?.3;
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
            if fd < 0 {
                return None;
            }
            let handle = unsafe { OwnedFd::from_raw_fd(fd) };
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let current = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(19)?
                .parse::<u64>()
                .ok()?;
            (start == current).then_some(handle)
        })
        .collect()
}
fn signal_handles(handles: &[OwnedFd], signal: i32) {
    for handle in handles {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                handle.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait_for_output(pty: &mut Pty, needle: &[u8]) {
        let mut output = Vec::new();
        let mut buffer = [0; 1024];
        let deadline = Instant::now() + Duration::from_secs(5);
        while !output.windows(needle.len()).any(|w| w == needle) {
            assert!(Instant::now() < deadline, "PTY output timed out");
            let mut poll = libc::pollfd {
                fd: pty.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut poll, 1, 100);
            }
            if let Ok(n) = pty.read(&mut buffer) {
                output.extend_from_slice(&buffer[..n]);
            }
        }
    }
    #[test]
    fn exit_status_and_hup_resistant_child_are_reaped() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "printf ready; exit 7"]);
        let mut pty = Pty::spawn(cmd, (80, 24)).unwrap();
        let pid = pty.child.id();
        wait_for_output(&mut pty, b"ready");
        unsafe {
            let mut status = std::mem::zeroed();
            assert_eq!(
                libc::waitid(libc::P_PID, pid, &mut status, libc::WEXITED | libc::WNOWAIT),
                0
            );
        }
        assert_eq!(pty.tick(), Some(7));
        drop(pty);
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "trap '' HUP; printf ready; exec sleep 30"]);
        let mut pty = Pty::spawn(cmd, (80, 24)).unwrap();
        let pid = pty.child.id();
        wait_for_output(&mut pty, b"ready");
        drop(pty);
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }
    #[test]
    fn redraw_restores_latest_size_under_controlled_clock() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exec sleep 30"]);
        let mut pty = Pty::spawn(cmd, (80, 24)).unwrap();
        let size = |pty: &Pty| {
            let mut win: libc::winsize = unsafe { std::mem::zeroed() };
            unsafe {
                libc::ioctl(pty.master.as_raw_fd(), libc::TIOCGWINSZ as _, &mut win);
            }
            (win.ws_col, win.ws_row)
        };
        pty.redraw();
        assert_eq!(size(&pty), (80, 23));
        pty.resize((100, 40));
        pty.redraw_at = Some(Instant::now());
        pty.tick();
        assert_eq!(size(&pty), (100, 40));
    }

    #[test]
    fn close_kills_hup_resistant_job_in_another_session() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "trap '' HUP; setsid /bin/sh -c 'trap \"\" HUP; printf child-ready; exec sleep 30' & wait"]);
        let mut pty = Pty::spawn(cmd, (80, 24)).unwrap();
        wait_for_output(&mut pty, b"child-ready");
        let handles = descendant_handles(pty.child.id());
        assert!(handles.len() >= 2);
        drop(pty);
        for handle in handles {
            let mut poll = libc::pollfd {
                fd: handle.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(
                unsafe { libc::poll(&mut poll, 1, 3000) },
                1,
                "terminal child survived closure"
            );
        }
    }
}
