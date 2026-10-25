//! QR login transitions; credentials and QR contents never enter diagnostics.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::Instant;

use super::cloud::{api_data, CloudClient, QR_EXPIRED};
use super::{Inner, UuLoginStage, UuLoginState};

pub(super) async fn authenticate(inner: &Inner, client: &CloudClient) -> Result<Value, String> {
    loop {
        let (status, response) = client.login_post("/api/v1/qrcode/gen/login", None).await?;
        let data = api_data("QR code", status, &response)?;
        let url = data
            .get("qrcode_jump_url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .ok_or("QR response has no URL")?;
        let ticket = data
            .get("status_query_ticket")
            .or_else(|| data.get("token"))
            .and_then(Value::as_str)
            .filter(|ticket| !ticket.is_empty())
            .ok_or("QR response has no status ticket")?;
        inner.login.send_replace(UuLoginState {
            stage: UuLoginStage::Waiting,
            qr_url: Some(url.to_owned()),
            message: None,
        });
        let mut previous = 1;
        loop {
            let started = Instant::now();
            let query = json!({"token": ticket, "qrcode_jump_url": url, "login_status": previous});
            let (status, response) = client
                .login_post("/api/v1/qrcode/login/status", Some(&query))
                .await?;
            if response.get("code").and_then(Value::as_i64) == Some(QR_EXPIRED) {
                return Err("QR code expired; refresh to sign in again".into());
            }
            let state = api_data("QR status", status, &response)?
                .get("status")
                .and_then(Value::as_i64)
                .ok_or("QR status missing")?;
            match state {
                0 => {
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    break;
                }
                1 | 2 => {
                    if state == 2 && previous != 2 {
                        inner.login.send_replace(UuLoginState {
                            stage: UuLoginStage::Scanned,
                            qr_url: Some(url.to_owned()),
                            message: None,
                        });
                    }
                    previous = state;
                    // A broken intermediary may return an unchanged state immediately.
                    // Bound polling even when the server fails to hold the long poll.
                    tokio::time::sleep_until(started + Duration::from_millis(350)).await;
                }
                3 => break,
                4 => {
                    let body = json!({"token": ticket, "qrcode_jump_url": url});
                    let (status, response) = client
                        .login_post("/api/v1/login/by_qrcode", Some(&body))
                        .await?;
                    return api_data("QR login", status, &response).cloned();
                }
                _ => return Err("QR login stopped; refresh to sign in again".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn service(path: &std::path::Path) -> super::super::UuRemoteService {
        super::super::UuRemoteService::new(
            path.into(),
            crate::video::VideoStreamManager::with_webrtc_streamer(
                crate::video::streamer::Streamer::new(),
                crate::webrtc::WebRtcStreamer::new(),
            ),
            Arc::new(crate::hid::HidController::new(
                crate::hid::HidBackendType::None,
                None,
            )),
            Arc::new(crate::audio::AudioController::default()),
        )
    }

    /// Script only protocol responses; assert the requests at the HTTP boundary.
    async fn server(
        script: Vec<(&'static str, u16, Value)>,
    ) -> (CloudClient, tokio::task::JoinHandle<Vec<Option<Value>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client =
            CloudClient::at_test_server(format!("http://{}", listener.local_addr().unwrap()));
        let task = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for (path, status, response) in script {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 1024];
                    let size = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(size, 0);
                    request.extend_from_slice(&buffer[..size]);
                    if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                assert_eq!(
                    headers.lines().next().unwrap().split_whitespace().nth(1),
                    Some(path)
                );
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let mut buffer = [0; 1024];
                    let size = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(size, 0);
                    request.extend_from_slice(&buffer[..size]);
                }
                bodies.push((length != 0).then(|| {
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap()
                }));
                let body = response.to_string();
                stream.write_all(format!("HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            bodies
        });
        (client, task)
    }

    fn qr(ticket: &str) -> Value {
        json!({"code":0,"data":{"qrcode_jump_url":"https://example.invalid/qr", "status_query_ticket":ticket,"token":"obsolete-ticket"}})
    }
    fn status(value: i64) -> Value {
        json!({"code":0,"data":{"status":value}})
    }

    #[tokio::test]
    async fn modern_ticket_and_only_confirmed_state_can_finish_login() {
        let gen = "/api/v1/qrcode/gen/login";
        let poll = "/api/v1/qrcode/login/status";
        let done = "/api/v1/login/by_qrcode";
        let (client, server) = server(vec![
            (gen, 200, qr("first")),
            (poll, 200, status(2)),
            (poll, 200, status(3)),
            (gen, 200, qr("second")),
            (poll, 200, status(0)),
            (gen, 200, qr("third")),
            (poll, 200, status(4)),
            (
                done,
                200,
                json!({"code":0,"data":{"user_id":"synthetic-user"}}),
            ),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            authenticate(&service.inner, &client),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["user_id"], "synthetic-user");
        let bodies = server.await.unwrap();
        for index in [0, 3, 5] {
            assert!(bodies[index].is_none(), "QR generation must have no body");
        }
        assert_eq!(bodies[1].as_ref().unwrap()["token"], "first");
        assert_eq!(bodies[2].as_ref().unwrap()["login_status"], 2);
        assert_eq!(bodies[4].as_ref().unwrap()["token"], "second");
        assert_eq!(bodies[6].as_ref().unwrap()["login_status"], 1);
        assert_eq!(bodies[7].as_ref().unwrap()["token"], "third");
    }

    #[tokio::test]
    async fn expired_or_failed_qr_poll_stops_until_a_new_login_is_requested() {
        for (http, response) in [
            (400, json!({"code":1190})),
            (500, json!({"code":9})),
            (200, status(9)),
        ] {
            let (client, server) = server(vec![
                ("/api/v1/qrcode/gen/login", 200, qr("current")),
                ("/api/v1/qrcode/login/status", http, response),
            ])
            .await;
            let dir = tempfile::tempdir().unwrap();
            let service = service(dir.path());
            assert!(tokio::time::timeout(
                Duration::from_secs(2),
                authenticate(&service.inner, &client)
            )
            .await
            .unwrap()
            .is_err());
            assert_eq!(server.await.unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn legacy_ticket_is_used_only_when_modern_field_is_absent() {
        for modern in [None, Some("")] {
            let mut generated = qr("unused");
            generated["data"]
                .as_object_mut()
                .unwrap()
                .remove("status_query_ticket");
            if let Some(value) = modern {
                generated["data"]["status_query_ticket"] = value.into();
            }
            let mut script = vec![("/api/v1/qrcode/gen/login", 200, generated)];
            if modern.is_none() {
                script.push(("/api/v1/qrcode/login/status", 400, json!({"code":1190})));
            }
            let (client, server) = server(script).await;
            let dir = tempfile::tempdir().unwrap();
            assert!(authenticate(&service(dir.path()).inner, &client)
                .await
                .is_err());
            let bodies = server.await.unwrap();
            if modern.is_none() {
                assert_eq!(bodies[1].as_ref().unwrap()["token"], "obsolete-ticket");
            } else {
                assert_eq!(bodies.len(), 1);
            }
        }
    }

    #[tokio::test]
    async fn unchanged_poll_state_has_a_minimum_retry_interval() {
        let (client, server) = server(vec![
            ("/api/v1/qrcode/gen/login", 200, qr("current")),
            ("/api/v1/qrcode/login/status", 200, status(1)),
            ("/api/v1/qrcode/login/status", 400, json!({"code":1190})),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        assert!(authenticate(&service(dir.path()).inner, &client)
            .await
            .is_err());
        assert_eq!(server.await.unwrap().len(), 3);
        assert!(started.elapsed() >= Duration::from_millis(350));
    }
}
