use serde::{Deserialize, Serialize};
use typeshare::typeshare;

pub use crate::extensions::ExtensionsConfig;
pub use crate::rustdesk::config::RustDeskConfig;

mod atx;
mod broadcast;
mod common;
mod computer_use;
mod hid;
mod mcp;
mod otg_network;
mod stream;
mod terminal;
mod uac;
mod watchdog;
mod web;

pub use atx::*;
pub use broadcast::*;
pub use common::*;
pub use computer_use::*;
pub use hid::*;
pub use mcp::*;
pub use otg_network::*;
pub use stream::*;
pub use terminal::*;
pub use uac::*;
pub use watchdog::*;
pub use web::*;

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct AppConfig {
    pub broadcast: BroadcastConfig,
    pub mcp: McpConfig,
    pub recording: RecordingConfig,
    pub initialized: bool,
    pub auth: AuthConfig,
    pub video: VideoConfig,
    pub hid: HidConfig,
    pub otg_network: OtgNetworkConfig,
    pub otg_serial: OtgSerialConfig,
    pub msd: MsdConfig,
    pub atx: AtxConfig,
    pub audio: AudioConfig,
    pub stream: StreamConfig,
    pub web: WebConfig,
    pub computer_use: ComputerUseConfig,
    pub extensions: ExtensionsConfig,
    pub rustdesk: RustDeskConfig,
    pub vnc: VncConfig,
    pub uu_remote: UuRemoteConfig,
    pub rtsp: RtspConfig,
    pub redfish: RedfishConfig,
    pub watchdog: WatchdogConfig,
    pub uac: UacConfig,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub location: String,
    /// Current recording folder per NAS, including when another target is selected.
    #[typeshare(serialized_as = "HashMap<String, String>")]
    pub folders: std::collections::BTreeMap<String, String>,
    /// Previously used NAS folders, most recent first, at most eight per location.
    #[typeshare(serialized_as = "HashMap<String, Vec<String>>")]
    pub location_folders: std::collections::BTreeMap<String, Vec<String>>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            location: "local".into(),
            folders: Default::default(),
            location_folders: Default::default(),
        }
    }
}

impl RecordingConfig {
    pub fn folders_for_location(&self, id: &str) -> Vec<String> {
        if id == "local" {
            return vec![String::new()];
        }
        // Include the legacy current folder even before history is first persisted.
        let current = self
            .folders
            .get(id)
            .map(String::as_str)
            .unwrap_or("One-KVM");
        let mut folders = vec![current.to_owned()];
        if let Some(history) = self.location_folders.get(id) {
            for folder in history {
                if folders.len() == 8 {
                    break;
                }
                if !folders.contains(folder) {
                    folders.push(folder.clone());
                }
            }
        }
        folders
    }

    pub fn remember_folder(&mut self, id: &str, folder: String) {
        if id == "local" {
            return;
        }
        let mut history = self.folders_for_location(id);
        history.retain(|previous| previous != &folder);
        history.insert(0, folder.clone());
        history.truncate(8);
        self.location_folders.insert(id.to_owned(), history);
        self.folders.insert(id.to_owned(), folder);
    }
}

impl AppConfig {
    pub fn enforce_invariants(&mut self) {
        #[cfg(feature = "vim4")]
        crate::platform::vim4::resolve_hardware_config(
            self,
            crate::platform::vim4::VideoBackend::current(),
        );
        if self.hid.backend != HidBackend::Otg {
            self.msd.enabled = false;
            self.otg_network.enabled = false;
            self.uac.enabled = false;
            self.otg_serial.enabled = false;
        }
        if self.hid.backend == HidBackend::Bluetooth {
            self.hid.mouse_absolute = false;
        }
        self.atx.normalize();
    }

    pub fn apply_platform_defaults(&mut self) {
        crate::platform::defaults::apply(self);
        self.enforce_invariants();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_watchdog_config_defaults_to_disabled() {
        let value = serde_json::to_value(AppConfig::default()).unwrap();
        let mut object = value.as_object().unwrap().clone();
        object.remove("watchdog");

        let config: AppConfig = serde_json::from_value(object.into()).unwrap();
        assert!(!config.watchdog.enabled);
    }
}
