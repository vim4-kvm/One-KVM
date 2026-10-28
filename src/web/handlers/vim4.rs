//! Board controls use the same privileged helpers and persistent settings as the manager.
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

static BOARD_CHANGE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize)]
pub struct BoardVideoStatus {
    edid: String,
    hdr_eotf: String,
    white: u32,
    peak: u32,
    hdr_remote_available: bool,
    hevc_quality_available: bool,
}

pub async fn status() -> Result<Json<BoardVideoStatus>> {
    // The helper owns root-only EDID state; permission errors must not look like the default preset.
    let edid = run_helper("/usr/libexec/vim4-edid", &["status".into()]).await?;
    let hdr_eotf = if crate::platform::vim4::VideoBackend::current()
        == crate::platform::vim4::VideoBackend::Wave5
    {
        let metadata = run_helper(
            "/usr/libexec/vim4-manager-helper",
            &["mainline-hardware".into()],
        )
        .await?;
        let value: serde_json::Value = serde_json::from_str(&metadata)
            .map_err(|error| AppError::Internal(format!("Invalid HDMI status: {error}")))?;
        value
            .get("hdr")
            .and_then(|value| value.as_str())
            .unwrap_or("Unknown")
            .to_string()
    } else {
        std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/hdmi_hdr_status")
            .unwrap_or_else(|_| "Unknown".into())
            .trim()
            .to_string()
    };
    let config = std::fs::read_to_string("/etc/default/vim4-kvm-hdr").unwrap_or_default();
    let nits = |key, default| {
        config
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once('=')?;
                if name.trim() == key {
                    value.trim().parse::<u32>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(default)
    };
    Ok(Json(BoardVideoStatus {
        edid,
        hdr_eotf,
        white: nits("HDR_SDR_WHITE", 203),
        peak: nits("HDR_SDR_PEAK", 1000),
        hdr_remote_available: crate::platform::vim4::hdr_remote_available(),
        hevc_quality_available: crate::platform::vim4::VideoBackend::current()
            == crate::platform::vim4::VideoBackend::Vendor,
    }))
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoardVideoChange {
    Edid { preset: String },
    Hdr { white: u32, peak: u32 },
}

impl BoardVideoChange {
    fn command(&self) -> Result<(&'static str, Vec<String>)> {
        match self {
            Self::Edid { preset }
                if [
                    "VIM4-4K60",
                    "VIM4-1080P60",
                    "VIM4-1080P60-SDR8",
                    "RTK-CONTROL",
                    "RTK-1080P120",
                ]
                .contains(&preset.as_str()) =>
            {
                Ok((
                    "/usr/libexec/vim4-edid",
                    vec!["preset".into(), preset.clone()],
                ))
            }
            Self::Hdr { white, peak }
                if (160..=1000).contains(white) && *peak > *white && *peak <= 10000 =>
            {
                Ok((
                    "/usr/libexec/vim4-manager-helper",
                    vec!["hdr-curve".into(), white.to_string(), peak.to_string()],
                ))
            }
            _ => Err(AppError::BadRequest(
                "Invalid EDID preset or HDR luminance range".into(),
            )),
        }
    }
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Json(change): Json<BoardVideoChange>,
) -> Result<Json<BoardVideoStatus>> {
    let (helper, args) = change.command()?;
    let _guard = BOARD_CHANGE
        .try_lock()
        .map_err(|_| AppError::BadRequest("A board video change is already in progress".into()))?;
    if crate::platform::vim4::VideoBackend::current() == crate::platform::vim4::VideoBackend::Wave5
        && matches!(change, BoardVideoChange::Edid { .. })
    {
        let _video = state.config_apply_locks.video.try_lock().map_err(|_| {
            AppError::BadRequest("A video configuration change is already in progress".into())
        })?;
        state
            .stream_manager
            .with_capture_released(async {
                let audio = state.audio.is_streaming().await;
                if audio {
                    state.audio.stop_streaming().await?;
                }
                let result = run_helper(helper, &args).await.map(|_| ());
                if result.is_ok() {
                    for _ in 0..50 {
                        if crate::platform::vim4::video_backend::wave5_input_status()
                            .width
                            .is_some()
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
                if audio {
                    if let Err(error) = state.audio.start_streaming().await {
                        tracing::warn!(%error, "Audio is recovering after HDMI change");
                    }
                    state.stream_manager.reconnect_webrtc_audio_sources().await;
                }
                result
            })
            .await?;
    } else {
        run_helper(helper, &args).await?;
    }
    status().await
}

pub(crate) async fn run_helper(helper: &str, args: &[String]) -> Result<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new("sudo")
            .args(["-n", helper])
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        AppError::Internal(
            "Board video update timed out; refresh its status before retrying".into(),
        )
    })?
    .map_err(|e| AppError::Internal(format!("Cannot run board video helper: {e}")))?;
    if !output.status.success() {
        tracing::warn!(helper, error = %String::from_utf8_lossy(&output.stderr), "Board video update failed");
        return Err(AppError::Internal(
            "Board video update failed; check the service log".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn board_changes_reject_invalid_ranges_before_running_a_helper() {
        for (white, peak) in [(159, 1000), (1001, 4000), (203, 203), (203, 10001)] {
            assert!(BoardVideoChange::Hdr { white, peak }.command().is_err());
        }
        assert!(BoardVideoChange::Hdr {
            white: 203,
            peak: 1000
        }
        .command()
        .is_ok());
        assert!(BoardVideoChange::Edid {
            preset: "RTK-CONTROL".into()
        }
        .command()
        .is_ok());
        assert!(BoardVideoChange::Edid {
            preset: "../../etc/passwd".into()
        }
        .command()
        .is_err());
    }
}
