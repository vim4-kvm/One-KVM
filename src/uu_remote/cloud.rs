//! UU cloud REST client: signed requests, host registration, QR login and room creation.
//! Windows and macOS identities use their respective platform and registration payloads.
//! Tokens, CSRF values, QR URLs and login tickets are never logged.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{UuRemoteConfig, UuRemoteSystem};
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const BASE_URL: &str = "https://api.nrd.nie.163.com";
/// Request-signing key built into the official UU PC client; the same for every user.
/// The credential file's `sign_key` overrides it if NetEase rotates the key.
pub const DEFAULT_SIGN_KEY: &str = "alWiSzXZTLu3WfFnw13uBru3";
/// API code for an invalid or expired login token (HTTP 422).
pub const TOKEN_INVALID: i64 = 1120;
/// API code for an expired login QR code (HTTP 400): generate a new one.
pub const QR_EXPIRED: i64 = 1190;

/// Host identity and account binding, stored privately (0600) in the One-KVM data directory.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub user_id: String,
    pub device_id: String,
    pub client_id: String,
    pub system_id: String,
    pub machine_guid: String,
    pub token: String,
    pub csrf: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_key: Option<String>,
    /// Account display name shown in the web UI; not a secret.
    pub nickname: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

impl Credentials {
    pub fn logged_in(&self) -> bool {
        [
            &self.user_id,
            &self.device_id,
            &self.client_id,
            &self.system_id,
            &self.token,
            &self.csrf,
        ]
        .iter()
        .all(|v| !v.is_empty())
    }

    /// Stable host identity reused across logins so the account keeps one device.
    pub fn ensure_identity(&mut self) -> bool {
        let mut changed = false;
        for (value, make) in [
            (&mut self.client_id, random_uuid as fn() -> String),
            (&mut self.system_id, || random_uuid().to_uppercase()),
            (&mut self.machine_guid, machine_guid),
        ] {
            if value.is_empty() {
                *value = make();
                changed = true;
            }
        }
        changed
    }

    pub fn logout(&mut self) {
        self.user_id.clear();
        self.token.clear();
        self.csrf.clear();
        self.nickname.clear();
    }

    fn sign_key(&self) -> &str {
        self.sign_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .unwrap_or(DEFAULT_SIGN_KEY)
    }
}

pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join("uu-remote").join("credentials.json")
}

pub fn load_credentials(path: &Path) -> Result<Credentials, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("invalid UU credentials file: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
        Err(e) => Err(format!("cannot read UU credentials: {e}")),
    }
}

pub fn save_credentials(path: &Path, creds: &Credentials) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or("credentials path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    file.write_all(&serde_json::to_vec_pretty(creds).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| format!("replace {}: {e}", path.display()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomData {
    pub signaling_server: String,
    pub token: String,
}

#[derive(Debug)]
pub enum CloudError {
    LoginExpired,
    Other(String),
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoginExpired => f.write_str("UU login token invalid or expired"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

pub struct CloudClient {
    creds: Credentials,
    http: reqwest::Client,
    system: UuRemoteSystem,
}

fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> Result<String, String> {
    use openssl::{hash::MessageDigest, pkey::PKey, sign::Signer};
    let key = PKey::hmac(key).map_err(|e| e.to_string())?;
    let mut signer = Signer::new(MessageDigest::sha256(), &key).map_err(|e| e.to_string())?;
    let mac = signer
        .sign_oneshot_to_vec(data)
        .map_err(|e| e.to_string())?;
    Ok(mac.iter().map(|b| format!("{b:02x}")).collect())
}

/// NRD signature: HMAC-SHA256(METHOD + path + sorted "x-param-k=v" joined by '&' + body).
fn sign(
    key: &str,
    method: &str,
    path: &str,
    params: &std::collections::BTreeMap<String, String>,
    body: &str,
) -> Result<String, String> {
    let joined = params
        .iter()
        .filter(|(k, _)| k.starts_with("x-param-") && k.as_str() != "x-param-sign")
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    hmac_sha256_hex(
        key.as_bytes(),
        format!("{}{path}{joined}{body}", method.to_uppercase()).as_bytes(),
    )
}

impl CloudClient {
    pub fn new(creds: Credentials) -> Self {
        Self::for_system(creds, UuRemoteSystem::Windows)
    }

    pub fn for_system(creds: Credentials, system: UuRemoteSystem) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .expect("failed to build UU HTTP client");
        Self {
            creds,
            http,
            system,
        }
    }

    fn headers(&self, method: &str, path: &str, body: &str) -> Result<HeaderMap, String> {
        let mut params = std::collections::BTreeMap::new();
        for (k, v) in [
            (
                "x-param-plat",
                if self.system == UuRemoteSystem::Macos {
                    "4"
                } else {
                    "1"
                },
            ),
            ("x-param-vn", "4.42.0.2770"),
            ("x-param-vc", "2770"),
            ("x-param-pkgn", "com.netease.gameviewer"),
            ("x-param-chn", "gwqd"),
            ("x-param-lang", "zh-CN"),
            ("x-param-cnt", "CN"),
        ] {
            params.insert(k.to_string(), v.to_string());
        }
        // Before login some identifiers do not exist yet; absent ones are neither sent nor signed.
        for (key, value) in [
            ("x-param-client-id", &self.creds.client_id),
            ("x-param-device-id", &self.creds.device_id),
            ("x-param-system-id", &self.creds.system_id),
            ("x-param-user-id", &self.creds.user_id),
        ] {
            if !value.is_empty() {
                params.insert(key.to_string(), value.clone());
            }
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        params.insert("x-param-ts".into(), ts.to_string());
        let signature = sign(self.creds.sign_key(), method, path, &params, body)?;

        let mut headers = HeaderMap::new();
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert(
            "User-Agent",
            HeaderValue::from_static(if self.system == UuRemoteSystem::Macos {
                "UURemote/4.42.0 (Macintosh; Intel Mac OS X)"
            } else {
                "UURemote/4.42.0 (Windows NT 10.0; Win64; x64)"
            }),
        );
        if !self.creds.token.is_empty() {
            let token = &self.creds.token;
            let csrf = &self.creds.csrf;
            for (name, value) in [
                ("Authorization", format!("Bearer {token}")),
                (
                    "Cookie",
                    format!("nrd_access_token={token}; nrd_csrf_access_token={csrf}"),
                ),
                ("X-CSRF-TOKEN", csrf.clone()),
            ] {
                if let Ok(value) = HeaderValue::from_str(&value) {
                    headers.insert(name, value);
                }
            }
        }
        for (k, v) in params {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(&v),
            ) {
                headers.insert(name, value);
            }
        }
        headers.insert(
            "X-Param-SIGN",
            HeaderValue::from_str(&signature).map_err(|e| e.to_string())?,
        );
        Ok(headers)
    }

    /// Signed JSON POST; returns (HTTP status, decoded body).
    pub async fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
        self.request(reqwest::Method::POST, path, body).await
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<(u16, Value), String> {
        let body = serde_json::to_string(body).map_err(|e| e.to_string())?;
        let response = self
            .http
            .request(method.clone(), format!("{BASE_URL}{path}"))
            .headers(self.headers(method.as_str(), path, &body)?)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("UU request {path} failed: {}", e.without_url()))?;
        let status = response.status().as_u16();
        Ok((status, response.json().await.unwrap_or(Value::Null)))
    }

    pub async fn rename_device(&self, name: &str) -> Result<(), String> {
        let path = format!("/api/v1/device/{}", self.creds.device_id);
        let (status, body) = self
            .request(reqwest::Method::PUT, &path, &json!({"alias": name}))
            .await?;
        let data = api_data("device name", status, &body)?;
        if data.get("alias").and_then(Value::as_str) != Some(name) {
            return Err("UU did not confirm the device name".into());
        }
        Ok(())
    }

    pub async fn create_room(&self) -> Result<RoomData, CloudError> {
        let (status, body) = self
            .post("/api/v1/room/create", &json!({}))
            .await
            .map_err(CloudError::Other)?;
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code == TOKEN_INVALID {
            return Err(CloudError::LoginExpired);
        }
        if status == 200 && code == 0 {
            if let Some(data) = body.get("data") {
                return serde_json::from_value(data.clone())
                    .map_err(|e| CloudError::Other(format!("room data decode failed: {e}")));
            }
        }
        Err(CloudError::Other(format!(
            "create room failed: http {status} code {code}"
        )))
    }
}

/// Successful API body `data`, else an error naming the request and API code only.
pub fn api_data<'a>(what: &str, status: u16, body: &'a Value) -> Result<&'a Value, String> {
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if status == 200 && code == 0 {
        return Ok(body.get("data").unwrap_or(&Value::Null));
    }
    Err(format!("{what} failed: http {status} code {code}"))
}

/// csrf claim inside the JWT access token.
pub fn csrf_from_token(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .get("csrf")?
        .as_str()
        .map(str::to_owned)
}

fn read_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().trim_end_matches('\0').to_string())
        .unwrap_or_default()
}

pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// /etc/machine-id as a GUID, the closest Linux analogue of Windows MachineGuid.
fn machine_guid() -> String {
    let id = read_trimmed("/etc/machine-id");
    if id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()) {
        format!(
            "{}-{}-{}-{}-{}",
            &id[..8],
            &id[8..12],
            &id[12..16],
            &id[16..20],
            &id[20..]
        )
    } else {
        random_uuid()
    }
}

/// MAC of the first device-backed interface, uppercase dash form like Windows.
fn mac() -> String {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().join("device").exists())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
        .iter()
        .map(|n| read_trimmed(&format!("/sys/class/net/{n}/address")))
        .find(|m| !m.is_empty() && m != "00:00:00:00:00:00")
        .map(|m| m.to_uppercase().replace(':', "-"))
        .unwrap_or_default()
}

/// Hardware summary for /device/windows/init (the controlled screen is the HDMI input).
pub fn device_summary(creds: &Credentials, screen: (u32, u32), profile: &UuRemoteConfig) -> Value {
    let board = Some(read_trimmed("/proc/device-tree/model"))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "One-KVM".into());
    let cpu = read_trimmed("/proc/cpuinfo")
        .lines()
        .find_map(|l| {
            l.strip_prefix("model name")
                .and_then(|r| r.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| std::env::consts::ARCH.into());
    let memory_mb = read_trimmed("/proc/meminfo")
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024);
    if profile.system == UuRemoteSystem::Macos {
        // Official Mac DeviceInfo uses different keys and a string memory value.
        return json!({
            "name": profile.device_name, "client_id": creds.client_id, "system_id": creds.system_id,
            "system_version": "macOS", "system_name": "macOS", "os": "macOS",
            "cpu": cpu, "memory": format!("{memory_mb} MB"), "model_identifier": board,
            "model": board, "model_number": board, "mac": mac(),
            "resolution": format!("{}x{}", screen.0, screen.1),
            "video": ["One-KVM HDMI capture"], "dpi": 96, "controllable": true,
        });
    }
    json!({
        "name": profile.device_name,
        "client_id": creds.client_id, "system_id": creds.system_id, "machine_guid": creds.machine_guid,
        "os": "Windows", "base_board": board, "cpu": cpu, "video": ["One-KVM HDMI capture"], "mac": mac(),
        "memory": memory_mb, "screen": format!("{}x{}", screen.0, screen.1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_covers_only_x_param_headers_in_sorted_order() {
        let mut params = std::collections::BTreeMap::new();
        params.insert("x-param-ts".to_string(), "1".to_string());
        params.insert("x-param-plat".to_string(), "1".to_string());
        params.insert("x-param-sign".to_string(), "ignored".to_string());
        let signed = sign("key", "post", "/p", &params, "{}").unwrap();
        let expected = hmac_sha256_hex(b"key", b"POST/px-param-plat=1&x-param-ts=1{}").unwrap();
        assert_eq!(signed, expected);
        // RFC 4231 test case 2.
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?").unwrap(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn csrf_is_read_from_jwt_payload() {
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"u","csrf":"c-1"}"#);
        assert_eq!(
            csrf_from_token(&format!("h.{payload}.s")),
            Some("c-1".into())
        );
        assert_eq!(csrf_from_token("bad"), None);
    }

    #[cfg(unix)]
    #[test]
    fn credentials_round_trip_privately_and_logout_keeps_identity() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = credentials_path(dir.path());
        assert!(!load_credentials(&path).unwrap().logged_in());
        let mut creds = Credentials {
            device_id: "d".into(),
            user_id: "u".into(),
            token: "t".into(),
            csrf: "c".into(),
            ..Default::default()
        };
        assert!(creds.ensure_identity());
        assert!(!creds.ensure_identity());
        assert!(creds.logged_in());
        save_credentials(&path, &creds).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut loaded = load_credentials(&path).unwrap();
        assert_eq!(loaded.client_id, creds.client_id);
        assert_eq!(format!("{loaded:?}"), "Credentials { .. }");
        loaded.logout();
        assert!(!loaded.logged_in());
        assert_eq!(loaded.device_id, "d");
    }
}
