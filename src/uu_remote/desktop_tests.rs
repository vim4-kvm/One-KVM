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
    packets: mpsc::UnboundedReceiver<bool>,
    _directory: tempfile::TempDir,
}

impl Viewer {
    async fn connect(desktop: Arc<DesktopCoordinator>, tool: bool) -> Self {
        Self::connect_kind(desktop, if tool { 8 } else { 1 }).await
    }

    async fn connect_kind(desktop: Arc<DesktopCoordinator>, capture_type: u64) -> Self {
        Self::connect_configured(
            desktop,
            capture_type,
            None,
            false,
            ViewerCapabilities::default(),
        )
        .await
    }

    async fn connect_configured(
        desktop: Arc<DesktopCoordinator>,
        capture_type: u64,
        configured_video: Option<Arc<VideoStreamManager>>,
        main10: bool,
        capabilities: ViewerCapabilities,
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
                    while let Ok((packet, _)) = track.read_rtp().await {
                        let _ = packets.send(packet.header.marker);
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
                video_codec: if main10 || capabilities.supports_main10(1280, 720) {
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
            capabilities,
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
        let mut setting = Vec::new();
        wire::put_varint(11, u64::from(enabled), &mut setting);
        self.set_capture_setting(setting, id).await;
    }

    #[cfg(feature = "vim4")]
    async fn set_capture_setting(&mut self, setting: Vec<u8>, id: u64) {
        let mut header = Vec::new();
        wire::put_varint(1, id, &mut header);
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

fn native_main10_capabilities() -> ViewerCapabilities {
    ViewerCapabilities::from_header(&serde_json::json!({
        "streamer_data": serde_json::json!({"device_capability": {
            "display_info": [{"hdr":0}],
            "video_codec_capability": [{"video_codec":2,"bit_depth":10,
                "chroma_sampling":1,"width":3840,"height":2160}]
        }}).to_string()
    }))
}

#[tokio::test]
async fn native_main10_capability_keeps_main_sdp_and_transport_binding() {
    let viewer = Viewer::connect_configured(
        Arc::new(DesktopCoordinator::default()),
        1,
        None,
        false,
        native_main10_capabilities(),
    )
    .await;
    let answer = viewer.host.pc.local_description().await.unwrap().sdp;
    assert!(!answer.contains("profile-id=2"));
    assert_eq!(viewer.host.shared.codec, VideoCodecType::H265);
    assert_eq!(viewer.host.shared.hevc_receive_profile, 2);
    assert!(viewer.host.shared.hdr_display_available);
    assert_eq!(viewer.pc.connection_state(), PcState::Connected);
    viewer.host.close().await;
    viewer.pc.close().await.unwrap();
}

/// Requires 3840x2160 HDR10/PQ HDMI input; run as the service user with its GStreamer
/// environment and all KVM services stopped.
#[cfg(feature = "vim4")]
#[tokio::test]
#[ignore = "requires exclusive VIM4 capture and encoder access"]
async fn live_uu_hdr_switch_and_takeover_restore_the_default() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,one_kvm=info")
        .try_init();
    assert_eq!(std::env::var("VIM4_HARDWARE_TEST").as_deref(), Ok("1"));
    assert!(crate::platform::vim4::hdr_remote_available());
    assert!(
        crate::platform::vim4::pq_input(),
        "HDR10/PQ HDMI input is required"
    );
    let default_hdr = crate::platform::vim4::global_hdr_default().unwrap_or(false);
    gstreamer::init().unwrap();
    let streamer = crate::webrtc::WebRtcStreamer::new();
    streamer
        .set_capture_device(crate::platform::vim4::VIDEO_DEVICE.into(), 80, None)
        .await;
    streamer
        .set_vim4_output(crate::platform::vim4::OutputSettings {
            height: 0,
            fps: 60,
            hdr: default_hdr,
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
    let mut first = Viewer::connect_configured(
        owner.clone(),
        1,
        Some(video.clone()),
        false,
        native_main10_capabilities(),
    )
    .await;
    let answer = first.host.pc.local_description().await.unwrap().sdp;
    assert!(
        !answer.contains("profile-id=2"),
        "native capability must not rewrite SDP"
    );
    assert_eq!(first.host.shared.hevc_receive_profile, 2);
    assert!(first.host.shared.hdr_control_available());
    assert!(!video.vim4_output_settings().await.unwrap().hdr);
    for (id, hdr) in [(69, false), (70, true), (71, false), (72, true)] {
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
                    assert_eq!(
                        crate::video::codec::h265_bitstream::profile_id(&frame.data),
                        Some(if hdr { 2 } else { 1 })
                    );
                    return;
                }
            }
            panic!("Encoder stopped");
        })
        .await
        .unwrap();
        // A single keyframe does not prove that capture, encoding and RTP
        // delivery keep running after switching the hardware encoder SDK.
        while first.packets.try_recv().is_ok() {}
        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut frames = 0;
            while frames < 240 {
                if first.packets.recv().await.expect("Video receiver closed") {
                    frames += 1;
                }
            }
        })
        .await
        .expect("Did not deliver 240 complete RTP frames within 30 seconds");
        println!(
            "UU HDR={hdr}: 240 complete RTP frames in {:?}, same peer",
            started.elapsed()
        );
    }
    // A native MacBook window produced this 3022x1700 output. The HEVC SDK
    // used to pass the unaligned width to firmware and hang on the first frame.
    let mut setting = Vec::new();
    wire::put_varint(16, 3024, &mut setting);
    wire::put_varint(17, 1700, &mut setting);
    first.set_capture_setting(setting, 74).await;
    for (id, hdr) in [(74, true), (75, false), (76, true)] {
        if id != 74 {
            first.set_hdr(hdr, id).await;
        }
        let config = video.get_encoding_config().await.unwrap();
        assert_eq!(
            config.resolution,
            crate::video::format::Resolution::new(if hdr { 3022 } else { 3008 }, 1700)
        );
        assert_eq!(config.output.hdr, hdr);
        while first.packets.try_recv().is_ok() {}
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut frames = 0;
            while frames < 30 {
                if first.packets.recv().await.expect("Video receiver closed") {
                    frames += 1;
                }
            }
        })
        .await
        .expect("Scaled video stopped before delivering 30 complete RTP frames");
        println!("UU scaled HDR={hdr}: 30 complete RTP frames, same peer");
    }
    let mut second = Viewer::connect_configured(
        owner,
        1,
        Some(video.clone()),
        true,
        ViewerCapabilities::default(),
    )
    .await;
    assert!(!first.host.is_desktop());
    let output = video.vim4_output_settings().await.unwrap();
    assert_eq!(
        output.hdr, false,
        "New desktop must start in SDR before the viewer requests HDR"
    );
    second.set_hdr(!default_hdr, 73).await;
    first.close().await;
    assert_eq!(
        video.get_encoding_config().await.unwrap().output.hdr,
        !default_hdr,
        "Closing the old viewer changed the new owner's setting"
    );
    second.close().await;
    // The final subscriber may stop the encoder. Check the retained output
    // preference, which must also restore correctly while the pipeline is idle.
    let output = video.vim4_output_settings().await.unwrap();
    assert_eq!(output.hdr, default_hdr);
    assert_eq!((output.height, output.fps), (1700, 60));
    video.stop().await.unwrap();
    println!("UU takeover/disconnect restored HDR default={default_hdr} and released the override");
}

/// Requires a 4K60 HDMI input; run with the service environment and all KVM
/// services stopped. No EDID, source mode or persistent output setting is changed.
#[cfg(feature = "vim4")]
#[tokio::test]
#[ignore = "requires exclusive VIM4 capture and encoder access"]
async fn live_uu_resolution_with_saved_hdr_on_an_sdr_viewer() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,one_kvm=info")
        .try_init();
    assert_eq!(std::env::var("VIM4_HARDWARE_TEST").as_deref(), Ok("1"));
    let native = crate::platform::vim4::signal().unwrap().0;
    assert_eq!((native.width, native.height), (3840, 2160));
    gstreamer::init().unwrap();
    let streamer = crate::webrtc::WebRtcStreamer::new();
    streamer
        .set_capture_device(crate::platform::vim4::VIDEO_DEVICE.into(), 80, None)
        .await;
    streamer
        .set_vim4_output(crate::platform::vim4::OutputSettings {
            fps: 60,
            hdr: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let video =
        VideoStreamManager::with_webrtc_streamer(crate::video::streamer::Streamer::new(), streamer);
    let capabilities = ViewerCapabilities::from_header(&serde_json::json!({
        "streamer_data": serde_json::json!({"device_capability": {
            "display_info": [{"hdr": -1}, {"hdr": -1}],
            "video_codec_capability": [{"video_codec": 2, "bit_depth": 10,
                "chroma_sampling": 1, "width": 3840, "height": 2160}]
        }}).to_string()
    }));
    let mut viewer = Viewer::connect_configured(
        Arc::new(DesktopCoordinator::default()),
        1,
        Some(video.clone()),
        false,
        capabilities,
    )
    .await;
    assert!(!viewer.host.shared.hdr_display_available);
    for (id, width, height) in [(81, 1920, 1080), (82, 1280, 720), (83, 3840, 2160)] {
        let mut setting = Vec::new();
        // The official client repeats a saved HDR preference with its size.
        wire::put_varint(11, 1, &mut setting);
        wire::put_varint(16, width, &mut setting);
        wire::put_varint(17, height, &mut setting);
        wire::put_varint(18, 60, &mut setting);
        viewer.set_capture_setting(setting, id).await;
        let config = video.get_encoding_config().await.unwrap();
        assert_eq!(
            (config.resolution.width, config.resolution.height),
            (width as u32, height as u32)
        );
        assert!(!config.output.hdr);
        assert_eq!(viewer.pc.connection_state(), PcState::Connected);
        while viewer.packets.try_recv().is_ok() {}
        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut frames = 0;
            while frames < 120 {
                if viewer.packets.recv().await.expect("Video receiver closed") {
                    frames += 1;
                }
            }
        })
        .await
        .expect("Resolution switch stopped video delivery");
        println!(
            "UU SDR viewer: {width}x{height}, 120 RTP frames in {:?}",
            started.elapsed()
        );
    }
    viewer.close().await;
    video.stop().await.unwrap();
}

#[tokio::test]
async fn capture_desktop_restarts_an_ended_task_without_replacing_the_tool_peer() {
    // Switching codec tries to open an unavailable capture device. Video returns
    // early while the audio subscriber would otherwise continue waiting forever.
    let output = crate::webrtc::WebRtcStreamer::with_config(crate::webrtc::WebRtcStreamerConfig {
        video_codec: VideoCodecType::H265,
        ..Default::default()
    });
    let directory = tempfile::tempdir().unwrap();
    output
        .set_capture_device(directory.path().join("missing-video-device"), 80, None)
        .await;
    let video =
        VideoStreamManager::with_webrtc_streamer(crate::video::streamer::Streamer::new(), output);
    let mut viewer = Viewer::connect_configured(
        Arc::new(DesktopCoordinator::default()),
        8,
        Some(video),
        false,
        ViewerCapabilities::default(),
    )
    .await;
    viewer.capture().await;
    viewer.terminal_list(51).await;
    let shared = viewer.host.shared.clone();
    let ended_id = shared.desktop_task.lock().as_ref().unwrap().id();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !shared.desktop_task.lock().as_ref().unwrap().is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("audio masked the failed video task");
    viewer.capture().await;
    viewer.terminal_list(52).await;
    let restarted_id = shared.desktop_task.lock().as_ref().unwrap().id();
    assert_ne!(
        restarted_id, ended_id,
        "capture_desktop left the video task stopped"
    );
    assert_eq!(viewer.pc.connection_state(), PcState::Connected);
    assert_eq!(viewer.host.pc.get_senders().await.len(), 2);
    assert_eq!(
        shared.deps.video.uu_negotiated_codec().await,
        Some(shared.codec)
    );
    viewer.close().await;
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
