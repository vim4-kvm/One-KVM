use serde::{Deserialize, Serialize};
use typeshare::typeshare;

pub const TERMINAL_BAUDS: [u32; 9] = [
    9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1500000,
];

#[typeshare]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UuTerminalConfig {
    pub board: UuTerminalBoardConfig,
    pub serial: UuTerminalSerialConfig,
    pub ssh: UuTerminalSshConfig,
}

#[typeshare]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UuTerminalBoardConfig {
    pub enabled: bool,
}

#[typeshare]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OtgSerialConfig {
    pub enabled: bool,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UuTerminalSerialConfig {
    pub enabled: bool,
    pub device: String,
    pub baud: u32,
}
impl Default for UuTerminalSerialConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device: String::new(),
            baud: 115200,
        }
    }
}
impl UuTerminalSerialConfig {
    pub fn path(&self) -> &str {
        if self.device == "usb-gadget" {
            "/dev/ttyGS0"
        } else {
            &self.device
        }
    }
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UuTerminalSshConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
}
impl Default for UuTerminalSshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            user: String::new(),
        }
    }
}
impl UuTerminalConfig {
    pub fn validate(&self) -> Result<(), String> {
        let device = &self.serial.device;
        let allowed_device = device.is_empty() || device == "usb-gadget" || {
            let path = std::path::Path::new(device);
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let numeric_tty = ["ttyUSB", "ttyACM"].iter().any(|p| {
                name.strip_prefix(p)
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            });
            (path.parent() == Some(std::path::Path::new("/dev")) && numeric_tty)
                || (path.parent() == Some(std::path::Path::new("/dev/serial/by-id"))
                    && !name.is_empty())
        };
        if !allowed_device
            || device.chars().any(char::is_control)
            || device.split('/').any(|p| matches!(p, "." | ".."))
        {
            return Err("请选择 USB 串口设备".into());
        }
        if !TERMINAL_BAUDS.contains(&self.serial.baud) {
            return Err("不支持此波特率".into());
        }
        self.ssh.validate()
    }
}
impl UuTerminalSshConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (value, max) in [(&self.host, 253), (&self.user, 64)] {
            if value.starts_with('-')
                || value.chars().count() > max
                || value.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err("SSH 地址和用户名不能以 - 开头、含空白或控制字符，且不能超长".into());
            }
        }
        if self.port == 0 {
            return Err("SSH 端口必须为 1–65535".into());
        }
        Ok(())
    }
    pub fn configured(&self) -> bool {
        !self.host.is_empty() && !self.user.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_devices_baud_and_ssh_arguments() {
        let mut c = UuTerminalConfig::default();
        for path in [
            "",
            "usb-gadget",
            "/dev/ttyUSB0",
            "/dev/ttyACM12",
            "/dev/serial/by-id/usb-FTDI",
        ] {
            c.serial.device = path.into();
            assert!(c.validate().is_ok());
        }
        for path in [
            "/etc/passwd",
            "/dev/ttyUSB0/../ttyS0",
            "/dev/ttyUSBx",
            "/dev/serial/by-id/../../passwd",
        ] {
            c.serial.device = path.into();
            assert!(c.validate().is_err());
        }
        c.serial.device.clear();
        c.serial.baud = 1500000;
        assert!(c.validate().is_ok());
        c.serial.baud = 12345;
        assert!(c.validate().is_err());
        c.serial.baud = 115200;
        for value in ["-ProxyCommand=x", "bad host", "host\n", "\0"] {
            c.ssh.host = value.into();
            assert!(c.validate().is_err());
            c.ssh.host.clear();
            c.ssh.user = value.into();
            assert!(c.validate().is_err());
            c.ssh.user.clear();
        }
        c.ssh.port = 0;
        assert!(c.validate().is_err());
    }
}
