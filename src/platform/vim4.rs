//! The board services own HDMI, USB descriptors and power negotiation.
use crate::config::{AppConfig, HidBackend, StreamMode};
use crate::video::format::{PixelFormat, Resolution};

pub const VIDEO_DEVICE: &str = "/dev/video_cap";
pub const GADGET: &str = "/sys/kernel/config/usb_gadget/kvm";

pub fn validate_bitrate(preset: crate::config::BitratePreset) -> crate::error::Result<()> {
    if !(1000..=12000).contains(&preset.bitrate_kbps()) {
        return Err(crate::error::AppError::BadRequest(
            "VIM4 bitrate must be 1000–12000 kbps".into(),
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
        if ![0, 720, 1080, 2160].contains(&self.height) || ![0, 10, 15, 30, 60].contains(&self.fps)
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
    parse_signal(&std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/info").ok()?)
        .map(|(size, fps)| (size, fps.round() as u32))
}

pub fn input_status() -> crate::video::device::VideoInputStatus {
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
        .arg(if config.ro { "true" } else { "false" });
    let output = command
        .output()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !output.status.success() {
        return Err(AppError::Internal(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(validate_bitrate(crate::config::BitratePreset::Custom(12001)).is_err());
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
