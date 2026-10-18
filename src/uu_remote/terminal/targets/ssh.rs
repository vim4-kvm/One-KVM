use crate::config::UuTerminalSshConfig;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use typeshare::typeshare;

#[typeshare]
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SshTestResult {
    Ok,
    PasswordRequired,
    UnknownHost,
    HostKeyChanged,
    Refused,
    Timeout,
    Dns,
    Unreachable,
    NotConfigured,
    Error,
}
#[typeshare]
#[derive(Debug, Serialize)]
pub struct SshTestResponse {
    pub result: SshTestResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
impl SshTestResponse {
    fn new(result: SshTestResult) -> Self {
        Self {
            result,
            fingerprint: None,
            key_type: None,
            message: None,
        }
    }
    fn error(message: &str) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::new(SshTestResult::Error)
        }
    }
}
pub struct Ssh {
    pub directory: PathBuf,
    // Test, trust, removal and initial key generation serialize writes to the private store.
    lock: tokio::sync::Mutex<()>,
}
impl Ssh {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            lock: tokio::sync::Mutex::new(()),
        }
    }
    async fn prepare_directory(&self) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.directory)
            .await
            .map_err(|_| "无法创建 SSH 密钥目录")?;
        if !tokio::fs::symlink_metadata(&self.directory)
            .await
            .map_err(|_| "无法读取 SSH 密钥目录")?
            .is_dir()
        {
            return Err("SSH 密钥目录无效".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| "无法设置 SSH 密钥目录权限")?;
        }
        for name in ["id_ed25519", "id_ed25519.pub", "known_hosts"] {
            let path = self.directory.join(name);
            if let Ok(meta) = tokio::fs::symlink_metadata(&path).await {
                if !meta.is_file() {
                    return Err("SSH 密钥文件无效".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    if meta.nlink() != 1 {
                        return Err("SSH 密钥文件无效".into());
                    }
                    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                        .await
                        .map_err(|_| "无法设置 SSH 密钥权限")?;
                }
            }
        }
        Ok(())
    }
    pub async fn ensure_key(&self, name: &str) -> Result<(), String> {
        let _guard = self.lock.lock().await;
        self.prepare_directory().await?;
        let key = self.directory.join("id_ed25519");
        if !key.exists() {
            let output = run(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C"])
                    .arg(format!("one-kvm@{name}"))
                    .arg("-f")
                    .arg(&key),
                Duration::from_secs(10),
            )
            .await
            .map_err(|_| "生成 SSH 密钥失败")?;
            if !output.status.success() {
                return Err("生成 SSH 密钥失败".into());
            }
        }
        self.prepare_directory().await?;
        if !self.directory.join("known_hosts").exists() {
            self.write_known("").await?;
        }
        Ok(())
    }
    pub async fn public_key(&self) -> Option<String> {
        let path = self.directory.join("id_ed25519.pub");
        let metadata = tokio::fs::symlink_metadata(&path).await.ok()?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return None;
        }
        let text = tokio::fs::read_to_string(path).await.ok()?;
        let key = text.trim();
        // Even a misconfigured file must never turn the status API into a private-key reader.
        (key.starts_with("ssh-ed25519 ") && !key.contains(['\n', '\r'])).then(|| key.to_owned())
    }
    pub fn args(
        &self,
        config: &UuTerminalSshConfig,
        interactive: bool,
    ) -> Result<Vec<String>, String> {
        config.validate()?;
        if !config.configured() {
            return Err("未设置地址".into());
        }
        let mut args = vec![];
        if interactive {
            args.push("-tt".into());
        }
        args.extend([
            "-F".into(),
            "/dev/null".into(),
            "-i".into(),
            self.directory
                .join("id_ed25519")
                .to_string_lossy()
                .into_owned(),
        ]);
        for option in [
            "IdentitiesOnly=yes".into(),
            "IdentityAgent=none".into(),
            format!(
                "UserKnownHostsFile={}",
                self.directory.join("known_hosts").display()
            ),
            "GlobalKnownHostsFile=/dev/null".into(),
            format!(
                "StrictHostKeyChecking={}",
                if interactive { "ask" } else { "yes" }
            ),
            "ConnectTimeout=10".into(),
            "ServerAliveInterval=15".into(),
            "ServerAliveCountMax=3".into(),
        ] {
            args.extend(["-o".into(), option]);
        }
        if !interactive {
            args.extend(["-o".into(), "BatchMode=yes".into()]);
        }
        args.extend([
            "-p".into(),
            config.port.to_string(),
            "-l".into(),
            config.user.clone(),
            "--".into(),
            config.host.clone(),
        ]);
        if !interactive {
            args.push("true".into());
        }
        Ok(args)
    }
    async fn scan(&self, config: &UuTerminalSshConfig) -> Result<Vec<HostKey>, SshTestResponse> {
        if !config.configured() {
            return Err(SshTestResponse::new(SshTestResult::NotConfigured));
        }
        config
            .validate()
            .map_err(|_| SshTestResponse::error("SSH 配置无效"))?;
        let output = run(
            Command::new("ssh-keyscan")
                .args(["-T", "4", "-p"])
                .arg(config.port.to_string())
                .args(["--", &config.host]),
            Duration::from_secs(6),
        )
        .await
        .map_err(|e| match e {
            RunError::Timeout => SshTestResponse::new(SshTestResult::Timeout),
            RunError::Io => SshTestResponse::error("无法执行 SSH 主机扫描"),
        })?;
        let lines = String::from_utf8_lossy(&output.stdout);
        let mut keys = Vec::new();
        for line in lines.lines().filter(|l| !l.starts_with('#')) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() != 3 {
                continue;
            }
            let public = format!("{} {}\n", fields[1], fields[2]);
            let file = private_file(&self.directory, &public)
                .await
                .map_err(|_| SshTestResponse::error("无法检查主机指纹"))?;
            let result = run(
                Command::new("ssh-keygen")
                    .args(["-l", "-E", "sha256", "-f"])
                    .arg(file.path()),
                Duration::from_secs(3),
            )
            .await;
            let Ok(result) = result else {
                continue;
            };
            let value = String::from_utf8_lossy(&result.stdout);
            if let Some(fingerprint) = value
                .split_whitespace()
                .nth(1)
                .filter(|f| f.starts_with("SHA256:"))
            {
                keys.push(HostKey {
                    line: line.into(),
                    kind: fields[1].into(),
                    blob: fields[2].into(),
                    fingerprint: fingerprint.into(),
                });
            }
        }
        keys.sort_by_key(|key| if key.kind == "ssh-ed25519" { 0 } else { 1 });
        if keys.is_empty() {
            // keyscan often exits silently on ECONNREFUSED. Probe transport only
            // after a failed scan so the API can distinguish its documented errors.
            let classified = classify(&output.stderr);
            Err(if classified.result == SshTestResult::Error {
                probe_failure(config).await
            } else {
                classified
            })
        } else {
            Ok(keys)
        }
    }
    async fn known(&self, config: &UuTerminalSshConfig) -> Result<Vec<(String, String)>, String> {
        if !self.directory.join("known_hosts").exists() {
            return Ok(Vec::new());
        }
        let output = run(
            Command::new("ssh-keygen")
                .arg("-F")
                .arg(host_pattern(config))
                .arg("-f")
                .arg(self.directory.join("known_hosts")),
            Duration::from_secs(3),
        )
        .await
        .map_err(|_| "无法读取主机信任记录")?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|s| !s.starts_with('#'))
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() >= 3 {
                    Some((fields[1].into(), fields[2].into()))
                } else {
                    None
                }
            })
            .collect())
    }
    pub async fn test(&self, config: &UuTerminalSshConfig) -> SshTestResponse {
        let _guard = self.lock.lock().await;
        if let Err(e) = self.prepare_directory().await {
            return SshTestResponse::error(&e);
        }
        let keys = match self.scan(config).await {
            Ok(k) => k,
            Err(e) => return e,
        };
        let known = match self.known(config).await {
            Ok(k) => k,
            Err(e) => return SshTestResponse::error(&e),
        };
        if known.is_empty() {
            let key = &keys[0];
            return SshTestResponse {
                fingerprint: Some(key.fingerprint.clone()),
                key_type: Some(
                    key.kind
                        .strip_prefix("ssh-")
                        .unwrap_or(&key.kind)
                        .to_uppercase(),
                ),
                ..SshTestResponse::new(SshTestResult::UnknownHost)
            };
        }
        if !keys
            .iter()
            .any(|k| known.contains(&(k.kind.clone(), k.blob.clone())))
        {
            return SshTestResponse::new(SshTestResult::HostKeyChanged);
        }
        let args = match self.args(config, false) {
            Ok(a) => a,
            Err(_) => return SshTestResponse::new(SshTestResult::NotConfigured),
        };
        match run(Command::new("ssh").args(args), Duration::from_secs(8)).await {
            Ok(output) if output.status.success() => SshTestResponse::new(SshTestResult::Ok),
            Ok(output) => classify(&output.stderr),
            Err(RunError::Timeout) => SshTestResponse::new(SshTestResult::Timeout),
            Err(RunError::Io) => SshTestResponse::error("无法执行 SSH 测试"),
        }
    }
    pub async fn trust(
        &self,
        config: &UuTerminalSshConfig,
        fingerprint: &str,
    ) -> Result<bool, String> {
        let _guard = self.lock.lock().await;
        self.prepare_directory().await?;
        let keys = self
            .scan(config)
            .await
            .map_err(|_| "无法重新扫描主机指纹")?;
        if !keys.iter().any(|k| k.fingerprint == fingerprint) {
            return Ok(false);
        }
        // Trust only the fingerprint the user reviewed, never other unreviewed scan keys.
        let key = keys.iter().find(|k| k.fingerprint == fingerprint).unwrap();
        self.remove_inner(config).await?;
        let mut known = tokio::fs::read_to_string(self.directory.join("known_hosts"))
            .await
            .unwrap_or_default();
        known.push_str(&key.line);
        known.push('\n');
        self.write_known(&known).await?;
        Ok(true)
    }
    pub async fn remove(&self, config: &UuTerminalSshConfig) -> Result<(), String> {
        let _guard = self.lock.lock().await;
        self.prepare_directory().await?;
        self.remove_inner(config).await
    }
    async fn remove_inner(&self, config: &UuTerminalSshConfig) -> Result<(), String> {
        config.validate()?;
        if !config.configured() {
            return Ok(());
        }
        let path = self.directory.join("known_hosts");
        if !path.exists() {
            return self.write_known("").await;
        }
        let result = run(
            Command::new("ssh-keygen")
                .arg("-R")
                .arg(host_pattern(config))
                .arg("-f")
                .arg(&path),
            Duration::from_secs(3),
        )
        .await
        .map_err(|_| "无法移除主机信任记录")?;
        let _ = tokio::fs::remove_file(self.directory.join("known_hosts.old")).await;
        if !result.status.success() {
            return Err("无法移除主机信任记录".into());
        }
        self.prepare_directory().await
    }
    async fn write_known(&self, text: &str) -> Result<(), String> {
        let temporary = private_file(&self.directory, text).await?;
        temporary
            .persist(self.directory.join("known_hosts"))
            .map(|_| ())
            .map_err(|_| "无法保存主机信任记录".into())
    }
}
fn host_pattern(c: &UuTerminalSshConfig) -> String {
    if c.port == 22 {
        c.host.clone()
    } else {
        format!("[{}]:{}", c.host, c.port)
    }
}
async fn private_file(directory: &Path, text: &str) -> Result<tempfile::NamedTempFile, String> {
    use tokio::io::AsyncWriteExt;
    let temporary =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| "无法创建 SSH 私有文件")?;
    let mut file =
        tokio::fs::File::from_std(temporary.reopen().map_err(|_| "无法打开 SSH 私有文件")?);
    file.write_all(text.as_bytes())
        .await
        .map_err(|_| "无法写入 SSH 私有文件")?;
    file.flush().await.map_err(|_| "无法写入 SSH 私有文件")?;
    Ok(temporary)
}
async fn probe_failure(config: &UuTerminalSshConfig) -> SshTestResponse {
    let addresses = match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((config.host.as_str(), config.port)),
    )
    .await
    {
        Err(_) => return SshTestResponse::new(SshTestResult::Timeout),
        Ok(Err(_)) => return SshTestResponse::new(SshTestResult::Dns),
        Ok(Ok(addresses)) => addresses.collect::<Vec<_>>(),
    };
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect(addresses.as_slice()),
    )
    .await
    {
        Err(_) => SshTestResponse::new(SshTestResult::Timeout),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            SshTestResponse::new(SshTestResult::Refused)
        }
        Ok(Err(_)) => SshTestResponse::new(SshTestResult::Unreachable),
        Ok(Ok(_)) => SshTestResponse::error("主机未提供可用的 SSH 密钥"),
    }
}
enum RunError {
    Timeout,
    Io,
}
async fn run(command: &mut Command, timeout: Duration) -> Result<std::process::Output, RunError> {
    command
        .kill_on_drop(true)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| RunError::Timeout)?
        .map_err(|_| RunError::Io)
}
struct HostKey {
    line: String,
    kind: String,
    blob: String,
    fingerprint: String,
}
fn classify(stderr: &[u8]) -> SshTestResponse {
    let s = String::from_utf8_lossy(stderr).to_lowercase();
    let result = if s.contains("remote host identification has changed") {
        SshTestResult::HostKeyChanged
    } else if s.contains("permission denied") {
        SshTestResult::PasswordRequired
    } else if s.contains("connection refused") {
        SshTestResult::Refused
    } else if s.contains("timed out") || s.contains("timeout") {
        SshTestResult::Timeout
    } else if s.contains("resolve")
        || s.contains("name or service not known")
        || s.contains("getaddrinfo")
    {
        SshTestResult::Dns
    } else if s.contains("no route") || s.contains("unreachable") {
        SshTestResult::Unreachable
    } else {
        return SshTestResponse::error("SSH 连接失败");
    };
    SshTestResponse::new(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_is_after_options_and_user_cannot_inject_options() {
        let ssh = Ssh::new(PathBuf::from("/private/terminal"));
        let mut c = UuTerminalSshConfig {
            host: "host".into(),
            user: "user".into(),
            ..Default::default()
        };
        let args = ssh.args(&c, true).unwrap();
        assert_eq!(&args[args.len() - 2..], ["--", "host"]);
        assert!(args.iter().any(|s| s == "IdentityAgent=none"));
        c.host = "-V".into();
        assert!(ssh.args(&c, true).is_err());
        c.host = "host".into();
        c.user = "-V".into();
        assert!(ssh.args(&c, true).is_err());
    }

    #[tokio::test]
    async fn status_never_returns_private_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = Ssh::new(dir.path().into());
        std::fs::write(
            dir.path().join("id_ed25519.pub"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nprivate\n",
        )
        .unwrap();
        assert!(ssh.public_key().await.is_none());
        std::fs::write(
            dir.path().join("id_ed25519.pub"),
            "ssh-ed25519 public\nprivate",
        )
        .unwrap();
        assert!(ssh.public_key().await.is_none());
    }

    #[tokio::test]
    #[ignore = "requires openssh-client, /usr/sbin/sshd and /run/sshd; run in an isolated Linux builder"]
    async fn local_server_checks_trust_key_change_login_and_removal() {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncBufReadExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let ssh = Ssh::new(dir.path().join("terminal"));
        ssh.ensure_key("test").await.unwrap();
        let public = ssh.public_key().await.unwrap();
        ssh.ensure_key("test").await.unwrap();
        assert_eq!(ssh.public_key().await.unwrap(), public);
        assert_eq!(
            std::fs::metadata(&ssh.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(ssh.directory.join("id_ed25519"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let host = dir.path().join("host");
        assert!(run(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&host),
            Duration::from_secs(5)
        )
        .await
        .unwrap_or_else(|_| panic!("keygen failed"))
        .status
        .success());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let authorized = dir.path().join("authorized_keys");
        std::fs::write(&authorized, &public).unwrap();
        let config_file = dir.path().join("sshd_config");
        std::fs::write(&config_file,format!("Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nStrictModes no\nPermitRootLogin yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nLogLevel VERBOSE\n",host.display(),dir.path().join("pid").display(),authorized.display())).unwrap();
        let mut server = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(config_file)
            .kill_on_drop(true)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(server.stderr.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line.contains("Server listening") {
                    return;
                }
            }
            panic!("sshd failed to listen");
        })
        .await
        .unwrap();
        let drain =
            tokio::spawn(async move { while lines.next_line().await.ok().flatten().is_some() {} });
        let config = UuTerminalSshConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port,
            user: std::env::var("USER").unwrap_or_else(|_| "root".into()),
        };
        let result = ssh.test(&config).await;
        assert_eq!(result.result, SshTestResult::UnknownHost);
        let fingerprint = result.fingerprint.unwrap();
        assert!(!ssh.trust(&config, "SHA256:wrong").await.unwrap());
        assert!(ssh.trust(&config, &fingerprint).await.unwrap());
        assert_eq!(ssh.test(&config).await.result, SshTestResult::Ok);
        std::fs::remove_file(&authorized).unwrap();
        assert_eq!(
            ssh.test(&config).await.result,
            SshTestResult::PasswordRequired
        );
        // An unrelated valid key under this host must be reported as changed.
        let parts: Vec<_> = public.split_whitespace().collect();
        ssh.write_known(&format!(
            "{} {} {}\n",
            host_pattern(&config),
            parts[0],
            parts[1]
        ))
        .await
        .unwrap();
        assert_eq!(
            ssh.test(&config).await.result,
            SshTestResult::HostKeyChanged
        );
        ssh.remove(&config).await.unwrap();
        assert_eq!(ssh.test(&config).await.result, SshTestResult::UnknownHost);
        server.kill().await.unwrap();
        server.wait().await.unwrap();
        drain.await.unwrap();
        assert_eq!(ssh.test(&config).await.result, SshTestResult::Refused);
        assert!(matches!(
            run(Command::new("sleep").arg("30"), Duration::from_millis(20)).await,
            Err(RunError::Timeout)
        ));
    }
}
