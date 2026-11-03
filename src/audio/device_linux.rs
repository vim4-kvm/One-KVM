use alsa::ctl::Ctl;
use alsa::pcm::HwParams;
use alsa::{Direction, PCM};
use tracing::{debug, info, warn};

use super::AudioDeviceInfo;
use crate::error::{AppError, Result};

fn card_id(index: i32) -> Option<String> {
    Ctl::new(&format!("hw:{index}"), true)
        .and_then(|ctl| ctl.card_info())
        .and_then(|info| info.get_id().map(str::to_owned))
        .ok()
}

fn resolve_numeric_name(name: &str, lookup: impl FnOnce(i32) -> Option<String>) -> String {
    if let Some((card, device)) = name.strip_prefix("hw:").and_then(|n| n.split_once(',')) {
        if let (Ok(index), Ok(pcm)) = (card.parse::<u32>(), device.parse::<u32>()) {
            if let Ok(index) = i32::try_from(index) {
                if let Some(id) = lookup(index) {
                    return format!("hw:{id},{pcm}");
                }
            }
        }
    }
    name.to_string()
}

pub(super) fn canonical_device_name(name: &str) -> String {
    // Resolve only the numeric card actually requested. A USB input at index
    // zero must never be interpreted as the board-owned HDMI input.
    resolve_numeric_name(name, card_id)
}

fn get_usb_bus_info(card_index: i32) -> Option<String> {
    if card_index < 0 {
        return None;
    }

    let device_path = format!("/sys/class/sound/card{}/device", card_index);
    let link_target = std::fs::read_link(&device_path).ok()?;
    let link_str = link_target.to_string_lossy();

    for component in link_str.split('/') {
        if component.contains('-')
            && !component.contains(':')
            && component.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Some(component.to_string());
        }
    }

    None
}

pub(super) fn enumerate_audio_devices_with_current(
    current_device: Option<&str>,
) -> Result<Vec<AudioDeviceInfo>> {
    let mut devices = Vec::new();

    let cards = alsa::card::Iter::new();

    for card_result in cards {
        let card = match card_result {
            Ok(c) => c,
            Err(e) => {
                debug!("Error iterating card: {}", e);
                continue;
            }
        };

        let card_index = card.get_index();
        let ctl = Ctl::new(&format!("hw:{card_index}"), true).ok();
        let card_id = card_id(card_index).unwrap_or_else(|| card_index.to_string());
        let card_name = card.get_name().unwrap_or_else(|_| "Unknown".to_string());
        let card_longname = card.get_longname().unwrap_or_else(|_| card_name.clone());

        debug!("Found audio card {}: {}", card_index, card_longname);

        let long_lower = card_longname.to_lowercase();
        let is_hdmi = long_lower.contains("hdmi")
            || long_lower.contains("capture")
            || long_lower.contains("usb");

        let usb_bus = get_usb_bus_info(card_index);

        for device_index in 0..8 {
            let endpoint_present = ctl.as_ref().is_some_and(|ctl| {
                ctl.pcm_info(device_index as u32, 0, Direction::Capture)
                    .is_ok()
            });
            let device_name = format!("hw:{card_id},{device_index}");
            let is_current_device = current_device.is_some_and(|current| {
                current == device_name || current == format!("hw:{card_index},{device_index}")
            });
            let board_hdmi = cfg!(feature = "vim4")
                && device_index == 0
                && matches!(card_id.as_str(), "VIM4HDMIRX" | "AMLAUGESOUND");
            let is_hdmi = is_hdmi || board_hdmi;
            let description = if board_hdmi {
                "HDMI input".to_string()
            } else {
                format!("{card_longname} - Device {device_index}")
            };

            let mut push_info =
                |sample_rates: Vec<u32>, channels: Vec<u32>, description: String| {
                    devices.push(AudioDeviceInfo {
                        name: device_name.clone(),
                        description,
                        card_index,
                        device_index,
                        sample_rates,
                        channels,
                        is_capture: true,
                        is_hdmi,
                        usb_bus: usb_bus.clone(),
                    });
                };

            if is_current_device {
                // An owned PCM is exclusive. Keep the real selected endpoint,
                // without opening it again or inventing sample-rate caps.
                push_info(vec![], vec![], format!("{description} (in use)"));
                continue;
            }

            match PCM::new(&device_name, Direction::Capture, false) {
                Ok(pcm) => {
                    let (sample_rates, channels) = query_device_caps(&pcm);

                    if !sample_rates.is_empty() && !channels.is_empty() {
                        push_info(sample_rates, channels, description.clone());
                    } else if endpoint_present {
                        push_info(vec![], vec![], description.clone());
                    }
                }
                Err(error) => {
                    if endpoint_present {
                        debug!(
                            "Capture endpoint {} is present but unavailable: {}; capabilities unknown",
                            device_name, error
                        );
                        push_info(vec![], vec![], description.clone());
                    }
                }
            }
        }
    }

    info!("Found {} audio capture devices", devices.len());
    Ok(devices)
}

fn query_device_caps(pcm: &PCM) -> (Vec<u32>, Vec<u32>) {
    let hwp = match HwParams::any(pcm) {
        Ok(h) => h,
        Err(_) => return (vec![], vec![]),
    };

    let common_rates = [
        8000, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000,
    ];
    let mut supported_rates = Vec::new();

    for rate in &common_rates {
        if hwp.test_rate(*rate).is_ok() {
            supported_rates.push(*rate);
        }
    }

    let mut supported_channels = Vec::new();
    for ch in 1..=8 {
        if hwp.test_channels(ch).is_ok() {
            supported_channels.push(ch);
        }
    }

    (supported_rates, supported_channels)
}

pub(super) fn find_best_audio_device() -> Result<AudioDeviceInfo> {
    let devices = enumerate_audio_devices_with_current(None)?;

    if devices.is_empty() {
        return Err(AppError::AudioError(
            "No audio capture devices found".to_string(),
        ));
    }

    let mut first_48k_stereo: Option<&AudioDeviceInfo> = None;
    for device in &devices {
        if !device.sample_rates.contains(&48000) || !device.channels.contains(&2) {
            continue;
        }
        if device.is_hdmi {
            info!("Selected HDMI audio device: {}", device.description);
            return Ok(device.clone());
        }
        if first_48k_stereo.is_none() {
            first_48k_stereo = Some(device);
        }
    }
    if let Some(device) = first_48k_stereo {
        info!("Selected audio device: {}", device.description);
        return Ok(device.clone());
    }

    let device = devices.into_iter().next().unwrap();
    warn!(
        "Using fallback audio device: {} (may not support optimal settings)",
        device.description
    );
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_configuration_resolves_only_its_actual_card_and_preserves_pcm() {
        let lookup = |index| match index {
            0 => Some("USBMic".to_string()),
            1 => Some("VIM4HDMIRX".to_string()),
            _ => None,
        };
        assert_eq!(resolve_numeric_name("hw:0,0", lookup), "hw:USBMic,0");
        assert_eq!(resolve_numeric_name("hw:1,0", lookup), "hw:VIM4HDMIRX,0");
        assert_eq!(resolve_numeric_name("hw:0,2", lookup), "hw:USBMic,2");
        assert_eq!(resolve_numeric_name("hw:9,0", lookup), "hw:9,0");
        for name in ["", "hw:AMLAUGESOUND,0", "plughw:0,0", "default", "hw:0,0,1"] {
            assert_eq!(
                resolve_numeric_name(name, |_| panic!("must not migrate this selector")),
                name
            );
        }
    }

    #[test]
    fn test_enumerate_devices() {
        let result = enumerate_audio_devices_with_current(None);
        println!("Audio devices: {:?}", result);
        assert!(result.is_ok());
    }
}
