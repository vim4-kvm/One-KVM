use serde::{Deserialize, Serialize};
use typeshare::typeshare;

#[typeshare]
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BroadcastConfig {
    pub srt: SrtOutputConfig,
    pub rtmp: RtmpOutputConfig,
}

// Destinations may contain publishing credentials; AppConfig is Debug elsewhere.
impl std::fmt::Debug for BroadcastConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BroadcastConfig")
            .field("srt_enabled", &self.srt.enabled)
            .field("rtmp_enabled", &self.rtmp.enabled)
            .finish_non_exhaustive()
    }
}

#[typeshare]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SrtOutputConfig {
    pub enabled: bool,
    pub port: u16,
    pub latency_ms: u32,
    pub passphrase: String,
    pub audio: bool,
}

impl Default for SrtOutputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8890,
            latency_ms: 120,
            passphrase: String::new(),
            audio: true,
        }
    }
}

#[typeshare]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtmpOutputConfig {
    pub enabled: bool,
    pub url: String,
    pub audio: bool,
}

impl Default for RtmpOutputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            audio: true,
        }
    }
}

impl BroadcastConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.srt.port < 1024 {
            return Err("SRT port must be between 1024 and 65535");
        }
        if !(20..=8000).contains(&self.srt.latency_ms) {
            return Err("SRT latency must be between 20 and 8000 ms");
        }
        if !self.srt.passphrase.is_empty()
            && (!(10..=79).contains(&self.srt.passphrase.len())
                || !self.srt.passphrase.is_ascii()
                || self.srt.passphrase.bytes().any(|b| b.is_ascii_control()))
        {
            return Err("SRT passphrase must contain 10–79 printable ASCII characters");
        }
        if self.rtmp.enabled || !self.rtmp.url.is_empty() {
            let url =
                reqwest::Url::parse(&self.rtmp.url).map_err(|_| "Invalid RTMP publishing URL")?;
            if self.rtmp.url.len() > 4096
                || self.rtmp.url.chars().any(char::is_whitespace)
                || !matches!(url.scheme(), "rtmp" | "rtmps")
                || url.host_str().is_none()
                || url.path().trim_matches('/').is_empty()
                || url.fragment().is_some()
            {
                return Err("Use an rtmp:// or rtmps:// publishing URL including the stream key");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publishing_destinations_and_srt_encryption_are_validated() {
        let mut c = BroadcastConfig::default();
        for url in [
            "file:///tmp/out",
            "https://example.com/live/key",
            "rtmp://",
            "rtmp://example.com/",
            "rtmp://example.com/live/key\n",
        ] {
            c.rtmp.url = url.into();
            assert!(c.validate().is_err(), "{url}");
        }
        for url in [
            "rtmp://127.0.0.1:1935/live/key",
            "rtmps://example.com/live/key?token=abc",
        ] {
            c.rtmp.url = url.into();
            assert!(c.validate().is_ok());
        }
        c.srt.passphrase = "short".into();
        assert!(c.validate().is_err());
        c.srt.passphrase = "test passphrase".into();
        assert!(c.validate().is_ok());
        let debug = format!("{c:?}");
        assert!(!debug.contains("test passphrase"));
        assert!(!debug.contains("token=abc"));
    }
}
