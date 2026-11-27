//! Download, verify and install one pinned artifact.
//!
//! The download goes through [`ArtifactSource`], the seam that keeps tests off the network: the
//! product uses [`OfficialReleaseSource`], tests serve fake archives from memory. Whatever the
//! source returns is hashed and compared with the pin BEFORE anything is unpacked, so a wrong
//! file (a compromised mirror, a truncated transfer, a fake with the wrong checksum) never
//! reaches the disk as an install.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::pins::{ArchiveKind, ArtifactPins, PlatformPin};
use super::platform::Platform;
use crate::tool_provisioning::{self, ProvisionError};

/// Where artifact bytes come from.
#[async_trait::async_trait]
pub trait ArtifactSource: Send + Sync {
    /// The complete file at `url`. `Err` carries a reason an operator can act on.
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// The product's source: HTTPS from the project's official release location only.
///
/// Every URL Camerata asks for comes from the pin table, which the tests hold to the official
/// locations. This source checks again at the moment of download, so a future caller that builds
/// a URL some other way is refused rather than trusted ([`is_official_url`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct OfficialReleaseSource;

/// The official release location of every artifact Camerata downloads.
pub fn official_sources() -> impl Iterator<Item = &'static str> {
    super::pins::ALL_ARTIFACTS
        .iter()
        .map(|a| super::pins::pins(*a).official_source)
}

/// True when `url` is under one of [`official_sources`].
pub fn is_official_url(url: &str) -> bool {
    official_sources().any(|prefix| url.starts_with(prefix))
}

#[async_trait::async_trait]
impl ArtifactSource for OfficialReleaseSource {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        if !is_official_url(url) {
            return Err(format!(
                "refusing to download {url}: it is not an official release location"
            ));
        }
        // Both timeouts are bounds, not estimates: the largest artifact (Node) is about 50 MB,
        // and a stalled transfer must end in a disclosed refusal rather than a hung inspection.
        let client = reqwest::Client::builder()
            .user_agent(concat!("camerata/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(15 * 60))
            .build()
            .map_err(|e| format!("could not build the HTTP client: {e}"))?;
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("download of {url} failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "download of {url} returned HTTP {}",
                resp.status().as_u16()
            ));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("download of {url} was cut off: {e}"))
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Refuse `bytes` unless they hash to `expected`.
pub fn verify_checksum(
    tool: &'static str,
    bytes: &[u8],
    expected: &str,
) -> Result<(), ProvisionError> {
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(ProvisionError::ChecksumMismatch {
            tool,
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Name of the file a complete install writes LAST, holding the pin it was verified against.
pub const INSTALL_MARKER: &str = ".camerata-pin";

/// The marker text for `pin`: name, version and checksum, so an install made from another pin
/// (an older release, or another platform's file) never reads as this one.
fn marker_text(artifact: &ArtifactPins, pin: &PlatformPin) -> String {
    format!(
        "{} {} {} {}\n",
        artifact.name,
        artifact.version,
        pin.platform.label(),
        pin.sha256
    )
}

/// `<tooling>/pinned/`: every downloaded artifact's install lives under it.
pub fn pinned_root(tooling: &Path) -> PathBuf {
    tooling.join("pinned")
}

/// The install folder of `artifact` at its pinned version.
pub fn install_dir(tooling: &Path, artifact: &ArtifactPins) -> PathBuf {
    pinned_root(tooling).join(artifact.install_dir_name())
}

/// True when `artifact` is installed for `platform` from exactly its pin: the marker names the
/// pin and every file the pin lists is present. Reads the disk only; starts no process, so the
/// tool list can be computed without side effects.
pub fn is_installed(tooling: &Path, artifact: &ArtifactPins, platform: Platform) -> bool {
    let Some(pin) = artifact.for_platform(platform) else {
        return false;
    };
    let dir = install_dir(tooling, artifact);
    let marker_ok = std::fs::read_to_string(dir.join(INSTALL_MARKER))
        .map(|m| m == marker_text(artifact, pin))
        .unwrap_or(false);
    marker_ok && pin.files.iter().all(|f| dir.join(f).is_file())
}

/// The entry point of an installed artifact (its first listed file).
pub fn entry_point(tooling: &Path, artifact: &ArtifactPins, platform: Platform) -> Option<PathBuf> {
    let pin = artifact.for_platform(platform)?;
    Some(install_dir(tooling, artifact).join(pin.files.first()?))
}

/// Install `artifact` for `platform` from `source` unless it is already installed, and return
/// its install folder.
///
/// Steps: probe; refuse a test process aimed at the real tooling folder; take the artifact's
/// provisioning lock and probe again; download; verify the checksum; unpack into a staging
/// folder; check every listed file is there; write the marker; rename the staging folder into
/// place. A failure at any step leaves no install behind (the staging folder is removed), so the
/// next attempt starts clean and a half-unpacked tree is never run.
pub async fn ensure_artifact(
    tooling: &Path,
    artifact: &'static ArtifactPins,
    platform: Platform,
    source: &dyn ArtifactSource,
) -> Result<PathBuf, ProvisionError> {
    let dir = install_dir(tooling, artifact);
    if is_installed(tooling, artifact, platform) {
        return Ok(dir);
    }
    let pin = artifact
        .for_platform(platform)
        .ok_or(ProvisionError::NoArtifactForPlatform {
            tool: artifact.name,
            platform: platform.label(),
        })?;
    tool_provisioning::refuse_real_tooling_in_test_process(tooling, artifact.name)?;
    let _lock = tool_provisioning::acquire_provision_lock(
        tooling,
        artifact.name,
        tool_provisioning::PROVISION_LOCK_TIMEOUT,
    )
    .await?;
    if is_installed(tooling, artifact, platform) {
        return Ok(dir);
    }

    let bytes = source
        .fetch(pin.url)
        .await
        .map_err(|detail| ProvisionError::DownloadFailed {
            tool: artifact.name,
            detail,
        })?;
    verify_checksum(artifact.name, &bytes, pin.sha256)?;

    let root = pinned_root(tooling);
    tokio::fs::create_dir_all(&root).await?;
    let staging = root.join(format!(
        ".staging-{}-{:x}",
        artifact.install_dir_name(),
        std::process::id()
    ));
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let built = build_install(&staging, artifact, pin, bytes).await;
    let published = match built {
        Ok(()) => publish(&staging, &dir).await,
        Err(e) => Err(e),
    };
    if published.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    published?;
    Ok(dir)
}

/// Unpack (or write) the verified bytes into `staging`, check the listed files, write the marker.
async fn build_install(
    staging: &Path,
    artifact: &'static ArtifactPins,
    pin: &'static PlatformPin,
    bytes: Vec<u8>,
) -> Result<(), ProvisionError> {
    tokio::fs::create_dir_all(staging).await?;
    match artifact.kind {
        ArchiveKind::TarGz => {
            let dest = staging.to_path_buf();
            tokio::task::spawn_blocking(move || unpack_tar_gz(&bytes, &dest))
                .await
                .map_err(|e| ProvisionError::InstallFailed {
                    tool: artifact.name,
                    detail: format!("unpacking task failed: {e}"),
                })?
                .map_err(|e| ProvisionError::InstallFailed {
                    tool: artifact.name,
                    detail: format!("could not unpack the verified archive: {e}"),
                })?;
        }
        ArchiveKind::Binary => {
            let name = pin.files.first().copied().unwrap_or(artifact.name);
            let dest = staging.join(name);
            tokio::fs::write(&dest, &bytes).await?;
            mark_executable(&dest).await?;
        }
    }
    for f in pin.files {
        if !staging.join(f).is_file() {
            return Err(ProvisionError::InstallFailed {
                tool: artifact.name,
                detail: format!("the verified archive has no {f}"),
            });
        }
    }
    tokio::fs::write(staging.join(INSTALL_MARKER), marker_text(artifact, pin)).await?;
    Ok(())
}

/// Unpack a gzip tar archive into `dest`. `tar`'s `unpack` refuses entries that would land
/// outside `dest` (absolute paths, `..`), and keeps the archive's modes so binaries stay
/// executable.
fn unpack_tar_gz(bytes: &[u8], dest: &Path) -> std::io::Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(true);
    archive.set_overwrite(true);
    archive.unpack(dest)
}

#[cfg(unix)]
async fn mark_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await
}

#[cfg(not(unix))]
async fn mark_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Move the finished staging folder to `dir`. A stale `dir` (an install from a different pin of
/// the same version folder, or a broken one) is moved aside first and removed after.
async fn publish(staging: &Path, dir: &Path) -> Result<(), ProvisionError> {
    let aside = dir.with_extension(format!("old-{:x}", std::process::id()));
    let had_old = tokio::fs::symlink_metadata(dir).await.is_ok();
    if had_old {
        tokio::fs::rename(dir, &aside).await?;
    }
    if let Err(e) = tokio::fs::rename(staging, dir).await {
        if had_old {
            let _ = tokio::fs::rename(&aside, dir).await;
        }
        return Err(ProvisionError::Io(e));
    }
    if had_old {
        let _ = tokio::fs::remove_dir_all(&aside).await;
    }
    Ok(())
}
