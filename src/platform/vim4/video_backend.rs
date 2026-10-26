//! Explicit selection keeps the experimental V4L2 path away from the vendor ABI.
use super::*;
use crate::error::{AppError, Result};
use crate::video::device::VideoInputStatus;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use v4l2r::bindings::{
    v4l2_dv_timings, v4l2_input, V4L2_DV_BT_656_1120, V4L2_IN_ST_NO_SIGNAL, V4L2_IN_ST_NO_SYNC,
};

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
        Self::parse(std::env::var("VIM4_VIDEO_BACKEND").ok().as_deref()).unwrap_or(Self::Vendor)
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
        validate_wave5_location(&release, data_dir, shared_data())?;
    }
    Ok(())
}

/// The VDIN MIF writes 16-byte strides. Never enlarge the HDMI input image.
pub fn output_resolution(output: OutputSettings, source: Resolution) -> Resolution {
    if output.height == 0 || output.height >= source.height {
        return source;
    }
    let width =
        (u64::from(source.width) * u64::from(output.height) / u64::from(source.height)) as u32;
    Resolution::new((width & !15).max(320), output.height)
}

/// HEVC's legacy visible-size alignment is stricter than AVC's crop units.
pub fn wave5_requires_h264(output: OutputSettings, source: Resolution) -> bool {
    let visible = output_resolution(output, source);
    visible.width % 8 != 0 || visible.height % 8 != 0
}

pub fn shared_data() -> bool {
    std::env::var("VIM4_SHARED_DATA").as_deref() == Ok("1")
}

fn validate_wave5_location(release: &str, data_dir: &Path, shared: bool) -> Result<()> {
    if !release.starts_with("7.") {
        return Err(AppError::Config(
            "Wave5 requires the experimental 7.x kernel".into(),
        ));
    }
    let resolved = data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.to_path_buf());
    let production = Path::new("/var/lib/one-kvm");
    if !resolved.is_absolute()
        || (resolved.starts_with(production) && !(shared && resolved == production))
    {
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
        let timings: v4l2_dv_timings = v4l2r::ioctl::query_dv_timings(&file)
            .map_err(|e| AppError::VideoError(e.to_string()))?;
        // G_FMT describes the old buffer layout until the pipeline releases its
        // buffers and applies S_FMT. Report the detected input independently so
        // the existing source monitor can initiate that rebuild.
        measured_input_status(timings)
    }
    probe().unwrap_or_else(|error| {
        tracing::debug!("Wave5 input status unavailable: {error}");
        VideoInputStatus::unavailable()
    })
}

fn measured_input_status(timings: v4l2_dv_timings) -> Result<VideoInputStatus> {
    let invalid = || AppError::VideoError("Unsupported HDMI input timings".into());
    if timings.type_ != V4L2_DV_BT_656_1120 {
        return Err(invalid());
    }
    // The tagged UAPI union contains BT timings only after the type check.
    let bt = unsafe { timings.__bindgen_anon_1.bt };
    let (width, height, pixelclock) = (bt.width, bt.height, bt.pixelclock);
    if bt.interlaced != 0
        || !(320..=3840).contains(&width)
        || !(180..=2160).contains(&height)
        || bt.width % 2 != 0
        || bt.height % 2 != 0
        || !(24_000_000..=600_000_000).contains(&pixelclock)
    {
        return Err(invalid());
    }
    let total_width = u64::from(bt.width)
        + u64::from(bt.hfrontporch)
        + u64::from(bt.hsync)
        + u64::from(bt.hbackporch);
    let total_height = u64::from(bt.height)
        + u64::from(bt.vfrontporch)
        + u64::from(bt.vsync)
        + u64::from(bt.vbackporch);
    let fps = bt.pixelclock as f64 / total_width as f64 / total_height as f64;
    if !(20.0..=120.0).contains(&fps) {
        return Err(invalid());
    }
    Ok(VideoInputStatus::locked(
        PixelFormat::Nv12,
        bt.width,
        bt.height,
        fps,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_fallback_uses_visible_output_including_scaling() {
        for source in [
            Resolution::new(1366, 768),
            Resolution::new(1440, 900),
            Resolution::new(1680, 1050),
        ] {
            assert!(wave5_requires_h264(OutputSettings::default(), source));
        }
        for source in [
            Resolution::new(640, 480),
            Resolution::new(720, 400),
            Resolution::new(1920, 1080),
            Resolution::new(3840, 2160),
        ] {
            assert!(!wave5_requires_h264(OutputSettings::default(), source));
        }
        assert!(!wave5_requires_h264(
            OutputSettings {
                height: 720,
                fps: 0,
                hdr: false,
                ..Default::default()
            },
            Resolution::new(1366, 768)
        ));
        assert!(wave5_requires_h264(
            OutputSettings {
                height: 900,
                fps: 0,
                hdr: false,
                ..Default::default()
            },
            Resolution::new(3840, 2160)
        ));
    }

    #[test]
    fn downscaled_dma_geometry_never_upscales_or_uses_fractional_mif_strides() {
        for source in [Resolution::new(3840, 2160), Resolution::new(1920, 1080)] {
            assert_eq!(output_resolution(OutputSettings::default(), source), source);
            for height in (180..=2160).step_by(2) {
                let output = output_resolution(
                    OutputSettings {
                        height,
                        fps: 0,
                        hdr: false,
                        ..Default::default()
                    },
                    source,
                );
                assert!(output.width <= source.width && output.height <= source.height);
                assert_eq!(output.width % 16, 0);
                assert_eq!(output.height % 2, 0);
                let ideal = u64::from(source.width) * u64::from(output.height);
                let actual = u64::from(output.width) * u64::from(source.height);
                assert!(ideal >= actual && ideal - actual < 16 * u64::from(source.height));
            }
        }
    }

    #[test]
    fn wave5_requires_explicit_selection_and_separate_configuration() {
        assert_eq!(VideoBackend::parse(None).unwrap(), VideoBackend::Vendor);
        assert!(VideoBackend::parse(Some("auto")).is_err());
        assert!(
            validate_wave5_location("5.15.137-vim4kvm", Path::new("/var/tmp/edge"), false).is_err()
        );
        assert!(
            validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm"), false).is_err()
        );
        assert!(
            validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm/child"), true)
                .is_err()
        );
        assert!(validate_wave5_location("7.3.0-rc7", Path::new("relative"), true).is_err());
        assert!(
            validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm-edge"), false).is_ok()
        );
        assert!(validate_wave5_location("7.3.0-rc7", Path::new("/var/lib/one-kvm"), true).is_ok());
        assert!(
            validate_wave5_location("5.15.137-vim4kvm", Path::new("/var/lib/one-kvm"), true)
                .is_err()
        );
    }

    #[test]
    fn wave5_reports_detected_modes_and_measured_fractional_rate() {
        // These are actual wire totals, independent of the output encoder rate.
        let mut mode: v4l2_dv_timings = unsafe { std::mem::zeroed() };
        mode.type_ = V4L2_DV_BT_656_1120;
        let mut bt: v4l2r::bindings::v4l2_bt_timings = unsafe { std::mem::zeroed() };
        bt.width = 3840;
        bt.height = 2160;
        bt.hfrontporch = 176;
        bt.hsync = 88;
        bt.hbackporch = 296;
        bt.vfrontporch = 8;
        bt.vsync = 10;
        bt.vbackporch = 72;
        bt.pixelclock = 593_406_593;
        mode.__bindgen_anon_1.bt = bt;
        let measured = measured_input_status(mode).unwrap();
        assert!((measured.fps.unwrap() - 60_000.0 / 1001.0).abs() < 0.0001);
        let mut fhd = bt;
        fhd.width = 1920;
        fhd.height = 1080;
        fhd.hfrontporch = 88;
        fhd.hsync = 44;
        fhd.hbackporch = 148;
        fhd.vfrontporch = 4;
        fhd.vsync = 5;
        fhd.vbackporch = 36;
        fhd.pixelclock = 148_500_000;
        mode.__bindgen_anon_1.bt = fhd;
        let changed = measured_input_status(mode).unwrap();
        assert_eq!((changed.width, changed.height), (Some(1920), Some(1080)));
        assert!((changed.fps.unwrap() - 60.0).abs() < 0.0001);
        bt.pixelclock = 297_000_000;
        mode.__bindgen_anon_1.bt = bt;
        assert!((measured_input_status(mode).unwrap().fps.unwrap() - 30.0).abs() < 0.001);
        bt.pixelclock = 0;
        mode.__bindgen_anon_1.bt = bt;
        assert!(measured_input_status(mode).is_err());
        bt.pixelclock = 594_000_000;
        bt.interlaced = 1;
        mode.__bindgen_anon_1.bt = bt;
        assert!(measured_input_status(mode).is_err());
        mode.type_ = u32::MAX;
        assert!(measured_input_status(mode).is_err());
    }
}
