use super::*;
use std::time::Duration;
use tokio::sync::mpsc;

async fn fixture() -> (
    tempfile::TempDir,
    Arc<Registry>,
    Arc<TerminalConn>,
    mpsc::Receiver<Vec<u8>>,
) {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::new(dir.path().join("terminal"));
    let mut config = UuTerminalConfig::default();
    config.serial.enabled = true;
    reg.configure(&config, "test").await.unwrap();
    reg.start();
    let (out, rx) = mpsc::channel(128);
    let conn = TerminalConn::new(reg.clone(), out);
    (dir, reg, conn, rx)
}
async fn receive(rx: &mut mpsc::Receiver<Vec<u8>>, kind: u8) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bytes = rx.recv().await.unwrap();
            if decode(&bytes).unwrap().kind == kind {
                return bytes;
            }
        }
    })
    .await
    .unwrap()
}
async fn open(conn: &TerminalConn, rx: &mut mpsc::Receiver<Vec<u8>>) -> u32 {
    conn.handle(&encode(
        OPEN,
        0,
        br#"{"cols":80,"rows":24,"option":"none"}"#,
    ))
    .await;
    let bytes = receive(rx, OPEN_RESP).await;
    let f = decode(&bytes).unwrap();
    assert_eq!(json(f.payload)["code"], 0);
    f.session
}
#[tokio::test]
async fn list_open_errors_limit_and_names_follow_contract() {
    let (_dir, reg, conn, mut rx) = fixture().await;
    reg.configure(&UuTerminalConfig::default(), "test")
        .await
        .unwrap();
    conn.handle(&encode(OPEN, 0, b"{}")).await;
    let bytes = receive(&mut rx, OPEN_RESP).await;
    assert_eq!(
        json(decode(&bytes).unwrap().payload),
        serde_json::json!({"code":1,"message":DISABLED})
    );
    let mut c = UuTerminalConfig::default();
    c.serial.enabled = true;
    reg.configure(&c, "test").await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(open(&conn, &mut rx).await);
    }
    conn.handle(&encode(OPEN, 0, b"{}")).await;
    let bytes = receive(&mut rx, OPEN_RESP).await;
    assert_eq!(json(decode(&bytes).unwrap().payload)["message"], LIMIT);
    conn.handle(&encode(LIST_REQ, 77, b"")).await;
    let bytes = receive(&mut rx, LIST_RESP).await;
    let f = decode(&bytes).unwrap();
    assert_eq!(f.session, 77);
    let list = json(f.payload);
    assert_eq!(list.as_array().unwrap().len(), 8);
    assert_eq!(list[0]["name"], "选择中");
    assert_eq!(list[1]["name"], "选择中 2");
    assert_eq!(list[0]["shell"], "选择终端");
    reg.shutdown().await;
    assert!(reg.snapshots().is_empty());
}
#[tokio::test]
async fn rename_validates_and_rpc_reports_real_result() {
    let (_dir, reg, conn, mut rx) = fixture().await;
    let id = open(&conn, &mut rx).await;
    let other = open(&conn, &mut rx).await;
    for name in [
        "".to_string(),
        " ".into(),
        "x".repeat(41),
        "x\ny".into(),
        "x\x1by".into(),
    ] {
        assert!(reg.rename(id, &name).is_err());
    }
    reg.rename(id, "  串口维护  ").unwrap();
    assert!(reg.rename(other, "串口维护").is_err());
    let mut body = Vec::new();
    wire::put_varint(1, u64::from(id), &mut body);
    wire::put_bytes(2, "我的终端".as_bytes(), &mut body);
    let mut request = Vec::new();
    wire::put_bytes(1, &[8, 77], &mut request);
    wire::put_bytes(29, &body, &mut request);
    let response = reg.rpc_response(&wire::decode(&request).unwrap()).unwrap();
    let response = wire::decode(&response).unwrap();
    assert_eq!(wire::message(&response, 1), Some(&[8, 77][..]));
    assert_eq!(reg.session(id).unwrap().status().name, "我的终端");
    reg.kill(id).await;
    assert_eq!(reg.rename(id, "gone").unwrap_err(), "该会话已结束");
    reg.shutdown().await;
}
#[tokio::test]
async fn detach_reattach_replays_chunks_and_old_connection_cannot_detach_new() {
    let (_dir, reg, conn, mut rx) = fixture().await;
    let id = open(&conn, &mut rx).await;
    // FIFO resize is a barrier after the initial redraws; wait for its narrow layout.
    conn.handle(&encode(RESIZE, id, br#"{"cols":30,"rows":24}"#))
        .await;
    loop {
        let bytes = receive(&mut rx, STDOUT).await;
        if String::from_utf8_lossy(decode(&bytes).unwrap().payload).contains("      未检测到设备")
        {
            break;
        }
    }
    conn.handle(&encode(DETACH, id, b"")).await;
    assert!(!reg.session(id).unwrap().status().attached);
    let history = vec![b'x'; SCROLLBACK];
    reg.session(id).unwrap().output(&history);
    let (tx, mut replay) = mpsc::channel(128);
    let new = TerminalConn::new(reg.clone(), tx);
    new.handle(&encode(OPEN, id, b"{}")).await;
    let bytes = receive(&mut replay, OPEN_RESP).await;
    assert_eq!(decode(&bytes).unwrap().session, id);
    let mut seen = Vec::new();
    while seen.len() < SCROLLBACK {
        let bytes = receive(&mut replay, STDOUT).await;
        let f = decode(&bytes).unwrap();
        assert!(f.payload.len() <= OUTPUT_CHUNK);
        seen.extend_from_slice(f.payload);
    }
    assert_eq!(seen, history);
    conn.close();
    assert!(reg.session(id).unwrap().status().attached);
    new.close();
    assert!(!reg.session(id).unwrap().status().attached);
    reg.shutdown().await;
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn disabling_target_prints_notice_then_exit_and_shutdown_reaps_shell() {
    let (_dir, reg, conn, mut rx) = fixture().await;
    let mut config = UuTerminalConfig::default();
    config.board.enabled = true;
    reg.configure(&config, "test").await.unwrap();
    let id = open(&conn, &mut rx).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bytes = receive(&mut rx, STDOUT).await;
            if String::from_utf8_lossy(decode(&bytes).unwrap().payload).contains("KVM 板子") {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(reg.session(id).unwrap().status().target, Target::Board);
    reg.configure(&UuTerminalConfig::default(), "test")
        .await
        .unwrap();
    let mut output = Vec::new();
    loop {
        let bytes = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let f = decode(&bytes).unwrap();
        if f.kind == EXIT {
            break;
        }
        if f.kind == STDOUT {
            output.extend_from_slice(f.payload);
        }
    }
    assert!(String::from_utf8_lossy(&output).contains("此终端类型已在网页中关闭。"));
    assert!(reg.snapshots().is_empty());
    reg.shutdown().await;
}
