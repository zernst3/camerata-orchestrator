//! camerata-rules: rule corpus loader, enforcement-kind classifier, and
//! per-task rule-subset selection.
//!
//! # Responsibilities
//!
//! 1. Recursively load TOML rule files from a corpus directory (the bundled
//!    `crates/rules/principles/` by default; override with `CAMERATA_CORPUS_PATH`).
//! 2. Parse each file into a [`Rule`] with the fields that the orchestrator
//!    cares about: `id`, `title`, `enforcement`, `domain`, `summary`.
//! 3. Index all loaded rules into a [`RuleSet`] — queryable by id or domain.
//! 4. Expose a pure [`select`] function: given a filter, return a
//!    `Vec<Rule>` — the per-task rule-subset.
//!
//! All I/O is `async`; pure selection helpers are synchronous.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use camerata_core::RuleId;
use serde::Deserialize;
use thiserror::Error;

pub mod applies_to;
pub use applies_to::AppliesTo;

mod lock;
pub use lock::{
    check_lock, corpus_lock_path, corpus_version, CorpusLock, LockEntry, LockError, LockProblem,
    RULE_HASH_PREFIX,
};

mod freshness;
pub mod severity_bounds;
pub use freshness::{
    client_rules_last_reviewed, confirm_domain, confirm_in_text, corpus_freshness_path,
    days_from_civil, effective_review_date, format_staleness_nag, parse_ymd, CorpusFreshness,
    DueSoonDomain, FreshnessEditError, FreshnessReport, OverdueDomain, OPERATOR_ONLY_MARKERS,
};

// ────────────────────────────────────────────────────────────────────────────
// Error type (RUST-DOMAIN-4 / RUST-DOMAIN-6)
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum RulesError {
    #[error("I/O error reading corpus at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("TOML parse error in {path}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("rule file {path} is missing a required field: {field}")]
    MissingField { path: PathBuf, field: &'static str },
}

// ────────────────────────────────────────────────────────────────────────────
// Enforcement kind
// ────────────────────────────────────────────────────────────────────────────

/// The emission tiers for a camerata rule (from CAMERATA-ANATOMY-1).
///
/// - `Prose`         — human-readable rationale only; no generated artifact.
/// - `Structured`    — emits a structured section (e.g. a CONVENTIONS.md entry).
/// - `Mechanical`    — emits a runnable check (linter, regex, CI gate, etc.).
/// - `Architectural` — a *deterministically* checkable structural rule that no
///   regex can express and no LLM is needed to judge: it requires parsing the
///   code into an AST and reasoning over its structure (e.g. "a handler does
///   not touch the DB directly", "a service does not bypass the repository",
///   "no cross-boundary imports"). Like `Mechanical`, it is a hard, repeatable
///   check; unlike `Mechanical`, the check is an AST/static-analysis pass rather
///   than a lint pattern. See
///   `docs/decisions/2026-06-19_ast_architectural_rule_tier.md`.
///
/// Tier ordering by strictness/automation: `Prose` < `Structured` < `Mechanical`
/// < `Architectural`. `Architectural` is the most precise tier — it never
/// produces a false "probably" the way a regex digest scan can.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnforcementKind {
    Prose,
    Structured,
    Mechanical,
    Architectural,
}

impl EnforcementKind {
    /// The lowercase wire/TOML string for this tier. Inverse of [`EnforcementKind::from_tag`].
    pub fn as_str(&self) -> &'static str {
        match self {
            EnforcementKind::Prose => "prose",
            EnforcementKind::Structured => "structured",
            EnforcementKind::Mechanical => "mechanical",
            EnforcementKind::Architectural => "architectural",
        }
    }

    /// Parse a tier from its lowercase wire/TOML string. Inverse of
    /// [`EnforcementKind::as_str`]. Unknown strings return `None`.
    ///
    /// Named `from_tag` rather than `from_str` so it is not confused with the
    /// `std::str::FromStr` trait method (which would return a `Result`).
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "prose" => Some(EnforcementKind::Prose),
            "structured" => Some(EnforcementKind::Structured),
            "mechanical" => Some(EnforcementKind::Mechanical),
            "architectural" => Some(EnforcementKind::Architectural),
            _ => None,
        }
    }

    /// Whether this tier is enforced at the CI / integration stage rather than at
    /// the write-time gate. Both `Mechanical` (lint / query-plan / migration audit)
    /// and `Architectural` (AST static analysis) run in CI: they need the full
    /// parsed module (or build/DB context), which the write-time gate does not have.
    /// `Prose` and `Structured` are human-reviewed at PR.
    pub fn is_ci_enforced(&self) -> bool {
        matches!(
            self,
            EnforcementKind::Mechanical | EnforcementKind::Architectural
        )
    }

    /// Whether this tier emits into `CONVENTIONS.md` (citable by id) rather than
    /// `AGENTS.md` (agent-judged prose). Everything except `Prose` is citable.
    pub fn emits_to_conventions(&self) -> bool {
        !matches!(self, EnforcementKind::Prose)
    }
}

impl std::fmt::Display for EnforcementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Rule kind (Pass-1 W2 — A5.1, `docs/plans/2026-10-09_audit-findings-tracker.md`)
// ────────────────────────────────────────────────────────────────────────────

/// The FOUR-WAY classification of how a multi-option rule's alternatives relate to the
/// rule's underlying requirement — orthogonal to [`EnforcementKind`] (which classifies HOW a
/// rule is emitted/enforced, not what its alternatives MEAN).
///
/// This is the direct fix for the shipped defect (A5, `ai_audit.rs` ~5885/~5921-5929): the
/// alternatives pass used to pick whichever option the repo follows and REPLACE the rule's
/// effective directive with it, so a repo that correctly follows the degenerate/absent form of
/// a requirement reported ZERO findings — "consistently incorrect" read as "compliant" because
/// the engine had no way to tell a genuine architectural alternative apart from the defect
/// itself. `RuleKind` is that missing distinction:
///
/// - [`RuleKind::Defect`] — there is NO acceptable alternative anywhere; every option other
///   than the correct one describes the absence or a degenerate form of the requirement
///   (see [`RuleOption::violates`]). A `defect`-kind rule NEVER enters the alternatives pass —
///   it behaves exactly like a single-directive rule, because there is nothing to decide among.
/// - [`RuleKind::Decision`] — the alternatives genuinely hold EQUAL weight (e.g. API
///   versioning scheme, ID representation, package-by-feature vs. package-by-layer). The only
///   reportable condition is INCONSISTENCY across the codebase, never "the repo picked option
///   B" by itself. No option on a `decision`-kind rule should carry `violates = true`.
/// - [`RuleKind::DefaultWithExceptions`] — one option is correct UNLESS justified; deviating
///   is a finding even when done consistently. Some non-default options may be legitimate,
///   justified alternatives (`violates = false`); others may be the degenerate/absent form of
///   the requirement wearing the same `[[option]]` shape (`violates = true`) — both can coexist
///   on the same rule.
/// - [`RuleKind::DecisionWithFloor`] — SEVERAL options hold equal weight (as in `Decision`),
///   AND the rule additionally names shapes that are faults under every one of them (as in
///   `DefaultWithExceptions`). W17; see that variant's own doc comment for the full pipeline
///   contract and for why this is not expressible as either of the two it sits between.
///
/// `serde(default)` deliberately does NOT exist here — see [`RuleToml::kind`]'s doc comment:
/// an absent `kind` must not silently resolve to a permissive value. [`Rule::kind`] is
/// `Option<RuleKind>`, and the corpus-load gate (`rules_missing_kind`, exercised by
/// `req_a5_1_every_corpus_rule_declares_a_kind`) fails the build on any `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// No acceptable alternative anywhere — every non-correct option is the absence/degenerate
    /// form of the requirement. Never enters the alternatives pass (see
    /// [`is_defect_kind`](Rule::is_defect_kind)).
    Defect,
    /// Alternatives genuinely hold equal weight. Only cross-codebase INCONSISTENCY is
    /// reportable; no option should carry `violates = true`.
    Decision,
    /// One option is correct unless justified; deviating is a finding even if done
    /// consistently. May mix legitimate, justified alternatives with degenerate ones on the
    /// same rule.
    DefaultWithExceptions,
    /// SEVERAL options hold genuinely equal weight — no privileged default, the choice follows
    /// from project context — AND the rule separately names shapes that are faults under every
    /// one of those choices. W17.
    ///
    /// # Why the three-way taxonomy could not express this
    ///
    /// [`RuleKind::Decision`] forbids `violates` on any option, so classifying a rule of this
    /// shape `decision` means DELETING real fault flags — the A5/gotcha-3 defect class, where
    /// "no RLS policy" reported as a convention the codebase chose.
    /// [`RuleKind::DefaultWithExceptions`] requires one privileged default with the rest as
    /// deviations FROM it, so classifying it DWE means fabricating a default that does not
    /// exist — which also drags in gotcha 2 (`default = false` + DWE is a contradiction) and
    /// leaves `has_default()` false while the kind asserts a default exists. Neither is a
    /// rounding error; each forces a false statement about the rule into the corpus.
    ///
    /// # Name
    ///
    /// "Decision" because the live alternatives genuinely hold equal weight; "with floor"
    /// because the rule also names a floor BENEATH that equality — shapes no choice among the
    /// legitimate options can excuse. The grammar deliberately parallels
    /// `default_with_exceptions`, so the four values read as one taxonomy. Note the floor here
    /// is on the OPTION SPACE and has nothing to do with [`Rule::min_severity`]'s severity
    /// floor; the two words collide and the mechanisms do not touch.
    ///
    /// # What it does to the pipeline
    ///
    /// Adding this variant required NO new pipeline behaviour, verified by reading the two
    /// branches rather than by assuming: `ai_audit::apply_alternative_decision` keys purely on
    /// the DECIDED option's `violates` and never consults `kind`, and the no-convention escape
    /// is gated independently on `kind`. The two are orthogonal, so a kind that permits flags
    /// AND withholds the escape was already reachable; only the value was missing.
    ///
    /// - **Arming: no effect.** `kind` is not consulted when deciding whether a rule arms.
    ///   That is [`Rule::is_auto_recommended`] (`verification`), [`Rule::is_opt_in_only`] and
    ///   [`Rule::inspectable`]. A `decision_with_floor` rule arms on exactly the same terms as
    ///   any other.
    /// - **Emission: no effect.** The CONVENTIONS/AGENTS partition is `arm.rs`'s own
    ///   `is_ci_tier` over [`EnforcementKind`]. `kind` never reaches it.
    /// - **Alternatives pass: ENTERS it.** [`Rule::is_defect_kind`] is false, so
    ///   `onboard::audit::is_semantic_multi_option_rule` admits the rule on the usual terms
    ///   (>= 2 options, non-CI-tier, not gate-armed, code-auditable). A decided
    ///   `violates = false` option rewrites the rule's effective directive and per-site rows
    ///   become [`FindingKind::Deviation`]; a decided `violates = true` option leaves the
    ///   directive untouched and emits the guaranteed evidence-backed `Defect`. Both branches
    ///   are the behaviour that already existed — which is the entire point of the shape: the
    ///   engine must be able to say "the repo picked a legitimate organization" AND "the repo
    ///   picked a named fault" for the same rule.
    /// - **No-convention escape: WITHHELD** ([`RuleKind::offers_no_convention_escape`] is
    ///   false, same as `default_with_exceptions`). This is a deliberate safety choice, not an
    ///   artifact of taking the cheaper branch. The escape severs the per-site violation path
    ///   in three layers, so granting it would let a repo that is split between a legitimate
    ///   option AND a named fault answer "no convention adopted" and have the fault go
    ///   unreported — reconstructing precisely the A5 defect this taxonomy exists to prevent.
    ///   A rule that names faults can always report them; it may not opt out of reporting
    ///   them. (`docs/RULES_REFERENCE.md` section 11 item 7 speculated the escape should be
    ///   allowed "among the genuine options"; the pipeline has no way to scope the escape to a
    ///   subset of options, so in practice that would be the unscoped escape.)
    /// - **Severity: no direct effect, one indirect consequence.**
    ///   [`Rule::deviation_ceiling`] is LIVE on this kind (contrast gotcha 6, where it is inert
    ///   on `defect` because such a rule emits no Deviation rows), because the rule does enter
    ///   the alternatives pass. A decided `violates` option's finding anchors at `"high"`; cap
    ///   it per rule with [`Rule::max_severity`] if that is too aggressive for a specific rule.
    /// - **Flags: PERMITTED and expected** ([`RuleKind::permits_violates_flag`] is true). The
    ///   `Rejected:` convention applies to every flagged option, and GATE 1
    ///   ([`options_with_rejected_prose_but_no_violates_flag`]) holds this kind to it;
    ///   [`suspicious_default_with_exceptions_rules`] deliberately does NOT sweep it, because
    ///   that gate's premise is a privileged default this kind has none of.
    ///
    /// # Authoring it honestly
    ///
    /// `default = false` and no `[decision].default`: there is no adopted default, which is the
    /// whole distinction from `default_with_exceptions`. At least two options whose own `why`
    /// blesses them ("the natural fit", "a legitimate choice"), at least one `violates = true`
    /// option whose `why` opens `Rejected:`, and a `[decision].why` that says the choice follows
    /// from project context rather than from a presumed winner. If only ONE option is blessed,
    /// the rule is `default_with_exceptions` (or `defect`) and not this.
    ///
    /// Exemplar and, as of W17, the only corpus rule classified here: `RUST-DOMAIN-6`. Its own
    /// `why` holds both halves in one paragraph — organizing errors by failure category or by
    /// module/aggregate is "a genuine organizational choice" that "follows from the crate's size
    /// and how its failures cluster, not from a presumed winner", while three other shapes "are
    /// not organizational choices but boundary failures, and are called out as the options to
    /// avoid regardless of which organization is chosen". The candidate sweep that found it the
    /// only instance is recorded in `crates/rules/tests/w17_decision_with_floor_kind.rs`.
    DecisionWithFloor,
}

impl RuleKind {
    /// Every variant, so a sweep or a round-trip test enumerates the taxonomy instead of
    /// restating a hand-maintained copy of it. Adding a variant without extending this is a
    /// compile error (the array length is checked), which is deliberate: the `as_str`/`from_tag`
    /// round-trip test iterates this, so a new kind cannot ship untested.
    pub const ALL: [RuleKind; 4] = [
        RuleKind::Defect,
        RuleKind::Decision,
        RuleKind::DefaultWithExceptions,
        RuleKind::DecisionWithFloor,
    ];

    /// The snake_case wire/TOML string for this kind. Inverse of [`RuleKind::from_tag`].
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleKind::Defect => "defect",
            RuleKind::Decision => "decision",
            RuleKind::DefaultWithExceptions => "default_with_exceptions",
            RuleKind::DecisionWithFloor => "decision_with_floor",
        }
    }

    /// Parse a kind from its snake_case wire/TOML string. Unknown strings return `None`.
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "defect" => Some(RuleKind::Defect),
            "decision" => Some(RuleKind::Decision),
            "default_with_exceptions" => Some(RuleKind::DefaultWithExceptions),
            "decision_with_floor" => Some(RuleKind::DecisionWithFloor),
            _ => None,
        }
    }

    /// Whether this kind PERMITS an option to carry `violates = true`.
    ///
    /// `true` for every kind except [`RuleKind::Decision`], whose premise is that its
    /// alternatives hold equal weight — a flag there is a contradiction, it is live at runtime,
    /// and [`decision_kind_rules_with_violates_flag`] fails the build on it.
    ///
    /// Exhaustive on purpose, with no `_` arm: a new variant must state its answer here rather
    /// than inherit one. A kind that silently matched a catch-all is exactly how this corpus
    /// accumulated three separate fully-implemented, fully-inert mechanisms.
    pub fn permits_violates_flag(&self) -> bool {
        match self {
            RuleKind::Defect => true,
            // Documentation plus gate signal only on a `defect` rule (it never enters the
            // alternatives pass), but permitted and expected.
            RuleKind::Decision => false,
            RuleKind::DefaultWithExceptions => true,
            RuleKind::DecisionWithFloor => true,
        }
    }

    /// Whether the alternatives pass may offer this kind the `[PURE DECISION — "no convention"
    /// is a valid answer]` escape, and whether the parser may honor a `no_convention` claim for
    /// it. Consumed by `camerata_server::ai_audit`'s `build_alternatives_block` and
    /// `parse_alternative_recommendations`.
    ///
    /// `true` ONLY for [`RuleKind::Decision`]. The escape severs the per-site violation path in
    /// three layers, so it is honest only when no option is a fault. A
    /// [`RuleKind::DecisionWithFloor`] rule names faults by construction and therefore may not
    /// opt out of reporting them; a [`RuleKind::DefaultWithExceptions`] rule has an
    /// externally-correct default, so "no convention" is not an honest outcome; a
    /// [`RuleKind::Defect`] rule never enters the pass at all.
    ///
    /// Exhaustive on purpose, same reason as [`RuleKind::permits_violates_flag`].
    pub fn offers_no_convention_escape(&self) -> bool {
        match self {
            RuleKind::Defect => false,
            RuleKind::Decision => true,
            RuleKind::DefaultWithExceptions => false,
            RuleKind::DecisionWithFloor => false,
        }
    }
}

impl std::fmt::Display for RuleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Verification status / provenance (rule grounding ladder)
// ────────────────────────────────────────────────────────────────────────────

/// The provenance / verification ladder for a rule: how confident we are that
/// the rule corresponds to a real, authoritative external standard rather than
/// being something an AI merely *designed*.
///
/// This is the foundation for the grounding pass that maps every rule to its
/// authoritative source (a published style guide, security standard, or a real
/// established linter rule).
///
/// Ladder (strictness increasing):
/// - `draft`    — AI-generated / designed, NOT yet checked against any external
///   authority. There may be no [`RuleSource`] at all. **Not shippable** —
///   draft rules are kept out of the demo'd / armed ruleset.
/// - `policy`   — honest project policy, with NO external authority: the rule
///   encodes a deliberate project/team decision (e.g. an internal convention
///   or process), and any cited [`RuleSource`]s are internal docs, not a
///   published standard or real linter rule. `policy` is NOT grounded, but it
///   DOES arm: [`Rule::is_auto_recommended`] includes it, so a `policy` rule is
///   pre-checked and runs unattended exactly like a `grounded` one. What
///   `policy` says is that the rule is not mislabeled `grounded` by virtue of
///   merely having a source citation — a provenance statement, not an arming
///   one. (W17: this bullet also used to claim `policy` was "kept out of the
///   armed ruleset the same as `draft`"; false since the A8 fix. See
///   [`Verification::Policy`].) Use this instead of `grounded` whenever the only
///   "source" is a Camerata doc citing Camerata (or another purely internal
///   document) rather than a real external authority.
/// - `grounded` — mapped to a cited authoritative source or a real, established
///   linter rule: a URL + identifier is present in [`Rule::sources`].
///   Machine-grounded; an automated grounding pass may emit this.
/// - `verified` — a human (the maintainer) has confirmed the grounding is
///   correct. **Only a human sets this** — no automated process may ever emit
///   `verified`. It is the strongest assertion the corpus can make about a rule.
/// - `needs_recheck` — a rule that WAS `verified`, but the cited source or
///   linter it was verified against has since drifted (a version bump moved the
///   ground out from under the human confirmation). It is still at-least-grounded
///   and usable, but the human verification is stale and must be re-confirmed.
///   Emitted by the [`crate`]'s staleness pass (in `camerata-checks`), never set
///   by hand in a TOML file under normal authoring.
///
/// `serde(default)` resolves to [`Verification::Draft`], so every existing rule
/// TOML (which predates this field) and any rule that omits `verification`
/// loads as `Draft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// AI-generated / designed; not yet checked against any external authority.
    #[default]
    Draft,
    /// Honest project policy: a deliberate project/team decision with NO
    /// external authority. Any cited source is internal, not a published
    /// standard or real linter rule.
    ///
    /// NOT grounded ([`Verification::is_grounded`] and
    /// [`Verification::is_shippable`] both return `false`) — but it DOES arm.
    /// [`Rule::is_auto_recommended`] returns `true` for
    /// `Grounded | Verified | Policy` (see its body), so a `policy` rule is
    /// pre-checked in the onboarding proposal and IS selected by an unattended
    /// scan via `camerata_cli::inspect_cmd::curated_rule_selection`. Every corpus
    /// rule carrying this value arms (the count moves as rules gain outside
    /// sources; `req_w17_every_policy_rule_in_the_corpus_arms` enforces it).
    ///
    /// W17 corrected a stale claim here. This comment used to read "kept out of
    /// the armed ruleset like `draft`, but labeled honestly rather than as
    /// `grounded`", which has been FALSE since the A8 fix added `Policy` to the
    /// auto-recommended set. Believing it is what disarmed 17 shipped rules on
    /// every repo — `SPIRIT-FILE-SIZE-1` and every `PROC-*` governance rule
    /// among them — because provenance ("no external authority") was conflated
    /// with worth-running. `verification` no longer disarms a rule anywhere;
    /// `rules_disarmed_by_verification` now fails the BUILD on any value that
    /// would. The honest statement of what `Policy` costs a rule is a weaker
    /// CITATION story, not a weaker arming story.
    Policy,
    /// Mapped to a cited authoritative source or real linter rule (sources present).
    Grounded,
    /// A human maintainer has confirmed the grounding. Human-only; never automated.
    Verified,
    /// Was `verified`, but the cited source/linter has since drifted; the human
    /// confirmation is stale and needs re-checking. Still at-least-grounded and
    /// usable. Serialised as `needs_recheck`.
    NeedsRecheck,
}

impl Verification {
    /// The snake_case wire/TOML string for this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            Verification::Draft => "draft",
            Verification::Policy => "policy",
            Verification::Grounded => "grounded",
            Verification::Verified => "verified",
            Verification::NeedsRecheck => "needs_recheck",
        }
    }

    /// Parse a status from its snake_case wire/TOML string. Unknown → `None`.
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Verification::Draft),
            "policy" => Some(Verification::Policy),
            "grounded" => Some(Verification::Grounded),
            "verified" => Some(Verification::Verified),
            "needs_recheck" => Some(Verification::NeedsRecheck),
            _ => None,
        }
    }

    /// Whether this status is exactly `verified` — the strongest, human-only
    /// assertion. `NeedsRecheck` returns `false`: a drifted verification is no
    /// longer a live human confirmation.
    pub fn is_verified(&self) -> bool {
        matches!(self, Verification::Verified)
    }

    /// Whether this status is at-least-grounded — i.e. `grounded`, `verified`, or
    /// `needs_recheck`. All three are backed by a cited EXTERNAL source and are
    /// usable; `draft` and `policy` are not — `policy` is honest project policy
    /// with no external authority, so it is excluded here just like `draft`.
    pub fn is_grounded(&self) -> bool {
        matches!(
            self,
            Verification::Grounded | Verification::Verified | Verification::NeedsRecheck
        )
    }

    /// Whether a rule at this status is shippable — true for every at-least-grounded
    /// status (`grounded`, `verified`, `needs_recheck`), false for `draft` and `policy`.
    pub fn is_shippable(&self) -> bool {
        self.is_grounded()
    }
}

impl std::fmt::Display for Verification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One authoritative source backing a rule's grounding.
///
/// A source is what moves a rule off the `draft` rung of the ladder: it points
/// at the published standard or the real linter rule that the camerata rule
/// mirrors.
///
/// In TOML these are `[[sources]]` array-of-tables blocks:
///
/// ```toml
/// [[sources]]
/// url = "https://google.github.io/styleguide/javaguide.html#s4.8.3.1-for-each"
/// title = "Google Java Style Guide — Enhanced for statement"
/// linter = "Checkstyle: FinalLocalVariable"
///
/// [[sources]]
/// url = "https://errcheck.dev"
/// title = "errcheck — unchecked errors"
/// # linter omitted for a style-guide/doc-only source
/// relevance = "errcheck's own docs state the default is to report every unchecked error \
///              return, which is exactly the check this rule mirrors."
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RuleSource {
    /// Canonical URL of the source (style guide section, standard, linter docs).
    pub url: String,
    /// Human-readable title of the source.
    pub title: String,
    /// The enforcing tool + rule id when this source is a real linter rule
    /// (e.g. `"golangci-lint: errcheck"`, `"Checkstyle: FinalLocalVariable"`).
    /// `None` for a style-guide / documentation-only source that no tool enforces.
    #[serde(default)]
    pub linter: Option<String>,
    /// Author-facing provenance: what THIS source actually states, and how that
    /// bears on the rule. Absent → `None`, which is the normal case; 19 sources
    /// across 7 `go/testing/` rules carried one as of W18.
    ///
    /// # Why the field exists (W18)
    ///
    /// Rule authors were already writing this content — they just had nowhere to
    /// put it, so 19 paragraphs went into a `qualifies` key nested inside the
    /// `[[sources]]` block, where this struct had no field for them and serde
    /// dropped every one silently. The owner's ruling was to keep the content, so
    /// it got a field. See [`SOURCE_FIELDS`] for the full history and
    /// [`corpus_rules_with_qualifies_nested_in_sources`] for the gate that stops
    /// the old spelling returning.
    ///
    /// # Why `relevance` and not `qualifies`
    ///
    /// This is NOT the rule-level `qualifies`/[`Rule::summary`] and must never be
    /// folded into it. `qualifies` says *when the rule applies*; `relevance` says
    /// *why this citation supports it*. Reusing one name for both meanings at two
    /// nesting levels is what produced the silent drop in the first place.
    ///
    /// # Consumption
    ///
    /// Deliberately none, today. It is author/reviewer-facing provenance for
    /// re-verifying a citation without re-reading the source, not client-facing
    /// deliverable text; the shipped artifacts cap what they say about a rule, and
    /// a paragraph per citation is an internal dump by any measure. Declared
    /// accepted-and-unconsumed in that sense — but, unlike `layer`/`emits`, it is
    /// at least PARSED and reachable, so a future rule-detail page can render it
    /// without another corpus migration.
    #[serde(default)]
    pub relevance: Option<String>,
}

/// Provenance for a human verification: who verified the rule, when, and which
/// source / linter versions it was verified against.
///
/// This is the durable record behind a [`Verification::Verified`] status. The
/// `against` list captures the exact versions of the cited sources/linters at the
/// time of verification (e.g. `"clippy 1.83"`, `"Checkstyle 10.12"`). A later
/// staleness pass compares these against the *current* versions: if any cited
/// version has drifted, the verification is no longer trustworthy and the rule is
/// demoted to [`Verification::NeedsRecheck`].
///
/// In TOML this is a `[verified]` table:
///
/// ```toml
/// [verified]
/// by = "zach"
/// at = "2026-06-20"
/// against = ["clippy 1.83", "Google Java Style Guide 2024-01"]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VerifiedProvenance {
    /// Who confirmed the verification (a human identifier — name / handle).
    pub by: String,
    /// When it was verified (an ISO-8601 date/timestamp string; stored verbatim).
    pub at: String,
    /// The source/linter versions the rule was verified against. Each entry is a
    /// free-form `"<name> <version>"` string the staleness pass compares against a
    /// current-versions map. Empty when no versioned sources were pinned.
    #[serde(default)]
    pub against: Vec<String>,
}

// ────────────────────────────────────────────────────────────────────────────
// Raw TOML shape (private; only the fields we need)
// ────────────────────────────────────────────────────────────────────────────

/// The raw deserialization target for a principle TOML file.
///
/// Optional fields that the orchestrator does not use are intentionally
/// omitted; `serde(deny_unknown_fields)` is NOT set so future corpus fields
/// are silently ignored rather than breaking the loader.
#[derive(Debug, Deserialize)]
struct RuleToml {
    id: String,
    title: String,
    enforcement: EnforcementKind,
    /// Optional in-file domain field. When present, a warning is logged if it
    /// disagrees with the folder-derived value — the derived value always wins.
    #[serde(default)]
    domain: Option<String>,
    /// Optional short summary. We derive it from the `decision.why` field when
    /// `qualifies` is absent, or fall back to the title.
    #[serde(default)]
    qualifies: Option<String>,
    #[serde(default)]
    decision: Option<DecisionToml>,
    /// Whether this rule ships an adopted default option. When `false`, the
    /// architect MUST choose an alternative at onboarding.
    #[serde(default)]
    default: bool,
    /// Whether this rule is OPT-IN ONLY: a grounded rule that must NEVER be
    /// auto-recommended / pre-checked in the onboarding proposal, even when it is
    /// `grounded`/`verified` and stack-relevant. It still appears in the proposal
    /// list so the architect can deliberately opt in; it is just never pre-ticked.
    /// Absent → `false`.
    #[serde(default)]
    opt_in_only: bool,
    /// Whether this rule is LAYER-3 ONLY: a CI-tier rule that must never run at
    /// layer-2 or at scan time (too heavy / not locally runnable — e.g. CodeQL's
    /// whole-program DB build). Carried now; consumed by the runners + the
    /// scan-time preview (Part B). Absent → `false`.
    #[serde(default)]
    layer3_only: bool,
    /// The alternatives the architect chooses among (`[[option]]` blocks).
    #[serde(default, rename = "option")]
    options: Vec<OptionToml>,
    /// Provenance / verification status. Absent → [`Verification::Draft`], so
    /// every pre-existing rule TOML loads as `draft` (not yet grounded).
    #[serde(default)]
    verification: Verification,
    /// Authoritative sources backing this rule (`[[sources]]` blocks). Absent →
    /// empty.
    #[serde(default)]
    sources: Vec<RuleSource>,
    /// Verified provenance (`[verified]` table). Absent → `None`. Present only on
    /// rules a human has confirmed; carries the versions verified against so a
    /// staleness pass can demote on drift.
    #[serde(default)]
    verified: Option<VerifiedProvenance>,
    /// Stack exceptions (`[[stack_exception]]` blocks) — see [`StackException`]. Absent →
    /// empty (no exceptions; the rule applies uniformly regardless of detected stack).
    #[serde(default, rename = "stack_exception")]
    stack_exceptions: Vec<StackExceptionToml>,
    /// Additional domains this rule should ALSO be proposed/armed for, beyond its own
    /// folder-derived domain — see [`Rule::extra_domains`]. Absent → empty.
    #[serde(default)]
    extra_domains: Vec<String>,
    /// The three-way rule-kind classification (Pass-1 W2, A5.1) — see [`RuleKind`]'s doc
    /// comment. Deliberately `Option<RuleKind>` with NO `#[serde(default)]` resolving to a
    /// concrete variant: an absent `kind` must not silently default to something permissive
    /// (e.g. `Decision`, which would silently exempt an unclassified rule from ever emitting a
    /// `violates` finding). `None` here is the normal result of a bare `#[serde(default)]`
    /// clause (which must be present so a TOML lacking `kind` still parses instead of hard
    /// erroring the whole corpus load) — the loader does not reject it; the CORPUS-LOAD GATE
    /// does (`rules_missing_kind`, exercised by `req_a5_1_every_corpus_rule_declares_a_kind`),
    /// failing the BUILD rather than silently shipping an unclassified rule.
    #[serde(default)]
    kind: Option<RuleKind>,
    /// Whether this rule is a SCAN TARGET (W16) — see [`Rule::inspectable`]'s doc comment for
    /// the semantics. REQUIRED on every corpus rule, with the same lenient-parse /
    /// strict-gate split `kind` uses and for the same reason: a bare `#[serde(default)]` so a
    /// TOML lacking the key still parses (fixture corpora in other crates do not declare it)
    /// while [`rules_missing_inspectable`] fails the BUILD on any real corpus rule that omits
    /// it. There is deliberately NO value-level default resolving `None` to `true` at this
    /// layer: every silent default this schema has shipped produced an invisible bug, and a
    /// newly-authored rule must be forced to decide whether the scan can assess it.
    #[serde(default)]
    inspectable: Option<bool>,
    /// A ceiling on the severity this rule may ever export, e.g. `"high"` — see [`Rule::
    /// max_severity`]'s doc comment and E2 (`docs/plans/2026-10-09_audit-findings-tracker.md`).
    /// Free-form lowercase severity string (`"critical"` | `"high"` | `"medium"` | `"low"`),
    /// matching the vocabulary `Finding::severity`/`report_export::normalize_severity` already
    /// use elsewhere — NOT a new enum, for the same reason `Finding::severity` itself isn't
    /// one (every severity comparison in this codebase already goes through a normalizing
    /// string compare, not a typed enum). `None` (the overwhelming majority of rules) means no
    /// ceiling — the rule's severity is whatever the floor/calibration pipeline produces.
    #[serde(default)]
    max_severity: Option<String>,
    /// A FLOOR on the severity this rule may ever export, e.g. `"high"` — see
    /// [`Rule::min_severity`]'s doc comment and W5-Q2's ruling
    /// (`docs/plans/2026-10-09_audit-findings-tracker.md`: *"Floor, it's the minimum
    /// rating."*). Same free-form lowercase severity vocabulary as `max_severity`, for the
    /// same reason. `None` (the overwhelming majority of rules) means no floor — the rule's
    /// severity is whatever the calibration pipeline produces.
    #[serde(default)]
    min_severity: Option<String>,
    /// A5.2 (`docs/plans/2026-10-09_audit-findings-tracker.md`): the severity ceiling for a
    /// CONSISTENCY DEVIATION reported under this rule — see [`Rule::deviation_ceiling`]'s doc
    /// comment. Same free-form lowercase severity vocabulary as `max_severity`. `None` (the
    /// overwhelming majority) means the pipeline's default deviation ceiling (`"medium"`)
    /// applies.
    #[serde(default)]
    deviation_ceiling: Option<String>,
    /// W32 (report credibility): external vulnerability-taxonomy codes this rule maps to, e.g.
    /// `cwe = ["CWE-89"]`. Lets a finding print a recognised anchor the client's own security
    /// team already tracks. Sourced from the rule's `[[sources]]` where they already cite a CWE
    /// URL. Empty for a rule with no recognised code (most non-security rules). See [`Rule::cwe`].
    #[serde(default)]
    cwe: Vec<String>,
    /// W32: OWASP ASVS requirement ids this rule maps to, e.g. `asvs = ["V5.3.4"]`. Same role as
    /// [`RuleToml::cwe`] for the ASVS taxonomy; empty when none applies. See [`Rule::asvs`].
    #[serde(default)]
    asvs: Vec<String>,
    /// W32: the year the ecosystem moved off the pattern this rule describes, e.g.
    /// `superseded_since = "2021"`, so a finding can say "current guidance moved on in 2021".
    /// Only authored on pattern-level architecture rules whose default genuinely aged out; `None`
    /// for every current rule. Pairs with [`RuleToml::current_recommendation`]. See
    /// [`Rule::superseded_since`].
    #[serde(default)]
    superseded_since: Option<String>,
    /// W32: a pointer to the current guidance that replaced a superseded pattern — a URL or short
    /// phrase a finding can render next to [`RuleToml::superseded_since`]. `None` unless the rule
    /// is marked superseded. See [`Rule::current_recommendation`].
    #[serde(default)]
    current_recommendation: Option<String>,
    /// The rule's revision number: an integer >= 1, bumped whenever anything about the rule
    /// changes. REQUIRED on every corpus rule, with the same lenient-parse / strict-gate split
    /// `kind` and `inspectable` use: `Option<u32>` with a bare `#[serde(default)]` so a TOML
    /// lacking the key still parses (fixture corpora in other crates do not declare it), while
    /// [`rules_missing_version`] and [`rules_with_invalid_version`] fail the BUILD on the real
    /// corpus. The bump itself is made mechanical, not a convention, by the lock gate
    /// (`CORPUS_LOCK.toml`). See [`Rule::version`].
    #[serde(default)]
    version: Option<u32>,
    /// `"YYYY-MM-DD"`: the date this ONE rule was individually confirmed current, between full
    /// domain sweeps. Optional. See [`Rule::last_reviewed`].
    #[serde(default)]
    last_reviewed: Option<String>,
    /// Token-efficiency rank 7: which files this rule can apply to, so the scan sends it only
    /// with those (`applies_to = { paths = ["**/*.sql"], languages = ["rust"] }`). Absent means
    /// every file. Validated at load by `AppliesTo`'s `Deserialize`: an unknown language or key,
    /// or a glob form the matcher does not implement, fails the file's load. See [`Rule::applies_to`].
    #[serde(default)]
    applies_to: Option<AppliesTo>,
    /// W17 (gotcha 8, half resolved): `tag` is GONE. It was authored on all 417 corpus rules,
    /// read by nothing, and W14 declared it here as accepted-but-unconsumed so
    /// [`corpus_files_with_unknown_fields`] could treat every OTHER unrecognized rule-level key
    /// as a build failure. The owner ruled on 2026-10-12 that it is dead weight and must go, on
    /// three measured grounds: three values across 417 rules (`"stack"` 388, `"universal"` 28,
    /// `"process"` 1 — 93 percent one value); agreement with the folder-derived domain on only
    /// 21 of 417; and the 28 `"universal"` rules being exactly the `testing/` and `universal/`
    /// folders, already derivable from the path. Deleted from all 417 TOMLs and from this
    /// struct, so the unknown-field gate now REJECTS any reappearance rather than silently
    /// accepting it — see `req_w17_no_corpus_rule_carries_a_tag_field`.
    ///
    /// `layer` below deliberately stays: it is NOT the same shape (see its doc comment).
    ///
    /// W14 (gotcha 8): ACCEPTED BUT UNCONSUMED — authored on all 417 rules, read by nothing,
    /// established 2026-10-11 by a workspace-wide search (the only other TOML parsers are
    /// `checks/src/manifest.rs`, `checks/src/architecture_config.rs` and
    /// `server/src/feature_flags.rs`, none of which touch the principle corpus). It is declared
    /// here so it is part of the DOCUMENTED schema rather than an unknown field.
    ///
    /// Unlike the deleted `tag`, `layer` carries information that is NOT derivable from the
    /// folder path, so it is the more plausible latent feature of the two, and the owner has NOT
    /// ruled on it. Measured distribution on 2026-10-11:
    /// `universal` 131, `framework` 113, `language` 90, `platform` 35, `library` 34,
    /// `architecture` 7, `testing` 7. Eight folder families hold MORE THAN ONE layer value, so
    /// the field genuinely sub-divides a domain rather than restating it (e.g. `go/` splits into
    /// `architecture` x4 and `language` x7; `rust/` into `language` x9 and `library` x2;
    /// `go:grpc` into `framework` x4 and `library` x1). It is also internally inconsistent in
    /// at least two places, which is what a hand-authored, unread field drifts into:
    /// `go:testing` is `layer = "testing"` while every other `<lang>:testing` folder is
    /// `layer = "language"`, and `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` is `layer = "universal"`
    /// while its three `javascript/` siblings are `layer = "language"`. Note also that
    /// `layer = "universal"` does NOT mean `domain == "universal"` (131 vs 25) and does not
    /// match any existing domain constant (`PIPELINE_SELECTED_DOMAINS`, `ALWAYS_ON_DOMAINS`,
    /// `STANCE_LAYER_DOMAINS`), so nothing currently derived duplicates it.
    #[serde(default)]
    #[allow(dead_code)]
    layer: Option<String>,
    /// W14 (gotcha 8 / section 3.19): ACCEPTED BUT UNCONSUMED. The `[[emits]]` blocks
    /// (`target =`/`scope =` keys) of the deprecated camerata-ai emit-partition format, present
    /// on exactly 2 rules on 2026-10-11 (`rust/rust-domain-2-newtype-ids.toml`,
    /// `universal/spirit-robustness-1.toml`) and parsed by nothing — the live
    /// CONVENTIONS/AGENTS partitioning is done by `arm.rs` from `enforcement`. Typed as raw
    /// [`toml::Value`] deliberately: declaring a concrete shape would assert a contract this
    /// workspace does not honour, and would make a future key inside the block a parse error
    /// for a block nothing reads.
    #[serde(default, rename = "emits")]
    #[allow(dead_code)]
    emits: Vec<toml::Value>,
}

#[derive(Debug, Deserialize)]
struct StackExceptionToml {
    framework: String,
    path_glob: String,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize)]
struct DecisionToml {
    /// The decision this rule frames, asked of the client's code (e.g. "How does the API
    /// deduplicate retried mutations?"). See `docs/RULE_AUTHORING.md`, `[decision].question`.
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    why: Option<String>,
    /// The id of the default option, when one is adopted.
    #[serde(default)]
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OptionToml {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
    #[serde(default)]
    why: String,
    /// Authored client-facing remediation text (2-4 imperative sentences: what to change,
    /// WHERE, and HOW TO VERIFY it's closed). Distinct from `directive`, which is the
    /// detection/enforcement recipe an agent reads — `remediation` is what a client-facing
    /// report's "Fix:" line binds to. `None`/absent when not yet authored for this rule; the
    /// report NEVER falls back to `directive` when this is absent (see
    /// `report_export::resolve_fix`).
    #[serde(default)]
    remediation: Option<String>,
    /// Authored, client-facing HEADLINE for a DETERMINISTIC FLOOR finding evaluated against
    /// this option (P4 — floor findings get finding-level treatment). See
    /// [`RuleOption::finding_headline`] for the full contract.
    #[serde(default)]
    finding_headline: Option<String>,
    /// Authored, client-facing DETAIL for a DETERMINISTIC FLOOR finding evaluated against this
    /// option (P4), paired with `finding_headline`. See [`RuleOption::finding_detail`] for the
    /// full contract.
    #[serde(default)]
    finding_detail: Option<String>,
    /// Optional `escalation = { condition = "…", severity = "…" }` inline table: present when
    /// choosing this option calls for escalation. Absent → this option does not escalate.
    #[serde(default)]
    escalation: Option<EscalationSpec>,
    /// Authored remediation-EFFORT tier for a finding evaluated against this option: `"low"` |
    /// `"medium"` | `"high"`, the SAME three-tier vocabulary as `Finding::effort` (C4-P3). This
    /// is the rule author's own floor/default estimate for how long the fix takes — consulted
    /// ONLY when a finding's own calibrated `effort` is absent (a deterministic-floor or
    /// RLS-replay finding never goes through the AI calibration pass that sets `Finding::effort`
    /// per-finding, so without this a client-visible row would show "not estimated this run"
    /// forever, no matter how proven the defect). `None`/absent when not yet authored; the
    /// report then falls back to its pre-existing "not yet estimated" honest gap (see
    /// `report_export::resolve_effort`) rather than fabricating a number.
    #[serde(default)]
    effort: Option<String>,
    /// Pass-1 W2 (A5.4): whether CHOOSING this option describes the ABSENCE or a DEGENERATE
    /// FORM of the rule's own requirement — see [`RuleOption::violates`]'s doc comment. Absent
    /// → `false` (the overwhelming majority of options: a genuine, non-degenerate alternative).
    #[serde(default)]
    violates: bool,
}

// ────────────────────────────────────────────────────────────────────────────
// Public domain types
// ────────────────────────────────────────────────────────────────────────────

/// One alternative the architect can codify for a rule (a `[[option]]` block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOption {
    /// Stable option id (what gets codified as the choice).
    pub id: String,
    /// Human label.
    pub label: String,
    /// The concrete directive this alternative codifies.
    pub directive: String,
    /// Why this alternative (the rationale; the default option says so).
    pub why: String,
    /// Authored client-facing remediation text (2-4 imperative sentences: what to change,
    /// WHERE, and HOW TO VERIFY it's closed). `None` when not yet authored — the report's
    /// "Fix:" block is OMITTED entirely in that case, never backfilled from `directive`.
    /// May contain the same placeholder tokens `directive` uses (e.g. `<table>`,
    /// `<function-name>`, `<bucket>`, `<path>`); the report layer substitutes those from the
    /// finding at render time (see `report_export::resolve_fix`).
    pub remediation: Option<String>,
    /// Authored, client-facing HEADLINE for a DETERMINISTIC FLOOR finding evaluated against
    /// this option (P4 — floor findings get finding-level treatment: `docs/plans/
    /// 2026-09-29_codebase-inspection-hardening.md` §P4). A short, specific sentence naming
    /// what's wrong IN THIS REPO — never the gate's own "Deny…" enforcement phrasing
    /// (`camerata_gateway::RULE_REGISTRY`'s `description` is an internal directive the AGENT
    /// reads, not client prose; leaking it into a report is exactly the defect this field
    /// fixes). May contain the same placeholder tokens `remediation` does (`<path>`/`<file>`
    /// plus any rule-specific capture the detector populates, e.g. `<key-kind>`),
    /// instantiated the same way (`report_export::instantiate_remediation`) from the finding's
    /// own `path`/`captures`. `None` when not yet authored — the report falls back to its
    /// pre-existing `defect_headline`-over-`detail` derivation (see
    /// `report_export::resolve_floor_finding_text`), so an unauthored floor rule degrades to
    /// today's behavior rather than panicking or rendering an empty headline. Never consulted
    /// for an AI-tier finding — those invent their own rule id the corpus never sees.
    pub finding_headline: Option<String>,
    /// Authored, client-facing DETAIL for a DETERMINISTIC FLOOR finding, paired with
    /// `finding_headline` — authored together or not at all (`resolve_floor_finding_text`
    /// requires both non-blank before using either, so a rule can't render half an authored
    /// pair and half the gate-text fallback). Convention: a ONE-SENTENCE plain-language line
    /// first (what this means for a non-technical founder), then 1-2 sentences of
    /// what/where/impact detail — never the gate's enforcement phrasing. Same placeholder
    /// mechanism as `finding_headline`. `None` when not yet authored.
    pub finding_detail: Option<String>,
    /// Present when CHOOSING this option calls for escalation: it carries a condition the agent
    /// watches for + a severity. `None` means this option does NOT escalate (the agent proceeds, or
    /// the gate denies + bounces as normal). Option-scoped so a rule can offer an escalating option
    /// alongside non-escalating ones, and only the selected option's spec is active.
    pub escalation: Option<EscalationSpec>,
    /// Authored remediation-EFFORT tier (C4-P3): `"low"` | `"medium"` | `"high"`, the same
    /// vocabulary `Finding::effort` uses. This is the rule author's floor/default estimate,
    /// consulted by `report_export::resolve_effort` ONLY when a finding's own calibrated
    /// `effort` is absent — the deterministic floor and RLS-replay checkers never go through
    /// the AI calibration pass that sets `Finding::effort` per-finding, so without this a
    /// proven, client-visible defect would show "not estimated this run" forever. `None` when
    /// not yet authored for this option; the report then renders its pre-existing honest gap
    /// rather than fabricating a number.
    pub effort: Option<String>,
    /// Pass-1 W2 (A5.4, `docs/plans/2026-10-09_audit-findings-tracker.md`): `true` when
    /// CHOOSING this option describes the ABSENCE or a DEGENERATE FORM of the rule's own
    /// requirement — this option IS the defect, authored as an `[[option]]` only because the
    /// corpus convention pairs the rule's correct behavior with the behavior(s) it was
    /// deliberately NOT adopting (e.g. `ARCH-IDEMPOTENCY-KEYS-1`'s `"the-api-does-no-server-
    /// side-deduplication-at-all"` option). This is the load-bearing fix for the shipped
    /// defect: the alternatives pass (`ai_audit::audit_repo`) must NEVER rewrite the rule's
    /// effective directive to a `violates` option's directive, and must NEVER let a `violates`
    /// selection silence the rule — selecting one emits a finding at the rule's own citation,
    /// carrying its own remediation (via `Finding::evaluated_option_id`, resolved by
    /// `report_export::resolve_fix` exactly like any other multi-option finding), regardless of
    /// whether any chunk-level violation pass ALSO happens to flag the same site. `false`
    /// (the default — see `OptionToml::violates`) for the overwhelming majority of options: a
    /// genuine, non-degenerate alternative a project may legitimately adopt. A `decision`-kind
    /// rule ([`RuleKind::Decision`]) should never mark any option `true` — see that variant's
    /// doc comment.
    pub violates: bool,
}

/// How a rule's escalation condition is handled when an agent's work meets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationSeverity {
    /// Log a warning and let the run CONTINUE (advisory). For non-guard conditions.
    SoftFlag,
    /// STOP the run for human review (the AwaitingReview pause + checkpoint/resume). The default,
    /// and the only safe choice for guard-area conditions.
    #[default]
    HardPause,
}

/// Declares that a chosen OPTION carries a CONDITION that, when met by an agent's work, calls for
/// escalation. First-class (an `escalation` field on a `[[option]]`) so "does this choice escalate?"
/// is a queryable property rather than a prose/option-id guess — and so it is OPTION-SCOPED: a rule
/// may offer an escalating option AND non-escalating ones, and only the selected option's spec is
/// active. The governed agent is grounded with the escalation conditions of its selected options and
/// self-escalates when its work meets one; the deterministic backstop fires only when the selected
/// (or default) option carries a spec. A met `HardPause` condition pauses the run for human review;
/// a `SoftFlag` logs + continues.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EscalationSpec {
    /// The condition the agent watches for, in plain language (e.g. "the change would modify or
    /// delete an existing test"). This is what the agent is grounded with and self-reports against.
    pub condition: String,
    /// How a met condition is handled. Absent → `HardPause`.
    #[serde(default)]
    pub severity: EscalationSeverity,
}

/// A single camerata principle loaded from the corpus.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Stable, traceable rule id — mapped from [`camerata_core::RuleId`].
    pub id: RuleId,
    /// Short human-readable title.
    pub title: String,
    /// Enforcement tier for this rule.
    pub enforcement: EnforcementKind,
    /// Category domain derived from the corpus folder path
    /// (e.g. `"rust"`, `"agentic"`, `"universal"`).
    pub domain: String,
    /// A one-paragraph summary — sourced from `qualifies`, then
    /// `decision.why`, then `title` as a final fallback.
    pub summary: String,
    /// The decision this rule frames (`[decision].question`), when present. The architect
    /// reads this to understand WHAT they are choosing between.
    pub decision_question: Option<String>,
    /// The rationale for the adopted default (`[decision].why`), when present.
    pub decision_why: Option<String>,
    /// The alternatives the architect chooses among. May be empty (a mechanical
    /// rule with no variants).
    pub options: Vec<RuleOption>,
    /// The default option id, when this rule adopts one. `None` means the
    /// architect MUST choose an alternative — there is no default to fall back on.
    pub default_option: Option<String>,
    /// Provenance / verification status (the draft → grounded → verified ladder).
    /// Defaults to [`Verification::Draft`] for any rule that omits the field.
    pub verification: Verification,
    /// Authoritative sources backing this rule's grounding. Empty for a `draft`
    /// rule that has not yet been mapped to any external authority.
    pub sources: Vec<RuleSource>,
    /// Verified provenance, present only when a human has confirmed the rule
    /// (`[verified]` TOML table). Carries who/when + the source/linter versions
    /// verified against, used by the staleness pass to detect drift.
    pub verified: Option<VerifiedProvenance>,
    /// Whether this rule is OPT-IN ONLY: never auto-recommended / pre-checked,
    /// even when grounded and stack-relevant. The architect must opt in manually.
    /// See [`Rule::is_opt_in_only`]. Defaults to `false`.
    pub opt_in_only: bool,
    /// Whether this rule is LAYER-3 ONLY: a CI-tier rule that must never run at
    /// layer-2 or at scan time (too heavy / not locally runnable). Carried for the
    /// runners + scan-time preview. See [`Rule::is_layer3_only`]. Defaults to `false`.
    pub layer3_only: bool,
    /// Stack exceptions (`[[stack_exception]]` TOML blocks) — see [`StackException`]'s doc
    /// comment for the general mechanism. Empty for the overwhelming majority of rules, whose
    /// premise never conflicts with a platform-idiomatic pattern. See [`Rule::stack_exception_for`].
    pub stack_exceptions: Vec<StackException>,
    /// Additional domains this rule should ALSO be proposed/armed for, beyond its own
    /// folder-derived [`Rule::domain`] (`extra_domains = ["sql"]` in the TOML). `domain` is
    /// strictly one-per-rule (derived from the corpus folder path — see [`load_one`]), so a
    /// rule whose premise generalizes beyond its home folder (e.g. a `supabase/database-
    /// functions/` rule that is really about ANY Postgres `SECURITY DEFINER` function, not
    /// just a Supabase-detected repo) needs a second, explicit way to match a broader stack
    /// without moving the file (which would also move it out of its corpus family grouping).
    /// Empty for the overwhelming majority of rules. See `propose_corpus_rules`'s domain-match
    /// step, which treats `domain == d || extra_domains.contains(d)` as one match test.
    pub extra_domains: Vec<String>,
    /// The three-way rule-kind classification (Pass-1 W2, A5.1) — see [`RuleKind`]'s doc
    /// comment. `None` means this rule has not yet been classified; the corpus-load gate
    /// (`rules_missing_kind`) fails the build on any such rule in the shipped corpus — `None`
    /// is only ever seen on an in-progress/test-fixture rule, never a real corpus one at test
    /// time. See [`Rule::kind`] / [`Rule::is_defect_kind`] for the accessors.
    pub kind: Option<RuleKind>,
    /// W16: is this rule a SCAN TARGET, or an EMITTED DIRECTIVE ONLY?
    ///
    /// # Why this field exists
    ///
    /// Until W16 every selected rule was scanned, with no exclusion of any kind. That was
    /// right for the code rules and wrong for the ~23 rules whose subject is the AUTHORING
    /// PROCESS rather than the artifact (`ORCH-MODEL-TIERING-1`, `SPIRIT-ROBUSTNESS-1`,
    /// `PROC-CITE-CONVENTION-ID-1`, the `PROCESS-*` commit-metadata family, …). Those rules
    /// armed on every unattended scan, ran in the semantic pass, found nothing — correctly,
    /// since there is nothing in a file tree to find — and were then recorded
    /// `RuleLedgerEntry::verified_clean()`, because that predicate is
    /// `ran && findings_emitted == 0 && !not_applicable` and nothing marked them otherwise.
    /// A paid client deliverable could therefore open its "What's healthy" list with
    /// "`ORCH-MODEL-TIERING-1`: verified clean" — a claim about how WE run our agents,
    /// presented as a verified property of THEIR codebase.
    ///
    /// This NARROWS the W4 ruling ("every single rule should be included in the AI semantic
    /// scan"); it does not reverse it. W4 was right that the old `ORCH-`/`SPIRIT-`/`PROC-`
    /// prefix gate over-excluded — `SPIRIT-FILE-SIZE-1` is a genuine scan rule, and so are
    /// `PROC-PR-CONCURRENCY-1` and `PROC-HIDE-DEAD-END-1`. Every one of those families is
    /// MIXED, which is exactly why the distinction has to be per-rule and declared in the
    /// TOML, where it is visible to the rule's author, rather than a lexical check elsewhere
    /// in the engine acting at a distance.
    ///
    /// # Semantics
    ///
    /// - `Some(true)` — today's behavior. The rule is emitted AND scanned.
    /// - `Some(false)` — **emitted, never scanned.** The rule is still proposed, still
    ///   auto-recommended, and still installed verbatim into the client's `AGENTS.md` /
    ///   `CONVENTIONS.md` / `.camerata/rules.json` as a directive governing future work: the
    ///   EMIT path is completely untouched. It is removed from the scan BEFORE arming
    ///   (`camerata_server::onboard::inspectable_rules_for_scan`), so it never reaches the
    ///   semantic prompt, never enters the scan ledger, and can therefore never be
    ///   `verified_clean`, never appear in "What's healthy", and never be counted in
    ///   `provenance.audited_rule_ids` or the Coverage sheet's "we checked N rules" line.
    ///   Excluding it before arming rather than filtering its findings afterwards is
    ///   deliberate: a rule that armed and was then suppressed still pollutes the ledger's
    ///   accounting, which is the same shape as the defect being closed.
    /// - `None` — the rule did not declare the field. The corpus-load gate
    ///   ([`rules_missing_inspectable`]) fails the BUILD on this for any real corpus rule.
    ///   At RUNTIME, [`Rule::is_inspectable`] resolves it to `true` (scan it), because the
    ///   fail-safe direction for an undeclared rule is to keep looking, not to go quiet — and
    ///   because fixture corpora in other crates legitimately omit the key.
    pub inspectable: Option<bool>,
    /// A ceiling on the severity this rule may ever export (E2, `docs/plans/
    /// 2026-10-09_audit-findings-tracker.md`) — `report_export`'s severity pipeline clamps
    /// DOWN to this value whenever the finding's own (possibly floor-raised) severity would
    /// otherwise rank above it; it never RAISES a lower severity. `None` (the overwhelming
    /// majority of rules) means no ceiling. Free-form lowercase string in the same vocabulary
    /// `Finding::severity`/`report_export::normalize_severity` already use
    /// (`"critical"`/`"high"`/`"medium"`/`"low"`) — the clamp itself is applied in
    /// `camerata_server::report_export::clamp_to_rule_max_severity`, which reuses that crate's
    /// own `severity_rank`/`normalize_severity` rather than duplicating the vocabulary here.
    pub max_severity: Option<String>,
    /// A FLOOR on the severity this rule may ever export — the structured mirror of
    /// [`Rule::max_severity`], and the whole of workstream W8.
    ///
    /// # Why this field exists (W5-Q2, ruled 2026-10-11)
    ///
    /// Roughly two dozen corpus rules state a severity for their own failure mode in prose
    /// ("Flag as high severity: …", "emit a medium-severity finding", "Severity is High.").
    /// W5 deliberately did not transcribe any of them into [`Rule::max_severity`], because
    /// that phrasing is ambiguous between an upper bound and a minimum, and over-reach on a
    /// ceiling silently caps real findings with nothing to catch it. The owner settled it:
    ///
    /// > **"Floor, it's the minimum rating."**
    ///
    /// So "Flag as high severity: …" means **at least high**, and calibration may still raise
    /// it. Before this field the only floor in the engine was the D5
    /// `ai_audit::severity_calibration_floor`, which decides severity by scanning the MODEL'S
    /// ENGLISH PROSE against a phrase table (finding C7) — so these rules authored a minimum
    /// the pipeline could not read, which is finding C8 restated for floors.
    ///
    /// # Semantics
    ///
    /// `report_export`'s severity pipeline RAISES a finding's severity up to this value when
    /// the finding's own severity ranks below it; it never LOWERS a severity that already
    /// ranks at or above it. `None` (the overwhelming majority of rules) means no floor. Same
    /// free-form lowercase vocabulary as `max_severity`
    /// (`"critical"`/`"high"`/`"medium"`/`"low"`/`"info"`); the raise itself is applied in
    /// `camerata_server::report_export::raise_to_rule_min_severity`.
    ///
    /// Two asymmetries with the ceiling are deliberate and documented at the raise site:
    /// the floor applies only to an ACCUSING [`FindingKind`](camerata_api_types) (defect /
    /// deviation), never to an informational row and never to a row whose kind is absent; and
    /// it is applied BEFORE every downstream integrity downgrade and ceiling, so neither the
    /// null-location safety net nor `max_severity` can be defeated by an authored floor.
    pub min_severity: Option<String>,
    /// A5.2 (`docs/plans/2026-10-09_audit-findings-tracker.md`): the severity ceiling for a
    /// CONSISTENCY DEVIATION (`FindingKind::Deviation`) reported under this rule. Deviations
    /// cap at `"medium"` BY DEFAULT — the cost of inconsistency is forward drift on future
    /// features, not current risk — but a rule whose drift cost is genuinely high can author
    /// `deviation_ceiling = "high"` as the structured escape hatch (an authoring-time judgment
    /// about the rule's decision domain, never a prose-scanning runtime heuristic). `None`
    /// means the pipeline default (`"medium"`) applies; the clamp itself lives in
    /// `camerata_server::report_export::clamp_deviation_severity` and applies ONLY to rows
    /// whose kind is `Deviation` — defects under the same rule are untouched by this field.
    /// Same free-form lowercase severity vocabulary as `max_severity`, for the same reason.
    pub deviation_ceiling: Option<String>,
    /// W32 (report credibility): external vulnerability-taxonomy codes, e.g. `["CWE-89"]`. A
    /// finding renders these as a recognised anchor beside the headline. Empty for most rules;
    /// authored on security rules, sourced from their `[[sources]]` CWE citations.
    pub cwe: Vec<String>,
    /// W32: OWASP ASVS requirement ids, e.g. `["V5.3.4"]`. Empty when none applies.
    pub asvs: Vec<String>,
    /// W32: the year the pattern this rule describes was superseded, e.g. `"2021"`. `None` for
    /// every current rule; set only on pattern-level architecture rules whose default aged out.
    pub superseded_since: Option<String>,
    /// W32: pointer to the guidance that replaced a superseded pattern (URL or short phrase).
    /// `None` unless [`Rule::superseded_since`] is set.
    pub current_recommendation: Option<String>,
    /// The rule's revision number, an integer >= 1 bumped whenever anything about the rule
    /// changes. `None` only on a fixture / in-progress rule: [`rules_missing_version`] fails the
    /// build on any real corpus rule that omits it. The bump is enforced mechanically: the lock
    /// gate compares [`Rule::content_hash`] (which EXCLUDES this field) against
    /// `CORPUS_LOCK.toml` and fails a rule whose content changed while its version did not.
    pub version: Option<u32>,
    /// `"YYYY-MM-DD"`: when this one rule was individually confirmed current (a single-rule
    /// review between full domain sweeps). `None` when never individually reviewed. Together with
    /// the domain's `[confirmed]` watermark it gives the rule's effective review date (the later
    /// of the two, see [`effective_review_date`]). Excluded from [`Rule::content_hash`]: a review
    /// is not a change to the rule.
    pub last_reviewed: Option<String>,
    /// Token-efficiency rank 7: the files this rule can apply to. `None` (the default, and the
    /// state of every rule that has not been scoped) means every file, so the scan routes it
    /// exactly as before. When set, the scan sends the rule only with files that match one of
    /// the declared globs or are in one of the declared languages (a file whose language cannot
    /// be determined is sent too: routing must never cost a finding). Part of
    /// [`Rule::content_hash`], so scoping a rule is a revision of it.
    pub applies_to: Option<AppliesTo>,
}

/// P7 (`docs/plans/2026-09-29_codebase-inspection-hardening.md`): a GENERAL mechanism for a
/// rule whose premise conflicts with an idiomatic pattern of a DETECTED platform to be
/// suppressed for that stack, rather than firing a false violation. Declared per-rule in the
/// corpus TOML as `[[stack_exception]]` blocks — a rule can carry any number of them (one per
/// idiomatic pattern it needs to except), and any rule in the corpus can declare one; this is
/// not wired to any specific rule id.
///
/// Example (`fullstack/arch-monolith-first-1.toml`):
/// ```toml
/// [[stack_exception]]
/// framework = "Supabase"
/// path_glob = "supabase/functions/**"
/// note = "Supabase Edge Functions are the idiomatic second deployable for webhooks on this stack — not a monolith-topology violation."
/// ```
///
/// Both conditions must hold for the exception to apply to a given finding: the DETECTED
/// stack (`RepoStack::frameworks` in `camerata-server`) must contain `framework`, AND the
/// finding's file path must match `path_glob` (see [`glob_match`]). A rule with no
/// `stack_exceptions` behaves exactly as before this field existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackException {
    /// The framework/language marker (matched against the detected stack's framework list,
    /// e.g. `"Supabase"`, `"Next.js"`) that must be present for this exception to apply.
    pub framework: String,
    /// A glob pattern (`*` matches any run of characters, including `/`; see [`glob_match`])
    /// matched against a finding's file path. Both `framework` present AND a path match are
    /// required.
    pub path_glob: String,
    /// Why this is idiomatic for the stack — internal documentation for whoever authors or
    /// reviews the exception; not currently rendered client-facing.
    pub note: String,
}

/// A minimal, dependency-free glob matcher: `*` matches any run of characters (zero or more),
/// including `/` — so a single `*` behaves like `**` in more featureful glob dialects. This is
/// deliberately simple (classic recursive wildcard matching, no character classes, no `?`)
/// because [`StackException::path_glob`] only ever needs "does this path sit under this
/// idiomatic-pattern directory" style patterns (e.g. `"supabase/functions/**"`,
/// `"app/api/**/route.ts"`) — not a full glob grammar.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn helper(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => {
                // Collapse consecutive `*` (equivalent to one) then try consuming
                // 0..=all of the remaining text before it.
                let mut rest = p;
                while rest.first() == Some(&b'*') {
                    rest = &rest[1..];
                }
                (0..=t.len()).any(|i| helper(rest, &t[i..]))
            }
            Some(pc) => match t.first() {
                Some(tc) if pc == tc => helper(&p[1..], &t[1..]),
                _ => false,
            },
        }
    }
    helper(pattern.as_bytes(), text.as_bytes())
}

impl Rule {
    /// The option this rule resolves to given the project's `chosen_option` (or its default when
    /// none is chosen). `None` when neither is present (the architect must choose and hasn't).
    pub fn resolved_option(&self, chosen_option: Option<&str>) -> Option<&RuleOption> {
        let target = chosen_option.or(self.default_option.as_deref())?;
        self.options.iter().find(|o| o.id == target)
    }

    /// The first [`StackException`] on this rule whose `framework` is present in
    /// `detected_frameworks` AND whose `path_glob` matches `path` — `None` when no exception
    /// applies (the common case). The caller (`camerata-server`'s finding-filtering pass)
    /// treats a `Some` result as "suppress this finding: it names an idiomatic platform
    /// pattern for the detected stack, not a violation."
    pub fn stack_exception_for<S: AsRef<str>>(
        &self,
        detected_frameworks: &[S],
        path: &str,
    ) -> Option<&StackException> {
        self.stack_exceptions.iter().find(|se| {
            detected_frameworks
                .iter()
                .any(|f| f.as_ref().eq_ignore_ascii_case(&se.framework))
                && glob_match(&se.path_glob, path)
        })
    }

    /// The ACTIVE escalation spec for this rule given the project's `chosen_option`: the SELECTED
    /// (or default) option's `escalation`, if it carries one. This is the single source of truth for
    /// "should this rule escalate, given what was selected" — used by BOTH the agent grounding and
    /// the deterministic backstop, so selecting a non-escalating option correctly disables it.
    pub fn selected_escalation(&self, chosen_option: Option<&str>) -> Option<&EscalationSpec> {
        self.resolved_option(chosen_option)?.escalation.as_ref()
    }

    /// Whether ANY option on this rule can escalate (for listing / "is this an escalation rule"),
    /// independent of what's selected. Activation still depends on the selected option.
    pub fn has_escalating_option(&self) -> bool {
        self.options.iter().any(|o| o.escalation.is_some())
    }

    /// Whether this rule is OPT-IN ONLY — a grounded rule that must NEVER be
    /// auto-recommended / pre-checked in the onboarding proposal. It is still
    /// listed (so the architect can opt in), just never pre-ticked. The propose
    /// logic in the server gates auto-recommend on `!is_opt_in_only()`.
    pub fn is_opt_in_only(&self) -> bool {
        self.opt_in_only
    }

    /// Whether this rule is LAYER-3 ONLY — a CI-tier rule that must never run at
    /// layer-2 or at scan time. Consumed by the runners + the scan-time preview
    /// (Part B); carried here so the metadata is available.
    pub fn is_layer3_only(&self) -> bool {
        self.layer3_only
    }

    /// This rule's [`RuleKind`] classification (Pass-1 W2, A5.1). `None` means unclassified —
    /// the corpus-load gate (`rules_missing_kind`) fails the build on any such rule in the
    /// shipped corpus, so a `None` here at scan time can only come from a test fixture, never
    /// a real corpus rule.
    pub fn kind(&self) -> Option<RuleKind> {
        self.kind
    }

    /// Whether this rule is [`RuleKind::Defect`] — "no acceptable alternative anywhere". A
    /// `defect`-kind rule must NEVER enter the alternatives pass (A5.4): there is nothing to
    /// decide among, so it is excluded from [`crate`]-external callers' "is this a multi-option
    /// semantic rule eligible for the alternatives pass" predicate (`camerata_server::onboard::
    /// audit::is_semantic_multi_option_rule`) even when it happens to carry `>= 2` options.
    /// `false` for an unclassified (`kind == None`) rule — conservative: a rule with no kind
    /// recorded is never treated as "definitely no alternative", it simply isn't eligible to
    /// make that claim yet (the corpus-load gate is what actually prevents this case from
    /// reaching a real scan).
    pub fn is_defect_kind(&self) -> bool {
        matches!(self.kind, Some(RuleKind::Defect))
    }

    /// W16: whether this rule is a SCAN TARGET — see [`Rule::inspectable`]'s doc comment for
    /// the full semantics and the defect it closes.
    ///
    /// `false` means EMITTED, NEVER SCANNED: the rule is still proposed, armed and installed
    /// into the client's `AGENTS.md`/`CONVENTIONS.md`, but is removed from the scan before
    /// arming, so no scan surface may claim anything about it.
    ///
    /// An UNDECLARED rule (`None`) resolves to `true` here. That is the fail-safe direction —
    /// keep looking rather than go quiet — and it is only ever reachable from a test fixture
    /// corpus, since [`rules_missing_inspectable`] fails the build on any real corpus rule
    /// that omits the field.
    pub fn is_inspectable(&self) -> bool {
        self.inspectable.unwrap_or(true)
    }
}

impl Rule {
    /// Whether this rule has an adopted default option.
    pub fn has_default(&self) -> bool {
        self.default_option.is_some()
    }

    /// This rule's provenance / verification status.
    pub fn verification(&self) -> Verification {
        self.verification
    }

    /// Whether this rule is exactly `verified` (a live human confirmation).
    /// `needs_recheck` returns `false` — a drifted verification is stale.
    pub fn is_verified(&self) -> bool {
        self.verification.is_verified()
    }

    /// Whether this rule is at-least-grounded — `grounded`, `verified`, or
    /// `needs_recheck`. Delegates to [`Verification::is_grounded`].
    pub fn is_grounded(&self) -> bool {
        self.verification.is_grounded()
    }

    /// Whether this rule is shippable — true for every at-least-grounded status
    /// (`grounded`, `verified`, `needs_recheck`), false for `draft` AND `policy`.
    ///
    /// W17: this doc comment used to say "false for `draft`" and "Used to keep
    /// `draft` ... out of the demo'd / armed ruleset". Both halves were wrong.
    /// It is also false for `policy` (delegates to
    /// [`Verification::is_shippable`] = [`Verification::is_grounded`]), and it
    /// gates NOTHING: a workspace-wide search finds zero production callers of
    /// `Rule::is_shippable` outside this crate's own tests. Arming is
    /// [`Rule::is_auto_recommended`]'s job and nothing else's. Reading this
    /// method as an arming control is precisely the mistake that disarmed 17
    /// shipped rules (A8).
    pub fn is_shippable(&self) -> bool {
        self.verification.is_shippable()
    }

    /// Whether this rule is AUTO-RECOMMENDED in the onboarding scan PROPOSAL.
    ///
    /// A rule is auto-recommended (pre-checked / pre-selected) iff it is `Grounded`,
    /// `Verified`, or `Policy`. `Draft` and `NeedsRecheck` rules are still *listed* in the
    /// proposal so the architect can review and opt in, but they are NOT pre-checked —
    /// `Draft` because it is unreviewed AI output carrying unreduced AI risk, `NeedsRecheck`
    /// because its human confirmation is known-stale and wants re-confirming.
    ///
    /// The distinction matters for the UI: `is_auto_recommended = true` means the
    /// checkbox is ticked by default; `false` means the row is visible but the
    /// checkbox is unchecked. It also decides which rules an UNATTENDED scan runs at all
    /// (`camerata_cli::inspect_cmd::curated_rule_selection` selects exactly the
    /// auto-recommended set), which is why the set has to be right and not merely tidy.
    ///
    /// # Why `Policy` belongs here (Pass-1 W4, finding A8)
    ///
    /// [`Verification::Policy`] is a statement about PROVENANCE — "this rule encodes a
    /// deliberate project decision and cites no external authority" — and nothing at all
    /// about whether the rule is correct, checkable, or worth running. Excluding it from
    /// auto-recommendation conflated the two and silently disarmed 17 shipped rules on every
    /// repo, among them `SPIRIT-FILE-SIZE-1` (A8) and every `PROC-*` governance rule the
    /// product is explicitly about. A rule that can never arm for ANY repo is a defect under
    /// requirement H1 regardless of how honest its provenance label is, and
    /// `rules_disarmed_by_verification` now fails the build on exactly that condition.
    /// `is_grounded`/`is_shippable` deliberately still return `false` for `Policy`: those
    /// answer the provenance question, and that answer has not changed.
    pub fn is_auto_recommended(&self) -> bool {
        matches!(
            self.verification,
            Verification::Grounded | Verification::Verified | Verification::Policy
        )
    }
}

impl Rule {
    /// Convenience: the string form of the rule id.
    pub fn id_str(&self) -> &str {
        &self.id.0
    }
}

// ────────────────────────────────────────────────────────────────────────────
// RuleSet
// ────────────────────────────────────────────────────────────────────────────

/// All loaded rules, indexed for fast lookup by id and by domain.
#[derive(Debug, Default, Clone)]
pub struct RuleSet {
    /// Ordered list preserving load order (stable iteration).
    rules: Vec<Rule>,
    /// Fast lookup: rule id string → index into `rules`.
    by_id: HashMap<String, usize>,
    /// Fast lookup: domain string → list of indices into `rules`.
    by_domain: HashMap<String, Vec<usize>>,
}

impl RuleSet {
    /// Number of rules in this set.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Look up a rule by its id string (e.g. `"RUST-DOMAIN-4"`).
    pub fn get_by_id(&self, id: &str) -> Option<&Rule> {
        self.by_id.get(id).map(|&i| &self.rules[i])
    }

    /// All rules whose `domain` field matches `domain` exactly.
    ///
    /// Note: the corpus uses `"universal"` for universal rules (derived from the
    /// `universal/` corpus folder). Pass `"universal"` to retrieve them, or use
    /// [`select_for_domains`] to include universals automatically.
    pub fn get_by_domain(&self, domain: &str) -> Vec<&Rule> {
        match self.by_domain.get(domain) {
            Some(indices) => indices.iter().map(|&i| &self.rules[i]).collect(),
            None => vec![],
        }
    }

    /// Iterate every rule in load order.
    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter()
    }

    /// All distinct domain strings present in the set.
    pub fn domains(&self) -> impl Iterator<Item = &str> {
        self.by_domain.keys().map(String::as_str)
    }

    /// The distinct TOP-LEVEL domains (the part before any `:`, so `rust:seaorm` is `rust`),
    /// sorted. This is the pack granularity the corpus-freshness watermark tracks.
    pub fn top_level_domains(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .domains()
            .map(|d| d.split(':').next().unwrap_or(d).to_string())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Add a rule into this set, indexing it by id and domain. Public so downstream crates
    /// (e.g. `camerata-server`'s tests) can build a synthetic, multi-rule [`RuleSet`] —
    /// `ruleset_with_unauthored_rule` only ever builds one fixed rule shape; this is the
    /// general escape hatch for a test that needs a rule with arbitrary fields (e.g. a
    /// `stack_exceptions` list) without going through the async file-based corpus loader.
    pub fn push(&mut self, rule: Rule) {
        let idx = self.rules.len();
        self.by_domain
            .entry(rule.domain.clone())
            .or_default()
            .push(idx);
        self.by_id.insert(rule.id.0.clone(), idx);
        self.rules.push(rule);
    }
}

/// Test-support constructor: a minimal, otherwise-empty [`Rule`] with only `id` and `domain`
/// set (no options, no sources, `draft` verification, no stack exceptions) — for a test that
/// needs a real `Rule` value to mutate (e.g. push a [`StackException`] onto `stack_exceptions`)
/// without hand-spelling every field of the struct. Combine with [`RuleSet::push`] to build a
/// synthetic multi-rule set: `let mut set = RuleSet::default(); set.push(bare_rule(id, domain));`.
///
/// `#[doc(hidden)]`: this is test scaffolding, not a production API — production code always
/// goes through [`load_corpus`] / [`load_corpus_lenient`] against the real bundled corpus.
#[doc(hidden)]
pub fn bare_rule(id: &str, domain: &str) -> Rule {
    Rule {
        id: RuleId(id.to_owned()),
        title: format!("Test-fixture rule {id}"),
        enforcement: EnforcementKind::Structured,
        domain: domain.to_owned(),
        summary: format!("Synthetic test-only rule ({id})."),
        decision_question: None,
        decision_why: None,
        options: Vec::new(),
        default_option: None,
        verification: Verification::Draft,
        sources: Vec::new(),
        verified: None,
        opt_in_only: false,
        layer3_only: false,
        stack_exceptions: Vec::new(),
        extra_domains: Vec::new(),
        kind: None,
        inspectable: None,
        max_severity: None,
        min_severity: None,
        deviation_ceiling: None,
        cwe: Vec::new(),
        asvs: Vec::new(),
        superseded_since: None,
        current_recommendation: None,
        version: None,
        last_reviewed: None,
        applies_to: None,
    }
}

/// Test-support constructor: build a single-rule [`RuleSet`] whose one [`Rule`] has `rule_id`,
/// a real non-empty `directive` on its (default) option, and `remediation: None` on that same
/// option — i.e. a rule with a real, resolvable default option but NO authored client-facing
/// remediation.
///
/// Exists so callers that need to exercise the "remediation is unauthored" fail-safe
/// (`report_export::resolve_fix` in `camerata-server` must OMIT the Fix block rather than
/// falling back to `directive` when `remediation` is absent) are not coupled to the real
/// corpus's authoring state. As of the corpus-wide remediation-authoring passes, every real
/// corpus rule's default option now HAS authored remediation, so no real rule id can
/// demonstrate the unauthored path anymore — a test that hardcodes a real rule id to prove
/// this behavior breaks every time authoring coverage grows. This constructor decouples that
/// assertion from authoring state permanently.
///
/// `#[doc(hidden)]`: this is test scaffolding, not a production API — production code always
/// goes through [`load_corpus`] / [`load_corpus_lenient`] against the real bundled corpus.
#[doc(hidden)]
pub fn ruleset_with_unauthored_rule(rule_id: &str) -> RuleSet {
    const OPTION_ID: &str = "default";
    let option = RuleOption {
        id: OPTION_ID.to_owned(),
        label: "Default".to_owned(),
        directive: format!("Test-fixture detection directive for {rule_id}."),
        why: "Test-fixture rationale.".to_owned(),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    };
    let rule = Rule {
        id: RuleId(rule_id.to_owned()),
        title: format!("Test-fixture rule {rule_id}"),
        enforcement: EnforcementKind::Mechanical,
        domain: "test-fixture".to_owned(),
        summary: format!("Synthetic test-only rule ({rule_id}) with an unauthored remediation."),
        decision_question: None,
        decision_why: None,
        options: vec![option],
        default_option: Some(OPTION_ID.to_owned()),
        verification: Verification::Draft,
        sources: Vec::new(),
        verified: None,
        opt_in_only: false,
        layer3_only: false,
        stack_exceptions: Vec::new(),
        extra_domains: Vec::new(),
        kind: None,
        inspectable: None,
        max_severity: None,
        min_severity: None,
        deviation_ceiling: None,
        cwe: Vec::new(),
        asvs: Vec::new(),
        superseded_since: None,
        current_recommendation: None,
        version: None,
        last_reviewed: None,
        applies_to: None,
    };
    let mut set = RuleSet::default();
    set.push(rule);
    set
}

// ────────────────────────────────────────────────────────────────────────────
// Loader (async I/O — RUST-DOMAIN-5)
// ────────────────────────────────────────────────────────────────────────────

/// Default corpus path: the rule TOML bundled IN this repo, under
/// `crates/rules/principles/`. Resolved from the crate's manifest dir so it works from
/// any working directory and the repo is self-contained (no external checkout needed).
/// Override at runtime with `CAMERATA_CORPUS_PATH` (see [`corpus_path`]).
pub const DEFAULT_CORPUS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/principles");

/// The corpus directory to load: the `CAMERATA_CORPUS_PATH` env override if set and
/// non-empty, else the bundled [`DEFAULT_CORPUS_PATH`].
pub fn corpus_path() -> std::path::PathBuf {
    corpus_path_from(std::env::var("CAMERATA_CORPUS_PATH").ok())
}

/// [`corpus_path`] given the override's value: a non-blank override wins, anything else is the
/// bundled corpus. Split out so it is testable without mutating the process environment.
fn corpus_path_from(override_value: Option<String>) -> std::path::PathBuf {
    override_value
        .filter(|p| !p.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_CORPUS_PATH))
}

/// Load the full rule corpus from `corpus_dir`, walking it recursively.
///
/// Files that fail to parse as valid TOML rule files are returned as errors;
/// the caller may choose to collect-and-continue or fail-fast.
///
/// Files whose names do not end in `.toml` are silently ignored.
pub async fn load_corpus(corpus_dir: &Path) -> Result<RuleSet, RulesError> {
    let paths = collect_toml_paths(corpus_dir).await?;
    let mut set = RuleSet::default();
    for path in paths {
        let rule = load_one(&path, corpus_dir).await?;
        set.push(rule);
    }
    Ok(set)
}

/// Load the corpus and silently skip any file that fails to parse.
///
/// Returns the successfully loaded [`RuleSet`] and a list of (path, error)
/// pairs for files that were skipped. Useful when the corpus is evolving and
/// some files may temporarily be malformed.
pub async fn load_corpus_lenient(corpus_dir: &Path) -> (RuleSet, Vec<(PathBuf, RulesError)>) {
    let paths = match collect_toml_paths(corpus_dir).await {
        Ok(p) => p,
        Err(e) => return (RuleSet::default(), vec![(corpus_dir.to_path_buf(), e)]),
    };

    let mut set = RuleSet::default();
    let mut errors = Vec::new();

    for path in paths {
        match load_one(&path, corpus_dir).await {
            Ok(rule) => set.push(rule),
            Err(e) => errors.push((path, e)),
        }
    }

    (set, errors)
}

/// Walk `corpus_dir` recursively and collect all `.toml` file paths.
async fn collect_toml_paths(corpus_dir: &Path) -> Result<Vec<PathBuf>, RulesError> {
    // Use a sync walkdir wrapped in spawn_blocking so we stay async-safe.
    let dir = corpus_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut paths = Vec::new();
        collect_toml_paths_sync(&dir, &mut paths)?;
        // Sort for deterministic load order across platforms.
        paths.sort();
        Ok(paths)
    })
    .await
    .unwrap_or_else(|join_err| {
        Err(RulesError::Io {
            path: corpus_dir.to_path_buf(),
            source: std::io::Error::other(join_err.to_string()),
        })
    })
}

fn collect_toml_paths_sync(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), RulesError> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| RulesError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|e| RulesError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_toml_paths_sync(&path, out)?;
        } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse a single TOML file into a [`Rule`].
///
/// `corpus_dir` is the root of the corpus; `path` must be inside it.
/// The `domain` field on the resulting [`Rule`] is derived from the TOML
/// file's parent folder path relative to `corpus_dir` — folder components
/// are joined with `:` (e.g. `corpus_dir/rust/dioxus/x.toml` → `"rust:dioxus"`).
/// An in-file `domain` field is optional; when present it is compared against
/// the derived value and a warning is logged on disagreement.
async fn load_one(path: &Path, corpus_dir: &Path) -> Result<Rule, RulesError> {
    let bytes = tokio::fs::read(path).await.map_err(|e| RulesError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let text = String::from_utf8_lossy(&bytes).into_owned();
    let raw: RuleToml = toml::from_str(&text).map_err(|e| RulesError::TomlParse {
        path: path.to_path_buf(),
        source: e,
    })?;

    if raw.id.is_empty() {
        return Err(RulesError::MissingField {
            path: path.to_path_buf(),
            field: "id",
        });
    }

    // Derive the category domain from the TOML's folder path relative to corpus_dir.
    // e.g. corpus_dir/rust/dioxus/x.toml → "rust:dioxus"
    //      corpus_dir/universal/x.toml   → "universal"
    let derived_domain: String = {
        let parent = path.parent().unwrap_or(corpus_dir);
        let rel = parent.strip_prefix(corpus_dir).unwrap_or(parent);
        let components: Vec<&str> = rel
            .components()
            .filter_map(|c| {
                if let std::path::Component::Normal(s) = c {
                    s.to_str()
                } else {
                    None
                }
            })
            .collect();
        if components.is_empty() {
            // File is directly in corpus_dir (no subfolder) — should not happen
            // in a well-formed corpus, but fall back gracefully.
            "universal".to_string()
        } else {
            components.join(":")
        }
    };

    // If the TOML specifies an explicit domain, warn if it disagrees with the
    // derived value (this catches stale/mismatched fields). The derived value
    // always wins.
    if let Some(ref explicit) = raw.domain {
        let normalised = if explicit == "*" {
            "universal"
        } else {
            explicit.as_str()
        };
        if normalised != derived_domain {
            tracing::warn!(
                path = %path.display(),
                explicit = %explicit,
                derived = %derived_domain,
                "corpus rule TOML domain field disagrees with folder path — derived value wins"
            );
        }
    }

    // Derive summary: qualifies > decision.why > title.
    let summary = raw
        .qualifies
        .filter(|s| !s.is_empty())
        .or_else(|| {
            raw.decision
                .as_ref()
                .and_then(|d| d.why.as_deref())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
        })
        .unwrap_or_else(|| raw.title.clone());

    // The default option id, only when the rule adopts one (`default = true`).
    let default_option = if raw.default {
        raw.decision
            .as_ref()
            .and_then(|d| d.default.clone())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    // The decision context the architect reads in the rule-detail view: the question being
    // decided and the rationale for the adopted default.
    let decision_question = raw
        .decision
        .as_ref()
        .and_then(|d| d.question.clone())
        .filter(|s| !s.is_empty());
    let decision_why = raw
        .decision
        .as_ref()
        .and_then(|d| d.why.clone())
        .filter(|s| !s.is_empty());

    let options = raw
        .options
        .into_iter()
        .map(|o| RuleOption {
            id: o.id,
            label: o.label,
            directive: o.directive,
            why: o.why,
            remediation: o.remediation.filter(|s| !s.trim().is_empty()),
            finding_headline: o.finding_headline.filter(|s| !s.trim().is_empty()),
            finding_detail: o.finding_detail.filter(|s| !s.trim().is_empty()),
            escalation: o.escalation,
            effort: o.effort.filter(|s| !s.trim().is_empty()),
            violates: o.violates,
        })
        .collect();

    let stack_exceptions = raw
        .stack_exceptions
        .into_iter()
        .map(|se| StackException {
            framework: se.framework,
            path_glob: se.path_glob,
            note: se.note,
        })
        .collect();

    Ok(Rule {
        id: RuleId(raw.id),
        title: raw.title,
        enforcement: raw.enforcement,
        domain: derived_domain,
        summary,
        decision_question,
        decision_why,
        options,
        default_option,
        verification: raw.verification,
        sources: raw.sources,
        verified: raw.verified,
        opt_in_only: raw.opt_in_only,
        layer3_only: raw.layer3_only,
        stack_exceptions,
        extra_domains: raw.extra_domains,
        kind: raw.kind,
        inspectable: raw.inspectable,
        max_severity: raw.max_severity.filter(|s| !s.trim().is_empty()),
        min_severity: raw.min_severity.filter(|s| !s.trim().is_empty()),
        deviation_ceiling: raw.deviation_ceiling.filter(|s| !s.trim().is_empty()),
        cwe: raw.cwe,
        asvs: raw.asvs,
        superseded_since: raw.superseded_since.filter(|s| !s.trim().is_empty()),
        current_recommendation: raw.current_recommendation.filter(|s| !s.trim().is_empty()),
        version: raw.version,
        last_reviewed: raw.last_reviewed.filter(|s| !s.trim().is_empty()),
        applies_to: raw.applies_to,
    })
}

// ────────────────────────────────────────────────────────────────────────────
// Corpus-load gate (Pass-1 W2, item 4 — `docs/plans/2026-10-09_audit-findings-tracker.md`)
// ────────────────────────────────────────────────────────────────────────────
//
// Mirrors the EXISTING precedent `camerata_server::mechanical_gate` set for the W1-item-4
// "declares mechanical enforcement but has no wired detector" defect: a `#[cfg(test)]`-gated
// (or CI-run) build-time test, not a runtime refusal — a scan must never refuse to run just
// because a corpus rule is unclassified; see `rules_missing_kind`'s doc comment for why the
// TOML-parse layer itself stays lenient (`RuleToml::kind: Option<RuleKind>` with no concrete
// default) while THIS layer is the one that actually enforces the invariant, by failing the
// BUILD.

/// Every rule in `set` with NO `kind` classification at all (`Rule::kind() == None`). Sorted
/// for stable, readable test output. A1 (`req_a5_1_every_corpus_rule_declares_a_kind`) asserts
/// this is EMPTY for the real shipped corpus — any rule found here means someone added or
/// edited a corpus TOML without classifying it, which the build must catch rather than let the
/// rule silently default to "safe-looking" behavior at scan time.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_missing_kind(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = set
        .iter()
        .filter(|r| r.kind.is_none())
        .map(|r| r.id.0.clone())
        .collect();
    offenders.sort();
    offenders
}

/// W16. Every rule in `set` that does NOT declare [`Rule::inspectable`] at all, as a sorted
/// list of ids. `req_w16_every_corpus_rule_declares_inspectable` asserts this is EMPTY for the
/// real shipped corpus.
///
/// Same build-time-gate philosophy, and the same lenient-parse/strict-gate split, as
/// [`rules_missing_kind`]: `RuleToml::inspectable` is `Option<bool>` with a bare
/// `#[serde(default)]` so a TOML lacking the key still parses (and so fixture corpora in other
/// crates, which do not declare it, keep working), while THIS function is what actually
/// enforces the invariant — by failing the BUILD, never by refusing a scan at runtime
/// (`docs/RULES_REFERENCE.md` section 10.6).
///
/// The field is REQUIRED rather than defaulted because of what every silent default in this
/// schema has already cost. `verification` defaulting to `draft` silently disarmed
/// `UI-UTC-DATES-1` on every repo (finding A7); an absent `kind` would silently exempt a rule
/// from ever emitting a `violates` finding (A5.1). "Can the scan assess this rule from a file
/// tree?" is a judgment only the rule's author can make, and the cost of guessing it wrongly
/// in either direction is a wrong claim in a paid deliverable: guess `true` and an
/// unassessable rule ships as "verified clean"; guess `false` and a real check silently stops
/// running. So a new rule is forced to answer.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_missing_inspectable(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = set
        .iter()
        .filter(|r| r.inspectable.is_none())
        .map(|r| r.id.0.clone())
        .collect();
    offenders.sort();
    offenders
}

/// W16 VACUITY GUARD for [`rules_missing_inspectable`]: every rule in `set` that declares
/// `inspectable = false`, as a sorted list of ids.
///
/// An empty offender list from the gate above means nothing on its own — it would also be
/// empty for a corpus in which the field existed but no rule ever used the interesting value,
/// which is the "complete, fully-tested, totally inert mechanism" failure mode
/// [`rules_declaring_max_severity`] was written in response to. This function is the probe
/// that proves the corpus actually EXERCISES the field, and
/// `req_w16_the_emit_only_population_is_pinned` pins its exact contents so the population
/// cannot drift silently in either direction.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_declaring_emit_only(set: &RuleSet) -> Vec<String> {
    let mut ids: Vec<String> = set
        .iter()
        .filter(|r| !r.is_inspectable())
        .map(|r| r.id.0.clone())
        .collect();
    ids.sort();
    ids
}

/// Every rule in `set` whose [`Verification`] value makes it UNSELECTABLE on every repo —
/// i.e. `!Rule::is_auto_recommended()`, which is what an unattended scan's rule selection
/// (`camerata_cli::inspect_cmd::curated_rule_selection`) filters on. Sorted for stable output.
///
/// This is the verification half of requirement H1 (Pass-1 W4,
/// `docs/plans/2026-10-09_audit-findings-tracker.md`): "every rule must be CAPABLE of running.
/// A rule that does not run because it is irrelevant to the scanned stack is CORRECT. A rule
/// that could never run for ANY stack is a DEFECT." A `verification` value is a REPO-INDEPENDENT
/// fact, so a rule disarmed by it is disarmed everywhere, forever — exactly the defect class
/// H1 names, and exactly what shipped as findings A7 (`UI-UTC-DATES-1` omitted `verification`
/// entirely, defaulted to [`Verification::Draft`], and had never armed on any repo — while
/// being the only `ui` rule with a shipped deterministic detector) and A8
/// (`SPIRIT-FILE-SIZE-1`, disarmed by `verification = "policy"`).
///
/// `req_h1_no_corpus_rule_is_disarmed_by_its_verification_value` asserts this is EMPTY for the
/// real shipped corpus, so a rule authored (or edited) into an unarmable verification state
/// fails the BUILD instead of quietly contributing nothing to every scan. Same build-time-gate
/// philosophy as [`rules_missing_kind`]: the TOML-parse layer stays lenient, this layer
/// enforces.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_disarmed_by_verification(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = set
        .iter()
        .filter(|r| !r.is_auto_recommended())
        .map(|r| format!("{} ({})", r.id.0, r.verification().as_str()))
        .collect();
    offenders.sort();
    offenders
}

/// Every [`RuleKind::DefaultWithExceptions`] rule in `set` that has options but NONE of them
/// is marked `violates` — suspicious per item 4 of the W2 plan: a `default_with_exceptions`
/// rule's whole point is that at least one non-default option is the degenerate/absent form of
/// the requirement (that's the "exceptions" half of the name). A rule classified this way with
/// zero `violates` options is either misclassified (it should be [`RuleKind::Decision`] —
/// genuinely no option is degenerate) or under-authored (a `violates` flag was never set on
/// the option that should carry one). Sorted for stable output. Does NOT fail the build by
/// itself (unlike [`rules_missing_kind`]) — "suspicious" is a signal for human review during
/// authoring/classification, not a hard invariant the way "every rule has A kind" is; callers
/// that want it enforced (e.g. a classification-coverage test) assert on this directly.
///
/// W17: [`RuleKind::DecisionWithFloor`] is deliberately NOT swept here, and that is a
/// substantive exclusion rather than an oversight. This gate's premise is the "exceptions"
/// half of `default_with_exceptions` — deviations FROM a privileged default — and a
/// `decision_with_floor` rule has no privileged default to deviate from. Its flags are not
/// exceptions to a winner; they are a floor beneath several winners. Sweeping it here would
/// report a correctly-authored rule as under-authored. The flag discipline that DOES apply to
/// it is GATE 1 ([`options_with_rejected_prose_but_no_violates_flag`]), which holds it to the
/// `Rejected:` convention like any other flag-permitting kind.
#[cfg_attr(not(test), allow(dead_code))]
pub fn suspicious_default_with_exceptions_rules(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = set
        .iter()
        .filter(|r| {
            r.kind == Some(RuleKind::DefaultWithExceptions)
                && !r.options.is_empty()
                && !r.options.iter().any(|o| o.violates)
        })
        .map(|r| r.id.0.clone())
        .collect();
    offenders.sort();
    offenders
}

// ────────────────────────────────────────────────────────────────────────────
// W14 corpus-load gates: flag/prose agreement, the decision-kind invariant, and
// the schema-vocabulary gate (`docs/plans/2026-10-09_audit-findings-tracker.md`)
// ────────────────────────────────────────────────────────────────────────────
//
// Same build-time-gate philosophy as `rules_missing_kind` above and the same reason: these
// fail the BUILD, never a scan. A scan must never refuse to run over corpus state, so none of
// these is wired into `load_corpus`/`load_corpus_lenient` as a runtime refusal.

/// The literal token a violating option's `why` is expected to OPEN with, per the corpus's
/// strongest-performing authoring convention (`docs/RULES_REFERENCE.md` section 10.4).
///
/// Why this token specifically: all 17 flagged `supabase/` rules used it, and that folder
/// scored 17 of 17 correct in the 244-rule `violates` audit while other folders ran as low as
/// 59 percent. An author forced to begin the option's rationale with `Rejected:` cannot write
/// it as a neutral alternative by accident, and the agreement between the prose and the flag
/// becomes mechanically checkable — which is what
/// [`options_with_rejected_prose_but_no_violates_flag`] does with it.
pub const REJECTED_PROSE_TOKEN: &str = "Rejected:";

/// Whether `why` opens with [`REJECTED_PROSE_TOKEN`]. Leading whitespace is trimmed because the
/// corpus authors `why` as both an inline string and a `"""`-delimited block (which begins with
/// a newline), and the two must be treated identically — a convention that depended on the
/// quoting style would be a convention in name only.
fn why_opens_with_rejected(why: &str) -> bool {
    why.trim_start().starts_with(REJECTED_PROSE_TOKEN)
}

/// GATE 1 (W14). Every option whose `why` OPENS with [`REJECTED_PROSE_TOKEN`] but which does
/// NOT carry `violates = true`, as `"<rule id> / <option id> (kind = <kind>)"`. Sorted for
/// stable output. `req_w14_every_rejected_option_is_flagged_violates` asserts this is EMPTY for
/// the real shipped corpus.
///
/// This is the direct gate against the 244-rule audit's DOMINANT error class: a MISSING
/// `violates` flag, which leaves the engine silent on a fault its own rule describes in plain
/// language. 76 of 244 flags (31 percent) were wrong, and the missing-flag shape is the one
/// that turns a security requirement into "the convention this codebase chose".
///
/// **Why `decision`-kind rules are deliberately OUT OF SCOPE, and why that is a scoping rule
/// rather than a carved exception.** The taxonomy forbids `violates` on a `decision`-kind rule
/// outright (see [`RuleKind`]'s doc comment and [`decision_kind_rules_with_violates_flag`]). If
/// this gate also demanded the flag on `decision` rules, the two W14 gates would contradict each
/// other on the same option and no corpus could satisfy both. So the invariant splits cleanly in
/// two along `kind`:
///
/// - on a rule whose kind PERMITS the flag (`defect`, `default_with_exceptions`,
///   `decision_with_floor`, or an absent kind — which `rules_missing_kind` separately fails the
///   build on), `Rejected:` prose MUST be paired with the flag, and that is this gate;
/// - on a `decision`-kind rule, `Rejected:` prose is instead evidence that the KIND is wrong,
///   which is [`decision_kind_rules_with_rejected_prose`]'s advisory finding.
///
/// W17: the scoping test is [`RuleKind::permits_violates_flag`] rather than an inline
/// `!= Decision` comparison, so the fourth kind states its answer in one exhaustive match
/// instead of inheriting one by falling past a comparison. `decision_with_floor` is IN scope —
/// it names faults by construction, so the prose/flag agreement is exactly as binding there as
/// on a `defect` rule.
///
/// Measured on all 417 corpus rules / 1124 options on 2026-10-11: 78 options open with the
/// token. 70 carry the flag and 8 do not — and all 70 flagged sit on `defect` (65) or
/// `default_with_exceptions` (5) rules while all 8 unflagged sit on `decision` rules. So within
/// this gate's scope the prose/flag agreement is 70 out of 70, with zero counter-examples; the
/// split is not a convenience, it is where the corpus's own data divides. (W17 moved
/// `RUST-DOMAIN-6` from `default_with_exceptions` to `decision_with_floor`; both kinds permit
/// the flag, so the in-scope population is unchanged.)
#[cfg_attr(not(test), allow(dead_code))]
pub fn options_with_rejected_prose_but_no_violates_flag(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = Vec::new();
    for r in set.iter() {
        // A `decision`-kind rule cannot satisfy this gate and the no-flag invariant at once;
        // its `Rejected:` options are reported by `decision_kind_rules_with_rejected_prose`.
        // An ABSENT kind stays in scope: `rules_missing_kind` fails the build on it separately,
        // and until then the conservative reading is that the prose is right.
        if r.kind.is_some_and(|k| !k.permits_violates_flag()) {
            continue;
        }
        for o in &r.options {
            if why_opens_with_rejected(&o.why) && !o.violates {
                offenders.push(format!(
                    "{} / {} (kind = {:?}): `why` opens with {REJECTED_PROSE_TOKEN:?} but \
                     `violates` is not set — the rule names this option as the fault in prose \
                     while the engine treats it as a genuine alternative",
                    r.id.0, o.id, r.kind
                ));
            }
        }
    }
    offenders.sort();
    offenders
}

/// GATE 1b (W14), ADVISORY — deliberately NOT build-failing. Every `decision`-kind rule with at
/// least one option whose `why` opens with [`REJECTED_PROSE_TOKEN`], as
/// `"<rule id> / <option id>"` per offending option. Sorted for stable output.
///
/// A rule cannot simultaneously hold that its alternatives are of EQUAL WEIGHT (`kind =
/// "decision"`, whose only reportable condition is cross-codebase inconsistency) and that one of
/// them is `Rejected:`. One of the two is wrong, and in the 244-rule audit the authored prose was
/// right about three times out of four — so this is evidence the rule's `kind` is wrong.
///
/// **Why advisory and not a build failure.** This is exactly the W12 defect shape (`kind =
/// "decision"`, zero `violates` flags, 2 options, one opening `Rejected:`) that saw 19 security
/// rules reclassified to `defect`. Resolving it means CHANGING a rule's `kind`, and 39 rules sit
/// on the owner's paused A5.7 classification hold — 6 of the 7 rules this function reports are on
/// that hold list (`docs/plans/2026-10-09_rule-kind-classification.md`, "NEEDS OWNER DECISION").
/// Failing the build would force a reclassification the owner has explicitly paused. The
/// requirement a gate can honestly carry here is that the population cannot GROW silently, which
/// is what `req_w14_decision_kind_rules_with_rejected_prose_match_the_known_roster` pins.
///
/// Measured on 2026-10-11: 8 options across 7 rules. The roster and the per-rule reading live
/// with the pinning test, in `crates/rules/tests/w14_corpus_schema_gates.rs`
/// (`DECISION_KIND_RULES_WITH_REJECTED_PROSE`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn decision_kind_rules_with_rejected_prose(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for r in set.iter() {
        if r.kind != Some(RuleKind::Decision) {
            continue;
        }
        for o in &r.options {
            if why_opens_with_rejected(&o.why) {
                found.push(format!("{} / {}", r.id.0, o.id));
            }
        }
    }
    found.sort();
    found
}

/// GATE 2 (W14). Every `decision`-kind rule in `set` carrying at least one `violates` option, as
/// `"<rule id>: [<option ids>]"`. Sorted for stable output.
/// `req_w14_no_decision_kind_rule_carries_a_violates_flag` asserts this is EMPTY for the real
/// shipped corpus.
///
/// The invariant is [`RuleKind`]'s own stated rule: *"No option on a `decision`-kind rule should
/// carry `violates = true`."* It was DOCUMENTED AND COMPLETELY UNENFORCED until W14. This
/// function exists to move it into the corpus-load gate FAMILY (alongside
/// [`rules_missing_kind`], [`rules_disarmed_by_verification`] and the severity gates) so it is a
/// reusable gate callable by any build/integrity consumer, not only an assertion living inside
/// one test file.
///
/// **The invariant is not cosmetic — the flag is LIVE at runtime on a decision rule.**
/// `camerata_server::ai_audit::apply_alternative_decision` branches purely on
/// `option.violates` and never consults the rule's `kind`, so a stray flag on a `decision` rule
/// silently changes behaviour: if the alternatives pass decides that option, the pipeline emits a
/// guaranteed high-severity `Defect` finding for a rule whose whole premise is that its options
/// are equivalent. That is a wrong accusation against conforming code, which is the mirror image
/// of the missing-flag class [`options_with_rejected_prose_but_no_violates_flag`] guards.
///
/// Prior coverage, stated plainly rather than duplicated silently: W11 already asserts this
/// corpus-wide in `crates/rules/tests/w11_kind_taxonomy.rs::
/// no_decision_kind_rule_in_the_corpus_carries_a_violates_flag`. That test is left exactly as
/// written (it also pins W11's seven reclassified ids, which this function deliberately does not
/// re-assert). What W14 adds is the gate-function form plus the vacuity guard below.
///
/// Measured on 2026-10-11: 156 `decision`-kind rules of 417, zero offenders.
#[cfg_attr(not(test), allow(dead_code))]
pub fn decision_kind_rules_with_violates_flag(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = set
        .iter()
        .filter(|r| r.kind == Some(RuleKind::Decision))
        .filter_map(|r| {
            let flagged: Vec<&str> = r
                .options
                .iter()
                .filter(|o| o.violates)
                .map(|o| o.id.as_str())
                .collect();
            (!flagged.is_empty()).then(|| format!("{}: {:?}", r.id.0, flagged))
        })
        .collect();
    offenders.sort();
    offenders
}

/// How many rules in `set` are `decision`-kind. Exists so
/// [`decision_kind_rules_with_violates_flag`]'s gate can prove it is not passing VACUOUSLY — an
/// empty offender list means nothing if the predicate selected no rules at all, which is the
/// exact failure mode (a complete, fully-tested, totally inert mechanism) that
/// [`rules_declaring_max_severity`] was written in response to.
#[cfg_attr(not(test), allow(dead_code))]
pub fn decision_kind_rule_count(set: &RuleSet) -> usize {
    set.iter()
        .filter(|r| r.kind == Some(RuleKind::Decision))
        .count()
}

// ── GATE 3: the schema-vocabulary gate (gotcha 9: a misspelled FIELD NAME vanishes) ──
//
// `RuleToml` deliberately does not set `serde(deny_unknown_fields)` (see its doc comment), so
// `max_severety = "high"`, or `violates = true` typed as `voilates = true`, parses cleanly and
// simply is not there. With three separate silent-no-op mechanisms already shipped in this
// corpus, that default costs more than it saves.
//
// Gate 3 is implemented as a CORPUS-LOAD GATE over the raw TOML rather than as
// `serde(deny_unknown_fields)` on the structs, for three reasons:
//
//  1. It is STRICTLY STRONGER. `deny_unknown_fields` on `RuleToml` would check rule-level keys
//     only; a typo inside `[[option]]`, `[decision]`, `[[sources]]`, `[verified]`,
//     `[[stack_exception]]` or an `escalation` inline table would still vanish. This gate walks
//     every one of those levels.
//  2. It keeps the gate a BUILD failure, never a runtime refusal — the house rule for every
//     other gate in this section (`docs/RULES_REFERENCE.md` section 10.6: "a scan must never
//     refuse to run over corpus state").
//  3. Flipping the serde attribute is BLOCKED by files this workstream does not own.
//     `crates/server/tests/onboarding_full_e2e.rs` (and its siblings) write tiny fixture
//     corpora in the deprecated camerata-ai format, carrying RULE-LEVEL `directive` and
//     `conformance` keys that `RuleToml` does not declare. With `deny_unknown_fields` on, every
//     one of those fixture files fails to parse, `load_corpus_lenient` drops it, and those
//     e2e tests break. Fixing them means editing `crates/server/**`, which W14 does not own, so
//     the attribute stays off and this gate carries the requirement instead. See the report.
//
// The vocabularies below MUST mirror the structs they name. The failure direction if one drifts
// is safe and loud: a field added to a struct but not to its vocabulary makes this gate reject
// the corpus (a red build), never silently accept an unread key.

/// Rule-level keys `RuleToml` declares, including the explicitly-accepted-but-unconsumed
/// `layer`/`emits` (see their doc comments on `RuleToml`). Mirrors `RuleToml`.
///
/// W17: `tag` is deliberately ABSENT. It was deleted from `RuleToml` and from all 417 TOMLs, so
/// this gate is now what rejects its reappearance — the whole point of removing a field from the
/// schema at the same time as removing it from the data.
const RULE_LEVEL_FIELDS: &[&str] = &[
    "id",
    "title",
    "enforcement",
    "domain",
    "qualifies",
    "decision",
    "default",
    "opt_in_only",
    "layer3_only",
    "option",
    "verification",
    "sources",
    "verified",
    "stack_exception",
    "extra_domains",
    "kind",
    "inspectable",
    "max_severity",
    "min_severity",
    "deviation_ceiling",
    // W32 (report credibility): external-taxonomy + supersession metadata.
    "cwe",
    "asvs",
    "superseded_since",
    "current_recommendation",
    // Per-rule freshness schema (design amendment 2026-10-21): the revision number and the
    // per-rule review date.
    "version",
    "last_reviewed",
    // Token-efficiency rank 7: where the rule can apply (routing metadata).
    "applies_to",
    // Accepted, unconsumed (gotcha 8), established 2026-10-11. `tag` was here too until W17
    // deleted it from the schema AND from all 417 files; do not re-add it.
    "layer",
    "emits",
];

/// `[[option]]` keys. Mirrors `OptionToml`. Measured 2026-10-11: zero unknown option-level keys
/// across all 417 rules / 1124 options, so this level of the gate is clean from day one.
const OPTION_LEVEL_FIELDS: &[&str] = &[
    "id",
    "label",
    "directive",
    "why",
    "remediation",
    "finding_headline",
    "finding_detail",
    "escalation",
    "effort",
    "violates",
];

/// `[decision]` keys. Mirrors `DecisionToml`.
const DECISION_FIELDS: &[&str] = &["question", "why", "default"];

/// `[[sources]]` keys. Mirrors [`RuleSource`] — PLUS `qualifies`, which is accepted here so that
/// [`corpus_rules_with_qualifies_nested_in_sources`] can be the gate that rejects it, with a
/// message that says what to do instead.
///
/// What W14 found: `qualifies` is a RULE-level field, but in 17 `go/` rules it is authored on a
/// line that sits BELOW a `[[sources]]` header, so TOML scopes it into the source table and
/// [`RuleSource`] discards it — 29 occurrences on 2026-10-11, gotcha 9 in the wild.
///
/// W17 re-measured those 29 and found they are TWO different shapes, which the single count had
/// hidden:
///
/// - **10 occurrences, one each in 10 `go/*.toml` rules, with NO rule-level `qualifies`.** The
///   text is rule-level in substance and the rule lost it: `Rule::summary` fell through to
///   `[decision].why`. This was the real defect and it is FIXED — all 10 moved to the rule level,
///   changing 10 rules' client-visible summary text. `corpus_rules_losing_qualifies_text_to_
///   sources` is now a build-failing gate asserting this shape never returns.
/// - **19 occurrences across 7 `go/testing/*.toml` rules that ALREADY carry a rule-level
///   `qualifies`.** These are not misplaced rule-level text at all: each one annotates the SOURCE
///   it sits under ("t.TempDir() returns a temporary directory unique to each test…",
///   "golangci-lint thelper: 'Detects tests helpers which do not start with…'"). They are per-
///   source notes wearing a rule-level field's name, and W17 left them in place pending an owner
///   ruling on whether to delete them or give `[[sources]]` a field of its own.
///
/// **W18 is that ruling, and it is "keep them".** [`RuleSource::relevance`] now exists, all 19
/// were migrated into it, and the corpus carries ZERO nested `qualifies` keys. The naming (W18):
/// `relevance`, not a second `note` (`[[stack_exception]].note` already owns that word for a
/// different scope) and not a verb like `substantiates`/`grounds` (`qualifies`/`violates` are
/// the corpus's verb-shaped keys and both read as claims about the RULE, which is the exact
/// level ambiguity being retired; `grounding` additionally collides with `verification =
/// "grounded"`).
///
/// `qualifies` stays in this vocabulary ON PURPOSE even though no corpus file uses it here any
/// more. Dropping it would make [`corpus_files_with_unknown_fields`] report it as a typo, which
/// is both wrong (it is a correctly-spelled key in the wrong place) and a worse message than the
/// dedicated gate's. That is the same non-overlap reasoning this module already applies to
/// unparseable TOML and to `rules_with_invalid_severity_floor`: one defect, one gate, one
/// message. The enforcement did not weaken — it moved from
/// [`corpus_rules_with_qualifies_nested_in_sources`], which fails the build on ANY nested
/// occurrence.
const SOURCE_FIELDS: &[&str] = &["url", "title", "linter", "relevance", "qualifies"];

/// `[verified]` keys. Mirrors [`VerifiedProvenance`].
const VERIFIED_FIELDS: &[&str] = &["by", "at", "against"];

/// `[[stack_exception]]` keys. Mirrors `StackExceptionToml`.
const STACK_EXCEPTION_FIELDS: &[&str] = &["framework", "path_glob", "note"];

/// `escalation = { ... }` inline-table keys. Mirrors [`EscalationSpec`].
const ESCALATION_FIELDS: &[&str] = &["condition", "severity"];

/// Push an offender line for every key of `table` that is not in `allowed`.
fn collect_unknown_keys(
    table: &toml::value::Table,
    allowed: &[&str],
    where_: &str,
    file: &str,
    out: &mut Vec<String>,
) {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            out.push(format!(
                "{file}: unknown field {key:?} at {where_} — not in the declared schema, so it \
                 is parsed by nothing and vanishes silently (gotcha 9). Check the spelling \
                 against `RuleToml`/`OptionToml`, or declare it as accepted-and-unconsumed."
            ));
        }
    }
}

/// GATE 3 (W14). Every unknown FIELD NAME found anywhere in the TOML files under `corpus_dir`,
/// as `"<relative path>: unknown field \"<key>\" at <location> — ..."`. Sorted for stable
/// output. `req_w14_no_corpus_rule_carries_an_unknown_field` asserts this is EMPTY for the real
/// shipped corpus.
///
/// Walks EVERY nesting level against the vocabularies above: rule-level, `[decision]`,
/// `[[option]]` (and each option's `escalation` inline table), `[[sources]]`, `[verified]`, and
/// `[[stack_exception]]`.
///
/// A file that fails to parse as TOML at all is NOT reported here — that is a different defect
/// (gotcha 10) with a different owner: `load_corpus`'s own error, which the test suite's
/// `errors.is_empty()` assertions already catch. Reporting it twice would make the two gates'
/// output overlap confusingly, the same reasoning
/// [`rules_with_invalid_severity_floor`] uses for an invalid `max_severity`.
///
/// Measured on all 417 corpus files on 2026-10-11: zero unknown fields, once `tag` (417),
/// `layer` (417), `emits` (2) and `sources.qualifies` (29) were declared as
/// accepted-and-unconsumed above. Those four were the ONLY unknown keys in the corpus; before
/// declaring them this gate reported 865 offenders.
///
/// W17 narrowed that list. `tag` was DELETED from all 417 files and from the schema, so this
/// gate now REJECTS it (417 offenders if it ever returns). `sources.qualifies` stays in
/// [`SOURCE_FIELDS`] — the 29 occurrences were MOVED (10 to the rule level by W17, the other 19
/// to [`RuleSource::relevance`] by W18), and the regression guard is the dedicated
/// [`corpus_rules_with_qualifies_nested_in_sources`] rather than this gate, so that a
/// correctly-spelled key in the wrong place gets a message about placement rather than about
/// spelling. That keeps this gate's synthetic fixtures (and `RuleSource`'s tolerance of a stray
/// key) unchanged. `layer` (417) and `emits` (2) remain accepted-and-unconsumed, both awaiting
/// an owner ruling; `relevance` (19) is accepted and PARSED, which is a different thing.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn corpus_files_with_unknown_fields(
    corpus_dir: &Path,
) -> Result<Vec<String>, RulesError> {
    let paths = collect_toml_paths(corpus_dir).await?;
    let mut out: Vec<String> = Vec::new();

    for path in paths {
        let bytes = tokio::fs::read(&path).await.map_err(|e| RulesError::Io {
            path: path.clone(),
            source: e,
        })?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // A hard TOML syntax error is gotcha 10's finding, not this gate's — skip quietly.
        let Ok(value) = text.parse::<toml::Value>() else {
            continue;
        };
        let Some(root) = value.as_table() else {
            continue;
        };
        let file = path
            .strip_prefix(corpus_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        collect_unknown_keys(root, RULE_LEVEL_FIELDS, "rule level", &file, &mut out);

        if let Some(decision) = root.get("decision").and_then(|v| v.as_table()) {
            collect_unknown_keys(decision, DECISION_FIELDS, "[decision]", &file, &mut out);
        }
        if let Some(verified) = root.get("verified").and_then(|v| v.as_table()) {
            collect_unknown_keys(verified, VERIFIED_FIELDS, "[verified]", &file, &mut out);
        }
        for (i, src) in array_tables(root, "sources").into_iter().enumerate() {
            collect_unknown_keys(
                src,
                SOURCE_FIELDS,
                &format!("[[sources]][{i}]"),
                &file,
                &mut out,
            );
        }
        for (i, ex) in array_tables(root, "stack_exception")
            .into_iter()
            .enumerate()
        {
            collect_unknown_keys(
                ex,
                STACK_EXCEPTION_FIELDS,
                &format!("[[stack_exception]][{i}]"),
                &file,
                &mut out,
            );
        }
        for (i, opt) in array_tables(root, "option").into_iter().enumerate() {
            let label = opt
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_else(|| format!("[{i}]"));
            collect_unknown_keys(
                opt,
                OPTION_LEVEL_FIELDS,
                &format!("[[option]] {label}"),
                &file,
                &mut out,
            );
            if let Some(esc) = opt.get("escalation").and_then(|v| v.as_table()) {
                collect_unknown_keys(
                    esc,
                    ESCALATION_FIELDS,
                    &format!("[[option]] {label}.escalation"),
                    &file,
                    &mut out,
                );
            }
        }
    }

    out.sort();
    Ok(out)
}

/// The tables of an array-of-tables field (`[[option]]`, `[[sources]]`, `[[stack_exception]]`),
/// or empty when the key is absent or not an array of tables.
fn array_tables<'a>(root: &'a toml::value::Table, key: &str) -> Vec<&'a toml::value::Table> {
    root.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_table()).collect())
        .unwrap_or_default()
}

/// ADVISORY (W14). Every corpus rule that authors `qualifies` INSIDE a `[[sources]]` table, as
/// `"<rule id> (sources[<i>], rule-level qualifies: present|ABSENT)"`. Sorted for stable output.
///
/// See [`SOURCE_FIELDS`] for the full story, including W17's finding that the original count of
/// 29 covered TWO different shapes. This function still reports BOTH. The shape that actually
/// loses a rule its summary is split out into [`corpus_rules_losing_qualifies_text_to_sources`],
/// which IS build-failing.
///
/// `req_w14_the_misplaced_qualifies_roster_has_not_grown` pins this roster's exact composition so
/// neither shape can change silently.
///
/// W18 note: the roster is now EMPTY — the 19 per-source annotations moved to
/// [`RuleSource::relevance`] — and the build-failing gate for the general shape is
/// [`corpus_rules_with_qualifies_nested_in_sources`]. This function is kept as the roster
/// REPORTER it has always been (it is the only one of the three that states, per occurrence,
/// whether the rule has a rule-level `qualifies`), and it stays advisory so that the gate which
/// fails the build is the one with the actionable message.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn corpus_rules_with_qualifies_misplaced_into_sources(
    corpus_dir: &Path,
) -> Result<Vec<String>, RulesError> {
    let paths = collect_toml_paths(corpus_dir).await?;
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        let bytes = tokio::fs::read(&path).await.map_err(|e| RulesError::Io {
            path: path.clone(),
            source: e,
        })?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let Ok(value) = text.parse::<toml::Value>() else {
            continue;
        };
        let Some(root) = value.as_table() else {
            continue;
        };
        let id = root
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("<no id>")
            .to_owned();
        let rule_level = if root.contains_key("qualifies") {
            "present"
        } else {
            "ABSENT"
        };
        for (i, src) in array_tables(root, "sources").into_iter().enumerate() {
            if src.contains_key("qualifies") {
                out.push(format!(
                    "{id} (sources[{i}], rule-level qualifies: {rule_level})"
                ));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// GATE (W17). Every corpus rule that authors `qualifies` inside a `[[sources]]` table AND has NO
/// rule-level `qualifies`, as `"<rule id> (sources[<i>]): …"`. Sorted for stable output.
/// `req_w17_no_corpus_rule_loses_its_qualifies_text_to_a_sources_block` asserts this is EMPTY.
///
/// This is the half of W14's 29-occurrence roster that CAUSED REAL HARM, separated from the half
/// that did not. When a rule has no rule-level `qualifies`, the nested one is discarded by
/// [`RuleSource`] and `Rule::summary` falls through to `[decision].why` — so the rule ships a
/// summary its author did not write, with no error, no warning, and no way to notice from the
/// outside. 10 `go/*.toml` rules were in that state; W17 moved all 10 to the rule level.
///
/// Build-failing, where [`corpus_rules_with_qualifies_misplaced_into_sources`] stays advisory,
/// because when this was written the two shapes had different owners. This one has a mechanical,
/// text-preserving fix (move the line up), so a new occurrence is always an author error and
/// should stop the build. The other shape — a per-source annotation on a rule whose rule-level
/// `qualifies` is already correct — lost nothing a client could see, and its resolution was a
/// corpus-content decision the owner had not yet made.
///
/// W18 made it: [`RuleSource::relevance`] holds that content, so the other shape is now an author
/// error too and [`corpus_rules_with_qualifies_nested_in_sources`] fails the build on BOTH. This
/// gate is retained rather than folded in, because it is the one that names the CONSEQUENCE
/// (`Rule::summary` silently became `[decision].why`) rather than the mistake, and that
/// consequence is the reason any of this is gated at all. It can only ever report a subset of
/// the W18 gate's offenders.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn corpus_rules_losing_qualifies_text_to_sources(
    corpus_dir: &Path,
) -> Result<Vec<String>, RulesError> {
    Ok(
        corpus_rules_with_qualifies_misplaced_into_sources(corpus_dir)
            .await?
            .into_iter()
            // The reporter encodes rule-level presence in its own line; ABSENT is the harmful shape.
            .filter(|line| line.contains("ABSENT"))
            .map(|line| {
                format!(
                "{line}: the nested `qualifies` is discarded by `RuleSource` and the rule has no \
                 rule-level one, so `Rule::summary` silently falls back to `[decision].why` \
                 instead of this authored text. Move the line ABOVE the first `[[sources]]` \
                 header."
            )
            })
            .collect(),
    )
}

/// GATE (W18). Every `[[sources]]` block in the corpus that authors a `qualifies` key, as
/// `"<rule id> (sources[<i>] \"<title>\"): …"`. Sorted for stable output.
/// `req_w18_no_corpus_rule_authors_qualifies_inside_a_sources_block` asserts this is EMPTY.
///
/// **This is the general shape, and until W18 it had no guard.** The two older gates are each
/// narrower, and the 19 per-source annotations lived in the gap between them:
///
/// - [`corpus_rules_with_qualifies_misplaced_into_sources`] reports exactly this shape but is
///   ADVISORY — it is pinned to a roster COUNT, so 19 known occurrences were a passing state and
///   only a 20th would have failed.
/// - [`corpus_rules_losing_qualifies_text_to_sources`] fails the build, but only on the subset
///   that ALSO has no rule-level `qualifies`. The 19 all had one, so it never fired on them.
///
/// The gap was deliberate while nested `qualifies` had two possible meanings and only one of them
/// was clearly an error. [`RuleSource::relevance`] removed the ambiguity: a nested `qualifies` is
/// now always one of two author mistakes, both with a mechanical fix, so it stops the build.
///
/// Why this and not simply deleting `qualifies` from [`SOURCE_FIELDS`] (which would make
/// [`corpus_files_with_unknown_fields`] reject it for free): that gate's message is "unknown
/// field — check the spelling", and the spelling is not the problem. An author who meant
/// `relevance` and an author who meant the rule's summary need two different instructions, and
/// this gate's offender line carries both.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn corpus_rules_with_qualifies_nested_in_sources(
    corpus_dir: &Path,
) -> Result<Vec<String>, RulesError> {
    let paths = collect_toml_paths(corpus_dir).await?;
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        let bytes = tokio::fs::read(&path).await.map_err(|e| RulesError::Io {
            path: path.clone(),
            source: e,
        })?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // A hard TOML syntax error is gotcha 10's finding, not this gate's — skip quietly.
        let Ok(value) = text.parse::<toml::Value>() else {
            continue;
        };
        let Some(root) = value.as_table() else {
            continue;
        };
        let id = root
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("<no id>")
            .to_owned();
        let rule_level = root.contains_key("qualifies");
        for (i, src) in array_tables(root, "sources").into_iter().enumerate() {
            if !src.contains_key("qualifies") {
                continue;
            }
            let title = src
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("<no title>");
            // The two mistakes need two different instructions, so say which one this looks
            // like: a rule that already has its own `qualifies` cannot have meant a second one.
            let fix = if rule_level {
                "the rule already has a rule-level `qualifies`, so this is an annotation about \
                 the SOURCE — rename the key to `relevance`"
            } else {
                "the rule has NO rule-level `qualifies`, so this is either the rule's own summary \
                 written one level too deep (move the line ABOVE the first `[[sources]]` header) \
                 or an annotation about the source (rename the key to `relevance`)"
            };
            out.push(format!(
                "{id} (sources[{i}] {title:?}): `qualifies` is not a `[[sources]]` field, so \
                 `RuleSource` parses it into nothing and the authored text is lost silently — \
                 {fix}."
            ));
        }
    }
    out.sort();
    Ok(out)
}

/// The COMPLETE severity vocabulary a corpus-authored ceiling ([`Rule::max_severity`],
/// [`Rule::deviation_ceiling`]) may use, most severe first. Mirrors the vocabulary
/// `camerata_server::report_export::normalize_severity` recognizes; this list exists so the
/// corpus can be GATED against it at build time without the rules crate depending on the
/// server crate.
pub const CEILING_SEVERITY_VOCABULARY: [&str; 5] = ["critical", "high", "medium", "low", "info"];

/// Rank of a ceiling value within [`CEILING_SEVERITY_VOCABULARY`] — lower is MORE severe, the
/// same direction as `camerata_server::report_export::severity_rank`. `None` for a value
/// outside the vocabulary, which is precisely what [`rules_with_invalid_severity_ceiling`]
/// reports rather than silently coercing.
fn ceiling_rank(sev: &str) -> Option<u8> {
    CEILING_SEVERITY_VOCABULARY
        .iter()
        .position(|v| *v == sev)
        .map(|i| i as u8)
}

/// Every rule in `set` whose authored severity ceiling is INVALID, as
/// `"<id> (<field> = \"<value>\": <reason>)"`. Sorted for stable output. Three defect shapes:
///
/// 1. `max_severity` outside [`CEILING_SEVERITY_VOCABULARY`] (a typo, or an unknown severity
///    name).
/// 2. `deviation_ceiling` outside the same vocabulary.
/// 3. `deviation_ceiling` ranking MORE severe than the same rule's `max_severity` — incoherent
///    and unreachable: `max_severity` is applied to every row of the rule including its
///    deviations, so a deviation ceiling above it can never be the binding constraint and
///    states an intent the pipeline will never honour.
///
/// Why this has to be a BUILD gate rather than a runtime tolerance: the clamp resolves a
/// ceiling through `report_export::normalize_severity`, whose fall-through arm maps every
/// UNRECOGNIZED string to `"low"`. So `max_severity = "critcal"` does not fail, and does not
/// no-op — it silently becomes the MOST restrictive ceiling in the vocabulary and caps every
/// finding of that rule at `low`. A silently-typo'd ceiling is therefore the same class of
/// defect as the inert mechanism E2 shipped with (a field that confidently appears to say one
/// thing while doing another), and it is caught here, by failing the build, for the same
/// reason [`rules_missing_kind`] is.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_with_invalid_severity_ceiling(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = Vec::new();
    for r in set.iter() {
        let max_rank = match r.max_severity.as_deref() {
            None => None,
            Some(v) => match ceiling_rank(v) {
                Some(rank) => Some(rank),
                None => {
                    offenders.push(format!(
                        "{} (max_severity = {v:?}: not a severity; valid values are {:?})",
                        r.id.0, CEILING_SEVERITY_VOCABULARY
                    ));
                    continue;
                }
            },
        };
        let dev_rank = match r.deviation_ceiling.as_deref() {
            None => None,
            Some(v) => match ceiling_rank(v) {
                Some(rank) => Some(rank),
                None => {
                    offenders.push(format!(
                        "{} (deviation_ceiling = {v:?}: not a severity; valid values are {:?})",
                        r.id.0, CEILING_SEVERITY_VOCABULARY
                    ));
                    continue;
                }
            },
        };
        if let (Some(max), Some(dev)) = (max_rank, dev_rank) {
            if dev < max {
                offenders.push(format!(
                    "{} (deviation_ceiling = {:?} ranks ABOVE max_severity = {:?}: unreachable, \
                     since max_severity clamps this rule's deviations too)",
                    r.id.0,
                    r.deviation_ceiling.as_deref().unwrap_or_default(),
                    r.max_severity.as_deref().unwrap_or_default(),
                ));
            }
        }
    }
    offenders.sort();
    offenders
}

/// Every rule in `set` whose authored severity FLOOR ([`Rule::min_severity`]) is INVALID, as
/// `"<id> (<field> = \"<value>\": <reason>)"`. Sorted for stable output. The W8 mirror of
/// [`rules_with_invalid_severity_ceiling`], kept as its OWN function rather than folded into
/// that one so the pre-existing ceiling gate and its positive-detection test keep asserting
/// exactly what they asserted before. Two defect shapes:
///
/// 1. `min_severity` outside [`CEILING_SEVERITY_VOCABULARY`] (a typo, or an unknown severity
///    name).
/// 2. `min_severity` ranking MORE severe than the same rule's `max_severity`. That pair is
///    UNSATISFIABLE, not merely odd: the floor demands at least X while the ceiling forbids
///    anything above Y, with X above Y. Which one binds would then depend purely on the order
///    the export pipeline happens to apply them in — exactly the kind of silent, positional
///    surprise a build gate exists to prevent. (`min_severity == max_severity` is NOT an
///    error: it pins the rule at one severity, which is what several rules' prose actually
///    authors — e.g. "a high-severity finding for review, not an automatic critical".)
///
/// Why this has to be a BUILD gate, same argument as the ceiling's:
/// `report_export::normalize_severity` maps every UNRECOGNIZED string to `"low"`. For a
/// CEILING a typo silently becomes the harshest cap in the vocabulary; for a FLOOR it silently
/// becomes the WEAKEST possible floor — `min_severity = "hgih"` does not error and does not
/// no-op, it quietly evaporates (`"low"` ranks at or below everything, so the raise never
/// fires). A floor that looks authored and does nothing is the inert-mechanism failure the
/// whole severity effort exists to stop, so it fails the build here.
///
/// Note on the vocabulary's bottom: `report_export::severity_rank` ranks `"low"` and `"info"`
/// identically, so `min_severity = "low"` or `"info"` is INERT by construction rather than
/// invalid. It is not reported here (it states a true, if useless, intent and breaks nothing),
/// but no corpus rule authors one, and none should — a rule whose prose caps itself at low
/// wants [`Rule::max_severity`], not a floor.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_with_invalid_severity_floor(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = Vec::new();
    for r in set.iter() {
        let Some(raw_floor) = r.min_severity.as_deref() else {
            continue;
        };
        let Some(floor_rank) = ceiling_rank(raw_floor) else {
            offenders.push(format!(
                "{} (min_severity = {raw_floor:?}: not a severity; valid values are {:?})",
                r.id.0, CEILING_SEVERITY_VOCABULARY
            ));
            continue;
        };
        // An invalid max_severity is the ceiling gate's finding to report, not this one's —
        // reporting it twice would make the two gates' output confusingly overlap.
        if let Some(max_rank) = r.max_severity.as_deref().and_then(ceiling_rank) {
            if floor_rank < max_rank {
                offenders.push(format!(
                    "{} (min_severity = {:?} ranks ABOVE max_severity = {:?}: unsatisfiable — \
                     the floor demands at least the former while the ceiling forbids anything \
                     above the latter)",
                    r.id.0,
                    raw_floor,
                    r.max_severity.as_deref().unwrap_or_default(),
                ));
            }
        }
    }
    offenders.sort();
    offenders
}

/// Every rule in `set` that declares a [`Rule::min_severity`] floor, as `"<id> (<value>)"`.
/// Sorted for stable output. The W8 anti-inertness gate
/// (`req_w8_the_corpus_actually_declares_severity_floors`) asserts this is NON-EMPTY for the
/// real shipped corpus.
///
/// This function exists because of what happened to the ceiling. `max_severity`,
/// `clamp_to_rule_max_severity` and all four `req_e2_*` tests shipped GREEN against a corpus
/// in which **not one rule of 417 declared a ceiling** — the mechanism was fully implemented,
/// fully unit-tested, and completely inert. The floor is the same shape of mechanism and would
/// fail the same way for free, so the gate is written at the same time as the field rather
/// than after discovering the problem a second time.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_declaring_min_severity(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter_map(|r| {
            r.min_severity
                .as_deref()
                .map(|c| format!("{} ({c})", r.id.0))
        })
        .collect();
    found.sort();
    found
}

/// Every rule in `set` that declares a [`Rule::max_severity`] ceiling, as `"<id> (<value>)"`.
/// Sorted for stable output. Used by the anti-inertness gate
/// (`req_e2_the_corpus_actually_declares_severity_ceilings`): E2's mechanism shipped complete
/// and fully unit-tested against a corpus in which this returned EMPTY for all 417 rules, so
/// every `req_e2_*` test passed while the requirement did nothing. A non-empty result is the
/// only thing that makes those tests mean anything about real rules.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_declaring_max_severity(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter_map(|r| {
            r.max_severity
                .as_deref()
                .map(|c| format!("{} ({c})", r.id.0))
        })
        .collect();
    found.sort();
    found
}

/// Every rule in `set` that declares a [`Rule::deviation_ceiling`] escape hatch, as
/// `"<id> (<value>)"`. Sorted. The A5.2 counterpart to [`rules_declaring_max_severity`], and
/// inert for the identical reason until at least one rule authors one: with none authored,
/// `clamp_deviation_severity` pins EVERY deviation at the `"medium"` default, so "must be able
/// to reach HIGH when genuinely problematic" was unsatisfiable by construction.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_declaring_deviation_ceiling(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter_map(|r| {
            r.deviation_ceiling
                .as_deref()
                .map(|c| format!("{} ({c})", r.id.0))
        })
        .collect();
    found.sort();
    found
}

/// Rank the free-form lowercase severity vocabulary (`"info"` < `"low"` < `"medium"` <
/// `"high"` < `"critical"`). An unrecognized string ranks as `high` (3) — conservative: an
/// unknown cap is treated as capable of reaching high, never silently exempted.
fn severity_rank(s: &str) -> u8 {
    match s.trim().to_ascii_lowercase().as_str() {
        "info" | "informational" => 0,
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "critical" => 4,
        _ => 3,
    }
}

/// Whether `rule` can export a finding at HIGH severity or above, faithful to how severity
/// actually anchors at scan/export time:
///
/// - A declared `min_severity` floor of high+ forces it.
/// - A `violates` option's finding anchors at `"high"` (see [`Rule::max_severity`]'s and the
///   alternatives-pass docs), so any rule carrying one can reach high.
/// - A `default_with_exceptions`/`decision` rule's ordinary CONSISTENCY DEVIATION is clamped to
///   `"medium"` BY DEFAULT (`report_export::clamp_deviation_severity`) and only reaches high if
///   the rule authored a `deviation_ceiling` of high+.
/// - A `defect`-kind finding anchors at `"high"` (SEC-/ARCH- floor injection pushes to
///   high/critical; a plain confirmed defect anchors high), bounded only by a `max_severity`
///   cap if one is declared.
///
/// A plain decision/exceptions rule with no `violates` option, no high `deviation_ceiling`, and
/// no high `min_severity` therefore CANNOT emit high+ — its deviations clamp to medium — and is
/// correctly excluded.
pub fn rule_can_emit_high_plus(rule: &Rule) -> bool {
    if rule
        .min_severity
        .as_deref()
        .is_some_and(|s| severity_rank(s) >= 3)
    {
        return true;
    }
    if rule.options.iter().any(|o| o.violates) {
        return true;
    }
    if rule
        .deviation_ceiling
        .as_deref()
        .is_some_and(|s| severity_rank(s) >= 3)
    {
        return true;
    }
    if rule.is_defect_kind() {
        return match rule.max_severity.as_deref() {
            Some(cap) => severity_rank(cap) >= 3,
            None => true,
        };
    }
    false
}

/// W26 (fix-and-estimate quality): every rule that can emit a HIGH+ finding must carry at least
/// one `[[sources]]` citation, because a high-severity row a client reads with no source behind
/// it is exactly the "no high+ row without a citation" defect. Returns the offending rule ids,
/// sorted. A `draft` rule legitimately has empty sources, so it is excluded — it cannot ship a
/// client finding in the first place.
pub fn high_plus_rules_without_citation(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter(|r| r.verification != Verification::Draft)
        .filter(|r| rule_can_emit_high_plus(r))
        .filter(|r| r.sources.is_empty())
        .map(|r| r.id.0.clone())
        .collect();
    found.sort();
    found
}

/// W26 counterpart: every rule that can emit a HIGH+ finding must have at least one option
/// carrying a non-empty remediation. `report_export::resolve_fix` resolves a finding's Fix line
/// from the option it was judged against (the model's recommended option), falling back to the
/// rule's `default_option`; whichever option it lands on, a fix exists only if some option
/// authored a remediation. A high+ rule with NO remediated option anywhere can never produce a
/// Fix line for any finding — the unambiguous corpus-static form of "no high+ row without a
/// fix". (A rule with `default_option = None` but remediations on its options is fine: a
/// violation is always tagged with the recommended option, which carries the fix. The stronger
/// per-option guarantee is a runtime concern at the export boundary, where the resolved option
/// is known.) Returns offending rule ids, sorted. `RuleOption::remediation` is already
/// normalized to `None` when blank at load time.
pub fn high_plus_rules_without_any_fix(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter(|r| r.verification != Verification::Draft)
        .filter(|r| rule_can_emit_high_plus(r))
        .filter(|r| r.options.iter().all(|o| o.remediation.is_none()))
        .map(|r| r.id.0.clone())
        .collect();
    found.sort();
    found
}

/// W26 (magnitude sanity on estimates): every authored option `effort` must be exactly one of
/// the three-tier vocabulary `"low"` / `"medium"` / `"high"`. `effort` is a free-form
/// `Option<String>`, unvalidated at load, and `report_export::effort_hours_bounds` maps anything
/// outside the set to "not yet estimated" — so a typo like `"lo"` or a stray `"2 days"` would
/// silently strip the estimate off every finding of the rule rather than erroring. Returns
/// `"<rule-id> / <option-id>: '<value>'"` for each offender, sorted. (Blank efforts are already
/// normalized to `None` at load and are a legitimate "not yet estimated" state, so they are not
/// flagged — only a NON-blank value outside the set is.)
pub fn options_with_invalid_effort(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rule in set.iter() {
        for opt in &rule.options {
            if let Some(effort) = opt.effort.as_deref() {
                if !matches!(effort, "low" | "medium" | "high") {
                    found.push(format!("{} / {}: '{}'", rule.id.0, opt.id, effort));
                }
            }
        }
    }
    found.sort();
    found
}

/// W32 (report credibility): every SECURITY rule must carry at least one `cwe` code, so a
/// finding can print the externally-recognised anchor a client's security team already tracks.
/// "Security rule" is the `SEC-` id prefix — the deterministic security family. Returns the
/// offending rule ids, sorted. (ASVS is optional and not gated: not every security rule maps
/// cleanly to an ASVS requirement, and a missing ASVS code does not undermine credibility the
/// way a missing CWE on a vulnerability finding does.)
pub fn security_rules_without_cwe(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter(|r| r.id.0.starts_with("SEC-"))
        .filter(|r| r.cwe.iter().all(|c| c.trim().is_empty()))
        .map(|r| r.id.0.clone())
        .collect();
    found.sort();
    found
}

/// W32: every authored `cwe` code is shaped like `CWE-<digits>` and every `superseded_since`
/// like a 4-digit year. A free-form typo (`"CWE89"`, `"cwe-89"`, `"21"`) would render a
/// malformed anchor in the client PDF. Returns `"<rule-id>: <bad-value>"` for each offender,
/// sorted.
pub fn rules_with_malformed_credibility_metadata(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rule in set.iter() {
        for code in &rule.cwe {
            let rest = code.strip_prefix("CWE-");
            if !rest.is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())) {
                found.push(format!("{}: cwe {:?}", rule.id.0, code));
            }
        }
        if let Some(year) = rule.superseded_since.as_deref() {
            if year.len() != 4 || !year.chars().all(|c| c.is_ascii_digit()) {
                found.push(format!("{}: superseded_since {:?}", rule.id.0, year));
            }
        }
    }
    found.sort();
    found
}

/// Every rule in `set` that does NOT declare [`Rule::version`], as a sorted list of ids.
/// `req_every_corpus_rule_declares_a_version_of_at_least_one` asserts this is EMPTY for the real
/// shipped corpus.
///
/// Same lenient-parse / strict-gate split as [`rules_missing_kind`] and
/// [`rules_missing_inspectable`], for the same reason: there is deliberately no default. A
/// version that silently defaulted to `1` would let a rule enter the lock without anyone having
/// decided what its revision is, and the whole point of the field is that a change to a rule is
/// a deliberate, visible act.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rules_missing_version(set: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = set
        .iter()
        .filter(|r| r.version.is_none())
        .map(|r| r.id.0.clone())
        .collect();
    offenders.sort();
    offenders
}

/// Every rule whose declared [`Rule::version`] is below 1, as `"<id>: version 0 (must be an
/// integer >= 1)"`, sorted. An ABSENT version is [`rules_missing_version`]'s finding, not this
/// one's, so one defect produces one message. (A negative or non-integer value never reaches
/// this gate: it fails the TOML parse, which every corpus-loading test already asserts empty.)
pub fn rules_with_invalid_version(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = set
        .iter()
        .filter(|r| r.version == Some(0))
        .map(|r| format!("{}: version 0 (must be an integer >= 1)", r.id.0))
        .collect();
    found.sort();
    found
}

/// Every rule whose authored [`Rule::last_reviewed`] is not a real `YYYY-MM-DD` calendar date,
/// or is later than `today`, as `"<id>: last_reviewed \"<value>\" (<reason>)"`, sorted. A rule
/// with no `last_reviewed` is fine (the field is optional).
///
/// `today` is passed in by the caller as `(year, month, day)`, so this gate is a pure function
/// and its tests never read a clock. A future date is rejected because it would assert a review
/// that has not happened yet, and an effective-review-date computation ([`effective_review_date`])
/// that trusted it would report the rule as fresher than anyone has verified.
pub fn rules_with_invalid_last_reviewed(set: &RuleSet, today: (i64, u32, u32)) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rule in set.iter() {
        let Some(raw) = rule.last_reviewed.as_deref() else {
            continue;
        };
        match parse_ymd(raw) {
            None => found.push(format!(
                "{}: last_reviewed {:?} (not a real YYYY-MM-DD date)",
                rule.id.0, raw
            )),
            Some(date) if date > today => found.push(format!(
                "{}: last_reviewed {:?} (in the future; today is {:04}-{:02}-{:02})",
                rule.id.0, raw, today.0, today.1, today.2
            )),
            Some(_) => {}
        }
    }
    found.sort();
    found
}

/// Every rule that sets exactly one of `superseded_since` / `current_recommendation`, as
/// `"<id>: <what is missing>"`, sorted. The two travel together: "the ecosystem moved off this
/// pattern in 2021" with no pointer to what replaced it is half a claim, and a recommendation
/// with no year cannot say how stale the rule is. Blank values are scrubbed to absent at load,
/// so a whitespace-only half does not satisfy the pair.
pub fn rules_with_unpaired_supersession(set: &RuleSet) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rule in set.iter() {
        match (
            rule.superseded_since.is_some(),
            rule.current_recommendation.is_some(),
        ) {
            (true, false) => found.push(format!(
                "{}: superseded_since is set but current_recommendation is not",
                rule.id.0
            )),
            (false, true) => found.push(format!(
                "{}: current_recommendation is set but superseded_since is not",
                rule.id.0
            )),
            _ => {}
        }
    }
    found.sort();
    found
}

// ────────────────────────────────────────────────────────────────────────────
// Rule-subset selection (pure — RUST-PURE-STATE-TRANSITIONS-1)
// ────────────────────────────────────────────────────────────────────────────

/// Criteria for selecting a rule subset from a [`RuleSet`].
///
/// Filters compose with OR semantics: a rule matches if it satisfies
/// **any** active criterion. Use [`Filter::And`] for AND semantics.
#[derive(Debug, Clone)]
pub enum Filter<'a> {
    /// Match rules whose id is in this list.
    ByIds(&'a [RuleId]),
    /// Match rules whose `domain` field equals this value exactly.
    ByDomain(&'a str),
    /// Match rules belonging to any of these domains.
    ByDomains(&'a [&'a str]),
    /// Match rules with a specific enforcement kind.
    ByEnforcement(EnforcementKind),
    /// All rules in the set.
    All,
    /// OR of two sub-filters.
    Or(Box<Filter<'a>>, Box<Filter<'a>>),
    /// AND of two sub-filters.
    And(Box<Filter<'a>>, Box<Filter<'a>>),
}

/// Select a rule subset from `rule_set` according to `filter`.
///
/// Returns rules in corpus load order; does not deduplicate (if a rule
/// matches multiple branches of an `Or`, it appears once because we iterate
/// the full set once and test each rule).
///
/// This is a pure function: given the same `rule_set` + `filter`, it always
/// returns the same result.
pub fn select<'a>(rule_set: &'a RuleSet, filter: &Filter<'_>) -> Vec<&'a Rule> {
    rule_set
        .iter()
        .filter(|r| matches_filter(r, filter))
        .collect()
}

/// Pure predicate — does `rule` satisfy `filter`?
fn matches_filter(rule: &Rule, filter: &Filter<'_>) -> bool {
    match filter {
        Filter::All => true,
        Filter::ByIds(ids) => ids.iter().any(|id| id == &rule.id),
        Filter::ByDomain(d) => rule.domain == *d,
        Filter::ByDomains(ds) => ds.iter().any(|d| rule.domain == *d),
        Filter::ByEnforcement(kind) => rule.enforcement == *kind,
        Filter::Or(a, b) => matches_filter(rule, a) || matches_filter(rule, b),
        Filter::And(a, b) => matches_filter(rule, a) && matches_filter(rule, b),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Convenience: select by owned ids (useful when callers hold Vec<RuleId>)
// ────────────────────────────────────────────────────────────────────────────

/// Select rules matching any of `ids` from `rule_set`.
///
/// Shorthand for `select(rule_set, &Filter::ByIds(ids))`.
pub fn select_by_ids<'a>(rule_set: &'a RuleSet, ids: &[RuleId]) -> Vec<&'a Rule> {
    select(rule_set, &Filter::ByIds(ids))
}

/// Select rules for the given domains (including `"universal"` rules).
///
/// The `"universal"` domain (derived from the `universal/` corpus folder) is
/// always included so callers get universal rules automatically without needing
/// to mention `"universal"` explicitly.
pub fn select_for_domains<'a>(rule_set: &'a RuleSet, domains: &[&str]) -> Vec<&'a Rule> {
    rule_set
        .iter()
        .filter(|r| r.domain == "universal" || domains.iter().any(|d| r.domain == *d))
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────
// Role builder
// ────────────────────────────────────────────────────────────────────────────

/// Build a [`camerata_core::Role`] from the corpus at `corpus_path`.
///
/// Rules are selected using OR semantics across two axes:
///
/// 1. **Domain match** — any rule whose `domain` field appears in `domains`
///    (e.g. `"rust"`, `"sql"`, `"agentic"`).  Universal rules (`domain = "universal"`)
///    are always included regardless of what `domains` contains.
/// 2. **Explicit id override** — any rule whose id string appears in
///    `rule_ids` is included even if its domain was not requested.
///
/// `domains` and `rule_ids` may each be empty; an empty `domains` with an
/// empty `rule_ids` produces a role containing only universal rules.
///
/// # `allowed_paths` default
///
/// When no explicit domain-to-path mapping is needed the caller can pass an
/// empty slice.  The function derives a sensible default: one glob per domain
/// in `domains` (e.g. `"rust"` → `"**/*.rs"`), plus `"**"` for universal
/// coverage. Callers that need precise path restrictions should call
/// [`camerata_core::Role`] constructors directly after obtaining the
/// `rule_subset`.
///
/// # Errors
///
/// Propagates [`RulesError`] from the corpus loader (I/O or TOML parse
/// failures).  Consider [`load_corpus_lenient`] if you prefer to skip
/// malformed files rather than fail.
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use camerata_rules::{role_from_corpus, DEFAULT_CORPUS_PATH};
///
/// # async fn example() {
/// let role = role_from_corpus(
///     Path::new(DEFAULT_CORPUS_PATH),
///     "Backend",
///     &["rust", "sql", "agentic"],
///     &[],
/// )
/// .await
/// .unwrap();
///
/// assert!(!role.rule_subset.is_empty());
/// # }
/// ```
pub async fn role_from_corpus(
    corpus_path: &Path,
    role_name: &str,
    domains: &[&str],
    rule_ids: &[&str],
) -> Result<camerata_core::Role, RulesError> {
    let set = load_corpus(corpus_path).await?;

    // Collect matching rules: universal + domain-match + explicit-id override.
    let mut subset: Vec<RuleId> = set
        .iter()
        .filter(|r| {
            // Universal rules always included.
            if r.domain == "universal" {
                return true;
            }
            // Domain match.
            if domains.iter().any(|d| r.domain == *d) {
                return true;
            }
            // Explicit id override — allows pulling in rules from foreign
            // domains when the caller knows the exact id.
            if rule_ids.iter().any(|id| r.id.0 == *id) {
                return true;
            }
            false
        })
        .map(|r| r.id.clone())
        .collect();

    // Stable sort by id string for deterministic ordering.
    subset.sort_by(|a, b| a.0.cmp(&b.0));

    // Derive sensible allowed_paths from domains.
    let allowed_paths = derive_allowed_paths(domains);

    Ok(camerata_core::Role {
        name: role_name.to_owned(),
        rule_subset: subset,
        allowed_paths,
    })
}

/// Derive a default `allowed_paths` glob list from a domain slice.
///
/// Maps well-known domains to file-extension globs; unknown domains fall back
/// to `"**"` (all files).  The list always includes `"**"` so the role is
/// never inadvertently restricted to zero paths.
fn derive_allowed_paths(domains: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = domains.iter().map(|d| domain_to_glob(d)).collect();

    // Always add a universal catch-all so the role is usable even if the
    // domain mapping is incomplete.
    if !paths.contains(&"**".to_owned()) {
        paths.push("**".to_owned());
    }

    paths
}

/// Map a single domain string to a file-glob pattern.
fn domain_to_glob(domain: &str) -> String {
    // Handle sub-domain variants (e.g. "rust:dioxus", "rust:seaorm") by
    // using the primary component only.
    let primary = domain.split(':').next().unwrap_or(domain);
    match primary {
        "rust" => "**/*.rs".to_owned(),
        "sql" => "**/*.sql".to_owned(),
        "javascript" => "**/*.{js,ts,jsx,tsx}".to_owned(),
        // UI rules reach component files in every JSX/TSX dialect and the hooks and helpers
        // beside them (`.jsx`, `.tsx`, `.ts`, `.js`), plus stylesheets. It used to be
        // `**/*.{tsx,css}`, which left a role scoped to UI work unable to write `.jsx`.
        "ui" => "**/*.{jsx,tsx,ts,js,css}".to_owned(),
        "iac" => "**/*.tf".to_owned(),
        "ci-cd" => "**/.github/**".to_owned(),
        "go" => "**/*.go".to_owned(),
        "python" => "**/*.py".to_owned(),
        // Supabase rules span the CLI project config, SQL migrations (RLS/functions),
        // and the JS/TS client code that talks to it (auth/secrets/storage misuse).
        "supabase" => "**/*.{sql,ts,tsx,toml,env}".to_owned(),
        // Firebase rules span the Security Rules files (`.rules`), the project and Realtime
        // Database rules JSON, the Cloud Functions (JS/TS) and the Flutter client code.
        "firebase" => "**/*.{rules,json,js,ts,jsx,tsx,mjs,cjs,dart}".to_owned(),
        "universal" => "**".to_owned(),
        _ => {
            tracing::warn!(domain = %domain, "no file-glob mapping for domain; falling back to **");
            "**".to_owned()
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tests (ORCH-NEW-PATH-TESTS-1)
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_rule(id: &str, domain: &str, enforcement: EnforcementKind) -> Rule {
        Rule {
            id: RuleId(id.to_owned()),
            title: format!("Rule {id}"),
            enforcement,
            domain: domain.to_owned(),
            summary: format!("Summary of {id}"),
            decision_question: None,
            decision_why: None,
            options: Vec::new(),
            default_option: None,
            verification: Verification::Draft,
            sources: Vec::new(),
            verified: None,
            opt_in_only: false,
            layer3_only: false,
            stack_exceptions: Vec::new(),
            extra_domains: Vec::new(),
            kind: None,
            inspectable: None,
            max_severity: None,
            min_severity: None,
            deviation_ceiling: None,
            cwe: Vec::new(),
            asvs: Vec::new(),
            superseded_since: None,
            current_recommendation: None,
            version: None,
            last_reviewed: None,
            applies_to: None,
        }
    }

    #[test]
    fn corpus_path_honours_a_non_blank_override_and_ignores_a_blank_one() {
        assert_eq!(
            corpus_path_from(Some("/override/corpus".to_owned())),
            std::path::PathBuf::from("/override/corpus")
        );
        for blank in [None, Some(String::new()), Some("   ".to_owned())] {
            assert_eq!(
                corpus_path_from(blank),
                std::path::PathBuf::from(DEFAULT_CORPUS_PATH)
            );
        }
    }

    #[tokio::test]
    async fn corpus_rule_loads_options_and_default() {
        // ARCH-BOUNDARY-VALIDATION-1 has a default + three [[option]] alternatives.
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the camerata-ai checkout
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let Some(rule) = set.get_by_id("ARCH-BOUNDARY-VALIDATION-1") else {
            return; // rule not in this corpus version
        };
        assert!(rule.has_default(), "this rule ships an adopted default");
        assert!(
            rule.options.len() >= 2,
            "it has alternatives to choose among: {}",
            rule.options.len()
        );
        let default_id = rule.default_option.clone().unwrap();
        assert!(
            rule.options.iter().any(|o| o.id == default_id),
            "the default option id resolves to a real option"
        );
    }

    // ── Verification / provenance schema ──────────────────────────────────────

    /// Minimal raw-TOML deserialization target mirroring a corpus rule, used to
    /// prove the new fields round-trip without touching the real corpus files.
    fn parse_rule(toml_src: &str) -> Rule {
        // Reuse the production parser by going through RuleToml + the same field
        // wiring load_one uses. We deserialize RuleToml directly here because
        // load_one is file-based; the field mapping is identical.
        // Since domain is now Option<String> (derived from path in production),
        // we use the explicit value if present or fall back to "rust" as a
        // sentinel for tests that are checking other fields (not the domain).
        let raw: RuleToml = toml::from_str(toml_src).expect("rule TOML parses");
        Rule {
            id: RuleId(raw.id),
            title: raw.title,
            enforcement: raw.enforcement,
            domain: raw.domain.unwrap_or_else(|| "rust".to_string()),
            summary: String::new(),
            decision_question: None,
            decision_why: raw.decision.as_ref().and_then(|d| d.why.clone()),
            default_option: raw.decision.as_ref().and_then(|d| d.default.clone()),
            options: raw
                .options
                .into_iter()
                .map(|o| RuleOption {
                    id: o.id,
                    label: o.label,
                    directive: o.directive,
                    why: o.why,
                    remediation: o.remediation.filter(|s| !s.trim().is_empty()),
                    finding_headline: o.finding_headline.filter(|s| !s.trim().is_empty()),
                    finding_detail: o.finding_detail.filter(|s| !s.trim().is_empty()),
                    escalation: o.escalation,
                    effort: o.effort.filter(|s| !s.trim().is_empty()),
                    violates: o.violates,
                })
                .collect(),
            verification: raw.verification,
            sources: raw.sources,
            verified: raw.verified,
            opt_in_only: raw.opt_in_only,
            layer3_only: raw.layer3_only,
            stack_exceptions: raw
                .stack_exceptions
                .into_iter()
                .map(|se| StackException {
                    framework: se.framework,
                    path_glob: se.path_glob,
                    note: se.note,
                })
                .collect(),
            extra_domains: raw.extra_domains,
            kind: raw.kind,
            inspectable: raw.inspectable,
            max_severity: raw.max_severity.filter(|s| !s.trim().is_empty()),
            min_severity: raw.min_severity.filter(|s| !s.trim().is_empty()),
            deviation_ceiling: raw.deviation_ceiling.filter(|s| !s.trim().is_empty()),
            cwe: raw.cwe,
            asvs: raw.asvs,
            superseded_since: raw.superseded_since.filter(|s| !s.trim().is_empty()),
            current_recommendation: raw.current_recommendation.filter(|s| !s.trim().is_empty()),
            version: raw.version,
            last_reviewed: raw.last_reviewed.filter(|s| !s.trim().is_empty()),
            applies_to: raw.applies_to,
        }
    }

    #[test]
    fn grounded_rule_with_sources_round_trips() {
        let src = r#"
            id = "JAVA-FINAL-LOCAL-1"
            title = "Local variables are final where possible"
            enforcement = "structured"
            domain = "java"
            verification = "grounded"

            [[sources]]
            url = "https://google.github.io/styleguide/javaguide.html"
            title = "Google Java Style Guide"
            linter = "Checkstyle: FinalLocalVariable"

            [[sources]]
            url = "https://errcheck.dev"
            title = "errcheck docs"
        "#;
        let rule = parse_rule(src);
        assert_eq!(rule.verification(), Verification::Grounded);
        assert_eq!(rule.sources.len(), 2);
        assert_eq!(
            rule.sources[0].linter.as_deref(),
            Some("Checkstyle: FinalLocalVariable")
        );
        assert_eq!(rule.sources[0].title, "Google Java Style Guide");
        // The second source is doc-only: no linter.
        assert_eq!(rule.sources[1].linter, None);
        assert!(rule.is_grounded());
        assert!(rule.is_shippable());
    }

    #[test]
    fn rule_without_provenance_fields_defaults_to_draft() {
        let src = r#"
            id = "RULE-NO-PROV-1"
            title = "A rule that predates the provenance schema"
            enforcement = "prose"
            domain = "rust"
        "#;
        let rule = parse_rule(src);
        assert_eq!(rule.verification(), Verification::Draft);
        assert!(rule.sources.is_empty());
        assert!(!rule.is_grounded());
        assert!(!rule.is_shippable());
    }

    #[test]
    fn is_shippable_only_for_grounded_or_verified() {
        let mut r = make_rule("R-SHIP-1", "rust", EnforcementKind::Structured);
        // Default Draft → not shippable.
        assert_eq!(r.verification(), Verification::Draft);
        assert!(!r.is_shippable());
        assert!(!r.is_grounded());

        r.verification = Verification::Grounded;
        assert!(r.is_grounded());
        assert!(r.is_shippable());

        r.verification = Verification::Verified;
        assert!(r.is_grounded());
        assert!(r.is_shippable());
    }

    #[test]
    fn policy_round_trips_and_is_not_grounded() {
        // Wire round-trip.
        assert_eq!(Verification::from_tag("policy"), Some(Verification::Policy));
        assert_eq!(Verification::Policy.as_str(), "policy");
        assert_eq!(Verification::Policy.to_string(), "policy");

        // Policy is honest project policy, NOT grounded — same as draft, it is
        // kept out of the armed ruleset, but it is labeled honestly rather than
        // pretending to be grounded.
        assert!(!Verification::Policy.is_grounded());
        assert!(!Verification::Policy.is_shippable());
        assert!(!Verification::Policy.is_verified());
    }

    #[test]
    fn verification_str_round_trip() {
        for v in [
            Verification::Draft,
            Verification::Policy,
            Verification::Grounded,
            Verification::Verified,
        ] {
            assert_eq!(
                Verification::from_tag(v.as_str()),
                Some(v),
                "round-trip {v}"
            );
        }
        assert_eq!(Verification::from_tag("nonsense"), None);
        // Display matches the wire string.
        assert_eq!(Verification::Grounded.to_string(), "grounded");
    }

    #[test]
    fn verification_default_is_draft() {
        assert_eq!(Verification::default(), Verification::Draft);
    }

    #[test]
    fn needs_recheck_round_trips_and_is_grounded_not_verified() {
        // Wire round-trip.
        assert_eq!(
            Verification::from_tag("needs_recheck"),
            Some(Verification::NeedsRecheck)
        );
        assert_eq!(Verification::NeedsRecheck.as_str(), "needs_recheck");
        assert_eq!(Verification::NeedsRecheck.to_string(), "needs_recheck");

        // is_verified() is true ONLY for Verified.
        assert!(Verification::Verified.is_verified());
        assert!(!Verification::NeedsRecheck.is_verified());
        assert!(!Verification::Grounded.is_verified());
        assert!(!Verification::Draft.is_verified());

        // is_grounded()/is_shippable() are true for Grounded, Verified, NeedsRecheck.
        for v in [
            Verification::Grounded,
            Verification::Verified,
            Verification::NeedsRecheck,
        ] {
            assert!(v.is_grounded(), "{v} should be at-least-grounded");
            assert!(v.is_shippable(), "{v} should be shippable");
        }
        // Draft is neither.
        assert!(!Verification::Draft.is_grounded());
        assert!(!Verification::Draft.is_shippable());
    }

    #[test]
    fn needs_recheck_deserializes_from_toml() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            verification: Verification,
        }
        let w: Wrapper = toml::from_str(r#"verification = "needs_recheck""#)
            .expect("needs_recheck should deserialize");
        assert_eq!(w.verification, Verification::NeedsRecheck);
    }

    #[test]
    fn verified_provenance_table_round_trips() {
        let src = r#"
            id = "RULE-VERIFIED-1"
            title = "A human-verified rule"
            enforcement = "mechanical"
            domain = "rust"
            verification = "verified"

            [verified]
            by = "zach"
            at = "2026-06-20"
            against = ["clippy 1.83", "Google Java Style Guide 2024-01"]
        "#;
        let rule = parse_rule(src);
        assert_eq!(rule.verification(), Verification::Verified);
        assert!(rule.is_verified());
        let prov = rule.verified.as_ref().expect("[verified] table present");
        assert_eq!(prov.by, "zach");
        assert_eq!(prov.at, "2026-06-20");
        assert_eq!(
            prov.against,
            vec!["clippy 1.83", "Google Java Style Guide 2024-01"]
        );
    }

    #[test]
    fn verified_provenance_absent_yields_none() {
        let src = r#"
            id = "RULE-NO-VERIFIED-1"
            title = "A rule with no [verified] table"
            enforcement = "structured"
            domain = "rust"
            verification = "grounded"
        "#;
        let rule = parse_rule(src);
        assert!(rule.verified.is_none(), "absent [verified] table → None");
        assert!(!rule.is_verified());
        assert!(rule.is_grounded());
    }

    #[test]
    fn verified_provenance_against_defaults_to_empty() {
        let src = r#"
            id = "RULE-VERIFIED-NOAGAINST-1"
            title = "Verified but no versions pinned"
            enforcement = "structured"
            domain = "rust"
            verification = "verified"

            [verified]
            by = "zach"
            at = "2026-06-20"
        "#;
        let rule = parse_rule(src);
        let prov = rule.verified.as_ref().expect("[verified] present");
        assert!(prov.against.is_empty(), "against defaults to empty");
    }

    #[tokio::test]
    async fn full_corpus_loads_with_provenance_defaults() {
        // Backstop: the entire bundled corpus still loads after adding the
        // additive provenance fields, and every rule has a verification status
        // (defaulting to Draft for the existing un-grounded corpus).
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        assert!(set.len() >= 50, "expected >= 50 rules, got {}", set.len());
        for rule in set.iter() {
            // Accessor is callable on every rule; nothing panics.
            let _ = rule.verification();
            let _ = rule.is_shippable();
        }

        // After the *→universal refactor: no rule should have domain="*".
        let star_rules: Vec<_> = set.iter().filter(|r| r.domain == "*").collect();
        assert!(
            star_rules.is_empty(),
            "no corpus rules should have domain='*' after refactor; found: {:?}",
            star_rules.iter().map(|r| r.id_str()).collect::<Vec<_>>()
        );
        // The universal domain should be non-empty if the corpus has universal/ rules.
        if set.len() > 50 {
            let universal_rules: Vec<_> = set.iter().filter(|r| r.domain == "universal").collect();
            assert!(
                !universal_rules.is_empty(),
                "expected universal rules to load as 'universal' domain"
            );
        }
    }

    /// C4-P3 corpus-LOAD gate (dev/CI only — this is a test, not a runtime refusal): every rule
    /// that resolves a DEFAULT option (the option a finding against it binds to when the
    /// project hasn't configured its own `chosen_option`) must carry authored, non-empty
    /// `remediation` text on that option. Without this, a curated finding's "Fix:" line is
    /// silently omitted (`report_export::resolve_fix`'s honest-gap degrade) for a rule nobody
    /// ever finished authoring — exactly the gap that let proven criticals ship with no fix text
    /// to fall back to when per-finding fix generation failed (item 4). Failing this test names
    /// the offending rule id(s) so the fix is "author the TOML", never a runtime workaround.
    ///
    /// Scope note: a rule with NO default option (a pure architect-must-choose stance decision —
    /// branch-naming convention, API versioning scheme, …) is intentionally OUT of scope here.
    /// No finding ever resolves a Fix line for such a rule until a project explicitly configures
    /// a `chosen_option`, and several of these rules legitimately offer a "no policy adopted"
    /// alternative with no remediation (there is nothing to fix if the project opts out of the
    /// rule entirely) — gating on every option would force authoring remediation for alternatives
    /// that, by design, flag nothing.
    #[tokio::test]
    async fn every_rule_with_a_default_option_has_authored_remediation() {
        let path = corpus_path();
        let set = load_corpus(&path).await.expect("corpus must load cleanly");
        let offenders: Vec<String> = set
            .iter()
            .filter_map(|rule| {
                let default_id = rule.default_option.as_deref()?;
                let option = rule.options.iter().find(|o| o.id == default_id)?;
                let has_remediation = option
                    .remediation
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                (!has_remediation)
                    .then(|| format!("{} (default option {default_id:?})", rule.id_str()))
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "every rule's default option must carry authored remediation text, so a curated \
             finding against it always ships a Fix line — missing on: {offenders:#?}"
        );
    }

    fn populated_set() -> RuleSet {
        let mut set = RuleSet::default();
        set.push(make_rule(
            "RUST-DOMAIN-1",
            "rust",
            EnforcementKind::Structured,
        ));
        set.push(make_rule(
            "RUST-DOMAIN-4",
            "rust",
            EnforcementKind::Structured,
        ));
        set.push(make_rule(
            "ORCH-NEW-PATH-TESTS-1",
            "agentic",
            EnforcementKind::Mechanical,
        ));
        set.push(make_rule(
            "SPIRIT-OPTIMIZE-1",
            "universal",
            EnforcementKind::Prose,
        ));
        set.push(make_rule(
            "ARCH-STRICT-LAYERING-1",
            "api-layer",
            EnforcementKind::Mechanical,
        ));
        set
    }

    // ── RuleSet indexing ─────────────────────────────────────────────────────

    #[test]
    fn ruleset_get_by_id_found() {
        let set = populated_set();
        let rule = set.get_by_id("RUST-DOMAIN-4").expect("should find rule");
        assert_eq!(rule.id_str(), "RUST-DOMAIN-4");
    }

    #[test]
    fn ruleset_get_by_id_missing() {
        let set = populated_set();
        assert!(set.get_by_id("DOES-NOT-EXIST").is_none());
    }

    #[test]
    fn ruleset_get_by_domain_returns_correct_subset() {
        let set = populated_set();
        let rust_rules = set.get_by_domain("rust");
        assert_eq!(rust_rules.len(), 2);
        assert!(rust_rules.iter().all(|r| r.domain == "rust"));
    }

    #[test]
    fn ruleset_get_by_domain_missing_returns_empty() {
        let set = populated_set();
        assert!(set.get_by_domain("nonexistent-domain").is_empty());
    }

    // ── ruleset_with_unauthored_rule (test-support constructor) ────────────────

    #[test]
    fn ruleset_with_unauthored_rule_resolves_directive_but_no_remediation() {
        let set = ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
        let rule = set
            .get_by_id("SEC-TEST-UNAUTHORED-1")
            .expect("synthetic rule must be indexed by id");
        let option = rule
            .resolved_option(None)
            .expect("synthetic rule must ship a resolvable default option");
        assert!(
            !option.directive.trim().is_empty(),
            "synthetic option must carry a real, non-empty directive"
        );
        assert_eq!(
            option.remediation, None,
            "synthetic option's remediation must be unauthored (None)"
        );
    }

    #[test]
    fn ruleset_len_reflects_all_rules() {
        let set = populated_set();
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn ruleset_domains_includes_all_expected() {
        let set = populated_set();
        let mut domains: Vec<&str> = set.domains().collect();
        domains.sort_unstable();
        assert!(domains.contains(&"rust"));
        assert!(domains.contains(&"agentic"));
        assert!(domains.contains(&"universal"));
        assert!(domains.contains(&"api-layer"));
    }

    // ── Filter::All ──────────────────────────────────────────────────────────

    #[test]
    fn select_all_returns_full_set() {
        let set = populated_set();
        let result = select(&set, &Filter::All);
        assert_eq!(result.len(), set.len());
    }

    // ── Filter::ByIds ────────────────────────────────────────────────────────

    #[test]
    fn select_by_ids_exact_match() {
        let set = populated_set();
        let ids = vec![RuleId("RUST-DOMAIN-1".to_owned())];
        let result = select(&set, &Filter::ByIds(&ids));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id_str(), "RUST-DOMAIN-1");
    }

    #[test]
    fn select_by_ids_no_match_returns_empty() {
        let set = populated_set();
        let ids = vec![RuleId("NONEXISTENT".to_owned())];
        let result = select(&set, &Filter::ByIds(&ids));
        assert!(result.is_empty());
    }

    #[test]
    fn select_by_ids_multiple_ids() {
        let set = populated_set();
        let ids = vec![
            RuleId("RUST-DOMAIN-1".to_owned()),
            RuleId("ORCH-NEW-PATH-TESTS-1".to_owned()),
        ];
        let result = select(&set, &Filter::ByIds(&ids));
        assert_eq!(result.len(), 2);
    }

    // ── Filter::ByDomain ─────────────────────────────────────────────────────

    #[test]
    fn select_by_domain_returns_matching() {
        let set = populated_set();
        let result = select(&set, &Filter::ByDomain("agentic"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id_str(), "ORCH-NEW-PATH-TESTS-1");
    }

    // ── Filter::ByDomains ────────────────────────────────────────────────────

    #[test]
    fn select_by_domains_combines_multiple() {
        let set = populated_set();
        let result = select(&set, &Filter::ByDomains(&["rust", "agentic"]));
        assert_eq!(result.len(), 3);
    }

    // ── Filter::ByEnforcement ────────────────────────────────────────────────

    #[test]
    fn select_by_enforcement_mechanical() {
        let set = populated_set();
        let result = select(&set, &Filter::ByEnforcement(EnforcementKind::Mechanical));
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .all(|r| r.enforcement == EnforcementKind::Mechanical));
    }

    #[test]
    fn select_by_enforcement_prose() {
        let set = populated_set();
        let result = select(&set, &Filter::ByEnforcement(EnforcementKind::Prose));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id_str(), "SPIRIT-OPTIMIZE-1");
    }

    // ── Filter::Or ───────────────────────────────────────────────────────────

    #[test]
    fn select_or_combines_results_without_duplication() {
        let set = populated_set();
        let filter = Filter::Or(
            Box::new(Filter::ByDomain("rust")),
            Box::new(Filter::ByDomain("agentic")),
        );
        let result = select(&set, &filter);
        // rust=2, agentic=1 — no overlap → 3
        assert_eq!(result.len(), 3);
    }

    // ── Filter::And ──────────────────────────────────────────────────────────

    #[test]
    fn select_and_narrows_results() {
        let set = populated_set();
        let filter = Filter::And(
            Box::new(Filter::ByDomain("rust")),
            Box::new(Filter::ByEnforcement(EnforcementKind::Structured)),
        );
        let result = select(&set, &filter);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|r| r.domain == "rust"));
    }

    // ── select_by_ids convenience ─────────────────────────────────────────────

    #[test]
    fn select_by_ids_convenience_fn() {
        let set = populated_set();
        let ids = vec![RuleId("SPIRIT-OPTIMIZE-1".to_owned())];
        let result = select_by_ids(&set, &ids);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].domain, "universal");
    }

    // ── select_for_domains (universal inclusion) ──────────────────────────────

    #[test]
    fn select_for_domains_includes_universal_rules() {
        let set = populated_set();
        // ask for "agentic" only — universal "universal" should appear too
        let result = select_for_domains(&set, &["agentic"]);
        let ids: Vec<&str> = result.iter().map(|r| r.id_str()).collect();
        assert!(
            ids.contains(&"ORCH-NEW-PATH-TESTS-1"),
            "agentic rule present"
        );
        assert!(
            ids.contains(&"SPIRIT-OPTIMIZE-1"),
            "universal rule included"
        );
        // rust and api-layer rules must NOT appear
        assert!(!ids.contains(&"RUST-DOMAIN-1"));
        assert!(!ids.contains(&"ARCH-STRICT-LAYERING-1"));
    }

    #[test]
    fn select_for_domains_empty_domain_list_returns_only_universals() {
        let set = populated_set();
        let result = select_for_domains(&set, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].domain, "universal");
    }

    // ── EnforcementKind Display ────────────────────────────────────────────

    #[test]
    fn enforcement_kind_display() {
        assert_eq!(EnforcementKind::Prose.to_string(), "prose");
        assert_eq!(EnforcementKind::Structured.to_string(), "structured");
        assert_eq!(EnforcementKind::Mechanical.to_string(), "mechanical");
        assert_eq!(EnforcementKind::Architectural.to_string(), "architectural");
    }

    #[test]
    fn enforcement_kind_str_round_trip() {
        for kind in [
            EnforcementKind::Prose,
            EnforcementKind::Structured,
            EnforcementKind::Mechanical,
            EnforcementKind::Architectural,
        ] {
            let s = kind.as_str();
            assert_eq!(
                EnforcementKind::from_tag(s),
                Some(kind.clone()),
                "round-trip for {s}"
            );
        }
        assert_eq!(EnforcementKind::from_tag("nonsense"), None);
    }

    #[test]
    fn enforcement_kind_toml_deserializes_architectural() {
        // The serde rename must accept the lowercase "architectural" tag from a
        // corpus TOML file.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            enforcement: EnforcementKind,
        }
        let w: Wrapper = toml::from_str(r#"enforcement = "architectural""#)
            .expect("architectural tier should deserialize");
        assert_eq!(w.enforcement, EnforcementKind::Architectural);
    }

    #[test]
    fn enforcement_kind_tier_partitioning() {
        // Architectural is CI-enforced (like mechanical) and citable in CONVENTIONS.md.
        assert!(EnforcementKind::Architectural.is_ci_enforced());
        assert!(EnforcementKind::Mechanical.is_ci_enforced());
        assert!(!EnforcementKind::Structured.is_ci_enforced());
        assert!(!EnforcementKind::Prose.is_ci_enforced());

        assert!(EnforcementKind::Architectural.emits_to_conventions());
        assert!(EnforcementKind::Mechanical.emits_to_conventions());
        assert!(EnforcementKind::Structured.emits_to_conventions());
        assert!(!EnforcementKind::Prose.emits_to_conventions());
    }

    // ── Async corpus loader (integration test against real corpus) ────────────

    #[tokio::test]
    async fn load_corpus_loads_real_corpus() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            // Skip if corpus not present (CI without the camerata-ai checkout).
            return;
        }
        let set = load_corpus(path).await.expect("corpus should load");
        // We know the corpus has 107 files; assert a reasonable lower bound.
        assert!(
            set.len() >= 50,
            "expected at least 50 rules, got {}",
            set.len()
        );
        // Every rule should have a non-empty id.
        for rule in set.iter() {
            assert!(!rule.id.0.is_empty(), "empty id in rule {:?}", rule.title);
        }
    }

    #[tokio::test]
    async fn corpus_loads_process_vcs_metadata_rules() {
        // The four VCS-gate process rules live in `principles/process/`, so their
        // domain must derive to "process". They are architectural + opt-in only
        // (deliberate opt-in, like the CI security rules — never pre-checked).
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        for id in [
            "PROCESS-CONVENTIONAL-COMMIT-1",
            "PROCESS-COMMIT-DOC-1",
            "PROCESS-BRANCH-NAMING-1",
            "PROCESS-ADO-LINK-1",
        ] {
            let rule = set
                .get_by_id(id)
                .unwrap_or_else(|| panic!("{id} must load from the corpus"));
            assert_eq!(
                rule.domain, "process",
                "{id} must derive domain 'process' from its folder"
            );
            assert_eq!(
                rule.enforcement,
                EnforcementKind::Architectural,
                "{id} must be the Architectural tier"
            );
            assert!(
                rule.opt_in_only,
                "{id} must be opt-in only (never pre-checked)"
            );
            // The caveat that distinguishes these from the usual SSOT layer-2+4
            // pattern must be present in the rule summary.
            assert!(
                rule.summary.contains("NOT enforced at layer 2"),
                "{id} summary must carry the layer-2 caveat"
            );
        }
    }

    #[tokio::test]
    async fn corpus_loads_architectural_tier_rules() {
        // The bundled corpus ships the example Architectural-tier rules; loading
        // them exercises the serde rename round-trip against real files.
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let Some(rule) = set.get_by_id("ARCH-HANDLER-NO-DB-1") else {
            return; // rule not in this corpus version
        };
        assert_eq!(
            rule.enforcement,
            EnforcementKind::Architectural,
            "ARCH-HANDLER-NO-DB-1 must load as the Architectural tier"
        );
        assert!(rule.enforcement.is_ci_enforced());
        assert!(rule.enforcement.emits_to_conventions());

        // Confirm the tier participates in enforcement-based selection.
        let arch = select(&set, &Filter::ByEnforcement(EnforcementKind::Architectural));
        assert!(
            arch.iter().any(|r| r.id_str() == "ARCH-HANDLER-NO-DB-1"),
            "Architectural filter must surface the rule"
        );
    }

    #[tokio::test]
    async fn load_corpus_lenient_skips_bad_files_but_loads_good_ones() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let (set, errors) = load_corpus_lenient(path).await;
        // Lenient load should succeed on every well-formed file.
        assert!(
            set.len() >= 50,
            "expected at least 50 rules in lenient load, got {}",
            set.len()
        );
        // The real corpus should be clean.
        assert!(errors.is_empty(), "unexpected parse errors: {errors:#?}");
    }

    #[test]
    fn top_level_domains_strips_subdomains_dedups_and_sorts() {
        let mut set = RuleSet::default();
        for (id, domain) in [
            ("A-1", "rust:seaorm"),
            ("B-1", "rust"),
            ("C-1", "supabase:auth"),
            ("D-1", "supabase:rls"),
            ("E-1", "universal"),
        ] {
            set.push(bare_rule(id, domain));
        }
        assert_eq!(
            set.top_level_domains(),
            vec![
                "rust".to_string(),
                "supabase".to_string(),
                "universal".to_string()
            ]
        );
        assert!(RuleSet::default().top_level_domains().is_empty());
    }

    #[tokio::test]
    async fn load_corpus_domain_index_consistent_with_iter() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        // Every rule returned by iter() must be reachable via get_by_id.
        for rule in set.iter() {
            let found = set
                .get_by_id(rule.id_str())
                .expect("iter rule must be in id index");
            assert_eq!(found.id.0, rule.id.0);
        }
        // Every rule returned by iter() must appear in its domain bucket.
        for rule in set.iter() {
            let bucket = set.get_by_domain(&rule.domain);
            let in_bucket = bucket.iter().any(|r| r.id.0 == rule.id.0);
            assert!(
                in_bucket,
                "rule {} (domain={}) not found in domain bucket",
                rule.id.0, rule.domain
            );
        }
    }

    // ── role_from_corpus (integration test against real corpus) ──────────────

    #[tokio::test]
    async fn role_from_corpus_backend_has_rust_domain_2() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            // Skip when corpus is not present (CI without camerata-ai checkout).
            return;
        }

        let role = role_from_corpus(path, "Backend", &["rust", "sql", "agentic"], &[])
            .await
            .expect("role_from_corpus should succeed");

        assert_eq!(role.name, "Backend");

        // rule_subset must be non-empty.
        assert!(
            !role.rule_subset.is_empty(),
            "expected non-empty rule_subset for Backend role"
        );

        // Must contain at least the well-known RUST-DOMAIN-2 rule.
        let known_id = RuleId("RUST-DOMAIN-2".to_owned());
        assert!(
            role.rule_subset.contains(&known_id),
            "expected RUST-DOMAIN-2 in Backend rule_subset; got {:?}",
            role.rule_subset
        );

        // allowed_paths must include at least one entry.
        assert!(
            !role.allowed_paths.is_empty(),
            "expected non-empty allowed_paths"
        );

        // Report the subset size for the caller.
        eprintln!(
            "[role_from_corpus test] Backend rule_subset size = {}",
            role.rule_subset.len()
        );
    }

    #[tokio::test]
    async fn role_from_corpus_explicit_rule_id_override() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }

        // Ask for no domains but pull in RUST-DOMAIN-2 by explicit id.
        let role = role_from_corpus(path, "Targeted", &[], &["RUST-DOMAIN-2"])
            .await
            .expect("role_from_corpus should succeed");

        let known_id = RuleId("RUST-DOMAIN-2".to_owned());
        assert!(
            role.rule_subset.contains(&known_id),
            "explicit rule_id override should include RUST-DOMAIN-2"
        );
    }

    #[tokio::test]
    async fn role_from_corpus_empty_args_returns_only_universals() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }

        let role = role_from_corpus(path, "Universal", &[], &[])
            .await
            .expect("role_from_corpus should succeed");

        // With no domains / ids the subset must consist only of universal rules.
        let set = load_corpus(path).await.expect("corpus loads");
        let universal_count = set.iter().filter(|r| r.domain == "universal").count();
        assert_eq!(
            role.rule_subset.len(),
            universal_count,
            "expected only universal rules when no domains specified"
        );
    }

    // ── domain_to_glob (via derive_allowed_paths) ─────────────────────────────

    #[test]
    fn derive_allowed_paths_always_appends_star_star_catch_all() {
        // Even for a known domain, the list must include "**" at the end so the
        // role is never inadvertently path-restricted to zero files.
        let paths = derive_allowed_paths(&["rust"]);
        assert!(
            paths.contains(&"**".to_string()),
            "rust domain must still have ** catch-all: {paths:?}"
        );
        // For an unknown domain the only entry returned is "**".
        let unknown = derive_allowed_paths(&["unknown-lang"]);
        assert!(unknown.contains(&"**".to_string()));
    }

    #[test]
    fn derive_allowed_paths_maps_known_domains() {
        // Spot-check that well-known domains produce their expected glob.
        let paths = derive_allowed_paths(&["rust"]);
        assert!(paths.contains(&"**/*.rs".to_string()), "{paths:?}");

        let paths = derive_allowed_paths(&["sql"]);
        assert!(paths.contains(&"**/*.sql".to_string()), "{paths:?}");

        let paths = derive_allowed_paths(&["javascript"]);
        assert!(
            paths.contains(&"**/*.{js,ts,jsx,tsx}".to_string()),
            "{paths:?}"
        );

        let paths = derive_allowed_paths(&["iac"]);
        assert!(paths.contains(&"**/*.tf".to_string()), "{paths:?}");

        let paths = derive_allowed_paths(&["go"]);
        assert!(paths.contains(&"**/*.go".to_string()), "{paths:?}");

        let paths = derive_allowed_paths(&["python"]);
        assert!(paths.contains(&"**/*.py".to_string()), "{paths:?}");

        // Firebase rules span the rules files, the project and rules JSON, the Cloud Functions
        // (JS/TS) and the Flutter client code that carries the Firebase options.
        let paths = derive_allowed_paths(&["firebase"]);
        assert!(
            paths.contains(&"**/*.{rules,json,js,ts,jsx,tsx,mjs,cjs,dart}".to_string()),
            "{paths:?}"
        );
    }

    #[test]
    fn domain_to_glob_subdomain_variant_strips_suffix() {
        // Sub-domains like "rust:dioxus" must map to the primary component's glob,
        // not fall through to the "**" catch-all.
        assert_eq!(domain_to_glob("rust:dioxus"), "**/*.rs");
        assert_eq!(domain_to_glob("rust:seaorm"), "**/*.rs");
        assert_eq!(domain_to_glob("sql:postgres"), "**/*.sql");
        assert_eq!(domain_to_glob("go:fiber"), "**/*.go");
    }

    #[test]
    fn domain_to_glob_supabase_and_its_subdomains_map_to_supabase_glob() {
        // The corpus's supabase rules all live under `supabase:<area>` (rls, auth,
        // secrets, storage, database-functions, exposure) — every one must map through
        // the primary-component strip to the same non-"**" glob, not fall back with a
        // warning.
        assert_eq!(domain_to_glob("supabase"), "**/*.{sql,ts,tsx,toml,env}");
        for sub in [
            "supabase:rls",
            "supabase:auth",
            "supabase:secrets",
            "supabase:storage",
            "supabase:database-functions",
            "supabase:exposure",
        ] {
            assert_eq!(
                domain_to_glob(sub),
                "**/*.{sql,ts,tsx,toml,env}",
                "{sub} must map to the supabase glob"
            );
        }
    }

    #[test]
    fn domain_to_glob_ui_covers_jsx_and_the_other_component_dialects() {
        // `ui` feeds an emitted `Role.allowed_paths`; the old `**/*.{tsx,css}` missed `.jsx`,
        // so a role scoped to UI work could not write a React `.jsx` component.
        let glob = domain_to_glob("ui");
        assert_eq!(glob, "**/*.{jsx,tsx,ts,js,css}");
        for ext in ["jsx", "tsx", "ts", "js", "css"] {
            assert!(glob.contains(ext), "{ext} must be covered by {glob}");
        }
        // Sub-domains of ui map the same way.
        assert_eq!(domain_to_glob("ui:forms"), glob);
        let paths = derive_allowed_paths(&["ui"]);
        assert!(paths.contains(&glob), "{paths:?}");
    }

    #[test]
    fn domain_to_glob_unknown_domain_returns_double_star() {
        assert_eq!(domain_to_glob("my-custom-domain"), "**");
        assert_eq!(domain_to_glob(""), "**");
    }

    #[test]
    fn derive_allowed_paths_does_not_duplicate_star_star() {
        // When the only domain is "unknown" (which maps to "**"), we should get
        // exactly one "**", not two.
        let paths = derive_allowed_paths(&["unknown-lang"]);
        let count = paths.iter().filter(|p| p.as_str() == "**").count();
        assert_eq!(count, 1, "should not duplicate the ** catch-all: {paths:?}");
    }

    #[test]
    fn derive_allowed_paths_empty_domains_returns_only_catch_all() {
        let paths = derive_allowed_paths(&[]);
        assert_eq!(paths, vec!["**".to_string()]);
    }

    // ── Rule::has_default + id_str convenience methods ───────────────────────

    #[test]
    fn rule_has_default_reflects_default_option_field() {
        let mut r = make_rule("R1", "rust", EnforcementKind::Structured);
        assert!(!r.has_default());
        r.default_option = Some("opt-a".to_string());
        assert!(r.has_default());
    }

    #[test]
    fn rule_id_str_returns_inner_string() {
        let r = make_rule("MY-RULE-1", "rust", EnforcementKind::Prose);
        assert_eq!(r.id_str(), "MY-RULE-1");
    }

    // ── Rule::is_auto_recommended ─────────────────────────────────────────────

    #[test]
    fn is_auto_recommended_true_for_grounded() {
        let mut r = make_rule("R-GROUND-1", "rust", EnforcementKind::Structured);
        r.verification = Verification::Grounded;
        assert!(
            r.is_auto_recommended(),
            "grounded rule must be auto-recommended"
        );
    }

    #[test]
    fn is_auto_recommended_true_for_verified() {
        let mut r = make_rule("R-VER-1", "rust", EnforcementKind::Structured);
        r.verification = Verification::Verified;
        assert!(
            r.is_auto_recommended(),
            "verified rule must be auto-recommended"
        );
    }

    #[test]
    fn is_auto_recommended_false_for_draft() {
        let r = make_rule("R-DRAFT-1", "rust", EnforcementKind::Prose);
        // make_rule defaults to Draft.
        assert_eq!(r.verification, Verification::Draft);
        assert!(
            !r.is_auto_recommended(),
            "draft rule must NOT be auto-recommended"
        );
    }

    #[test]
    fn is_auto_recommended_false_for_needs_recheck() {
        let mut r = make_rule("R-RECHECK-1", "rust", EnforcementKind::Structured);
        r.verification = Verification::NeedsRecheck;
        assert!(
            !r.is_auto_recommended(),
            "needs_recheck rule must NOT be auto-recommended (stale verification)"
        );
    }

    // ── opt_in_only / layer3_only flags ──────────────────────────────────────

    #[test]
    fn opt_in_only_and_layer3_only_default_false() {
        // A rule TOML that omits both flags loads them as false.
        let src = r#"
            id = "RULE-NO-FLAGS-1"
            title = "A rule without the CI-tier flags"
            enforcement = "mechanical"
            domain = "ci-cd"
        "#;
        let rule = parse_rule(src);
        assert!(!rule.is_opt_in_only(), "opt_in_only defaults to false");
        assert!(!rule.is_layer3_only(), "layer3_only defaults to false");
    }

    #[test]
    fn opt_in_only_and_layer3_only_parse_when_set() {
        let src = r#"
            id = "RULE-FLAGS-1"
            title = "A rule that sets both CI-tier flags"
            enforcement = "mechanical"
            domain = "ci-cd"
            opt_in_only = true
            layer3_only = true
        "#;
        let rule = parse_rule(src);
        assert!(rule.is_opt_in_only(), "opt_in_only = true parses");
        assert!(rule.is_layer3_only(), "layer3_only = true parses");
    }

    // ── P7: stack exceptions ────────────────────────────────────────────────────────

    #[test]
    fn glob_match_matches_a_trailing_double_star_directory() {
        assert!(glob_match(
            "supabase/functions/**",
            "supabase/functions/send-invite/index.ts"
        ));
        assert!(glob_match("supabase/functions/**", "supabase/functions/x"));
        assert!(!glob_match(
            "supabase/functions/**",
            "supabase/migrations/1.sql"
        ));
    }

    #[test]
    fn glob_match_matches_a_mid_pattern_wildcard() {
        assert!(glob_match("app/api/*/route.ts", "app/api/users/route.ts"));
        assert!(!glob_match("app/api/*/route.ts", "app/api/users/route.js"));
    }

    #[test]
    fn glob_match_exact_pattern_requires_exact_text() {
        assert!(glob_match("exact/path.rs", "exact/path.rs"));
        assert!(!glob_match("exact/path.rs", "exact/path.rs.bak"));
        assert!(!glob_match("exact/path.rs", "exact/pathXrs"));
    }

    #[test]
    fn no_stack_exceptions_declared_means_no_exception_ever_applies() {
        let rule = make_rule("SOME-RULE-1", "fullstack", EnforcementKind::Structured);
        assert!(rule
            .stack_exception_for(&["Supabase"], "supabase/functions/x/index.ts")
            .is_none());
    }

    /// The corpus-declared shape: a rule whose `[[stack_exception]]` block requires BOTH the
    /// framework AND the path glob to match. Proves parsing round-trips through `parse_rule`
    /// (the same field wiring `load_one` uses in production).
    #[test]
    fn stack_exception_parses_and_requires_both_framework_and_path_match() {
        let src = r#"
            id = "ARCH-EXAMPLE-STACK-EXCEPTION-1"
            title = "Example rule with a stack exception"
            enforcement = "structured"
            domain = "fullstack"

            [[stack_exception]]
            framework = "Supabase"
            path_glob = "supabase/functions/**"
            note = "Supabase Edge Functions are the idiomatic second deployable for webhooks."
        "#;
        let rule = parse_rule(src);
        assert_eq!(rule.stack_exceptions.len(), 1);
        assert_eq!(rule.stack_exceptions[0].framework, "Supabase");

        // Framework present AND path matches -> excepted.
        assert!(rule
            .stack_exception_for(&["Supabase", "Next.js"], "supabase/functions/x/index.ts")
            .is_some());
        // Framework present but path does NOT match the glob -> not excepted.
        assert!(rule
            .stack_exception_for(&["Supabase"], "src/main.rs")
            .is_none());
        // Path matches but the framework is NOT in the detected stack -> not excepted (proves
        // the mechanism needs BOTH conditions, not just a path match).
        assert!(rule
            .stack_exception_for(&["Next.js"], "supabase/functions/x/index.ts")
            .is_none());
    }

    /// GENERALITY: the mechanism is not hardcoded to Supabase/monolith-first — an entirely
    /// different rule + framework pair works identically, proving a rule can declare stack
    /// exceptions for ANY platform pattern, not just the one wired instance.
    #[test]
    fn stack_exception_mechanism_generalizes_to_a_different_rule_and_framework() {
        let src = r#"
            id = "SOME-OTHER-RULE-1"
            title = "A rule unrelated to monolith-first or Supabase"
            enforcement = "structured"
            domain = "javascript"

            [[stack_exception]]
            framework = "Next.js"
            path_glob = "app/api/**/route.ts"
            note = "Route Handlers are the idiomatic REST surface in the Next.js App Router."
        "#;
        let rule = parse_rule(src);
        assert!(rule
            .stack_exception_for(&["Next.js"], "app/api/users/route.ts")
            .is_some());
        assert!(rule
            .stack_exception_for(&["Next.js"], "pages/api/users.ts")
            .is_none());
        assert!(rule
            .stack_exception_for(&["Vue"], "app/api/users/route.ts")
            .is_none());
    }

    #[test]
    fn escalation_is_option_scoped_and_respects_the_selection() {
        let src = r#"
            id = "AGENTIC-EXAMPLE-1"
            title = "Example: an escalating option alongside a non-escalating one"
            enforcement = "prose"
            domain = "agentic"
            [decision]
            question = "How are test edits handled?"
            default = "escalate-on-test-edit"

            [[option]]
            id = "escalate-on-test-edit"
            label = "Escalate before editing tests"
            directive = "Stop and escalate to a human."
            why = "A human should confirm the edit is legitimate."
            escalation = { condition = "the change would modify or delete an existing test", severity = "hard-pause" }

            [[option]]
            id = "allow-test-edits"
            label = "Allow test edits"
            directive = "Proceed without escalation."
            why = "Trusted team."
        "#;
        let rule = parse_rule(src);
        assert!(
            rule.has_escalating_option(),
            "the rule offers an escalating option"
        );

        // Default (no explicit selection) → resolves to the escalate option → spec is active.
        let by_default = rule
            .selected_escalation(None)
            .expect("default option escalates");
        assert_eq!(by_default.severity, EscalationSeverity::HardPause);
        assert!(by_default.condition.contains("existing test"));

        // Explicitly choosing the escalate option → active.
        assert!(rule
            .selected_escalation(Some("escalate-on-test-edit"))
            .is_some());

        // THE KEY CORRECTNESS POINT: choosing the NON-escalating option → no escalation.
        assert!(
            rule.selected_escalation(Some("allow-test-edits")).is_none(),
            "selecting a non-escalating option must NOT escalate"
        );

        // Severity defaults to hard-pause when omitted from the inline table.
        let src2 = r#"
            id = "X-1"
            title = "t"
            enforcement = "prose"
            domain = "agentic"
            [[option]]
            id = "o1"
            label = "l"
            escalation = { condition = "c" }
        "#;
        let r2 = parse_rule(src2);
        assert_eq!(
            r2.selected_escalation(Some("o1")).unwrap().severity,
            EscalationSeverity::HardPause,
            "severity defaults to hard-pause"
        );
    }

    #[test]
    fn opt_in_only_rule_is_not_auto_recommended_even_when_grounded() {
        // is_auto_recommended() on the Rule reflects only grounding (the server
        // propose logic ANDs in `!is_opt_in_only()`), but the corpus contract is
        // that an opt_in_only rule, though grounded, must never be pre-checked.
        // We assert the gate the server uses: grounded AND !opt_in_only.
        let mut r = make_rule("R-OPTIN-1", "ci-cd", EnforcementKind::Mechanical);
        r.verification = Verification::Grounded;
        r.opt_in_only = true;
        assert!(
            r.is_grounded(),
            "the rule is grounded (so it would otherwise be pre-checked)"
        );
        let auto_recommended = r.is_auto_recommended() && !r.is_opt_in_only();
        assert!(
            !auto_recommended,
            "an opt_in_only rule must never be auto-recommended even when grounded"
        );
    }

    // ── New corpus rules: CI security (Semgrep / CodeQL) ──────────────────────

    #[tokio::test]
    async fn corpus_loads_ci_security_rules_with_flags() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus
        }
        let set = load_corpus(path).await.expect("corpus loads");

        // Semgrep: opt_in_only, mechanical, two options, no default.
        if let Some(rule) = set.get_by_id("CICD-SEMGREP-SECURITY-SCAN-1") {
            assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
            assert_eq!(rule.domain, "ci-cd");
            assert!(rule.is_opt_in_only(), "Semgrep rule is opt_in_only");
            assert!(!rule.is_layer3_only(), "Semgrep CE runs at scan + layer-2");
            assert!(rule.is_grounded(), "Semgrep rule is grounded");
            assert!(!rule.has_default(), "no default → forces a tier choice");
            assert_eq!(rule.options.len(), 2, "two tier options");
            assert!(rule
                .options
                .iter()
                .any(|o| o.id == "semgrep-community-edition"));
            assert!(rule
                .options
                .iter()
                .any(|o| o.id == "semgrep-appsec-platform-pro"));
            // The opt_in_only rule, though grounded, must never be auto-recommended.
            assert!(
                !rule.is_auto_recommended() || rule.is_opt_in_only(),
                "Semgrep rule must never be pre-checked"
            );
        }

        // CodeQL: opt_in_only + layer3_only, mechanical, two options, no default.
        if let Some(rule) = set.get_by_id("CICD-CODEQL-SECURITY-SCAN-1") {
            assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
            assert_eq!(rule.domain, "ci-cd");
            assert!(rule.is_opt_in_only(), "CodeQL rule is opt_in_only");
            assert!(
                rule.is_layer3_only(),
                "CodeQL is layer-3 only (heavy DB build)"
            );
            assert!(rule.is_grounded(), "CodeQL rule is grounded");
            assert!(!rule.has_default(), "no default → forces a tier choice");
            assert_eq!(rule.options.len(), 2, "two tier options");
            assert!(rule.options.iter().any(|o| o.id == "codeql-public-free"));
            assert!(rule.options.iter().any(|o| o.id == "codeql-ghas-paid"));
        }

        // P7: JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1 is opt-in only (its option set doesn't yet
        // separate "how routes are organized" from "how auth is enforced" — see the corpus
        // audit) — it must never be pre-checked, even though it's grounded and stack-relevant.
        let route_placement = set
            .get_by_id("JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1")
            .expect("JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1 must exist in the bundled corpus");
        assert!(
            route_placement.is_opt_in_only(),
            "the route-placement rule must be opt_in_only until its option set is redesigned"
        );
        assert!(
            route_placement.is_grounded(),
            "it stays grounded — opt-in only changes recommendation, not provenance"
        );
        assert!(
            !route_placement.is_auto_recommended() || route_placement.is_opt_in_only(),
            "the route-placement rule must never be auto-recommended despite being grounded"
        );
    }

    /// P7: ARCH-IDEMPOTENCY-KEYS-1's default-option directive — the text the AI audit prompt
    /// actually reads when checking code against this rule — must explicitly rule out "a helper
    /// function exists but is unused/uncalled" as evidence of an idempotency defect. Without
    /// this, the model conflated "an idempotency-key helper is defined but not wired into any
    /// endpoint" (a dead-code/hygiene observation) with "this endpoint has no idempotency-key
    /// mechanism" (the actual defect this rule polices).
    #[tokio::test]
    async fn idempotency_keys_directive_rules_out_the_unused_helper_false_positive() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let rule = set
            .get_by_id("ARCH-IDEMPOTENCY-KEYS-1")
            .expect("ARCH-IDEMPOTENCY-KEYS-1 must exist in the bundled corpus");
        let default = rule
            .resolved_option(None)
            .expect("the rule must resolve to a default option");
        let directive_lc = default.directive.to_ascii_lowercase();
        assert!(
            directive_lc.contains("unused")
                || directive_lc.contains("not (yet) called")
                || directive_lc.contains("not called"),
            "the directive must explicitly rule out an unused/uncalled helper as a violation: {}",
            default.directive
        );
        assert!(
            directive_lc.contains("endpoint"),
            "the directive must anchor the violation on the ENDPOINT, not a helper's own \
             definition: {}",
            default.directive
        );
    }

    // ── derive-from-folder (unit tests for the path → domain derivation) ─────

    #[tokio::test]
    async fn derived_domain_from_folder_path() {
        // Create a temporary corpus dir with a rule in a nested folder.
        let tmp = tempfile::tempdir().expect("tempdir");
        let corpus_dir = tmp.path();

        // Create rust/dioxus/test-rule.toml
        let rust_dioxus = corpus_dir.join("rust").join("dioxus");
        std::fs::create_dir_all(&rust_dioxus).expect("create dirs");
        let toml_path = rust_dioxus.join("test-rule.toml");
        std::fs::write(
            &toml_path,
            r#"
            id = "RUST-DIOXUS-TEST-1"
            title = "Test rule"
            enforcement = "prose"
        "#,
        )
        .expect("write toml");

        let set = load_corpus(corpus_dir).await.expect("corpus loads");
        let rule = set.get_by_id("RUST-DIOXUS-TEST-1").expect("rule loaded");
        assert_eq!(
            rule.domain, "rust:dioxus",
            "domain derived from rust/dioxus/ folder"
        );
    }

    #[tokio::test]
    async fn universal_folder_derives_universal_domain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let corpus_dir = tmp.path();

        let universal_dir = corpus_dir.join("universal");
        std::fs::create_dir_all(&universal_dir).expect("create dirs");
        let toml_path = universal_dir.join("arch-lifecycle.toml");
        std::fs::write(
            &toml_path,
            r#"
            id = "ARCH-RESOURCE-LIFECYCLE-TEST-1"
            title = "Test universal rule"
            enforcement = "prose"
        "#,
        )
        .expect("write toml");

        let set = load_corpus(corpus_dir).await.expect("corpus loads");
        let rule = set
            .get_by_id("ARCH-RESOURCE-LIFECYCLE-TEST-1")
            .expect("rule loaded");
        assert_eq!(
            rule.domain, "universal",
            "universal/ folder derives 'universal' domain"
        );

        // Must be selected when no domains specified (universal always included).
        let selected = select_for_domains(&set, &[]);
        assert!(
            selected
                .iter()
                .any(|r| r.id_str() == "ARCH-RESOURCE-LIFECYCLE-TEST-1"),
            "universal rule is selected with empty domain list"
        );
    }

    // ── C6-B4: every deterministic (CI-enforced) rule option with authored client-facing
    // remediation carries an authored effort band ──────────────────────────────────────

    /// A deterministic/floor finding never goes through the AI calibration pass that sets
    /// `Finding::effort` per-finding (see `report_export::resolve_effort`'s doc comment in
    /// camerata-server) — its ONLY source of a remediation-effort estimate is this rule's own
    /// authored [`RuleOption::effort`] band. `Mechanical`/`Architectural` enforcement
    /// ([`EnforcementKind::is_ci_enforced`]) IS that deterministic tier by this corpus's own
    /// taxonomy: a hard, repeatable lint pattern or an AST/static-analysis pass, as opposed to
    /// `Prose`/`Structured` rules a human or the AI semantic-audit pass judges (which DO get a
    /// per-finding calibrated effort, so an authored band is a nice-to-have there, never the
    /// only source). This is a BUILD/CI-TIME corpus test, not a runtime gate — a new
    /// deterministic rule (or a new option on an existing one) that ships authored
    /// client-facing `remediation` but no `effort` band fails THIS test in `cargo test`,
    /// never a paying client's report (the fifth-consecutive-run alarm C6-B4 exists to close).
    ///
    /// Scoped to options that actually carry `remediation`: an option with none never renders
    /// a Fix line at all (see `resolve_fix`'s doc comment in camerata-server), so there is
    /// nothing for `effort` to pair with — a rejected/non-default alternative with no
    /// remediation is a pre-existing, accepted shape in this corpus (e.g.
    /// `SUPABASE-RLS-ENABLED-1`'s rejected `regex-grep-each-migration-independently` option)
    /// and stays intentionally exempt here too.
    #[tokio::test]
    async fn every_deterministic_rule_option_with_remediation_has_an_authored_effort_band() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            // Skip when the corpus checkout is not present (mirrors every other real-corpus
            // test in this module).
            return;
        }
        let (set, errors) = load_corpus_lenient(path).await;
        assert!(
            errors.is_empty(),
            "the real corpus must load without parse errors: {errors:?}"
        );

        let mut missing: Vec<String> = Vec::new();
        for rule in set.iter() {
            if !rule.enforcement.is_ci_enforced() {
                // Prose/Structured: AI-judged: calibration sets Finding::effort per-finding.
                continue;
            }
            for option in &rule.options {
                let has_remediation = option
                    .remediation
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty());
                if !has_remediation {
                    continue;
                }
                if option.effort.is_none() {
                    missing.push(format!("{} / option `{}`", rule.id_str(), option.id));
                }
            }
        }

        assert!(
            missing.is_empty(),
            "every deterministic (mechanical/architectural) rule option with authored \
             client-facing remediation must also carry an authored `effort` band — a \
             deterministic finding never goes through AI calibration, so this is its ONLY \
             path to a remediation-effort estimate (see `report_export::resolve_effort` in \
             camerata-server). Missing on: {missing:#?}"
        );
    }

    // ── New domain-class rules: storage / auth / sql-integrity / realtime ────────────────
    //
    // Domain-class authoring pass (not fixture-driven): object/file storage defect class,
    // Supabase auth-configuration defect class, relational schema-integrity defect class, and
    // a brand-new `supabase:realtime` sub-domain for the pub/sub channel-authorization defect
    // class. Each assertion below is a minimal "does this rule exist, is it shaped correctly,
    // and will it ever reach anything" check — the real content lives in the rule's own prose,
    // reviewed by hand against `docs/RULE_AUTHORING.md`'s checklist at authoring time.

    /// A rule id is "code-auditable" (reachable by the semantic/AI pass per
    /// `onboard::audit::is_code_auditable_rule` in camerata-server) unless it carries a
    /// governance/process prefix (`ORCH-`/`SPIRIT-`/`PROC-`). None of the ids below do, so each
    /// is guaranteed a route to evaluation regardless of its declared enforcement tier (see
    /// `mechanical_gate`'s module doc in camerata-server) — this local check mirrors that
    /// predicate without taking a dependency on the server crate.
    fn is_code_auditable(id: &str) -> bool {
        !(id.starts_with("ORCH-") || id.starts_with("SPIRIT-") || id.starts_with("PROC-"))
    }

    /// Common shape assertions for a newly-authored rule: it loads, is grounded (cites a real
    /// external authority), carries at least one `[[sources]]` entry, is code-auditable (so it
    /// is guaranteed to reach the semantic pass even with no shipped detector), offers at least
    /// one alternative option alongside its adopted choice, and every option with a non-empty
    /// directive — and, where an option carries remediation, an authored `effort` band too.
    ///
    /// Most new rules adopt a default (`default = true`, resolvable via `resolved_option(None)`);
    /// this is checked when a default is present. A rule can ALSO legitimately ship with no
    /// adopted default (`default = false`, typically paired with `opt_in_only = true` — e.g. a
    /// rule whose right answer depends on project-specific infrastructure the architect must
    /// consciously choose, per RULE_AUTHORING's "else the architect MUST choose" note); in that
    /// case every option is still checked for a non-empty directive instead of just the default.
    fn assert_well_formed_new_rule(set: &RuleSet, id: &str, expected_domain: &str) {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_eq!(rule.domain, expected_domain, "{id} domain");
        assert!(
            rule.is_grounded(),
            "{id} must be grounded (cites a real authority)"
        );
        assert!(
            !rule.sources.is_empty(),
            "{id} must carry at least one [[sources]] entry"
        );
        assert!(
            is_code_auditable(id),
            "{id} must be code-auditable (reachable by semantic pass)"
        );
        // Every one of these new rules ships at least one rejected/alternative option alongside
        // the adopted (or, for a no-default rule, the architect-chosen) option, per
        // RULE_AUTHORING's "options/alternatives" expectation.
        assert!(
            rule.options.len() >= 2,
            "{id} must offer at least one alternative option alongside its default: {} option(s)",
            rule.options.len()
        );
        if rule.default_option.is_some() {
            let default = rule
                .resolved_option(None)
                .unwrap_or_else(|| panic!("{id} must resolve a default option"));
            assert!(
                !default.directive.trim().is_empty(),
                "{id} default option directive"
            );
        } else {
            for opt in &rule.options {
                assert!(
                    !opt.directive.trim().is_empty(),
                    "{id} option {} directive",
                    opt.id
                );
            }
        }
        for opt in &rule.options {
            if let Some(remediation) = opt.remediation.as_deref() {
                if !remediation.trim().is_empty() {
                    assert!(
                        opt.effort.is_some(),
                        "{id}'s option {} carries remediation and must also carry an authored effort band",
                        opt.id
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn new_storage_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        for id in [
            "SUPABASE-STORAGE-KEY-SANITIZATION-1",
            "SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1",
            "SUPABASE-STORAGE-SIGNED-URL-1",
            "SUPABASE-STORAGE-DOWNLOAD-OWNERSHIP-1",
        ] {
            assert_well_formed_new_rule(&set, id, "supabase:storage");
        }
    }

    #[tokio::test]
    async fn new_auth_config_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        for id in [
            "SUPABASE-AUTH-ANON-SIGNIN-1",
            "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
            "SUPABASE-AUTH-SESSION-LIFETIME-1",
            "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
            "SUPABASE-AUTH-PASSWORD-POLICY-1",
            "SUPABASE-AUTH-MFA-PRIVILEGED-1",
        ] {
            assert_well_formed_new_rule(&set, id, "supabase:auth");
        }
        // Regression guard against duplicating SUPABASE-RLS-USER-METADATA-1: the "user-metadata
        // trusted for authorization" defect is already covered there (supabase:rls domain), so
        // no new supabase:auth rule should re-cover the same user_metadata/raw_user_meta_data
        // token match.
        assert!(
            set.get_by_id("SUPABASE-RLS-USER-METADATA-1").is_some(),
            "the pre-existing user-metadata-authorization rule must still be present (not duplicated)"
        );
    }

    #[tokio::test]
    async fn new_sql_integrity_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        for id in [
            "SQL-FK-CASCADE-FINANCIAL-1",
            "SQL-MONEY-FLOAT-1",
            "SQL-INVARIANTS-CONSTRAINTS-1",
            "SQL-TIMESTAMPTZ-1",
            "SQL-ENUM-TEXT-CONSTRAINT-1",
            "SQL-PRIVILEGED-WRITE-AUDIT-1",
            "SQL-SOFT-DELETE-PARTIAL-INDEX-1",
        ] {
            assert_well_formed_new_rule(&set, id, "sql");
        }
    }

    /// `SQL-MONEY-FLOAT-1` declares `mechanical` enforcement with no shipped detector — the
    /// W4 mechanical-gate invariant (see `mechanical_gate` in camerata-server) requires that any
    /// such rule be code-auditable so it still reaches the semantic pass. Pinned here as a
    /// regression guard specific to this rule, since it is the one new rule in this pass that
    /// chose the mechanical tier.
    #[tokio::test]
    async fn sql_money_float_is_mechanical_with_no_detector_but_code_auditable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let rule = set
            .get_by_id("SQL-MONEY-FLOAT-1")
            .expect("SQL-MONEY-FLOAT-1 must exist in the bundled corpus");
        assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
        assert!(is_code_auditable("SQL-MONEY-FLOAT-1"));
    }

    #[tokio::test]
    async fn new_realtime_subdomain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        for id in [
            "SUPABASE-REALTIME-AUTHORIZATION-1",
            "SUPABASE-REALTIME-CHANNEL-SCOPE-1",
            "SUPABASE-REALTIME-PAYLOAD-EXPOSURE-1",
            "SUPABASE-REALTIME-PRESENCE-IDENTITY-1",
        ] {
            assert_well_formed_new_rule(&set, id, "supabase:realtime");
        }
        // The new sub-domain must be reachable the same way every other supabase:<area> domain
        // is: select_for_domains matches on the exact domain string.
        let selected = select_for_domains(&set, &["supabase:realtime"]);
        assert!(
            selected
                .iter()
                .any(|r| r.id_str() == "SUPABASE-REALTIME-AUTHORIZATION-1"),
            "a repo whose stack resolves to the supabase:realtime domain must select the new rules"
        );
    }

    /// Regression guard: every id in `ids` appears EXACTLY ONCE across the whole loaded corpus.
    /// `load_corpus` has no built-in duplicate-id detection (it just pushes every parsed file into
    /// the `RuleSet`), so a copy-pasted id from an existing rule would otherwise silently shadow
    /// rather than fail to load — this is the explicit check that would catch it.
    fn assert_ids_unique_in_corpus(set: &RuleSet, ids: &[&str]) {
        for id in ids {
            let count = set.iter().filter(|r| r.id_str() == *id).count();
            assert_eq!(
                count, 1,
                "{id} must appear exactly once in the corpus, found {count}"
            );
        }
    }

    #[tokio::test]
    async fn new_payments_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let ids = [
            "ARCH-SERVER-COMPUTED-AMOUNT-1",
            "ARCH-WEBHOOK-SIGNATURE-VERIFICATION-1",
            "ARCH-WEBHOOK-EVENT-DEDUP-1",
            "ARCH-REFUND-AUTHORIZATION-1",
            "ARCH-CURRENCY-CONSISTENCY-1",
            "ARCH-NO-PAN-STORAGE-1",
            "ARCH-PAYMENT-RECONCILIATION-1",
        ];
        for id in ids {
            assert_well_formed_new_rule(&set, id, "api-layer");
        }
        assert_ids_unique_in_corpus(&set, &ids);
        // api-layer is an existing, already-wired domain — confirm the new rules are selected
        // alongside the pre-existing ARCH-IDEMPOTENCY-KEYS-1 rather than needing new wiring.
        let selected = select_for_domains(&set, &["api-layer"]);
        for id in ids {
            assert!(
                selected.iter().any(|r| r.id_str() == id),
                "a repo whose stack resolves to the api-layer domain must select {id}"
            );
        }
        assert!(
            selected
                .iter()
                .any(|r| r.id_str() == "ARCH-IDEMPOTENCY-KEYS-1"),
            "the pre-existing idempotency-key rule must still be present (retry/double-charge is not duplicated here)"
        );
    }

    /// `ARCH-NO-PAN-STORAGE-1` declares `mechanical` enforcement with no shipped detector — same
    /// shape as `SQL-MONEY-FLOAT-1` above: deterministically checkable in principle (a column/
    /// field-name and card-shape pattern scan), but Camerata does not ship that scanner yet.
    /// Pinned here as a regression guard so a future change to its `enforcement` field, or to
    /// `is_code_auditable`'s prefix list, is caught rather than silently leaving this rule
    /// unreachable by the semantic pass.
    #[tokio::test]
    async fn arch_no_pan_storage_is_mechanical_with_no_detector_but_code_auditable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let rule = set
            .get_by_id("ARCH-NO-PAN-STORAGE-1")
            .expect("ARCH-NO-PAN-STORAGE-1 must exist in the bundled corpus");
        assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
        assert!(is_code_auditable("ARCH-NO-PAN-STORAGE-1"));
    }

    /// Server-to-client data-exposure defect class: spans three existing domains (api-layer,
    /// fullstack, javascript) rather than one, so this groups the whole class in a single test
    /// with one loop per domain, mirroring `assert_well_formed_new_rule`'s per-domain contract.
    #[tokio::test]
    async fn new_exposure_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");

        let api_layer_ids = [
            "ARCH-NO-LEAKY-ERROR-DETAILS-1",
            "ARCH-RESPONSE-FIELD-PROJECTION-1",
            "ARCH-NO-STORE-SENSITIVE-RESPONSES-1",
        ];
        for id in api_layer_ids {
            assert_well_formed_new_rule(&set, id, "api-layer");
        }

        let fullstack_ids = ["ARCH-SERVER-CLIENT-PROJECTION-1"];
        for id in fullstack_ids {
            assert_well_formed_new_rule(&set, id, "fullstack");
        }

        let javascript_ids = ["JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1"];
        for id in javascript_ids {
            assert_well_formed_new_rule(&set, id, "javascript");
        }

        let all_ids: Vec<&str> = api_layer_ids
            .iter()
            .chain(fullstack_ids.iter())
            .chain(javascript_ids.iter())
            .copied()
            .collect();
        assert_ids_unique_in_corpus(&set, &all_ids);

        // Each domain is already wired (api-layer, fullstack, javascript are existing, armed
        // domains) — confirm the new rules are selected per-domain with no new wiring needed.
        let api_layer_selected = select_for_domains(&set, &["api-layer"]);
        for id in api_layer_ids {
            assert!(
                api_layer_selected.iter().any(|r| r.id_str() == id),
                "a repo whose stack resolves to the api-layer domain must select {id}"
            );
        }
        let fullstack_selected = select_for_domains(&set, &["fullstack"]);
        assert!(
            fullstack_selected
                .iter()
                .any(|r| r.id_str() == "ARCH-SERVER-CLIENT-PROJECTION-1"),
            "a repo whose stack resolves to the fullstack domain must select the new rule"
        );
        let javascript_selected = select_for_domains(&set, &["javascript"]);
        assert!(
            javascript_selected
                .iter()
                .any(|r| r.id_str() == "JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1"),
            "a repo whose stack resolves to the javascript domain must select the new rule"
        );
    }

    /// `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` declares `mechanical` enforcement with no shipped
    /// detector — same shape as `SQL-MONEY-FLOAT-1` / `ARCH-NO-PAN-STORAGE-1` above.
    #[tokio::test]
    async fn javascript_public_env_prefix_secret_is_mechanical_with_no_detector_but_code_auditable()
    {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let rule = set
            .get_by_id("JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1")
            .expect("JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1 must exist in the bundled corpus");
        assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
        assert!(is_code_auditable("JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1"));
    }

    /// Client-side credential/sensitive-data storage defect class: spans two existing domains
    /// (ui, permissions). `ARCH-NO-SECRETS-IN-URL-1` already covers tokens-in-URLs (a different
    /// transport-location question), so that sub-case is deliberately not re-covered here — see
    /// `UI-AUTH-TOKEN-STORAGE-1`'s own `why` for the explicit cross-reference. Cookies missing
    /// Secure/SameSite are likewise not a separate rule: `UI-AUTH-TOKEN-STORAGE-1`'s own default
    /// option directive already mandates HttpOnly+Secure+SameSite as the single cookie-issuance
    /// remediation, so a standalone "cookie attributes" rule would just restate the same
    /// checklist from a narrower angle.
    #[tokio::test]
    async fn new_client_credential_storage_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");

        let ui_ids = ["UI-AUTH-TOKEN-STORAGE-1"];
        for id in ui_ids {
            assert_well_formed_new_rule(&set, id, "ui");
        }
        let permissions_ids = ["ARCH-TOKEN-REFRESH-ROTATION-1"];
        for id in permissions_ids {
            assert_well_formed_new_rule(&set, id, "permissions");
        }

        let all_ids: Vec<&str> = ui_ids
            .iter()
            .chain(permissions_ids.iter())
            .copied()
            .collect();
        assert_ids_unique_in_corpus(&set, &all_ids);

        let ui_selected = select_for_domains(&set, &["ui"]);
        assert!(
            ui_selected
                .iter()
                .any(|r| r.id_str() == "UI-AUTH-TOKEN-STORAGE-1"),
            "a repo whose stack resolves to the ui domain must select the new rule"
        );
        let permissions_selected = select_for_domains(&set, &["permissions"]);
        assert!(
            permissions_selected
                .iter()
                .any(|r| r.id_str() == "ARCH-TOKEN-REFRESH-ROTATION-1"),
            "a repo whose stack resolves to the permissions domain must select the new rule"
        );

        // Regression guard against duplicating ARCH-NO-SECRETS-IN-URL-1: tokens-in-URLs is
        // already covered there, so no new rule in this batch should re-cover that sub-case.
        assert!(
            set.get_by_id("ARCH-NO-SECRETS-IN-URL-1").is_some(),
            "the pre-existing tokens-in-URLs rule must still be present (not duplicated)"
        );
    }

    // ── New domain-class rules: CI/CD pipeline security ───────────────────────────────────────
    //
    // ci-cd was thin (~7 pre-existing rules) before this pass. Seven new rules cover: secrets
    // injected as workflow-YAML literals rather than the platform's secrets store (and unscoped
    // across jobs that don't need them), steps that print the environment or fail to mask a
    // derived secret, privileged deploy/migration jobs with no environment-protection approval
    // gate, third-party actions pinned to a mutable tag/branch instead of a commit SHA, the
    // default job token left at a broad permission scope, pull_request_target combined with
    // checking out and executing a fork PR's own untrusted code, and published artifacts with no
    // verifiable build provenance.

    #[tokio::test]
    async fn new_cicd_security_domain_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");

        let ids = [
            "CICD-SECRETS-VIA-SECRETS-STORE-1",
            "CICD-NO-SECRET-LOGGING-1",
            "CICD-PRIVILEGED-DEPLOY-GATE-1",
            "CICD-PIN-THIRD-PARTY-ACTIONS-1",
            "CICD-LEAST-PRIVILEGE-TOKEN-1",
            "CICD-NO-SECRETS-ON-FORK-PR-1",
        ];
        for id in ids {
            assert_well_formed_new_rule(&set, id, "ci-cd");
        }
        assert_ids_unique_in_corpus(&set, &ids);

        let selected = select_for_domains(&set, &["ci-cd"]);
        for id in ids {
            assert!(
                selected.iter().any(|r| r.id_str() == id),
                "a repo whose stack resolves to the ci-cd domain must select {id}"
            );
        }
        // Pre-existing ci-cd rules must still be reachable alongside the new ones — this batch
        // extends the domain rather than replacing anything in it.
        for pre_existing in [
            "CICD-CODEQL-SECURITY-SCAN-1",
            "CICD-SEMGREP-SECURITY-SCAN-1",
            "CICD-DEPENDENCY-AUDIT-1",
            "ARCH-TRIGGER-ENV-1",
            "ARCH-TRUNK-SYNC-1",
        ] {
            assert!(
                set.get_by_id(pre_existing).is_some(),
                "pre-existing ci-cd rule {pre_existing} must still be present"
            );
        }

        // CICD-BUILD-PROVENANCE-1 is deliberately opt_in_only with no adopted default (a genuine
        // project decision — not every artifact type/distribution channel supports attestation
        // verification yet — not a universal floor), same shape as CICD-CODEQL-SECURITY-SCAN-1 /
        // CICD-SEMGREP-SECURITY-SCAN-1 above, so it is checked separately rather than through
        // `assert_well_formed_new_rule` (which requires a resolvable default option).
        let provenance = set
            .get_by_id("CICD-BUILD-PROVENANCE-1")
            .expect("CICD-BUILD-PROVENANCE-1 must exist in the bundled corpus");
        assert_eq!(provenance.domain, "ci-cd");
        assert!(
            provenance.is_grounded(),
            "CICD-BUILD-PROVENANCE-1 must be grounded"
        );
        assert!(!provenance.sources.is_empty());
        assert!(
            provenance.is_opt_in_only(),
            "CICD-BUILD-PROVENANCE-1 is opt_in_only"
        );
        assert!(
            !provenance.has_default(),
            "CICD-BUILD-PROVENANCE-1 has no adopted default — it forces a conscious project decision"
        );
        assert_eq!(provenance.options.len(), 2, "two options: attest, or don't");
        assert!(
            !provenance.is_auto_recommended() || provenance.is_opt_in_only(),
            "CICD-BUILD-PROVENANCE-1 must never be auto-recommended"
        );
        assert_ids_unique_in_corpus(&set, &["CICD-BUILD-PROVENANCE-1"]);
    }

    /// `CICD-PIN-THIRD-PARTY-ACTIONS-1` and `CICD-LEAST-PRIVILEGE-TOKEN-1` both declare
    /// `mechanical` enforcement with no shipped detector (a regex/header-presence scan over
    /// workflow YAML) — same shape as the other no-detector mechanical rules pinned above.
    #[tokio::test]
    async fn cicd_mechanical_rules_have_no_detector_but_are_code_auditable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        for id in [
            "CICD-PIN-THIRD-PARTY-ACTIONS-1",
            "CICD-LEAST-PRIVILEGE-TOKEN-1",
        ] {
            let rule = set
                .get_by_id(id)
                .unwrap_or_else(|| panic!("{id} must exist in the bundled corpus"));
            assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
            assert!(is_code_auditable(id));
        }
    }

    /// Camerata capability: sensitive data reaching a log sink (TAINT). The corpus rule both
    /// taint-log-{secret,pii} semgrep rule families ground to via
    /// `camerata_server::semgrep_floor_category` (see
    /// `crates/server/assets/semgrep-rules/taint-security.yml`'s "Sensitive data reaching a
    /// log sink" section). `mechanical` with a REAL detector (the bundled semgrep taint
    /// rules), unlike this test module's other no-detector mechanical pins above.
    #[tokio::test]
    async fn sensitive_data_in_logs_rule_is_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        assert_well_formed_new_rule(&set, "SEC-NO-SENSITIVE-DATA-IN-LOGS-1", "universal");
        let rule = set
            .get_by_id("SEC-NO-SENSITIVE-DATA-IN-LOGS-1")
            .expect("must exist in the bundled corpus");
        assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
        assert!(
            rule.sources
                .iter()
                .any(|s| s.url.contains("cwe.mitre.org/data/definitions/532")),
            "must cite CWE-532 (Insertion of Sensitive Information into Log File)"
        );
        assert_ids_unique_in_corpus(&set, &["SEC-NO-SENSITIVE-DATA-IN-LOGS-1"]);
    }

    /// Camerata capability: under-logging / observability thoroughness (SEMANTIC, CAPPED
    /// LOW). Three narrowly-scoped corpus rules — a swallowed error with no logging,
    /// inconsistent unstructured logging alongside an adopted structured logger, and missing
    /// correlation/request id propagation. Well-formedness only; the structural LOW-severity
    /// cap and action-bucket exclusion are exercised by `camerata_server::ai_audit`'s and
    /// `camerata_server::report_export`'s own test suites (that cap is implemented in the
    /// server crate, not here — this crate only owns the corpus data).
    #[tokio::test]
    async fn observability_convention_rules_are_well_formed_and_reachable() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let ids = [
            "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
            "ARCH-OBSERVABILITY-LOG-CONSISTENCY-1",
            "ARCH-OBSERVABILITY-CORRELATION-ID-1",
        ];
        for id in ids {
            assert_well_formed_new_rule(&set, id, "universal");
            let rule = set
                .get_by_id(id)
                .unwrap_or_else(|| panic!("{id} must exist in the bundled corpus"));
            assert_eq!(
                rule.enforcement,
                EnforcementKind::Architectural,
                "{id} is SEMANTIC/judgment-shaped, not mechanically gated"
            );
        }
        assert_ids_unique_in_corpus(&set, &ids);
    }

    // ── Pass-1 W2: rule kind / violates / max_severity schema ──────────────────────────────
    // (`docs/plans/2026-10-09_audit-findings-tracker.md`, A5.1 + the corpus-load gate)

    /// req_a5_1: EVERY [`RuleKind`] variant round-trips through TOML in both directions — the
    /// wire string parses to the right variant, and an ABSENT `kind` field parses to
    /// `None` rather than silently defaulting to a permissive variant (the back-compat
    /// requirement: `RuleToml::kind` has `#[serde(default)]` so the loader never hard-errors
    /// on a pre-classification TOML, but the resulting `Rule::kind()` must stay `None`, never
    /// resolve to e.g. `Decision`).
    ///
    /// FAILS BEFORE THE FIX: before this field existed at all, `kind = "defect"` in a TOML was
    /// silently ignored by serde (unknown field, no `deny_unknown_fields`) and `Rule` had no
    /// `kind` accessor to assert against — this test could not even be written. After the fix,
    /// each wire string resolves to its variant and the absent case resolves to `None`.
    ///
    /// TEST DISCIPLINE exception (a), W17: this test hardcoded the three variants and was
    /// renamed from `req_a5_1_three_rule_kinds_round_trip_through_toml` when
    /// [`RuleKind::DecisionWithFloor`] was added. It now iterates [`RuleKind::ALL`] rather than
    /// a hand-maintained copy of the variant list, so a FIFTH kind is covered the moment it is
    /// declared instead of shipping untested — which is the failure this edit was one step away
    /// from being.
    #[test]
    fn req_a5_1_every_rule_kind_round_trips_through_toml() {
        assert_eq!(
            RuleKind::ALL.len(),
            4,
            "RuleKind::ALL must list every variant; a new kind belongs in it, and in this test \
             by consequence"
        );
        for expected in RuleKind::ALL {
            let wire = expected.as_str();
            let src = format!(
                r#"
                id = "KIND-ROUNDTRIP-1"
                title = "Round trip"
                enforcement = "structured"
                domain = "rust"
                kind = "{wire}"
                "#
            );
            let rule = parse_rule(&src);
            assert_eq!(
                rule.kind(),
                Some(expected),
                "wire string {wire:?} must parse to {expected:?}"
            );
            assert_eq!(
                rule.kind().unwrap().as_str(),
                wire,
                "as_str is the inverse of from_tag"
            );
            assert_eq!(
                RuleKind::from_tag(wire),
                Some(expected),
                "from_tag must accept every variant's own wire string — its `_ => None` arm is \
                 where a new kind silently stops parsing"
            );
        }

        // Absent `kind` -> None, not a silent permissive default.
        let no_kind_src = r#"
            id = "KIND-ABSENT-1"
            title = "No kind declared"
            enforcement = "structured"
            domain = "rust"
            "#;
        let rule = parse_rule(no_kind_src);
        assert_eq!(
            rule.kind(),
            None,
            "an absent `kind` must resolve to None, never silently to a concrete variant"
        );
        assert!(
            !rule.is_defect_kind(),
            "an unclassified rule is never defect-kind"
        );
    }

    /// req_a5_1: `RuleOption::violates` round-trips — present-and-true, present-and-false, and
    /// ABSENT (defaulting to `false`, never silently `true`).
    ///
    /// FAILS BEFORE THE FIX: `OptionToml`/`RuleOption` had no `violates` field at all before
    /// this change — `violates = true` in an `[[option]]` block was silently dropped by serde
    /// and there was no accessor to assert against.
    #[test]
    fn req_a5_1_violates_round_trips_through_toml() {
        let src = r#"
            id = "VIOLATES-ROUNDTRIP-1"
            title = "Round trip"
            enforcement = "structured"
            domain = "rust"
            kind = "default_with_exceptions"

            [decision]
            default = "good"

            [[option]]
            id = "good"
            label = "The correct way"
            directive = "do it right"
            why = "because"

            [[option]]
            id = "bad"
            label = "The degenerate way"
            directive = "do nothing"
            why = "rejected"
            violates = true

            [[option]]
            id = "unspecified"
            label = "No violates key at all"
            directive = "do something else"
            why = "also fine"
            "#;
        let rule = parse_rule(src);
        let good = rule.options.iter().find(|o| o.id == "good").unwrap();
        let bad = rule.options.iter().find(|o| o.id == "bad").unwrap();
        let unspecified = rule.options.iter().find(|o| o.id == "unspecified").unwrap();
        assert!(!good.violates, "explicit absence of the key must be false");
        assert!(
            bad.violates,
            "an explicit `violates = true` must round-trip to true"
        );
        assert!(
            !unspecified.violates,
            "an option with no `violates` key at all defaults to false"
        );
    }

    /// req_a5_1 (corpus-load gate, item 4): every rule in the REAL shipped corpus declares a
    /// `kind`. This is the build-time gate — mirrors `camerata_server::mechanical_gate`'s
    /// existing precedent for the W1-item-4 defect (a `#[cfg(test)]` assertion, not a runtime
    /// refusal; see `rules_missing_kind`'s doc comment).
    ///
    /// FAILS BEFORE THE FIX: before the classification pass, zero corpus rules declared `kind`
    /// at all (the field didn't exist) — `rules_missing_kind` would have returned all ~417 ids.
    /// After classifying the whole corpus, it returns empty.
    #[tokio::test]
    async fn req_a5_1_every_corpus_rule_declares_a_kind() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus checked out
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let missing = rules_missing_kind(&set);
        assert!(
            missing.is_empty(),
            "every corpus rule must declare `kind` (A5.1): {missing:?}"
        );
    }

    /// Corpus-load gate companion (item 4): a `default_with_exceptions` rule with options but
    /// NO option marked `violates` is suspicious — it is either misclassified (should be
    /// `decision`) or under-authored. The real corpus must have zero of these: the
    /// classification pass only ever assigned `default_with_exceptions` when at least one
    /// option was marked `violates`.
    #[tokio::test]
    async fn req_a5_1_corpus_has_no_suspicious_default_with_exceptions_rules() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let suspicious = suspicious_default_with_exceptions_rules(&set);
        assert!(
            suspicious.is_empty(),
            "a default_with_exceptions rule must have at least one `violates` option: {suspicious:?}"
        );
    }

    /// The nineteen SECURITY rules reclassified from `kind = "decision"` to `kind = "defect"`
    /// in W12. Every one shared one identical shape before the change: `kind = "decision"`,
    /// ZERO `violates` flags, exactly two `[[option]]` blocks, and exactly one option whose
    /// `why` opens with the literal token `Rejected:`. Two options, one of which the rule's own
    /// author explicitly marked `Rejected:`, is not a decision under any reading — it is a
    /// defect and its degenerate form, and leaving it classified `decision` with no `violates`
    /// flag is what made a missing RLS policy, a `service_role` key in client code, a dynamic
    /// SQL injection and a committed private key each report as "the convention this codebase
    /// chose" instead of as a fault.
    ///
    /// Hardcoded deliberately rather than derived from the corpus: a derived list would move
    /// with the corpus and could not catch a rule being quietly reclassified back to
    /// `decision`, which is exactly the regression this roster exists to prevent.
    #[cfg(test)]
    const W12_SECURITY_DEFECT_RULE_IDS: &[&str] = &[
        "SEC-NO-COMMAND-INJECTION-1",
        "SEC-NO-PRIVATE-KEY-1",
        "SEC-NO-VENDOR-TOKEN-1",
        "SUPABASE-RLS-NO-POLICY-1",
        "SUPABASE-RLS-PERMISSIVE-TRUE-1",
        "SUPABASE-RLS-USER-METADATA-1",
        "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
        "SUPABASE-STORAGE-KEY-SANITIZATION-1",
        "SUPABASE-STORAGE-OBJECT-POLICY-1",
        "SUPABASE-STORAGE-SIGNED-URL-1",
        "SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1",
        "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
        "SUPABASE-AUTH-MFA-PRIVILEGED-1",
        "SQL-PRIVILEGED-WRITE-AUDIT-1",
        "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
        "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
        "SUPABASE-FUNC-SEARCH-PATH-1",
        "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
        "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
    ];

    /// req_w12: every rule in [`W12_SECURITY_DEFECT_RULE_IDS`] is classified
    /// [`RuleKind::Defect`] and carries EXACTLY ONE `violates` option — the one whose `why`
    /// opens with `Rejected:`, i.e. the degenerate/absent form its own author named.
    ///
    /// Three separate things are asserted per rule, because each has its own failure mode and
    /// a weaker test would let two of them regress silently:
    ///
    /// 1. `kind == Defect` — a `decision`-kind rule cannot report a violation at all (the
    ///    alternatives pass just records whichever option the repo follows), which is the
    ///    original defect.
    /// 2. EXACTLY ONE `violates` option — zero means the rule is still silent; two means the
    ///    correct option is flagged too, so conforming code would be reported as a fault.
    /// 3. The flag sits on the `Rejected:` option SPECIFICALLY — direction, not just count.
    ///    The 244-rule audit that preceded this work found 76 wrong flags, including ones
    ///    sitting on the SAFE option, so "one flag somewhere" is not a sufficient invariant.
    ///
    /// FAILS BEFORE THE FIX: all nineteen were `kind = "decision"` with zero `violates`
    /// options, so assertion 1 failed on all nineteen and assertion 2 failed on all nineteen.
    #[tokio::test]
    async fn req_w12_security_rules_are_defect_kind_with_exactly_one_violates_option() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus checked out
        }
        let set = load_corpus(path).await.expect("corpus loads");

        let mut problems: Vec<String> = Vec::new();
        for id in W12_SECURITY_DEFECT_RULE_IDS {
            let Some(rule) = set.get_by_id(id) else {
                problems.push(format!("{id}: not present in the corpus at all"));
                continue;
            };

            if rule.kind() != Some(RuleKind::Defect) {
                problems.push(format!(
                    "{id}: kind is {:?}, must be Defect — a `decision`-kind rule with no \
                     `violates` flag cannot report a violation at all",
                    rule.kind()
                ));
            }

            let flagged: Vec<&RuleOption> = rule.options.iter().filter(|o| o.violates).collect();
            if flagged.len() != 1 {
                problems.push(format!(
                    "{id}: {} options marked `violates`, must be exactly 1 (flagged: {:?})",
                    flagged.len(),
                    flagged.iter().map(|o| &o.id).collect::<Vec<_>>()
                ));
                continue;
            }

            // Direction: the single flag must sit on the option the author marked `Rejected:`,
            // never on the correct one.
            let rejected: Vec<&RuleOption> = rule
                .options
                .iter()
                .filter(|o| o.why.trim_start().starts_with("Rejected:"))
                .collect();
            if rejected.len() != 1 {
                problems.push(format!(
                    "{id}: {} options open their `why` with `Rejected:`, expected exactly 1 — \
                     this rule's shape no longer matches the W12 set and its flag direction \
                     cannot be checked automatically",
                    rejected.len()
                ));
                continue;
            }
            if flagged[0].id != rejected[0].id {
                problems.push(format!(
                    "{id}: `violates` sits on `{}` but the `Rejected:` option is `{}` — the \
                     flag is on the SAFE option, which would report conforming code as a fault",
                    flagged[0].id, rejected[0].id
                ));
            }
        }

        assert!(
            problems.is_empty(),
            "the W12 security rules must each be `defect`-kind with exactly one `violates` \
             option, on the `Rejected:` option:\n  {}",
            problems.join("\n  ")
        );
    }

    /// req_h1 (Pass-1 W4, `docs/plans/2026-10-09_audit-findings-tracker.md`): no rule in the
    /// REAL shipped corpus may sit at a [`Verification`] value that makes it unselectable on
    /// every repo. A `verification` value is repo-independent, so a rule disarmed by it is
    /// disarmed EVERYWHERE — the exact "could never run for ANY stack" defect H1 names.
    ///
    /// FAILS BEFORE THE FIX: `rules_disarmed_by_verification` returned 21 ids — the 17
    /// `verification = "policy"` rules (including `SPIRIT-FILE-SIZE-1`, finding A8) plus the 4
    /// that omitted the field entirely and therefore defaulted to `draft`
    /// (`UI-UTC-DATES-1` — finding A7 — `GO-HANDLER-SERVICE-REPOSITORY-1`,
    /// `JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1`, `RUST-DIOXUS-1`). Verified by
    /// running this test on unmodified HEAD before touching either `is_auto_recommended` or
    /// the four TOMLs.
    #[tokio::test]
    async fn req_h1_no_corpus_rule_is_disarmed_by_its_verification_value() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus checked out
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let disarmed = rules_disarmed_by_verification(&set);
        assert!(
            disarmed.is_empty(),
            "H1: every corpus rule must be CAPABLE of being selected; these are disarmed on \
             every repo by their verification value alone: {disarmed:?}"
        );
    }

    /// req_h1/req_a8: the positive-detection half of the gate above — a `draft` rule IS
    /// reported by `rules_disarmed_by_verification`, and a `policy` rule is NOT (A8's fix:
    /// `policy` is a provenance statement, never a disarm). Without this, the corpus-level
    /// test could pass vacuously if `rules_disarmed_by_verification` were ever weakened to
    /// return nothing.
    #[test]
    fn req_h1_disarmed_by_verification_detector_fires_on_draft_and_spares_policy() {
        let mut set = RuleSet::default();
        let mut draft = bare_rule("DRAFT-1", "rust");
        draft.verification = Verification::Draft;
        let mut policy = bare_rule("POLICY-1", "rust");
        policy.verification = Verification::Policy;
        let mut grounded = bare_rule("GROUNDED-1", "rust");
        grounded.verification = Verification::Grounded;
        set.push(draft);
        set.push(policy);
        set.push(grounded);
        assert_eq!(
            rules_disarmed_by_verification(&set),
            vec!["DRAFT-1 (draft)".to_string()],
            "only `draft` (and `needs_recheck`) disarm; `policy` and `grounded` must not"
        );
    }

    /// req_a8: `Verification::Policy` is auto-recommended (so a policy rule can arm), while
    /// still NOT being grounded/shippable — the two questions are deliberately separate, and
    /// conflating them is what disarmed `SPIRIT-FILE-SIZE-1` and 16 other rules.
    #[test]
    fn req_a8_policy_is_auto_recommended_but_still_not_grounded() {
        let mut r = bare_rule("POLICY-1", "universal");
        r.verification = Verification::Policy;
        assert!(
            r.is_auto_recommended(),
            "a policy rule must be selectable — provenance is not reliability"
        );
        assert!(!r.is_grounded(), "policy still cites no external authority");
        assert!(!r.is_shippable(), "policy is still not at-least-grounded");
    }

    /// A synthetic `default_with_exceptions` rule with options but no `violates` flag anywhere
    /// IS flagged by `suspicious_default_with_exceptions_rules` — the positive-detection half
    /// of the item-4 check (the corpus-level test above only proves the REAL corpus is clean;
    /// this proves the detector itself actually fires on the shape it's meant to catch).
    #[test]
    fn suspicious_default_with_exceptions_detector_fires_on_the_defect_shape() {
        let src = r#"
            id = "SUSPICIOUS-1"
            title = "Looks like default_with_exceptions but marks nothing violates"
            enforcement = "structured"
            domain = "rust"
            kind = "default_with_exceptions"

            [[option]]
            id = "a"
            label = "A"
            directive = "do A"
            why = "why"

            [[option]]
            id = "b"
            label = "B"
            directive = "do B"
            why = "why"
            "#;
        let rule = parse_rule(src);
        let mut set = RuleSet::default();
        set.push(rule);
        let suspicious = suspicious_default_with_exceptions_rules(&set);
        assert_eq!(suspicious, vec!["SUSPICIOUS-1".to_string()]);
    }

    // ── Pass-1 W5: E2 / A5.2 severity ceilings are AUTHORED, not merely implemented ────────

    /// req_e2 (anti-inertness gate): the REAL shipped corpus actually declares severity
    /// ceilings. E2 says "rules may declare a maximum severity; no report may exceed it" —
    /// a prior workstream shipped the whole mechanism (`Rule::max_severity`,
    /// `report_export::clamp_to_rule_max_severity`, four passing `req_e2_*` unit tests) against
    /// a corpus where NOT ONE of 417 rules declared a ceiling. Every test passed; the
    /// requirement did nothing. This is the test that distinguishes those two states.
    ///
    /// FAILS BEFORE THE FIX: verified on unmodified HEAD (2799284) —
    /// `grep -rl '^max_severity' crates/rules/principles/ | wc -l` returned 0 of 417, so
    /// `rules_declaring_max_severity` returned an EMPTY vec and this assertion failed. It
    /// passes only once real rules carry transcribed ceilings.
    #[tokio::test]
    async fn req_e2_the_corpus_actually_declares_severity_ceilings() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus checked out
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let declared = rules_declaring_max_severity(&set);
        assert!(
            !declared.is_empty(),
            "E2 is INERT: not one corpus rule declares `max_severity`, so \
             `clamp_to_rule_max_severity` can never clamp anything and every `req_e2_*` unit \
             test passes vacuously. Ceilings authored in rule PROSE (finding C8) must be \
             transcribed into the structured field."
        );
    }

    /// req_a5_2 (anti-inertness gate): the REAL shipped corpus actually declares at least one
    /// `deviation_ceiling`. Same defect class as the test above: with none authored,
    /// `clamp_deviation_severity` pins every `Deviation` row at the `"medium"` default, so
    /// A5.2's "must be able to reach HIGH when genuinely problematic" was unsatisfiable by
    /// construction no matter how problematic the deviation.
    ///
    /// FAILS BEFORE THE FIX: verified on unmodified HEAD (2799284) —
    /// `grep -rl deviation_ceiling crates/rules/principles/ | wc -l` returned 0.
    #[tokio::test]
    async fn req_a5_2_the_corpus_actually_declares_deviation_ceilings() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let declared = rules_declaring_deviation_ceiling(&set);
        assert!(
            !declared.is_empty(),
            "A5.2's escape hatch is INERT: no corpus rule declares `deviation_ceiling`, so \
             every consistency deviation clamps at the `medium` default and none can ever \
             reach high."
        );
    }

    /// req_e2 (corpus-load gate): every ceiling the real corpus declares is a VALID severity,
    /// and no rule's `deviation_ceiling` ranks above its own `max_severity`. This is the
    /// build-time gate, same philosophy as `req_a5_1_every_corpus_rule_declares_a_kind`.
    ///
    /// Why it has teeth: `report_export::normalize_severity`'s fall-through arm maps any
    /// unrecognized string to `"low"`, so a typo'd `max_severity = "critcal"` silently becomes
    /// the HARSHEST possible ceiling instead of erroring or no-opping. Without this gate a
    /// one-character authoring slip would cap every finding of that rule at `low`, invisibly.
    #[tokio::test]
    async fn req_e2_corpus_declares_only_valid_severity_ceilings() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let invalid = rules_with_invalid_severity_ceiling(&set);
        assert!(
            invalid.is_empty(),
            "every authored `max_severity`/`deviation_ceiling` must be a valid severity and \
             internally coherent: {invalid:?}"
        );
    }

    /// req_e2 (positive detection): the gate above actually FIRES on each of the three defect
    /// shapes it claims to catch — a typo'd `max_severity`, a typo'd `deviation_ceiling`, and a
    /// `deviation_ceiling` ranking above the rule's own `max_severity`. Without this, the
    /// corpus-level gate could pass vacuously if the detector were ever weakened, which is the
    /// exact failure mode this whole workstream exists to close.
    #[test]
    fn req_e2_invalid_severity_ceiling_detector_fires_on_typos_and_incoherence() {
        let mut set = RuleSet::default();

        let mut typo_max = bare_rule("TYPO-MAX-1", "rust");
        typo_max.max_severity = Some("critcal".to_string());
        set.push(typo_max);

        let mut typo_dev = bare_rule("TYPO-DEV-1", "rust");
        typo_dev.deviation_ceiling = Some("hgih".to_string());
        set.push(typo_dev);

        let mut incoherent = bare_rule("INCOHERENT-1", "rust");
        incoherent.max_severity = Some("medium".to_string());
        incoherent.deviation_ceiling = Some("critical".to_string());
        set.push(incoherent);

        // Valid shapes that must NOT be reported: a coherent pair, a ceiling-free rule, and a
        // deviation ceiling EQUAL to max_severity (binding, not unreachable).
        let mut ok_pair = bare_rule("OK-PAIR-1", "rust");
        ok_pair.max_severity = Some("high".to_string());
        ok_pair.deviation_ceiling = Some("medium".to_string());
        set.push(ok_pair);
        set.push(bare_rule("OK-NONE-1", "rust"));
        let mut ok_equal = bare_rule("OK-EQUAL-1", "rust");
        ok_equal.max_severity = Some("high".to_string());
        ok_equal.deviation_ceiling = Some("high".to_string());
        set.push(ok_equal);

        let offenders = rules_with_invalid_severity_ceiling(&set);
        assert_eq!(
            offenders.len(),
            3,
            "exactly the three defect shapes must be reported, got {offenders:?}"
        );
        assert!(
            offenders.iter().any(|o| o.starts_with("TYPO-MAX-1 (")),
            "a typo'd max_severity must be caught, not coerced to low: {offenders:?}"
        );
        assert!(
            offenders.iter().any(|o| o.starts_with("TYPO-DEV-1 (")),
            "a typo'd deviation_ceiling must be caught: {offenders:?}"
        );
        assert!(
            offenders.iter().any(|o| o.starts_with("INCOHERENT-1 (")),
            "a deviation_ceiling above max_severity is unreachable: {offenders:?}"
        );
    }

    /// req_e2: the authored ceilings parse out of the REAL corpus TOMLs with the exact values
    /// their prose states — the transcription half of the requirement. Named rules rather than
    /// a count, so a later edit that drops one fails here with the rule's own id.
    ///
    /// FAILS BEFORE THE FIX: every `max_severity`/`deviation_ceiling` lookup below returned
    /// `None` on unmodified HEAD (2799284) — no corpus TOML carried either field.
    #[tokio::test]
    async fn req_e2_named_rules_carry_the_ceilings_their_prose_authored() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let ceiling = |id: &str| -> Option<String> {
            set.iter()
                .find(|r| r.id.0 == id)
                .and_then(|r| r.max_severity.clone())
        };
        // "needs_review, not an automatic critical" / "high-severity ... not an automatic
        // critical" / rejects "unconditionally at critical severity".
        for id in [
            "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
            "SUPABASE-EXPOSURE-MATVIEW-1",
            "SUPABASE-AUTH-EDGE-JWT-1",
            "SUPABASE-RLS-PERMISSIVE-TRUE-1",
            "SUPABASE-STORAGE-OBJECT-POLICY-1",
            "SUPABASE-REALTIME-PRESENCE-IDENTITY-1",
        ] {
            assert_eq!(
                ceiling(id).as_deref(),
                Some("high"),
                "{id}'s prose bounds it at high"
            );
        }
        // "a lower severity than the missing-RLS findings" / rejects "the same tier as the
        // security-relevant RLS findings".
        for id in ["SUPABASE-RLS-NO-POLICY-1", "SUPABASE-RLS-INITPLAN-1"] {
            assert_eq!(
                ceiling(id).as_deref(),
                Some("medium"),
                "{id}'s prose bounds it at medium"
            );
        }
        // "deliberately CAPPED LOW (owner directive)" / "force this rule's findings to
        // severity Low ... regardless of the model's proposed severity".
        for id in [
            "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
            "ARCH-OBSERVABILITY-LOG-CONSISTENCY-1",
            "ARCH-OBSERVABILITY-CORRELATION-ID-1",
        ] {
            assert_eq!(
                ceiling(id).as_deref(),
                Some("low"),
                "{id}'s prose caps it at low"
            );
        }
        // A5.2 forward-drift escape hatches.
        let dev = |id: &str| -> Option<String> {
            set.iter()
                .find(|r| r.id.0 == id)
                .and_then(|r| r.deviation_ceiling.clone())
        };
        for id in [
            "ARCH-STRICT-LAYERING-1",
            "ARCH-REPO-RETURNS-DOMAIN-1",
            "ARCH-SERVICE-DI-1",
            "ARCH-EXPLICIT-TX-1",
            "ARCH-STRUCTURED-ERRORS-1",
            "ARCH-FETCH-THEN-AUTHORIZE-1",
            "RUST-DOMAIN-6",
        ] {
            assert_eq!(
                dev(id).as_deref(),
                Some("high"),
                "{id}'s decision domain compounds forward drift (A5.2)"
            );
        }
    }

    // ── W8 / W5-Q2: severity FLOORS are AUTHORED, not merely implemented ───────────────────

    /// req_w8 (anti-inertness gate): the REAL shipped corpus actually declares severity
    /// FLOORS. This is the test E2 did not have and needed: `max_severity`,
    /// `clamp_to_rule_max_severity` and all four `req_e2_*` tests shipped green against a
    /// corpus where NOT ONE of 417 rules declared a ceiling, so the mechanism was fully
    /// implemented, fully "tested", and completely inert. W8's mechanism is the same shape and
    /// would have failed the same way for free.
    ///
    /// FAILS BEFORE THE FIX: verified on `55515b5` (floor mechanism landed, corpus untouched)
    /// — `grep -rl '^min_severity' crates/rules/principles/ | wc -l` returned 0 of 417, so
    /// `rules_declaring_min_severity` returned an EMPTY vec and this assertion failed. Re-
    /// verified after transcription by `git checkout 55515b5 -- crates/rules/principles/`.
    #[tokio::test]
    async fn req_w8_the_corpus_actually_declares_severity_floors() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return; // skip without the bundled corpus checked out
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let declared = rules_declaring_min_severity(&set);
        assert!(
            !declared.is_empty(),
            "W8 is INERT: not one corpus rule declares `min_severity`, so \
             `report_export::raise_to_rule_min_severity` can never raise anything and every \
             `req_w8_*` unit test passes vacuously. The ~23 rules whose PROSE authors a minimum \
             severity (W5-Q2: \"Floor, it's the minimum rating\") must have it transcribed into \
             the structured field."
        );
    }

    /// req_w8 (corpus-load gate): every floor the real corpus declares is a VALID severity, and
    /// no rule's `min_severity` ranks above its own `max_severity`. Same build-gate philosophy
    /// as `req_e2_corpus_declares_only_valid_severity_ceilings`.
    ///
    /// Why it has teeth in the FLOOR direction: `report_export::normalize_severity` maps any
    /// unrecognized string to `"low"`, and `"low"` ranks at or below everything, so a typo'd
    /// `min_severity = "hgih"` neither errors nor no-ops — it silently EVAPORATES. A field that
    /// looks authored and does nothing is exactly the inert-mechanism failure this whole
    /// severity effort exists to stop.
    #[tokio::test]
    async fn req_w8_corpus_declares_only_valid_severity_floors() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let invalid = rules_with_invalid_severity_floor(&set);
        assert!(
            invalid.is_empty(),
            "every authored `min_severity` must be a valid severity and must not rank above the \
             same rule's `max_severity`: {invalid:?}"
        );
    }

    /// req_w8 (positive detection): the floor gate actually FIRES on both defect shapes it
    /// claims to catch — a typo'd `min_severity`, and a `min_severity` ranking ABOVE the rule's
    /// own `max_severity` (unsatisfiable: the floor demands at least X while the ceiling forbids
    /// anything above Y, with X above Y). Without this the corpus-level gate could pass
    /// vacuously if the detector were ever weakened.
    ///
    /// Also pins the three shapes that must NOT be reported, because each is a real authoring
    /// pattern: a coherent floor/ceiling pair, a floor with no ceiling at all, and a floor
    /// EQUAL to the ceiling (which pins the rule at one severity — what several rules' prose
    /// actually authors).
    #[test]
    fn req_w8_invalid_severity_floor_detector_fires_on_typos_and_unsatisfiable_pairs() {
        let mut set = RuleSet::default();

        let mut typo = bare_rule("TYPO-MIN-1", "rust");
        typo.min_severity = Some("hgih".to_string());
        set.push(typo);

        let mut unsatisfiable = bare_rule("UNSATISFIABLE-1", "rust");
        unsatisfiable.min_severity = Some("critical".to_string());
        unsatisfiable.max_severity = Some("high".to_string());
        set.push(unsatisfiable);

        let mut ok_pair = bare_rule("OK-PAIR-1", "rust");
        ok_pair.min_severity = Some("medium".to_string());
        ok_pair.max_severity = Some("high".to_string());
        set.push(ok_pair);
        let mut ok_floor_only = bare_rule("OK-FLOOR-ONLY-1", "rust");
        ok_floor_only.min_severity = Some("high".to_string());
        set.push(ok_floor_only);
        let mut ok_pinned = bare_rule("OK-PINNED-1", "rust");
        ok_pinned.min_severity = Some("high".to_string());
        ok_pinned.max_severity = Some("high".to_string());
        set.push(ok_pinned);
        set.push(bare_rule("OK-NONE-1", "rust"));

        let offenders = rules_with_invalid_severity_floor(&set);
        assert_eq!(
            offenders.len(),
            2,
            "exactly the two defect shapes must be reported, got {offenders:?}"
        );
        assert!(
            offenders.iter().any(|o| o.starts_with("TYPO-MIN-1 (")),
            "a typo'd min_severity must be caught, not silently coerced to low: {offenders:?}"
        );
        assert!(
            offenders.iter().any(|o| o.starts_with("UNSATISFIABLE-1 (")),
            "a min_severity above max_severity is unsatisfiable: {offenders:?}"
        );
    }

    /// req_w8: the authored floors parse out of the REAL corpus TOMLs with the exact values
    /// their prose states — the transcription half of the requirement. Named rules rather than
    /// a count, so a later edit that drops one fails here with the rule's own id.
    ///
    /// Each rule appears below ONLY because its own TOML states that severity for its own
    /// flagged condition. Nothing here was chosen by what would help any repository score; the
    /// grouping is by the severity the prose names, which is a property of the rule's domain.
    ///
    /// FAILS BEFORE THE FIX: every `min_severity` lookup below returned `None` on `55515b5` —
    /// no corpus TOML carried the field.
    #[tokio::test]
    async fn req_w8_named_rules_carry_the_floors_their_prose_authored() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let floor = |id: &str| -> Option<String> {
            set.iter()
                .find(|r| r.id.0 == id)
                .unwrap_or_else(|| panic!("{id} must exist in the corpus"))
                .min_severity
                .clone()
        };

        // "Flag as CRITICAL severity any such handler" / "Flag any match ... as a CRITICAL
        // finding" / "Any match is a critical, unconditional finding" / "this is always
        // SEVERITY_CRITICAL" / "emit a critical finding" / "Any match is a critical finding".
        // Every one of these uses unconditional language ("any match", "always",
        // "unconditional"), which is what makes a floor at the TOP of the vocabulary a
        // transcription rather than an inference.
        for id in [
            "ARCH-WEBHOOK-SIGNATURE-VERIFICATION-1",
            "ARCH-NO-PAN-STORAGE-1",
            "SUPABASE-AUTH-USERS-EXPOSED-1",
            // "either shape means the secret reaches every visitor's browser, which is the
            // single most severe finding class this audit produces" (default option `why`).
            "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
            "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
            "SUPABASE-RLS-POLICY-DISABLED-1",
            "SUPABASE-RLS-USER-METADATA-1",
        ] {
            assert_eq!(
                floor(id).as_deref(),
                Some("critical"),
                "{id}'s prose names critical, unconditionally, for its own flagged condition"
            );
        }

        // "Flag as high severity: …" / "Report at high severity when …" / "Emit a
        // high-severity finding" / "Severity is High." — one named severity, one failure mode.
        for id in [
            "ARCH-SERVER-COMPUTED-AMOUNT-1",
            "SQL-FK-CASCADE-FINANCIAL-1",
            "SQL-MONEY-FLOAT-1",
            "SQL-PRIVILEGED-WRITE-AUDIT-1",
            "SQL-SOFT-DELETE-PARTIAL-INDEX-1",
            "SUPABASE-AUTH-ANON-SIGNIN-1",
            "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
            "SUPABASE-AUTH-GETSESSION-SERVER-1",
            "SUPABASE-AUTH-MFA-PRIVILEGED-1",
            "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
            "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1",
            "SUPABASE-AUTH-SESSION-LIFETIME-1",
            "SUPABASE-EXPOSURE-MATVIEW-1",
            "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
            "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
            "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
            "SUPABASE-FUNC-SEARCH-PATH-1",
            "SUPABASE-REALTIME-AUTHORIZATION-1",
            "SUPABASE-REALTIME-CHANNEL-SCOPE-1",
            "SUPABASE-REALTIME-PAYLOAD-EXPOSURE-1",
            "SUPABASE-RLS-VIEW-INVOKER-1",
            "SUPABASE-STORAGE-DOWNLOAD-OWNERSHIP-1",
            "SUPABASE-STORAGE-KEY-SANITIZATION-1",
            "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
            "SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1",
        ] {
            assert_eq!(
                floor(id).as_deref(),
                Some("high"),
                "{id}'s prose names high for its own flagged condition"
            );
        }

        // "Flag as medium severity: …" / "emit a medium-severity finding" / "it ships at
        // medium severity".
        for id in [
            "SQL-ENUM-TEXT-CONSTRAINT-1",
            "SQL-TIMESTAMPTZ-1",
            "SUPABASE-EXPOSURE-SCHEMAS-1",
            "SUPABASE-RLS-INITPLAN-1",
            "SUPABASE-RLS-NO-POLICY-1",
        ] {
            assert_eq!(
                floor(id).as_deref(),
                Some("medium"),
                "{id}'s prose names medium for its own flagged condition"
            );
        }

        // MULTI-BRANCH rules: the prose names TWO severities for two branches, so the only
        // coherent RULE-level floor is the LOWEST one named — the single value that cannot
        // inflate either branch. Transcribing the higher one would silently raise the branch
        // the author deliberately softened, which is the over-reach W5 refused and Zach
        // endorsed refusing.
        for (id, why) in [
            // "Flag as high severity a minimum_password_length absent or below 8 … Flag as
            // medium severity a password_requirements that is unset or digits-only."
            ("SUPABASE-AUTH-PASSWORD-POLICY-1", "high + medium branches"),
            // "Write-side matches … are high severity … SELECT-only with USING (true) …
            // emit a medium-severity finding with needs_review."
            ("SUPABASE-RLS-PERMISSIVE-TRUE-1", "high + medium branches"),
            // "Write/delete matches are high severity; read-only unscoped policies are medium
            // with needs_review."
            ("SUPABASE-STORAGE-OBJECT-POLICY-1", "high + medium branches"),
            // "Flag as medium-to-high severity (scaled by how sensitive the extra field is)"
            // — an explicit RANGE, whose bottom is the floor and whose top is the ceiling
            // W5 already transcribed.
            (
                "SUPABASE-REALTIME-PRESENCE-IDENTITY-1",
                "medium-to-high range",
            ),
            // "a credential … is HIGH … regulated PII … is calibrated MEDIUM; treating it as
            // HIGH would inflate it past genuinely credential-bearing findings."
            ("SEC-NO-SENSITIVE-DATA-IN-LOGS-1", "high + medium branches"),
        ] {
            assert_eq!(
                floor(id).as_deref(),
                Some("medium"),
                "{id} names two severities ({why}); the rule-level floor is the lower one"
            );
        }

        // CONTROL (attribution): a rule whose prose caps itself at LOW must carry NO floor.
        // Without this the suite would also pass if someone blanket-assigned a floor across
        // the corpus, and the three observability rules are precisely where a floor would do
        // the most damage — their whole authored intent is "never higher".
        for id in [
            "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
            "ARCH-OBSERVABILITY-LOG-CONSISTENCY-1",
            "ARCH-OBSERVABILITY-CORRELATION-ID-1",
        ] {
            assert_eq!(
                floor(id),
                None,
                "{id} is CAPPED LOW by owner directive; a floor here would contradict it"
            );
        }
    }

    /// req_w5_q3: every per-stack MIRROR of the universal architectural spine carries the same
    /// `deviation_ceiling = "high"` the spine rule carries. Zach, 2026-10-11: *"yes these
    /// should match."*
    ///
    /// The defect this closes: a Java repo with broken layering capped at `medium` under
    /// `JAVA-SPRING-LAYERED-ARCHITECTURE-1` while a repo on an unmirrored stack reached `high`
    /// under `ARCH-STRICT-LAYERING-1`. Identical defect, different severity, decided purely by
    /// which rule happened to fire. Severity must describe the problem, not the corpus filing.
    ///
    /// The mapping is asserted per-mirror, never as a count or a folder pattern, because the
    /// ruling's own constraint is that a per-stack rule which does not correspond to one of the
    /// seven spine rules gets NOTHING.
    ///
    /// W9, 2026-10-11 completed the mirror set across three spine concepts that were still
    /// internally inconsistent, on the owner's direction that *"they all must be consistent, so
    /// make it so"*: the five `ARCH-STRICT-LAYERING-1` mirrors W8-Q3 had identified but left
    /// unauthored because they sit on the 58-rule classification hold (see
    /// `req_a5_7_no_held_rule_carries_an_inferred_deviation_ceiling`, which now carries that
    /// authorization as an explicit allow-list), plus the `ARCH-SERVICE-DI-1` and
    /// `ARCH-EXPLICIT-TX-1` mirrors W8 simply never matched. Before it, a Python or Ruby or Go
    /// repo with broken layering still capped at `medium` while Java, C#, JS and Rust reached
    /// `high` — the exact asymmetry this requirement exists to remove, merely relocated.
    ///
    /// W10, 2026-10-11 closed the loop across the WHOLE corpus, on this direction verbatim:
    ///
    /// > "Extend it. Consider this directive, get all the rules consistent in this sense, you
    /// > have the green light for ALL instances of this."
    ///
    /// Three pairs were added, each found by reading the spine rule's decided axis and option
    /// set and then sweeping for rules deciding that same axis in a stack-specific vocabulary —
    /// never by name or folder, because both of those produce false positives and false
    /// negatives here (`CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1` sounds like a layering
    /// mirror and is not; `ARCH-STRUCTURED-ERRORS-1`'s four mirrors carry four different naming
    /// conventions). `ARCH-REPO-RETURNS-DOMAIN-1` now has the mirror W9-Q2 recorded as missing,
    /// so SIX of the seven spine concepts have a matched mirror set. The seventh,
    /// `RUST-DOMAIN-6`, still has none, and `ARCH-FETCH-THEN-AUTHORIZE-1`'s nearest neighbours
    /// are deliberately NOT mirrors (they decide where authorization is DECLARED; the spine
    /// rule decides its ORDER relative to the fetch, and an annotation firing before the method
    /// body is closer to the option the spine rule REJECTS). Both are recorded in the plan's
    /// report rather than guessed at here.
    ///
    /// FAILS BEFORE THE FIX: every lookup below returned `None` on `d94c463`; only the seven
    /// spine rules declared a `deviation_ceiling`. The ten pairs W9 added returned `None` on
    /// `2d80e2f`, and the three W10 added returned `None` on `b814dd2` — each proved by
    /// reverting `crates/rules/principles/` to that commit and re-running.
    #[tokio::test]
    async fn req_w5_q3_every_per_stack_mirror_inherits_its_spine_rules_deviation_ceiling() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        let set = load_corpus(path).await.expect("corpus loads");
        let ceiling = |id: &str| -> Option<String> {
            set.iter()
                .find(|r| r.id.0 == id)
                .unwrap_or_else(|| panic!("{id} must exist in the corpus"))
                .deviation_ceiling
                .clone()
        };

        // (spine rule, its per-stack mirrors). Both sides are asserted, so this also fails if
        // a spine rule ever LOSES its ceiling and the mirrors silently out-rank their origin.
        const SPINE_AND_MIRRORS: [(&str, &[&str]); 5] = [
            (
                "ARCH-STRICT-LAYERING-1",
                &[
                    "JAVA-SPRING-LAYERED-ARCHITECTURE-1",
                    "JAVA-SPRING-THIN-CONTROLLERS-1",
                    "JAVASCRIPT-EXPRESS-THIN-CONTROLLERS-1",
                    "JAVASCRIPT-NEST-THIN-CONTROLLERS-DELEGATE-1",
                    "CSHARP-ASPNETCORE-THIN-CONTROLLERS-1",
                    "RUST-AXUM-HANDLERS-THIN-DELEGATE-1",
                    // W9: the five W8-Q3 mirrors. All five are on the 58-rule classification
                    // hold, and the owner's W9 direction authorizes the ceiling (not the `kind`)
                    // specifically so this set stops being internally inconsistent.
                    "GO-HANDLER-SERVICE-REPOSITORY-1",
                    "PYTHON-DJANGO-FAT-MODEL-SERVICE-1",
                    "PYTHON-FLASK-SERVICE-LAYER-1",
                    "PYTHON-SERVICE-LAYER-1",
                    "RUBY-RAILS-SKINNY-CONTROLLERS-1",
                    // W10: not held. The corpus asserts this equivalence itself — this rule's
                    // own `why` says "The pattern parallels the Express backend rule: route
                    // handlers delegate to a service layer; Angular components delegate to a
                    // service layer", and the Express rule is already a matched mirror above.
                    "JAVASCRIPT-ANGULAR-TYPED-HTTPCLIENT-1",
                ],
            ),
            (
                "ARCH-SERVICE-DI-1",
                &[
                    "JAVA-SPRING-CONSTRUCTOR-INJECTION-1",
                    "JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1",
                    "RUST-AXUM-STATE-SHARED-DEPS-1",
                    // W9: neither is held — W8 simply never matched them. Both re-litigate how
                    // a class obtains its dependencies, and both reject an option the spine
                    // rule rejects (the service locator; manual `new` past the DI container).
                    "CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1",
                    "JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1",
                ],
            ),
            (
                "ARCH-EXPLICIT-TX-1",
                &[
                    "JAVA-SPRING-TRANSACTIONAL-SERVICE-LAYER-1",
                    "RUST-DOMAIN-7",
                    "RUST-SEAORM-INTRA-AGGREGATE-TX-1",
                    // W9: none is held — W8 simply never matched them. Each re-litigates the
                    // same transactional-ownership contract, and each names as its violating
                    // option the spine rule's own rejected per-call short transaction.
                    "CSHARP-EFCORE-EXPLICIT-TRANSACTIONS-1",
                    "GO-GORM-TRANSACTIONS-MULTI-WRITE-1",
                    "RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1",
                ],
            ),
            (
                "ARCH-STRUCTURED-ERRORS-1",
                &[
                    "PYTHON-FLASK-ERROR-HANDLERS-1",
                    "JAVASCRIPT-EXPRESS-CENTRAL-ERROR-HANDLER-1",
                    "GO-WEB-STRUCTURED-ERROR-RESPONSES-1",
                    "RUST-AXUM-TYPED-ERROR-RESPONSE-1",
                    // W10: not held. gRPC fixes the envelope at the protocol level, so the
                    // status code IS the error contract; both of this rule's violating options
                    // are spine-rejected positions (blanket INTERNAL with the detail in the
                    // human message = plain-string errors; inconsistent per handler =
                    // per-endpoint custom shapes). `kind = "defect"`, so the ceiling is inert
                    // as shipped — see W9-Q1 and this rule's own TOML comment.
                    "GO-GRPC-STATUS-CODES-CORRECT-1",
                ],
            ),
            (
                // W10 closed W9-Q2: this spine concept had no matched mirror at all, because
                // its only one sits on the 58-rule classification hold. The owner's W10
                // direction names it explicitly and he verified the mapping himself.
                "ARCH-REPO-RETURNS-DOMAIN-1",
                &["RUST-SEAORM-RAW-SQL-ESCAPE-1"],
            ),
        ];
        for (spine, mirrors) in SPINE_AND_MIRRORS {
            assert_eq!(
                ceiling(spine).as_deref(),
                Some("high"),
                "{spine} is the spine rule the mirrors below inherit from; without its own \
                 ceiling the inheritance has no origin"
            );
            for mirror in mirrors {
                assert_eq!(
                    ceiling(mirror).as_deref(),
                    Some("high"),
                    "{mirror} mirrors {spine}; W5-Q3 says the identical defect may not cap \
                     lower just because a per-stack rule fired instead of the general one"
                );
            }
        }

        // CONTROL (attribution): a per-stack rule in the SAME folders as several mirrors, that
        // does NOT re-litigate any of the seven spine rules, must carry NO ceiling. Without
        // this the test would also pass if someone blanket-assigned `high` to every framework
        // rule, which is precisely what the ruling's own constraint forbids.
        for id in [
            // Spring, but about config binding and N+1 fetching, not layering/DI/tx/errors.
            "JAVA-SPRING-CONFIGURATION-PROPERTIES-1",
            "JAVA-SPRING-NO-NPLUS1-FETCH-JOIN-1",
            // Axum, but about timeouts and middleware placement.
            "RUST-AXUM-TIMEOUT-LIMITS-1",
            // Express, but about security headers.
            "JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1",
            // W10 added a mirror in each of these three folders, so each now needs its own
            // same-folder control or a blanket-by-folder assignment would pass again.
            // Angular, but about change-detection strategy.
            "JAVASCRIPT-ANGULAR-ONPUSH-CHANGE-DETECTION-1",
            // go/grpc, but about hand-editing generated code.
            "GO-GRPC-PROTO-GENERATED-IMMUTABLE-1",
            // rust/seaorm, but about declaring FK relations on both sides.
            "RUST-ENTITIES-4",
        ] {
            assert_eq!(
                ceiling(id),
                None,
                "{id} does not re-litigate any of the seven spine rules, so W5-Q3 gives it \
                 nothing; a ceiling here means the mirrors were blanket-assigned by folder"
            );
        }

        // CONTROL (near-miss attribution), added by W10. Each of these decides an axis that
        // READS like a spine concept and is not one, and each was ruled out on the text of its
        // own TOML. They are pinned here so the ruling cannot be silently reversed: flipping
        // one into a mirror now requires deliberately editing this list, and the reason it is
        // not a mirror is stated beside it.
        for (id, why_not) in [
            // Sounds like ARCH-SERVICE-DI-1. Decides interface GRANULARITY by cohesion, and its
            // own `why` explicitly disclaims the DI rationale ("not mockability"). No
            // composition root, no abstraction-vs-locator axis.
            (
                "JAVA-SMALL-INTERFACES-1",
                "interface granularity, not DI posture",
            ),
            // Sounds like ARCH-SERVICE-DI-1 (provide at the app root vs. global statics).
            // Decides how cross-tree UI STATE propagates, judged on re-render scope; the thing
            // provided is state, not a dependency abstraction.
            (
                "RUST-DIOXUS-4",
                "cross-tree UI state propagation, not DI posture",
            ),
            // Sounds like ARCH-SERVICE-DI-1. All three options inject something through the
            // constructor; the axis is whether config arrives as a typed startup-validated POCO
            // or as string-keyed reads. Matches W9's PYTHON-FASTAPI-DI-SESSION-1 reasoning.
            (
                "CSHARP-ASPNETCORE-OPTIONS-PATTERN-1",
                "config typing, not DI posture",
            ),
            // Sounds like ARCH-REPO-RETURNS-DOMAIN-1. Its directive and `why` are about the API
            // serialization boundary ("forces every API response through an explicit DTO plus
            // mapper"), which is ARCH-API-DTOS-1's axis — and that spine rule has no ceiling.
            (
                "RUST-ENTITIES-2",
                "HTTP/serialization boundary, not the repository boundary",
            ),
            // Same reason, and this is W8-Q2's ruling preserved: both of these guard the HTTP
            // boundary, so they mirror ARCH-API-DTOS-1, which carries no ceiling.
            (
                "JAVA-SPRING-DTO-BOUNDARY-1",
                "HTTP boundary, not the repository boundary",
            ),
            (
                "PYTHON-FASTAPI-PYDANTIC-MODELS-1",
                "HTTP boundary, not the repository boundary",
            ),
            // Sounds like ARCH-STRUCTURED-ERRORS-1. Its own title says "regardless of the
            // envelope shape" — the corpus text rules itself out.
            (
                "ARCH-NO-LEAKY-ERROR-DETAILS-1",
                "leak-prevention, not the envelope contract",
            ),
            // Sounds like ARCH-STRICT-LAYERING-1. Decides responsibility distribution AMONG
            // components; both options keep a service layer. The Angular layering mirror is
            // JAVASCRIPT-ANGULAR-TYPED-HTTPCLIENT-1, matched above.
            (
                "JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1",
                "component decomposition, not the data-access boundary",
            ),
            // W9's two rulings, preserved. `default = false` on the C# one: it authors no
            // default position, and BOTH its legitimate options already require layering.
            (
                "CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1",
                "routing style, not layering",
            ),
            (
                "PYTHON-FASTAPI-DI-SESSION-1",
                "resource lifecycle, not DI posture",
            ),
            // W8-Q2's ruling, preserved: these decide WHERE authorization is declared, while
            // ARCH-FETCH-THEN-AUTHORIZE-1 decides its ORDER relative to the fetch. An
            // annotation that fires before the method body is nearer the option the spine rule
            // REJECTS than the one it adopts.
            (
                "JAVA-SPRING-METHOD-SECURITY-1",
                "where authz is declared, not fetch/authz order",
            ),
            (
                "CSHARP-ASPNETCORE-AUTHORIZE-ATTRIBUTE-1",
                "where authz is declared, not fetch/authz order",
            ),
            (
                "JAVASCRIPT-NEST-GUARDS-FOR-AUTH-1",
                "where authn is declared, not fetch/authz order",
            ),
            (
                "PYTHON-FASTAPI-AUTH-DEPENDENCY-1",
                "where authn is declared, not fetch/authz order",
            ),
            (
                "PYTHON-FLASK-AUTH-ON-PROTECTED-ROUTES-1",
                "where authn is declared, not fetch/authz order",
            ),
            // Same-stack SIBLING of RUST-DOMAIN-6 rather than a per-stack re-litigation:
            // RUST-DOMAIN-6's own `why` says it ASSUMES this rule's default and decides the
            // narrower follow-on question. Capping it would extend the spine set, which is a
            // different decision from "mirrors match their spine" — reported, not authored.
            (
                "RUST-DOMAIN-4",
                "parent rule of the spine rule, not a per-stack mirror",
            ),
        ] {
            assert_eq!(
                ceiling(id),
                None,
                "{id} is a deliberate NEAR-MISS, ruled out because its decided axis is \
                 {why_not}; a ceiling here means a later round matched by name or vibe instead \
                 of by the axis the rule actually decides"
            );
        }
    }

    /// HARD CONSTRAINT check (W8's mirror of
    /// `req_a5_7_no_held_rule_carries_an_inferred_deviation_ceiling`): a rule on the owner's
    /// 58-rule classification hold list may carry a `min_severity` ONLY when that floor was
    /// transcribed verbatim from the rule's own prose.
    ///
    /// The distinction is the whole point of the hold. Transcribing a rule's own explicit
    /// prose floor is the OWNER'S authored intent and is fine even for a held rule — exactly
    /// the reasoning that let W5 transcribe `max_severity` onto held rules. INVENTING one
    /// presumes the classification he paused. A plain "no floors on held rules" ban would have
    /// been wrong (it would silently drop 12 of the 23 rules W5-Q2 is about, most of the
    /// `SUPABASE-FUNC-*` and `SUPABASE-STORAGE-*` set among them), so the guard is an explicit
    /// allow-list instead: a later agent cannot add a floor to a held rule without deliberately
    /// editing this list, and the 58 ids are inlined here — not read from the plan doc — so the
    /// hold survives any edit to that document.
    #[tokio::test]
    async fn req_w8_a_held_rule_carries_a_floor_only_if_its_own_prose_authored_one() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        // The 58 rules whose classification review is ON HOLD
        // (`docs/plans/2026-10-09_rule-kind-classification.md`).
        const HELD: [&str; 58] = [
            "ARCH-OBSERVABILITY-CORRELATION-ID-1",
            "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
            "ARCH-PARALLEL-INDEPENDENT-1",
            "ARCH-SERVER-CLIENT-PROJECTION-1",
            "ARCH-TRUNK-SYNC-1",
            "CICD-BUILD-PROVENANCE-1",
            "CICD-DEPENDENCY-AUDIT-1",
            "CSHARP-TESTING-MOCK-AT-BOUNDARY-1",
            "CSHARP-TESTING-SEPARATE-UNIT-PROJECT-1",
            "GO-ACCEPT-INTERFACES-RETURN-STRUCTS-1",
            "GO-HANDLER-SERVICE-REPOSITORY-1",
            "GO-TESTING-COLOCATED-TEST-FILES-1",
            "INTEGRATION-EVENT-WIRING-1",
            "JAVA-TESTING-MOCK-BOUNDARIES-1",
            "JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1",
            "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
            "JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1",
            "PROC-MIGRATION-ROLE-GRANTS-1",
            "PROCESS-ADO-LINK-1",
            "PROCESS-BRANCH-NAMING-1",
            "PROCESS-COMMIT-DOC-1",
            "PROCESS-CONVENTIONAL-COMMIT-1",
            "PYTHON-DJANGO-FAT-MODEL-SERVICE-1",
            "PYTHON-DJANGO-FORMS-SERIALIZERS-VALIDATE-1",
            "PYTHON-FLASK-SERVICE-LAYER-1",
            "PYTHON-SERVICE-LAYER-1",
            "PYTHON-TESTING-MOCK-BOUNDARIES-1",
            "RUBY-CONCERNS-JUDICIOUSLY-1",
            "RUBY-FROZEN-STRING-LITERAL-1",
            "RUBY-RAILS-SKINNY-CONTROLLERS-1",
            "RUBY-TESTING-INTEGRATION-LOCATION-1",
            "RUBY-TESTING-UNIT-FILE-LOCATION-1",
            "RUST-SEAORM-RAW-SQL-ESCAPE-1",
            "SEC-NO-COMMAND-INJECTION-1",
            "SEC-NO-PRIVATE-KEY-1",
            "SEC-NO-VENDOR-TOKEN-1",
            "SPIRIT-ROBUSTNESS-1",
            "SQL-PRIVILEGED-WRITE-AUDIT-1",
            "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
            "SUPABASE-AUTH-GETSESSION-SERVER-1",
            "SUPABASE-AUTH-MFA-PRIVILEGED-1",
            "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
            "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
            "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
            "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
            "SUPABASE-FUNC-SEARCH-PATH-1",
            "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
            "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
            "SUPABASE-RLS-INITPLAN-1",
            "SUPABASE-RLS-NO-POLICY-1",
            "SUPABASE-RLS-PERMISSIVE-TRUE-1",
            "SUPABASE-RLS-USER-METADATA-1",
            "SUPABASE-STORAGE-KEY-SANITIZATION-1",
            "SUPABASE-STORAGE-OBJECT-POLICY-1",
            "SUPABASE-STORAGE-SIGNED-URL-1",
            "SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1",
            "TESTING-AS-DOCUMENTATION-1",
            "UI-QUERY-LIBRARY-1",
        ];
        // Held rules whose OWN prose names a severity for their OWN flagged condition, with the
        // construction transcribed. Every entry is quoted in
        // `req_w8_named_rules_carry_the_floors_their_prose_authored`'s comments.
        const TRANSCRIBED_FROM_OWN_PROSE: [&str; 18] = [
            "SQL-PRIVILEGED-WRITE-AUDIT-1",
            "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
            "SUPABASE-AUTH-GETSESSION-SERVER-1",
            "SUPABASE-AUTH-MFA-PRIVILEGED-1",
            "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
            "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
            "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
            "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
            "SUPABASE-FUNC-SEARCH-PATH-1",
            // Item 19 (2026-10-28), corpus-audit Group 1 note: "no severity bound while its
            // prose says 'unconditionally critical' twice". That prose was later revoiced; the
            // default option's `why` still names the severity: "the single most severe finding
            // class this audit produces".
            "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
            "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
            "SUPABASE-RLS-INITPLAN-1",
            "SUPABASE-RLS-NO-POLICY-1",
            "SUPABASE-RLS-PERMISSIVE-TRUE-1",
            "SUPABASE-RLS-USER-METADATA-1",
            "SUPABASE-STORAGE-KEY-SANITIZATION-1",
            "SUPABASE-STORAGE-OBJECT-POLICY-1",
            "SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1",
        ];
        let set = load_corpus(path).await.expect("corpus loads");
        let violations: Vec<String> = set
            .iter()
            .filter(|r| {
                HELD.contains(&r.id.0.as_str())
                    && r.min_severity.is_some()
                    && !TRANSCRIBED_FROM_OWN_PROSE.contains(&r.id.0.as_str())
            })
            .map(|r| r.id.0.clone())
            .collect();
        assert!(
            violations.is_empty(),
            "a rule on the owner's 58-rule classification hold list may carry a `min_severity` \
             only when it was transcribed from the rule's OWN prose; these were not on that \
             list: {violations:?}"
        );
        // The allow-list must not rot into cover for rules that no longer carry a floor.
        let stale: Vec<&str> = TRANSCRIBED_FROM_OWN_PROSE
            .iter()
            .copied()
            .filter(|id| {
                set.iter()
                    .find(|r| r.id.0 == *id)
                    .is_none_or(|r| r.min_severity.is_none())
            })
            .collect();
        assert!(
            stale.is_empty(),
            "these ids are allow-listed as held rules carrying a transcribed floor but no \
             longer carry one — remove them from the list rather than leaving a blanket \
             exemption behind: {stale:?}"
        );
        // And every allow-listed id must really be on the hold list, or the allow-list is
        // quietly exempting rules the hold never covered.
        let not_held: Vec<&str> = TRANSCRIBED_FROM_OWN_PROSE
            .iter()
            .copied()
            .filter(|id| !HELD.contains(id))
            .collect();
        assert!(
            not_held.is_empty(),
            "the allow-list may only name rules that are actually on the hold list: {not_held:?}"
        );
    }

    /// HARD CONSTRAINT check: a rule in the `NEEDS OWNER DECISION (58 rules)` hold list
    /// (`docs/plans/2026-10-09_rule-kind-classification.md`) may carry a `deviation_ceiling`
    /// ONLY where the owner named it himself. INFERRING one is still banned, because inferring
    /// presumes the kind classification he explicitly paused ("pause the rule review for me for
    /// when I can sit down and look at these"). Transcribing an explicit prose `max_severity` on
    /// a held rule IS permitted — that is his own authored intent — so this gate deliberately
    /// checks only the deviation field.
    ///
    /// W9, 2026-10-11 — the allow-list below exists because of this direction, verbatim:
    ///
    /// > "Lift the hold and ensure all are consistent here, regardless of any holds or locks in
    /// > place, you can make individual changes based on what I direct. In this case, I direct
    /// > that they all must be consistent, so make it so."
    ///
    /// It authorized the `deviation_ceiling` on the five textbook `ARCH-STRICT-LAYERING-1`
    /// mirrors that W8-Q3 identified and then refused to author.
    ///
    /// W10, 2026-10-11 — the owner then closed the loop across the whole corpus, verbatim:
    ///
    /// > "Extend it. Consider this directive, get all the rules consistent in this sense, you
    /// > have the green light for ALL instances of this."
    ///
    /// So the authorization is no longer those five ids: it is **every held rule that is a
    /// genuine per-stack mirror of a spine rule carrying the ceiling**, and nothing else. Read
    /// carefully, because the two halves of that sentence do different work:
    ///
    /// - The scope widened. W10's corpus-wide sweep found exactly one further held mirror,
    ///   `RUST-SEAORM-RAW-SQL-ESCAPE-1` (`ARCH-REPO-RETURNS-DOMAIN-1`'s only per-stack
    ///   re-litigation, the asymmetry W9-Q2 recorded and could not close). The owner named it
    ///   explicitly and said he verified the mapping himself.
    /// - **The hold on `kind` is NOT lifted**, by either direction. No held rule's `kind` was
    ///   touched in W9 or W10, the classification review of the 58 stays paused, and `A5.7`
    ///   therefore stays `Exempt` in the traceability registry.
    ///
    /// Why a ceiling on a held rule is safe where a `kind` edit would not be: all six ship as
    /// `kind = "decision"`, so they ALREADY emit deviation rows today, and a ceiling only BOUNDS
    /// rows that already exist. It presumes nothing about the paused classification. If the owner
    /// later reclassifies one as `defect`, the ceiling becomes dead config — a `defect`-kind rule
    /// never enters the alternatives pass ([`Rule::is_defect_kind`]) so it emits no deviations —
    /// rather than wrong behaviour.
    ///
    /// The guard stays an explicit ALLOW-LIST and was deliberately NOT loosened into a predicate
    /// such as "any held rule may carry a ceiling", which the broader direction would have
    /// superficially licensed and which would have made this test unable to fail. "Genuine
    /// per-stack mirror" is a judgement about a rule's decided axis that no predicate over the
    /// corpus can evaluate, so it is recorded here as ids, with the spine rule each one mirrors
    /// named beside it. A ceiling cannot reach any OTHER held rule without a later agent
    /// deliberately editing this list, which is a reviewable act rather than a silent one; the
    /// 52 other held rules are still blocked. Three anti-rot assertions back it up — every
    /// allow-listed id must really be on the hold list, must really carry a ceiling, and must
    /// carry the spine rule's own `high` — so the list cannot decay into a blanket exemption at
    /// an arbitrary severity.
    ///
    /// FAILS BEFORE THE CHANGE in the direction that matters: adding a `deviation_ceiling` to any
    /// held rule outside `AUTHORIZED_BY_OWNER_DIRECTION` still reds this test. W9 proved it on
    /// `SPIRIT-ROBUSTNESS-1`; W10 re-proved it after widening the list, on
    /// `SUPABASE-STORAGE-SIGNED-URL-1` (held, unnamed, and not a mirror of anything).
    #[tokio::test]
    async fn req_a5_7_no_held_rule_carries_an_inferred_deviation_ceiling() {
        let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
        if !path.exists() {
            return;
        }
        // The 58 rules whose classification review is ON HOLD.
        const HELD: [&str; 58] = [
            "ARCH-OBSERVABILITY-CORRELATION-ID-1",
            "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
            "ARCH-PARALLEL-INDEPENDENT-1",
            "ARCH-SERVER-CLIENT-PROJECTION-1",
            "ARCH-TRUNK-SYNC-1",
            "CICD-BUILD-PROVENANCE-1",
            "CICD-DEPENDENCY-AUDIT-1",
            "CSHARP-TESTING-MOCK-AT-BOUNDARY-1",
            "CSHARP-TESTING-SEPARATE-UNIT-PROJECT-1",
            "GO-ACCEPT-INTERFACES-RETURN-STRUCTS-1",
            "GO-HANDLER-SERVICE-REPOSITORY-1",
            "GO-TESTING-COLOCATED-TEST-FILES-1",
            "INTEGRATION-EVENT-WIRING-1",
            "JAVA-TESTING-MOCK-BOUNDARIES-1",
            "JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1",
            "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
            "JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1",
            "PROC-MIGRATION-ROLE-GRANTS-1",
            "PROCESS-ADO-LINK-1",
            "PROCESS-BRANCH-NAMING-1",
            "PROCESS-COMMIT-DOC-1",
            "PROCESS-CONVENTIONAL-COMMIT-1",
            "PYTHON-DJANGO-FAT-MODEL-SERVICE-1",
            "PYTHON-DJANGO-FORMS-SERIALIZERS-VALIDATE-1",
            "PYTHON-FLASK-SERVICE-LAYER-1",
            "PYTHON-SERVICE-LAYER-1",
            "PYTHON-TESTING-MOCK-BOUNDARIES-1",
            "RUBY-CONCERNS-JUDICIOUSLY-1",
            "RUBY-FROZEN-STRING-LITERAL-1",
            "RUBY-RAILS-SKINNY-CONTROLLERS-1",
            "RUBY-TESTING-INTEGRATION-LOCATION-1",
            "RUBY-TESTING-UNIT-FILE-LOCATION-1",
            "RUST-SEAORM-RAW-SQL-ESCAPE-1",
            "SEC-NO-COMMAND-INJECTION-1",
            "SEC-NO-PRIVATE-KEY-1",
            "SEC-NO-VENDOR-TOKEN-1",
            "SPIRIT-ROBUSTNESS-1",
            "SQL-PRIVILEGED-WRITE-AUDIT-1",
            "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
            "SUPABASE-AUTH-GETSESSION-SERVER-1",
            "SUPABASE-AUTH-MFA-PRIVILEGED-1",
            "SUPABASE-AUTH-REDIRECT-ALLOWLIST-1",
            "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
            "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
            "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
            "SUPABASE-FUNC-SEARCH-PATH-1",
            "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
            "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
            "SUPABASE-RLS-INITPLAN-1",
            "SUPABASE-RLS-NO-POLICY-1",
            "SUPABASE-RLS-PERMISSIVE-TRUE-1",
            "SUPABASE-RLS-USER-METADATA-1",
            "SUPABASE-STORAGE-KEY-SANITIZATION-1",
            "SUPABASE-STORAGE-OBJECT-POLICY-1",
            "SUPABASE-STORAGE-SIGNED-URL-1",
            "SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1",
            "TESTING-AS-DOCUMENTATION-1",
            "UI-QUERY-LIBRARY-1",
        ];
        // The ONLY held rules the owner's direction authorizes a `deviation_ceiling` on: held
        // rules that are a genuine per-stack mirror of a spine rule carrying the ceiling. Each
        // is an unambiguous re-litigation of its spine rule by its own title and directive, and
        // each records the spine rule AND this authorization in a comment above its own
        // `deviation_ceiling` line, so the TOML explains itself without this test. The spine
        // rule is named here too, so the pairing is reviewable without opening six files.
        //
        // Adding an id here is the ONLY way to let a ceiling onto a held rule. Do not add one
        // without (a) a direction from the owner, quoted at the point of the edit, and (b) the
        // spine rule it mirrors, established from that rule's own decided axis and option set —
        // never from its name or its folder. The 52 held rules absent from this list are still
        // blocked, and that is the one job this test exists to do.
        const AUTHORIZED_BY_OWNER_DIRECTION: [(&str, &str); 6] = [
            // W9 — the five `ARCH-STRICT-LAYERING-1` mirrors W8-Q3 named and left unauthored.
            ("GO-HANDLER-SERVICE-REPOSITORY-1", "ARCH-STRICT-LAYERING-1"),
            (
                "PYTHON-DJANGO-FAT-MODEL-SERVICE-1",
                "ARCH-STRICT-LAYERING-1",
            ),
            ("PYTHON-FLASK-SERVICE-LAYER-1", "ARCH-STRICT-LAYERING-1"),
            ("PYTHON-SERVICE-LAYER-1", "ARCH-STRICT-LAYERING-1"),
            ("RUBY-RAILS-SKINNY-CONTROLLERS-1", "ARCH-STRICT-LAYERING-1"),
            // W10 — the one further held mirror the corpus-wide sweep found. Its title names
            // the seam ("... at the repository boundary") and its adopted directive states the
            // spine rule's commitment verbatim: "only typed domain types cross the trait
            // boundary". The owner named this rule and verified the mapping himself.
            ("RUST-SEAORM-RAW-SQL-ESCAPE-1", "ARCH-REPO-RETURNS-DOMAIN-1"),
        ];
        let set = load_corpus(path).await.expect("corpus loads");
        let ceiling_of = |id: &str| -> Option<String> {
            set.iter()
                .find(|r| r.id.0 == id)
                .unwrap_or_else(|| panic!("{id} must exist in the corpus"))
                .deviation_ceiling
                .clone()
        };
        // The gate proper: every held rule OTHER than the ones he named must still carry nothing.
        let violations: Vec<String> = set
            .iter()
            .filter(|r| {
                HELD.contains(&r.id.0.as_str())
                    && r.deviation_ceiling.is_some()
                    && !AUTHORIZED_BY_OWNER_DIRECTION
                        .iter()
                        .any(|(id, _)| *id == r.id.0.as_str())
            })
            .map(|r| r.id.0.clone())
            .collect();
        assert!(
            violations.is_empty(),
            "a rule on the owner's 58-rule classification hold list must not carry an inferred \
             `deviation_ceiling` — only a held rule his direction named, and that is a genuine \
             per-stack mirror of a spine rule, may carry one. These were not among them: \
             {violations:?}"
        );
        // ANTI-ROT 1: the allow-list may only name rules that are actually on the hold list, or
        // it is quietly exempting rules the hold never covered in the first place.
        let not_held: Vec<&str> = AUTHORIZED_BY_OWNER_DIRECTION
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !HELD.contains(id))
            .collect();
        assert!(
            not_held.is_empty(),
            "the allow-list may only name rules that are actually on the hold list: {not_held:?}"
        );
        // ANTI-ROT 2 + 3: every allow-listed id must still carry a ceiling, and specifically the
        // `high` its spine rule carries. Without this the list could decay into a standing
        // exemption at any severity for rules nobody is still authoring.
        let wrong: Vec<String> = AUTHORIZED_BY_OWNER_DIRECTION
            .iter()
            .filter_map(|(id, spine)| {
                let ceiling = ceiling_of(id);
                (ceiling.as_deref() != Some("high"))
                    .then(|| format!("{id} (mirror of {spine}) = {ceiling:?}"))
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "the owner's direction was that the per-stack mirror set be CONSISTENT, so each \
             allow-listed held rule must carry its spine rule's own \
             `deviation_ceiling = \"high\"`; remove the id from the allow-list rather than \
             leaving a blanket exemption behind: {wrong:?}"
        );
        // ANTI-ROT 4 (W10): the spine rule each exemption claims to inherit from must itself
        // carry the ceiling. An exemption whose stated origin has no ceiling is not an
        // inheritance, it is a bare exemption wearing a justification.
        let originless: Vec<String> = AUTHORIZED_BY_OWNER_DIRECTION
            .iter()
            .filter_map(|(id, spine)| {
                let ceiling = ceiling_of(spine);
                (ceiling.as_deref() != Some("high"))
                    .then(|| format!("{id} claims to inherit from {spine}, which has {ceiling:?}"))
            })
            .collect();
        assert!(
            originless.is_empty(),
            "a held rule's ceiling is authorized only as INHERITANCE from a spine rule that \
             carries the ceiling itself; these exemptions have no origin: {originless:?}"
        );
    }

    // ── applies_to (token-efficiency rank 7) ──────────────────────────────────

    /// Write one rule file with `extra` top-level TOML lines and load the folder leniently.
    async fn load_with_extra(extra: &str) -> (RuleSet, Vec<(PathBuf, RulesError)>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("universal");
        std::fs::create_dir_all(&dir).expect("create dirs");
        std::fs::write(
            dir.join("scoped.toml"),
            format!(
                "id = \"SCOPED-TEST-1\"\ntitle = \"Scoped\"\nenforcement = \"prose\"\n{extra}\n"
            ),
        )
        .expect("write toml");
        load_corpus_lenient(tmp.path()).await
    }

    #[tokio::test]
    async fn a_rule_without_applies_to_applies_everywhere() {
        let (set, errors) = load_with_extra("").await;
        assert!(errors.is_empty(), "{errors:?}");
        assert!(set.get_by_id("SCOPED-TEST-1").unwrap().applies_to.is_none());
    }

    #[tokio::test]
    async fn a_declared_applies_to_loads_onto_the_rule() {
        let (set, errors) =
            load_with_extra(r#"applies_to = { paths = ["**/*.sql"], languages = ["rust"] }"#).await;
        assert!(errors.is_empty(), "{errors:?}");
        let a = set
            .get_by_id("SCOPED-TEST-1")
            .unwrap()
            .applies_to
            .clone()
            .expect("declared");
        assert_eq!(a.paths, vec!["**/*.sql"]);
        assert_eq!(a.languages, vec!["rust"]);
    }

    #[tokio::test]
    async fn an_unknown_applies_to_value_fails_the_rules_load() {
        for bad in [
            r#"applies_to = { languages = ["cobol"] }"#,
            r#"applies_to = { frameworks = ["next"] }"#,
            r#"applies_to = { paths = ["src/*.{sql,toml}"] }"#,
            r#"applies_to = {}"#,
            r#"applies_to = "sql""#,
        ] {
            let (set, errors) = load_with_extra(bad).await;
            assert!(
                set.get_by_id("SCOPED-TEST-1").is_none(),
                "`{bad}` must not load as a rule that silently matches nothing"
            );
            assert_eq!(errors.len(), 1, "`{bad}` must be reported: {errors:?}");
        }
    }
}
