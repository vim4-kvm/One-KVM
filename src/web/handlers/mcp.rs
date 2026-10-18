use crate::{
    error::AppError,
    mcp::types::{McpConfigUpdate, McpKey},
    state::AppState,
};
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
fn no_store(value: impl serde::Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}
pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    no_store(state.mcp.status(&state.snapshot).await)
}
pub async fn clear_activity(State(state): State<Arc<AppState>>) -> Response {
    state.mcp.clear_activity();
    no_store(state.mcp.status(&state.snapshot).await)
}
pub async fn activity_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    match state.mcp.call_detail(id) {
        Some(detail) => no_store(detail),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response(),
    }
}
pub async fn activity_image(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    match state.mcp.call_image(id) {
        Some(image) => (
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::CONTENT_TYPE, "image/jpeg"),
            ],
            image,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response(),
    }
}
pub async fn config(
    State(state): State<Arc<AppState>>,
    Json(update): Json<McpConfigUpdate>,
) -> Result<Response, AppError> {
    let _guard = state.mcp.mutation.lock().await;
    if update
        .screenshot_long_edge
        .is_some_and(|value| !crate::config::valid_screenshot_long_edge(value))
    {
        return Err(AppError::BadRequest(
            "Screenshot long edge must be 1280, 1568, 1920 or 2560.".into(),
        ));
    }
    if let Some(enabled) = update.enabled {
        if enabled {
            state
                .mcp
                .credentials
                .lock()
                .ensure()
                .map_err(AppError::Internal)?;
        }
    }
    state
        .config
        .update(|config| {
            if let Some(value) = update.screenshot_long_edge {
                config.mcp.screenshot_long_edge = value;
            }
            if let Some(enabled) = update.enabled {
                config.mcp.enabled = enabled;
            }
            if let Some(natural) = update.host_natural_scrolling {
                config.mcp.host_natural_scrolling = natural;
            }
        })
        .await?;
    if let Some(enabled) = update.enabled {
        state.mcp.set_enabled(enabled);
        if !enabled {
            state.hid.cancel_mcp().await?;
        }
    }
    Ok(no_store(state.mcp.status(&state.snapshot).await))
}
pub async fn key(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    let _guard = state.mcp.mutation.lock().await;
    let api_key = state
        .mcp
        .credentials
        .lock()
        .ensure()
        .map_err(AppError::Internal)?;
    Ok(no_store(McpKey { api_key }))
}
pub async fn regenerate(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    let _guard = state.mcp.mutation.lock().await;
    let api_key = state
        .mcp
        .credentials
        .lock()
        .regenerate()
        .map_err(AppError::Internal)?;
    state.mcp.cancel_operations();
    state.hid.cancel_mcp().await?;
    Ok(no_store(McpKey { api_key }))
}
