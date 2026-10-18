//! Engine.IO v4 / Socket.IO signaling client for the UU room.

use std::collections::VecDeque;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::ice::SessionIce;
use super::wire;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoacMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gzip_sdp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ice_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdp_mline_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_control_id: Option<Value>,
}

impl SoacMessage {
    pub fn ice_key(&self) -> String {
        self.ice_id
            .as_ref()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default()
    }
}

pub enum SignalingEvent {
    Connected,
    DeviceInfoChanged(Value),
    /// be-controlled with its binary app_data (ConnectOptions field 1 = capture type).
    ConnectRequest {
        capture_type: u64,
        ice_id: Option<String>,
        transport: SessionIce,
    },
    Soac(SoacMessage),
    Released(Value),
    /// The cloud expired the room (leave without ice_id) or the socket closed.
    Disconnected,
}

/// The cloud reclaims a room after about 20 minutes unless the host refreshes its reconnect
/// key: first after 30 s, then every 300 s after success, retrying every 30 s on failure.
const REFRESH_FIRST: Duration = Duration::from_secs(30);
const REFRESH_RETRY: Duration = Duration::from_secs(30);
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// A refresh ack is `["success", {"reconnect_key": "..."}]`; anything else is a failure.
fn refresh_succeeded(ack: &Value) -> bool {
    let Some(values) = ack.as_array() else {
        return false;
    };
    values.first().and_then(Value::as_str) == Some("success")
        && values
            .get(1)
            .and_then(|v| v.get("reconnect_key"))
            .and_then(Value::as_str)
            .is_some_and(|key| !key.is_empty() && key.len() <= 16_384)
}

pub struct SignalingClient {
    outgoing: mpsc::Sender<String>,
}

fn ice_id_of(value: &Value) -> Option<String> {
    ["ice_id", "iceId"]
        .iter()
        .find_map(|k| {
            value
                .get(*k)
                .or_else(|| value.get("data").and_then(|d| d.get(*k)))
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

impl SignalingClient {
    #[cfg(test)]
    pub(super) fn with_test_sender(outgoing: mpsc::Sender<String>) -> Self {
        Self { outgoing }
    }

    pub async fn connect(
        url: &str,
        room_token: &str,
        events: mpsc::Sender<SignalingEvent>,
    ) -> Result<Self, String> {
        let ws_url = format!(
            "{}/socket.io/?transport=websocket&EIO=4",
            url.trim_end_matches('/')
        );
        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("Invalid signaling URL: {e}"))?;
        request.headers_mut().insert(
            "X-NRD-AUTH",
            room_token
                .parse()
                .map_err(|_| "Invalid room token header")?,
        );
        let (stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| format!("Signaling connect failed: {e}"))?;
        info!("UU signaling connected");
        let (mut write, mut read) = stream.split();
        let (outgoing, mut outgoing_rx) = mpsc::channel::<String>(64);

        let writer = tokio::spawn(async move {
            while let Some(text) = outgoing_rx.recv().await {
                if let Err(e) = write.send(Message::Text(text.into())).await {
                    warn!("UU signaling send failed: {e}");
                    break;
                }
            }
            let _ = write.close().await;
        });

        let replies = outgoing.clone();
        tokio::spawn(async move {
            let mut ack_id = 10u64;
            // Socket.IO binary event awaiting its attachment: (event name, header JSON).
            let mut pending_binary: Option<(String, Value)> = None;
            let mut connected = false;
            let mut next_refresh = Instant::now() + REFRESH_FIRST;
            let mut refresh_acks: VecDeque<u64> = VecDeque::new();
            let mut accepted_refresh = 0u64;
            // Engine.IO v4 requires a ping within pingInterval + pingTimeout.
            // TCP can remain half-open after a NAT drop without a read error.
            let mut ping_window = Duration::from_secs(45);
            let mut ping_deadline = Instant::now() + ping_window;
            loop {
                let message = tokio::select! {
                    // The room loop dropped its receiver: this room is obsolete.
                    _ = events.closed() => break,
                    _ = sleep_until(ping_deadline) => {
                        warn!("UU signaling heartbeat lost; reconnecting");
                        let _ = events.send(SignalingEvent::Disconnected).await;
                        break;
                    }
                    _ = sleep_until(next_refresh), if connected => {
                        ack_id += 1;
                        refresh_acks.push_back(ack_id);
                        if refresh_acks.len() > 16 {
                            refresh_acks.pop_front();
                        }
                        if replies.send(format!("42{ack_id}[\"refresh_reconnect_key\"]")).await.is_err() {
                            break;
                        }
                        next_refresh = Instant::now() + REFRESH_RETRY;
                        debug!("UU reconnect-key refresh requested");
                        continue;
                    }
                    message = read.next() => match message {
                        Some(message) => message,
                        None => {
                            let _ = events.send(SignalingEvent::Disconnected).await;
                            break;
                        }
                    },
                };
                match message {
                    Ok(Message::Text(text)) => {
                        let text = text.as_str();
                        if text.starts_with('0') {
                            if let Ok(open) = serde_json::from_str::<Value>(&text[1..]) {
                                if let (Some(interval), Some(timeout)) = (
                                    open.get("pingInterval").and_then(Value::as_u64),
                                    open.get("pingTimeout").and_then(Value::as_u64),
                                ) {
                                    ping_window = Duration::from_millis(
                                        interval.saturating_add(timeout).clamp(5_000, 300_000),
                                    );
                                    ping_deadline = Instant::now() + ping_window;
                                }
                            }
                            let _ = replies.send("40".into()).await; // Engine.IO open -> Socket.IO connect
                        } else if text == "2" {
                            ping_deadline = Instant::now() + ping_window;
                            let _ = replies.send("3".into()).await; // ping -> pong
                        } else if text.starts_with("40") {
                            connected = true;
                            next_refresh = Instant::now() + REFRESH_FIRST;
                            ack_id += 1;
                            let _ = replies.send(format!("42{ack_id}[\"room_info\"]")).await;
                            let _ = events.send(SignalingEvent::Connected).await;
                        } else if let Some(ack) = text.strip_prefix("43") {
                            let begin = ack.find('[').unwrap_or(ack.len());
                            let id = ack[..begin].parse::<u64>().ok();
                            match id.filter(|id| refresh_acks.contains(id)) {
                                Some(id) => {
                                    refresh_acks.retain(|pending| *pending != id);
                                    let success = serde_json::from_str::<Value>(&ack[begin..])
                                        .is_ok_and(|v| refresh_succeeded(&v));
                                    // A delayed ack must not push back the schedule of a newer success.
                                    if success && id > accepted_refresh {
                                        accepted_refresh = id;
                                        next_refresh = Instant::now() + REFRESH_INTERVAL;
                                    }
                                    info!(
                                        "UU reconnect-key refresh acknowledged: success={success}"
                                    );
                                }
                                None => debug!("UU room_info acknowledged"),
                            }
                        } else if text.starts_with("45") || text.starts_with("46") {
                            let body =
                                text.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
                            let parsed = serde_json::from_str::<Value>(body).ok();
                            let name: String = parsed
                                .as_ref()
                                .and_then(|v| v.get(0)?.as_str().map(str::to_owned))
                                .unwrap_or_default()
                                .chars()
                                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                                .take(48)
                                .collect();
                            let header = parsed
                                .and_then(|v| v.get(1).cloned())
                                .unwrap_or(Value::Null);
                            pending_binary = Some((name, header));
                        } else if let Some(payload) = text.strip_prefix("42") {
                            let Ok(Value::Array(array)) = serde_json::from_str::<Value>(payload)
                            else {
                                continue;
                            };
                            let (Some(name), Some(data)) =
                                (array.first().and_then(Value::as_str), array.get(1))
                            else {
                                continue;
                            };
                            debug!("UU signaling event: {name}");
                            let event = match name {
                                "bmsg_push" => {
                                    Some(SignalingEvent::DeviceInfoChanged(data.clone()))
                                }
                                "soac" => {
                                    let (inner, client_id) = match data.get("data") {
                                        Some(inner) => (
                                            inner.clone(),
                                            data.get("client_id")
                                                .and_then(Value::as_str)
                                                .map(str::to_owned),
                                        ),
                                        None => (data.clone(), None),
                                    };
                                    match serde_json::from_value::<SoacMessage>(inner) {
                                        Ok(mut soac) => {
                                            soac.client_id = soac.client_id.or(client_id);
                                            Some(SignalingEvent::Soac(soac))
                                        }
                                        Err(e) => {
                                            warn!("UU soac message ignored: {e}");
                                            None
                                        }
                                    }
                                }
                                "released" => Some(SignalingEvent::Released(data.clone())),
                                "left" | "leave" => Some(match ice_id_of(data) {
                                    Some(_) => SignalingEvent::Released(data.clone()),
                                    None => {
                                        info!("UU room expired; renewing");
                                        SignalingEvent::Disconnected
                                    }
                                }),
                                _ => None,
                            };
                            if let Some(event) = event {
                                if events.send(event).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(Message::Binary(data)) => {
                        // Engine.IO v4 sends attachments raw; tolerate a legacy 0x04 prefix.
                        let data: &[u8] =
                            if data.first() == Some(&4) && wire::decode(&data).is_err() {
                                &data[1..]
                            } else {
                                &data
                            };
                        let Some((name, header)) = pending_binary.take() else {
                            continue;
                        };
                        if name != "be-controlled" {
                            continue;
                        }
                        let capture_type = wire::decode(data)
                            .ok()
                            .and_then(|f| wire::number(&f, 1))
                            .unwrap_or(0);
                        let ice_id = ice_id_of(&header);
                        info!(
                            "UU connect request: capture_type={capture_type} ice_id_present={}",
                            ice_id.is_some()
                        );
                        let event = SignalingEvent::ConnectRequest {
                            capture_type,
                            ice_id,
                            transport: SessionIce::from_header(&header),
                        };
                        if events.send(event).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => {
                        let _ = events.send(SignalingEvent::Disconnected).await;
                        break;
                    }
                    _ => {}
                }
            }
            // Peer callbacks may still hold this client's sender; stop writing to the old room.
            writer.abort();
            debug!("UU signaling receiver stopped");
        });

        Ok(Self { outgoing })
    }

    pub async fn emit_event(&self, event: &str, payload: Value) -> Result<(), String> {
        self.outgoing
            .send(format!("42{}", serde_json::json!([event, payload])))
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn emit_soac(&self, soac: SoacMessage) -> Result<(), String> {
        let payload = match &soac.client_id {
            Some(client_id) => serde_json::json!({ "client_id": client_id, "data": soac }),
            None => serde_json::to_value(&soac).map_err(|e| e.to_string())?,
        };
        self.emit_event("soac", payload).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn heartbeat_keeps_socket_alive_then_detects_silent_half_open_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (silent_tx, silent_rx) = tokio::sync::oneshot::channel();
        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            socket
                .send(Message::Text(
                    r#"0{"pingInterval":3000,"pingTimeout":2000}"#.into(),
                ))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::Text("40".into())
            );
            socket.send(Message::Text("40{}".into())).await.unwrap();
            // Cross the original five-second deadline with acknowledged pings.
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_secs(3)).await;
                socket.send(Message::Text("2".into())).await.unwrap();
                loop {
                    if socket.next().await.unwrap().unwrap() == Message::Text("3".into()) {
                        break;
                    }
                }
            }
            silent_tx.send(()).unwrap();
            let _ = hold_rx.await; // Retain TCP without sending pings or a close frame.
        });
        let (sender, mut events) = mpsc::channel(8);
        let client = SignalingClient::connect(&format!("ws://{address}"), "synthetic-only", sender)
            .await
            .unwrap();
        assert!(matches!(
            events.recv().await,
            Some(SignalingEvent::Connected)
        ));
        tokio::time::timeout(Duration::from_secs(10), silent_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(8), events.recv())
                .await
                .unwrap(),
            Some(SignalingEvent::Disconnected)
        ));
        // Even retained peer callbacks must lose access to the obsolete writer.
        client.outgoing.closed().await;
        drop(hold_tx);
        server.await.unwrap();
    }

    #[test]
    fn refresh_ack_requires_success_and_a_bounded_key() {
        assert!(refresh_succeeded(
            &json!(["success", {"reconnect_key": "k"}])
        ));
        assert!(!refresh_succeeded(
            &json!(["success", {"reconnect_key": ""}])
        ));
        assert!(!refresh_succeeded(&json!(["success", {}])));
        assert!(!refresh_succeeded(
            &json!(["error", {"reconnect_key": "k"}])
        ));
        assert!(!refresh_succeeded(&json!({"reconnect_key": "k"})));
        let long = "k".repeat(16_385);
        assert!(!refresh_succeeded(
            &json!(["success", {"reconnect_key": long}])
        ));
    }
}
