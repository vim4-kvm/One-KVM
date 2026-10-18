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
pub async fn config(
    State(state): State<Arc<AppState>>,
    Json(update): Json<McpConfigUpdate>,
) -> Result<Response, AppError> {
    let _guard = state.mcp.mutation.lock().await;
    if let Some(enabled) = update.enabled {
        if enabled {
            state
                .mcp
                .credentials
                .lock()
                .ensure()
                .map_err(AppError::Internal)?;
        }
        state
            .config
            .update(|config| config.mcp.enabled = enabled)
            .await?;
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
