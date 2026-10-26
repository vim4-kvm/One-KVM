//! Per-connection cloud ICE configuration. Never derive Debug or log credentials.

use serde_json::Value;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

#[derive(Clone, Default)]
pub struct SessionIce {
    servers: Vec<RTCIceServer>,
    relay_only: bool,
    #[cfg(test)]
    host_only: bool,
}

impl SessionIce {
    // Empty cloud iceServers retains the production public-STUN fallback.
    // Local integration fixtures must explicitly disable external gathering.
    #[cfg(test)]
    pub(crate) fn host_only() -> Self {
        Self {
            host_only: true,
            ..Default::default()
        }
    }

    /// ICE servers and the relay policy the cloud attaches to a be-controlled request.
    pub fn from_header(header: &Value) -> Self {
        let data = header.get("data").unwrap_or(header);
        let field = |name: &str| header.get(name).or_else(|| data.get(name));
        let mut result = Self {
            relay_only: field("force_relay")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Default::default()
        };
        for server in field("iceServers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(16)
        {
            let urls: Vec<String> = match server.get("urls").or_else(|| server.get("url")) {
                Some(Value::String(url)) => vec![url.clone()],
                Some(Value::Array(urls)) => urls
                    .iter()
                    .filter_map(Value::as_str)
                    .take(16)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            }
            .into_iter()
            .filter(|url| {
                url.len() <= 2048
                    && ["stun:", "stuns:", "turn:", "turns:"]
                        .iter()
                        .any(|p| url.starts_with(p))
            })
            .collect();
            if urls.is_empty() {
                continue;
            }
            let string = |name| {
                server
                    .get(name)
                    .and_then(Value::as_str)
                    .filter(|s| s.len() <= 4096)
                    .unwrap_or("")
                    .to_owned()
            };
            result.servers.push(RTCIceServer {
                urls,
                username: string("username"),
                credential: string("credential"),
            });
        }
        result
    }

    pub fn configuration(self) -> Result<RTCConfiguration, String> {
        #[cfg(test)]
        if self.host_only {
            return Ok(RTCConfiguration::default());
        }
        let has_turn = self.servers.iter().any(|s| {
            s.urls
                .iter()
                .any(|u| u.starts_with("turn:") || u.starts_with("turns:"))
        });
        if self.relay_only && !has_turn {
            return Err("Relay-only session requested without usable TURN configuration".into());
        }
        let supplied = !self.servers.is_empty();
        let ice_servers = if supplied {
            self.servers
        } else {
            vec![RTCIceServer {
                urls: vec![
                    "stun:stun.l.google.com:19302".into(),
                    "stun:stun.miwifi.com:3478".into(),
                ],
                ..Default::default()
            }]
        };
        tracing::info!(
            "UU session ICE: cloud_supplied={supplied} servers={} turn={has_turn} relay_only={}",
            ice_servers.len(),
            self.relay_only
        );
        Ok(RTCConfiguration {
            ice_servers,
            ice_transport_policy: if self.relay_only {
                RTCIceTransportPolicy::Relay
            } else {
                RTCIceTransportPolicy::All
            },
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_credentials_are_scoped_and_relay_policy_is_honored() {
        let header = serde_json::json!({"data":{"iceServers":[{"urls":["turn:example.invalid:3478?transport=udp"],"username":"test-user","credential":"test-secret"}],"force_relay":true}});
        let config = SessionIce::from_header(&header).configuration().unwrap();
        assert_eq!(config.ice_transport_policy, RTCIceTransportPolicy::Relay);
        assert_eq!(config.ice_servers[0].username, "test-user");
        assert_eq!(config.ice_servers[0].credential, "test-secret");
    }

    #[test]
    fn unsupported_urls_are_rejected_and_relay_never_falls_back() {
        let header = serde_json::json!({"force_relay":true,"iceServers":[{"urls":"https://example.invalid"}]});
        assert!(SessionIce::from_header(&header).configuration().is_err());
        let config = SessionIce::default().configuration().unwrap();
        assert_eq!(config.ice_transport_policy, RTCIceTransportPolicy::All);
        assert!(config.ice_servers[0]
            .urls
            .iter()
            .all(|s| s.starts_with("stun:")));
    }
}
