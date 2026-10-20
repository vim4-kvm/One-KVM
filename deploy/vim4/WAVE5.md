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
GStreamer 1.24.2 also needs the V4L2 bytesused/offset, encoder pool reference and
EOS buffer ownership patches from the main workspace's `packaging/kernel-edge`.
Without the lifetime fixes, repeated pipelines retain encoder handles or mapped
buffers and can exhaust CMA. Use `tools/wave5-lifecycle-check.py` in that workspace
with all video applications stopped to check both codecs after rebuilding the plugin.

This stage supports fixed RGB8 1080p60/UHD60 input and H.264/H.265 WebRTC video,
bitrate changes and keyframe requests. Scaling and MJPEG are not enabled.
Fresh experimental data enables the board-owned keyboard and absolute/relative
mouse (`full_no_consumer`) and disables audio, media, networking, UAC and serial.
The matched experimental USB service provisions the three HID instances before
application startup. HID configuration uses the existing restricted configfs
helper, preserving the device instances and numbering across reconfiguration.
Unsupported USB functions are rejected before active HID handles are closed.
Existing experimental databases keep their saved backend; enable `otg` with the
`full_no_consumer` profile explicitly when migrating a video-only installation.
Consumer keys, USB media/network/UAC/serial, HDMI audio, EDID/HDR updates and
dynamic HDMI mode recovery require separate validation.

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
