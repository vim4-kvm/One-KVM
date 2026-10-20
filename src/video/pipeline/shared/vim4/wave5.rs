//! Wave5 imports two plane views of one capture allocation, retaining its lease.
use super::*;
use crate::platform::vim4::video_backend::wave5_device;
use gstreamer_allocators::{DmaBufAllocator, DmaBufAllocatorExtManual, DmaBufMemory};
use gstreamer_app::{AppSink, AppSinkCallbacks, AppSrc};
use std::os::fd::BorrowedFd;

pub(super) fn description(
    config: &SharedVideoPipelineConfig,
    source: Resolution,
    fps: u32,
) -> Result<String> {
    if config.output.height != 0 && config.output.height < source.height {
        return Err(AppError::VideoError(
            "Wave5 scaling is not yet available".into(),
        ));
    }
    let codec = match config.output_codec {
        VideoEncoderType::H264 => "h264",
        VideoEncoderType::H265 => "h265",
        _ => return Err(AppError::VideoError("Wave5 requires H.264 or H.265".into())),
    };
    let hevc = if codec == "h265" {
        "video/x-h265,profile=main,level=(string)5.1 ! "
    } else {
        ""
    };
    Ok(format!(
        "v4l2src name=source io-mode=dmabuf ! video/x-raw,format=NV12,width={},height={},framerate=60/1 ! \
         {}queue max-size-buffers=3 max-size-bytes=0 max-size-time=0 ! \
         videorate drop-only=true ! video/x-raw,framerate={fps}/1 ! \
         appsink name=plane_bridge emit-signals=false sync=false async=false max-buffers=1 \
         appsrc name=encoder_source is-live=true format=time block=true max-buffers=2 \
         caps=\"video/x-raw,format=NV12,width={},height={},framerate={fps}/1,colorimetry=bt709,interlace-mode=progressive,pixel-aspect-ratio=1/1\" ! \
         v4l2{codec}enc name=encoder output-io-mode=dmabuf-import ! {hevc}\
         {codec}parse config-interval=-1 ! video/x-{codec},stream-format=byte-stream,alignment=au ! \
         appsink name=frames sync=false max-buffers=2",
        source.width, source.height, crate::video::snapshot::vim4::BRANCH,
        source.width, source.height,
    ))
}

fn plane_view(original: &gst::Buffer, width: u32, height: u32) -> Result<gst::Buffer> {
    let fail = |message: &str| AppError::VideoError(format!("Wave5 DMA planes: {message}"));
    if width == 0
        || height == 0
        || width % 2 != 0
        || height % 2 != 0
        || width > 4096
        || height > 2160
    {
        return Err(fail("unsupported NV12 geometry"));
    }
    let luma = width as usize * height as usize;
    let aligned_luma = width as usize * ((height as usize + 15) & !15);
    if original.n_memory() != 1 || original.size() != luma * 3 / 2 {
        return Err(fail("expected one packed NV12 allocation"));
    }
    let memory = original.peek_memory(0);
    let dma = memory
        .downcast_memory_ref::<DmaBufMemory>()
        .ok_or_else(|| fail("capture did not export DMA-buf memory"))?;
    if memory.offset() != 0 {
        return Err(fail("capture memory has an unexpected base offset"));
    }
    let padded;
    let memory = if aligned_luma != luma {
        // The fd is borrowed only while the original buffer owns its memory.
        let fd = unsafe { BorrowedFd::borrow_raw(dma.fd()) }.try_clone_to_owned()?;
        let file = std::fs::File::from(fd);
        let capacity =
            usize::try_from(file.metadata()?.len()).map_err(|_| fail("allocation too large"))?;
        if capacity < luma + aligned_luma / 2 {
            return Err(fail("capture allocation lacks encoder tail padding"));
        }
        // Own a duplicated fd; mapping is never enlarged beyond its allocation.
        padded = unsafe { DmaBufAllocator::new().alloc_dmabuf(file, capacity) }
            .map_err(|e| fail(&e.to_string()))?;
        padded.as_ref()
    } else {
        memory
    };
    let mut view = gst::Buffer::new();
    let writable = view
        .get_mut()
        .ok_or_else(|| fail("new buffer is not writable"))?;
    writable.append_memory(memory.share(0..aligned_luma));
    writable.append_memory(memory.share(luma..luma + aligned_luma / 2));
    // GstMemory ownership alone does not stop the capture pool reusing a buffer.
    gst::ParentBufferMeta::add(writable, original);
    gstreamer_video::VideoMeta::add_full(
        writable,
        gstreamer_video::VideoFrameFlags::empty(),
        gstreamer_video::VideoFormat::Nv12,
        width,
        height,
        &[0, aligned_luma],
        &[width as i32, width as i32],
    )
    .map_err(|e| fail(&e.to_string()))?;
    original
        .copy_into(
            writable,
            gst::BufferCopyFlags::TIMESTAMPS | gst::BufferCopyFlags::FLAGS,
            ..,
        )
        .map_err(|e| fail(&e.to_string()))?;
    Ok(view)
}

pub(super) fn configure(
    pipeline: &gst::Pipeline,
    config: &SharedVideoPipelineConfig,
    source: Resolution,
) -> Result<()> {
    let missing = |name| AppError::VideoError(format!("Missing Wave5 element {name}"));
    let capture = pipeline
        .by_name("source")
        .ok_or_else(|| missing("source"))?;
    capture.set_property(
        "device",
        wave5_device("t7-vdin-test")?.to_string_lossy().as_ref(),
    );
    let encoder = pipeline
        .by_name("encoder")
        .ok_or_else(|| missing("encoder"))?;
    encoder.set_property(
        "device",
        wave5_device("C&M Wave5 VPU encoder")?
            .to_string_lossy()
            .as_ref(),
    );
    let mut controls = gst::Structure::builder("controls")
        .field(
            "video_bitrate",
            (config.bitrate_kbps().clamp(1000, 60000) * 1000) as i32,
        )
        .field("video_gop_size", 60i32)
        .field("frame_level_rate_control_enable", 1i32)
        .field("h264_mb_level_rate_control", 1i32)
        .build();
    if config.output_codec == VideoEncoderType::H265 {
        controls.set("hevc_level", 8i32);
    }
    encoder.set_property("extra-controls", controls);
    let input = pipeline
        .by_name("encoder_source")
        .and_then(|e| e.downcast::<AppSrc>().ok())
        .ok_or_else(|| missing("encoder_source"))?;
    let bridge = pipeline
        .by_name("plane_bridge")
        .and_then(|e| e.downcast::<AppSink>().ok())
        .ok_or_else(|| missing("plane_bridge"))?;
    let eos_input = input.clone();
    bridge.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
                let view = plane_view(&buffer, source.width, source.height).map_err(|error| {
                    tracing::error!("{error}");
                    gst::FlowError::Error
                })?;
                input.push_buffer(view)
            })
            .eos(move |_| {
                let _ = eos_input.end_of_stream();
            })
            .build(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A file-backed DMA allocator exercises ownership/layout without hardware;
    // actual exported DMA-buf import is covered by the board test.
    fn buffer(width: usize, height: usize, capacity: usize) -> gst::Buffer {
        gst::init().unwrap();
        let file = tempfile::tempfile().unwrap();
        file.set_len(capacity as u64).unwrap();
        let memory = unsafe { DmaBufAllocator::new().alloc_dmabuf(file, capacity) }.unwrap();
        let mut result = gst::Buffer::new();
        let b = result.get_mut().unwrap();
        b.append_memory(memory.share(0..width * height * 3 / 2));
        b.set_pts(gst::ClockTime::from_mseconds(42));
        result
    }

    #[test]
    fn nv12_plane_views_preserve_capture_lease_and_real_uv_address() {
        for height in [1080usize, 2160] {
            let width = 1920usize;
            let luma = width * height;
            let aligned = width * ((height + 15) & !15);
            let mut parent = buffer(width, height, luma + aligned / 2);
            assert!(parent.get_mut().is_some());
            let view = plane_view(&parent, width as u32, height as u32).unwrap();
            assert!(parent.get_mut().is_none());
            assert_eq!(view.n_memory(), 2);
            assert_eq!(view.peek_memory(0).size(), aligned);
            assert_eq!(view.peek_memory(1).offset(), luma);
            assert_eq!(view.peek_memory(1).size(), aligned / 2);
            assert_eq!(view.pts(), parent.pts());
            assert_eq!(
                view.meta::<gstreamer_video::VideoMeta>().unwrap().offset(),
                &[0, aligned]
            );
            assert_eq!(
                view.meta::<gst::ParentBufferMeta>()
                    .unwrap()
                    .parent()
                    .as_ptr(),
                parent.as_ptr()
            );
            drop(view);
            assert!(parent.get_mut().is_some());
        }
    }

    #[test]
    fn plane_views_reject_missing_padding_and_non_dma_capture() {
        let short = buffer(1920, 1080, 1920 * 1080 * 3 / 2);
        assert!(plane_view(&short, 1920, 1080).is_err());
        let ordinary = gst::Buffer::with_size(1920 * 1080 * 3 / 2).unwrap();
        assert!(plane_view(&ordinary, 1920, 1080).is_err());
        assert!(plane_view(&short, 1919, 1080).is_err());
        assert!(plane_view(&short, 1920, 2161).is_err());
    }
}
