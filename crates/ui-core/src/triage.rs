//! Onboarding-triage state machine, extracted from the Dioxus scan adapter (RUST-HEADLESS-CORE-1).
//!
//! This module owns the PURE triage model: the data shapes (`FindingView`, `Disposition`), the
//! stable-identity + state-derivation helpers (`finding_key`, `finding_state`), and the
//! framework-agnostic state machine (`TriageModel`) that the Dioxus layer drives. The adapter
//! keeps the reactive signals, the chorale table effects, the toasts, and the rsx; every pure
//! transition + derivation lives here and is unit-tested with no VirtualDom.
//!
//! The architect moves each finding between five tables (Unresolved / Confirmed / Ignored /
//! Tech debt / False positive) until nothing is Unresolved, then buckets the tech-debt findings
//! (resolve later / now). This is LOCAL triage state — the backend commit (baseline waiver /
//! ticket / dev-engine import) happens at Process, not on each move. False positive and
//! Confirmed are the odd ones out: neither is committed anywhere — see
//! `TriageState::FalsePositive`'s and `TriageState::Confirmed`'s doc comments.

use std::collections::HashMap;

/// Where a finding sits in onboarding triage. The architect moves each finding between these
/// FIVE tables (a single-select switches the view) until nothing is Unresolved; then the
/// ignored and tech-debt buckets are processed. This is LOCAL triage state — the backend
/// commit (baseline waiver / ticket / dev-engine import) happens at Process, not on each move.
///
/// `FalsePositive` is NOT "accepted risk" like `Ignored` — it means the tool was WRONG. It
/// never reaches the baseline and never files a ticket; at Process it is a pure client-local
/// no-op (see the Process handler in `cockpit/scan.rs`), counted once in the report's
/// Methodology section and otherwise excluded entirely. Durably suppressing a false positive
/// via the baseline would be exactly the invisible, reasonless-waiver hole the require-reason
/// baseline invariant prevents — if the same code trips again next scan, that is correct: the
/// reviewer re-judges it fresh.
///
/// ## Why the variant order is the switcher order
///
/// [`TriageState::all`] is the single source of the sub-tab order, so the declaration order
/// here IS what the curator sees: Unresolved, Confirmed, Ignored, Tech debt, False positive.
/// Nothing on the wire depends on it (serde writes unit variants by NAME), so reordering is a
/// presentation change only.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum TriageState {
    #[default]
    Unresolved,
    /// THE AFFIRMATIVE DISPOSITION (design §3/§4, Pass 2 step 3): the curator read this
    /// finding, judged it REAL, and it ships in the report as a live finding.
    ///
    /// ### The defect this closes
    ///
    /// Before it existed, every exit from `Unresolved` was a way of NOT shipping a row:
    /// `Ignored` accepts the risk and baselines it, `TechDebt` defers the fix to a ticket,
    /// `FalsePositive` excludes it from the report entirely. So the only way to keep a finding
    /// LIVE was to leave it undecided — which meant `Unresolved` was doing double duty as both
    /// "not yet looked at" and "confirmed and shipping", and the system could not tell those
    /// apart. The product's first promise is that one person reads every finding and signs;
    /// this is the variant that puts data behind it. It is also the MAJORITY outcome of an
    /// audit, which is why it is the gesture that costs the least (one click, no reason).
    ///
    /// ### What it is not
    ///
    /// It is not a schedule and it is not an acceptance. The export gives a `Confirmed`
    /// finding the same severity-derived action bucket an open one gets and still counts it as
    /// outstanding work — see `camerata_server::report_export::Disposition::Confirmed`.
    ///
    /// ### Scope
    ///
    /// LOCAL, exactly like `FalsePositive` (owner's Q2 ruling, 2026-10-12): it lives in the
    /// auto-saved draft, the report export reads it out of the disposition map, and Process
    /// commits NOTHING for it. A durable per-finding record across engagements is a real
    /// backend surface and Pass 2 does not invent one as a side effect of a UI build.
    Confirmed,
    Ignored,
    TechDebt,
    FalsePositive,
}

/// The names serde writes for the variants (unit variants are written by NAME), for reading a
/// draft leniently ([`crate::lenient`]). A test pins this to what serde actually writes.
impl crate::lenient::WireEnum for TriageState {
    fn from_wire(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|s| s.wire_name() == name)
    }
}

impl TriageState {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Confirmed => "Confirmed",
            Self::Ignored => "Ignored",
            Self::TechDebt => "TechDebt",
            Self::FalsePositive => "FalsePositive",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Confirmed => "Confirmed",
            Self::Ignored => "Ignored",
            Self::TechDebt => "Tech debt",
            Self::FalsePositive => "False positive",
        }
    }

    /// The five triage tables in sub-tab order (Unresolved is the landing table and always
    /// first; Confirmed sits next to it because it is the common exit from it).
    ///
    /// Named once so the switcher, the tally and the tests cannot enumerate a different set —
    /// a sixth disposition added without a tab would be invisible, and a tab whose state the
    /// tally skips would show a permanent zero.
    pub fn all() -> [TriageState; 5] {
        [
            Self::Unresolved,
            Self::Confirmed,
            Self::Ignored,
            Self::TechDebt,
            Self::FalsePositive,
        ]
    }

    /// Whether the curator has DECIDED this row.
    ///
    /// This is the Process gate's question, and the reason `Confirmed` had to exist: the gate
    /// blocks on rows nobody has decided, and a confirmed finding is decided — it is simply
    /// decided to be real. Every state except `Unresolved` is a decision, so the gate's
    /// blocking count is exactly the `Unresolved` tally (see [`TriageCounts::outstanding`]).
    pub fn is_decided(self) -> bool {
        !matches!(self, Self::Unresolved)
    }

    /// Whether moving a row HERE requires a recorded reason.
    ///
    /// `Ignored` and `FalsePositive` do, and for the same underlying cause: each makes a
    /// claim BEYOND the finding — that the risk is accepted, or that the tool was wrong — and
    /// the report has to be able to say why. `Confirmed` and `TechDebt` do not: confirming
    /// agrees with a finding that already states its own case, and tech debt schedules it
    /// rather than disputing it. See [`TriageModel::confirm`]'s doc for the long form.
    ///
    /// Named once, here, because three call sites now enforce it — the per-row buttons, the
    /// bulk bar, and (since the detail pane) the keyboard. The keyboard is the one where
    /// dropping it would be cheapest and least visible, which is exactly why the rule is the
    /// enum's own answer rather than a condition repeated at each gesture.
    pub fn requires_reason(self) -> bool {
        matches!(self, Self::Ignored | Self::FalsePositive)
    }
}

/// Which tech-debt bucket a finding is in: resolve LATER (file a tracked ticket) or NOW (pull
/// into the dev engine as the first story). Only meaningful when state == TechDebt.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum TechDebtBucket {
    Later,
    Now,
}

/// One finding's triage disposition: its table, the (required) ignore reason, and its
/// tech-debt bucket. Absence from the dispositions map == Unresolved with defaults.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Disposition {
    pub state: TriageState,
    pub reason: String,
    pub bucket: TechDebtBucket,
}

impl Default for Disposition {
    fn default() -> Self {
        Self {
            state: TriageState::Unresolved,
            reason: String::new(),
            bucket: TechDebtBucket::Later,
        }
    }
}

/// One audit finding, as it deserializes off the BFF scan/audit response. Wide, plain serde
/// DTO — no transport concern, no rendering concern. Shared by the CSV export, the findings
/// table, and the triage model.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FindingView {
    #[serde(default)]
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    pub snippet: String,
    pub detail: String,
    /// `active` (enforced), `suppressed-inline`, or `suppressed-baseline`.
    #[serde(default = "default_finding_status")]
    pub status: String,
    /// Other rule ids this same location also violates (the server merged them into this
    /// row). Empty for an un-merged finding. Surfaced as a "+N" on the rule and listed in
    /// the detail modal.
    #[serde(default)]
    pub also_matches: Vec<String>,
    /// PREVIEW (CI-security Part B): the server's scan-time deterministic preview pass ran
    /// the rule's underlying tool ITSELF and produced this finding, even though the rule is
    /// NOT yet wired into the repo's gate. Deterministic (stable tool rule-id) but ADVISORY:
    /// "preview — not enforced until wired". Defaults to `false` (back-compatible).
    #[serde(default)]
    pub preview: bool,
    /// For a preview finding, the tool that produced it (`clippy` | `ruff` | `eslint` |
    /// `semgrep`). `None` for non-preview findings. Shown in the Authority badge label.
    #[serde(default)]
    pub preview_tool: Option<String>,
    /// True when this finding is in test/fixture scope.
    #[serde(default)]
    pub in_test: bool,
    /// True when this finding needs manual verification.
    #[serde(default)]
    pub needs_review: bool,
    /// Structured calibration confidence: `"high"` | `"needs-review"`. `None` for findings
    /// calibration never saw (the deterministic floor, preview findings). Mirrors
    /// `camerata_server::onboard::Finding::confidence` on the wire — kept in sync so a field
    /// added server-side is never silently dropped by serde here (see the round-trip
    /// contract tests at the bottom of this module).
    #[serde(default)]
    pub confidence: Option<String>,
    /// Structured remediation-effort estimate: `"low"` | `"medium"` | `"high"`. `None` for
    /// findings calibration never saw. Mirrors `camerata_server::onboard::Finding::effort`.
    #[serde(default)]
    pub effort: Option<String>,
    /// The alternative option this finding was judged AGAINST (audit-integrated alternative
    /// recommendation, docs/design/2026-09-22_audit-integrated-alternatives.md). `None` for a
    /// single-option rule, or for a finding produced before this field existed. Mirrors
    /// `camerata_server::onboard::Finding::evaluated_option_id`. Used by the scan UI to label
    /// each multi-option rule group "Evaluated against: <option label>".
    #[serde(default)]
    pub evaluated_option_id: Option<String>,
    /// WHAT THIS ROW IS — the five-kind taxonomy (A5.6), mirrored verbatim from
    /// `camerata_server::onboard::Finding::kind` with the same `#[serde(default, flatten)]`
    /// shape, so the wire's single flat `"kind"` discriminator plus the variant's payload
    /// fields (`deviating_sites`, `convention_label`, `reason`, …) land here as siblings of
    /// this struct's own fields. The UI READS this; it never derives it (the taxonomy's hard
    /// constraint: structured identity, never prose).
    ///
    /// `None` is the `"unspecified"` ALARM state, not a synonym for `Defect`: it means the
    /// producer never assigned a kind, i.e. an unmapped emission path. See
    /// [`kind_surface`] (such a row stays in the Queue, visibly flagged) and
    /// [`is_unspecified_kind`].
    /// Evidence sites ABSORBED into this row when the server merged several findings at
    /// (or around) one location — mirrors `camerata_server::onboard::Finding::also_locations`
    /// verbatim, as [`MergedSiteView`].
    ///
    /// This is C5's field. Pass-1 W7 added it because an absorbed site was reaching NO
    /// artifact: a three-site defect exported as one row naming one file, and the other two
    /// places the bug lives simply vanished. The UI carrying the wire's `kind` but dropping
    /// `also_locations` would re-open that hole on the surface the curator actually reads,
    /// which is the worst place to have it — so it is mirrored with the same round-trip
    /// contract test the other mirrored fields have.
    ///
    /// Empty for an un-merged finding (back-compatible serde default). Note that this is NOT
    /// [`FindingView::also_matches`]: that is the set of other RULE IDS violated at this same
    /// location, while this is the set of other LOCATIONS this one defect touches.
    #[serde(default)]
    pub also_locations: Vec<MergedSiteView>,
    /// The detector's placeholder bindings for this finding (`table` -> `profiles`,
    /// `function-name` -> `send-invite`), mirrored from `camerata_server::onboard::Finding::captures`.
    /// The code viewer's `S` key pre-fills its symbol search from here. Empty for most
    /// AI-judged findings.
    #[serde(default)]
    pub captures: std::collections::BTreeMap<String, String>,
    #[serde(default, flatten)]
    pub kind: Option<camerata_api_types::audit::FindingKind>,
}

/// One evidence site absorbed into a merged finding — the view-model mirror of
/// `camerata_server::onboard::MergedLocation`, field for field.
///
/// Re-declared here rather than imported because `camerata-ui-core` must not depend on
/// `camerata-server` (the crate is the BFF; the UI is its HTTP client). The shape is pinned
/// against the wire by the round-trip contract tests, which is the same seam
/// `Disposition`/`TriageState` already live on.
#[derive(Clone, PartialEq, Eq, Debug, serde::Deserialize, serde::Serialize)]
pub struct MergedSiteView {
    /// `owner/repo` this site lives in — may differ from the primary's in a multi-repo scan.
    #[serde(default)]
    pub repo: String,
    /// File path within `repo`.
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// The rule id this site was originally flagged under, before it was absorbed.
    pub rule_id: String,
    /// The offending line this site cited.
    #[serde(default)]
    pub snippet: String,
    /// True when this site is a pure CONSEQUENCE of the primary's root cause rather than an
    /// additional place that needs the same fix. The detail pane subordinates these under an
    /// "also affects" sub-label, which is exactly what the server field's doc says it means.
    #[serde(default)]
    pub consequence: bool,
}

/// How many places this one finding touches: its own site, plus every absorbed one.
///
/// Computed rather than read off a wire field, per design §10: "the UI can compute
/// `1 + also_locations.len()` identically, which is the better choice — one source of truth,
/// no new wire field." Never zero: a finding always has its own site.
pub fn site_count(f: &FindingView) -> usize {
    1 + f.also_locations.len()
}

/// Which of the curator workbench's four kind surfaces a finding row belongs on
/// (`docs/plans/2026-10-11_pass2-curator-ui-design.md` §1-§2).
///
/// The split is NOT a UI taste: it is the data model's own
/// [`FindingKind::is_informational`](camerata_api_types::audit::FindingKind::is_informational)
/// boundary. The two ACCUSING kinds share one decision shape (is this real, do we act) and so
/// share one burn-down Queue; each informational kind asserts nothing is wrong, has no site to
/// dispose and no severity worth showing, and gets its own list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum KindSurface {
    /// `Defect` + `Deviation` + every kind-less (`unspecified`) row — the triage Queue.
    Queue,
    /// `ConventionObserved` — the codebase already follows a convention, zero violations.
    Conventions,
    /// `NoConvention` — the codebase is inconsistent with no clear majority (one row per rule).
    NoConvention,
    /// `NotAssessed` — the rule was armed but could not be evaluated. An honesty row.
    NotAssessed,
}

impl KindSurface {
    /// The switcher's tab label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Conventions => "Conventions",
            Self::NoConvention => "No convention",
            Self::NotAssessed => "Not assessed",
        }
    }

    /// The four surfaces in switcher order (Queue is primary and always first).
    pub fn all() -> [KindSurface; 4] {
        [
            Self::Queue,
            Self::Conventions,
            Self::NoConvention,
            Self::NotAssessed,
        ]
    }
}

/// The surface this row belongs on. Total and structural — it reads the row's own
/// [`FindingKind`](camerata_api_types::audit::FindingKind) and nothing else.
///
/// A kind-less row routes to the Queue DELIBERATELY. It is an alarm (an unmapped emission
/// path), and the Queue is the surface the curator actually looks at; hiding it in an
/// informational list would be exactly the silent bucket the taxonomy's backfill rule
/// forbids. It is flagged there, never silently counted as a `Defect` — see
/// [`is_unspecified_kind`] and [`unspecified_kind_count`].
pub fn kind_surface(f: &FindingView) -> KindSurface {
    use camerata_api_types::audit::FindingKind;
    match &f.kind {
        Some(FindingKind::ConventionObserved(_)) => KindSurface::Conventions,
        Some(FindingKind::NoConvention(_)) => KindSurface::NoConvention,
        Some(FindingKind::NotAssessed(_)) => KindSurface::NotAssessed,
        // Defect, Deviation, and the kind-less alarm row all accuse (or need looking at) and
        // all share the dispose-this-row decision shape.
        Some(FindingKind::Defect) | Some(FindingKind::Deviation(_)) | None => KindSurface::Queue,
    }
}

/// The row's kind as its stable wire tag — the kind's own serde discriminator, or
/// `"unspecified"` when the producer never assigned one. Delegates to the api-types helper so
/// the badge text can never drift from the wire.
pub fn kind_tag(f: &FindingView) -> &'static str {
    camerata_api_types::audit::FindingKind::tag_or_unspecified(f.kind.as_ref())
}

/// True when this row reached the UI with NO kind — an unmapped emission path. The curator
/// surface must make this impossible to miss, never fold it into `Defect`.
pub fn is_unspecified_kind(f: &FindingView) -> bool {
    f.kind.is_none()
}

/// How many rows reached the UI with no kind. Drives the Queue tab's red alarm chip.
pub fn unspecified_kind_count(findings: &[FindingView]) -> usize {
    findings.iter().filter(|f| is_unspecified_kind(f)).count()
}

/// Every row routed to `surface`, in input order (the caller sorts).
pub fn findings_on_surface(findings: &[FindingView], surface: KindSurface) -> Vec<FindingView> {
    findings
        .iter()
        .filter(|f| kind_surface(f) == surface)
        .cloned()
        .collect()
}

/// Per-surface row counts for the kind switcher, in [`KindSurface::all`] order.
pub fn surface_counts(findings: &[FindingView]) -> [(KindSurface, usize); 4] {
    let mut out = KindSurface::all().map(|s| (s, 0usize));
    for f in findings {
        let s = kind_surface(f);
        for slot in out.iter_mut() {
            if slot.0 == s {
                slot.1 += 1;
            }
        }
    }
    out
}

/// THE ONE-RULE-ONE-SURFACE INVARIANT (design §8).
///
/// The pipeline guarantees a rule emits deviations OR a convention row OR one no-convention
/// row OR a not-assessed row — never two of those in the same scan. The UI's job is to ASSERT
/// that, not to defend against it: a rule on two surfaces is a pipeline bug, and the honest
/// response is to surface it loudly rather than to pick a winner and hide the conflict.
///
/// Returns `(rule_id, surfaces)` for every offending rule, sorted, so a caller can render the
/// violation verbatim. Empty is the healthy case.
///
/// Scoped per `(repo, rule_id)`, NOT per rule id alone. In a multi-repo scan one repo can
/// honestly follow a convention while another deviates from it — that is two different
/// assessments of two different codebases, not a conflict. Keying on the rule id alone would
/// make a legitimate multi-repo scan ring the alarm permanently, which would train the curator
/// to ignore it.
pub fn rules_on_multiple_surfaces(
    findings: &[FindingView],
) -> Vec<((String, String), Vec<KindSurface>)> {
    let mut seen: std::collections::BTreeMap<
        (String, String),
        std::collections::BTreeSet<KindSurface>,
    > = std::collections::BTreeMap::new();
    for f in findings {
        seen.entry((f.repo.clone(), f.rule_id.clone()))
            .or_default()
            .insert(kind_surface(f));
    }
    seen.into_iter()
        .filter(|(_, surfaces)| surfaces.len() > 1)
        .map(|(k, surfaces)| (k, surfaces.into_iter().collect()))
        .collect()
}

/// THE RULE HUB (design §8): everything this scan knows about ONE rule, in one place.
///
/// > Rule -> everything about that rule. This is THE join, and it gets the investment.
///
/// The design is explicit that the alternative — point-to-point cross-links between every pair
/// of surfaces — is n-squared noise. So this is the only join built, and it is built once: a
/// pure derivation over the scan's own rows, with no second source and no second ranking.
///
/// ## What it refuses to say
///
/// Two refusals are load-bearing, and both are the design's own words:
///
/// * **Conforming sites are not in the data, only counts.** [`ConformingSites`] is two
///   integers; there is no list, no link, and no field that could grow into one.
/// * **A rule appears in exactly one kind surface per scan.** When it does not, that is a
///   pipeline bug to surface rather than to resolve, so [`RuleHubView::conflict_surfaces`]
///   carries [`rules_on_multiple_surfaces`]'s own verdict verbatim instead of this function
///   re-deciding which surface "really" owns the rule.
#[derive(Clone, PartialEq, Debug)]
pub struct RuleHubView {
    /// `owner/repo` this hub is scoped to.
    pub repo: String,
    pub rule_id: String,
    /// The ONE kind surface this rule's rows reached — and, when the invariant is violated,
    /// the surface of the rule's FIRST row rather than a winner picked by this function.
    pub surface: KindSurface,
    /// The wire tag of the first row's kind, from [`kind_tag`] — never re-derived.
    pub kind_tag: &'static str,
    /// The kind outcome as one sentence. For a deviation this is
    /// [`DeviationHeadline::sentence`] verbatim, the same words the group header carries.
    pub outcome: String,
    /// How many ROWS this rule has in this scan, within this repo.
    pub row_count: usize,
    /// How many SITES those rows touch — [`site_count`] summed, so an absorbed merge site is
    /// counted once as a site and never as an extra row (design §6's conservation rule).
    pub site_count: usize,
    /// The observed convention's authored label, for the two kinds that carry one.
    pub convention_label: Option<String>,
    /// The alternative option the audit evaluated this rule against, when it is multi-option.
    /// Relocated here from the standalone `RuleAlternativesPanel` (design §9.1).
    pub evaluated_option_id: Option<String>,
    /// Counts only. See [`ConformingSites`].
    pub conforming: Option<ConformingSites>,
    /// Empty in the healthy case. See [`rules_on_multiple_surfaces`].
    pub conflict_surfaces: Vec<KindSurface>,
    /// Where "take me to this rule" goes: the Queue (filtered to the rule) or the panel that
    /// holds it. Exactly one destination, because the rule has exactly one surface.
    pub jump: KindSurface,
}

/// Build the hub for one `(repo, rule_id)` out of the scan's rows.
///
/// Scoped per `(repo, rule_id)` — the SAME key [`rules_on_multiple_surfaces`] uses — because
/// in a multi-repo scan one repo can honestly follow a convention while another deviates from
/// it, and merging those two assessments into one "this rule in this scan" strip would state a
/// contradiction as a fact.
///
/// `None` when the rule has no rows in that repo: the hub describes what a scan FOUND, and a
/// rule it never touched has nothing to describe. (That is also why there is no "0 findings"
/// hub — an empty strip is a dead affordance.)
pub fn rule_hub(findings: &[FindingView], repo: &str, rule_id: &str) -> Option<RuleHubView> {
    use camerata_api_types::audit::FindingKind;
    let rows: Vec<&FindingView> = findings
        .iter()
        .filter(|f| f.repo == repo && f.rule_id == rule_id)
        .collect();
    let first = *rows.first()?;
    let surface = kind_surface(first);
    let conflict_surfaces = rules_on_multiple_surfaces(findings)
        .into_iter()
        .find(|((r, rid), _)| r == repo && rid == rule_id)
        .map(|(_, surfaces)| surfaces)
        .unwrap_or_default();
    let headline = deviation_headline(first);
    let convention_label = match &first.kind {
        Some(FindingKind::Deviation(d)) => Some(d.convention_label.clone()),
        Some(FindingKind::ConventionObserved(c)) => Some(c.convention_label.clone()),
        _ => None,
    }
    .filter(|l| !l.is_empty());
    let row_count = rows.len();
    let site_count: usize = rows.iter().copied().map(site_count).sum();
    let outcome = match &first.kind {
        // The deviation sentence, verbatim. Not a second phrasing of the same numbers: the
        // curator reads this strip right after reading the group header, and two wordings of
        // one claim is how a reader starts wondering which one is the real number.
        Some(FindingKind::Deviation(_)) => headline
            .as_ref()
            .map(DeviationHeadline::sentence)
            .unwrap_or_default(),
        Some(FindingKind::Defect) => {
            if site_count == row_count {
                format!("{row_count} defect row(s)")
            } else {
                // Named separately because the difference is C5's whole point: a three-site
                // defect that exports as one row is exactly the shape that lost two sites.
                format!("{row_count} defect row(s) across {site_count} site(s)")
            }
        }
        Some(FindingKind::ConventionObserved(c)) => format!(
            "Convention observed with zero violations: \"{}\"",
            c.convention_label
        ),
        Some(FindingKind::NoConvention(n)) => format!(
            "No convention \u{2014} the codebase is split across {} observed option(s)",
            n.observed_options.len()
        ),
        Some(FindingKind::NotAssessed(na)) => format!(
            "Not assessed \u{2014} {}{}",
            na.reason.as_str(),
            if na.reason_detail.is_empty() {
                String::new()
            } else {
                format!(": {}", na.reason_detail)
            }
        ),
        // The ALARM. Deliberately does not use the word "defect": an unmapped emission path is
        // a pipeline bug, and describing it as the kind it most resembles is precisely the
        // silent backfill the taxonomy's own rule forbids.
        None => format!(
            "UNSPECIFIED \u{2014} {row_count} row(s) reached the UI with no kind. An unmapped \
             emission path upstream, not a finding class."
        ),
    };
    Some(RuleHubView {
        repo: repo.to_string(),
        rule_id: rule_id.to_string(),
        surface,
        kind_tag: kind_tag(first),
        outcome,
        row_count,
        site_count,
        convention_label,
        evaluated_option_id: rows
            .iter()
            .find_map(|f| f.evaluated_option_id.clone())
            .filter(|o| !o.is_empty()),
        conforming: headline.as_ref().and_then(DeviationHeadline::conforming),
        conflict_surfaces,
        jump: surface,
    })
}

/// THE DEVIATION SENTENCE, as structured values (design §2/§4, Pass 2 step 2).
///
/// A `Deviation` row carries its whole claim as fields — numerator, denominator, convention
/// label — specifically so a renderer never has to recover any of it from English. This struct
/// is those three values lifted off one row, plus [`DeviationHeadline::sentence`], the single
/// place the words are chosen.
///
/// Why one row suffices: `DeviationKind`'s own doc says the counts are GROUP-level (per rule,
/// across the whole scan) and are "repeated verbatim on every deviating row for that rule", so
/// no join and no aggregation is needed to state the group's claim.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviationHeadline {
    /// NUMERATOR: sites that deviate, verbatim from `DeviationKind::deviating_sites`.
    pub deviating_sites: usize,
    /// DENOMINATOR: sites the rule's decision applies to, verbatim from
    /// `DeviationKind::assessed_sites`. `None` = the producer could not count honestly.
    pub assessed_sites: Option<usize>,
    /// The convention's human label, verbatim from `DeviationKind::convention_label` (authored
    /// corpus text). May be empty when the producer established a majority outside the
    /// authored option set.
    pub convention_label: String,
}

impl DeviationHeadline {
    /// Whether a denominator may be PRINTED.
    ///
    /// `None` is the field's own "could not count honestly" signal. `Some(0)` is a producer
    /// contradiction (a rule whose decision applies to nothing cannot have deviating sites),
    /// and "2 of 0" is not a sentence worth putting in front of a curator — so both render the
    /// numerator alone. This is deliberately the SAME rule [`deviation_ratio_ppm`] already
    /// applies when it declines to place such a row in the cheapness ordering; two different
    /// answers to "is this denominator usable" would be a drift waiting to happen.
    fn printable_denominator(&self) -> Option<usize> {
        self.assessed_sites.filter(|m| *m > 0)
    }

    /// The canonical sentence, assembled from the fields above and NOTHING else.
    ///
    /// Three shapes, one per honesty case:
    ///
    /// * denominator known — `2 of 47 sites deviate from "URI-versioned endpoints"`
    /// * denominator absent — `3 sites deviate from "URI-versioned endpoints"`, the numerator
    ///   alone. Never a fabricated total, never a printed zero: the field's doc is explicit
    ///   that `None` means the producer could not count, and inventing 47 or printing 0 would
    ///   both be the UI asserting something no producer said. The workbook's "not counted"
    ///   renders the same refusal.
    /// * convention unnamed — `… deviate from the observed convention`. `convention_label` is
    ///   authored corpus text; when the producer established a majority outside the authored
    ///   option set it arrives empty, and `deviate from ""` would read as a bug. Naming the
    ///   thing generically states exactly what is known (a convention was observed) without
    ///   inventing a name for it.
    ///
    /// The word is **sites**, not "endpoints": the design's worked example happened to be API
    /// endpoints, but the fields count sites and the module doc in `audit.rs` states the
    /// contract as "N of M sites deviate from <convention>". Printing a domain noun the data
    /// does not carry would be the smallest possible version of the prose-invention this whole
    /// taxonomy exists to end.
    pub fn sentence(&self) -> String {
        let n = self.deviating_sites;
        let subject = match self.printable_denominator() {
            // "1 of 47 sites deviates" — the verb agrees with the one, not with the forty-seven.
            Some(m) if n == 1 => format!("1 of {m} sites deviates"),
            Some(m) => format!("{n} of {m} sites deviate"),
            None if n == 1 => "1 site deviates".to_string(),
            None => format!("{n} sites deviate"),
        };
        if self.convention_label.is_empty() {
            format!("{subject} from the observed convention")
        } else {
            format!("{subject} from \"{}\"", self.convention_label)
        }
    }

    /// How many sites CONFORM, when the denominator is printable.
    ///
    /// This is a subtraction over two authored numbers and nothing more. It exists so the rule
    /// hub can state "45 of 47 conform" without re-deriving the denominator rule: the guard is
    /// [`DeviationHeadline::printable_denominator`], the same one [`DeviationHeadline::sentence`]
    /// uses, so the hub and the group header can never disagree about whether a denominator is
    /// usable.
    ///
    /// `None` when the producer could not count (design §4's "never a fabricated denominator")
    /// and also when the numerator EXCEEDS the denominator — that is a producer contradiction,
    /// and an underflowed "conforming" count would be a fabricated number wearing arithmetic's
    /// authority.
    ///
    /// What this deliberately does NOT do is make the conforming sites reachable. Design §8:
    /// "conforming sites are not in the data (only counts), so there is no 'show me the 45
    /// conforming endpoints' link to offer. Do not fabricate one." This returns two integers.
    pub fn conforming(&self) -> Option<ConformingSites> {
        let assessed = self.printable_denominator()?;
        let conforming = assessed.checked_sub(self.deviating_sites)?;
        Some(ConformingSites {
            conforming,
            assessed,
        })
    }
}

/// How many sites of a deviation's assessed total conform, as COUNTS ONLY.
///
/// A deliberately inert pair of integers. The conforming sites themselves are not in the scan
/// data — the pipeline reports the deviating sites and a total, never the conforming set — so
/// this type carries no identifiers, no locations and no handle of any kind. Design §8 names
/// fabricating one as the thing not to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConformingSites {
    /// `assessed - deviating`.
    pub conforming: usize,
    /// The producer's own denominator.
    pub assessed: usize,
}

/// This row's deviation headline, or `None` for every other kind.
///
/// Reads [`FindingKind::Deviation`](camerata_api_types::audit::FindingKind::Deviation)'s
/// payload fields directly. There is no prose path here and there must never be one.
pub fn deviation_headline(f: &FindingView) -> Option<DeviationHeadline> {
    use camerata_api_types::audit::FindingKind;
    match &f.kind {
        Some(FindingKind::Deviation(d)) => Some(DeviationHeadline {
            deviating_sites: d.deviating_sites,
            assessed_sites: d.assessed_sites,
            convention_label: d.convention_label.clone(),
        }),
        _ => None,
    }
}

/// Separator between the rule id and its sentence on a group header. A middle dot, matching
/// the design's own sketch — and deliberately not a dash of any width.
pub const RULE_GROUP_SEPARATOR: &str = " \u{b7} ";

/// THE GROUP-HEADER LINE for a row: the rule id, plus the deviation sentence when the row is a
/// deviation.
///
/// This is the value the Queue table GROUPS BY, which is what puts the sentence on the header
/// without a parallel rendering mechanism: chorale builds each group header's label from the
/// group-by column's cell value, so making the cell value the full header line makes chorale
/// render it — surviving collapse, manual re-sorts and group reordering for free, because they
/// are all chorale's own machinery operating on its own view.
///
/// Grouping is unaffected. The sentence's values are group-level and identical on every row of
/// a rule, so `rule_id` and `rule_id + sentence` partition the rows identically. If two rows of
/// one rule ever DID disagree, they would split into two visibly-contradictory headers rather
/// than one of the two numbers silently winning — which is the honest failure for a pipeline
/// invariant the UI is told to assert rather than defend against (design §8).
///
/// A defect (or kind-less) row yields the bare rule id, so defect headers stay plain — design
/// §4, and the data's own reason: a defect group is N independent accusations that merely share
/// a rule id, with no group-level claim to state.
pub fn rule_group_label(f: &FindingView) -> String {
    match deviation_headline(f) {
        Some(h) => format!("{}{RULE_GROUP_SEPARATOR}{}", f.rule_id, h.sentence()),
        None => f.rule_id.clone(),
    }
}

/// One deviation rule group in the current view: its header line, and the rows a rule-level
/// bulk action would move.
///
/// `rows` is the group as it exists IN THIS VIEW (one triage table, after filtering), because
/// that is exactly the set a bulk button can honestly claim to act on.
#[derive(Clone, PartialEq)]
pub struct DeviationGroup {
    /// The group-by value — identical to [`rule_group_label`] of every row in `rows`, and
    /// therefore identical to the label chorale renders on the group header.
    pub label: String,
    /// The rule id, for the action's own label.
    pub rule_id: String,
    /// The structured claim (see [`DeviationHeadline`]).
    pub headline: DeviationHeadline,
    /// Every row of this group in this view, in view order.
    pub rows: Vec<FindingView>,
}

impl DeviationGroup {
    /// How many rows a bulk action on this group would dispose.
    ///
    /// NOT the same number as `headline.deviating_sites`, and the difference is real rather
    /// than a bug: the headline counts SITES across the whole scan (including sites absorbed
    /// into a merged row, and rows the curator has already moved to another triage table),
    /// while this counts the rows in front of him right now. The button is labelled with THIS
    /// number because it is what the click will actually touch.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// The deviation rule groups in `findings`, in first-occurrence order.
///
/// Keyed on [`rule_group_label`] — the same key chorale partitions on — so a group here is
/// precisely a group there, with the same membership and the same count. Keying on the rule id
/// instead would silently merge two headers chorale shows separately.
///
/// `findings` is expected to be the already-filtered, already-ordered view (what
/// [`TriageModel::visible`] returns), so the groups come out in the order the table shows them.
pub fn deviation_groups(findings: &[FindingView]) -> Vec<DeviationGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut by_label: HashMap<String, DeviationGroup> = HashMap::new();
    for f in findings {
        let Some(headline) = deviation_headline(f) else {
            continue;
        };
        let label = rule_group_label(f);
        let entry = by_label.entry(label.clone()).or_insert_with(|| {
            order.push(label.clone());
            DeviationGroup {
                label: label.clone(),
                rule_id: f.rule_id.clone(),
                headline,
                rows: Vec::new(),
            }
        });
        entry.rows.push(f.clone());
    }
    order
        .into_iter()
        .filter_map(|l| by_label.remove(&l))
        .collect()
}

/// Ordering rank of the severity string as the pipeline CALIBRATED it (critical > high >
/// medium > everything else).
///
/// This orders an authored string; it does not re-derive, re-weight or second-guess a
/// severity. Comparability between kinds is manufactured UPSTREAM by the rules' `max_severity`
/// ceilings, `min_severity` floors and `deviation_ceiling` — a UI that recomputed severity
/// would be a second calibration layer, which is precisely the mid-layer accretion this effort
/// exists to remove.
///
/// Public because the MANUAL column sort needs the identical ordering of the identical
/// authored string (see [`severity_sort_value`]): two rank tables for one ladder would drift,
/// and the drift would show up as the curator's own re-cut disagreeing with the default.
pub fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// The four calibrated severities in ladder order — the authored vocabulary this surface
/// orders, named once so [`severity_sort_values`] and its tests cannot drift from
/// [`severity_rank`].
pub const CALIBRATED_SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];

/// The Severity column's CELL VALUE: the authored severity prefixed by its calibrated ladder
/// position, so that a plain LEXICOGRAPHIC sort of the cell is the CALIBRATED sort.
///
/// ## Why the sort order has to live in the cell value
///
/// chorale orders a column by exactly the `CellValue` the column's accessor returns
/// (`chorale_core::views::filtered_sorted_pairs` compares `(col.accessor)(a)` against
/// `(col.accessor)(b)`), and `chorale-core` 0.2.3 exposes no separate sort-key hook on
/// `ColumnDef` — `sortable` is a bool, not a comparator. So for a header click to produce a
/// correct order, the cell value IS the sort key; there is nowhere else to put it.
///
/// Severity is the one Queue column whose words do not sort into their own meaning:
/// alphabetically `low` lands above `medium`, so a bare lexicographic header cut reads
/// "critical, high, low, medium" — a visibly wrong ladder in BOTH directions. Prefixing the
/// authored word with its rank makes chorale's own comparator right without standing up a
/// second, parallel sort mechanism alongside it.
///
/// ## Why this is not a second calibration layer
///
/// Nothing is re-derived, re-weighted or recomputed. The prefix is [`severity_rank`] — the
/// same ordering of the same authored string that [`queue_sort_key`] already applies — and
/// the authored word travels through verbatim. Severity's calibration still happens entirely
/// upstream (`max_severity` ceilings, `min_severity` floors, `deviation_ceiling`); a Deviation
/// and a Defect that authored the same severity get byte-identical sort values here, because
/// the kind is not an input.
///
/// ## Reading the value
///
/// Ranks are 1..=4 — single digits, so lexicographic ordering of the prefix is numeric
/// ordering, with no zero-padding needed. The authored word stays in the string so every
/// consumer that wants the severity itself can still read it, and an off-vocabulary severity
/// passes through rather than being dropped or renamed: it shares rank 4 with `low` exactly as
/// it does in the default key (`severity_rank`'s catch-all), then sub-orders alphabetically.
///
/// Display is NOT this string: the column maps each value back to its authored label through
/// chorale's badge map, and the row highlight, the CSV export and the report all read
/// `FindingView::severity` directly.
pub fn severity_sort_value(severity: &str) -> String {
    format!("{} {}", severity_rank(severity) + 1, severity)
}

/// The four calibrated severities as Severity-column cell values, in ladder order — the
/// column's multi-select filter options.
///
/// Built from [`severity_sort_value`], the same function the accessor uses, so the filter
/// vocabulary can never drift from the cell it filters. (A hand-written option list that fell
/// out of sync would silently filter every row out, which is a worse failure than a wrong
/// sort: it looks like "no findings".)
pub fn severity_sort_values() -> Vec<String> {
    CALIBRATED_SEVERITIES
        .iter()
        .map(|s| severity_sort_value(s))
        .collect()
}

/// THE CONFIDENCE COLUMN's cell value — STRUCTURED FIELDS ONLY (design §9.3).
///
/// > The "Needs review" 300px column with inline prose … Superseded by the structured
/// > `confidence` chip … a UI parsing model prose violates the same principle the backend just
/// > spent a whole pass eliminating ("structured identity, never prose").
///
/// What the column it replaces read was `in_test || needs_review || split_needs_review(detail)
/// .1.is_some()` — two structured flags OR'd with a regex over the model's own English. This
/// reads the two structured flags and the structured `confidence` field, and nothing else.
///
/// ## Why dropping the prose term loses nothing a current scan carries
///
/// `camerata_server::ai_audit::apply_verdicts` writes the `[needs review: …]` detail tag and
/// sets `needs_review` in the same pass: the tag reads `"needs review"` exactly when
/// `low_conf` is true, and `f.needs_review = low_conf` is set UNCONDITIONALLY two lines above
/// it. The two are a biconditional by construction (the server's own comment says so), so for
/// every row the pipeline produces today, `detail` carrying the marker implies `needs_review`.
/// The regex was reading a shadow of the field beside it.
///
/// A LEGACY row — one serialized before the structured fields existed, carrying the prose and
/// nothing else — now reads `unrated` here rather than `needs-review`. That is deliberate and
/// it is not a loss of disclosure: the hedge still reaches the curator, in the detail pane,
/// which is exactly where §9.3 puts it ("the reason text lives in the detail pane"). What it
/// stops doing is minting a structured-looking chip out of a sentence.
///
/// ## The values
///
/// * `test` — the finding is in test/fixture scope. Kept from the old column: `in_test` is a
///   structured flag, it answers a different question from confidence, and it is the one the
///   curator triages first.
/// * `needs-review` — calibration hedged this finding.
/// * `high` — calibration looked and did not hedge.
/// * `unrated` — calibration never saw it (the deterministic floor, preview findings) or the
///   row predates the field. NOT a synonym for `high`: "we did not judge this" and "we judged
///   it confidently" are different statements and the chip must not merge them.
pub fn confidence_tag(f: &FindingView) -> &'static str {
    if f.in_test {
        return "test";
    }
    // The structured flag and the structured field are a biconditional upstream; either one
    // alone is enough here, and taking both means a desync between them resolves toward
    // DISCLOSING the hedge rather than hiding it.
    if f.needs_review || f.confidence.as_deref() == Some("needs-review") {
        return "needs-review";
    }
    match f.confidence.as_deref() {
        Some("high") => "high",
        _ => "unrated",
    }
}

/// The Confidence column's multi-select filter vocabulary, in the order the chip ladder reads.
///
/// Built from the same closed set [`confidence_tag`] returns, so the filter cannot drift from
/// the cell it filters — the identical discipline [`severity_sort_values`] exists for, and for
/// the identical reason: a stale option list silently filters every row out and reads as "no
/// findings".
pub fn confidence_tags() -> Vec<String> {
    ["test", "needs-review", "high", "unrated"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// The heading an on-demand reason prompt shows (design §3, §9.2).
///
/// > Reason capture moves out of the toolbar. The two permanently visible textareas (ignore
/// > reason, FP reason) become an on-demand inline prompt that appears when the action is
/// > invoked … Same require-reason invariant, enforced at the same point.
///
/// It NAMES THE COUNT, which the two textareas it replaces never did. A textarea sitting
/// beside a selection says nothing about how many rows the button next to it will move; a
/// prompt raised BY the gesture knows, and a bulk disposition is exactly where "I thought that
/// was three rows" is expensive.
pub fn bulk_reason_heading(state: TriageState, count: usize) -> String {
    let noun = if count == 1 { "finding" } else { "findings" };
    format!(
        "Reason for \u{201c}{}\u{201d} \u{2014} {count} {noun} (required)",
        state.label()
    )
}

/// Ordering rank of the row's KIND within one severity band (design §5 step 3).
///
/// `Defect` before `Deviation`: an accusation of objective fault outranks an accusation of
/// inconsistency at equal severity. This mirrors the pipeline's own `kind_actionability_rank`
/// in the merge layer rather than inventing a second ranking.
///
/// A kind-less (`unspecified`) row ranks FIRST, ahead of `Defect`. It is an alarm about the
/// pipeline itself, there should be zero of them, and burying one under hundreds of real rows
/// is how an unmapped emission path survives a release. Ranking it first is also why it is not
/// "treated as a Defect": it does not share Defect's rank, badge, or disposition set.
fn kind_rank(f: &FindingView) -> u8 {
    use camerata_api_types::audit::FindingKind;
    match &f.kind {
        None => 0,
        Some(FindingKind::Defect) => 1,
        Some(FindingKind::Deviation(_)) => 2,
        // Informational kinds never enter the Queue (that is the whole point of the surface
        // split), so this arm orders nothing in practice. It is last rather than unreachable
        // so the function stays total and a routing regression degrades visibly, not by panic.
        Some(_) => 3,
    }
}

/// Deviation ratio as a sortable integer in parts-per-million, ASCENDING (design §5 step 4).
///
/// A 2-of-47 deviation is a near-unanimous convention with two clear outliers — the fastest,
/// highest-confidence decision in the queue — while 19-of-47 is drifting toward no-convention
/// and needs thought. Cheapest decisions first.
///
/// An unknown (or zero) denominator yields [`u64::MAX`] so it sorts AFTER every known ratio:
/// the producer could not count honestly, so the row cannot claim a position in the cheapness
/// ordering. Non-deviation rows yield 0, which never decides anything — [`kind_rank`] has
/// already separated them by the time this term is reached.
fn deviation_ratio_ppm(f: &FindingView) -> u64 {
    use camerata_api_types::audit::FindingKind;
    match &f.kind {
        Some(FindingKind::Deviation(d)) => match d.assessed_sites {
            Some(assessed) if assessed > 0 => {
                (d.deviating_sites as u64).saturating_mul(1_000_000) / assessed as u64
            }
            _ => u64::MAX,
        },
        _ => 0,
    }
}

/// The Queue's default curated order (design §5), lexicographic:
///
/// 1. `status` — active before suppressed.
/// 2. `severity` — as CALIBRATED upstream; never re-derived here.
/// 3. `kind` — unspecified alarm, then Defect, then Deviation.
/// 4. deviation ratio, ascending; unknown denominators last.
/// 5. tie-break: rule id, then path, then line.
///
/// The default is opinionated; the table is not locked — chorale's column headers still
/// re-sort on demand.
pub fn queue_sort_key(f: &FindingView) -> (u8, u8, u8, u64, String, String, usize) {
    let enforced = if f.status == "active" { 0 } else { 1 };
    (
        enforced,
        severity_rank(&f.severity),
        kind_rank(f),
        deviation_ratio_ppm(f),
        f.rule_id.clone(),
        f.path.clone(),
        f.line,
    )
}

/// The default finding status (`"active"`). FindingView's `#[serde(default)]` provider; lives in
/// the scan surface (re-exported here so `FindingView`'s serde attribute resolves within core).
pub use crate::scan::default_finding_status;

/// Stable identity for a finding across the triage tables (repo + rule + location + snippet),
/// so its disposition survives table switches and re-sorts.
pub fn finding_key(f: &FindingView) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        f.repo, f.rule_id, f.path, f.line, f.snippet
    )
}

/// The disposition state for a finding (Unresolved when absent from the map).
pub fn finding_state(dispositions: &HashMap<String, Disposition>, f: &FindingView) -> TriageState {
    dispositions
        .get(&finding_key(f))
        .map(|d| d.state)
        .unwrap_or(TriageState::Unresolved)
}

/// The framework-agnostic onboarding-triage state machine. Owns the finding -> disposition map
/// plus the currently-viewed triage table, and exposes the pure transitions + derivations the
/// Dioxus layer drives (hooks/table/toast stay OUTSIDE; the mutation happens INSIDE). Extracted
/// verbatim from the scan adapter's onclick handlers + inline filter/sort.
pub struct TriageModel {
    pub dispositions: HashMap<String, Disposition>,
    pub triage_view: TriageState,
}

impl TriageModel {
    pub fn new() -> Self {
        Self {
            dispositions: HashMap::new(),
            triage_view: TriageState::Unresolved,
        }
    }

    /// The disposition state for a finding under this model's map (Unresolved when absent).
    pub fn state_of(&self, f: &FindingView) -> TriageState {
        finding_state(&self.dispositions, f)
    }

    /// Move each finding to `state` (its bucket/reason are left as-is / defaulted on insert).
    pub fn move_to(&mut self, findings: &[FindingView], state: TriageState) {
        for f in findings {
            self.dispositions.entry(finding_key(f)).or_default().state = state;
        }
    }

    /// CONFIRM each finding: "I read this, it is real, it ships in the report."
    ///
    /// Takes NO reason, deliberately, and that asymmetry is the point. `Ignored` and
    /// `FalsePositive` require one because each makes a claim BEYOND the finding — that the
    /// risk is acceptable, or that the tool was wrong — and the report has to be able to say
    /// why. Confirming makes no claim beyond the finding; the finding already states its own
    /// case, with its evidence, its citation and its severity. Demanding a sentence to agree
    /// with it would tax the majority outcome of every audit for no audit trail gained.
    ///
    /// Deliberately a thin wrapper over [`TriageModel::move_to`] rather than a second write
    /// path, so an existing reason/bucket on the disposition is left exactly as `move_to`
    /// leaves it (confirming does not erase the history of how the row got here, and nothing
    /// renders a `Confirmed` row's reason).
    pub fn confirm(&mut self, findings: &[FindingView]) {
        self.move_to(findings, TriageState::Confirmed);
    }

    /// RULE-LEVEL BULK: confirm every row of a deviation group, in one act (design §2/§4).
    ///
    /// The deviation case the design singles out: "if the curator believes the convention is
    /// real, all N sites are confirmed together far more often than not." This is that
    /// gesture.
    ///
    /// Returns `Some(n)` with the number of rows moved, or `None` for the ONE refusal — an
    /// empty group, where nothing moved so nothing may be claimed to have moved. There is no
    /// reason check here because [`TriageModel::confirm`] has none; see its doc comment for
    /// why that asymmetry with [`TriageModel::bulk_false_positive`] is correct rather than an
    /// oversight.
    ///
    /// Like its false-positive sibling it is a thin wrapper over the PER-ROW path given the
    /// group's rows, not a second write path. That is what makes "one click equals N
    /// gestures" a property of the code rather than a hope — see
    /// `req_pass2_10_bulk_confirm_equals_confirming_each_row_individually`.
    pub fn bulk_confirm(&mut self, group: &DeviationGroup) -> Option<usize> {
        if group.rows.is_empty() {
            return None;
        }
        self.confirm(&group.rows);
        Some(group.rows.len())
    }

    /// Ignore each finding: set state = Ignored AND record the (required) reason together, so a
    /// durable ignore never lands without its baseline-recorded reason.
    pub fn ignore(&mut self, findings: &[FindingView], reason: &str) {
        for f in findings {
            let e = self.dispositions.entry(finding_key(f)).or_default();
            e.state = TriageState::Ignored;
            e.reason = reason.to_string();
        }
    }

    /// Mark each finding as a FALSE POSITIVE: set state = FalsePositive AND record the
    /// (required) reason together — mirrors `ignore`'s require-reason invariant. The reason
    /// is the audit trail (why the tool was wrong) and feeds the report's methodology count
    /// ("N dispositioned as false positives by the auditor and excluded"). Unlike `ignore`,
    /// this disposition is a Process-time NO-OP: it never reaches the baseline and never
    /// files a ticket (see the Process handler in `cockpit/scan.rs`).
    pub fn mark_false_positive(&mut self, findings: &[FindingView], reason: &str) {
        for f in findings {
            let e = self.dispositions.entry(finding_key(f)).or_default();
            e.state = TriageState::FalsePositive;
            e.reason = reason.to_string();
        }
    }

    /// RULE-LEVEL BULK: mark every row of a deviation group as a false positive, in one act.
    ///
    /// Returns `Some(n)` with the number of rows moved, or `None` when the call was REFUSED
    /// and nothing was written. There are exactly two refusals, and both are silent-failure
    /// risks worth making explicit rather than leaving to the call site:
    ///
    /// * a blank reason — the same require-reason invariant as the per-row button. A bulk
    ///   gesture is where a reasonless disposition would be cheapest and most costly: one
    ///   click, N rows excluded from the report with no audit trail.
    /// * an empty group — nothing to move, so nothing is claimed to have moved.
    ///
    /// It is deliberately a thin wrapper over [`TriageModel::mark_false_positive`] given the
    /// group's rows, NOT a second write path. That is what makes "one click equals N
    /// gestures" a property of the code rather than a hope (see
    /// `req_pass2_9_bulk_false_positive_equals_marking_each_row_individually`).
    ///
    /// Its affirmative sibling is [`TriageModel::bulk_confirm`] (Pass 2 step 3), the same
    /// shape against `TriageState::Confirmed` and without the reason check.
    pub fn bulk_false_positive(&mut self, group: &DeviationGroup, reason: &str) -> Option<usize> {
        if reason.trim().is_empty() || group.rows.is_empty() {
            return None;
        }
        self.mark_false_positive(&group.rows, reason);
        Some(group.rows.len())
    }

    /// Bucket each tech-debt finding (resolve later / now). Leaves the state unchanged; only the
    /// Bucket flag moves.
    pub fn set_bucket(&mut self, findings: &[FindingView], bucket: TechDebtBucket) {
        for f in findings {
            self.dispositions.entry(finding_key(f)).or_default().bucket = bucket;
        }
    }

    /// The findings visible in the current `triage_view`: the QUEUE rows (see
    /// [`kind_surface`]) in that disposition, in the curated default order
    /// ([`queue_sort_key`]).
    ///
    /// The kind filter is load-bearing, not cosmetic. An informational row asserts nothing is
    /// wrong: it has no site to dispose and no severity worth showing, so leaving it here made
    /// a compliment compete with a critical injection finding for the curator's attention AND
    /// parked it as Unresolved forever, which the Process gate blocks on. Informational rows
    /// are served by [`findings_on_surface`] instead.
    pub fn visible(&self, findings: &[FindingView]) -> Vec<FindingView> {
        let mut out: Vec<FindingView> = findings
            .iter()
            .filter(|f| kind_surface(f) == KindSurface::Queue)
            .filter(|f| self.state_of(f) == self.triage_view)
            .cloned()
            .collect();
        out.sort_by_key(queue_sort_key);
        out
    }

    /// Tally every disposition over `findings` — see [`TriageCounts`].
    ///
    /// `findings` must already be the QUEUE row set ([`findings_on_surface`] with
    /// [`KindSurface::Queue`]). An informational row has no disposition and no table in which
    /// to acquire one, so counting it here parks it as `Unresolved` forever and the Process
    /// gate blocks on a row saying the codebase is CORRECT — the live bug step 1 fixed. The
    /// filter deliberately lives at the call site rather than inside this loop: this function
    /// is also how a single triage table's own tally is taken, and silently re-filtering its
    /// input would make those two uses disagree.
    pub fn counts(&self, findings: &[FindingView]) -> TriageCounts {
        let mut counts = TriageCounts::default();
        for f in findings {
            match self.state_of(f) {
                TriageState::Unresolved => counts.unresolved += 1,
                TriageState::Confirmed => counts.confirmed += 1,
                TriageState::Ignored => counts.ignored += 1,
                TriageState::TechDebt => counts.tech_debt += 1,
                TriageState::FalsePositive => counts.false_positive += 1,
            }
        }
        counts
    }
}

/// The per-disposition tally over one finding set: the sub-tab counts, and everything the
/// Process gate decides from.
///
/// A named struct rather than the tuple this used to be, because the tuple was already at four
/// positionally-identical `usize`s and `Confirmed` would have made five. The two readings a
/// caller actually wants — "is the curator done" and "is there anything to commit" — are
/// methods here rather than inline arithmetic at the call site, so the gate's rule is stated
/// once and unit-tested once.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TriageCounts {
    pub unresolved: usize,
    pub confirmed: usize,
    pub ignored: usize,
    pub tech_debt: usize,
    pub false_positive: usize,
}

impl TriageCounts {
    /// This tally's count for one disposition — so the sub-tab loop can read
    /// [`TriageState::all`] and ask for each, instead of a five-arm match that could drift
    /// from the enum.
    pub fn of(self, state: TriageState) -> usize {
        match state {
            TriageState::Unresolved => self.unresolved,
            TriageState::Confirmed => self.confirmed,
            TriageState::Ignored => self.ignored,
            TriageState::TechDebt => self.tech_debt,
            TriageState::FalsePositive => self.false_positive,
        }
    }

    /// Every row counted.
    pub fn total(self) -> usize {
        self.unresolved + self.confirmed + self.ignored + self.tech_debt + self.false_positive
    }

    /// Rows the curator has NOT decided yet — the ONLY thing the Process gate blocks on.
    ///
    /// Reads [`TriageState::is_decided`] over the whole vocabulary rather than returning
    /// `self.unresolved` directly, so a future disposition is outstanding or not according to
    /// the enum's own answer and never because this function forgot about it.
    pub fn outstanding(self) -> usize {
        TriageState::all()
            .into_iter()
            .filter(|s| !s.is_decided())
            .map(|s| self.of(s))
            .sum()
    }

    /// Whether Process has anything to DO.
    ///
    /// Only `Ignored` (baseline waivers) and `TechDebt` (tickets) commit anything server-side.
    /// `Confirmed` and `FalsePositive` are both client-local by design, so a queue made
    /// entirely of them is fully triaged AND has nothing to process — which is why the gate
    /// needs this as a separate question from [`TriageCounts::outstanding`].
    pub fn has_committable_work(self) -> bool {
        self.ignored > 0 || self.tech_debt > 0
    }

    /// Whether the Process button may be enabled: nothing outstanding, and something to commit.
    pub fn can_process(self) -> bool {
        self.outstanding() == 0 && self.has_committable_work()
    }
}

impl Default for TriageModel {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// THE DOCKED DETAIL PANE: row focus, keystroke scoping, undo (design §2, §13 Q5)
// ─────────────────────────────────────────────────────────────────────────────
//
// A note on chorale, because the design document is wrong about it and the wrong version is
// what a future reader will find first. Design §13's dependency check recorded
// `detail_renderer` / `set_detail_column_enabled` / `detail_column_enabled` as "the docked
// pane" in chorale 0.2.3. They are not. `detail_renderer` is an EXPANDABLE INLINE ROW DETAIL:
// `chorale-dioxus` renders it as `<tr class="chorale-detail-panel"><td colspan=N>…</td></tr>`
// directly beneath its parent row (`detail_panel_tr`, components.rs:1998), with a chevron
// column to expand it. That is a different thing from a pane docked beside the table, and it
// does not deliver the gesture this step exists for: an inline panel pushes every row below
// it down the page, so cursor-down-read-decide would reflow the table under the cursor on
// every move. So the pane is built in Camerata. This mirrors step 2's finding that the
// group-header "content hook" in the same dependency check was also not one.
//
// What chorale DOES give, and what these types are shaped around, is the ordered list of rows
// it is actually showing: `chorale_core::visible_grouped_view` is public and returns the
// post-filter, post-sort, post-COLLAPSE sequence. Expressing focus as a position in that list
// is what makes "focus survives a re-sort" and "focus survives a collapse" one rule instead of
// two features.

/// Which way an arrow key moves the row cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusMove {
    Up,
    Down,
}

/// THE ROW CURSOR: which Queue row the docked detail pane is describing.
///
/// Anchored to the row's [`finding_key`] and NOT to its index, with the index kept only as
/// the fallback for when that key is gone. That one choice answers both coexistence
/// questions this step had to settle:
///
/// * **A re-sort** permutes the visible list without removing anything, so the focused key is
///   still there and the same finding stays focused at its new position. An index-anchored
///   cursor would silently re-point at whatever row landed under it, and the pane would start
///   describing a different finding without the curator touching anything — the worst
///   possible failure for a surface whose entire job is "read this, then press a key".
/// * **A collapse, a filter, or the disposition itself** removes the focused row from the
///   list. Then the index is the honest fallback: focus clamps to the row that took the
///   vacated position (or to the last row, when the tail went away). For the disposition case
///   that IS the burn-down gesture — decide the row, and its successor is already focused.
///
/// Every method is pure and returns a new cursor; nothing here mutates, so the adapter can
/// resolve the cursor against the current frame's row list during render without writing to a
/// signal mid-render.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RowCursor {
    /// The focused row's [`finding_key`]. `None` before anything is focused, and after the
    /// view empties.
    key: Option<String>,
    /// The focused row's position the last time this cursor was resolved. The fallback anchor
    /// when `key` is no longer on screen.
    index: usize,
}

/// The key of the finding the cursor is ON right now: the stored cursor resolved against the rows
/// currently on screen, so it is never a key that has left the table (a decided row) and never
/// `None` while any row is visible. Every reader of "the focused finding" (the pane, the context
/// request, a re-cut of the table) asks this one function, not the stored key.
pub fn focused_key(cursor: &RowCursor, visible: &[String]) -> Option<String> {
    cursor.resolve(visible).key().map(str::to_string)
}

impl RowCursor {
    /// A cursor focused on nothing. The first [`RowCursor::resolve`] against a non-empty list
    /// lands it on the first row.
    pub fn new() -> Self {
        Self::default()
    }

    /// A cursor aimed at a specific row — what a row CLICK produces, so the mouse and the
    /// keyboard drive one cursor rather than two competing ones. If that key turns out not to
    /// be on screen, [`RowCursor::resolve`] applies the usual clamp.
    pub fn focused_on(key: impl Into<String>) -> Self {
        Self {
            key: Some(key.into()),
            index: 0,
        }
    }

    /// The focused row's key, or `None` when nothing is focused.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The focused row's position in the list this cursor was last resolved against.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Re-anchor against the rows currently on screen, in the order they are on screen.
    ///
    /// `visible` is the list of [`finding_key`]s for chorale's own
    /// [`visible_grouped_view`](https://docs.rs/chorale-core) data rows, so EVERY table
    /// operation — manual re-sort, column filter, group collapse, the disposition removing
    /// the row — reaches the cursor as a change to this one argument, and is answered by this
    /// one function.
    ///
    /// Total: an empty list yields an unfocused cursor, and a cursor is never left pointing at
    /// a key that is not in `visible`.
    pub fn resolve(&self, visible: &[String]) -> Self {
        if visible.is_empty() {
            return Self {
                key: None,
                index: 0,
            };
        }
        if let Some(k) = self.key.as_deref() {
            if let Some(pos) = visible.iter().position(|v| v == k) {
                return Self {
                    key: Some(k.to_string()),
                    index: pos,
                };
            }
        }
        // Either nothing was focused, or the focused row is gone (collapsed, filtered, or
        // dispositioned out of this table). Take whatever now occupies its index, clamped.
        let idx = self.index.min(visible.len() - 1);
        Self {
            key: Some(visible[idx].clone()),
            index: idx,
        }
    }

    /// Move one row in `dir`, CLAMPED at both ends.
    ///
    /// Deliberately no wrap-around. At 400 rows a held cursor-down that silently restarts at
    /// the top is indistinguishable from one that has stopped, and the curator would re-review
    /// rows believing he was making progress. Resolves first, so moving from a stale cursor
    /// starts from a real on-screen position.
    pub fn moved(&self, visible: &[String], dir: FocusMove) -> Self {
        let here = self.resolve(visible);
        if visible.is_empty() {
            return here;
        }
        let idx = match dir {
            FocusMove::Up => here.index.saturating_sub(1),
            FocusMove::Down => (here.index + 1).min(visible.len() - 1),
        };
        Self {
            key: Some(visible[idx].clone()),
            index: idx,
        }
    }
}

/// WHERE KEYBOARD FOCUS IS, as far as the detail pane is concerned. The whole of the owner's
/// Q5 scoping clause is carried by this enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneFocus {
    /// The pane's row cursor holds focus. Keys are COMMANDS.
    Cursor,
    /// A text input inside the pane — the on-demand reason prompt — holds focus. Keys are
    /// TEXT. This is the case the scoping clause was written for: the prompt sits inches from
    /// the row it describes, `I` and `F` are what OPEN it, and the very next thing the
    /// curator does is type a sentence into it.
    ReasonPrompt,
    /// Focus is outside the pane: the table, a column filter, the toolbar, the rescan bar,
    /// nothing at all. Keys are not ours.
    Elsewhere,
}

/// Modifier keys held with a press.
///
/// `shift` is tracked but is NOT a blocker — it is how a capital `C` gets typed. `ctrl`,
/// `alt` and `meta` are, because Cmd-C is copy and Ctrl-C is interrupt, and copying a snippet
/// out of the pane is a thing a curator does constantly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyMods {
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub shift: bool,
}

impl KeyMods {
    /// No modifiers held.
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether a modifier that changes what the press MEANS is held.
    fn claims_the_press(self) -> bool {
        self.ctrl || self.alt || self.meta
    }
}

/// What a press in the detail pane asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneCommand {
    /// Apply this disposition to the focused row NOW. Only ever a state that needs no reason.
    Dispose(TriageState),
    /// Open the on-demand reason prompt for this disposition. NOTHING is written yet — see
    /// the module note on why the reason-carrying keys prompt rather than apply.
    PromptFor(TriageState),
    /// Move the row cursor.
    MoveFocus(FocusMove),
    /// Open the in-app code viewer on the focused finding. Moves no row and writes nothing,
    /// so it needs no undo and no reason.
    OpenCode,
}

/// The single-key hotkey for a disposition, or `None` for a state that has none.
///
/// `C` / `I` / `T` / `F`, exactly the owner's four. `Unresolved` has none deliberately: it is
/// the state rows START in rather than a decision, and a key that silently un-decides a row is
/// not something to hand someone reviewing 400 findings at 1am.
pub fn state_hotkey(state: TriageState) -> Option<&'static str> {
    match state {
        TriageState::Unresolved => None,
        TriageState::Confirmed => Some("c"),
        TriageState::Ignored => Some("i"),
        TriageState::TechDebt => Some("t"),
        TriageState::FalsePositive => Some("f"),
    }
}

/// THE SCOPED KEY HANDLER, as a pure function (owner's Q5 ruling, design §13).
///
/// > `C` / `I` / `T` / `F` on the focused row, **scoped so the keys only fire while the detail
/// > pane holds focus**, plus a **five-second undo toast**.
///
/// The scoping is the first thing this does and it is unconditional: anything but
/// [`PaneFocus::Cursor`] returns `None` for every key there is. It is a whitelist rather than
/// a blacklist of known-bad inputs, because the failure mode being prevented is a keystroke
/// reaching a row from somewhere nobody thought of.
///
/// ## Why `I` and `F` prompt instead of applying
///
/// Those two dispositions require a reason, and the report export reads it. There were three
/// honest options: prompt, apply-then-collect the reason, or give keys only to the two
/// reasonless states. This is the prompt, which is also what design §3 already specified for
/// the buttons ("an on-demand inline prompt that appears when the action is invoked
/// (keystroke or button), pre-focused, Enter to commit, Esc to cancel. Same require-reason
/// invariant, enforced at the same point").
///
/// Apply-then-collect was rejected on a concrete failure rather than on taste: it puts the row
/// into `Ignored` with an empty reason for as long as it takes the curator to type one, and
/// the curator's next gesture in this UI is cursor-down. A reasonless `Ignored` row is a
/// baseline waiver with no audit trail — the exact hole the require-reason invariant and
/// [`TriageModel::bulk_false_positive`]'s refusal both exist to close. Keys-only-for-the-
/// reasonless was rejected because it drops two of the four keys the ruling names.
///
/// The prompt also composes with the scoping instead of fighting it: opening it moves focus to
/// [`PaneFocus::ReasonPrompt`], so the keys disarm for exactly as long as the curator is
/// typing, and re-arm when the prompt closes.
pub fn pane_command(focus: PaneFocus, key: &str, mods: KeyMods) -> Option<PaneCommand> {
    // THE SCOPE GATE. Everything below is unreachable from anywhere but the pane's cursor.
    if focus != PaneFocus::Cursor {
        return None;
    }
    if mods.claims_the_press() {
        return None;
    }
    match key {
        "ArrowDown" => return Some(PaneCommand::MoveFocus(FocusMove::Down)),
        "ArrowUp" => return Some(PaneCommand::MoveFocus(FocusMove::Up)),
        _ => {}
    }
    // One character only, matched case-insensitively against the disposition hotkeys. Looping
    // `TriageState::all()` rather than spelling out four arms keeps the key set and the
    // prompt/apply split derived from the enum: a sixth disposition with a hotkey gets both
    // for free, and one without a hotkey stays keyless.
    if key.chars().count() != 1 {
        return None;
    }
    let pressed = key.to_ascii_lowercase();
    // `O` opens the code viewer. It is not a disposition, so it is matched before the
    // hotkey loop and can never collide with one (`state_hotkey` has no `o`).
    if pressed == "o" {
        return Some(PaneCommand::OpenCode);
    }
    let state = TriageState::all()
        .into_iter()
        .find(|s| state_hotkey(*s) == Some(pressed.as_str()))?;
    Some(if state.requires_reason() {
        PaneCommand::PromptFor(state)
    } else {
        PaneCommand::Dispose(state)
    })
}

/// What the pane's focus state becomes when the DOM reports `incoming`, given whether the pane's
/// own reason prompt is open. `None` means leave it as it is.
///
/// While the prompt is open the state is `ReasonPrompt` and nothing else: a blur out of the pane
/// must not disarm the prompt's own state, and a focus back INTO the pane (what closing the code
/// viewer does, by calling `focus_detail_pane`) must not re-arm the disposition keys over a
/// half-written reason. Disarming early is the safe direction.
pub fn next_pane_focus(incoming: PaneFocus, prompt_open: bool) -> Option<PaneFocus> {
    if prompt_open {
        return match incoming {
            PaneFocus::Elsewhere => None,
            PaneFocus::Cursor | PaneFocus::ReasonPrompt => Some(PaneFocus::ReasonPrompt),
        };
    }
    Some(incoming)
}

/// Whether the disposition keys will fire under this focus — what the pane's armed/off
/// indicator renders.
///
/// Defined as "would `C` produce a command" rather than as a second match on [`PaneFocus`],
/// so what the curator is TOLD about the keys and what the handler actually does cannot drift
/// apart. A pane claiming "keys armed" while [`pane_command`] silently refuses would be the
/// ships-inert shape in miniature.
pub fn pane_keys_armed(focus: PaneFocus) -> bool {
    pane_command(focus, "c", KeyMods::none()).is_some()
}

/// How long a keyboard disposition stays undoable — **five seconds**, per the owner's Q5
/// ruling ("plus a five-second undo toast … the toast removes the 1am risk").
///
/// The window is what makes a one-keystroke disposition safe to offer at all: the cost of a
/// mis-press is bounded by five seconds of attention rather than by noticing it later in an
/// exported report.
pub const UNDO_WINDOW_MS: u64 = 5_000;

/// An applied disposition that can still be taken back, and everything needed to take it
/// back exactly.
///
/// Holds the PRIOR [`Disposition`] of every row it touched — `None` for a row that had no
/// entry at all, so undo restores ABSENCE rather than writing an explicit `Unresolved`
/// record. That distinction is not pedantry: the map is persisted in the auto-saved draft and
/// read by the report export, so an undone action that leaves a row behind has not fully
/// undone itself.
///
/// Timestamps are plain `u64` milliseconds supplied by the caller rather than read from a
/// clock here, which is what keeps the window testable without sleeping and keeps this module
/// free of a time dependency.
#[derive(Clone, PartialEq, Debug)]
pub struct UndoEntry {
    /// What the toast says it will reverse.
    message: String,
    /// `(finding_key, prior disposition)` for every touched row. `None` = absent from the map.
    prior: Vec<(String, Option<Disposition>)>,
    /// When the action was applied, in the caller's millisecond clock.
    armed_at_ms: u64,
    /// Whether this entry has already been used. An undo is single use.
    spent: bool,
}

impl UndoEntry {
    /// What the toast says it will reverse — the disposition and the count, never an unnamed
    /// "Undo".
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether the undo still works at `now_ms`. The window closes at exactly
    /// `armed_at + `[`UNDO_WINDOW_MS`], and a spent entry is never live.
    pub fn is_live(&self, now_ms: u64) -> bool {
        !self.spent && now_ms.saturating_sub(self.armed_at_ms) < UNDO_WINDOW_MS
    }

    /// Milliseconds left in the window, for the toast's countdown. `0` once it has closed, so
    /// the toast can dismiss itself the moment the button stops working rather than a frame
    /// later — a visible "Undo" that silently refuses is the thing the window is for.
    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        if self.spent {
            return 0;
        }
        UNDO_WINDOW_MS.saturating_sub(now_ms.saturating_sub(self.armed_at_ms))
    }

    /// How many rows this entry would put back.
    pub fn row_count(&self) -> usize {
        self.prior.len()
    }
}

/// What [`TriageModel::undo`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UndoOutcome {
    /// Reverted this many rows.
    Reverted(usize),
    /// The five seconds are up; nothing was written.
    WindowClosed,
    /// This entry was already used; nothing was written. Reverting twice could reach back
    /// over a newer decision.
    AlreadyUndone,
}

/// The toast sentence for an applied disposition.
///
/// Reads [`TriageState::label`] for the three it cannot phrase as a verb, so a renamed
/// disposition renames itself here too.
fn undo_message(state: TriageState, n: usize) -> String {
    let noun = if n == 1 { "finding" } else { "findings" };
    match state {
        TriageState::Confirmed => format!("Confirmed {n} {noun}."),
        TriageState::Unresolved => format!("Moved {n} {noun} back to Unresolved."),
        other => format!("Marked {n} {noun} as {}.", other.label()),
    }
}

impl TriageModel {
    /// Apply `state` to `rows`, capturing what it overwrote so the next five seconds can take
    /// it back (owner's Q5 ruling).
    ///
    /// Returns `None` — having written NOTHING — for the two refusals:
    ///
    /// * a reason-carrying state ([`TriageState::requires_reason`]) with a blank reason. The
    ///   same invariant the buttons and [`TriageModel::bulk_false_positive`] enforce, at the
    ///   same point. The keyboard is where dropping it would be cheapest and least visible,
    ///   which is exactly why it is checked here rather than at the call site.
    /// * an empty row set. Nothing moved, so no undo may be offered for it.
    ///
    /// Routes every write through the EXISTING per-row paths (`confirm` / `ignore` /
    /// `mark_false_positive` / `move_to`) rather than touching the map itself, so the
    /// keyboard provably leaves the state the buttons leave instead of becoming a second,
    /// divergent write path — the same discipline `bulk_confirm` and `bulk_false_positive`
    /// are built on.
    pub fn dispose_with_undo(
        &mut self,
        rows: &[FindingView],
        state: TriageState,
        reason: &str,
        now_ms: u64,
    ) -> Option<UndoEntry> {
        if rows.is_empty() {
            return None;
        }
        if state.requires_reason() && reason.trim().is_empty() {
            return None;
        }
        // Snapshot BEFORE the write, deduplicated by key so the same row listed twice does
        // not record its own post-write value as its "prior".
        let mut prior: Vec<(String, Option<Disposition>)> = Vec::with_capacity(rows.len());
        for f in rows {
            let key = finding_key(f);
            if prior.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let before = self.dispositions.get(&key).cloned();
            prior.push((key, before));
        }
        match state {
            TriageState::Confirmed => self.confirm(rows),
            TriageState::Ignored => self.ignore(rows, reason),
            TriageState::FalsePositive => self.mark_false_positive(rows, reason),
            other => self.move_to(rows, other),
        }
        Some(UndoEntry {
            message: undo_message(state, prior.len()),
            prior,
            armed_at_ms: now_ms,
            spent: false,
        })
    }

    /// Put back everything `entry` overwrote, if it still may be.
    ///
    /// Writes nothing outside the window and nothing for an entry already used, and SAYS
    /// which it was ([`UndoOutcome`]) rather than returning a bare bool — a toast whose button
    /// quietly does nothing is the failure this whole mechanism exists to avoid, and a
    /// silently-refusing undo would be a second instance of it.
    ///
    /// Restores absence as absence: a row that had no disposition before the action has its
    /// entry REMOVED, not set to a default-constructed `Unresolved`.
    pub fn undo(&mut self, entry: &mut UndoEntry, now_ms: u64) -> UndoOutcome {
        if entry.spent {
            return UndoOutcome::AlreadyUndone;
        }
        if !entry.is_live(now_ms) {
            return UndoOutcome::WindowClosed;
        }
        for (key, before) in &entry.prior {
            match before {
                Some(d) => {
                    self.dispositions.insert(key.clone(), d.clone());
                }
                None => {
                    self.dispositions.remove(key);
                }
            }
        }
        entry.spent = true;
        UndoOutcome::Reverted(entry.prior.len())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_lenient_wire_names_are_exactly_what_serde_writes() {
        use crate::lenient::WireEnum;
        for state in TriageState::all() {
            let written = serde_json::to_string(&state).unwrap();
            assert_eq!(written, format!("\"{}\"", state.wire_name()));
            assert_eq!(TriageState::from_wire(state.wire_name()), Some(state));
        }
        assert_eq!(TriageState::from_wire("Archived"), None);
    }

    fn rows(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| (*k).to_string()).collect()
    }

    /// M1: the focused key is the RESOLVED one. A stored key that has left the table (the row was
    /// decided) answers whatever now sits at its place, and a never-set cursor answers the first
    /// row, not nothing.
    #[test]
    fn focused_key_resolves_a_gone_or_unset_cursor_against_the_rows_on_screen() {
        let on_screen = rows(&["a", "b", "c"]);
        assert_eq!(
            focused_key(&RowCursor::focused_on("b"), &on_screen),
            Some("b".into())
        );
        // "b" was decided and left; the cursor was last resolved at index 1.
        let at_b = RowCursor::focused_on("b").resolve(&on_screen);
        let after = rows(&["a", "c"]);
        assert_eq!(focused_key(&at_b, &after), Some("c".into()));
        assert_eq!(at_b.key(), Some("b"), "the STORED key is the stale one");
        // Never set.
        assert_eq!(focused_key(&RowCursor::new(), &on_screen), Some("a".into()));
        // Nothing on screen (every group collapsed): nothing focused.
        assert_eq!(focused_key(&RowCursor::focused_on("b"), &[]), None);
    }

    use super::*;

    fn finding(json: serde_json::Value) -> FindingView {
        serde_json::from_value(json).expect("valid FindingView fixture")
    }

    // ── moved from the scan adapter (path prefix changed only) ────────────────

    #[test]
    fn finding_key_combines_identity_fields() {
        let f = finding(serde_json::json!({
            "repo": "owner/repo", "path": "src/a.rs", "line": 7,
            "rule_id": "RULE-X", "severity": "high", "snippet": "snip", "detail": ""
        }));
        let key = finding_key(&f);
        assert!(key.contains("owner/repo"));
        assert!(key.contains("RULE-X"));
        assert!(key.contains("src/a.rs"));
        assert!(key.contains('7'));
        assert!(key.contains("snip"));
    }

    #[test]
    fn finding_state_defaults_to_unresolved_when_absent() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        let map = HashMap::new();
        assert_eq!(finding_state(&map, &f), TriageState::Unresolved);
    }

    #[test]
    fn finding_state_reads_disposition_when_present() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        let mut map = HashMap::new();
        map.insert(
            finding_key(&f),
            Disposition {
                state: TriageState::Ignored,
                reason: "noise".to_string(),
                bucket: TechDebtBucket::Later,
            },
        );
        assert_eq!(finding_state(&map, &f), TriageState::Ignored);
    }

    #[test]
    fn default_finding_status_is_active() {
        assert_eq!(default_finding_status(), "active");
    }

    // ── evaluated_option_id (audit-integrated alternative recommendation) ──────────────────

    #[test]
    fn evaluated_option_id_round_trips_when_present() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": "",
            "evaluated_option_id": "option-b"
        }));
        assert_eq!(f.evaluated_option_id.as_deref(), Some("option-b"));
    }

    #[test]
    fn evaluated_option_id_defaults_to_none_when_absent() {
        // Back-compat: a finding from before this field existed (or a single-option rule's
        // finding, which the server never tags) must still deserialize.
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        assert_eq!(f.evaluated_option_id, None);
    }

    #[test]
    fn triage_state_defaults_to_unresolved() {
        assert_eq!(TriageState::default(), TriageState::Unresolved);
    }

    // ── new: the TriageModel state machine ────────────────────────────────────

    fn f(repo: &str, line: usize, severity: &str, status: &str) -> FindingView {
        finding(serde_json::json!({
            "repo": repo, "path": "p", "line": line,
            "rule_id": "R", "severity": severity, "snippet": "s", "detail": "", "status": status
        }))
    }

    #[test]
    fn disposition_default_is_unresolved_empty_later() {
        let d = Disposition::default();
        assert_eq!(d.state, TriageState::Unresolved);
        assert_eq!(d.reason, "");
        // TechDebtBucket derives no Debug (kept identical to the pre-extraction type), so compare
        // via PartialEq rather than assert_eq!'s Debug formatting.
        assert!(d.bucket == TechDebtBucket::Later);
    }

    #[test]
    fn move_to_round_trips_state() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        assert_eq!(m.state_of(&item), TriageState::Unresolved);
        m.move_to(std::slice::from_ref(&item), TriageState::TechDebt);
        assert_eq!(m.state_of(&item), TriageState::TechDebt);
        m.move_to(std::slice::from_ref(&item), TriageState::Unresolved);
        assert_eq!(m.state_of(&item), TriageState::Unresolved);
    }

    #[test]
    fn ignore_sets_state_and_reason_together() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        m.ignore(std::slice::from_ref(&item), "noise");
        let d = m.dispositions.get(&finding_key(&item)).expect("recorded");
        assert_eq!(d.state, TriageState::Ignored);
        assert_eq!(d.reason, "noise");
    }

    #[test]
    fn set_bucket_flips_later_and_now_via_or_default() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        // First set on an absent key goes through or_default (starts Later), then flips to Now.
        m.set_bucket(std::slice::from_ref(&item), TechDebtBucket::Now);
        assert!(m.dispositions.get(&finding_key(&item)).unwrap().bucket == TechDebtBucket::Now);
        m.set_bucket(std::slice::from_ref(&item), TechDebtBucket::Later);
        assert!(m.dispositions.get(&finding_key(&item)).unwrap().bucket == TechDebtBucket::Later);
    }

    #[test]
    fn visible_filters_to_view_and_sorts() {
        let mut m = TriageModel::new();
        let unresolved_crit = f("r", 1, "critical", "active");
        let unresolved_low = f("r", 2, "low", "active");
        let unresolved_high_suppressed = f("r", 3, "high", "suppressed-baseline");
        let ignored = f("r", 4, "critical", "active");
        m.ignore(std::slice::from_ref(&ignored), "noise");

        let all = vec![
            unresolved_low.clone(),
            unresolved_high_suppressed.clone(),
            unresolved_crit.clone(),
            ignored.clone(),
        ];

        // Unresolved view: the ignored finding is filtered out.
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 3);
        // active-before-suppressed, then critical > high > medium > low:
        // active/critical (line 1), active/low (line 2), suppressed/high (line 3).
        assert_eq!(vis[0].line, 1);
        assert_eq!(vis[1].line, 2);
        assert_eq!(vis[2].line, 3);

        // Ignored view shows only the ignored finding.
        m.triage_view = TriageState::Ignored;
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].line, 4);
    }

    // EDITED for Pass 2 step 3 (exception (a): a fifth disposition legitimately touches count
    // assertions). The tally gained a `confirmed` field and became a named struct; the
    // one-of-each shape of the assertion is unchanged.
    #[test]
    fn counts_tallies_every_bucket() {
        let mut m = TriageModel::new();
        let a = f("r", 1, "low", "active");
        let b = f("r", 2, "low", "active");
        let c = f("r", 3, "low", "active");
        let d = f("r", 4, "low", "active");
        let e = f("r", 5, "low", "active");
        m.ignore(std::slice::from_ref(&b), "noise");
        m.move_to(std::slice::from_ref(&c), TriageState::TechDebt);
        m.mark_false_positive(
            std::slice::from_ref(&d),
            "not exploitable — mocked in test scope",
        );
        m.confirm(std::slice::from_ref(&e));
        let all = vec![a, b, c, d, e];
        assert_eq!(
            m.counts(&all),
            TriageCounts {
                unresolved: 1,
                confirmed: 1,
                ignored: 1,
                tech_debt: 1,
                false_positive: 1,
            }
        );
        assert_eq!(m.counts(&all).total(), 5);
    }

    // ── FalsePositive disposition (the explicit ask) ──────────────────────────

    #[test]
    fn false_positive_label_reads_false_positive() {
        assert_eq!(TriageState::FalsePositive.label(), "False positive");
    }

    #[test]
    fn mark_false_positive_sets_state_and_reason_together() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        m.mark_false_positive(
            std::slice::from_ref(&item),
            "tool misfired on generated code",
        );
        let d = m.dispositions.get(&finding_key(&item)).expect("recorded");
        assert_eq!(d.state, TriageState::FalsePositive);
        assert_eq!(d.reason, "tool misfired on generated code");
    }

    #[test]
    fn mark_false_positive_is_distinct_from_ignore() {
        // FalsePositive and Ignored must never collapse into the same state — a false
        // positive is "the tool was wrong" (excluded, no baseline entry), Ignored is
        // "real but accepted risk" (committed to the baseline). Mixing them up would be
        // dishonest in both directions.
        let mut m = TriageModel::new();
        let fp = f("r", 1, "low", "active");
        let ignored = f("r", 2, "low", "active");
        m.mark_false_positive(std::slice::from_ref(&fp), "false alarm");
        m.ignore(std::slice::from_ref(&ignored), "accepted for now");
        assert_eq!(m.state_of(&fp), TriageState::FalsePositive);
        assert_eq!(m.state_of(&ignored), TriageState::Ignored);
        assert_ne!(TriageState::FalsePositive, TriageState::Ignored);
    }

    #[test]
    fn visible_filters_false_positive_into_its_own_view() {
        let mut m = TriageModel::new();
        let unresolved = f("r", 1, "high", "active");
        let fp = f("r", 2, "critical", "active");
        m.mark_false_positive(
            std::slice::from_ref(&fp),
            "generated fixture, not real code",
        );
        let all = vec![unresolved.clone(), fp.clone()];

        // Unresolved view excludes the false positive.
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].line, 1);

        // FalsePositive view shows only the false positive.
        m.triage_view = TriageState::FalsePositive;
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].line, 2);
    }

    // ── FindingView confidence/effort mirror (server `Finding` sync) ──────────

    #[test]
    fn finding_view_confidence_and_effort_default_to_none() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        assert_eq!(f.confidence, None);
        assert_eq!(f.effort, None);
    }

    #[test]
    fn finding_view_deserializes_confidence_and_effort_when_present() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "high", "snippet": "s", "detail": "",
            "confidence": "needs-review", "effort": "high"
        }));
        assert_eq!(f.confidence.as_deref(), Some("needs-review"));
        assert_eq!(f.effort.as_deref(), Some("high"));
    }

    // ── Pass 2 step 1: the five-kind taxonomy reaches the curator UI ──────────
    //
    // RED BEFORE THE FIX. Both of these fail on `850cac4`:
    //  1. `FindingView` drops the wire's flattened `kind` discriminator on the floor, so the
    //     UI cannot tell a compliment from an accusation at all.
    //  2. Consequently every informational row sits in the triage Queue next to real defects.

    /// req_pass2_1: the wire's flattened `kind` discriminator and its payload survive the trip
    /// into `FindingView` and back out — the same round-trip contract `confidence`/`effort`
    /// already hold. A field the server sets and the UI silently drops is the whole defect.
    #[test]
    fn req_pass2_1_finding_view_mirrors_the_wire_kind_discriminator() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "API-VERSIONING-1", "severity": "medium", "snippet": "s", "detail": "",
            "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
            "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
        }));
        let back = serde_json::to_value(&f).expect("FindingView serializes");
        assert_eq!(
            back["kind"], "deviation",
            "the kind discriminator must survive into the view model and back out"
        );
        assert_eq!(
            back["deviating_sites"], 2,
            "the deviation numerator must survive"
        );
        assert_eq!(
            back["assessed_sites"], 47,
            "the deviation denominator must survive"
        );
        assert_eq!(back["convention_label"], "URI-versioned endpoints");
    }

    /// req_pass2_2: an informational row (a convention the codebase already follows — a
    /// COMPLIMENT) must never appear in the triage Queue, where it competes for attention with
    /// an accusation and blocks the Process gate by sitting Unresolved forever.
    #[test]
    fn req_pass2_2_informational_rows_are_not_in_the_triage_queue() {
        let defect = finding(serde_json::json!({
            "repo": "r", "path": "src/a.rs", "line": 1, "rule_id": "SQL-INJECTION-1",
            "severity": "critical", "snippet": "s", "detail": "", "kind": "defect"
        }));
        let compliment = finding(serde_json::json!({
            "repo": "r", "path": "src/b.rs", "line": 2, "rule_id": "API-VERSIONING-1",
            "severity": "info", "snippet": "s", "detail": "", "kind": "convention_observed",
            "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
        }));
        let m = TriageModel::new();
        let visible = m.visible(&[defect, compliment]);
        assert_eq!(
            visible.len(),
            1,
            "the Queue must hold only the accusing kinds; got {} rows: {:?}",
            visible.len(),
            visible
                .iter()
                .map(|f| f.rule_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(visible[0].rule_id, "SQL-INJECTION-1");
    }

    // ── Pass 2 step 1: kind routing, the sort, and the one-surface invariant ──

    /// Build a finding with an explicit kind payload merged into the row (the wire shape).
    fn kinded(rule_id: &str, severity: &str, kind: serde_json::Value) -> FindingView {
        let mut row = serde_json::json!({
            "repo": "r", "path": "src/x.rs", "line": 1, "rule_id": rule_id,
            "severity": severity, "snippet": "s", "detail": "", "status": "active"
        });
        for (k, v) in kind.as_object().expect("kind payload is an object") {
            row[k] = v.clone();
        }
        finding(row)
    }

    fn defect(rule_id: &str, severity: &str) -> FindingView {
        kinded(rule_id, severity, serde_json::json!({"kind": "defect"}))
    }

    fn deviation(rule_id: &str, severity: &str, num: usize, den: Option<usize>) -> FindingView {
        kinded(
            rule_id,
            severity,
            serde_json::json!({
                "kind": "deviation", "deviating_sites": num, "assessed_sites": den,
                "convention_option_id": "opt", "convention_label": "A convention"
            }),
        )
    }

    fn convention(rule_id: &str) -> FindingView {
        kinded(
            rule_id,
            "info",
            serde_json::json!({
                "kind": "convention_observed", "convention_option_id": "opt",
                "convention_label": "A convention"
            }),
        )
    }

    fn no_convention(rule_id: &str) -> FindingView {
        kinded(
            rule_id,
            "info",
            serde_json::json!({"kind": "no_convention", "observed_options": []}),
        )
    }

    fn not_assessed(rule_id: &str) -> FindingView {
        kinded(
            rule_id,
            "info",
            serde_json::json!({
                "kind": "not_assessed", "reason": "tool_failed", "reason_detail": "eslint exited 2"
            }),
        )
    }

    fn unspecified(rule_id: &str, severity: &str) -> FindingView {
        kinded(rule_id, severity, serde_json::json!({}))
    }

    /// req_pass2_1: a legacy row with no `kind` key still deserializes (back-compat) and lands
    /// in the explicit `"unspecified"` alarm state — NEVER silently as a `Defect`.
    #[test]
    fn req_pass2_1_absent_kind_is_the_unspecified_alarm_never_a_defect() {
        let f = unspecified("LEGACY-1", "high");
        assert!(f.kind.is_none());
        assert!(is_unspecified_kind(&f));
        assert_eq!(kind_tag(&f), "unspecified");
        assert_ne!(kind_tag(&f), "defect");
        assert_eq!(unspecified_kind_count(&[f, defect("D", "high")]), 1);
    }

    /// req_pass2_1: every kind's tag reaches the badge verbatim from api-types, so the badge
    /// text can never drift from the wire discriminator.
    #[test]
    fn req_pass2_1_kind_tag_matches_the_wire_discriminator_for_every_kind() {
        assert_eq!(kind_tag(&defect("D", "high")), "defect");
        assert_eq!(
            kind_tag(&deviation("V", "medium", 2, Some(47))),
            "deviation"
        );
        assert_eq!(kind_tag(&convention("C")), "convention_observed");
        assert_eq!(kind_tag(&no_convention("N")), "no_convention");
        assert_eq!(kind_tag(&not_assessed("A")), "not_assessed");
    }

    /// req_pass2_2: each of the five kinds routes to exactly one surface, and the three
    /// informational kinds route OUT of the Queue.
    #[test]
    fn req_pass2_2_every_kind_routes_to_exactly_one_surface() {
        assert_eq!(kind_surface(&defect("D", "high")), KindSurface::Queue);
        assert_eq!(
            kind_surface(&deviation("V", "medium", 2, Some(47))),
            KindSurface::Queue
        );
        assert_eq!(kind_surface(&convention("C")), KindSurface::Conventions);
        assert_eq!(kind_surface(&no_convention("N")), KindSurface::NoConvention);
        assert_eq!(kind_surface(&not_assessed("A")), KindSurface::NotAssessed);
        // The alarm row stays where the curator is looking, flagged — not hidden in a list.
        assert_eq!(kind_surface(&unspecified("U", "high")), KindSurface::Queue);
    }

    /// req_pass2_2: the three informational panels each get their own rows and nothing else,
    /// and the four surface counts partition the input exactly (no row lost, none duplicated).
    #[test]
    fn req_pass2_2_surfaces_partition_the_finding_set() {
        let all = vec![
            defect("D", "high"),
            deviation("V", "medium", 2, Some(47)),
            unspecified("U", "low"),
            convention("C1"),
            convention("C2"),
            no_convention("N"),
            not_assessed("A"),
        ];
        assert_eq!(findings_on_surface(&all, KindSurface::Queue).len(), 3);
        let conventions = findings_on_surface(&all, KindSurface::Conventions);
        assert_eq!(
            conventions
                .iter()
                .map(|f| f.rule_id.clone())
                .collect::<Vec<_>>(),
            vec!["C1", "C2"]
        );
        assert_eq!(
            findings_on_surface(&all, KindSurface::NoConvention).len(),
            1
        );
        assert_eq!(findings_on_surface(&all, KindSurface::NotAssessed).len(), 1);

        let counts = surface_counts(&all);
        assert_eq!(counts[0], (KindSurface::Queue, 3));
        assert_eq!(counts[1], (KindSurface::Conventions, 2));
        assert_eq!(counts[2], (KindSurface::NoConvention, 1));
        assert_eq!(counts[3], (KindSurface::NotAssessed, 1));
        assert_eq!(
            counts.iter().map(|(_, n)| n).sum::<usize>(),
            all.len(),
            "the four surfaces must partition the set — every row on exactly one"
        );
    }

    /// req_pass2_3: the curated default order — active before suppressed, then severity AS
    /// CALIBRATED, then Defect before Deviation, then ascending deviation ratio.
    #[test]
    fn req_pass2_3_default_sort_is_status_then_severity_then_kind_then_ratio() {
        let crit_defect = defect("Z-DEFECT", "critical");
        let high_defect = defect("A-DEFECT", "high");
        let high_deviation = deviation("A-DEVIATION", "high", 2, Some(47));
        let mut suppressed = defect("SUPPRESSED", "critical");
        suppressed.status = "suppressed-baseline".to_string();

        let m = TriageModel::new();
        let out = m.visible(&[
            suppressed.clone(),
            high_deviation.clone(),
            high_defect.clone(),
            crit_defect.clone(),
        ]);
        assert_eq!(
            out.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>(),
            vec!["Z-DEFECT", "A-DEFECT", "A-DEVIATION", "SUPPRESSED"],
            "active critical, then active high defect, then the equal-severity deviation \
             (kind breaks the tie, not the rule id), then the suppressed row last"
        );
    }

    /// req_pass2_3: at equal status/severity/kind, deviations order by ASCENDING ratio — the
    /// near-unanimous conventions (cheapest decisions) first — and an unknown denominator
    /// sorts after every known one rather than fabricating a position.
    #[test]
    fn req_pass2_3_deviations_order_by_ascending_ratio_unknown_denominator_last() {
        let cheap = deviation("B-CHEAP", "medium", 2, Some(47)); // ~4%
        let costly = deviation("A-COSTLY", "medium", 19, Some(47)); // ~40%
        let unknown = deviation("A-UNKNOWN", "medium", 3, None);
        let m = TriageModel::new();
        let out = m.visible(&[unknown.clone(), costly.clone(), cheap.clone()]);
        assert_eq!(
            out.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>(),
            vec!["B-CHEAP", "A-COSTLY", "A-UNKNOWN"],
            "ascending ratio wins over the alphabetical rule-id tie-break; unknown last"
        );
    }

    /// req_pass2_3: the kind-less ALARM row sorts to the very top of its severity band, ahead
    /// of a Defect. It must not be buried, and it must not share Defect's rank.
    #[test]
    fn req_pass2_3_unspecified_rows_rank_ahead_of_defects_not_as_defects() {
        let m = TriageModel::new();
        let out = m.visible(&[
            defect("A-DEFECT", "high"),
            unspecified("Z-UNMAPPED", "high"),
        ]);
        assert_eq!(
            out.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>(),
            vec!["Z-UNMAPPED", "A-DEFECT"]
        );
    }

    /// req_pass2_3: the UI never re-derives severity. A deviation whose rule authored a HIGH
    /// ceiling ranks in the high band purely because the row says `high` — there is no
    /// kind-based severity adjustment anywhere in the sort. If the UI discounted deviations,
    /// this high deviation would sink below the medium defect; it must not.
    #[test]
    fn req_pass2_3_sort_never_re_derives_severity_from_kind() {
        let m = TriageModel::new();
        let out = m.visible(&[
            defect("A-MEDIUM-DEFECT", "medium"),
            deviation("Z-HIGH-DEVIATION", "high", 19, Some(20)),
        ]);
        assert_eq!(
            out[0].rule_id, "Z-HIGH-DEVIATION",
            "the authored severity ceiling/floor is the only calibration layer; a high \
             deviation outranks a medium defect"
        );
    }

    // ── req_pass2_8: the MANUAL severity cut (design §5, Zach 2026-10-12) ────
    //
    // "Keep sort with [unspecified] first, sure, as long as the table can be manually sorted
    // by me as the user." The default order below is unchanged; these cover the cell value the
    // Severity column hands chorale so a HEADER CLICK walks the calibrated ladder instead of
    // the alphabet. The rank comes from `severity_rank`, the same function the default key
    // uses, which is the whole point.

    /// req_pass2_8: the manual severity cut orders the CALIBRATED ladder, not the alphabet.
    /// Alphabetically `low` precedes `medium`; as sort values it must not.
    #[test]
    fn req_pass2_8_severity_sort_value_orders_the_calibrated_ladder_not_the_alphabet() {
        let v = |s: &str| severity_sort_value(s);
        assert!(v("critical") < v("high"), "critical above high");
        assert!(v("high") < v("medium"), "high above medium");
        assert!(
            v("medium") < v("low"),
            "medium above low — the whole reason the rank is in the cell value: the raw words \
             sort the other way round ({:?} < {:?})",
            "low",
            "medium"
        );
        // The bug being fixed, stated outright: the authored words alone are mis-ordered.
        assert!(
            "low" < "medium",
            "the raw authored words sort low above medium"
        );
    }

    /// req_pass2_8: ascending and descending are exact reverses of ONE ladder — a manual cut
    /// cannot produce an order that is neither (which is what the alphabetical cut did).
    #[test]
    fn req_pass2_8_severity_sort_values_are_a_total_ladder_in_both_directions() {
        let mut asc = severity_sort_values();
        asc.sort();
        assert_eq!(
            asc,
            severity_sort_values(),
            "the option list is already in ascending sort order"
        );
        let mut desc = severity_sort_values();
        desc.sort();
        desc.reverse();
        let mut expected = severity_sort_values();
        expected.reverse();
        assert_eq!(desc, expected, "descending is the exact reverse");
    }

    /// req_pass2_8: the sort value carries the AUTHORED severity verbatim and adds only its
    /// ladder position. Nothing is renamed, bucketed or dropped — including a severity outside
    /// the calibrated vocabulary, which passes through rather than being silently normalised.
    #[test]
    fn req_pass2_8_severity_sort_value_carries_the_authored_word_verbatim() {
        for s in CALIBRATED_SEVERITIES {
            assert!(
                severity_sort_value(s).ends_with(s),
                "{s}: the authored word must survive into the cell value"
            );
        }
        assert!(
            severity_sort_value("blocker").ends_with("blocker"),
            "an off-vocabulary severity passes through rather than being renamed"
        );
    }

    /// req_pass2_8: the manual cut never re-derives severity from kind. A Deviation and a
    /// Defect that authored the SAME severity produce byte-identical sort values, so no header
    /// click can introduce the kind-based discount that
    /// `req_pass2_3_sort_never_re_derives_severity_from_kind` forbids in the default order.
    #[test]
    fn req_pass2_8_severity_sort_value_never_re_derives_severity_from_kind() {
        let d = defect("A-DEFECT", "high");
        let dev = deviation("Z-DEVIATION", "high", 19, Some(20));
        assert_eq!(
            severity_sort_value(&d.severity),
            severity_sort_value(&dev.severity),
            "kind is not an input to the severity sort — a high deviation sorts in the high \
             band exactly like a high defect"
        );
    }

    /// req_pass2_8 (anti-drift): the manual cut and the default order read ONE rank table.
    /// For every ordered pair of calibrated severities, the sort-value comparison and
    /// `severity_rank` must agree — if someone re-tabulated the ladder in one place only, this
    /// is what catches it.
    #[test]
    fn req_pass2_8_severity_sort_value_agrees_with_the_default_keys_rank() {
        for a in CALIBRATED_SEVERITIES {
            for b in CALIBRATED_SEVERITIES {
                assert_eq!(
                    severity_sort_value(a).cmp(&severity_sort_value(b)),
                    severity_rank(a).cmp(&severity_rank(b)),
                    "{a} vs {b}: the manual cut must order exactly as the default key's \
                     severity term does"
                );
            }
        }
    }

    /// req_pass2_8: an off-vocabulary severity takes the SAME position in the manual cut that
    /// it takes in the default key (`severity_rank`'s catch-all shares `low`'s rank), rather
    /// than inventing a position for it. Same honesty as the unknown deviation denominator.
    #[test]
    fn req_pass2_8_off_vocabulary_severity_shares_lows_rank_in_both_orders() {
        assert_eq!(severity_rank("blocker"), severity_rank("low"));
        let (unknown, low) = (severity_sort_value("blocker"), severity_sort_value("low"));
        assert_eq!(
            unknown.split(' ').next(),
            low.split(' ').next(),
            "same ladder position as low, exactly as in the default key"
        );
        assert!(
            severity_sort_value("medium") < unknown,
            "and it still sits below every calibrated severity above low"
        );
    }

    /// req_pass2_4: the one-rule-one-surface invariant. A healthy scan reports nothing; a rule
    /// that reaches two surfaces is reported verbatim so the UI can surface the PIPELINE bug
    /// loudly instead of silently picking a winner.
    #[test]
    fn req_pass2_4_one_rule_one_surface_invariant_is_asserted_not_tolerated() {
        let healthy = vec![
            defect("D", "high"),
            deviation("V", "medium", 2, Some(47)),
            convention("C"),
            not_assessed("A"),
        ];
        assert!(
            rules_on_multiple_surfaces(&healthy).is_empty(),
            "a healthy scan must not ring the alarm"
        );

        // The same rule emitting BOTH a convention row and deviating sites: impossible by the
        // pipeline's own contract, therefore a bug the UI must name.
        let broken = vec![
            convention("API-VERSIONING-1"),
            deviation("API-VERSIONING-1", "medium", 2, Some(47)),
        ];
        let violations = rules_on_multiple_surfaces(&broken);
        assert_eq!(violations.len(), 1, "the conflicting rule must be reported");
        assert_eq!(violations[0].0 .1, "API-VERSIONING-1");
        assert_eq!(
            violations[0].1,
            vec![KindSurface::Queue, KindSurface::Conventions]
        );
    }

    /// req_pass2_4: the invariant is scoped per (repo, rule) ON PURPOSE. One repo following a
    /// convention while another deviates from it is two honest assessments of two codebases,
    /// not a conflict — a rule-id-only key would ring the alarm on every legitimate multi-repo
    /// scan and train the curator to ignore it.
    #[test]
    fn req_pass2_4_invariant_is_scoped_per_repo_so_multi_repo_scans_do_not_false_alarm() {
        let mut clean_repo = convention("API-VERSIONING-1");
        clean_repo.repo = "me/api".to_string();
        let mut drifting_repo = deviation("API-VERSIONING-1", "medium", 2, Some(47));
        drifting_repo.repo = "me/worker".to_string();
        assert!(
            rules_on_multiple_surfaces(&[clean_repo, drifting_repo]).is_empty(),
            "same rule, different repos, different outcomes — that is not a pipeline bug"
        );
    }

    /// req_pass2_2: the disposition tally is taken over QUEUE rows only, so informational rows
    /// can never park themselves as Unresolved and block the Process gate forever.
    #[test]
    fn req_pass2_2_informational_rows_cannot_block_the_process_gate() {
        let m = TriageModel::new();
        let all = vec![convention("C"), no_convention("N"), not_assessed("A")];
        let queue = findings_on_surface(&all, KindSurface::Queue);
        assert!(queue.is_empty());
        // EDITED for Pass 2 step 3 (exception (a)): `counts` returns `TriageCounts` rather
        // than a 4-tuple. The assertion is the same one, now stated through the gate's own
        // reading of the tally instead of a destructured position.
        assert_eq!(
            m.counts(&queue).outstanding(),
            0,
            "a scan whose only rows are informational is fully triaged on arrival"
        );
    }

    // ── Pass 2 step 2: the deviation sentence + rule-level bulk ──────────────

    /// A deviation row with an explicit convention label (the `deviation` helper above fixes
    /// the label at "A convention"; these cases need to vary it).
    fn deviation_labelled(
        rule_id: &str,
        num: usize,
        den: Option<usize>,
        label: &str,
    ) -> FindingView {
        kinded(
            rule_id,
            "medium",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": num, "assessed_sites": den,
                "convention_option_id": "opt", "convention_label": label
            }),
        )
    }

    /// req_pass2_9: the sentence is assembled from the three STRUCTURED fields.
    ///
    /// The canonical form the owner asked for, verbatim: numerator, denominator, and the
    /// convention named from `convention_label` rather than recovered from `detail`.
    #[test]
    fn req_pass2_9_deviation_sentence_is_built_from_structured_fields() {
        let f = deviation_labelled("API-VERSIONING-1", 2, Some(47), "URI-versioned endpoints");
        let h = deviation_headline(&f).expect("a deviation row has a headline");
        assert_eq!(h.deviating_sites, 2);
        assert_eq!(h.assessed_sites, Some(47));
        assert_eq!(h.convention_label, "URI-versioned endpoints");
        assert_eq!(
            h.sentence(),
            "2 of 47 sites deviate from \"URI-versioned endpoints\""
        );
        assert_eq!(
            rule_group_label(&f),
            "API-VERSIONING-1 \u{b7} 2 of 47 sites deviate from \"URI-versioned endpoints\"",
            "the group header line is the rule id plus its own claim"
        );
    }

    /// req_pass2_9 (THE HONESTY CASE): `assessed_sites: None` renders the NUMERATOR ALONE.
    ///
    /// `None` is the producer saying it could not count the total honestly. The two ways to
    /// get this wrong are both silent: fabricate a denominator, or let `unwrap_or(0)` print
    /// "3 of 0". Neither may ever appear, so both are asserted against explicitly.
    #[test]
    fn req_pass2_9_absent_denominator_renders_the_numerator_alone() {
        let f = deviation_labelled("DRIFT-1", 3, None, "snake_case table names");
        let h = deviation_headline(&f).expect("a deviation row has a headline");
        assert_eq!(h.assessed_sites, None);
        assert_eq!(
            h.sentence(),
            "3 sites deviate from \"snake_case table names\"",
            "numerator alone — no denominator was established"
        );
        let s = h.sentence();
        assert!(!s.contains(" of "), "no fabricated denominator in `{s}`");
        assert!(!s.contains('0'), "and no printed zero in `{s}`");
    }

    /// req_pass2_9: a ZERO denominator is treated exactly like an absent one.
    ///
    /// `Some(0)` with deviating sites is a producer contradiction; "2 of 0 sites deviate" is
    /// not a sentence to put in front of a curator, and the brief forbids printing the zero.
    /// This is the same rule `deviation_ratio_ppm` already applies when it refuses to place
    /// such a row in the cheapness ordering — asserted together so the two cannot drift.
    #[test]
    fn req_pass2_9_zero_denominator_is_treated_as_absent() {
        let f = deviation_labelled("DRIFT-2", 2, Some(0), "a convention");
        let s = deviation_headline(&f).expect("headline").sentence();
        assert_eq!(s, "2 sites deviate from \"a convention\"");
        assert!(!s.contains(" of "), "no `of 0` clause in `{s}`");
        assert_eq!(
            deviation_ratio_ppm(&f),
            u64::MAX,
            "and the default sort declines to place it, by the same rule"
        );
    }

    /// req_pass2_9: an UNNAMED convention is stated generically, never as `from ""`.
    #[test]
    fn req_pass2_9_unnamed_convention_is_not_rendered_as_empty_quotes() {
        let f = deviation_labelled("DRIFT-3", 2, Some(9), "");
        let s = deviation_headline(&f).expect("headline").sentence();
        assert_eq!(s, "2 of 9 sites deviate from the observed convention");
        assert!(!s.contains("\"\""), "no empty quotes in `{s}`");
    }

    /// req_pass2_9: the verb agrees with a single deviating site, in both denominator cases.
    #[test]
    fn req_pass2_9_single_deviating_site_reads_as_one() {
        assert_eq!(
            deviation_headline(&deviation_labelled("R", 1, Some(47), "L"))
                .expect("headline")
                .sentence(),
            "1 of 47 sites deviates from \"L\""
        );
        assert_eq!(
            deviation_headline(&deviation_labelled("R", 1, None, "L"))
                .expect("headline")
                .sentence(),
            "1 site deviates from \"L\""
        );
    }

    /// req_pass2_9: a DEFECT (or kind-less) row has no headline and keeps a bare group label.
    ///
    /// This is what keeps defect group headers plain, and it is also the guard that the
    /// sentence can never be synthesized for a row that does not carry the numbers.
    #[test]
    fn req_pass2_9_non_deviation_rows_have_no_sentence() {
        for f in [
            defect("SQL-INJECTION-1", "critical"),
            kinded("LEGACY-1", "high", serde_json::json!({})),
            convention("ALREADY-GOOD-1"),
        ] {
            assert!(
                deviation_headline(&f).is_none(),
                "{} has no headline",
                f.rule_id
            );
            assert_eq!(
                rule_group_label(&f),
                f.rule_id,
                "{}'s group header is the bare rule id",
                f.rule_id
            );
        }
    }

    /// req_pass2_9: deviation groups partition on exactly the key chorale groups on, in view
    /// order, and carry every row a bulk action would touch.
    #[test]
    fn req_pass2_9_deviation_groups_match_the_group_header_partition() {
        let mut a1 = deviation_labelled("API-VERSIONING-1", 2, Some(47), "URI-versioned");
        a1.path = "src/routes.rs".into();
        a1.line = 12;
        let mut a2 = a1.clone();
        a2.line = 40;
        let b = deviation_labelled("NAMING-1", 5, None, "snake_case");
        let groups = deviation_groups(&[a1.clone(), defect("D", "high"), a2.clone(), b.clone()]);

        assert_eq!(
            groups.len(),
            2,
            "one group per deviation rule; the defect is not one"
        );
        assert_eq!(groups[0].rule_id, "API-VERSIONING-1");
        assert_eq!(groups[0].row_count(), 2);
        assert_eq!(groups[0].label, rule_group_label(&a1));
        assert_eq!(
            groups[0].rows.iter().map(|f| f.line).collect::<Vec<_>>(),
            vec![12, 40],
            "rows in view order"
        );
        assert_eq!(groups[1].rule_id, "NAMING-1");
        assert_eq!(groups[1].label, rule_group_label(&b));

        // The button's count is the ROW count, which is allowed to differ from the site count
        // in the sentence — merged sites and already-dispositioned rows are both real reasons.
        assert_eq!(groups[1].row_count(), 1);
        assert_eq!(groups[1].headline.deviating_sites, 5);
    }

    /// req_pass2_9 (THE BULK EQUIVALENCE): `FP all N` on a group leaves EXACTLY the state that
    /// marking its N rows one at a time leaves.
    ///
    /// This is the whole claim of a bulk action. If it ever stopped holding — a row skipped, a
    /// reason not recorded on every row, a key computed differently in the bulk path — the
    /// curator would have a one-click gesture that quietly means something other than the N
    /// gestures it replaces, and the first evidence would be a report with a hole in it.
    #[test]
    fn req_pass2_9_bulk_false_positive_equals_marking_each_row_individually() {
        let mut a1 = deviation_labelled("API-VERSIONING-1", 3, Some(47), "URI-versioned");
        a1.path = "src/routes.rs".into();
        a1.line = 12;
        let mut a2 = a1.clone();
        a2.line = 40;
        let mut a3 = a1.clone();
        a3.path = "src/admin.rs".into();
        let other = defect("SQL-INJECTION-1", "critical");
        let all = vec![a1, a2, a3, other];
        let group = deviation_groups(&all).remove(0);
        assert_eq!(
            group.row_count(),
            3,
            "three distinct rows, no key collision"
        );

        // One act on the group — through the SHIPPED entry point, not a hand-rolled loop, so
        // this proves the product's own path rather than a convenient stand-in for it.
        let mut bulk = TriageModel::new();
        assert_eq!(
            bulk.bulk_false_positive(&group, "the linter mis-parsed the router macro"),
            Some(3),
            "the bulk call reports exactly the rows it moved"
        );

        // The same thing, one row at a time.
        let mut by_row = TriageModel::new();
        for row in &group.rows {
            by_row.mark_false_positive(
                std::slice::from_ref(row),
                "the linter mis-parsed the router macro",
            );
        }

        assert_eq!(
            bulk.dispositions.len(),
            by_row.dispositions.len(),
            "same number of dispositions written"
        );
        for (key, d) in &by_row.dispositions {
            let other = bulk.dispositions.get(key).expect("bulk wrote the same key");
            assert_eq!(other.state, d.state);
            assert_eq!(other.reason, d.reason);
            // `TechDebtBucket` is deliberately not `Debug` (it is a two-state flag on a serde
            // DTO), so this is an `assert!` rather than an `assert_eq!`.
            assert!(other.bucket == d.bucket, "bucket differs for {key}");
        }
        // And nothing outside the group moved.
        for f in &all {
            let expected = if group.rows.iter().any(|r| finding_key(r) == finding_key(f)) {
                TriageState::FalsePositive
            } else {
                TriageState::Unresolved
            };
            assert_eq!(bulk.state_of(f), expected, "{} moved correctly", f.rule_id);
        }
    }

    /// req_pass2_9: the bulk action REFUSES a blank reason and writes nothing.
    ///
    /// Same invariant as the per-row false positive, enforced at the same point. The failure
    /// this guards against is not a wrong reason but an absent one on N rows at once: the
    /// report's methodology section counts false positives, and a silent reasonless bulk
    /// would leave N exclusions with no audit trail behind them.
    #[test]
    fn req_pass2_9_bulk_false_positive_refuses_a_blank_reason() {
        let f = deviation_labelled("API-VERSIONING-1", 2, Some(47), "URI-versioned");
        let group = deviation_groups(std::slice::from_ref(&f)).remove(0);
        let mut m = TriageModel::new();
        for blank in ["", "   ", "\n\t "] {
            assert_eq!(
                m.bulk_false_positive(&group, blank),
                None,
                "refused: {blank:?}"
            );
            assert!(m.dispositions.is_empty(), "and nothing was written");
            assert_eq!(m.state_of(&f), TriageState::Unresolved);
        }
        assert_eq!(m.bulk_false_positive(&group, "tool mis-parse"), Some(1));
        assert_eq!(m.state_of(&f), TriageState::FalsePositive);
    }

    /// req_pass2_9: a bulk call over an EMPTY group moves nothing and says so.
    #[test]
    fn req_pass2_9_bulk_false_positive_on_an_empty_group_is_a_no_op() {
        let mut m = TriageModel::new();
        let empty = DeviationGroup {
            label: "R".to_string(),
            rule_id: "R".to_string(),
            headline: DeviationHeadline {
                deviating_sites: 0,
                assessed_sites: None,
                convention_label: "L".to_string(),
            },
            rows: Vec::new(),
        };
        assert_eq!(m.bulk_false_positive(&empty, "a real reason"), None);
        assert!(m.dispositions.is_empty());
    }

    // ── Pass 2 step 3: the `Confirmed` disposition ──────────────────────────
    //
    // RED BEFORE THE FIX, on `1a61a7c`: none of this compiled, because neither
    // `TriageState::Confirmed` nor `TriageModel::confirm` existed. The defect those absences
    // ARE is stated in `req_pass2_10_confirmed_is_the_only_affirmative_disposition` below.
    // The export half of the same defect — a `Confirmed` row arriving at the exporter and
    // being indistinguishable from an unopened one — is red as a genuine assertion failure,
    // recorded in
    // `camerata_server::report_export::tests::req_pass2_10_a_confirmed_finding_is_distinguishable_from_an_unopened_one_in_the_export`.

    /// req_pass2_10 (THE DEFECT): `Confirmed` is the ONE disposition that both decides a row
    /// AND leaves it shipping as a live finding.
    ///
    /// Every other exit from `Unresolved` is a way of NOT shipping:
    ///
    /// * `Ignored` — real, but accepted as risk. Baselined; out of the action tiers.
    /// * `TechDebt` — real, but deferred. Leaves as a ticket.
    /// * `FalsePositive` — not real. Excluded from the report entirely.
    ///
    /// So before this variant the only way to keep a finding LIVE was to leave it
    /// `Unresolved` — i.e. to not decide it — which made `Unresolved` mean both "never opened"
    /// and "read, confirmed, shipping" at the same time. That is the whole defect, and it is
    /// why the sign-off promise ("one person reads every finding and signs") had no data
    /// behind it.
    #[test]
    fn req_pass2_10_confirmed_is_the_only_affirmative_disposition() {
        // The whole vocabulary, and which members are a DECISION.
        assert_eq!(TriageState::all().len(), 5);
        assert!(!TriageState::Unresolved.is_decided());
        for decided in [
            TriageState::Confirmed,
            TriageState::Ignored,
            TriageState::TechDebt,
            TriageState::FalsePositive,
        ] {
            assert!(decided.is_decided(), "{} is a decision", decided.label());
        }
        // And `Confirmed` is its own state, never an alias for one of the four that preceded
        // it — which is exactly what would have happened had this shipped as "just leave it
        // Unresolved" or "Ignore it, we know what we mean".
        for other in [
            TriageState::Unresolved,
            TriageState::Ignored,
            TriageState::TechDebt,
            TriageState::FalsePositive,
        ] {
            assert_ne!(TriageState::Confirmed, other);
        }
        assert_eq!(TriageState::Confirmed.label(), "Confirmed");
    }

    /// req_pass2_10: a finding the curator READ AND CONFIRMED is distinguishable from one he
    /// never opened — the single sentence the whole step exists to make true.
    #[test]
    fn req_pass2_10_a_reviewed_and_real_finding_is_distinguishable_from_an_unopened_one() {
        let never_opened = defect("NEVER-OPENED-1", "critical");
        let read_and_real = defect("READ-AND-REAL-1", "critical");

        let mut m = TriageModel::new();
        m.confirm(std::slice::from_ref(&read_and_real));

        assert_eq!(m.state_of(&read_and_real), TriageState::Confirmed);
        assert_eq!(m.state_of(&never_opened), TriageState::Unresolved);
        assert_ne!(
            m.state_of(&read_and_real),
            m.state_of(&never_opened),
            "a finding that was read and judged real must not record the same state as one \
             nobody opened"
        );

        let counts = m.counts(&[never_opened, read_and_real]);
        assert_eq!(counts.confirmed, 1);
        assert_eq!(counts.unresolved, 1);
    }

    /// req_pass2_10: confirming requires NO reason, and the model's own API says so — there is
    /// no parameter to pass one through. The contrast with `ignore` / `mark_false_positive`
    /// is deliberate (see [`TriageModel::confirm`]'s doc comment); this pins it so a later
    /// "consistency" refactor cannot quietly tax the majority gesture of every audit.
    #[test]
    fn req_pass2_10_confirming_needs_no_reason_and_records_none() {
        let mut m = TriageModel::new();
        let item = defect("SQL-INJECTION-1", "critical");
        m.confirm(std::slice::from_ref(&item));
        let d = m.dispositions.get(&finding_key(&item)).expect("recorded");
        assert_eq!(d.state, TriageState::Confirmed);
        assert_eq!(
            d.reason, "",
            "no reason was asked for and none was invented"
        );
    }

    /// req_pass2_10: `Confirmed` gets its own triage table, exactly like the other four.
    #[test]
    fn req_pass2_10_visible_filters_confirmed_into_its_own_view() {
        let mut m = TriageModel::new();
        let unresolved = defect("A-OPEN-1", "high");
        let confirmed = defect("Z-REAL-1", "critical");
        m.confirm(std::slice::from_ref(&confirmed));
        let all = vec![unresolved.clone(), confirmed.clone()];

        let vis = m.visible(&all);
        assert_eq!(
            vis.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>(),
            vec!["A-OPEN-1"],
            "the Unresolved table no longer holds the confirmed row"
        );

        m.triage_view = TriageState::Confirmed;
        let vis = m.visible(&all);
        assert_eq!(
            vis.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>(),
            vec!["Z-REAL-1"]
        );
    }

    /// req_pass2_10 (THE PROCESS GATE): a confirmed finding is DECIDED, so it does not block
    /// Process — and confirming commits nothing, so it does not unblock it either.
    ///
    /// Both halves matter. The first is the step's requirement. The second is the trap next to
    /// it: `Confirmed` is client-local (owner's Q2), so a queue made entirely of confirmed
    /// rows is fully triaged AND has nothing for Process to commit. Enabling the button there
    /// would be a button that does nothing, which is the dead-affordance shape this product
    /// forbids.
    #[test]
    fn req_pass2_10_confirmed_rows_do_not_block_the_process_gate_and_do_not_unblock_it() {
        let a = defect("SQL-INJECTION-1", "critical");
        let b = defect("NO-RAW-SQL-1", "high");
        let all = vec![a.clone(), b.clone()];

        // Nothing touched: outstanding, and nothing to commit.
        let m = TriageModel::new();
        assert_eq!(m.counts(&all).outstanding(), 2);
        assert!(!m.counts(&all).can_process());

        // Both confirmed: NOTHING outstanding (the requirement) ...
        let mut m = TriageModel::new();
        m.confirm(&all);
        let counts = m.counts(&all);
        assert_eq!(
            counts.outstanding(),
            0,
            "a confirmed finding is decided, not outstanding — the gate must not block on it"
        );
        // ... but still nothing to Process, because Confirmed commits nothing server-side.
        assert!(!counts.has_committable_work());
        assert!(!counts.can_process());

        // One confirmed, one ignored: still nothing outstanding, and now there IS something
        // to commit, so the gate opens.
        let mut m = TriageModel::new();
        m.confirm(std::slice::from_ref(&a));
        m.ignore(std::slice::from_ref(&b), "defense in depth elsewhere");
        let counts = m.counts(&all);
        assert_eq!(counts.outstanding(), 0);
        assert!(counts.can_process());

        // One left unresolved: blocked again, by that row alone.
        let mut m = TriageModel::new();
        m.ignore(std::slice::from_ref(&b), "defense in depth elsewhere");
        let counts = m.counts(&all);
        assert_eq!(counts.outstanding(), 1);
        assert!(!counts.can_process());
    }

    /// req_pass2_10 (REGRESSION GUARD for step 1's live bug): informational rows still cannot
    /// block the gate, now read through the gate's own `outstanding()` and with a `Confirmed`
    /// row in the mix.
    ///
    /// Step 1 fixed a shipped bug where a row saying "your codebase does this correctly" sat
    /// `Unresolved` forever and disabled Process permanently. Adding a fifth disposition
    /// rewrote the tally, which is exactly the kind of edit that silently reinstates it.
    #[test]
    fn req_pass2_10_informational_rows_still_cannot_block_the_gate_after_the_fifth_disposition() {
        let real = defect("SQL-INJECTION-1", "critical");
        let all = vec![
            real.clone(),
            convention("ALREADY-GOOD-1"),
            no_convention("SPLIT-1"),
            not_assessed("UNRUN-1"),
        ];
        let queue = findings_on_surface(&all, KindSurface::Queue);
        assert_eq!(queue.len(), 1, "only the defect is a Queue row");

        let mut m = TriageModel::new();
        m.confirm(std::slice::from_ref(&real));
        assert_eq!(
            m.counts(&queue).outstanding(),
            0,
            "every Queue row is decided, so nothing is outstanding"
        );
        // And the tally is taken over the QUEUE, never the raw set — the actual mechanism of
        // the original bug. Over the raw set the three informational rows would still read as
        // Unresolved, which is why the call site must never pass it.
        assert_eq!(
            m.counts(&all).outstanding(),
            3,
            "the raw set still contains three undisposable informational rows; the gate must \
             never be fed this"
        );
    }

    /// req_pass2_10 (THE BULK EQUIVALENCE): `Confirm all N` on a deviation group leaves
    /// EXACTLY the state that confirming its N rows one at a time leaves.
    ///
    /// The same claim step 2 proved for `False positive all N`, and for the same reason: a
    /// one-click gesture that quietly means something other than the N gestures it replaces
    /// would first show up as a report with a hole in it.
    #[test]
    fn req_pass2_10_bulk_confirm_equals_confirming_each_row_individually() {
        let mut a1 = deviation_labelled("API-VERSIONING-1", 3, Some(47), "URI-versioned");
        a1.path = "src/routes.rs".into();
        a1.line = 12;
        let mut a2 = a1.clone();
        a2.line = 40;
        let mut a3 = a1.clone();
        a3.path = "src/admin.rs".into();
        let other = defect("SQL-INJECTION-1", "critical");
        let all = vec![a1, a2, a3, other];
        let group = deviation_groups(&all).remove(0);
        assert_eq!(
            group.row_count(),
            3,
            "three distinct rows, no key collision"
        );

        // One act on the group, through the SHIPPED entry point.
        let mut bulk = TriageModel::new();
        assert_eq!(
            bulk.bulk_confirm(&group),
            Some(3),
            "the bulk call reports exactly the rows it moved"
        );

        // The same thing, one row at a time.
        let mut by_row = TriageModel::new();
        for row in &group.rows {
            by_row.confirm(std::slice::from_ref(row));
        }

        assert_eq!(
            bulk.dispositions.len(),
            by_row.dispositions.len(),
            "same number of dispositions written"
        );
        for (key, d) in &by_row.dispositions {
            let got = bulk.dispositions.get(key).expect("bulk wrote the same key");
            assert_eq!(got.state, d.state);
            assert_eq!(got.reason, d.reason);
            // `TechDebtBucket` is deliberately not `Debug`, so this is `assert!`.
            assert!(got.bucket == d.bucket, "bucket differs for {key}");
        }
        // And nothing outside the group moved.
        for f in &all {
            let expected = if group.rows.iter().any(|r| finding_key(r) == finding_key(f)) {
                TriageState::Confirmed
            } else {
                TriageState::Unresolved
            };
            assert_eq!(bulk.state_of(f), expected, "{} moved correctly", f.rule_id);
        }
    }

    /// req_pass2_10: `Confirm all N` needs NO reason, where `False positive all N` refuses
    /// without one. Asserted side by side so the asymmetry is visibly intentional.
    #[test]
    fn req_pass2_10_bulk_confirm_needs_no_reason_where_bulk_false_positive_demands_one() {
        let f = deviation_labelled("API-VERSIONING-1", 2, Some(47), "URI-versioned");
        let group = deviation_groups(std::slice::from_ref(&f)).remove(0);

        let mut confirming = TriageModel::new();
        assert_eq!(confirming.bulk_confirm(&group), Some(1));
        assert_eq!(confirming.state_of(&f), TriageState::Confirmed);

        let mut fp = TriageModel::new();
        assert_eq!(fp.bulk_false_positive(&group, "   "), None);
        assert!(fp.dispositions.is_empty());
    }

    /// req_pass2_10: a bulk confirm over an EMPTY group moves nothing and says so.
    #[test]
    fn req_pass2_10_bulk_confirm_on_an_empty_group_is_a_no_op() {
        let mut m = TriageModel::new();
        let empty = DeviationGroup {
            label: "R".to_string(),
            rule_id: "R".to_string(),
            headline: DeviationHeadline {
                deviating_sites: 0,
                assessed_sites: None,
                convention_label: "L".to_string(),
            },
            rows: Vec::new(),
        };
        assert_eq!(m.bulk_confirm(&empty), None);
        assert!(m.dispositions.is_empty());
    }

    /// req_pass2_10: `Confirmed` round-trips on the wire as the literal `"Confirmed"` the
    /// server's `classify` matches on.
    ///
    /// This is the seam between the two crates — the UI writes this value into the
    /// auto-saved draft's disposition map, the export reads it back out of the same map, and
    /// they cannot share the type. A rename here with no rename there is exactly how a
    /// disposition ships inert, so the literal is pinned on both sides.
    #[test]
    fn req_pass2_10_confirmed_serializes_as_the_wire_value_the_export_matches_on() {
        let d = Disposition {
            state: TriageState::Confirmed,
            reason: String::new(),
            bucket: TechDebtBucket::Later,
        };
        let v = serde_json::to_value(&d).expect("Disposition serializes");
        assert_eq!(
            v["state"], "Confirmed",
            "camerata_server::report_export::classify matches this string literally"
        );
        let back: Disposition = serde_json::from_value(v).expect("and deserializes");
        assert_eq!(back.state, TriageState::Confirmed);
    }

    /// req_pass2_10: a draft written BEFORE this step still loads. Old drafts contain only the
    /// four prior states, and `Unresolved` remains the default for anything absent.
    #[test]
    fn req_pass2_10_pre_existing_drafts_still_deserialize() {
        for legacy in ["Unresolved", "Ignored", "TechDebt", "FalsePositive"] {
            let d: Disposition = serde_json::from_value(serde_json::json!({
                "state": legacy, "reason": "", "bucket": "Later"
            }))
            .unwrap_or_else(|e| panic!("legacy state {legacy} must still load: {e}"));
            assert_ne!(d.state, TriageState::Confirmed);
        }
        assert_eq!(TriageState::default(), TriageState::Unresolved);
    }

    // ── req_pass2_11: the docked detail pane (design §2/§4, §6; Q5 ruling §13) ──────────
    //
    // RED BEFORE THE FIX, all of them. The absorbed-sites group fails at RUNTIME on
    // `d70d669` (`FindingView` silently drops `also_locations`, so a merged row's other
    // sites reach the curator's detail surface nowhere); the cursor, scoping and undo
    // groups fail to COMPILE, because none of that API exists yet.

    /// req_pass2_11: a merged row's ABSORBED evidence sites survive the trip into the view
    /// model. `camerata_server::onboard::Finding::also_locations` is the field W7 added to
    /// close C5 ("an absorbed site reaching NO artifact"); the UI dropping it on the floor
    /// re-opens exactly that hole on the one surface the curator actually reads.
    #[test]
    fn req_pass2_11_finding_view_mirrors_absorbed_sites_from_the_wire() {
        let f = finding(serde_json::json!({
            "repo": "me/api", "path": "src/handlers.rs", "line": 88,
            "rule_id": "SQL-INJECTION-1", "severity": "critical", "snippet": "query(fmt!(..))",
            "detail": "", "kind": "defect",
            "also_locations": [
                { "repo": "me/api", "path": "src/export.rs", "line": 31,
                  "rule_id": "SQL-INJECTION-1", "snippet": "query(fmt!(..))",
                  "consequence": false },
                { "repo": "me/api", "path": "docs/threat.md", "line": 4,
                  "rule_id": "DATA-EXFIL-1", "snippet": "customer rows readable",
                  "consequence": true }
            ]
        }));
        assert_eq!(
            f.also_locations.len(),
            2,
            "both absorbed sites must reach the view model"
        );
        assert_eq!(f.also_locations[0].path, "src/export.rs");
        assert_eq!(f.also_locations[0].line, 31);
        assert_eq!(f.also_locations[0].rule_id, "SQL-INJECTION-1");
        assert!(!f.also_locations[0].consequence, "a peer evidence site");
        assert!(
            f.also_locations[1].consequence,
            "a CONSEQUENCE site — the pane subordinates it under \"also affects\""
        );
        // And back out again, the same round-trip contract `kind`/`confidence`/`effort` hold.
        let back = serde_json::to_value(&f).expect("FindingView serializes");
        assert_eq!(back["also_locations"].as_array().map(Vec::len), Some(2));
        assert_eq!(back["also_locations"][1]["rule_id"], "DATA-EXFIL-1");
    }

    /// req_pass2_11: `site_count` is 1 + the absorbed sites, computed here rather than taken
    /// from a new wire field — design §10 chose that deliberately ("one source of truth, no
    /// new wire field").
    #[test]
    fn req_pass2_11_site_count_is_one_plus_the_absorbed_sites() {
        let plain = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1, "rule_id": "R",
            "severity": "high", "snippet": "s", "detail": ""
        }));
        assert_eq!(
            site_count(&plain),
            1,
            "an un-merged row is one site, never zero"
        );
        let merged = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1, "rule_id": "R",
            "severity": "high", "snippet": "s", "detail": "",
            "also_locations": [
                { "repo": "r", "path": "q", "line": 2, "rule_id": "R", "snippet": "s2",
                  "consequence": false },
                { "repo": "r", "path": "t", "line": 3, "rule_id": "R", "snippet": "s3",
                  "consequence": false }
            ]
        }));
        assert_eq!(site_count(&merged), 3);
    }

    /// req_pass2_11: a row from before the field existed still loads, with no sites absorbed.
    #[test]
    fn req_pass2_11_absent_also_locations_defaults_to_empty() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1, "rule_id": "R",
            "severity": "low", "snippet": "s", "detail": ""
        }));
        assert!(f.also_locations.is_empty());
    }

    // ── req_pass2_11: the ROW CURSOR (arrow-key focus; what survives a re-sort) ─────────
    //
    // The cursor walks the keys of the rows chorale is actually showing, in the order it is
    // showing them, so these tests express every coexistence requirement as a change to that
    // list: a re-sort PERMUTES it, a group collapse REMOVES a contiguous run, a disposition
    // REMOVES one row, a filter REMOVES several.

    fn keys(ks: &[&str]) -> Vec<String> {
        ks.iter().map(|s| (*s).to_string()).collect()
    }

    /// req_pass2_11: with nothing focused yet, the cursor lands on the FIRST row, so the pane
    /// is never blank while the table has rows.
    #[test]
    fn req_pass2_11_focus_lands_on_the_first_row_when_nothing_is_focused() {
        let c = RowCursor::new().resolve(&keys(&["a", "b", "c"]));
        assert_eq!(c.key(), Some("a"));
        assert_eq!(c.index(), 0);
    }

    /// req_pass2_11 (THE RE-SORT CASE): focus is anchored to the row's IDENTITY, not to its
    /// position. A manual column re-sort moves the focused row to a different index and the
    /// SAME row stays focused — the pane does not silently start describing whatever row
    /// happens to have landed under the cursor.
    #[test]
    fn req_pass2_11_focus_follows_the_row_itself_across_a_re_sort() {
        let before = keys(&["a", "b", "c", "d"]);
        let c = RowCursor::new()
            .resolve(&before)
            .moved(&before, FocusMove::Down);
        assert_eq!(c.key(), Some("b"), "cursor-down from the first row");
        assert_eq!(c.index(), 1);

        // The curator clicks a column header; chorale hands back the same rows reversed.
        let after = keys(&["d", "c", "b", "a"]);
        let c = c.resolve(&after);
        assert_eq!(c.key(), Some("b"), "the SAME finding is still focused");
        assert_eq!(c.index(), 2, "at its new position in the re-cut order");

        // And the next cursor-down moves to its new neighbour, not to its old one.
        assert_eq!(c.moved(&after, FocusMove::Down).key(), Some("a"));
    }

    /// req_pass2_11 (THE DISPOSITION CASE): when the focused row LEAVES the view — the usual
    /// way, by being dispositioned out of this triage table — focus falls to whatever row
    /// took its index. That is the burn-down gesture: decide the top row, and the next row is
    /// already focused, with no mouse.
    #[test]
    fn req_pass2_11_focus_falls_to_the_row_that_took_its_place() {
        let before = keys(&["a", "b", "c"]);
        let c = RowCursor::new()
            .resolve(&before)
            .moved(&before, FocusMove::Down);
        assert_eq!(c.key(), Some("b"));
        // `b` is confirmed and drops out of the Unresolved table.
        let after = keys(&["a", "c"]);
        let c = c.resolve(&after);
        assert_eq!(c.key(), Some("c"), "the row that took index 1");
        assert_eq!(c.index(), 1);
    }

    /// req_pass2_11 (THE COLLAPSE CASE): collapsing the focused row's group removes a whole
    /// run of rows at once. Focus does not stay on a row nobody can see and does not reset to
    /// the top of the table — it clamps forward to the first row still on screen at or after
    /// the vacated index, which for a collapsed trailing group is the group header's
    /// successor, and for a collapsed tail is the last visible row.
    #[test]
    fn req_pass2_11_collapsing_the_focused_group_moves_focus_to_a_row_still_on_screen() {
        // Two rule groups: RULE-A has three rows, RULE-B has two.
        let expanded = keys(&["a1", "a2", "a3", "b1", "b2"]);
        let c = RowCursor::new()
            .resolve(&expanded)
            .moved(&expanded, FocusMove::Down)
            .moved(&expanded, FocusMove::Down);
        assert_eq!(c.key(), Some("a3"), "focused on the last row of RULE-A");

        // The curator collapses RULE-A. chorale's grouped view drops all three of its rows.
        let collapsed = keys(&["b1", "b2"]);
        let c = c.resolve(&collapsed);
        assert_eq!(
            c.key(),
            Some("b2"),
            "index 2 no longer exists, so focus clamps to the last visible row"
        );
        assert_eq!(c.index(), 1);
        assert!(
            collapsed.iter().any(|k| Some(k.as_str()) == c.key()),
            "focus is ALWAYS on a row that is actually on screen"
        );
    }

    /// req_pass2_11: collapsing every group empties the view, and focus goes to nothing
    /// rather than to a stale key the pane would keep rendering.
    #[test]
    fn req_pass2_11_focus_clears_when_the_view_empties() {
        let before = keys(&["a", "b"]);
        let c = RowCursor::new().resolve(&before);
        assert_eq!(c.key(), Some("a"));
        let c = c.resolve(&[]);
        assert_eq!(c.key(), None, "nothing on screen means nothing focused");
    }

    /// req_pass2_11: and it comes back. Expanding the groups again re-focuses, so the pane
    /// recovers rather than staying blank until the curator clicks a row.
    #[test]
    fn req_pass2_11_focus_recovers_when_rows_come_back() {
        let c = RowCursor::new().resolve(&keys(&["a", "b"])).resolve(&[]);
        assert_eq!(c.key(), None);
        assert_eq!(c.resolve(&keys(&["a", "b"])).key(), Some("a"));
    }

    /// req_pass2_11: the arrow keys walk the list and STOP at both ends. Wrapping would mean
    /// a curator holding cursor-down silently restarts a 400-row queue from the top.
    #[test]
    fn req_pass2_11_arrow_movement_stops_at_both_ends() {
        let ks = keys(&["a", "b", "c"]);
        let top = RowCursor::new().resolve(&ks);
        assert_eq!(
            top.moved(&ks, FocusMove::Up).key(),
            Some("a"),
            "no wrap at the top"
        );
        let bottom = top
            .moved(&ks, FocusMove::Down)
            .moved(&ks, FocusMove::Down)
            .moved(&ks, FocusMove::Down);
        assert_eq!(bottom.key(), Some("c"), "no wrap at the bottom");
    }

    /// req_pass2_11: clicking a row focuses it, so the mouse and the keyboard drive one
    /// cursor rather than two competing ones.
    #[test]
    fn req_pass2_11_clicking_a_row_moves_the_same_cursor() {
        let ks = keys(&["a", "b", "c"]);
        let c = RowCursor::focused_on("c").resolve(&ks);
        assert_eq!(c.key(), Some("c"));
        assert_eq!(c.index(), 2);
        assert_eq!(c.moved(&ks, FocusMove::Up).key(), Some("b"));
    }

    /// req_pass2_11: focusing a key that is not on screen (a stale click, a row filtered away
    /// between the click and the resolve) degrades to the clamp rule rather than leaving the
    /// pane pointed at nothing.
    #[test]
    fn req_pass2_11_focusing_an_off_screen_row_degrades_to_the_clamp() {
        let ks = keys(&["a", "b"]);
        let c = RowCursor::focused_on("zzz").resolve(&ks);
        assert_eq!(c.key(), Some("a"));
    }

    // ── req_pass2_11: KEYSTROKE SCOPING (owner's Q5 ruling, design §13) ────────────────
    //
    // > `C` / `I` / `T` / `F` on the focused row, **scoped so the keys only fire while the
    // > detail pane holds focus**, plus a **five-second undo toast**.
    //
    // The scoping is the requirement, not the keystroke. Every one of these asks whether a
    // press PRODUCES A COMMAND, because that is the only question the live handler asks.

    /// req_pass2_11: with the pane's row cursor focused, each of the four keys produces its
    /// disposition command. Case-insensitive: a capital C from a held shift is still C.
    #[test]
    fn req_pass2_11_the_four_keys_fire_while_the_pane_cursor_holds_focus() {
        let m = KeyMods::none();
        for (key, want) in [
            ("c", PaneCommand::Dispose(TriageState::Confirmed)),
            ("C", PaneCommand::Dispose(TriageState::Confirmed)),
            ("t", PaneCommand::Dispose(TriageState::TechDebt)),
            ("T", PaneCommand::Dispose(TriageState::TechDebt)),
            ("i", PaneCommand::PromptFor(TriageState::Ignored)),
            ("f", PaneCommand::PromptFor(TriageState::FalsePositive)),
        ] {
            assert_eq!(
                pane_command(PaneFocus::Cursor, key, m),
                Some(want),
                "{key} must fire while the pane cursor holds focus"
            );
        }
        assert_eq!(
            pane_command(PaneFocus::Cursor, "ArrowDown", m),
            Some(PaneCommand::MoveFocus(FocusMove::Down))
        );
        assert_eq!(
            pane_command(PaneFocus::Cursor, "ArrowUp", m),
            Some(PaneCommand::MoveFocus(FocusMove::Up))
        );
    }

    /// req_pass2_11 — **THE SCOPING TEST.** A `c` typed into the reason prompt is the letter
    /// c. It is not a Confirm, and it is not any other command either.
    ///
    /// This is the clause's whole reason for existing: the prompt sits inches from the row it
    /// describes, `I` and `F` are the keys that OPEN it, and the very next thing the curator
    /// does is type a sentence into it. An unscoped handler would dispose a row on the first
    /// `c` of "cannot reproduce" — a reasonless Confirm over the Ignore the curator was
    /// mid-way through writing.
    #[test]
    fn req_pass2_11_a_key_typed_into_the_reason_prompt_is_text_not_a_disposition() {
        let m = KeyMods::none();
        for key in [
            "c",
            "C",
            "i",
            "I",
            "t",
            "T",
            "f",
            "F",
            "ArrowDown",
            "ArrowUp",
        ] {
            assert_eq!(
                pane_command(PaneFocus::ReasonPrompt, key, m),
                None,
                "{key} must be TEXT while the reason prompt holds focus, never a command"
            );
        }
        // Every letter of a real reason, start to finish, writes nothing.
        for ch in "cannot reproduce; the fixture is test-only".chars() {
            assert_eq!(
                pane_command(PaneFocus::ReasonPrompt, &ch.to_string(), m),
                None,
                "typing {ch:?} into a reason must not dispose anything"
            );
        }
    }

    /// req_pass2_11: focus anywhere else on the page — the table's own filter boxes, the
    /// ignore textarea in the toolbar, the rescan bar — fires nothing either. "Only while the
    /// detail pane holds focus" is a whitelist, not a blacklist of known-bad inputs.
    #[test]
    fn req_pass2_11_keys_do_nothing_while_focus_is_outside_the_pane() {
        let m = KeyMods::none();
        for key in ["c", "i", "t", "f", "ArrowDown", "ArrowUp"] {
            assert_eq!(
                pane_command(PaneFocus::Elsewhere, key, m),
                None,
                "{key} outside the pane"
            );
        }
    }

    /// req_pass2_11: a MODIFIED press is never a disposition. Cmd-C is copy, Ctrl-C is
    /// interrupt, and both are things a curator does over a snippet he is reading in the
    /// pane. Shift is not a modifier for this purpose — it is how a capital letter is typed.
    #[test]
    fn req_pass2_11_modified_presses_are_never_dispositions() {
        for m in [
            KeyMods {
                ctrl: true,
                ..KeyMods::none()
            },
            KeyMods {
                alt: true,
                ..KeyMods::none()
            },
            KeyMods {
                meta: true,
                ..KeyMods::none()
            },
        ] {
            for key in ["c", "i", "t", "f"] {
                assert_eq!(
                    pane_command(PaneFocus::Cursor, key, m),
                    None,
                    "a modified {key} is a shortcut, not a disposition"
                );
            }
        }
        assert_eq!(
            pane_command(
                PaneFocus::Cursor,
                "c",
                KeyMods {
                    shift: true,
                    ..KeyMods::none()
                }
            ),
            Some(PaneCommand::Dispose(TriageState::Confirmed)),
            "shift is how a capital C is typed, not a modifier that cancels it"
        );
    }

    /// Closing the code viewer refocuses the pane; with the pane's reason prompt still open that
    /// must not re-arm the keys.
    #[test]
    fn an_open_reason_prompt_keeps_the_pane_focus_state_whatever_the_dom_reports() {
        assert_eq!(
            next_pane_focus(PaneFocus::Cursor, true),
            Some(PaneFocus::ReasonPrompt)
        );
        assert_eq!(
            next_pane_focus(PaneFocus::ReasonPrompt, true),
            Some(PaneFocus::ReasonPrompt)
        );
        assert_eq!(next_pane_focus(PaneFocus::Elsewhere, true), None);
        for f in [
            PaneFocus::Cursor,
            PaneFocus::ReasonPrompt,
            PaneFocus::Elsewhere,
        ] {
            assert_eq!(
                next_pane_focus(f, false),
                Some(f),
                "no prompt: the DOM is believed"
            );
        }
        // And the keys stay off in the state it keeps.
        let kept = next_pane_focus(PaneFocus::Cursor, true).unwrap();
        assert!(!pane_keys_armed(kept));
    }

    /// A blank reason is refused even when the row is already in that state, so a caller
    /// cannot infer "it landed" from the disposition map alone.
    #[test]
    fn a_blank_reason_is_refused_even_for_a_row_already_in_that_state() {
        let f: FindingView = serde_json::from_value(serde_json::json!({
            "repo": "o/r", "path": "a.rs", "line": 1, "rule_id": "R", "severity": "high",
            "snippet": "s", "detail": "d"
        }))
        .unwrap();
        let mut model = TriageModel::new();
        assert!(model
            .dispose_with_undo(
                std::slice::from_ref(&f),
                TriageState::Ignored,
                "test fixture",
                0
            )
            .is_some());
        assert_eq!(finding_state(&model.dispositions, &f), TriageState::Ignored);
        // Already Ignored; a blank reason is still a refusal.
        assert!(model
            .dispose_with_undo(std::slice::from_ref(&f), TriageState::Ignored, "  ", 1)
            .is_none());
        assert_eq!(finding_state(&model.dispositions, &f), TriageState::Ignored);
    }

    /// Code viewer A.1: `O` opens the viewer, scoped exactly like the disposition keys. Held
    /// with a modifier, or typed while a reason prompt or anything outside the pane has focus,
    /// it is nothing (a stray `o` in a reason is text).
    #[test]
    fn o_opens_the_code_viewer_under_the_same_scope_as_the_disposition_keys() {
        let m = KeyMods::none();
        for key in ["o", "O"] {
            assert_eq!(
                pane_command(PaneFocus::Cursor, key, m),
                Some(PaneCommand::OpenCode)
            );
        }
        for focus in [PaneFocus::ReasonPrompt, PaneFocus::Elsewhere] {
            assert_eq!(pane_command(focus, "o", m), None, "{focus:?}");
        }
        for mods in [
            KeyMods {
                ctrl: true,
                ..KeyMods::none()
            },
            KeyMods {
                alt: true,
                ..KeyMods::none()
            },
            KeyMods {
                meta: true,
                ..KeyMods::none()
            },
        ] {
            assert_eq!(pane_command(PaneFocus::Cursor, "o", mods), None);
        }
        // It is not a disposition: no state has `o` for a hotkey.
        assert!(TriageState::all()
            .into_iter()
            .all(|s| state_hotkey(s) != Some("o")));
    }

    /// req_pass2_11: the keys the pane does not claim stay unclaimed, so Tab, Escape, Enter
    /// and ordinary typing keep their browser meaning.
    #[test]
    fn req_pass2_11_unclaimed_keys_produce_no_command() {
        let m = KeyMods::none();
        for key in [
            "a",
            "x",
            "Tab",
            "Escape",
            "Enter",
            " ",
            "ArrowLeft",
            "ArrowRight",
            "1",
        ] {
            assert_eq!(
                pane_command(PaneFocus::Cursor, key, m),
                None,
                "{key} is not ours"
            );
        }
    }

    /// req_pass2_11: the pane's own armed/off indicator is DEFINED as "would a key fire",
    /// not as a second reading of the focus enum — so the thing the curator sees and the
    /// thing the handler does cannot drift apart.
    #[test]
    fn req_pass2_11_the_armed_indicator_is_the_handlers_own_gate() {
        assert!(pane_keys_armed(PaneFocus::Cursor));
        assert!(!pane_keys_armed(PaneFocus::ReasonPrompt));
        assert!(!pane_keys_armed(PaneFocus::Elsewhere));
        for focus in [
            PaneFocus::Cursor,
            PaneFocus::ReasonPrompt,
            PaneFocus::Elsewhere,
        ] {
            assert_eq!(
                pane_keys_armed(focus),
                pane_command(focus, "c", KeyMods::none()).is_some(),
                "the indicator must be the gate itself"
            );
        }
    }

    /// req_pass2_11: `I` and `F` PROMPT; they never dispose. The two reason-carrying
    /// dispositions keep their require-reason invariant under the keyboard exactly as they do
    /// under the buttons — the export reads those reasons, and a keystroke is not a licence
    /// to drop them.
    #[test]
    fn req_pass2_11_reason_carrying_keys_prompt_rather_than_dispose() {
        let m = KeyMods::none();
        assert_eq!(
            pane_command(PaneFocus::Cursor, "i", m),
            Some(PaneCommand::PromptFor(TriageState::Ignored))
        );
        assert_eq!(
            pane_command(PaneFocus::Cursor, "f", m),
            Some(PaneCommand::PromptFor(TriageState::FalsePositive))
        );
        // And the split is driven by the state's own answer, not by a second list of keys.
        for state in TriageState::all() {
            let prompts = matches!(
                pane_command(PaneFocus::Cursor, state_hotkey(state).unwrap_or("x"), m),
                Some(PaneCommand::PromptFor(_))
            );
            if state_hotkey(state).is_some() {
                assert_eq!(
                    prompts,
                    state.requires_reason(),
                    "{} must prompt iff it requires a reason",
                    state.label()
                );
            }
        }
    }

    /// req_pass2_11: `Unresolved` has no hotkey. It is the state rows START in, not a
    /// decision, and a key that un-decides a row is not something to hand a curator at 1am.
    #[test]
    fn req_pass2_11_unresolved_has_no_hotkey() {
        assert_eq!(state_hotkey(TriageState::Unresolved), None);
        for s in TriageState::all() {
            assert_eq!(
                state_hotkey(s).is_some(),
                s.is_decided(),
                "{} — exactly the decisions get keys",
                s.label()
            );
        }
    }

    // ── req_pass2_11: THE FIVE-SECOND UNDO (owner's Q5 ruling, design §13) ────────────
    //
    // > plus a **five-second undo toast**. … the toast removes the 1am risk.
    //
    // An undo that is rendered and does nothing would be the ships-inert shape again, so
    // every one of these asserts the DISPOSITION MAP, never the toast's presence.

    fn row(path: &str) -> FindingView {
        finding(serde_json::json!({
            "repo": "me/api", "path": path, "line": 1, "rule_id": "SQL-INJECTION-1",
            "severity": "critical", "snippet": "s", "detail": "", "kind": "defect"
        }))
    }

    /// req_pass2_11: the window is five seconds, because the ruling says five seconds.
    #[test]
    fn req_pass2_11_the_undo_window_is_five_seconds() {
        assert_eq!(UNDO_WINDOW_MS, 5_000);
    }

    /// req_pass2_11: WITHIN the window, undo puts the row back exactly as it was.
    #[test]
    fn req_pass2_11_undo_within_the_window_reverts_the_disposition() {
        let f = row("src/a.rs");
        let mut m = TriageModel::new();
        let mut entry = m
            .dispose_with_undo(std::slice::from_ref(&f), TriageState::Confirmed, "", 1_000)
            .expect("Confirm needs no reason, so it is never refused");
        assert_eq!(
            m.state_of(&f),
            TriageState::Confirmed,
            "applied immediately"
        );

        assert_eq!(m.undo(&mut entry, 1_000 + 4_999), UndoOutcome::Reverted(1));
        assert_eq!(m.state_of(&f), TriageState::Unresolved, "and put back");
    }

    /// req_pass2_11 — **THE OTHER HALF.** AFTER the window, undo changes nothing and SAYS it
    /// changed nothing. A button that silently no-ops is worse than no button.
    #[test]
    fn req_pass2_11_undo_after_the_window_closes_changes_nothing() {
        let f = row("src/a.rs");
        let mut m = TriageModel::new();
        let mut entry = m
            .dispose_with_undo(std::slice::from_ref(&f), TriageState::Confirmed, "", 1_000)
            .expect("applied");

        assert_eq!(
            m.undo(&mut entry, 1_000 + UNDO_WINDOW_MS),
            UndoOutcome::WindowClosed,
            "five seconds exactly is already closed"
        );
        assert_eq!(
            m.state_of(&f),
            TriageState::Confirmed,
            "the decision stands"
        );
        assert_eq!(
            m.undo(&mut entry, 1_000 + 60_000),
            UndoOutcome::WindowClosed
        );
        assert_eq!(m.state_of(&f), TriageState::Confirmed);
    }

    /// req_pass2_11: the live window is readable, so the toast can count down and can
    /// dismiss itself the moment the undo stops working rather than a frame later.
    #[test]
    fn req_pass2_11_the_toast_can_tell_whether_its_own_undo_still_works() {
        let f = row("src/a.rs");
        let mut m = TriageModel::new();
        let entry = m
            .dispose_with_undo(&[f], TriageState::Confirmed, "", 1_000)
            .expect("applied");
        assert!(entry.is_live(1_000));
        assert!(entry.is_live(5_999));
        assert!(
            !entry.is_live(6_000),
            "the window closes exactly at +5000ms"
        );
        assert_eq!(entry.remaining_ms(1_000), 5_000);
        assert_eq!(entry.remaining_ms(3_500), 2_500);
        assert_eq!(entry.remaining_ms(99_999), 0);
    }

    /// req_pass2_11: undo restores the row's PRIOR disposition, not merely "Unresolved". A
    /// row that was Ignored with a recorded reason, re-confirmed by a stray keypress, comes
    /// back Ignored WITH its reason — the reason is the baseline audit trail and losing it
    /// quietly is the failure this test exists for.
    #[test]
    fn req_pass2_11_undo_restores_the_prior_state_and_its_reason() {
        let f = row("src/a.rs");
        let mut m = TriageModel::new();
        m.ignore(std::slice::from_ref(&f), "accepted: fixture-only path");
        m.set_bucket(std::slice::from_ref(&f), TechDebtBucket::Now);

        let mut entry = m
            .dispose_with_undo(std::slice::from_ref(&f), TriageState::Confirmed, "", 10_000)
            .expect("applied");
        assert_eq!(m.state_of(&f), TriageState::Confirmed);

        assert_eq!(m.undo(&mut entry, 11_000), UndoOutcome::Reverted(1));
        let d = m.dispositions.get(&finding_key(&f)).expect("still present");
        assert_eq!(d.state, TriageState::Ignored);
        assert_eq!(
            d.reason, "accepted: fixture-only path",
            "the audit trail comes back"
        );
        assert_eq!(d.bucket, TechDebtBucket::Now, "and so does the bucket");
    }

    /// req_pass2_11: undoing a row that had NO disposition removes the entry entirely rather
    /// than leaving an explicit `Unresolved` record behind. The draft is persisted, and an
    /// undone action that leaves a trace has not fully undone anything.
    #[test]
    fn req_pass2_11_undo_restores_absence_not_an_explicit_unresolved() {
        let f = row("src/a.rs");
        let mut m = TriageModel::new();
        let mut entry = m
            .dispose_with_undo(std::slice::from_ref(&f), TriageState::TechDebt, "", 0)
            .expect("applied");
        assert_eq!(m.dispositions.len(), 1);
        assert_eq!(m.undo(&mut entry, 1), UndoOutcome::Reverted(1));
        assert!(
            m.dispositions.is_empty(),
            "the map must be byte-identical to before the action, not merely equivalent"
        );
    }

    /// req_pass2_11: an undo is SINGLE USE. Undoing twice must not re-apply the revert over
    /// a decision the curator has made since.
    #[test]
    fn req_pass2_11_an_undo_is_single_use() {
        let f = row("src/a.rs");
        let mut m = TriageModel::new();
        let mut entry = m
            .dispose_with_undo(std::slice::from_ref(&f), TriageState::Confirmed, "", 0)
            .expect("applied");
        assert_eq!(m.undo(&mut entry, 100), UndoOutcome::Reverted(1));
        // The curator then deliberately marks it tech debt.
        m.move_to(std::slice::from_ref(&f), TriageState::TechDebt);
        assert_eq!(m.undo(&mut entry, 200), UndoOutcome::AlreadyUndone);
        assert_eq!(
            m.state_of(&f),
            TriageState::TechDebt,
            "a second undo must not reach back over a newer decision"
        );
    }

    /// req_pass2_11: the keyboard path honours the require-reason invariant. A blank reason
    /// is REFUSED and nothing is written — no disposition, and no undo entry claiming one.
    #[test]
    fn req_pass2_11_dispose_with_undo_refuses_a_reason_carrying_state_with_no_reason() {
        for state in [TriageState::Ignored, TriageState::FalsePositive] {
            let f = row("src/a.rs");
            let mut m = TriageModel::new();
            assert!(
                m.dispose_with_undo(std::slice::from_ref(&f), state, "   ", 0)
                    .is_none(),
                "{} with a blank reason must be refused",
                state.label()
            );
            assert!(m.dispositions.is_empty(), "and must write nothing");
            assert_eq!(m.state_of(&f), TriageState::Unresolved);
        }
    }

    /// req_pass2_11: with a reason, the reason-carrying states go through the SAME write
    /// paths the buttons use, so the keyboard cannot become a second, divergent one.
    #[test]
    fn req_pass2_11_dispose_with_undo_is_the_same_write_the_buttons_make() {
        let f = row("src/a.rs");
        for (state, reason) in [
            (TriageState::Confirmed, ""),
            (TriageState::TechDebt, ""),
            (TriageState::Ignored, "accepted risk"),
            (TriageState::FalsePositive, "the tool mis-parsed the macro"),
        ] {
            let mut via_pane = TriageModel::new();
            via_pane
                .dispose_with_undo(std::slice::from_ref(&f), state, reason, 0)
                .unwrap_or_else(|| panic!("{} must apply", state.label()));

            let mut via_button = TriageModel::new();
            match state {
                TriageState::Confirmed => via_button.confirm(std::slice::from_ref(&f)),
                TriageState::Ignored => via_button.ignore(std::slice::from_ref(&f), reason),
                TriageState::FalsePositive => {
                    via_button.mark_false_positive(std::slice::from_ref(&f), reason)
                }
                other => via_button.move_to(std::slice::from_ref(&f), other),
            }
            assert!(
                via_pane.dispositions == via_button.dispositions,
                "{} via the pane must leave EXACTLY the state the button leaves",
                state.label()
            );
        }
    }

    /// req_pass2_11: the toast says what it will undo, naming the disposition and the count,
    /// so "Undo" is never an offer to reverse something unnamed.
    #[test]
    fn req_pass2_11_the_undo_entry_names_what_it_would_reverse() {
        let mut m = TriageModel::new();
        let one = m
            .dispose_with_undo(&[row("src/a.rs")], TriageState::Confirmed, "", 0)
            .expect("applied");
        assert_eq!(one.message(), "Confirmed 1 finding.");
        let mut m = TriageModel::new();
        let many = m
            .dispose_with_undo(
                &[row("src/a.rs"), row("src/b.rs"), row("src/c.rs")],
                TriageState::FalsePositive,
                "mis-parsed",
                0,
            )
            .expect("applied");
        assert_eq!(many.message(), "Marked 3 findings as False positive.");
    }

    /// req_pass2_11: an action over NO rows produces no entry, so the toast never offers to
    /// undo something that never happened.
    #[test]
    fn req_pass2_11_disposing_nothing_arms_no_undo() {
        let mut m = TriageModel::new();
        assert!(m
            .dispose_with_undo(&[], TriageState::Confirmed, "", 0)
            .is_none());
        assert!(m.dispositions.is_empty());
    }

    // ── req_pass2_12: THE RULE HUB (design §8) ───────────────────────────────
    //
    // "Rule -> everything about that rule. This is THE join, and it gets the investment."
    // These tests pin the DERIVATION: what the hub knows about one rule in one scan, and —
    // just as load-bearing — what it refuses to claim.

    fn hub_row(repo: &str, rule_id: &str, path: &str, kind: serde_json::Value) -> FindingView {
        let mut v = serde_json::json!({
            "repo": repo, "path": path, "line": 3, "rule_id": rule_id,
            "severity": "medium", "snippet": "x", "detail": "d"
        });
        if let Some(obj) = kind.as_object() {
            for (k, val) in obj {
                v[k] = val.clone();
            }
        }
        finding(v)
    }

    /// req_pass2_12: the hub states the rule's kind OUTCOME for this scan, its counts, and the
    /// surface it lives on — the four things §8 names, derived rather than re-stated.
    #[test]
    fn req_pass2_12_hub_states_the_kind_outcome_and_counts_for_a_deviation() {
        let rows = vec![
            hub_row(
                "o/r",
                "API-VERSIONING-1",
                "src/routes.rs",
                serde_json::json!({
                    "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                    "convention_label": "URI-versioned endpoints",
                    "evaluated_option_id": "uri-versioned"
                }),
            ),
            hub_row(
                "o/r",
                "API-VERSIONING-1",
                "src/other.rs",
                serde_json::json!({
                    "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                    "convention_label": "URI-versioned endpoints",
                    "evaluated_option_id": "uri-versioned"
                }),
            ),
        ];
        let hub = rule_hub(&rows, "o/r", "API-VERSIONING-1").expect("the rule is in this scan");
        assert_eq!(hub.surface, KindSurface::Queue);
        assert_eq!(hub.kind_tag, "deviation");
        assert_eq!(hub.row_count, 2);
        assert_eq!(hub.site_count, 2);
        assert_eq!(
            hub.convention_label.as_deref(),
            Some("URI-versioned endpoints")
        );
        assert_eq!(hub.evaluated_option_id.as_deref(), Some("uri-versioned"));
        assert!(
            hub.outcome.contains("2 of 47"),
            "the outcome must be the structured deviation sentence, got {:?}",
            hub.outcome
        );
    }

    /// req_pass2_12: THE HONESTY CONSTRAINT, stated in §8. Conforming sites are not in the
    /// data — only counts. The hub may state the count; it must expose no handle that implies
    /// the 45 conforming endpoints can be listed.
    #[test]
    fn req_pass2_12_hub_counts_conforming_sites_but_offers_no_way_to_list_them() {
        let rows = vec![hub_row(
            "o/r",
            "API-VERSIONING-1",
            "src/routes.rs",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                "convention_label": "URI-versioned endpoints"
            }),
        )];
        let hub = rule_hub(&rows, "o/r", "API-VERSIONING-1").expect("present");
        let c = hub.conforming.expect("a known denominator yields a count");
        assert_eq!((c.conforming, c.assessed), (45, 47));
        // The ONE jump the hub offers is its own surface. There is no second target, and in
        // particular no conforming-sites target, because that data does not exist.
        assert_eq!(hub.jump, KindSurface::Queue);
    }

    /// req_pass2_12: an unknown denominator yields NO conforming count. The producer could not
    /// count honestly, so neither can the hub.
    #[test]
    fn req_pass2_12_hub_claims_no_conforming_count_without_a_denominator() {
        let rows = vec![hub_row(
            "o/r",
            "API-VERSIONING-1",
            "src/routes.rs",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": null,
                "convention_label": "URI-versioned endpoints"
            }),
        )];
        let hub = rule_hub(&rows, "o/r", "API-VERSIONING-1").expect("present");
        assert!(hub.conforming.is_none());
        assert!(
            !hub.outcome.contains("of "),
            "no fabricated denominator in the sentence: {:?}",
            hub.outcome
        );
    }

    /// req_pass2_12: the hub reaches the informational surfaces too — §8 says it is reachable
    /// from every surface showing a rule id, which means it must have something to say about
    /// a rule whose only row is a compliment.
    #[test]
    fn req_pass2_12_hub_describes_each_informational_surface() {
        let conv = vec![hub_row(
            "o/r",
            "SQL-STYLE-1",
            "-",
            serde_json::json!({
                "kind": "convention_observed", "convention_option_id": "opt-a",
                "convention_label": "Parameterized queries"
            }),
        )];
        let h = rule_hub(&conv, "o/r", "SQL-STYLE-1").expect("present");
        assert_eq!(h.surface, KindSurface::Conventions);
        assert_eq!(h.convention_label.as_deref(), Some("Parameterized queries"));
        assert!(
            h.outcome.contains("Parameterized queries"),
            "{:?}",
            h.outcome
        );

        let none = vec![hub_row(
            "o/r",
            "SPLIT-1",
            "-",
            serde_json::json!({
                "kind": "no_convention",
                "observed_options": [
                    {"option_id": "a", "label": "A", "sites": 3},
                    {"option_id": "b", "label": "B", "sites": null}
                ]
            }),
        )];
        let h = rule_hub(&none, "o/r", "SPLIT-1").expect("present");
        assert_eq!(h.surface, KindSurface::NoConvention);
        assert!(h.conforming.is_none());

        let na = vec![hub_row(
            "o/r",
            "NA-1",
            "-",
            serde_json::json!({
                "kind": "not_assessed", "reason": "tool_failed",
                "reason_detail": "eslint exited 2"
            }),
        )];
        let h = rule_hub(&na, "o/r", "NA-1").expect("present");
        assert_eq!(h.surface, KindSurface::NotAssessed);
        assert!(h.outcome.contains("tool_failed"), "{:?}", h.outcome);
    }

    /// req_pass2_12: the hub SURFACES the one-rule-one-surface invariant rather than
    /// re-deriving it. The conflict list it carries is exactly what
    /// [`rules_on_multiple_surfaces`] reports for that `(repo, rule)`.
    #[test]
    fn req_pass2_12_hub_surfaces_the_cross_surface_conflict_it_does_not_redecide_it() {
        let rows = vec![
            hub_row(
                "o/r",
                "DUP-1",
                "src/a.rs",
                serde_json::json!({"kind": "defect"}),
            ),
            hub_row(
                "o/r",
                "DUP-1",
                "-",
                serde_json::json!({
                    "kind": "convention_observed", "convention_option_id": "o",
                    "convention_label": "L"
                }),
            ),
        ];
        let hub = rule_hub(&rows, "o/r", "DUP-1").expect("present");
        let expected: Vec<KindSurface> = rules_on_multiple_surfaces(&rows)
            .into_iter()
            .find(|((repo, rid), _)| repo == "o/r" && rid == "DUP-1")
            .expect("the invariant fires")
            .1;
        assert_eq!(hub.conflict_surfaces, expected);
        // Healthy rules say nothing.
        let clean = vec![hub_row(
            "o/r",
            "OK-1",
            "src/a.rs",
            serde_json::json!({"kind": "defect"}),
        )];
        assert!(rule_hub(&clean, "o/r", "OK-1")
            .expect("present")
            .conflict_surfaces
            .is_empty());
    }

    /// req_pass2_12: the hub is scoped per `(repo, rule)`, the same key the invariant uses. A
    /// second repo's rows for the same rule are a different assessment and must not be counted
    /// into this one.
    #[test]
    fn req_pass2_12_hub_is_scoped_per_repo_like_the_invariant() {
        let rows = vec![
            hub_row(
                "o/a",
                "R-1",
                "src/a.rs",
                serde_json::json!({"kind": "defect"}),
            ),
            hub_row(
                "o/b",
                "R-1",
                "src/b.rs",
                serde_json::json!({"kind": "defect"}),
            ),
            hub_row(
                "o/b",
                "R-1",
                "src/c.rs",
                serde_json::json!({"kind": "defect"}),
            ),
        ];
        assert_eq!(rule_hub(&rows, "o/a", "R-1").expect("present").row_count, 1);
        assert_eq!(rule_hub(&rows, "o/b", "R-1").expect("present").row_count, 2);
        assert!(rule_hub(&rows, "o/c", "R-1").is_none());
    }

    /// req_pass2_12: a merged row's absorbed sites are counted as SITES, never as extra rows —
    /// the same conservation discipline §6 imposed on the detail pane.
    #[test]
    fn req_pass2_12_hub_counts_absorbed_sites_without_doubling_rows() {
        let mut f = hub_row(
            "o/r",
            "R-1",
            "src/a.rs",
            serde_json::json!({"kind": "defect"}),
        );
        f.also_locations = vec![
            MergedSiteView {
                repo: "o/r".to_string(),
                path: "src/b.rs".to_string(),
                line: 9,
                rule_id: "R-1".to_string(),
                snippet: String::new(),
                consequence: false,
            },
            MergedSiteView {
                repo: "o/r".to_string(),
                path: "src/c.rs".to_string(),
                line: 11,
                rule_id: "R-1".to_string(),
                snippet: String::new(),
                consequence: true,
            },
        ];
        let hub = rule_hub(&[f], "o/r", "R-1").expect("present");
        assert_eq!(hub.row_count, 1);
        assert_eq!(hub.site_count, 3);
    }

    // ── req_pass2_13: the REMOVALS sweep (design §9) ─────────────────────────
    //
    // Each of these proves the REPLACEMENT reaches the information the removed affordance
    // carried. A removal whose replacement falls short is a capability loss dressed as
    // cleanup, which is the specific risk of a sweep like this.

    fn conf_row(json: serde_json::Value) -> FindingView {
        let mut v = serde_json::json!({
            "repo": "o/r", "path": "src/a.rs", "line": 1, "rule_id": "R-1",
            "severity": "medium", "snippet": "s", "detail": "d"
        });
        for (k, val) in json.as_object().expect("object") {
            v[k] = val.clone();
        }
        finding(v)
    }

    /// req_pass2_13 (§9.3): the structured Confidence chip reads the STRUCTURED fields and
    /// reports every state the "Needs review" column reported — test scope, hedged, confident
    /// — plus one the old column could not say at all: "calibration never looked".
    #[test]
    fn req_pass2_13_confidence_reads_structured_fields_and_keeps_every_old_state() {
        assert_eq!(
            confidence_tag(&conf_row(serde_json::json!({"in_test": true}))),
            "test"
        );
        assert_eq!(
            confidence_tag(&conf_row(serde_json::json!({"needs_review": true}))),
            "needs-review"
        );
        assert_eq!(
            confidence_tag(&conf_row(serde_json::json!({"confidence": "needs-review"}))),
            "needs-review"
        );
        assert_eq!(
            confidence_tag(&conf_row(serde_json::json!({"confidence": "high"}))),
            "high"
        );
        // The deterministic floor: calibration never saw it. Distinct from "high" on purpose.
        assert_eq!(confidence_tag(&conf_row(serde_json::json!({}))), "unrated");
    }

    /// req_pass2_13 (§9.3): the chip never parses prose. A row whose `detail` carries the
    /// legacy `[needs review: …]` marker but whose STRUCTURED verdict is `high` reads `high`.
    ///
    /// This is the removal's whole point, so it is asserted in the direction that would hurt:
    /// if the regex were still in the path, this row would be chipped "needs review" on the
    /// authority of a sentence the calibration pass itself has since contradicted.
    #[test]
    fn req_pass2_13_confidence_never_parses_the_prose_marker() {
        let contradicted = conf_row(serde_json::json!({
            "detail": "Debatable on a small app. [needs review: premature for a mini app]",
            "confidence": "high",
            "needs_review": false
        }));
        assert_eq!(confidence_tag(&contradicted), "high");
        // A row carrying BOTH, as the current pipeline emits them, reads the same either way
        // — the regex was reading a shadow of the field beside it.
        let agreeing = conf_row(serde_json::json!({
            "detail": "Hedged. [needs review: premature for a mini app]",
            "confidence": "needs-review",
            "needs_review": true
        }));
        assert_eq!(confidence_tag(&agreeing), "needs-review");
        // The legacy shape: prose only, no structured verdict. It reads `unrated` rather than
        // `needs-review`, and the hedge still reaches the curator through the detail pane's
        // own `split_needs_review` rendering of the body — §9.3's "the reason text lives in
        // the detail pane".
        let legacy = conf_row(serde_json::json!({
            "detail": "Old report. [needs review: no structured verdict here]"
        }));
        assert_eq!(confidence_tag(&legacy), "unrated");
        assert!(
            crate::rules::split_needs_review(&legacy.detail).1.is_some(),
            "the pane must still be able to surface a legacy row's hedge"
        );
    }

    /// req_pass2_13 (anti-drift): the Confidence filter vocabulary is exactly the set the
    /// accessor can emit. A stale option list filters every row out and reads as "no
    /// findings", which is a worse failure than a wrong order.
    #[test]
    fn req_pass2_13_confidence_filter_vocabulary_matches_the_cell_it_filters() {
        let rows = [
            conf_row(serde_json::json!({"in_test": true})),
            conf_row(serde_json::json!({"needs_review": true})),
            conf_row(serde_json::json!({"confidence": "high"})),
            conf_row(serde_json::json!({})),
        ];
        let vocab = confidence_tags();
        for r in &rows {
            assert!(
                vocab.contains(&confidence_tag(r).to_string()),
                "{} is emitted but not filterable",
                confidence_tag(r)
            );
        }
        assert_eq!(
            vocab.len(),
            4,
            "and nothing filterable that is never emitted"
        );
    }

    /// req_pass2_13 (§9.2): the on-demand reason prompt NAMES the disposition and the count.
    /// The two always-visible textareas it replaces named neither — a textarea beside a
    /// selection cannot say how many rows the button next to it will move, and a bulk
    /// disposition is exactly where that gap is expensive.
    #[test]
    fn req_pass2_13_the_reason_prompt_names_the_disposition_and_the_count() {
        let one = bulk_reason_heading(TriageState::Ignored, 1);
        assert!(one.contains("Ignored"), "{one}");
        assert!(one.contains("1 finding"), "{one}");
        assert!(!one.contains("1 findings"), "singular agreement; {one}");
        let many = bulk_reason_heading(TriageState::FalsePositive, 7);
        assert!(many.contains("False positive"), "{many}");
        assert!(many.contains("7 findings"), "{many}");
        for h in [&one, &many] {
            assert!(
                h.contains("required"),
                "the invariant is stated up front; {h}"
            );
        }
    }

    /// req_pass2_12: a kind-less row keeps its ALARM identity inside the hub too — it is not
    /// quietly described as a defect.
    #[test]
    fn req_pass2_12_hub_keeps_the_unspecified_alarm_an_alarm() {
        let rows = vec![hub_row("o/r", "R-1", "src/a.rs", serde_json::json!({}))];
        let hub = rule_hub(&rows, "o/r", "R-1").expect("present");
        assert_eq!(hub.kind_tag, "unspecified");
        assert_eq!(hub.surface, KindSurface::Queue);
        assert!(
            !hub.outcome.to_lowercase().contains("defect"),
            "an unmapped emission path must never be described as a defect: {:?}",
            hub.outcome
        );
    }
}
