use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use alsa::pcm::{Access, Format, Frames, HwParams, State, IO};
use alsa::{Direction, ValueOr, PCM};
use bytes::Bytes;
use tokio::sync::{broadcast, watch};
use tracing::debug;

use super::{AudioConfig, AudioFrame, CaptureState};
use crate::error::{AppError, Result};
use crate::utils::LogThrottler;
use crate::warn_throttled;

const RETRY_DELAY: Duration = Duration::from_millis(5);
const MAX_CONSECUTIVE_READ_ERRORS: u32 = 10;

pub(super) fn run_capture(
    config: &AudioConfig,
    state: &watch::Sender<CaptureState>,
    frame_tx: &broadcast::Sender<AudioFrame>,
    stop_flag: &AtomicBool,
    log_throttler: &LogThrottler,
) -> Result<()> {
    #[cfg(feature = "vim4")]
    let hdmi = crate::audio::hdmi_linux::HdmiSource::open(&config.device_name)?;
    #[cfg(feature = "vim4")]
    let format = hdmi.as_ref().map(|source| source.snapshot()).transpose()?;
    #[cfg(feature = "vim4")]
    if format.is_some_and(|format| !format.ready()) {
        return Err(AppError::AudioSourceChanged);
    }
    #[cfg(feature = "vim4")]
    if let Some(source) = &hdmi {
        // Controls can lag a physical link transition by one receiver-worker
        // tick. Require the same generation after that qualification window.
        for _ in 0..40 {
            if stop_flag.load(Ordering::Acquire) {
                return Ok(());
            }
            thread::sleep(RETRY_DELAY);
        }
        if !format.unwrap().can_open_after(source.snapshot()?) {
            return Err(AppError::AudioSourceChanged);
        }
    }
    let device_name = &config.device_name;
    #[cfg(feature = "vim4")]
    let device_name = hdmi.as_ref().map_or(device_name, |source| &source.pcm_name);
    #[cfg(feature = "vim4")]
    let hdmi_config = hdmi
        .as_ref()
        .map(|_| format.unwrap().capture_config(config));
    #[cfg(feature = "vim4")]
    let mut resampler = if let Some(native) = &hdmi_config {
        crate::audio::hdmi_resampler::HdmiResampler::new(native.sample_rate, config.sample_rate)?
    } else {
        None
    };
    #[cfg(feature = "vim4")]
    let output_rate = config.sample_rate;
    #[cfg(feature = "vim4")]
    let config = hdmi_config.as_ref().unwrap_or(config);
    // Non-blocking mode guarantees that stop() can always join the worker.
    let pcm = PCM::new(device_name, Direction::Capture, true).map_err(|error| {
        AppError::AudioError(format!(
            "Failed to open audio device {}: {}",
            config.device_name, error
        ))
    })?;

    #[cfg(feature = "vim4")]
    let native_hdmi = hdmi.is_some();
    #[cfg(not(feature = "vim4"))]
    let native_hdmi = false;
    configure_pcm(&pcm, config, native_hdmi)?;
    pcm.prepare()
        .map_err(|error| pcm_setup_error(error, native_hdmi))?;
    let _ = state.send(CaptureState::Running);

    let period_frames = pcm
        .hw_params_current()
        .ok()
        .and_then(|params| params.get_period_size().ok())
        .map(|frames| frames as usize)
        .unwrap_or(config.period_frames as usize)
        .max(256);
    let mut buffer = vec![0u8; period_frames * config.channels as usize * 2];
    let io: IO<u8> = pcm.io_bytes();
    let mut consecutive_errors = 0;
    let mut native_stats = if config.channels == 2 {
        crate::audio::diagnostics::PcmStats::new("native", config.sample_rate)
    } else {
        None
    };
    if let (Some(stats), Ok(params)) = (&mut native_stats, pcm.hw_params_current()) {
        if let (Ok(period), Ok(buffer)) = (params.get_period_size(), params.get_buffer_size()) {
            stats.set_pcm_geometry(period as u64, buffer as u64);
        }
    }

    while !stop_flag.load(Ordering::Acquire) {
        if let Some(stats) = &mut native_stats {
            stats.tick();
        }
        // Check before XRUN recovery too: the old native clock must never be
        // restarted after a source change. The controller opens a fresh native PCM.
        #[cfg(feature = "vim4")]
        if let Some(source) = &hdmi {
            if Some(source.snapshot()?) != format {
                return Err(AppError::AudioSourceChanged);
            }
        }
        match pcm.state() {
            State::XRun => {
                if let Some(stats) = &mut native_stats {
                    stats.xruns += 1;
                }
                warn_throttled!(log_throttler, "xrun", "Audio buffer overrun, recovering");
                pcm.prepare()
                    .map_err(|error| pcm_setup_error(error, native_hdmi))?;
                consecutive_errors = 0;
                continue;
            }
            State::Suspended => {
                warn_throttled!(
                    log_throttler,
                    "suspended",
                    "Audio device suspended, recovering"
                );
                if pcm.resume().is_err() {
                    pcm.prepare()
                        .map_err(|error| pcm_setup_error(error, native_hdmi))?;
                }
                consecutive_errors = 0;
                continue;
            }
            _ => {}
        }

        match io.readi(&mut buffer) {
            Ok(0) => thread::sleep(RETRY_DELAY),
            Ok(frames_read) => {
                consecutive_errors = 0;
                let byte_count = frames_read * config.channels as usize * 2;
                if let Some(stats) = &mut native_stats {
                    stats.observe(&buffer[..byte_count]);
                }
                if frame_tx.receiver_count() > 0 {
                    #[cfg(feature = "vim4")]
                    let converted = resampler
                        .as_mut()
                        .map(|resampler| resampler.convert(&buffer[..byte_count]))
                        .transpose()?;
                    #[cfg(feature = "vim4")]
                    let (data, rate) = (
                        converted.as_deref().unwrap_or(&buffer[..byte_count]),
                        output_rate,
                    );
                    #[cfg(not(feature = "vim4"))]
                    let (data, rate) = (&buffer[..byte_count], config.sample_rate);
                    if data.is_empty() {
                        continue;
                    }
                    let frame = AudioFrame::new_interleaved(
                        Bytes::copy_from_slice(data),
                        config.channels,
                        rate,
                    );
                    let _ = frame_tx.send(frame);
                }
            }
            Err(error) if error.errno() == libc::EAGAIN => {
                if let Some(stats) = &mut native_stats {
                    stats.eagain += 1;
                }
                thread::sleep(RETRY_DELAY);
            }
            Err(error) if native_hdmi && matches!(error.errno(), libc::ENOLINK | libc::ERANGE) => {
                return Err(AppError::AudioSourceChanged);
            }
            Err(error) if is_device_lost_errno(error.errno()) => {
                return Err(AppError::AudioError(format!(
                    "Audio device lost while reading {}: {}",
                    config.device_name, error
                )));
            }
            Err(error) if error.errno() == libc::EPIPE => {
                if let Some(stats) = &mut native_stats {
                    stats.xruns += 1;
                }
                warn_throttled!(log_throttler, "buffer_overrun", "Audio buffer overrun");
                pcm.prepare()
                    .map_err(|prepare_error| pcm_setup_error(prepare_error, native_hdmi))?;
                consecutive_errors = 0;
            }
            Err(error) => {
                consecutive_errors += 1;
                warn_throttled!(log_throttler, "read_error", "Audio read error: {}", error);
                if consecutive_errors >= MAX_CONSECUTIVE_READ_ERRORS {
                    return Err(AppError::AudioError(format!(
                        "Audio capture failed {consecutive_errors} times consecutively: {error}"
                    )));
                }
                thread::sleep(RETRY_DELAY);
            }
        }
    }

    debug!("ALSA capture worker stopped");
    Ok(())
}

fn pcm_setup_error(error: alsa::Error, native_hdmi: bool) -> AppError {
    // These errors can race the controls snapshot: let serialized recovery
    // requalify and reopen, preserving real I/O/configuration errors below.
    if native_hdmi && matches!(error.errno(), libc::ENOLINK | libc::EAGAIN | libc::ERANGE) {
        AppError::AudioSourceChanged
    } else {
        AppError::AudioError(format!("Failed to configure audio PCM: {error}"))
    }
}

fn configure_pcm(pcm: &PCM, config: &AudioConfig, native_hdmi: bool) -> Result<()> {
    let params = HwParams::any(pcm)
        .map_err(|error| AppError::AudioError(format!("Failed to get HwParams: {error}")))?;
    params
        .set_channels(config.channels)
        .and_then(|_| params.set_rate(config.sample_rate, ValueOr::Nearest))
        .and_then(|_| params.set_format(Format::s16()))
        .and_then(|_| params.set_access(Access::RWInterleaved))
        .and_then(|_| params.set_buffer_size_near(config.buffer_frames as Frames))
        .and_then(|_| params.set_period_size_near(config.period_frames as Frames, ValueOr::Nearest))
        .and_then(|_| pcm.hw_params(&params))
        .map_err(|error| pcm_setup_error(error, native_hdmi))?;

    let actual = pcm
        .hw_params_current()
        .map_err(|error| AppError::AudioError(format!("Failed to read PCM parameters: {error}")))?;
    let actual_rate = actual
        .get_rate()
        .map_err(|error| AppError::AudioError(format!("Failed to read sample rate: {error}")))?;
    let actual_channels = actual
        .get_channels()
        .map_err(|error| AppError::AudioError(format!("Failed to read channels: {error}")))?;
    if actual_rate != config.sample_rate || actual_channels != config.channels {
        return Err(AppError::AudioError(format!(
            "Audio device negotiated {actual_rate} Hz/{actual_channels} ch; expected {} Hz/{} ch",
            config.sample_rate, config.channels
        )));
    }
    Ok(())
}

fn is_device_lost_errno(errno: i32) -> bool {
    matches!(errno, libc::ENODEV | libc::ENXIO | libc::ESHUTDOWN)
}
