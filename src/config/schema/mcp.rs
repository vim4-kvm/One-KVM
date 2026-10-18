use serde::{Deserialize, Serialize};
use typeshare::typeshare;

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub enabled: bool,
    pub host_natural_scrolling: bool,
    pub screenshot_long_edge: u32,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host_natural_scrolling: false,
            screenshot_long_edge: 1280,
        }
    }
}

pub fn valid_screenshot_long_edge(value: u32) -> bool {
    matches!(value, 1280 | 1568 | 1920 | 2560)
}
