//! Platform selection and capability reporting.

pub mod capabilities;
pub mod defaults;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(unix)]
pub mod usb_reset;
#[cfg(feature = "vim4")]
pub mod vim4;
#[cfg(windows)]
pub mod windows;

pub use capabilities::{FeatureCapability, PlatformCapabilities, PlatformMode};
