//! Viewer diagnostics: only allowlisted numeric timing/counter fields may leave the payload.
use std::io::Read;

use base64::Engine;
use serde_json::Value;

const LIMIT: usize = 256 * 1024;
const KEYS: &[&str] = &[
    "avg_received_fps",
    "avg_decode_fps",
    "avg_render_fps",
    "avg_assembly_ms_interval",
    "avg_current_delay_ms_interval",
    "avg_decode_ms_interval",
    "avg_delay_ms_interval",
    "avg_jitter_buffer_ms_interval",
    "avg_processing_delay_interval",
    "avg_render_delay_ms_interval",
    "avg_target_delay_ms_interval",
    "received_framerate",
    "actual_received_framerate",
    "decode_framerate",
    "render_framerate",
    "actual_framerate",
    "current_delay",
    "average_delay",
    "max_delay",
    "current_delay_ms_interval",
    "target_delay_ms_interval",
    "jitter_buffer_ms_interval",
    "render_delay_ms_interval",
    "assembly_ms_interval",
    "decode_ms_interval",
    "processing_delay_interval",
    "sending_delay_interval",
    "frames_received",
    "frames_decoded",
    "frames_actual_rendered",
    "packets_received",
    "packets_lost",
    "packet_loss_rate",
    "current_packet_loss_rate",
    "frame_capture_delay_ms_interval",
    "frame_encode_delay_ms_interval",
    "frame_pacer_delay_ms_interval",
    "frame_transport_delay_ms_interval",
    "frame_decode_delay_ms_interval",
    "frame_e2e_delay_ms_interval",
];

fn unpack(text: &str) -> Option<Value> {
    if text.len() > LIMIT {
        return None;
    }
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()?;
    let mut output = Vec::new();
    if bytes.starts_with(&[31, 139]) {
        flate2::read::GzDecoder::new(&bytes[..])
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut output)
            .ok()?;
    } else if bytes.first() == Some(&0x78) {
        flate2::read::ZlibDecoder::new(&bytes[..])
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut output)
            .ok()?;
    } else {
        output = bytes;
    }
    if output.len() > LIMIT {
        return None;
    }
    serde_json::from_slice(&output).ok()
}

pub fn collect(event: &Value) -> Vec<(String, f64)> {
    fn walk(
        value: &Value,
        path: &str,
        selected: bool,
        depth: usize,
        budget: &mut usize,
        out: &mut Vec<(String, f64)>,
    ) {
        if depth > 12 || *budget == 0 || out.len() >= 128 {
            return;
        }
        *budget -= 1;
        match value {
            Value::Number(n) if selected => {
                if let Some(n) = n.as_f64().filter(|n| n.is_finite()) {
                    out.push((path.into(), n));
                }
            }
            Value::Object(map) => {
                for (key, value) in map {
                    let allowed = KEYS.contains(&key.as_str())
                        || (selected
                            && [
                                "min", "max", "avg", "mean", "sum", "count", "value", "current",
                                "total",
                            ]
                            .contains(&key.as_str()));
                    let next = if allowed {
                        format!("{path}.{key}")
                    } else {
                        path.into()
                    };
                    walk(value, &next, allowed, depth + 1, budget, out);
                }
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, path, selected, depth + 1, budget, out);
                }
            }
            Value::String(text) => {
                if selected {
                    if let Ok(n) = text.parse::<f64>() {
                        if n.is_finite() {
                            out.push((path.into(), n));
                        }
                        return;
                    }
                }
                if let Some(value) = unpack(text) {
                    walk(&value, path, selected, depth + 1, budget, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(event, "stats", false, 0, &mut 8192, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_metrics_exclude_credentials_and_unknown_fields() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(br#"{"token":"private","current_delay_ms_interval":{"avg":56,"private":123},"render_framerate":"60"}"#).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap());
        assert_eq!(
            collect(&serde_json::json!({"data":data})),
            vec![
                ("stats.current_delay_ms_interval.avg".into(), 56.0),
                ("stats.render_framerate".into(), 60.0)
            ]
        );
        assert!(unpack(&"A".repeat(LIMIT + 1)).is_none());
        assert!(collect(&serde_json::json!({"render_framerate":"NaN", "cookie":123})).is_empty());
    }
}
