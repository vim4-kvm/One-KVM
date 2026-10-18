use serde::{Deserialize, Serialize};
use typeshare::typeshare;
#[typeshare]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpActivity {
    #[typeshare(typescript(type = "\"idle\" | \"active\" | \"manual\""))]
    pub state: String,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub client: Option<String>,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub tool: Option<String>,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub detail: Option<String>,
    pub since: String,
}
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallRecord {
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub text: Option<String>,
    pub at: String,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub client: Option<String>,
    pub tool: String,
    pub ok: bool,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub error: Option<String>,
    pub duration_ms: u32,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub detail: Option<String>,
}
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCapabilities {
    pub keyboard: bool,
    pub absolute_mouse: bool,
    pub video_signal: bool,
}
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStatus {
    pub screenshot_long_edge: u32,
    #[typeshare(serialized_as = "String", typescript(type = "[number, number] | null"))]
    pub source_size: Option<[u32; 2]>,
    pub enabled: bool,
    pub host_natural_scrolling: bool,
    pub endpoint_path: String,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub api_key_preview: Option<String>,
    #[typeshare(serialized_as = "String", typescript(type = "string | null"))]
    pub credentials_error: Option<String>,
    pub activity: McpActivity,
    pub recent: Vec<McpCallRecord>,
    pub capabilities: McpCapabilities,
    pub computer_use_active: bool,
}
#[typeshare]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfigUpdate {
    pub screenshot_long_edge: Option<u32>,
    pub enabled: Option<bool>,
    pub host_natural_scrolling: Option<bool>,
}
#[typeshare]
#[derive(Serialize)]
pub struct McpKey {
    pub api_key: String,
}
