use super::*;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

fn request(url: &str) -> Request {
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("x-nrd-auth", "fixture-room-token".parse().unwrap());
    request
}

async fn read_headers(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(socket.read_u8().await.unwrap());
        assert!(bytes.len() < 16384);
    }
    String::from_utf8(bytes).unwrap()
}

fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

fn upgrade_response(headers: &str) -> Vec<u8> {
    let accept = derive_accept_key(header(headers, "sec-websocket-key").unwrap().as_bytes());
    let mut reply = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").into_bytes();
    // The HTTP client must preserve the first Engine.IO frame read with the headers.
    reply.extend_from_slice(&[0x81, 3, b'0', b'{', b'}']);
    reply
}

#[tokio::test]
async fn upgrade_preserves_prefetched_frames_and_live_bidirectional_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/socket.io/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let headers = read_headers(&mut socket).await;
        assert_eq!(header(&headers, "x-nrd-auth"), Some("fixture-room-token"));
        socket.write_all(&upgrade_response(&headers)).await.unwrap();
        let mut socket = WebSocketStream::from_raw_socket(socket, Role::Server, None).await;
        assert!(matches!(socket.next().await, Some(Ok(Message::Text(v))) if v == "ping"));
        socket.send(Message::Text("pong".into())).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .build()
        .unwrap();
    let mut socket = upgrade(&client, request(&url)).await.unwrap();
    assert!(matches!(socket.next().await, Some(Ok(Message::Text(v))) if v == "0{}"));
    // Advancing beyond the establishment deadline must not kill an upgraded socket.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;
    tokio::time::resume();
    socket.send(Message::Text("ping".into())).await.unwrap();
    assert!(matches!(socket.next().await, Some(Ok(Message::Text(v))) if v == "pong"));
    server.await.unwrap();
}

// Environment changes stay in a subprocess, never racing other tests.
#[tokio::test]
async fn environment_policy_probe() {
    let Ok(url) = std::env::var("UU_TEST_ENDPOINT") else {
        return;
    };
    assert!(url.starts_with("http://127.0.0.1:"));
    let client = super::super::cloud::CloudClient::at_test_server(url.clone());
    client.login_post("/fixture", None).await.unwrap();
    let (events, _received) = tokio::sync::mpsc::channel(8);
    super::super::signaling::SignalingClient::connect(
        &url.replacen("http:", "ws:", 1),
        "fixture-room-token",
        events,
    )
    .await
    .unwrap();
}

async fn serve_policy(listener: TcpListener, proxy: bool, routes: tokio::sync::mpsc::Sender<bool>) {
    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let headers = read_headers(&mut socket).await;
        routes.send(proxy).await.unwrap();
        if header(&headers, "upgrade").is_some() {
            assert_eq!(header(&headers, "x-nrd-auth"), Some("fixture-room-token"));
            socket.write_all(&upgrade_response(&headers)).await.unwrap();
        } else {
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"code\":0,\"data\":{}}").await.unwrap();
        }
    }
}

#[tokio::test]
async fn rest_and_signaling_share_proxy_opt_in_and_no_proxy() {
    for (opt_in, bypass, via_proxy) in [
        (false, false, false),
        (true, false, true),
        (true, true, false),
    ] {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", origin.local_addr().unwrap());
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let (routes, mut received) = tokio::sync::mpsc::channel(4);
        let origin = tokio::spawn(serve_policy(origin, false, routes.clone()));
        let proxy = tokio::spawn(serve_policy(proxy, true, routes));
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .kill_on_drop(true)
            .args([
                "--exact",
                "uu_remote::transport::tests::environment_policy_probe",
                "--nocapture",
            ])
            .env_clear()
            .env("UU_TEST_ENDPOINT", endpoint)
            .env("HTTP_PROXY", proxy_url);
        if opt_in {
            child.env("UU_USE_PROXY", "1");
        }
        if bypass {
            child.env("NO_PROXY", "127.0.0.1");
        }
        let output = tokio::time::timeout(Duration::from_secs(10), child.output())
            .await
            .unwrap()
            .unwrap();
        origin.abort();
        proxy.abort();
        assert!(
            output.status.success(),
            "isolated proxy policy check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            received.recv().await,
            Some(via_proxy),
            "REST route, opt_in={opt_in}, bypass={bypass}"
        );
        assert_eq!(
            received.recv().await,
            Some(via_proxy),
            "WebSocket route, opt_in={opt_in}, bypass={bypass}"
        );
    }
}

#[tokio::test]
async fn wss_proxy_connect_verifies_tls_and_keeps_room_auth_inside_the_tunnel() {
    use axum::extract::ws::WebSocketUpgrade;
    use axum_server::tls_openssl::OpenSSLAcceptor;
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pem = cert.cert.pem();
    let config = crate::utils::tls::server_config_from_pem(
        pem.as_bytes(),
        cert.signing_key.serialize_pem().as_bytes(),
    )
    .unwrap();
    for trusted in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = axum_server::Handle::new();
        let app = axum::Router::new().route(
            "/socket.io/",
            axum::routing::get(
                |headers: reqwest::header::HeaderMap, ws: WebSocketUpgrade| async move {
                    assert_eq!(headers.get("x-nrd-auth").unwrap(), "fixture-room-token");
                    assert!(!headers.contains_key("proxy-authorization"));
                    ws.on_upgrade(|mut socket| async move {
                        socket
                            .send(axum::extract::ws::Message::Text("0{}".into()))
                            .await
                            .unwrap();
                    })
                },
            ),
        );
        let server = tokio::spawn(
            axum_server::from_tcp(listener)
                .unwrap()
                .acceptor(OpenSSLAcceptor::new(config.clone()))
                .handle(handle.clone())
                .serve(app.into_make_service()),
        );
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let tunnel = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.unwrap();
            let headers = read_headers(&mut socket).await;
            assert!(headers.starts_with(&format!("CONNECT localhost:{} HTTP/1.1", address.port())));
            assert!(header(&headers, "proxy-authorization").is_some());
            assert!(!headers.contains("fixture-room-token"));
            let mut origin = TcpStream::connect(address).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut origin).await;
        });
        let proxy = reqwest::Proxy::https(proxy_url)
            .unwrap()
            .basic_auth("fixture-user", "fixture-proxy-secret");
        let mut builder = http_client_builder().no_proxy().proxy(proxy).http1_only();
        if trusted {
            builder = builder
                .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap());
        }
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            upgrade(
                &builder.build().unwrap(),
                request(&format!("wss://localhost:{}/socket.io/", address.port())),
            ),
        )
        .await
        .unwrap();
        if trusted {
            let mut socket = result.unwrap();
            assert!(matches!(socket.next().await, Some(Ok(Message::Text(v))) if v == "0{}"));
        } else {
            let error = result.err().expect("untrusted certificate was accepted");
            assert!(!error.contains("fixture"));
        }
        handle.shutdown();
        server.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), tunnel)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn refused_or_invalid_upgrade_does_not_follow_redirects_or_expose_content() {
    for status in [302, 407, 101] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!(
            "http://{}/fixture-secret",
            destination.local_addr().unwrap()
        );
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_headers(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 {status} Reply\r\nLocation: {location}\r\nContent-Length: 19\r\n\r\nfixture-body-secret").as_bytes()).await.unwrap();
        });
        let client = http_client_builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let error = upgrade(&client, request(&url)).await.err().unwrap();
        assert!(!error.contains("fixture"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), destination.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}
