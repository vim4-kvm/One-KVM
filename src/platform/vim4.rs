//! The board services own HDMI, USB descriptors and power negotiation.
use crate::config::{AppConfig, HidBackend, StreamMode};
use crate::video::format::{PixelFormat, Resolution};

pub mod hdr10;
mod usb_serial;
pub mod video_backend;
pub mod wave5_caps;
pub use video_backend::VideoBackend;

pub const VIDEO_DEVICE: &str = "/dev/video_cap";
pub const GADGET: &str = "/sys/kernel/config/usb_gadget/kvm";

/// Shared default; absent on older installations that still use the app preference.
pub fn global_hdr_default() -> Option<bool> {
    let text = std::fs::read_to_string("/etc/default/vim4-kvm-hdr").ok()?;
    parse_global_hdr_default(&text)
}

fn parse_global_hdr_default(text: &str) -> Option<bool> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if key.trim() != "HDR_SDR_ENABLED" {
                return None;
            }
            match value.trim() {
                "0" => Some(true),
                "1" => Some(false),
                _ => None,
            }
        })
        .last()
}

pub fn hdr_remote_available() -> bool {
    if VideoBackend::current() == VideoBackend::Wave5 {
        return wave5_caps::current().p010_main10
            && wave5_caps::capture_has_amly()
            && gstreamer::init().is_ok()
            && gstreamer::ElementFactory::find("vim4amlysrc").is_some()
            && gstreamer::ElementFactory::find("v4l2h265enc").is_some_and(|factory| {
                let caps = "video/x-raw,format=P010_10LE"
                    .parse::<gstreamer::Caps>()
                    .unwrap();
                factory.static_pad_templates().iter().any(|pad| {
                    pad.direction() == gstreamer::PadDirection::Sink
                        && pad.caps().can_intersect(&caps)
                })
            });
    }
    VideoBackend::current() == VideoBackend::Vendor
        && std::path::Path::new("/usr/lib/vim4-kvm/gstreamer-1.0/libgstvim4hevc10.so").exists()
        && std::fs::read_to_string("/sys/module/vfm_cap/parameters/hdr_metadata_version")
            .is_ok_and(|value| value.trim() == "1")
        && std::fs::read_to_string("/sys/module/aml_media/parameters/vdin0_native_hdr")
            .is_ok_and(|value| value.trim() == "Y")
}

pub fn pq_input() -> bool {
    if VideoBackend::current() == VideoBackend::Wave5 {
        return hdr10::current().is_some();
    }
    std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/info")
        .is_ok_and(|info| parse_pq_input(&info))
}

fn parse_pq_input(info: &str) -> bool {
    // The vendor driver spells ST2084 as ST_2048 in this sysfs ABI.
    parse_signal(info).is_some()
        && info.lines().any(|line| {
            line.split_once(':').is_some_and(|(key, value)| {
                key.trim() == "HDR EOTF" && value.trim() == "SMPTE_ST_2048"
            })
        })
}

fn usb_helper() -> &'static str {
    match VideoBackend::current() {
        VideoBackend::Wave5 if !video_backend::shared_data() => "/usr/libexec/vim4-onekvm-edge",
        VideoBackend::Wave5 => "/usr/libexec/vim4-onekvm",
        VideoBackend::Vendor => "/usr/libexec/vim4-onekvm",
    }
}

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
        let id = format!(
            "vim4-{}",
            self.id
                .bytes()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let path = std::path::Path::new(root).join(&self.name);
        let mut image = crate::msd::ImageInfo::new(id, self.name, path, self.size);
        image.source = Some(self.source);
        Ok(image)
    }
}

pub async fn catalog_images() -> crate::error::Result<Vec<crate::msd::ImageInfo>> {
    use crate::error::AppError;
    let output = tokio::process::Command::new("sudo")
        .args(["-n", usb_helper(), "catalog"])
        .output()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !output.status.success() {
        return Err(AppError::Internal(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    let entries: Vec<CatalogEntry> =
        serde_json::from_slice(&output.stdout).map_err(|e| AppError::Internal(e.to_string()))?;
    entries
        .into_iter()
        .filter(|entry| entry.source != "onekvm")
        .map(CatalogEntry::into_image)
        .collect()
}

pub async fn find_image(
    manager: &crate::msd::ImageManager,
    id: &str,
) -> crate::error::Result<crate::msd::ImageInfo> {
    if id.starts_with("vim4-") {
        return catalog_images()
            .await?
            .into_iter()
            .find(|image| image.id == id)
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

/// Validate endpoint capacity before closing active HID handles.
pub fn validate_usb_config(previous: &AppConfig, config: &AppConfig) -> crate::error::Result<()> {
    validate_usb_profile(
        &config.hid,
        &config.msd,
        &config.otg_network,
        &config.uac,
        &config.otg_serial,
        VideoBackend::current() == VideoBackend::Vendor || usb_serial::available(),
    )?;
    if VideoBackend::current() == VideoBackend::Wave5 {
        let path = std::path::Path::new(GADGET).join("functions/mass_storage.disk0/lun.0/file");
        let mounted = match std::fs::read_to_string(path) {
            Ok(value) => !value.trim().is_empty(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(crate::error::AppError::Config(format!(
                    "Cannot check USB media: {error}"
                )))
            }
        };
        validate_mounted_usb_change(previous, config, mounted)?;
    }
    Ok(())
}

fn validate_mounted_usb_change(
    previous: &AppConfig,
    requested: &AppConfig,
    mounted: bool,
) -> crate::error::Result<()> {
    let old = (
        &previous.hid,
        &previous.msd,
        &previous.otg_network,
        &previous.uac,
        &previous.otg_serial,
    );
    let new = (
        &requested.hid,
        &requested.msd,
        &requested.otg_network,
        &requested.uac,
        &requested.otg_serial,
    );
    if mounted && old != new {
        return Err(crate::error::AppError::BadRequest(
            "Safely eject virtual media on the controlled computer before changing USB functions"
                .into(),
        ));
    }
    Ok(())
}

fn validate_usb_profile(
    hid: &crate::config::HidConfig,
    msd: &crate::config::MsdConfig,
    network: &crate::config::OtgNetworkConfig,
    uac: &crate::config::UacConfig,
    serial: &crate::config::OtgSerialConfig,
    serial_available: bool,
) -> crate::error::Result<(crate::config::OtgHidFunctions, u8)> {
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
    if serial.enabled && !serial_available {
        return Err(AppError::BadRequest(
            "USB serial requires the running kernel's CONFIG_USB_CONFIGFS_ACM and usb_f_acm module; install a matching ACM-enabled kernel and modules".into(),
        ));
    }
    Ok((functions, endpoints))
}

pub async fn configure_usb(
    hid: &crate::config::HidConfig,
    msd: &crate::config::MsdConfig,
    network: &crate::config::OtgNetworkConfig,
    uac: &crate::config::UacConfig,
    serial: &crate::config::OtgSerialConfig,
) -> crate::error::Result<()> {
    use crate::error::AppError;
    let (functions, endpoints) = validate_usb_profile(
        hid,
        msd,
        network,
        uac,
        serial,
        VideoBackend::current() == VideoBackend::Vendor || usb_serial::available(),
    )?;
    if VideoBackend::current() == VideoBackend::Wave5 {
        // Keep video usable if USB preparation failed and HID is disabled.
        // Otherwise use the same restricted configfs helper as production;
        // it preserves the board-owned HID instances when changing links.
        if endpoints == 0 && !std::path::Path::new(GADGET).exists() {
            return Ok(());
        }
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
        .args(["-n", usb_helper(), "usb", &profile.to_string()])
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "qp", rename_all = "snake_case")]
pub enum HevcQuality {
    #[default]
    Bitrate,
    FixedQp(u8),
    Lossless,
}

impl HevcQuality {
    fn is_bitrate(&self) -> bool {
        matches!(self, Self::Bitrate)
    }
    pub fn encoder_properties(self) -> String {
        match self {
            Self::Bitrate => String::new(),
            Self::FixedQp(qp) => format!("fixed-qp={qp}"),
            Self::Lossless => "lossless=true".into(),
        }
    }
}

/// Output limits are separate from HDMI timings, which always follow the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputSettings {
    /// Maximum output height; zero preserves the input size. Never upscale.
    pub height: u32,
    /// Maximum frame rate; zero follows the source. Each encoder applies its own limit.
    pub fps: u32,
    /// Preserve HDR10 in HEVC Main10. AVC and JPEG retain SDR compatibility.
    pub hdr: bool,
    #[serde(skip_serializing_if = "HevcQuality::is_bitrate")]
    pub hevc_quality: HevcQuality,
}

impl OutputSettings {
    pub fn validate(self) -> crate::error::Result<()> {
        if matches!(self.hevc_quality, HevcQuality::FixedQp(qp) if qp > 51) {
            return Err(crate::error::AppError::BadRequest(
                "HEVC QP must be between 0 and 51".into(),
            ));
        }
        if (self.height != 0 && (!(180..=2160).contains(&self.height) || self.height % 2 != 0))
            || (self.fps != 0 && !(5..=120).contains(&self.fps))
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

    pub fn h26x_frame_rate(
        self,
        source: Resolution,
        source_fps: u32,
        backend: VideoBackend,
    ) -> u32 {
        // Scope high refresh to native 1080p and 1440p on the vendor path.
        // Scaling a UHD source down must not raise its capture/encoding budget.
        let maximum = if backend == VideoBackend::Vendor
            && matches!((source.width, source.height), (1920, 1080) | (2560, 1440))
        {
            120
        } else {
            60
        };
        self.frame_rate(source_fps, maximum)
    }

    /// Build a scaled stream with the encoder's horizontal pixel alignment.
    /// Callers use two for ordinary 4:2:0, sixteen for vendor HEVC DMA rows.
    pub fn filter(self, source: Resolution, fps: u32, width_alignment: u32) -> String {
        let mut filter =
            format!("videorate name=output_rate drop-only=true ! video/x-raw,framerate={fps}/1 ! ");
        if self.height > 0 && self.height < source.height {
            // The vendor plugin assumes tightly packed input. The common DMA
            // scaler separates visible width from pitch for widths like 3440.
            let (scaler, format) = if source.width % 64 != 0 {
                ("vim4scale name=scaler", "NV12")
            } else {
                ("amlvconv name=scaler", "NV12")
            };
            let width = (u64::from(source.width) * u64::from(self.height)
                / u64::from(source.height)) as u32;
            let width = width.max(width_alignment) / width_alignment * width_alignment;
            filter.push_str(&format!(
                "{scaler} ! video/x-raw,format={format},width={},height={} ! \
                 queue max-size-buffers=1 max-size-bytes=0 max-size-time=0 ! ",
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
            config.audio.device = "hw:VIM4HDMIRX,0".into();
            config.audio.enabled = std::path::Path::new("/proc/asound/VIM4HDMIRX/id").exists();
            config.hid.otg_profile = crate::config::OtgHidProfile::FullNoConsumer;
            config.msd.enabled = false;
            config.otg_network.enabled = false;
            config.uac.enabled = false;
            config.otg_serial.enabled = false;
        }
    }
}

/// Resolve board-owned device names without changing user-selected USB audio.
pub fn resolve_hardware_config(config: &mut AppConfig, backend: VideoBackend) {
    if matches!(
        config.audio.device.as_str(),
        "hw:AMLAUGESOUND,0" | "hw:VIM4HDMIRX,0"
    ) {
        config.audio.device = match backend {
            VideoBackend::Vendor => "hw:AMLAUGESOUND,0",
            VideoBackend::Wave5 => "hw:VIM4HDMIRX,0",
        }
        .into();
    }
    if matches!(config.video.format.as_deref(), Some("NV12" | "NV21")) {
        config.video.format = Some(
            match backend {
                VideoBackend::Vendor => "NV21",
                VideoBackend::Wave5 => "NV12",
            }
            .into(),
        );
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
        return Some((
            Resolution::new(status.width?, status.height?),
            status.fps?.round() as u32,
        ));
    }
    if !vendor_signal_ready() {
        return None;
    }
    let info = std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/info").ok()?;
    vendor_signal_ready().then(|| parse_capture_signal(&info))?
}

// hw_info reports cached RX state, without register access. Bit 16 is
// FSM_SIG_READY in hdmi_rx_repeater.h; timing values stay stale during loss.
fn signal_locked(text: &str) -> bool {
    u32::from_str_radix(text.trim(), 16).is_ok_and(|value| value & (1 << 16) != 0)
}

fn vendor_signal_ready() -> bool {
    match std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/hw_info") {
        Ok(text) => signal_locked(&text),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Confirm the same changed timing twice. A fleeting read or alternating modes
/// must not tear down a healthy DMA pipeline; sustained loss must release it.
#[derive(Default)]
pub(crate) struct SignalChange {
    candidate: Option<(Resolution, u32)>,
    count: u8,
}

impl SignalChange {
    pub fn observe(&mut self, current: (Resolution, u32), next: Option<(Resolution, u32)>) -> bool {
        if next == Some(current) {
            self.count = 0;
            return false;
        }
        if next != self.candidate {
            self.candidate = next;
            self.count = 0;
        }
        self.count = self.count.saturating_add(1);
        self.count >= 2
    }
}

// Capture caps and encoder budgets follow the base timing during VRR. Actual
// cadence remains visible in input_status(), and buffer PTS stays authoritative.
fn parse_capture_signal(info: &str) -> Option<(Resolution, u32)> {
    let (size, measured) = parse_signal(info)?;
    let rate = info.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "Capture Frame Rate").then_some(value.trim())
    });
    let fps = match rate {
        Some(value) => {
            let hundredths: u32 = value.parse().ok()?;
            if !(100..=12000).contains(&hundredths) {
                return None;
            }
            (hundredths + 50) / 100
        }
        None => measured.round() as u32,
    };
    Some((size, fps))
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
    configure_media_result(lun, config, expected)
        .await
        .map(|_| ())
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
    command.args(["-n", usb_helper(), "media"]);
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
    let result: Mounted =
        serde_json::from_slice(&output.stdout).map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(result.file)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stale_timings_do_not_establish_receiver_lock() {
        for text in ["3b11", "0", "", "invalid"] {
            assert!(!signal_locked(text));
        }
        assert!(signal_locked("13b11\n"));
    }

    #[test]
    fn signal_change_confirms_geometry_or_loss_without_restarting_on_flapping() {
        let current = (Resolution::new(3840, 2160), 60);
        let next = (Resolution::new(3440, 1440), 60);
        for target in [Some(next), None] {
            let mut change = SignalChange::default();
            assert!(!change.observe(current, target));
            assert!(change.observe(current, target));
        }
        let mut change = SignalChange::default();
        for target in [
            Some(next),
            None,
            Some(next),
            Some(current),
            None,
            Some(current),
        ] {
            assert!(!change.observe(current, target));
        }
    }

    #[test]
    fn shared_hdr_default_migrates_only_an_explicit_valid_setting() {
        for text in [
            "",
            "HDR_SDR_WHITE=203\n",
            "HDR_SDR_ENABLED=false\n",
            "HDR_SDR_ENABLED=2\n",
        ] {
            assert_eq!(super::parse_global_hdr_default(text), None);
        }
        assert_eq!(
            super::parse_global_hdr_default("HDR_SDR_ENABLED=0\nHDR_SDR_WHITE=250\n"),
            Some(true)
        );
        assert_eq!(
            super::parse_global_hdr_default("HDR_SDR_ENABLED=0\nHDR_SDR_ENABLED=1\n"),
            Some(false)
        );
    }
    use super::*;
    #[test]
    fn shared_config_roundtrip_preserves_user_settings_and_resolves_hardware() {
        let mut config = AppConfig::default();
        config.initialized = true;
        config.audio.device = "hw:AMLAUGESOUND,0".into();
        config.video.format = Some("NV21".into());
        config.video.width = 1920;
        config.audio.enabled = false;
        let original = serde_json::to_value(&config).unwrap();
        resolve_hardware_config(&mut config, VideoBackend::Wave5);
        assert_eq!(config.audio.device, "hw:VIM4HDMIRX,0");
        assert_eq!(config.video.format.as_deref(), Some("NV12"));
        resolve_hardware_config(&mut config, VideoBackend::Vendor);
        assert_eq!(serde_json::to_value(&config).unwrap(), original);
        config.audio.device = "hw:USB,0".into();
        resolve_hardware_config(&mut config, VideoBackend::Wave5);
        assert_eq!(config.audio.device, "hw:USB,0");
    }
    #[test]
    fn usb_profiles_reserve_media_capacity_and_reject_endpoint_overcommit() {
        use crate::config::*;
        for serial_available in [false, true] {
            for mask in 0u16..256 {
                let enabled = |bit: u32| mask & (1u16 << bit) != 0u16;
                let mut config = AppConfig::default();
                config.hid.backend = HidBackend::Otg;
                config.hid.otg_profile = OtgHidProfile::Custom;
                config.hid.otg_functions = OtgHidFunctions {
                    keyboard: enabled(0),
                    mouse_absolute: enabled(1),
                    mouse_relative: enabled(2),
                    consumer: enabled(3),
                };
                config.msd.enabled = enabled(4);
                config.otg_network.enabled = enabled(5);
                config.uac.enabled = enabled(6);
                config.otg_serial.enabled = enabled(7);
                let endpoints = (mask & 0x1fu16).count_ones()
                    + u32::from(enabled(5)) * 2
                    + u32::from(enabled(6))
                    + u32::from(enabled(7)) * 2;
                let result = validate_usb_profile(
                    &config.hid,
                    &config.msd,
                    &config.otg_network,
                    &config.uac,
                    &config.otg_serial,
                    serial_available,
                );
                assert_eq!(
                    result.is_ok(),
                    endpoints <= 5 && (serial_available || !enabled(7)),
                    "serial_available={serial_available} {mask:#x}"
                );
            }
        }
    }
    #[test]
    fn mounted_media_blocks_usb_changes_but_allows_idempotent_or_video_updates() {
        let original = AppConfig::default();
        let mut requested = original.clone();
        assert!(validate_mounted_usb_change(&original, &requested, true).is_ok());
        requested.video.width = 1920;
        assert!(validate_mounted_usb_change(&original, &requested, true).is_ok());
        requested.hid.otg_descriptor.product = "Changed USB identity".into();
        assert!(validate_mounted_usb_change(&original, &requested, true).is_err());
        assert!(validate_mounted_usb_change(&original, &requested, false).is_ok());
        requested = original.clone();
        requested.msd.enabled = !original.msd.enabled;
        assert!(validate_mounted_usb_change(&original, &requested, true).is_err());
    }
    #[test]
    fn msd_catalog_identity_preserves_source_and_real_lun_path() {
        let make = |source: &str| {
            CatalogEntry {
                id: format!("{source}/install disk.img"),
                name: "install disk.img".into(),
                source: source.into(),
                size: 4096,
            }
            .into_image()
            .unwrap()
        };
        let data = make("data");
        let foreign = make("pikvm");
        assert_ne!(data.id, foreign.id);
        assert_eq!(data.id, make("data").id);
        assert!(!data.id.contains('/') && !data.id.contains(' '));
        assert_eq!(
            data.path,
            std::path::Path::new("/var/lib/vim4-kvm/usb/install disk.img")
        );
        assert_eq!(foreign.source.as_deref(), Some("pikvm"));
        assert_eq!(
            foreign.path,
            std::path::Path::new("/var/lib/kvmd/msd/install disk.img")
        );
    }

    #[test]
    fn output_limits_preserve_aspect_ratio_and_never_increase_source_rate() {
        let settings = OutputSettings {
            height: 1080,
            fps: 30,
            hdr: false,
            ..Default::default()
        };
        assert_eq!(settings.frame_rate(24, 60), 24);
        assert_eq!(settings.frame_rate(60, 60), 30);
        assert_eq!(settings.frame_rate(60, 10), 10);
        assert!(settings
            .filter(Resolution::new(3840, 2160), 30, 2)
            .contains("width=1920,height=1080"));
        assert!(settings
            .filter(Resolution::new(2880, 2160), 30, 2)
            .contains("width=1440,height=1080"));
        assert!(!settings
            .filter(Resolution::new(1280, 720), 30, 2)
            .contains("amlvconv"));
        assert!(OutputSettings {
            height: 719,
            fps: 30,
            hdr: false,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(validate_bitrate(crate::config::BitratePreset::Custom(60000)).is_ok());
        assert!(validate_bitrate(crate::config::BitratePreset::Custom(60001)).is_err());
    }
    #[test]
    fn high_refresh_is_limited_by_native_input_backend_and_user_cap() {
        let native = Resolution::new(1920, 1080);
        let automatic = OutputSettings::default();
        assert_eq!(
            automatic.h26x_frame_rate(native, 120, VideoBackend::Vendor),
            120
        );
        assert_eq!(
            automatic.h26x_frame_rate(native, 60, VideoBackend::Vendor),
            60
        );
        assert_eq!(
            automatic.h26x_frame_rate(native, 120, VideoBackend::Wave5),
            60
        );
        let limited = OutputSettings {
            height: 720,
            fps: 60,
            hdr: false,
            ..Default::default()
        };
        assert_eq!(
            limited.h26x_frame_rate(native, 120, VideoBackend::Vendor),
            60
        );
        let high = OutputSettings {
            height: 1080,
            fps: 120,
            hdr: false,
            ..Default::default()
        };
        assert!(high.validate().is_ok());
        assert_eq!(high.h26x_frame_rate(native, 100, VideoBackend::Vendor), 100);
        assert_eq!(
            high.h26x_frame_rate(Resolution::new(3840, 2160), 120, VideoBackend::Vendor),
            60
        );
        assert_eq!(
            high.h26x_frame_rate(Resolution::new(2560, 1440), 120, VideoBackend::Vendor),
            120
        );
        assert_eq!(
            automatic.h26x_frame_rate(Resolution::new(2560, 1440), 120, VideoBackend::Wave5),
            60
        );
        assert_eq!(
            limited.h26x_frame_rate(Resolution::new(2560, 1440), 120, VideoBackend::Vendor),
            60
        );
        assert_eq!(
            automatic.h26x_frame_rate(Resolution::new(3440, 1440), 120, VideoBackend::Vendor),
            60
        );
        assert!(OutputSettings { fps: 121, ..high }.validate().is_err());
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

    #[test]
    fn native_hdr_requires_a_locked_pq_input() {
        let signal = "Hactive: 3840\nVactive: 2160\nFrame Rate: 5999\n";
        for eotf in ["SDR", "HDR", "HLG", "", "SMPTE_ST_2048"] {
            let info = format!("{signal}HDR EOTF: {eotf}\n");
            assert_eq!(parse_pq_input(&info), eotf == "SMPTE_ST_2048");
            assert!(!parse_pq_input(&info.replace("5999", "0")));
        }
        assert!(!parse_pq_input(signal));
    }

    #[test]
    fn vrr_cadence_does_not_change_capture_mode_but_geometry_still_does() {
        for rate in [4800, 5994, 7200, 9000, 11988, 12000] {
            let info = format!(
                "Hactive: 1920\nVactive: 1080\nFrame Rate: {rate}\nCapture Frame Rate: 12000\n"
            );
            assert_eq!(
                parse_capture_signal(&info),
                Some((Resolution::new(1920, 1080), 120))
            );
            assert_eq!(parse_signal(&info).unwrap().1, f64::from(rate) / 100.0);
            assert_eq!(
                parse_capture_signal(&info.replace("Vactive: 1080", "Vactive: 720")),
                Some((Resolution::new(1920, 720), 120))
            );
        }
        let info = "Hactive: 1920\nVactive: 1080\nFrame Rate: 5000\n";
        assert_eq!(
            parse_capture_signal(info),
            Some((Resolution::new(1920, 1080), 50))
        );
        assert_eq!(
            parse_capture_signal(&format!("{info}Capture Frame Rate: 0\n")),
            None
        );
        assert_eq!(
            parse_capture_signal(&format!("{info}Capture Frame Rate: 14400\n")),
            None
        );
    }
}
