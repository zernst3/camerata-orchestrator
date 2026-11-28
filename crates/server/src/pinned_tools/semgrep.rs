//! semgrep, installed into a virtual environment made from the pinned CPython.
//!
//! semgrep is distributed as a Python wheel (it carries its native engine inside), so it needs a
//! Python. The interpreter is the pinned python-build-standalone CPython ([`super::pins`]), never
//! a `python3` from the user's `PATH`. semgrep and every dependency install from PyPI pinned by
//! version and SHA-256 ([`SEMGREP_REQUIREMENTS`], pip `--require-hashes --no-deps`), so the
//! installed tree is the same on every machine and a changed file is refused by pip.

use std::path::{Path, PathBuf};

use super::fetch::{pinned_root, sha256_hex, INSTALL_MARKER};
use super::pins::{Artifact, PYPI_INDEX, SEMGREP_PLATFORMS, SEMGREP_VERSION};
use super::ToolEnv;
use crate::tool_provisioning::{self, ProvisionError};

/// semgrep and every dependency, by version and hash. See the file's header for how it is made.
pub const SEMGREP_REQUIREMENTS: &str = include_str!("../../assets/semgrep/requirements.txt");

/// `<tooling>/pinned/semgrep-<version>/`.
fn semgrep_dir(tooling: &Path) -> PathBuf {
    pinned_root(tooling).join(format!("semgrep-{SEMGREP_VERSION}"))
}

fn venv_dir(tooling: &Path) -> PathBuf {
    semgrep_dir(tooling).join("venv")
}

/// The semgrep program inside the pinned venv.
pub fn semgrep_bin(tooling: &Path) -> PathBuf {
    tool_provisioning::semgrep_bin(&venv_dir(tooling))
}

fn venv_python(tooling: &Path) -> PathBuf {
    let venv = venv_dir(tooling);
    #[cfg(windows)]
    let python = venv.join("Scripts").join("python.exe");
    #[cfg(not(windows))]
    let python = venv.join("bin").join("python");
    python
}

fn marker_text() -> String {
    format!(
        "semgrep {SEMGREP_VERSION} {}\n",
        sha256_hex(SEMGREP_REQUIREMENTS.as_bytes())
    )
}

/// True when semgrep is installed from exactly the pinned requirements, on the pinned Python.
/// Reads the disk only.
pub fn semgrep_is_installed(env: &ToolEnv) -> bool {
    env.is_installed(Artifact::Python)
        && semgrep_bin(&env.tooling).is_file()
        && std::fs::read_to_string(semgrep_dir(&env.tooling).join(INSTALL_MARKER))
            .is_ok_and(|m| m == marker_text())
}

/// The reason semgrep cannot be provisioned on this machine, when it cannot.
pub fn semgrep_unavailable_reason(env: &ToolEnv) -> Option<String> {
    match env.platform {
        None => Some(super::platform::unsupported_platform_reason()),
        Some(p) if !SEMGREP_PLATFORMS.contains(&p) => Some(format!(
            "semgrep {SEMGREP_VERSION} publishes no build for {}",
            p.label()
        )),
        Some(_) => None,
    }
}

/// A command for a program of the semgrep install: the child `PATH`, no user site packages, no
/// inherited Python settings.
fn python_child(env: &ToolEnv, program: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.kill_on_drop(true)
        .env("PATH", env.child_path())
        .env("PYTHONNOUSERSITE", "1")
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV");
    camerata_core::apply_semgrep_env(cmd.as_std_mut());
    cmd
}

async fn run_step(mut cmd: tokio::process::Command, what: &str) -> Result<(), ProvisionError> {
    let out = cmd.output().await?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(ProvisionError::InstallFailed {
        tool: "semgrep",
        detail: format!(
            "{what} failed: {}",
            stderr.trim().chars().take(600).collect::<String>()
        ),
    })
}

/// semgrep, provisioned if it is not there yet: the pinned Python first, then a venv made from
/// it, then the hash-locked install. Returns the semgrep program.
pub async fn ensure_semgrep(env: &ToolEnv) -> Result<PathBuf, ProvisionError> {
    if let Some(detail) = semgrep_unavailable_reason(env) {
        return Err(ProvisionError::UnsupportedPlatform {
            tool: "semgrep",
            detail,
        });
    }
    let bin = semgrep_bin(&env.tooling);
    if semgrep_is_installed(env) {
        return Ok(bin);
    }
    let python = env.ensure(Artifact::Python).await?;
    // pip would reach PyPI: offline, a missing semgrep is a refusal.
    if env.source.offline() {
        return Err(super::fetch::offline("semgrep", &env.tooling));
    }
    tool_provisioning::refuse_real_tooling_in_test_process(&env.tooling, "semgrep")?;
    let _lock = tool_provisioning::acquire_provision_lock(
        &env.tooling,
        "semgrep",
        tool_provisioning::PROVISION_LOCK_TIMEOUT,
    )
    .await?;
    if semgrep_is_installed(env) {
        return Ok(bin);
    }

    // Built in place, under the lock: a venv is not relocatable (its scripts name the absolute
    // path of their interpreter), so it cannot be staged and renamed. The marker is written last,
    // so an interrupted install never reads as installed and is rebuilt from scratch next time.
    let dir = semgrep_dir(&env.tooling);
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await?;
    let requirements = dir.join("requirements.txt");
    tokio::fs::write(&requirements, SEMGREP_REQUIREMENTS).await?;

    let mut venv = python_child(env, &python);
    venv.args(["-m", "venv"]).arg(venv_dir(&env.tooling));
    run_step(venv, "creating the semgrep virtual environment").await?;

    let mut pip = python_child(env, &venv_python(&env.tooling));
    pip.args([
        "-m",
        "pip",
        "install",
        "--isolated",
        "--disable-pip-version-check",
        "--no-input",
        "--quiet",
        "--require-hashes",
        "--only-binary=:all:",
        "--no-deps",
        "--index-url",
        PYPI_INDEX,
        "-r",
    ])
    .arg(&requirements);
    run_step(pip, "installing semgrep from its hash-locked requirements").await?;

    let mut probe = python_child(env, &bin);
    probe.arg("--version");
    run_step(probe, "the installed semgrep answering --version").await?;

    tokio::fs::write(dir.join(INSTALL_MARKER), marker_text()).await?;
    Ok(bin)
}
