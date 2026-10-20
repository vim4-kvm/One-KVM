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
Fresh experimental data disables audio and USB functions. EDID/HDR and USB
updates reject requests rather than invoking 5.15 board helpers. HDMI audio,
USB integration and dynamic HDMI mode recovery require separate validation.

Use the main workspace's `packaging/kernel-edge/one-kvm-edge.service` and its
prepare/launch scripts. The unit requires both the matched kernel and the
`vim4.one_kvm=1` boot argument. Its executable, database, registry and loopback
port 8082 are separate from production. The default 5.15 application and its
systemd service remain installed; returning to the default boot entry starts
that service normally. Access the test listener through an SSH tunnel.

Build and test with the existing Linux ARM64 `vim4` commands in
[VIM4.md](../../VIM4.md#构建与安装). The plane-view tests exercise allocation bounds
and buffer ownership without video hardware; successful tests do not establish
hardware DMA import, browser decoding, recovery or performance.
