//! Native UU capabilities accompany ConnectOptions separately from WebRTC SDP.

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct ViewerCapabilities {
    display_hdr: Option<Vec<i64>>,
    main10_sizes: Vec<(u32, u32)>,
}

impl ViewerCapabilities {
    pub fn from_header(header: &Value) -> Self {
        let data = header.get("data").unwrap_or(header);
        let Some(streamer) = header
            .get("streamer_data")
            .or_else(|| data.get("streamer_data"))
            .and_then(Value::as_str)
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
        else {
            return Self::default();
        };
        let Some(capability) = streamer.get("device_capability") else {
            return Self::default();
        };
        let display_hdr = capability
            .get("display_info")
            .and_then(Value::as_array)
            .map(|displays| {
                // This is support, not the current HDR switch: the official
                // Mac SDK emits 0 for an EDR-capable screen and -1 otherwise.
                displays
                    .iter()
                    .filter_map(|display| display.get("hdr").and_then(Value::as_i64))
                    .collect()
            });
        let main10_sizes = capability
            .get("video_codec_capability")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|codec| {
                codec.get("video_codec").and_then(Value::as_u64) == Some(2)
                    && codec.get("chroma_sampling").and_then(Value::as_u64) == Some(1)
                    && codec.get("bit_depth").and_then(Value::as_u64) == Some(10)
            })
            .filter_map(|codec| {
                let width = u32::try_from(codec.get("width")?.as_u64()?).ok()?;
                let height = u32::try_from(codec.get("height")?.as_u64()?).ok()?;
                (width > 0 && height > 0).then_some((width, height))
            })
            .collect();
        Self {
            display_hdr,
            main10_sizes,
        }
    }

    pub fn hdr_display(&self) -> Option<bool> {
        self.display_hdr
            .as_ref()
            .map(|displays| displays.contains(&0))
    }

    pub fn supports_main10(&self, width: i32, height: i32) -> bool {
        width > 0
            && height > 0
            && self.main10_sizes.iter().any(|&(max_width, max_height)| {
                width as u32 <= max_width && height as u32 <= max_height
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_main10_decoder_and_sdr_screen_are_independent() {
        // Numeric capability entries observed from the official Mac 4.42.0
        // viewer; zero-size entries explicitly mean unsupported.
        let mut capability = json!({
            "display_info": [{"hdr": -1}],
            "video_codec_capability": [
                {"video_codec":1,"bit_depth":10,"chroma_sampling":1,"width":0,"height":0},
                {"video_codec":2,"bit_depth":8,"chroma_sampling":1,"width":3840,"height":2160},
                {"video_codec":2,"bit_depth":10,"chroma_sampling":1,"width":3840,"height":2160}
            ]
        });
        let header = |caps: &Value| json!({"data":{"streamer_data":json!({"device_capability":caps}).to_string()}});
        let viewer = ViewerCapabilities::from_header(&header(&capability));
        assert_eq!(viewer.hdr_display(), Some(false));
        assert!(viewer.supports_main10(3840, 2160));
        assert!(!viewer.supports_main10(3840, 2161));
        assert!(!viewer.supports_main10(4096, 2160));
        capability["display_info"][0]["hdr"] = json!(0);
        capability["video_codec_capability"][2]["width"] = json!(0);
        let viewer = ViewerCapabilities::from_header(&header(&capability));
        assert_eq!(viewer.hdr_display(), Some(true));
        assert!(!viewer.supports_main10(1920, 1080));
        capability["video_codec_capability"][2]["width"] = json!(3840);
        capability["video_codec_capability"][2]["chroma_sampling"] = json!(3);
        assert!(!ViewerCapabilities::from_header(&header(&capability)).supports_main10(1920, 1080));
    }

    #[test]
    fn missing_or_invalid_capabilities_do_not_grant_main10() {
        for header in [
            json!({}),
            json!({"streamer_data":"invalid"}),
            json!({"streamer_data":json!({"device_capability":{}}).to_string()}),
        ] {
            let viewer = ViewerCapabilities::from_header(&header);
            assert_eq!(viewer.hdr_display(), None);
            assert!(!viewer.supports_main10(1920, 1080));
        }
    }
}
