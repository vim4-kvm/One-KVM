# Offline RTP sender benchmark

This standalone crate includes the production `video_track.rs`, H.264 helpers
and H.265 payloader. Only application error and resolution types are adapted.
It does not compile or open capture, encoder, HDMI, USB or application services.
It uses the workspace's patched DTLS and the production WebRTC dependency version
and `vendored-openssl` feature. The default `vim4` feature selects the production
first-packet playout-delay extension branch.
The local SRTP patch matches the application. Default `openssl-hmac` enables its
VIM4 authentication backend; disable only this feature for a controlled baseline:

```sh
cargo build --locked --release --manifest-path test/rtp-bench/Cargo.toml \
  --no-default-features --features vim4
cp test/rtp-bench/target/release/onekvm-rtp-bench /tmp/rtp-bench-upstream-hmac
cargo build --locked --release --manifest-path test/rtp-bench/Cargo.toml
cp test/rtp-bench/target/release/onekvm-rtp-bench /tmp/rtp-bench-openssl-hmac
```

Keep fixtures, profiles, rounds, iteration counts and VIM4 track behavior equal.
Reports record the selected feature and embedded SRTP source hashes; all encrypted
transcript hashes must match across backends. See [backend notes](../../libs/srtp/VIM4.md).

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

## UDP batching comparison

The default `vim4` benchmark feature includes the optional
[task-scoped UDP batching implementation](../../libs/webrtc-util/VIM4.md).
Use the same executable, fixtures and cipher profiles for both arms; the
environment switch selects only whether the production video track enters that
scope. An unset variable is the baseline (the benchmark checks presence, so do
not set it to `0`). Reports include `udp_batch_sender` and the UDP source hashes.

```sh
env -u RTP_BENCH_UDP_BATCH test/rtp-bench/target/release/onekvm-rtp-bench \
  h264.frames.bin h265.frames.bin 1 100 baseline.json peer_udp_buffered
RTP_BENCH_UDP_BATCH=1 test/rtp-bench/target/release/onekvm-rtp-bench \
  h264.frames.bin h265.frames.bin 1 100 batch.json peer_udp_buffered
env -u RTP_BENCH_UDP_BATCH test/rtp-bench/target/release/onekvm-rtp-bench \
  h264.frames.bin h265.frames.bin 1 100 baseline-repeat.json peer_udp_buffered
```

Repeat the complete A/B/A sequence under the same affinity and load. Require
successful authentication and exact packet/frame/duplicate counts before using
sender timings. A receiver whose actual socket buffer is capped by the host may
overflow during burst tests; retain that failed report, and use a separately
documented private receiver buffer for a new diagnostic. Do not change global
socket defaults to make the test pass.

The timer includes packetization, encryption, batch assembly and the final
flush. It does not start at the first `sendmmsg`, which would hide the cost of
building that batch. Loopback peers share CPU/runtime scheduling with the sender:
median gains do not excuse worse tails or establish browser latency. Measure
both ends of the real video path before changing the application default.

`RTP_BENCH_UNCONSTRAINED=1` separately removes Tokio's cooperative budget around
the timed frame future for a scheduler diagnostic. It is not a production
option. Leave it unset in UDP comparisons; reports record `unconstrained_sender`.

## Isolated OpenSSL HMAC experiment

This section preserves the original experiment against the unmodified 0.17.2
release. The production fork now uses an explicit feature; use the commands
above for current A/B tests. Do not apply this archived patch to `libs/srtp`.

`hmac-openssl-experiment.patch` is an experimental patch for a clean copy of
`webrtc-srtp` 0.17.2. It is not connected to this crate's or the application's
normal dependency graph. With the existing `openssl` feature, it replaces the
RustCrypto SHA-1 HMAC state with precomputed inner/outer OpenSSL SHA-1 states;
the algorithm, key derivation and authentication tag lengths remain unchanged.

Copy the clean package into a private directory, preserving its license files.
Never patch Cargo's shared registry cache. For example, from the One-KVM root:

```sh
mkdir -p .tmp
cp -R /path/to/clean/webrtc-srtp-0.17.2 .tmp/srtp-openssl-candidate
patch --dry-run -d .tmp/srtp-openssl-candidate -p1 < test/rtp-bench/hmac-openssl-experiment.patch
patch -d .tmp/srtp-openssl-candidate -p1 < test/rtp-bench/hmac-openssl-experiment.patch
```

Build the normal benchmark first, then save its executable under a distinct
name before any candidate build can overwrite a shared target directory:

```sh
cp test/rtp-bench/target/release/onekvm-rtp-bench .tmp/rtp-bench-baseline
mkdir -p .tmp/rtp-bench-hmac
cp test/rtp-bench/Cargo.toml .tmp/rtp-bench-hmac/Cargo.toml
cp test/rtp-bench/Cargo.lock .tmp/rtp-bench-hmac/Cargo.lock
```

In that temporary manifest, add the binary declaration below and the SRTP entry
to its existing `[patch.crates-io]` table. Keep the DTLS patch, dependency versions,
features and release profile unchanged. Using the original source path preserves
the production modules and source snapshots included by the benchmark.

```toml
[[bin]]
name = "onekvm-rtp-bench"
path = "../../test/rtp-bench/src/main.rs"

[patch.crates-io]
webrtc-srtp = { path = "../srtp-openssl-candidate" }
dtls = { path = "../../libs/dtls" }
```

The first candidate build must refresh the temporary lockfile for the path
override; preserve that lockfile and use `--locked` for subsequent builds.
The executable paths here assume no `CARGO_TARGET_DIR` override; use the actual
target directory if one is configured.

```sh
cargo build --release --manifest-path .tmp/rtp-bench-hmac/Cargo.toml
cp .tmp/rtp-bench-hmac/target/release/onekvm-rtp-bench .tmp/rtp-bench-candidate
cargo tree --locked --manifest-path .tmp/rtp-bench-hmac/Cargo.toml -e features -i webrtc-srtp
```

Confirm that feature tree activates the OpenSSL backend.
Run candidate library tests directly with
`cargo test --manifest-path .tmp/srtp-openssl-candidate/Cargo.toml --features vendored-openssl --lib`.
The isolated candidate passed all 40 library tests, including the two added
RFC 2202 and RTP/SRTCP authentication equivalence tests; the main workspace
retains the run in `logs/rtp-hmac-candidate-tests-online.log`.

Compare saved baseline, candidate and baseline again under the same host/load
conditions. Use identical input files, stages, rounds and iterations, and fresh
report paths. For `srtp_context` and `srtp_udp`, require matching
`encrypted_transcript_sha256` for every codec/profile/stage, plus successful
decryption and packet delivery validation. Record executable, actual manifest,
lockfile and patched dependency source hashes. The benchmark embeds its actual
manifest/lock hashes; it does not embed the experimental dependency's source.

This remains a performance investigation, not production adoption. The safe
`openssl::sha::Sha1` wrapper stores a low-level `SHA_CTX`; its underlying
`SHA1_Init/Update/Final` APIs are deprecated in OpenSSL 3.0. That upstream
maintenance constraint needs review before adopting or maintaining a dependency
fork, even when byte equivalence and offline measurements pass.

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
