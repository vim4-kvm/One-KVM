# Experimental 7.x video backend

The normal `vim4` build keeps the vendor backend by default. Set
`VIM4_VIDEO_BACKEND=wave5` only for the separately installed 7.x experiment.
Startup rejects other kernels and the production `/var/lib/one-kvm` data tree
before opening its database. Unrecognized backend names are errors.

Wave5 uses standard V4L2 capture and encoding. Nodes are resolved by their sysfs
names (`t7-vdin-test` and `C&M Wave5 VPU encoder`), never by assumed video numbers.
The application still exposes its logical `/dev/video_cap` device to existing UI
and stream selection code; no device symlink or vendor ioctl wrapper is needed.

The Rust appsink/appsrc bridge splits each packed NV12 allocation into two DMA
plane views, without copying pixels. Parent-buffer metadata holds the capture
pool lease until the encoder releases both views. At 1080p, allocation capacity
must include the encoder's 1088-row alignment while UV remains after active Y.
The kernel must support nonzero DMA plane offsets and on-demand keyframes.
The matched experimental service also requires leased direct VDIN DMA. Capture
buffers remain owned until AXI completion and encoder release; a failed stop
quarantines hardware-used allocations until reboot. This removes the kernel's
private-ring frame copy while keeping the application plane-view lease.
The encoder's compressed output is copied into cached memory before NAL parsing;
bytewise parsing directly on the coherent DMA mapping otherwise consumes a large
amount of CPU. This copy covers the compressed payload only. Raw NV12 plane views
still retain their DMA allocation without copying pixels in the application.
The bridge and encoded-frame sinks disable last-sample caching so consumed
samples do not hold capture leases while input is stopped or being reconfigured.
GStreamer 1.24.2 also needs the V4L2 bytesused/offset, encoder pool reference and
EOS buffer ownership patches from the main workspace's `packaging/kernel-edge`.
Without the lifetime fixes, repeated pipelines retain encoder handles or mapped
buffers and can exhaust CMA. Use `tools/wave5-lifecycle-check.py` in that workspace
with all video applications stopped to check both codecs after rebuilding the plugin.

This stage supports RGB8 1080p60/UHD60 input and H.264/H.265 WebRTC video,
bitrate changes and keyframe requests. Scaling and MJPEG are not enabled.
Input status requires the matching receiver's `VIDIOC_QUERY_DV_TIMINGS` support:
it reports the measured frame rate instead of assuming exactly 60 Hz. With the
matching receiver's `dynamic_modes=1`, detected input dimensions are independent
of the old capture allocation. The existing source monitor rebuilds capture and
encoding while retaining subscribers; the driver applies the new S_FMT only
after old buffers are released, then validates geometry again before DMA starts.
The default driver configuration remains fixed unless that parameter is enabled.
Unsupported timings are not reported as locked; other input modes remain separate.
On-demand screenshots use the shared raw tap and standard `videoconvert` /
`videoscale` CPU conversion, followed by JPEG compression. Fit and odd-coordinate
region requests keep their existing geometry and deadline limits; screenshot
conversion does not change the live DMA capture/encode pipeline. The vendor
backend retains GE2D conversion. This does not enable scaled live video or MJPEG.
The authenticated `/api/snapshot` endpoint uses a fresh raw-tap frame on Wave5,
including while WebRTC is active; it does not depend on the MJPEG frame cache.
Fresh experimental data enables the board-owned keyboard and absolute/relative
mouse (`full_no_consumer`) and disables media, networking, UAC and serial.
The matched audio-test DTB and receiver's `audio_test=1` expose the independent
`VIM4HDMIRX` ALSA capture card. When present, fresh experimental data enables
stereo LPCM capture through `hw:VIM4HDMIRX,0` and the existing Opus pipeline.
Existing databases need their audio device and enabled state changed explicitly.
The audio EDID prefers RGB8 UHD60, includes 1080p60, and advertises only stereo
16-bit LPCM at 32/44.1/48 kHz; multichannel, compressed audio and USB UAC are separate.
The matched experimental USB service provisions the three HID instances before
application startup. HID configuration uses the existing restricted configfs
helper, preserving the device instances and numbering across reconfiguration.
Unsupported USB functions are rejected before active HID handles are closed.
Both codecs use GOP 0 and the standard V4L2 cyclic intra refresh period of 45
frames. At UHD, the matched Wave5 driver spreads refresh across three AVC MB
rows or one HEVC CTU row per frame. Explicit recovery and new-viewer keyframes
remain enabled. Startup checks that the driver exposes the required refresh
control before starting the encoder; an older module is an error.
Existing experimental databases keep their saved backend; enable `otg` with the
`full_no_consumer` profile explicitly when migrating a video-only installation.
Consumer keys, USB media/network/UAC/serial, EDID/HDR updates and input modes
outside RGB8 1080p60/UHD60 require separate validation.

Use the main workspace's `packaging/kernel-edge/one-kvm-edge.service` and its
prepare/launch scripts. The unit requires both the matched kernel and the
`vim4.one_kvm=1` boot argument. Its executable, database, registry and loopback
port 8082 are separate from production. The default 5.15 application and its
systemd service remain installed; returning to the default boot entry starts
that service normally. Access the test listener through an SSH tunnel or the
separate experimental LAN login gateway on port 8080, using the existing manager
account. The backend remains on loopback; do not expose managed authentication
directly to the LAN.

Build and test with the existing Linux ARM64 `vim4` commands in
[VIM4.md](../../VIM4.md#构建与安装). The plane-view tests exercise allocation bounds
and buffer ownership without video hardware; successful tests do not establish
hardware DMA import, browser decoding, recovery or performance.

The main workspace's `tools/onekvm-local-bench.py` runs the ignored release
`wave5::benchmark::local_buffer_paths` test, calling the same production helpers
without opening video devices. It can run in the builder or as a temporary test
executable on the board's current kernel. File-backed memory timings do not
represent the VIM4's coherent DMA mappings; see that workspace's
`docs/local-video-benchmark.md` for workload, comparison and hardware boundaries.
