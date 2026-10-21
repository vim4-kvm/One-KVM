//! On-demand screenshots from the current capture pipeline, independent of its codec.
use bytes::Bytes;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[cfg(feature = "vim4")]
pub(crate) mod vim4;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_JPEG_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct SnapshotFrame {
    pub jpeg: Bytes,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum SnapshotTarget {
    Fit {
        max_long_edge: u32,
    },
    /// Coordinates refer to the current output frame, before screenshot scaling.
    Region {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        max_long_edge: u32,
        max_upscale: f32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("No video signal.")]
    NoSignal,
    #[error("Screenshot timed out.")]
    Timeout,
    #[error("Another screenshot is still running.")]
    Busy,
    #[error("Screenshot unavailable: {0}")]
    Unavailable(String),
}

impl SnapshotError {
    pub(crate) fn unavailable(error: impl std::fmt::Display) -> Self {
        Self::Unavailable(error.to_string())
    }
}

pub struct SnapshotService {
    manager: Arc<super::VideoStreamManager>,
    slot: Arc<Semaphore>,
    stopped: tokio_util::sync::CancellationToken,
}

impl SnapshotService {
    pub fn new(manager: Arc<super::VideoStreamManager>) -> Self {
        Self {
            manager,
            slot: Arc::new(Semaphore::new(1)),
            stopped: Default::default(),
        }
    }

    pub async fn source_size(&self) -> Option<(u32, u32)> {
        if self.stopped.is_cancelled() {
            return None;
        }
        #[cfg(feature = "vim4")]
        {
            self.manager.snapshot_source_size().await
        }
        #[cfg(not(feature = "vim4"))]
        {
            None
        }
    }

    /// Wait for a frame dequeued after this request. Zero timeout selects the 3s default.
    /// Cancellation closes the raw tap; an in-progress conversion retains the single slot
    /// until its native resources are released, even if the caller stops waiting.
    pub async fn capture(
        &self,
        target: SnapshotTarget,
        quality: u8,
        timeout: Duration,
    ) -> Result<SnapshotFrame, SnapshotError> {
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| SnapshotError::Busy)?;
        if self.stopped.is_cancelled() {
            return Err(SnapshotError::unavailable("service stopped"));
        }
        let timeout = if timeout.is_zero() {
            DEFAULT_TIMEOUT
        } else {
            timeout
        };
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| SnapshotError::unavailable("timeout is too large"))?;
        let manager = self.manager.clone();
        let cancelled = self.stopped.child_token();
        let _cancel_on_drop = cancelled.clone().drop_guard();
        // Starting a shared pipeline has several await points after spawning its
        // owner. Let startup finish registration even when the caller disappears.
        let work = tokio::spawn(async move {
            #[cfg(feature = "vim4")]
            {
                let size = manager
                    .snapshot_source_size()
                    .await
                    .ok_or(SnapshotError::NoSignal)?;
                geometry(target, size)?;
                if !(1..=100).contains(&quality) {
                    return Err(SnapshotError::unavailable("quality must be 1..100"));
                }
                if cancelled.is_cancelled() {
                    return Err(SnapshotError::Timeout);
                }
                let lease = manager.snapshot_lease().await?;
                let sample = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => return Err(SnapshotError::Timeout),
                    result = lease.tap().sample() => result?,
                };
                let output_height = lease.output_height;
                // Native conversion cannot be interrupted safely. Its owner keeps the permit
                // and lease through teardown, bounding work after timeout or cancellation.
                tokio::task::spawn_blocking(move || {
                    let (_permit, _lease) = (permit, lease);
                    vim4::encode(
                        sample,
                        target,
                        quality,
                        deadline,
                        output_height,
                        crate::platform::vim4::VideoBackend::current(),
                    )
                })
                .await
                .map_err(SnapshotError::unavailable)?
            }
            #[cfg(not(feature = "vim4"))]
            {
                let _ = (permit, manager, target, quality, deadline, cancelled);
                Err(SnapshotError::unavailable("raw screenshots require VIM4"))
            }
        });
        let result = tokio::select! {
            _ = self.stopped.cancelled() => Err(SnapshotError::unavailable("service stopped")),
            result = tokio::time::timeout(timeout, work) => match result {
                Ok(result) => result.map_err(SnapshotError::unavailable)?,
                Err(_) => Err(SnapshotError::Timeout),
            }
        };
        if !self.stopped.is_cancelled()
            && matches!(
                &result,
                Err(SnapshotError::Timeout | SnapshotError::Unavailable(_))
            )
            && self.source_size().await.is_none()
        {
            Err(SnapshotError::NoSignal)
        } else {
            result
        }
    }

    pub async fn shutdown(&self) {
        self.stopped.cancel();
        // Give the native owner its normal budget to return DMA before streaming
        // shuts down. A stuck driver must not hang application shutdown forever.
        if tokio::time::timeout(DEFAULT_TIMEOUT, self.slot.acquire())
            .await
            .is_err()
        {
            tracing::warn!("Snapshot conversion is still releasing resources during shutdown");
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Geometry {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub width: u32,
    pub height: u32,
}

fn geometry(target: SnapshotTarget, (sw, sh): (u32, u32)) -> Result<Geometry, SnapshotError> {
    if sw == 0 || sh == 0 {
        return Err(SnapshotError::NoSignal);
    }
    let (x, y, w, h, edge, upscale) = match target {
        SnapshotTarget::Fit { max_long_edge } => (0, 0, sw, sh, max_long_edge, 1.0),
        SnapshotTarget::Region {
            x,
            y,
            w,
            h,
            max_long_edge,
            max_upscale,
        } => (x, y, w, h, max_long_edge, f64::from(max_upscale)),
    };
    if w == 0
        || h == 0
        || x.checked_add(w).is_none_or(|v| v > sw)
        || y.checked_add(h).is_none_or(|v| v > sh)
        || edge == 0
        || edge > 4096
        || !upscale.is_finite()
        || upscale <= 0.0
        || upscale > 4.0
    {
        return Err(SnapshotError::unavailable(
            "invalid screenshot dimensions or region",
        ));
    }
    let scale = (f64::from(edge) / f64::from(w.max(h))).min(upscale);
    Ok(Geometry {
        x,
        y,
        w,
        h,
        width: (f64::from(w) * scale).round().max(1.0) as u32,
        height: (f64::from(h) * scale).round().max(1.0) as u32,
    })
}

/// Match OutputSettings' GE2D dimensions without its frame-rate filter. The tap is
/// before videorate, whose rewritten PTS cannot establish capture freshness.
pub(crate) fn output_size((width, height): (u32, u32), limit: u32) -> (u32, u32) {
    if limit > 0 && limit < height {
        (
            ((u64::from(width) * u64::from(limit) / u64::from(height)) as u32 & !1).max(2),
            limit,
        )
    } else {
        (width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_slot_and_shutdown_reject_work_before_capture_starts() {
        let manager = super::super::VideoStreamManager::with_webrtc_streamer(
            super::super::Streamer::new(),
            crate::webrtc::WebRtcStreamer::new(),
        );
        let service = SnapshotService::new(manager);
        let held = service.slot.clone().try_acquire_owned().unwrap();
        let target = SnapshotTarget::Fit {
            max_long_edge: 1280,
        };
        assert!(matches!(
            service.capture(target, 80, DEFAULT_TIMEOUT).await,
            Err(SnapshotError::Busy)
        ));
        drop(held);
        service.shutdown().await;
        assert!(service.source_size().await.is_none());
        assert!(matches!(
            service.capture(target, 80, DEFAULT_TIMEOUT).await,
            Err(SnapshotError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn shutdown_waits_for_admitted_work_to_release_its_slot() {
        let manager = super::super::VideoStreamManager::with_webrtc_streamer(
            super::super::Streamer::new(),
            crate::webrtc::WebRtcStreamer::new(),
        );
        let service = SnapshotService::new(manager);
        let held = service.slot.clone().try_acquire_owned().unwrap();
        let mut shutdown = Box::pin(service.shutdown());
        assert!(futures::poll!(&mut shutdown).is_pending());
        assert!(service.stopped.is_cancelled());
        drop(held);
        shutdown.await;
    }
    #[test]
    fn fit_preserves_aspect_and_never_upscales() {
        for (source, expected) in [
            ((3840, 2160), (1280, 720)),
            ((4096, 2160), (1280, 675)),
            ((640, 480), (640, 480)),
            ((1080, 1920), (720, 1280)),
        ] {
            let g = geometry(
                SnapshotTarget::Fit {
                    max_long_edge: 1280,
                },
                source,
            )
            .unwrap();
            assert_eq!((g.width, g.height), expected);
        }
    }
    #[test]
    fn region_is_bounded_in_source_space() {
        let region = |x, w, up| SnapshotTarget::Region {
            x,
            y: 100,
            w,
            h: 100,
            max_long_edge: 1280,
            max_upscale: up,
        };
        let g = geometry(region(100, 200, 4.0), (3840, 2160)).unwrap();
        assert_eq!((g.width, g.height), (800, 400));
        assert!(geometry(region(u32::MAX, 200, 4.0), (3840, 2160)).is_err());
        assert!(geometry(region(3800, 200, 4.0), (3840, 2160)).is_err());
        assert!(geometry(region(100, 0, 4.0), (3840, 2160)).is_err());
        assert!(geometry(region(100, 200, f32::NAN), (3840, 2160)).is_err());
        assert!(matches!(
            geometry(region(0, 100, 1.0), (0, 0)),
            Err(SnapshotError::NoSignal)
        ));
    }
}
