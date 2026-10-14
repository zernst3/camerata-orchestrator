//! Onboarding-triage state machine, extracted from the Dioxus scan adapter (RUST-HEADLESS-CORE-1).
//!
//! This module owns the PURE triage model: the data shapes (`FindingView`, `Disposition`), the
//! stable-identity + state-derivation helpers (`finding_key`, `finding_state`), and the
//! framework-agnostic state machine (`TriageModel`) that the Dioxus layer drives. The adapter
//! keeps the reactive signals, the chorale table effects, the toasts, and the rsx; every pure
//! transition + derivation lives here and is unit-tested with no VirtualDom.
//!
//! The architect moves each finding between four tables (Unresolved / Ignored / Tech debt /
//! False positive) until nothing is Unresolved, then buckets the tech-debt findings (resolve
//! later / now). This is LOCAL triage state — the backend commit (baseline waiver / ticket /
//! dev-engine import) happens at Process, not on each move. False positive is the odd one out:
//! it is never committed anywhere — see `TriageState::FalsePositive`'s doc comment.

use std::collections::HashMap;

/// Where a finding sits in onboarding triage. The architect moves each finding between these
/// FOUR tables (a single-select switches the view) until nothing is Unresolved; then the
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
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum TriageState {
    #[default]
    Unresolved,
    Ignored,
    TechDebt,
    FalsePositive,
}

impl TriageState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ignored => "Ignored",
            Self::TechDebt => "Tech debt",
            Self::FalsePositive => "False positive",
        }
    }
}

/// Which tech-debt bucket a finding is in: resolve LATER (file a tracked ticket) or NOW (pull
/// into the dev engine as the first story). Only meaningful when state == TechDebt.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TechDebtBucket {
    Later,
    Now,
}

/// One finding's triage disposition: its table, the (required) ignore reason, and its
/// tech-debt bucket. Absence from the dispositions map == Unresolved with defaults.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
    #[serde(default, flatten)]
    pub kind: Option<camerata_api_types::audit::FindingKind>,
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
    let mut seen: std::collections::BTreeMap<(String, String), std::collections::BTreeSet<KindSurface>> =
        std::collections::BTreeMap::new();
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
    /// Step 3's `Confirm all N` is the same shape against `TriageState::Confirmed`, which does
    /// not exist yet — so it is not written here, because the four states that DO exist offer
    /// nothing that honestly means "yes, this is real, it ships".
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

    /// Tally the four buckets over `findings`: `(Unresolved, Ignored, TechDebt, FalsePositive)`.
    pub fn counts(&self, findings: &[FindingView]) -> (usize, usize, usize, usize) {
        let mut unresolved = 0;
        let mut ignored = 0;
        let mut tech_debt = 0;
        let mut false_positive = 0;
        for f in findings {
            match self.state_of(f) {
                TriageState::Unresolved => unresolved += 1,
                TriageState::Ignored => ignored += 1,
                TriageState::TechDebt => tech_debt += 1,
                TriageState::FalsePositive => false_positive += 1,
            }
        }
        (unresolved, ignored, tech_debt, false_positive)
    }
}

impl Default for TriageModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn counts_tallies_the_four_buckets() {
        let mut m = TriageModel::new();
        let a = f("r", 1, "low", "active");
        let b = f("r", 2, "low", "active");
        let c = f("r", 3, "low", "active");
        let d = f("r", 4, "low", "active");
        m.ignore(std::slice::from_ref(&b), "noise");
        m.move_to(std::slice::from_ref(&c), TriageState::TechDebt);
        m.mark_false_positive(std::slice::from_ref(&d), "not exploitable — mocked in test scope");
        let all = vec![a, b, c, d];
        assert_eq!(m.counts(&all), (1, 1, 1, 1));
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
        m.mark_false_positive(std::slice::from_ref(&item), "tool misfired on generated code");
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
        m.mark_false_positive(std::slice::from_ref(&fp), "generated fixture, not real code");
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
        assert_eq!(back["deviating_sites"], 2, "the deviation numerator must survive");
        assert_eq!(back["assessed_sites"], 47, "the deviation denominator must survive");
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
            visible.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>()
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
        assert_eq!(kind_tag(&deviation("V", "medium", 2, Some(47))), "deviation");
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
            conventions.iter().map(|f| f.rule_id.clone()).collect::<Vec<_>>(),
            vec!["C1", "C2"]
        );
        assert_eq!(findings_on_surface(&all, KindSurface::NoConvention).len(), 1);
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
        let out = m.visible(&[defect("A-DEFECT", "high"), unspecified("Z-UNMAPPED", "high")]);
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
        assert!("low" < "medium", "the raw authored words sort low above medium");
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
        let broken = vec![convention("API-VERSIONING-1"), deviation("API-VERSIONING-1", "medium", 2, Some(47))];
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
        let (unresolved, _, _, _) = m.counts(&queue);
        assert_eq!(
            unresolved, 0,
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
            assert!(deviation_headline(&f).is_none(), "{} has no headline", f.rule_id);
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

        assert_eq!(groups.len(), 2, "one group per deviation rule; the defect is not one");
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
        assert_eq!(group.row_count(), 3, "three distinct rows, no key collision");

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
            assert_eq!(m.bulk_false_positive(&group, blank), None, "refused: {blank:?}");
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
}
