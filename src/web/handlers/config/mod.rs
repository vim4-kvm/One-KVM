pub(crate) mod apply;
mod types;

mod atx;
mod audio;
mod auth;
mod hid;
#[cfg(unix)]
mod msd;
#[cfg(unix)]
mod otg;
#[cfg(unix)]
mod otg_network;
mod redfish;
mod rtsp;
mod rustdesk;
mod stream;
#[cfg(unix)]
mod uac;
mod usb_update;
mod uu_remote;
pub(crate) mod video;
mod vnc;
mod watchdog;
mod web;

pub use atx::{get_atx_config, update_atx_config};
pub use audio::{get_audio_config, update_audio_config};
pub use auth::{get_auth_config, update_auth_config};
pub use hid::{get_hid_config, update_hid_config};
#[cfg(unix)]
pub use msd::{get_msd_config, update_msd_config};
#[cfg(unix)]
pub use otg::{get_otg_config, update_otg_config};
#[cfg(unix)]
pub use otg_network::{get_otg_network_config, get_otg_network_status, update_otg_network_config};
pub use redfish::{get_redfish_config, update_redfish_config};
pub use rtsp::{
    get_rtsp_config, get_rtsp_status, start_rtsp_service, stop_rtsp_service, update_rtsp_config,
};
pub use rustdesk::{
    get_device_password, get_rustdesk_config, get_rustdesk_status, regenerate_device_id,
    regenerate_device_password, start_rustdesk_service, stop_rustdesk_service,
    update_rustdesk_config,
};
pub use stream::{get_stream_config, update_stream_config};
#[cfg(unix)]
pub use uac::{get_uac_config, update_uac_config};
pub use uu_remote::{
    cancel_uu_remote_login, get_uu_remote_login, get_uu_remote_status, logout_uu_remote,
    start_uu_remote_login, update_uu_remote_config,
    delete_uu_terminal_session, test_uu_terminal_ssh, trust_uu_terminal_ssh, remove_uu_terminal_known_host,
};
pub use video::{get_video_config, update_video_config};
pub use vnc::{
    get_vnc_config, get_vnc_status, start_vnc_service, stop_vnc_service, update_vnc_config,
};
pub use watchdog::{get_watchdog_config, update_watchdog_config};
pub use web::{get_web_config, update_web_config};

use axum::{extract::State, Json};
use std::sync::Arc;

use crate::config::AppConfig;
use crate::state::AppState;

fn sanitize_config_for_api(config: &mut AppConfig) {
    config.stream.turn_password = None;
    config.computer_use.api_key = None;

    config.rustdesk.device_password.clear();
    config.rustdesk.relay_key = None;
    config.rustdesk.public_key = None;
    config.rustdesk.private_key = None;
    config.rustdesk.signing_public_key = None;
    config.rustdesk.signing_private_key = None;

    config.rtsp.password = None;
    config.vnc.password = None;
}

pub async fn get_all_config(State(state): State<Arc<AppState>>) -> Json<AppConfig> {
    let mut config = (*state.config.get()).clone();
    sanitize_config_for_api(&mut config);
    Json(config)
}
