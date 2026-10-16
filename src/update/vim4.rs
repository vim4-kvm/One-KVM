use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};

// This channel accepts only our stamped Ubuntu versions, never generic firmware.
pub(super) fn version_parts(input: &str) -> Result<Vec<u64>> {
    let invalid = || AppError::BadRequest("Expected x.y.z+vim4.YYYYMMDD.N package version".into());
    let (base, revision) = input.split_once("+vim4.").ok_or_else(invalid)?;
    // Installed development packages sit between their base and the next release.
    // Commit and dirty markers describe sources; the allocated sequence orders builds.
    let (revision, development) = match revision.split_once(".dev.") {
        Some((revision, suffix)) => {
            let suffix = suffix.strip_suffix(".dirty").unwrap_or(suffix);
            let (sequence, commit) = suffix.split_once(".g").ok_or_else(invalid)?;
            if sequence.is_empty()
                || !sequence.bytes().all(|value| value.is_ascii_digit())
                || !(7..=40).contains(&commit.len())
                || !commit
                    .bytes()
                    .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
            {
                return Err(invalid());
            }
            (
                revision,
                Some(sequence.parse::<u64>().map_err(|_| invalid())?),
            )
        }
        None => (revision, None),
    };
    let base: Vec<_> = base.split('.').collect();
    let revision: Vec<_> = revision.split('.').collect();
    if base.len() != 3 || revision.len() != 2 || revision[0].len() != 8 {
        return Err(invalid());
    }
    let mut parts: Vec<u64> = base
        .into_iter()
        .chain(revision)
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|value| value.is_ascii_digit()) {
                return Err(invalid());
            }
            part.parse().map_err(|_| invalid())
        })
        .collect::<Result<_>>()?;
    if let Some(sequence) = development {
        parts.push(sequence);
    }
    Ok(parts)
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

    #[test]
    fn development_packages_update_to_the_next_release() {
        let release = "2.5.1+vim4.20261016.1020";
        let dev = "2.5.1+vim4.20261016.1020.dev.9.g49c45f2.dirty";
        assert!(version_parts(dev).unwrap() > version_parts(release).unwrap());
        assert!(
            version_parts("2.5.1+vim4.20261016.1020.dev.10.gabcdef0").unwrap()
                > version_parts(dev).unwrap()
        );
        assert!(version_parts("2.5.1+vim4.20261016.1022").unwrap() > version_parts(dev).unwrap());
        assert!(version_parts("2.5.1+vim4.20261017.1").unwrap() > version_parts(dev).unwrap());
        for bad in [
            ".dev.1",
            ".dev.x.gabcdef0",
            ".dev.1.gABCDEF0",
            ".dev.1.gabcdef0.extra",
        ] {
            assert!(version_parts(&format!("{release}{bad}")).is_err());
        }
    }
}
