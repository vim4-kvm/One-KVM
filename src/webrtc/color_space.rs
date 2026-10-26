//! WebRTC color-space metadata accompanies the final packet of an access unit.
//! The decoder's Main10 profile alone does not identify PQ or BT.2020.
use rtp::extension::HeaderExtension;
use webrtc::util::{Marshal, MarshalSize};

pub(crate) const URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/color-space";

struct ColorSpace([u8; 4]);

impl MarshalSize for ColorSpace {
    fn marshal_size(&self) -> usize {
        4
    }
}

impl Marshal for ColorSpace {
    fn marshal_to(&self, buffer: &mut [u8]) -> webrtc::util::Result<usize> {
        buffer
            .get_mut(..4)
            .ok_or_else(|| webrtc::util::Error::Other("color-space buffer too short".into()))?
            .copy_from_slice(&self.0);
        Ok(4)
    }
}

pub(crate) fn extension(color: Option<[u8; 4]>, last_packet: bool) -> Option<HeaderExtension> {
    color
        .filter(|_| last_packet)
        .map(|color| HeaderExtension::Custom {
            uri: URI.into(),
            extension: Box::new(ColorSpace(color)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_metadata_is_only_attached_to_the_final_packet() {
        let pq2020 = [9, 16, 9, 0x10];
        assert!(extension(Some(pq2020), false).is_none());
        assert!(extension(None, true).is_none());
        let HeaderExtension::Custom {
            uri,
            extension: payload,
        } = extension(Some(pq2020), true).unwrap()
        else {
            panic!("custom extension required")
        };
        assert_eq!(uri, URI);
        let mut bytes = [0; 4];
        assert_eq!(payload.marshal_to(&mut bytes).unwrap(), 4);
        assert_eq!(bytes, pq2020);
        assert!(payload.marshal_to(&mut [0; 3]).is_err());
    }
}
