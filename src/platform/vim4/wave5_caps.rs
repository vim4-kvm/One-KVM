//! Probe a private, bufferless encoder context. No REQBUFS or STREAMON is issued.
use super::{
    video_backend::{wave5_device, wave5_encoder_device},
    HevcQuality, VideoBackend,
};
use std::{fs::OpenOptions, os::fd::AsRawFd};
use v4l2r::bindings::*;
use v4l2r::bindings::{
    v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE as CAPTURE,
    v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_OUTPUT as SELECTION_OUTPUT,
    v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE as OUTPUT,
};

const HEVC: u32 = u32::from_le_bytes(*b"HEVC");
const NV12M: u32 = u32::from_le_bytes(*b"NM12");
const P010: u32 = u32::from_le_bytes(*b"P010");
// Private packed422 format from linux/amlogic-vdin.h; never encoder P010.
const AMLY: u32 = u32::from_le_bytes(*b"AMLY");

v4l2r::nix::ioctl_readwrite!(set_format, b'V', 5, v4l2_format);
v4l2r::nix::ioctl_readwrite!(get_format, b'V', 4, v4l2_format);
v4l2r::nix::ioctl_readwrite!(set_selection, b'V', 95, v4l2_selection);
v4l2r::nix::ioctl_readwrite!(query_control, b'V', 36, v4l2_queryctrl);
v4l2r::nix::ioctl_readwrite!(set_control, b'V', 28, v4l2_control);
v4l2r::nix::ioctl_readwrite!(enumerate_format, b'V', 2, v4l2_fmtdesc);
v4l2r::nix::ioctl_readwrite!(query_extended_control, b'V', 103, v4l2_query_ext_ctrl);

/// Read the capture format list without changing an active capture context.
/// Do not cache this alongside encoder capabilities: capture can be reopened
/// in a different mode after a source or global mapping policy change.
pub fn capture_has_amly() -> bool {
    let Ok(device) = wave5_device("t7-vdin-test") else {
        return false;
    };
    let Ok(file) = OpenOptions::new().read(true).open(device) else {
        return false;
    };
    let formats = capture_lists_amly(|index| {
        let mut fmt: v4l2_fmtdesc = unsafe { std::mem::zeroed() };
        fmt.index = index;
        fmt.type_ = v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE;
        unsafe { enumerate_format(file.as_raw_fd(), &mut fmt) }
            .ok()
            .map(|_| fmt.pixelformat)
    });
    formats
        && [
            (
                V4L2_CID_COLORIMETRY_HDR10_MASTERING_DISPLAY,
                std::mem::size_of::<v4l2_ctrl_hdr10_mastering_display>(),
            ),
            (
                V4L2_CID_COLORIMETRY_HDR10_CLL_INFO,
                std::mem::size_of::<v4l2_ctrl_hdr10_cll_info>(),
            ),
        ]
        .into_iter()
        .all(|(id, size)| {
            let mut ctrl: v4l2_query_ext_ctrl = unsafe { std::mem::zeroed() };
            ctrl.id = id;
            unsafe { query_extended_control(file.as_raw_fd(), &mut ctrl) }.is_ok()
                && ctrl.flags & V4L2_CTRL_FLAG_DISABLED == 0
                && ctrl.elems == 1
                && ctrl.elem_size as usize == size
        })
}

fn capture_lists_amly(mut format: impl FnMut(u32) -> Option<u32>) -> bool {
    for index in 0.. {
        match format(index) {
            Some(AMLY) => return true,
            Some(_) => continue,
            None => return false,
        }
    }
    false
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub visible_hevc: bool,
    pub fixed_qp: bool,
    pub lossless: bool,
    /// Encoder negotiation only; not an end-to-end HDR capability declaration.
    pub p010_main10: bool,
}

impl Capabilities {
    pub fn supports(self, quality: HevcQuality) -> bool {
        match quality {
            HevcQuality::Bitrate => true,
            HevcQuality::FixedQp(qp) => self.fixed_qp && qp <= 51,
            HevcQuality::Lossless => self.lossless,
        }
    }
}

pub fn current() -> Capabilities {
    if VideoBackend::current() == VideoBackend::Vendor {
        return Capabilities {
            visible_hevc: true,
            fixed_qp: true,
            lossless: true,
            p010_main10: true,
        };
    }
    // The on-board VPU cannot be hotplugged. Cache the probe so source polling
    // does not repeatedly open encoder contexts. Restart after driver changes.
    static CAPS: std::sync::OnceLock<Capabilities> = std::sync::OnceLock::new();
    *CAPS.get_or_init(|| {
        let caps = probe().unwrap_or_default();
        tracing::info!(?caps, "Wave5 encoder capabilities");
        caps
    })
}

fn quality_capabilities(mut accepts: impl FnMut(u32, i32, i32) -> bool) -> Capabilities {
    let rc = accepts(V4L2_CID_MPEG_VIDEO_FRAME_RC_ENABLE, 0, 0)
        && accepts(V4L2_CID_MPEG_VIDEO_MB_RC_ENABLE, 0, 0);
    Capabilities {
        visible_hevc: false,
        fixed_qp: rc
            && [
                V4L2_CID_MPEG_VIDEO_HEVC_MIN_QP,
                V4L2_CID_MPEG_VIDEO_HEVC_MAX_QP,
                V4L2_CID_MPEG_VIDEO_HEVC_I_FRAME_QP,
                V4L2_CID_MPEG_VIDEO_HEVC_P_FRAME_QP,
                V4L2_CID_MPEG_VIDEO_HEVC_B_FRAME_QP,
            ]
            .into_iter()
            .all(|id| accepts(id, 0, 51)),
        // Both supported Wave5 kernels unconditionally set skip_intra_trans=1.
        // LOSSLESS_CU therefore needs only frame/CU rate control disabled.
        lossless: rc && accepts(V4L2_CID_MPEG_VIDEO_HEVC_LOSSLESS_CU, 0, 1),
        p010_main10: false,
    }
}

fn probe() -> Option<Capabilities> {
    let device = wave5_encoder_device().ok()?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .ok()?;
    let fd = file.as_raw_fd();
    let mut caps = quality_capabilities(|id, min, max| {
        let mut ctrl: v4l2_queryctrl = unsafe { std::mem::zeroed() };
        ctrl.id = id;
        unsafe { query_control(fd, &mut ctrl) }.is_ok() && writable_range(&ctrl, min, max)
    });
    // Driver support alone is insufficient: stock GStreamer treats storage
    // alignment as visible caps and rejects the frame before S_SELECTION.
    let crop_caps = gstreamer::init().is_ok()
        && gstreamer::ElementFactory::find("v4l2h265enc")
            .is_some_and(|factory| factory.metadata("vim4-wave5-visible-crop-api") == Some("1"));
    caps.visible_hevc = crop_caps
        && probe_visible_hevc(
            |get, fmt| {
                if get {
                    unsafe { get_format(fd, fmt) }.is_ok()
                } else {
                    unsafe { set_format(fd, fmt) }.is_ok()
                }
            },
            |selection| unsafe { set_selection(fd, selection) }.is_ok(),
        );
    caps.p010_main10 = probe_p010_main10(
        |fmt| unsafe { set_format(fd, fmt) }.is_ok(),
        |ctrl| unsafe { set_control(fd, ctrl) }.is_ok(),
    );
    Some(caps)
}

fn probe_p010_main10(
    mut format: impl FnMut(&mut v4l2_format) -> bool,
    mut control: impl FnMut(&mut v4l2_control) -> bool,
) -> bool {
    let mut fmt: v4l2_format = unsafe { std::mem::zeroed() };
    fmt.type_ = CAPTURE;
    fmt.fmt.pix_mp.width = 640;
    fmt.fmt.pix_mp.height = 480;
    fmt.fmt.pix_mp.pixelformat = HEVC;
    if !format(&mut fmt) || unsafe { fmt.fmt.pix_mp.pixelformat } != HEVC {
        return false;
    }
    let mut profile = v4l2_control {
        id: V4L2_CID_MPEG_VIDEO_HEVC_PROFILE,
        value: v4l2_mpeg_video_hevc_profile_V4L2_MPEG_VIDEO_HEVC_PROFILE_MAIN_10 as i32,
    };
    if !control(&mut profile) {
        return false;
    }
    fmt = unsafe { std::mem::zeroed() };
    fmt.type_ = OUTPUT;
    fmt.fmt.pix_mp.width = 640;
    fmt.fmt.pix_mp.height = 480;
    fmt.fmt.pix_mp.pixelformat = P010;
    fmt.fmt.pix_mp.colorspace = v4l2_colorspace_V4L2_COLORSPACE_BT2020;
    fmt.fmt.pix_mp.xfer_func = v4l2_xfer_func_V4L2_XFER_FUNC_SMPTE2084 as u8;
    fmt.fmt.pix_mp.__bindgen_anon_1.ycbcr_enc = v4l2_ycbcr_encoding_V4L2_YCBCR_ENC_BT2020 as u8;
    fmt.fmt.pix_mp.quantization = v4l2_quantization_V4L2_QUANTIZATION_LIM_RANGE as u8;
    if !format(&mut fmt) {
        return false;
    }
    let raw = unsafe { fmt.fmt.pix_mp };
    let stride = raw.plane_fmt[0].bytesperline;
    // S_FMT can silently substitute a default format. Validate the actual
    // layout, including capacity for a full-width, half-height chroma plane.
    raw.pixelformat == P010
        && raw.width == 640
        && raw.height == 480
        && raw.num_planes == 1
        && stride >= 1280
        && stride % 32 == 0
        && u64::from(raw.plane_fmt[0].sizeimage) >= u64::from(stride) * 480 * 3 / 2
        && raw.colorspace == v4l2_colorspace_V4L2_COLORSPACE_BT2020
        && raw.xfer_func == v4l2_xfer_func_V4L2_XFER_FUNC_SMPTE2084 as u8
        && unsafe { raw.__bindgen_anon_1.ycbcr_enc }
            == v4l2_ycbcr_encoding_V4L2_YCBCR_ENC_BT2020 as u8
        && raw.quantization == v4l2_quantization_V4L2_QUANTIZATION_LIM_RANGE as u8
}

fn writable_range(ctrl: &v4l2_queryctrl, min: i32, max: i32) -> bool {
    ctrl.flags & (V4L2_CTRL_FLAG_DISABLED | V4L2_CTRL_FLAG_READ_ONLY) == 0
        && ctrl.minimum <= min
        && ctrl.maximum >= max
        && ctrl.step == 1
}

fn probe_visible_hevc(
    mut format: impl FnMut(bool, &mut v4l2_format) -> bool,
    mut select: impl FnMut(&mut v4l2_selection) -> bool,
) -> bool {
    (|| {
        let mut fmt: v4l2_format = unsafe { std::mem::zeroed() };
        fmt.type_ = CAPTURE;
        fmt.fmt.pix_mp.width = 1376;
        fmt.fmt.pix_mp.height = 912;
        fmt.fmt.pix_mp.pixelformat = HEVC;
        if !format(false, &mut fmt) {
            return None;
        }
        if unsafe { fmt.fmt.pix_mp.pixelformat } != HEVC {
            return None;
        }
        fmt.type_ = OUTPUT;
        fmt.fmt.pix_mp.width = 1376;
        fmt.fmt.pix_mp.height = 912;
        fmt.fmt.pix_mp.pixelformat = NV12M;
        if !format(false, &mut fmt) {
            return None;
        }
        if unsafe { fmt.fmt.pix_mp.pixelformat } != NV12M {
            return None;
        }
        let mut selection: v4l2_selection = unsafe { std::mem::zeroed() };
        selection.type_ = SELECTION_OUTPUT;
        selection.target = V4L2_SEL_TGT_CROP;
        selection.r.width = 1366;
        selection.r.height = 900;
        if !select(&mut selection) {
            return None;
        }
        fmt.type_ = CAPTURE;
        if !format(true, &mut fmt) {
            return None;
        }
        let coded = unsafe { fmt.fmt.pix_mp };
        Some(
            selection.r.width == 1366
                && selection.r.height == 900
                && coded.pixelformat == HEVC
                && coded.width == 1368
                && coded.height == 904,
        )
    })()
    .unwrap_or(false)
}

/// Use explicit per-picture QPs; CONSTANT_QUALITY is a rounded percentage.
pub fn quality_controls(quality: HevcQuality) -> Vec<(&'static str, i32)> {
    if quality == HevcQuality::Bitrate {
        return Vec::new();
    }
    let mut controls = vec![
        ("frame_level_rate_control_enable", 0),
        ("h264_mb_level_rate_control", 0),
    ];
    match quality {
        HevcQuality::FixedQp(qp) => {
            for name in [
                "hevc_minimum_qp_value",
                "hevc_maximum_qp_value",
                "hevc_i_frame_qp_value",
                "hevc_p_frame_qp_value",
                "hevc_b_frame_qp_value",
            ] {
                controls.push((name, i32::from(qp)));
            }
        }
        HevcQuality::Lossless => controls.push(("hevc_lossless_encoding", 1)),
        HevcQuality::Bitrate => unreachable!(),
    }
    controls
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_probe_requires_amly_and_stops_at_enumeration_end() {
        assert!(capture_lists_amly(|index| [NV12M, AMLY]
            .get(index as usize)
            .copied()));
        assert!(!capture_lists_amly(|index| [NV12M, P010]
            .get(index as usize)
            .copied()));
        assert!(!capture_lists_amly(|_| None));
    }
    #[test]
    fn main10_probe_rejects_substitution_short_buffers_and_profile_failure() {
        for (substitute, capacity, profile_ok, expected) in [
            (P010, 1280 * 480 * 3 / 2, true, true),
            (NV12M, 1280 * 480 * 3 / 2, true, false),
            (P010, 1280 * 480, true, false),
            (P010, 1280 * 480 * 3 / 2, false, false),
        ] {
            assert_eq!(
                probe_p010_main10(
                    |fmt| {
                        if fmt.type_ == OUTPUT {
                            fmt.fmt.pix_mp.pixelformat = substitute;
                            fmt.fmt.pix_mp.num_planes = 1;
                            unsafe {
                                fmt.fmt.pix_mp.plane_fmt[0].bytesperline = 1280;
                                fmt.fmt.pix_mp.plane_fmt[0].sizeimage = capacity;
                            }
                        }
                        true
                    },
                    |_| profile_ok,
                ),
                expected
            );
        }
        assert!(!probe_p010_main10(|_| false, |_| panic!("format rejected")));
    }
    #[test]
    fn advertised_controls_must_accept_the_entire_requested_range() {
        let mut ctrl: v4l2_queryctrl = unsafe { std::mem::zeroed() };
        ctrl.maximum = 51;
        ctrl.step = 1;
        assert!(writable_range(&ctrl, 0, 51));
        ctrl.minimum = 8;
        assert!(!writable_range(&ctrl, 0, 51));
        ctrl.minimum = 0;
        ctrl.maximum = 50;
        assert!(!writable_range(&ctrl, 0, 51));
        ctrl.maximum = 51;
        ctrl.step = 2;
        assert!(!writable_range(&ctrl, 0, 51));
        ctrl.step = 1;
        for flags in [V4L2_CTRL_FLAG_DISABLED, V4L2_CTRL_FLAG_READ_ONLY] {
            ctrl.flags = flags;
            assert!(!writable_range(&ctrl, 0, 51));
        }
    }
    #[test]
    fn visible_crop_requires_acceptance_and_padded_capture_geometry() {
        for (accept, clamp, coded_width, expected) in [
            (false, false, 1368, false), // old kernel rejects selection
            (true, false, 1368, true),
            (true, true, 1368, false),  // silently rounded visible width
            (true, false, 1366, false), // no coded padding
        ] {
            let found = probe_visible_hevc(
                |get, fmt| {
                    if get {
                        fmt.fmt.pix_mp.pixelformat = HEVC;
                        fmt.fmt.pix_mp.width = coded_width;
                        fmt.fmt.pix_mp.height = 904;
                    }
                    true
                },
                |selection| {
                    if clamp {
                        selection.r.width = 1368;
                    }
                    accept
                },
            );
            assert_eq!(found, expected);
        }
        assert!(!probe_visible_hevc(
            |_, _| false,
            |_| panic!("format failed")
        ));
    }
    #[test]
    fn missing_picture_qp_does_not_hide_independent_lossless_support() {
        let old = quality_capabilities(|id, _, _| id != V4L2_CID_MPEG_VIDEO_HEVC_P_FRAME_QP);
        assert!(!old.supports(HevcQuality::FixedQp(30)));
        assert!(old.supports(HevcQuality::Lossless));
        let no_rc = quality_capabilities(|id, _, _| id != V4L2_CID_MPEG_VIDEO_FRAME_RC_ENABLE);
        assert!(!no_rc.fixed_qp && !no_rc.lossless);
        let all = quality_capabilities(|_, _, _| true);
        for qp in [0, 1, 50, 51] {
            assert!(all.supports(HevcQuality::FixedQp(qp)));
            let controls = quality_controls(HevcQuality::FixedQp(qp));
            assert_eq!(
                controls
                    .iter()
                    .filter(|(name, _)| name.contains("qp"))
                    .count(),
                5
            );
            assert!(controls
                .iter()
                .filter(|(name, _)| name.contains("qp"))
                .all(|(_, value)| *value == i32::from(qp)));
        }
        assert!(!all.supports(HevcQuality::FixedQp(52)));
        assert!(!quality_controls(HevcQuality::Lossless)
            .iter()
            .any(|(name, _)| name.contains("qp")));
    }
}
