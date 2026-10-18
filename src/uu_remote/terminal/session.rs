use super::{
    frame::*,
    picker::{self, Action, Picker},
    targets, unique_name, Registry, Target, TerminalConn, TerminalSessionStatus,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc, Weak,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) enum Command {
    Input(Vec<u8>),
    Resize((u16, u16)),
    Redraw,
    Signal(i32),
    Wake,
}
pub(super) struct Metadata {
    pub status: TerminalSessionStatus,
    pub renamed: bool,
    pub serial_path: Option<PathBuf>,
}
struct Output {
    sink: Option<(
        u64,
        tokio::sync::mpsc::Sender<Vec<u8>>,
        tokio_util::sync::CancellationToken,
    )>,
    scrollback: VecDeque<u8>,
}
pub(super) struct Session {
    pub meta: Mutex<Metadata>,
    id: u32,
    output: Mutex<Output>,
    size: Mutex<(u16, u16)>,
    tx: SyncSender<Command>,
    stop: AtomicBool,
    reason: Mutex<Option<&'static str>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Session {
    pub fn new(id: u32, name: String, size: (u16, u16)) -> (Arc<Self>, Receiver<Command>) {
        let (tx, rx) = mpsc::sync_channel(64);
        (
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
                }),
                size: Mutex::new(size),
                tx,
                stop: AtomicBool::new(false),
                reason: Mutex::new(None),
                worker: Mutex::new(None),
            }),
            rx,
        )
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
        if self.tx.try_send(cmd).is_err() {
            self.stop(None);
        }
    }
    pub fn stop(&self, reason: Option<&'static str>) {
        if let Some(reason) = reason {
            *self.reason.lock() = Some(reason);
        }
        self.stop.store(true, Ordering::Release);
        let _ = self.tx.try_send(Command::Wake);
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
            .is_some_and(|(_, tx, cancelled)| !tx.is_closed() && !cancelled.is_cancelled());
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
        for chunk in out.scrollback.make_contiguous().chunks(OUTPUT_CHUNK) {
            connection.send(STDOUT, self.id, chunk);
        }
        *self.size.lock() = size;
        self.command(Command::Resize(size));
        self.command(Command::Redraw);
    }
    pub(super) fn output(&self, data: &[u8]) {
        let mut out = self.output.lock();
        out.scrollback.extend(data);
        let excess = out.scrollback.len().saturating_sub(SCROLLBACK);
        out.scrollback.drain(..excess);
        for chunk in data.chunks(OUTPUT_CHUNK) {
            if out.sink.as_ref().is_some_and(|(_, tx, cancelled)| {
                if tx.try_send(encode(STDOUT, self.id, chunk)).is_err() {
                    cancelled.cancel();
                    true
                } else {
                    false
                }
            }) {
                out.sink = None;
            }
        }
    }
    fn finish(&self, code: i32) {
        let payload = serde_json::to_vec(&serde_json::json!({"exit_code":code})).unwrap();
        let mut out = self.output.lock();
        if let Some((_, tx, _)) = out.sink.take() {
            let _ = tx.try_send(encode(EXIT, self.id, &payload));
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
            let command = rx.recv_timeout(Duration::from_millis(20)).ok();
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            match command {
                Some(Command::Input(bytes)) => {
                    if let Some(target) = running.as_mut() {
                        match target {
                            Running::Serial(serial) => self.serial_input(serial, &bytes),
                            #[cfg(target_os = "linux")]
                            Running::Pty(_) => {
                                if pending.len() + bytes.len() > 1024 * 1024 {
                                    self.stop(None);
                                } else {
                                    pending.extend(bytes);
                                }
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
                Some(Command::Resize(size)) => {
                    *self.size.lock() = size;
                    #[cfg(target_os = "linux")]
                    if let Some(Running::Pty(pty)) = running.as_mut() {
                        pty.resize(size);
                    }
                    if running.is_none() {
                        self.draw(&reg, &picker, &items);
                    }
                }
                Some(Command::Redraw) => {
                    #[cfg(target_os = "linux")]
                    if let Some(Running::Pty(pty)) = running.as_mut() {
                        pty.redraw();
                    }
                    if running.is_none() {
                        self.draw(&reg, &picker, &items);
                    }
                }
                Some(Command::Signal(signal)) =>
                {
                    #[cfg(target_os = "linux")]
                    if let Some(Running::Pty(pty)) = &running {
                        pty.signal(signal);
                    }
                }
                _ => {}
            }
            match running.as_mut() {
                Some(Running::Serial(serial)) => {
                    for _ in 0..16 {
                        match serial.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => self.output(&buffer[..n]),
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
                    for _ in 0..16 {
                        match pty.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => self.output(&buffer[..n]),
                        }
                    }
                    if let Some(exit) = pty.tick() {
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
        tracing::info!(session_id = self.id, exit_code = code, "UU terminal ended");
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
