//! UU Remote (网易UU远程) host: the official Windows/Mac/mobile viewers control the
//! KVM target through NetEase's cloud signaling and a WebRTC peer per connection.
//! The HDMI input is the desktop; keyboard and mouse go through One-KVM HID.

pub mod cloud;
mod ice;
mod input;
mod login;
mod metrics;
mod peer;
mod signaling;
mod status;
pub mod terminal;
mod video_metrics;
mod wire;

pub use status::UuSessionStatus;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
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
/// ConnectOptions.capture_type: 0 unknown, 1 desktop, 2 window; terminal (8)
/// is an independent data-only peer with explicitly enabled targets.
/// File transfer (5) and port mapping (9) remain unsupported.
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
    pub terminal: terminal::TerminalStatus,
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

#[derive(Clone)]
struct AccountState {
    credentials: Credentials,
    generation: u64,
}

impl AccountState {
    fn replace(&mut self, credentials: Credentials) {
        self.credentials = credentials;
        self.generation = self.generation.wrapping_add(1);
    }
}

struct Inner {
    credentials_path: PathBuf,
    profile: Mutex<UuRemoteConfig>,
    credentials: Mutex<AccountState>,
    /// Woken when credentials change (login or logout).
    credentials_changed: Notify,
    state: Mutex<(UuRemoteServiceState, Option<String>)>,
    login: watch::Sender<UuLoginState>,
    peers: tokio::sync::Mutex<HashMap<String, Arc<Peer>>>,
    video: Arc<VideoStreamManager>,
    hid: Arc<HidController>,
    audio: Arc<AudioController>,
    terminal: Arc<terminal::Registry>,
    desktop: Arc<peer::DesktopCoordinator>,
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
                terminal: terminal::Registry::new(
                    credentials_path.parent().unwrap().join("terminal"),
                ),
                credentials_path,
                profile: Mutex::new(UuRemoteConfig::default()),
                credentials: Mutex::new(AccountState {
                    credentials,
                    generation: 0,
                }),
                credentials_changed: Notify::new(),
                state: Mutex::new((UuRemoteServiceState::Stopped, None)),
                login: watch::channel(UuLoginState::idle()).0,
                peers: tokio::sync::Mutex::new(HashMap::new()),
                desktop: Arc::new(peer::DesktopCoordinator::default()),
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
        profile.terminal.validate()?;
        let old = self.inner.profile.lock().clone();
        if old.system != profile.system {
            let path = profile_path(&self.inner.credentials_path, profile.system);
            let credentials =
                load_profile_credentials(&path, &self.inner.credentials.lock().credentials)?;
            self.cancel_login().await;
            self.stop().await;
            *self.inner.profile.lock() = profile.clone();
            self.inner.credentials.lock().replace(credentials);
        } else {
            // A pending QR flow must not later overwrite a newly applied profile.
            if old.device_name != profile.device_name {
                self.cancel_login().await;
            }
        }
        if old.system == profile.system && old.device_name != profile.device_name {
            let credentials = self.inner.credentials.lock().credentials.clone();
            if credentials.logged_in() {
                register_device(&self.inner, &credentials, profile).await?;
            }
        }
        if old.system == profile.system
            && (old.reverse_scroll_enabled() != profile.reverse_scroll_enabled()
                || old.video_codec != profile.video_codec)
        {
            // Desktop preferences do not revoke terminal access or detached jobs.
            let task = self.run_task.lock().take();
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
            let mut peers = self.inner.peers.lock().await;
            let desktop: Vec<_> = peers
                .iter()
                .filter(|(_, p)| p.is_desktop())
                .map(|(id, _)| id.clone())
                .collect();
            for id in desktop {
                if let Some(peer) = peers.get(&id).filter(|p| p.is_terminal()) {
                    peer.stop_desktop().await;
                    continue;
                }
                if let Some(peer) = peers.remove(&id) {
                    peer.close().await;
                }
            }
        }
        self.inner
            .terminal
            .configure(&profile.terminal, &profile.device_name)
            .await?;
        *self.inner.profile.lock() = profile.clone();
        Ok(())
    }

    pub fn terminal(&self) -> &Arc<terminal::Registry> {
        &self.inner.terminal
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
        let credentials = self.inner.credentials.lock().credentials.clone();
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
        let mut terminal_peers = 0;
        for (id, peer) in peers {
            if !peer.ended() {
                if peer.is_terminal() {
                    terminal_peers += 1;
                }
                if peer.is_desktop() {
                    sessions.push(peer.status(id).await);
                }
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
            terminal: self.inner.terminal.status(terminal_peers).await,
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
        self.inner.terminal.start();
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
            let mut current = self.inner.credentials.lock();
            let mut credentials = current.credentials.clone();
            credentials.logout();
            current.replace(credentials.clone());
            credentials
        };
        let persisted = save(&self.inner, &credentials);
        info!("UU account unbound");
        self.inner.credentials_changed.notify_waiters();
        close_all(&self.inner).await;
        persisted
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
    inner.terminal.shutdown().await;
}

async fn login_expired(inner: &Inner, failed: AccountState) {
    let path = profile_path(&inner.credentials_path, inner.profile.lock().system);
    {
        let mut current = inner.credentials.lock();
        if current.generation != failed.generation {
            info!("UU obsolete login-expiry response ignored");
            return;
        }
        error!("UU login is invalid or expired; a new QR login is required");
        let mut expired = current.credentials.clone();
        expired.logout();
        if let Err(error) = cloud::save_credentials(&path, &expired) {
            warn!("{error}");
        }
        // Revoke in memory even if persistence fails. Generation and disk writes
        // share this lock with login/logout, including a reissued identical token.
        current.replace(expired);
    }
    close_all(inner).await;
}

fn save(inner: &Inner, credentials: &Credentials) -> Result<(), String> {
    let path = profile_path(&inner.credentials_path, inner.profile.lock().system);
    let mut current = inner.credentials.lock();
    cloud::save_credentials(&path, credentials)?;
    current.replace(credentials.clone());
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
/// mobile app: gen/login -> long-poll login/status -> by_qrcode only after status 4.
async fn login_flow(inner: &Inner) -> Result<String, String> {
    let profile = inner.profile.lock().clone();
    let mut credentials = inner.credentials.lock().credentials.clone();
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
    let data = login::authenticate(inner, &client).await?;
    let token = data
        .get("token")
        .and_then(Value::as_str)
        .ok_or("login returned no token")?;
    credentials.user_id = data
        .get("user_id")
        .and_then(Value::as_str)
        .ok_or("login returned no user id")?
        .to_string();
    credentials.csrf = cloud::csrf_from_token(token).ok_or("login token carries no csrf claim")?;
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
    Ok(if credentials.nickname.is_empty() {
        credentials.user_id
    } else {
        credentials.nickname
    })
}

async fn run(inner: Arc<Inner>) {
    loop {
        let account = inner.credentials.lock().clone();
        let credentials = account.credentials.clone();
        if !credentials.logged_in() {
            set_state(&inner, UuRemoteServiceState::WaitingLogin, None);
            let changed = inner.credentials_changed.notified();
            if !inner.credentials.lock().credentials.logged_in() {
                changed.await;
            }
            continue;
        }
        set_state(&inner, UuRemoteServiceState::Connecting, None);
        inner.terminal.start();
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
                login_expired(&inner, account).await;
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
        terminal: inner.terminal.clone(),
        desktop: inner.desktop.clone(),
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
                let mut peers = inner.peers.lock().await;
                let targets = released_peers(
                    &value,
                    peers
                        .iter()
                        .map(|(id, peer)| (id.clone(), !peer.is_desktop())),
                );
                info!("UU viewer disconnected: closing={}", targets.len());
                let desktop_only =
                    matches!(signaling::ice_id_of(&value), signaling::IceId::Missing);
                let mut closing = Vec::new();
                let mut demoting = Vec::new();
                for id in targets {
                    if desktop_only && peers.get(&id).is_some_and(|p| p.is_terminal()) {
                        demoting.extend(peers.get(&id).cloned());
                    } else {
                        closing.extend(peers.remove(&id));
                    }
                }
                drop(peers);
                for peer in demoting {
                    peer.stop_desktop().await;
                }
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
    let Some(ice) = soac.ice_key() else {
        warn!("UU message ignored: invalid session identifier");
        return;
    };
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
            if capture_type > MAX_DESKTOP_CAPTURE_TYPE && capture_type != 8 {
                info!("UU connection type {capture_type} is not offered by a KVM; ignored");
                return;
            }
            let terminal = capture_type == 8;
            let evict = replacement_peers(
                terminal,
                peers
                    .iter()
                    .map(|(id, p)| (id.clone(), p.is_terminal(), p.created_at)),
            );
            for id in evict {
                if let Some(peer) = peers.remove(&id) {
                    peer.close().await;
                }
            }
            if let Some(peer) = peers.remove(&ice) {
                peer.close().await;
            }
            let peer = match Peer::new(deps.clone(), transport, &soac, terminal).await {
                Ok(peer) => Arc::new(peer),
                Err(e) => {
                    error!("UU peer creation failed: {e}");
                    return;
                }
            };
            // Shutdown can cancel negotiation after the answer starts reaching the
            // viewer; register first so close_all still owns that connection.
            peers.insert(ice.clone(), peer.clone());
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
                    info!(terminal, "UU connection answered");
                }
                Err(e) => {
                    error!("UU offer rejected: {e}");
                    peers.remove(&ice);
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

fn released_peers(value: &Value, peers: impl Iterator<Item = (String, bool)>) -> Vec<String> {
    let scope = signaling::ice_id_of(value);
    peers
        .filter_map(|(id, terminal)| match &scope {
            signaling::IceId::Session(target) if *target == id => Some(id),
            signaling::IceId::Missing if !terminal => Some(id),
            _ => None,
        })
        .collect()
}

fn replacement_peers(
    incoming_terminal: bool,
    peers: impl Iterator<Item = (String, bool, std::time::Instant)>,
) -> Vec<String> {
    let mut same: Vec<_> = peers
        .filter(|(_, terminal, _)| *terminal == incoming_terminal)
        .collect();
    same.sort_by_key(|(_, _, created)| *created);
    let count = if incoming_terminal {
        same.len().saturating_sub(3)
    } else {
        same.len()
    };
    same.into_iter().take(count).map(|(id, _, _)| id).collect()
}

#[cfg(test)]
mod terminal_peer_tests {
    use super::*;

    #[tokio::test]
    async fn obsolete_expiry_cannot_revoke_a_new_login_or_its_terminal_sessions() {
        use terminal::frame::*;
        for same_token in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let service = UuRemoteService::new(
                dir.path().into(),
                VideoStreamManager::with_webrtc_streamer(
                    crate::video::streamer::Streamer::new(),
                    crate::webrtc::WebRtcStreamer::new(),
                ),
                Arc::new(HidController::new(crate::hid::HidBackendType::None, None)),
                Arc::new(AudioController::default()),
            );
            let mut old = Credentials {
                user_id: "synthetic-user".into(),
                device_id: "synthetic-device".into(),
                token: "synthetic-old".into(),
                csrf: "synthetic-csrf".into(),
                ..Default::default()
            };
            old.ensure_identity();
            save(&service.inner, &old).unwrap();
            let failed = service.inner.credentials.lock().clone();
            let (complete, response) = tokio::sync::oneshot::channel();
            let inner = service.inner.clone();
            let late_response = tokio::spawn(async move {
                response.await.unwrap();
                login_expired(&inner, failed).await;
            });
            let mut fresh = old;
            if !same_token {
                fresh.token = "synthetic-new".into();
            }
            save(&service.inner, &fresh).unwrap();
            let registry = &service.inner.terminal;
            let mut config = crate::config::UuTerminalConfig::default();
            config.serial.enabled = true;
            registry.configure(&config, "test").await.unwrap();
            registry.start();
            let (out, mut replies) = mpsc::channel(64);
            let conn = terminal::TerminalConn::new(registry.clone(), out);
            conn.handle(&encode(OPEN, 0, b"{}")).await;
            loop {
                let bytes = replies.recv().await.unwrap();
                let frame = decode(&bytes).unwrap();
                if frame.kind == OPEN_RESP {
                    assert_eq!(json(frame.payload)["code"], 0);
                    break;
                }
            }
            complete.send(()).unwrap();
            late_response.await.unwrap();
            assert_eq!(
                service.inner.credentials.lock().credentials.token,
                fresh.token
            );
            let saved = cloud::load_credentials(&service.inner.credentials_path).unwrap();
            assert_eq!(saved.token, fresh.token);
            assert_eq!(registry.status(0).await.sessions.len(), 1);
            conn.handle(&encode(OPEN, 0, b"{}")).await;
            loop {
                let bytes = replies.recv().await.unwrap();
                let frame = decode(&bytes).unwrap();
                if frame.kind == OPEN_RESP {
                    assert_eq!(
                        json(frame.payload)["code"],
                        0,
                        "new login must remain accepting"
                    );
                    break;
                }
            }
            registry.shutdown().await;
        }
    }

    #[test]
    fn explicit_release_targets_only_its_session_even_when_id_is_numeric() {
        let peers = vec![("desktop".to_owned(), false), ("123".to_owned(), true)];
        let targets = |value: Value| released_peers(&value, peers.clone().into_iter());
        assert_eq!(targets(json!({"ice_id":123})), vec!["123"]);
        assert_eq!(targets(json!({"data":{"iceId":"123"}})), vec!["123"]);
        for id in [json!(999), Value::Null, json!(false), json!({}), json!([])] {
            assert!(targets(json!({"ice_id":id})).is_empty());
        }
        assert_eq!(targets(json!({})), vec!["desktop"]);
        let message: SoacMessage =
            serde_json::from_value(json!({"type":"candidate","ice_id":123})).unwrap();
        assert_eq!(message.ice_key().as_deref(), Some("123"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn expired_login_ends_attached_and_detached_sessions_even_if_save_fails() {
        use terminal::frame::*;
        for save_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let service = UuRemoteService::new(
                dir.path().into(),
                VideoStreamManager::with_webrtc_streamer(
                    crate::video::streamer::Streamer::new(),
                    crate::webrtc::WebRtcStreamer::new(),
                ),
                Arc::new(HidController::new(crate::hid::HidBackendType::None, None)),
                Arc::new(AudioController::default()),
            );
            let registry = &service.inner.terminal;
            let mut config = crate::config::UuTerminalConfig::default();
            config.serial.enabled = true;
            registry.configure(&config, "test").await.unwrap();
            registry.start();
            let (tx, mut rx) = mpsc::channel(64);
            let conn = terminal::TerminalConn::new(registry.clone(), tx);
            for detached in [true, false] {
                conn.handle(&encode(OPEN, 0, b"{}")).await;
                let id = loop {
                    let bytes = rx.recv().await.unwrap();
                    let frame = decode(&bytes).unwrap();
                    if frame.kind == OPEN_RESP {
                        break frame.session;
                    }
                };
                if detached {
                    conn.handle(&encode(DETACH, id, b"")).await;
                }
            }
            assert_eq!(registry.status(0).await.sessions.len(), 2);
            if save_fails {
                std::fs::create_dir_all(&service.inner.credentials_path).unwrap();
            }
            let credentials = Credentials {
                user_id: "expired-user".into(),
                token: "expired-token".into(),
                ..Default::default()
            };
            service.inner.credentials.lock().replace(credentials);
            let failed = service.inner.credentials.lock().clone();
            login_expired(&service.inner, failed).await;
            assert!(service
                .inner
                .credentials
                .lock()
                .credentials
                .token
                .is_empty());
            assert!(registry.status(0).await.sessions.is_empty());
            conn.handle(&encode(OPEN, 0, b"{}")).await;
            loop {
                let bytes = rx.recv().await.unwrap();
                let frame = decode(&bytes).unwrap();
                if frame.kind == OPEN_RESP {
                    assert_ne!(json(frame.payload)["code"], 0);
                    break;
                }
            }
        }
    }
    #[test]
    fn tool_and_desktop_replacement_are_independent() {
        let now = std::time::Instant::now();
        let peers = vec![
            ("desktop".into(), false, now),
            ("t1".into(), true, now),
            ("t2".into(), true, now + Duration::from_secs(1)),
            ("t3".into(), true, now + Duration::from_secs(2)),
            ("t4".into(), true, now + Duration::from_secs(3)),
        ];
        assert_eq!(
            replacement_peers(false, peers.clone().into_iter()),
            vec!["desktop"]
        );
        assert_eq!(
            replacement_peers(true, peers.clone().into_iter()),
            vec!["t1"]
        );
        assert!(replacement_peers(true, peers.into_iter().take(4)).is_empty());
    }
}
