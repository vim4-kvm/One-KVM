//! Stateless MCP transport, independent of web sessions and built-in CUA.
pub mod auth;
mod credentials;
mod definitions;
pub mod keys;
pub mod server;
#[cfg(all(test, unix))]
mod tests;
mod tools;
pub mod types;

use crate::{
    config::ConfigStore,
    events::{EventBus, SystemEvent},
    hid::HidController,
};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use types::*;

pub struct McpService {
    enabled: std::sync::atomic::AtomicBool,
    stopping: std::sync::atomic::AtomicBool,
    cancellation: parking_lot::Mutex<tokio_util::sync::CancellationToken>,
    pub config: ConfigStore,
    pub hid: Arc<HidController>,
    pub(crate) credentials: parking_lot::Mutex<credentials::Credentials>,
    pub mutation: tokio::sync::Mutex<()>,
    status: parking_lot::Mutex<StatusMemory>,
}
struct StatusMemory {
    call_id: u64,
    activity: McpActivity,
    recent: VecDeque<McpCallRecord>,
    last_input: Option<tokio::time::Instant>,
}
pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
pub fn input_capabilities(config: &crate::config::HidConfig) -> (bool, bool) {
    use crate::config::HidBackend;
    match config.backend {
        HidBackend::None => (false, false),
        HidBackend::Otg => {
            let f = config.effective_otg_functions();
            (f.keyboard, f.mouse_absolute)
        }
        HidBackend::Bluetooth => (true, false),
        HidBackend::Ch9329 => (true, config.mouse_absolute),
    }
}
impl McpService {
    pub fn new(
        config: ConfigStore,
        hid: Arc<HidController>,
        data: &std::path::Path,
        events: Arc<EventBus>,
    ) -> Arc<Self> {
        let mut credentials = credentials::Credentials::load(data);
        if config.get().mcp.enabled {
            let _ = credentials.ensure();
        }
        hid.input_control.set_enabled(config.get().mcp.enabled);
        let service = Arc::new(Self {
            enabled: std::sync::atomic::AtomicBool::new(config.get().mcp.enabled),
            stopping: std::sync::atomic::AtomicBool::new(false),
            cancellation: parking_lot::Mutex::new(tokio_util::sync::CancellationToken::new()),
            config,
            hid,
            credentials: parking_lot::Mutex::new(credentials),
            mutation: tokio::sync::Mutex::new(()),
            status: parking_lot::Mutex::new(StatusMemory {
                call_id: 0,
                activity: McpActivity {
                    state: "idle".into(),
                    client: None,
                    tool: None,
                    detail: None,
                    since: now(),
                },
                recent: VecDeque::new(),
                last_input: None,
            }),
        });
        let weak = Arc::downgrade(&service);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(200));
            let mut previous = None;
            loop {
                interval.tick().await;
                let Some(service) = weak.upgrade() else { break };
                // Also observe configuration imports, not only the MCP settings API.
                let enabled = service.config.get().mcp.enabled;
                if service.stopping.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                service.set_enabled(enabled);
                let activity = service.activity();
                if previous.as_ref() != Some(&activity) {
                    events.publish(SystemEvent::McpActivity(activity.clone()));
                    previous = Some(activity);
                }
            }
        });
        service
    }
    pub fn operation_token(&self) -> tokio_util::sync::CancellationToken {
        let token = self.cancellation.lock().clone();
        if !self.enabled.load(std::sync::atomic::Ordering::Acquire)
            || self.stopping.load(std::sync::atomic::Ordering::Acquire)
        {
            token.cancel();
        }
        token
    }
    pub fn cancel_operations(&self) {
        let mut token = self.cancellation.lock();
        token.cancel();
        *token = tokio_util::sync::CancellationToken::new();
        self.hid.input_control.cancel(crate::hid::control::DISABLED);
    }
    pub fn set_enabled(&self, enabled: bool) {
        if self
            .enabled
            .swap(enabled, std::sync::atomic::Ordering::AcqRel)
            != enabled
        {
            self.cancel_operations();
            self.hid.input_control.set_enabled(enabled);
        }
    }
    pub fn stop(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        self.cancel_operations();
        self.hid.input_control.set_enabled(false);
    }
    pub fn activity(&self) -> McpActivity {
        let mut s = self.status.lock();
        let mut state = self.hid.input_control.activity();
        if state == "idle"
            && s.last_input
                .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            state = "active";
        }
        if !self.config.get().mcp.enabled {
            state = "idle";
        }
        if state != s.activity.state {
            s.activity.state = state.into();
            s.activity.since = now();
        }
        s.activity.clone()
    }
    pub fn client(&self, name: Option<&str>) {
        self.status.lock().activity.client =
            name.map(|s| s.chars().filter(|c| !c.is_control()).take(80).collect());
    }
    pub fn begin(&self, tool: &str, detail: Option<String>) -> u64 {
        let mut s = self.status.lock();
        s.call_id += 1;
        s.activity.tool = Some(tool.into());
        s.activity.detail = detail;
        s.call_id
    }
    pub fn record(
        &self,
        call_id: u64,
        tool: &str,
        input: bool,
        started: tokio::time::Instant,
        error: Option<String>,
        detail: Option<String>,
    ) {
        let mut s = self.status.lock();
        // An older concurrent call must not replace the latest call's summary.
        if s.call_id == call_id {
            s.activity.detail = detail.clone();
        }
        if input {
            s.last_input = Some(tokio::time::Instant::now());
        }
        let record = McpCallRecord {
            at: now(),
            client: s.activity.client.clone(),
            tool: tool.into(),
            ok: error.is_none(),
            error,
            duration_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            detail,
        };
        s.recent.push_front(record);
        s.recent.truncate(20);
    }
    pub async fn status(&self, snapshot: &crate::video::snapshot::SnapshotService) -> McpStatus {
        let cfg = self.config.get();
        let video_signal = snapshot.source_size().await.is_some();
        let activity = self.activity();
        let credentials = self.credentials.lock();
        McpStatus {
            enabled: cfg.mcp.enabled,
            endpoint_path: "/api/mcp".into(),
            api_key_preview: credentials.preview(),
            credentials_error: credentials.error.clone(),
            activity,
            recent: self.status.lock().recent.iter().cloned().collect(),
            capabilities: McpCapabilities {
                keyboard: input_capabilities(&cfg.hid).0,
                absolute_mouse: input_capabilities(&cfg.hid).1,
                video_signal,
            },
            computer_use_active: self.hid.input_control.cua_active(),
        }
    }
}
