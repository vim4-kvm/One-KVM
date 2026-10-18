use crate::{
    hid::{control::InputLease, types::KeyboardReport},
    state::AppState,
    video::snapshot::{SnapshotError, SnapshotTarget},
};
use base64::Engine;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

pub const NAMES: &[&str] = &[
    "screenshot",
    "zoom",
    "cursor_position",
    "mouse_move",
    "left_click",
    "right_click",
    "middle_click",
    "double_click",
    "triple_click",
    "left_click_drag",
    "left_mouse_down",
    "left_mouse_up",
    "scroll",
    "type",
    "key",
    "hold_key",
    "wait",
];
pub fn read_only(name: &str) -> bool {
    matches!(name, "screenshot" | "zoom" | "cursor_position" | "wait")
}
pub use super::definitions::list;

fn validate(name: &str, args: &Value) -> Result<(), String> {
    let schema = list()
        .into_iter()
        .find(|v| v["name"] == name)
        .ok_or("Unknown tool.")?;
    let object = args
        .as_object()
        .ok_or("Tool arguments must be an object.")?;
    let properties = schema["inputSchema"]["properties"].as_object().unwrap();
    for key in schema["inputSchema"]["required"].as_array().unwrap() {
        if !object.contains_key(key.as_str().unwrap()) {
            return Err(format!("Missing argument {}.", key));
        }
    }
    for (key, value) in object {
        let p = properties
            .get(key)
            .ok_or_else(|| format!("Unknown argument {key}."))?;
        let valid = match p["type"].as_str().unwrap() {
            "string" => value.as_str().is_some_and(|s| {
                p["maxLength"]
                    .as_u64()
                    .is_none_or(|max| s.chars().count() <= max as usize)
            }),
            "integer" => value.as_i64().is_some(),
            "number" => value.as_f64().is_some_and(f64::is_finite),
            "array" => value.as_array().is_some_and(|a| {
                a.len() == p["minItems"].as_u64().unwrap() as usize
                    && a.iter().all(|v| v.as_i64().is_some())
            }),
            _ => false,
        };
        if !valid
            || p["minimum"]
                .as_f64()
                .is_some_and(|min| value.as_f64().unwrap_or(f64::NEG_INFINITY) < min)
            || p["maximum"]
                .as_f64()
                .is_some_and(|max| value.as_f64().unwrap_or(f64::INFINITY) > max)
            || p["enum"]
                .as_array()
                .is_some_and(|options| !options.contains(value))
        {
            return Err(format!("Invalid argument {key}."));
        }
    }
    Ok(())
}
pub fn display_size(w: u32, h: u32) -> (u32, u32) {
    let scale = (1280.0 / w.max(h) as f64).min(1.0);
    (
        (w as f64 * scale).round().max(1.0) as u32,
        (h as f64 * scale).round().max(1.0) as u32,
    )
}
fn point(value: &Value, size: (u32, u32)) -> Result<(u16, u16), String> {
    let x = value[0].as_i64().ok_or("Invalid coordinate.")?;
    let y = value[1].as_i64().ok_or("Invalid coordinate.")?;
    if x < 0 || y < 0 || x >= size.0 as i64 || y >= size.1 as i64 {
        return Err(format!(
            "coordinate ({x}, {y}) is outside the {}x{} screen",
            size.0, size.1
        ));
    }
    Ok((
        ((x as f64 / size.0.saturating_sub(1).max(1) as f64) * 32767.0).round() as u16,
        ((y as f64 / size.1.saturating_sub(1).max(1) as f64) * 32767.0).round() as u16,
    ))
}
fn text_result(text: impl Into<String>) -> Value {
    json!({"content":[{"type":"text","text":text.into()}]})
}
pub async fn call(state: Arc<AppState>, name: &str, args: Value) -> Value {
    let started = tokio::time::Instant::now();
    let token = state.mcp.operation_token();
    let validation = validate(name, &args);
    let mut detail = validation
        .as_ref()
        .ok()
        .and_then(|()| argument_detail(name, &args));
    let call_id = state.mcp.begin(name, detail.clone());
    let result = match validation {
        Ok(()) => {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(crate::hid::control::DISABLED.into()),
                result = execute(&state, name, &args, &mut detail) => result,
            }
        }
        Err(e) => Err(e),
    };
    state.mcp.record(
        call_id,
        name,
        !read_only(name),
        started,
        result.as_ref().err().cloned(),
        detail,
    );
    match result {
        Ok(value) => value,
        Err(error) => json!({"isError":true,"content":[{"type":"text","text":error}]}),
    }
}
/// Arguments have passed schema validation. Typed text is represented only by its length.
fn argument_detail(name: &str, args: &Value) -> Option<String> {
    match name {
        "type" => Some(format!("{} chars", args["text"].as_str()?.chars().count())),
        "key" | "hold_key" => super::keys::normalized_chord(args["text"].as_str()?).ok(),
        "zoom" => Some(args["region"].to_string()),
        "scroll" => Some(format!(
            "{} {}",
            args["scroll_direction"].as_str()?,
            args["scroll_amount"]
        )),
        "left_click_drag" => Some(format!(
            "({}, {}) → ({}, {})",
            args["start_coordinate"][0],
            args["start_coordinate"][1],
            args["coordinate"][0],
            args["coordinate"][1]
        )),
        _ => args
            .get("coordinate")
            .map(|c| format!("({}, {})", c[0], c[1])),
    }
}

async fn execute(
    state: &Arc<AppState>,
    name: &str,
    args: &Value,
    detail: &mut Option<String>,
) -> Result<Value, String> {
    if name == "wait" {
        tokio::time::sleep(Duration::from_secs_f64(args["duration"].as_f64().unwrap())).await;
        return Ok(text_result("OK"));
    }
    if matches!(name, "screenshot" | "zoom") {
        return screenshot(state, name, args, detail).await;
    }
    let source = state.snapshot.source_size().await;
    let size = source.map(|(w, h)| display_size(w, h));
    if name == "cursor_position" {
        let (x, y) = state
            .hid
            .input_control
            .cursor()
            .ok_or("cursor position is unknown")?;
        let (w, h) = size.ok_or("screen size is unknown: no video signal")?;
        return Ok(text_result(format!(
            "X={}, Y={}",
            (x as f64 / 32767.0 * w.saturating_sub(1) as f64).round() as u32,
            (y as f64 / 32767.0 * h.saturating_sub(1) as f64).round() as u32
        )));
    }
    let coordinate = args
        .get("coordinate")
        .map(|v| point(v, size.ok_or("screen size is unknown: no video signal")?))
        .transpose()?;
    let start = args
        .get("start_coordinate")
        .map(|v| point(v, size.ok_or("screen size is unknown: no video signal")?))
        .transpose()?;
    // Reject the whole action before writing even its optional initial move.
    if let Some(text) = args["text"].as_str() {
        match name {
            "key" | "hold_key" => {
                super::keys::chord(text)?;
            }
            "type" => {}
            _ => {
                super::keys::modifiers(text)?;
            }
        }
    }
    let cfg = state.config.get();
    let keyboard = matches!(name, "type" | "key" | "hold_key")
        || args.get("text").is_some()
        || matches!(args["scroll_direction"].as_str(), Some("left" | "right"));
    let (keyboard_enabled, absolute_enabled) = super::input_capabilities(&cfg.hid);
    if keyboard && !keyboard_enabled {
        return Err("USB keyboard is not enabled in One-KVM USB settings.".into());
    }
    if !matches!(name, "type" | "key" | "hold_key") && !absolute_enabled {
        return Err("Absolute mouse is not enabled in One-KVM USB settings.".into());
    }
    let lease = state.hid.input_control.acquire().await?;
    let position = state
        .hid
        .input_control
        .cursor()
        .map(|(x, y)| (x as u16, y as u16))
        .or(start)
        .or(coordinate);
    if position.is_none() && !matches!(name, "type" | "key" | "hold_key") {
        return Err("cursor position is unknown: supply a coordinate first.".into());
    }
    let mut input = Input {
        state,
        lease: &lease,
        position: position.unwrap_or((0, 0)),
        buttons: state.hid.input_control.mcp_buttons(),
    };
    let result = if name == "type" {
        tokio::time::timeout(
            Duration::from_secs(30),
            input.run(name, args, coordinate, start),
        )
        .await
        .map_err(|_| "Typing timed out after 30 seconds.".to_owned())
        .and_then(|r| r)
    } else {
        input.run(name, args, coordinate, start).await
    };
    if result.is_ok() {
        lease.finish();
    }
    result
}
pub(super) fn scroll_wheel(direction: &str, host_natural_scrolling: bool) -> i8 {
    let wheel = if matches!(direction, "up" | "left") {
        1
    } else {
        -1
    };
    if host_natural_scrolling {
        -wheel
    } else {
        wheel
    }
}

struct Input<'a> {
    state: &'a Arc<AppState>,
    lease: &'a InputLease,
    position: (u16, u16),
    buttons: u8,
}
impl Input<'_> {
    async fn keyboard(&self, report: KeyboardReport) -> Result<(), String> {
        self.state
            .hid
            .mcp_keyboard(self.lease.generation, report)
            .await
    }
    async fn mouse(&self, wheel: i8) -> Result<(), String> {
        self.state
            .hid
            .mcp_mouse(
                self.lease.generation,
                self.buttons,
                self.position.0,
                self.position.1,
                wheel,
            )
            .await
    }
    async fn delay(&self, millis: u64) -> Result<(), String> {
        self.lease.sleep(Duration::from_millis(millis)).await
    }
    async fn run(
        &mut self,
        name: &str,
        args: &Value,
        coordinate: Option<(u16, u16)>,
        start: Option<(u16, u16)>,
    ) -> Result<Value, String> {
        if let Some(position) = start.or(coordinate) {
            self.position = position;
            self.mouse(0).await?;
        }
        if matches!(name, "key" | "hold_key") {
            let report = super::keys::chord(args["text"].as_str().unwrap())?;
            let repeat = args["repeat"].as_u64().unwrap_or(1);
            for n in 0..repeat {
                self.keyboard(report.clone()).await?;
                self.lease
                    .sleep(Duration::from_secs_f64(
                        args["duration"].as_f64().unwrap_or(0.05),
                    ))
                    .await?;
                self.keyboard(Default::default()).await?;
                if n + 1 < repeat {
                    self.delay(50).await?;
                }
            }
            return Ok(text_result("OK"));
        }
        if name == "type" {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            let mut skipped = String::new();
            let mut typed = 0;
            for c in args["text"].as_str().unwrap().chars() {
                let Some((key, modifiers)) = super::keys::character(c) else {
                    skipped.push(c);
                    continue;
                };
                if tokio::time::Instant::now() >= deadline {
                    return Err("Typing timed out after 30 seconds.".into());
                }
                let mut report = KeyboardReport {
                    modifiers,
                    ..Default::default()
                };
                report.keys[0] = key;
                self.keyboard(report).await?;
                self.keyboard(Default::default()).await?;
                typed += 1;
                self.delay(30).await?;
            }
            return Ok(text_result(if skipped.is_empty() {
                "OK".into()
            } else {
                format!(
                    "Typed {typed} characters. Skipped {} unsupported: {skipped:?}",
                    skipped.chars().count()
                )
            }));
        }
        let mut modifiers = args["text"]
            .as_str()
            .map(super::keys::modifiers)
            .transpose()?
            .unwrap_or_default();
        if matches!(args["scroll_direction"].as_str(), Some("left" | "right")) {
            modifiers.modifiers |= 2;
        }
        let has_modifiers = modifiers.modifiers != 0;
        if has_modifiers {
            self.keyboard(modifiers).await?;
        }
        match name {
            "mouse_move" => {}
            "left_mouse_down" => {
                self.buttons = 1;
                self.mouse(0).await?;
            }
            "left_mouse_up" => {
                self.buttons = 0;
                self.mouse(0).await?;
            }
            "left_click_drag" => {
                let from = self.position;
                let to = coordinate.unwrap();
                self.buttons = 1;
                self.mouse(0).await?;
                for step in 1..=12 {
                    self.delay(20).await?;
                    self.position = (
                        (from.0 as f64 + (to.0 as f64 - from.0 as f64) * step as f64 / 12.0).round()
                            as u16,
                        (from.1 as f64 + (to.1 as f64 - from.1 as f64) * step as f64 / 12.0).round()
                            as u16,
                    );
                    self.mouse(0).await?;
                }
                self.delay(50).await?;
                self.buttons = 0;
                self.mouse(0).await?;
            }
            "scroll" => {
                let direction = args["scroll_direction"].as_str().unwrap();
                let wheel = scroll_wheel(
                    direction,
                    self.state.config.get().mcp.host_natural_scrolling,
                );
                let amount = args["scroll_amount"].as_u64().unwrap();
                for n in 0..amount {
                    self.mouse(wheel).await?;
                    if n + 1 < amount {
                        self.delay(15).await?;
                    }
                }
            }
            _ => {
                let count = match name {
                    "double_click" => 2,
                    "triple_click" => 3,
                    _ => 1,
                };
                let button = match name {
                    "right_click" => 2,
                    "middle_click" => 4,
                    _ => 1,
                };
                for n in 0..count {
                    self.buttons = button;
                    self.mouse(0).await?;
                    self.delay(50).await?;
                    self.buttons = 0;
                    self.mouse(0).await?;
                    if n + 1 < count {
                        self.delay(80).await?;
                    }
                }
            }
        }
        if has_modifiers {
            self.keyboard(Default::default()).await?;
        }
        Ok(text_result("OK"))
    }
}
async fn screenshot(
    state: &AppState,
    name: &str,
    args: &Value,
    detail: &mut Option<String>,
) -> Result<Value, String> {
    let target = if name == "zoom" {
        let (sw, sh) = state
            .snapshot
            .source_size()
            .await
            .ok_or("screen size is unknown: no video signal")?;
        let (w, h) = display_size(sw, sh);
        let r = &args["region"];
        let values: Vec<i64> = r
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        let (x0, y0, x1, y1) = (values[0], values[1], values[2], values[3]);
        if x0 < 0 || y0 < 0 || x0 >= x1 || y0 >= y1 || x1 > w as i64 || y1 > h as i64 {
            return Err("Invalid zoom region: use an ordered rectangle inside the screen.".into());
        }
        let x = (x0 as u64 * sw as u64 / w as u64) as u32;
        let y = (y0 as u64 * sh as u64 / h as u64) as u32;
        SnapshotTarget::Region {
            x,
            y,
            w: (x1 as u64 * sw as u64 / w as u64) as u32 - x,
            h: (y1 as u64 * sh as u64 / h as u64) as u32 - y,
            max_long_edge: 1280,
            max_upscale: 4.0,
        }
    } else {
        SnapshotTarget::Fit {
            max_long_edge: 1280,
        }
    };
    let frame = state
        .snapshot
        .capture(target, 80, Duration::from_secs(3))
        .await
        .map_err(|e| match e {
            SnapshotError::NoSignal => "No video signal.".into(),
            SnapshotError::Timeout => "Screenshot timed out.".into(),
            SnapshotError::Busy => "Another screenshot is running. Retry after it finishes.".into(),
            SnapshotError::Unavailable(reason) => format!("Screenshot unavailable: {reason}"),
        })?;
    if frame.jpeg.len() > 1024 * 1024 {
        return Err("Screenshot exceeds the 1 MiB response limit.".into());
    }
    let text = if name == "zoom" {
        let (w, h) = display_size(frame.source_width, frame.source_height);
        format!(
            "Zoomed region {} at {}x{}. Coordinates stay in the {w}x{h} screen space.",
            args["region"], frame.width, frame.height
        )
    } else {
        *detail = Some(format!("{}x{}", frame.width, frame.height));
        format!(
            "Screen {}x{} (source {}x{}).",
            frame.width, frame.height, frame.source_width, frame.source_height
        )
    };
    Ok(json!({
        "content": [
            {
                "type": "image",
                "mimeType": "image/jpeg",
                "data": base64::engine::general_purpose::STANDARD.encode(frame.jpeg)
            },
            {"type": "text", "text": text}
        ]
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinate_contract() {
        assert_eq!(display_size(3840, 2160), (1280, 720));
        assert_eq!(display_size(1920, 1080), (1280, 720));
        assert_eq!(display_size(1024, 768), (1024, 768));
        assert_eq!(point(&json!([0, 0]), (1280, 720)).unwrap(), (0, 0));
        assert_eq!(
            point(&json!([1279, 719]), (1280, 720)).unwrap(),
            (32767, 32767)
        );
        assert!(point(&json!([1280, 0]), (1280, 720)).is_err());
    }
    #[test]
    fn validates_bounds_and_unknown_arguments() {
        assert!(validate("type", &json!({"text":"中".repeat(1000)})).is_ok());
        assert!(validate("type", &json!({"text":"a".repeat(1001)})).is_err());
        assert!(validate("key", &json!({"text":"ctrl+s","repeat":101})).is_err());
        assert!(validate("mouse_move", &json!({"coordinate":[1,2],"other":1})).is_err());
        assert!(validate("wait", &json!({"duration":31})).is_err());
    }
}
