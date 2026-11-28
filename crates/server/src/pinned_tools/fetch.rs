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

    /// True when this source downloads nothing ([`OfflineSource`]): a missing tool is then a
    /// refusal before any install step runs (npm and pip would otherwise reach their registries).
    fn offline(&self) -> bool {
        false
    }
}

/// The source when Camerata is offline ([`super::OFFLINE_ENV`]): it never downloads.
#[derive(Debug, Default, Clone, Copy)]
pub struct OfflineSource;

#[async_trait::async_trait]
impl ArtifactSource for OfflineSource {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        Err(format!(
            "Camerata is offline ({}=1), so {url} was not downloaded",
            super::OFFLINE_ENV
        ))
    }

    fn offline(&self) -> bool {
        true
    }
}

/// The product's source: HTTPS from the project's official release location only.
///
/// Every URL Camerata asks for comes from the pin table, which the tests hold to the official
/// locations. This source checks again at the moment of download, so a future caller that builds
/// a URL some other way is refused rather than trusted ([`is_official_url`]).
#[derive(Debug, Default, Clone)]
pub struct OfficialReleaseSource {
    /// Extra trusted root certificates (PEM), on top of the bundled web roots. Only the
    /// on-demand first-run harness sets it, for a machine whose network re-signs TLS through a
    /// proxy; the product never does.
    extra_roots_pem: Option<Vec<u8>>,
    /// Set only by [`OfficialReleaseSource::for_the_first_run_harness`]: the one test allowed to
    /// download.
    harness: bool,
}

impl OfficialReleaseSource {
    /// The source the on-demand first-run harness uses: the only one a cargo test binary may
    /// download through. `pem` (one or more PEM certificates) is also trusted, for a network that
    /// re-signs TLS through a proxy.
    pub fn for_the_first_run_harness(pem: Option<Vec<u8>>) -> Self {
        Self {
            extra_roots_pem: pem,
            harness: true,
        }
    }
}

/// True when this process is a cargo test binary: it may not download (no test reaches the
/// internet; a test hands in a fixture source).
fn in_a_test_process() -> bool {
    std::env::current_exe()
        .ok()
        .is_some_and(|exe| crate::tool_provisioning::is_cargo_test_binary(&exe))
}

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
        if !self.harness && in_a_test_process() {
            return Err(format!(
                "refusing to download {url} from a test process: a test serves its tools from a \
                 fixture source"
            ));
        }
        let client = download_client(self.extra_roots_pem.as_deref())?;
        let mut resp = client
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
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_DOWNLOAD_BYTES as u64)
        {
            return Err(format!(
                "download of {url} is larger than the {MAX_DOWNLOAD_BYTES} bytes Camerata accepts"
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("download of {url} was cut off: {e}"))?
        {
            take_chunk(&mut bytes, &chunk, MAX_DOWNLOAD_BYTES)
                .map_err(|e| format!("download of {url}: {e}"))?;
        }
        Ok(bytes)
    }
}

/// The largest file Camerata downloads: a bound well above the largest pin (Node.js, about
/// 50 MB), so a wrong or hostile response cannot fill the disk.
pub(crate) const MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;

/// The HTTP client every download uses: HTTPS only, redirects included (a release host redirects
/// to its CDN; a redirect to plain http is refused), at most five redirects, and both timeouts as
/// bounds (the largest pin is about 50 MB; a stalled transfer ends in a refusal, never a hang).
pub(crate) fn download_client(extra_roots_pem: Option<&[u8]>) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("camerata/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(15 * 60));
    if let Some(pem) = extra_roots_pem {
        let roots = reqwest::Certificate::from_pem_bundle(pem)
            .map_err(|e| format!("could not read the extra root certificates: {e}"))?;
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
    }
    builder
        .build()
        .map_err(|e| format!("could not build the HTTP client: {e}"))
}

/// Append one received chunk to `buf`, refusing once the total would pass `cap`.
pub(crate) fn take_chunk(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> Result<(), String> {
    if buf.len() + chunk.len() > cap {
        return Err(format!(
            "the download is larger than the {cap} bytes Camerata accepts; it was stopped"
        ));
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// Where an older install of the same folder is moved while the new one is published: a hidden
/// sibling that keeps the whole install name (a version's dots are not an extension).
pub(crate) fn aside_path(dir: &Path) -> PathBuf {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.with_file_name(format!(".{name}.old-{:x}", std::process::id()))
}

/// Remove every staging folder an earlier, interrupted install of `artifact` left. Called under the
/// artifact's lock, so no live install owns one; another artifact's folders are left alone.
async fn sweep_stale_staging(root: &Path, artifact: &ArtifactPins) {
    let prefix = format!(".staging-{}-", artifact.install_dir_name());
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
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
pub(crate) fn marker_text(artifact: &ArtifactPins, pin: &PlatformPin) -> String {
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
    if source.offline() {
        return Err(offline(artifact.name, tooling));
    }
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
    // We hold the lock, so a staging folder still here belongs to a dead install.
    sweep_stale_staging(&root, artifact).await;
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

/// The refusal for `tool` missing from `tooling` while Camerata is offline.
pub fn offline(tool: &'static str, tooling: &Path) -> ProvisionError {
    ProvisionError::Offline {
        tool,
        folder: tooling.display().to_string(),
        env: super::OFFLINE_ENV,
    }
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
    let aside = aside_path(dir);
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
