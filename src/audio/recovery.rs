//! Audio device-loss monitoring and serialized recovery.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use super::capture::AudioConfig;
use super::controller::AudioRecoveredCallback;
use super::device::{enumerate_audio_devices, AudioDeviceInfo};
use super::monitor::AudioHealthMonitor;
use super::streamer::{AudioStreamState, AudioStreamer, AudioStreamerConfig};
use super::types::AudioControllerConfig;
use crate::error::AppError;
use crate::events::{EventBus, StreamKind, SystemEvent};

const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryCause {
    DeviceLost,
    SourceChanged,
}

fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs((1u64 << attempt.min(5)).min(30))
}

struct RecoveryControl {
    /// Even values are idle; the following odd value is that recovery's token.
    /// A single compare-exchange therefore owns both activity and generation.
    state: AtomicU64,
}

impl RecoveryControl {
    fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    fn begin(&self) -> Option<u64> {
        let idle = self.state.load(Ordering::Acquire);
        if !idle.is_multiple_of(2) {
            return None;
        }
        let token = idle.wrapping_add(1);
        self.state
            .compare_exchange(idle, token, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| token)
    }

    fn is_current(&self, token: u64) -> bool {
        self.state.load(Ordering::Acquire) == token
    }

    fn finish(&self, token: u64) {
        let _ = self.state.compare_exchange(
            token,
            token.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn cancel(&self) {
        let token = self.state.load(Ordering::Acquire);
        if !token.is_multiple_of(2) {
            let _ = self.state.compare_exchange(
                token,
                token.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

struct RecoveryLease {
    control: Arc<RecoveryControl>,
    generation: u64,
}

impl Drop for RecoveryLease {
    fn drop(&mut self) {
        self.control.finish(self.generation);
    }
}

struct RecoveryInner {
    config: Arc<RwLock<AudioControllerConfig>>,
    streamer: Arc<RwLock<Option<Arc<AudioStreamer>>>>,
    event_bus: Arc<RwLock<Option<Arc<EventBus>>>>,
    monitor: Arc<AudioHealthMonitor>,
    recovered_callback: Arc<RwLock<Option<AudioRecoveredCallback>>>,
    operation: Arc<Mutex<()>>,
    control: Arc<RecoveryControl>,
}

#[derive(Clone)]
pub(super) struct AudioRecovery {
    inner: Arc<RecoveryInner>,
}

impl AudioRecovery {
    pub(super) fn new(
        config: Arc<RwLock<AudioControllerConfig>>,
        streamer: Arc<RwLock<Option<Arc<AudioStreamer>>>>,
        event_bus: Arc<RwLock<Option<Arc<EventBus>>>>,
        monitor: Arc<AudioHealthMonitor>,
        recovered_callback: Arc<RwLock<Option<AudioRecoveredCallback>>>,
        operation: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            inner: Arc::new(RecoveryInner {
                config,
                streamer,
                event_bus,
                monitor,
                recovered_callback,
                operation,
                control: Arc::new(RecoveryControl::new()),
            }),
        }
    }

    pub(super) fn cancel(&self) {
        self.inner.control.cancel();
    }

    pub(super) fn monitor(&self, streamer: Arc<AudioStreamer>, device: String) {
        let recovery = self.clone();
        let mut state = streamer.state_watch();
        tokio::spawn(async move {
            loop {
                let current_state = *state.borrow();
                let cause = match current_state {
                    AudioStreamState::Error => RecoveryCause::DeviceLost,
                    AudioStreamState::SourceChanged => RecoveryCause::SourceChanged,
                    AudioStreamState::Stopped => return,
                    AudioStreamState::Starting | AudioStreamState::Running => {
                        if state.changed().await.is_err() {
                            return;
                        }
                        continue;
                    }
                };

                // Serialize the ownership check with user-driven start/stop
                // operations. If a stop already owns the operation lock, it
                // removes the streamer before this monitor may start recovery.
                let _operation = recovery.inner.operation.lock().await;
                let is_current = recovery
                    .inner
                    .streamer
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &streamer));
                if !is_current {
                    return;
                }

                let reason = match cause {
                    RecoveryCause::DeviceLost => format!("Audio device lost: {device}"),
                    RecoveryCause::SourceChanged => format!("HDMI audio source changed: {device}"),
                };
                if cause == RecoveryCause::DeviceLost {
                    recovery
                        .inner
                        .monitor
                        .report_error(&reason, "device_lost")
                        .await;
                }
                recovery.start_recovery(device, reason, cause);
                return;
            }
        });
    }

    pub(super) fn start(&self, lost_device: String, reason: String) {
        self.start_recovery(lost_device, reason, RecoveryCause::DeviceLost);
    }

    pub(super) fn source_changed(&self, device: String) {
        self.start_recovery(
            device,
            "HDMI audio source changed".into(),
            RecoveryCause::SourceChanged,
        );
    }

    fn start_recovery(&self, lost_device: String, reason: String, cause: RecoveryCause) {
        let Some(generation) = self.inner.control.begin() else {
            debug!("Audio recovery already in progress");
            return;
        };
        let recovery = self.clone();
        tokio::spawn(async move {
            let _lease = RecoveryLease {
                control: recovery.inner.control.clone(),
                generation,
            };
            recovery.run(generation, lost_device, reason, cause).await;
        });
    }

    async fn run(
        &self,
        generation: u64,
        lost_device: String,
        reason: String,
        mut cause: RecoveryCause,
    ) {
        if cause == RecoveryCause::DeviceLost {
            warn!("Audio recovery started for {lost_device}: {reason}");
            self.publish_device_lost(&lost_device, &reason).await;
        } else {
            info!("Waiting for HDMI audio source to stabilize: {lost_device}");
        }

        let mut attempt = 0u32;
        while self.inner.control.is_current(generation) {
            let config = self.inner.config.read().await.clone();
            if !config.enabled {
                return;
            }
            if self
                .inner
                .streamer
                .read()
                .await
                .as_ref()
                .is_some_and(|streamer| streamer.is_running())
            {
                return;
            }

            // Poll the receiver controls without opening a PCM or repeatedly
            // enumerating cards while HDMI is absent/unsupported. Reconnect as
            // soon as its format is stable, independent of the failure backoff.
            #[cfg(all(target_os = "linux", feature = "vim4"))]
            match super::hdmi_linux::source_ready(&config.device) {
                Ok(false) => {
                    attempt = 0;
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }
                Err(error) => {
                    debug!("Cannot query HDMI audio source: {error}");
                    if self
                        .handle_start_failure(generation, &mut cause, &lost_device, &error)
                        .await
                    {
                        attempt = 0;
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        continue;
                    }
                }
                Ok(true) => {}
            }
            let delay = if cause == RecoveryCause::SourceChanged && attempt == 0 {
                std::time::Duration::ZERO
            } else {
                retry_delay(attempt)
            };
            attempt = attempt.saturating_add(1);
            if cause == RecoveryCause::DeviceLost {
                self.publish_reconnecting(&lost_device, attempt).await;
                self.publish_state(
                    "device_lost",
                    Some(lost_device.clone()),
                    Some("audio_reconnecting"),
                    Some(delay.as_millis() as u64),
                )
                .await;
            }
            tokio::time::sleep(delay).await;
            if !self.inner.control.is_current(generation) {
                return;
            }

            // Backoff and card enumeration are not a lease on the HDMI link.
            // Do not open a PCM using a readiness sample taken before sleep.
            #[cfg(all(target_os = "linux", feature = "vim4"))]
            if !super::hdmi_linux::source_ready(&config.device).unwrap_or(false) {
                continue;
            }

            // A format/link transition must reopen the same input, rather
            // than probing busy PCMs or selecting another microphone.
            let device_name = if cause == RecoveryCause::SourceChanged {
                config.device.clone()
            } else {
                let devices = match enumerate_audio_devices() {
                    Ok(devices) => devices,
                    Err(error) => {
                        debug!("Audio recovery enumeration attempt {attempt} failed: {error}");
                        continue;
                    }
                };
                let Some(device) = select_recovery_device(&devices, &config.device) else {
                    debug!("No audio device found on recovery attempt {attempt}");
                    continue;
                };
                device.name
            };
            let streamer = Arc::new(AudioStreamer::with_config(AudioStreamerConfig {
                capture: AudioConfig {
                    device_name: device_name.clone(),
                    ..Default::default()
                },
                opus: config.quality.to_opus_config(),
            }));

            if let Err(error) = streamer.start().await {
                debug!(
                    "Audio recovery attempt {attempt} failed with {}: {error}",
                    device_name
                );
                let was_source_change = cause == RecoveryCause::SourceChanged;
                if self
                    .handle_start_failure(generation, &mut cause, &device_name, &error)
                    .await
                {
                    attempt = 0;
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                } else if was_source_change && cause == RecoveryCause::DeviceLost {
                    // The quiet reopen was not a failed device retry. Start
                    // genuine failure backoff at its ordinary first second.
                    attempt = 0;
                }
                continue;
            }

            // Commit a recovered streamer under the same operation lock used by
            // user-driven start/stop/config updates. Cancellation is rechecked
            // after acquiring the lock so an old task cannot resurrect itself.
            let _operation = self.inner.operation.lock().await;
            if !self.inner.control.is_current(generation) || !self.inner.config.read().await.enabled
            {
                let _ = streamer.stop().await;
                return;
            }

            self.inner.config.write().await.device = device_name.clone();
            *self.inner.streamer.write().await = Some(streamer.clone());
            self.inner.monitor.report_recovered().await;
            self.publish_recovered(&device_name).await;
            if let Some(callback) = self.inner.recovered_callback.read().await.clone() {
                callback();
            }
            self.publish_state("streaming", Some(device_name.clone()), None, None)
                .await;
            info!(
                "Audio recovered with {} after {} attempts",
                device_name, attempt
            );
            self.inner.control.finish(generation);
            self.monitor(streamer, device_name);
            drop(_operation);
            return;
        }
    }

    async fn handle_start_failure(
        &self,
        generation: u64,
        cause: &mut RecoveryCause,
        device: &str,
        error: &AppError,
    ) -> bool {
        if matches!(error, AppError::AudioSourceChanged) {
            return true;
        }
        if *cause == RecoveryCause::SourceChanged {
            let _operation = self.inner.operation.lock().await;
            if !self.inner.control.is_current(generation) || !self.inner.config.read().await.enabled
            {
                return false;
            }
            // Do not hide a real PCM/configuration failure behind the quiet
            // source-transition path. Restore ordinary error reporting/backoff.
            *cause = RecoveryCause::DeviceLost;
            let reason = format!("Audio capture failed for {device}: {error}");
            self.inner
                .monitor
                .report_error(&reason, "capture_error")
                .await;
            self.publish_device_lost(device, &reason).await;
        }
        false
    }

    async fn publish_state(
        &self,
        state: &str,
        device: Option<String>,
        reason: Option<&str>,
        next_retry_ms: Option<u64>,
    ) {
        if let Some(bus) = self.inner.event_bus.read().await.as_ref() {
            bus.publish(SystemEvent::StreamStateChanged {
                kind: StreamKind::Audio,
                state: state.to_string(),
                device,
                reason: reason.map(str::to_string),
                next_retry_ms,
            });
            bus.mark_device_info_dirty();
        }
    }

    async fn publish_device_lost(&self, device: &str, reason: &str) {
        if let Some(bus) = self.inner.event_bus.read().await.as_ref() {
            bus.publish(SystemEvent::StreamDeviceLost {
                kind: StreamKind::Audio,
                device: device.to_string(),
                reason: reason.to_string(),
            });
        }
        self.publish_state(
            "device_lost",
            Some(device.to_string()),
            Some("audio_device_lost"),
            Some(RETRY_DELAY.as_millis() as u64),
        )
        .await;
    }

    async fn publish_reconnecting(&self, device: &str, attempt: u32) {
        if let Some(bus) = self.inner.event_bus.read().await.as_ref() {
            bus.publish(SystemEvent::StreamReconnecting {
                device: device.to_string(),
                attempt,
            });
        }
    }

    async fn publish_recovered(&self, device: &str) {
        if let Some(bus) = self.inner.event_bus.read().await.as_ref() {
            bus.publish(SystemEvent::StreamRecovered {
                device: device.to_string(),
            });
        }
    }
}

pub(super) fn select_recovery_device(
    devices: &[AudioDeviceInfo],
    preferred: &str,
) -> Option<AudioDeviceInfo> {
    devices
        .iter()
        .find(|device| !preferred.trim().is_empty() && device.matches_name(preferred))
        .or_else(|| {
            devices.iter().find(|device| {
                device.is_hdmi
                    && device.sample_rates.contains(&48_000)
                    && device.channels.contains(&2)
            })
        })
        .or_else(|| {
            devices.iter().find(|device| {
                device.sample_rates.contains(&48_000) && device.channels.contains(&2)
            })
        })
        .or_else(|| devices.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recovery(enabled: bool) -> AudioRecovery {
        AudioRecovery::new(
            Arc::new(RwLock::new(AudioControllerConfig {
                enabled,
                device: "hw:VIM4HDMIRX,0".into(),
                ..Default::default()
            })),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(Some(Arc::new(EventBus::new())))),
            Arc::new(AudioHealthMonitor::new()),
            Arc::new(RwLock::new(None)),
            Arc::new(Mutex::new(())),
        )
    }

    #[tokio::test]
    async fn repeated_source_changes_do_not_publish_device_loss_or_health_error() {
        // Disabled configuration bounds the worker before any hardware opens.
        // Exercise the actual monitor/ownership/start path for repeated changes.
        let recovery = recovery(false);
        let mut events = recovery
            .inner
            .event_bus
            .read()
            .await
            .as_ref()
            .unwrap()
            .subscribe();
        for expected_token in [2, 4] {
            let streamer = Arc::new(AudioStreamer::new());
            streamer.set_test_state(AudioStreamState::SourceChanged);
            *recovery.inner.streamer.write().await = Some(streamer.clone());
            recovery.monitor(streamer, "hw:VIM4HDMIRX,0".into());
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while recovery.inner.control.state.load(Ordering::Acquire) != expected_token {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!recovery.inner.monitor.is_error().await);
            assert!(matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
            assert_eq!(recovery.inner.config.read().await.device, "hw:VIM4HDMIRX,0");
        }
    }

    #[tokio::test]
    async fn failed_source_reopen_escalates_real_failure_but_not_another_source_change() {
        let recovery = recovery(true);
        let mut events = recovery
            .inner
            .event_bus
            .read()
            .await
            .as_ref()
            .unwrap()
            .subscribe();
        let token = recovery.inner.control.begin().unwrap();
        let mut cause = RecoveryCause::SourceChanged;
        assert!(
            recovery
                .handle_start_failure(
                    token,
                    &mut cause,
                    "hw:VIM4HDMIRX,0",
                    &AppError::AudioSourceChanged
                )
                .await
        );
        assert!(!recovery.inner.monitor.is_error().await);
        assert!(events.try_recv().is_err());
        assert!(
            !recovery
                .handle_start_failure(
                    token,
                    &mut cause,
                    "hw:VIM4HDMIRX,0",
                    &AppError::AudioError("PCM permission denied".into())
                )
                .await
        );
        assert_eq!(cause, RecoveryCause::DeviceLost);
        assert!(recovery.inner.monitor.is_error().await);
        assert!(matches!(
            events.try_recv().unwrap(),
            SystemEvent::StreamDeviceLost {
                kind: StreamKind::Audio,
                ..
            }
        ));
        assert!(matches!(
            events.try_recv().unwrap(),
            SystemEvent::StreamStateChanged {
                kind: StreamKind::Audio,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn cancelled_source_reopen_cannot_publish_a_late_device_failure() {
        let recovery = recovery(true);
        let mut events = recovery
            .inner
            .event_bus
            .read()
            .await
            .as_ref()
            .unwrap()
            .subscribe();
        let token = recovery.inner.control.begin().unwrap();
        recovery.cancel();
        let mut cause = RecoveryCause::SourceChanged;
        recovery
            .handle_start_failure(
                token,
                &mut cause,
                "hw:VIM4HDMIRX,0",
                &AppError::AudioError("late PCM failure".into()),
            )
            .await;
        assert!(!recovery.inner.monitor.is_error().await);
        assert!(events.try_recv().is_err());
    }

    #[cfg(all(target_os = "linux", feature = "vim4"))]
    #[tokio::test(start_paused = true)]
    async fn source_recovery_worker_reports_a_missing_control_device_as_real_failure() {
        let recovery = recovery(true);
        recovery.inner.config.write().await.device = "hw:__one_kvm_missing_card__,0".into();
        let mut events = recovery
            .inner
            .event_bus
            .read()
            .await
            .as_ref()
            .unwrap()
            .subscribe();
        recovery.source_changed("hw:__one_kvm_missing_card__,0".into());
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            SystemEvent::StreamDeviceLost {
                kind: StreamKind::Audio,
                ..
            }
        ));
        assert!(recovery.inner.monitor.is_error().await);
        // Exercise the real worker after escalation, including its first two
        // retries. No sound device exists or is opened by this test.
        assert!(matches!(
            events.recv().await.unwrap(),
            SystemEvent::StreamStateChanged { .. }
        ));
        for (attempt, next_retry_ms) in [(1, 1000), (2, 2000)] {
            assert!(matches!(events.recv().await.unwrap(),
                SystemEvent::StreamReconnecting { attempt: actual, .. } if actual == attempt));
            assert!(matches!(events.recv().await.unwrap(),
                SystemEvent::StreamStateChanged { next_retry_ms: Some(actual), .. }
                if actual == next_retry_ms));
            tokio::time::advance(std::time::Duration::from_millis(next_retry_ms)).await;
        }
        recovery.cancel();
    }

    fn device(name: &str, compatible: bool, hdmi: bool) -> AudioDeviceInfo {
        AudioDeviceInfo {
            name: name.to_string(),
            description: name.to_string(),
            card_index: 0,
            device_index: 0,
            sample_rates: if compatible {
                vec![48_000]
            } else {
                vec![44_100]
            },
            channels: vec![2],
            is_capture: true,
            is_hdmi: hdmi,
            usb_bus: None,
        }
    }

    #[test]
    fn stale_recovery_cannot_finish_a_new_generation() {
        let control = RecoveryControl::new();
        let stale = control.begin().unwrap();
        control.cancel();
        let current = control.begin().unwrap();

        control.finish(stale);
        assert!(control.is_current(current));
    }

    #[test]
    fn completed_recovery_cannot_finish_the_next_recovery() {
        let control = RecoveryControl::new();
        let completed = control.begin().unwrap();
        control.finish(completed);
        let current = control.begin().unwrap();

        control.finish(completed);
        assert!(control.is_current(current));
    }

    #[test]
    fn repeated_failure_backoff_is_bounded_even_after_counter_saturation() {
        let delays: Vec<_> = (0..8)
            .map(|attempt| retry_delay(attempt).as_secs())
            .collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_delay(u32::MAX).as_secs(), 30);
    }

    #[test]
    fn recovery_prefers_requested_then_compatible_hdmi() {
        let devices = vec![device("fallback", true, false), device("hdmi", true, true)];
        assert_eq!(
            select_recovery_device(&devices, "fallback").unwrap().name,
            "fallback"
        );
        assert_eq!(
            select_recovery_device(&devices, "missing").unwrap().name,
            "hdmi"
        );
    }

    #[test]
    fn legacy_numeric_preference_stays_on_its_usb_card_instead_of_hdmi() {
        let usb = device("hw:USBMic,0", true, false);
        let mut hdmi = device("hw:VIM4HDMIRX,0", true, true);
        hdmi.card_index = 1;
        let devices = vec![hdmi, usb];
        assert_eq!(
            select_recovery_device(&devices, "hw:0,0").unwrap().name,
            "hw:USBMic,0"
        );
        assert_eq!(
            select_recovery_device(&devices, "hw:1,0").unwrap().name,
            "hw:VIM4HDMIRX,0"
        );
    }
}
