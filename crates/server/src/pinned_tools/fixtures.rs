//! Fake pinned tools for tests: no test downloads anything (#587).
//!
//! [`FakeTools`] builds, for one platform, a fake archive of every artifact (each tool a small
//! `/bin/sh` script), pins over those archives' REAL SHA-256 values, and a [`FakeSource`] that
//! serves them from memory. A [`ToolEnv`] over a temporary tooling folder then provisions
//! through the very code the product runs: download, checksum check, unpack, marker, and for
//! semgrep and eslint the venv and npm steps (the fake Python and the fake Node answer them).
//!
//! Every fake program appends the `PATH` it was started with to `<program>.runs`, so a test can
//! prove which `PATH` a tool saw and that it ran at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::fetch::{sha256_hex, ArtifactSource};
use super::pins::{self, ArchiveKind, Artifact, ArtifactPins, PlatformPin};
use super::platform::Platform;
use super::ToolEnv;

/// Serves files from memory, records every URL asked for, and can be made to fail.
#[derive(Default)]
pub struct FakeSource {
    served: Mutex<HashMap<String, Vec<u8>>>,
    fetched: Mutex<Vec<String>>,
    fail_all: Mutex<Option<String>>,
}

impl FakeSource {
    /// Serve `bytes` at `url`.
    pub fn serve(&self, url: &str, bytes: Vec<u8>) {
        self.served
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), bytes);
    }

    /// Make every later fetch fail with `why` (a network outage).
    pub fn fail_everything(&self, why: &str) {
        *self.fail_all.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.to_string());
    }

    /// Every URL fetched so far, in order.
    pub fn fetched(&self) -> Vec<String> {
        self.fetched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait::async_trait]
impl ArtifactSource for FakeSource {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        self.fetched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(url.to_string());
        if let Some(why) = self
            .fail_all
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Err(why);
        }
        self.served
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(url)
            .cloned()
            .ok_or_else(|| format!("the fixture serves nothing at {url}"))
    }
}

/// A gzip tar archive of `(path, content)` files, every one mode 0755.
pub fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, path, *content)
            .expect("append to an in-memory archive");
    }
    builder
        .into_inner()
        .and_then(|gz| gz.finish())
        .expect("finish an in-memory archive")
}

/// The fake ruff: answers `--version`; for `check`, reports one finding of the first selected
/// code in `app.py` of the folder it runs in.
pub const FAKE_RUFF: &str = r#"#!/bin/sh
echo "$PATH" >> "$0.runs"
if [ "$1" = "--version" ]; then echo "ruff 0.17.5"; exit 0; fi
sel=""; prev=""
for a in "$@"; do if [ "$prev" = "--select" ]; then sel="$a"; fi; prev="$a"; done
code="${sel%%,*}"
printf '[{"code":"%s","filename":"%s/app.py","location":{"row":1,"column":1},"message":"fake ruff finding"}]\n' "$code" "$PWD"
"#;

/// The fake osv-scanner: answers `--version`; a scan reports no vulnerabilities.
pub const FAKE_OSV: &str = r#"#!/bin/sh
echo "$PATH" >> "$0.runs"
if [ "$1" = "--version" ]; then echo "osv-scanner version: 1.9.2"; exit 0; fi
echo '{"results":[]}'
"#;

/// The fake semgrep a fake `pip install` writes into the venv.
const FAKE_SEMGREP: &str = r#"#!/bin/sh
echo "$PATH" >> "$0.runs"
if [ "$1" = "--version" ]; then echo "1.185.0"; exit 0; fi
echo "{\"version\":\"2.1.0\",\"runs\":[{\"results\":[]}]}"
"#;

/// The fake CPython: `-m venv DIR` makes a venv whose python is this script; that python's
/// `-m pip install` writes the fake semgrep beside it.
fn fake_python() -> String {
    format!(
        r#"#!/bin/sh
echo "$PATH" >> "$0.runs"
if [ "$1" = "--version" ]; then echo "Python 3.12.16"; exit 0; fi
if [ "$1" = "-m" ] && [ "$2" = "venv" ]; then
  mkdir -p "$3/bin" && cp "$0" "$3/bin/python" && exit 0
fi
if [ "$1" = "-m" ] && [ "$2" = "pip" ]; then
  d=$(dirname "$0")
  SEMGREP_BODY='{FAKE_SEMGREP}'
  printf '%s' "$SEMGREP_BODY" > "$d/semgrep" && chmod +x "$d/semgrep" && exit 0
fi
echo "fake python: unexpected $*" >&2
exit 2
"#
    )
}

/// The fake Node.js: runs npm's script (`--version`, and `ci`/`install`, which lay down every
/// pinned eslint package) and eslint's script (`--version`, and a lint that reports one clean
/// file).
fn fake_node() -> String {
    let mut install = String::from(
        "    rm -rf node_modules\n    mkdir -p node_modules/.bin node_modules/eslint/bin\n",
    );
    for spec in crate::tool_provisioning::ESLINT_NPM_PACKAGES {
        let name = crate::tool_provisioning::npm_package_name(spec);
        let version = crate::tool_provisioning::npm_pinned_version(spec).unwrap_or("0.0.0");
        install.push_str(&format!(
            "    mkdir -p node_modules/{name}\n    printf '{{\"version\":\"{version}\"}}' > node_modules/{name}/package.json\n"
        ));
    }
    install.push_str(
        "    printf '#!/bin/sh\\nexit 0\\n' > node_modules/.bin/eslint\n    chmod +x node_modules/.bin/eslint\n    printf '// fake eslint\\n' > node_modules/eslint/bin/eslint.js\n",
    );
    format!(
        r#"#!/bin/sh
echo "$PATH" >> "$0.runs"
if [ "$1" = "--version" ]; then echo "v24.22.3"; exit 0; fi
script="$1"; shift
case "$script" in
  *npm-cli.js)
    if [ "$1" = "--version" ]; then echo "11.0.0"; exit 0; fi
{install}    exit 0
    ;;
  *eslint.js)
    if [ "$1" = "--version" ]; then echo "v9.39.5"; exit 0; fi
    printf '[{{"filePath":"%s/app.js","messages":[]}}]\n' "$PWD"
    exit 0
    ;;
esac
echo "fake node: unexpected $script" >&2
exit 2
"#
    )
}

/// A leaked copy of `real`'s pin for `platform`, pointing at a fixture URL and pinned to the
/// checksum of `bytes`.
fn fake_pins(
    real: &'static ArtifactPins,
    platform: Platform,
    bytes: &[u8],
) -> &'static ArtifactPins {
    let real_pin = real
        .for_platform(platform)
        .expect("the real pins cover every platform the fixture builds for");
    let pin: &'static [PlatformPin] = Box::leak(Box::new([PlatformPin {
        platform,
        url: Box::leak(
            format!("https://fixture.invalid/{}", real.install_dir_name()).into_boxed_str(),
        ),
        sha256: Box::leak(sha256_hex(bytes).into_boxed_str()),
        files: real_pin.files,
    }]));
    Box::leak(Box::new(ArtifactPins { pins: pin, ..*real }))
}

/// The fake archive of `artifact` laid out as `pin`'s files.
fn fake_archive(artifact: Artifact, pin: &PlatformPin, kind: ArchiveKind) -> Vec<u8> {
    let entry = pin.files[0];
    match (artifact, kind) {
        (Artifact::OsvScanner, _) | (_, ArchiveKind::Binary) => FAKE_OSV.as_bytes().to_vec(),
        (Artifact::Ruff, _) => tar_gz(&[(entry, FAKE_RUFF.as_bytes())]),
        (Artifact::Python, _) => tar_gz(&[(entry, fake_python().as_bytes())]),
        (Artifact::Node, _) => tar_gz(&[
            (entry, fake_node().as_bytes()),
            (pin.files[1], b"// fake npm-cli.js\n"),
        ]),
    }
}

/// A temporary tooling folder, fake archives of every artifact served from memory, and a
/// [`ToolEnv`] over both with pins that match the fakes.
pub struct FakeTools {
    pub env: ToolEnv,
    pub source: Arc<FakeSource>,
    pub platform: Platform,
    archives: HashMap<Artifact, (String, Vec<u8>)>,
    _dir: tempfile::TempDir,
}

impl FakeTools {
    /// Fakes for the platform this test runs on (Linux x86_64 when it has none).
    pub fn new() -> Self {
        Self::for_platform(Platform::current().unwrap_or(Platform::LinuxX64))
    }

    pub fn for_platform(platform: Platform) -> Self {
        Self::with_archives(platform, &[])
    }

    /// Fakes where osv-scanner is `script` (a test that needs it to report something).
    pub fn with_osv_script(script: &str) -> Self {
        Self::with_tool_script(Artifact::OsvScanner, script)
    }

    /// Fakes where the single-program tool `artifact` (ruff or osv-scanner) is `script`, packed
    /// the way its release is (a tar archive for ruff, a bare binary for osv-scanner).
    pub fn with_tool_script(artifact: Artifact, script: &str) -> Self {
        let platform = Platform::current().unwrap_or(Platform::LinuxX64);
        let real = pins::pins(artifact);
        let entry = real
            .for_platform(platform)
            .expect("the real pins cover every platform")
            .files[0];
        let bytes = match real.kind {
            ArchiveKind::TarGz => tar_gz(&[(entry, script.as_bytes())]),
            ArchiveKind::Binary => script.as_bytes().to_vec(),
        };
        Self::with_archives(platform, &[(artifact, bytes)])
    }

    /// Fakes for `platform`, with `overrides` as the exact bytes served (and pinned) for those
    /// artifacts.
    pub fn with_archives(platform: Platform, overrides: &[(Artifact, Vec<u8>)]) -> Self {
        let dir = tempfile::TempDir::new().expect("a temporary tooling folder");
        let source = Arc::new(FakeSource::default());
        let mut table: HashMap<Artifact, &'static ArtifactPins> = HashMap::new();
        let mut archives = HashMap::new();
        for artifact in pins::ALL_ARTIFACTS {
            let real = pins::pins(artifact);
            let real_pin = real
                .for_platform(platform)
                .expect("the real pins cover every platform");
            let bytes = overrides
                .iter()
                .find(|(a, _)| *a == artifact)
                .map(|(_, b)| b.clone())
                .unwrap_or_else(|| fake_archive(artifact, real_pin, real.kind));
            let fake = fake_pins(real, platform, &bytes);
            let url = fake.pins[0].url.to_string();
            source.serve(&url, bytes.clone());
            archives.insert(artifact, (url, bytes));
            table.insert(artifact, fake);
        }
        let env = ToolEnv::new(dir.path().join("tooling"), Some(platform), source.clone())
            .with_pins(move |a| table[&a]);
        Self {
            env,
            source,
            platform,
            archives,
            _dir: dir,
        }
    }

    /// Serve different bytes than the pin's for `artifact`: its download now fails the checksum.
    pub fn tamper(&self, artifact: Artifact) {
        let (url, bytes) = &self.archives[&artifact];
        let mut tampered = bytes.clone();
        tampered.extend_from_slice(b"\n# tampered\n");
        self.source.serve(url, tampered);
    }

    /// The tooling folder.
    pub fn tooling(&self) -> &Path {
        &self.env.tooling
    }

    /// The installed entry point of `artifact` (whether or not it is installed yet).
    pub fn entry(&self, artifact: Artifact) -> PathBuf {
        super::fetch::entry_point(&self.env.tooling, self.env.pins(artifact), self.platform)
            .expect("the fake pins name an entry point")
    }

    /// Every `PATH` the program at `program` was started with, one per run.
    pub fn runs_of(program: &Path) -> Vec<String> {
        let mut log = program.as_os_str().to_owned();
        log.push(".runs");
        std::fs::read_to_string(PathBuf::from(log))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }
}

impl Default for FakeTools {
    fn default() -> Self {
        Self::new()
    }
}
