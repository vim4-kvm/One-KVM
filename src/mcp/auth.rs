use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::sync::Arc;

/// Set by the listener owner; forwarded headers are not trusted for Origin checks.
#[derive(Clone, Copy)]
pub struct ListenerHttps(pub bool);

pub async fn authenticate(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if !state.config.get().mcp.enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"MCP service is disabled"})),
        )
            .into_response();
    }
    let valid = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|key| state.mcp.credentials.lock().accepts(key));
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({"error":"invalid MCP API key"})),
        )
            .into_response();
    }
    let https = request
        .extensions()
        .get::<ListenerHttps>()
        .map(|value| value.0)
        .unwrap_or(state.config.get().web.https_enabled);
    if !same_origin(request.headers(), https) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"cross-origin MCP requests are forbidden"})),
        )
            .into_response();
    }
    next.run(request).await
}
fn same_origin(headers: &axum::http::HeaderMap, https: bool) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|s| reqwest::Url::parse(s).ok())
    else {
        return false;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let scheme = if https { "https" } else { "http" };
    let Some(target) = reqwest::Url::parse(&format!("{scheme}://{host}")).ok() else {
        return false;
    };
    origin.scheme() == target.scheme()
        && origin.host_str() == target.host_str()
        && origin.port_or_known_default() == target.port_or_known_default()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
        && origin.username().is_empty()
        && origin.password().is_none()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_cross_origin_and_null() {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::HOST, "kvm:8080".parse().unwrap());
        assert!(same_origin(&h, false));
        for value in [
            "null",
            "http://other:8080",
            "https://kvm:8080",
            "http://kvm",
        ] {
            h.insert(header::ORIGIN, value.parse().unwrap());
            assert!(!same_origin(&h, false));
        }
        h.insert(header::ORIGIN, "http://kvm:8080".parse().unwrap());
        assert!(same_origin(&h, false));
    }
}
