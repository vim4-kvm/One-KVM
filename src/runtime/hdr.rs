//! The board file owns the default. UU settings remain a runtime-only lease.
use crate::config::ConfigStore;
use crate::state::AppState;
use crate::video::VideoStreamManager;
use std::sync::Arc;
use std::time::Duration;

async fn synchronize(
    store: &ConfigStore,
    video: &VideoStreamManager,
    hdr: bool,
) -> crate::error::Result<()> {
    if store.get().video.output.hdr != hdr {
        store.update(|config| config.video.output.hdr = hdr).await?;
    }
    // A live UU viewer owns its current HDR setting until disconnect.
    // Changing the saved default must not interrupt that viewer.
    if video.uu_negotiated_codec().await.is_none()
        && video
            .vim4_output_settings()
            .await
            .is_some_and(|output| output.hdr != hdr)
    {
        video.restore_vim4_hdr(hdr).await;
    }
    Ok(())
}

pub(super) fn watch(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        loop {
            timer.tick().await;
            let Some(hdr) = crate::platform::vim4::global_hdr_default() else {
                continue;
            };
            let hdr = hdr && crate::platform::vim4::hdr_remote_available();
            let Ok(_guard) = state.config_apply_locks.video.try_lock() else {
                continue;
            };
            if let Err(error) = synchronize(&state.config, &state.stream_manager, hdr).await {
                tracing::warn!(%error, "Cannot synchronize the board HDR default");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::traits::VideoOutput;

    #[tokio::test]
    async fn global_default_changes_preserve_a_uu_override_and_unrelated_output_settings() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::db::open_database_pool(directory.path())
            .await
            .unwrap();
        let store = ConfigStore::new(db.clone_pool());
        store.load().await.unwrap();
        store
            .update(|config| {
                config.video.output.height = 720;
                config.video.output.fps = 15;
            })
            .await
            .unwrap();
        let webrtc = crate::webrtc::WebRtcStreamer::new();
        webrtc
            .set_vim4_output(store.get().video.output)
            .await
            .unwrap();
        let video = VideoStreamManager::with_webrtc_streamer(
            crate::video::streamer::Streamer::new(),
            webrtc.clone(),
        );
        let events = Arc::new(crate::events::EventBus::new());
        let mut receiver = events.subscribe();
        video.set_event_bus(events).await;
        synchronize(&store, &video, true).await.unwrap();
        assert!(webrtc.vim4_output_settings().await.unwrap().hdr);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            crate::events::SystemEvent::StreamConfigChanging { .. }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            crate::events::SystemEvent::WebRTCReady { .. }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            crate::events::SystemEvent::StreamConfigApplied { .. }
        ));
        synchronize(&store, &video, true).await.unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "unchanged default must not reconnect viewers"
        );
        video
            .set_uu_negotiated_codec(Some(crate::video::codec::VideoCodecType::H265))
            .await;
        synchronize(&store, &video, false).await.unwrap();
        assert!(!store.get().video.output.hdr);
        assert!(
            webrtc.vim4_output_settings().await.unwrap().hdr,
            "global default interrupted the UU override"
        );
        assert!(
            receiver.try_recv().is_err(),
            "UU override must not reconnect viewers"
        );
        video.set_uu_negotiated_codec(None).await;
        synchronize(&store, &video, false).await.unwrap();
        let output = webrtc.vim4_output_settings().await.unwrap();
        assert!(!output.hdr);
        assert_eq!((output.height, output.fps), (720, 15));
    }
}
