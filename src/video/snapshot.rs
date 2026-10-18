//! Integration stub. The snapshot worktree supplies the capture implementation.
use super::VideoStreamManager;
use bytes::Bytes;
use std::{sync::Arc, time::Duration};

pub struct SnapshotFrame {
    pub jpeg: Bytes,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
}

pub enum SnapshotTarget {
    Fit {
        max_long_edge: u32,
    },
    Region {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        max_long_edge: u32,
        max_upscale: f32,
    },
}

#[derive(Debug)]
pub enum SnapshotError {
    NoSignal,
    Timeout,
    Busy,
    Unavailable(String),
}

pub struct SnapshotService {
    stream: Arc<VideoStreamManager>,
}

impl SnapshotService {
    pub fn new(stream: Arc<VideoStreamManager>) -> Self {
        Self { stream }
    }

    pub async fn source_size(&self) -> Option<(u32, u32)> {
        let info = self.stream.get_video_info().await;
        info.resolution
            .filter(|(w, h)| info.online && *w > 0 && *h > 0)
    }

    pub async fn capture(
        &self,
        _target: SnapshotTarget,
        _quality: u8,
        _timeout: Duration,
    ) -> Result<SnapshotFrame, SnapshotError> {
        Err(SnapshotError::Unavailable("not implemented".into()))
    }
}
