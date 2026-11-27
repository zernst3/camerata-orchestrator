//! Every artifact Camerata downloads, at its pinned version, with the SHA-256 of each platform's
//! file as the project publishes it.
//!
//! # Where each checksum came from
//!
//! Each value below was copied from the project's own checksum file at the pinned release and
//! then confirmed by hashing the downloaded file (2026-11-27):
//!
//! - ruff 0.17.5: the `<asset>.sha256` file beside each asset on
//!   `github.com/astral-sh/ruff/releases/tag/0.17.5`.
//! - Node.js v24.22.3 (LTS "Krypton"): `nodejs.org/dist/v24.22.3/SHASUMS256.txt`.
//! - CPython 3.12.16 from python-build-standalone 20261110: the release's `SHA256SUMS` on
//!   `github.com/astral-sh/python-build-standalone/releases/tag/20261110`.
//! - osv-scanner v1.9.2: `osv-scanner_SHA256SUMS` on
//!   `github.com/google/osv-scanner/releases/tag/v1.9.2`.
//!
//! Every pin was at least two weeks old when it was chosen, so a release pulled for a defect in
//! its first days is not the one Camerata ships.
//!
//! # Bumping a pin
//!
//! Change the version, every URL and every checksum together, from the new release's own
//! checksum file. A pin bump installs beside the old one (the install folder carries the
//! version), so a machine never runs half of one release and half of another. The tests hold
//! every URL to its project's official release location ([`ArtifactPins::official_source`]).

use super::platform::Platform;

/// A downloadable artifact: a tool or a runtime a tool needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Artifact {
    /// The ruff linter (Python), a single static binary.
    Ruff,
    /// The Node.js runtime with its bundled npm; eslint runs on it.
    Node,
    /// A relocatable CPython build; semgrep is installed into a virtual environment made from it.
    Python,
    /// The osv-scanner dependency auditor, a single static binary.
    OsvScanner,
}

/// How the downloaded file becomes an install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// A gzip-compressed tar archive, unpacked into the install folder as it is.
    TarGz,
    /// A bare executable, written into the install folder under [`PlatformPin::files`]'s first
    /// name and marked executable.
    Binary,
}

/// One platform's file for an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformPin {
    pub platform: Platform,
    /// Where the file is published. Always under [`ArtifactPins::official_source`].
    pub url: &'static str,
    /// The SHA-256 of the file, lowercase hex. A download that hashes to anything else is refused.
    pub sha256: &'static str,
    /// Paths under the install folder that a complete install has. The first is the entry point
    /// (the program Camerata runs); the rest are files it also needs (npm's script for Node).
    pub files: &'static [&'static str],
}

/// An artifact at its pinned version, with every platform it is published for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactPins {
    pub artifact: Artifact,
    /// The name an operator reads ("ruff", "node", "python", "osv-scanner").
    pub name: &'static str,
    pub version: &'static str,
    /// The project's official release location. Every URL starts with it, and the download
    /// source refuses any URL that does not.
    pub official_source: &'static str,
    pub kind: ArchiveKind,
    pub pins: &'static [PlatformPin],
}

impl ArtifactPins {
    /// The file for `platform`, or `None` when the project publishes none.
    pub fn for_platform(&self, platform: Platform) -> Option<&'static PlatformPin> {
        self.pins.iter().find(|p| p.platform == platform)
    }

    /// The folder name of this pin's install under the tooling folder: name plus version, so a
    /// bump installs beside the previous release instead of over it.
    pub fn install_dir_name(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// The pins for `artifact`.
pub fn pins(artifact: Artifact) -> &'static ArtifactPins {
    match artifact {
        Artifact::Ruff => &RUFF,
        Artifact::Node => &NODE,
        Artifact::Python => &PYTHON,
        Artifact::OsvScanner => &OSV_SCANNER,
    }
}

/// Every artifact, in report order.
pub const ALL_ARTIFACTS: [Artifact; 4] = [
    Artifact::Ruff,
    Artifact::Node,
    Artifact::Python,
    Artifact::OsvScanner,
];

pub const RUFF_VERSION: &str = "0.17.5";

static RUFF: ArtifactPins = ArtifactPins {
    artifact: Artifact::Ruff,
    name: "ruff",
    version: RUFF_VERSION,
    official_source: "https://github.com/astral-sh/ruff/releases/download/",
    kind: ArchiveKind::TarGz,
    pins: &[
        PlatformPin {
            platform: Platform::MacArm64,
            url: "https://github.com/astral-sh/ruff/releases/download/0.17.5/ruff-aarch64-apple-darwin.tar.gz",
            sha256: "4ffadabec5c5fee1ddb2eecac0787ffe6fa18c665b73da9258b84211f40c5829",
            files: &["ruff-aarch64-apple-darwin/ruff"],
        },
        PlatformPin {
            platform: Platform::MacX64,
            url: "https://github.com/astral-sh/ruff/releases/download/0.17.5/ruff-x86_64-apple-darwin.tar.gz",
            sha256: "892f70854ebcd70d873b1bee8358cfd3a1e3180388e3851e3fd3e4a1d5a4dfdd",
            files: &["ruff-x86_64-apple-darwin/ruff"],
        },
        PlatformPin {
            platform: Platform::LinuxX64,
            url: "https://github.com/astral-sh/ruff/releases/download/0.17.5/ruff-x86_64-unknown-linux-gnu.tar.gz",
            sha256: "7d261fc981a784668e19e9fa6ad437bfd95b67f2dc263fc1eab87d54d72fcc90",
            files: &["ruff-x86_64-unknown-linux-gnu/ruff"],
        },
        PlatformPin {
            platform: Platform::LinuxArm64,
            url: "https://github.com/astral-sh/ruff/releases/download/0.17.5/ruff-aarch64-unknown-linux-gnu.tar.gz",
            sha256: "4d959d4322a8cd3effe11e9fc6743da1dbb41cd0b2476e433b6e6bfd9a69b626",
            files: &["ruff-aarch64-unknown-linux-gnu/ruff"],
        },
    ],
};

pub const NODE_VERSION: &str = "v24.22.3";

static NODE: ArtifactPins = ArtifactPins {
    artifact: Artifact::Node,
    name: "node",
    version: NODE_VERSION,
    official_source: "https://nodejs.org/dist/",
    kind: ArchiveKind::TarGz,
    pins: &[
        PlatformPin {
            platform: Platform::MacArm64,
            url: "https://nodejs.org/dist/v24.22.3/node-v24.22.3-darwin-arm64.tar.gz",
            sha256: "ff6e17d4874b76d272212b908cc020d6e815fba5a6c7eaaa06add725840307fd",
            files: &[
                "node-v24.22.3-darwin-arm64/bin/node",
                "node-v24.22.3-darwin-arm64/lib/node_modules/npm/bin/npm-cli.js",
            ],
        },
        PlatformPin {
            platform: Platform::MacX64,
            url: "https://nodejs.org/dist/v24.22.3/node-v24.22.3-darwin-x64.tar.gz",
            sha256: "977614af0504c81045e7fd85596dbca0e5f951b96681516dadafedcca8c447f7",
            files: &[
                "node-v24.22.3-darwin-x64/bin/node",
                "node-v24.22.3-darwin-x64/lib/node_modules/npm/bin/npm-cli.js",
            ],
        },
        PlatformPin {
            platform: Platform::LinuxX64,
            url: "https://nodejs.org/dist/v24.22.3/node-v24.22.3-linux-x64.tar.gz",
            sha256: "061ec3e3062115705b56b51fa9cc330ab50d34b6969387d4e4b1a50c6809acc2",
            files: &[
                "node-v24.22.3-linux-x64/bin/node",
                "node-v24.22.3-linux-x64/lib/node_modules/npm/bin/npm-cli.js",
            ],
        },
        PlatformPin {
            platform: Platform::LinuxArm64,
            url: "https://nodejs.org/dist/v24.22.3/node-v24.22.3-linux-arm64.tar.gz",
            sha256: "261af2af250dfd7e9f25cf151d1b2713a0adf8f10a9ad116c4947f68627affe8",
            files: &[
                "node-v24.22.3-linux-arm64/bin/node",
                "node-v24.22.3-linux-arm64/lib/node_modules/npm/bin/npm-cli.js",
            ],
        },
    ],
};

pub const PYTHON_VERSION: &str = "3.12.16+20261110";

static PYTHON: ArtifactPins = ArtifactPins {
    artifact: Artifact::Python,
    name: "python",
    version: PYTHON_VERSION,
    official_source: "https://github.com/astral-sh/python-build-standalone/releases/download/",
    kind: ArchiveKind::TarGz,
    pins: &[
        PlatformPin {
            platform: Platform::MacArm64,
            url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-aarch64-apple-darwin-install_only.tar.gz",
            sha256: "e956e60bc9465d577ef8062059aa05cd3a2665b4b8678a2ed24e432368330b8f",
            files: &["python/bin/python3"],
        },
        PlatformPin {
            platform: Platform::MacX64,
            url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-x86_64-apple-darwin-install_only.tar.gz",
            sha256: "83f2922437e6d546d0fcdb8c11f150a8e52787e8d9ea8651b1974e20908eb1bc",
            files: &["python/bin/python3"],
        },
        PlatformPin {
            platform: Platform::LinuxX64,
            url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-x86_64-unknown-linux-gnu-install_only.tar.gz",
            sha256: "c2f87d4b4e8db3abddb0f116c6ccfc503d0cbdbb9ee8faacddad1b679ec12e99",
            files: &["python/bin/python3"],
        },
        PlatformPin {
            platform: Platform::LinuxArm64,
            url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-aarch64-unknown-linux-gnu-install_only.tar.gz",
            sha256: "4792f81750282597fb2a3389fe18b945ed2ba6aad5e45ad406d122c58cb33081",
            files: &["python/bin/python3"],
        },
    ],
};

pub const OSV_SCANNER_VERSION: &str = "v1.9.2";

static OSV_SCANNER: ArtifactPins = ArtifactPins {
    artifact: Artifact::OsvScanner,
    name: "osv-scanner",
    version: OSV_SCANNER_VERSION,
    official_source: "https://github.com/google/osv-scanner/releases/download/",
    kind: ArchiveKind::Binary,
    pins: &[
        PlatformPin {
            platform: Platform::MacArm64,
            url: "https://github.com/google/osv-scanner/releases/download/v1.9.2/osv-scanner_darwin_arm64",
            sha256: "393f2c7089d9431bd26a3804d6e46d417b1c05abd5d49c41c7dfc174c520acf0",
            files: &["osv-scanner"],
        },
        PlatformPin {
            platform: Platform::MacX64,
            url: "https://github.com/google/osv-scanner/releases/download/v1.9.2/osv-scanner_darwin_amd64",
            sha256: "487ab433b2c2a8c80b737c0bd428a80e6d2e211b4adf775a52a6964163fa3249",
            files: &["osv-scanner"],
        },
        PlatformPin {
            platform: Platform::LinuxX64,
            url: "https://github.com/google/osv-scanner/releases/download/v1.9.2/osv-scanner_linux_amd64",
            sha256: "d6af4b67fa5de658598bd2d445efb99e90d1734b3146962418719c4350ecb74b",
            files: &["osv-scanner"],
        },
        PlatformPin {
            platform: Platform::LinuxArm64,
            url: "https://github.com/google/osv-scanner/releases/download/v1.9.2/osv-scanner_linux_arm64",
            sha256: "9c6160afb26c79449a1f1b667323b989a57dda8fc19f22936c9ff920fd97ddfa",
            files: &["osv-scanner"],
        },
    ],
};

/// The semgrep release installed into the pinned Python. Its wheel and every dependency's are
/// pinned by hash in `assets/semgrep/requirements.txt` (generated with
/// `uv pip compile --universal --python-version 3.12 --generate-hashes --only-binary :all:
/// --exclude-newer 2026-11-13`, from `pypi.org/simple`).
pub const SEMGREP_VERSION: &str = "1.185.0";

/// The platforms semgrep publishes a wheel for at [`SEMGREP_VERSION`]. semgrep stopped
/// publishing Intel macOS wheels after 1.99.0, so on an Intel Mac semgrep cannot be provisioned.
pub const SEMGREP_PLATFORMS: [Platform; 3] =
    [Platform::MacArm64, Platform::LinuxX64, Platform::LinuxArm64];

/// The package index semgrep and its dependencies install from: PyPI, semgrep's official
/// distribution channel.
pub const PYPI_INDEX: &str = "https://pypi.org/simple";
