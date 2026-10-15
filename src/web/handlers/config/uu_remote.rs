use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use typeshare::typeshare;

use crate::config::{UuRemoteConfig, UuRemoteSystem};
use crate::error::{AppError, Result};
use crate::runtime::try_apply_lock;
use crate::uu_remote::{UuLoginState, UuRemoteStatus, UuVideoOutput};
use crate::web::state::RemoteAccessApiState;

#[typeshare]
#[derive(Debug, Serialize)]
pub struct UuRemoteStatusResponse {
    pub config: UuRemoteConfig,
    pub status: UuRemoteStatus,
    pub video: Option<UuVideoOutput>,
}

#[typeshare]
#[derive(Debug, Deserialize)]
pub struct UuRemoteConfigUpdate {
    pub enabled: Option<bool>,
    pub system: Option<UuRemoteSystem>,
    pub device_name: Option<String>,
}

async fn status_response(state: &RemoteAccessApiState) -> UuRemoteStatusResponse {
    UuRemoteStatusResponse {
        config: state.config.get().uu_remote.clone(),
        status: state.coordinator.uu_remote_status().await,
        video: state.coordinator.uu_remote().video_output().await,
    }
}

pub async fn get_uu_remote_status(
    State(state): State<RemoteAccessApiState>,
) -> Json<UuRemoteStatusResponse> {
    Json(status_response(&state).await)
}

pub async fn update_uu_remote_config(
    State(state): State<RemoteAccessApiState>,
    Json(req): Json<UuRemoteConfigUpdate>,
) -> Result<Json<UuRemoteStatusResponse>> {
    let _apply_guard = try_apply_lock(&state.uu_remote_apply_lock, "uu_remote")?;
    let mut new_config = state.config.get().uu_remote.clone();
    if let Some(enabled) = req.enabled {
        new_config.enabled = enabled;
    }
    if let Some(system) = req.system {
        new_config.system = system;
    }
    if let Some(name) = req.device_name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            return Err(AppError::BadRequest(
                "Device name must contain 1–64 characters without control characters".into(),
            ));
        }
        new_config.device_name = name.to_string();
    }
    // Validate and apply before persisting so a codec conflict leaves the stored config intact.
    state.coordinator.apply_uu_remote(&new_config).await?;
    state
        .config
        .update(|config| config.uu_remote = new_config.clone())
        .await?;
    Ok(Json(status_response(&state).await))
}

pub async fn get_uu_remote_login(State(state): State<RemoteAccessApiState>) -> Json<UuLoginState> {
    Json(state.coordinator.uu_remote().login_state())
}

pub async fn start_uu_remote_login(
    State(state): State<RemoteAccessApiState>,
) -> Json<UuLoginState> {
    Json(state.coordinator.uu_remote().begin_login())
}

pub async fn cancel_uu_remote_login(
    State(state): State<RemoteAccessApiState>,
) -> Json<UuLoginState> {
    let service = state.coordinator.uu_remote();
    service.cancel_login().await;
    Json(service.login_state())
}

pub async fn logout_uu_remote(
    State(state): State<RemoteAccessApiState>,
) -> Result<Json<UuRemoteStatusResponse>> {
    state
        .coordinator
        .uu_remote()
        .logout()
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(status_response(&state).await))
}
