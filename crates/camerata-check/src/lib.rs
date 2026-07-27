//! `camerata-check` — the Layer-3 CI-parity distributable.
//!
//! See `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.4 for the gap this
//! closes: the native architectural checkers (`camerata_checks::arch_checker::all_checkers`)
//! run inside Camerata (the brownfield scan and the Layer-2 governed-dev loop), but the
//! generated client-side `.github/workflows/camerata-gates.yml` runs in the CLIENT's own
//! GitHub Actions, where no Camerata binary exists — so native checkers couldn't enforce
//! there. This crate is that missing distributable: a small, standalone binary a client's CI
//! can invoke directly, with **no** dependency on `camerata-server` (no HTTP, no DB, no
//! GitHub client — none of that is available, or needed, in a bare CI runner).
//!
//! # Reuse, not duplication
//!
//! Every piece of actual checking logic is reused verbatim from `camerata-checks`:
//! - The checker registry: [`camerata_checks::arch_checker::all_checkers`].
//! - The shared input currency: [`camerata_checks::arch_checker::RepoView`].
//! - The "does this checker even apply" gate: [`camerata_checks::arch_checker::checker_applies`].
//! - The pruned, glob-scoped file walk: [`camerata_checks::arch_check_runner::collect_interest_files`]
//!   — the EXACT function [`camerata_checks::NativeArchCheckRunner`] (the Layer-2 runner) uses,
//!   made `pub` for this crate to reuse rather than re-implementing a second file walk.
//!
//! This crate itself contributes only: CLI plumbing, an optional `--config` override, the
//! deterministic-vs-needs-review split for CI exit-code semantics, and human/JSON rendering.
//!
//! # D3 (config-aware degradation) parity
//!
//! Nothing special is done here to "honor" D3 — it falls out for free. Every config-gated
//! checker (`ImportBoundaryChecker`, `HandlerNoDbChecker`, `StrictLayeringCallChecker`) already
//! degrades to zero deterministic findings when `.camerata/architecture.toml` is absent or
//! malformed (`architecture_config_from_files` returns `Ok(None)`/`Err`, and every one of those
//! checkers' own `check()` treats both as "abstain"). Since this binary calls the exact same
//! `check()` methods over the exact same [`RepoView`](camerata_checks::arch_checker::RepoView)
//! shape the Layer-2 runner and the scan use, a repo with no config gets the identical
//! "unconfigured — stays silent" behavior here, with no extra logic required.
//!
//! # Deterministic vs. needs-review (the exit-code split)
//!
//! A CI gate must only hard-fail on what's deterministically true — that's the whole point of
//! the deterministic/advisory split this system is built on (see the design memo's own framing
//! and `docs/design/2026-07-27_ast-extractor-layer.md` §0's D3). Two independent signals mark a
//! violation as "needs-review" rather than a hard verdict, both ALREADY established elsewhere
//! in this codebase (not invented here):
//!
//! 1. [`camerata_checks::arch_checker::ArchChecker::advisory_coexisting`] — a checker whose
//!    findings are `needs-review` UNCONDITIONALLY, by the rule's own design (today:
//!    `ResourceLifecycleChecker`'s spawn facet).
//! 2. The `[needs review` message-suffix convention (`crates/ui_core/src/rules.rs`'s
//!    `split_needs_review`; e.g. `HandlerNoDbChecker`'s attribute/name-fallback tiers embed
//!    `"[needs review: ...]"` in the message). This crate doesn't depend on `camerata-ui-core`
//!    (that crate is UI-rendering-logic-flavored and pulling it in for one string match would be
//!    an odd cross-domain dependency for a CI binary) — [`NEEDS_REVIEW_MARKER`] mirrors the
//!    exact same literal marker `split_needs_review` matches on
//!    (`detail.rfind("[needs review")`), so the two stay in lockstep by convention, the same way
//!    every checker that emits the marker already keeps in lockstep with the UI's parser today.
//!
//! Default CI behavior: only violations that are neither (1) nor (2) — genuinely deterministic,
//! hard verdicts — fail the build ([`RunReport::exit_code`] with `strict = false`). Pass
//! `--strict` to also fail on needs-review-grade findings.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use camerata_checks::arch_check_runner::collect_interest_files;
use camerata_checks::arch_checker::{all_checkers, checker_applies, RepoView};
use camerata_checks::architecture_config::ARCHITECTURE_CONFIG_PATH;

/// The message-suffix marker that flags a violation as `needs-review` rather than a hard
/// deterministic verdict — see the module doc's "Deterministic vs. needs-review" section.
/// Mirrors `ui_core::rules::split_needs_review`'s own literal (`"[needs review"`, no closing
/// bracket in the match — the UI's parser also just does a `rfind` on this open prefix).
pub const NEEDS_REVIEW_MARKER: &str = "[needs review";

/// The exit code this binary uses when it could not even complete a run (unreadable repo
/// path, an explicit `--config` override that doesn't exist, a rendering failure). Distinct
/// from the CI-gate exit codes (0 = clean, 1 = deterministic violation found) so a CI author
/// can tell "the gate ran and found a problem" apart from "the gate itself is broken."
pub const EXIT_CODE_RUN_ERROR: i32 = 2;

/// What to check and how — the fully-resolved input to [`run`]. Built by `main.rs` from CLI
/// flags; kept as a plain struct (not `clap`-derived) so [`run`] has no `clap` dependency and
/// tests can call it directly without going through argument parsing at all.
#[derive(Debug, Clone)]
pub struct Options {
    /// The repo (or worktree) root to scan. Everything is repo-relative from here.
    pub repo_root: PathBuf,
    /// An explicit path to a `.camerata/architecture.toml` to use INSTEAD of (or in addition
    /// to, if the repo also has one — this wins) whatever the walk finds under
    /// `repo_root/.camerata/architecture.toml`. Useful when the config lives outside the
    /// scanned directory (e.g. a monorepo subdir CI job scanning one package but sharing a
    /// root-level boundary map).
    pub config_override: Option<PathBuf>,
    /// Restrict to these corpus rule ids. Empty (the default) runs every registered checker.
    /// A checker runs iff at least one of its own `rule_ids()` is in this set.
    pub rule_ids: Vec<String>,
}

/// One violation, in the shape this crate's CLI renders (human and JSON alike). A direct,
/// stable projection of [`camerata_checks::arch_checker::ArchViolation`] plus the
/// [`needs_review`](Self::needs_review) verdict-class bit computed per the module doc.
///
/// # JSON schema stability
///
/// This is the wire shape a downstream CI step parses (`jq`, a custom script, ...). Field
/// names and types are covered by [`json_schema_is_stable`] (this crate's test suite) — never
/// rename or retype a field without treating it as a breaking change for every CI config that
/// consumes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ViolationJson {
    pub rule_id: String,
    pub file: String,
    pub line: usize,
    pub object: Option<String>,
    pub message: String,
    pub severity: String,
    pub needs_review: bool,
}

/// The full result of one [`run`] — what gets rendered (human or JSON) and what
/// [`Self::exit_code`] decides the process exit status from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunReport {
    /// The scanned repo root, as given (display form).
    pub repo: String,
    /// How many registered checkers were selected to run (after any `--rule-id` filter).
    pub checkers_run: usize,
    /// `--rule-id` values that were requested but that NO registered checker answers at all
    /// (deterministic or advisory) — e.g. a typo, or one of the two Group E rules
    /// (`ARCH-STRUCTURED-ERRORS-1`, `ARCH-EXACT-DECIMALS-1`) that stay explicitly deferred to
    /// AI review (see `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group E). Surfaced so
    /// a CI author gets an explicit signal instead of a silent no-op.
    pub unmatched_rule_ids: Vec<String>,
    pub violations: Vec<ViolationJson>,
    /// Violations that are hard deterministic verdicts (neither `advisory_coexisting` nor
    /// `[needs review`-marked). This is what a non-`--strict` run fails the build on.
    pub deterministic_count: usize,
    /// Violations that are needs-review grade (excluded from `deterministic_count`).
    pub needs_review_count: usize,
    /// `true` iff `violations` is empty. Convenience mirror of `violations.is_empty()` so a
    /// JSON consumer doesn't have to check array length for the common case.
    pub clean: bool,
}

impl RunReport {
    /// The process exit code for this report: standard CI gate semantics.
    ///
    /// - `strict = false` (default): non-zero iff `deterministic_count > 0`. Needs-review-grade
    ///   findings alone never fail the build — that's the entire point of keeping the two
    ///   classes separate (see the module doc).
    /// - `strict = true`: non-zero iff ANY violation exists, deterministic or needs-review.
    pub fn exit_code(&self, strict: bool) -> i32 {
        let should_fail = if strict {
            !self.violations.is_empty()
        } else {
            self.deterministic_count > 0
        };
        i32::from(should_fail)
    }
}

/// Whether an [`camerata_checks::arch_checker::ArchViolation`]'s message carries the
/// needs-review marker — see the module doc.
fn message_is_needs_review(message: &str) -> bool {
    message.contains(NEEDS_REVIEW_MARKER)
}

/// Run every selected checker (per `opts.rule_ids`) against `opts.repo_root` and build the
/// full [`RunReport`]. Never panics: an unreadable subdirectory or file is skipped by
/// [`collect_interest_files`] (already panic-free by construction); a malformed
/// `--config` override or malformed `.camerata/architecture.toml` degrades to "unconfigured"
/// inside each checker (D3), never an `Err` from here. The only `Err` this returns is a
/// genuinely fatal setup problem: `--config` was given but the path can't be read at all.
pub fn run(opts: &Options) -> anyhow::Result<RunReport> {
    let all = all_checkers();

    let selected: Vec<_> = if opts.rule_ids.is_empty() {
        all.iter().collect()
    } else {
        let wanted: HashSet<&str> = opts.rule_ids.iter().map(String::as_str).collect();
        all.iter()
            .filter(|c| c.rule_ids().iter().any(|id| wanted.contains(id)))
            .collect()
    };

    let unmatched_rule_ids: Vec<String> = if opts.rule_ids.is_empty() {
        Vec::new()
    } else {
        let covered: HashSet<&str> = all
            .iter()
            .flat_map(|c| c.rule_ids().iter().copied())
            .collect();
        opts.rule_ids
            .iter()
            .filter(|id| !covered.contains(id.as_str()))
            .cloned()
            .collect()
    };

    let mut globs: Vec<&str> = selected
        .iter()
        .flat_map(|c| c.interest_globs().iter().copied())
        .collect();
    globs.sort_unstable();
    globs.dedup();

    let mut files = collect_interest_files(&opts.repo_root, &globs);
    apply_config_override(&mut files, opts.config_override.as_deref())?;

    let repo_label = opts.repo_root.display().to_string();
    let view = RepoView {
        spec: &repo_label,
        files: &files,
    };

    let mut violations = Vec::new();
    for checker in &selected {
        // Zero matching files -> skip; never a false "ran and found nothing" read as "clean"
        // (same "interest_globs, cheap" contract the Layer-2 runner and scan both honor).
        if !checker_applies(checker.as_ref(), &files) {
            continue;
        }
        let advisory = checker.advisory_coexisting();
        for v in checker.check(&view) {
            let needs_review = advisory || message_is_needs_review(&v.message);
            violations.push(ViolationJson {
                rule_id: v.rule_id,
                file: v.file,
                line: v.line,
                object: v.object,
                message: v.message,
                severity: v.severity.to_string(),
                needs_review,
            });
        }
    }
    // Stable, deterministic ordering regardless of checker registration order — a downstream
    // CI script diffing output run-to-run shouldn't see spurious reordering.
    violations.sort_by(|a, b| (&a.file, a.line, &a.rule_id).cmp(&(&b.file, b.line, &b.rule_id)));

    let deterministic_count = violations.iter().filter(|v| !v.needs_review).count();
    let needs_review_count = violations.len() - deterministic_count;
    let clean = violations.is_empty();

    Ok(RunReport {
        repo: repo_label,
        checkers_run: selected.len(),
        unmatched_rule_ids,
        violations,
        deterministic_count,
        needs_review_count,
        clean,
    })
}

/// If `config_override` is given, read it from disk and splice its content into `files` under
/// the canonical [`ARCHITECTURE_CONFIG_PATH`] key, REPLACING any entry the walk itself already
/// collected at that path (an explicit `--config` always wins over whatever's on disk at the
/// default location). Non-UTF8 bytes are lossily decoded, never rejected — consistent with
/// every other adversarial-input path in this checker family.
///
/// The only `Err` case: the override path itself can't be read at all (missing / permission
/// denied) — that's a genuine CLI usage error, not a "degrade gracefully" case, since the user
/// explicitly asked for this exact file.
fn apply_config_override(
    files: &mut Vec<(String, String)>,
    config_override: Option<&Path>,
) -> anyhow::Result<()> {
    let Some(path) = config_override else {
        return Ok(());
    };
    let bytes = std::fs::read(path)
        .with_context(|| format!("--config override at {} could not be read", path.display()))?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    files.retain(|(p, _)| p != ARCHITECTURE_CONFIG_PATH);
    files.push((ARCHITECTURE_CONFIG_PATH.to_string(), content));
    Ok(())
}

/// Render a [`RunReport`] as the human-readable CLI output.
pub fn render_human(report: &RunReport) -> String {
    let mut out = String::new();
    if report.clean {
        out.push_str(&format!(
            "camerata-check: clean — {} checker(s) ran over {}, no violations\n",
            report.checkers_run, report.repo
        ));
    } else {
        out.push_str(&format!(
            "camerata-check: {} violation(s) in {} ({} deterministic, {} needs-review)\n\n",
            report.violations.len(),
            report.repo,
            report.deterministic_count,
            report.needs_review_count
        ));
        for v in &report.violations {
            let tag = if v.needs_review {
                "needs-review"
            } else {
                "deterministic"
            };
            let obj = v
                .object
                .as_deref()
                .map(|o| format!(" {o}"))
                .unwrap_or_default();
            if v.line > 0 {
                out.push_str(&format!(
                    "{}:{} [{}] [{}]{} — {}\n",
                    v.file, v.line, tag, v.rule_id, obj, v.message
                ));
            } else {
                out.push_str(&format!(
                    "{} [{}] [{}]{} — {}\n",
                    v.file, tag, v.rule_id, obj, v.message
                ));
            }
        }
    }
    if !report.unmatched_rule_ids.is_empty() {
        out.push_str(&format!(
            "\nNote: no registered checker answers: {} (no native checker exists for this rule id — \
             it stays AI-advisory only; see docs/design/2026-07-27_ast-extractor-layer.md §4 Group E \
             for the two rules deliberately deferred this way).\n",
            report.unmatched_rule_ids.join(", ")
        ));
    }
    out
}

/// Render a [`RunReport`] as pretty-printed JSON — the stable machine-readable schema (see
/// [`ViolationJson`]'s doc).
pub fn render_json(report: &RunReport) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create tempdir")
    }

    fn write_migration(dir: &Path, name: &str, sql: &str) {
        let migrations = dir.join("supabase").join("migrations");
        fs::create_dir_all(&migrations).unwrap();
        fs::write(migrations.join(name), sql).unwrap();
    }

    fn write_config(dir: &Path, name: &str, toml: &str) {
        fs::create_dir_all(dir.join("supabase")).unwrap();
        fs::write(dir.join("supabase").join(name), toml).unwrap();
    }

    fn opts(root: &Path) -> Options {
        Options {
            repo_root: root.to_path_buf(),
            config_override: None,
            rule_ids: Vec::new(),
        }
    }

    // ── clean repo -> clean report, exit code 0 ───────────────────────────────

    #[test]
    fn clean_repo_produces_clean_report_and_zero_exit() {
        let dir = tmpdir();
        write_config(
            dir.path(),
            "config.toml",
            "project_id = \"x\"\n[api]\nschemas = [\"public\"]\n",
        );
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.orders (id uuid primary key, user_id uuid not null);\n\
             alter table public.orders enable row level security;\n\
             create policy \"p\" on public.orders for select using (true);\n",
        );
        let report = run(&opts(dir.path())).expect("must not err");
        assert!(report.clean, "{report:?}");
        assert_eq!(report.violations.len(), 0);
        assert_eq!(report.exit_code(false), 0);
        assert_eq!(report.exit_code(true), 0);
    }

    // ── a real deterministic violation -> non-zero default exit code ─────────

    #[test]
    fn deterministic_violation_fails_the_default_gate() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n",
        );
        let report = run(&opts(dir.path())).expect("must not err");
        assert!(!report.clean);
        assert_eq!(report.deterministic_count, 1, "{report:?}");
        assert_eq!(report.exit_code(false), 1);
        assert_eq!(report.exit_code(true), 1);
        let v = &report.violations[0];
        assert_eq!(v.rule_id, "SUPABASE-RLS-ENABLED-1");
        assert_eq!(v.file, "supabase/migrations/20240101000000_init.sql");
        assert_eq!(v.line, 1);
        assert!(
            !v.needs_review,
            "an RLS finding is a hard deterministic verdict: {v:?}"
        );
    }

    // ── needs-review-only findings don't fail the default gate, but --strict does ──

    #[test]
    fn needs_review_only_findings_pass_default_gate_but_fail_strict() {
        // ResourceLifecycleChecker is `advisory_coexisting` ALWAYS (D3) — a spawn with no
        // `.kill_on_drop(true)` is needs-review, never a hard verdict.
        let dir = tmpdir();
        fs::write(
            dir.path().join("main.rs"),
            "fn f() { tokio::process::Command::new(\"x\").spawn().unwrap(); }\n",
        )
        .unwrap();
        let report = run(&opts(dir.path())).expect("must not err");
        assert!(
            !report.violations.is_empty(),
            "expected a resource-lifecycle finding: {report:?}"
        );
        assert!(
            report.violations.iter().all(|v| v.needs_review),
            "every finding in this fixture must be needs-review: {report:?}"
        );
        assert_eq!(report.deterministic_count, 0);
        assert_eq!(
            report.exit_code(false),
            0,
            "needs-review-only must not fail the default gate"
        );
        assert_eq!(
            report.exit_code(true),
            1,
            "--strict must fail on needs-review findings too"
        );
    }

    // ── D3 parity: unconfigured repo -> config-gated rules stay silent ────────

    #[test]
    fn unconfigured_repo_config_gated_checkers_stay_silent() {
        let dir = tmpdir();
        // Identical shape to the configured e2e fixture's violation, but with NO
        // .camerata/architecture.toml at all — HandlerNoDbChecker must abstain (D3), exactly
        // as it does inside Camerata's own scan/Layer-2 gate.
        fs::create_dir_all(dir.path().join("src/routes")).unwrap();
        fs::write(
            dir.path().join("src/routes/orders.ts"),
            "export function listOrders(db: Db) {\n  return db.query('select * from orders');\n}\n",
        )
        .unwrap();
        let report = run(&opts(dir.path())).expect("must not err");
        assert!(
            report.clean,
            "an unconfigured repo must get zero deterministic findings from config-gated \
             checkers, same as inside Camerata: {report:?}"
        );
    }

    // ── --rule-id filter narrows the checker set ──────────────────────────────

    #[test]
    fn rule_id_filter_excludes_unselected_checkers() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n",
        );
        let mut o = opts(dir.path());
        o.rule_ids = vec!["UI-UTC-DATES-1".to_string()]; // unrelated to the RLS finding above
        let report = run(&o).expect("must not err");
        assert!(
            report.clean,
            "RLS checker must not run when only an unrelated rule id is selected: {report:?}"
        );
        assert_eq!(report.checkers_run, 1);
    }

    #[test]
    fn unmatched_rule_id_is_reported_not_silently_dropped() {
        let dir = tmpdir();
        let mut o = opts(dir.path());
        o.rule_ids = vec!["ARCH-STRUCTURED-ERRORS-1".to_string()]; // Group E: deliberately deferred, no checker
        let report = run(&o).expect("must not err");
        assert_eq!(report.checkers_run, 0);
        assert_eq!(
            report.unmatched_rule_ids,
            vec!["ARCH-STRUCTURED-ERRORS-1".to_string()]
        );
        assert!(report.clean);
    }

    // ── --config override ─────────────────────────────────────────────────────

    #[test]
    fn config_override_is_used_even_when_repo_has_no_camerata_dir() {
        let dir = tmpdir();
        fs::create_dir_all(dir.path().join("src/routes")).unwrap();
        fs::write(
            dir.path().join("src/routes/orders.ts"),
            "export function listOrders(db: Db) {\n  return db.query('select * from orders');\n}\n",
        )
        .unwrap();

        let cfg_dir = tmpdir();
        let cfg_path = cfg_dir.path().join("architecture.toml");
        fs::write(
            &cfg_path,
            "version = 1\n[layers]\nhandlers = [\"src/routes/**\"]\n[imports]\nhandlers = []\n\
             [db]\nhandles = [\"db\"]\nallowed_in = []\n",
        )
        .unwrap();

        let mut o = opts(dir.path());
        o.config_override = Some(cfg_path);
        let report = run(&o).expect("must not err");
        assert!(
            !report.clean,
            "the --config override must make HandlerNoDbChecker deterministic even though the \
             scanned repo itself has no .camerata/architecture.toml: {report:?}"
        );
        // With an empty `[db].allowed_in`, both the call-site facets over ARCH-HANDLER-NO-DB-1
        // AND ARCH-STRICT-LAYERING-1 fire for the same `db.query(...)` call site (two distinct
        // rule ids, by design — see `strict_layering_call_checker`'s own "two owners" note).
        assert_eq!(report.deterministic_count, 2, "{report:?}");
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule_id == "ARCH-HANDLER-NO-DB-1"),
            "{report:?}"
        );
    }

    #[test]
    fn config_override_missing_file_is_a_fatal_run_error() {
        let dir = tmpdir();
        let mut o = opts(dir.path());
        o.config_override = Some(dir.path().join("does-not-exist.toml"));
        let result = run(&o);
        assert!(
            result.is_err(),
            "an explicit --config path that can't be read must error, not silently degrade"
        );
    }

    // ── adversarial: malformed SQL / malformed config / binary files never panic ──

    #[test]
    fn malformed_sql_never_panics_and_returns_ok() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_garbage.sql",
            "create table public.x (id uuid); \
             create function public.f() returns void as $$ begin -- never closed\n\
             insert into t values ('unterminated string;\n\
             /* unterminated comment\n\
             \u{0}\u{1}\u{2} garbage bytes",
        );
        let result = run(&opts(dir.path()));
        assert!(
            result.is_ok(),
            "malformed SQL must never propagate as a fatal error: {result:?}"
        );
    }

    #[test]
    fn malformed_architecture_config_degrades_to_unconfigured_never_panics() {
        let dir = tmpdir();
        fs::create_dir_all(dir.path().join(".camerata")).unwrap();
        fs::write(
            dir.path().join(".camerata/architecture.toml"),
            "}{ not valid toml at all",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src/routes")).unwrap();
        fs::write(
            dir.path().join("src/routes/orders.ts"),
            "export function listOrders(db: Db) {\n  return db.query('select * from orders');\n}\n",
        )
        .unwrap();
        let report = run(&opts(dir.path())).expect("malformed config must degrade, never error");
        assert!(
            report.clean,
            "malformed config must be treated as absent (D3), not crash: {report:?}"
        );
    }

    #[test]
    fn non_utf8_files_are_lossily_decoded_never_panic() {
        let dir = tmpdir();
        let migrations = dir.path().join("supabase").join("migrations");
        fs::create_dir_all(&migrations).unwrap();
        fs::write(
            migrations.join("20240101000000_binary.sql"),
            [0xff, 0xfe, 0x00, 0x80, 0x81],
        )
        .unwrap();
        let result = run(&opts(dir.path()));
        assert!(
            result.is_ok(),
            "non-UTF8 file content must never panic the run: {result:?}"
        );
    }

    // ── rendering ──────────────────────────────────────────────────────────────

    #[test]
    fn render_human_clean_report_says_clean() {
        let report = RunReport {
            repo: "some/repo".into(),
            checkers_run: 8,
            unmatched_rule_ids: vec![],
            violations: vec![],
            deterministic_count: 0,
            needs_review_count: 0,
            clean: true,
        };
        let s = render_human(&report);
        assert!(s.contains("clean"));
        assert!(s.contains("some/repo"));
    }

    #[test]
    fn render_human_violation_includes_file_line_rule_id_object_message() {
        let report = RunReport {
            repo: "r".into(),
            checkers_run: 1,
            unmatched_rule_ids: vec![],
            violations: vec![ViolationJson {
                rule_id: "SUPABASE-RLS-ENABLED-1".into(),
                file: "supabase/migrations/x.sql".into(),
                line: 3,
                object: Some("public.profiles".into()),
                message: "no RLS".into(),
                severity: "critical".into(),
                needs_review: false,
            }],
            deterministic_count: 1,
            needs_review_count: 0,
            clean: false,
        };
        let s = render_human(&report);
        assert!(s.contains("supabase/migrations/x.sql"));
        assert!(s.contains(":3"));
        assert!(s.contains("SUPABASE-RLS-ENABLED-1"));
        assert!(s.contains("public.profiles"));
        assert!(s.contains("no RLS"));
        assert!(s.contains("deterministic"));
    }

    #[test]
    fn render_human_notes_unmatched_rule_ids() {
        let report = RunReport {
            repo: "r".into(),
            checkers_run: 0,
            unmatched_rule_ids: vec!["ARCH-EXACT-DECIMALS-1".into()],
            violations: vec![],
            deterministic_count: 0,
            needs_review_count: 0,
            clean: true,
        };
        let s = render_human(&report);
        assert!(s.contains("ARCH-EXACT-DECIMALS-1"));
    }

    // ── JSON schema stability ───────────────────────────────────────────────────

    #[test]
    fn json_schema_is_stable() {
        let report = RunReport {
            repo: "owner/repo".into(),
            checkers_run: 8,
            unmatched_rule_ids: vec!["ARCH-EXACT-DECIMALS-1".into()],
            violations: vec![ViolationJson {
                rule_id: "SUPABASE-RLS-ENABLED-1".into(),
                file: "supabase/migrations/x.sql".into(),
                line: 3,
                object: Some("public.profiles".into()),
                message: "no RLS".into(),
                severity: "critical".into(),
                needs_review: false,
            }],
            deterministic_count: 1,
            needs_review_count: 0,
            clean: false,
        };
        let json = render_json(&report).expect("must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");

        // Top-level fields.
        for field in [
            "repo",
            "checkers_run",
            "unmatched_rule_ids",
            "violations",
            "deterministic_count",
            "needs_review_count",
            "clean",
        ] {
            assert!(
                value.get(field).is_some(),
                "missing top-level field `{field}` in {json}"
            );
        }
        assert_eq!(value["repo"], "owner/repo");
        assert_eq!(value["checkers_run"], 8);
        assert_eq!(value["clean"], false);
        assert_eq!(value["deterministic_count"], 1);
        assert_eq!(value["needs_review_count"], 0);
        assert!(value["unmatched_rule_ids"].is_array());
        assert!(value["violations"].is_array());

        // Per-violation fields.
        let v = &value["violations"][0];
        for field in [
            "rule_id",
            "file",
            "line",
            "object",
            "message",
            "severity",
            "needs_review",
        ] {
            assert!(
                v.get(field).is_some(),
                "missing violation field `{field}` in {json}"
            );
        }
        assert_eq!(v["rule_id"], "SUPABASE-RLS-ENABLED-1");
        assert_eq!(v["file"], "supabase/migrations/x.sql");
        assert_eq!(v["line"], 3);
        assert_eq!(v["object"], "public.profiles");
        assert_eq!(v["severity"], "critical");
        assert_eq!(v["needs_review"], false);

        // Round-trip: the schema deserializes back into the same struct (a downstream CI
        // parser depends on this shape being stable, not just "some valid JSON").
        let back: RunReport = serde_json::from_str(&json).expect("must round-trip");
        assert_eq!(back, report);
    }

    #[test]
    fn json_object_field_is_null_when_violation_has_no_object() {
        let report = RunReport {
            repo: "r".into(),
            checkers_run: 1,
            unmatched_rule_ids: vec![],
            violations: vec![ViolationJson {
                rule_id: "UI-UTC-DATES-1".into(),
                file: "src/date_label.ts".into(),
                line: 5,
                object: None,
                message: "m".into(),
                severity: "medium".into(),
                needs_review: true,
            }],
            deterministic_count: 0,
            needs_review_count: 1,
            clean: false,
        };
        let json = render_json(&report).expect("must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
        assert!(value["violations"][0]["object"].is_null());
    }
}
