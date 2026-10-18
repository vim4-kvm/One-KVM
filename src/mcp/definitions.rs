//! Model-visible tool contracts. Keep each tool's schema beside its description.
use serde_json::{json, Value};

fn coordinate(description: &str) -> Value {
    json!({
        "type": "array",
        "items": {"type": "integer"},
        "minItems": 2,
        "maxItems": 2,
        "description": description
    })
}

fn modifiers() -> Value {
    json!({
        "type": "string",
        "description": "Modifier keys held only during this action: shift, ctrl, alt, super, joined with + (for example ctrl+shift)."
    })
}

fn click_properties() -> Value {
    json!({
        "coordinate": coordinate("[x, y] in screenshot pixels."),
        "text": modifiers()
    })
}

fn key_text() -> Value {
    json!({
        "type": "string",
        "description": "Key or combination joined with +, for example ctrl+s."
    })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let annotations = if super::tools::read_only(name) {
        json!({"readOnlyHint": true})
    } else {
        json!({
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true
        })
    };
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        },
        "annotations": annotations
    })
}

pub fn list() -> Vec<Value> {
    vec![
        tool(
            "screenshot",
            "Screenshot",
            "Capture the current screen of the computer behind the KVM. Returns a JPEG and its size. All coordinates for other tools are pixels in this image. Take one before acting and after each group of actions.",
            json!({}),
            &[],
        ),
        tool(
            "zoom",
            "Zoom",
            "Capture part of the screen at full source resolution to read small text or icons. The region uses screenshot coordinates; keep using screenshot coordinates afterwards, not coordinates in the zoomed image.",
            json!({
                "region": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "minItems": 4,
                    "maxItems": 4,
                    "description": "[x0, y0, x1, y1] in screenshot pixels: top-left and bottom-right corners."
                }
            }),
            &["region"],
        ),
        tool(
            "cursor_position",
            "Cursor position",
            "Return the last pointer position sent by the KVM, in screenshot coordinates. A person may have moved the mouse since.",
            json!({}),
            &[],
        ),
        tool(
            "mouse_move",
            "Move mouse",
            "Move the pointer to a position without clicking, for example to hover.",
            json!({"coordinate": coordinate("[x, y] in screenshot pixels.")}),
            &["coordinate"],
        ),
        tool(
            "left_click",
            "Left click",
            "Left-click at coordinate, or at the current pointer position if omitted.",
            click_properties(),
            &[],
        ),
        tool(
            "right_click",
            "Right click",
            "Right-click at coordinate, or at the current pointer position if omitted.",
            click_properties(),
            &[],
        ),
        tool(
            "middle_click",
            "Middle click",
            "Middle-click at coordinate, or at the current pointer position if omitted.",
            click_properties(),
            &[],
        ),
        tool(
            "double_click",
            "Double click",
            "Double-click the left button at coordinate, or at the current pointer position if omitted.",
            click_properties(),
            &[],
        ),
        tool(
            "triple_click",
            "Triple click",
            "Triple-click the left button (selects a line or paragraph) at coordinate, or at the current pointer position if omitted.",
            click_properties(),
            &[],
        ),
        tool(
            "left_click_drag",
            "Drag",
            "Press the left button at start_coordinate, move smoothly to coordinate, and release. Use for selecting text, moving windows, and sliders.",
            json!({
                "coordinate": coordinate("[x, y] in screenshot pixels where the drag ends."),
                "start_coordinate": coordinate("[x, y] in screenshot pixels where the drag starts."),
                "text": modifiers()
            }),
            &["start_coordinate", "coordinate"],
        ),
        tool(
            "left_mouse_down",
            "Left button down",
            "Press and keep holding the left button at the current pointer position. Use mouse_move and then left_mouse_up to finish a drag that left_click_drag cannot express. Released automatically after 30 seconds.",
            json!({}),
            &[],
        ),
        tool(
            "left_mouse_up",
            "Left button up",
            "Release the left button at the current pointer position.",
            json!({}),
            &[],
        ),
        tool(
            "scroll",
            "Scroll",
            "Scroll the mouse wheel at coordinate, or at the current pointer position if omitted. left and right are sent as Shift plus the vertical wheel, which most applications treat as horizontal scrolling.",
            json!({
                "coordinate": coordinate("[x, y] in screenshot pixels."),
                "text": modifiers(),
                "scroll_direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"],
                    "description": "Direction to scroll."
                },
                "scroll_amount": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Number of wheel steps (1-50). About 3 lines per step in most applications."
                }
            }),
            &["scroll_direction", "scroll_amount"],
        ),
        tool(
            "type",
            "Type text",
            "Type text at the current keyboard focus using a US keyboard layout. Characters that cannot be typed (for example CJK or emoji) are skipped and reported. Up to 1000 characters.",
            json!({
                "text": {
                    "type": "string",
                    "maxLength": 1000,
                    "description": "Text to type."
                }
            }),
            &["text"],
        ),
        tool(
            "key",
            "Press key",
            "Press a key or key combination once or several times, for example Return, Escape, ctrl+c, ctrl+shift+t, alt+Tab, super, F5. Names follow xdotool (Return, BackSpace, Page_Up, Up) and are case-insensitive.",
            json!({
                "text": key_text(),
                "repeat": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 1,
                    "description": "How many times to press it (1-100). Defaults to 1."
                }
            }),
            &["text"],
        ),
        tool(
            "hold_key",
            "Hold key",
            "Hold a key or key combination down for duration seconds, then release it.",
            json!({
                "text": key_text(),
                "duration": {
                    "type": "number",
                    "minimum": 0.05,
                    "maximum": 30,
                    "description": "Seconds to hold (0.05-30)."
                }
            }),
            &["text", "duration"],
        ),
        tool(
            "wait",
            "Wait",
            "Wait for duration seconds, for example while a page loads. Does not touch the keyboard or mouse.",
            json!({
                "duration": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": 30,
                    "description": "Seconds to wait (0-30)."
                }
            }),
            &["duration"],
        ),
    ]
}
