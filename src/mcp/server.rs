//! Deliberately bounded JSON-only Streamable HTTP profile. No sessions or SSE.
//!
//! rmcp's Tower transport supports Axum and stateless JSON responses, but its
//! JSON preference permits SSE fallback and tracks a broader protocol surface.
//! This implementation pins the three 2025 versions and the GET/DELETE contract
//! without adding another transport or dependency graph. ARM64 compatibility is
//! validated with the application's locked Rust toolchain and protocol tests.
use crate::state::AppState;
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;
pub const INSTRUCTIONS: &str = "This server controls a physical computer through a hardware KVM (HDMI capture + USB keyboard/mouse).\nCall screenshot first. All coordinates are pixels in the screenshot's coordinate space (shown in its text);\nuse zoom to read small text, coordinates stay in the full screenshot space.\nEnd each group of actions with a screenshot to verify the result.\nA person may take over at any time by using the keyboard or mouse; your current action is then cancelled\nand input tools fail for about 2 seconds after they stop. Wait, take a new screenshot, and continue.\nTyping uses a US keyboard layout; characters that cannot be typed are skipped and reported.\nTyping sends US-layout key presses through the host's active input method. If a CJK input method (for example Chinese Pinyin) is active, letters and punctuation may be converted or swallowed: switch the host to an English layout first (for example from the menu-bar input menu) and verify typed text with zoom. Do not press Shift alone; some input methods use it to switch modes.\nAfter actions that open windows or menus, wait about 1 second before taking a screenshot.";
fn error(id: Value, code: i32, message: &str) -> Response {
    Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})).into_response()
}
pub async fn transport(State(state): State<Arc<AppState>>, request: Request) -> Response {
    match *request.method() {
        Method::DELETE => return StatusCode::NO_CONTENT.into_response(),
        Method::POST => {}
        _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
    let bytes = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error(Value::Null, -32700, "Parse error"),
    };
    let id = value.get("id").cloned();
    if value["jsonrpc"] != "2.0"
        || !value["method"].is_string()
        || id
            .as_ref()
            .is_some_and(|v| !(v.is_string() || v.is_number() || v.is_null()))
    {
        return error(Value::Null, -32600, "Invalid Request");
    }
    let method = value["method"].as_str().unwrap();
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = id.unwrap();
    let params = &value["params"];
    let result = match method {
        "initialize" => {
            state.mcp.client(params["clientInfo"]["name"].as_str());
            let requested = params["protocolVersion"].as_str().unwrap_or("");
            let version = if ["2025-11-25", "2025-06-18", "2025-03-26"].contains(&requested) {
                requested
            } else {
                "2025-11-25"
            };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": "one-kvm",
                    "version": option_env!("PACKAGE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
                },
                "instructions": INSTRUCTIONS
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":super::tools::list()}),
        "tools/call" => {
            let Some(name) = params["name"]
                .as_str()
                .filter(|name| super::tools::NAMES.contains(name))
            else {
                return error(id, -32602, "Unknown tool");
            };
            super::tools::call(
                state,
                name,
                params.get("arguments").cloned().unwrap_or(json!({})),
            )
            .await
        }
        _ => return error(id, -32601, "Method not found"),
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}
