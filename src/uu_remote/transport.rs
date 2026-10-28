//! UU REST and signaling share one explicit environment-proxy policy.

use std::time::Duration;

use reqwest::header::{CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, UPGRADE};
use tokio_tungstenite::{
    tungstenite::{handshake::client::Request, handshake::derive_accept_key, protocol::Role},
    WebSocketStream,
};

pub(super) fn http_client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    if std::env::var("UU_USE_PROXY").as_deref() == Ok("1") {
        builder
    } else {
        builder.no_proxy()
    }
}

/// Bound DNS, proxy CONNECT, TLS and HTTP upgrade, without limiting the live socket.
pub(super) async fn connect(
    request: Request,
) -> Result<WebSocketStream<reqwest::Upgraded>, String> {
    let client = http_client_builder()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| "UU signaling HTTP client setup failed")?;
    tokio::time::timeout(Duration::from_secs(20), upgrade(&client, request))
        .await
        .map_err(|_| "UU signaling connect timed out")?
}

async fn upgrade(
    client: &reqwest::Client,
    request: Request,
) -> Result<WebSocketStream<reqwest::Upgraded>, String> {
    let (mut parts, _) = request.into_parts();
    let mut url =
        reqwest::Url::parse(&parts.uri.to_string()).map_err(|_| "Invalid UU signaling URL")?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return Err("Unsupported UU signaling URL scheme".into()),
    };
    url.set_scheme(scheme)
        .map_err(|_| "Invalid UU signaling URL")?;
    let key = parts
        .headers
        .get(SEC_WEBSOCKET_KEY)
        .ok_or("Missing WebSocket key")?;
    let accept = derive_accept_key(key.as_bytes());
    if let Some(token) = parts.headers.get_mut("x-nrd-auth") {
        token.set_sensitive(true);
    }
    // reqwest owns proxy selection and TLS. Never retry directly or follow redirects
    // with the room credential; WebSocket framing starts only after a valid upgrade.
    let response = client
        .get(url)
        .headers(parts.headers)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "UU signaling connect timed out"
            } else if e.is_connect() {
                "UU signaling connection failed"
            } else {
                "UU signaling upgrade request failed"
            }
        })?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        return Err(format!(
            "UU signaling upgrade refused: http_status={}",
            response.status().as_u16()
        ));
    }
    let headers = response.headers();
    let connection_upgrade = headers.get_all(CONNECTION).iter().any(|v| {
        v.to_str().is_ok_and(|v| {
            v.split(',')
                .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
        })
    });
    if response.version() != reqwest::Version::HTTP_11
        || !connection_upgrade
        || !headers
            .get(UPGRADE)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        || headers.get(SEC_WEBSOCKET_ACCEPT).map(|v| v.as_bytes()) != Some(accept.as_bytes())
        || headers.contains_key("sec-websocket-extensions")
        || headers.contains_key("sec-websocket-protocol")
    {
        return Err("Invalid UU signaling WebSocket upgrade response".into());
    }
    let stream = response
        .upgrade()
        .await
        .map_err(|_| "UU signaling upgrade failed")?;
    Ok(WebSocketStream::from_raw_socket(stream, Role::Client, None).await)
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
