# Wave5 HEVC capabilities

The application probes standard V4L2 on a private encoder fd before streaming.
It never allocates buffers, queues frames or starts firmware for this probe.
The result is cached for the process lifetime; restart after loading/changing
the driver. An unavailable device or failed ioctl conservatively disables the
affected capability. The vendor backend retains its original paths and controls.

## Visible rectangles

Both b1300eb808f43 and 213f271505411 advertise an HEVC coded-size step of eight.
That enumeration alone cannot distinguish conformance-window support. The newer
kernel also changes RAW enumeration to storage alignment, but that is not used
as a proxy for HEVC support.

The probe selects HEVC CAPTURE, sets NV12M OUTPUT storage to 1376x912, then uses
VIDEO_OUTPUT/CROP selection for 1366x900. b1300eb rejects the non-eight-aligned
rectangle; 213f2715 accepts it and G_FMT(CAPTURE_MPLANE) returns 1368x904 HEVC.
Both the unchanged visible rectangle and padded coded dimensions must match.
S_FMT/S_SELECTION only update this fd's software state before buffers/STREAMON;
closing it leaves active encoder contexts untouched. No trial encode or kernel
release string is used. The old AVC fallback remains at both shared-pipeline
and WebRTC codec selection boundaries when the probe fails.

## Fixed QP and lossless

QUERYCTRL must report writable, enabled controls with the required range/step.
Fixed QP requires FRAME_RC_ENABLE, MB_RC_ENABLE and HEVC MIN/MAX/I/P/B QP covering
0–51. Both RC controls are disabled; all five QPs receive the requested value.
CONSTANT_QUALITY is intentionally unused: percentage rounding cannot represent
every QP exactly. CQ is also unnecessary when FRAME_RC_ENABLE is explicitly zero.
The new kernel exposes CQ/CONSTANT_QUALITY and P/B QPs; b1300eb has MIN/MAX/I QP
(0–63) but lacks P/B QPs and CQ/CONSTANT_QUALITY, so fixed QP is unavailable there.

Lossless independently requires both RC controls and HEVC_LOSSLESS_CU accepting
one. Both audited kernels set `skip_intra_trans=1` unconditionally in
wave5_set_enc_openparam, satisfying the firmware's transform-skip requirement
without an additional user control. MB_RC_ENABLE also disables CU RC/HVS QP.
The GStreamer field name is `hevc_lossless_encoding`, derived from QUERYCTRL's
standard name, not the C control identifier. Lossless never maps to QP zero.
This establishes a valid requested configuration, not proof of firmware fidelity;
decoded encoder-input YUV samples must still be compared on hardware.

`GET /api/vim4/video` reports `hevc_quality_available` (fixed QP) and
`hevc_lossless_available` separately. PATCH video rejects unsupported requests.
Saved preferences unsupported after a kernel change remain saved, while the
stream logs an explicit bitrate fallback. H.264 ignores the HEVC preference.

## Other applications

The audited NanoKVM `server/common/encoder_rate_vim4.go`, JetKVM
`internal/native/vim4_encoder.go`, uStreamer `src/ustreamer/vim4.c` and kvmd media
paths do not have the One-KVM non-eight-aligned AVC fallback or a fixed-QP/lossless
settings surface. Their Wave5 encoder is the shared `vim4wave5enc` plugin.
That plugin has no explicit eight-pixel visible-size rejection or quality mode;
its sixteen-pixel scaled-width rounding is a DMA stride constraint, also used
by One-KVM, and remains in place. No changes to these repositories are required.

## Pending automated board acceptance (no hardware result claimed)

Offline validation: ARM64 Ubuntu container `cargo test --locked --features vim4
--lib` passed 814 tests (four existing ignored tests); development and release
VIM4 builds passed. Frontend typecheck/build and 39 frontend tests passed;
35 deployment helper tests passed in Linux. The default non-VIM4 `cargo check
--locked` is blocked by the container's FFmpeg headers lacking the upstream
Rockchip path's `AV_HWDEVICE_TYPE_RKMPP`. No device or board was accessed.

Run with exclusive encoder ownership, one kernel at a time. Use generated raw
NV12 fixtures or existing HDMI input; no HID, host interaction or sound tests.
Bound each observation to at most 600 seconds. Preserve and restore API settings.

| Kernel | Automated operation | Pass criterion |
|---|---|---|
| b1300eb, 213f2715 | Start One-KVM; collect capability log and authenticated GET /api/vim4/video; compare V4L2 QUERYCTRL output | visible_hevc false/true respectively; fixed QP false/true; lossless only when required controls are writable; no STREAMON or firmware timeout from probing |
| 213f2715 | Script PATCH /api/config/video output height 900 and HEVC selection on existing >=900p input; repeat 720/900/native ten times; parse shared output SPS and decoded dimensions/frame counters | HEVC retained at 900; conformance window matches requested visible output; frames advance after every change; no timeout/isolation/CMA drift |
| 213f2715 | Feed generated 1366x768, 1440x900 and 1680x1050 NV12 fixtures through a standard V4L2 HEVC pipeline; parse SPS and decode | Visible dimensions and all four image edges preserved; padded coded dimensions are multiples of eight; decoded frame count equals fixture count |
| b1300eb | Same scripted non-aligned output request and session reconnection | AVC selected before encoding; decoded visible dimensions match output; no HEVC firmware stall |
| 213f2715 | Script fixed QP 0/30/51 on HEVC; capture shared stream and parse slice QP | I/P slices use requested QP, RC disabled; no bitrate clamp; switch back to bitrate successfully |
| b1300eb, 213f2715 | Generate deterministic NV12 fixture, encode with enumerated lossless/RC controls, decode to raw NV12 and compare each visible Y/U/V sample | Exact sample equality; SPS/PPS confirm lossless tools; no QP-zero substitution; unsupported fixed QP on old kernel rejected by API |
| 5.15 Vendor | Script bitrate/fixed QP/lossless requests and parse/decode generated fixtures; alternate modes ten times | Existing vendor properties and capabilities retained; exact YUV equality for lossless and advancing stream counters |
| 213f2715 | Script simultaneous authenticated WebRTC/UU test clients and SRT receiver for 120 seconds with video only | All shared receivers report same HEVC dimensions/QP and advancing frame counters; no encoder restart loop or kernel error |
