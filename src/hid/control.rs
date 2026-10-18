//! MCP ownership is checked by the same worker that writes manual input.
use super::{CanonicalKey, KeyEventType, KeyboardEvent, MouseEvent, MouseEventType};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub const MANUAL: &str = "A person is using the keyboard or mouse. Retry in about 2 seconds.";
pub const PREEMPTED: &str = "Cancelled: a person took over the keyboard or mouse.";
pub const DISABLED: &str = "Cancelled: the MCP service was disabled.";
pub const BUSY: &str = "Another MCP action is still running. Retry after it finishes.";
pub const CUA: &str = "One-KVM's built-in Computer Use session is running. Stop it first.";
pub const HOLD_TIMEOUT: &str = "Cancelled: mouse button hold timed out after 30 seconds.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManualKind {
    WebSocket,
    DataChannel,
    Uu,
    Vnc,
    RustDesk,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    Manual(ManualKind),
    Mcp(u64),
    ComputerUse,
    System,
}

pub struct InputControl {
    state: parking_lot::Mutex<ControlState>,
    lane: Arc<Semaphore>,
    pub(crate) wake: Notify,
}
struct ControlState {
    generation: u64,
    active: bool,
    enabled: bool,
    cua: u32,
    token: CancellationToken,
    reason: &'static str,
    last_manual: Option<Instant>,
    keys: HashSet<(ManualKind, CanonicalKey)>,
    modifiers: std::collections::HashMap<ManualKind, u8>,
    buttons: std::collections::HashMap<ManualKind, u8>,
    cursor: Option<(i32, i32)>,
    mcp_buttons: u8,
}
impl Default for InputControl {
    fn default() -> Self {
        Self {
            state: parking_lot::Mutex::new(ControlState {
                generation: 1,
                active: false,
                enabled: false,
                cua: 0,
                token: CancellationToken::new(),
                reason: DISABLED,
                last_manual: None,
                keys: HashSet::new(),
                modifiers: Default::default(),
                buttons: Default::default(),
                cursor: None,
                mcp_buttons: 0,
            }),
            lane: Arc::new(Semaphore::new(1)),
            wake: Notify::new(),
        }
    }
}
impl ControlState {
    fn manual(&self) -> bool {
        !self.keys.is_empty()
            || self.modifiers.values().any(|v| *v != 0)
            || self.buttons.values().any(|v| *v != 0)
            || self
                .last_manual
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
    }
    fn cancel(&mut self, reason: &'static str) {
        self.reason = reason;
        self.token.cancel();
        self.generation += 1;
        self.active = false;
        self.mcp_buttons = 0;
    }
    fn allowed(&self) -> Result<(), String> {
        if !self.enabled {
            Err(DISABLED.into())
        } else if self.cua != 0 {
            Err(CUA.into())
        } else if self.manual() {
            Err(MANUAL.into())
        } else {
            Ok(())
        }
    }
}
impl InputControl {
    pub fn set_enabled(&self, enabled: bool) {
        let mut s = self.state.lock();
        if s.enabled != enabled {
            s.enabled = enabled;
            if !enabled {
                s.cancel(DISABLED);
                self.wake.notify_one();
            }
        }
    }
    pub fn cancel(&self, reason: &'static str) {
        self.state.lock().cancel(reason);
        self.wake.notify_one();
    }
    pub fn valid(&self, generation: u64) -> bool {
        let s = self.state.lock();
        s.enabled && s.generation == generation && !s.token.is_cancelled()
    }
    pub fn error(&self) -> String {
        self.state.lock().reason.into()
    }
    pub fn activity(&self) -> &'static str {
        let s = self.state.lock();
        if s.manual() {
            "manual"
        } else if s.active {
            "active"
        } else {
            "idle"
        }
    }
    pub fn cursor(&self) -> Option<(i32, i32)> {
        self.state.lock().cursor
    }
    pub fn mcp_buttons(&self) -> u8 {
        self.state.lock().mcp_buttons
    }
    pub(crate) fn set_mcp_buttons(&self, generation: u64, buttons: u8) {
        let mut s = self.state.lock();
        if s.generation == generation {
            s.mcp_buttons = buttons;
        }
    }
    pub(crate) fn sent_cursor(&self, x: i32, y: i32) {
        self.state.lock().cursor = Some((x, y));
    }
    pub async fn acquire(self: &Arc<Self>) -> Result<InputLease, String> {
        let token = {
            let mut s = self.state.lock();
            s.allowed()?;
            if s.token.is_cancelled() {
                s.token = CancellationToken::new();
            }
            s.token.clone()
        };
        let permit = tokio::select! { biased;
            _ = token.cancelled() => return Err(self.error()),
            permit = tokio::time::timeout(Duration::from_secs(1), self.lane.clone().acquire_owned()) => permit,
        }
                .map_err(|_| BUSY.to_string())?
                .map_err(|_| DISABLED.to_string())?;
        let mut s = self.state.lock();
        s.allowed()?;
        if s.token.is_cancelled() {
            s.token = CancellationToken::new();
        }
        s.active = true;
        Ok(InputLease {
            control: self.clone(),
            generation: s.generation,
            token: s.token.clone(),
            permit: Some(permit),
            finished: false,
        })
    }
    pub fn start_cua(self: &Arc<Self>) -> Result<ComputerUseGuard, String> {
        let mut s = self.state.lock();
        if s.active || s.mcp_buttons != 0 {
            return Err("External MCP client is controlling the keyboard and mouse.".into());
        }
        s.cua += 1;
        Ok(ComputerUseGuard(self.clone()))
    }
    pub fn cua_active(&self) -> bool {
        self.state.lock().cua != 0
    }
    pub(crate) fn manual_keyboard(&self, kind: ManualKind, event: &KeyboardEvent) {
        let mut s = self.state.lock();
        s.cancel(PREEMPTED);
        s.last_manual = Some(Instant::now());
        match event.event_type {
            KeyEventType::Down => {
                s.keys.insert((kind, event.key));
            }
            KeyEventType::Up => {
                s.keys.remove(&(kind, event.key));
            }
        }
        if let Some(bit) = event.key.modifier_bit() {
            let mods = s.modifiers.entry(kind).or_default();
            match event.event_type {
                KeyEventType::Down => *mods |= bit,
                KeyEventType::Up => *mods &= !bit,
            }
        } else {
            s.modifiers.insert(kind, event.modifiers.to_hid_byte());
        }
    }
    pub(crate) fn manual_mouse(&self, kind: ManualKind, event: &MouseEvent) {
        let mut s = self.state.lock();
        s.cancel(PREEMPTED);
        s.last_manual = Some(Instant::now());
        if let Some(button) = event.button {
            let buttons = s.buttons.entry(kind).or_default();
            match event.event_type {
                MouseEventType::Down => *buttons |= button.to_hid_bit(),
                MouseEventType::Up => *buttons &= !button.to_hid_bit(),
                _ => {}
            }
        }
    }
    pub(crate) fn manual_consumer(&self) {
        let mut s = self.state.lock();
        s.cancel(PREEMPTED);
        s.last_manual = Some(Instant::now());
    }
    pub(crate) fn reset(&self) {
        let mut s = self.state.lock();
        s.cancel(DISABLED);
        s.keys.clear();
        s.modifiers.clear();
        s.buttons.clear();
    }
    pub(crate) fn end_manual(&self, kind: ManualKind) {
        let mut s = self.state.lock();
        s.keys.retain(|(owner, _)| *owner != kind);
        s.modifiers.remove(&kind);
        s.buttons.remove(&kind);
        // Teardown is not human activity: retain the last real event's timestamp.
    }
}
pub struct ComputerUseGuard(Arc<InputControl>);
impl Drop for ComputerUseGuard {
    fn drop(&mut self) {
        self.0.state.lock().cua -= 1;
    }
}

pub struct InputLease {
    pub control: Arc<InputControl>,
    pub generation: u64,
    pub token: CancellationToken,
    permit: Option<OwnedSemaphorePermit>,
    finished: bool,
}
impl InputLease {
    pub async fn sleep(&self, duration: Duration) -> Result<(), String> {
        tokio::select! { biased;
            _ = self.token.cancelled() => Err(self.control.error()),
            _ = tokio::time::sleep(duration) => Ok(()),
        }
    }
    pub fn finish(mut self) {
        self.finished = true;
    }
}
impl Drop for InputLease {
    fn drop(&mut self) {
        let mut s = self.control.state.lock();
        if s.generation == self.generation {
            s.active = false;
            if !self.finished {
                s.cancel(PREEMPTED);
                self.control.wake.notify_one();
            }
        }
        self.permit.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::{KeyboardModifiers, MouseButton};
    #[tokio::test(start_paused = true)]
    async fn cooldown_requires_all_manual_input_released_and_two_seconds() {
        let c = Arc::new(InputControl::default());
        c.set_enabled(true);
        let lease = c.acquire().await.unwrap();
        c.manual_keyboard(
            ManualKind::WebSocket,
            &KeyboardEvent::key_down(CanonicalKey::KeyA, KeyboardModifiers::default()),
        );
        assert!(lease.token.is_cancelled());
        assert_eq!(c.error(), PREEMPTED);
        drop(lease);
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(c.acquire().await.err().unwrap(), MANUAL);
        c.manual_keyboard(
            ManualKind::WebSocket,
            &KeyboardEvent::key_up(CanonicalKey::KeyA, KeyboardModifiers::default()),
        );
        tokio::time::advance(Duration::from_millis(1999)).await;
        assert_eq!(c.acquire().await.err().unwrap(), MANUAL);
        c.manual_mouse(ManualKind::Uu, &MouseEvent::move_abs(0, 0));
        tokio::time::advance(Duration::from_secs(2)).await;
        c.acquire().await.unwrap().finish();
        c.manual_mouse(ManualKind::Uu, &MouseEvent::button_down(MouseButton::Left));
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(c.acquire().await.err().unwrap(), MANUAL);
    }
    #[tokio::test(start_paused = true)]
    async fn busy_disable_and_cua_are_mutually_exclusive() {
        let c = Arc::new(InputControl::default());
        c.set_enabled(true);
        let lease = c.acquire().await.unwrap();
        assert!(c.start_cua().is_err());
        let wait = c.acquire();
        tokio::pin!(wait);
        assert!(futures::poll!(&mut wait).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(wait.await.err().unwrap(), BUSY);
        c.set_enabled(false);
        assert!(lease.token.is_cancelled());
        assert_eq!(c.error(), DISABLED);
        drop(lease);
        c.set_enabled(true);
        let cua = c.start_cua().unwrap();
        assert_eq!(c.acquire().await.err().unwrap(), CUA);
        drop(cua);
        c.acquire().await.unwrap().finish();
    }
    #[tokio::test(start_paused = true)]
    async fn disconnect_cleanup_does_not_start_a_new_manual_cooldown() {
        let c = Arc::new(InputControl::default());
        c.set_enabled(true);
        c.manual_keyboard(
            ManualKind::Uu,
            &KeyboardEvent::key_down(CanonicalKey::KeyA, KeyboardModifiers::default()),
        );
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(c.acquire().await.err().unwrap(), MANUAL);
        c.end_manual(ManualKind::Uu);
        c.acquire().await.unwrap().finish();
        c.manual_mouse(
            ManualKind::WebSocket,
            &MouseEvent::button_down(MouseButton::Left),
        );
        c.end_manual(ManualKind::Uu);
        assert_eq!(c.acquire().await.err().unwrap(), MANUAL);
    }
}
