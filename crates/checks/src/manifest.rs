//! `.camerata/checks.toml` — single source of truth for deterministic gate checks.
//!
//! This manifest is the authoritative list of CUSTOM (non-built-in) deterministic
//! checks that Camerata enforces at BOTH:
//!
//! - **Layer 2** (in-loop): checks marked `in_loop = true` are run in the governed
//!   dev loop AFTER the built-in language runners (fmt/clippy/test/polyglot). A
//!   violation bounces the work back for revision exactly as a clippy failure would.
//! - **Layer 3** (CI): the *entire* manifest (both `in_loop = true` AND
//!   `in_loop = false`) is consumed by the workflow generator to produce
//!   `.github/workflows/camerata-gates.yml`, so CI is the superset backstop.
//!
//! # Parity guarantee
//!
//! The set of commands Layer 2 runs (built-ins + manifest `in_loop` checks) MUST be
//! a SUBSET of what the generated CI workflow runs. This is structurally enforced:
//! both [`crate::ManifestCheckRunner`] and [`crate::workflow_gen`] consume the
//! SAME shared function [`layer2_commands`] / [`all_commands`] from this module,
//! so they cannot diverge by construction. A unit test asserts the subset invariant.
//!
//! # Trust model
//!
//! Manifest commands are **repo-authored shell** executed in the worktree — the same
//! trust model as running the project's own clippy/test/CI scripts. The Part-4
//! hard-guard in `camerata-gateway` (SEC-NO-CAMERATA-CONFIG-1) ensures that AGENTS
//! cannot author or modify `.camerata/checks.toml`, so the operator (the human
//! running Camerata) is always the one who decides what runs.
//!
//! # Schema
//!
//! ```toml
//! # .camerata/checks.toml
//! [[check]]
//! id       = "ARCH-API-LAYERING-1"
//! name     = "API layering"
//! command  = "scripts/check_layering.sh"
//! severity = "high"
//! in_loop  = true
//! ```
//!
//! Field semantics:
//! - `id`       — stable rule id, matches the rule corpus where applicable.
//! - `name`     — short human-readable label.
//! - `command`  — shell command run with CWD = repo/worktree root.
//! - `severity` — `"high"` | `"medium"` | `"low"`.
//! - `in_loop`  — `true` = also run at Layer 2; `false` = CI-only (use for
//!   checks that need secrets, services, or long-running time budgets).
//!
//! # Absent / malformed manifest
//!
//! A missing or unparseable manifest is NEVER fatal: [`load_manifest`] returns
//! `None` (absent) or `Err` (parse error) and callers treat either as "zero manifest
//! checks", degrading gracefully while logging a warning. The built-in runners are
//! completely unaffected.

use serde::Deserialize;
use std::path::Path;

// ─── schema ──────────────────────────────────────────────────────────────────

/// A single check entry in `.camerata/checks.toml`.
///
/// Matches the TOML `[[check]]` array-of-tables shape. All fields are required
/// (no serde defaults) so a misconfigured entry fails loudly at parse time
/// rather than silently running a wrong command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManifestCheck {
    /// Stable rule id. Should match a rule in the Camerata corpus where the
    /// check enforces a named rule (e.g. `"ARCH-API-LAYERING-1"`). Used as the
    /// violation id when the command exits non-zero.
    pub id: String,

    /// Short human-readable name, e.g. `"API layering"`.
    pub name: String,

    /// Shell command to run, with CWD = repo/worktree root. May be a script
    /// path (`"scripts/check_layering.sh"`), an inline invocation
    /// (`"npm run lint:layers"`), or any other shell-executable string.
    /// Executed by the OS shell (`sh -c <command>`).
    pub command: String,

    /// Severity label for reporting. One of `"high"`, `"medium"`, or `"low"`.
    /// Informational only at the gate level — any non-zero exit is a violation
    /// regardless of severity; severity shapes the bounce-back message priority.
    pub severity: String,

    /// Whether this check should also run at **Layer 2** (the in-loop gate).
    ///
    /// - `true`  — run in the governed dev loop (fast, no secrets/services).
    /// - `false` — CI-only (use for checks that need secrets, external services,
    ///   or long-running time budgets that would stall the agent loop).
    pub in_loop: bool,
}

/// The parsed `.camerata/checks.toml` manifest.
///
/// A flat list of [`ManifestCheck`] entries under the `check` key (TOML
/// array-of-tables: `[[check]]`). An empty `checks` list is valid — it means
/// no custom checks are configured.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct CheckManifest {
    /// All declared checks, in declaration order.
    #[serde(default, rename = "check")]
    pub checks: Vec<ManifestCheck>,
}

impl CheckManifest {
    /// Returns only the checks that should run at Layer 2 (in the dev loop).
    ///
    /// This is the SHARED command-list function consumed by BOTH
    /// [`crate::ManifestCheckRunner`] (for Layer-2 execution) and
    /// [`crate::workflow_gen::generate_gates_workflow`] (for CI generation).
    /// They derive from the SAME source, so Layer-2 ⊆ Layer-3 is structural.
    pub fn in_loop_checks(&self) -> impl Iterator<Item = &ManifestCheck> {
        self.checks.iter().filter(|c| c.in_loop)
    }

    /// Returns ALL checks (Layer 2 + CI-only), the superset for CI generation.
    pub fn all_checks(&self) -> impl Iterator<Item = &ManifestCheck> {
        self.checks.iter()
    }
}

// ─── loader ──────────────────────────────────────────────────────────────────

/// Error returned when `.camerata/checks.toml` exists but cannot be parsed.
#[derive(Debug)]
pub struct ManifestParseError {
    pub path: std::path::PathBuf,
    pub cause: String,
}

impl std::fmt::Display for ManifestParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to parse {}: {}",
            self.path.display(),
            self.cause
        )
    }
}

/// Load `.camerata/checks.toml` from the given repo/worktree root.
///
/// # Returns
///
/// - `Ok(Some(manifest))` — file found and parsed successfully.
/// - `Ok(None)`           — file not found (absent manifest; zero custom checks).
/// - `Err(e)`             — file found but TOML parse failed. Callers SHOULD log
///   `e` and treat it as zero custom checks (non-fatal; see crate-level docs).
///
/// # Panics
///
/// Never panics. All I/O and parse errors are returned or swallowed by callers.
pub fn load_manifest(
    repo_root: &Path,
) -> Result<Option<CheckManifest>, ManifestParseError> {
    let path = repo_root.join(".camerata/checks.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            // Any other I/O error (permissions, etc.) — treat as parse error
            // so callers handle it the same way.
            return Err(ManifestParseError {
                path,
                cause: e.to_string(),
            });
        }
    };
    toml::from_str::<CheckManifest>(&text).map(Some).map_err(|e| ManifestParseError {
        path,
        cause: e.to_string(),
    })
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cam-manifest-test-{}-{}-{}",
            std::process::id(),
            seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── absent manifest ────────────────────────────────────────────────────────

    #[test]
    fn missing_manifest_returns_none() {
        let root = tmpdir();
        let result = load_manifest(&root);
        assert!(
            matches!(result, Ok(None)),
            "absent .camerata/checks.toml must return Ok(None), got {result:?}"
        );
    }

    // ── valid manifest ─────────────────────────────────────────────────────────

    #[test]
    fn valid_manifest_parses_correctly() {
        let root = tmpdir();
        let camerata_dir = root.join(".camerata");
        fs::create_dir_all(&camerata_dir).unwrap();
        fs::write(
            camerata_dir.join("checks.toml"),
            r#"
[[check]]
id       = "ARCH-API-LAYERING-1"
name     = "API layering"
command  = "scripts/check_layering.sh"
severity = "high"
in_loop  = true

[[check]]
id       = "SEC-SECRETS-SCAN-1"
name     = "Secrets scan"
command  = "trufflehog filesystem ."
severity = "high"
in_loop  = false
"#,
        )
        .unwrap();

        let manifest = load_manifest(&root)
            .expect("no parse error")
            .expect("manifest present");

        assert_eq!(manifest.checks.len(), 2);

        let first = &manifest.checks[0];
        assert_eq!(first.id, "ARCH-API-LAYERING-1");
        assert_eq!(first.name, "API layering");
        assert_eq!(first.command, "scripts/check_layering.sh");
        assert_eq!(first.severity, "high");
        assert!(first.in_loop, "first check must be in_loop=true");

        let second = &manifest.checks[1];
        assert_eq!(second.id, "SEC-SECRETS-SCAN-1");
        assert!(!second.in_loop, "second check must be in_loop=false");
    }

    // ── malformed manifest ─────────────────────────────────────────────────────

    #[test]
    fn malformed_manifest_returns_err() {
        let root = tmpdir();
        let camerata_dir = root.join(".camerata");
        fs::create_dir_all(&camerata_dir).unwrap();
        fs::write(
            camerata_dir.join("checks.toml"),
            "this is not valid toml }{",
        )
        .unwrap();

        let result = load_manifest(&root);
        assert!(
            result.is_err(),
            "malformed TOML must return Err, got {result:?}"
        );
    }

    #[test]
    fn manifest_with_missing_required_field_returns_err() {
        // `in_loop` field is required (no serde default).
        let root = tmpdir();
        let camerata_dir = root.join(".camerata");
        fs::create_dir_all(&camerata_dir).unwrap();
        fs::write(
            camerata_dir.join("checks.toml"),
            r#"
[[check]]
id       = "ARCH-API-LAYERING-1"
name     = "API layering"
command  = "scripts/check_layering.sh"
severity = "high"
# in_loop is intentionally omitted — must error
"#,
        )
        .unwrap();

        let result = load_manifest(&root);
        assert!(
            result.is_err(),
            "manifest missing required in_loop field must return Err"
        );
    }

    // ── empty manifest ─────────────────────────────────────────────────────────

    #[test]
    fn empty_manifest_is_valid_with_zero_checks() {
        let root = tmpdir();
        let camerata_dir = root.join(".camerata");
        fs::create_dir_all(&camerata_dir).unwrap();
        fs::write(camerata_dir.join("checks.toml"), "# no checks yet\n").unwrap();

        let manifest = load_manifest(&root)
            .expect("no parse error")
            .expect("manifest present");
        assert!(
            manifest.checks.is_empty(),
            "empty manifest must have zero checks"
        );
    }

    // ── in_loop filtering ──────────────────────────────────────────────────────

    #[test]
    fn in_loop_checks_filters_correctly() {
        let manifest = CheckManifest {
            checks: vec![
                ManifestCheck {
                    id: "A".to_string(),
                    name: "A".to_string(),
                    command: "cmd_a".to_string(),
                    severity: "high".to_string(),
                    in_loop: true,
                },
                ManifestCheck {
                    id: "B".to_string(),
                    name: "B".to_string(),
                    command: "cmd_b".to_string(),
                    severity: "low".to_string(),
                    in_loop: false,
                },
                ManifestCheck {
                    id: "C".to_string(),
                    name: "C".to_string(),
                    command: "cmd_c".to_string(),
                    severity: "medium".to_string(),
                    in_loop: true,
                },
            ],
        };

        let loop_cmds: Vec<&str> = manifest.in_loop_checks().map(|c| c.command.as_str()).collect();
        assert_eq!(loop_cmds, vec!["cmd_a", "cmd_c"]);

        let all_cmds: Vec<&str> = manifest.all_checks().map(|c| c.command.as_str()).collect();
        assert_eq!(all_cmds, vec!["cmd_a", "cmd_b", "cmd_c"]);
    }
}
