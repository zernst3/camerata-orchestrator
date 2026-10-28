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
//! hard verdicts — AND that rank at or above [`FAIL_AT_SEVERITY`] fail the build
//! ([`RunReport::exit_code`] with `strict = false`). Pass `--strict` to also fail on
//! needs-review-grade and below-the-bar findings.
//!
//! # Severity: the corpus bounds, then the fail bar
//!
//! A violation's severity is the checker's own, run through the corpus's authored severity
//! bounds ([`camerata_rules::severity_bounds`]: the `min_severity` floor, the C4-R1 unlocated
//! downgrade, the `max_severity` ceiling, the deviation ceiling), the SAME implementation the
//! Camerata scan and its exports apply, so this gate reports a finding at the severity the client's
//! report shows. Before 2026-10-28 it did not, and reported ARCH-DEAD-CODE-1 at `medium` although
//! the corpus caps it at `low`.
//!
//! The corpus is read from [`camerata_rules::corpus_path`] (the bundled rules, or
//! `CAMERATA_CORPUS_PATH`). When it cannot be loaded the bounds that need no authored rule still
//! apply, and [`RunReport::corpus_bounds_applied`] is `false` so the output says so.
//!
//! A deterministic violation BELOW [`FAIL_AT_SEVERITY`] (a `low` or `info` finding, such as a
//! consistency rule the corpus caps at `low`) is ADVISORY: reported, counted, and exit code 0.
//! Decision (Opus, 2026-10-27, recorded in `docs/OVERNIGHT_DECISIONS.md`; MASTER-SEQUENCE section
//! 2b): a client's CI build should not hard-fail on a low-severity consistency finding; `medium`
//! and above fail it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use camerata_checks::arch_check_runner::collect_interest_files;
use camerata_checks::arch_checker::{all_checkers, checker_applies, RepoView, ViolationGrade};
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

/// The lowest (bounded) severity at which a deterministic violation fails the default gate.
/// Anything ranking below it (`low`, `info`, or an unrecognized severity, which normalizes to
/// `low`) is advisory: reported, never a failure without `--strict`. See the module doc.
pub const FAIL_AT_SEVERITY: &str = "medium";

/// Whether a violation at `severity` (any casing) ranks at or above [`FAIL_AT_SEVERITY`].
pub fn severity_fails_gate(severity: &str) -> bool {
    use camerata_rules::severity_bounds::{normalize_severity, severity_rank};
    severity_rank(&normalize_severity(severity)) <= severity_rank(FAIL_AT_SEVERITY)
}

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
    /// The rule corpus whose severity bounds apply to every violation. `None` (not loadable)
    /// still applies the bounds that need no authored rule; see the module doc.
    pub corpus: Option<camerata_rules::RuleSet>,
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
    /// `--rule-id` values that name a rule this binary CANNOT evaluate: the clone-based rules
    /// (`ARCH-SINGLE-SOURCE-LOGIC-1`, `ARCH-NAMING-ONE-CONCEPT-1`, `UI-SHARED-PRIMITIVE-1`). They
    /// find repeated code deterministically but need a reviewed judgment to decide which repeats
    /// are real, and that judgment is the scan's advisory tier, which a bare CI runner does not
    /// have. Reported here, never folded into `clean`: an empty result for these ids is not a
    /// clean result. Absent in older output (`serde(default)`).
    #[serde(default)]
    pub unsupported_rule_ids: Vec<String>,
    pub violations: Vec<ViolationJson>,
    /// Inputs a checker could not assess (an unreadable rules file, rules that live only in the
    /// console). Never a violation and never counted as one, and never clean: a run with any of
    /// these is not clean, and `--strict` fails on them. Absent in older output
    /// (`serde(default)`).
    #[serde(default)]
    pub not_assessed: Vec<ViolationJson>,
    /// Violations that are hard deterministic verdicts (neither `advisory_coexisting` nor
    /// `[needs review`-marked), at any severity.
    pub deterministic_count: usize,
    /// The deterministic violations whose bounded severity ranks at or above
    /// [`FAIL_AT_SEVERITY`]. This is what a non-`--strict` run fails the build on; the rest of
    /// `deterministic_count` is advisory. Absent in older output (`serde(default)`).
    #[serde(default)]
    pub blocking_count: usize,
    /// Violations that are needs-review grade (excluded from `deterministic_count`).
    pub needs_review_count: usize,
    /// `true` iff `violations` is empty. Convenience mirror of `violations.is_empty()` so a
    /// JSON consumer doesn't have to check array length for the common case.
    pub clean: bool,
    /// `true` when the rule corpus loaded and its authored severity bounds were applied.
    /// `false` means only the bounds that need no authored rule were applied, so a severity may
    /// read higher than the client's report shows it. Absent in older output (`serde(default)`).
    #[serde(default)]
    pub corpus_bounds_applied: bool,
}

impl RunReport {
    /// The process exit code for this report: standard CI gate semantics.
    ///
    /// - `strict = false` (default): non-zero iff `blocking_count > 0`. Needs-review-grade
    ///   findings alone never fail the build — that's the entire point of keeping the two
    ///   classes separate (see the module doc) — and neither does a deterministic finding below
    ///   [`FAIL_AT_SEVERITY`].
    /// - `strict = true`: non-zero iff ANY violation exists, at any grade or severity, or a
    ///   requested rule could not be evaluated at all ([`Self::unsupported_rule_ids`]), or a
    ///   checker could not assess an input ([`Self::not_assessed`]): a strict gate must not pass
    ///   on something it never checked.
    pub fn exit_code(&self, strict: bool) -> i32 {
        let should_fail = if strict {
            !self.violations.is_empty()
                || !self.unsupported_rule_ids.is_empty()
                || !self.not_assessed.is_empty()
        } else {
            self.blocking_count > 0
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

    // The clone-based rules are real rules this binary cannot answer: say so, by name, instead
    // of letting them fall into "no checker answers" (a typo or a deferred rule) or, worse,
    // into a clean result.
    let mut unsupported_rule_ids: Vec<String> = opts
        .rule_ids
        .iter()
        .filter(|id| camerata_checks::clone_rules::RULE_IDS.contains(&id.as_str()))
        .cloned()
        .collect();
    unsupported_rule_ids.sort();
    unsupported_rule_ids.dedup();

    let unmatched_rule_ids: Vec<String> = if opts.rule_ids.is_empty() {
        Vec::new()
    } else {
        let covered: HashSet<&str> = all
            .iter()
            .flat_map(|c| c.rule_ids().iter().copied())
            .collect();
        opts.rule_ids
            .iter()
            .filter(|id| !covered.contains(id.as_str()) && !unsupported_rule_ids.contains(id))
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
    let selected_refs: Vec<&dyn camerata_checks::arch_checker::ArchChecker> =
        selected.iter().map(|c| c.as_ref()).collect();
    camerata_checks::arch_check_runner::add_named_files(
        &opts.repo_root,
        &selected_refs,
        &mut files,
    );
    apply_config_override(&mut files, opts.config_override.as_deref())?;

    let repo_label = opts.repo_root.display().to_string();
    let view = RepoView {
        spec: &repo_label,
        files: &files,
    };

    let mut projected: Vec<Projected> = Vec::new();
    for checker in &selected {
        // Zero matching files -> skip; never a false "ran and found nothing" read as "clean"
        // (same "interest_globs, cheap" contract the Layer-2 runner and scan both honor).
        if !checker_applies(checker.as_ref(), &files) {
            continue;
        }
        let advisory = checker.defects_need_review();
        projected.extend(
            checker
                .check(&view)
                .into_iter()
                .map(|v| project(v, advisory)),
        );
    }
    apply_severity_bounds(&mut projected, opts.corpus.as_ref());
    let mut report = build_report(
        repo_label,
        selected.len(),
        unmatched_rule_ids,
        unsupported_rule_ids,
        projected,
    );
    report.corpus_bounds_applied = opts.corpus.is_some();
    Ok(report)
}

/// Run every projected row through the corpus's severity bounds, the one implementation the
/// scan and the exports share ([`camerata_rules::severity_bounds`]). `None` applies an EMPTY rule
/// set, as the scan does without a corpus, so the bounds that need no authored rule still run.
fn apply_severity_bounds(projected: &mut [Projected], corpus: Option<&camerata_rules::RuleSet>) {
    let empty = camerata_rules::RuleSet::default();
    let corpus = corpus.unwrap_or(&empty);
    camerata_rules::severity_bounds::apply_corpus_severity_bounds(projected, |id| {
        corpus.get_by_id(id)
    });
}

/// One checker output, projected into this crate's wire shape and sorted into the list it
/// belongs on.
enum Projected {
    Violation(ViolationJson),
    NotAssessed(ViolationJson),
}

impl Projected {
    fn json(&self) -> &ViolationJson {
        match self {
            Projected::Violation(v) | Projected::NotAssessed(v) => v,
        }
    }
}

/// The bounds read a violation exactly as the scan's adapter (`arch_violation_to_finding`) shapes
/// it: a violation (defect or held for review) is a Defect, a not-assessed notice is
/// informational.
impl camerata_rules::severity_bounds::BoundedFinding for Projected {
    fn severity(&self) -> &str {
        &self.json().severity
    }

    fn rule_id(&self) -> &str {
        &self.json().rule_id
    }

    fn bound_kind(&self) -> camerata_rules::severity_bounds::BoundKind {
        match self {
            Projected::Violation(_) => camerata_rules::severity_bounds::BoundKind::Defect,
            Projected::NotAssessed(_) => camerata_rules::severity_bounds::BoundKind::Informational,
        }
    }

    fn is_unlocated(&self) -> bool {
        let v = self.json();
        camerata_rules::severity_bounds::is_unlocated(&v.file, v.line)
    }

    fn set_severity(&mut self, severity: String) {
        match self {
            Projected::Violation(v) | Projected::NotAssessed(v) => v.severity = severity,
        }
    }
}

/// Project one [`camerata_checks::arch_checker::ArchViolation`]. A held-for-review fact
/// ([`ViolationGrade::Review`]) is needs-review exactly like an advisory-coexisting checker's
/// finding; a not-assessed notice is not a violation at all.
fn project(v: camerata_checks::arch_checker::ArchViolation, advisory: bool) -> Projected {
    let grade = v.grade;
    let needs_review =
        advisory || grade == ViolationGrade::Review || message_is_needs_review(&v.message);
    let json = ViolationJson {
        rule_id: v.rule_id,
        file: v.file,
        line: v.line,
        object: v.object,
        message: v.message,
        severity: v.severity.to_string(),
        needs_review,
    };
    match grade {
        ViolationGrade::NotAssessed => Projected::NotAssessed(json),
        ViolationGrade::Defect | ViolationGrade::Review => Projected::Violation(json),
    }
}

fn build_report(
    repo: String,
    checkers_run: usize,
    unmatched_rule_ids: Vec<String>,
    unsupported_rule_ids: Vec<String>,
    projected: Vec<Projected>,
) -> RunReport {
    let mut violations = Vec::new();
    let mut not_assessed = Vec::new();
    for p in projected {
        match p {
            Projected::Violation(v) => violations.push(v),
            Projected::NotAssessed(v) => not_assessed.push(v),
        }
    }
    // Stable, deterministic ordering regardless of checker registration order — a downstream
    // CI script diffing output run-to-run shouldn't see spurious reordering.
    let order = |a: &ViolationJson, b: &ViolationJson| {
        (&a.file, a.line, &a.rule_id).cmp(&(&b.file, b.line, &b.rule_id))
    };
    violations.sort_by(order);
    not_assessed.sort_by(order);

    let deterministic_count = violations.iter().filter(|v| !v.needs_review).count();
    let blocking_count = violations
        .iter()
        .filter(|v| !v.needs_review && severity_fails_gate(&v.severity))
        .count();
    let needs_review_count = violations.len() - deterministic_count;
    // A requested rule that could not be evaluated, or an input a checker could not assess, is
    // never "clean".
    let clean = violations.is_empty() && unsupported_rule_ids.is_empty() && not_assessed.is_empty();

    RunReport {
        repo,
        checkers_run,
        unmatched_rule_ids,
        unsupported_rule_ids,
        violations,
        not_assessed,
        deterministic_count,
        blocking_count,
        needs_review_count,
        clean,
        corpus_bounds_applied: false,
    }
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
    } else if report.violations.is_empty() {
        // Nothing found by what ran, but something was never evaluated: not clean.
        out.push_str(&format!(
            "camerata-check: NOT CLEAN — {} checker(s) ran over {} with no violations, but {} \
             requested rule(s) and {} input(s) were not assessed\n",
            report.checkers_run,
            report.repo,
            report.unsupported_rule_ids.len(),
            report.not_assessed.len()
        ));
    } else {
        out.push_str(&format!(
            "camerata-check: {} violation(s) in {} ({} deterministic, of which {} advisory below \
             {FAIL_AT_SEVERITY}; {} needs-review)\n\n",
            report.violations.len(),
            report.repo,
            report.deterministic_count,
            report.deterministic_count - report.blocking_count,
            report.needs_review_count
        ));
        for v in &report.violations {
            let tag = if v.needs_review {
                "needs-review"
            } else if severity_fails_gate(&v.severity) {
                "deterministic"
            } else {
                "deterministic, advisory"
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
    if !report.not_assessed.is_empty() {
        out.push_str(
            "\nNot assessed (a checker could not read these inputs; this is not a clean result):\n",
        );
        for v in &report.not_assessed {
            if v.line > 0 {
                out.push_str(&format!(
                    "{}:{} [{}] — {}\n",
                    v.file, v.line, v.rule_id, v.message
                ));
            } else {
                out.push_str(&format!("{} [{}] — {}\n", v.file, v.rule_id, v.message));
            }
        }
    }
    if !report.corpus_bounds_applied
        && !(report.violations.is_empty() && report.not_assessed.is_empty())
    {
        out.push_str(
            "\nNote: the rule corpus could not be loaded (set CAMERATA_CORPUS_PATH to the corpus \
             directory), so the corpus's severity bounds were not applied; a severity may read \
             higher than the Camerata report shows it.\n",
        );
    }
    if !report.unmatched_rule_ids.is_empty() {
        out.push_str(&format!(
            "\nNote: no registered checker answers: {} (no native checker exists for this rule id — \
             it stays AI-advisory only; see docs/design/2026-07-27_ast-extractor-layer.md §4 Group E \
             for the two rules deliberately deferred this way).\n",
            report.unmatched_rule_ids.join(", ")
        ));
    }
    if !report.unsupported_rule_ids.is_empty() {
        out.push_str(&format!(
            "\nNot assessed: {} (unsupported here: these rules find repeated code across files and \
             need the scan's advisory tier, a reviewed judgment of which repeats are real, which a \
             CI run does not have; an empty result for them is not a clean result).\n",
            report.unsupported_rule_ids.join(", ")
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
            corpus: None,
        }
    }

    /// Options scoped to exactly the config-gated rules (the ones whose checkers consult
    /// `.camerata/architecture.toml` and abstain without it, D3): the import-boundary family,
    /// handler-no-db and strict-layering. The D3 parity tests use this so they exercise ONLY
    /// the checkers they are about, not always-on checkers (e.g. ARCH-DEAD-CODE-1) that
    /// legitimately report on a tiny fixture repo with no entry point.
    fn config_gated_opts(root: &Path) -> Options {
        let mut o = opts(root);
        o.rule_ids = [
            "ARCH-HANDLER-NO-DB-1",
            "ARCH-STRICT-LAYERING-1",
            "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1",
            "ARCH-API-DTOS-1",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        o
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
        let report = run(&config_gated_opts(dir.path())).expect("must not err");
        assert!(
            report.checkers_run > 0,
            "the config-gated checkers must actually be selected, or this test proves nothing: \
             {report:?}"
        );
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

    #[test]
    fn clone_based_rule_ids_are_unsupported_not_unmatched_and_never_clean() {
        let dir = tmpdir();
        let mut o = opts(dir.path());
        o.rule_ids = vec![
            "ARCH-SINGLE-SOURCE-LOGIC-1".to_string(),
            "UI-SHARED-PRIMITIVE-1".to_string(),
            "ARCH-SINGLE-SOURCE-LOGIC-1".to_string(), // repeated: reported once
            "ARCH-STRUCTURED-ERRORS-1".to_string(),   // a different story: no checker at all
        ];
        let report = run(&o).expect("must not err");
        assert_eq!(
            report.unsupported_rule_ids,
            vec![
                "ARCH-SINGLE-SOURCE-LOGIC-1".to_string(),
                "UI-SHARED-PRIMITIVE-1".to_string()
            ]
        );
        assert_eq!(
            report.unmatched_rule_ids,
            vec!["ARCH-STRUCTURED-ERRORS-1".to_string()]
        );
        assert!(report.violations.is_empty());
        assert!(!report.clean, "an unevaluated rule is never a clean result");
        assert_eq!(
            report.exit_code(false),
            0,
            "the default gate does not fail on it"
        );
        assert_eq!(
            report.exit_code(true),
            1,
            "a strict gate must not pass on it"
        );
        let human = render_human(&report);
        assert!(human.contains("NOT CLEAN"), "{human}");
        assert!(human.contains("Not assessed: ARCH-SINGLE-SOURCE-LOGIC-1, UI-SHARED-PRIMITIVE-1"));
        assert!(!human.contains("camerata-check: clean"));
    }

    #[test]
    fn a_run_that_does_not_ask_for_a_clone_rule_is_unaffected() {
        let dir = tmpdir();
        let report = run(&opts(dir.path())).expect("must not err");
        assert!(report.unsupported_rule_ids.is_empty());
        assert!(report.clean);
        assert_eq!(report.exit_code(true), 0);
    }

    #[test]
    fn a_report_without_the_unsupported_field_still_deserializes() {
        let old = r#"{"repo":"r","checkers_run":1,"unmatched_rule_ids":[],"violations":[],
            "deterministic_count":0,"needs_review_count":0,"clean":true}"#;
        let report: RunReport = serde_json::from_str(old).expect("older output must parse");
        assert!(report.unsupported_rule_ids.is_empty());
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

        let mut o = config_gated_opts(dir.path());
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
        let report = run(&config_gated_opts(dir.path()))
            .expect("malformed config must degrade, never error");
        assert!(
            report.checkers_run > 0,
            "the config-gated checkers must actually be selected, or this test proves nothing: \
             {report:?}"
        );
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

    // ── graded violations (Defect / Review / NotAssessed) ───────────────────────

    fn graded_violation(
        grade: ViolationGrade,
        rule: &str,
    ) -> camerata_checks::arch_checker::ArchViolation {
        camerata_checks::arch_checker::ArchViolation {
            grade,
            rule_id: rule.to_string(),
            file: "firestore.rules".to_string(),
            line: 4,
            object: None,
            message: "a fact".to_string(),
            severity: "medium",
        }
    }

    fn report_of(grades: &[(ViolationGrade, &str)]) -> RunReport {
        let projected = grades
            .iter()
            .map(|(g, r)| project(graded_violation(*g, r), false))
            .collect();
        build_report("r".into(), 1, vec![], vec![], projected)
    }

    #[test]
    fn a_review_graded_violation_is_needs_review_and_does_not_fail_a_default_run() {
        let report = report_of(&[(ViolationGrade::Review, "R-1")]);
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].needs_review);
        assert_eq!(
            (report.deterministic_count, report.needs_review_count),
            (0, 1)
        );
        assert_eq!(report.exit_code(false), 0);
        assert_eq!(report.exit_code(true), 1);
    }

    #[test]
    fn a_defect_graded_violation_is_deterministic_and_fails_a_default_run() {
        let report = report_of(&[(ViolationGrade::Defect, "D-1")]);
        assert_eq!(report.deterministic_count, 1);
        assert_eq!(report.exit_code(false), 1);
    }

    // ── corpus severity bounds, then the fail bar ───────────────────────────────

    fn bounded_report_of(
        violations: Vec<camerata_checks::arch_checker::ArchViolation>,
        corpus: Option<&camerata_rules::RuleSet>,
    ) -> RunReport {
        let mut projected: Vec<Projected> =
            violations.into_iter().map(|v| project(v, false)).collect();
        apply_severity_bounds(&mut projected, corpus);
        build_report("r".into(), 1, vec![], vec![], projected)
    }

    fn corpus_with(
        rule: &str,
        configure: impl FnOnce(&mut camerata_rules::Rule),
    ) -> camerata_rules::RuleSet {
        let mut r = camerata_rules::bare_rule(rule, "test-fixture");
        configure(&mut r);
        let mut set = camerata_rules::RuleSet::default();
        set.push(r);
        set
    }

    /// The ARCH-DEAD-CODE-1 shape: the checker says `medium`, the corpus caps the rule at
    /// `low`. The gate must report it at `low` (as the scan and the client's report do) and,
    /// being below the fail bar, as advisory.
    #[test]
    fn a_corpus_ceiling_lowers_a_violation_and_a_low_one_is_advisory() {
        let corpus = corpus_with("CAP-1", |r| r.max_severity = Some("low".to_string()));
        let report = bounded_report_of(
            vec![graded_violation(ViolationGrade::Defect, "CAP-1")],
            Some(&corpus),
        );
        assert_eq!(report.violations[0].severity, "low");
        assert_eq!((report.deterministic_count, report.blocking_count), (1, 0));
        assert_eq!(report.exit_code(false), 0, "a low finding is advisory");
        assert_eq!(report.exit_code(true), 1, "--strict still fails on it");
        let text = render_human(&report);
        assert!(text.contains("deterministic, advisory"), "{text}");
        assert!(text.contains("1 advisory below medium"), "{text}");
    }

    #[test]
    fn a_corpus_floor_raises_a_violation_into_the_failing_range() {
        let corpus = corpus_with("FLOOR-1", |r| r.min_severity = Some("high".to_string()));
        let mut v = graded_violation(ViolationGrade::Defect, "FLOOR-1");
        v.severity = "low";
        let report = bounded_report_of(vec![v], Some(&corpus));
        assert_eq!(report.violations[0].severity, "high");
        assert_eq!(report.blocking_count, 1);
        assert_eq!(report.exit_code(false), 1);
    }

    #[test]
    fn a_rule_the_corpus_does_not_bound_keeps_the_checkers_severity() {
        let corpus = corpus_with("CAP-1", |r| r.max_severity = Some("low".to_string()));
        let report = bounded_report_of(
            vec![graded_violation(ViolationGrade::Defect, "OTHER-1")],
            Some(&corpus),
        );
        assert_eq!(report.violations[0].severity, "medium");
        assert_eq!(report.exit_code(false), 1, "medium fails the default gate");
    }

    /// No corpus is still a bound chain: C4-R1 needs no authored rule, so an unlocated
    /// violation is `info` (advisory) here exactly as it is in the scan.
    #[test]
    fn without_a_corpus_the_rule_free_bounds_still_apply() {
        let mut v = graded_violation(ViolationGrade::Defect, "ANY-1");
        v.file = String::new();
        v.line = 0;
        v.severity = "high";
        let report = bounded_report_of(vec![v], None);
        assert_eq!(report.violations[0].severity, "info");
        assert_eq!(report.exit_code(false), 0);
        assert!(!report.corpus_bounds_applied);
        let text = render_human(&report);
        assert!(text.contains("CAMERATA_CORPUS_PATH"), "{text}");
    }

    #[test]
    fn the_fail_bar_is_medium_and_an_unknown_severity_is_below_it() {
        assert_eq!(FAIL_AT_SEVERITY, "medium");
        for sev in ["critical", "high", "medium", "Medium"] {
            assert!(severity_fails_gate(sev), "{sev}");
        }
        for sev in ["low", "info", "unknown"] {
            assert!(!severity_fails_gate(sev), "{sev}");
        }
    }

    #[test]
    fn a_needs_review_violation_never_blocks_whatever_its_severity() {
        let mut v = graded_violation(ViolationGrade::Review, "R-1");
        v.severity = "critical";
        let report = bounded_report_of(vec![v], None);
        assert_eq!(report.blocking_count, 0);
        assert_eq!(report.exit_code(false), 0);
    }

    #[test]
    fn a_not_assessed_notice_is_listed_apart_and_is_never_clean() {
        let report = report_of(&[(ViolationGrade::NotAssessed, "N-1")]);
        assert!(report.violations.is_empty(), "not a violation");
        assert_eq!(report.not_assessed.len(), 1);
        assert!(!report.clean, "an unassessed input is never a clean result");
        assert_eq!(
            report.exit_code(false),
            0,
            "a default run does not fail on it"
        );
        assert_eq!(report.exit_code(true), 1, "a strict run does");
        let text = render_human(&report);
        assert!(text.contains("Not assessed"), "{text}");
        assert!(text.contains("firestore.rules:4"), "{text}");
        assert!(text.contains("NOT CLEAN"), "{text}");
    }

    #[test]
    fn json_without_the_not_assessed_field_still_parses() {
        let report = report_of(&[(ViolationGrade::Defect, "D-1")]);
        let mut value = serde_json::to_value(&report).unwrap();
        value.as_object_mut().unwrap().remove("not_assessed");
        let back: RunReport = serde_json::from_value(value).unwrap();
        assert!(back.not_assessed.is_empty());
    }

    // ── rendering ──────────────────────────────────────────────────────────────

    #[test]
    fn render_human_clean_report_says_clean() {
        let report = RunReport {
            repo: "some/repo".into(),
            checkers_run: 8,
            unmatched_rule_ids: vec![],
            unsupported_rule_ids: vec![],
            violations: vec![],
            not_assessed: vec![],
            deterministic_count: 0,
            blocking_count: 0,
            needs_review_count: 0,
            clean: true,
            corpus_bounds_applied: true,
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
            unsupported_rule_ids: vec![],
            violations: vec![ViolationJson {
                rule_id: "SUPABASE-RLS-ENABLED-1".into(),
                file: "supabase/migrations/x.sql".into(),
                line: 3,
                object: Some("public.profiles".into()),
                message: "no RLS".into(),
                severity: "critical".into(),
                needs_review: false,
            }],
            not_assessed: vec![],
            deterministic_count: 1,
            blocking_count: 1,
            needs_review_count: 0,
            clean: false,
            corpus_bounds_applied: true,
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
            unsupported_rule_ids: vec![],
            violations: vec![],
            not_assessed: vec![],
            deterministic_count: 0,
            blocking_count: 0,
            needs_review_count: 0,
            clean: true,
            corpus_bounds_applied: true,
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
            unsupported_rule_ids: vec![],
            violations: vec![ViolationJson {
                rule_id: "SUPABASE-RLS-ENABLED-1".into(),
                file: "supabase/migrations/x.sql".into(),
                line: 3,
                object: Some("public.profiles".into()),
                message: "no RLS".into(),
                severity: "critical".into(),
                needs_review: false,
            }],
            not_assessed: vec![],
            deterministic_count: 1,
            blocking_count: 1,
            needs_review_count: 0,
            clean: false,
            corpus_bounds_applied: true,
        };
        let json = render_json(&report).expect("must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");

        // Top-level fields.
        for field in [
            "repo",
            "checkers_run",
            "unmatched_rule_ids",
            "unsupported_rule_ids",
            "violations",
            "deterministic_count",
            "blocking_count",
            "needs_review_count",
            "clean",
            "corpus_bounds_applied",
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
        assert_eq!(value["blocking_count"], 1);
        assert_eq!(value["needs_review_count"], 0);
        assert_eq!(value["corpus_bounds_applied"], true);
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
            unsupported_rule_ids: vec![],
            violations: vec![ViolationJson {
                rule_id: "UI-UTC-DATES-1".into(),
                file: "src/date_label.ts".into(),
                line: 5,
                object: None,
                message: "m".into(),
                severity: "medium".into(),
                needs_review: true,
            }],
            not_assessed: vec![],
            deterministic_count: 0,
            blocking_count: 0,
            needs_review_count: 1,
            clean: false,
            corpus_bounds_applied: true,
        };
        let json = render_json(&report).expect("must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
        assert!(value["violations"][0]["object"].is_null());
    }
}
