# Offline RTP sender benchmark

This standalone crate includes the production `video_track.rs`, H.264 helpers
and H.265 payloader. Only application error and resolution types are adapted.
It does not compile or open capture, encoder, HDMI, USB or application services.
It uses the workspace's patched DTLS and the production WebRTC dependency version
and `vendored-openssl` feature. The default `vim4` feature selects the production
first-packet playout-delay extension branch.

Build from the One-KVM checkout so its ancestor `.cargo/config.toml` supplies
the ARM AES/PMULL settings; do not override `RUSTFLAGS`:

```sh
cargo build --locked --release --manifest-path test/rtp-bench/Cargo.toml
cargo tree --locked --manifest-path test/rtp-bench/Cargo.toml -e features -i webrtc-srtp
cargo test --locked --release --manifest-path test/rtp-bench/Cargo.toml
```

Supply diagnostic captures consisting of repeated little-endian
`u64 GstBuffer PTS, u64 AU length, AU bytes`. Each input must contain one codec.
The saved `logs/down-h264.frames.bin` and `logs/down-h265.frames.bin` in the main
workspace are suitable; the captures are private diagnostic artifacts and are
not checked into this repository. Reads and fixture selection occur before
timing. Four real AUs per codec are selected near 38, 48, 180 and 220 RTP packets;
the report records their actual counts, indices, PTS, sizes and SHA-256 hashes.

```sh
test/rtp-bench/target/release/onekvm-rtp-bench \
  ../logs/down-h264.frames.bin ../logs/down-h265.frames.bin \
  3 100 ../logs/rtp-replay-new.json \
  packetize,track_mock,interceptors_mock,srtp_context,srtp_udp,peer_udp_buffered
```

The arguments are H.264 input, H.265 input, rounds, iterations per case and a
new report path. An optional final comma-separated argument chooses stages:

| Stage | Timed work |
|---|---|
| `packetize` | Production bitstream normalization and payloader |
| `track_mock` | Production track and actual negotiated binding → counting interceptor sink |
| `interceptors_mock` | The above plus default WebRTC interceptors → counting sink |
| `srtp_context` | Production SRTP Context over prepared RTP packets, retaining ciphertext |
| `srtp_udp` | Same Context → real loopback UDP socket |
| `peer_udp` | Production track → default interceptors → negotiated DTLS/SRTP → ICE → loopback UDP → remote RTP track |
| `peer_udp_buffered` | Same sender; benchmark receiver owns a 4 MiB UDP socket through ICE UDPMux |

AES-CM/HMAC-SHA1-80 and AEAD AES-128-GCM are measured separately. The paired
peers each permit only the reported profile, so the test cannot silently fall
back to a different cipher. No TWCC sender is added: the application uses the
default interceptor registry's TWCC receiver.

`srtp_context` and `srtp_udp` decrypt every generated ciphertext outside the timed
section and compare the entire RTP plaintext, including sequence rollover.
They also hash the encrypted transcript outside timing: each packet is prefixed
with its little-endian `u64` length. Fixed keys, salt, SSRC, sequence numbers and
timestamps make transcripts comparable between dependency implementations when
fixtures, rounds and iterations match; warmup packets are included.
The raw UDP receiver requests a private 4 MiB socket buffer and reports the
actual size; this does not alter host defaults or production sockets.
Paired peers use their default socket behavior and count every received RTP
packet, marker and duplicate. They first confirm a complete setup frame has
arrived and report setup sent/received counts separately; connection establishment
alone is insufficient to declare the media reader ready. Linux network namespace
UDP counters before/after each paired-peer stage expose kernel socket drops.
These counters cover the namespace, so concurrent network workloads invalidate
attribution. Missing delivery retains sender samples and receiver statistics,
but marks the stage and whole report invalid for performance comparisons.
`peer_udp_buffered` changes only the benchmark receiver, reports the actual
socket buffer and closes its private mux on completion. It is a separate
scenario, not a production sender change or proof of Chrome's socket settings.
Omitting the stage argument runs both peer scenarios, including the default
socket diagnostic. In the isolated ARM Linux VM pilot, default peers lost
13/34 H.264 and 5/8 H.265 packets for AES-CM/GCM respectively; each missing
count exactly matched the Linux `RcvbufErrors` increment. All frame markers
arrived. The four buffered-peer scenarios received every packet with no
duplicates or kernel drops (actual receive buffer 8 MiB). These results identify
that pilot's receiver socket overflow, not packet loss in a production browser.
The failed default results remain in `logs/rtp-bench-peer-pilot5.json` in the main
workspace; they must not be used as successful performance baselines.

Each case excludes ten local warmup iterations. Raw per-frame times and round
numbers are retained; packet decryption, receiver draining and ≥16.67ms between
paired-peer frame submissions are outside the sender timing. This is a software
CPU replay, not HDMI-to-browser latency. Fixed-AU cases deliberately isolate
packet-count scaling; they are not an average-bitrate model of a live source.
Mock sinks add counting, locking and duplicate-detection overhead. Do not simply
subtract their medians from another stage and call that an exact component cost.

The output is checkpointed after each stage; failures remain in the report,
`valid=false`, and the process exits unsuccessfully after attempting other
stages. An interrupted report remains `complete=false`. Use a fresh filename
for each run. Full peer stages fail on missing or duplicate packets; failed
startup/throughput pilots must be kept rather than silently rerun to a pass.
Report actual sample counts: short offline cases do not establish production
P99, playout smoothness, optical latency or hardware encoder performance.

The executable embeds source snapshots and reports their SHA-256 plus its own
Linux executable SHA-256, allowing the same artifact to be copied to a CPU-only
replay host without a source checkout. Runtime Git/compiler metadata may be
unavailable there. Record host CPU/frequency/load separately for comparisons.
Run performance cases alone under the workspace heavy lock. A board replay
needs coordination even though it does not open codec devices: it consumes CPU.

## Optional Chrome interoperability check

File signaling accepts an ICE-complete `{type,sdp}` offer from an isolated test
Chrome and writes an ICE-complete answer to a new file:

```sh
test/rtp-bench/target/release/onekvm-rtp-bench --chrome \
  offer.json answer.json h264 ../logs/down-h264.frames.bin \
  65 auto default sender-report.json
```

Arguments after `--chrome` are offer, new answer, codec, capture, duration in
seconds, answering DTLS role (`auto`/`server`), profiles (`default`/`gcm`/`fallback`)
and new report. `fallback` offers GCM then AES-CM; this does not guarantee which
profile the client's preference chooses. Inspect Chrome's actual `srtpCipher`.
The sender loops the first 600 consecutive AUs starting with the saved IDR and
parameter sets at 60Hz. It never repeats an isolated P frame as a decoder test.
Chrome mode allows loopback/private IPv4 candidates; the paired-peer benchmark
uses loopback exclusively. There are no STUN/TURN servers or application login.
For a container, use a host-routable private container IP; container loopback is
not the Mac host's loopback. Decoder/render verification belongs to the receiver.
