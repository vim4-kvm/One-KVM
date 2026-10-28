//! Explicit, task-scoped batching of already encrypted video RTP datagrams.
//! ICE still selects each destination. Other tasks and non-RTP traffic use the
//! normal socket path; there is no timer, background queue or cross-frame wait.
use std::cell::RefCell;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};

use nix::libc;
use socket2::SockAddr;
use tokio::io::unix::AsyncFd;
use tokio::net::UdpSocket;

const BATCH_SIZE: usize = 32;
const MAX_DATAGRAM: usize = 2048;

tokio::task_local! {
    static PENDING: RefCell<Option<Batch>>;
}

struct Datagram {
    bytes: Vec<u8>,
    target: SockAddr,
}

struct Batch {
    original_fd: RawFd,
    // Keep the selected socket alive even if the peer closes before frame flush.
    socket: OwnedFd,
    datagrams: Vec<Datagram>,
}

/// Flush queued datagrams before returning the frame's result. A partial send
/// resumes at the first unsent packet; flush errors are returned to the caller.
/// Cancelling the future drops unsent datagrams and the duplicated descriptor.
/// Spawned tasks do not inherit this scope (audio, RTCP/NACK and ICE stay immediate).
pub async fn send_frame<F: Future>(frame: F) -> io::Result<F::Output> {
    if is_active() {
        return Ok(frame.await);
    }
    PENDING
        .scope(RefCell::new(None), async {
            let result = frame.await;
            flush_pending().await?;
            Ok(result)
        })
        .await
}

pub(super) fn is_active() -> bool {
    PENDING.try_with(|_| ()).is_ok()
}

async fn flush_pending() -> io::Result<()> {
    let batch = PENDING.with(|pending| pending.borrow_mut().take());
    if let Some(batch) = batch {
        batch.flush().await?;
    }
    Ok(())
}

pub(super) async fn send_to(
    socket: &UdpSocket,
    bytes: &[u8],
    target: SocketAddr,
) -> io::Result<usize> {
    // The RTP header remains clear after SRTP. Also allow the application's
    // negotiated HEVC payload type 49. STUN, DTLS, RTCP and TURN must not wait:
    // their callers can wait for a response before the frame future completes.
    if bytes.len() < 12
        || bytes.len() > MAX_DATAGRAM
        || bytes[0] >> 6 != 2
        || (192..=223).contains(&bytes[1])
    {
        flush_pending().await?;
        return socket.send_to(bytes, target).await;
    }
    let changed = PENDING.with(|pending| {
        pending
            .borrow()
            .as_ref()
            .is_some_and(|batch| batch.original_fd != socket.as_raw_fd())
    });
    if changed {
        flush_pending().await?;
    }
    let full = PENDING.with(|pending| -> io::Result<bool> {
        let mut pending = pending.borrow_mut();
        if pending.is_none() {
            *pending = Some(Batch {
                original_fd: socket.as_raw_fd(),
                socket: socket.as_fd().try_clone_to_owned()?,
                datagrams: Vec::with_capacity(BATCH_SIZE),
            });
        }
        let batch = pending.as_mut().unwrap();
        batch.datagrams.push(Datagram {
            bytes: bytes.to_vec(),
            target: target.into(),
        });
        Ok(batch.datagrams.len() == BATCH_SIZE)
    })?;
    if full {
        flush_pending().await?;
        // Bound uninterrupted video work even when every socket is writable.
        tokio::task::yield_now().await;
    }
    Ok(bytes.len())
}

impl Batch {
    async fn flush(self) -> io::Result<()> {
        let mut sent = 0;
        let mut readiness: Option<AsyncFd<OwnedFd>> = None;
        while sent < self.datagrams.len() {
            let result = if let Some(socket) = &readiness {
                let mut writable = socket.writable().await?;
                match writable
                    .try_io(|socket| send_batch(socket.as_raw_fd(), &self.datagrams[sent..]))
                {
                    Ok(result) => result,
                    Err(_) => continue,
                }
            } else {
                send_batch(self.socket.as_raw_fd(), &self.datagrams[sent..])
            };
            match result {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "empty UDP batch send",
                    ))
                }
                Ok(count) => sent += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    // Register only on real backpressure; the normal path needs
                    // no additional epoll registration per batch.
                    if readiness.is_none() {
                        readiness = Some(AsyncFd::new(self.socket.as_fd().try_clone_to_owned()?)?);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn send_batch(fd: RawFd, datagrams: &[Datagram]) -> io::Result<usize> {
    // These stack descriptors only borrow immutable packet buffers during the
    // syscall. No raw pointer survives an await or buffer reallocation.
    let mut vectors: [libc::iovec; BATCH_SIZE] = unsafe { std::mem::zeroed() };
    let mut messages: [libc::mmsghdr; BATCH_SIZE] = unsafe { std::mem::zeroed() };
    for (index, packet) in datagrams.iter().enumerate() {
        vectors[index] = libc::iovec {
            iov_base: packet.bytes.as_ptr() as *mut _,
            iov_len: packet.bytes.len(),
        };
        messages[index].msg_hdr.msg_name = packet.target.as_ptr() as *mut _;
        messages[index].msg_hdr.msg_namelen = packet.target.len();
        messages[index].msg_hdr.msg_iov = &mut vectors[index];
        messages[index].msg_hdr.msg_iovlen = 1;
    }
    let count = unsafe {
        libc::sendmmsg(
            fd,
            messages.as_mut_ptr(),
            datagrams.len() as _,
            libc::MSG_DONTWAIT,
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    for (message, packet) in messages.iter().zip(datagrams).take(count as usize) {
        if message.msg_len as usize != packet.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short UDP datagram send",
            ));
        }
    }
    Ok(count as usize)
}

#[cfg(test)]
mod tests;
