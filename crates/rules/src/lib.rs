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
///   published standard or real linter rule. `policy` is NOT grounded — it is
///   kept out of the armed ruleset the same as `draft`, but it is labeled
///   honestly as policy rather than being mislabeled `grounded` by virtue of
///   merely having a source citation. Use this instead of `grounded` whenever
///   the only "source" is a Camerata doc citing Camerata (or another purely
///   internal document) rather than a real external authority.
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
    /// standard or real linter rule. Not grounded; kept out of the armed
    /// ruleset like `draft`, but labeled honestly rather than as `grounded`.
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
    /// The decision this rule frames (e.g. "What position does the project take on …?").
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

    /// Whether this rule is shippable — true for every at-least-grounded status,
    /// false for `draft`. Used to keep `draft` (un-grounded, AI-designed) rules
    /// out of the demo'd / armed ruleset.
    pub fn is_shippable(&self) -> bool {
        self.verification.is_shippable()
    }

    /// Whether this rule is AUTO-RECOMMENDED in the onboarding scan PROPOSAL.
    ///
    /// A rule is auto-recommended (pre-checked / pre-selected) iff it is
    /// `Grounded` or `Verified` — i.e. it is backed by an authoritative source
    /// that was machine-verified or human-confirmed. `Draft` and `NeedsRecheck`
    /// rules are still *listed* in the proposal so the architect can review and
    /// opt in, but they are NOT pre-checked because they carry unreduced AI risk.
    ///
    /// The distinction matters for the UI: `is_auto_recommended = true` means the
    /// checkbox is ticked by default; `false` means the row is visible but the
    /// checkbox is unchecked.
    pub fn is_auto_recommended(&self) -> bool {
        matches!(
            self.verification,
            Verification::Grounded | Verification::Verified
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
#[derive(Debug, Default)]
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
    std::env::var("CAMERATA_CORPUS_PATH")
        .ok()
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
        let parent = path
            .parent()
            .unwrap_or(corpus_dir);
        let rel = parent
            .strip_prefix(corpus_dir)
            .unwrap_or(parent);
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
        let normalised = if explicit == "*" { "universal" } else { explicit.as_str() };
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
    })
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
        "ui" => "**/*.{tsx,css}".to_owned(),
        "iac" => "**/*.tf".to_owned(),
        "ci-cd" => "**/.github/**".to_owned(),
        "go" => "**/*.go".to_owned(),
        "python" => "**/*.py".to_owned(),
        // Supabase rules span the CLI project config, SQL migrations (RLS/functions),
        // and the JS/TS client code that talks to it (auth/secrets/storage misuse).
        "supabase" => "**/*.{sql,ts,tsx,toml,env}".to_owned(),
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
        set.push(make_rule("SPIRIT-OPTIMIZE-1", "universal", EnforcementKind::Prose));
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
            assert!(rule.opt_in_only, "{id} must be opt-in only (never pre-checked)");
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
        assert!(rule.has_escalating_option(), "the rule offers an escalating option");

        // Default (no explicit selection) → resolves to the escalate option → spec is active.
        let by_default = rule.selected_escalation(None).expect("default option escalates");
        assert_eq!(by_default.severity, EscalationSeverity::HardPause);
        assert!(by_default.condition.contains("existing test"));

        // Explicitly choosing the escalate option → active.
        assert!(rule.selected_escalation(Some("escalate-on-test-edit")).is_some());

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
                !(rule.is_auto_recommended() && !rule.is_opt_in_only()),
                "Semgrep rule must never be pre-checked"
            );
        }

        // CodeQL: opt_in_only + layer3_only, mechanical, two options, no default.
        if let Some(rule) = set.get_by_id("CICD-CODEQL-SECURITY-SCAN-1") {
            assert_eq!(rule.enforcement, EnforcementKind::Mechanical);
            assert_eq!(rule.domain, "ci-cd");
            assert!(rule.is_opt_in_only(), "CodeQL rule is opt_in_only");
            assert!(rule.is_layer3_only(), "CodeQL is layer-3 only (heavy DB build)");
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
            !(route_placement.is_auto_recommended() && !route_placement.is_opt_in_only()),
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
        let rule = set
            .get_by_id("RUST-DIOXUS-TEST-1")
            .expect("rule loaded");
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

    /// Common shape assertions for a newly-authored rule: it loads, resolves a default option
    /// with a non-empty directive, is grounded (cites a real external authority), carries at
    /// least one `[[sources]]` entry, is code-auditable (so it is guaranteed to reach the
    /// semantic pass even with no shipped detector), and — when its default option carries
    /// remediation — also carries an authored `effort` band.
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
        let default = rule
            .resolved_option(None)
            .unwrap_or_else(|| panic!("{id} must resolve a default option"));
        assert!(
            !default.directive.trim().is_empty(),
            "{id} default option directive"
        );
        if let Some(remediation) = default.remediation.as_deref() {
            if !remediation.trim().is_empty() {
                assert!(
                    default.effort.is_some(),
                    "{id}'s default option carries remediation and must also carry an authored effort band"
                );
            }
        }
        // Every one of these new rules ships at least one rejected/alternative option alongside
        // the adopted default, per RULE_AUTHORING's "options/alternatives" expectation.
        assert!(
            rule.options.len() >= 2,
            "{id} must offer at least one alternative option alongside its default: {} option(s)",
            rule.options.len()
        );
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
}
