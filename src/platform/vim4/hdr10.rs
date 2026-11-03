//! Capture metadata comes from checksum-qualified HDMI DRM, never EDID defaults.
use super::video_backend::wave5_device;
use crate::video::codec::h265_bitstream::Hdr10Metadata;
use std::{fs::OpenOptions, os::fd::AsRawFd};
use v4l2r::bindings::*;

v4l2r::nix::ioctl_readwrite!(get_controls, b'V', 71, v4l2_ext_controls);

pub fn current() -> Option<Hdr10Metadata> {
    let device = wave5_device("t7-vdin-test").ok()?;
    let file = OpenOptions::new().read(true).open(device).ok()?;
    let mut mastering: v4l2_ctrl_hdr10_mastering_display = unsafe { std::mem::zeroed() };
    let mut cll: v4l2_ctrl_hdr10_cll_info = unsafe { std::mem::zeroed() };
    let mut controls: [v4l2_ext_control; 2] = unsafe { std::mem::zeroed() };
    controls[0].id = V4L2_CID_COLORIMETRY_HDR10_MASTERING_DISPLAY;
    controls[0].size = std::mem::size_of_val(&mastering) as u32;
    controls[0].__bindgen_anon_1.p_hdr10_mastering_display = &mut mastering;
    controls[1].id = V4L2_CID_COLORIMETRY_HDR10_CLL_INFO;
    controls[1].size = std::mem::size_of_val(&cll) as u32;
    controls[1].__bindgen_anon_1.p_hdr10_cll_info = &mut cll;
    let mut query: v4l2_ext_controls = unsafe { std::mem::zeroed() };
    query.count = controls.len() as u32;
    query.controls = controls.as_mut_ptr();
    unsafe { get_controls(file.as_raw_fd(), &mut query) }.ok()?;
    let mut values = Vec::with_capacity(24);
    for i in 0..3 {
        values.extend(mastering.display_primaries_x[i].to_be_bytes());
        values.extend(mastering.display_primaries_y[i].to_be_bytes());
    }
    values.extend(mastering.white_point_x.to_be_bytes());
    values.extend(mastering.white_point_y.to_be_bytes());
    values.extend(mastering.max_display_mastering_luminance.to_be_bytes());
    values.extend(mastering.min_display_mastering_luminance.to_be_bytes());
    let mut light = [0; 4];
    light[..2].copy_from_slice(&cll.max_content_light_level.to_be_bytes());
    light[2..].copy_from_slice(&cll.max_pic_average_light_level.to_be_bytes());
    Some(Hdr10Metadata {
        mastering: values.try_into().ok()?,
        cll: light,
    })
}
