use super::*;
use control::{InputSource, ManualKind};

#[derive(Debug, PartialEq, Eq)]
enum Report {
    Keyboard([u8; 8]),
    Mouse(u8, u16, u16),
    ManualKey(CanonicalKey),
    ManualMouse,
}
struct Backend {
    reports: mpsc::UnboundedSender<Report>,
    permits: Arc<tokio::sync::Semaphore>,
    runtime: tokio::sync::watch::Sender<()>,
    fail: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl HidBackend for Backend {
    async fn init(&self) -> Result<()> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
    async fn reset(&self) -> Result<()> {
        panic!("MCP must not globally reset HID")
    }
    async fn send_keyboard(&self, e: KeyboardEvent) -> Result<()> {
        self.reports.send(Report::ManualKey(e.key)).unwrap();
        Ok(())
    }
    async fn send_mouse(&self, _: MouseEvent) -> Result<()> {
        self.reports.send(Report::ManualMouse).unwrap();
        Ok(())
    }
    async fn write_keyboard_report(&self, r: types::KeyboardReport) -> Result<()> {
        self.reports.send(Report::Keyboard(r.to_bytes())).unwrap();
        self.permits.acquire().await.unwrap().forget();
        if self.fail.load(Ordering::Relaxed) {
            Err(AppError::BadRequest("test write failed".into()))
        } else {
            Ok(())
        }
    }
    async fn write_absolute_report(&self, b: u8, x: u16, y: u16, _: i8) -> Result<()> {
        self.reports.send(Report::Mouse(b, x, y)).unwrap();
        self.permits.acquire().await.unwrap().forget();
        Ok(())
    }
    fn runtime_snapshot(&self) -> HidBackendRuntimeSnapshot {
        Default::default()
    }
    fn subscribe_runtime(&self) -> tokio::sync::watch::Receiver<()> {
        self.runtime.subscribe()
    }
}
async fn setup() -> (
    Arc<HidController>,
    mpsc::UnboundedReceiver<Report>,
    Arc<tokio::sync::Semaphore>,
    Arc<AtomicBool>,
) {
    #[cfg(unix)]
    let c = Arc::new(HidController::new(HidBackendType::None, None));
    #[cfg(not(unix))]
    let c = Arc::new(HidController::new(HidBackendType::None));
    let (tx, rx) = mpsc::unbounded_channel();
    let permits = Arc::new(tokio::sync::Semaphore::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    *c.backend.write().await = Some(Arc::new(Backend {
        reports: tx,
        permits: permits.clone(),
        runtime: tokio::sync::watch::channel(()).0,
        fail: fail.clone(),
    }));
    c.backend_available.store(true, Ordering::Release);
    c.input_control.set_enabled(true);
    c.start_event_worker().await;
    (c, rx, permits, fail)
}
#[tokio::test]
async fn worker_releases_mcp_before_manual_and_discards_queued_generation() {
    let (c, mut reports, permits, _) = setup().await;
    let lease = c.input_control.acquire().await.unwrap();
    let report = types::KeyboardReport {
        modifiers: 1,
        keys: [4, 0, 0, 0, 0, 0],
        ..Default::default()
    };
    let write = c.mcp_keyboard(lease.generation, report);
    tokio::pin!(write);
    assert!(futures::poll!(&mut write).is_pending());
    assert_eq!(
        reports.recv().await.unwrap(),
        Report::Keyboard([1, 0, 4, 0, 0, 0, 0, 0])
    );
    assert!(futures::poll!(&mut write).is_pending()); // USB has not completed.
    c.send_keyboard_from(
        InputSource::Manual(ManualKind::Uu),
        KeyboardEvent::key_down(CanonicalKey::KeyB, Default::default()),
    )
    .await
    .unwrap();
    let stale = c.mcp_mouse(lease.generation, 0, 100, 200, 0);
    tokio::pin!(stale);
    assert!(futures::poll!(&mut stale).is_pending());
    permits.add_permits(2);
    let _ = write.await;
    assert_eq!(reports.recv().await.unwrap(), Report::Keyboard([0; 8]));
    assert_eq!(
        reports.recv().await.unwrap(),
        Report::ManualKey(CanonicalKey::KeyB)
    );
    assert_eq!(stale.await.unwrap_err(), control::PREEMPTED);
    assert!(lease.token.is_cancelled());
    drop(lease);
    // A later manual event acts as a worker barrier; no MCP cleanup can release B.
    c.send_mouse_from(
        InputSource::Manual(ManualKind::Uu),
        MouseEvent::move_abs(1, 2),
    )
    .await
    .unwrap();
    assert_eq!(reports.recv().await.unwrap(), Report::ManualMouse);
    assert!(reports.try_recv().is_err());
    assert_eq!(
        c.input_control.acquire().await.err().unwrap(),
        control::MANUAL
    );
}
#[tokio::test]
async fn failed_write_is_not_acknowledged_as_success_and_disable_releases() {
    let (c, mut reports, permits, fail) = setup().await;
    let lease = c.input_control.acquire().await.unwrap();
    fail.store(true, Ordering::Relaxed);
    let write = c.mcp_keyboard(
        lease.generation,
        types::KeyboardReport {
            keys: [4, 0, 0, 0, 0, 0],
            ..Default::default()
        },
    );
    tokio::pin!(write);
    assert!(futures::poll!(&mut write).is_pending());
    reports.recv().await.unwrap();
    permits.add_permits(1);
    assert!(write
        .await
        .unwrap_err()
        .starts_with("USB HID write failed:"));
    fail.store(false, Ordering::Relaxed);
    c.input_control.set_enabled(false);
    permits.add_permits(1);
    assert_eq!(reports.recv().await.unwrap(), Report::Keyboard([0; 8]));
    assert!(lease.token.is_cancelled());
}
#[tokio::test(start_paused = true)]
async fn held_button_survives_lease_and_expires_after_thirty_seconds() {
    let (c, mut reports, permits, _) = setup().await;
    let lease = c.input_control.acquire().await.unwrap();
    let write = c.mcp_mouse(lease.generation, 1, 10, 20, 0);
    tokio::pin!(write);
    assert!(futures::poll!(&mut write).is_pending());
    assert_eq!(reports.recv().await.unwrap(), Report::Mouse(1, 10, 20));
    permits.add_permits(1);
    write.await.unwrap();
    lease.finish();
    assert_eq!(c.input_control.mcp_buttons(), 1);
    let next = c.input_control.acquire().await.unwrap();
    assert_eq!(c.input_control.mcp_buttons(), 1);
    next.finish();
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(reports.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    permits.add_permits(1);
    assert_eq!(reports.recv().await.unwrap(), Report::Mouse(0, 10, 20));
    assert_eq!(c.input_control.mcp_buttons(), 0);
}
#[tokio::test]
async fn absolute_coalescing_never_crosses_sources() {
    #[cfg(unix)]
    let c = HidController::new(HidBackendType::None, None);
    #[cfg(not(unix))]
    let c = HidController::new(HidBackendType::None);
    for kind in [ManualKind::WebSocket, ManualKind::Uu] {
        c.enqueue_from(
            QueuedHidEvent::Mouse(MouseEvent::move_abs(1, 2)),
            InputSource::Manual(kind),
            None,
        )
        .await
        .unwrap();
    }
    let mut rx = c.hid_rx.lock().await.take().unwrap();
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_ok());
}

#[tokio::test]
async fn cancelled_request_releases_its_report_and_unblocks_the_lane() {
    let (c, mut reports, permits, _) = setup().await;
    let lease = c.input_control.acquire().await.unwrap();
    permits.add_permits(1);
    c.mcp_mouse(lease.generation, 1, 90, 80, 0).await.unwrap();
    assert_eq!(reports.recv().await.unwrap(), Report::Mouse(1, 90, 80));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _lease = lease;
        started_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    started_rx.await.unwrap();
    task.abort();
    let _ = task.await;
    permits.add_permits(1);
    assert_eq!(reports.recv().await.unwrap(), Report::Mouse(0, 90, 80));
    c.input_control.acquire().await.unwrap().finish();
}
