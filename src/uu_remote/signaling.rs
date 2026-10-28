//! Engine.IO v4 / Socket.IO signaling client for the UU room.

use std::collections::VecDeque;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::capabilities::ViewerCapabilities;
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
    pub fn ice_key(&self) -> Option<String> {
        match self.ice_id.as_ref() {
            Some(value) => session_key(value),
            None => Some(String::new()),
        }
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
        capabilities: ViewerCapabilities,
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
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

async fn enqueue<T>(sender: &mpsc::Sender<T>, value: T) -> Result<(), String> {
    tokio::time::timeout(DELIVERY_TIMEOUT, sender.send(value))
        .await
        .map_err(|_| "UU signaling delivery timed out".to_owned())?
        .map_err(|_| "UU signaling queue closed".to_owned())
}

/// Socket liveness must not depend on the room consumer draining its event queue.
async fn supervise_connection(
    mut reader: JoinHandle<()>,
    mut writer: JoinHandle<()>,
    mut heartbeat: watch::Receiver<Instant>,
    events: mpsc::Sender<SignalingEvent>,
) {
    loop {
        let deadline = *heartbeat.borrow_and_update();
        tokio::select! {
            _ = &mut reader => break,
            _ = &mut writer => break,
            _ = events.closed() => break,
            changed = heartbeat.changed() => if changed.is_err() { break; },
            _ = sleep_until(deadline) => {
                warn!("UU signaling heartbeat lost; reconnecting");
                break;
            }
        }
    }
    reader.abort();
    writer.abort();
    if !reader.is_finished() {
        let _ = reader.await;
    }
    if !writer.is_finished() {
        let _ = writer.await;
    }
    // Close the socket first. A full queue may delay this notification, but must
    // neither keep the old socket alive nor silently lose the reconnect request.
    let _ = events.send(SignalingEvent::Disconnected).await;
}

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

pub(super) fn session_key(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        Value::Number(id) if id.is_i64() || id.is_u64() => Some(id.to_string()),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum IceId {
    Missing,
    Session(String),
    Invalid,
}

pub(super) fn ice_id_of(value: &Value) -> IceId {
    let raw = ["ice_id", "iceId"].iter().find_map(|k| {
        value
            .get(*k)
            .or_else(|| value.get("data").and_then(|d| d.get(*k)))
    });
    match raw {
        None => IceId::Missing,
        Some(value) => session_key(value).map_or(IceId::Invalid, IceId::Session),
    }
}

fn leave_event(value: &Value) -> Option<SignalingEvent> {
    match ice_id_of(value) {
        IceId::Missing => Some(SignalingEvent::Disconnected),
        IceId::Session(_) => Some(SignalingEvent::Released(value.clone())),
        IceId::Invalid => None,
    }
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
        let stream = super::transport::connect(request).await?;
        info!("UU signaling connected");
        let (mut write, mut read) = stream.split();
        let (outgoing, mut outgoing_rx) = mpsc::channel::<String>(64);
        let (heartbeat_tx, heartbeat_rx) = watch::channel(Instant::now() + Duration::from_secs(45));

        let writer = tokio::spawn(async move {
            while let Some(text) = outgoing_rx.recv().await {
                if !matches!(
                    tokio::time::timeout(DELIVERY_TIMEOUT, write.send(Message::Text(text.into())))
                        .await,
                    Ok(Ok(()))
                ) {
                    warn!("UU signaling send failed or timed out");
                    break;
                }
            }
        });

        let replies = outgoing.clone();
        let reader_events = events.clone();
        let reader = tokio::spawn(async move {
            let events = reader_events;
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
            loop {
                let message = tokio::select! {
                    // The room loop dropped its receiver: this room is obsolete.
                    _ = events.closed() => break,
                    _ = sleep_until(next_refresh), if connected => {
                        ack_id += 1;
                        refresh_acks.push_back(ack_id);
                        if refresh_acks.len() > 16 {
                            refresh_acks.pop_front();
                        }
                        if enqueue(&replies, format!("42{ack_id}[\"refresh_reconnect_key\"]")).await.is_err() {
                            break;
                        }
                        next_refresh = Instant::now() + REFRESH_RETRY;
                        debug!("UU reconnect-key refresh requested");
                        continue;
                    }
                    message = read.next() => match message {
                        Some(message) => message,
                        None => break,
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
                                    heartbeat_tx.send_replace(Instant::now() + ping_window);
                                }
                            }
                            if enqueue(&replies, "40".into()).await.is_err() {
                                break;
                            }
                        } else if text == "2" {
                            heartbeat_tx.send_replace(Instant::now() + ping_window);
                            if enqueue(&replies, "3".into()).await.is_err() {
                                break;
                            }
                        } else if text.starts_with("40") {
                            connected = true;
                            next_refresh = Instant::now() + REFRESH_FIRST;
                            ack_id += 1;
                            if enqueue(&replies, format!("42{ack_id}[\"room_info\"]"))
                                .await
                                .is_err()
                                || enqueue(&events, SignalingEvent::Connected).await.is_err()
                            {
                                break;
                            }
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
                                "left" | "leave" => leave_event(data),
                                _ => None,
                            };
                            if let Some(event) = event {
                                if enqueue(&events, event).await.is_err() {
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
                        let ice_id = match ice_id_of(&header) {
                            IceId::Missing => None,
                            IceId::Session(id) => Some(id),
                            IceId::Invalid => continue,
                        };
                        info!(
                            "UU connect request: capture_type={capture_type} ice_id_present={}",
                            ice_id.is_some()
                        );
                        let event = SignalingEvent::ConnectRequest {
                            capture_type,
                            ice_id,
                            transport: SessionIce::from_header(&header),
                            capabilities: ViewerCapabilities::from_header(&header),
                        };
                        if enqueue(&events, event).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => {
                        break;
                    }
                    _ => {}
                }
            }
            debug!("UU signaling receiver stopped");
        });
        tokio::spawn(supervise_connection(reader, writer, heartbeat_rx, events));

        Ok(Self { outgoing })
    }

    pub async fn emit_event(&self, event: &str, payload: Value) -> Result<(), String> {
        enqueue(
            &self.outgoing,
            format!("42{}", serde_json::json!([event, payload])),
        )
        .await
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

    #[test]
    fn numeric_and_invalid_leaves_never_become_room_expiration() {
        assert!(matches!(
            leave_event(&json!({})),
            Some(SignalingEvent::Disconnected)
        ));
        for header in [json!({"ice_id":123}), json!({"data":{"iceId":"123"}})] {
            assert!(matches!(
                leave_event(&header),
                Some(SignalingEvent::Released(_))
            ));
            assert_eq!(ice_id_of(&header), IceId::Session("123".into()));
        }
        for id in [
            Value::Null,
            json!(false),
            json!({}),
            json!([]),
            json!(1.25),
            json!(""),
        ] {
            assert!(leave_event(&json!({"ice_id":id})).is_none());
        }
    }

    #[tokio::test]
    async fn stalled_handshake_releases_the_reconnect_loop() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (events, _received) = mpsc::channel(1);
        let connecting = tokio::spawn(async move {
            SignalingClient::connect(&url, "fixture-room-token", events).await
        });
        // Hold the accepted TCP connection without answering the WebSocket upgrade.
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(21)).await;
        tokio::task::yield_now().await;
        let finished = connecting.is_finished();
        if !finished {
            connecting.abort();
        }
        assert!(finished, "stalled handshake prevented reconnect retry");
        assert!(connecting.await.unwrap().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_closes_blocked_delivery_before_reporting_disconnect() {
        let (events, mut received) = mpsc::channel(1);
        events.send(SignalingEvent::Connected).await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (reader_life, reader_closed) = tokio::sync::oneshot::channel::<()>();
        let blocked = events.clone();
        let reader = tokio::spawn(async move {
            let _life = reader_life;
            started_tx.send(()).unwrap();
            let _ = enqueue(&blocked, SignalingEvent::DeviceInfoChanged(Value::Null)).await;
        });
        let (writer_life, writer_closed) = tokio::sync::oneshot::channel::<()>();
        let writer = tokio::spawn(async move {
            let _life = writer_life;
            std::future::pending::<()>().await;
        });
        let (_heartbeat, deadline) = watch::channel(Instant::now() + Duration::from_secs(1));
        let supervisor = tokio::spawn(supervise_connection(reader, writer, deadline, events));
        started_rx.await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(reader_closed.await.is_err());
        assert!(writer_closed.await.is_err());
        assert!(matches!(
            received.recv().await,
            Some(SignalingEvent::Connected)
        ));
        assert!(matches!(
            received.recv().await,
            Some(SignalingEvent::Disconnected)
        ));
        supervisor.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_delivery_requests_reconnect_without_waiting_for_ping_deadline() {
        let (events, mut received) = mpsc::channel(1);
        events.send(SignalingEvent::Connected).await.unwrap();
        let blocked = events.clone();
        let reader = tokio::spawn(async move {
            assert!(
                enqueue(&blocked, SignalingEvent::DeviceInfoChanged(Value::Null))
                    .await
                    .is_err()
            );
        });
        let (writer_life, writer_closed) = tokio::sync::oneshot::channel::<()>();
        let writer = tokio::spawn(async move {
            let _life = writer_life;
            std::future::pending::<()>().await;
        });
        let (_heartbeat, deadline) = watch::channel(Instant::now() + Duration::from_secs(60));
        let supervisor = tokio::spawn(supervise_connection(reader, writer, deadline, events));
        // Awaiting the guard drives paused time to the delivery deadline.
        assert!(writer_closed.await.is_err());
        assert!(matches!(
            received.recv().await,
            Some(SignalingEvent::Connected)
        ));
        assert!(matches!(
            received.recv().await,
            Some(SignalingEvent::Disconnected)
        ));
        supervisor.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn producer_has_a_bounded_wait_when_writer_queue_is_full() {
        let (outgoing, mut received) = mpsc::channel(1);
        outgoing.send("already queued".into()).await.unwrap();
        let client = SignalingClient::with_test_sender(outgoing);
        assert!(client.emit_event("soac", Value::Null).await.is_err());
        assert_eq!(received.recv().await.unwrap(), "already queued");
        assert!(received.try_recv().is_err());
    }

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
