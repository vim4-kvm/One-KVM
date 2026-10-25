//! Protobuf wire helpers and CONTROL payloads used by the official UU Remote
//! viewers. Field numbers follow the official hosts; no message shapes are
//! invented and unverified feature versions are never declared.

#[derive(Debug, PartialEq, Eq)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

#[derive(Debug, PartialEq, Eq)]
pub struct WireField<'a> {
    pub number: u32,
    pub value: WireValue<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    VarintOverflow,
    InvalidField,
    UnsupportedWireType(u8),
}

fn read_varint(input: &[u8], offset: &mut usize) -> Result<u64, WireError> {
    let mut result = 0;
    for shift in (0..70).step_by(7) {
        let byte = *input.get(*offset).ok_or(WireError::Truncated)?;
        *offset += 1;
        if shift == 63 && byte > 1 {
            return Err(WireError::VarintOverflow);
        }
        result |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(result);
        }
    }
    Err(WireError::VarintOverflow)
}

fn take<'a>(input: &'a [u8], offset: &mut usize, length: usize) -> Result<&'a [u8], WireError> {
    let end = offset.checked_add(length).ok_or(WireError::Truncated)?;
    let value = input.get(*offset..end).ok_or(WireError::Truncated)?;
    *offset = end;
    Ok(value)
}

/// Decode one protobuf message, keeping nested payloads opaque. Groups are rejected.
pub fn decode(input: &[u8]) -> Result<Vec<WireField<'_>>, WireError> {
    let mut offset = 0;
    let mut fields = Vec::new();
    while offset < input.len() {
        let key = read_varint(input, &mut offset)?;
        let number = key >> 3;
        if number == 0 || number > 0x1fff_ffff {
            return Err(WireError::InvalidField);
        }
        let value = match (key & 7) as u8 {
            0 => WireValue::Varint(read_varint(input, &mut offset)?),
            1 => WireValue::Fixed64(take(input, &mut offset, 8)?.try_into().unwrap()),
            2 => {
                let length = usize::try_from(read_varint(input, &mut offset)?)
                    .map_err(|_| WireError::Truncated)?;
                WireValue::Bytes(take(input, &mut offset, length)?)
            }
            5 => WireValue::Fixed32(take(input, &mut offset, 4)?.try_into().unwrap()),
            wire => return Err(WireError::UnsupportedWireType(wire)),
        };
        fields.push(WireField {
            number: number as u32,
            value,
        });
    }
    Ok(fields)
}

pub fn number(fields: &[WireField], n: u32) -> Option<u64> {
    fields.iter().find_map(|f| match (f.number, &f.value) {
        (x, WireValue::Varint(v)) if x == n => Some(*v),
        _ => None,
    })
}

pub fn message<'a>(fields: &[WireField<'a>], n: u32) -> Option<&'a [u8]> {
    fields.iter().find_map(|f| match (f.number, &f.value) {
        (x, WireValue::Bytes(b)) if x == n => Some(*b),
        _ => None,
    })
}

pub fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

pub fn put_varint(field: u32, value: u64, out: &mut Vec<u8>) {
    varint(u64::from(field) << 3, out);
    varint(value, out);
}

pub fn put_bytes(field: u32, value: &[u8], out: &mut Vec<u8>) {
    varint((u64::from(field) << 3) | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

/// proto3 scalar: zero is the default and is omitted. int32 negatives use 10-byte varints.
fn put_int(field: u32, value: i32, out: &mut Vec<u8>) {
    if value != 0 {
        put_varint(field, value as i64 as u64, out);
    }
}

fn put_text(field: u32, value: &str, out: &mut Vec<u8>) {
    if !value.is_empty() {
        put_bytes(field, value.as_bytes(), out);
    }
}

/// Message { 1 seq, 2 timestamp, <which> payload }.
pub fn envelope(seq: u64, timestamp: u64, which: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if seq != 0 {
        put_varint(1, seq, &mut out);
    }
    if timestamp != 0 {
        put_varint(2, timestamp, &mut out);
    }
    put_bytes(which, payload, &mut out);
    out
}

pub mod which {
    pub const SIMPLE_ACTION: u32 = 3;
    pub const SCREEN_SOURCES: u32 = 7;
    pub const CAPTURE_CHANGE: u32 = 8;
    pub const SYSTEM_METRICS: u32 = 13;
    pub const SYSTEM_STATE_CHANGE: u32 = 15;
    pub const QUERY_SYSTEM_STATE: u32 = 16;
    pub const CODEC_NEGOTIATION: u32 = 18;
    pub const INPUT: u32 = 20;
    pub const RPC_REQUEST: u32 = 21;
    pub const RPC_RESPONSE: u32 = 22;
    pub const CAPTURE_SETTING_SYNC: u32 = 25;
}

/// Official SimpleAction ActionType values.
pub mod action {
    pub const ECHO_REQUEST: u64 = 0;
    pub const SHOW_DESKTOP: u64 = 2;
    pub const LOCK_SYSTEM: u64 = 10;
    pub const SHOW_TASKMGR: u64 = 13;
}

/// FeatureFlag.ff_capture_setting = CUSTOM_BITRATE: fps, preset quality and custom bitrate RPCs.
pub const FF_CAPTURE_SETTING: u64 = 4;

/// SimpleAction echo_response (action 1) with args `{"seq":<request seq>}` and this host's
/// FeatureFlag (field 4). The viewer only uses features declared here, so only the capture
/// setting RPCs and Linux board-file transfer are declared; clipboard/mute/auto-lock are absent.
pub fn echo_response(seq: u64, timestamp: u64, request_seq: u64) -> Vec<u8> {
    let mut flags = Vec::new();
    put_varint(1, FF_CAPTURE_SETTING, &mut flags);
    #[cfg(target_os = "linux")]
    put_varint(6, 2, &mut flags); // FileTransferFtp LIST_COMPRESS
    let mut action = Vec::new();
    put_varint(1, 1, &mut action);
    put_bytes(
        2,
        format!(r#"{{"seq":{request_seq}}}"#).as_bytes(),
        &mut action,
    );
    put_bytes(4, &flags, &mut action);
    envelope(seq, timestamp, which::SIMPLE_ACTION, &action)
}

/// The reference host has no terminal-specific FeatureFlag. Data-only peers must
/// not advertise desktop settings. Linux tools share the board-file responder.
pub fn terminal_echo_response(seq: u64, timestamp: u64, request_seq: u64) -> Vec<u8> {
    let mut action = Vec::new();
    put_varint(1, 1, &mut action);
    put_bytes(
        2,
        format!(r#"{{"seq":{request_seq}}}"#).as_bytes(),
        &mut action,
    );
    let mut flags = Vec::new();
    #[cfg(target_os = "linux")]
    put_varint(6, 2, &mut flags);
    put_bytes(4, &flags, &mut action);
    envelope(seq, timestamp, which::SIMPLE_ACTION, &action)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub width: i32,
    pub height: i32,
}

/// Message 18 CodecNegotiation from the official Windows host descriptor:
/// encoder_cap_list { capture_mode, fps, codec_type, width, height }.
/// Only advertise the selected codec: the shared encoder stays locked to it
/// for this session, even if the viewer's offer contained additional codecs.
pub fn codec_negotiation(
    seq: u64,
    timestamp: u64,
    size: Rect,
    fps: u32,
    codec: crate::video::codec::VideoCodecType,
) -> Vec<u8> {
    let mut cap = Vec::new();
    put_varint(1, 1, &mut cap); // DESKTOP capture mode
    put_varint(2, u64::from(fps), &mut cap);
    put_varint(
        3,
        if codec == crate::video::codec::VideoCodecType::H265 {
            2
        } else {
            1
        },
        &mut cap,
    );
    put_int(4, size.width, &mut cap);
    put_int(5, size.height, &mut cap);
    let mut payload = Vec::new();
    put_bytes(1, &cap, &mut payload);
    envelope(seq, timestamp, which::CODEC_NEGOTIATION, &payload)
}

impl Rect {
    fn encode(self) -> Vec<u8> {
        // WinRect { 1 left, 2 top, 3 width, 4 height, 5 pixel_width, 6 pixel_height }
        let mut out = Vec::new();
        for (field, value) in [
            (3, self.width),
            (4, self.height),
            (5, self.width),
            (6, self.height),
        ] {
            put_int(field, value, &mut out);
        }
        out
    }
}

/// Message 7 ScreenSources with the single HDMI source (id 0, video track 0).
pub fn screen_sources(
    seq: u64,
    timestamp: u64,
    size: Rect,
    fps: i32,
    name: &str,
) -> Result<Vec<u8>, String> {
    screen_sources_with_output(seq, timestamp, size, size, fps, name)
}

pub fn screen_sources_with_output(
    seq: u64,
    timestamp: u64,
    native: Rect,
    size: Rect,
    fps: i32,
    name: &str,
) -> Result<Vec<u8>, String> {
    if size.width <= 0 || size.height <= 0 || fps <= 0 {
        return Err("Screen geometry and fps must be positive".into());
    }
    let rect = size.encode();
    let mut screen = Vec::new();
    put_int(2, fps, &mut screen);
    put_bytes(3, &native.encode(), &mut screen);
    for sixths in [5, 4, 3, 2] {
        let scaled = Rect {
            width: native.width * sixths / 6 & !1,
            height: native.height * sixths / 6 & !1,
        };
        if scaled.width >= 640 && scaled.height >= 360 {
            put_bytes(3, &scaled.encode(), &mut screen);
        }
    }
    put_bytes(4, &rect, &mut screen); // current_resolution
    put_bytes(6, &native.encode(), &mut screen); // init_resolution
    put_int(7, 1, &mut screen); // primary
    screen.push(65); // field 8 dpr: fixed64 double
    screen.extend_from_slice(&1.0f64.to_le_bytes());
    put_text(10, name, &mut screen);
    let mut payload = Vec::new();
    put_bytes(1, &screen, &mut payload);
    Ok(envelope(seq, timestamp, which::SCREEN_SOURCES, &payload))
}

/// Message 8 CaptureChange { 1 type = desktop (0), 2 type_value, 3 description }.
pub fn capture_change(seq: u64, timestamp: u64, description: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    put_text(3, description, &mut payload);
    envelope(seq, timestamp, which::CAPTURE_CHANGE, &payload)
}

/// Message 13 SystemMetrics { 3 general { 2 system_sound_support: 2 supported },
/// 4 setting { 8 capture, 9 operation, 10 control permission: 2 allow } }.
pub fn system_metrics(seq: u64, timestamp: u64, sound: bool) -> Vec<u8> {
    let mut general = Vec::new();
    if sound {
        put_varint(2, 2, &mut general);
    }
    let mut setting = Vec::new();
    for field in [8, 9, 10] {
        put_varint(field, 2, &mut setting);
    }
    let mut payload = Vec::new();
    put_bytes(3, &general, &mut payload);
    put_bytes(4, &setting, &mut payload);
    envelope(seq, timestamp, which::SYSTEM_METRICS, &payload)
}

/// Message 15 SystemStateChange { 1 SecureDesktopState { 1 is_locked, 5 is_support_auto_logon } }.
/// The KVM cannot observe the target's lock screen, so it reports unlocked without auto logon.
pub fn secure_desktop_unlocked(seq: u64, timestamp: u64) -> Vec<u8> {
    let mut change = Vec::new();
    put_bytes(1, &[], &mut change);
    envelope(seq, timestamp, which::SYSTEM_STATE_CHANGE, &change)
}

/// Official GvPb_VideoQuality values.
pub mod quality {
    pub const FAST: u64 = 1;
    pub const GENERAL: u64 = 2;
    pub const HD: u64 = 3;
    pub const BLURAY: u64 = 4;
    pub const AUTO: u64 = 5;
    pub const CUSTOM: u64 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureRequest {
    pub quality: Option<u64>,
    /// Viewer max_custom_bitrate in bits/s, normalized when decoding the RPC.
    pub custom_bitrate: Option<u64>,
    pub fps: Option<u64>,
    /// None leaves the size unchanged; Some(None) restores native HDMI size.
    pub output: Option<Option<(u64, u64)>>,
}

impl CaptureRequest {
    /// Encoder bitrate requested by the viewer menu (高清 2M, 超清 8M, 原画 20M); None keeps
    /// the current One-KVM setting (自动, or custom without a value).
    pub fn bitrate_kbps(&self) -> Option<u32> {
        let kbps = match self.quality? {
            quality::FAST => 1_000,
            quality::GENERAL => 2_000,
            quality::HD => 8_000,
            quality::BLURAY => 20_000,
            quality::CUSTOM => self.custom_bitrate.filter(|v| *v > 0)? / 1_000,
            _ => return None,
        };
        Some(kbps.min(u64::from(u32::MAX)) as u32)
    }
}

/// Current viewers use bit/s. Legacy values below 1000 were Mbit/s;
/// 1000..200000 were kbit/s (current viewers do not offer rates below 200 kbit/s).
fn custom_bitrate_bits(value: u64) -> u64 {
    match value {
        0..1_000 => value * 1_000_000,
        1_000..200_000 => value * 1_000,
        _ => value,
    }
}

/// RpcRequest { 1 header, 2 CaptureSettingRequest | 3 FpsSettingRequest |
/// 4 VideoQualitySettingRequest | 6 ResolutionSettingRequest }. Returns the request and the
/// RpcResponse { 1 header, 2 CaptureSettingResponse{} } payload, or None for other RPCs.
/// CaptureSettingRequest: 1 fps option index, 2 quality, 8 custom bitrate, 18 fpsCount.
pub fn capture_setting_rpc(rpc: &[WireField]) -> Option<(CaptureRequest, Vec<u8>)> {
    let mut request = CaptureRequest::default();
    if let Some(body) = message(rpc, 2) {
        let fields = decode(body).ok()?;
        request.quality = number(&fields, 2).filter(|v| *v > 0);
        request.custom_bitrate = number(&fields, 8)
            .filter(|v| *v > 0)
            .map(custom_bitrate_bits);
        request.fps = number(&fields, 18)
            .filter(|v| *v > 0)
            .or_else(|| number(&fields, 1).and_then(fps_from_option));
        request.output = if number(&fields, 10) == Some(3) {
            Some(None)
        } else {
            number(&fields, 16)
                .zip(number(&fields, 17))
                .or_else(|| number(&fields, 5).zip(number(&fields, 6)))
                .map(|size| {
                    if size.0 == 0 || size.1 == 0 {
                        None
                    } else {
                        Some(size)
                    }
                })
        };
    } else if let Some(body) = message(rpc, 4) {
        request.quality = number(&decode(body).ok()?, 1).filter(|v| *v > 0);
    } else if let Some(body) = message(rpc, 3) {
        request.fps = number(&decode(body).ok()?, 1).and_then(fps_from_option);
    } else if let Some(body) = message(rpc, 6) {
        let fields = decode(body).ok()?;
        let size = (
            number(&fields, 1).unwrap_or(0),
            number(&fields, 2).unwrap_or(0),
        );
        request.output = Some(if size.0 == 0 || size.1 == 0 {
            None
        } else {
            Some(size)
        });
    } else {
        return None;
    }
    Some((request, rpc_response(rpc, 2)))
}

fn fps_from_option(value: u64) -> Option<u64> {
    match value {
        0 => None,
        1 => Some(30),
        2 => Some(60),
        3 => Some(90),
        4 => Some(144),
        fps => Some(fps),
    }
}

/// RpcResponse { 1 header (same request id), <field> {} }: success with default error code.
pub fn rpc_response(rpc: &[WireField], field: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(1, message(rpc, 1).unwrap_or(&[]), &mut out);
    put_bytes(field, &[], &mut out);
    out
}

/// Session RPCs answered with success: 15 send_video_track -> 11 (the single track 0 is always
/// sent), 21 virtual_audio_driver_policy -> 17 (no virtual driver is needed).
pub fn session_rpc(rpc: &[WireField]) -> Option<Vec<u8>> {
    [(15, 11), (21, 17)]
        .into_iter()
        .find(|(request, _)| message(rpc, *request).is_some())
        .map(|(_, response)| rpc_response(rpc, response))
}

/// Plugin RPCs 16–20 (queue reminder tools): none exist here. Query (16) answers disabled
/// (response 12); enable/install/notification answer error_code 1 (responses 13–16).
pub fn plugin_rpc(rpc: &[WireField]) -> Option<Vec<u8>> {
    let (request, response_field, id_field) = [
        (16, 12, 3),
        (17, 13, 3),
        (18, 14, 2),
        (19, 15, 2),
        (20, 16, 2),
    ]
    .into_iter()
    .find_map(|(req, resp, id)| message(rpc, req).map(|b| (b, resp, id)))?;
    let plugin_id = message(&decode(request).ok()?, 1)
        .unwrap_or_default()
        .to_vec();
    let mut payload = Vec::new();
    if response_field != 12 {
        put_varint(1, 1, &mut payload);
    }
    put_bytes(id_field, &plugin_id, &mut payload);
    let mut out = Vec::new();
    put_bytes(1, message(rpc, 1).unwrap_or(&[]), &mut out);
    put_bytes(response_field, &payload, &mut out);
    Some(out)
}

/// Message 25 CaptureSettingSync: what the host encodes now.
pub fn capture_sync(fps: u32, quality: u64, size: Rect) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(1, u64::from(fps), &mut out);
    put_varint(2, quality, &mut out);
    put_varint(3, 1, &mut out); // ChromaFormat_420
    for field in [5, 9] {
        put_varint(field, size.width as u64, &mut out);
        put_varint(field + 1, size.height as u64, &mut out);
    }
    out
}

pub fn capture_sync_with_bitrate(fps: u32, quality: u64, size: Rect, kbps: u32) -> Vec<u8> {
    let mut out = capture_sync(fps, quality, size);
    if quality == quality::CUSTOM {
        put_varint(4, u64::from(kbps) * 1_000, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(request_id: u64, field: u32, body: &[u8]) -> Vec<u8> {
        let mut header = Vec::new();
        put_varint(1, request_id, &mut header);
        let mut out = Vec::new();
        put_bytes(1, &header, &mut out);
        put_bytes(field, body, &mut out);
        out
    }

    #[test]
    fn decoder_preserves_nested_fields_and_rejects_bad_input() {
        let bytes = [8, 150, 1, 18, 2, 8, 3, 29, 1, 2, 3, 4];
        assert_eq!(
            decode(&bytes),
            Ok(vec![
                WireField {
                    number: 1,
                    value: WireValue::Varint(150)
                },
                WireField {
                    number: 2,
                    value: WireValue::Bytes(&[8, 3])
                },
                WireField {
                    number: 3,
                    value: WireValue::Fixed32([1, 2, 3, 4])
                },
            ])
        );
        assert_eq!(decode(&[18, 4, 1]), Err(WireError::Truncated));
        assert_eq!(decode(&[0]), Err(WireError::InvalidField));
        assert_eq!(decode(&[11]), Err(WireError::UnsupportedWireType(3)));
        let mut overflow = vec![8];
        overflow.extend_from_slice(&[255; 9]);
        overflow.push(2);
        assert_eq!(decode(&overflow), Err(WireError::VarintOverflow));
    }

    #[test]
    fn echo_response_correlates_request_and_declares_supported_features() {
        let reply = echo_response(1, 2, 200);
        assert_eq!(&reply[..4], &[8, 1, 16, 2]);
        let outer = decode(&reply).unwrap();
        let action = decode(message(&outer, 3).unwrap()).unwrap();
        assert_eq!(number(&action, 1), Some(1));
        assert_eq!(message(&action, 2), Some(&br#"{"seq":200}"#[..]));
        let flags = decode(message(&action, 4).unwrap()).unwrap();
        assert_eq!(flags.len(), if cfg!(target_os = "linux") { 2 } else { 1 });
        assert_eq!(
            number(&flags, 6),
            if cfg!(target_os = "linux") {
                Some(2)
            } else {
                None
            }
        );
        assert_eq!(number(&flags, 1), Some(FF_CAPTURE_SETTING));
    }

    #[test]
    fn screen_sources_reports_measured_size_and_fixed64_dpr() {
        assert!(screen_sources(
            1,
            1,
            Rect {
                width: 0,
                height: 1080
            },
            60,
            ""
        )
        .is_err());
        let msg = screen_sources(
            7,
            9,
            Rect {
                width: 1920,
                height: 1080,
            },
            60,
            "HDMI",
        )
        .unwrap();
        let inventory = decode(message(&decode(&msg).unwrap(), 7).unwrap()).unwrap();
        assert_eq!(number(&inventory, 2), None); // current id 0 is the default
        let screen = decode(message(&inventory, 1).unwrap()).unwrap();
        assert_eq!(number(&screen, 1), None);
        assert_eq!(number(&screen, 2), Some(60));
        assert_eq!(number(&screen, 12), None); // video track 0
        let rect = decode(message(&screen, 4).unwrap()).unwrap();
        assert_eq!(
            (number(&rect, 3), number(&rect, 4), number(&rect, 6)),
            (Some(1920), Some(1080), Some(1080))
        );
        assert!(screen
            .iter()
            .any(|f| f.number == 8 && f.value == WireValue::Fixed64(1.0f64.to_le_bytes())));
        assert_eq!(message(&screen, 10), Some(&b"HDMI"[..]));
    }

    #[test]
    fn metrics_allow_control_and_report_sound_enum() {
        let msg = system_metrics(1, 2, true);
        let metrics = decode(message(&decode(&msg).unwrap(), 13).unwrap()).unwrap();
        assert_eq!(
            number(&decode(message(&metrics, 3).unwrap()).unwrap(), 2),
            Some(2)
        );
        let setting = decode(message(&metrics, 4).unwrap()).unwrap();
        assert_eq!(
            setting.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![8, 9, 10]
        );
        assert!(setting.iter().all(|f| f.value == WireValue::Varint(2)));
        let silent = system_metrics(1, 2, false);
        let silent = decode(message(&decode(&silent).unwrap(), 13).unwrap()).unwrap();
        assert_eq!(message(&silent, 3), Some(&[][..]));
    }

    #[test]
    fn capture_setting_rpc_maps_quality_and_echoes_request_id() {
        let mut body = Vec::new();
        put_varint(1, 2, &mut body); // fps option index
        put_varint(2, quality::GENERAL, &mut body);
        put_varint(18, 60, &mut body);
        let request = rpc(77, 2, &body);
        let (setting, response) = capture_setting_rpc(&decode(&request).unwrap()).unwrap();
        assert_eq!(setting.bitrate_kbps(), Some(2_000));
        assert_eq!(setting.fps, Some(60));
        let fields = decode(&response).unwrap();
        assert_eq!(message(&fields, 1), Some(&[8u8, 77][..]));
        assert_eq!(message(&fields, 2), Some(&[][..]));

        let (bluray, _) =
            capture_setting_rpc(&decode(&rpc(2, 4, &[8, quality::BLURAY as u8])).unwrap()).unwrap();
        assert_eq!(bluray.bitrate_kbps(), Some(20_000));
        let (fps_only, _) = capture_setting_rpc(&decode(&rpc(3, 3, &[8, 1])).unwrap()).unwrap();
        assert_eq!(fps_only.bitrate_kbps(), None);
        assert_eq!(fps_only.fps, Some(30));
        assert!(capture_setting_rpc(&decode(&rpc(4, 9, &[])).unwrap()).is_none());
    }

    #[test]
    fn capture_request_prefers_pixel_size_and_real_fps_and_can_restore_native() {
        let mut body = Vec::new();
        for (field, value) in [
            (1, 2),
            (18, 24),
            (5, 1280),
            (6, 720),
            (16, 1920),
            (17, 1080),
        ] {
            put_varint(field, value, &mut body);
        }
        let parse = |body: &[u8]| {
            capture_setting_rpc(&decode(&rpc(5, 2, body)).unwrap())
                .unwrap()
                .0
        };
        let request = parse(&body);
        assert_eq!(request.fps, Some(24));
        assert_eq!(request.output, Some(Some((1920, 1080))));
        put_varint(10, 3, &mut body);
        assert_eq!(parse(&body).output, Some(None));
        let request = capture_setting_rpc(&decode(&rpc(6, 6, &[8, 0, 16, 0])).unwrap())
            .unwrap()
            .0;
        assert_eq!(request.output, Some(None));
    }

    #[test]
    fn custom_bitrate_request_and_sync_use_bits_per_second() {
        for (wire_value, kbps) in [
            (1_000_000, 1_000),
            (12_500_000, 12_500),
            (60_000_000, 60_000),
            (12, 12_000),
            (15_000, 15_000),
        ] {
            let mut body = Vec::new();
            put_varint(2, quality::CUSTOM, &mut body);
            put_varint(8, wire_value, &mut body);
            let mut rpc_body = Vec::new();
            put_bytes(2, &body, &mut rpc_body);
            let (request, _) = capture_setting_rpc(&decode(&rpc_body).unwrap()).unwrap();
            assert_eq!(request.bitrate_kbps(), Some(kbps));
            let sync = capture_sync_with_bitrate(
                30,
                quality::CUSTOM,
                Rect {
                    width: 1920,
                    height: 1080,
                },
                kbps,
            );
            assert_eq!(
                number(&decode(&sync).unwrap(), 4),
                Some(u64::from(kbps) * 1_000)
            );
        }
    }

    #[test]
    fn custom_bitrate_clamps_before_narrowing_and_auto_keeps_current() {
        let custom = |v| CaptureRequest {
            quality: Some(quality::CUSTOM),
            custom_bitrate: v,
            ..Default::default()
        };
        assert_eq!(custom(Some(u64::MAX)).bitrate_kbps(), Some(u32::MAX));
        assert_eq!(custom(None).bitrate_kbps(), None);
        assert_eq!(
            CaptureRequest {
                quality: Some(quality::AUTO),
                custom_bitrate: None,
                ..Default::default()
            }
            .bitrate_kbps(),
            None
        );
    }

    #[test]
    fn session_and_plugin_rpcs_keep_header_and_ids() {
        let response = session_rpc(&decode(&rpc(5, 15, &[8, 0])).unwrap()).unwrap();
        let fields = decode(&response).unwrap();
        assert_eq!(message(&fields, 1), Some(&[8u8, 5][..]));
        assert_eq!(message(&fields, 11), Some(&[][..]));
        // RpcRequest { 1 header{1:9}, 16 query{1:"wow"} }
        let request = [10, 2, 8, 9, 130, 1, 5, 10, 3, b'w', b'o', b'w'];
        let fields = decode(&request).unwrap();
        let response = plugin_rpc(&fields).unwrap();
        let out = decode(&response).unwrap();
        assert_eq!(out[0].value, WireValue::Bytes(&[8, 9]));
        assert_eq!(out[1].number, 12);
        assert_eq!(out[1].value, WireValue::Bytes(&[26, 3, b'w', b'o', b'w']));
        assert!(plugin_rpc(&decode(&[10, 2, 8, 9]).unwrap()).is_none());
    }

    #[test]
    fn capture_sync_reports_fps_quality_and_size() {
        let sync = capture_sync(
            30,
            quality::AUTO,
            Rect {
                width: 3840,
                height: 2160,
            },
        );
        let fields = decode(&sync).unwrap();
        assert_eq!(number(&fields, 1), Some(30));
        assert_eq!(number(&fields, 2), Some(quality::AUTO));
        assert_eq!(
            (number(&fields, 5), number(&fields, 10)),
            (Some(3840), Some(2160))
        );
    }

    #[test]
    fn secure_state_is_explicitly_present_and_unlocked() {
        let state = secure_desktop_unlocked(1, 2);
        let change = decode(message(&decode(&state).unwrap(), 15).unwrap()).unwrap();
        assert_eq!(message(&change, 1), Some(&[][..]));
    }
}
