//! The board services own HDMI, USB descriptors and power negotiation.
use crate::config::{AppConfig, HidBackend, StreamMode};
use crate::video::format::{PixelFormat, Resolution};

pub const VIDEO_DEVICE: &str = "/dev/video_cap";
pub const GADGET: &str = "/sys/kernel/config/usb_gadget/kvm";

pub fn defaults(config: &mut AppConfig) {
    if !config.initialized {
        config.video.device = Some(VIDEO_DEVICE.into());
        config.video.format = Some("NV21".into());
        config.video.width = 3840;
        config.video.height = 2160;
        config.video.fps = 60;
        config.stream.mode = StreamMode::WebRTC;
        config.hid.backend = HidBackend::Otg;
        config.audio.enabled = true;
        config.audio.device = "hw:AMLAUGESOUND,0".into();
    }
}

/// HDMI RX reports the clock-domain width (half-width for YUV420).
pub fn parse_signal(info: &str) -> Option<(Resolution, u32)> {
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
    Some((Resolution::new(width, height), (rate + 50) / 100))
}

pub fn signal() -> Option<(Resolution, u32)> {
    parse_signal(&std::fs::read_to_string("/sys/class/hdmirx/hdmirx0/info").ok()?)
}

pub fn input_status() -> crate::video::device::VideoInputStatus {
    match signal() {
        Some((size, fps)) => crate::video::device::VideoInputStatus::locked(
            PixelFormat::Nv21,
            size.width,
            size.height,
            f64::from(fps),
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
    fn hdmi_signal_handles_420_fractional_rate_and_missing_signal() {
        let info = "Hactive: 1920\nVactive: 2160\nFrame Rate: 5999\nColor Space: 3-YUV420\n";
        assert_eq!(parse_signal(info), Some((Resolution::new(3840, 2160), 60)));
        assert_eq!(parse_signal(&info.replace("5999", "0")), None);
        assert_eq!(parse_signal(""), None);
        assert_eq!(
            parse_signal(&info.replace("3-YUV420", "0-RGB")),
            Some((Resolution::new(1920, 2160), 60))
        );
    }
}
