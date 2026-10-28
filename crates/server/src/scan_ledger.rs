//! W1 — the pipeline-integrity ledger.
//!
//! Two real defects motivated this module, both found on an unseen repo scan:
//!
//! 1. Rules that never ran were reported as "verified clean" / "excluded from this audit"
//!    while the selector's own evidence text described the violation. Six corpus rules
//!    declared `enforcement = "mechanical"` with NO wired detector at all, and were counted
//!    as checked anyway — because "was this rule selected" and "did this rule actually run"
//!    were the SAME boolean everywhere in the pipeline (see `onboard::ScanProvenance::
//!    audited_rule_ids`, which lists every SELECTED rule, never checking execution).
//! 2. The semantic/AI tier emitted zero rows for a repo and nothing noticed — a total pass
//!    failure was indistinguishable from a genuinely clean codebase.
//!
//! Both are SILENT KNOWLEDGE LOSS between pipeline stages. This module makes that
//! impossible to hide: a [`ScanLedger`] accumulated during a scan records, per rule, whether
//! it ACTUALLY ran (never inferred from mere selection), and, per pipeline stage, a
//! reconciled row count — `rows_in == rows_out + accounted`, where `accounted` is an
//! explicit breakdown (merged away, routed to held, routed to informational, deduped, or
//! genuinely unaccounted). An `unaccounted > 0` stage is an integrity violation.
//!
//! ## Runtime vs. test-time
//!
//! At RUNTIME this ledger never drops a row and never refuses an export — an integrity
//! violation is surfaced as a [`crate::ai_audit::FailedPass`]-style disclosure (see
//! [`stage_disclosure`] / [`rule_disclosure`]), which rides the EXISTING `failed_passes`
//! mechanism into the report's methodology and executive summary. At TEST time, the
//! consolidated `export_invariants_gate` (`report_export.rs`) and this module's own unit
//! tests assert zero unaccounted rows and fail the build otherwise — see
//! `crate::mechanical_gate` for the companion corpus-level build gate (item 4 of the W1
//! plan), which closes the SOURCE of the six-phantom-rule defect rather than just detecting
//! its symptom at scan time.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Which engine a rule's evaluation belongs to. Drives nothing structurally today (the
/// invariants below are tier-agnostic) but lets a report or operator ask "how much of my
/// coverage is deterministic vs. model-judged" honestly, straight from the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTier {
    /// Camerata's own code: the content floor (`onboard::audit_files`) — the always-on
    /// platform/floor regex set run unconditionally over every file.
    Deterministic,
    /// Camerata's own code: the deterministic architectural engine — a registered
    /// `ArchChecker` or the gateway's own rule registry arm (`onboard::audit_architectural`).
    /// Split out from [`RuleTier::Deterministic`] (CLI-inspect ledger-summary work) so a
    /// human-readable family breakdown can tell "the content floor ran" apart from "the
    /// architectural engine ran" instead of folding both into one undifferentiated bucket.
    Architectural,
    /// A third-party static-analysis tool (Semgrep, Bandit, gosec, …) Camerata shells out to.
    ExternalTool,
    /// The LLM-judged semantic audit tier.
    Semantic,
    /// W13 — the ONE member of this enum that is not an engine: a corpus file that did not
    /// PARSE, so no engine was ever chosen for it.
    ///
    /// # Why this is its own member rather than folded into one of the four engines
    ///
    /// Every other variant answers "which thing evaluated this rule". A rule file that failed
    /// to load was evaluated by nothing, and there is no way to find out which engine it would
    /// have belonged to: the tier is derived from the rule's `enforcement` field, which is
    /// exactly the content the loader could not read. Recording it under
    /// [`RuleTier::Deterministic`] (or any other) would be a false claim about an engine, which
    /// is the A3 lesson one level down — see [`RuleTier::family_label`]'s doc comment on why a
    /// label that promises membership the tier does not control is a defect in itself.
    ///
    /// It is deliberately LAST in [`RuleTier::all`], so every existing display order is
    /// unchanged and the new line appears at the bottom of a coverage summary. On a healthy
    /// scan that line reads `recorded=0 ran=0`, which is the POSITIVE disclosure "no rule file
    /// failed to load this scan" — the one tier for which recording nothing is the good state.
    Unloaded,
}

impl RuleTier {
    /// Human-readable family label for this tier, used by `camerata inspect`'s ledger summary
    /// (`crates/cli/src/inspect_cmd.rs::render_ledger_summary`) to group rules the way an
    /// operator actually reasons about coverage ("did the taint pass run at all?") rather than
    /// by raw enum name.
    ///
    /// # Every label names an ENGINE, never a rule family (Pass-1 W4, finding A3)
    ///
    /// [`RuleTier`] is an ENGINE axis — "which thing evaluated this rule" — so a label that
    /// reads as a rule-family name is a false claim about the rows underneath it.
    /// `RuleTier::Architectural` used to be labelled "Architectural checks", which a reader
    /// inevitably takes as "the `ARCH-*` rules". It is not: membership is decided purely by
    /// `mechanical_gate::detector_channel` resolving to `arch_checker` or
    /// `gateway_rule_registry`, and on every benchmark repo that resolved to eight Supabase
    /// migration-replay rules (`SUPABASE-RLS-*`, `SUPABASE-FUNC-SEARCH-PATH-1`) which the
    /// external grader scores as *security* findings — while the four genuine `ARCH-*` static
    /// analysers were switched off for want of a `.camerata/architecture.toml` (finding A4,
    /// fixed separately). So the family header claimed an architecture-coverage story the rows
    /// could not support.
    ///
    /// Fixed by naming the engine: "Deterministic static-analysis / migration-replay
    /// checkers". `req_a3_no_tier_family_label_borrows_a_corpus_rule_family_name` is the gate
    /// — no word in any tier label may be (or begin with) a corpus rule-id family prefix, so
    /// the next label that reaches for a rule family's name fails the build.
    pub fn family_label(&self) -> &'static str {
        match self {
            RuleTier::Deterministic => "Deterministic platform/floor checks",
            RuleTier::Architectural => "Deterministic static-analysis / migration-replay checkers",
            RuleTier::ExternalTool => "External-tool / taint (Semgrep, etc.)",
            RuleTier::Semantic => "Semantic / advisory tier",
            RuleTier::Unloaded => "Rule files that failed to load",
        }
    }

    /// Every family, in the fixed display order the ledger summary renders them — so a family
    /// with ZERO recorded rules still gets its own line instead of being silently omitted (see
    /// `render_ledger_summary`'s doc comment).
    pub fn all() -> [RuleTier; 5] {
        [
            RuleTier::Deterministic,
            RuleTier::Architectural,
            RuleTier::ExternalTool,
            RuleTier::Semantic,
            // W13, deliberately last — see `RuleTier::Unloaded`'s doc comment.
            RuleTier::Unloaded,
        ]
    }
}

/// One rule's outcome for this scan. The load-bearing invariant the rest of the pipeline
/// must respect: a rule is "verified clean" ONLY when `ran && findings_emitted == 0`. A rule
/// with `ran == false` is "not run (reason)" — NEVER clean, NEVER silently folded into
/// "excluded from this audit" without a reason attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleLedgerEntry {
    pub rule_id: String,
    pub tier: RuleTier,
    pub ran: bool,
    /// Why this rule did not run: "not armed for this repo", "no matching files", "tool
    /// absent", "gated off", or (the W1-item-4 defect) "declares mechanical/architectural
    /// enforcement but has no wired detector". `None` when `ran` is true, or (rare) when a
    /// rule was recorded as not-run without a caller-supplied reason — the ledger still
    /// refuses to call it clean either way.
    pub skip_reason: Option<String>,
    pub files_evaluated: usize,
    pub findings_emitted: usize,
    /// An honest, ADDITIVE disclosure that survives regardless of `ran`/`skip_reason` — unlike
    /// `skip_reason` (which only ever means "why this rule did NOT run" and is cleared the
    /// moment `ran` becomes true), this field exists for a rule that DOES have a registered
    /// checker but did not get a credited deterministic verdict for THIS repo (today: a
    /// config-gated architectural checker — `ARCH-HANDLER-NO-DB-1`, `ARCH-API-DTOS-1`,
    /// `ARCH-STRICT-LAYERING-1`, `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` — with no
    /// `.camerata/architecture.toml` present). Set via [`ScanLedger::note_config_gated_rule`]
    /// BEFORE the rule's `ran` fact is known (the semantic phase settles that later), so a
    /// reader of a "ran=true, Semantic tier, healthy" row still learns this was really a
    /// mechanical/architectural rule that only ever got a model's semantic read, not the
    /// deterministic check its enforcement tier promised — never silently indistinguishable
    /// from an ordinary prose-only rule. `None` for every rule this doesn't apply to (the
    /// overwhelming majority).
    #[serde(default)]
    pub coverage_note: Option<String>,
    /// Pass-1 W2 (A5.4): `true` when the alternatives pass judged this rule's underlying
    /// concern NOT APPLICABLE to this codebase at all (no evidence it exists here — e.g. no
    /// pagination anywhere, no API versioning anywhere), requiring POSITIVE evidence to set
    /// (`RuleRecommendation::applicable == false`, which `parse_alternative_recommendations`
    /// already only ever produces from an explicit model claim or an absent-evidence
    /// correction — never a default). This is its OWN outcome, distinct from "ran and found
    /// nothing" — see [`Self::verified_clean`]'s doc comment for why `findings_emitted == 0`
    /// alone is not enough to call a not-applicable rule healthy. Set via
    /// [`ScanLedger::note_not_applicable_rule`] the same way [`Self::coverage_note`] is: queued
    /// before the entry exists, consumed on every subsequent [`ScanLedger::record_rule`] call
    /// for the same id. `false` for the overwhelming majority of rules (the ordinary "ran,
    /// genuinely looked, found nothing" case).
    #[serde(default)]
    pub not_applicable: bool,
    /// Set when the rule ran on SOME of the code and the scan was stopped before it ran on the
    /// rest (today: the spend limit). The rule did run (`ran` is true, its findings are real),
    /// but it was NOT checked everywhere, so it is never [`Self::verified_clean`], the
    /// integrity gate fails on it as incomplete coverage, and the export lists it with this
    /// reason. Distinct from `ran == false`, which means it did not run at all. Set via
    /// [`ScanLedger::note_partial_rule`].
    #[serde(default)]
    pub partial_reason: Option<String>,
}

impl RuleLedgerEntry {
    /// The one predicate every "what's healthy" / scorecard "Clean" derivation must use
    /// instead of inferring cleanliness from mere selection.
    ///
    /// Pass-1 W2 (A5.4): a rule judged [`Self::not_applicable`] is NEVER verified-clean, even
    /// when `ran && findings_emitted == 0` — "this rule's concern doesn't exist in this
    /// codebase" is a DIFFERENT, honest claim from "this rule was checked and the codebase
    /// complies with it," and folding the two together is exactly the silencing shape this
    /// whole W2 pass exists to close (a `not_applicable` determination must never be
    /// indistinguishable from a rule that genuinely ran and found nothing to report).
    pub fn verified_clean(&self) -> bool {
        self.ran
            && self.findings_emitted == 0
            && !self.not_applicable
            && self.partial_reason.is_none()
    }
}

/// One finding that was absorbed into another row during a merge stage, with the id of the
/// row it was merged INTO — the "merged-into (with target)" breakdown entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedRow {
    pub rule_id: String,
    pub target_rule_id: String,
}

/// The explicit disposition breakdown a stage instrumentation site supplies for every row
/// that did NOT survive as its own output row. Any row not covered by one of these buckets
/// becomes `unaccounted` in the resulting [`StageLedgerEntry`] — see
/// [`ScanLedger::record_stage`].
///
/// W1 (Pass-1 seal fix): a stage can also GAIN rows (the scan-time external-tool preview
/// merge, the dependency audit, the evidence-of-violation gate all APPEND findings rather
/// than remove them) — `added` is the symmetric counterpart of `other` for that direction:
/// `(label, count)` naming honestly WHY `rows_out` exceeds `rows_in`. Before this, the ledger
/// could only account for row LOSSES (`rows_in >= rows_out` was baked into every call site
/// that reduces a `Vec<Finding>`); a stage that added rows had nowhere to disclose them, so
/// the two real injection points this fixes (the preview merge in `merge_scan_preview`, and
/// the evidence gate in `report_export::apply_evidence_gate`) ran OUTSIDE the ledger's view
/// entirely. See [`ScanLedger::record_stage`]'s doc comment for the now-bidirectional
/// reconciliation this enables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageAccounting {
    pub merged_into: Vec<MergedRow>,
    pub routed_held: usize,
    pub routed_informational: usize,
    pub deduped: usize,
    /// Escape hatch for a stage-specific disposition that doesn't fit the four named buckets
    /// above (e.g. report_export's "excluded as a false positive" or "carved out to the
    /// dependency snapshot") — `(label, count)`. Still fully counted toward `accounted()`, so
    /// using this never masks a real integrity gap; it just names the bucket honestly instead
    /// of forcing it into a wrong-shaped one.
    pub other: Vec<(String, usize)>,
    /// `(label, count)` of rows this stage ADDED (`rows_out > rows_in`) — e.g.
    /// `("external_tool_preview_findings", 6)` or `("evidence_gate_synthesized", 2)`. Counted
    /// toward [`Self::added_total`], never [`Self::accounted`] (the two totals reconcile
    /// opposite directions — see [`ScanLedger::record_stage`]).
    #[serde(default)]
    pub added: Vec<(String, usize)>,
}

impl StageAccounting {
    /// Total of every named REMOVAL-side disposition — reconciles against `rows_in -
    /// rows_out` (a stage that shrank the row set).
    pub fn accounted(&self) -> usize {
        self.merged_into.len()
            + self.routed_held
            + self.routed_informational
            + self.deduped
            + self.other.iter().map(|(_, n)| n).sum::<usize>()
    }

    /// Total of every named ADDITION-side disposition — reconciles against `rows_out -
    /// rows_in` (a stage that grew the row set). See [`Self::added`]'s doc comment.
    pub fn added_total(&self) -> usize {
        self.added.iter().map(|(_, n)| n).sum::<usize>()
    }

    pub fn push_merge(&mut self, rule_id: impl Into<String>, target_rule_id: impl Into<String>) {
        self.merged_into.push(MergedRow {
            rule_id: rule_id.into(),
            target_rule_id: target_rule_id.into(),
        });
    }

    /// Disclose `count` rows this stage ADDED under `label` — the addition-side counterpart
    /// of `other`'s escape hatch. See [`Self::added`]'s doc comment.
    pub fn push_added(&mut self, label: impl Into<String>, count: usize) {
        self.added.push((label.into(), count));
    }
}

/// One pipeline stage's row-accounting for a single instrumented pass. The invariant (now
/// bidirectional — W1 Pass-1 seal fix):
/// `rows_in + added_total == rows_out + accounted_total + unaccounted`,
/// i.e. a stage may EITHER shrink the row set (`rows_in > rows_out`, explained by
/// `merged_into`/`routed_held`/`routed_informational`/`deduped`/`other`) OR grow it
/// (`rows_out > rows_in`, explained by `added`) — never both reconciling loosely. `unaccounted`
/// is computed by [`ScanLedger::record_stage`] as the remainder in WHICHEVER direction applies
/// — never supplied directly — so an instrumentation site cannot accidentally claim a clean
/// reconciliation it didn't actually prove.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageLedgerEntry {
    pub stage: String,
    pub rows_in: usize,
    pub rows_out: usize,
    pub merged_into: Vec<MergedRow>,
    pub routed_held: usize,
    pub routed_informational: usize,
    pub deduped: usize,
    pub other: Vec<(String, usize)>,
    /// See [`StageAccounting::added`]'s doc comment. `#[serde(default)]` so a persisted
    /// pre-W1-Pass-1 ledger (no stage ever recorded an addition) still deserializes.
    #[serde(default)]
    pub added: Vec<(String, usize)>,
    pub unaccounted: usize,
}

impl StageLedgerEntry {
    pub fn is_integral(&self) -> bool {
        self.unaccounted == 0
    }

    /// W2: the total of every NAMED REMOVAL-side disposition this entry recorded (merged +
    /// held + informational + deduped + the `other` escape-hatch buckets) — i.e. `rows_in -
    /// rows_out` minus whatever `unaccounted` already had to absorb, when this stage shrank the
    /// row set. Mirrors [`StageAccounting::accounted`] but over the already-reconciled,
    /// persisted entry, so a caller (or test) can state the `rows_in == rows_out +
    /// accounted_total() + unaccounted` identity explicitly instead of only checking
    /// `unaccounted == 0`. Zero (not meaningful) for a stage that grew the row set instead —
    /// see [`Self::added_total`] for that direction.
    pub fn accounted_total(&self) -> usize {
        self.merged_into.len()
            + self.routed_held
            + self.routed_informational
            + self.deduped
            + self.other.iter().map(|(_, n)| n).sum::<usize>()
    }

    /// The ADDITION-side counterpart of [`Self::accounted_total`]: the total of every named
    /// reason this stage's `rows_out` exceeded its `rows_in` (e.g. the external-tool preview
    /// merge's net-new findings, or the evidence gate's synthesized advisory rows). Zero for a
    /// stage that shrank or left the row set unchanged.
    pub fn added_total(&self) -> usize {
        self.added.iter().map(|(_, n)| n).sum::<usize>()
    }
}

/// The scan-wide evaluation ledger: per-rule execution facts + per-stage row accounting,
/// accumulated during a scan (see `onboard::audit_repos`, `ai_audit::audit_repo`) and carried
/// into the report build (`report_export::build_report_json`). See the module doc comment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanLedger {
    rules: HashMap<String, RuleLedgerEntry>,
    stages: Vec<StageLedgerEntry>,
    /// Rule ids queued for [`RuleLedgerEntry::coverage_note`] via
    /// [`Self::note_config_gated_rule`], consumed by every subsequent [`Self::record_rule`]
    /// call for the same id. Transient scan-time plumbing only — a rule's settled
    /// `coverage_note` lives on its [`RuleLedgerEntry`] once recorded, so this queue is never
    /// meaningful to persist across a (de)serialize round-trip.
    #[serde(skip)]
    config_gated_pending: BTreeSet<String>,
    /// Rule ids queued for [`INCOMPLETE_SCAN_NOTE`] via [`Self::note_incomplete_scan_rule`],
    /// consumed exactly like `config_gated_pending`; the incomplete-scan note wins when both
    /// are queued. Transient scan-time plumbing only.
    #[serde(skip)]
    incomplete_scan_pending: BTreeSet<String>,
    /// Rule ids queued for [`ADVISORY_UNAVAILABLE_NOTE`] via
    /// [`Self::note_advisory_unavailable_rule`]: a clone-based rule whose deterministic pass
    /// found candidates but whose reviewed judgment did not run. Transient scan-time plumbing.
    #[serde(skip)]
    advisory_unavailable_pending: BTreeSet<String>,
    /// Rule ids queued for [`ADVISORY_PARTIAL_NOTE`] via [`Self::note_advisory_partial_rule`]:
    /// a clone-based rule whose judgment ran but left candidates undecided. Transient.
    #[serde(skip)]
    advisory_partial_pending: BTreeSet<String>,
    /// Rule ids queued for [`RuleLedgerEntry::not_applicable`] via [`Self::
    /// note_not_applicable_rule`], consumed by every subsequent [`Self::record_rule`] call for
    /// the same id — same queue-then-settle shape as `config_gated_pending`, for the same
    /// reason (the rule's `ran` fact is settled later than the not-applicable determination).
    /// Transient scan-time plumbing only.
    #[serde(skip)]
    not_applicable_pending: BTreeSet<String>,
    /// What this scan was actually FED. See [`ScanInputFacts`].
    #[serde(default)]
    input: ScanInputFacts,
    /// Whether the sealed rows' paths are really paths. See [`PathHygiene`].
    #[serde(default)]
    path_hygiene: PathHygiene,
    /// Per-stage token and cost telemetry for the scan (operator-only; see
    /// [`crate::stage_telemetry`]). Not an integrity fact: no gate reads it and no exporter
    /// renders it. `None` for a scan that made no model call or a ledger written before this
    /// field existed.
    #[serde(default)]
    stage_telemetry: Option<crate::stage_telemetry::StageTelemetry>,
    /// The question this scan's AI findings answered (model, prompt version, rule text). The
    /// delta rescan rebuilds its prior manifest from a stored inspection, and the inspection
    /// carries this ledger, so this is how the rebuilt prior proves its carried findings still
    /// answer today's question. `None` for a ledger written before the key existed; the rescan
    /// then re-scans in full. Operator-only, like [`Self::stage_telemetry`].
    #[serde(default)]
    ai_key: Option<crate::scan_cache::AiScanKey>,
    /// Which token-efficiency measures were on for this scan ([`crate::efficiency`]), so a cost
    /// or behavior change between two runs can be traced to a switch. Operator-only. `None`
    /// for a ledger written before the switches existed.
    #[serde(default)]
    efficiency: Option<crate::efficiency::EfficiencyConfig>,
    /// What the `--max-cost-usd` spend cap did, when one was set: actual spend and everything
    /// it stopped. Operator-only (it carries dollar figures); the export gets only a
    /// "capped" fact. See [`crate::cost_cap`].
    #[serde(default)]
    cost_cap: Option<crate::cost_cap::CostCapRecord>,
}

/// The facts about a scan's INPUT, recorded so the integrity gate can judge them.
///
/// # Why the gate needed this, and why it lives on the ledger
///
/// Every family the gate had asked a question about the MACHINERY: did the tier run, was the
/// skip justified, do the stages reconcile. All of them were satisfied on 2026-10-16 by a
/// hearth scan whose audit prompt was **63% Camerata's own prior output** — the engine had
/// written `.camerata-scan-cache.json` into the scanned repo and collected it back as source
/// on the next run, displacing real code until `code_chars` went from 103,307 to 277,543 at
/// an unchanged 88 files. Twelve rules then "found nothing", the gate passed, and the defect
/// was caught only by an external grader reading a shipped client report.
///
/// No output-side check could have caught that honestly. A regression floor comparing this
/// export against a previous one would have reported twelve rule regressions and sent
/// somebody to read twelve rule definitions — which is exactly what happened, twice, before
/// the input was measured. **The question "did we feed it the right thing" is cheaper, is
/// decidable, and is the one that was never asked.**
///
/// These live on the LEDGER rather than being passed alongside it because
/// `camerata integrity-gate --ledger <path>` evaluates a ledger read from disk, often on a
/// different machine or days later. A fact the gate cannot find in that file is a fact the
/// standalone gate cannot check at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanInputFacts {
    /// Files admitted into the scan, after noise pruning.
    #[serde(default)]
    pub files_scanned: usize,
    /// Files pruned as build/dep/cache/generated noise before scanning.
    #[serde(default)]
    pub files_excluded: usize,
    /// Characters of code across the admitted files — the honest size of the digest the model
    /// was shown.
    #[serde(default)]
    pub code_chars: usize,
    /// Files admitted whose path matched the engine's own reserved `.camerata-` prefix
    /// (`onboard::files::is_engine_artifact`).
    ///
    /// **This must be zero.** Any other value means Camerata audited its own output, which is
    /// the 2026-10-16 defect exactly. Counted at collection time, where the predicate already
    /// runs, so recording it costs nothing and needs no new instrumentation — a gate that
    /// needs new instrumentation to work is a gate that silently passes until somebody
    /// remembers to instrument.
    #[serde(default)]
    pub engine_artifacts_admitted: usize,
    /// The model the audit pass actually used, as resolved rather than as requested.
    ///
    /// `None` means NOT RECORDED, which is not the same as "no model ran" — the resolution
    /// chain falls through to `CAMERATA_AUDIT_MODEL` and then the client default, and only an
    /// explicit caller pick used to be written down. That gap made every before/after
    /// comparison in the 2026-10-16 sweep unfalsifiable: the baseline recorded
    /// `claude-sonnet-5` and both later runs recorded nothing, so "the engine regressed" could
    /// be neither confirmed nor refuted. A graded run must not repeat that.
    #[serde(default)]
    pub audit_model: Option<String>,
    /// Auditable files the scan did NOT read, by path and reason (over the size cap, or past
    /// the file cap). H4: listed under Coverage in the report and `findings.json` so a skipped
    /// file is named, never silently absent. `serde(default)` so an older ledger still loads.
    #[serde(default)]
    pub skipped_files: Vec<SkippedFile>,
    /// Skipped files NOT named in `skipped_files`, because the list holds at most
    /// [`SKIPPED_FILES_LISTED_PER_REASON`] paths per reason. Every renderer states it as "and K
    /// more", so a bounded list never reads as the whole list. `serde(default)` for older ledgers.
    #[serde(default)]
    pub skipped_beyond_cap_total: usize,
}

/// How many skipped paths are named per reason. A huge repository can have tens of thousands of
/// files past the file cap; a report that listed them all would bury the rest of Coverage, so
/// the first paths (in walk order) are named and the remainder is counted.
pub const SKIPPED_FILES_LISTED_PER_REASON: usize = 1_000;

/// Collects skipped files while honouring [`SKIPPED_FILES_LISTED_PER_REASON`]. Pure, so the
/// bound is testable without a real tree.
#[derive(Debug, Default)]
pub struct SkippedFileList {
    listed: Vec<SkippedFile>,
    listed_per_reason: std::collections::HashMap<SkipReason, usize>,
    beyond_cap_total: usize,
}

impl SkippedFileList {
    /// Record one skipped file: named while its reason is under the bound, counted after.
    pub fn push(&mut self, repo: &str, path: String, reason: SkipReason) {
        let seen = self.listed_per_reason.entry(reason).or_insert(0);
        if *seen < SKIPPED_FILES_LISTED_PER_REASON {
            *seen += 1;
            self.listed.push(SkippedFile {
                repo: repo.to_string(),
                path,
                reason,
            });
        } else {
            self.beyond_cap_total += 1;
        }
    }

    /// The named files and the count of those not named.
    pub fn finish(self) -> (Vec<SkippedFile>, usize) {
        (self.listed, self.beyond_cap_total)
    }
}

/// One auditable file the scan did not read. See [`ScanInputFacts::skipped_files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFile {
    /// The repo (`owner/repo`).
    pub repo: String,
    /// Repo-relative, forward-slashed path.
    pub path: String,
    pub reason: SkipReason,
}

/// Why a file was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Larger than `onboard::files::MAX_FILE_BYTES` (400 KB).
    Oversize,
    /// Past `onboard::files::HARD_CAP_FILES` (20,000 files) in its repo.
    BeyondFileCap,
}

impl SkipReason {
    /// The client-facing reason, as the Coverage section states it.
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::Oversize => "over the 400 KB file size limit",
            SkipReason::BeyondFileCap => "beyond the 20,000-file limit",
        }
    }
}

/// W21 — whether every row's `path` is actually a repository-relative FILE PATH.
///
/// # The two shapes this counts, both measured in a shipped export
///
/// On the clean 2026-10-18 hearth export, 42 of 237 rows carried something other than a path
/// in the field a client reads as one:
///
/// - **32 absolute paths**, every one from the ESLint preview tier, of the form
///   `/Users/auditor/Documents/Repos/.../app/admin/page.tsx`. That discloses the auditor's
///   username and directory habits, and it is useless to the client, who cannot open a path
///   rooted on a machine they have no access to.
/// - **10 prose paths**, where an unparseable evidence citation put the whole sentence in the
///   field: `"app/api/bookings/route.ts:15-39 — booking insert, payment insert, and slot
///   update execute as three separate awaits with no transaction boundary"`, 180 characters
///   rendered as a file path in the PDF and the workbook.
///
/// Both sources are fixed. This counts what reaches the SEALED row set anyway, because a
/// future tier can reintroduce either and neither source fix can see that happen.
///
/// # Why it is computed at the seal point
///
/// [`crate::onboard::ScanReport::seal`] is the documented moment the row set becomes final —
/// every row-adding pass has run and no exporter can mutate it afterwards. That is the only
/// place where "every row" is a complete and stable statement, and it is upstream of all
/// three exporters, so one audit covers the PDF, the workbook and the JSON rather than
/// needing a check per renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathHygiene {
    /// Rows examined.
    #[serde(default)]
    pub rows_total: usize,
    /// Rows whose `path` is absolute — a POSIX leading `/` or a Windows `C:\` / `C:/` drive
    /// prefix. Always wrong in an artifact: the client's checkout is somewhere else.
    #[serde(default)]
    pub absolute: usize,
    /// Rows whose `path` contains a backslash separator. Wrong even on Windows, because the
    /// artifacts and the client's own tooling both address files with forward slashes.
    #[serde(default)]
    pub backslash: usize,
    /// Rows whose `path` is prose rather than a path: it carries an em/en-dash note
    /// separator, joins several locations with `, `, or exceeds any plausible path length.
    #[serde(default)]
    pub prose: usize,
    /// One offending value, truncated, so a failure names something instead of only counting.
    /// A gate that reports "7 bad paths" sends somebody grepping; one that quotes the worst
    /// row sends them to the right file.
    #[serde(default)]
    pub worst_example: Option<String>,
    /// Distinct rows failing at least one axis. Tracked rather than derived, because the
    /// three counts overlap unpredictably: 32 absolute plus 10 prose is 42 bad rows when the
    /// sets are disjoint and 32 when prose is a subset, and a max() or a sum would be wrong
    /// in opposite directions.
    #[serde(default)]
    pub offending_rows: usize,
}

impl PathHygiene {
    /// Classify one row's `path`. Returns `(absolute, backslash, prose)`.
    ///
    /// The prose signals are deliberately narrow and drawn from the measured defect rather
    /// than from what prose could look like in principle. A real repository path may contain
    /// spaces (the hearth fixture lives under a folder whose name has one), so spaces alone prove
    /// nothing; `" - "` is excluded too, despite the citation parser splitting on it, because
    /// a hyphen with spaces is plausible enough in a path that flagging it would cry wolf.
    /// What a path never contains is an em-dash note separator, a `, ` joining two locations,
    /// or 200 characters.
    pub fn classify(path: &str) -> (bool, bool, bool) {
        let bytes = path.as_bytes();
        let absolute = path.starts_with('/')
            || (bytes.len() > 2
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/'));
        let backslash = path.contains('\\');
        let prose =
            path.contains(" — ") || path.contains(" – ") || path.contains(", ") || path.len() > 200;
        (absolute, backslash, prose)
    }

    /// `true` when nothing is wrong.
    pub fn is_clean(&self) -> bool {
        self.absolute == 0 && self.backslash == 0 && self.prose == 0
    }

    /// Audit a whole row set. `paths` is every sealed row's `path`.
    pub fn audit<'a>(paths: impl Iterator<Item = &'a str>) -> Self {
        let mut h = Self::default();
        for path in paths {
            h.rows_total += 1;
            if path.is_empty() {
                // An intentionally unlocated row (W21: an unparseable citation now yields an
                // empty path rather than prose). Absence is correct here, not a defect.
                continue;
            }
            let (absolute, backslash, prose) = Self::classify(path);
            if absolute {
                h.absolute += 1;
            }
            if backslash {
                h.backslash += 1;
            }
            if prose {
                h.prose += 1;
            }
            if absolute || backslash || prose {
                h.offending_rows += 1;
                // Keep the LONGEST offender: prose paths are the hardest to recognise from a
                // count alone, and they are the long ones.
                if h.worst_example
                    .as_ref()
                    .is_none_or(|w| w.len() < path.len())
                {
                    h.worst_example = Some(path.chars().take(160).collect());
                }
            }
        }
        h
    }
}

impl ScanLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.stages.is_empty()
    }

    /// Record what this scan was fed. See [`ScanInputFacts`].
    ///
    /// Called once, at the point the file collection and the model resolution are both
    /// settled. Overwrites rather than accumulates: these are facts about one scan, not a
    /// running total, and a second call means a second scan that should have its own ledger.
    pub fn record_input(&mut self, input: ScanInputFacts) {
        self.input = input;
    }

    /// Record the scan's per-stage cost telemetry. Overwrites: one scan, one record.
    pub fn record_stage_telemetry(&mut self, telemetry: crate::stage_telemetry::StageTelemetry) {
        self.stage_telemetry = Some(telemetry);
    }

    /// Record the question this scan's AI findings answered. See [`Self::ai_key`].
    pub fn record_ai_key(&mut self, key: crate::scan_cache::AiScanKey) {
        self.ai_key = Some(key);
    }

    /// Record which efficiency measures were on. Overwrites: one scan, one record.
    pub fn record_efficiency(&mut self, efficiency: crate::efficiency::EfficiencyConfig) {
        self.efficiency = Some(efficiency);
    }

    /// The efficiency measures that were on, when the scan recorded them.
    pub fn efficiency(&self) -> Option<&crate::efficiency::EfficiencyConfig> {
        self.efficiency.as_ref()
    }

    /// Record what the spend cap did. Overwrites: one scan, one record.
    pub fn record_cost_cap(&mut self, record: crate::cost_cap::CostCapRecord) {
        self.cost_cap = Some(record);
    }

    /// What the spend cap did, when one was set.
    pub fn cost_cap(&self) -> Option<&crate::cost_cap::CostCapRecord> {
        self.cost_cap.as_ref()
    }

    /// True when the spend cap stopped part of this scan, so the inspection is incomplete.
    pub fn is_cost_capped(&self) -> bool {
        self.cost_cap
            .as_ref()
            .is_some_and(crate::cost_cap::CostCapRecord::stopped_the_run)
    }

    /// The question this scan's AI findings answered, when it recorded one.
    pub fn ai_key(&self) -> Option<&crate::scan_cache::AiScanKey> {
        self.ai_key.as_ref()
    }

    /// The scan's per-stage cost telemetry, when it recorded any.
    pub fn stage_telemetry(&self) -> Option<&crate::stage_telemetry::StageTelemetry> {
        self.stage_telemetry.as_ref()
    }

    /// What this scan was fed. Default (all zero, no model) when nothing recorded it — which
    /// the gate treats as unproven rather than clean, since a scan that did not say what it
    /// read cannot be shown to have read the right thing.
    pub fn input(&self) -> &ScanInputFacts {
        &self.input
    }

    /// Record the sealed row set's path hygiene. Called from
    /// [`crate::onboard::ScanReport::seal`], the one point the row set is final.
    pub fn record_path_hygiene(&mut self, hygiene: PathHygiene) {
        self.path_hygiene = hygiene;
    }

    /// Whether the sealed rows' paths are really paths. See [`PathHygiene`].
    pub fn path_hygiene(&self) -> &PathHygiene {
        &self.path_hygiene
    }

    /// Queue the honest "config-gated, reviewed semantically instead" disclosure for
    /// `rule_id`, consumed by [`Self::record_rule`] on every call (past or future) for the
    /// same id. Called from `onboard::audit_repos`'s deterministic-phase loop for a CI-tier
    /// rule that [`camerata_checks::arch_checker::any_registered_checker_answers`] confirms
    /// has a REAL registered checker, but whose checker produced no credited deterministic
    /// verdict for THIS repo (missing `.camerata/architecture.toml`) — see
    /// [`CONFIG_GATED_NOTE`]. Called BEFORE the rule's `ran` fact is known (the semantic-phase
    /// recording further down `audit_repos`'s loop settles that), which is why this queues the
    /// note rather than writing it directly onto an entry that may not exist yet.
    pub fn note_config_gated_rule(&mut self, rule_id: impl Into<String>) {
        self.config_gated_pending.insert(rule_id.into());
    }

    /// Queue the [`INCOMPLETE_SCAN_NOTE`] disclosure for a rule the scan WITHHELD because it
    /// reasons from the absence of a reference and the scan stopped at its file limit. Same
    /// queue-then-settle shape as [`Self::note_config_gated_rule`].
    pub fn note_incomplete_scan_rule(&mut self, rule_id: impl Into<String>) {
        self.incomplete_scan_pending.insert(rule_id.into());
    }

    /// Queue the [`ADVISORY_UNAVAILABLE_NOTE`] disclosure for a clone-based rule (Wave C) whose
    /// deterministic pass found candidate sites but whose reviewed judgment did not run this
    /// scan (no model, deterministic-only, compliance block). Same queue-then-settle shape as
    /// [`Self::note_incomplete_scan_rule`].
    pub fn note_advisory_unavailable_rule(&mut self, rule_id: impl Into<String>) {
        self.advisory_unavailable_pending.insert(rule_id.into());
    }

    /// Queue the [`ADVISORY_PARTIAL_NOTE`] disclosure for a clone-based rule whose judgment ran
    /// but left candidate sites undecided (over the per-scan limit, `unsure`, no verdict, or a
    /// failed batch).
    pub fn note_advisory_partial_rule(&mut self, rule_id: impl Into<String>) {
        self.advisory_partial_pending.insert(rule_id.into());
    }

    /// The coverage note queued for `rule_id`. Precedence: the incomplete-scan note, then the
    /// advisory-unavailable note, then the advisory-partial note, then config-gated.
    fn pending_coverage_note(&self, rule_id: &str) -> Option<String> {
        if self.incomplete_scan_pending.contains(rule_id) {
            Some(INCOMPLETE_SCAN_NOTE.to_string())
        } else if self.advisory_unavailable_pending.contains(rule_id) {
            Some(ADVISORY_UNAVAILABLE_NOTE.to_string())
        } else if self.advisory_partial_pending.contains(rule_id) {
            Some(ADVISORY_PARTIAL_NOTE.to_string())
        } else {
            self.config_gated_pending
                .contains(rule_id)
                .then(|| CONFIG_GATED_NOTE.to_string())
        }
    }

    /// Pass-1 W2 (A5.4): queue the "judged not applicable" disclosure for `rule_id`, consumed
    /// by [`Self::record_rule`] on every call (past or future) for the same id — same
    /// queue-then-settle shape as [`Self::note_config_gated_rule`]. Called from `onboard::
    /// audit_repos`'s semantic-phase recording for every rule id a repo's `RuleRecommendation`
    /// marked `applicable == false`, BEFORE the rule's `ran` fact is known for this call.
    pub fn note_not_applicable_rule(&mut self, rule_id: impl Into<String>) {
        self.not_applicable_pending.insert(rule_id.into());
    }

    /// Record (or, for a rule already seen this scan — e.g. a multi-repo run — ACCUMULATE)
    /// one rule's outcome. Accumulation rule: `ran` is OR'd (one repo running it is enough to
    /// call it run), `skip_reason` is kept from the first not-run observation and cleared the
    /// moment any repo actually ran it, and the usage counters sum across repos.
    ///
    /// If [`Self::note_config_gated_rule`] was ever called for this `rule_id` (in this call or
    /// an earlier one), the resulting entry's `coverage_note` is set to [`CONFIG_GATED_NOTE`]
    /// and stays set regardless of `ran` — see [`RuleLedgerEntry::coverage_note`]'s doc comment
    /// for why this is deliberately independent of the `ran`/`skip_reason` accumulation rules.
    /// Likewise, if [`Self::note_not_applicable_rule`] was ever called for this `rule_id`, the
    /// resulting entry's `not_applicable` is `true` and stays `true` for the rest of this scan
    /// (OR-accumulated, same direction as `ran` — a determination that the concern is absent
    /// from one repo in a multi-repo scan is still a genuine finding about THAT repo, and a
    /// multi-repo scan's ledger is project-wide; this is a documented simplification, the same
    /// one `ScanReport::recommendations`'s own doc comment already makes for the "last repo
    /// wins" shape of the underlying recommendation).
    pub fn record_rule(
        &mut self,
        rule_id: impl Into<String>,
        tier: RuleTier,
        ran: bool,
        skip_reason: Option<String>,
        files_evaluated: usize,
        findings_emitted: usize,
    ) -> &RuleLedgerEntry {
        let rule_id = rule_id.into();
        let coverage_note = self.pending_coverage_note(&rule_id);
        let not_applicable = self.not_applicable_pending.contains(&rule_id);
        self.rules
            .entry(rule_id.clone())
            .and_modify(|e| {
                e.ran = e.ran || ran;
                if e.ran {
                    e.skip_reason = None;
                } else if e.skip_reason.is_none() {
                    e.skip_reason = skip_reason.clone();
                }
                e.files_evaluated += files_evaluated;
                e.findings_emitted += findings_emitted;
                if coverage_note.is_some() {
                    e.coverage_note = coverage_note.clone();
                }
                e.not_applicable = e.not_applicable || not_applicable;
            })
            .or_insert(RuleLedgerEntry {
                rule_id: rule_id.clone(),
                tier,
                ran,
                skip_reason,
                files_evaluated,
                findings_emitted,
                coverage_note,
                not_applicable,
                partial_reason: None,
            });
        self.rules.get(&rule_id).expect("just inserted")
    }

    /// Mark a rule that [`Self::record_rule`] already recorded as having run on only PART of the
    /// code (see [`RuleLedgerEntry::partial_reason`]). The first reason is kept. A rule the
    /// ledger has no entry for is ignored: there is nothing to qualify.
    pub fn note_partial_rule(&mut self, rule_id: &str, reason: impl Into<String>) {
        if let Some(entry) = self.rules.get_mut(rule_id) {
            entry.partial_reason.get_or_insert_with(|| reason.into());
        }
    }

    /// W13 — record a rule whose CORPUS FILE could not be parsed, so the rule never entered
    /// this scan's universe at all.
    ///
    /// # The defect this closes
    ///
    /// `camerata_rules::load_corpus_lenient` returns `(RuleSet, Vec<(PathBuf, RulesError)>)` and
    /// every production call site bound the second element to `_errs`. A rule file with a
    /// typo'd enum value therefore failed to parse, vanished from every scan, and NOTHING
    /// reported it: not this ledger, not the coverage sheet, not `camerata integrity-gate`, not
    /// a log line. That is a second route to the H1 outcome the mechanical build gate exists to
    /// prevent ("a rule that could never run for ANY stack is a DEFECT"), and it is strictly
    /// worse than the first, because a rule that never LOADED is not even a skip — it is absent
    /// from the ledger's universe, so the gate could not see it even in principle.
    ///
    /// This method is the fix's entry point into the ledger: it makes the absent rule PRESENT
    /// as `ran = false` with [`CORPUS_LOAD_FAILED_REASON`], using the ledger's own existing
    /// vocabulary rather than a parallel channel. Everything downstream then works unchanged:
    /// [`Self::excluded_rules`] lists it (so both the report JSON and the workbook disclose it),
    /// [`Self::healthy_rule_ids`] excludes it (it can never read as verified-clean), and
    /// [`crate::integrity_gate::SkipClass::classify`] maps the reason to its own
    /// `corpus_load_failed` class, which fails the gate.
    ///
    /// `rule_id` is derived from the file STEM by the caller
    /// (`camerata_server::rule_id_from_corpus_path`) — the id inside the file is unreadable by
    /// definition, and in this corpus the stem IS the lowercased rule id for every file.
    ///
    /// # Why this is NOT just a [`Self::record_rule`] call
    ///
    /// It was, in the first cut, and the e2e harness caught it immediately. `record_rule`
    /// ACCUMULATES: it keeps the `skip_reason` from the FIRST not-run observation. By the time
    /// this runs, `audit_repos` has usually already recorded every SELECTED rule — and a
    /// selection can still name a rule whose file has since broken (selections are persisted
    /// per project; the corpus moves underneath them). So the ledger already held
    /// `skip_reason: "AI/semantic review not requested this run"`, and `record_rule` dropped
    /// the real reason on the floor. That is worse than the defect being fixed: the ledger
    /// AFFIRMATIVELY CLAIMED a justified scope choice for a rule that did not exist, which
    /// classifies as [`crate::integrity_gate::SkipClass::ScopedOut`] and PASSES the gate.
    ///
    /// Three cases, each handled by what is actually true rather than by one blanket rule:
    ///
    /// 1. **No prior entry** (the dominant production shape — the selection is derived FROM the
    ///    corpus, so a rule missing from the corpus is normally never selected). Insert at
    ///    [`RuleTier::Unloaded`] with [`CORPUS_LOAD_FAILED_REASON`].
    /// 2. **A prior entry that did NOT run.** The corpus-load reason WINS: any other recorded
    ///    reason is incidental (derived from a selection list, not from the corpus) while "its
    ///    file would not parse" is the dominant, root cause. `tier` and the counters are
    ///    PRESERVED, deliberately — retiering would silently move the rule out of whichever
    ///    engine's bucket the gate's `dark_tier` / `tier_absent` arithmetic counted it in.
    /// 3. **A prior entry that DID run.** Reachable: the always-on content floor
    ///    (`onboard::AUDIT_RULES`) keys on hardcoded rule IDS, not on corpus content, so its
    ///    detector can genuinely evaluate a rule whose `.toml` is broken. `ran`, the counters
    ///    and `skip_reason` are left EXACTLY as they stand — walking `ran` back to `false`
    ///    would be a false claim, and zeroing `findings_emitted` would erase real findings from
    ///    the ledger's own accounting.
    ///
    /// In all three cases [`RuleLedgerEntry::coverage_note`] is set to
    /// [`CORPUS_LOAD_FAILED_NOTE`]. That field exists for precisely this shape — an "honest,
    /// ADDITIVE disclosure that survives regardless of `ran`/`skip_reason`" — and it gives the
    /// integrity gate ONE uniform trigger that works in case 3, where there is no skip reason
    /// to classify. A rule that ran without its authored text still produced a degraded report
    /// (no authored remediation, no severity bounds, no options), so it must still fail the
    /// gate.
    pub fn record_corpus_load_failure(&mut self, rule_id: impl Into<String>) -> &RuleLedgerEntry {
        let rule_id = rule_id.into();
        match self.rules.get_mut(&rule_id) {
            Some(existing) => {
                if !existing.ran {
                    existing.skip_reason = Some(CORPUS_LOAD_FAILED_REASON.to_string());
                }
                existing.coverage_note = Some(CORPUS_LOAD_FAILED_NOTE.to_string());
            }
            None => {
                self.rules.insert(
                    rule_id.clone(),
                    RuleLedgerEntry {
                        rule_id: rule_id.clone(),
                        tier: RuleTier::Unloaded,
                        ran: false,
                        skip_reason: Some(CORPUS_LOAD_FAILED_REASON.to_string()),
                        files_evaluated: 0,
                        findings_emitted: 0,
                        coverage_note: Some(CORPUS_LOAD_FAILED_NOTE.to_string()),
                        not_applicable: false,
                        partial_reason: None,
                    },
                );
            }
        }
        self.rules.get(&rule_id).expect("just recorded")
    }

    /// Record one stage's row accounting, computing `unaccounted` as the remainder NOT
    /// explained by `accounting`, in WHICHEVER direction `rows_in`/`rows_out` actually moved:
    /// `rows_in > rows_out` (a stage that shrank the row set) reconciles against
    /// `accounting.accounted()` exactly as before; `rows_out > rows_in` (a stage that GREW the
    /// row set — W1 Pass-1 seal fix) reconciles against `accounting.added_total()` instead. A
    /// stage can never owe both directions at once (one of `rows_in.saturating_sub(rows_out)` /
    /// `rows_out.saturating_sub(rows_in)` is always zero), so `unaccounted` is simply whichever
    /// side's shortfall is nonzero. Multiple entries for the SAME stage name are allowed (e.g.
    /// one per repo in a multi-repo scan) — each is reconciled independently. Returns the
    /// freshly-pushed entry so the caller can immediately check `is_integral()` / build a
    /// runtime disclosure (see [`stage_disclosure`]).
    pub fn record_stage(
        &mut self,
        stage: impl Into<String>,
        rows_in: usize,
        rows_out: usize,
        accounting: StageAccounting,
    ) -> &StageLedgerEntry {
        let lost = rows_in.saturating_sub(rows_out);
        let gained = rows_out.saturating_sub(rows_in);
        let accounted = accounting.accounted();
        let added_total = accounting.added_total();
        let unaccounted_loss = lost.saturating_sub(accounted);
        let unaccounted_gain = gained.saturating_sub(added_total);
        let unaccounted = unaccounted_loss + unaccounted_gain;
        self.stages.push(StageLedgerEntry {
            stage: stage.into(),
            rows_in,
            rows_out,
            merged_into: accounting.merged_into,
            routed_held: accounting.routed_held,
            routed_informational: accounting.routed_informational,
            deduped: accounting.deduped,
            other: accounting.other,
            added: accounting.added,
            unaccounted,
        });
        self.stages.last().expect("just pushed")
    }

    pub fn rules(&self) -> impl Iterator<Item = &RuleLedgerEntry> {
        self.rules.values()
    }

    /// Run `redact` over the free text of every rule entry (`skip_reason`, `coverage_note`) so no
    /// secret value is persisted in the ledger; `redact` returns how many values it replaced.
    pub(crate) fn redact_text(&mut self, redact: &mut dyn FnMut(&mut String) -> usize) -> usize {
        let mut count = 0;
        for entry in self.rules.values_mut() {
            if let Some(reason) = entry.skip_reason.as_mut() {
                count += redact(reason);
            }
            if let Some(note) = entry.coverage_note.as_mut() {
                count += redact(note);
            }
        }
        count
    }

    /// Forcibly OVERWRITE a rule's `ran`/`skip_reason`/`findings_emitted` facts — unlike
    /// [`record_rule`](Self::record_rule) (which OR-accumulates `ran` across repos within a
    /// scan, appropriate for a multi-repo run), this REPLACES whatever was recorded.
    ///
    /// # Why this exists (W3)
    ///
    /// The scan-time external-tool pass (`crate::merge_scan_preview` / `scan_tools::run_scan_tools`)
    /// runs in a SEPARATE, later stage than the one that first populates the ledger
    /// (`onboard::audit_repos`'s CI-tier rule loop, which optimistically records a
    /// semgrep-mapped rule as `ran = true, findings_emitted = 0` on the strength of "a semgrep
    /// rule EXISTS for this corpus rule," before the tool has actually run for this scan).
    /// That optimism is sometimes wrong — the tool can be absent, fail to provision, or error
    /// out entirely — and by the time the real pass's result is known, `record_rule`'s
    /// OR-accumulate semantics can no longer walk `ran` back to `false` (by design: it must
    /// never let a later "it didn't run here" silently erase an earlier real "it ran there").
    /// This method is the narrow, explicit escape hatch for that one correction: the caller
    /// (`crate::reconcile_external_tool_ledger`) uses it ONLY after the external-tool pass has
    /// actually completed, to replace a speculative pre-pass entry with the real outcome —
    /// never to downgrade a rule that genuinely ran somewhere.
    ///
    /// Preserves the existing entry's `tier`/`files_evaluated`/`coverage_note` when one is
    /// already present (there usually is, from the pre-pass optimistic record); defaults to
    /// [`RuleTier::ExternalTool`] / `0` / `None` for a rule_id the ledger hasn't seen at all
    /// yet. Also applies [`Self::note_config_gated_rule`]'s queued note, same as
    /// [`record_rule`](Self::record_rule), for consistency even though no external-tool rule
    /// is config-gated today.
    pub fn correct_rule_after_external_pass(
        &mut self,
        rule_id: impl Into<String>,
        ran: bool,
        skip_reason: Option<String>,
        findings_emitted: usize,
    ) -> &RuleLedgerEntry {
        let rule_id = rule_id.into();
        let (tier, files_evaluated, existing_note, existing_not_applicable) = self
            .rules
            .get(&rule_id)
            .map(|e| {
                (
                    e.tier,
                    e.files_evaluated,
                    e.coverage_note.clone(),
                    e.not_applicable,
                )
            })
            .unwrap_or((RuleTier::ExternalTool, 0, None, false));
        let coverage_note = existing_note.or_else(|| self.pending_coverage_note(&rule_id));
        let not_applicable =
            existing_not_applicable || self.not_applicable_pending.contains(&rule_id);
        self.rules.insert(
            rule_id.clone(),
            RuleLedgerEntry {
                rule_id: rule_id.clone(),
                tier,
                ran,
                skip_reason,
                files_evaluated,
                findings_emitted,
                coverage_note,
                not_applicable,
                partial_reason: None,
            },
        );
        self.rules.get(&rule_id).expect("just inserted")
    }

    pub fn rule(&self, rule_id: &str) -> Option<&RuleLedgerEntry> {
        self.rules.get(rule_id)
    }

    pub fn stages(&self) -> &[StageLedgerEntry] {
        &self.stages
    }

    /// Rule ids that genuinely ran and emitted zero findings — the ONLY honest definition of
    /// "verified clean" / "What's healthy". Sorted for stable output.
    pub fn healthy_rule_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .rules
            .values()
            .filter(|r| r.verified_clean())
            .map(|r| r.rule_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Pass-1 W2 (A5.4): rule ids judged NOT APPLICABLE to this codebase (positive evidence
    /// the concern is absent, never the default) — its OWN outcome bucket, distinct from
    /// [`Self::healthy_rule_ids`] (which a `not_applicable` rule is EXCLUDED from — see
    /// [`RuleLedgerEntry::verified_clean`]). Sorted for stable output.
    pub fn not_applicable_rule_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .rules
            .values()
            .filter(|r| r.not_applicable)
            .map(|r| r.rule_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Rule ids that did NOT run this scan, or ran on only part of the code, each with its
    /// reason — the "excluded from this audit" section. Sorted by rule id for stable output.
    pub fn excluded_rules(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = self
            .rules
            .values()
            .filter(|r| !r.ran || r.partial_reason.is_some())
            .map(|r| {
                (
                    r.rule_id.as_str(),
                    // A rule that ran on part of the code is listed with WHY it is incomplete.
                    r.partial_reason
                        .as_deref()
                        .or(r.skip_reason.as_deref())
                        .unwrap_or("did not run this scan"),
                )
            })
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// Every rule id that emitted at least one finding this scan — a category/scorecard
    /// badge may never read "Clean" for a rule in this set, regardless of whether that
    /// finding survived as its own row or was folded into another row's `also_matches` by a
    /// later merge stage (findings are counted here at DETECTION time, before any merge).
    pub fn fired_rule_ids(&self) -> HashSet<String> {
        self.rules
            .values()
            .filter(|r| r.findings_emitted > 0)
            .map(|r| r.rule_id.clone())
            .collect()
    }

    /// Sum of every stage's `unaccounted` count — zero on a fully-reconciled scan.
    pub fn total_unaccounted(&self) -> usize {
        self.stages.iter().map(|s| s.unaccounted).sum()
    }
}

/// Convenience for the common "this stage only ever adds rows, all under ONE label" shape —
/// e.g. the scan-time external-tool preview merge (`crate::merge_scan_preview`) and the
/// dependency audit, both of which strictly APPEND findings and never remove any. Equivalent to
/// building a [`StageAccounting`] with a single `push_added(label, rows_out - rows_in)` and
/// calling [`ScanLedger::record_stage`] directly. Deliberately a NO-OP (records nothing) when
/// `rows_out <= rows_in` — the common case when a pass finds nothing new this run — so a scan
/// with no additions doesn't accumulate a zero-sized stage entry for every repo it touches.
pub fn record_addition_stage(
    ledger: &mut ScanLedger,
    stage: impl Into<String>,
    rows_in: usize,
    rows_out: usize,
    label: &str,
) {
    if rows_out <= rows_in {
        return;
    }
    let mut acc = StageAccounting::default();
    acc.push_added(label, rows_out - rows_in);
    ledger.record_stage(stage, rows_in, rows_out, acc);
}

/// A runtime disclosure for one stage's integrity gap, or `None` when the stage reconciled
/// cleanly. Rides the EXISTING `FailedPass` mechanism (`crate::ai_audit::FailedPass`) into
/// the report's methodology/executive-summary disclosure list — never a refused export, never
/// a dropped row, just an honest statement that something didn't add up.
pub fn stage_disclosure(
    repo_label: &str,
    stage: &StageLedgerEntry,
) -> Option<crate::ai_audit::FailedPass> {
    if stage.unaccounted == 0 {
        return None;
    }
    Some(crate::ai_audit::FailedPass {
        repo: repo_label.to_string(),
        pass: format!("pipeline integrity ({})", stage.stage),
        reason: format!(
            "{} of {} row(s) entering this stage have no recorded disposition (rows_out={}, \
             merged={}, held={}, informational={}, deduped={})",
            stage.unaccounted,
            stage.rows_in,
            stage.rows_out,
            stage.merged_into.len(),
            stage.routed_held,
            stage.routed_informational,
            stage.deduped,
        ),
        files_not_reviewed: 0,
    })
}

/// The exact substring [`rule_disclosure`] keys on to recognize the W1-item-4 "declared
/// mechanical/architectural enforcement but nothing ever evaluates it" defect shape.
///
/// W3 hardening: this used to be a bare string literal duplicated at the call site
/// (`onboard::audit_repos`'s skip-reason constructor) and the check site (`rule_disclosure`
/// below) — a future wording edit to either copy would silently decouple the two and the
/// disclosure would stop firing with no test failure pointing at why. Every producer of a
/// "no detector" skip reason MUST embed this constant verbatim (not retype the phrase), and
/// `rule_disclosure` checks against the SAME constant, so the two can never drift apart.
pub const NO_WIRED_DETECTOR_REASON: &str = "no wired detector";

/// W13 — the exact `skip_reason` recorded for a rule whose CORPUS FILE could not be parsed,
/// and the substring [`crate::integrity_gate::SkipClass::classify`] keys on to recognise that
/// shape. Same producer/checker-share-one-constant discipline as
/// [`NO_WIRED_DETECTOR_REASON`]: every producer must embed this constant verbatim rather than
/// retype the phrase, so a wording edit cannot silently decouple the recording site from the
/// gate that fails on it.
///
/// # Why the wording is this bare
///
/// This string is CLIENT-FACING. A `skip_reason` flows straight into the report JSON's
/// `rules_not_run` section and the workbook's "Rules not run this scan" sheet, so it may never
/// carry the thing an operator actually needs — the file path and the parser's own message.
/// Those live in the `tracing::error!` line and the internal [`crate::onboard::CoverageNote`]
/// that `camerata_server::record_corpus_load_failures` writes alongside this entry; neither
/// reaches a client artifact (only `osv-scanner` notes are exported, see
/// `report_export::build_report_json`'s `dep_coverage_notes`). This is the W6 precedent held
/// the right way round: the detail is KEPT, just not in the deliverable.
pub const CORPUS_LOAD_FAILED_REASON: &str = "rule file could not be loaded";

/// W13 — the ADDITIVE [`RuleLedgerEntry::coverage_note`] text set on every rule whose corpus
/// file would not parse, and the marker [`crate::integrity_gate`] keys on to fail the gate.
///
/// Why a second constant rather than only [`CORPUS_LOAD_FAILED_REASON`]: `skip_reason` only
/// ever means "why this rule did NOT run", and the content floor
/// (`crate::onboard::AUDIT_RULES`) keys on hardcoded rule IDS rather than corpus content, so a
/// rule with a broken `.toml` can still genuinely RUN. In that case there is no skip reason to
/// classify, and `coverage_note` is the field designed to survive regardless of `ran` — so the
/// gate reads THIS, giving it one uniform trigger across all three cases
/// [`ScanLedger::record_corpus_load_failure`] documents.
///
/// Unlike `CORPUS_LOAD_FAILED_REASON` this is OPERATOR-facing (a `coverage_note` reaches the
/// persisted ledger and the UI, never a client artifact — only `skip_reason` is exported), so
/// it may explain the consequence. It still carries no PATH: the ledger is diffed across
/// machines, and the path belongs in the `tracing::error!` line and the per-file
/// `crate::onboard::CoverageNote`.
pub const CORPUS_LOAD_FAILED_NOTE: &str =
    "rule file could not be loaded: this rule's corpus file failed to parse, so none of its \
     authored text was available this scan (no directive, no remediation, no severity bounds, \
     no options). Any evaluation recorded for it came from a hardcoded detector, never from \
     the rule as authored.";

/// The honest [`RuleLedgerEntry::coverage_note`] text for a CI-tier rule whose registered
/// architectural checker is CONFIG-GATED and could not produce a deterministic verdict for
/// this repo because `.camerata/architecture.toml` is absent — `ARCH-HANDLER-NO-DB-1`,
/// `ARCH-API-DTOS-1`, `ARCH-STRICT-LAYERING-1`, `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` today. This
/// is NOT the `NO_WIRED_DETECTOR_REASON` shape — a real checker exists and is registered
/// (`camerata_checks::arch_checker::any_registered_checker_answers` confirms it); it just
/// didn't run for THIS repo. Set via [`ScanLedger::note_config_gated_rule`] /
/// [`ScanLedger::record_rule`]; never a `skip_reason` (which only ever describes why a rule
/// did NOT run — this rule, as of the W4 fix, still reaches and is genuinely evaluated by the
/// semantic/AI pass).
pub const CONFIG_GATED_NOTE: &str =
    "config-gated: no architecture config present (.camerata/architecture.toml); the \
     registered architectural checker produced no deterministic verdict for this repo — \
     reviewed semantically instead of mechanically";

/// The honest [`RuleLedgerEntry::coverage_note`] text for a rule whose deterministic check was
/// WITHHELD because the scan stopped at its file limit: a check that reports a file as unused
/// cannot be trusted when an importer may be among the files that were dropped. Not a
/// configuration problem, so it must not read as [`CONFIG_GATED_NOTE`].
pub const INCOMPLETE_SCAN_NOTE: &str =
    "withheld: the scan stopped at its file limit, so the deterministic check, which reports \
     what nothing references, was not run for this repo (an importer may be among the files \
     that were not read) — reviewed semantically instead of mechanically";

/// The honest [`RuleLedgerEntry::coverage_note`] (and, when the rule did not run at all, the
/// `skip_reason`) for a clone-based rule (Wave C) whose deterministic pass found candidate
/// sites but whose reviewed judgment did not run this scan. The candidates are never promoted:
/// the rule is reported as not assessed, one `not_assessed` row per rule with the count.
pub const ADVISORY_UNAVAILABLE_NOTE: &str =
    "withheld: the deterministic pass found candidate sites but the reviewed judgment that \
     decides which are real did not run this scan, so the rule is reported as not assessed \
     rather than clean";

/// The honest [`RuleLedgerEntry::coverage_note`] for a clone-based rule whose judgment ran but
/// left some candidate sites undecided.
pub const ADVISORY_PARTIAL_NOTE: &str =
    "partly assessed: the deterministic pass found candidate sites that the reviewed judgment \
     did not decide this scan (the per-scan limit, an undecided verdict or a failed batch), so \
     the rule is reported as not fully assessed rather than clean";

/// A runtime disclosure for a rule that declared mechanical/architectural enforcement but has
/// no wired detector (the W1-item-4 defect, closed at build time by `crate::mechanical_gate`
/// for the known corpus — this is the scan-time symptom-level backstop for any rule that
/// slips through, e.g. a future corpus addition before the next build-gate run). Deliberately
/// NOT raised for every not-run rule — an ordinary, honestly-scoped skip ("AI review not
/// requested this run", "deterministic scan deselected") is not a pipeline defect, just a
/// disclosed scope choice already reflected in "excluded from this audit"; only the
/// no-detector case is a genuine integrity gap worth its own alarm.
pub fn rule_disclosure(
    repo_label: &str,
    rule: &RuleLedgerEntry,
) -> Option<crate::ai_audit::FailedPass> {
    if rule.ran {
        return None;
    }
    let reason = rule.skip_reason.as_deref().unwrap_or("");
    if !reason.contains(NO_WIRED_DETECTOR_REASON) {
        return None;
    }
    Some(crate::ai_audit::FailedPass {
        repo: repo_label.to_string(),
        pass: "mechanical-enforcement coverage".to_string(),
        reason: format!("{} {}", rule.rule_id, reason),
        files_not_reviewed: 0,
    })
}

/// One stage/merge call site's before/after sample, packaged for a caller (e.g.
/// `ai_audit::audit_repo`) that cannot hold a live `&mut ScanLedger` across an API boundary
/// without a disruptive signature change — the caller returns a `Vec<StageSample>` alongside
/// its normal result, and the orchestrator folds each sample into its own ledger via
/// [`ScanLedger::record_stage_sample`]. Mirrors the existing `FailedPass` accumulation
/// pattern (`Vec<FailedPass>` returned alongside findings) already used throughout
/// `ai_audit.rs` for the identical reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSample {
    pub stage: String,
    pub rows_in: usize,
    pub rows_out: usize,
    pub accounting: StageAccounting,
}

impl ScanLedger {
    pub fn record_stage_sample(&mut self, sample: StageSample) -> &StageLedgerEntry {
        self.record_stage(
            sample.stage,
            sample.rows_in,
            sample.rows_out,
            sample.accounting,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── req_a3: a tier family label names an ENGINE, never a rule family ─────────────────
    //
    // `RuleTier` is an engine axis, so a label that reads as a rule-family name is a false
    // claim about the rows underneath it. "Architectural checks" read as "the ARCH-* rules"
    // while in practice resolving to eight Supabase migration-replay rules the grader scores
    // as security (finding A3). The gate below is mechanical: no word in any tier label may be
    // (or begin with) a corpus rule-id family prefix.

    /// Every rule-id FAMILY PREFIX in the real shipped corpus — the token before the first
    /// `-`, uppercased (`ARCH`, `SEC`, `SUPABASE`, `RUST`, `ORCH`, `PROC`, `TESTING`, ...).
    /// Returns `None` when the bundled corpus isn't checked out so the gate skips rather than
    /// fails, matching the other corpus-backed gates.
    async fn corpus_rule_family_prefixes() -> Option<std::collections::BTreeSet<String>> {
        let path = camerata_rules::corpus_path();
        if !path.exists() {
            return None;
        }
        let (set, _errs) = camerata_rules::load_corpus_lenient(&path).await;
        Some(
            set.iter()
                .filter_map(|r| r.id.0.split('-').next().map(str::to_ascii_uppercase))
                .filter(|p| p.len() >= 2)
                .collect(),
        )
    }

    /// req_a3 (Pass-1 W4): no [`RuleTier::family_label`] may borrow a corpus rule-id family
    /// name. A tier groups by WHICH ENGINE RAN, so a family header naming a rule family
    /// promises membership the tier does not control and, in the shipped case, did not have.
    ///
    /// FAILS BEFORE THE FIX: `RuleTier::Architectural`'s label was "Architectural checks", and
    /// `ARCHITECTURAL` begins with the corpus family prefix `ARCH` — the assertion reported
    /// `Architectural ("Architectural checks"): word "Architectural" borrows rule family
    /// "ARCH"`. Verified by running this test against the pre-rename label.
    #[tokio::test]
    async fn req_a3_no_tier_family_label_borrows_a_corpus_rule_family_name() {
        let Some(prefixes) = corpus_rule_family_prefixes().await else {
            return;
        };
        let mut offenders: Vec<String> = Vec::new();
        for tier in RuleTier::all() {
            let label = tier.family_label();
            for word in label.split(|c: char| !c.is_ascii_alphabetic()) {
                if word.len() < 2 {
                    continue;
                }
                let upper = word.to_ascii_uppercase();
                if let Some(hit) = prefixes.iter().find(|p| upper.starts_with(p.as_str())) {
                    offenders.push(format!(
                        "{tier:?} ({label:?}): word {word:?} borrows rule family {hit:?}"
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "A3: a RuleTier family label groups by ENGINE and must not read as a rule family — \
             the rows under it are decided by detector_channel, not by rule id: {offenders:#?}"
        );
    }

    /// req_a3 (the fact that made the old label a lie, pinned so it cannot change silently):
    /// the rule ids a registered `ArchChecker` answers are NOT predominantly `ARCH-*`. If a
    /// future pass makes the `ARCH-*` static analysers the bulk of this tier, this test is
    /// where that shows up and the label can be revisited deliberately rather than by vibe.
    #[test]
    fn req_a3_the_architectural_tier_is_not_populated_only_by_arch_rules() {
        let ids = camerata_checks::arch_checker::all_checker_rule_ids();
        assert!(!ids.is_empty(), "sanity: checkers are registered");
        let non_arch: Vec<&&str> = ids.iter().filter(|id| !id.starts_with("ARCH-")).collect();
        assert!(
            !non_arch.is_empty(),
            "A3: if EVERY registered checker rule id became ARCH-*, the old \
             \"Architectural checks\" label would finally be true and this test should be \
             revisited together with family_label: {ids:?}"
        );
    }

    // ── Rule-level ───────────────────────────────────────────────────────────────────────

    #[test]
    fn a_rule_that_ran_with_zero_findings_is_healthy() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );
        assert_eq!(
            ledger.healthy_rule_ids(),
            vec!["SEC-NO-HARDCODED-SECRETS-1".to_string()]
        );
        assert!(ledger.excluded_rules().is_empty());
        assert!(ledger.fired_rule_ids().is_empty());
    }

    #[test]
    fn a_rule_that_did_not_run_is_excluded_with_a_reason_never_healthy() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "PYTHON-PARAMETERIZED-SQL-1",
            RuleTier::Deterministic,
            false,
            Some("declares mechanical enforcement but has no wired detector".to_string()),
            0,
            0,
        );
        assert!(ledger.healthy_rule_ids().is_empty());
        let excluded = ledger.excluded_rules();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].0, "PYTHON-PARAMETERIZED-SQL-1");
        assert!(excluded[0].1.contains("no wired detector"));
    }

    /// W4 (root-cause fix) regression: a corpus rule that declares `mechanical` enforcement
    /// but ships NO deterministic detector is, as of W4, ALWAYS routed to the semantic/AI pass
    /// (see `onboard::semantic_rule_ids_for_repo`). This mirrors the EXACT `record_rule` call
    /// sequence `onboard::audit_repos`'s loop makes for that case: NOTHING is recorded during
    /// the deterministic phase (no detector channel matched, so that loop skips it entirely —
    /// see its doc comment), and the semantic phase records it directly as `RuleTier::Semantic`
    /// with the model's real finding count once the AI pass actually runs. The rule must land
    /// as genuinely FIRED (not healthy), and it must NEVER be recorded as excluded/not-run —
    /// the old defect this whole fix closes was exactly a no-detector rule ending up "checked"
    /// in name only; the new behavior is that it is checked for real, by the model.
    #[test]
    fn a_mechanical_rule_with_no_detector_that_the_model_evaluated_is_recorded_as_fired_semantic_never_excluded(
    ) {
        let mut ledger = ScanLedger::new();
        // The semantic-phase recording `onboard::audit_repos` makes once `audit_repo` returns:
        // one real finding for this id, at the Semantic tier.
        ledger.record_rule(
            "RUBY-FROZEN-STRING-LITERAL-1",
            RuleTier::Semantic,
            true,
            None,
            3, // files_evaluated
            1, // the model's one real finding
        );
        let entry = ledger
            .rule("RUBY-FROZEN-STRING-LITERAL-1")
            .expect("just recorded");
        assert!(entry.ran, "the semantic pass genuinely ran this rule");
        assert_eq!(entry.tier, RuleTier::Semantic);
        assert!(
            !entry.verified_clean(),
            "a rule that fired must never read as verified clean"
        );
        assert!(
            ledger.excluded_rules().is_empty(),
            "a rule the model actually ran must never appear as excluded/not-run"
        );
        assert!(
            ledger
                .fired_rule_ids()
                .contains("RUBY-FROZEN-STRING-LITERAL-1"),
            "the rule must be recorded as genuinely fired"
        );
    }

    /// The companion zero-findings case: the SAME no-detector mechanical rule, model-reviewed,
    /// came back clean this run. It is honestly healthy (the model looked and found nothing —
    /// a true claim), tagged `RuleTier::Semantic` (never silently implied to be a mechanical
    /// check), and critically still NEVER appears in `excluded_rules()`.
    #[test]
    fn a_mechanical_rule_with_no_detector_that_the_model_reviewed_clean_is_healthy_via_semantic_tier(
    ) {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "RUBY-FROZEN-STRING-LITERAL-1",
            RuleTier::Semantic,
            true,
            None,
            3,
            0,
        );
        let entry = ledger
            .rule("RUBY-FROZEN-STRING-LITERAL-1")
            .expect("just recorded");
        assert_eq!(entry.tier, RuleTier::Semantic);
        assert!(entry.verified_clean());
        assert!(ledger
            .healthy_rule_ids()
            .contains(&"RUBY-FROZEN-STRING-LITERAL-1".to_string()));
        assert!(ledger.excluded_rules().is_empty());
    }

    #[test]
    fn a_rule_that_fired_is_never_healthy_and_counts_as_fired() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "ARCH-STRICT-LAYERING-1",
            RuleTier::Deterministic,
            true,
            None,
            5,
            2,
        );
        assert!(ledger.healthy_rule_ids().is_empty());
        assert!(ledger.excluded_rules().is_empty());
        assert!(ledger.fired_rule_ids().contains("ARCH-STRICT-LAYERING-1"));
    }

    /// A rule absorbed into another row's `also_matches` by a merge stage still genuinely
    /// fired — the ledger records `findings_emitted` at DETECTION time (before any merge), so
    /// it stays in `fired_rule_ids` / out of `healthy_rule_ids` regardless of what a later
    /// merge stage does to the finding's surviving shape. Mirrors the C5-1 regression this
    /// generalizes (`report_export.rs`'s `matched_rule_ids_by_category` doc comment).
    #[test]
    fn a_merged_away_rule_still_counts_as_fired_never_healthy() {
        let mut ledger = ScanLedger::new();
        // Detected at the semantic tier before merge absorbs it into the deterministic primary.
        ledger.record_rule("AI-SITE-DEFECT-1", RuleTier::Semantic, true, None, 3, 1);
        // The merge stage itself: one row in, one row out (absorbed into the primary).
        let mut acc = StageAccounting::default();
        acc.push_merge("AI-SITE-DEFECT-1", "SEC-DETERMINISTIC-SITE-1");
        ledger.record_stage("cross-family-merge", 2, 1, acc);
        assert!(
            ledger.fired_rule_ids().contains("AI-SITE-DEFECT-1"),
            "a merged-away rule must still count as fired"
        );
        assert!(!ledger
            .healthy_rule_ids()
            .contains(&"AI-SITE-DEFECT-1".to_string()));
    }

    // ── Pass-1 W2 (A5.4): "judged not applicable" is its own outcome ───────────────────────
    // (`docs/plans/2026-10-09_audit-findings-tracker.md`)

    /// req_a5_4: a rule recorded with `ran && findings_emitted == 0` AFTER
    /// `note_not_applicable_rule` was queued for it is NEVER verified-clean / healthy — this is
    /// its own outcome ("judged not applicable"), honestly distinct from "ran, genuinely
    /// looked, found nothing."
    ///
    /// FAILS BEFORE THE FIX: before `not_applicable`/`note_not_applicable_rule` existed,
    /// `verified_clean()` was exactly `ran && findings_emitted == 0` — this EXACT call sequence
    /// (queue, then record with zero findings) would have made `healthy_rule_ids()` include the
    /// rule, which is the silencing shape A5.4 closes: "no evidence this concern exists here"
    /// read as "checked and compliant."
    #[test]
    fn req_a5_4_a_rule_judged_not_applicable_is_never_healthy_even_with_zero_findings() {
        let mut ledger = ScanLedger::new();
        ledger.note_not_applicable_rule("ARCH-API-VERSIONING-1");
        ledger.record_rule(
            "ARCH-API-VERSIONING-1",
            RuleTier::Semantic,
            true,
            None,
            5,
            0,
        );
        let entry = ledger.rule("ARCH-API-VERSIONING-1").expect("recorded");
        assert!(
            entry.ran,
            "the alternatives pass DID genuinely run and judge this rule"
        );
        assert_eq!(entry.findings_emitted, 0);
        assert!(entry.not_applicable);
        assert!(
            !entry.verified_clean(),
            "not_applicable must never read as verified-clean, even at zero findings"
        );
        assert!(
            !ledger
                .healthy_rule_ids()
                .contains(&"ARCH-API-VERSIONING-1".to_string()),
            "must never appear in the healthy/what's-healthy bucket"
        );
        assert!(
            ledger
                .not_applicable_rule_ids()
                .contains(&"ARCH-API-VERSIONING-1".to_string()),
            "must appear in its own not_applicable outcome bucket"
        );
    }

    /// The positive twin: a rule recorded with `ran && findings_emitted == 0` WITHOUT ever
    /// being queued as not-applicable is the ordinary, honest "genuinely ran and found
    /// nothing" case — still verified-clean/healthy, proving the new field doesn't change
    /// behavior for every rule that was never judged not-applicable at all (the overwhelming
    /// majority).
    #[test]
    fn req_a5_4_a_rule_never_judged_not_applicable_with_zero_findings_stays_healthy() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "ARCH-API-VERSIONING-1",
            RuleTier::Semantic,
            true,
            None,
            5,
            0,
        );
        let entry = ledger.rule("ARCH-API-VERSIONING-1").expect("recorded");
        assert!(!entry.not_applicable);
        assert!(entry.verified_clean());
        assert!(ledger
            .healthy_rule_ids()
            .contains(&"ARCH-API-VERSIONING-1".to_string()));
        assert!(ledger.not_applicable_rule_ids().is_empty());
    }

    #[test]
    fn accumulates_across_repos_ran_wins_and_counters_sum() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "UI-UTC-DATES-1",
            RuleTier::Deterministic,
            false,
            Some("no matching files".to_string()),
            0,
            0,
        );
        ledger.record_rule("UI-UTC-DATES-1", RuleTier::Deterministic, true, None, 7, 1);
        let entry = ledger.rule("UI-UTC-DATES-1").unwrap();
        assert!(
            entry.ran,
            "one repo running it is enough to call it run overall"
        );
        assert_eq!(
            entry.skip_reason, None,
            "ran clears any earlier skip reason"
        );
        assert_eq!(entry.files_evaluated, 7);
        assert_eq!(entry.findings_emitted, 1);
    }

    // ── Stage-level ──────────────────────────────────────────────────────────────────────

    #[test]
    fn a_fully_explained_stage_has_zero_unaccounted() {
        let mut ledger = ScanLedger::new();
        let mut acc = StageAccounting::default();
        acc.push_merge("AI-FOO-1", "ARCH-BAR-1");
        acc.routed_held = 2;
        acc.routed_informational = 1;
        acc.deduped = 1;
        // rows_in=10, rows_out=5 -> 5 lost, and 1+2+1+1=5 accounted.
        let entry = ledger.record_stage("test-stage", 10, 5, acc);
        assert_eq!(entry.unaccounted, 0);
        assert!(entry.is_integral());
        assert_eq!(ledger.total_unaccounted(), 0);
    }

    /// The core integrity check: a stage that loses a row with NO recorded disposition for it
    /// must produce `unaccounted > 0`, never silently reconcile.
    #[test]
    fn a_stage_that_loses_a_row_with_no_disposition_is_unaccounted() {
        let mut ledger = ScanLedger::new();
        let acc = StageAccounting::default(); // nothing explained
        let entry = ledger.record_stage("lossy-stage", 10, 8, acc); // 2 rows vanished
        assert_eq!(entry.unaccounted, 2);
        assert!(!entry.is_integral());
        assert_eq!(ledger.total_unaccounted(), 2);
    }

    // ── req_c1: additions are accounted exactly as deletions already are ───────────────────
    // (docs/plans/2026-10-09_audit-findings-tracker.md, requirement C1 — the ledger used to be
    // able to represent ONLY a stage that shrinks the row set; a stage that APPENDS findings
    // (the scan-time external-tool preview merge, the evidence-of-violation gate) had no bucket
    // at all, so the two real defects this fixes ran entirely outside the ledger's view. These
    // tests pin the new, symmetric `added`/`added_total` machinery directly, independent of the
    // scan-pipeline/export-level req_c1 integration test in `report_export.rs`.)

    /// A stage that GROWS the row set with every added row disclosed via `push_added` must
    /// reconcile to zero unaccounted — the direct mirror of
    /// `a_fully_explained_stage_has_zero_unaccounted` for the addition direction.
    #[test]
    fn req_c1_a_stage_that_adds_rows_with_a_disclosed_reason_is_fully_accounted() {
        let mut ledger = ScanLedger::new();
        let mut acc = StageAccounting::default();
        acc.push_added("external_tool_preview_findings", 6);
        // 40 rows in, 46 out: 6 NEW rows, all disclosed.
        let entry = ledger.record_stage("external-tool-preview-merge", 40, 46, acc);
        assert_eq!(
            entry.unaccounted, 0,
            "every added row must be accounted: {entry:?}"
        );
        assert!(entry.is_integral());
        assert_eq!(entry.added_total(), 6);
        assert_eq!(ledger.total_unaccounted(), 0);
    }

    /// The core integrity check for the addition direction: a stage whose `rows_out` exceeds
    /// `rows_in` with NO disclosed reason must produce `unaccounted > 0` — the exact shape of
    /// the shipped defect (rows appended after the row set should have been final, with nothing
    /// in the ledger ever noticing).
    #[test]
    fn req_c1_a_stage_that_adds_rows_with_no_disclosed_reason_is_unaccounted() {
        let mut ledger = ScanLedger::new();
        let acc = StageAccounting::default(); // nothing explained
        let entry = ledger.record_stage("evidence-gate-synthesis", 40, 42, acc); // 2 rows appeared
        assert_eq!(entry.unaccounted, 2);
        assert!(!entry.is_integral());
        assert_eq!(entry.added_total(), 0);
        assert_eq!(ledger.total_unaccounted(), 2);
    }

    /// A stage can only owe ONE direction at a time: a stage recorded as a pure addition never
    /// has removal-side accounting (`accounted_total`) nonzero, and vice versa — pinning that
    /// the two totals are genuinely independent, not aliases of each other.
    #[test]
    fn req_c1_added_total_and_accounted_total_are_independent_axes() {
        let mut ledger = ScanLedger::new();
        let mut acc = StageAccounting::default();
        acc.push_added("evidence_gate_synthesized", 3);
        let entry = ledger.record_stage("evidence-gate-synthesis", 10, 13, acc);
        assert_eq!(entry.added_total(), 3);
        assert_eq!(
            entry.accounted_total(),
            0,
            "an addition-only stage must not also report removal-side accounting"
        );
        assert_eq!(entry.unaccounted, 0);
    }

    /// W2: the semantic/AI tier's own accounting identity, stated as an explicit arithmetic
    /// check (not just `unaccounted == 0`) over BOTH real ai-tier stages
    /// (`ai_audit::audit_repo` records `"ai-location-merge"` and `"ai-calibration"` by these
    /// exact names): `rows_in == rows_out + accounted` for every stage a Semantic-tier rule's
    /// findings pass through. This is the regression contract for the zero-row collapse at the
    /// ledger level — a scan with `rows_in > 0` for the semantic tier can never reconcile to
    /// `rows_out == 0` with nothing accounting for the rest.
    #[test]
    fn semantic_tier_advisory_rows_reconcile_rows_out_plus_accounted_equals_rows_in() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "AI-CONFIG-HANDLER-DEFECT-1",
            RuleTier::Semantic,
            true,
            None,
            2,
            3,
        );

        // ai-location-merge: 3 raw findings in, 1 deduped away, 2 location-merged into 1 row.
        let mut location_acc = StageAccounting {
            deduped: 1,
            ..StageAccounting::default()
        };
        location_acc.push_merge("AI-DUP-NAME-1", "AI-CONFIG-HANDLER-DEFECT-1");
        let location_entry = ledger.record_stage("ai-location-merge", 3, 1, location_acc);
        assert_eq!(
            location_entry.rows_in,
            location_entry.rows_out + location_entry.accounted_total(),
            "ai-location-merge must reconcile exactly"
        );
        assert_eq!(location_entry.unaccounted, 0);

        // ai-calibration: never drops a row on its own (see `audit_repo`'s doc comment) — the
        // ONE row from above passes straight through.
        let calibration_entry =
            ledger.record_stage("ai-calibration", 1, 1, StageAccounting::default());
        assert_eq!(
            calibration_entry.rows_in,
            calibration_entry.rows_out + calibration_entry.accounted_total(),
            "ai-calibration must reconcile exactly"
        );
        assert_eq!(calibration_entry.unaccounted, 0);

        // The scan-wide identity holds across every stage the semantic tier's rows touched.
        assert_eq!(
            ledger.total_unaccounted(),
            0,
            "a semantic-tier scan with rows_in > 0 must never leave any row unaccounted: {:#?}",
            ledger.stages()
        );
        assert!(
            ledger
                .fired_rule_ids()
                .contains("AI-CONFIG-HANDLER-DEFECT-1"),
            "the rule that produced these rows must be recorded as fired, never silently clean"
        );
    }

    #[test]
    fn stage_disclosure_is_none_when_integral_and_some_when_not() {
        let mut ledger = ScanLedger::new();
        let clean = ledger
            .record_stage("clean-stage", 5, 5, StageAccounting::default())
            .clone();
        assert!(stage_disclosure("acme/app", &clean).is_none());

        let lossy = ledger
            .record_stage("lossy-stage", 5, 3, StageAccounting::default())
            .clone();
        let fp = stage_disclosure("acme/app", &lossy).expect("must disclose a lossy stage");
        assert_eq!(fp.repo, "acme/app");
        assert!(fp.pass.contains("lossy-stage"));
        assert!(
            fp.reason.contains('2'),
            "must name the unaccounted count: {}",
            fp.reason
        );
    }

    #[test]
    fn rule_disclosure_fires_only_for_the_no_detector_reason() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "RUBY-RAILS-NO-STRING-SQL-1",
            RuleTier::Deterministic,
            false,
            Some("declares mechanical enforcement but has no wired detector".to_string()),
            0,
            0,
        );
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            false,
            Some("deterministic scan deselected for this run".to_string()),
            0,
            0,
        );
        let phantom = ledger.rule("RUBY-RAILS-NO-STRING-SQL-1").unwrap();
        let ordinary_skip = ledger.rule("SEC-NO-HARDCODED-SECRETS-1").unwrap();
        assert!(rule_disclosure("acme/app", phantom).is_some());
        assert!(
            rule_disclosure("acme/app", ordinary_skip).is_none(),
            "an ordinary, honestly-scoped skip is not a pipeline-integrity alarm"
        );
    }

    #[test]
    fn empty_ledger_has_no_unaccounted_rows() {
        let ledger = ScanLedger::new();
        assert!(ledger.is_empty());
        assert_eq!(ledger.total_unaccounted(), 0);
        assert!(ledger.healthy_rule_ids().is_empty());
        assert!(ledger.excluded_rules().is_empty());
    }

    // ── Config-gated architectural rules (the five-rule misclassification) ─────────────
    //
    // `ARCH-HANDLER-NO-DB-1`, `ARCH-API-DTOS-1`, `ARCH-STRICT-LAYERING-1`, and
    // `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` each have a REGISTERED checker
    // (`camerata_checks::arch_checker::any_registered_checker_answers` confirms it), but that
    // checker is config-gated and produces nothing without `.camerata/architecture.toml`. The
    // real `onboard::audit_repos` sequence for one of these on an unconfigured repo is:
    // `note_config_gated_rule` during the deterministic-phase loop (no detector channel this
    // repo), then a LATER `record_rule` call once the semantic phase settles `ran`/
    // `findings_emitted`. These tests drive exactly that two-call sequence.

    const CONFIG_GATED_FOUR: [&str; 4] = [
        "ARCH-HANDLER-NO-DB-1",
        "ARCH-API-DTOS-1",
        "ARCH-STRICT-LAYERING-1",
        "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1",
    ];

    #[test]
    fn a_config_gated_rule_the_model_evaluated_is_healthy_with_an_accurate_note_never_a_no_detector_alarm(
    ) {
        let mut ledger = ScanLedger::new();
        // Deterministic phase: no detector channel for this repo, but a real checker exists.
        ledger.note_config_gated_rule("ARCH-HANDLER-NO-DB-1");
        // Semantic phase settles it later: the model reviewed it and found nothing this run.
        ledger.record_rule("ARCH-HANDLER-NO-DB-1", RuleTier::Semantic, true, None, 5, 0);
        let entry = ledger.rule("ARCH-HANDLER-NO-DB-1").expect("recorded");
        assert!(entry.ran, "the semantic pass genuinely reviewed it");
        assert_eq!(entry.tier, RuleTier::Semantic);
        assert!(
            entry.verified_clean(),
            "reviewed and found nothing — honestly healthy"
        );
        assert_eq!(
            entry.coverage_note.as_deref(),
            Some(CONFIG_GATED_NOTE),
            "must carry the honest config-gated disclosure even though it ran and is healthy"
        );
        assert!(
            ledger.excluded_rules().is_empty(),
            "a rule the model actually ran must never appear as excluded/not-run"
        );
        assert!(
            rule_disclosure("acme/app", entry).is_none(),
            "a rule that genuinely reached the semantic pass is not a pipeline-integrity alarm, \
             regardless of its coverage_note"
        );
    }

    #[test]
    fn a_config_gated_rule_the_model_flagged_is_fired_with_an_accurate_note_tier_stays_semantic() {
        let mut ledger = ScanLedger::new();
        ledger.note_config_gated_rule("ARCH-STRICT-LAYERING-1");
        ledger.record_rule(
            "ARCH-STRICT-LAYERING-1",
            RuleTier::Semantic,
            true,
            None,
            5,
            2,
        );
        let entry = ledger.rule("ARCH-STRICT-LAYERING-1").expect("recorded");
        assert!(!entry.verified_clean());
        assert!(ledger.fired_rule_ids().contains("ARCH-STRICT-LAYERING-1"));
        assert_eq!(entry.coverage_note.as_deref(), Some(CONFIG_GATED_NOTE));
        assert_eq!(entry.tier, RuleTier::Semantic);
    }

    #[test]
    fn a_config_gated_rule_never_reviewed_at_all_keeps_its_ordinary_skip_reason_and_the_note_but_is_not_a_no_detector_alarm(
    ) {
        let mut ledger = ScanLedger::new();
        ledger.note_config_gated_rule("ARCH-API-DTOS-1");
        // AI review was off this run too — the ONLY possible route also didn't run.
        ledger.record_rule(
            "ARCH-API-DTOS-1",
            RuleTier::Semantic,
            false,
            Some("AI/semantic review not requested this run".to_string()),
            0,
            0,
        );
        let entry = ledger.rule("ARCH-API-DTOS-1").expect("recorded");
        assert!(!entry.ran);
        assert_eq!(
            entry.skip_reason.as_deref(),
            Some("AI/semantic review not requested this run")
        );
        assert_eq!(
            entry.coverage_note.as_deref(),
            Some(CONFIG_GATED_NOTE),
            "the config-gated disclosure must survive even when the fallback route also didn't \
             run this time"
        );
        let excluded = ledger.excluded_rules();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].0, "ARCH-API-DTOS-1");
        assert!(
            rule_disclosure("acme/app", entry).is_none(),
            "an ordinary, honestly-scoped skip is not the W1-item-4 'no wired detector' alarm, \
             even for a config-gated rule"
        );
    }

    #[test]
    fn note_config_gated_rule_called_before_any_record_rule_call_still_attaches_on_first_insert() {
        // The real call order in `onboard::audit_repos`: `note_config_gated_rule` always runs
        // BEFORE the entry exists at all (the deterministic-phase loop queues it; the
        // semantic-phase loop further down is what actually inserts the entry). This pins that
        // ordering explicitly rather than relying on the other tests' incidental sequencing.
        let mut ledger = ScanLedger::new();
        assert!(ledger.rule("ARCH-NO-CROSS-BOUNDARY-IMPORTS-1").is_none());
        ledger.note_config_gated_rule("ARCH-NO-CROSS-BOUNDARY-IMPORTS-1");
        assert!(
            ledger.rule("ARCH-NO-CROSS-BOUNDARY-IMPORTS-1").is_none(),
            "queuing a note must not fabricate an entry before the rule's real outcome is known"
        );
        ledger.record_rule(
            "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1",
            RuleTier::Semantic,
            true,
            None,
            3,
            0,
        );
        assert_eq!(
            ledger
                .rule("ARCH-NO-CROSS-BOUNDARY-IMPORTS-1")
                .unwrap()
                .coverage_note
                .as_deref(),
            Some(CONFIG_GATED_NOTE)
        );
    }

    /// Per-rule unit assertion for all four config-gated rules at once: every one of them, run
    /// through the exact queue-then-settle sequence, ends up healthy/fired as appropriate, with
    /// the honest note, and NEVER with the `NO_WIRED_DETECTOR_REASON` substring anywhere in its
    /// `skip_reason` — the literal misclassification the real scan exhibited.
    #[test]
    fn none_of_the_four_config_gated_rules_ever_classify_as_no_wired_detector() {
        for id in CONFIG_GATED_FOUR {
            let mut ledger = ScanLedger::new();
            ledger.note_config_gated_rule(id);
            ledger.record_rule(id, RuleTier::Semantic, true, None, 4, 0);
            let entry = ledger
                .rule(id)
                .unwrap_or_else(|| panic!("{id} must be recorded"));
            assert_eq!(
                entry.coverage_note.as_deref(),
                Some(CONFIG_GATED_NOTE),
                "{id} must carry the config-gated disclosure"
            );
            if let Some(reason) = &entry.skip_reason {
                assert!(
                    !reason.contains(NO_WIRED_DETECTOR_REASON),
                    "{id} must never be classified as '{NO_WIRED_DETECTOR_REASON}': {reason}"
                );
            }
            assert!(rule_disclosure("acme/app", entry).is_none());
        }
    }

    /// `ARCH-RESOURCE-LIFECYCLE-1` is the fifth rule, but a DIFFERENT shape from the other
    /// four: its checker is not config-gated at all (it is unconditionally advisory-coexisting
    /// by design), so the FIXED pipeline credits it directly at the Architectural tier — it
    /// never goes through `note_config_gated_rule` at all. This pins that it is genuinely
    /// healthy/fired via a real deterministic run, with NO coverage_note (there is nothing to
    /// disclose — the checker really did run), distinguishing it from the four config-gated
    /// siblings above.
    #[test]
    fn resource_lifecycle_is_credited_directly_as_architectural_never_needs_a_config_gated_note() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "ARCH-RESOURCE-LIFECYCLE-1",
            RuleTier::Architectural,
            true,
            None,
            6,
            1,
        );
        let entry = ledger.rule("ARCH-RESOURCE-LIFECYCLE-1").unwrap();
        assert_eq!(entry.tier, RuleTier::Architectural);
        assert!(entry.coverage_note.is_none());
        assert!(ledger
            .fired_rule_ids()
            .contains("ARCH-RESOURCE-LIFECYCLE-1"));
        assert!(ledger.excluded_rules().is_empty());
    }

    // ── req_w13: a rule whose CORPUS FILE would not parse ────────────────────────────────
    //
    // The ledger half. Every production corpus load discarded `load_corpus_lenient`'s error
    // list, so a rule file with a typo'd enum value vanished from the scan and the ledger —
    // the honest record of what ran — had no entry for it at all. Absent is strictly worse
    // than skipped: a skip is at least visible to `excluded_rules()` and the integrity gate.

    /// req_w13: an unloadable rule is recorded as NOT RUN, can never read as verified clean,
    /// and reaches the "rules not run this scan" surface every artifact renders from.
    #[test]
    fn req_w13_a_corpus_load_failure_is_recorded_as_not_run_and_never_verified_clean() {
        let mut ledger = ScanLedger::new();
        // A healthy rule beside it, so this is not passing merely because nothing is credited.
        ledger.record_rule(
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::Deterministic,
            true,
            None,
            3,
            0,
        );
        ledger.record_corpus_load_failure("SEC-NO-SILENT-CORPUS-DROP-1");

        let entry = ledger.rule("SEC-NO-SILENT-CORPUS-DROP-1").expect(
            "the ledger must carry an entry for a rule file that would not parse — \
                    ABSENT is the defect",
        );
        assert!(!entry.ran, "a rule that never loaded never ran");
        assert!(
            !entry.verified_clean(),
            "an unloadable rule must never read as verified clean"
        );
        assert_eq!(entry.tier, RuleTier::Unloaded);
        assert_eq!(entry.findings_emitted, 0);
        assert_eq!(entry.files_evaluated, 0);

        assert!(
            !ledger
                .healthy_rule_ids()
                .contains(&"SEC-NO-SILENT-CORPUS-DROP-1".to_string()),
            "it must be excluded from What's healthy: {:?}",
            ledger.healthy_rule_ids()
        );
        assert!(
            ledger
                .healthy_rule_ids()
                .contains(&"SEC-NO-RAW-SQL-CONCAT-1".to_string()),
            "anti-vacuity: the genuinely clean rule is still credited"
        );

        let excluded = ledger.excluded_rules();
        let hit = excluded
            .iter()
            .find(|(id, _)| *id == "SEC-NO-SILENT-CORPUS-DROP-1")
            .expect(
                "it must appear in `excluded_rules()`, which is what the report JSON's \
                    `rules_not_run` and the workbook's coverage sheet both render from",
            );
        assert_eq!(
            hit.1, CORPUS_LOAD_FAILED_REASON,
            "the reason must be the shared constant verbatim, so the gate's classifier cannot \
             decouple from this producer"
        );
    }

    /// req_w13: the recorded reason is CLIENT-SAFE. It flows straight into the report JSON's
    /// `rules_not_run` and the workbook's "Rules not run this scan" sheet, so it may not carry
    /// a filesystem path, a file extension, or TOML/parser vocabulary. The operator detail
    /// lives in the `tracing::error!` line and the internal `CoverageNote` instead — the W6
    /// precedent (`run_capture_stdout` discarding stderr) held the right way round: the detail
    /// is KEPT, just not in the deliverable.
    #[test]
    fn req_w13_the_recorded_reason_is_client_safe_and_carries_no_operator_detail() {
        let mut ledger = ScanLedger::new();
        ledger.record_corpus_load_failure("SEC-NO-SILENT-CORPUS-DROP-1");
        let reason = ledger
            .rule("SEC-NO-SILENT-CORPUS-DROP-1")
            .and_then(|e| e.skip_reason.clone())
            .expect("a reason must be recorded");

        for leak in [
            ".toml",
            "/",
            "\\",
            "TOML",
            "toml",
            "parse error",
            "expected",
        ] {
            assert!(
                !reason.contains(leak),
                "the client-facing reason must not contain {leak:?}: {reason}"
            );
        }
        assert!(
            reason.len() < 60,
            "G3 concision: this is a cell in a client's coverage sheet, not a diagnostic dump \
             ({} chars): {reason}",
            reason.len()
        );
        // And it is one line, never a multi-line error rendering.
        assert_eq!(reason.lines().count(), 1, "exactly one line: {reason}");
    }

    /// req_w13, CASE 2 — the one the e2e harness caught in the first cut of this fix. When the
    /// ledger ALREADY holds a not-run entry for the rule (a persisted project selection can
    /// still name a rule whose file has since broken), the corpus-load reason must WIN.
    ///
    /// `record_rule`'s accumulate semantics keep the FIRST not-run reason, so a naive
    /// implementation left `skip_reason: "AI/semantic review not requested this run"` in
    /// place — which is WORSE than the defect being fixed. That reason classifies as
    /// `ScopedOut`, i.e. JUSTIFIED, so the ledger affirmatively claimed a legitimate scope
    /// choice for a rule that did not exist, and the gate PASSED.
    #[test]
    fn req_w13_the_corpus_load_reason_overrides_an_incidental_skip_reason() {
        let mut ledger = ScanLedger::new();
        // Exactly what `audit_repos` records for a selected rule on a no-AI-review scan.
        ledger.record_rule(
            "SUPABASE-RLS-ENABLED-1",
            RuleTier::Semantic,
            false,
            Some("AI/semantic review not requested this run".to_string()),
            0,
            0,
        );
        ledger.record_corpus_load_failure("SUPABASE-RLS-ENABLED-1");

        let entry = ledger.rule("SUPABASE-RLS-ENABLED-1").unwrap();
        assert_eq!(
            entry.skip_reason.as_deref(),
            Some(CORPUS_LOAD_FAILED_REASON),
            "the ROOT cause must win over the incidental one — otherwise the ledger claims a \
             justified scope choice for a rule that does not exist"
        );
        assert_eq!(
            entry.coverage_note.as_deref(),
            Some(CORPUS_LOAD_FAILED_NOTE)
        );
        assert_eq!(
            entry.tier,
            RuleTier::Semantic,
            "the tier is PRESERVED: retiering would move the rule out of the bucket the gate's \
             dark_tier / tier_absent arithmetic already counted it in"
        );
    }

    /// req_w13, CASE 3 — a rule can RUN while its own corpus file is unparseable, because the
    /// always-on content floor (`onboard::AUDIT_RULES`) keys on hardcoded rule IDS rather than
    /// on corpus content. Its execution facts must be left alone (walking `ran` back to
    /// `false` would be a false claim; zeroing `findings_emitted` would erase real findings
    /// from the ledger's own accounting), and the disclosure must ride the ADDITIVE
    /// `coverage_note` instead — which is what the gate reads in this case.
    #[test]
    fn req_w13_a_rule_that_ran_despite_a_broken_file_keeps_its_execution_facts() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::Deterministic,
            true,
            None,
            7,
            3,
        );
        ledger.record_corpus_load_failure("SEC-NO-RAW-SQL-CONCAT-1");

        let entry = ledger.rule("SEC-NO-RAW-SQL-CONCAT-1").unwrap();
        assert!(
            entry.ran,
            "a rule that genuinely ran must not be downgraded"
        );
        assert_eq!(
            entry.findings_emitted, 3,
            "real findings must not be erased"
        );
        assert_eq!(entry.files_evaluated, 7);
        assert_eq!(entry.tier, RuleTier::Deterministic);
        assert!(
            entry.skip_reason.is_none(),
            "`skip_reason` only ever means 'why it did NOT run'; this rule ran"
        );
        assert_eq!(
            entry.coverage_note.as_deref(),
            Some(CORPUS_LOAD_FAILED_NOTE),
            "the additive disclosure is the ONLY honest channel here, and is what the gate \
             reads for this case"
        );
        // Still credited as fired, so no accounting elsewhere silently shifts.
        assert!(ledger.fired_rule_ids().contains("SEC-NO-RAW-SQL-CONCAT-1"));
    }

    /// req_w13: the `Unloaded` tier is the one member of `RuleTier` that is not an engine, so
    /// pin the two properties that keep it honest — it is LAST in `all()` (every pre-existing
    /// display order is unchanged) and its label still obeys the A3 rule that no tier label may
    /// borrow a corpus rule-id family name. The A3 gate itself
    /// (`req_a3_no_tier_family_label_borrows_a_corpus_rule_family_name`) covers the label
    /// against the real corpus; this pins the ordering the A3 gate cannot see.
    #[test]
    fn req_w13_the_unloaded_tier_is_last_and_does_not_disturb_the_existing_display_order() {
        let all = RuleTier::all();
        assert_eq!(
            all.last().copied(),
            Some(RuleTier::Unloaded),
            "Unloaded must be last so no existing coverage summary reorders"
        );
        assert_eq!(
            &all[..4],
            &[
                RuleTier::Deterministic,
                RuleTier::Architectural,
                RuleTier::ExternalTool,
                RuleTier::Semantic,
            ],
            "the four ENGINE tiers keep their order and their positions"
        );
        assert_eq!(
            RuleTier::Unloaded.family_label(),
            "Rule files that failed to load",
            "pinned: this label is rendered into `camerata inspect`'s coverage summary and the \
             integrity gate's report"
        );
    }

    #[test]
    fn an_incomplete_scan_note_is_distinct_from_the_config_gated_note_and_wins_over_it() {
        let mut ledger = ScanLedger::new();
        ledger.note_config_gated_rule("ARCH-DEAD-CODE-1");
        ledger.note_incomplete_scan_rule("ARCH-DEAD-CODE-1");
        ledger.record_rule("ARCH-DEAD-CODE-1", RuleTier::Semantic, true, None, 5, 0);
        let entry = ledger.rule("ARCH-DEAD-CODE-1").expect("recorded");
        assert_eq!(entry.coverage_note.as_deref(), Some(INCOMPLETE_SCAN_NOTE));
        assert!(INCOMPLETE_SCAN_NOTE.contains("stopped at its file limit"));
        assert!(!INCOMPLETE_SCAN_NOTE.contains("config-gated"));
        assert!(!INCOMPLETE_SCAN_NOTE.contains("architecture.toml"));

        // A rule that only had the config-gated note keeps it.
        let mut other = ScanLedger::new();
        other.note_config_gated_rule("ARCH-HANDLER-NO-DB-1");
        other.record_rule("ARCH-HANDLER-NO-DB-1", RuleTier::Semantic, true, None, 5, 0);
        assert_eq!(
            other
                .rule("ARCH-HANDLER-NO-DB-1")
                .and_then(|e| e.coverage_note.as_deref()),
            Some(CONFIG_GATED_NOTE)
        );
    }
}
