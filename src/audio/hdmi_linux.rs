//! Live HDMI format controls exposed by the T7 receiver. Old kernels retain
//! their ordinary ALSA capture path; this card uses native capture and conversion.
use alsa::ctl::{Ctl, ElemId, ElemIface, ElemType, ElemValue};
use std::ffi::CString;

use super::AudioConfig;
use crate::error::{AppError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SourceFormat {
    pub generation: i64,
    rate: i64,
    channels: i64,
    state: i64,
}

impl SourceFormat {
    pub fn can_open_after(self, current: Self) -> bool {
        self.ready() && self == current
    }

    pub fn capture_config(self, config: &AudioConfig) -> AudioConfig {
        let rate = self.rate as u32;
        // Native capture avoids ALSA rate's fractional slave rings. T7 needs
        // whole DMA periods; all seven rates have integral 20 ms periods.
        AudioConfig {
            sample_rate: rate,
            period_frames: rate / 50,
            buffer_frames: rate / 10,
            ..config.clone()
        }
    }

    pub fn ready(self) -> bool {
        self.state == 1
            && self.channels == 2
            && matches!(
                self.rate,
                32000 | 44100 | 48000 | 88200 | 96000 | 176400 | 192000
            )
    }
}

pub(super) struct HdmiSource {
    ctl: Ctl,
    pub pcm_name: String,
}

fn error(error: impl std::fmt::Display) -> AppError {
    AppError::AudioError(format!("HDMI source controls: {error}"))
}

impl HdmiSource {
    pub fn open(device: &str) -> Result<Option<Self>> {
        let Some(address) = device
            .strip_prefix("hw:")
            .or_else(|| device.strip_prefix("plughw:"))
        else {
            return Ok(None);
        };
        let card = address.split(',').next().unwrap_or(address);
        let ctl = Ctl::new(&format!("hw:{card}"), true).map_err(error)?;
        if ctl.card_info().map_err(error)?.get_id().map_err(error)? != "VIM4HDMIRX" {
            return Ok(None);
        }
        let source = Self {
            ctl,
            pcm_name: format!("hw:{address}"),
        };
        match source.integer("Generation") {
            // A baseline kernel has no live-format ABI. Preserve its old path.
            Err(failure) if failure.errno() == libc::ENOENT => Ok(None),
            Err(failure) => Err(error(failure)),
            Ok(_) => Ok(Some(source)),
        }
    }

    fn integer(&self, field: &str) -> alsa::Result<i64> {
        let mut id = ElemId::new(ElemIface::Mixer);
        id.set_name(&CString::new(format!("HDMI RX Audio {field}")).unwrap());
        let mut value = ElemValue::new(ElemType::Integer)?;
        value.set_id(&id);
        self.ctl.elem_read(&mut value)?;
        Ok(i64::from(value.get_integer(0).unwrap()))
    }

    pub fn snapshot(&self) -> Result<SourceFormat> {
        // Controls are separate ALSA reads. A generation read on either side
        // prevents combining a new rate with the preceding link's state.
        for _ in 0..3 {
            let generation = self.integer("Generation").map_err(error)?;
            let format = SourceFormat {
                generation,
                rate: self.integer("Rate").map_err(error)?,
                channels: self.integer("Channels").map_err(error)?,
                state: self.integer("State").map_err(error)?,
            };
            if generation == self.integer("Generation").map_err(error)? {
                return Ok(format);
            }
        }
        Err(AppError::AudioSourceChanged)
    }
}

pub(super) fn source_ready(device: &str) -> Result<bool> {
    match HdmiSource::open(device)? {
        Some(source) => Ok(source.snapshot()?.ready()),
        None => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_or_unsupported_source_never_opens_pcm() {
        let valid = SourceFormat {
            generation: 3,
            rate: 44100,
            channels: 2,
            state: 1,
        };
        assert!(valid.ready());
        assert!(!SourceFormat { state: 0, ..valid }.ready());
        assert!(!SourceFormat { state: 2, ..valid }.ready());
        assert!(!SourceFormat {
            channels: 8,
            ..valid
        }
        .ready());
        assert!(!SourceFormat { rate: 0, ..valid }.ready());
    }

    #[test]
    fn loss_or_new_generation_during_qualification_defers_pcm_open() {
        let ready = SourceFormat {
            generation: 3,
            rate: 48000,
            channels: 2,
            state: 1,
        };
        let lost = SourceFormat { state: 0, ..ready };
        let reconnected = SourceFormat {
            generation: 5,
            ..ready
        };
        assert!(!ready.can_open_after(lost));
        assert!(!ready.can_open_after(reconnected));
        assert!(!lost.can_open_after(lost));
        assert!(reconnected.can_open_after(reconnected));
        assert!(!ready.can_open_after(SourceFormat {
            rate: 44100,
            ..ready
        }));
    }

    #[test]
    fn resampling_keeps_whole_periods_on_both_sides_of_the_ring() {
        for rate in [32000, 44100, 48000, 88200, 96000, 176400, 192000] {
            let format = SourceFormat {
                generation: 1,
                rate,
                channels: 2,
                state: 1,
            };
            let config = format.capture_config(&AudioConfig::default());
            assert_eq!(config.sample_rate, rate as u32);
            assert_eq!(config.buffer_frames % config.period_frames, 0);
            assert_eq!(config.period_frames * 50, config.sample_rate);
        }
    }
}
