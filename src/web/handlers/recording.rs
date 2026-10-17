use crate::{
    error::Result,
    recording::{RecordingErrorCode, RecordingFile, RecordingStatus},
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
use tokio::io::AsyncReadExt;

pub async fn status(State(state): State<Arc<AppState>>) -> Json<RecordingStatus> {
    let mut status = state.recording.status().await;
    if !status.recording {
        let config = state.stream_manager.get_encoding_config().await;
        let current_codec = state.stream_manager.current_video_codec().await;
        let audio = state.audio.status().await;
        status.blocker = if !state.stream_manager.is_webrtc_enabled().await
            || !matches!(current_codec, VideoCodecType::H264 | VideoCodecType::H265)
            || config.is_some_and(|c| {
                !matches!(
                    c.output_codec,
                    VideoEncoderType::H264 | VideoEncoderType::H265
                )
            }) {
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
pub async fn files(State(state): State<Arc<AppState>>) -> Result<Json<Vec<RecordingFile>>> {
    Ok(Json(state.recording.files()?))
}
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    state.recording.delete(&name).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}
#[derive(serde::Deserialize)]
pub struct DownloadQuery {
    filename: Option<String>,
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
    let file = state.recording.open(&name).await?;
    let size = file.metadata()?.len();
    let download_name = match query.filename {
        Some(requested) if is_download_name(&requested) => requested,
        _ => state.recording.download_name(&name, &file)?,
    };
    let mut file = tokio::fs::File::from_std(file);
    let stream = async_stream::try_stream! {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let len = file.read(&mut buffer).await?;
            if len == 0 { break; }
            yield bytes::Bytes::copy_from_slice(&buffer[..len]);
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
