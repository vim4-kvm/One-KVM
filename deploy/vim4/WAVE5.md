# Experimental 7.x video backend

The normal `vim4` build keeps the vendor backend by default. Set
`VIM4_VIDEO_BACKEND=wave5` for the matched 7.x runtime. By default, startup rejects
other kernels and the production `/var/lib/one-kvm` data tree before opening its
database. Unrecognized backend names are errors.

The common board launcher explicitly sets `VIM4_SHARED_DATA=1` for the integrated
runtime. This permits exactly `/var/lib/one-kvm`, not arbitrary children, and uses
the normal `one-kvm.service`, port 8081 and `/usr/libexec/vim4-onekvm` USB helper.
The helper applies mainline mount-namespace and mounted-media guards on 7.x while
retaining the production media directory and journal. Board-owned HDMI audio names
and NV12/NV21 are resolved in memory; the saved configuration keeps vendor-compatible
names so the same application version can use it after a kernel switch. User-selected
USB audio, accounts, images and other settings are preserved. The independent edge
service keeps its existing isolated data, helper and port 8082.

Wave5 uses standard V4L2 capture and encoding. Nodes are resolved by their sysfs
names (`t7-vdin-test` and `C&M Wave5 VPU encoder`), never by assumed video numbers.
The application still exposes its logical `/dev/video_cap` device to existing UI
and stream selection code; no device symlink or vendor ioctl wrapper is needed.

The Rust appsink/appsrc bridge splits each packed NV12 allocation into two DMA
plane views, without copying pixels. Parent-buffer metadata holds the capture
pool lease until the encoder releases both views. At 1080p, allocation capacity
must include the encoder's 1088-row alignment while UV remains after active Y.
Plane offsets and encoder alignment use the capture metadata's byte stride.
The HDMI NV12 producer pads rows to Wave5's 32-byte requirement. The capture
allocation query requests VideoMeta before the snapshot tee so padded rows
remain visible to both the encoder bridge and JPEG snapshots.
Native widths such as 1366 keep their visible width while DMA rows are padded
to 1376 bytes. Both plane views retain the same capture lease.
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

Input status follows measured progressive V4L2 timings within the matched
receiver's envelope: even 320–3840 by 180–2160 pixels, 20–120 Hz and a
24–600 MHz pixel clock. Capture caps let V4L2 report the input frame rate;
the output frame-rate setting remains an upper limit. The kernel must provide
the corresponding PHY and colour conversion; status admission alone does not
establish hardware coverage for every mode or colour depth. The qualified
UHD60 YUV42010 HDR10/SDR path remains available. H.264/H.265 WebRTC supports
bitrate changes and keyframe requests. Output height selects the VDIN hardware
scaler before DMA, preserving the full scene without a raw-frame CPU copy.
Zero means native input size; smaller outputs preserve aspect ratio with width
rounded down to a 16-byte stride and even height. Upscaling is not requested.
The physical input status and HDMI EDID remain independent of output size.
MJPEG uses the separate T7 HCODEC driver and `vim4jpegenc`, not Wave5. Missing
hardware/codec plugins are errors on this backend. The matched DTB selects the
P0 HCODEC clock path, and its OP-TEE integration requires the licensed video
firmware preload to complete before application startup. Imports are released
after every JPEG; pipeline errors close the engine before recycling input.
Exclusive board tests measured approximately 47 fps at 4K and 60 fps at
1080p/720p, with a 666 MHz JPEG clock. These are content/configuration-specific
results; hardware JPEG support does not establish 4K60 JPEG throughput.
UU Remote's existing H.264/H.265 restriction still prevents MJPEG while enabled.
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
backend retains GE2D conversion. On the experimental backend, the tap observes
the active scaled DMA image, so screenshots follow that capture resolution.
The authenticated `/api/snapshot` endpoint uses a fresh raw-tap frame on Wave5,
including while WebRTC is active; it does not depend on the MJPEG frame cache.
Fresh experimental data enables the board-owned keyboard and absolute/relative
mouse (`full_no_consumer`) and disables media, networking, UAC and serial.
The matched audio-test DTB and receiver's `audio_test=1` expose the independent
`VIM4HDMIRX` ALSA capture card. When present, fresh experimental data enables
stereo LPCM capture through `hw:VIM4HDMIRX,0` and the existing Opus pipeline.
Existing databases retain their enabled state; known board HDMI aliases resolve
to the current kernel's card without changing user-selected USB audio devices.
The audio EDID prefers RGB8 UHD60, includes 1080p60, and advertises only stereo
16-bit LPCM at 32/44.1/48 kHz; multichannel, compressed audio and USB UAC are separate.
The matched experimental USB service provisions the three HID instances before
application startup. HID configuration uses the existing restricted configfs
helper, preserving the device instances and numbering across reconfiguration.
USB media, consumer keys, NCM/ECM/RNDIS configuration and UAC2 use the separately
installed `/usr/libexec/vim4-onekvm-edge` helper and its dedicated sudoers rule.
It uses `/var/lib/one-kvm-edge/msd` and `/run/vim4-onekvm-edge-usb`; production
media and USB journals remain separate. The preparation service adds unlinked
NCM and playback-only UAC2 instances without replacing the existing HID devices.
The API reserves media capacity within the five-IN-endpoint budget and rejects
USB configuration changes while a host still owns a mounted image. The helper
checks again under the shared media locks. Service stop preserves mounted media
and its USB profile; after host SCSI eject the baseline can be restored safely.
The root helper joins PID 1's mount namespace before opening media; otherwise
systemd's private StateDirectory mount can disappear while the kernel still
owns the backing file, changing its reported path after an application restart.
This edge helper requires the board's Python 3.12 `os.setns` support.
USB serial remains unavailable in the matched kernel. Actual host acceptance
and remaining audio/host-platform limits are recorded in the main workspace's
`docs/bringup/2026-10-21-edge-usb-functions.md`.
Both codecs use GOP 0 and the standard V4L2 cyclic intra refresh period of 45
frames. At UHD, the matched Wave5 driver spreads refresh across three AVC MB
rows or one HEVC CTU row per frame. Explicit recovery and new-viewer keyframes
remain enabled. Startup checks that the driver exposes the required refresh
control before starting the encoder; an older module is an error.
Existing experimental databases keep their saved backend; enable `otg` with the
`full_no_consumer` profile explicitly when migrating a video-only installation.
USB serial remains unavailable in the matched 7.x kernel. Runtime EDID uses the
receiver's guarded V4L2 interface; the shared HDR curve uses the native driver
attribute. Other input formats still require qualification. Enabling a USB
function is not proof of host data/audio acceptance.

Use the main workspace's `packaging/kernel-edge/one-kvm-edge.service` and its
prepare/launch scripts. The unit requires both the matched kernel and the
`vim4.one_kvm=1` boot argument. Its executable, database, registry and loopback
port 8082 are separate from production. The default 5.15 application and its
systemd service remain installed; returning to the default boot entry starts
that service normally. Access the test listener through an SSH tunnel or the
separate experimental LAN login gateway on port 8080, using the existing manager
account. The backend remains on loopback; do not expose managed authentication
directly to the LAN.

Input-only `stream.config_applied` notifications do not renegotiate an already
connected peer unless a configuration change or transition is pending. Duplicate
source recovery/discovery notifications must remain harmless; explicit changes,
disconnected clients and MJPEG retain their existing recovery paths. Run the
production-handler event-order regression with `npm --prefix web test`.
Input discovery can precede a backend transition's `mode_ready`. A connected
peer waits for that transition rather than starting another switch in response
to the discovery event; the pending transition still finishes normally.

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
