use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};

// This channel accepts only our stamped Ubuntu versions, never generic firmware.
pub(super) fn version_parts(input: &str) -> Result<Vec<u64>> {
    let invalid = || AppError::BadRequest("Expected x.y.z+vim4.YYYYMMDD.N package version".into());
    let (base, revision) = input.split_once("+vim4.").ok_or_else(invalid)?;
    let base: Vec<_> = base.split('.').collect();
    let revision: Vec<_> = revision.split('.').collect();
    if base.len() != 3 || revision.len() != 2 || revision[0].len() != 8 {
        return Err(invalid());
    }
    base.into_iter()
        .chain(revision)
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|value| value.is_ascii_digit()) {
                return Err(invalid());
            }
            part.parse().map_err(|_| invalid())
        })
        .collect()
}

pub(super) async fn install(path: &Path, checksum: &str, version: &str) -> Result<PathBuf> {
    let checksum = super::normalize_sha256(checksum)
        .ok_or_else(|| AppError::BadRequest("Invalid package SHA256".into()))?;
    let output = tokio::process::Command::new("sudo")
        .args(["-n", "/usr/libexec/vim4-onekvm-update"])
        .arg(path)
        .arg(checksum)
        .arg(version)
        .output()
        .await?;
    if !output.status.success() {
        return Err(AppError::Internal(format!(
            "Ubuntu package installation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(PathBuf::from("/opt/vim4-onekvm/one-kvm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_package_revisions_and_rejects_generic_firmware() {
        assert!(
            version_parts("2.5.1+vim4.20261014.10").unwrap()
                > version_parts("2.5.1+vim4.20261014.9").unwrap()
        );
        assert!(
            version_parts("2.5.1+vim4.20261015.1").unwrap()
                > version_parts("2.5.1+vim4.20261014.10").unwrap()
        );
        for bad in [
            "0.2.8",
            "v0.2.8",
            "2.5.1+vim4.20261014.x",
            "2.5.1+vim4.123.1",
        ] {
            assert!(version_parts(bad).is_err(), "{bad}");
        }
    }
}
