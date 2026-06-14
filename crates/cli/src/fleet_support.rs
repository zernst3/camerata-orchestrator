//! Shared scaffolding for the LIVE governed FLEET demos (`build-demo`, `po-demo`).
//!
//! Both demos do the same governed-fleet plumbing: build governed roles from the
//! real corpus (with the gateway's enforced gate rules blended in), locate the
//! gateway binary, scaffold a temp cargo library crate as the shared worktree,
//! and `cargo build`/`cargo test` whatever the governed agents produced. This
//! module owns that plumbing so the two demos share ONE governed path rather than
//! drifting apart. The demos differ only in WHERE the stage tasks come from:
//! `build-demo` hand-specifies two tasks; `po-demo` derives them from a lead
//! engineer's [`camerata_intake::Plan`].

use std::path::{Path, PathBuf};

use camerata_core::{CheckRunner, Role, RuleId};
use camerata_gateway::{gov1_rule, sec_no_hardcoded_secrets_1_rule};
use camerata_rules::role_from_corpus;
pub use camerata_rules::DEFAULT_CORPUS_PATH;

/// Domains the fleet roles are scoped to in the corpus selection. The code the
/// agents write is plain Rust, so the `rust` family (+ universal `*` rules) is
/// the relevant slice; `agentic` rides along because these ARE agentic runs.
pub const FLEET_DOMAINS: &[&str] = &["rust", "agentic"];

/// A layer-2 check runner that reports NO structural violations.
///
/// The demos' real layer-2 verification is `cargo build` + `cargo test` on the
/// finished crate AFTER the fleet completes (a partially-written crate mid-fleet
/// would not build, so per-stage cargo checks would be meaningless). The fleet's
/// bounce-and-revise machinery is still exercised end-to-end by the coordinator
/// tests; here we keep the layer-2 seam a no-op and let the final cargo gates be
/// the judge.
pub struct NoopChecks;

#[async_trait::async_trait]
impl CheckRunner for NoopChecks {
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        Ok(vec![])
    }
}

/// Build a governed role from the real corpus, named `role_name`, and ensure the
/// gateway-enforced gate rules (GOV-1 + the hardcoded-secret rule) are in the
/// delivered subset so the per-session governance is genuinely active — the same
/// honest blend the live single-agent demo uses.
pub async fn governed_role(role_name: &str) -> anyhow::Result<Role> {
    let corpus = Path::new(DEFAULT_CORPUS_PATH);
    let mut role = role_from_corpus(corpus, role_name, FLEET_DOMAINS, &[]).await?;

    for gate_rule in [sec_no_hardcoded_secrets_1_rule(), gov1_rule()] {
        if !role.rule_subset.contains(&gate_rule) {
            role.rule_subset.insert(0, gate_rule);
        }
    }
    Ok(role)
}

/// Locate the built `camerata-gateway` binary (release preferred, debug
/// fallback).
pub fn locate_gateway_bin() -> anyhow::Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("cannot locate workspace root from {manifest_dir:?}"))?;

    for profile in ["release", "debug"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join("camerata-gateway");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "camerata-gateway binary not found under {}/target/{{release,debug}}. \
         Build it first: `cargo build -p camerata-gateway`.",
        workspace_root.display()
    )
}

/// Scaffold a fresh cargo library crate at `dir` (the shared worktree).
///
/// Writes a `Cargo.toml` and a placeholder `src/lib.rs`. The placeholder is
/// overwritten by the first agent's governed write; it exists only so the
/// directory is a valid (if empty) crate before the agents run.
pub fn scaffold_crate(dir: &Path, crate_name: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir.join("src"))?;
    let cargo_toml = format!(
        "[package]\nname = \"{crate_name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n"
    );
    std::fs::write(dir.join("Cargo.toml"), cargo_toml)?;
    std::fs::write(
        dir.join("src").join("lib.rs"),
        "// placeholder — to be overwritten by the first governed agent\n",
    )?;
    Ok(())
}

/// The result of running `cargo <subcommand>` on the produced crate.
pub struct CargoOutcome {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `cargo <subcommand>` in `dir` and capture its outcome.
pub async fn run_cargo(dir: &Path, subcommand: &str) -> anyhow::Result<CargoOutcome> {
    let out = tokio::process::Command::new("cargo")
        .arg(subcommand)
        .current_dir(dir)
        .output()
        .await?;
    Ok(CargoOutcome {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Return the last `n` lines of `s` as owned strings (for bounded output).
pub fn tail_lines(s: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|l| l.to_string()).collect()
}
