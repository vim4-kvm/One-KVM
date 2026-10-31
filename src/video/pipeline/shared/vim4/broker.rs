//! Optional independent capture input. DMA memory owns the remote frame lease.
use super::*;
use gstreamer_allocators::{DmaBufAllocator, DmaBufAllocatorExtManual};
use gstreamer_app::{AppSrc, AppSrcCallbacks};
use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

pub(super) fn enabled() -> bool {
    std::env::var_os("VIM4_BROKER_SOCKET").is_some()
}

struct Connection(OwnedFd);

impl Connection {
    fn send(&self, message: &serde_json::Value) -> std::io::Result<()> {
        let data = serde_json::to_vec(message)?;
        let n = unsafe {
            libc::send(
                self.0.as_raw_fd(),
                data.as_ptr().cast(),
                data.len(),
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if n != data.len() as isize {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn receive(&self) -> std::io::Result<(serde_json::Value, Option<OwnedFd>)> {
        let mut data = [0u8; 8192];
        let mut control = [0usize; 16];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control);
        let n = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut fds = Vec::new();
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let count =
                        ((*c).cmsg_len - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<i32>();
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(
                            *(libc::CMSG_DATA(c).cast::<i32>().add(i)),
                        ));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        if msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 || fds.len() > 1 {
            return Err(std::io::Error::other(
                "truncated broker message or extra fds",
            ));
        }
        Ok((serde_json::from_slice(&data[..n as usize])?, fds.pop()))
    }
}

struct Retire {
    connection: Arc<Connection>,
    message: serde_json::Value,
}
unsafe extern "C" fn retired(data: *mut c_void, _: *mut gst::ffi::GstMiniObject) {
    let lease = unsafe { Box::from_raw(data.cast::<Retire>()) };
    // A failed ACK leaves the server's lease pinned. Never invent retirement
    // after losing the connection; the server must resolve its reader fence.
    let _ = lease.connection.send(&lease.message);
}

pub(super) fn attach(pipeline: &gst::Pipeline, width: u32, height: u32) -> Result<()> {
    let fail = |error: String| AppError::VideoError(format!("capture broker: {error}"));
    let path = std::env::var("VIM4_BROKER_SOCKET").map_err(|e| fail(e.to_string()))?;
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path.len() >= address.sun_path.len() || !path.starts_with('/') {
        return Err(fail(
            "socket path must be an absolute Unix socket path".into(),
        ));
    }
    address.sun_family = libc::AF_UNIX as _;
    for (out, byte) in address.sun_path.iter_mut().zip(path.bytes()) {
        *out = byte as _;
    }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return Err(fail(std::io::Error::last_os_error().to_string()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe {
        libc::connect(
            raw,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as _,
        )
    } != 0
    {
        return Err(fail(std::io::Error::last_os_error().to_string()));
    }
    let timeout = libc::timeval {
        tv_sec: 1,
        tv_usec: 0,
    };
    if unsafe {
        libc::setsockopt(
            raw,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&timeout as *const libc::timeval).cast(),
            std::mem::size_of_val(&timeout) as _,
        )
    } != 0
    {
        return Err(fail(std::io::Error::last_os_error().to_string()));
    }
    let connection = Arc::new(Connection(fd));
    connection
        .send(
            &serde_json::json!({"version":1,"op":"subscribe","name":"One-KVM","in_flight_limit":7}),
        )
        .map_err(|e| fail(e.to_string()))?;
    let input = pipeline
        .by_name("source")
        .and_then(|e| e.downcast::<AppSrc>().ok())
        .ok_or_else(|| fail("missing broker appsrc".into()))?;
    let origin = Mutex::new(None::<u64>);
    input.set_callbacks(AppSrcCallbacks::builder().need_data(move |source, _| {
        let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
            connection.send(&serde_json::json!({"version":1,"op":"take"}))?;
            let (message, fd) = loop {
                let (message, fd) = connection.receive()?;
                if message["version"] != 1 { return Err("unsupported broker version".into()); }
                match message["type"].as_str() {
                    Some("frame") => break (message, fd.ok_or("frame missing fd")?),
                    Some("ready" | "release-pending") if fd.is_none() => continue,
                    Some("idle") if fd.is_none() => {
                        if source.current_state() < gst::State::Paused { return Ok(()); }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        connection.send(&serde_json::json!({"version":1,"op":"take"}))?;
                    }
                    _ => return Err("invalid broker response".into()),
                }
            };
            let frame = &message["frame"];
            let number = |key: &str| frame[key].as_u64().ok_or("invalid frame field");
            let (w, h, pitch, uv) = (number("width")?, number("height")?, number("pitch")?, number("uv_offset")?);
            if message["format"] != "NV12" || message["modifier"] != 0 ||
                frame["colorspace"] != "rec709" || frame["quantization"] != "limited" ||
                w != width as u64 || h != height as u64 || pitch < w || pitch > 16384 || uv != pitch * h {
                return Err("unsupported broker NV12 layout or changed geometry".into());
            }
            let size = uv.checked_add(pitch * h.div_ceil(2)).ok_or("frame overflow")?;
            if message["allocation_size"].as_u64().ok_or("missing allocation size")? < size {
                return Err("broker allocation is smaller than NV12 planes".into());
            }
            let allocator = DmaBufAllocator::new();
            let memory = unsafe { allocator.alloc_dmabuf(fd, size as usize)? };
            let lease = Box::new(Retire { connection: connection.clone(),
                message: serde_json::json!({"version":1,"op":"release","token":message["token"],
                                          "generation":number("generation")?}) });
            unsafe { gst::ffi::gst_mini_object_weak_ref(memory.as_ptr() as *mut gst::ffi::GstMiniObject, Some(retired),
                                                       Box::into_raw(lease).cast()); }
            let mut buffer = gst::Buffer::new();
            let writable = buffer.get_mut().unwrap();
            writable.append_memory(memory);
            gstreamer_video::VideoMeta::add_full(writable,
                gstreamer_video::VideoFrameFlags::empty(), gstreamer_video::VideoFormat::Nv12,
                width, height, &[0, uv as usize], &[pitch as i32, pitch as i32])?;
            let timestamp = number("timestamp_ns")?;
            let mut first = origin.lock().unwrap();
            let first = *first.get_or_insert(timestamp);
            writable.set_pts(gst::ClockTime::from_nseconds(timestamp.saturating_sub(first)));
            source.push_buffer(buffer)?;
            Ok(())
        })();
        if let Err(error) = result {
            gst::element_error!(source, gst::ResourceError::Read, ("Broker input failed: {error}"));
            let _ = source.end_of_stream();
        }
    }).build());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_gst_memory_releases_remote_lease_only_after_last_view() {
        gst::init().unwrap();
        let mut sockets = [0; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, sockets.as_mut_ptr())
            },
            0
        );
        let connection = Arc::new(Connection(unsafe { OwnedFd::from_raw_fd(sockets[0]) }));
        let peer = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
        let file = std::fs::File::open("/dev/zero").unwrap();
        let memory = unsafe { DmaBufAllocator::new().alloc_dmabuf(file, 4096).unwrap() };
        let lease = Box::new(Retire {
            connection,
            message: serde_json::json!({"version":1,
            "op":"release","token":42,"generation":3}),
        });
        unsafe {
            gst::ffi::gst_mini_object_weak_ref(
                memory.as_ptr() as *mut gst::ffi::GstMiniObject,
                Some(retired),
                Box::into_raw(lease).cast(),
            );
        }
        let mut buffer = gst::Buffer::new();
        buffer.get_mut().unwrap().append_memory(memory);
        let view = buffer.copy();
        drop(buffer);
        let mut message = [0u8; 256];
        assert_eq!(
            unsafe {
                libc::recv(
                    peer.as_raw_fd(),
                    message.as_mut_ptr().cast(),
                    message.len(),
                    libc::MSG_DONTWAIT,
                )
            },
            -1
        );
        drop(view);
        let n = unsafe {
            libc::recv(
                peer.as_raw_fd(),
                message.as_mut_ptr().cast(),
                message.len(),
                libc::MSG_DONTWAIT,
            )
        };
        assert!(n > 0);
        let message: serde_json::Value = serde_json::from_slice(&message[..n as usize]).unwrap();
        assert_eq!(message["token"], 42);
        assert_eq!(message["generation"], 3);
    }
}
