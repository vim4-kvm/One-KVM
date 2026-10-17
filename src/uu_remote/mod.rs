//! UU Remote (网易UU远程) host: the official Windows/Mac/mobile viewers control the
//! KVM target through NetEase's cloud signaling and a WebRTC peer per connection.
//! The HDMI input is the desktop; keyboard and mouse go through One-KVM HID.

pub mod cloud;
mod ice;
mod input;
mod metrics;
mod peer;
mod signaling;
mod status;
mod video_metrics;
mod wire;

pub use status::UuSessionStatus;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use typeshare::typeshare;

use self::cloud::{api_data, CloudClient, CloudError, Credentials};
use self::peer::{Peer, PeerDeps};
use self::signaling::{SignalingClient, SignalingEvent, SoacMessage};
use crate::audio::AudioController;
use crate::config::{UuRemoteConfig, UuRemoteSystem};
use crate::hid::HidController;
use crate::video::VideoStreamManager;

/// Idle rooms are renewed like the official host; renewal waits while a viewer is connected.
const ROOM_RENEWAL: Duration = Duration::from_secs(900);
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// One QR code stays valid for about five minutes; stop offering new ones after this.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// ConnectOptions.capture_type: 0 unknown, 1 desktop, 2 window. File transfer (5),
/// terminal (8) and port mapping (9) would expose the KVM board itself, not the target.
const MAX_DESKTOP_CAPTURE_TYPE: u64 = 2;

#[typeshare]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UuRemoteServiceState {
    Stopped,
    /// Enabled but no account is bound; the web UI shows the QR login.
    WaitingLogin,
    Connecting,
    Online,
    Error,
}

#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct UuRemoteStatus {
    pub state: UuRemoteServiceState,
    pub error: Option<String>,
    pub logged_in: bool,
    pub nickname: Option<String>,
    pub device_id: Option<String>,
    pub connection_count: u32,
    pub video_codec: Option<crate::config::UuVideoCodec>,
    pub sessions: Vec<UuSessionStatus>,
}

#[typeshare]
#[derive(Debug, Serialize)]
pub struct UuVideoOutput {
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UuLoginStage {
    Idle,
    Waiting,
    Scanned,
    Success,
    Failed,
}

/// QR login progress. `qr_url` is a login credential: it is only returned to the
/// authenticated web session and never logged.
#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct UuLoginState {
    pub stage: UuLoginStage,
    pub qr_url: Option<String>,
    pub message: Option<String>,
}

impl UuLoginState {
    fn idle() -> Self {
        Self {
            stage: UuLoginStage::Idle,
            qr_url: None,
            message: None,
        }
    }
}

struct Inner {
    credentials_path: PathBuf,
    profile: Mutex<UuRemoteConfig>,
    credentials: Mutex<Credentials>,
    /// Woken when credentials change (login or logout).
    credentials_changed: Notify,
    state: Mutex<(UuRemoteServiceState, Option<String>)>,
    login: watch::Sender<UuLoginState>,
    peers: tokio::sync::Mutex<HashMap<String, Arc<Peer>>>,
    video: Arc<VideoStreamManager>,
    hid: Arc<HidController>,
    audio: Arc<AudioController>,
}

pub struct UuRemoteService {
    inner: Arc<Inner>,
    run_task: Mutex<Option<JoinHandle<()>>>,
    login_task: Mutex<Option<JoinHandle<()>>>,
}

impl UuRemoteService {
    pub fn new(
        data_dir: PathBuf,
        video: Arc<VideoStreamManager>,
        hid: Arc<HidController>,
        audio: Arc<AudioController>,
    ) -> Self {
        let credentials_path = cloud::credentials_path(&data_dir);
        let credentials = cloud::load_credentials(&credentials_path).unwrap_or_else(|e| {
            warn!("{e}");
            Credentials::default()
        });
        Self {
            inner: Arc::new(Inner {
                credentials_path,
                profile: Mutex::new(UuRemoteConfig::default()),
                credentials: Mutex::new(credentials),
                credentials_changed: Notify::new(),
                state: Mutex::new((UuRemoteServiceState::Stopped, None)),
                login: watch::channel(UuLoginState::idle()).0,
                peers: tokio::sync::Mutex::new(HashMap::new()),
                video,
                hid,
                audio,
            }),
            run_task: Mutex::new(None),
            login_task: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.run_task
            .lock()
            .as_ref()
            .is_some_and(|t| !t.is_finished())
    }

    pub async fn configure(&self, profile: &UuRemoteConfig) -> Result<(), String> {
        let old = self.inner.profile.lock().clone();
        if old.system != profile.system {
            let path = profile_path(&self.inner.credentials_path, profile.system);
            let credentials = load_profile_credentials(&path, &self.inner.credentials.lock())?;
            self.cancel_login().await;
            self.stop().await;
            *self.inner.profile.lock() = profile.clone();
            *self.inner.credentials.lock() = credentials;
        } else {
            // A pending QR flow must not later overwrite a newly applied profile.
            if old.device_name != profile.device_name {
                self.cancel_login().await;
            }
        }
        if old.system == profile.system && old.device_name != profile.device_name {
            let credentials = self.inner.credentials.lock().clone();
            if credentials.logged_in() {
                register_device(&self.inner, &credentials, profile).await?;
            }
        }
        if old.system == profile.system
            && (old.reverse_scroll_enabled() != profile.reverse_scroll_enabled()
                || old.video_codec != profile.video_codec)
        {
            self.stop().await;
        }
        *self.inner.profile.lock() = profile.clone();
        Ok(())
    }

    pub fn video_codec_preference(&self) -> crate::config::UuVideoCodec {
        self.inner.profile.lock().video_codec
    }

    pub async fn status(&self) -> UuRemoteStatus {
        let (state, error) = if self.is_running() {
            self.inner.state.lock().clone()
        } else {
            (UuRemoteServiceState::Stopped, None)
        };
        let credentials = self.inner.credentials.lock().clone();
        let peers: Vec<_> = self
            .inner
            .peers
            .lock()
            .await
            .iter()
            .map(|(id, peer)| (id.clone(), peer.clone()))
            .collect();
        // Do not hold the signaling peer-map lock while querying ICE transports.
        let mut sessions = Vec::new();
        for (id, peer) in peers {
            if !peer.ended() {
                sessions.push(peer.status(id).await);
            }
        }
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        let connection_count = sessions.iter().filter(|s| s.state == "connected").count() as u32;
        UuRemoteStatus {
            state,
            error,
            logged_in: credentials.logged_in(),
            nickname: Some(credentials.nickname).filter(|n| !n.is_empty()),
            device_id: Some(credentials.device_id).filter(|d| !d.is_empty()),
            connection_count,
            sessions,
            video_codec: self.inner.video.uu_negotiated_codec().await.map(|codec| {
                if codec == crate::video::codec::VideoCodecType::H265 {
                    crate::config::UuVideoCodec::H265
                } else {
                    crate::config::UuVideoCodec::H264
                }
            }),
        }
    }

    pub async fn video_output(&self) -> Option<UuVideoOutput> {
        #[cfg(feature = "vim4")]
        {
            let config = self.inner.video.get_encoding_config().await?;
            Some(UuVideoOutput {
                height: config.output.height,
                fps: config.output.fps,
                bitrate_kbps: config.bitrate_kbps(),
            })
        }
        #[cfg(not(feature = "vim4"))]
        {
            None
        }
    }

    pub fn start(&self) {
        let mut task = self.run_task.lock();
        if task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        info!("Starting UU Remote host");
        *task = Some(tokio::spawn(run(self.inner.clone())));
    }

    pub async fn stop(&self) {
        let task = self.run_task.lock().take();
        if let Some(task) = task {
            info!("Stopping UU Remote host");
            task.abort();
            let _ = task.await;
        }
        close_all(&self.inner).await;
        *self.inner.state.lock() = (UuRemoteServiceState::Stopped, None);
    }

    pub fn login_state(&self) -> UuLoginState {
        self.inner.login.borrow().clone()
    }

    /// Start (or continue) the QR login; the web UI polls `login_state`.
    pub fn begin_login(&self) -> UuLoginState {
        let mut task = self.login_task.lock();
        if !task.as_ref().is_some_and(|t| !t.is_finished()) {
            self.inner.login.send_replace(UuLoginState {
                stage: UuLoginStage::Waiting,
                qr_url: None,
                message: None,
            });
            let inner = self.inner.clone();
            *task = Some(tokio::spawn(async move {
                let result = tokio::time::timeout(LOGIN_TIMEOUT, login_flow(&inner)).await;
                let state = match result {
                    Ok(Ok(name)) => UuLoginState {
                        stage: UuLoginStage::Success,
                        qr_url: None,
                        message: Some(name),
                    },
                    Ok(Err(e)) => {
                        warn!("UU login failed: {e}");
                        UuLoginState {
                            stage: UuLoginStage::Failed,
                            qr_url: None,
                            message: Some(e),
                        }
                    }
                    Err(_) => UuLoginState {
                        stage: UuLoginStage::Failed,
                        qr_url: None,
                        message: Some("login timed out".into()),
                    },
                };
                inner.login.send_replace(state);
            }));
        }
        drop(task);
        self.login_state()
    }

    pub async fn cancel_login(&self) {
        let task = self.login_task.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.inner.login.send_replace(UuLoginState::idle());
    }

    /// Forget the account binding; the registered host identity is kept for the next login.
    pub async fn logout(&self) -> Result<(), String> {
        self.cancel_login().await;
        let credentials = {
            let mut credentials = self.inner.credentials.lock();
            credentials.logout();
            credentials.clone()
        };
        save(&self.inner, &credentials)?;
        info!("UU account unbound");
        self.inner.credentials_changed.notify_waiters();
        close_all(&self.inner).await;
        Ok(())
    }
}

fn set_state(inner: &Inner, state: UuRemoteServiceState, error: Option<String>) {
    *inner.state.lock() = (state, error);
}

async fn close_all(inner: &Inner) {
    // Match handle_soac's replacement ordering: logout can run alongside the
    // room loop, so new offers must wait until old input releases finish.
    let mut peers = inner.peers.lock().await;
    for (_, peer) in peers.drain() {
        peer.close().await;
    }
}

fn save(inner: &Inner, credentials: &Credentials) -> Result<(), String> {
    cloud::save_credentials(
        &profile_path(&inner.credentials_path, inner.profile.lock().system),
        credentials,
    )?;
    *inner.credentials.lock() = credentials.clone();
    Ok(())
}

fn profile_path(windows: &std::path::Path, system: UuRemoteSystem) -> PathBuf {
    match system {
        UuRemoteSystem::Windows => windows.to_path_buf(),
        UuRemoteSystem::Macos => windows.with_file_name("credentials-macos.json"),
    }
}

fn load_profile_credentials(
    path: &std::path::Path,
    current: &Credentials,
) -> Result<Credentials, String> {
    let mut credentials = cloud::load_credentials(path)?;
    if credentials.client_id.is_empty() {
        // Stable hardware identifiers, but each platform must obtain its own login token.
        credentials = current.clone();
        credentials.logout();
        credentials.device_id.clear();
    }
    Ok(credentials)
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn platform_switch_keeps_logins_separate_and_restores_saved_identity() {
        let dir = tempfile::tempdir().unwrap();
        let windows_path = dir.path().join("credentials.json");
        let windows = Credentials {
            client_id: "client".into(),
            system_id: "system".into(),
            device_id: "windows-device".into(),
            user_id: "user".into(),
            token: "windows-token".into(),
            csrf: "windows-csrf".into(),
            ..Default::default()
        };
        cloud::save_credentials(&windows_path, &windows).unwrap();
        let mac_path = profile_path(&windows_path, UuRemoteSystem::Macos);
        let mut mac = load_profile_credentials(&mac_path, &windows).unwrap();
        assert_eq!(mac.client_id, windows.client_id);
        assert!(!mac.logged_in());
        assert!(mac.device_id.is_empty() && mac.token.is_empty() && mac.csrf.is_empty());
        mac.device_id = "mac-device".into();
        mac.user_id = "user".into();
        mac.token = "mac-token".into();
        mac.csrf = "mac-csrf".into();
        cloud::save_credentials(&mac_path, &mac).unwrap();
        let restored = load_profile_credentials(&windows_path, &mac).unwrap();
        assert_eq!(restored.token, windows.token);
        assert_eq!(restored.device_id, windows.device_id);
        assert_eq!(
            load_profile_credentials(&mac_path, &windows).unwrap().token,
            mac.token
        );
    }

    #[test]
    fn old_configs_keep_windows_and_get_default_device_name() {
        let profile: UuRemoteConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(profile.enabled);
        assert_eq!(profile.system, UuRemoteSystem::Windows);
        assert_eq!(profile.device_name, "vim4-kvm");
        assert!(serde_json::from_str::<UuRemoteConfig>(r#"{"system":"unknown"}"#).is_err());
    }
}

async fn register_device(
    inner: &Inner,
    credentials: &Credentials,
    profile: &UuRemoteConfig,
) -> Result<String, String> {
    let screen = inner
        .video
        .get_encoding_config()
        .await
        .map_or((1920, 1080), |c| (c.resolution.width, c.resolution.height));
    let path = match profile.system {
        UuRemoteSystem::Windows => "/api/v1/device/windows/init",
        UuRemoteSystem::Macos => "/api/v1/device/macos/init",
    };
    let client = CloudClient::for_system(credentials.clone(), profile.system);
    let (status, body) = client
        .post(path, &cloud::device_summary(credentials, screen, profile))
        .await?;
    let data = api_data("device profile", status, &body)?;
    let id = data
        .get("device_id")
        .and_then(Value::as_str)
        .ok_or("Device profile returned no id")?;
    if !credentials.device_id.is_empty() && credentials.device_id != id {
        return Err("Device identity changed; please sign in again".into());
    }
    if credentials.logged_in() {
        client.rename_device(&profile.device_name).await?;
    }
    Ok(id.to_string())
}

/// Register this host (once) and bind it to an account by scanning the QR code with the UU
/// mobile app: gen/login -> long-poll login/status (1 wait, 2 scanned, 3/4 final) -> by_qrcode.
async fn login_flow(inner: &Inner) -> Result<String, String> {
    let profile = inner.profile.lock().clone();
    let mut credentials = inner.credentials.lock().clone();
    if credentials.ensure_identity() {
        save(inner, &credentials)?;
    }
    {
        credentials.device_id = register_device(inner, &credentials, &profile).await?;
        save(inner, &credentials)?;
        info!("UU host registered");
    }
    // Login requests are signed without any stale account token.
    let client = CloudClient::for_system(
        Credentials {
            user_id: String::new(),
            token: String::new(),
            csrf: String::new(),
            ..credentials.clone()
        },
        profile.system,
    );
    loop {
        let (status, body) = client.post("/api/v1/qrcode/gen/login", &json!({})).await?;
        let data = api_data("QR code", status, &body)?;
        let url = data
            .get("qrcode_jump_url")
            .and_then(Value::as_str)
            .ok_or("QR response has no URL")?
            .to_string();
        let ticket = data
            .get("token")
            .or_else(|| data.get("status_query_ticket"))
            .and_then(Value::as_str)
            .ok_or("QR response has no ticket")?
            .to_string();
        inner.login.send_replace(UuLoginState {
            stage: UuLoginStage::Waiting,
            qr_url: Some(url.clone()),
            message: None,
        });
        let mut login_status = 1i64;
        for _ in 0..150 {
            let query =
                json!({"token": ticket, "qrcode_jump_url": url, "login_status": login_status});
            let Ok((status, body)) = client.post("/api/v1/qrcode/login/status", &query).await
            else {
                continue; // long-poll timeout: ask again
            };
            if body.get("code").and_then(Value::as_i64) == Some(cloud::QR_EXPIRED) {
                break;
            }
            let current = api_data("QR status", status, &body)?
                .get("status")
                .and_then(Value::as_i64)
                .unwrap_or(login_status);
            if current == login_status {
                continue;
            }
            login_status = current;
            if current == 2 {
                inner.login.send_replace(UuLoginState {
                    stage: UuLoginStage::Scanned,
                    qr_url: Some(url.clone()),
                    message: None,
                });
                continue;
            }
            if !matches!(current, 3 | 4) {
                break;
            }
            let (status, body) = client
                .post(
                    "/api/v1/login/by_qrcode",
                    &json!({"token": ticket, "qrcode_jump_url": url}),
                )
                .await?;
            let Ok(data) = api_data("QR login", status, &body) else {
                break; // cancelled on the phone: offer a new code
            };
            let token = data
                .get("token")
                .and_then(Value::as_str)
                .ok_or("login returned no token")?;
            credentials.user_id = data
                .get("user_id")
                .and_then(Value::as_str)
                .ok_or("login returned no user id")?
                .to_string();
            credentials.csrf =
                cloud::csrf_from_token(token).ok_or("login token carries no csrf claim")?;
            credentials.token = token.to_string();
            credentials.nickname = data
                .get("nickname")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            save(inner, &credentials)?;
            inner.credentials_changed.notify_waiters();
            CloudClient::for_system(credentials.clone(), profile.system)
                .rename_device(&profile.device_name)
                .await?;
            info!("UU account bound");
            return Ok(if credentials.nickname.is_empty() {
                credentials.user_id.clone()
            } else {
                credentials.nickname.clone()
            });
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn run(inner: Arc<Inner>) {
    loop {
        let credentials = inner.credentials.lock().clone();
        if !credentials.logged_in() {
            set_state(&inner, UuRemoteServiceState::WaitingLogin, None);
            let changed = inner.credentials_changed.notified();
            if !inner.credentials.lock().logged_in() {
                changed.await;
            }
            continue;
        }
        set_state(&inner, UuRemoteServiceState::Connecting, None);
        let profile = inner.profile.lock().clone();
        if let Err(error) = register_device(&inner, &credentials, &profile).await {
            warn!("UU device profile refresh failed: {error}");
        }
        let system = profile.system;
        let room = match CloudClient::for_system(credentials.clone(), system)
            .create_room()
            .await
        {
            Ok(room) => room,
            Err(CloudError::LoginExpired) => {
                error!("UU login is invalid or expired; a new QR login is required");
                let mut expired = credentials;
                expired.logout();
                if let Err(e) = save(&inner, &expired) {
                    warn!("{e}");
                    *inner.credentials.lock() = expired;
                }
                continue;
            }
            Err(e) => {
                warn!("UU room creation failed: {e}");
                set_state(&inner, UuRemoteServiceState::Error, Some(e.to_string()));
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };
        let (events_tx, events) = mpsc::channel(64);
        let signaling =
            match SignalingClient::connect(&room.signaling_server, &room.token, events_tx.clone())
                .await
            {
                Ok(client) => Arc::new(client),
                Err(e) => {
                    warn!("{e}");
                    set_state(&inner, UuRemoteServiceState::Error, Some(e));
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            };
        let renewal = tokio::spawn(renew_when_idle(inner.clone(), events_tx));
        let logout = inner.credentials_changed.notified();
        tokio::select! {
            // Media does not pass through signaling: a room reconnect keeps connected viewers.
            _ = room_loop(&inner, signaling, events) => {}
            _ = logout => {
                info!("UU credentials changed; leaving room");
                close_all(&inner).await;
            }
        }
        renewal.abort();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn renew_when_idle(inner: Arc<Inner>, events: mpsc::Sender<SignalingEvent>) {
    tokio::time::sleep(ROOM_RENEWAL).await;
    loop {
        if !inner.peers.lock().await.values().any(|p| p.connected()) {
            info!("UU idle room renewal");
            let _ = events.send(SignalingEvent::Disconnected).await;
            return;
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn room_loop(
    inner: &Arc<Inner>,
    signaling: Arc<SignalingClient>,
    mut events: mpsc::Receiver<SignalingEvent>,
) {
    // be-controlled announces the capture type and ICE servers before the offer arrives.
    let mut pending: HashMap<String, (u64, ice::SessionIce)> = HashMap::new();
    let mut next_pending: Option<(u64, ice::SessionIce)> = None;
    let profile = inner.profile.lock().clone();
    let deps = PeerDeps {
        system: profile.system,
        reverse_scroll: profile.reverse_scroll_enabled(),
        video_codec: profile.video_codec,
        video: inner.video.clone(),
        hid: inner.hid.clone(),
        audio: inner.audio.clone(),
        signaling: signaling.clone(),
    };
    while let Some(event) = events.recv().await {
        match event {
            SignalingEvent::Connected => {
                info!("UU host online");
                set_state(inner, UuRemoteServiceState::Online, None);
            }
            SignalingEvent::DeviceInfoChanged(value) => {
                let availability = value
                    .get("data")
                    .and_then(|d| d.get("publisher_availability_status"))
                    .and_then(Value::as_str);
                info!("UU device status: availability={availability:?}");
            }
            SignalingEvent::ConnectRequest {
                capture_type,
                ice_id,
                transport,
            } => {
                if pending.len() >= 32 {
                    pending.clear();
                }
                match ice_id {
                    Some(id) => {
                        pending.insert(id, (capture_type, transport));
                    }
                    None => next_pending = Some((capture_type, transport)),
                }
            }
            SignalingEvent::Soac(soac) => {
                handle_soac(
                    inner,
                    &deps,
                    &signaling,
                    soac,
                    &mut pending,
                    &mut next_pending,
                )
                .await
            }
            SignalingEvent::Released(value) => {
                let ice = ["ice_id", "iceId"]
                    .iter()
                    .find_map(|k| {
                        value
                            .get(*k)
                            .or_else(|| value.get("data").and_then(|d| d.get(*k)))
                    })
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let mut peers = inner.peers.lock().await;
                let targets: Vec<String> = match ice {
                    Some(id) if peers.contains_key(&id) => vec![id],
                    _ => peers.keys().cloned().collect(),
                };
                info!("UU viewer disconnected: closing={}", targets.len());
                let closing: Vec<Arc<Peer>> =
                    targets.iter().filter_map(|k| peers.remove(k)).collect();
                drop(peers);
                for peer in closing {
                    peer.close().await;
                }
            }
            SignalingEvent::Disconnected => {
                warn!("UU signaling disconnected; reconnecting");
                return;
            }
        }
    }
}

async fn handle_soac(
    inner: &Arc<Inner>,
    deps: &PeerDeps,
    signaling: &SignalingClient,
    soac: SoacMessage,
    pending: &mut HashMap<String, (u64, ice::SessionIce)>,
    next_pending: &mut Option<(u64, ice::SessionIce)>,
) {
    let ice = soac.ice_key();
    let mut peers = inner.peers.lock().await;
    let ended: Vec<String> = peers
        .iter()
        .filter(|(_, p)| p.ended())
        .map(|(k, _)| k.clone())
        .collect();
    for key in ended {
        if let Some(peer) = peers.remove(&key) {
            peer.close().await;
        }
    }
    match soac.msg_type.as_str() {
        "offer" => {
            let (capture_type, transport) = pending
                .remove(&ice)
                .or_else(|| next_pending.take())
                .unwrap_or((1, ice::SessionIce::default()));
            if capture_type > MAX_DESKTOP_CAPTURE_TYPE {
                info!("UU connection type {capture_type} is not offered by a KVM; ignored");
                return;
            }
            // One desktop stream at a time: a new desktop offer replaces older ones.
            for (_, peer) in peers.drain() {
                peer.close().await;
            }
            let peer = match Peer::new(deps.clone(), transport, &soac).await {
                Ok(peer) => Arc::new(peer),
                Err(e) => {
                    error!("UU peer creation failed: {e}");
                    return;
                }
            };
            match peer.handle_offer(&soac).await {
                Ok(sdp) => {
                    let answer = SoacMessage {
                        msg_type: "answer".into(),
                        sdp: Some(sdp),
                        gzip_sdp: None,
                        candidate: None,
                        ice_id: soac.ice_id.clone(),
                        sdp_mid: None,
                        sdp_mline_index: None,
                        client_id: soac.client_id.clone(),
                        app_control_id: soac.app_control_id.clone(),
                    };
                    if let Err(e) = signaling.emit_soac(answer).await {
                        error!("UU answer signaling failed: {e}");
                    }
                    info!("UU desktop connection answered");
                    peers.insert(ice, peer);
                }
                Err(e) => {
                    error!("UU offer rejected: {e}");
                    peer.close().await;
                }
            }
        }
        "candidate" => match peers.get(&ice) {
            Some(peer) => {
                if let Err(e) = peer.add_candidate(&soac).await {
                    warn!("UU remote candidate: {e}");
                }
            }
            None => tracing::debug!("UU candidate for unknown connection"),
        },
        _ => {}
    }
}
