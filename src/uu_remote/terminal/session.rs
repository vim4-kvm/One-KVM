use super::{
    frame::*,
    picker::{self, Action, Picker},
    targets, unique_name, Outbound, Registry, Target, TerminalConn, TerminalSessionStatus,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc, Weak,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) enum Command {
    Input(Vec<u8>),
    Resize((u16, u16)),
    Redraw,
    Signal(i32),
}
pub(super) struct Metadata {
    pub status: TerminalSessionStatus,
    pub renamed: bool,
    pub serial_path: Option<PathBuf>,
}
struct Output {
    sink: Option<(u64, Arc<Outbound>, tokio_util::sync::CancellationToken)>,
    scrollback: VecDeque<u8>,
    pending: VecDeque<u8>,
    skipped: u64,
}
const INPUT_LIMIT: usize = 1024 * 1024;

struct WakePipe {
    reader: File,
    writer: File,
}
impl WakePipe {
    fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        let (reader, writer) =
            nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC | nix::fcntl::OFlag::O_NONBLOCK)?;
        #[cfg(not(target_os = "linux"))]
        let (reader, writer) = {
            use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
            let pair = nix::unistd::pipe()?;
            for fd in [&pair.0, &pair.1] {
                fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
                fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
            }
            pair
        };
        Ok(Self {
            reader: reader.into(),
            writer: writer.into(),
        })
    }
    fn notify(&self) {
        // A full pipe already represents a pending wakeup.
        while matches!((&self.writer).write(&[1]), Err(error) if error.kind() == io::ErrorKind::Interrupted)
        {
        }
    }
    fn drain(&self) {
        let mut buffer = [0; 256];
        loop {
            match (&self.reader).read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    }
}
pub(super) struct Session {
    pub meta: Mutex<Metadata>,
    id: u32,
    output: Mutex<Output>,
    size: Mutex<(u16, u16)>,
    tx: SyncSender<Command>,
    resize: Mutex<Option<(u16, u16)>>,
    wake_pipe: WakePipe,
    dropped_input: AtomicU64,
    dropped_commands: AtomicU64,
    dropped_output: AtomicU64,
    stop: AtomicBool,
    reason: Mutex<Option<&'static str>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Session {
    pub fn new(
        id: u32,
        name: String,
        size: (u16, u16),
    ) -> io::Result<(Arc<Self>, Receiver<Command>)> {
        let (tx, rx) = mpsc::sync_channel(64);
        Ok((
            Arc::new(Self {
                meta: Mutex::new(Metadata {
                    status: TerminalSessionStatus {
                        id,
                        name,
                        target: Target::Picker,
                        detail: "选择终端".into(),
                        attached: false,
                        created_at_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    },
                    renamed: false,
                    serial_path: None,
                }),
                id,
                output: Mutex::new(Output {
                    sink: None,
                    scrollback: VecDeque::new(),
                    pending: VecDeque::new(),
                    skipped: 0,
                }),
                size: Mutex::new(size),
                tx,
                resize: Mutex::new(None),
                wake_pipe: WakePipe::new()?,
                dropped_input: AtomicU64::new(0),
                dropped_commands: AtomicU64::new(0),
                dropped_output: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                reason: Mutex::new(None),
                worker: Mutex::new(None),
            }),
            rx,
        ))
    }
    pub fn spawn(
        self: &Arc<Self>,
        registry: Weak<Registry>,
        rx: Receiver<Command>,
    ) -> std::io::Result<()> {
        let session = self.clone();
        *self.worker.lock() = Some(
            std::thread::Builder::new()
                .name(format!("uu-term-{}", self.id))
                .spawn(move || session.run(registry, rx))?,
        );
        Ok(())
    }
    pub fn command(&self, cmd: Command) {
        if let Command::Resize(size) = cmd {
            *self.resize.lock() = Some(size);
        } else if let Err(mpsc::TrySendError::Full(cmd) | mpsc::TrySendError::Disconnected(cmd)) =
            self.tx.try_send(cmd)
        {
            match cmd {
                Command::Input(bytes) => {
                    self.dropped_input
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                }
                _ => {
                    self.dropped_commands.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.wake();
    }
    pub fn wake(&self) {
        self.wake_pipe.notify();
    }
    fn queue_input(&self, pending: &mut VecDeque<u8>, bytes: &[u8]) {
        let accepted = bytes.len().min(INPUT_LIMIT.saturating_sub(pending.len()));
        pending.extend(&bytes[..accepted]);
        self.dropped_input
            .fetch_add((bytes.len() - accepted) as u64, Ordering::Relaxed);
    }
    pub fn stop(&self, reason: Option<&'static str>) {
        if let Some(reason) = reason {
            *self.reason.lock() = Some(reason);
        }
        self.stop.store(true, Ordering::Release);
        self.wake();
    }
    pub fn join(&self) {
        let worker = self.worker.lock().take();
        if let Some(worker) = worker {
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
    pub fn status(&self) -> TerminalSessionStatus {
        let mut status = self.meta.lock().status.clone();
        status.attached = self
            .output
            .lock()
            .sink
            .as_ref()
            .is_some_and(|(_, out, cancelled)| {
                !out.tx.lock().is_closed() && !cancelled.is_cancelled()
            });
        status
    }
    pub fn attached_to(&self, id: u64) -> bool {
        self.output
            .lock()
            .sink
            .as_ref()
            .is_some_and(|(owner, _, cancelled)| *owner == id && !cancelled.is_cancelled())
    }
    pub fn detach(&self, id: u64) {
        let mut out = self.output.lock();
        if out.sink.as_ref().is_some_and(|(owner, _, _)| *owner == id) {
            out.sink = None;
            out.pending.clear();
            out.skipped = 0;
            self.wake();
        }
    }
    pub fn attach(&self, connection: &TerminalConn, size: (u16, u16)) {
        let mut out = self.output.lock();
        connection.send(OPEN_RESP, self.id, &reply(0, "ok"));
        out.sink = Some((
            connection.id,
            connection.out.clone(),
            connection.cancelled.clone(),
        ));
        out.pending = out.scrollback.clone();
        out.skipped = 0;
        self.flush(&mut out);
        *self.size.lock() = size;
        self.command(Command::Resize(size));
        self.command(Command::Redraw);
    }
    pub(super) fn output(&self, data: &[u8]) {
        let mut out = self.output.lock();
        Self::remember(&mut out, data);
        if out.sink.is_some() {
            out.pending.extend(data);
            // Reattachment can enqueue a full history between the worker's
            // capacity check and read. Keep room for that one in-flight chunk.
            // Repeated picker redraws and local notices also stay bounded.
            let excess = out.pending.len().saturating_sub(SCROLLBACK + OUTPUT_CHUNK);
            out.pending.drain(..excess);
            self.flush(&mut out);
        }
    }
    fn remember(out: &mut Output, data: &[u8]) {
        out.scrollback.extend(data);
        let excess = out.scrollback.len().saturating_sub(SCROLLBACK);
        out.scrollback.drain(..excess);
    }
    fn flush(&self, out: &mut Output) {
        let Some((_, sink, cancelled)) = &out.sink else {
            return;
        };
        if cancelled.is_cancelled() || sink.tx.lock().is_closed() {
            out.sink = None;
            out.pending.clear();
            out.skipped = 0;
            return;
        }
        while !out.pending.is_empty() {
            let count = out.pending.len().min(OUTPUT_CHUNK);
            if !sink.data(self.id, &out.pending.make_contiguous()[..count]) {
                return;
            }
            out.pending.drain(..count);
        }
        if out.skipped > 0 {
            let notice = format!(
                "\r\n\x1b[2m[已跳过 {} KB 串口输出]\x1b[0m\r\n",
                out.skipped.div_ceil(1024)
            );
            if sink.data(self.id, notice.as_bytes()) {
                out.skipped = 0;
            }
        }
    }
    fn output_ready(&self) -> bool {
        let mut out = self.output.lock();
        self.flush(&mut out);
        out.pending.is_empty() && out.sink.as_ref().is_none_or(|(_, sink, _)| sink.ready())
    }
    fn serial_output(&self, data: &[u8]) {
        let mut out = self.output.lock();
        Self::remember(&mut out, data);
        self.flush(&mut out);
        for chunk in data.chunks(OUTPUT_CHUNK) {
            if let Some((_, sink, _)) = &out.sink {
                if !out.pending.is_empty() || out.skipped > 0 || !sink.data(self.id, chunk) {
                    out.skipped += chunk.len() as u64;
                    self.dropped_output
                        .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
            }
        }
    }
    fn finish(&self, code: i32) {
        let payload = serde_json::to_vec(&serde_json::json!({"exit_code":code})).unwrap();
        let mut out = self.output.lock();
        if let Some((_, sink, _)) = out.sink.take() {
            sink.control(EXIT, self.id, &payload);
        }
    }
    fn draw(&self, registry: &Registry, picker: &Picker, items: &[picker::Item]) {
        let name = registry.state.lock().name.clone();
        let cols = self.size.lock().0;
        self.output(picker.render(&name, cols, items).as_bytes());
    }
    fn open_target(
        &self,
        registry: &Registry,
        target: Target,
        takeover: Option<u32>,
    ) -> Result<Running, String> {
        if let Some(id) = takeover {
            let state = registry.state.lock();
            if self.stop.load(Ordering::Acquire)
                || !state.accepting
                || !state.config.serial.enabled
                || super::busy(&state, state.config.serial.path(), self.id)
                    .is_none_or(|(owner, _)| owner != id)
            {
                return Err("串口占用状态已变化，请重新选择".into());
            }
            drop(state);
            registry.kill_blocking(id);
        }
        let mut state = registry.state.lock();
        if self.stop.load(Ordering::Acquire) || !state.accepting || !target.enabled(&state.config) {
            return Err("此终端类型已关闭".into());
        }
        let config = state.config.clone();
        let size = *self.size.lock();
        let (running, detail, banner, path) = match target {
            Target::Serial => {
                if super::busy(&state, config.serial.path(), self.id).is_some() {
                    return Err("串口正在另一会话中使用，请重新选择接管".into());
                }
                let serial = targets::serial::Serial::open(
                    std::path::Path::new(config.serial.path()),
                    config.serial.baud,
                )
                .map_err(|_| "无法打开串口：设备忙、已移除或没有访问权限")?;
                let short = std::path::Path::new(config.serial.path())
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let label = if config.serial.device == "usb-gadget" {
                    "USB 串口".into()
                } else {
                    targets::serial::device_label(std::path::Path::new(config.serial.path()))
                };
                (
                    Running::Serial(serial),
                    format!("{short} · {}", config.serial.baud),
                    format!(
                        "被控机串口 · {label} · {} 8N1 · Ctrl-] 菜单 · 按回车显示登录提示",
                        config.serial.baud
                    ),
                    Some(PathBuf::from(config.serial.path())),
                )
            }
            #[cfg(target_os = "linux")]
            Target::Board => {
                let detail = format!("{}@{}", super::user(), state.name);
                (
                    Running::Pty(targets::pty::Pty::board(size).map_err(|_| "无法启动板子 shell")?),
                    detail.clone(),
                    format!("KVM 板子 · {detail}"),
                    None,
                )
            }
            #[cfg(target_os = "linux")]
            Target::Ssh => {
                let mut cmd = std::process::Command::new("ssh");
                cmd.args(registry.ssh.args(&config.ssh, true)?);
                let detail = format!("{}@{}", config.ssh.user, config.ssh.host);
                (
                    Running::Pty(
                        targets::pty::Pty::spawn(cmd, size).map_err(|_| "无法启动 SSH 客户端")?,
                    ),
                    detail.clone(),
                    format!("被控机 SSH · {detail}:{}", config.ssh.port),
                    None,
                )
            }
            _ => return Err("此平台不支持此终端".into()),
        };
        let name = unique_name(&state, target.name(), self.id);
        let mut meta = self.meta.lock();
        meta.status.target = target;
        meta.status.detail = detail;
        meta.serial_path = path;
        if !meta.renamed {
            meta.status.name = name;
        }
        state.last = Some(target);
        drop(meta);
        drop(state);
        tracing::info!(
            session_id = self.id,
            ?target,
            cols = size.0,
            rows = size.1,
            "UU terminal target opened"
        );
        self.output(
            format!(
                "{}\x1b[2m{}\x1b[0m\r\n",
                picker::CLEAR,
                picker::truncate(&banner, usize::from(size.0))
            )
            .as_bytes(),
        );
        Ok(running)
    }
    fn run(self: Arc<Self>, registry: Weak<Registry>, rx: Receiver<Command>) {
        let Some(reg) = registry.upgrade() else {
            return;
        };
        let mut items = reg.items(self.id);
        let mut picker = Picker::new(&items, reg.state.lock().last);
        let mut running = None;
        let mut footer_until = None;
        let mut last_check = Instant::now();
        let mut disconnected = false;
        let mut pending = VecDeque::new();
        let mut code = 0;
        if let Some(target) = picker::direct_target(&items) {
            match self.open_target(&reg, target, None) {
                Ok(target) => running = Some(target),
                Err(error) => {
                    picker.footer = Some((error, "31"));
                }
            }
        }
        if running.is_none() {
            self.draw(&reg, &picker, &items);
        }
        drop(reg);
        let mut buffer = vec![0; OUTPUT_CHUNK];
        while !self.stop.load(Ordering::Acquire) {
            let Some(reg) = registry.upgrade() else {
                break;
            };
            self.wake_pipe.drain();
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            let resize = self.resize.lock().take().map(Command::Resize);
            for command in resize.into_iter().chain(rx.try_iter().take(64)) {
                match command {
                    Command::Input(bytes) => {
                        if let Some(target) = running.as_mut() {
                            match target {
                                Running::Serial(serial) => self.serial_input(serial, &bytes),
                                #[cfg(target_os = "linux")]
                                Running::Pty(_) => {
                                    self.queue_input(&mut pending, &bytes);
                                }
                            }
                        } else {
                            for byte in bytes {
                                match picker.key(byte, &items) {
                                    Action::Quit => {
                                        self.stop(None);
                                        break;
                                    }
                                    Action::Redraw => {
                                        if picker.confirm.is_none() && picker.footer.is_some() {
                                            footer_until =
                                                Some(Instant::now() + Duration::from_secs(2));
                                        }
                                        self.draw(&reg, &picker, &items);
                                    }
                                    Action::Open(target, takeover) => {
                                        match self.open_target(&reg, target, takeover) {
                                            Ok(target) => running = Some(target),
                                            Err(error) => {
                                                picker.footer = Some((error, "31"));
                                                self.draw(&reg, &picker, &items);
                                            }
                                        }
                                        break;
                                    }
                                    Action::None => {}
                                }
                            }
                        }
                    }
                    Command::Resize(size) => {
                        *self.size.lock() = size;
                        #[cfg(target_os = "linux")]
                        if let Some(Running::Pty(pty)) = running.as_mut() {
                            pty.resize(size);
                        }
                        if running.is_none() {
                            self.draw(&reg, &picker, &items);
                        }
                    }
                    Command::Redraw => {
                        #[cfg(target_os = "linux")]
                        if let Some(Running::Pty(pty)) = running.as_mut() {
                            pty.redraw();
                        }
                        if running.is_none() {
                            self.draw(&reg, &picker, &items);
                        }
                    }
                    Command::Signal(signal) =>
                    {
                        #[cfg(target_os = "linux")]
                        if let Some(Running::Pty(pty)) = &running {
                            pty.signal(signal);
                        }
                    }
                }
            }
            self.output_ready();
            match running.as_mut() {
                Some(Running::Serial(serial)) => {
                    for _ in 0..16 {
                        match serial.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => self.serial_output(&buffer[..n]),
                        }
                    }
                    if !serial.connected() && !disconnected {
                        self.output(
                            "\r\n\x1b[33m串口设备已断开，重新连接后自动恢复…\x1b[0m\r\n".as_bytes(),
                        );
                        disconnected = true;
                    }
                }
                #[cfg(target_os = "linux")]
                Some(Running::Pty(pty)) => {
                    if !pending.is_empty() {
                        match pty.write(pending.make_contiguous()) {
                            Ok(n) => {
                                pending.drain(..n);
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(_) => {
                                self.stop(None);
                            }
                        }
                    }
                    let mut drained = false;
                    for _ in 0..16 {
                        if !self.output_ready() {
                            break;
                        }
                        match pty.read(&mut buffer) {
                            Ok(0) | Err(_) => {
                                drained = true;
                                break;
                            }
                            Ok(n) => self.output(&buffer[..n]),
                        }
                    }
                    if let Some(exit) = pty.tick().filter(|_| drained) {
                        code = exit;
                        break;
                    }
                }
                _ => {}
            }
            if last_check.elapsed() >= Duration::from_secs(1) {
                last_check = Instant::now();
                if let Some(Running::Serial(serial)) = running.as_mut() {
                    if !serial.check_presence() && !disconnected {
                        self.output(
                            "\r\n\x1b[33m串口设备已断开，重新连接后自动恢复…\x1b[0m\r\n".as_bytes(),
                        );
                        disconnected = true;
                    }
                    if disconnected && serial.reconnect() {
                        self.output("\r\n串口已重新连接\r\n".as_bytes());
                        disconnected = false;
                    }
                } else if running.is_none() {
                    let next = reg.items(self.id);
                    if next != items {
                        items = next;
                        picker.refresh(&items);
                        self.draw(&reg, &picker, &items);
                    }
                }
            }
            if running.is_none() && footer_until.is_some_and(|t| Instant::now() >= t) {
                footer_until = None;
                if picker.confirm.is_none() {
                    picker.footer = None;
                    self.draw(&reg, &picker, &items);
                }
            }
            // Do not retain the registry across poll: dropping the service must
            // still be able to stop and wake this worker.
            drop(reg);
            self.wait_for_io(
                running.as_ref(),
                !pending.is_empty(),
                last_check,
                footer_until,
            );
        }
        if let Some(reason) = *self.reason.lock() {
            self.output(reason.as_bytes());
        }
        if let Some(Running::Serial(serial)) = &running {
            tracing::info!(
                session_id = self.id,
                dropped_bytes = serial.dropped,
                "UU serial closed"
            );
        }
        drop(running);
        self.finish(code);
        if let Some(reg) = registry.upgrade() {
            let mut state = reg.state.lock();
            if state
                .sessions
                .get(&self.id)
                .is_some_and(|s| Arc::ptr_eq(s, &self))
            {
                state.sessions.remove(&self.id);
            }
        }
        tracing::info!(
            session_id = self.id,
            exit_code = code,
            dropped_input_bytes = self.dropped_input.load(Ordering::Relaxed),
            dropped_commands = self.dropped_commands.load(Ordering::Relaxed),
            dropped_output_bytes = self.dropped_output.load(Ordering::Relaxed),
            "UU terminal ended"
        );
    }

    fn wait_for_io(
        &self,
        running: Option<&Running>,
        writing: bool,
        last_check: Instant,
        footer: Option<Instant>,
    ) {
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        let mut fds = vec![libc::pollfd {
            fd: self.wake_pipe.reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let mut deadline = None;
        match running {
            #[cfg(target_os = "linux")]
            Some(Running::Pty(pty)) => {
                let reading = self.output_ready();
                if reading || writing {
                    fds.push(libc::pollfd {
                        fd: pty.fd(),
                        events: (if reading { libc::POLLIN } else { 0 })
                            | (if writing { libc::POLLOUT } else { 0 }),
                        revents: 0,
                    });
                }
                if reading {
                    if let Some(fd) = pty.exit_fd() {
                        fds.push(libc::pollfd {
                            fd,
                            events: libc::POLLIN,
                            revents: 0,
                        });
                    } else {
                        deadline = Some(last_check + Duration::from_secs(1));
                    }
                }
                deadline = deadline.into_iter().chain(pty.redraw_deadline()).min();
            }
            Some(Running::Serial(serial)) => {
                if let Some(fd) = serial.fd() {
                    fds.push(libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                }
                deadline = Some(last_check + Duration::from_secs(1));
            }
            None => {
                deadline = Some(last_check + Duration::from_secs(1))
                    .into_iter()
                    .chain(footer)
                    .min()
            }
        }
        let timeout = deadline.map_or(-1, |at| {
            at.saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32
        });
        unsafe {
            libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout);
        }
    }

    fn serial_input(&self, serial: &mut targets::serial::Serial, bytes: &[u8]) {
        use targets::serial::MenuAction;
        for &byte in bytes {
            match serial.menu.key(byte, serial.baud) {
                MenuAction::Send(byte) => serial.write(&[byte]),
                MenuAction::Print(text) => self.output(text.as_bytes()),
                MenuAction::Break => self.output(
                    if serial.send_break().is_ok() {
                        "\r\n已发送 Break\r\n"
                    } else {
                        "\r\n发送 Break 失败\r\n"
                    }
                    .as_bytes(),
                ),
                MenuAction::Baud(baud) => {
                    if serial.set_baud(baud).is_ok() {
                        self.output(format!("\r\n波特率 {baud}\r\n").as_bytes());
                        let short = serial
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        self.meta.lock().status.detail = format!("{short} · {baud}");
                    } else {
                        self.output("\r\n无法设置波特率\r\n".as_bytes());
                    }
                }
                MenuAction::Disconnect => {
                    self.stop(None);
                    break;
                }
                MenuAction::None => {}
            }
        }
    }
}
enum Running {
    Serial(targets::serial::Serial),
    #[cfg(target_os = "linux")]
    Pty(targets::pty::Pty),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached() -> (
        Arc<Session>,
        Arc<TerminalConn>,
        tokio::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let registry = Registry::new(PathBuf::new());
        let (session, _commands) = Session::new(1, "test".into(), (80, 24)).unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let conn = TerminalConn::new(registry.clone(), tx);
        registry.state.lock().sessions.insert(1, session.clone());
        session.attach(&conn, (80, 24));
        (session, conn, rx)
    }

    #[test]
    fn input_overload_is_counted_and_latest_resize_survives_full_queue() {
        let (session, rx) = Session::new(1, "test".into(), (80, 24)).unwrap();
        for _ in 0..64 {
            session.command(Command::Input(vec![1; 4]));
        }
        session.command(Command::Input(vec![2; 7]));
        session.command(Command::Signal(libc::SIGINT));
        session.command(Command::Resize((90, 30)));
        session.command(Command::Resize((100, 40)));
        assert_eq!(*session.resize.lock(), Some((100, 40)));
        assert_eq!(rx.try_iter().count(), 64);
        assert_eq!(session.dropped_input.load(Ordering::Relaxed), 7);
        assert_eq!(session.dropped_commands.load(Ordering::Relaxed), 1);
        let mut pending = VecDeque::from(vec![1; INPUT_LIMIT - 2]);
        session.queue_input(&mut pending, &[2; 5]);
        assert_eq!(pending.len(), INPUT_LIMIT);
        assert_eq!(session.dropped_input.load(Ordering::Relaxed), 10);
        pending.clear();
        session.queue_input(&mut pending, b"recovered");
        assert_eq!(pending.make_contiguous(), b"recovered");
        assert!(!session.stop.load(Ordering::Acquire));
    }

    #[test]
    fn congestion_reserves_control_frames_and_replays_without_cancelling() {
        let (session, conn, mut rx) = attached();
        rx.try_recv().unwrap(); // OPEN_RESP
        while session.output_ready() {
            session.output(&[42; OUTPUT_CHUNK]);
        }
        assert_eq!(conn.out.tx.lock().capacity(), super::super::CONTROL_RESERVE);
        for kind in [OPEN_RESP, EXIT, LIST_RESP, KILL_RESP, ERROR] {
            conn.send(kind, 1, b"{}");
        }
        let mut controls = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            let frame = decode(&bytes).unwrap();
            if frame.kind != STDOUT {
                controls.push(frame.kind);
            }
        }
        assert_eq!(controls, [OPEN_RESP, EXIT, LIST_RESP, KILL_RESP, ERROR]);
        assert!(session.output_ready());
        assert!(session.status().attached);
        assert!(!conn.cancelled.is_cancelled());

        session.detach(conn.id);
        session.output(&vec![19; SCROLLBACK]);
        // Reattach with another session's data already occupying the queue.
        while conn.out.data(2, &[8]) {}
        session.attach(&conn, (80, 24));
        assert_eq!(session.output.lock().pending.len(), SCROLLBACK);
        session.output(&[20; OUTPUT_CHUNK]);
        assert_eq!(
            session.output.lock().pending.len(),
            SCROLLBACK + OUTPUT_CHUNK
        );
        let mut replay = Vec::new();
        while replay.len() < SCROLLBACK + OUTPUT_CHUNK {
            while let Ok(bytes) = rx.try_recv() {
                let frame = decode(&bytes).unwrap();
                if frame.kind == STDOUT && frame.session == 1 {
                    replay.extend_from_slice(frame.payload);
                }
            }
            session.output_ready();
        }
        assert_eq!(&replay[..SCROLLBACK], vec![19; SCROLLBACK]);
        assert_eq!(&replay[SCROLLBACK..], &[20; OUTPUT_CHUNK]);
    }

    #[test]
    fn serial_congestion_keeps_history_and_reports_skips_even_without_new_bytes() {
        let (session, conn, mut rx) = attached();
        while conn.out.data(2, b"busy") {}
        session.serial_output(&vec![3; 2048]);
        session.serial_output(&vec![4; 1024]);
        assert_eq!(session.output.lock().skipped, 3072);
        assert_eq!(session.output.lock().scrollback.len(), 3072);
        assert!(session.output.lock().pending.is_empty());
        while rx.try_recv().is_ok() {}
        session.output_ready();
        let bytes = rx.try_recv().unwrap();
        assert_eq!(
            decode(&bytes).unwrap().payload,
            "\r\n\x1b[2m[已跳过 3 KB 串口输出]\x1b[0m\r\n".as_bytes()
        );
        session.serial_output(b"resumed");
        let bytes = rx.try_recv().unwrap();
        assert_eq!(decode(&bytes).unwrap().payload, b"resumed");
        assert_eq!(session.output.lock().skipped, 0);
        assert!(session.status().attached);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn serial_worker_reads_during_congestion_and_flushes_on_capacity_wakeup() {
        let pair = nix::pty::openpty(None, None).unwrap();
        let path = std::fs::read_link(format!("/proc/self/fd/{}", pair.slave.as_raw_fd())).unwrap();
        let mut master = File::from(pair.master);
        let registry = Registry::new(PathBuf::new());
        let (session, commands) = Session::new(1, "test".into(), (80, 24)).unwrap();
        {
            let mut state = registry.state.lock();
            state.accepting = true;
            state.config.serial.enabled = true;
            // A private PTY stands in for USB hardware; production validation
            // intentionally accepts only configured USB device paths.
            state.config.serial.device = path.to_string_lossy().into_owned();
            state.config.serial.baud = 1500000;
            state.sessions.insert(1, session.clone());
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let conn = TerminalConn::new(registry.clone(), tx);
        session.attach(&conn, (80, 24));
        session.spawn(Arc::downgrade(&registry), commands).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let bytes = rx.recv().await.unwrap();
                if String::from_utf8_lossy(decode(&bytes).unwrap().payload).contains("1500000") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        while conn.out.data(2, b"busy") {}
        master.write_all(&vec![7; 3072]).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while session.dropped_output.load(Ordering::Relaxed) < 3072 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            session
                .output
                .lock()
                .scrollback
                .iter()
                .filter(|&&byte| byte == 7)
                .count()
                == 3072
        );
        while rx.try_recv().is_ok() {}
        conn.output_ready();
        let bytes = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(decode(&bytes).unwrap().payload)
            .contains("[已跳过 3 KB 串口输出]"));
        master.write_all(b"resumed").unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(decode(&bytes).unwrap().payload, b"resumed");
        assert!(session.status().attached);
        registry.shutdown().await;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn poll_wakes_for_commands_capacity_and_pty_output() {
        let (session, conn, mut rx) = attached();
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "stty -echo; read line; printf ready; read line"]);
        let pty = targets::pty::Pty::spawn(command, (80, 24)).unwrap();
        let mut running = Running::Pty(pty);
        session.wake_pipe.drain();
        session.command(Command::Resize((100, 40)));
        session.wait_for_io(Some(&running), false, Instant::now(), None);
        session.wake_pipe.drain();

        while conn.out.data(2, b"busy") {}
        rx.try_recv().unwrap();
        conn.output_ready();
        session.wait_for_io(Some(&running), false, Instant::now(), None);
        session.wake_pipe.drain();
        while rx.try_recv().is_ok() {}

        if let Running::Pty(pty) = &mut running {
            pty.write(b"go\n").unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while !output.windows(5).any(|b| b == b"ready") {
            assert!(Instant::now() < deadline);
            session.wait_for_io(Some(&running), false, Instant::now(), None);
            let mut buf = [0; 1024];
            if let Running::Pty(pty) = &mut running {
                if let Ok(n) = pty.read(&mut buf) {
                    output.extend_from_slice(&buf[..n]);
                }
            }
        }
        // With no data, commands or redraw timer, an idle PTY must stay parked
        // beyond the former 20 ms polling interval, then stop must wake it.
        let (done, completed) = mpsc::channel();
        let waiting = session.clone();
        let worker = std::thread::spawn(move || {
            waiting.wait_for_io(Some(&running), false, Instant::now(), None);
            done.send(()).unwrap();
        });
        let idle = completed.recv_timeout(Duration::from_millis(100));
        session.stop(None);
        if idle.is_err() {
            completed.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        worker.join().unwrap();
        assert!(matches!(idle, Err(mpsc::RecvTimeoutError::Timeout)));
    }
}
