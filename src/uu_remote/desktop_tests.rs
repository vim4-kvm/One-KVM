use super::*;
use tokio::sync::{mpsc, Notify};
use webrtc::rtp_transceiver::{
    rtp_transceiver_direction::RTCRtpTransceiverDirection, RTCRtpTransceiverInit,
};

struct Viewer {
    host: Peer,
    pc: Arc<RTCPeerConnection>,
    control: Arc<RTCDataChannel>,
    binary: Arc<RTCDataChannel>,
    incoming: mpsc::UnboundedReceiver<(bool, Vec<u8>)>,
    packets: mpsc::UnboundedReceiver<()>,
    _directory: tempfile::TempDir,
}

impl Viewer {
    async fn connect(desktop: Arc<DesktopCoordinator>, tool: bool) -> Self {
        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
        let pc = Arc::new(
            APIBuilder::new()
                .with_media_engine(media)
                .build()
                .new_peer_connection(Default::default())
                .await
                .unwrap(),
        );
        for kind in [RTPCodecType::Video, RTPCodecType::Audio] {
            pc.add_transceiver_from_kind(
                kind,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: Vec::new(),
                }),
            )
            .await
            .unwrap();
        }
        let (packets, received_packets) = mpsc::unbounded_channel();
        pc.on_track(Box::new(move |track, _, _| {
            let packets = packets.clone();
            Box::pin(async move {
                if track.kind() == RTPCodecType::Video {
                    while track.read_rtp().await.is_ok() {
                        let _ = packets.send(());
                    }
                }
            })
        }));
        let (out, incoming) = mpsc::unbounded_channel();
        let control = pc
            .create_data_channel("CONTROL_DATA_CHANNEL", None)
            .await
            .unwrap();
        let binary = pc
            .create_data_channel("BINARY_DATA_CHANNEL", None)
            .await
            .unwrap();
        let opened = Arc::new(Notify::new());
        for (is_terminal, channel) in [(false, &control), (true, &binary)] {
            let out = out.clone();
            channel.on_message(Box::new(move |m| {
                let out = out.clone();
                Box::pin(async move {
                    let _ = out.send((is_terminal, m.data.to_vec()));
                })
            }));
            let opened = opened.clone();
            channel.on_open(Box::new(move || {
                let opened = opened.clone();
                Box::pin(async move {
                    opened.notify_one();
                })
            }));
        }
        let offer = pc.create_offer(None).await.unwrap();
        let mut gathered = pc.gathering_complete_promise().await;
        pc.set_local_description(offer).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), gathered.recv())
            .await
            .unwrap();
        let offer = pc.local_description().await.unwrap();
        let soac: SoacMessage = serde_json::from_value(serde_json::json!({
            "type":"offer", "sdp":offer.sdp, "ice_id":"desktop-regression"
        }))
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let terminal = super::super::terminal::Registry::new(directory.path().join("terminal"));
        terminal.start();
        let video = desktop
            .0
            .lock()
            .await
            .upgrade()
            .map(|owner| owner.deps.video.clone())
            .unwrap_or_else(|| {
                VideoStreamManager::with_webrtc_streamer(
                    crate::video::streamer::Streamer::new(),
                    crate::webrtc::WebRtcStreamer::new(),
                )
            });
        let (out, _signals) = mpsc::channel(64);
        let host = Peer::new(
            PeerDeps {
                system: crate::config::UuRemoteSystem::Windows,
                reverse_scroll: false,
                video_codec: crate::config::UuVideoCodec::H264,
                video,
                hid: Arc::new(HidController::new(crate::hid::HidBackendType::None, None)),
                audio: Arc::new(AudioController::default()),
                signaling: Arc::new(SignalingClient::with_test_sender(out)),
                terminal,
                desktop,
            },
            SessionIce::from_header(&serde_json::json!({"iceServers":[]})),
            &soac,
            tool,
        )
        .await
        .unwrap();
        let mut gathered = host.pc.gathering_complete_promise().await;
        host.handle_offer(&soac).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), gathered.recv())
            .await
            .unwrap();
        pc.set_remote_description(host.pc.local_description().await.unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let changed = opened.notified();
                if [control.ready_state(), binary.ready_state()]
                    .iter()
                    .all(|s| *s == RTCDataChannelState::Open)
                {
                    break;
                }
                changed.await;
            }
        })
        .await
        .unwrap();
        Self {
            host,
            pc,
            control,
            binary,
            incoming,
            packets: received_packets,
            _directory: directory,
        }
    }

    async fn terminal_list(&mut self, id: u32) {
        use super::super::terminal::frame;
        self.binary
            .send(&Bytes::from(frame::encode(frame::LIST_REQ, id, &[])))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some((terminal, bytes)) = self.incoming.recv().await {
                if terminal {
                    let reply = frame::decode(&bytes).unwrap();
                    assert_eq!((reply.kind, reply.session), (frame::LIST_RESP, id));
                    return;
                }
            }
            panic!("Terminal disconnected");
        })
        .await
        .unwrap();
    }

    async fn capture(&mut self) {
        let mut role = self.host.shared.desktop.subscribe();
        self.control
            .send(&Bytes::from(wire::envelope(1, 0, 3, &[8, 4])))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            while !*role.borrow_and_update() {
                role.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    async fn close(self) {
        self.host.close().await;
        self.pc.close().await.unwrap();
    }
}

#[tokio::test]
async fn tool_promotion_is_idempotent_and_keeps_terminal_and_video_on_the_same_peer() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let owner = Arc::new(DesktopCoordinator::default());
        let mut viewer = Viewer::connect(owner, true).await;
        assert!(!viewer.host.is_desktop());
        viewer.terminal_list(41).await;
        let pc = viewer.host.pc.clone();
        viewer.capture().await;
        assert!(Arc::ptr_eq(&pc, &viewer.host.pc));
        let task = viewer
            .host
            .shared
            .desktop_task
            .lock()
            .as_ref()
            .unwrap()
            .id();
        viewer.capture().await;
        // A terminal reply is a processing barrier on the still-open tool transport.
        viewer.terminal_list(42).await;
        assert_eq!(
            viewer
                .host
                .shared
                .desktop_task
                .lock()
                .as_ref()
                .unwrap()
                .id(),
            task
        );
        assert_eq!(viewer.host.pc.get_senders().await.len(), 2);
        let media = viewer.host.shared.media.get().unwrap();
        media
            .video
            .write(
                &Sample {
                    data: Bytes::from_static(b"\0\0\0\x01\x65\x88\x84"),
                    duration: Duration::from_millis(16),
                    ..Default::default()
                },
                &mut HevcPackets::new(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), viewer.packets.recv())
            .await
            .unwrap()
            .unwrap();
        viewer.terminal_list(43).await;
        let shared = viewer.host.shared.clone();
        let connection = shared.terminal_conn.lock().as_ref().unwrap().clone();
        viewer.close().await;
        assert!(!*shared.desktop.borrow());
        assert!(shared.desktop_task.lock().is_none());
        tokio::time::timeout(Duration::from_secs(1), connection.cancelled())
            .await
            .unwrap();
        assert_eq!(shared.deps.video.uu_negotiated_codec().await, None);
        let lifetime = Arc::downgrade(&shared);
        drop(shared);
        drop(pc);
        drop(connection);
        tokio::time::timeout(Duration::from_secs(2), async {
            while lifetime.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        }).await.expect("Closed peer retained its application state");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn desktop_takeover_demotes_tools_without_closing_their_terminal() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let owner = Arc::new(DesktopCoordinator::default());
        let first = Viewer::connect(owner.clone(), false).await;
        let mut tool = Viewer::connect(owner.clone(), true).await;
        tool.capture().await;
        assert!(!first.host.is_desktop());
        assert_eq!(first.host.pc.connection_state(), PcState::Closed);
        let last = Viewer::connect(owner, false).await;
        assert!(!tool.host.is_desktop());
        assert!(tool.host.connected());
        tool.terminal_list(51).await;
        tool.capture().await;
        assert_eq!(last.host.pc.connection_state(), PcState::Closed);
        tool.terminal_list(52).await;
        first.close().await;
        last.close().await;
        // Closing an obsolete desktop cannot clear the current owner's codec.
        assert_eq!(
            tool.host.video.uu_negotiated_codec().await,
            Some(VideoCodecType::H264)
        );
        tool.close().await;
    })
    .await
    .unwrap();
}
