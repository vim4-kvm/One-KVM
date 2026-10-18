use crate::{
    error::Result,
    recording::{RecordingErrorCode, RecordingStatus},
    state::AppState,
    video::codec::{VideoCodecType, VideoEncoderType},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

pub async fn status(State(state): State<Arc<AppState>>) -> Json<RecordingStatus> {
    let mut status = state.recording.status().await;
    if !status.recording {
        let config = state.stream_manager.get_encoding_config().await;
        let current_codec = state.stream_manager.current_video_codec().await;
        let audio = state.audio.status().await;
        status.blocker = if status.storage_reason.is_some() {
            Some(RecordingErrorCode::StorageUnavailable)
        } else if !state.stream_manager.is_webrtc_enabled().await
            || !matches!(current_codec, VideoCodecType::H264 | VideoCodecType::H265)
            || config.is_some_and(|c| {
                !matches!(
                    c.output_codec,
                    VideoEncoderType::H264 | VideoEncoderType::H265
                )
            })
        {
            Some(RecordingErrorCode::VideoCodec)
        } else if !state.config.get().audio.enabled || !audio.enabled || audio.error.is_some() {
            Some(RecordingErrorCode::AudioUnavailable)
        } else if status
            .storage
            .as_ref()
            .is_some_and(|s| s.free_bytes < s.reserve_bytes)
        {
            Some(RecordingErrorCode::LowStorage)
        } else {
            None
        };
    }
    Json(status)
}
pub async fn start(State(state): State<Arc<AppState>>) -> Response {
    match state
        .recording
        .start(state.stream_manager.clone(), state.audio.clone())
        .await
    {
        Ok(status) => Json(status).into_response(),
        Err(error) => {
            let code = crate::recording::error_code(&error);
            let status = if code == RecordingErrorCode::AlreadyRecording {
                axum::http::StatusCode::CONFLICT
            } else {
                axum::http::StatusCode::BAD_REQUEST
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string(), "error_code": code})),
            )
                .into_response()
        }
    }
}
pub async fn stop(State(state): State<Arc<AppState>>) -> Json<RecordingStatus> {
    Json(state.recording.stop().await)
}
#[derive(Default, serde::Deserialize)]
pub struct FilesQuery {
    #[serde(default)]
    details: bool,
}
pub async fn files(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilesQuery>,
) -> Json<serde_json::Value> {
    let (files, unavailable) = state.recording.files().await;
    // Preserve the legacy array; clients requesting details also receive omissions.
    Json(if query.details {
        serde_json::json!({"files": files, "unavailable": unavailable})
    } else {
        serde_json::json!(files)
    })
}
pub async fn locations(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<crate::recording::locations::Availability>> {
    Json(state.recording.locations().await)
}
#[derive(serde::Deserialize)]
pub struct LocationUpdate {
    location: String,
    folder: Option<String>,
}
pub async fn location(
    State(state): State<Arc<AppState>>,
    Json(update): Json<LocationUpdate>,
) -> Result<Json<serde_json::Value>> {
    state
        .recording
        .set_location(update.location, update.folder)
        .await?;
    Ok(Json(serde_json::json!({"success": true})))
}
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<DownloadQuery>,
) -> Result<Json<serde_json::Value>> {
    state
        .recording
        .delete_at(
            query.location.as_deref().unwrap_or("local"),
            query.folder.as_deref(),
            &name,
        )
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}
#[derive(serde::Deserialize)]
pub struct DownloadQuery {
    filename: Option<String>,
    location: Option<String>,
    folder: Option<String>,
}

/// Browsers name files in the viewer's timezone; the device clock is often UTC.
fn is_download_name(name: &str) -> bool {
    name.strip_prefix("one-kvm-")
        .and_then(|rest| rest.strip_suffix(".mkv"))
        .is_some_and(|stamp| {
            stamp.len() == 15
                && stamp.bytes().enumerate().all(|(i, b)| {
                    if i == 8 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                })
        })
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response> {
    let (file, size, default_name) = state
        .recording
        .open_at(
            query.location.as_deref().unwrap_or("local"),
            query.folder.as_deref(),
            &name,
        )
        .await?;
    let download_name = match query.filename {
        Some(requested) if is_download_name(&requested) => requested,
        _ => default_name,
    };
    let mut file = file;
    let gate = state
        .recording
        .download_gate(query.location.as_deref().unwrap_or("local"));
    let stream = async_stream::try_stream! {
        loop {
            let (returned, data) = crate::recording::locations::bounded(gate.clone(), move || {
                use std::io::Read;
                let mut buffer = vec![0u8; 64 * 1024];
                let result = file.read(&mut buffer).map(|len| { buffer.truncate(len); buffer });
                (file, result)
            }).await.map_err(std::io::Error::other)?;
            file = returned;
            let data = data?;
            if data.is_empty() { break; }
            yield bytes::Bytes::from(data);
        }
    };
    let stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send>,
    > = Box::pin(stream);
    let body = Body::from_stream(stream);
    Ok((
        [
            (header::CONTENT_TYPE, "video/x-matroska".to_string()),
            (header::CONTENT_LENGTH, size.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{download_name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::is_download_name;

    #[test]
    fn requested_download_names_are_timestamp_only() {
        assert!(is_download_name("one-kvm-20261017-201500.mkv"));
        for name in [
            "one-kvm-20261017-201500.mkv\"",
            "one-kvm-2026101-7201500.mkv",
            "../one-kvm-20261017-201500.mkv",
            "one-kvm-20261017-2015000.mkv",
            "x.mkv",
        ] {
            assert!(!is_download_name(name), "{name}");
        }
    }
}
