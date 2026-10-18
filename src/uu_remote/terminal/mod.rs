//! UU terminal sessions belong to one UU service, independently of its WebRTC peers.
pub mod frame;
mod picker;
mod session;
pub mod targets;
#[cfg(test)]
mod tests;

use super::wire;
use crate::config::UuTerminalConfig;
use frame::*;
use parking_lot::Mutex;
use serde::Serialize;
use session::{Command, Session};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use typeshare::typeshare;

pub const DISABLED: &str =
    "终端未开启：请在 One-KVM 网页「设置 › 第三方访问 › UU 远程 › 终端」中开启";
pub const LIMIT: &str = "终端会话已达上限（8 个），请在会话列表中终止不用的会话";
#[typeshare]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Picker,
    Board,
    Serial,
    Ssh,
}
impl Target {
    fn enabled(self, config: &UuTerminalConfig) -> bool {
        match self {
            Self::Picker => true,
            Self::Board => config.board.enabled,
            Self::Serial => config.serial.enabled,
            Self::Ssh => config.ssh.enabled,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Picker => "选择中",
            Self::Board => "板子",
            Self::Serial => "串口",
            Self::Ssh => "SSH",
        }
    }
}
#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct TerminalSessionStatus {
    pub id: u32,
    pub name: String,
    pub target: Target,
    pub detail: String,
    pub attached: bool,
    #[typeshare(serialized_as = "number")]
    pub created_at_ms: u64,
}
#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct TerminalSerialStatus {
    pub usb_gadget_enabled: bool,
    pub devices: Vec<targets::serial::SerialDevice>,
    pub busy_session: Option<u32>,
}
#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct TerminalSshStatus {
    pub public_key: Option<String>,
}
#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct TerminalStatus {
    pub peers: u32,
    pub sessions: Vec<TerminalSessionStatus>,
    pub serial: TerminalSerialStatus,
    pub ssh: TerminalSshStatus,
}

struct State {
    config: UuTerminalConfig,
    name: String,
    sessions: BTreeMap<u32, Arc<Session>>,
    next_id: u32,
    last: Option<Target>,
    accepting: bool,
}
pub struct Registry {
    state: Mutex<State>,
    pub ssh: targets::ssh::Ssh,
    next_connection: AtomicU64,
}
impl Registry {
    pub fn new(directory: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                config: Default::default(),
                name: "vim4-kvm".into(),
                sessions: BTreeMap::new(),
                next_id: 1,
                last: None,
                accepting: false,
            }),
            ssh: targets::ssh::Ssh::new(directory),
            next_connection: AtomicU64::new(1),
        })
    }
    pub fn start(&self) {
        self.state.lock().accepting = true;
    }
    pub async fn configure(&self, config: &UuTerminalConfig, name: &str) -> Result<(), String> {
        config.validate()?;
        if config.ssh.enabled {
            self.ssh.ensure_key(name).await?;
        }
        let closing = {
            let mut state = self.state.lock();
            state.config = config.clone();
            state.name = name.into();
            let ids: Vec<_> = state
                .sessions
                .iter()
                .filter(|(_, s)| !s.meta.lock().status.target.enabled(config))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| state.sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        for s in &closing {
            s.stop(Some("\r\n此终端类型已在网页中关闭。\r\n"));
        }
        join_sessions(closing).await;
        Ok(())
    }
    pub async fn shutdown(&self) {
        let sessions = {
            let mut state = self.state.lock();
            state.accepting = false;
            std::mem::take(&mut state.sessions)
                .into_values()
                .collect::<Vec<_>>()
        };
        for s in &sessions {
            s.stop(None);
        }
        join_sessions(sessions).await;
    }
    pub async fn kill(&self, id: u32) -> bool {
        let session = self.state.lock().sessions.remove(&id);
        if let Some(s) = session {
            s.stop(None);
            join_sessions(vec![s]).await;
            true
        } else {
            false
        }
    }
    fn kill_blocking(&self, id: u32) -> bool {
        let session = self.state.lock().sessions.remove(&id);
        if let Some(s) = session {
            s.stop(None);
            s.join();
            true
        } else {
            false
        }
    }
    fn session(&self, id: u32) -> Option<Arc<Session>> {
        self.state.lock().sessions.get(&id).cloned()
    }
    fn snapshots(&self) -> Vec<TerminalSessionStatus> {
        self.state
            .lock()
            .sessions
            .values()
            .map(|s| s.status())
            .collect()
    }
    pub async fn status(&self, peers: u32) -> TerminalStatus {
        let config = self.state.lock().config.clone();
        let busy_session = self.busy(config.serial.path(), 0).map(|(id, _)| id);
        TerminalStatus {
            peers,
            sessions: self.snapshots(),
            serial: TerminalSerialStatus {
                usb_gadget_enabled: false,
                devices: targets::serial::devices(&config.serial.device),
                busy_session,
            },
            ssh: TerminalSshStatus {
                public_key: self.ssh.public_key().await,
            },
        }
    }
    fn busy(&self, path: &str, except: u32) -> Option<(u32, String)> {
        busy(&self.state.lock(), path, except)
    }
    fn items(&self, except: u32) -> Vec<picker::Item> {
        let state = self.state.lock();
        let c = &state.config;
        let mut items = Vec::new();
        if c.board.enabled {
            items.push(picker::Item {
                target: Target::Board,
                detail: format!("{}@{}", user(), state.name),
                unavailable: None,
                busy: None,
            });
        }
        if c.serial.enabled {
            items.push(picker::Item {
                target: Target::Serial,
                detail: format!(
                    "{} · {}",
                    if c.serial.device == "usb-gadget" {
                        "USB 串口".into()
                    } else {
                        targets::serial::device_label(Path::new(c.serial.path()))
                    },
                    c.serial.baud
                ),
                unavailable: (!Path::new(c.serial.path()).exists()).then(|| "未检测到设备".into()),
                busy: busy(&state, c.serial.path(), except),
            });
        }
        if c.ssh.enabled {
            items.push(picker::Item {
                target: Target::Ssh,
                detail: format!("{}@{}", c.ssh.user, c.ssh.host),
                unavailable: (!c.ssh.configured()).then(|| "未设置地址".into()),
                busy: None,
            });
        }
        items
    }
    pub fn rename(&self, id: u32, name: &str) -> Result<(), String> {
        let state = self.state.lock();
        let session = state.sessions.get(&id).ok_or("该会话已结束")?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
            return Err("名称须为 1–40 字符且不能含控制字符".into());
        }
        if state
            .sessions
            .iter()
            .any(|(other, s)| *other != id && s.meta.lock().status.name == name)
        {
            return Err("会话名称已存在".into());
        }
        let mut meta = session.meta.lock();
        meta.status.name = name.into();
        meta.renamed = true;
        Ok(())
    }
    pub fn rpc_response(&self, rpc: &[wire::WireField]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        wire::put_bytes(1, wire::message(rpc, 1).unwrap_or(&[]), &mut out);
        if wire::message(rpc, 28).is_some() {
            let mut body = Vec::new();
            wire::put_bytes(2, b"ok", &mut body);
            wire::put_bytes(24, &body, &mut out);
            return Some(out);
        }
        let request = wire::decode(wire::message(rpc, 29)?).ok()?;
        let id = wire::number(&request, 1)
            .and_then(|id| u32::try_from(id).ok())
            .unwrap_or(0);
        let name = wire::message(&request, 2)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("");
        let result = self.rename(id, name);
        let mut body = Vec::new();
        wire::put_varint(1, u64::from(id), &mut body);
        if result.is_err() {
            wire::put_varint(2, 1, &mut body);
        }
        wire::put_bytes(
            3,
            result.err().as_deref().unwrap_or("ok").as_bytes(),
            &mut body,
        );
        wire::put_bytes(26, &body, &mut out);
        Some(out)
    }
    fn open(
        self: &Arc<Self>,
        requested: u32,
        size: (u16, u16),
        connection: &TerminalConn,
    ) -> Result<(), String> {
        let mut state = self.state.lock();
        if !state.accepting {
            return Err(DISABLED.into());
        }
        if let Some(session) = state.sessions.get(&requested) {
            session.attach(connection, size);
            return Ok(());
        }
        if state.sessions.len() >= 8 {
            return Err(LIMIT.into());
        }
        if !state.config.board.enabled && !state.config.serial.enabled && !state.config.ssh.enabled
        {
            return Err(DISABLED.into());
        }
        let id = if requested != 0 {
            requested
        } else {
            loop {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                if id != 0 && !state.sessions.contains_key(&id) {
                    break id;
                }
            }
        };
        let name = unique_name(&state, Target::Picker.name(), id);
        let (session, rx) = Session::new(id, name, size).map_err(|_| "无法创建终端唤醒管道")?;
        state.sessions.insert(id, session.clone());
        if session.spawn(Arc::downgrade(self), rx).is_err() {
            state.sessions.remove(&id);
            return Err("无法创建终端工作线程".into());
        }
        // The worker takes the registry lock before producing output. Queue the
        // acknowledgement while still holding it, but only after spawn succeeds.
        session.attach(connection, size);
        Ok(())
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        for s in self.state.get_mut().sessions.values() {
            s.stop(None);
        }
        for s in self.state.get_mut().sessions.values() {
            s.join();
        }
    }
}
async fn join_sessions(sessions: Vec<Arc<Session>>) {
    if !sessions.is_empty() {
        let _ = tokio::task::spawn_blocking(move || {
            for s in sessions {
                s.join();
            }
        })
        .await;
    }
}
fn same_device(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
}
fn busy(state: &State, path: &str, except: u32) -> Option<(u32, String)> {
    state
        .sessions
        .iter()
        .filter(|(id, _)| **id != except)
        .find_map(|(id, s)| {
            let m = s.meta.lock();
            m.serial_path
                .as_ref()
                .filter(|p| same_device(p, Path::new(path)))
                .map(|_| (*id, m.status.name.clone()))
        })
}
fn unique_name(state: &State, base: &str, except: u32) -> String {
    let mut name = base.into();
    let mut suffix = 2;
    while state
        .sessions
        .iter()
        .any(|(id, s)| *id != except && s.meta.lock().status.name == name)
    {
        name = format!("{base} {suffix}");
        suffix += 1;
    }
    name
}
fn user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "khadas".into())
}

// All producers share this lock so concurrent sessions cannot consume the
// control reserve after independently observing the same available capacity.
struct Outbound {
    tx: Mutex<tokio::sync::mpsc::Sender<Vec<u8>>>,
}
const CONTROL_RESERVE: usize = 8;
impl Outbound {
    fn ready(&self) -> bool {
        let tx = self.tx.lock();
        tx.is_closed() || tx.capacity() > CONTROL_RESERVE
    }
    fn data(&self, id: u32, data: &[u8]) -> bool {
        let tx = self.tx.lock();
        tx.capacity() > CONTROL_RESERVE && tx.try_send(encode(STDOUT, id, data)).is_ok()
    }
    fn control(&self, kind: u8, id: u32, data: &[u8]) {
        let _ = self.tx.lock().try_send(encode(kind, id, data));
    }
}

pub struct TerminalConn {
    registry: Arc<Registry>,
    id: u64,
    out: Arc<Outbound>,
    closed: AtomicBool,
    cancelled: tokio_util::sync::CancellationToken,
}
impl TerminalConn {
    pub fn new(registry: Arc<Registry>, out: tokio::sync::mpsc::Sender<Vec<u8>>) -> Arc<Self> {
        let id = registry.next_connection.fetch_add(1, Ordering::Relaxed);
        Arc::new(Self {
            registry,
            id,
            out: Arc::new(Outbound {
                tx: Mutex::new(out),
            }),
            closed: AtomicBool::new(false),
            cancelled: tokio_util::sync::CancellationToken::new(),
        })
    }
    fn send(&self, kind: u8, id: u32, payload: &[u8]) {
        self.out.control(kind, id, payload);
    }
    pub fn output_ready(&self) {
        for session in self.registry.state.lock().sessions.values() {
            if session.attached_to(self.id) {
                session.wake();
            }
        }
    }
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancelled.cancel();
        self.detach();
    }
    pub async fn cancelled(&self) {
        self.cancelled.cancelled().await;
    }
    pub fn detach(&self) {
        for s in self.registry.state.lock().sessions.values() {
            s.detach(self.id);
        }
    }
    pub async fn handle(&self, data: &[u8]) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let Some(frame) = decode(data) else {
            return;
        };
        let existing = self.registry.session(frame.session);
        match frame.kind {
            OPEN => {
                if let Err(error) =
                    self.registry
                        .open(frame.session, dims(&json(frame.payload)), self)
                {
                    self.send(OPEN_RESP, frame.session, &reply(1, &error));
                }
            }
            STDIN => {
                if let Some(s) = existing {
                    if s.attached_to(self.id) {
                        s.command(Command::Input(frame.payload.to_vec()));
                    }
                }
            }
            RESIZE => {
                if let Some(s) = existing {
                    if s.attached_to(self.id) {
                        s.command(Command::Resize(dims(&json(frame.payload))));
                    }
                }
            }
            SIGNAL => {
                if let Some(s) = existing {
                    if s.attached_to(self.id) {
                        s.command(Command::Signal(signal_number(frame.payload)));
                    }
                }
            }
            CLOSE => {
                self.registry.kill(frame.session).await;
            }
            DETACH => {
                if let Some(s) = existing {
                    s.detach(self.id);
                }
            }
            KILL_REQ => {
                let id = if frame.session != 0 {
                    frame.session
                } else {
                    json(frame.payload)["session_id"]
                        .as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0)
                };
                let found = self.registry.kill(id).await;
                self.send(
                    KILL_RESP,
                    id,
                    &reply(
                        if found { 0 } else { 1 },
                        if found { "ok" } else { "该会话已结束" },
                    ),
                );
            }
            LIST_REQ => {
                let list:Vec<_>=self.registry.snapshots().iter().map(|s|serde_json::json!({"session_id":s.id,"name":s.name,"shell":s.detail,"state":if s.attached{"running"}else{"detached"},"created_at_ms":s.created_at_ms})).collect();
                self.send(
                    LIST_RESP,
                    frame.session,
                    &serde_json::to_vec(&list).unwrap(),
                );
            }
            UNLOCK_REQ => self.send(UNLOCK_RESP, frame.session, &reply(0, "ok")),
            _ => self.send(ERROR, frame.session, &reply(1, "不支持的终端帧")),
        }
    }
}
impl Drop for TerminalConn {
    fn drop(&mut self) {
        self.close();
    }
}
fn signal_number(payload: &[u8]) -> i32 {
    let v = json(payload);
    let text = v
        .as_str()
        .or_else(|| {
            v.as_object()
                .and_then(|o| o.values().find_map(|v| v.as_str()))
        })
        .unwrap_or_else(|| std::str::from_utf8(payload).unwrap_or(""));
    let number = v
        .as_object()
        .and_then(|o| o.values().find_map(|v| v.as_i64()));
    match number {
        Some(n) if (1..=64).contains(&n) => n as i32,
        _ => match text.trim().trim_start_matches("SIG") {
            "TERM" => 15,
            "KILL" => 9,
            "HUP" => 1,
            "QUIT" => 3,
            _ => 2,
        },
    }
}
