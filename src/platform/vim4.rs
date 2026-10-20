//! The board services own HDMI, USB descriptors and power negotiation.
use crate::config::{AppConfig, HidBackend, StreamMode};
use crate::video::format::{PixelFormat, Resolution};

pub mod video_backend;
pub use video_backend::VideoBackend;

pub const VIDEO_DEVICE: &str = "/dev/video_cap";
pub const GADGET: &str = "/sys/kernel/config/usb_gadget/kvm";

#[derive(serde::Deserialize)]
struct CatalogEntry {
    id: String,
    name: String,
    source: String,
    size: u64,
}

impl CatalogEntry {
    fn into_image(self) -> crate::error::Result<crate::msd::ImageInfo> {
        let root = match self.source.as_str() {
            "shared" => "/var/lib/vim4-kvm/images",
            "jetkvm" => "/var/lib/jetkvm/images",
            "pikvm" => "/var/lib/kvmd/msd",
            "data" => "/var/lib/vim4-kvm/usb",
            _ => return Err(crate::error::MsdErrorCode::MsdInvalidRequest.into()),
        };
        // Keep catalog identity in a single URL segment, separate from local IDs.
        let id = format!("vim4-{}", self.id.bytes().map(|b| format!("{b:02x}")).collect::<String>());
        let path = std::path::Path::new(root).join(&self.name);
        let mut image = crate::msd::ImageInfo::new(id, self.name, path, self.size);
        image.source = Some(self.source);
        Ok(image)
    }
}

pub async fn catalog_images() -> crate::error::Result<Vec<crate::msd::ImageInfo>> {
    use crate::error::AppError;
    let output = tokio::process::Command::new("sudo")
        .args(["-n", "/usr/libexec/vim4-onekvm", "catalog"])
        .output().await.map_err(|e| AppError::Internal(e.to_string()))?;
    if !output.status.success() {
        return Err(AppError::Internal(String::from_utf8_lossy(&output.stderr).trim().into()));
    }
    let entries: Vec<CatalogEntry> = serde_json::from_slice(&output.stdout)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    entries.into_iter().filter(|entry| entry.source != "onekvm")
        .map(CatalogEntry::into_image).collect()
}

pub async fn find_image(manager: &crate::msd::ImageManager, id: &str) -> crate::error::Result<crate::msd::ImageInfo> {
    if id.starts_with("vim4-") {
        return catalog_images().await?.into_iter().find(|image| image.id == id)
            .ok_or_else(|| crate::error::MsdErrorCode::MsdResourceNotFound.into());
    }
    manager.get(id)
}

pub fn hid_paths(hid: &crate::config::HidConfig) -> crate::otg::HidDevicePaths {
    let functions = hid.constrained_otg_functions();
    crate::otg::HidDevicePaths {
        keyboard: functions.keyboard.then(|| "/dev/hidg0".into()),
        mouse_absolute: functions.mouse_absolute.then(|| "/dev/hidg1".into()),
        mouse_relative: functions.mouse_relative.then(|| "/dev/hidg2".into()),
        consumer: functions.consumer.then(|| "/dev/hidg3".into()),
        udc: hid.resolved_otg_udc(),
        keyboard_leds_enabled: hid.effective_otg_keyboard_leds(),
    }
}

pub async fn configure_usb(
    hid: &crate::config::HidConfig,
    msd: &crate::config::MsdConfig,
    network: &crate::config::OtgNetworkConfig,
    uac: &crate::config::UacConfig,
    serial: &crate::config::OtgSerialConfig,
) -> crate::error::Result<()> {
    use crate::error::AppError;
    if !network.bridge_interface.is_empty() {
        return Err(AppError::BadRequest("VIM4 USB networking uses a dedicated shared connection; Ethernet bridging is unavailable".into()));
    }
    let mut functions = hid.constrained_otg_functions();
    if hid.backend != HidBackend::Otg {
        functions.keyboard = false;
        functions.mouse_absolute = false;
        functions.mouse_relative = false;
        functions.consumer = false;
    }
    let endpoints = u8::from(functions.keyboard)
        + u8::from(functions.mouse_absolute)
        + u8::from(functions.mouse_relative)
        + u8::from(functions.consumer)
        + u8::from(network.enabled) * 2
        + u8::from(uac.enabled)
        + u8::from(serial.enabled) * 2
        + u8::from(msd.enabled);
    if endpoints > 5 {
        return Err(AppError::BadRequest("VIM4 supports at most 5 USB IN endpoints; disable some HID, network, microphone, serial or virtual media functions".into()));
    }
    if VideoBackend::current() == VideoBackend::Wave5 {
        return if endpoints == 0 {
            Ok(())
        } else {
            Err(AppError::BadRequest("USB configuration is unavailable in the Wave5 video experiment".into()))
        };
    }
    let profile = serde_json::json!({
        "hid": functions,
        "descriptor": {
            "idVendor": format!("0x{:04x}", hid.otg_descriptor.vendor_id),
            "idProduct": format!("0x{:04x}", hid.otg_descriptor.product_id),
            "strings/0x409/manufacturer": hid.otg_descriptor.manufacturer,
            "strings/0x409/product": hid.otg_descriptor.product,
            "strings/0x409/serialnumber": hid.otg_descriptor.serial_number.as_deref().filter(|s| !s.is_empty()),
        },
        "network": if network.enabled { network.driver_mode.function_name() } else { "" },
        "uac": uac.enabled,
        "serial": serial.enabled,
        "inquiry": { "flash": msd.flash_inquiry_string, "cdrom": msd.cdrom_inquiry_string },
    });
    let output = tokio::process::Command::new("sudo")
        .args([
            "-n",
            "/usr/libexec/vim4-onekvm",
            "usb",
            &profile.to_string(),
        ])
        .output()
        .await
        .map_err(|e| AppError::Config(e.to_string()))?;
    if !output.status.success() {
        return Err(AppError::BadRequest(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(())
}

pub fn validate_bitrate(preset: crate::config::BitratePreset) -> crate::error::Result<()> {
    if !(1000..=60000).contains(&preset.bitrate_kbps()) {
        return Err(crate::error::AppError::BadRequest(
            "VIM4 bitrate must be 1000–60000 kbps".into(),
        ));
    }
    Ok(())
}

/// Output limits are separate from HDMI timings, which always follow the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputSettings {
    /// Maximum output height; zero preserves the input size. Never upscale.
    pub height: u32,
    /// Maximum frame rate; zero follows the source. MJPEG is capped at 10 fps.
    pub fps: u32,
}

impl OutputSettings {
    pub fn validate(self) -> crate::error::Result<()> {
        if (self.height != 0 && (!(180..=2160).contains(&self.height) || self.height % 2 != 0))
            || (self.fps != 0 && !(5..=60).contains(&self.fps))
        {
            return Err(crate::error::AppError::BadRequest(
                "Unsupported output size or frame rate".into(),
            ));
        }
        Ok(())
    }

    pub fn frame_rate(self, source_fps: u32, maximum: u32) -> u32 {
        source_fps
            .min(if self.fps == 0 {
                maximum
            } else {
                self.fps.min(maximum)
            })
            .max(1)
    }

    pub fn filter(self, source: Resolution, fps: u32) -> String {
        let mut filter = format!("videorate drop-only=true ! video/x-raw,framerate={fps}/1 ! ");
        if self.height > 0 && self.height < source.height {
            // GE2D keeps scaling in DMA memory. Preserve aspect ratio and even chroma dimensions.
            let width = (u64::from(source.width) * u64::from(self.height)
                / u64::from(source.height)) as u32
                & !1;
            filter.push_str(&format!(
                "amlvconv ! video/x-raw,format=NV12,width={},height={} ! ",
                width.max(2),
                self.height
            ));
        }
        filter
    }
}

pub fn defaults(config: &mut AppConfig) {
    if !config.initialized {
        config.video.device = Some(VIDEO_DEVICE.into());
        config.video.format = Some("NV21".into());
        config.video.width = 3840;
        config.video.height = 2160;
        config.video.fps = 60;
        config.stream.mode = StreamMode::WebRTC;
        config.hid.backend = HidBackend::Otg;
        config.msd.enabled = true;
        config.audio.enabled = true;
        config.audio.device = "hw:AMLAUGESOUND,0".into();
        if VideoBackend::current() == VideoBackend::Wave5 {
            config.video.format = Some("NV12".into());
            config.audio.enabled = false;
            config.hid.backend = HidBackend::None;
            config.msd.enabled = false;
            config.otg_network.enabled = false;
            config.uac.enabled = false;
            config.otg_serial.enabled = false;
        }
    }
}

/// HDMI RX reports the clock-domain width (half-width for YUV420).
pub fn parse_signal(info: &str) -> Option<(Resolution, f64)> {
    let value = |key: &str| {
        info.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == key).then_some(value.trim())
        })
    };
    let mut width: u32 = value("Hactive")?.parse().ok()?;
    let height: u32 = value("Vactive")?.parse().ok()?;
    let rate: u32 = value("Frame Rate")?.parse().ok()?;
    if value("Color Space").is_some_and(|v| v.starts_with("3-")) {
        width = width.checked_mul(2)?;
    }
    if width == 0 || height == 0 || width > 4096 || height > 2160 || !(100..=12000).contains(&rate)
    {
        return None;
    }
    Some((Resolution::new(width, height), f64::from(rate) / 100.0))
}

pub fn signal() -> Option<(Resolution, u32)> {
    if VideoBackend::current() == VideoBackend::Wave5 {
        let status = video_backend::wave5_input_status();
        return Some((Resolution::new(status.width?, status.height?), status.fps?.round() as u32));
    }
    parse_signal(&std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/info").ok()?)
        .map(|(size, fps)| (size, fps.round() as u32))
}

pub fn input_status() -> crate::video::device::VideoInputStatus {
    if VideoBackend::current() == VideoBackend::Wave5 {
        return video_backend::wave5_input_status();
    }
    let Ok(info) = std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/info") else {
        return crate::video::device::VideoInputStatus::unavailable();
    };
    match parse_signal(&info) {
        Some((size, fps)) => crate::video::device::VideoInputStatus::locked(
            PixelFormat::Nv21,
            size.width,
            size.height,
            fps,
        ),
        None => crate::video::device::VideoInputStatus::no_signal(),
    }
}

pub async fn configure_media(
    lun: u8,
    config: &crate::otg::msd::MsdLunConfig,
) -> crate::error::Result<()> {
    configure_media_expected(lun, config, std::path::Path::new("")).await
}

pub async fn configure_media_expected(
    lun: u8,
    config: &crate::otg::msd::MsdLunConfig,
    expected: &std::path::Path,
) -> crate::error::Result<()> {
    configure_media_result(lun, config, expected).await.map(|_| ())
}

pub async fn configure_media_result(
    lun: u8,
    config: &crate::otg::msd::MsdLunConfig,
    expected: &std::path::Path,
) -> crate::error::Result<std::path::PathBuf> {
    use crate::error::AppError;
    if lun != 0 {
        return Err(AppError::BadRequest(
            "VIM4 supports one virtual media LUN".into(),
        ));
    }
    let mut command = tokio::process::Command::new("sudo");
    command.args(["-n", "/usr/libexec/vim4-onekvm", "media"]);
    command
        .arg(&config.file)
        .arg(if config.cdrom { "true" } else { "false" })
        .arg(if config.ro { "true" } else { "false" })
        .arg(expected);
    let output = command
        .output()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("MSD_EXTERNAL_MEDIA") {
            return Err(crate::error::MsdErrorCode::MsdExternalMedia.into());
        }
        return Err(AppError::Internal(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    #[derive(serde::Deserialize)]
    struct Mounted {
        file: std::path::PathBuf,
    }
    let result: Mounted = serde_json::from_slice(&output.stdout)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(result.file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn msd_catalog_identity_preserves_source_and_real_lun_path() {
        let make = |source: &str| CatalogEntry {
            id: format!("{source}/install disk.img"),
            name: "install disk.img".into(), source: source.into(), size: 4096,
        }.into_image().unwrap();
        let data = make("data");
        let foreign = make("pikvm");
        assert_ne!(data.id, foreign.id);
        assert_eq!(data.id, make("data").id);
        assert!(!data.id.contains('/') && !data.id.contains(' '));
        assert_eq!(data.path, std::path::Path::new("/var/lib/vim4-kvm/usb/install disk.img"));
        assert_eq!(foreign.source.as_deref(), Some("pikvm"));
        assert_eq!(foreign.path, std::path::Path::new("/var/lib/kvmd/msd/install disk.img"));
    }

    #[test]
    fn output_limits_preserve_aspect_ratio_and_never_increase_source_rate() {
        let settings = OutputSettings {
            height: 1080,
            fps: 30,
        };
        assert_eq!(settings.frame_rate(24, 60), 24);
        assert_eq!(settings.frame_rate(60, 60), 30);
        assert_eq!(settings.frame_rate(60, 10), 10);
        assert!(settings
            .filter(Resolution::new(3840, 2160), 30)
            .contains("width=1920,height=1080"));
        assert!(settings
            .filter(Resolution::new(2880, 2160), 30)
            .contains("width=1440,height=1080"));
        assert!(!settings
            .filter(Resolution::new(1280, 720), 30)
            .contains("amlvconv"));
        assert!(OutputSettings {
            height: 719,
            fps: 30
        }
        .validate()
        .is_err());
        assert!(validate_bitrate(crate::config::BitratePreset::Custom(60000)).is_ok());
        assert!(validate_bitrate(crate::config::BitratePreset::Custom(60001)).is_err());
    }
    #[test]
    fn initial_defaults_restore_media_after_generic_config_normalization() {
        let mut config = AppConfig::default();
        config.enforce_invariants();
        assert!(!config.msd.enabled);
        defaults(&mut config);
        config.enforce_invariants();
        assert!(config.msd.enabled);
        config.initialized = true;
        config.msd.enabled = false;
        defaults(&mut config);
        assert!(!config.msd.enabled);
    }
    #[test]
    fn hdmi_signal_handles_420_fractional_rate_and_missing_signal() {
        let info = "Hactive: 1920\nVactive: 2160\nFrame Rate: 5999\nColor Space: 3-YUV420\n";
        assert_eq!(
            parse_signal(info),
            Some((Resolution::new(3840, 2160), 59.99))
        );
        assert_eq!(parse_signal(&info.replace("5999", "0")), None);
        assert_eq!(parse_signal(""), None);
        assert_eq!(
            parse_signal(&info.replace("3-YUV420", "0-RGB")),
            Some((Resolution::new(1920, 2160), 59.99))
        );
    }
}
