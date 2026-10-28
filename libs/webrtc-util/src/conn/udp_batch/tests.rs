use super::*;
use crate::Conn;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

fn packet(sequence: u16) -> Vec<u8> {
    let mut bytes = vec![0x5a; 1200];
    bytes[0] = 0x80;
    bytes[1] = 96;
    bytes[2..4].copy_from_slice(&sequence.to_be_bytes());
    bytes
}

async fn socket() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").await.unwrap()
}

async fn receive(socket: &UdpSocket, expected: &[u8]) -> SocketAddr {
    let mut data = [0; 4096];
    let (length, source) = timeout(Duration::from_secs(1), socket.recv_from(&mut data))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&data[..length], expected);
    source
}

fn empty(socket: &UdpSocket) {
    assert_eq!(
        socket.try_recv(&mut [0; 4096]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn flushes_full_batches_and_short_frame_tails_in_order() {
    let tx = socket().await;
    let rx = socket().await;
    let target = rx.local_addr().unwrap();
    for count in [1, 31, 32, 33, 95] {
        send_frame(async {
            for i in 0..count {
                Conn::send_to(&tx, &packet(i), target).await.unwrap();
                if (i + 1) % 32 == 0 {
                    for n in i - 31..=i {
                        assert_eq!(receive(&rx, &packet(n)).await, tx.local_addr().unwrap());
                    }
                }
            }
            empty(&rx);
        })
        .await
        .unwrap();
        for i in (count / 32) * 32..count {
            receive(&rx, &packet(i)).await;
        }
        empty(&rx);
    }
}

#[tokio::test]
async fn destinations_and_socket_changes_keep_the_selected_source() {
    let first = socket().await;
    let second = socket().await;
    let rx1 = socket().await;
    let rx2 = socket().await;
    send_frame(async {
        Conn::send_to(&first, &packet(1), rx1.local_addr().unwrap())
            .await
            .unwrap();
        Conn::send_to(&first, &packet(2), rx2.local_addr().unwrap())
            .await
            .unwrap();
        Conn::send_to(&second, &packet(3), rx1.local_addr().unwrap())
            .await
            .unwrap();
        assert_eq!(receive(&rx1, &packet(1)).await, first.local_addr().unwrap());
        assert_eq!(receive(&rx2, &packet(2)).await, first.local_addr().unwrap());
    })
    .await
    .unwrap();
    assert_eq!(
        receive(&rx1, &packet(3)).await,
        second.local_addr().unwrap()
    );
}

#[tokio::test]
async fn control_traffic_flushes_prior_packets_without_waiting_for_frame_end() {
    let tx = socket().await;
    let rx = socket().await;
    for header in [[0x00, 0x01], [0x16, 0xfe], [0x80, 200], [0x40, 0]] {
        let mut control = vec![0; 28];
        control[..2].copy_from_slice(&header);
        send_frame(async {
            Conn::send_to(&tx, &packet(1), rx.local_addr().unwrap())
                .await
                .unwrap();
            Conn::send_to(&tx, &control, rx.local_addr().unwrap())
                .await
                .unwrap();
            receive(&rx, &packet(1)).await;
            receive(&rx, &control).await;
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn negotiated_hevc_type_49_and_marker_are_batched() {
    let tx = socket().await;
    let rx = socket().await;
    let mut hevc = packet(7);
    hevc[1] = 49 | 128;
    send_frame(async {
        Conn::send_to(&tx, &hevc, rx.local_addr().unwrap())
            .await
            .unwrap();
        empty(&rx);
    })
    .await
    .unwrap();
    receive(&rx, &hevc).await;
}

#[tokio::test]
async fn spawned_feedback_is_immediate_and_does_not_inherit_the_frame() {
    let tx = Arc::new(socket().await);
    let rx = socket().await;
    let target = rx.local_addr().unwrap();
    send_frame(async {
        Conn::send_to(tx.as_ref(), &packet(1), target)
            .await
            .unwrap();
        let feedback = tx.clone();
        tokio::spawn(async move {
            Conn::send_to(feedback.as_ref(), &packet(9), target)
                .await
                .unwrap();
        })
        .await
        .unwrap();
        receive(&rx, &packet(9)).await;
        empty(&rx);
    })
    .await
    .unwrap();
    receive(&rx, &packet(1)).await;
}

#[tokio::test]
async fn cancellation_drops_pending_packets_without_a_later_flush() {
    let tx = Arc::new(socket().await);
    let rx = socket().await;
    let target = rx.local_addr().unwrap();
    let (queued, ready) = tokio::sync::oneshot::channel();
    let sender = tx.clone();
    let task = tokio::spawn(send_frame(async move {
        Conn::send_to(sender.as_ref(), &packet(1), target)
            .await
            .unwrap();
        queued.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    ready.await.unwrap();
    empty(&rx);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    Conn::send_to(tx.as_ref(), &packet(2), target)
        .await
        .unwrap();
    receive(&rx, &packet(2)).await;
    empty(&rx);
}

#[tokio::test]
async fn flush_retains_socket_and_preserves_the_frame_result() {
    let tx = socket().await;
    let rx = socket().await;
    let source = tx.local_addr().unwrap();
    let result = send_frame(async {
        Conn::send_to(&tx, &packet(1), rx.local_addr().unwrap())
            .await
            .unwrap();
        drop(tx);
        Err::<(), _>("encoder stopped")
    })
    .await
    .unwrap();
    assert_eq!(result, Err("encoder stopped"));
    assert_eq!(receive(&rx, &packet(1)).await, source);
}

#[tokio::test]
async fn partial_send_reports_error_without_resending_successful_datagrams() {
    let tx = socket().await;
    let rx = socket().await;
    let result = send_frame(async {
        Conn::send_to(&tx, &packet(1), rx.local_addr().unwrap())
            .await
            .unwrap();
        Conn::send_to(&tx, &packet(2), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
    })
    .await;
    assert!(result.is_err());
    receive(&rx, &packet(1)).await;
    empty(&rx);
}

#[tokio::test]
async fn ipv6_preserves_datagram_boundaries() {
    let tx = UdpSocket::bind("[::1]:0").await.unwrap();
    let rx = UdpSocket::bind("[::1]:0").await.unwrap();
    send_frame(async {
        Conn::send_to(&tx, &packet(1), rx.local_addr().unwrap())
            .await
            .unwrap();
        Conn::send_to(&tx, &packet(2), rx.local_addr().unwrap())
            .await
            .unwrap();
    })
    .await
    .unwrap();
    receive(&rx, &packet(1)).await;
    receive(&rx, &packet(2)).await;
}
