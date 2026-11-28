//! Camerata provisions every scan tool and every runtime a tool needs itself, at pinned
//! versions, into its own tooling folder. Nothing is taken from the user's `PATH` or global
//! installs, and nothing is installed onto them (#587).
//!
//! # What is provisioned
//!
//! | Tool        | Runs on                         | Downloaded from                                 |
//! |-------------|---------------------------------|-------------------------------------------------|
//! | ruff        | itself (static binary)          | github.com/astral-sh/ruff releases              |
//! | eslint      | the pinned Node.js              | nodejs.org/dist, then registry.npmjs.org (lock) |
//! | semgrep     | the pinned CPython (a venv)     | python-build-standalone releases, then PyPI     |
//! | osv-scanner | itself (static binary)          | github.com/google/osv-scanner releases          |
//!
//! Each downloaded file is checked against a SHA-256 pinned in [`pins`] before it is unpacked.
//! The eslint packages are pinned by the committed `package-lock.json` (npm checks every
//! package's integrity hash), and semgrep with every dependency by the committed hash-locked
//! `assets/semgrep/requirements.txt` (pip `--require-hashes`).
//!
//! # When
//!
//! The tool set an inspection needs is decided from the stacks it detects and the rules it
//! selected ([`tool_set`]), reported by `camerata inspect --plan-only` and the pre-flight, and
//! provisioned when the inspection is triggered, before the first model call
//! ([`provision_tool_set`]). A client inspection whose tool set cannot be provisioned is refused
//! before anything is spent. A tool that cannot be provisioned later, mid-run, stops the run
//! ([`crate::scan_tools::ToolProvisioningStop`]); it is never a coverage note.
//!
//! # The child environment
//!
//! Every provisioned tool is started by its absolute path, with `PATH` set to
//! [`ToolEnv::child_path`]: Camerata's own runtime folders first, then the operating system's
//! base folders (`/usr/bin:/bin:/usr/sbin:/sbin`, which hold OS utilities such as `uname` that
//! semgrep's engine calls). The user's `PATH` is never passed on.

pub mod fetch;
pub mod pins;
pub mod platform;
mod semgrep;
mod tool_set;

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use fetch::{ArtifactSource, OfficialReleaseSource, OfflineSource};
pub use pins::Artifact;
pub use platform::Platform;
pub use semgrep::{ensure_semgrep, semgrep_bin, semgrep_is_installed, SEMGREP_REQUIREMENTS};
pub use tool_set::{
    provision_tool_set, required_tool_set, tool_set_report, Component, ComponentState,
    RequiredTools, ToolSetEntry, ToolSetRefusal, ToolSetReport,
};

use crate::tool_provisioning::ProvisionError;

/// The environment variable that makes Camerata offline: a tool that is not installed is a
/// refusal, never a download. Every test that starts the `camerata` binary sets it.
pub const OFFLINE_ENV: &str = "CAMERATA_OFFLINE";

/// True when [`OFFLINE_ENV`] is set to anything but empty or `0`.
pub fn offline_requested() -> bool {
    std::env::var_os(OFFLINE_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// The operating system's base folders a tool may find OS utilities in. Never a folder a
/// package manager installs tools into (no `/usr/local/bin`, no `/opt/homebrew/bin`).
pub const SYSTEM_PATH_DIRS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Where tools are provisioned, for which platform, and where their files come from.
///
/// The product uses [`ToolEnv::product`]. Tests build one over a temporary folder with a
/// source that serves fake archives, so no test downloads anything.
#[derive(Clone)]
pub struct ToolEnv {
    pub tooling: PathBuf,
    pub platform: Option<Platform>,
    pub source: Arc<dyn ArtifactSource>,
    /// The pins this environment installs from: [`pins::pins`] in the product. A test replaces
    /// them with pins over fake archives ([`ToolEnv::with_pins`]), so the checksum check runs on
    /// every test install exactly as it does in the product.
    pins: Arc<dyn Fn(Artifact) -> &'static pins::ArtifactPins + Send + Sync>,
}

impl std::fmt::Debug for ToolEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEnv")
            .field("tooling", &self.tooling)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl ToolEnv {
    pub fn new(
        tooling: impl Into<PathBuf>,
        platform: Option<Platform>,
        source: Arc<dyn ArtifactSource>,
    ) -> Self {
        Self {
            tooling: tooling.into(),
            platform,
            source,
            pins: Arc::new(pins::pins),
        }
    }

    /// This environment over other pins (tests: pins over fake archives).
    pub fn with_pins(
        mut self,
        pins: impl Fn(Artifact) -> &'static pins::ArtifactPins + Send + Sync + 'static,
    ) -> Self {
        self.pins = Arc::new(pins);
        self
    }

    /// The pins of `artifact` in this environment.
    pub fn pins(&self, artifact: Artifact) -> &'static pins::ArtifactPins {
        (self.pins)(artifact)
    }

    /// The product's environment: the per-user tooling folder, this machine's platform and the
    /// official release locations. `Err` when the data folder cannot be resolved.
    pub fn product() -> Result<Self, String> {
        let tooling = crate::tool_provisioning::tooling_dir().ok_or_else(|| {
            "could not resolve Camerata's data folder for the tooling folder".to_string()
        })?;
        let source: Arc<dyn ArtifactSource> = if offline_requested() {
            Arc::new(fetch::OfflineSource)
        } else {
            Arc::new(OfficialReleaseSource::default())
        };
        Ok(Self::new(tooling, Platform::current(), source))
    }

    /// The platform, or the refusal for a machine Camerata does not provision for.
    pub fn require_platform(&self, tool: &'static str) -> Result<Platform, ProvisionError> {
        self.platform
            .ok_or_else(|| ProvisionError::UnsupportedPlatform {
                tool,
                detail: platform::unsupported_platform_reason(),
            })
    }

    /// The bin folder of an installed runtime, when it is installed.
    fn runtime_bin_dir(&self, artifact: Artifact) -> Option<PathBuf> {
        let platform = self.platform?;
        let pins = self.pins(artifact);
        if !fetch::is_installed(&self.tooling, pins, platform) {
            return None;
        }
        fetch::entry_point(&self.tooling, pins, platform)?
            .parent()
            .map(Path::to_path_buf)
    }

    /// The `PATH` every provisioned tool is started with: the installed pinned runtimes' bin
    /// folders, then [`SYSTEM_PATH_DIRS`]. Never the user's `PATH`.
    pub fn child_path(&self) -> OsString {
        let mut dirs: Vec<PathBuf> = [Artifact::Node, Artifact::Python]
            .into_iter()
            .filter_map(|a| self.runtime_bin_dir(a))
            .collect();
        dirs.extend(SYSTEM_PATH_DIRS.iter().map(PathBuf::from));
        std::env::join_paths(dirs).unwrap_or_else(|_| SYSTEM_PATH_DIRS.join(":").into())
    }

    /// Install (or verify) one downloaded artifact and return its entry point.
    pub async fn ensure(&self, artifact: Artifact) -> Result<PathBuf, ProvisionError> {
        let pins = self.pins(artifact);
        let platform = self.require_platform(pins.name)?;
        fetch::ensure_artifact(&self.tooling, pins, platform, self.source.as_ref()).await?;
        fetch::entry_point(&self.tooling, pins, platform).ok_or(
            ProvisionError::NoArtifactForPlatform {
                tool: pins.name,
                platform: platform.label(),
            },
        )
    }

    /// True when `artifact` is installed from its pin. Reads the disk only.
    pub fn is_installed(&self, artifact: Artifact) -> bool {
        self.platform
            .is_some_and(|p| fetch::is_installed(&self.tooling, self.pins(artifact), p))
    }

    /// The pinned ruff binary, provisioned if it is not there yet.
    pub async fn ensure_ruff(&self) -> Result<PathBuf, ProvisionError> {
        self.ensure(Artifact::Ruff).await
    }

    /// The pinned osv-scanner binary, provisioned if it is not there yet.
    pub async fn ensure_osv_scanner(&self) -> Result<PathBuf, ProvisionError> {
        self.ensure(Artifact::OsvScanner).await
    }

    /// The pinned Node.js runtime, provisioned if it is not there yet.
    pub async fn ensure_node(&self) -> Result<NodeRuntime, ProvisionError> {
        let node = self.ensure(Artifact::Node).await?;
        Ok(NodeRuntime::from_node(node))
    }

    /// The installed Node.js runtime, without provisioning.
    pub fn installed_node(&self) -> Option<NodeRuntime> {
        let platform = self.platform?;
        let pins = self.pins(Artifact::Node);
        if !fetch::is_installed(&self.tooling, pins, platform) {
            return None;
        }
        fetch::entry_point(&self.tooling, pins, platform).map(NodeRuntime::from_node)
    }

    /// The `npm` command of the pinned Node.js: `node npm-cli.js`, with the child `PATH`, so
    /// neither npm nor anything it starts finds a `node` on the user's `PATH`.
    pub fn npm_command(&self, node: &NodeRuntime) -> crate::tool_provisioning::NpmCommand {
        crate::tool_provisioning::NpmCommand {
            launcher: node.node.clone().into_os_string(),
            prefix: vec![node.npm_cli.clone().into_os_string()],
            path_env: Some(self.child_path()),
            offline: self.source.offline(),
        }
    }
}

/// The paths of a pinned Node.js install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRuntime {
    /// `<install>/node-<version>-<platform>/bin/node`.
    pub node: PathBuf,
    /// `<install>/node-<version>-<platform>/lib/node_modules/npm/bin/npm-cli.js`.
    pub npm_cli: PathBuf,
}

impl NodeRuntime {
    fn from_node(node: PathBuf) -> Self {
        let root = node
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let npm_cli = root
            .join("lib")
            .join("node_modules")
            .join("npm")
            .join("bin")
            .join("npm-cli.js");
        Self { node, npm_cli }
    }
}
