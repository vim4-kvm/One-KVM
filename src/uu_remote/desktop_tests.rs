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
    _file: Arc<RTCDataChannel>,
    incoming: mpsc::UnboundedReceiver<(u8, Vec<u8>)>,
    packets: mpsc::UnboundedReceiver<()>,
    _directory: tempfile::TempDir,
}

impl Viewer {
    async fn connect(desktop: Arc<DesktopCoordinator>, tool: bool) -> Self {
        Self::connect_kind(desktop, if tool { 8 } else { 1 }).await
    }

    async fn connect_kind(desktop: Arc<DesktopCoordinator>, capture_type: u64) -> Self {
        Self::connect_configured(desktop, capture_type, None, false).await
    }

    async fn connect_configured(
        desktop: Arc<DesktopCoordinator>,
        capture_type: u64,
        configured_video: Option<Arc<VideoStreamManager>>,
        main10: bool,
    ) -> Self {
        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
        if main10 {
            media
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: RTCRtpCodecCapability {
                            mime_type: "video/H265".into(),
                            clock_rate: 90_000,
                            sdp_fmtp_line: "profile-id=2;tier-flag=0;level-id=180;tx-mode=SRST"
                                .into(),
                            ..Default::default()
                        },
                        payload_type: 123,
                        ..Default::default()
                    },
                    RTPCodecType::Video,
                )
                .unwrap();
        }
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
        let file = pc
            .create_data_channel("FILE_DATA_CHANNEL", None)
            .await
            .unwrap();
        let opened = Arc::new(Notify::new());
        for (kind, channel) in [(0, &control), (1, &binary), (2, &file)] {
            let out = out.clone();
            channel.on_message(Box::new(move |m| {
                let out = out.clone();
                Box::pin(async move {
                    let _ = out.send((kind, m.data.to_vec()));
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
        let video = if let Some(video) = configured_video {
            video
        } else {
            desktop
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
                })
        };
        let (out, _signals) = mpsc::channel(64);
        let host = Peer::new(
            PeerDeps {
                system: crate::config::UuRemoteSystem::Windows,
                reverse_scroll: false,
                video_codec: if main10 {
                    crate::config::UuVideoCodec::H265
                } else {
                    crate::config::UuVideoCodec::H264
                },
                video,
                hid: Arc::new(HidController::new(crate::hid::HidBackendType::None, None)),
                audio: Arc::new(AudioController::default()),
                signaling: Arc::new(SignalingClient::with_test_sender(out)),
                terminal,
                desktop,
                #[cfg(target_os = "linux")]
                files: super::super::files::storage::Storage::new(directory.path().join("files")),
            },
            SessionIce::host_only(),
            &soac,
            capture_type,
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
                if [
                    control.ready_state(),
                    binary.ready_state(),
                    file.ready_state(),
                ]
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
            _file: file,
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
                if terminal == 1 {
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

    #[cfg(feature = "vim4")]
    async fn set_hdr(&mut self, enabled: bool, id: u64) {
        let mut header = Vec::new();
        wire::put_varint(1, id, &mut header);
        let mut setting = Vec::new();
        wire::put_varint(11, u64::from(enabled), &mut setting);
        let mut request = Vec::new();
        wire::put_bytes(1, &header, &mut request);
        wire::put_bytes(2, &setting, &mut request);
        self.control
            .send(&Bytes::from(wire::envelope(
                id,
                0,
                wire::which::RPC_REQUEST,
                &request,
            )))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(12), async {
            while let Some((channel, bytes)) = self.incoming.recv().await {
                if channel != 0 {
                    continue;
                }
                let fields = wire::decode(&bytes).unwrap();
                let Some(response) = wire::message(&fields, wire::which::RPC_RESPONSE) else {
                    continue;
                };
                let response = wire::decode(response).unwrap();
                let header = wire::decode(wire::message(&response, 1).unwrap()).unwrap();
                if wire::number(&header, 1) == Some(id) {
                    return;
                }
            }
            panic!("CONTROL closed before HDR acknowledgement");
        })
        .await
        .unwrap();
    }
}

/// Run as the service user, with its GStreamer environment and all KVM services stopped.
#[cfg(feature = "vim4")]
#[tokio::test]
#[ignore = "requires exclusive VIM4 capture and encoder access"]
async fn live_uu_hdr_switch_and_takeover_restore_the_default() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,one_kvm=info")
        .try_init();
    assert_eq!(std::env::var("VIM4_HARDWARE_TEST").as_deref(), Ok("1"));
    assert!(crate::platform::vim4::hdr_remote_available());
    gstreamer::init().unwrap();
    let streamer = crate::webrtc::WebRtcStreamer::new();
    streamer
        .set_capture_device(crate::platform::vim4::VIDEO_DEVICE.into(), 80, None)
        .await;
    streamer
        .set_vim4_output(crate::platform::vim4::OutputSettings {
            height: 720,
            fps: 15,
            ..Default::default()
        })
        .await
        .unwrap();
    let _initial_pipeline = streamer.ensure_video_pipeline_for_external().await.unwrap();
    let video = VideoStreamManager::with_webrtc_streamer(
        crate::video::streamer::Streamer::new(),
        streamer.clone(),
    );
    let owner = Arc::new(DesktopCoordinator::default());
    let mut first = Viewer::connect_configured(owner.clone(), 1, Some(video.clone()), true).await;
    assert!(first.host.shared.hdr_control_available());
    for (id, hdr) in [(70, true), (71, false), (72, true)] {
        first.set_hdr(hdr, id).await;
        let config = video.get_encoding_config().await.unwrap();
        assert_eq!(config.output.hdr, hdr);
        assert_eq!(first.pc.connection_state(), PcState::Connected);
        let mut frames = video.subscribe_encoded_frames().await.unwrap();
        video.request_keyframe().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = frames.recv().await {
                if let Some(compatible) =
                    crate::video::codec::h265_bitstream::conforms_to_profile(&frame.data, 2)
                {
                    assert!(compatible);
                    if hdr {
                        assert_eq!(
                            crate::video::codec::h265_bitstream::profile_id(&frame.data),
                            Some(2)
                        );
                    }
                    return;
                }
            }
            panic!("Encoder stopped");
        })
        .await
        .unwrap();
        println!("UU HDR={hdr}: compatible keyframe and same peer confirmed");
    }
    let mut second = Viewer::connect_configured(owner, 1, Some(video.clone()), true).await;
    assert!(!first.host.is_desktop());
    let config = video.get_encoding_config().await.unwrap();
    assert!(
        !config.output.hdr,
        "Takeover inherited the previous viewer's temporary HDR"
    );
    second.set_hdr(true, 73).await;
    first.close().await;
    assert!(
        video.get_encoding_config().await.unwrap().output.hdr,
        "Closing the old viewer changed the new owner's setting"
    );
    second.close().await;
    let config = video.get_encoding_config().await.unwrap();
    assert!(!config.output.hdr);
    streamer
        .ensure_video_pipeline_for_external()
        .await
        .unwrap()
        .stop_and_wait(Duration::from_secs(5))
        .await
        .unwrap();
    println!("UU takeover/disconnect restored SDR default and released Main10 override");
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
                None,
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
        })
        .await
        .expect("Closed peer retained its application state");
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn file_tool_replies_on_file_channel_before_and_after_desktop_promotion() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut viewer = Viewer::connect_kind(Arc::new(DesktopCoordinator::default()), 5).await;
        assert!(viewer.host.is_tool() && !viewer.host.is_terminal());
        for promoted in [false, true] {
            if promoted {
                viewer.capture().await;
            }
            let mut task = Vec::new();
            wire::put_varint(1, 33, &mut task);
            let mut directory = Vec::new();
            wire::put_bytes(1, &task, &mut directory);
            wire::put_bytes(2, b"~/", &mut directory);
            let mut ftp = Vec::new();
            wire::put_bytes(10, &directory, &mut ftp);
            let mut header = Vec::new();
            wire::put_varint(1, 99, &mut header);
            let mut rpc = Vec::new();
            wire::put_bytes(1, &header, &mut rpc);
            wire::put_bytes(8, &ftp, &mut rpc);
            viewer
                .control
                .send(&Bytes::from(wire::envelope(1, 0, 21, &rpc)))
                .await
                .unwrap();
            loop {
                let (channel, bytes) = viewer.incoming.recv().await.unwrap();
                let Ok(message) = wire::decode(&bytes) else {
                    continue;
                };
                let Some(rpc) = wire::message(&message, 22) else {
                    continue;
                };
                let rpc = wire::decode(rpc).unwrap();
                let header = wire::decode(wire::message(&rpc, 1).unwrap()).unwrap();
                if wire::number(&header, 1) != Some(99) {
                    continue;
                }
                assert_eq!(channel, 2, "FTP response escaped onto CONTROL");
                let ftp = wire::decode(wire::message(&rpc, 4).unwrap()).unwrap();
                let directory = wire::decode(wire::message(&ftp, 7).unwrap()).unwrap();
                assert_eq!(wire::number(&directory, 4), Some(1));
                let mut list = Vec::new();
                flate2::read::ZlibDecoder::new(wire::message(&directory, 5).unwrap())
                    .read_to_end(&mut list)
                    .unwrap();
                assert!(list.windows(b"/files".len()).any(|p| p == b"/files"));
                break;
            }
        }
        viewer.close().await;
    })
    .await
    .unwrap();
}
