//! Linux platform capabilities.

use super::{FeatureCapability, PlatformCapabilities, PlatformMode};

pub fn capabilities() -> PlatformCapabilities {
    let mut capabilities = PlatformCapabilities {
        mode: PlatformMode::Linux,
        mode_label: PlatformMode::Linux.label(),
        video_capture: FeatureCapability::available(["v4l2"]),
        encoder: FeatureCapability::available([
            "software", "vaapi", "nvenc", "qsv", "amf", "rkmpp", "v4l2m2m",
        ]),
        hid: FeatureCapability::available(["otg", "ch9329", "none"]),
        atx: FeatureCapability::available(["gpio", "usb_relay", "serial", "wol", "none"]),
        msd: FeatureCapability::available(["configfs"]),
        otg: FeatureCapability::available(["configfs"]),
        audio: FeatureCapability::available(["alsa"]),
        rustdesk: FeatureCapability::available(["builtin"]),
        vnc: FeatureCapability::available(["builtin", "tight_jpeg", "h264"]),
        diagnostics: FeatureCapability::available(["linux"]),
        extensions: FeatureCapability::available(["linux"]),
        service_installation: FeatureCapability::available(["systemd"]),
    };
    #[cfg(feature = "vim4")]
    {
        capabilities.mode_label = "VIM4 Ubuntu";
        capabilities.video_capture = FeatureCapability::available(["vfm_cap"]);
        capabilities.encoder = FeatureCapability::available(["auto"]);
        capabilities.hid = FeatureCapability::available(["otg", "none"]);
        capabilities.atx = FeatureCapability::unsupported("No ATX hardware is connected");
        capabilities.otg = FeatureCapability::available(["vim4-board-gadget"]);
        capabilities.service_installation =
            FeatureCapability::unsupported("Installed as the vim4-onekvm Ubuntu package");
        if super::vim4::VideoBackend::current() == super::vim4::VideoBackend::Wave5 {
            capabilities.video_capture = FeatureCapability::available(["v4l2"]);
            capabilities.msd = FeatureCapability::unsupported("Not enabled in this video experiment");
            capabilities.otg = FeatureCapability::available(["vim4-hid-gadget"]);
            if !std::path::Path::new("/proc/asound/VIM4HDMIRX/id").exists() {
                capabilities.audio = FeatureCapability::unsupported("The VIM4 HDMI capture card is unavailable");
            }
        }
    }
    capabilities
}
