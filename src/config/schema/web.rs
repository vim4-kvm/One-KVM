use serde::{Deserialize, Serialize};
use typeshare::typeshare;

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub session_timeout_secs: u32,
    pub single_user_allow_multiple_sessions: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_timeout_secs: 3600 * 24,
            single_user_allow_multiple_sessions: false,
        }
    }
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VideoConfig {
    #[cfg(feature = "vim4")]
    #[typeshare(skip)]
    pub output: crate::platform::vim4::OutputSettings,
    pub device: Option<String>,
    pub format: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub quality: u32,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            #[cfg(feature = "vim4")]
            output: Default::default(),
            device: None,
            format: None,
            width: 1920,
            height: 1080,
            fps: 30,
            quality: 80,
        }
    }
}

#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MsdConfig {
    pub enabled: bool,
    pub msd_dir: String,
    pub flash_inquiry_string: String,
    pub cdrom_inquiry_string: String,
}

pub const DEFAULT_FLASH_INQUIRY_STRING: &str = "One-KVM Virtual Flash";
pub const DEFAULT_CDROM_INQUIRY_STRING: &str = "One-KVM Virtual CD-ROM";
pub const MAX_INQUIRY_STRING_BYTES: usize = 28;

impl Default for MsdConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            msd_dir: String::new(),
            flash_inquiry_string: DEFAULT_FLASH_INQUIRY_STRING.to_string(),
            cdrom_inquiry_string: DEFAULT_CDROM_INQUIRY_STRING.to_string(),
        }
    }
}

impl MsdConfig {
    pub fn validate(&self) -> crate::error::Result<()> {
        Self::validate_inquiry_string("Flash", &self.flash_inquiry_string)?;
        Self::validate_inquiry_string("CD-ROM", &self.cdrom_inquiry_string)
    }

    pub fn validate_inquiry_string(kind: &str, value: &str) -> crate::error::Result<()> {
        let value = value.trim();
        if value.is_empty() {
            return Err(crate::error::AppError::BadRequest(format!(
                "MSD {kind} inquiry string cannot be empty"
            )));
        }
        if value.len() > MAX_INQUIRY_STRING_BYTES {
            return Err(crate::error::AppError::BadRequest(format!(
                "MSD {kind} inquiry string must be at most {MAX_INQUIRY_STRING_BYTES} bytes"
            )));
        }
        if !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            return Err(crate::error::AppError::BadRequest(format!(
                "MSD {kind} inquiry string must contain printable ASCII characters only"
            )));
        }
        Ok(())
    }

    pub fn msd_dir_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.msd_dir)
    }

    pub fn images_dir(&self) -> std::path::PathBuf {
        self.msd_dir_path().join("images")
    }

    pub fn ventoy_dir(&self) -> std::path::PathBuf {
        self.msd_dir_path().join("ventoy")
    }

    pub fn drive_path(&self) -> std::path::PathBuf {
        self.ventoy_dir().join("ventoy.img")
    }
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    pub device: String,
    pub quality: String,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device: String::new(),
            quality: "balanced".to_string(),
        }
    }
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    pub auth_mode: AuthMode,
    pub http_port: u16,
    pub https_port: u16,
    pub bind_addresses: Vec<String>,
    pub bind_address: String,
    pub https_enabled: bool,
    pub ssl_cert_path: Option<String>,
    pub ssl_key_path: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            auth_mode: AuthMode::Native,
            http_port: 8080,
            https_port: 8443,
            bind_addresses: Vec::new(),
            bind_address: "0.0.0.0".to_string(),
            https_enabled: false,
            ssl_cert_path: None,
            ssl_key_path: None,
        }
    }
}

#[typeshare]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    Native,
    Managed,
}

impl WebConfig {
    pub fn validate_managed(&self) -> crate::error::Result<()> {
        if self.auth_mode != AuthMode::Managed {
            return Ok(());
        }
        let addresses = if self.bind_addresses.is_empty() {
            std::slice::from_ref(&self.bind_address)
        } else {
            &self.bind_addresses
        };
        if addresses.iter().any(|address| {
            !address
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        }) || self.https_enabled
        {
            return Err(crate::error::AppError::BadRequest(
                "Managed authentication requires loopback HTTP addresses and HTTPS disabled".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_accepts_only_loopback_http() {
        let mut web: WebConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(web.auth_mode, AuthMode::Native);
        assert!(web.validate_managed().is_ok());
        web.auth_mode = AuthMode::Managed;
        for address in [
            "0.0.0.0",
            "::",
            "192.168.1.1",
            "localhost",
            "::ffff:127.0.0.1",
        ] {
            web.bind_address = address.into();
            assert!(web.validate_managed().is_err(), "{address}");
        }
        web.bind_addresses = vec!["127.0.0.2".into(), "::1".into()];
        assert!(web.validate_managed().is_ok());
        web.https_enabled = true;
        assert!(web.validate_managed().is_err());
        assert!(serde_json::from_str::<WebConfig>(r#"{"auth_mode":"invalid"}"#).is_err());
    }

    #[test]
    fn msd_inquiry_strings_default_and_validate() {
        assert!(MsdConfig::default().validate().is_ok());
        assert!(MsdConfig::validate_inquiry_string("Flash", "  Custom Drive  ").is_ok());
        assert!(MsdConfig::validate_inquiry_string("Flash", "").is_err());
        assert!(MsdConfig::validate_inquiry_string("Flash", &"x".repeat(29)).is_err());
        assert!(MsdConfig::validate_inquiry_string("CD-ROM", "虚拟光驱").is_err());
        assert!(MsdConfig::validate_inquiry_string("CD-ROM", "bad\tname").is_err());
    }
}
