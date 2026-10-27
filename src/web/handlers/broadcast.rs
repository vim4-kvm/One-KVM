use crate::{
    error::{AppError, Result},
    state::AppState,
};
use axum::{extract::State, Json};
use serde::Deserialize;
use std::sync::Arc;

pub async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = state.config.get();
    let c = &config.broadcast;
    Json(serde_json::json!({
        "config": {
            "srt": { "enabled": c.srt.enabled, "port": c.srt.port, "latency_ms": c.srt.latency_ms, "audio": c.srt.audio, "passphrase_set": !c.srt.passphrase.is_empty() },
            "rtmp": { "enabled": c.rtmp.enabled, "audio": c.rtmp.audio, "url_set": !c.rtmp.url.is_empty() }
        },
        "status": state.broadcast.status().await
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    srt: Option<SrtUpdate>,
    rtmp: Option<RtmpUpdate>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SrtUpdate {
    enabled: Option<bool>,
    port: Option<u16>,
    latency_ms: Option<u32>,
    audio: Option<bool>,
    passphrase: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RtmpUpdate {
    enabled: Option<bool>,
    audio: Option<bool>,
    url: Option<String>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Json(update): Json<Update>,
) -> Result<Json<serde_json::Value>> {
    // Serialize broadcast edits and validate the merged value; unrelated config
    // changes are retained by ConfigStore's update operation.
    let _guard = crate::runtime::try_apply_lock(&state.config_apply_locks.stream, "broadcast")?;
    let mut config = state.config.get().broadcast.clone();
    if let Some(u) = update.srt {
        if let Some(v) = u.enabled {
            config.srt.enabled = v;
        }
        if let Some(v) = u.port {
            config.srt.port = v;
        }
        if let Some(v) = u.latency_ms {
            config.srt.latency_ms = v;
        }
        if let Some(v) = u.audio {
            config.srt.audio = v;
        }
        if let Some(v) = u.passphrase {
            config.srt.passphrase = v;
        }
    }
    if let Some(u) = update.rtmp {
        if let Some(v) = u.enabled {
            config.rtmp.enabled = v;
        }
        if let Some(v) = u.audio {
            config.rtmp.audio = v;
        }
        if let Some(v) = u.url {
            config.rtmp.url = v;
        }
    }
    config
        .validate()
        .map_err(|e| AppError::BadRequest(e.into()))?;
    state.config.update(|c| c.broadcast = config).await?;
    Ok(Json(serde_json::json!({"success": true})))
}
