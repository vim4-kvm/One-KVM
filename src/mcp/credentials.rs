//! Private, atomic credential persistence; a corrupt file never regenerates itself.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileContents {
    api_key: String,
}
pub struct Credentials {
    directory: PathBuf,
    key: Option<String>,
    pub error: Option<String>,
}
impl Credentials {
    pub fn load(data: &Path) -> Self {
        let directory = data.join("mcp");
        let result = fs::read(directory.join("credentials.json"));
        let (key, error) = match result {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(_) => (None, Some("Cannot read MCP credentials.".into())),
            Ok(bytes) => match serde_json::from_slice::<FileContents>(&bytes) {
                Ok(c) if valid_key(&c.api_key) => (Some(c.api_key), None),
                _ => (
                    None,
                    Some("MCP credentials are corrupt. Regenerate the API key.".into()),
                ),
            },
        };
        Self {
            directory,
            key,
            error,
        }
    }
    pub fn accepts(&self, candidate: &str) -> bool {
        self.key.as_ref().is_some_and(|key| {
            key.len() == candidate.len()
                && openssl::memcmp::eq(key.as_bytes(), candidate.as_bytes())
        })
    }
    pub fn preview(&self) -> Option<String> {
        self.key
            .as_ref()
            .map(|key| format!("{}…{}", &key[..13], &key[key.len() - 4..]))
    }
    pub fn ensure(&mut self) -> Result<String, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if let Some(key) = &self.key {
            return Ok(key.clone());
        }
        self.regenerate()
    }
    pub fn regenerate(&mut self) -> Result<String, String> {
        let result = self.persist();
        match result {
            Ok(key) => {
                self.key = Some(key.clone());
                self.error = None;
                Ok(key)
            }
            Err(_) => {
                self.error = Some("Cannot save MCP credentials.".into());
                Err(self.error.clone().unwrap())
            }
        }
    }
    fn persist(&self) -> std::io::Result<String> {
        fs::create_dir_all(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
        }
        let mut random = [0u8; 32];
        openssl::rand::rand_bytes(&mut random).map_err(std::io::Error::other)?;
        let key = format!("okvm_mcp_{}", URL_SAFE_NO_PAD.encode(random));
        let mut temp = tempfile::NamedTempFile::new_in(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temp.write_all(&serde_json::to_vec(&FileContents {
            api_key: key.clone(),
        })?)?;
        temp.as_file().sync_all()?;
        temp.persist(self.directory.join("credentials.json"))
            .map_err(|e| e.error)?;
        File::open(&self.directory)?.sync_all()?;
        Ok(key)
    }
}
fn valid_key(key: &str) -> bool {
    key.strip_prefix("okvm_mcp_")
        .and_then(|v| URL_SAFE_NO_PAD.decode(v).ok())
        .is_some_and(|b| b.len() == 32)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_rotates_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Credentials::load(dir.path());
        let first = c.ensure().unwrap();
        assert!(Credentials::load(dir.path()).accepts(&first));
        assert!(!c.accepts("short"));
        let second = c.regenerate().unwrap();
        assert!(!c.accepts(&first));
        assert!(c.accepts(&second));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("mcp"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(dir.path().join("mcp/credentials.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::write(dir.path().join("mcp/credentials.json"), "bad").unwrap();
        let mut c = Credentials::load(dir.path());
        assert!(c.ensure().is_err());
        assert!(!c.accepts(&second));
        assert!(c.regenerate().is_ok());
    }
}
