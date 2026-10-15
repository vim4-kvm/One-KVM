//! Authenticated session diagnostics. Only the selected ICE pair is reported;
//! signaling credentials, SDP and unselected candidates are never exposed.

use serde::Serialize;
use typeshare::typeshare;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::ice_transport::ice_candidate_pair::RTCIceCandidatePair;
use webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;

#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct UuSessionStatus {
    pub id: String,
    pub client_id: Option<String>,
    pub state: String,
    pub ice_state: String,
    pub video_codec: String,
    pub link: Option<UuSessionLink>,
}

#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct UuSessionLink {
    pub relayed: bool,
    pub local: UuIceEndpoint,
    pub remote: UuIceEndpoint,
}

#[typeshare]
#[derive(Debug, Clone, Serialize)]
pub struct UuIceEndpoint {
    pub address: String,
    pub port: u16,
    /// ICE candidate protocol, not the transport between a peer and its TURN server.
    pub protocol: String,
    pub candidate_type: String,
    pub address_family: Option<String>,
}

impl From<RTCIceCandidate> for UuIceEndpoint {
    fn from(candidate: RTCIceCandidate) -> Self {
        let address_family = candidate
            .address
            .parse::<std::net::IpAddr>()
            .ok()
            .map(|ip| if ip.is_ipv4() { "IPv4" } else { "IPv6" }.to_string());
        Self {
            address: candidate.address,
            port: candidate.port,
            protocol: candidate.protocol.to_string(),
            candidate_type: candidate.typ.to_string(),
            address_family,
        }
    }
}

impl From<RTCIceCandidatePair> for UuSessionLink {
    fn from(pair: RTCIceCandidatePair) -> Self {
        Self {
            relayed: pair.local.typ == RTCIceCandidateType::Relay
                || pair.remote.typ == RTCIceCandidateType::Relay,
            local: pair.local.into(),
            remote: pair.remote.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_pair_detects_relay_at_either_end_and_preserves_address_families() {
        for (local_type, remote_type, relayed) in [
            (RTCIceCandidateType::Host, RTCIceCandidateType::Srflx, false),
            (RTCIceCandidateType::Relay, RTCIceCandidateType::Host, true),
            (RTCIceCandidateType::Host, RTCIceCandidateType::Relay, true),
        ] {
            let link = UuSessionLink::from(RTCIceCandidatePair::new(
                RTCIceCandidate {
                    address: "192.0.2.1".into(),
                    typ: local_type,
                    ..Default::default()
                },
                RTCIceCandidate {
                    address: "2001:db8::1".into(),
                    typ: remote_type,
                    ..Default::default()
                },
            ));
            assert_eq!(link.relayed, relayed);
            assert_eq!(link.local.address_family.as_deref(), Some("IPv4"));
            assert_eq!(link.remote.address_family.as_deref(), Some("IPv6"));
        }
        let endpoint = UuIceEndpoint::from(RTCIceCandidate {
            address: "peer.local".into(),
            ..Default::default()
        });
        assert_eq!(endpoint.address_family, None);
    }
}
