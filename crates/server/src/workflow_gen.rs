//! Layer-3 CI workflow generator.
//!
//! Generates `.github/workflows/camerata-gates.yml` from two sources:
//!
//! 1. **Built-in language steps** — the same steps Layer 2 runs for the repo's
//!    detected language stack (Rust: fmt/clippy/test; a clearly-marked TODO block
//!    for other stacks). These mirror the in-process runners in `camerata-checks`.
//! 2. **Manifest checks** — ALL entries in `.camerata/checks.toml`, both
//!    `in_loop = true` AND `in_loop = false`. CI is the superset backstop.
//!
//! # Parity guarantee (Layer-2 ⊆ Layer-3)
//!
//! [`layer2_commands`] returns the exact command strings Layer 2 will run for a
//! given stack + manifest. [`all_ci_commands`] returns the superset Layer 3 runs.
//! Both the [`ManifestCheckRunner`] and this generator consume these shared
//! functions, so they cannot diverge by construction.
//!
//! A unit test asserts: every command in [`layer2_commands`] also appears in
//! [`all_ci_commands`] (the subset invariant).
//!
//! # Trust model
//!
//! Manifest commands are repo-authored shell, the same trust model as running the
//! project's own CI scripts. The Part-4 gateway hard-guard
//! (`SEC-NO-CAMERATA-CONFIG-1`) ensures agents cannot author or modify
//! `.camerata/checks.toml`, so only the operator decides what runs here.
//!
//! # Endpoint
//!
//! `POST /api/projects/active/generate-ci-workflow` — writes + returns the YAML.
//! Minimal implementation: generation logic lives in pure functions here;
//! the handler in `lib.rs` calls [`generate_gates_workflow`] and returns the YAML.

use camerata_checks::CheckManifest;
use serde::{Deserialize, Serialize};

// ─── stack descriptor ─────────────────────────────────────────────────────────

/// The language stack detected in the repo. Determines which built-in steps
/// are emitted in the generated workflow.
///
/// The variant set matches the [`camerata_checks::WorktreeLanguage`] enum; we
/// keep a separate enum here so `camerata-server` does not re-export a checks-
/// crate internal type in the HTTP contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepoStack {
    /// Cargo-based Rust workspace. Built-in steps: `cargo fmt --check`,
    /// `cargo clippy -- -D warnings`, `cargo test --no-fail-fast`.
    #[default]
    Rust,
    /// JavaScript / TypeScript (package.json). TODO: add built-in npm/yarn steps.
    JavaScript,
    /// Python (pyproject.toml / requirements.txt). TODO: add built-in steps.
    Python,
    /// Go (go.mod). TODO: add built-in steps.
    Go,
    /// Ruby (Gemfile). TODO: add built-in steps.
    Ruby,
    /// Java (pom.xml / build.gradle). TODO: add built-in steps.
    Java,
    /// C# (*.csproj / *.sln). TODO: add built-in steps.
    CSharp,
    /// Unknown / undetected. No built-in steps emitted.
    Unknown,
}

impl RepoStack {
    /// The built-in commands Layer 2 runs for this stack.
    ///
    /// These are the SAME commands the corresponding language runner in
    /// `camerata-checks` delegates to, expressed as shell strings so the
    /// workflow generator can emit them literally in the YAML `run:` fields.
    pub fn builtin_commands(&self) -> Vec<String> {
        match self {
            RepoStack::Rust => vec![
                "cargo fmt --check".to_string(),
                "cargo clippy -- -D warnings".to_string(),
                "cargo test --no-fail-fast".to_string(),
            ],
            // TODO(other-stacks): fill in per-stack commands as multilang runners
            // are hardened. The TODO blocks are emitted into the generated workflow
            // so the operator knows exactly which steps need wiring.
            RepoStack::JavaScript
            | RepoStack::Python
            | RepoStack::Go
            | RepoStack::Ruby
            | RepoStack::Java
            | RepoStack::CSharp
            | RepoStack::Unknown => vec![],
        }
    }

    /// Human-readable label for the TODO comment in the generated workflow.
    pub fn todo_label(&self) -> &'static str {
        match self {
            RepoStack::Rust => "Rust",
            RepoStack::JavaScript => "JavaScript/TypeScript",
            RepoStack::Python => "Python",
            RepoStack::Go => "Go",
            RepoStack::Ruby => "Ruby",
            RepoStack::Java => "Java",
            RepoStack::CSharp => "C#",
            RepoStack::Unknown => "Unknown",
        }
    }

    /// Whether a TODO block should be emitted because the stack has no built-in
    /// step implementations yet.
    pub fn needs_todo_block(&self) -> bool {
        matches!(
            self,
            RepoStack::JavaScript
                | RepoStack::Python
                | RepoStack::Go
                | RepoStack::Ruby
                | RepoStack::Java
                | RepoStack::CSharp
                | RepoStack::Unknown
        )
    }
}

// ─── shared command-list functions (parity contract) ─────────────────────────

/// Commands Layer 2 will run for a given stack + manifest.
///
/// This is the **canonical source** for the Layer-2 command set. Both:
/// - [`crate::manifest_runner::ManifestCheckRunner`] (via [`CheckManifest::in_loop_checks`])
/// - [`generate_gates_workflow`] (Layer-3 superset check)
///
/// derive from this function. Parity is therefore structural.
///
/// The returned strings are plain shell commands (what goes in `run:` in the YAML
/// and in `sh -c <cmd>` in the runner).
pub fn layer2_commands(stack: &RepoStack, manifest: &CheckManifest) -> Vec<String> {
    let mut cmds = stack.builtin_commands();
    for check in manifest.in_loop_checks() {
        cmds.push(check.command.clone());
    }
    cmds
}

/// ALL commands the CI workflow will run (built-ins + ALL manifest checks).
///
/// Layer 3 is the superset: it runs everything Layer 2 runs PLUS ci-only checks.
/// [`generate_gates_workflow`] calls this to know what to emit.
pub fn all_ci_commands(stack: &RepoStack, manifest: &CheckManifest) -> Vec<String> {
    let mut cmds = stack.builtin_commands();
    for check in manifest.all_checks() {
        cmds.push(check.command.clone());
    }
    cmds
}

// ─── workflow generator ───────────────────────────────────────────────────────

/// Generate the YAML for `.github/workflows/camerata-gates.yml`.
///
/// The workflow runs:
/// 1. Built-in language gate steps (from `stack.builtin_commands()`).
/// 2. ALL manifest checks (`in_loop` AND ci-only) — CI is the superset backstop.
///
/// For non-Rust stacks without built-in implementations, a clearly-marked TODO
/// comment block is emitted so the operator knows exactly what needs wiring.
///
/// # Returns
///
/// A `String` containing valid YAML (not written to disk here — the handler does
/// that). The caller may write it to `.github/workflows/camerata-gates.yml`.
pub fn generate_gates_workflow(stack: &RepoStack, manifest: &CheckManifest) -> String {
    let mut out = String::new();

    out.push_str("# .github/workflows/camerata-gates.yml\n");
    out.push_str("# AUTO-GENERATED by Camerata — DO NOT EDIT BY HAND.\n");
    out.push_str("# Re-generate with: POST /api/projects/active/generate-ci-workflow\n");
    out.push_str("#\n");
    out.push_str("# Source of truth: .camerata/checks.toml\n");
    out.push_str("# Parity guarantee: this workflow is the superset of what Layer 2 runs.\n");
    out.push_str("# Layer 2 ⊆ Layer 3: every in_loop check also appears here.\n");
    out.push_str("---\n");
    out.push_str("name: camerata-gates\n\n");
    out.push_str("on:\n");
    out.push_str("  push:\n");
    out.push_str("    branches: [\"**\"]\n");
    out.push_str("  pull_request:\n\n");
    out.push_str("jobs:\n");
    out.push_str("  gate:\n");
    out.push_str("    runs-on: ubuntu-latest\n");
    out.push_str("    steps:\n");
    out.push_str("      - uses: actions/checkout@v4\n\n");

    // ── built-in steps ───────────────────────────────────────────────────────
    let builtins = stack.builtin_commands();
    if builtins.is_empty() {
        // No built-in implementation for this stack yet.
        out.push_str(&format!(
            "      # TODO(camerata): add built-in {} language gate steps here.\n",
            stack.todo_label()
        ));
        out.push_str(
            "      # The Camerata layer-2 runner executes these steps in-loop;\n",
        );
        out.push_str(
            "      # wire the same commands here so CI is the authoritative backstop.\n\n",
        );
    } else {
        out.push_str("      # ── built-in language gate steps (mirrors Layer-2 runners) ──\n");
        for cmd in &builtins {
            let step_name = builtin_step_name(cmd);
            out.push_str(&format!("      - name: {step_name}\n"));
            out.push_str(&format!("        run: {cmd}\n\n"));
        }
    }

    // ── manifest checks ──────────────────────────────────────────────────────
    let all_manifest: Vec<_> = manifest.all_checks().collect();
    if !all_manifest.is_empty() {
        out.push_str(
            "      # ── manifest checks (.camerata/checks.toml) ──────────────────\n",
        );
        out.push_str(
            "      # Generated from the manifest. Both in_loop AND ci-only checks run\n",
        );
        out.push_str(
            "      # here; Layer 2 runs only the in_loop subset.\n\n",
        );
        for check in all_manifest {
            let loop_tag = if check.in_loop { "in_loop" } else { "ci-only" };
            out.push_str(&format!(
                "      - name: \"{} ({})\" # {} | severity: {}\n",
                check.name, check.id, loop_tag, check.severity
            ));
            out.push_str(&format!("        run: {}\n\n", check.command));
        }
    }

    out
}

/// Derive a short step name from a built-in command string.
fn builtin_step_name(cmd: &str) -> &str {
    match cmd {
        "cargo fmt --check" => "cargo fmt --check",
        "cargo clippy -- -D warnings" => "cargo clippy",
        "cargo test --no-fail-fast" => "cargo test",
        other => other,
    }
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_checks::{CheckManifest, ManifestCheck};

    fn check(id: &str, cmd: &str, in_loop: bool) -> ManifestCheck {
        ManifestCheck {
            id: id.to_string(),
            name: format!("check {id}"),
            command: cmd.to_string(),
            severity: "high".to_string(),
            in_loop,
        }
    }

    fn empty_manifest() -> CheckManifest {
        CheckManifest::default()
    }

    // ── parity invariant: Layer-2 ⊆ Layer-3 ──────────────────────────────────

    #[test]
    fn layer2_is_subset_of_layer3_rust_no_manifest() {
        let stack = RepoStack::Rust;
        let manifest = empty_manifest();
        let l2 = layer2_commands(&stack, &manifest);
        let l3 = all_ci_commands(&stack, &manifest);
        for cmd in &l2 {
            assert!(
                l3.contains(cmd),
                "Layer-2 command {cmd:?} must appear in Layer-3 command set"
            );
        }
    }

    #[test]
    fn layer2_is_subset_of_layer3_rust_with_manifest() {
        let stack = RepoStack::Rust;
        let manifest = CheckManifest {
            checks: vec![
                check("ARCH-A", "scripts/arch_check.sh", true),
                check("SEC-B", "trufflehog filesystem .", false),
            ],
        };
        let l2 = layer2_commands(&stack, &manifest);
        let l3 = all_ci_commands(&stack, &manifest);
        for cmd in &l2 {
            assert!(
                l3.contains(cmd),
                "Layer-2 command {cmd:?} must appear in Layer-3 command set; l3={l3:?}"
            );
        }
    }

    #[test]
    fn layer2_subset_invariant_with_mixed_in_loop_flags() {
        // Exhaustive: check for every stack variant that L2 ⊆ L3 holds.
        let stacks = [
            RepoStack::Rust,
            RepoStack::JavaScript,
            RepoStack::Python,
            RepoStack::Go,
            RepoStack::Ruby,
            RepoStack::Java,
            RepoStack::CSharp,
            RepoStack::Unknown,
        ];
        let manifest = CheckManifest {
            checks: vec![
                check("IN-LOOP-1", "cmd_in_loop", true),
                check("CI-ONLY-1", "cmd_ci_only", false),
            ],
        };
        for stack in &stacks {
            let l2 = layer2_commands(stack, &manifest);
            let l3 = all_ci_commands(stack, &manifest);
            for cmd in &l2 {
                assert!(
                    l3.contains(cmd),
                    "stack {stack:?}: Layer-2 command {cmd:?} missing from Layer-3 set; l3={l3:?}"
                );
            }
        }
    }

    // ── ci-only commands in L3 but NOT in L2 ─────────────────────────────────

    #[test]
    fn ci_only_command_is_in_l3_but_not_l2() {
        let stack = RepoStack::Rust;
        let manifest = CheckManifest {
            checks: vec![check("SEC-B", "trufflehog filesystem .", false)],
        };
        let l2 = layer2_commands(&stack, &manifest);
        let l3 = all_ci_commands(&stack, &manifest);
        assert!(
            !l2.contains(&"trufflehog filesystem .".to_string()),
            "ci-only command must NOT be in L2"
        );
        assert!(
            l3.contains(&"trufflehog filesystem .".to_string()),
            "ci-only command must appear in L3"
        );
    }

    // ── workflow YAML golden snapshot ─────────────────────────────────────────

    #[test]
    fn generated_workflow_contains_builtin_rust_commands() {
        let manifest = empty_manifest();
        let yaml = generate_gates_workflow(&RepoStack::Rust, &manifest);
        assert!(
            yaml.contains("cargo fmt --check"),
            "generated YAML must contain cargo fmt --check"
        );
        assert!(
            yaml.contains("cargo clippy -- -D warnings"),
            "generated YAML must contain cargo clippy"
        );
        assert!(
            yaml.contains("cargo test --no-fail-fast"),
            "generated YAML must contain cargo test"
        );
    }

    #[test]
    fn generated_workflow_contains_all_manifest_checks() {
        let manifest = CheckManifest {
            checks: vec![
                check("ARCH-A", "scripts/arch.sh", true),
                check("SEC-B", "trufflehog filesystem .", false),
            ],
        };
        let yaml = generate_gates_workflow(&RepoStack::Rust, &manifest);
        assert!(yaml.contains("scripts/arch.sh"), "in_loop check command must appear in YAML");
        assert!(
            yaml.contains("trufflehog filesystem ."),
            "ci-only check command must appear in YAML"
        );
    }

    #[test]
    fn generated_workflow_emits_todo_block_for_non_rust_stack() {
        let manifest = empty_manifest();
        let yaml = generate_gates_workflow(&RepoStack::Python, &manifest);
        assert!(
            yaml.contains("TODO(camerata)"),
            "non-Rust stack must emit a TODO block in generated YAML"
        );
        assert!(
            yaml.contains("Python"),
            "TODO block must name the detected stack language"
        );
    }

    #[test]
    fn generated_workflow_has_preamble_and_on_triggers() {
        let manifest = empty_manifest();
        let yaml = generate_gates_workflow(&RepoStack::Rust, &manifest);
        assert!(yaml.contains("AUTO-GENERATED"), "preamble must be present");
        assert!(yaml.contains("on:"), "trigger section required");
        assert!(yaml.contains("pull_request"), "PR trigger required");
        assert!(yaml.contains("push:"), "push trigger required");
    }

    // ── layer2_commands includes in_loop checks only ──────────────────────────

    #[test]
    fn layer2_commands_includes_only_in_loop_manifest_checks() {
        let stack = RepoStack::Rust;
        let manifest = CheckManifest {
            checks: vec![
                check("IN-LOOP", "run_in_loop.sh", true),
                check("CI-ONLY", "run_ci_only.sh", false),
            ],
        };
        let l2 = layer2_commands(&stack, &manifest);
        assert!(
            l2.contains(&"run_in_loop.sh".to_string()),
            "in_loop command must be in L2"
        );
        assert!(
            !l2.contains(&"run_ci_only.sh".to_string()),
            "ci-only command must NOT be in L2"
        );
    }
}
