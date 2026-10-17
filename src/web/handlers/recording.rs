use crate::{
    error::Result,
    recording::{RecordingFile, RecordingStatus},
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
use tokio::io::AsyncReadExt;

pub async fn status(State(state): State<Arc<AppState>>) -> Json<RecordingStatus> {
    Json(state.recording.status().await)
}
pub async fn start(State(state): State<Arc<AppState>>) -> Result<Json<RecordingStatus>> {
    Ok(Json(
        state
            .recording
            .start(state.stream_manager.clone(), state.audio.clone())
            .await?,
    ))
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
) -> Result<StatusCode> {
    state.recording.delete(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn download(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Response> {
    let file = state.recording.open(&name).await?;
    let size = file.metadata()?.len();
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
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}
