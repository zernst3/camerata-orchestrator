//! camerata-orchestrator-core: the confidence engine's effect-signature classifier.
//!
//! See `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`, the
//! "Architect-orchestrator design (decided 2026-07-10)" section: "The decision CLASS
//! (A/B/C/D) comes from the action's effect-signature at the gate boundary ... detected
//! by the same interception the deny-before-execute gate owns."
//!
//! This crate builds ONLY the substrate: a pure, heavily-tested `classify` function.
//! The orchestrator LOOP that calls it on every proposed action, drives the fleet, and
//! records the outcome is a later phase.
//!
//! # Why effect-signature, never LLM self-report
//! The design's core thesis is that the decision class is never asked of the model —
//! an LLM grading its own confidence is exactly the self-report failure mode
//! governance exists to avoid. The class is instead DERIVED from the action's effect
//! signature: what path is being written, what content it carries, what command is
//! about to run. That is mechanical, deterministic, and independent of anything the
//! model claims about itself. Confidence (a separate, coarser ordinal) is likewise
//! computed from checkable context features (see [`ClassifyCtx`]), never self-reported.
//!
//! # Grounded in the existing gate
//! Several of the Irreversible (C) triggers below are not reimplemented from scratch —
//! they call straight into [`camerata_gateway::lookup_arm`] (the SAME rule arms the
//! write-time deny-before-execute gate runs) and [`camerata_gateway::is_git_state_mutation`],
//! so a write/command this classifier calls Irreversible is denied (or would be denied)
//! by the identical logic the real gate enforces. This keeps the two layers from
//! silently diverging on "what counts as dangerous."
//!
//! # Class D is NOT here
//! The design doc's classification table has a fourth class, "genuine intent
//! ambiguity" (D). [`classify`] never returns a D-equivalent: ambiguity is a property
//! of INTAKE (does the orchestrator understand what the human wants?), not of an
//! action's effect signature. Class D is decided by the intake/clarification machinery
//! (`crates/intake`) BEFORE an [`Action`] like the ones here is even proposed. This is
//! why [`DecisionClass`] is deliberately three-armed, not four.

use camerata_gateway::lookup_arm;

/// Secret detector + scrubber (FOLD E — the chat secret-interceptor). See the
/// module doc for the full design; applied today at `camerata_server`'s
/// `submit_feedback` ingest path.
pub mod secrets;

// ─── decision class + confidence ──────────────────────────────────────────────

/// The confidence-dial's decision class: WHAT catches an autonomously-decided action if
/// it turns out to be wrong. See the module doc and the plan's classification table.
///
/// Deliberately three-armed — Class D (genuine ambiguity) is decided at intake, not
/// here; see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionClass {
    /// A. Mechanically verified: the compiler + gate + Layer-2 checks + integration
    /// gate are the backstop. A wrong pick is suboptimal-but-safe, never unsafe.
    MechanicallyVerified,
    /// B. Cheaply reversible + visible: the live preview is the backstop — a human
    /// sees it and can say "change it" cheaply.
    PreviewReversible,
    /// C. Irreversible / high blast radius: no backstop judges "was this the right
    /// call" — destructive, costly, or escapes review. ALWAYS human-gated; the dial
    /// can never override this (see the module doc).
    Irreversible,
}

impl DecisionClass {
    /// The stable lowercase wire string for this class. This is the vocabulary
    /// `camerata_persistence::OrchestratorDecision::class` stores (kept as a plain
    /// `String` there, mirroring `GovernanceEvent`'s stringly-typed `kind`, so the
    /// persistence crate does not need to depend on this one) — callers convert with
    /// this method at the record-decision call site.
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionClass::MechanicallyVerified => "mechanically_verified",
            DecisionClass::PreviewReversible => "preview_reversible",
            DecisionClass::Irreversible => "irreversible",
        }
    }
}

/// The confidence-dial's confidence ordinal: a COARSE ordinal from checkable features
/// (never an LLM self-report — see module doc), consulted within Class A/B to decide
/// how aggressively the dial auto-decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// The stable lowercase wire string for this confidence ordinal (mirrors
    /// [`DecisionClass::as_str`]'s reasoning).
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

/// One classification result: the decision class + the confidence ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    pub class: DecisionClass,
    pub confidence: Confidence,
}

// ─── action input ──────────────────────────────────────────────────────────────

/// A signal about the CONTENT of a proposed write, when the caller has it in hand.
/// Kept separate from the bare `path` so [`classify`] can run the same content-based
/// checks the gate's rule arms run (a secret literal, a destructive DDL statement)
/// without this crate re-parsing anything the gate does not already know how to parse.
#[derive(Debug, Clone, Default)]
pub struct ContentSignal {
    /// The full text content of the write, when available (e.g. a `gated_write`
    /// call's `content` field). `None` when the caller does not have it in hand —
    /// classification then falls back to path-only checks.
    pub content: Option<String>,
    /// Whether this write is known to introduce a NEW dependency (crate/package) not
    /// previously present. Only consulted for a `Cargo.toml` write; irrelevant
    /// otherwise. `None` (unknown) is treated conservatively — see
    /// `classify_write`'s doc for why.
    pub adds_dependency: Option<bool>,
}

/// One action the orchestrator is about to take, described STRUCTURALLY (never as raw
/// LLM prose) so [`classify`] can pattern-match on it deterministically.
///
/// Deliberately extensible: this is the seam a later phase adds more variants to (e.g.
/// an outbound-network-request action) as the orchestrator's action surface grows —
/// adding a variant is additive to `classify`'s existing arms, not a rewrite of them.
#[derive(Debug, Clone)]
pub enum Action {
    /// A `gated_write`-shaped file write: the same call the gate already intercepts.
    Write {
        path: String,
        content_signal: ContentSignal,
    },
    /// A shell/subprocess invocation. `args` are already split (not a single shell
    /// string), so `classify` never has to resolve shell-quoting ambiguity itself.
    Command { program: String, args: Vec<String> },
}

/// Context [`classify`] consults for the CONFIDENCE ordinal (never the class — the
/// class is effect-signature-only; see module doc).
#[derive(Debug, Clone, Copy, Default)]
pub struct ClassifyCtx {
    /// True when the action's target sits inside the vetted-skeleton's known
    /// structure (the scaffolded template's pre-approved layout) — the strongest
    /// positive confidence signal (per the design doc: "the vetted skeleton is part
    /// of the backstop").
    pub inside_vetted_skeleton: bool,
    /// True when the action would introduce a dependency that is NOT in the vetted
    /// skeleton's pre-approved dependency set.
    pub adds_out_of_vetted_dep: bool,
    /// True when this decision SHAPE (a caller-defined key, e.g. "which rule option to
    /// pick for X") has been redirected by a human before. A repeat redirect is a
    /// stronger negative signal than a first-time out-of-vetted-set dependency, so it
    /// takes priority in [`classify`]'s confidence ordering.
    pub previously_redirected: bool,
}

// ─── classify ───────────────────────────────────────────────────────────────────

/// Classify one proposed action: derive its decision CLASS from its effect signature
/// (never from what the model claims about itself) and its CONFIDENCE from checkable
/// context features. Pure — no I/O, no async, same input always yields the same
/// output.
pub fn classify(action: &Action, ctx: &ClassifyCtx) -> Classification {
    Classification {
        class: classify_effect(action),
        confidence: classify_confidence(ctx),
    }
}

fn classify_effect(action: &Action) -> DecisionClass {
    match action {
        Action::Write { path, content_signal } => classify_write(path, content_signal),
        Action::Command { program, args } => classify_command(program, args),
    }
}

/// Confidence rule (see [`ClassifyCtx`]'s field docs for the reasoning behind each
/// factor):
/// - `previously_redirected` -> [`Confidence::Low`] (a repeat redirect is the
///   strongest negative signal, so it is checked first and short-circuits).
/// - `adds_out_of_vetted_dep`, OR not `inside_vetted_skeleton` -> [`Confidence::Medium`]
///   (a red flag, or simply the absence of positive vetted-structure confirmation).
/// - otherwise (inside the vetted skeleton, no out-of-vetted-set dep) ->
///   [`Confidence::High`].
fn classify_confidence(ctx: &ClassifyCtx) -> Confidence {
    if ctx.previously_redirected {
        Confidence::Low
    } else if ctx.adds_out_of_vetted_dep || !ctx.inside_vetted_skeleton {
        Confidence::Medium
    } else {
        Confidence::High
    }
}

/// Classify a proposed file write.
///
/// Irreversible (C) triggers, checked first (fail-closed: the FIRST matching trigger
/// wins, mirroring `camerata_gateway::evaluate_call`'s "first denial wins" ordering):
/// - a `migrations/` path, or a `*.sql` file (extension-based, independent of
///   directory — a stray `.sql` file outside `migrations/` is still a migration in
///   spirit).
/// - a `terraform/` path, or a `*.tf` file. Deliberately OVER-inclusive (any file
///   under a `terraform/` directory, not just `.tf` ones, and any `.tf` file
///   regardless of directory) — a false negative here (missing an infra change) is a
///   bigger risk than a false positive.
/// - a secret-bearing file name (`.env`, private keys, keystores): reuses
///   `SEC-NO-SECRET-FILES-1`'s own rule arm via [`lookup_arm`], so this agrees
///   byte-for-byte with what the write-time gate would deny.
/// - a path that escapes the worktree jail (`..` traversal, or a `.git`/`.ssh`
///   internals write): reuses `SEC-NO-PATH-ESCAPE-1`'s rule arm the same way.
/// - a `Cargo.toml` dependency change (adds a dep). DECISIONS NEEDED (flagged, minimal
///   safe choice made): detecting "ADDS a dependency" precisely requires a diff
///   against the file's previous content, which this pure classifier does not have.
///   [`ContentSignal::adds_dependency`] lets a caller that DOES have the diff clear
///   this explicitly (`Some(false)`); absent that, ANY `Cargo.toml` write is
///   conservatively treated as Irreversible — the ambiguous case (a write this
///   classifier cannot positively clear) is exactly the one this rule exists to catch.
/// - a destructive DDL statement (`DROP TABLE`) anywhere in the written content —
///   defense in depth alongside the migrations/`*.sql` path check above, since a
///   destructive statement pasted into a non-migrations file is still destructive.
///
/// Below the irreversible floor, which backstop covers the write:
/// - `.rs` files under `src/pages/` or `src/components/` (the vetted skeleton's own
///   view-module directories — see `crates/scaffold/templates/skeleton`) are
///   [`DecisionClass::PreviewReversible`]; DECISIONS NEEDED (flagged, minimal safe
///   choice made): the design doc's example for Class B is "rsx/view modules", but in
///   this Dioxus skeleton view logic lives in ordinary `.rs` files (Dioxus has no
///   separate view-file extension — `rsx!` is a macro used inline), so this grounds
///   "view module" in the ACTUAL vetted skeleton's directory structure rather than a
///   file extension that does not exist for Dioxus.
/// - any OTHER `.rs` file is [`DecisionClass::MechanicallyVerified`] (server/logic —
///   the compiler is the backstop).
/// - any NON-`.rs` file (`.css`, markup, config, JS, ...) defaults to
///   [`DecisionClass::PreviewReversible`], since none of those have a compiler
///   backstop either and the live preview is what actually catches them.
fn classify_write(path: &str, signal: &ContentSignal) -> DecisionClass {
    let lower = path.to_ascii_lowercase();
    let content = signal.content.as_deref().unwrap_or("");

    if has_path_segment(&lower, "migrations") || lower.ends_with(".sql") {
        return DecisionClass::Irreversible;
    }
    if has_path_segment(&lower, "terraform") || lower.ends_with(".tf") {
        return DecisionClass::Irreversible;
    }
    if lookup_arm("SEC-NO-SECRET-FILES-1").is_some_and(|arm| arm(path, content).is_err()) {
        return DecisionClass::Irreversible;
    }
    if lookup_arm("SEC-NO-PATH-ESCAPE-1").is_some_and(|arm| arm(path, content).is_err()) {
        return DecisionClass::Irreversible;
    }
    if file_name(path) == Some("Cargo.toml") && signal.adds_dependency != Some(false) {
        return DecisionClass::Irreversible;
    }
    if content.to_ascii_uppercase().contains("DROP TABLE") {
        return DecisionClass::Irreversible;
    }

    if lower.ends_with(".rs") {
        if is_view_module_path(&lower) {
            DecisionClass::PreviewReversible
        } else {
            DecisionClass::MechanicallyVerified
        }
    } else {
        DecisionClass::PreviewReversible
    }
}

/// Classify a proposed command invocation.
///
/// Irreversible (C) triggers:
/// - `terraform apply` / `terraform destroy`.
/// - a cloud-CLI deploy: `az` / `gcloud` / `aws` with a `deploy` argument.
/// - a git operation that mutates working-tree state (`git stash`, `git reset --hard`,
///   `git clean -f`/`-d`, `git checkout -- `/`git checkout .`, `git rebase`, `git
///   restore`, `git worktree remove`): reuses [`camerata_gateway::is_git_state_mutation`]
///   directly, applying the gate's existing content-scoped guard to a proposed
///   COMMAND instead.
/// - `git push --force` / `--force-with-lease`: NOT covered by
///   `is_git_state_mutation` (that guard is scoped to LOCAL working-tree state); a
///   force-push rewrites SHARED remote history, an external side effect outside this
///   workspace entirely.
/// - `rm` with both a recursive flag (`-r`/`-R`/`--recursive`, or a short-flag
///   combination containing `r`) and a force flag (`-f`/`--force`, or a short-flag
///   combination containing `f`) — covers `rm -rf`, `rm -fr`, and `rm -r -f`.
/// - a destructive DDL statement (`DROP TABLE`) in any argument — the command-side
///   mirror of [`classify_write`]'s content check (e.g. `psql -c "DROP TABLE ..."`).
///
/// DECISIONS NEEDED (flagged, NOT implemented): the design doc also lists "an
/// external network side effect" as an Irreversible command trigger, but gives no
/// concrete verb to pattern-match (unlike terraform/az/gcloud/aws/git/rm, which are
/// named explicitly). Guessing at a curl/wget/http-client heuristic here would be
/// inventing a new rule with no grounding in the existing gate or the plan doc, so it
/// is deliberately left for a future `Action` variant (e.g. an explicit
/// outbound-network-request action) with a real call site to ground it against.
///
/// Below the irreversible floor: today's fleet only runs read/idempotent tooling
/// commands (`cargo build`/`check`/`test`/`clippy`/`fmt`, `git status`/`diff`/`log`,
/// `dx serve`), which the compiler/test run itself backstops — so an unmatched
/// command defaults to [`DecisionClass::MechanicallyVerified`]. If a broader shell
/// surface is exposed later this default needs revisiting.
fn classify_command(program: &str, args: &[String]) -> DecisionClass {
    if program == "terraform" && args.iter().any(|a| a == "apply" || a == "destroy") {
        return DecisionClass::Irreversible;
    }
    if matches!(program, "az" | "gcloud" | "aws") && args.iter().any(|a| a == "deploy") {
        return DecisionClass::Irreversible;
    }
    if program == "git" {
        let joined = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        if camerata_gateway::is_git_state_mutation(&joined) {
            return DecisionClass::Irreversible;
        }
        let is_push = args.first().map(String::as_str) == Some("push");
        let is_forced = args
            .iter()
            .any(|a| a == "--force" || a == "-f" || a == "--force-with-lease");
        if is_push && is_forced {
            return DecisionClass::Irreversible;
        }
    }
    if program == "rm" && rm_is_recursive_and_forced(args) {
        return DecisionClass::Irreversible;
    }
    if args.iter().any(|a| a.to_ascii_uppercase().contains("DROP TABLE")) {
        return DecisionClass::Irreversible;
    }

    DecisionClass::MechanicallyVerified
}

/// True when `args` (an `rm` invocation's arguments) carry BOTH a recursive flag and a
/// force flag, in any combination: a single combined short flag (`-rf`, `-fr`), or two
/// separate flags (`-r -f`, `-r --force`, `--recursive -f`, ...).
fn rm_is_recursive_and_forced(args: &[String]) -> bool {
    let is_short_flag = |a: &str| a.starts_with('-') && !a.starts_with("--");
    let has_recursive = args
        .iter()
        .any(|a| a == "--recursive" || (is_short_flag(a) && a.contains('r')));
    let has_force = args
        .iter()
        .any(|a| a == "--force" || (is_short_flag(a) && a.contains('f')));
    has_recursive && has_force
}

/// True when a `.rs` write path sits under a view-module directory of the vetted
/// skeleton (`src/pages/` or `src/components/` — see
/// `crates/scaffold/templates/skeleton`). `lower_path` must already be lowercased.
fn is_view_module_path(lower_path: &str) -> bool {
    has_path_segment(lower_path, "pages") || has_path_segment(lower_path, "components")
}

/// The last path segment (the file name), splitting on both `/` and `\`. Compares
/// against the ORIGINAL (non-lowercased) path — `Cargo.toml` is a case-sensitive
/// canonical file name.
fn file_name(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).next()
}

/// True when any `/`- or `\`-separated segment of `path` equals `segment` exactly.
fn has_path_segment(path: &str, segment: &str) -> bool {
    path.split(['/', '\\']).any(|s| s == segment)
}

// ─────────────────────────────────────────────────────────────────────────────────
// Tests (ORCH-NEW-PATH-TESTS-1): every classify_write / classify_command /
// classify_confidence branch gets a concrete case.
// ─────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &str) -> Action {
        Action::Write {
            path: path.to_string(),
            content_signal: ContentSignal::default(),
        }
    }

    fn write_with_content(path: &str, content: &str) -> Action {
        Action::Write {
            path: path.to_string(),
            content_signal: ContentSignal {
                content: Some(content.to_string()),
                adds_dependency: None,
            },
        }
    }

    fn command(program: &str, args: &[&str]) -> Action {
        Action::Command {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn default_ctx() -> ClassifyCtx {
        ClassifyCtx::default()
    }

    // ── write: Irreversible (C) triggers ────────────────────────────────────

    #[test]
    fn write_under_migrations_dir_is_irreversible() {
        let a = write("migrations/0001_init.rs");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_sql_file_anywhere_is_irreversible() {
        let a = write("src/schema.sql");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_under_terraform_dir_is_irreversible() {
        let a = write("terraform/README.md");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_tf_file_anywhere_is_irreversible() {
        let a = write("infra/main.tf");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_dotenv_file_is_irreversible() {
        let a = write(".env");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_private_key_file_is_irreversible() {
        let a = write("secrets/id_rsa");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_path_traversal_is_irreversible() {
        let a = write("../outside/file.rs");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_into_dot_git_is_irreversible() {
        let a = write(".git/hooks/pre-commit");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_cargo_toml_with_unknown_dependency_signal_is_irreversible() {
        // Conservative default: `adds_dependency` unset (None) — cannot be positively
        // cleared, so it is treated as Irreversible.
        let a = write("Cargo.toml");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_cargo_toml_with_dependency_add_explicitly_true_is_irreversible() {
        let a = Action::Write {
            path: "Cargo.toml".to_string(),
            content_signal: ContentSignal {
                content: None,
                adds_dependency: Some(true),
            },
        };
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_cargo_toml_with_dependency_add_explicitly_cleared_is_not_irreversible() {
        // The caller positively knows no dependency was added (e.g. a version bump) —
        // Cargo.toml is not `.rs`, so it falls through to the PreviewReversible default.
        let a = Action::Write {
            path: "Cargo.toml".to_string(),
            content_signal: ContentSignal {
                content: None,
                adds_dependency: Some(false),
            },
        };
        assert_eq!(classify_effect(&a), DecisionClass::PreviewReversible);
    }

    #[test]
    fn write_drop_table_in_content_anywhere_is_irreversible() {
        let a = write_with_content("src/admin.rs", "let sql = \"DROP TABLE users\";");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn write_drop_table_case_insensitive_is_irreversible() {
        let a = write_with_content("notes.txt", "remember to drop table sessions later");
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    // ── write: below the floor — A vs. B split ──────────────────────────────

    #[test]
    fn write_rs_file_under_pages_is_preview_reversible() {
        let a = write("src/pages/home.rs");
        assert_eq!(classify_effect(&a), DecisionClass::PreviewReversible);
    }

    #[test]
    fn write_rs_file_under_components_is_preview_reversible() {
        let a = write("src/components/button.rs");
        assert_eq!(classify_effect(&a), DecisionClass::PreviewReversible);
    }

    #[test]
    fn write_rs_file_elsewhere_is_mechanically_verified() {
        let a = write("src/server.rs");
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn write_rs_file_at_root_is_mechanically_verified() {
        let a = write("src/main.rs");
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn write_css_file_is_preview_reversible() {
        let a = write("assets/styles/index.css");
        assert_eq!(classify_effect(&a), DecisionClass::PreviewReversible);
    }

    #[test]
    fn write_other_extension_defaults_to_preview_reversible() {
        let a = write("assets/manifest.json");
        assert_eq!(classify_effect(&a), DecisionClass::PreviewReversible);
    }

    // ── command: Irreversible (C) triggers ──────────────────────────────────

    #[test]
    fn terraform_apply_is_irreversible() {
        let a = command("terraform", &["apply"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn terraform_destroy_is_irreversible() {
        let a = command("terraform", &["destroy", "-auto-approve"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn terraform_plan_is_not_irreversible() {
        let a = command("terraform", &["plan"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn az_deploy_is_irreversible() {
        let a = command("az", &["webapp", "deploy"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn gcloud_deploy_is_irreversible() {
        let a = command("gcloud", &["app", "deploy"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn aws_deploy_is_irreversible() {
        let a = command("aws", &["deploy", "create-deployment"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn az_non_deploy_command_is_not_irreversible() {
        let a = command("az", &["account", "show"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn git_reset_hard_is_irreversible() {
        let a = command("git", &["reset", "--hard", "HEAD"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn git_stash_is_irreversible() {
        let a = command("git", &["stash"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn git_push_force_is_irreversible() {
        let a = command("git", &["push", "--force", "origin", "main"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn git_push_force_with_lease_is_irreversible() {
        let a = command("git", &["push", "--force-with-lease"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn git_push_without_force_is_not_irreversible() {
        let a = command("git", &["push", "origin", "main"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn git_status_is_not_irreversible() {
        let a = command("git", &["status"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn rm_rf_combined_flag_is_irreversible() {
        let a = command("rm", &["-rf", "/tmp/some/dir"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn rm_fr_combined_flag_is_irreversible() {
        let a = command("rm", &["-fr", "/tmp/some/dir"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn rm_separate_recursive_and_force_flags_is_irreversible() {
        let a = command("rm", &["-r", "-f", "/tmp/some/dir"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn rm_long_flags_is_irreversible() {
        let a = command("rm", &["--recursive", "--force", "/tmp/some/dir"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn rm_without_force_is_not_irreversible() {
        let a = command("rm", &["-r", "/tmp/some/dir"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn rm_single_file_no_flags_is_not_irreversible() {
        let a = command("rm", &["file.txt"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn command_arg_drop_table_is_irreversible() {
        let a = command("psql", &["-c", "DROP TABLE users"]);
        assert_eq!(classify_effect(&a), DecisionClass::Irreversible);
    }

    #[test]
    fn unmatched_command_defaults_to_mechanically_verified() {
        let a = command("cargo", &["build", "--workspace"]);
        assert_eq!(classify_effect(&a), DecisionClass::MechanicallyVerified);
    }

    // ── confidence ───────────────────────────────────────────────────────────

    #[test]
    fn confidence_high_when_inside_vetted_skeleton_with_no_flags() {
        let ctx = ClassifyCtx {
            inside_vetted_skeleton: true,
            adds_out_of_vetted_dep: false,
            previously_redirected: false,
        };
        assert_eq!(classify_confidence(&ctx), Confidence::High);
    }

    #[test]
    fn confidence_medium_when_adds_out_of_vetted_dep() {
        let ctx = ClassifyCtx {
            inside_vetted_skeleton: true,
            adds_out_of_vetted_dep: true,
            previously_redirected: false,
        };
        assert_eq!(classify_confidence(&ctx), Confidence::Medium);
    }

    #[test]
    fn confidence_medium_when_outside_vetted_skeleton() {
        let ctx = ClassifyCtx {
            inside_vetted_skeleton: false,
            adds_out_of_vetted_dep: false,
            previously_redirected: false,
        };
        assert_eq!(classify_confidence(&ctx), Confidence::Medium);
    }

    #[test]
    fn confidence_low_when_previously_redirected_even_if_otherwise_clean() {
        let ctx = ClassifyCtx {
            inside_vetted_skeleton: true,
            adds_out_of_vetted_dep: false,
            previously_redirected: true,
        };
        assert_eq!(classify_confidence(&ctx), Confidence::Low);
    }

    #[test]
    fn confidence_low_takes_priority_over_out_of_vetted_dep() {
        let ctx = ClassifyCtx {
            inside_vetted_skeleton: false,
            adds_out_of_vetted_dep: true,
            previously_redirected: true,
        };
        assert_eq!(classify_confidence(&ctx), Confidence::Low);
    }

    // ── classify(): class + confidence combined ─────────────────────────────

    #[test]
    fn classify_combines_class_and_confidence() {
        let a = write("src/server.rs");
        let ctx = ClassifyCtx {
            inside_vetted_skeleton: true,
            adds_out_of_vetted_dep: false,
            previously_redirected: false,
        };
        let result = classify(&a, &ctx);
        assert_eq!(result.class, DecisionClass::MechanicallyVerified);
        assert_eq!(result.confidence, Confidence::High);
    }

    #[test]
    fn classify_irreversible_action_still_reports_a_confidence_ordinal() {
        // Class C is ALWAYS human-gated regardless of confidence (the dial cannot
        // override it — see module doc), but `classify` still returns a well-formed
        // Classification rather than a special-cased "N/A".
        let a = write("migrations/0001_init.sql");
        let ctx = default_ctx();
        let result = classify(&a, &ctx);
        assert_eq!(result.class, DecisionClass::Irreversible);
        assert_eq!(result.confidence, Confidence::Medium); // default ctx: not inside vetted skeleton
    }

    // ── as_str() wire vocabulary ─────────────────────────────────────────────

    #[test]
    fn decision_class_as_str_is_stable() {
        assert_eq!(DecisionClass::MechanicallyVerified.as_str(), "mechanically_verified");
        assert_eq!(DecisionClass::PreviewReversible.as_str(), "preview_reversible");
        assert_eq!(DecisionClass::Irreversible.as_str(), "irreversible");
    }

    #[test]
    fn confidence_as_str_is_stable() {
        assert_eq!(Confidence::High.as_str(), "high");
        assert_eq!(Confidence::Medium.as_str(), "medium");
        assert_eq!(Confidence::Low.as_str(), "low");
    }
}
