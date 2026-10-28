//! W6 / I2 — the PRE-FLIGHT INTEGRITY GATE.
//!
//! > **I1 + I2 ABSOLUTELY AGREED** — self-measurement is a V1 requirement, not a nice-to-have.
//! > Zach's added rationale: these two also **prevent overspending on tokens** by catching
//! > regressions before an external graded run.
//! >
//! > — the audit findings tracker
//!
//! # The problem this exists to solve
//!
//! Every defect in that tracker — architecture rules silenced, rows injected into the PDF
//! only, `findings.json` contradicting itself, a merge losing rows, a whole tool tier never
//! running — was found by paying for an external graded run against benchmark repos. The
//! in-repo suite was green throughout. The engine has elaborate HONESTY machinery
//! ([`crate::scan_ledger::ScanLedger`], [`crate::ai_audit::FailedPass`]) and no
//! SELF-MEASUREMENT machinery: the ledger records everything needed to notice a degraded
//! scan, and then a human reads the summary and eyeballs it.
//!
//! This module turns that reading into a VERDICT with an exit code. A graded sweep of four
//! repos costs roughly $26 and four hours of wall clock; anything the gate catches first is
//! money and time not spent.
//!
//! # The distinction the gate must honour (H1)
//!
//! > **H1 AGREED, with a sharpening** — every rule must be CAPABLE of running. A rule that
//! > does not run because it is irrelevant to the scanned stack is CORRECT. A rule that could
//! > never run for ANY stack is a DEFECT.
//!
//! So "did not run" alone can never be the trigger. The skip must be JUSTIFIED. A Python rule
//! that did not run against a Rust repo is the gate working as intended; the Semgrep taint
//! layer with 41 rules that never ran for any repo in any code path is the gate's whole
//! reason to exist. [`SkipClass`] is where that line is drawn, and it is drawn on the
//! `skip_reason` the pipeline already writes — the gate adds no new recording obligation,
//! because a gate that needs new instrumentation to work is a gate that silently passes until
//! someone remembers to instrument.
//!
//! # Fail-closed on the UNRECOGNISED case
//!
//! [`SkipClass::classify`] maps a skip reason to justified or defect. An unrecognised reason
//! is a DEFECT, never a pass. This is the single most important design decision in the module
//! and it is deliberately the inconvenient direction: a new skip reason added elsewhere in the
//! pipeline will fail this gate until someone classifies it. The alternative — unknown reasons
//! pass — reproduces the exact failure mode W6 exists to close, where a pipeline change
//! quietly widens the set of things that can go wrong without anything noticing. A false alarm
//! costs one classification; a false pass costs a $26 graded sweep.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::scan_ledger::{
    RuleTier, ScanLedger, ADVISORY_UNAVAILABLE_NOTE, CORPUS_LOAD_FAILED_NOTE,
    CORPUS_LOAD_FAILED_REASON, NO_WIRED_DETECTOR_REASON,
};

/// Why a rule did not run, reduced to the only question the gate asks: was the skip
/// JUSTIFIED (correct behaviour, disclose and move on) or is it a DEFECT (something that
/// should have run did not)?
///
/// See the module doc comment for H1, the decision this encodes, and for why
/// [`Self::Unrecognised`] is a defect rather than a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipClass {
    /// The rule's concern is irrelevant to what was scanned: no file of its language, the
    /// rule was not armed for this repo, the stack has no such construct. H1's CORRECT case.
    NotApplicableToStack,
    /// A disclosed SCOPE choice by the operator: AI review was not requested this run, the
    /// deterministic scan deselected it, this tier was turned off. Honest and intended — but
    /// note [`IntegrityGateOptions::require_semantic_tier`], because "AI review was off" is
    /// justified for a cheap smoke scan and a BLOCKER before a graded run that grades the
    /// semantic tier.
    ScopedOut,
    /// The rule declares mechanical/architectural enforcement and nothing anywhere evaluates
    /// it — [`NO_WIRED_DETECTOR_REASON`]. H1's DEFECT case: it could never run for ANY stack.
    NoWiredDetector,
    /// The rule's backing tool was invoked and FAILED (wholesale, or for this rule). The C11
    /// shape: eslint aborted on every repo and 8 rules fell to NOT RUN. A defect — the tool
    /// was supposed to run and did not.
    ToolFailed,
    /// W13 — the rule's own CORPUS FILE could not be parsed
    /// ([`CORPUS_LOAD_FAILED_REASON`]), so the rule never entered the scan at all.
    ///
    /// Its OWN class, not folded into [`Self::ToolFailed`] or [`Self::NoWiredDetector`],
    /// because it is a different failure with a different fix and a different blast radius.
    /// `ToolFailed` means an engine was invoked and broke. `NoWiredDetector` means the rule
    /// loaded fine and nothing evaluates it. This one means the rule TEXT is unreadable: no
    /// engine was ever selected, the rule is absent from every stack's universe, and the fix is
    /// in the `.toml` rather than anywhere in the pipeline. Reusing a class that means
    /// something else would send an operator to the wrong place.
    ///
    /// NEVER justified, under any profile. A corpus file that will not parse is neither a scope
    /// choice nor stack-irrelevance; there is no repo for which an unparseable rule file is
    /// correct behaviour.
    CorpusLoadFailed,
    /// A skip reason this module does not recognise. A DEFECT by construction — see the
    /// module doc comment's fail-closed section.
    Unrecognised,
}

impl SkipClass {
    /// `true` when this class is correct behaviour rather than something to fix.
    pub fn is_justified(&self) -> bool {
        matches!(self, SkipClass::NotApplicableToStack | SkipClass::ScopedOut)
    }

    /// Classify a `skip_reason` string.
    ///
    /// Matching is substring-based and case-insensitive against the reasons the pipeline
    /// actually writes. That is a lexical test, which this repo has learned to distrust (C3's
    /// six-phrase substring scan is on the tracker precisely for being one) — so two
    /// properties keep it honest:
    ///
    /// 1. It fails CLOSED. A reason matching nothing is [`SkipClass::Unrecognised`], a defect.
    ///    A lexical miss therefore raises an alarm; it cannot silence one. This is the
    ///    opposite polarity from C3, where a lexical miss silently dropped a finding.
    /// 2. The defect phrases are checked FIRST, so a reason that contains both a
    ///    justified-sounding phrase and a failure phrase (a real shape — the tool-failure
    ///    reason embeds the whole `CoverageNote` message, which can mention languages and
    ///    files) classifies as the defect.
    ///
    /// [`NO_WIRED_DETECTOR_REASON`] is matched against the shared constant rather than a
    /// retyped phrase, so a wording edit at the producer cannot decouple the two.
    pub fn classify(skip_reason: Option<&str>) -> SkipClass {
        // No reason at all is not a justification. `excluded_rules()` substitutes the
        // placeholder "did not run this scan", which is a restatement, not a reason.
        let Some(reason) = skip_reason else {
            return SkipClass::Unrecognised;
        };
        let reason = reason.trim();
        if reason.is_empty() {
            return SkipClass::Unrecognised;
        }
        let lower = reason.to_ascii_lowercase();

        // ── Defect phrases first. See property 2 above. ──────────────────────────────────
        // W13 is checked before every other phrase: a rule whose FILE would not parse has the
        // most specific, least recoverable cause, and its reason text must never be shadowed by
        // a broader phrase added later. Matched against the shared constant, never a retyped
        // phrase, for the same reason `NO_WIRED_DETECTOR_REASON` is.
        if lower.contains(&CORPUS_LOAD_FAILED_REASON.to_ascii_lowercase()) {
            return SkipClass::CorpusLoadFailed;
        }
        if lower.contains(&NO_WIRED_DETECTOR_REASON.to_ascii_lowercase()) {
            return SkipClass::NoWiredDetector;
        }
        // `reconcile_external_tool_ledger` writes "<pass label> did not run: <reason>" for
        // both a hard tool failure and a never-invoked tool. The embedded `CoverageNote`
        // phrasing ("could not preview", "produced no output") and the never-invoked phrasing
        // ("was never invoked for any scanned repo") both land here.
        const TOOL_FAILURE_PHRASES: [&str; 5] = [
            "could not preview",
            "produced no output",
            "was never invoked",
            "did not run:",
            "provisioning",
        ];
        if TOOL_FAILURE_PHRASES.iter().any(|p| lower.contains(p)) {
            return SkipClass::ToolFailed;
        }

        // ── Justified phrases. ───────────────────────────────────────────────────────────
        const NOT_APPLICABLE_PHRASES: [&str; 6] = [
            "no matching files",
            "not armed",
            "no supported language",
            "not applicable",
            "language not present",
            "no files of this",
        ];
        if NOT_APPLICABLE_PHRASES.iter().any(|p| lower.contains(p)) {
            return SkipClass::NotApplicableToStack;
        }
        const SCOPED_OUT_PHRASES: [&str; 5] = [
            "not requested this run",
            "deselected",
            "gated off",
            "not selected",
            "excluded by configuration",
        ];
        if SCOPED_OUT_PHRASES.iter().any(|p| lower.contains(p)) {
            return SkipClass::ScopedOut;
        }
        // Wave C: a clone-based rule's reviewed judgment did not run because the AI review was
        // not requested or was blocked (the seam only records this reason when no model was
        // available for the scan), which is the same disclosed scope choice as the phrases
        // above. A judgment that was attempted and failed uses its own, unrecognised wording.
        if lower.contains(&ADVISORY_UNAVAILABLE_NOTE.to_ascii_lowercase()) {
            return SkipClass::ScopedOut;
        }

        SkipClass::Unrecognised
    }
}

/// One thing the gate found wrong, with enough detail to act on without re-running the scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityFailure {
    /// Stable machine-readable code (`unjustified_skip`, `dark_tier`, `unaccounted_rows`,
    /// `empty_ledger`, `tier_absent`). Stable so a wrapper script can branch on it.
    pub code: String,
    /// What is wrong, in one line, naming the specific rule / tier / stage.
    pub detail: String,
}

/// What the gate requires BEYOND structural integrity. These are the questions whose right
/// answer depends on what the upcoming run is for, so they are the operator's call rather
/// than this module's — a cheap smoke scan legitimately has the semantic tier off, and a
/// graded run that grades the semantic tier legitimately must not.
#[derive(Debug, Clone)]
pub struct IntegrityGateOptions {
    /// Tiers that MUST have at least one rule that ran. A tier listed here with zero rules
    /// recorded, or with rules recorded but none that ran, is a `dark_tier` failure.
    ///
    /// This is the assertion that would have caught the headline defect: a whole Semgrep taint
    /// layer with 41 rules never ran, for any repo, in any code path, and nothing said a word.
    pub required_tiers: Vec<RuleTier>,
    /// When `true`, [`SkipClass::ScopedOut`] stops counting as justified. Set this before a
    /// graded run: "AI/semantic review not requested this run" is an honest disclosure and a
    /// completely unacceptable state in which to spend $26 grading the semantic tier.
    pub require_semantic_tier: bool,
    /// When `true`, a scan that did not record which model its audit pass used is a
    /// `model_unrecorded` failure.
    ///
    /// Separate from the tier switches because it is not a coverage question: the tier can
    /// have run perfectly and still leave the run unusable as EVIDENCE. A graded sweep exists
    /// to compare one engine against another, and an unrecorded model makes that comparison
    /// unfalsifiable in both directions — which is precisely what happened on 2026-10-16 and
    /// cost a week of reasoning about a regression that did not exist.
    ///
    /// Relaxed for a smoke scan, where nobody is comparing anything.
    pub require_recorded_model: bool,
}

impl Default for IntegrityGateOptions {
    /// The PRE-GRADED-RUN default, deliberately the strict one: a scoped-out tier is a
    /// blocker, and three of the four engines must have actually run.
    ///
    /// The default is strict because of who calls this and when. The gate's whole purpose is
    /// to be the last thing that runs before money is spent, so a default that lets a degraded
    /// scan through is a default that does nothing. An operator running a cheap smoke scan can
    /// relax it explicitly; nobody relaxes it by accident.
    ///
    /// # Why [`RuleTier::Architectural`] is NOT required by default
    ///
    /// Requiring it would violate the very H1 distinction this gate exists to honour, one
    /// level up. That tier's membership is whatever `mechanical_gate::detector_channel`
    /// resolves to `arch_checker` / `gateway_rule_registry`, which in practice is dominated by
    /// stack-specific checkers (the Supabase migration-replay family). A Rust-only repo
    /// legitimately records zero of them, exactly as a Rust-only repo legitimately runs no
    /// Python rule. Requiring it would make the gate cry wolf on correct scans, and a gate
    /// that cries wolf gets disabled — which costs more than the defect it was guarding.
    ///
    /// The other three are required because each must run for ANY repo:
    ///
    /// - `Deterministic` — the content floor runs unconditionally over every file.
    /// - `ExternalTool` — this is the 41-rule dark-taint-layer defect's own tier. Making it
    ///   required by default is the whole point: that layer never ran, for any repo, in any
    ///   code path, and nothing said a word.
    /// - `Semantic` — a graded run grades the semantic tier.
    fn default() -> Self {
        Self {
            required_tiers: vec![
                RuleTier::Deterministic,
                RuleTier::ExternalTool,
                RuleTier::Semantic,
            ],
            require_semantic_tier: true,
            require_recorded_model: true,
        }
    }
}

impl IntegrityGateOptions {
    /// The RELAXED profile for a cheap local smoke scan: structural integrity only. Every
    /// stage must still reconcile and no rule may be skipped for an unrecognised or failure
    /// reason, but no tier is required to have run and a disclosed scope choice ("AI review
    /// not requested this run") stays justified.
    ///
    /// Never use this before a graded run — it cannot catch a dark tier, which is the single
    /// most expensive thing the gate is for.
    pub fn smoke() -> Self {
        Self {
            required_tiers: Vec::new(),
            require_semantic_tier: false,
            // A smoke scan compares nothing against anything, so an unrecorded model costs
            // it nothing. `engine_self_ingestion` and `empty_input` are NOT relaxed here:
            // auditing our own cache file corrupts a cheap local scan exactly as much as a
            // graded one, and it is free to detect.
            require_recorded_model: false,
        }
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityVerdict {
    /// Everything wrong. Empty ⇔ [`Self::passed`].
    pub failures: Vec<IntegrityFailure>,
    /// Justified skips, by class, with the rule ids — reported, never a failure. Present so
    /// the gate's output is auditable: "47 rules skipped" with no breakdown is the eyeballing
    /// this module replaces.
    pub justified_skips: BTreeMap<SkipClass, Vec<String>>,
    /// Per-tier `(rules_recorded, rules_that_ran, findings_emitted)`, in
    /// [`RuleTier::all`] order. The coverage picture, so a PASS is still informative.
    pub tier_summary: Vec<TierSummary>,
    /// W19 — what the scan was FED, echoed into the verdict so a PASS states its own basis.
    ///
    /// A gate that prints "everything ran" without printing what it ran OVER is how the
    /// 2026-10-16 self-ingestion survived five repos: the coverage block looked healthy and
    /// nothing on screen said the digest had tripled.
    pub input: crate::scan_ledger::ScanInputFacts,
    /// W21 — whether the sealed rows' paths are really paths, echoed for the same reason as
    /// [`Self::input`]: a PASS should state its own basis.
    pub path_hygiene: crate::scan_ledger::PathHygiene,
}

/// One tier's coverage facts. See [`IntegrityVerdict::tier_summary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierSummary {
    pub tier: RuleTier,
    pub family_label: String,
    pub rules_recorded: usize,
    pub rules_ran: usize,
    pub findings_emitted: usize,
}

impl IntegrityVerdict {
    /// `true` when nothing is wrong. The gate's exit code is `0` on `true`, `1` on `false`.
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// The process exit code this verdict implies: `0` pass, `1` fail. Named rather than
    /// inlined at the call site so the exit semantics are testable without spawning a process.
    pub fn exit_code(&self) -> i32 {
        if self.passed() {
            0
        } else {
            1
        }
    }
}

/// Run the gate over a completed scan's ledger.
///
/// Pure: no I/O, no clock, no model call. Given the same ledger it returns the same verdict,
/// so the gate itself can never be the flaky thing that blocks a run.
///
/// Five failure families, each tied to a specific shipped defect:
///
/// 1. `empty_ledger` — a ledger with nothing in it. The scan did not happen, or did not
///    record that it happened; either way there is nothing to be confident about.
/// 2. `tier_absent` / `dark_tier` — a required tier with no rules recorded, or with rules but
///    none that ran. The Semgrep-taint-layer defect.
/// 3. `unjustified_skip` — a rule that did not run for a reason that is not a justification
///    (H1). The C11 and no-wired-detector defects.
/// 4. `unaccounted_rows` — a pipeline stage whose rows do not reconcile. The merge-data-loss
///    defects: an over-merge is observationally identical to a detection miss, so the only
///    place it can be caught is the stage arithmetic.
/// 5. `corpus_load_failure` (W13) — a rule whose own `.toml` would not parse. Every production
///    corpus load was lenient AND threw the parse errors away (`let (corpus, _errs) = ...`), so
///    a rule file with a typo'd enum value silently left the corpus and nothing anywhere said
///    so. This gate could not see it even in principle, because a rule that never loaded is not
///    a skip — it is absent from the ledger's universe. The fix makes it present; this family is
///    what fails on it. See [`SkipClass::CorpusLoadFailed`].
pub fn evaluate(ledger: &ScanLedger, opts: &IntegrityGateOptions) -> IntegrityVerdict {
    let mut failures: Vec<IntegrityFailure> = Vec::new();
    let mut justified_skips: BTreeMap<SkipClass, Vec<String>> = BTreeMap::new();

    // ── 1. An empty ledger is never a pass. ──────────────────────────────────────────────
    if ledger.is_empty() {
        failures.push(IntegrityFailure {
            code: "empty_ledger".to_string(),
            detail: "the ledger recorded no rules and no stages — either no scan ran, or the \
                     scan did not record that it ran. A gate cannot confirm a scan it has no \
                     evidence of."
                .to_string(),
        });
    }

    // ── 1b. INPUT integrity (W19). ───────────────────────────────────────────────────────
    //
    // Every other family in this function asks about the MACHINERY: did the tier run, was the
    // skip justified, do the stages reconcile. All of them passed on 2026-10-16 for a scan
    // whose audit prompt was 63% Camerata's own prior output, and the defect reached a client
    // report. The question nobody was asking is the cheapest one: WAS THE RIGHT THING FED IN.
    //
    // Deliberately placed before the tier checks. An input defect makes every downstream
    // observation untrustworthy, so reporting "the semantic tier ran" above "the semantic
    // tier was shown the engine's own cache file" would be ordering the symptom first.
    let input = ledger.input();
    if input.engine_artifacts_admitted > 0 {
        failures.push(IntegrityFailure {
            code: "engine_self_ingestion".to_string(),
            detail: format!(
                "{} file(s) matching Camerata's own reserved `.camerata-` prefix were admitted \
                 into the scan — the engine audited its own output. On 2026-10-16 this put \
                 174,236 characters of a prior run's findings into a hearth audit prompt, \
                 displaced real source to ~35% of what the model read, and shipped a CRITICAL \
                 located inside `.camerata-scan-cache.json` at queue position #1. No finding \
                 from this scan can be trusted.",
                input.engine_artifacts_admitted
            ),
        });
    }
    if opts.require_recorded_model && input.audit_model.is_none() {
        failures.push(IntegrityFailure {
            code: "model_unrecorded".to_string(),
            detail: "the audit model was not recorded. `None` means UNRECORDED, not \
                     'no model ran' — resolution falls through to `CAMERATA_AUDIT_MODEL` and \
                     then the client default, so the run used something and did not say what. \
                     That is disqualifying before a graded run: the 2026-10-16 sweep recorded \
                     `claude-sonnet-5` on its baseline and nothing on either later run, which \
                     left 'did the engine regress' neither confirmable nor refutable."
                .to_string(),
        });
    }
    // Guarded on `!is_empty()` so an empty ledger reports `empty_ledger` once rather than
    // also reporting the three input facts it trivially lacks.
    if !ledger.is_empty() && input.files_scanned == 0 {
        failures.push(IntegrityFailure {
            code: "empty_input".to_string(),
            detail: "the scan recorded rules or stages but zero files scanned — the pipeline \
                     ran over nothing. Every rule is then vacuously 'clean', which is the one \
                     failure mode a coverage summary cannot distinguish from real health."
                .to_string(),
        });
    }

    // ── 1c. PATH hygiene of the sealed rows (W21). ───────────────────────────────────────
    //
    // Both known sources are fixed (the external-tool tier normalises to repo-relative; an
    // unparseable evidence citation now yields an empty path instead of its own prose). This
    // is the backstop: a future tier can reintroduce either shape, and neither source fix can
    // observe that happening. Default (all zero) when nothing recorded it, which reads as
    // clean — correct, because a report with no rows has no bad paths.
    let hygiene = ledger.path_hygiene();
    if !hygiene.is_clean() {
        let mut parts: Vec<String> = Vec::new();
        if hygiene.absolute > 0 {
            parts.push(format!("{} absolute", hygiene.absolute));
        }
        if hygiene.backslash > 0 {
            parts.push(format!("{} with backslash separators", hygiene.backslash));
        }
        if hygiene.prose > 0 {
            parts.push(format!(
                "{} carrying prose rather than a path",
                hygiene.prose
            ));
        }
        failures.push(IntegrityFailure {
            code: "path_hygiene".to_string(),
            detail: format!(
                "{} of {} sealed rows do not carry a repository-relative file path ({}){}. A \
                 client cannot open an absolute path rooted on the auditor's machine, and an \
                 absolute one additionally discloses the auditor's username and directory \
                 layout. The 2026-10-18 hearth export shipped 42 such rows.",
                hygiene.offending_rows,
                hygiene.rows_total,
                parts.join(", "),
                hygiene
                    .worst_example
                    .as_deref()
                    .map(|w| format!("; worst: `{w}`"))
                    .unwrap_or_default(),
            ),
        });
    }

    // ── 2. Tier coverage. ────────────────────────────────────────────────────────────────
    let mut tier_summary = Vec::new();
    // `HashSet` rather than `BTreeSet`: `RuleTier` is `Hash` but deliberately not
    // `Ord` (there is no meaningful order over engines), and this gate must not reach into
    // `scan_ledger` to add a derive it does not otherwise need.
    let required: HashSet<RuleTier> = opts.required_tiers.iter().copied().collect();
    for tier in RuleTier::all() {
        let rules: Vec<_> = ledger.rules().filter(|r| r.tier == tier).collect();
        let rules_ran = rules.iter().filter(|r| r.ran).count();
        let findings_emitted: usize = rules.iter().map(|r| r.findings_emitted).sum();
        tier_summary.push(TierSummary {
            tier,
            family_label: tier.family_label().to_string(),
            rules_recorded: rules.len(),
            rules_ran,
            findings_emitted,
        });

        if !required.contains(&tier) {
            continue;
        }
        if rules.is_empty() {
            failures.push(IntegrityFailure {
                code: "tier_absent".to_string(),
                detail: format!(
                    "no rule at all was recorded for the `{}` tier ({}) — this engine never \
                     ran, and a tier that records nothing is indistinguishable from a tier \
                     that ran and found nothing",
                    tier_key(tier),
                    tier.family_label(),
                ),
            });
        } else if rules_ran == 0 {
            failures.push(IntegrityFailure {
                code: "dark_tier".to_string(),
                detail: format!(
                    "all {} rule(s) recorded for the `{}` tier ({}) were skipped — NOT ONE \
                     ran this scan",
                    rules.len(),
                    tier_key(tier),
                    tier.family_label(),
                ),
            });
        }
    }

    // ── 3. Per-rule skip justification (H1). ─────────────────────────────────────────────
    for rule in ledger.rules() {
        // Incomplete coverage: the rule ran, but the scan was stopped (the spend limit) before it
        // ran everywhere. Its findings are real and not a contradiction; what it cannot support
        // is any claim that the rest of the code is clean, so the gate fails on it.
        if let Some(why) = rule.partial_reason.as_deref().filter(|_| rule.ran) {
            failures.push(IntegrityFailure {
                code: "partial_coverage".to_string(),
                detail: format!(
                    "{} ran on only part of the code, so the scan is incomplete for it: {why}",
                    rule.rule_id,
                ),
            });
        }
        if rule.ran {
            // W13, case 3: a rule can RUN while its own corpus file is unparseable — the
            // always-on content floor keys on hardcoded rule IDS, not on corpus content. There
            // is no skip reason to classify here, so the gate reads the ADDITIVE
            // `coverage_note` marker instead (see `CORPUS_LOAD_FAILED_NOTE`). It still fails:
            // a rule evaluated without its authored directive, remediation, severity bounds or
            // options produced a degraded report, and the fix is in the `.toml` either way.
            if rule
                .coverage_note
                .as_deref()
                .is_some_and(|n| n.contains(CORPUS_LOAD_FAILED_NOTE))
            {
                failures.push(IntegrityFailure {
                    code: "corpus_load_failure".to_string(),
                    detail: format!(
                        "the rule file for {} could not be parsed. A hardcoded detector still \
                         evaluated it, so it reads as RUN, but none of its authored text (no \
                         directive, no remediation, no severity bounds, no options) was \
                         available this scan. The file path and the parser's own message are \
                         in the operator log",
                        rule.rule_id,
                    ),
                });
            }
            continue;
        }
        let class = SkipClass::classify(rule.skip_reason.as_deref());
        let treat_as_justified =
            class.is_justified() && !(opts.require_semantic_tier && class == SkipClass::ScopedOut);

        if treat_as_justified {
            justified_skips
                .entry(class)
                .or_default()
                .push(rule.rule_id.clone());
            continue;
        }
        // W13: its OWN failure family, never `unjustified_skip`. See
        // `SkipClass::CorpusLoadFailed`'s doc comment — "this rule's file would not parse" and
        // "this rule was skipped for an unjustified reason" send an operator to two different
        // places, and a gate whose code does not distinguish them wastes the trip.
        if class == SkipClass::CorpusLoadFailed {
            failures.push(IntegrityFailure {
                code: "corpus_load_failure".to_string(),
                detail: format!(
                    "the rule file for {} could not be parsed, so the rule never entered this \
                     scan at all — it is not a skip, it is ABSENT from every stack's universe. \
                     The file path and the parser's own message are in the operator log; a \
                     client artifact deliberately carries neither",
                    rule.rule_id,
                ),
            });
            continue;
        }
        failures.push(IntegrityFailure {
            code: "unjustified_skip".to_string(),
            detail: format!(
                "{} did not run, and its reason is not a justification [{}]: {}",
                rule.rule_id,
                skip_class_key(class),
                rule.skip_reason
                    .as_deref()
                    .unwrap_or("(no reason recorded)"),
            ),
        });
    }
    for ids in justified_skips.values_mut() {
        ids.sort();
    }

    // ── 4. Stage row reconciliation. ─────────────────────────────────────────────────────
    for stage in ledger.stages() {
        if stage.unaccounted == 0 {
            continue;
        }
        failures.push(IntegrityFailure {
            code: "unaccounted_rows".to_string(),
            detail: format!(
                "stage `{}` does not reconcile: {} row(s) have no recorded disposition \
                 (rows_in={} rows_out={} accounted={} added={})",
                stage.stage,
                stage.unaccounted,
                stage.rows_in,
                stage.rows_out,
                stage.accounted_total(),
                stage.added_total(),
            ),
        });
    }

    // Stable output: a gate that reorders its own findings between runs is a gate whose
    // output nobody can diff.
    failures.sort_by(|a, b| (&a.code, &a.detail).cmp(&(&b.code, &b.detail)));

    IntegrityVerdict {
        failures,
        justified_skips,
        tier_summary,
        input: ledger.input().clone(),
        path_hygiene: ledger.path_hygiene().clone(),
    }
}

/// Short stable key for a tier, for machine-readable gate output.
fn tier_key(tier: RuleTier) -> &'static str {
    match tier {
        RuleTier::Deterministic => "deterministic",
        RuleTier::Architectural => "architectural",
        RuleTier::ExternalTool => "external_tool",
        RuleTier::Semantic => "semantic",
        RuleTier::Unloaded => "unloaded",
    }
}

/// Short stable key for a skip class, for machine-readable gate output.
fn skip_class_key(class: SkipClass) -> &'static str {
    match class {
        SkipClass::NotApplicableToStack => "not_applicable_to_stack",
        SkipClass::ScopedOut => "scoped_out",
        SkipClass::NoWiredDetector => "no_wired_detector",
        SkipClass::ToolFailed => "tool_failed",
        SkipClass::CorpusLoadFailed => "corpus_load_failed",
        SkipClass::Unrecognised => "unrecognised",
    }
}

/// Render a verdict as the operator-facing report the gate prints. Deterministic and
/// diff-friendly.
pub fn render(verdict: &IntegrityVerdict) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    out.push_str("PRE-FLIGHT INTEGRITY GATE\n");
    out.push_str("=========================\n\n");

    let i = &verdict.input;
    out.push_str("Input (what the scan was fed):\n");
    let _ = writeln!(
        out,
        "  {:<52} {} scanned, {} excluded as noise",
        "files", i.files_scanned, i.files_excluded
    );
    let _ = writeln!(out, "  {:<52} {}", "code characters", i.code_chars);
    let _ = writeln!(
        out,
        "  {:<52} {}",
        "audit model",
        i.audit_model.as_deref().unwrap_or("NOT RECORDED")
    );
    let _ = writeln!(
        out,
        "  {:<52} {}{}",
        "Camerata's own artifacts admitted",
        i.engine_artifacts_admitted,
        if i.engine_artifacts_admitted == 0 {
            ""
        } else {
            "   <-- the engine audited its own output"
        }
    );
    let h = &verdict.path_hygiene;
    let _ = writeln!(
        out,
        "  {:<52} {} of {} rows{}",
        "rows whose path is not a repo-relative path",
        h.offending_rows,
        h.rows_total,
        if h.is_clean() {
            ""
        } else {
            "   <-- absolute, prose, or backslash"
        }
    );
    out.push('\n');

    out.push_str("Coverage by engine:\n");
    for t in &verdict.tier_summary {
        let _ = writeln!(
            out,
            "  {:<52} recorded={:<5} ran={:<5} findings={}",
            t.family_label, t.rules_recorded, t.rules_ran, t.findings_emitted,
        );
    }
    out.push('\n');

    if verdict.justified_skips.is_empty() {
        out.push_str("Justified skips: none\n\n");
    } else {
        out.push_str("Justified skips (correct behaviour, reported for audit):\n");
        for (class, ids) in &verdict.justified_skips {
            let _ = writeln!(out, "  [{}] {} rule(s)", skip_class_key(*class), ids.len());
            for id in ids {
                let _ = writeln!(out, "      {id}");
            }
        }
        out.push('\n');
    }

    if verdict.passed() {
        out.push_str(
            "VERDICT: PASS — every required engine ran, every skip is justified, and every \
             stage reconciles.\n",
        );
    } else {
        let _ = writeln!(
            out,
            "VERDICT: FAIL — {} integrity problem(s). Do NOT spend a graded run on this scan.\n",
            verdict.failures.len(),
        );
        for f in &verdict.failures {
            let _ = writeln!(out, "  [{}] {}", f.code, f.detail);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_ledger::ScanInputFacts;
    use crate::scan_ledger::StageAccounting;

    // ── req_i2: the pre-flight integrity gate ────────────────────────────────────────────
    //
    // the audit findings tracker, requirement I2. Each test below
    // names the shipped defect it would have caught. Every one of those defects was found by
    // a PAID external graded run while the in-repo suite was green; these are the assertions
    // that make the next one free.

    /// A ledger describing a healthy scan: every required engine ran, nothing skipped,
    /// every stage reconciled.
    fn healthy_ledger() -> ScanLedger {
        let mut l = ScanLedger::new();
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 10, 1);
        l.record_rule("TAINT-SQLI-1", RuleTier::ExternalTool, true, None, 10, 2);
        l.record_rule("AI-ADVISORY-1", RuleTier::Semantic, true, None, 10, 0);
        let mut acc = StageAccounting::default();
        acc.push_merge("AI-ADVISORY-DUP-1", "SEC-FLOOR-1");
        l.record_stage("cross-family-merge", 4, 3, acc);
        l.record_input(healthy_input());
        l
    }

    /// W19 — what a CLEAN scan's input looks like. Zero engine artifacts, a real file count,
    /// and a recorded model.
    ///
    /// Every fixture in this module gained this when the input-integrity families landed, and
    /// that is the point rather than busywork: before it, these ledgers asserted a "healthy
    /// scan" while recording nothing about what the scan had been fed, which is exactly the
    /// blind spot that let a run whose prompt was 63% Camerata's own output pass this gate on
    /// five repos.
    fn healthy_input() -> ScanInputFacts {
        ScanInputFacts {
            files_scanned: 87,
            files_excluded: 12,
            code_chars: 98_522,
            engine_artifacts_admitted: 0,
            audit_model: Some("claude-sonnet-5".to_string()),
            skipped_files: Vec::new(),
            skipped_beyond_cap_total: 0,
        }
    }

    #[test]
    fn req_i2_a_healthy_scan_passes_and_exits_zero() {
        let verdict = evaluate(&healthy_ledger(), &IntegrityGateOptions::default());
        assert!(
            verdict.passed(),
            "a healthy scan must pass: {:#?}",
            verdict.failures
        );
        assert_eq!(verdict.exit_code(), 0);
        assert!(render(&verdict).contains("VERDICT: PASS"));
    }

    /// req_i2, THE HEADLINE DEFECT: a whole tool tier that never ran must FAIL the gate.
    ///
    /// > A whole Semgrep taint layer with 41 rules never ran, for any repo, in any code
    /// > path, and nothing said a word.
    ///
    /// Two shapes, both of which actually occurred and neither of which anything noticed:
    /// the tier recorded rules but ran none, and the tier recorded nothing at all.
    #[test]
    fn req_i2_a_tier_that_recorded_rules_but_ran_none_fails_as_a_dark_tier() {
        let mut l = ScanLedger::new();
        l.record_input(healthy_input());
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 10, 0);
        l.record_rule("AI-ADVISORY-1", RuleTier::Semantic, true, None, 10, 0);
        // 41 taint rules selected; not one of them ran.
        for i in 0..41 {
            l.record_rule(
                format!("TAINT-{i}"),
                RuleTier::ExternalTool,
                false,
                Some("the commodity taint pass did not run: semgrep provisioning failed".into()),
                0,
                0,
            );
        }
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        assert_eq!(verdict.exit_code(), 1);
        assert!(
            verdict.failures.iter().any(|f| f.code == "dark_tier"),
            "a tier where NOT ONE rule ran must be reported as dark: {:#?}",
            verdict.failures
        );
        assert!(render(&verdict).contains("Do NOT spend a graded run"));
    }

    #[test]
    fn req_i2_a_required_tier_that_recorded_nothing_at_all_fails_as_absent() {
        let mut l = ScanLedger::new();
        l.record_input(healthy_input());
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 10, 0);
        l.record_rule("AI-ADVISORY-1", RuleTier::Semantic, true, None, 10, 0);
        // Nothing whatsoever recorded for ExternalTool.
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        let absent: Vec<&IntegrityFailure> = verdict
            .failures
            .iter()
            .filter(|f| f.code == "tier_absent")
            .collect();
        assert_eq!(absent.len(), 1, "{:#?}", verdict.failures);
        assert!(
            absent[0].detail.contains("external_tool"),
            "must name the dark tier: {}",
            absent[0].detail
        );
    }

    /// req_i2 + H1: the distinction the gate MUST honour. A rule that did not run because it
    /// is irrelevant to the scanned stack is CORRECT and must not fail the gate — otherwise
    /// the gate cries wolf on every correct scan and gets switched off.
    #[test]
    fn req_i2_h1_a_rule_irrelevant_to_the_scanned_stack_is_a_justified_skip_not_a_failure() {
        let mut l = healthy_ledger();
        l.record_rule(
            "PYTHON-PARAMETERIZED-SQL-1",
            RuleTier::Deterministic,
            false,
            Some("no matching files".to_string()),
            0,
            0,
        );
        l.record_rule(
            "VUE-COMPONENT-API-STYLE-1",
            RuleTier::Deterministic,
            false,
            Some("not armed for this repo".to_string()),
            0,
            0,
        );
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(
            verdict.passed(),
            "an irrelevant-to-stack skip is CORRECT behaviour: {:#?}",
            verdict.failures
        );
        let justified = verdict
            .justified_skips
            .get(&SkipClass::NotApplicableToStack)
            .expect("both skips must be reported as justified");
        assert_eq!(
            justified,
            &vec![
                "PYTHON-PARAMETERIZED-SQL-1".to_string(),
                "VUE-COMPONENT-API-STYLE-1".to_string()
            ],
            "a justified skip is still REPORTED — unreported is how eyeballing starts"
        );
        assert!(render(&verdict).contains("not_applicable_to_stack"));
    }

    /// req_i2 + H1's sharpening: a rule that could never run for ANY stack is a DEFECT.
    #[test]
    fn req_i2_h1_a_rule_with_no_wired_detector_is_never_a_justified_skip() {
        let mut l = healthy_ledger();
        l.record_rule(
            "RUBY-FROZEN-STRING-LITERAL-1",
            RuleTier::Deterministic,
            false,
            Some(format!(
                "declares mechanical enforcement but has {NO_WIRED_DETECTOR_REASON}"
            )),
            0,
            0,
        );
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        assert!(
            verdict.failures.iter().any(|f| f.code == "unjustified_skip"
                && f.detail.contains("RUBY-FROZEN-STRING-LITERAL-1")
                && f.detail.contains("no_wired_detector")),
            "{:#?}",
            verdict.failures
        );
        assert!(
            verdict.justified_skips.is_empty(),
            "a no-detector rule must never be filed as justified"
        );
    }

    /// req_i2 + C11: the gate must FAIL on a tool-tier wholesale failure rather than letting
    /// it degrade quietly to "not run". This is the exact ledger state the broken eslint pass
    /// produced — `reconcile_external_tool_ledger` wrote this skip_reason verbatim.
    ///
    /// Note it fails as `unjustified_skip` even though the ExternalTool tier as a whole is NOT
    /// dark (semgrep ran and found things): a partially dead tier is still a defect, and the
    /// tier-level check alone would have missed it.
    #[test]
    fn req_i2_c11_an_eslint_wholesale_failure_fails_the_gate_rather_than_degrading_quietly() {
        let mut l = healthy_ledger();
        for id in ["JS-EQEQEQ-1", "JS-NO-VAR-1", "TS-NO-EXPLICIT-ANY-1"] {
            l.record_rule(
                id,
                RuleTier::ExternalTool,
                false,
                Some(
                    "the eslint preview pass did not run: Could not preview 10 rule(s) with \
                     eslint: EOF while parsing a value at line 1 column 0. It enforces once \
                     wired into CI."
                        .to_string(),
                ),
                0,
                0,
            );
        }
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed(), "C11 must fail the gate");
        let tool_failures: Vec<&IntegrityFailure> = verdict
            .failures
            .iter()
            .filter(|f| f.code == "unjustified_skip" && f.detail.contains("tool_failed"))
            .collect();
        assert_eq!(
            tool_failures.len(),
            3,
            "every rule the failed tier was supposed to cover must be named: {:#?}",
            verdict.failures
        );
        assert!(
            !verdict
                .failures
                .iter()
                .any(|f| f.code == "dark_tier" || f.code == "tier_absent"),
            "the tier is only PARTIALLY dead (semgrep ran) — the per-rule check is what \
             catches this, which is why both checks exist: {:#?}",
            verdict.failures
        );
    }

    /// req_i2 + C5: a stage that loses a row with no recorded disposition must fail. Three
    /// separate merge data-loss bugs shipped silently because an over-merge is
    /// observationally identical to a detection miss — the stage arithmetic is the only place
    /// it is visible at all.
    #[test]
    fn req_i2_c5_a_stage_that_does_not_reconcile_fails_the_gate() {
        let mut l = healthy_ledger();
        l.record_stage("ai-location-merge", 12, 9, StageAccounting::default());
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        let f = verdict
            .failures
            .iter()
            .find(|f| f.code == "unaccounted_rows")
            .expect("a lossy stage must be reported");
        assert!(f.detail.contains("ai-location-merge"), "{}", f.detail);
        assert!(
            f.detail.contains('3'),
            "must name the number of vanished rows: {}",
            f.detail
        );
    }

    /// req_i2 + B1/B2/B3: the ADDITION direction. Rows appended with no disclosed reason —
    /// the defect that put rows in the PDF only and made findings.json contradict itself —
    /// must fail the gate too, not just row losses.
    #[test]
    fn req_i2_b2_a_stage_that_adds_rows_with_no_disclosed_reason_fails_the_gate() {
        let mut l = healthy_ledger();
        l.record_stage(
            "evidence-gate-synthesis",
            40,
            43,
            StageAccounting::default(),
        );
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        assert!(
            verdict
                .failures
                .iter()
                .any(|f| f.code == "unaccounted_rows"
                    && f.detail.contains("evidence-gate-synthesis")),
            "{:#?}",
            verdict.failures
        );
    }

    /// req_i2: an addition that IS disclosed reconciles and must not fail — the obvious
    /// over-correction guard for the test above.
    #[test]
    fn req_i2_a_disclosed_addition_reconciles_and_passes() {
        let mut l = healthy_ledger();
        let mut acc = StageAccounting::default();
        acc.push_added("external_tool_preview_findings", 3);
        l.record_stage("external-tool-preview-merge", 40, 43, acc);
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(verdict.passed(), "{:#?}", verdict.failures);
    }

    /// req_i2, FAIL-CLOSED: an UNRECOGNISED skip reason is a defect, never a pass.
    ///
    /// This is the module's most important property. It is deliberately the inconvenient
    /// direction: a new skip reason added anywhere in the pipeline fails this gate until
    /// someone classifies it. The alternative — unknown reasons pass — is exactly the failure
    /// mode W6 exists to close, where a pipeline change quietly widens the set of things that
    /// can go wrong and nothing notices.
    #[test]
    fn req_i2_an_unrecognised_skip_reason_fails_closed() {
        let mut l = healthy_ledger();
        l.record_rule(
            "SOME-NEW-RULE-1",
            RuleTier::Deterministic,
            false,
            Some("the frobnicator was unavailable".to_string()),
            0,
            0,
        );
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(
            !verdict.passed(),
            "an unclassified skip reason must never pass the gate"
        );
        assert!(
            verdict
                .failures
                .iter()
                .any(|f| f.detail.contains("unrecognised")),
            "{:#?}",
            verdict.failures
        );
    }

    /// req_i2: and a rule with NO skip reason at all is likewise a defect — "did not run,
    /// reason unknown" is the state the ledger was built to make impossible.
    #[test]
    fn req_i2_a_skip_with_no_reason_recorded_fails_closed() {
        let mut l = healthy_ledger();
        l.record_rule("MYSTERY-1", RuleTier::Deterministic, false, None, 0, 0);
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        assert!(verdict
            .failures
            .iter()
            .any(|f| f.detail.contains("MYSTERY-1") && f.detail.contains("no reason recorded")));
    }

    /// req_i2: an empty ledger is never a pass. A gate that green-lights "no evidence a scan
    /// happened" is worse than no gate, because it manufactures confidence.
    /// W19 — the gate now asks whether the right thing was FED IN, which is the question
    /// that was never asked and the one that mattered.
    ///
    /// On 2026-10-16 a hearth scan was 63% Camerata's own prior output: the engine had
    /// written `.camerata-scan-cache.json` into the scanned repo and collected it back as
    /// source, taking `code_chars` from 103,307 to 277,543 at an unchanged 88 files. Twelve
    /// rules then found nothing, THIS GATE PASSED, and the defect was caught by an external
    /// grader reading a shipped client report.
    ///
    /// Every pre-existing family would pass that ledger again today, because each asks about
    /// the machinery and the machinery ran perfectly over the wrong input.
    #[test]
    fn req_w19_a_scan_that_audited_its_own_output_fails_the_gate() {
        let mut l = healthy_ledger();
        l.record_input(ScanInputFacts {
            files_scanned: 88,
            code_chars: 277_543,
            engine_artifacts_admitted: 1,
            ..healthy_input()
        });
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed(), "self-ingestion must never pass");
        assert!(
            verdict
                .failures
                .iter()
                .any(|f| f.code == "engine_self_ingestion"),
            "expected engine_self_ingestion, got {:?}",
            verdict.failures
        );
        assert_eq!(verdict.exit_code(), 1);

        // And it is NOT relaxed by the smoke profile. Auditing our own cache corrupts a cheap
        // local scan exactly as much as a graded one, and detecting it is free.
        let smoke = evaluate(&l, &IntegrityGateOptions::smoke());
        assert!(
            smoke
                .failures
                .iter()
                .any(|f| f.code == "engine_self_ingestion"),
            "smoke must still catch self-ingestion, got {:?}",
            smoke.failures
        );
    }

    /// W19 — an unrecorded model is disqualifying before a graded run.
    ///
    /// `None` means UNRECORDED, not "no model ran": resolution falls through to
    /// `CAMERATA_AUDIT_MODEL` and then the client default, so the run used something and did
    /// not say what. The 2026-10-16 sweep recorded `claude-sonnet-5` on its baseline and
    /// nothing on either later run, which left "did the engine regress" neither confirmable
    /// nor refutable — a week of reasoning about a regression that did not exist.
    ///
    /// Relaxed for smoke, where nothing is being compared to anything.
    #[test]
    fn req_w19_an_unrecorded_audit_model_blocks_a_graded_run_but_not_a_smoke_scan() {
        let mut l = healthy_ledger();
        l.record_input(ScanInputFacts {
            audit_model: None,
            ..healthy_input()
        });

        let graded = evaluate(&l, &IntegrityGateOptions::default());
        assert!(
            graded.failures.iter().any(|f| f.code == "model_unrecorded"),
            "a graded run must refuse an unrecorded model, got {:?}",
            graded.failures
        );

        let smoke = evaluate(&l, &IntegrityGateOptions::smoke());
        assert!(
            !smoke.failures.iter().any(|f| f.code == "model_unrecorded"),
            "a smoke scan compares nothing, so it must not care: {:?}",
            smoke.failures
        );
    }

    /// W19 — rules and stages recorded over ZERO files is the one shape a coverage summary
    /// cannot tell apart from real health: every rule is vacuously clean.
    #[test]
    fn req_w19_a_scan_over_zero_files_fails_even_though_every_rule_ran() {
        let mut l = healthy_ledger();
        l.record_input(ScanInputFacts {
            files_scanned: 0,
            code_chars: 0,
            ..healthy_input()
        });
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(
            verdict.failures.iter().any(|f| f.code == "empty_input"),
            "expected empty_input, got {:?}",
            verdict.failures
        );

        // An EMPTY ledger reports `empty_ledger` and must not also report `empty_input` —
        // one defect, one message.
        let empty = evaluate(&ScanLedger::new(), &IntegrityGateOptions::default());
        assert!(empty.failures.iter().any(|f| f.code == "empty_ledger"));
        assert!(
            !empty.failures.iter().any(|f| f.code == "empty_input"),
            "an empty ledger must not double-report: {:?}",
            empty.failures
        );
    }

    /// The W20 regression guard: a clean scan records zero engine artifacts and still passes.
    /// Without this, the three tests above could all be satisfied by a gate that fails
    /// everything.
    #[test]
    fn req_w19_a_clean_input_still_passes() {
        let verdict = evaluate(&healthy_ledger(), &IntegrityGateOptions::default());
        assert!(
            verdict.passed(),
            "clean input must pass: {:?}",
            verdict.failures
        );
        assert_eq!(healthy_input().engine_artifacts_admitted, 0);
    }

    /// W21 — the backstop over the SEALED row set.
    ///
    /// Both known sources of a bad path are fixed, so this exists for the third one nobody
    /// has written yet. The shapes are the two measured in the 2026-10-18 hearth export: 32
    /// absolute paths from the ESLint preview tier disclosing the auditor's home directory,
    /// and 10 rows where an unparseable evidence citation put a 180-character sentence in
    /// the field a client reads as a file path.
    #[test]
    fn req_w21_absolute_and_prose_paths_in_the_sealed_rows_fail_the_gate() {
        use crate::scan_ledger::PathHygiene;

        let paths = [
            // Clean, and the majority — so a failure here cannot be the gate refusing
            // everything.
            "app/api/checkout/route.ts",
            "supabase/migrations/0003_bookings.sql",
            // Intentionally unlocated. W21 made an unparseable citation yield an empty path;
            // absence is correct and must NOT count against hygiene.
            "",
            // The real absolute shape.
            "/Users/auditor/Documents/Repos/clients/acme/app/admin/page.tsx",
            // The real prose shape.
            "app/api/bookings/route.ts:15-39 — booking insert, payment insert, and slot \
             update execute as three separate awaits with no transaction boundary",
            // Windows separators.
            "src\\handlers\\checkout.ts",
        ];
        let h = PathHygiene::audit(paths.iter().copied());
        assert_eq!(h.rows_total, 6);
        assert_eq!(h.absolute, 1, "one absolute path");
        assert_eq!(h.prose, 1, "one prose path");
        assert_eq!(h.backslash, 1, "one backslash path");
        assert_eq!(h.offending_rows, 3, "three DISTINCT bad rows");
        assert!(!h.is_clean());
        assert!(
            h.worst_example
                .as_deref()
                .is_some_and(|w| w.contains("transaction boundary")),
            "the worst example should be the prose one, the hardest to spot from a count: {:?}",
            h.worst_example
        );

        let mut l = healthy_ledger();
        l.record_path_hygiene(h);
        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        let f = verdict
            .failures
            .iter()
            .find(|f| f.code == "path_hygiene")
            .expect("expected a path_hygiene failure");
        assert!(f.detail.contains("3 of 6"), "{}", f.detail);
        assert!(
            f.detail.contains("worst:"),
            "must name an offender, not just count: {}",
            f.detail
        );
    }

    /// A repository path may legitimately contain spaces, and the prose signals must not
    /// fire on one. The hearth fixture itself lives under a folder whose name has a space, so this is the
    /// exact false positive that would have made the gate cry wolf on every real run.
    #[test]
    fn req_w21_path_hygiene_does_not_cry_wolf_on_ordinary_paths() {
        use crate::scan_ledger::PathHygiene;
        let h = PathHygiene::audit(
            [
                "app/listings/[id]/page.tsx",
                "client fixtures/hearth/app/admin/page.tsx",
                "src/my-service/orders.rs",
                "docs/decisions/2026-05-14-per-resource-permission-flags.md",
                "a/very/deeply/nested/path/that/goes/on/for/quite/a/while/but/is/still/a/real/path/handler.ts",
                "",
            ]
            .into_iter(),
        );
        assert!(h.is_clean(), "ordinary paths must pass: {h:?}");
        assert_eq!(h.offending_rows, 0);

        let verdict = evaluate(&healthy_ledger(), &IntegrityGateOptions::default());
        assert!(verdict.passed(), "{:?}", verdict.failures);
    }

    #[test]
    fn req_i2_an_empty_ledger_never_passes() {
        let verdict = evaluate(&ScanLedger::new(), &IntegrityGateOptions::default());
        assert!(!verdict.passed());
        assert!(verdict.failures.iter().any(|f| f.code == "empty_ledger"));
    }

    /// req_i2: `require_semantic_tier` is the operator's call, and it changes the verdict.
    /// "AI/semantic review not requested this run" is an honest disclosure and a perfectly
    /// fine state for a cheap smoke scan — and a complete blocker before spending $26 grading
    /// the semantic tier. Both readings are correct; which one applies is not this module's
    /// decision to make silently.
    #[test]
    fn req_i2_a_scoped_out_semantic_tier_is_justified_for_a_smoke_scan_and_a_blocker_before_grading(
    ) {
        let mut l = ScanLedger::new();
        l.record_input(healthy_input());
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 10, 1);
        l.record_rule("TAINT-SQLI-1", RuleTier::ExternalTool, true, None, 10, 0);
        l.record_rule(
            "AI-ADVISORY-1",
            RuleTier::Semantic,
            false,
            Some("AI/semantic review not requested this run".to_string()),
            0,
            0,
        );

        let smoke = evaluate(&l, &IntegrityGateOptions::smoke());
        assert!(
            smoke.passed(),
            "a disclosed scope choice is fine for a smoke scan: {:#?}",
            smoke.failures
        );
        assert!(smoke
            .justified_skips
            .get(&SkipClass::ScopedOut)
            .is_some_and(|ids| ids.contains(&"AI-ADVISORY-1".to_string())));

        let graded = evaluate(&l, &IntegrityGateOptions::default());
        assert!(
            !graded.passed(),
            "the same ledger must BLOCK a graded run that grades the semantic tier"
        );
        assert!(
            graded
                .failures
                .iter()
                .any(|f| f.code == "dark_tier" && f.detail.contains("semantic")),
            "{:#?}",
            graded.failures
        );
    }

    /// req_i2: the tier summary is populated even on a PASS, and covers every engine —
    /// including one with zero rules — so a passing gate is still informative rather than a
    /// bare green tick.
    #[test]
    fn req_i2_the_verdict_reports_every_engine_even_on_a_pass() {
        let verdict = evaluate(&healthy_ledger(), &IntegrityGateOptions::default());
        assert_eq!(verdict.tier_summary.len(), RuleTier::all().len());
        let rendered = render(&verdict);
        for tier in RuleTier::all() {
            assert!(
                rendered.contains(tier.family_label()),
                "every engine must appear in the report, including empty ones: {rendered}"
            );
        }
        let det = verdict
            .tier_summary
            .iter()
            .find(|t| t.tier == RuleTier::Deterministic)
            .expect("deterministic summary");
        assert_eq!(det.rules_recorded, 1);
        assert_eq!(det.rules_ran, 1);
        assert_eq!(det.findings_emitted, 1);
    }

    /// req_i2: the classifier checks DEFECT phrases first, so a tool-failure reason that also
    /// happens to mention languages or files cannot launder itself into a justified skip. The
    /// real `reconcile_external_tool_ledger` reason embeds a whole `CoverageNote` message, so
    /// this overlap is a real shape, not a hypothetical.
    #[test]
    fn req_i2_a_reason_containing_both_a_failure_and_a_justified_phrase_classifies_as_the_failure()
    {
        let class = SkipClass::classify(Some(
            "the eslint preview pass did not run: Could not preview 3 rule(s) with eslint: \
             no matching files were readable",
        ));
        assert_eq!(
            class,
            SkipClass::ToolFailed,
            "a failure that mentions files must not be laundered into a stack justification"
        );
        assert!(!class.is_justified());
    }

    /// req_i2: the classifier's whole table, pinned. A wording change at any producer that
    /// drops out of these phrase sets lands in `Unrecognised` and fails the gate loudly —
    /// which is the intended, fail-closed consequence.
    #[test]
    fn req_i2_skip_reason_classification_table() {
        for (reason, want) in [
            ("no matching files", SkipClass::NotApplicableToStack),
            ("not armed for this repo", SkipClass::NotApplicableToStack),
            (
                "no supported language was present",
                SkipClass::NotApplicableToStack,
            ),
            (
                "AI/semantic review not requested this run",
                SkipClass::ScopedOut,
            ),
            (
                "deterministic scan deselected for this run",
                SkipClass::ScopedOut,
            ),
            ("gated off", SkipClass::ScopedOut),
            (
                "the commodity taint pass did not run: semgrep was never invoked for any \
                 scanned repo this scan",
                SkipClass::ToolFailed,
            ),
            (
                "the eslint preview pass did not run: eslint provisioning: npm install failed",
                SkipClass::ToolFailed,
            ),
            ("did not run this scan", SkipClass::Unrecognised),
            ("", SkipClass::Unrecognised),
        ] {
            assert_eq!(
                SkipClass::classify(Some(reason)),
                want,
                "classification of {reason:?}"
            );
        }
        assert_eq!(SkipClass::classify(None), SkipClass::Unrecognised);
        assert_eq!(
            SkipClass::classify(Some(&format!("has {NO_WIRED_DETECTOR_REASON}"))),
            SkipClass::NoWiredDetector
        );
    }

    /// Wave C: a clone-based rule whose reviewed judgment did not run because no model was
    /// available is a disclosed scope choice (justified), while a judgment that was attempted
    /// and failed carries wording the gate does not recognise, so it stays a defect.
    #[test]
    fn a_clone_rule_whose_judgment_was_unavailable_is_scoped_out_and_a_failed_one_is_not() {
        assert_eq!(
            SkipClass::classify(Some(ADVISORY_UNAVAILABLE_NOTE)),
            SkipClass::ScopedOut
        );
        assert_eq!(
            SkipClass::classify(Some(
                "clone-candidate judgment failed: every batch returned an error"
            )),
            SkipClass::Unrecognised
        );
    }

    /// req_i2: the gate is PURE — same ledger in, same verdict out, byte for byte. A gate
    /// whose own output wobbles between runs is a gate nobody can diff or trust to block on.
    #[test]
    fn req_i2_the_verdict_is_deterministic_and_its_failures_are_stably_ordered() {
        let mut l = healthy_ledger();
        l.record_rule("ZZZ-1", RuleTier::Deterministic, false, None, 0, 0);
        l.record_rule("AAA-1", RuleTier::Deterministic, false, None, 0, 0);
        l.record_stage("lossy", 5, 3, StageAccounting::default());
        let a = evaluate(&l, &IntegrityGateOptions::default());
        let b = evaluate(&l, &IntegrityGateOptions::default());
        assert_eq!(a, b, "the gate must be deterministic");
        assert_eq!(render(&a), render(&b));
        let codes: Vec<&str> = a.failures.iter().map(|f| f.code.as_str()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "failures must be stably ordered: {codes:?}");
    }

    // ── req_w13: a rule whose CORPUS FILE would not parse ────────────────────────────────
    //
    // Every production corpus load was `let (corpus, _errs) = load_corpus_lenient(..)`. A rule
    // file with a typo'd enum value therefore failed to parse, left the corpus, and NOTHING
    // reported it. The gate could not see it even in principle: a rule that never loaded is
    // not a skip, it is absent from the ledger's universe. These tests cover the gate half —
    // that once the ledger carries it (W13's `ScanLedger::record_corpus_load_failure`), the
    // gate fails on it, in its OWN family, under EVERY profile.

    /// req_w13: the load-bearing requirement. A corpus file that would not parse fails
    /// `camerata integrity-gate`, with exit code 1, in its own `corpus_load_failure` family.
    ///
    /// FAILS BEFORE THE FIX in two independent ways, both verified: (1) `SkipClass` had no
    /// `CorpusLoadFailed` variant and no `corpus_load_failure` code existed, so this did not
    /// compile; (2) more importantly, nothing ever put such an entry in a ledger, so even a
    /// hand-written equivalent assertion could not fire on a real scan. See
    /// `tests/corpus_load_failure_e2e.rs` for the end-to-end half.
    #[test]
    fn req_w13_a_rule_whose_corpus_file_failed_to_parse_fails_the_gate_in_its_own_family() {
        let mut l = healthy_ledger();
        l.record_corpus_load_failure("SEC-NO-SILENT-CORPUS-DROP-1");

        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        assert!(
            !verdict.passed(),
            "an unparseable rule file must never pass the gate: {:#?}",
            verdict.failures
        );
        assert_eq!(verdict.exit_code(), 1, "a failing verdict exits 1");

        let hits: Vec<&IntegrityFailure> = verdict
            .failures
            .iter()
            .filter(|f| f.code == "corpus_load_failure")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "exactly one `corpus_load_failure`, naming the rule: {:#?}",
            verdict.failures
        );
        assert!(
            hits[0].detail.contains("SEC-NO-SILENT-CORPUS-DROP-1"),
            "the failure must name the rule so an operator can find the file: {}",
            hits[0].detail
        );

        // Its OWN family, never borrowed from one that means something else. `unjustified_skip`
        // means "it was skipped for a bad reason"; this rule was never even selectable, and the
        // fix is in a `.toml` rather than in the pipeline.
        assert!(
            !verdict
                .failures
                .iter()
                .any(|f| f.code == "unjustified_skip"),
            "a corpus-load failure must not be reported as an unjustified skip: {:#?}",
            verdict.failures
        );
        assert!(render(&verdict).contains("Do NOT spend a graded run"));
    }

    /// req_w13: NEVER justified, under EVERY profile — including the relaxed smoke profile a
    /// cheap local scan uses. `IntegrityGateOptions::smoke()` legitimately tolerates a dark
    /// tier and a scoped-out semantic tier; it may not tolerate a rule library that would not
    /// load, because there is no repo for which an unparseable rule file is correct behaviour.
    #[test]
    fn req_w13_a_corpus_load_failure_is_never_justified_under_any_profile() {
        let mut l = healthy_ledger();
        l.record_corpus_load_failure("SEC-NO-SILENT-CORPUS-DROP-1");

        for (label, opts) in [
            ("graded", IntegrityGateOptions::default()),
            ("smoke", IntegrityGateOptions::smoke()),
        ] {
            let verdict = evaluate(&l, &opts);
            assert!(
                verdict
                    .failures
                    .iter()
                    .any(|f| f.code == "corpus_load_failure"),
                "the {label} profile must fail on an unparseable rule file: {:#?}",
                verdict.failures
            );
            assert!(
                !verdict
                    .justified_skips
                    .contains_key(&SkipClass::CorpusLoadFailed),
                "the {label} profile must never file this under justified skips: {:#?}",
                verdict.justified_skips
            );
        }
        assert!(
            !SkipClass::CorpusLoadFailed.is_justified(),
            "the class itself must report as unjustified"
        );
    }

    /// req_w13: the classifier keys on the SHARED constant, not a retyped phrase — so a
    /// wording edit at the recording site cannot silently decouple it from the gate (the W3
    /// hardening `NO_WIRED_DETECTOR_REASON` exists for, applied to the new reason).
    ///
    /// Also pins the precedence: W13's phrase is checked BEFORE every other defect phrase, so a
    /// reason that embeds both cannot be misfiled.
    #[test]
    fn req_w13_the_corpus_load_reason_is_classified_against_the_shared_constant() {
        assert_eq!(
            SkipClass::classify(Some(CORPUS_LOAD_FAILED_REASON)),
            SkipClass::CorpusLoadFailed
        );
        assert_eq!(
            SkipClass::classify(Some(&CORPUS_LOAD_FAILED_REASON.to_ascii_uppercase())),
            SkipClass::CorpusLoadFailed,
            "matching is case-insensitive, like every other class"
        );
        assert_eq!(
            SkipClass::classify(Some(&format!(
                "the eslint preview pass did not run: {CORPUS_LOAD_FAILED_REASON}"
            ))),
            SkipClass::CorpusLoadFailed,
            "W13's phrase wins over a tool-failure phrase: a file that would not parse is the \
             most specific and least recoverable cause, and must not be shadowed"
        );
        // And the entry a real recording site produces classifies the same way, so the
        // producer and the gate are pinned to each other rather than to a literal.
        let mut l = ScanLedger::new();
        l.record_input(healthy_input());
        l.record_corpus_load_failure("SEC-NO-SILENT-CORPUS-DROP-1");
        let entry = l
            .rule("SEC-NO-SILENT-CORPUS-DROP-1")
            .expect("just recorded");
        assert_eq!(
            SkipClass::classify(entry.skip_reason.as_deref()),
            SkipClass::CorpusLoadFailed
        );
    }

    /// req_w13, CASE 3: a rule that RAN (a hardcoded floor detector keys on its id, not on its
    /// corpus content) while its own `.toml` would not parse must STILL fail the gate. There is
    /// no skip reason to classify in this case, so the gate reads the additive
    /// `coverage_note` marker instead.
    ///
    /// Without this arm the fix would have a silent hole in exactly the shape it exists to
    /// close: a rule evaluated without its authored directive, remediation, severity bounds or
    /// options produced a degraded client report, and nothing would have said so.
    #[test]
    fn req_w13_a_rule_that_ran_with_an_unparseable_file_still_fails_the_gate() {
        let mut l = healthy_ledger();
        // `SEC-FLOOR-1` is already recorded as ran=true,findings=1 by `healthy_ledger`.
        l.record_corpus_load_failure("SEC-FLOOR-1");
        let entry = l.rule("SEC-FLOOR-1").expect("still recorded");
        assert!(entry.ran, "precondition: this arm is about a rule that RAN");

        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        let hits: Vec<&IntegrityFailure> = verdict
            .failures
            .iter()
            .filter(|f| f.code == "corpus_load_failure")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "a rule that ran without its authored text must still fail: {:#?}",
            verdict.failures
        );
        assert!(hits[0].detail.contains("SEC-FLOOR-1"), "{}", hits[0].detail);
        assert_eq!(verdict.exit_code(), 1);
    }

    /// req_w13: the three cases produce EXACTLY ONE failure each, never two. The `!ran` cases
    /// are reported by the skip loop and the `ran` case by the coverage-note loop; a rule must
    /// not be double-counted by both, or the gate's own failure count stops being a count.
    #[test]
    fn req_w13_each_unloadable_rule_is_reported_exactly_once() {
        let mut l = healthy_ledger();
        // Case 1: no prior entry.
        l.record_corpus_load_failure("NEW-RULE-1");
        // Case 2: a prior not-run entry with an incidental reason.
        l.record_rule(
            "SCOPED-RULE-1",
            RuleTier::Semantic,
            false,
            Some("AI/semantic review not requested this run".to_string()),
            0,
            0,
        );
        l.record_corpus_load_failure("SCOPED-RULE-1");
        // Case 3: a prior entry that RAN.
        l.record_corpus_load_failure("SEC-FLOOR-1");

        let verdict = evaluate(&l, &IntegrityGateOptions::default());
        let hits: Vec<&str> = verdict
            .failures
            .iter()
            .filter(|f| f.code == "corpus_load_failure")
            .map(|f| f.detail.as_str())
            .collect();
        assert_eq!(
            hits.len(),
            3,
            "exactly one failure per unloadable rule, no duplicates: {:#?}",
            verdict.failures
        );
        for id in ["NEW-RULE-1", "SCOPED-RULE-1", "SEC-FLOOR-1"] {
            assert_eq!(
                hits.iter().filter(|d| d.contains(id)).count(),
                1,
                "{id} must be reported exactly once: {hits:#?}"
            );
        }
        // And none of them is misfiled as an unjustified skip.
        assert!(
            !verdict
                .failures
                .iter()
                .any(|f| f.code == "unjustified_skip"),
            "{:#?}",
            verdict.failures
        );
    }

    /// req_w13 + H1's other half: the gate must stay switched on. A scan whose rule library
    /// loaded cleanly must NOT acquire a `corpus_load_failure`, and the new `Unloaded` tier
    /// recording zero rules must not read as a dark or absent tier. A gate that cries wolf on
    /// a correct scan gets disabled, which costs more than the defect it guarded.
    #[test]
    fn req_w13_a_scan_whose_rule_library_loaded_cleanly_is_not_flagged() {
        let verdict = evaluate(&healthy_ledger(), &IntegrityGateOptions::default());
        assert!(
            verdict.passed(),
            "a clean corpus load must stay a PASS: {:#?}",
            verdict.failures
        );
        let unloaded = verdict
            .tier_summary
            .iter()
            .find(|t| t.tier == RuleTier::Unloaded)
            .expect("the Unloaded tier must appear in the summary even with nothing in it");
        assert_eq!(
            (unloaded.rules_recorded, unloaded.rules_ran),
            (0, 0),
            "on a healthy scan this tier reads zero, which IS the positive disclosure"
        );
        assert!(
            !IntegrityGateOptions::default()
                .required_tiers
                .contains(&RuleTier::Unloaded),
            "the Unloaded tier must never be a REQUIRED tier — requiring it would demand that \
             every scan have a broken rule file"
        );
    }
}
