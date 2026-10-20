//! Explicit selection keeps the experimental V4L2 path away from the vendor ABI.
use super::*;
use crate::error::{AppError, Result};
use crate::video::device::VideoInputStatus;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use v4l2r::bindings::{v4l2_input, V4L2_IN_ST_NO_SIGNAL, V4L2_IN_ST_NO_SYNC};

v4l2r::nix::ioctl_readwrite!(query_input, b'V', 26, v4l2_input);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoBackend {
    Vendor,
    Wave5,
}

impl VideoBackend {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value {
            None | Some("vendor") => Ok(Self::Vendor),
            Some("wave5") => Ok(Self::Wave5),
            Some(_) => Err(AppError::Config(
                "VIM4_VIDEO_BACKEND must be vendor or wave5".into(),
            )),
        }
    }

    pub fn current() -> Self {
        // Capability reporting also runs before RuntimeBuilder. Invalid values
        // remain inert here; startup returns a configuration error before DB access.
        Self::parse(std::env::var("VIM4_VIDEO_BACKEND").ok().as_deref())
            .unwrap_or(Self::Vendor)
    }

    pub fn input_format(self) -> PixelFormat {
        match self {
            Self::Vendor => PixelFormat::Nv21,
            Self::Wave5 => PixelFormat::Nv12,
        }
    }
}

pub fn validate_runtime(data_dir: &Path) -> Result<()> {
    let backend = VideoBackend::parse(std::env::var("VIM4_VIDEO_BACKEND").ok().as_deref())?;
    if backend == VideoBackend::Wave5 {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")?;
        validate_wave5_location(&release, data_dir)?;
    }
    Ok(())
}

fn validate_wave5_location(release: &str, data_dir: &Path) -> Result<()> {
    if !release.starts_with("7.") {
        return Err(AppError::Config(
            "Wave5 requires the experimental 7.x kernel".into(),
        ));
    }
    let resolved = data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.to_path_buf());
    if !resolved.is_absolute() || resolved.starts_with("/var/lib/one-kvm") {
        return Err(AppError::Config(
            "Wave5 requires a separate absolute data directory".into(),
        ));
    }
    Ok(())
}

pub fn wave5_device(name: &str) -> Result<PathBuf> {
    let mut devices = Vec::new();
    for entry in std::fs::read_dir("/sys/class/video4linux")? {
        let entry = entry?;
        if std::fs::read_to_string(entry.path().join("name"))?.trim() == name {
            devices.push(Path::new("/dev").join(entry.file_name()));
        }
    }
    if devices.len() != 1 {
        return Err(AppError::VideoError(format!(
            "Expected one {name} device, found {}",
            devices.len()
        )));
    }
    Ok(devices.remove(0))
}

pub fn wave5_input_status() -> VideoInputStatus {
    fn probe() -> Result<VideoInputStatus> {
        let file = File::open(wave5_device("t7-vdin-test")?)?;
        let caps: v4l2r::ioctl::Capability =
            v4l2r::ioctl::querycap(&file).map_err(|e| AppError::VideoError(e.to_string()))?;
        if caps.driver != "t7-vdin-test" {
            return Err(AppError::VideoError(
                "Unexpected HDMI capture driver".into(),
            ));
        }
        // v4l2_input is a kernel UAPI struct consisting of integer/byte fields.
        let mut input: v4l2_input = unsafe { std::mem::zeroed() };
        unsafe { query_input(file.as_raw_fd(), &mut input) }
            .map_err(|e| AppError::VideoError(e.to_string()))?;
        if input.status & (V4L2_IN_ST_NO_SIGNAL | V4L2_IN_ST_NO_SYNC) != 0 {
            return Ok(VideoInputStatus::no_signal());
        }
        let format: v4l2r::Format = v4l2r::ioctl::g_fmt(&file, v4l2r::QueueType::VideoCapture)
            .map_err(|e| AppError::VideoError(e.to_string()))?;
        if PixelFormat::from_v4l2r(format.pixelformat) != Some(PixelFormat::Nv12) {
            return Err(AppError::VideoError(
                "Wave5 capture must be configured for NV12".into(),
            ));
        }
        // The experimental receiver currently supports fixed 1080p60/UHD60 only.
        Ok(VideoInputStatus::locked(
            PixelFormat::Nv12,
            format.width,
            format.height,
            60.0,
        ))
    }
    probe().unwrap_or_else(|error| {
        tracing::debug!("Wave5 input status unavailable: {error}");
        VideoInputStatus::unavailable()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave5_requires_explicit_selection_and_separate_configuration() {
        assert_eq!(VideoBackend::parse(None).unwrap(), VideoBackend::Vendor);
        assert!(VideoBackend::parse(Some("auto")).is_err());
        assert!(validate_wave5_location("5.15.137-vim4kvm", Path::new("/var/tmp/edge")).is_err());
        assert!(validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm")).is_err());
        assert!(validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm/child")).is_err());
        assert!(validate_wave5_location("7.3.0-rc7", Path::new("relative")).is_err());
        assert!(validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm-edge")).is_ok());
    }
}
