//! TERM v1 layout from uu-remote-linux / the official Mac host codec; see ../NOTICE.
pub const OPEN: u8 = 1;
pub const STDIN: u8 = 2;
pub const RESIZE: u8 = 3;
pub const CLOSE: u8 = 4;
pub const STDOUT: u8 = 5;
pub const EXIT: u8 = 6;
pub const SIGNAL: u8 = 7;
pub const ERROR: u8 = 8;
pub const LIST_REQ: u8 = 9;
pub const LIST_RESP: u8 = 10;
pub const KILL_REQ: u8 = 11;
pub const KILL_RESP: u8 = 12;
pub const OPEN_RESP: u8 = 13;
pub const UNLOCK_REQ: u8 = 14;
pub const UNLOCK_RESP: u8 = 15;
pub const DETACH: u8 = 16;
pub const OUTPUT_CHUNK: usize = 16 * 1024;
pub const SCROLLBACK: usize = 256 * 1024;

// Deliberately no Debug: accidental diagnostics must not print terminal bytes.
pub struct Frame<'a> {
    pub kind: u8,
    pub session: u32,
    pub payload: &'a [u8],
}
pub fn encode(kind: u8, session: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + payload.len());
    out.extend_from_slice(b"TERM");
    out.extend_from_slice(&[1, kind, 0, 0]);
    out.extend_from_slice(&session.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}
pub fn decode(data: &[u8]) -> Option<Frame<'_>> {
    if data.len() < 16 || &data[..4] != b"TERM" || data[4] != 1 || !(1..=16).contains(&data[5]) {
        return None;
    }
    let length = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;
    if length > 1024 * 1024 || data.len() != length + 16 {
        return None;
    }
    Some(Frame {
        kind: data[5],
        session: u32::from_le_bytes(data[8..12].try_into().ok()?),
        payload: &data[16..],
    })
}
pub fn reply(code: u32, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"code":code,"message":message})).unwrap()
}
pub fn json(payload: &[u8]) -> serde_json::Value {
    serde_json::from_slice(payload).unwrap_or_default()
}
pub fn dims(v: &serde_json::Value) -> (u16, u16) {
    let get = |key: &str, default| {
        v.get(key)
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
            .unwrap_or(default)
            .clamp(1, 1000) as u16
    };
    (get("cols", 80), get("rows", 24))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn official_layout_and_invalid_lengths() {
        let bytes = encode(OPEN_RESP, 7, &reply(0, "ok"));
        assert_eq!(&bytes[..12], b"TERM\x01\x0d\0\0\x07\0\0\0");
        let f = decode(&bytes).unwrap();
        assert_eq!(f.session, 7);
        assert_eq!(json(f.payload)["code"], 0);
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
        let mut invalid = bytes.clone();
        invalid[4] = 2;
        assert!(decode(&invalid).is_none());
        invalid = bytes;
        invalid[12..16].copy_from_slice(&1048577u32.to_le_bytes());
        assert!(decode(&invalid).is_none());
    }
}
