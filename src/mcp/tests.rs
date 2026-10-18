use super::*;
use axum::http::StatusCode;
use serde_json::{json, Value};

#[test]
fn natural_scrolling_reverses_both_axes() {
    for (direction, wheel) in [("up", 1), ("down", -1), ("left", 1), ("right", -1)] {
        assert_eq!(tools::scroll_wheel(direction, false), wheel);
        assert_eq!(tools::scroll_wheel(direction, true), -wheel);
    }
    let legacy: crate::config::McpConfig =
        serde_json::from_value(json!({"enabled": true})).unwrap();
    assert!(!legacy.host_natural_scrolling);
    assert_eq!(legacy.screenshot_long_edge, 1280);
}

async fn state(data: &std::path::Path) -> Arc<crate::state::AppState> {
    use crate::{
        audio::AudioController,
        auth::*,
        computer_use::ComputerUseManager,
        extensions::ExtensionManager,
        hid::HidBackendType,
        otg::OtgService,
        update::UpdateService,
        video::{Streamer, VideoStreamManager},
        webrtc::WebRtcStreamer,
    };
    let db = crate::db::open_database_pool(data).await.unwrap();
    let config = ConfigStore::new(db.clone_pool());
    config.load().await.unwrap();
    let hid = Arc::new(HidController::new(HidBackendType::None, None));
    let webrtc = WebRtcStreamer::new();
    let stream = VideoStreamManager::with_webrtc_streamer(Streamer::new(), webrtc.clone());
    crate::state::AppState::new(
        db.clone(),
        config.clone(),
        SessionStore::new(3600),
        UserStore::new(db.clone_pool()),
        TwoFactorService::new(db.clone_pool()),
        Arc::new(OtgService::new()),
        stream,
        webrtc,
        hid.clone(),
        ComputerUseManager::new(config, hid),
        None,
        None,
        Arc::new(AudioController::default()),
        Arc::new(ExtensionManager::new()),
        Arc::new(EventBus::new()),
        Arc::new(UpdateService::new()),
        tokio::sync::broadcast::channel(1).0,
        data.to_owned(),
    )
}

#[tokio::test]
async fn activity_details_normalize_keys_and_do_not_copy_typed_text() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path()).await;
    for (name, args, expected) in [
        ("key", json!({"text": "Control+KeyS"}), Some("ctrl+s")),
        (
            "hold_key",
            json!({"text": "SHIFT+Control+KeyS", "duration": 0.1}),
            Some("ctrl+shift+s"),
        ),
        ("key", json!({"text": "private-invalid-key"}), None),
        ("type", json!({"text": "secret中文"}), Some("8 chars")),
        (
            "scroll",
            json!({"scroll_direction": "down", "scroll_amount": 3}),
            Some("down 3"),
        ),
        (
            "left_click_drag",
            json!({"start_coordinate": [1, 2], "coordinate": [3, 4]}),
            Some("(1, 2) → (3, 4)"),
        ),
        (
            "zoom",
            json!({"region": [0, 0, 100, 200]}),
            Some("[0,0,100,200]"),
        ),
        ("mouse_move", json!({"coordinate": [1, 2]}), Some("(1, 2)")),
        ("mouse_move", json!({"coordinate": ["secret", 2]}), None),
    ] {
        tools::call(state.clone(), name, args).await;
        let status = state.mcp.status(&state.snapshot).await;
        let record = &status.recent[0];
        assert_eq!(record.tool, name);
        assert_eq!(record.detail.as_deref(), expected, "{name}");
        assert_eq!(status.activity.tool.as_deref(), Some(name));
        assert_eq!(status.activity.detail.as_deref(), expected, "{name}");
    }
}

#[tokio::test]
async fn activity_keeps_the_latest_call_summary_when_calls_finish_out_of_order() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path()).await;
    assert_eq!(state.mcp.activity().detail, None);
    let started = tokio::time::Instant::now();
    let first = state.mcp.begin("screenshot", None);
    state.mcp.record(
        first,
        "screenshot",
        false,
        started,
        None,
        Some("1280x720".into()),
        None,
    );
    assert_eq!(state.mcp.activity().detail.as_deref(), Some("1280x720"));

    let older = state.mcp.begin("left_click", Some("(1, 2)".into()));
    let latest = state.mcp.begin("left_click", Some("(640, 360)".into()));
    state.mcp.record(
        older,
        "left_click",
        true,
        started,
        None,
        Some("(1, 2)".into()),
        None,
    );
    assert_eq!(state.mcp.activity().detail.as_deref(), Some("(640, 360)"));
    state.mcp.record(
        latest,
        "left_click",
        true,
        started,
        None,
        Some("(640, 360)".into()),
        None,
    );
    state.mcp.begin("wait", None);
    assert_eq!(state.mcp.activity().detail, None);
}

#[tokio::test]
async fn protocol_and_credentials_are_isolated_from_web_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}/api");
    // The listening scheme can differ from persisted settings (CLI overrides).
    state
        .config
        .update(|c| c.web.https_enabled = true)
        .await
        .unwrap();
    let router =
        crate::web::create_router(state.clone()).layer(axum::Extension(auth::ListenerHttps(false)));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/mcp");
    assert_eq!(
        client.post(&endpoint).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let session = state.sessions.create("test-user").await.unwrap();
    let key_url = format!("{endpoint}/key");
    let response = client
        .get(&key_url)
        .bearer_auth(&session.id)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let key = response.json::<Value>().await.unwrap()["api_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = client
        .patch(format!("{endpoint}/config"))
        .bearer_auth(&session.id)
        .json(&json!({"enabled":true, "host_natural_scrolling":true}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(
        response.json::<Value>().await.unwrap()["host_natural_scrolling"],
        true
    );
    let response = client
        .patch(format!("{endpoint}/config"))
        .bearer_auth(&session.id)
        .json(&json!({"host_natural_scrolling":false}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let status = response.json::<Value>().await.unwrap();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["host_natural_scrolling"], false);
    assert!(!state.config.get().mcp.host_natural_scrolling);
    for edge in [1280, 1568, 1920, 2560, 0, 720, 1281, u32::MAX] {
        let response = client
            .patch(format!("{endpoint}/config"))
            .bearer_auth(&session.id)
            .json(&json!({"screenshot_long_edge": edge}))
            .send()
            .await
            .unwrap();
        if crate::config::valid_screenshot_long_edge(edge) {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.json::<Value>().await.unwrap()["screenshot_long_edge"],
                edge
            );
        } else {
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(state.config.get().mcp.screenshot_long_edge, 2560);
        }
    }
    tools::call(state.clone(), "type", json!({"text": "private activity"})).await;
    for credential in [None, Some(key.as_str())] {
        let request = client.delete(format!("{endpoint}/activity"));
        let request = if let Some(value) = credential {
            request.bearer_auth(value)
        } else {
            request
        };
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert!(!state.mcp.status(&state.snapshot).await.recent.is_empty());
    let response = client
        .delete(format!("{endpoint}/activity"))
        .bearer_auth(&session.id)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.json::<Value>().await.unwrap()["recent"], json!([]));
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&key)
            .header("Origin", format!("http://{address}"))
            .json(&json!({"jsonrpc":"2.0","id":0,"method":"ping"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert!(!status.to_string().contains(&key));
    assert_eq!(
        client
            .post(&endpoint)
            .header("Cookie", format!("one_kvm_session={}", session.id))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&key_url)
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base}/config"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for version in ["2025-11-25", "2025-06-18", "2025-03-26", "unknown"] {
        let response: Value = client
            .post(&endpoint)
            .bearer_auth(&key)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": version,
                    "clientInfo": {"name": "test-client"}
                }
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            response["result"]["protocolVersion"],
            if version == "unknown" {
                "2025-11-25"
            } else {
                version
            }
        );
        assert_eq!(response["result"]["serverInfo"]["name"], "one-kvm");
        assert_eq!(response["result"]["instructions"], server::INSTRUCTIONS);
    }
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&key)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let list: Value = client
        .post(&endpoint)
        .bearer_auth(&key)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 17);
    let mut descriptions = std::collections::HashSet::new();
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["title"].is_string());
        assert!(tool["annotations"]["readOnlyHint"].is_boolean());
        let description = tool["description"].as_str().unwrap();
        assert!(!description.trim().is_empty());
        assert!(
            descriptions.insert(description),
            "duplicate tool description"
        );
        for parameter in tool["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .values()
        {
            assert!(parameter["description"]
                .as_str()
                .is_some_and(|description| !description.trim().is_empty()));
        }
    }
    for (body, code) in [
        ("{", -32700),
        (r#"{"jsonrpc":"2.0","id":1,"method":"missing"}"#, -32601),
        ("[]", -32600),
    ] {
        let response: Value = client
            .post(&endpoint)
            .bearer_auth(&key)
            .body(body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], code);
    }
    let call: Value = client
        .post(&endpoint)
        .bearer_auth(&key)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "key", "arguments": {"text": "Return"}}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(call["result"]["isError"], true);
    assert_eq!(
        call["result"]["content"][0]["text"],
        "USB keyboard is not enabled in One-KVM USB settings."
    );
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        client
            .delete(&endpoint)
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&key)
            .header("Origin", "https://attacker.example")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&key)
            .body("x".repeat(1024 * 1024 + 1))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let rotated = client
        .post(format!("{endpoint}/key/regenerate"))
        .bearer_auth(&session.id)
        .send()
        .await
        .unwrap();
    assert!(rotated.status().is_success());
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    server.abort();
}

#[tokio::test]
async fn readonly_wait_does_not_hold_input_and_disable_cancels_it() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path()).await;
    state.config.update(|c| c.mcp.enabled = true).await.unwrap();
    state.mcp.set_enabled(true);
    tokio::time::pause();
    let call = tools::call(state.clone(), "wait", json!({"duration":30}));
    tokio::pin!(call);
    assert!(futures::poll!(&mut call).is_pending());
    let lease = state.hid.input_control.acquire().await.unwrap();
    state.mcp.set_enabled(false);
    let result = call.await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], crate::hid::control::DISABLED);
    assert!(lease.token.is_cancelled());
}

#[tokio::test]
async fn mouse_without_a_known_position_does_not_jump_to_the_origin() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path()).await;
    state
        .config
        .update(|c| {
            c.mcp.enabled = true;
            c.hid.backend = crate::config::HidBackend::Ch9329;
        })
        .await
        .unwrap();
    state.mcp.set_enabled(true);
    let result = tools::call(state, "left_click", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["content"][0]["text"],
        "cursor position is unknown: supply a coordinate first."
    );
}

#[tokio::test]
async fn activity_events_exclude_text_but_private_history_keeps_fifty_and_can_clear() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path()).await;
    state.config.update(|c| c.mcp.enabled = true).await.unwrap();
    state.mcp.set_enabled(true);
    tokio::time::pause();
    let mut events = state.events.subscribe_topic("mcp.activity").unwrap();
    let lease = state.hid.input_control.acquire().await.unwrap();
    let call_id = state.mcp.begin("key", Some("ctrl+s".into()));
    tokio::time::advance(Duration::from_millis(200)).await;
    let event = events.recv().await.unwrap();
    assert!(
        matches!(event,SystemEvent::McpActivity(ref a) if a.state == "active" && a.detail.as_deref() == Some("ctrl+s"))
    );
    lease.finish();
    state.mcp.record(
        call_id,
        "key",
        true,
        tokio::time::Instant::now(),
        None,
        Some("ctrl+s".into()),
        None,
    );
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(state.mcp.activity().state, "idle");
    assert_eq!(state.mcp.activity().detail.as_deref(), Some("ctrl+s"));
    let sensitive = "private input value";
    for _ in 0..52 {
        tools::call(state.clone(), "type", json!({"text":sensitive})).await;
    }
    let status = state.mcp.status(&state.snapshot).await;
    assert_eq!(status.recent.len(), 50);
    assert_eq!(status.recent[0].text.as_deref(), Some(sensitive));
    assert!(!serde_json::to_string(&status.activity)
        .unwrap()
        .contains(sensitive));
    assert_eq!(status.recent[0].detail.as_deref(), Some("19 chars"));
    state.mcp.clear_activity();
    assert!(state.mcp.status(&state.snapshot).await.recent.is_empty());
}
