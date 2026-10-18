# VIM4 snapshot service validation

`AppState.snapshot` implements the `SnapshotService` contract in the workspace's
`logs/onekvm-mcp/design.md` §3. The public frame, target, error variants and method
signatures are unchanged. MCP and `/api/snapshot` are not modified here.

The raw tee is immediately after the existing `vfmcapsrc`, before queues and
`videorate`. Its valve is closed when idle; the branch queue and appsink each
have capacity one, drop excess frames, and never cache a last sample. A request
opens the valve and accepts only a sample whose original running-time PTS is
strictly later than the arm time. Missing, equal and older timestamps are rejected.
`vfmcapsrc` stamps dequeue time, not HDMI exposure time; this proves a new capture
delivery, not the moment the source computer rendered the pixels.

Fit uses a separate **conversion-only** appsrc pipeline: GE2D first, then RGB.
For odd output sizes GE2D rounds up to even chroma dimensions and software scaling
finishes the exact requested size. Region obtains the source at the current video
output dimensions, converts to RGB before cropping (including odd coordinates),
then scales. If video output limits are active, Region first applies those limits
with GE2D; “source” means the video output coordinate space specified in the design.
TurboJPEG uses 4:4:4 for text clarity. Capture DMA ownership and the conversion
pipeline are released **before** JPEG compression and its one quality-halving retry.
JPEG size must be strictly below 1 MiB. Neither hardware JPEG nor H.26x encoding
is opened by the screenshot conversion. There is no second capture source,
codec switch, keyframe request, or VPU mutex manipulation.

The service admits one operation. Caller cancellation closes the tap. Native work
retains the slot until teardown, so timeout cannot create an unbounded worker pool.
Pipeline startup finishes registration even if the caller disappears. Short
capture leases preserve MJPEG's 5-second idle grace and use the shared H.26x
pipeline's 3-second idle grace; active viewers/recording keep their pipeline.
Runtime shutdown cancels waiting screenshots before stopping streaming.

Zero timeout selects the exported `DEFAULT_TIMEOUT` (3 seconds); a nonzero timeout
is one end-to-end budget. Quality must be 1–100, dimensions nonzero, region in bounds,
`max_long_edge` at most 4096, and Region `max_upscale` finite and in (0, 4]. These
validation limits include all design callers (1280, 4). Non-VIM4 builds expose the
same service interface but report no source / unsupported raw capture.

This branch has **not been run on a board**. Bounded queues and no IDR avoid known
causes of stream stalls, but do not prove that DMA/GE2D/CPU contention has no effect
on live streams. That is a hardware acceptance condition, not a measured result.

## Builder checks

Use the ARM64 Ubuntu builder from VIM4.md, from the repository root:

```sh
npm --prefix web ci --ignore-scripts
npm --prefix web run build
sh deploy/vim4/build-dma-allocator.sh
cargo test --locked --features vim4 --lib
cargo build --features vim4
cargo build --locked --features vim4 --example snapshot-bench
```

Tests include geometry and invalid regions, stale/missing timestamps, actual raw
tap delivery, cancellation and pipeline destruction, timeout, single admission,
real RGB-to-JPEG decoding, exact odd region dimensions, and both JPEG size retry
outcomes. GE2D, HDMI signal detection and DMA ownership require the board checks.

## Exclusive board procedure

1. Coordinate exclusive board access. Record commit, input resolution/rate, output
   limits, codec, bitrate, motion workload and active viewers/recording. Use a
   physical HDMI motion counter, not a virtual desktop or a static screen.
2. Build the debug `snapshot-bench` example and deploy it with the ordinary app's
   runtime libraries. Stop the ordinary One-KVM service before starting the example:
   this is a replacement application process and must never run alongside another
   capture owner. Run as `khadas` with the same environment/permissions, especially
   the private DMA allocator's `LD_LIBRARY_PATH`. Do not remove `/tmp/vpu_mutex.map`.
3. Run, for example:

   ```sh
   ./snapshot-bench /var/lib/one-kvm 0.0.0.0:8080 fit 100 > fit.jsonl
   ./snapshot-bench /var/lib/one-kvm 0.0.0.0:8080 region 100 > region.jsonl
   ```

   Run these sequentially. The tool runs the normal authenticated web router and
   runtime, waits 30 seconds for viewer setup, then captures every two seconds.
   It installs no measurement API. Region covers the central half of the source.
   JSON lines contain timings, dimensions, sizes and errors, never image bytes.
   The last line reports successful-sample P50/P95 and success count. Keep failures;
   a low P95 with failed requests is not a pass. Release builds refuse to run it.
4. Repeat at 1920×1080 and 3840×2160, in MJPEG, H.264 and H.265. For H.26x cover
   WebRTC alone, UU alone, both viewers plus device recording, and no viewers.
   Exercise output limits (1080p/720p), portrait/4096-wide sources when available,
   and small regions that reach the 4× limit through the service/MCP after merging.
5. Collect `pidstat -p PID 1` (or equivalent `/proc` CPU samples) for the complete
   30-second baseline and capture window. Keep CPU and RSS per second, not only an
   average. Do not attach strace to the streaming process.
6. Keep the WebRTC page visible with `/?log=debug`; save its VideoCadence windows,
   decoded fps and presentation gap tails. Save UU sender/receiver timing windows
   and fps, with sample ages. Compare baseline, capture and recovery windows, and
   confirm recording continues without gaps. Reject regressions in fps or tail
   latency even when snapshot latency is under three seconds.
7. Verify fresh motion-counter content, unplug/replug HDMI, cancel pending captures,
   race two captures (one Busy), change output settings during a capture, and stop
   the app during acquisition/conversion. After each batch, check fd/RSS stability.
   With no viewers, verify capture releases after the idle grace and subsequent
   screenshots work. Repeated Fit/Region must not exhaust DMA buffers or log EBADF.
8. Stop the example and restore the normal service. Store all results with the
   workspace bringup record; do not describe offline tests as hardware acceptance.
