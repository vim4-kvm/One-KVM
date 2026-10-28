# UDP frame batching experiment

This is `webrtc-util` 0.17.2 with an optional Linux `udp-batch` feature. The
application currently enables a frame scope only when `ONEKVM_UDP_BATCH=1`.
Without that scope, `UdpSocket` retains its original `send_to` behavior.

`conn::udp_batch::send_frame` collects up to 32 already encrypted RTP datagrams
from the current task and uses `sendmmsg`. It flushes the short tail before the
frame operation returns. There is no timer, background sender, or wait for a
subsequent frame. ICE still selects each packet's destination; RTP, SRTP, the
interceptors and retransmission cache retain their existing implementation.
The application's session receiver awaits each complete frame submission before
starting the next one. Nested scopes share the outer frame's pending batch.

The scope is not inherited by spawned tasks. Audio, feedback, and ICE tasks
therefore remain immediate. STUN, DTLS, RTCP and TURN encapsulations also bypass
batching within a scope, flushing earlier RTP first. The RTP filter accepts the
application's negotiated HEVC payload type 49 as well as dynamic payload types.
This experiment does not optimize relayed TURN traffic.

A duplicated descriptor keeps each selected UDP socket alive until its batch
finishes. Partial sends resume from the first unsent packet; errors propagate,
and cancellation discards unsent packets. Full batches yield to other tasks.
Backpressure registers writable readiness only after an actual `EAGAIN`.

Run the behavioral tests on Linux:

```sh
cargo test --manifest-path libs/webrtc-util/Cargo.toml --features udp-batch --lib
```

They cover frame tails, ordering, socket/destination changes, control traffic,
HEVC type 49, independent feedback, cancellation, socket lifetime, partial
failure, and IPv6. These are transport tests, not a browser latency acceptance.
Use the [RTP benchmark](../../test/rtp-bench/README.md) for controlled replay;
retain both median and tail results, including failed delivery or regressions.
