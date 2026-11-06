//! The closed reason sets and the escalation arithmetic of the review tree (design 4.3 and 4.4).
//!
//! The group reviewer (stage 2) picks every reason from a closed list, so the review UI sorts by
//! the enum and never matches on text. The arithmetic here is pure and deterministic: how many
//! escalations a run may put in the architect's queue ([`EscalationBudget::compute`]), and which
//! candidates fall inside that number ([`re_mark`]). Moving the line later runs [`re_mark`] over
//! the stored candidates; no model call is involved.

use serde::{Deserialize, Serialize};

// ── Reasons ───────────────────────────────────────────────────────────────────────────────

/// Why the group reviewer put a site in the architect's queue. Ranked (D4): the order of the
/// variants IS the priority order, a constant and never a prompt. Severe reach first, then
/// disagreement between the stages, then a likely false alarm at high severity, then outliers,
/// rule misfit, unverified evidence and "needs more code".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    UnauthenticatedReach,
    SecretExposure,
    DataLoss,
    CrossTenant,
    /// A caller can do what the code never lets them do: a missing or wrong ownership or role
    /// check, a trusted client-supplied identity (R-3).
    AuthorizationBypass,
    /// Input reaches an interpreter: SQL, a shell, a template, markup that runs (stored XSS)
    /// (R-3).
    Injection,
    ConflictingVerdicts,
    LikelyFalseAlarmAtHighSeverity,
    OutlierInGroup,
    RuleMisfit,
    EvidenceUnverified,
    NeedsMoreCode,
}

impl EscalationReason {
    /// Every reason, in priority order.
    pub const ALL: [EscalationReason; 12] = [
        Self::UnauthenticatedReach,
        Self::SecretExposure,
        Self::DataLoss,
        Self::CrossTenant,
        Self::AuthorizationBypass,
        Self::Injection,
        Self::ConflictingVerdicts,
        Self::LikelyFalseAlarmAtHighSeverity,
        Self::OutlierInGroup,
        Self::RuleMisfit,
        Self::EvidenceUnverified,
        Self::NeedsMoreCode,
    ];

    /// The wire name (`unauthenticated_reach`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnauthenticatedReach => "unauthenticated_reach",
            Self::SecretExposure => "secret_exposure",
            Self::DataLoss => "data_loss",
            Self::CrossTenant => "cross_tenant",
            Self::AuthorizationBypass => "authorization_bypass",
            Self::Injection => "injection",
            Self::ConflictingVerdicts => "conflicting_verdicts",
            Self::LikelyFalseAlarmAtHighSeverity => "likely_false_alarm_at_high_severity",
            Self::OutlierInGroup => "outlier_in_group",
            Self::RuleMisfit => "rule_misfit",
            Self::EvidenceUnverified => "evidence_unverified",
            Self::NeedsMoreCode => "needs_more_code",
        }
    }

    /// The reason named `name`, when it is one of the closed list.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == name.trim())
    }

    /// The reason in words, for a sentence (`unauthenticated reach`).
    pub fn phrase(self) -> &'static str {
        match self {
            Self::UnauthenticatedReach => "unauthenticated reach",
            Self::SecretExposure => "secret exposure",
            Self::DataLoss => "data loss",
            Self::CrossTenant => "cross-tenant access",
            Self::AuthorizationBypass => "authorization bypass",
            Self::Injection => "injection",
            Self::ConflictingVerdicts => "conflicting verdicts",
            Self::LikelyFalseAlarmAtHighSeverity => "a likely false alarm at high severity",
            Self::OutlierInGroup => "an outlier in its group",
            Self::RuleMisfit => "a rule misfit",
            Self::EvidenceUnverified => "unverified evidence",
            Self::NeedsMoreCode => "a need for more code",
        }
    }

    /// The priority rank: 0 is the first reason. A lower rank is looked at first.
    pub fn rank(self) -> u8 {
        // The variants are declared in priority order and `ALL` lists them in that order.
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0) as u8
    }

    /// The four severe-reach reasons (unauthenticated reach, secret exposure, data loss,
    /// cross-tenant).
    pub fn is_severe_reach(self) -> bool {
        matches!(
            self,
            Self::UnauthenticatedReach | Self::SecretExposure | Self::DataLoss | Self::CrossTenant
        )
    }

    /// The reasons that say a security flaw is at the site, which a row of a convention rule
    /// (style, layering, error shape) never carries: the group reviewer's view of a defect near
    /// the row is a row of that defect's own rule.
    pub fn is_security_flaw(self) -> bool {
        matches!(
            self,
            Self::UnauthenticatedReach
                | Self::SecretExposure
                | Self::CrossTenant
                | Self::AuthorizationBypass
                | Self::Injection
        )
    }

    /// The reasons that go past the budget at high severity as well as critical (R-3): the
    /// flaws that cost a client most when they are missed. Unauthenticated reach is not among
    /// them: the group reviewer gives it to anything a logged-out caller touches, and a high one
    /// still has to win a slot.
    pub fn is_severe_at_high(self) -> bool {
        matches!(
            self,
            Self::SecretExposure
                | Self::DataLoss
                | Self::CrossTenant
                | Self::AuthorizationBypass
                | Self::Injection
        )
    }

    /// The reasons that may exceed the budget by default (D5, R-3): the severe reasons (unauthenticated
    /// reach at critical severity; secret exposure, data loss, cross-tenant access,
    /// authorization bypass and injection at critical or high) and `evidence_unverified`,
    /// because the release gate refuses that row without a decision anyway.
    pub fn default_over_budget_set() -> Vec<EscalationReason> {
        vec![
            Self::UnauthenticatedReach,
            Self::SecretExposure,
            Self::DataLoss,
            Self::CrossTenant,
            Self::AuthorizationBypass,
            Self::Injection,
            Self::EvidenceUnverified,
        ]
    }

    /// Whether a candidate with this reason and severity may go over the budget under the
    /// project's `allowed` set: the reason must be in the set and the severity critical, or high
    /// for the reasons of [`Self::is_severe_at_high`].
    /// `evidence_unverified` is always allowed, at any severity, whatever the stored set says: the
    /// release gate refuses that row without a decision, and a project file written before the
    /// settings route refused such a set can still hold one.
    pub fn may_exceed_budget(self, severity: &str, allowed: &[EscalationReason]) -> bool {
        if self == Self::EvidenceUnverified {
            return true;
        }
        let severe_enough = severity.eq_ignore_ascii_case("critical")
            || (self.is_severe_at_high() && severity.eq_ignore_ascii_case("high"));
        allowed.contains(&self) && severe_enough
    }
}

/// Why a site does not belong with the rest of its sub-group (design 4.3 and 8). The grouping
/// pass writes `different_object`; stage 1 writes `severity_raise_held`; the wording stages'
/// review flags map onto `precondition_added`, `unverified_name`, `fix_ungrounded`,
/// `contradiction` and `methodology_leak` (a flag only, never a hold); the group reviewer may
/// write any of them. An outlier never escalates a site on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlierReason {
    SeverityRaiseHeld,
    DifferentObject,
    DifferentConstruct,
    OnlySiteInFile,
    Stage2Outlier,
    PreconditionAdded,
    UnverifiedName,
    FixUngrounded,
    Contradiction,
    EvidenceUnverified,
    InTestCode,
    CouldBeIntentional,
    /// The tool's own name or process shows in client text. Wording, a flag only (A8).
    MethodologyLeak,
}

impl OutlierReason {
    /// Every reason.
    pub const ALL: [OutlierReason; 13] = [
        Self::SeverityRaiseHeld,
        Self::DifferentObject,
        Self::DifferentConstruct,
        Self::OnlySiteInFile,
        Self::Stage2Outlier,
        Self::PreconditionAdded,
        Self::UnverifiedName,
        Self::FixUngrounded,
        Self::Contradiction,
        Self::EvidenceUnverified,
        Self::InTestCode,
        Self::CouldBeIntentional,
        Self::MethodologyLeak,
    ];

    /// The wire name (`severity_raise_held`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SeverityRaiseHeld => "severity_raise_held",
            Self::DifferentObject => "different_object",
            Self::DifferentConstruct => "different_construct",
            Self::OnlySiteInFile => "only_site_in_file",
            Self::Stage2Outlier => "stage2_outlier",
            Self::PreconditionAdded => "precondition_added",
            Self::UnverifiedName => "unverified_name",
            Self::FixUngrounded => "fix_ungrounded",
            Self::Contradiction => "contradiction",
            Self::EvidenceUnverified => "evidence_unverified",
            Self::InTestCode => "in_test_code",
            Self::CouldBeIntentional => "could_be_intentional",
            Self::MethodologyLeak => "methodology_leak",
        }
    }

    /// The reason named `name`, when it is one of the closed list.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == name.trim())
    }

    /// Whether the reason says the site's CAUSE differs from the rest of its group (as opposed to
    /// a wording or evidence flag on a site that shares the cause). A summary has to name the
    /// sites of these reasons (A9).
    pub fn differs_in_cause(self) -> bool {
        matches!(
            self,
            Self::DifferentObject
                | Self::DifferentConstruct
                | Self::OnlySiteInFile
                | Self::Stage2Outlier
        )
    }
}

// ── The budget ────────────────────────────────────────────────────────────────────────────

/// How many escalations a run may put in the architect's queue, and what came of it (4.4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EscalationBudget {
    /// Escalations per 1,000 counted lines. Project setting, default 1.0.
    pub rate_per_kloc: f32,
    /// The fewest escalations a run may carry, so a small fixture still escalates something.
    pub floor: usize,
    /// The lines of code the scan counted (`ScanReport::code_lines`).
    pub counted_lines: usize,
    /// `max(floor, ceil(counted_lines / 1000 * rate_per_kloc))`.
    pub budget: usize,
    /// Candidates marked escalated: the first `budget` plus the ones over budget.
    pub escalated: usize,
    /// `escalated - budget` when positive.
    pub over_budget: usize,
    /// Candidates in all, above and below the line.
    pub candidates: usize,
}

/// The default `escalation_rate_per_kloc`.
pub const DEFAULT_ESCALATION_RATE_PER_KLOC: f32 = 1.0;
/// The default `escalation_floor`.
pub const DEFAULT_ESCALATION_FLOOR: usize = 20;

impl Default for EscalationBudget {
    fn default() -> Self {
        Self::compute(
            DEFAULT_ESCALATION_RATE_PER_KLOC,
            DEFAULT_ESCALATION_FLOOR,
            0,
        )
    }
}

impl EscalationBudget {
    /// `max(floor, ceil(counted_lines / 1000 * rate))`, with no escalations placed yet. The rate
    /// is taken to three decimal places and the arithmetic is on whole numbers, so 0.3 per 1,000
    /// lines of 10,000 lines is exactly 3 and never 4.
    pub fn compute(rate_per_kloc: f32, floor: usize, counted_lines: usize) -> Self {
        let rate_milli = (f64::from(rate_per_kloc.max(0.0)) * 1000.0).round() as u128;
        let by_rate = (counted_lines as u128 * rate_milli).div_ceil(1_000_000) as usize;
        Self {
            rate_per_kloc,
            floor,
            counted_lines,
            budget: floor.max(by_rate),
            escalated: 0,
            over_budget: 0,
            candidates: 0,
        }
    }
}

/// Where a site stands against the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationState {
    /// The group reviewer did not want to escalate this site.
    #[default]
    NotACandidate,
    /// Wanted, but past the budget: kept below the line, visible, one click to raise.
    BelowTheLine,
    /// In the architect's queue.
    Escalated,
}

/// One site the group reviewer (or the engine, for unverified evidence) wanted to escalate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationCandidate {
    /// The site key.
    pub key: String,
    /// The node the site was reviewed under (a rule node).
    pub node_id: String,
    pub reason: EscalationReason,
    /// The site's severity when it was ranked.
    pub severity: String,
    /// The group reviewer's own rank inside its node (1 is first).
    pub stage2_rank: u32,
    /// The position over every node, 1-based; the order [`re_mark`] reads the line in.
    pub global_rank: u32,
    /// The slot of the budget this candidate sits in, 1-based (R-1). Rows at one site (the same
    /// repo, path and line) share one slot whatever their rules, so the budget counts sites and
    /// not rows. 0 is a candidate stored before slots existed: it is a slot of its own.
    #[serde(default)]
    pub slot: u32,
    pub escalated: bool,
    /// Set when the candidate is escalated past the budget: the reason it may be.
    pub over_budget_reason: Option<EscalationReason>,
    /// The group reviewer's one-line reason in the architect's words.
    pub why: String,
}

impl EscalationCandidate {
    /// Where this candidate stands against the line.
    pub fn state(&self) -> EscalationState {
        if self.escalated {
            EscalationState::Escalated
        } else {
            EscalationState::BelowTheLine
        }
    }
}

/// The slot a candidate at list position `position` sits in: its stored slot, or, for a candidate
/// stored before slots existed (slot 0), one of its own. The flag tells the two apart so a legacy
/// position can never collide with a stored slot number.
fn slot_id(c: &EscalationCandidate, position: usize) -> (bool, u32) {
    if c.slot == 0 {
        (false, position as u32)
    } else {
        (true, c.slot)
    }
}

/// How many slots (sites) `candidates` fill, and how many of those are escalated.
pub fn count_slots<'a>(
    candidates: impl IntoIterator<Item = &'a EscalationCandidate>,
) -> (usize, usize) {
    let mut seen: std::collections::HashMap<(bool, u32), bool> = std::collections::HashMap::new();
    for (position, c) in candidates.into_iter().enumerate() {
        let escalated = seen.entry(slot_id(c, position)).or_insert(false);
        *escalated |= c.escalated;
    }
    (seen.len(), seen.values().filter(|e| **e).count())
}

/// Mark the line again over stored candidates, with no model call. `candidates` are in
/// `global_rank` order; the first `budget` SLOTS are escalated (rows at one site share a slot,
/// R-1), and past it only a slot with a candidate whose reason may exceed the budget under
/// `allowed` ([`EscalationReason::may_exceed_budget`]) is, with its `over_budget_reason`
/// recorded on every row of the slot. The budget is recomputed from `rate_per_kloc`, `floor` and
/// `counted_lines`, and its counts (in slots) are rewritten.
pub fn re_mark(
    candidates: &mut [EscalationCandidate],
    budget: &mut EscalationBudget,
    allowed: &[EscalationReason],
) {
    let fresh = EscalationBudget::compute(budget.rate_per_kloc, budget.floor, budget.counted_lines);
    budget.budget = fresh.budget;
    // The slots in the order they first appear, each with its rows.
    let mut ordinal: std::collections::HashMap<(bool, u32), usize> =
        std::collections::HashMap::new();
    let mut slots: Vec<Vec<usize>> = Vec::new();
    for (position, c) in candidates.iter().enumerate() {
        let at = *ordinal.entry(slot_id(c, position)).or_insert_with(|| {
            slots.push(Vec::new());
            slots.len() - 1
        });
        slots[at].push(position);
    }
    let mut escalated = 0usize;
    for (at, members) in slots.iter().enumerate() {
        let (is_escalated, over) = if at < budget.budget {
            (true, None)
        } else {
            let allowed_by = members
                .iter()
                .map(|&m| &candidates[m])
                .find(|c| c.reason.may_exceed_budget(&c.severity, allowed))
                .map(|c| c.reason);
            (allowed_by.is_some(), allowed_by)
        };
        for &m in members {
            candidates[m].escalated = is_escalated;
            candidates[m].over_budget_reason = over;
        }
        escalated += usize::from(is_escalated);
    }
    budget.escalated = escalated;
    budget.over_budget = escalated.saturating_sub(budget.budget);
    budget.candidates = slots.len();
}

// ── The columns stage 2 writes on a site row (design A5) ─────────────────────────────────

/// Which of stage 1's answers the group reviewer disagrees with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisagreementField {
    Severity,
    Real,
}

impl DisagreementField {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Severity => "severity",
            Self::Real => "real",
        }
    }

    /// The field named `name`.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "severity" => Some(Self::Severity),
            "real" => Some(Self::Real),
            _ => None,
        }
    }
}

/// The group reviewer's recorded disagreement with stage 1 about one site. Stored beside stage 1's
/// answer and never over it: `stage1` is read from the row, `mine` is what the group reviewer said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage2Disagreement {
    pub field: DisagreementField,
    /// Stage 1's value, taken from the row.
    pub stage1: String,
    /// The group reviewer's value.
    pub mine: String,
    /// Why, in the architect's words.
    pub why: String,
}

/// Everything the group reviewer says about one site, as columns on the site's row (A5). Each
/// field of the stage 2 reply has its home here or in the node's summary; a field with no home
/// is not in the reply schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteReview {
    /// Why the site is a candidate for the architect's queue, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_reason: Option<EscalationReason>,
    /// The group reviewer's one-line reason for the escalation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub escalation_why: String,
    /// The group reviewer's own rank inside its node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage2_rank: Option<u32>,
    /// The position among every candidate of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_rank: Option<u32>,
    /// Escalated, below the line, or not a candidate.
    #[serde(default)]
    pub escalation_state: EscalationState,
    /// Set when the site is escalated past the budget: the reason it may be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_budget_reason: Option<EscalationReason>,
    /// The reason the group reviewer gave for the site not belonging with the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outlier_reason: Option<OutlierReason>,
    /// The group reviewer's one-line reason for the outlier mark.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outlier_why: String,
    /// The label of the sub-group the group reviewer placed the site in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subgroup_label: String,
    /// The shared cause of that sub-group.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subgroup_cause: String,
    /// The group reviewer's disagreements with stage 1 about this site: at most one per field
    /// (`severity`, `real`). Stage 1's own answers on the row are never changed by them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage2_disagreement: Vec<Stage2Disagreement>,
}

impl SiteReview {
    /// True for a row the group reviewer has said nothing about. Serde leaves such a column off.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Write the escalation columns from a ranked candidate.
    pub fn set_escalation(&mut self, c: &EscalationCandidate) {
        self.escalation_reason = Some(c.reason);
        self.escalation_why.clone_from(&c.why);
        self.stage2_rank = Some(c.stage2_rank);
        self.global_rank = Some(c.global_rank);
        self.escalation_state = c.state();
        self.over_budget_reason = c.over_budget_reason;
    }

    /// Clear the escalation columns (the site is not a candidate).
    pub fn clear_escalation(&mut self) {
        self.escalation_reason = None;
        self.escalation_why.clear();
        self.stage2_rank = None;
        self.global_rank = None;
        self.escalation_state = EscalationState::NotACandidate;
        self.over_budget_reason = None;
    }
}

// ── How a row was reviewed (design 3B item 3) ─────────────────────────────────────────────

/// How closely a row was reviewed. A closed set, written on every row of every export, so the
/// claim "this row was reviewed" is true row by row and never a rate.
///
/// The server computes it in ONE place, the reviewer overlay (`apply_overlay`), and nowhere else.
/// Every exporter reads it from the reviewed report; none works it out itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReviewLevel {
    /// The reviewer looked at this site: a site-scope decision, an edit, or a decision on an
    /// escalated site.
    PersonallyReviewed,
    /// The reviewer decided the pattern (the tree node with this id) and the site inherited the
    /// decision.
    ReviewedAsPattern(String),
    /// Nobody has decided it; the engine's evidence check passed.
    EngineVerified,
}

/// The wire name of [`ReviewLevel::PersonallyReviewed`].
pub const PERSONALLY_REVIEWED: &str = "personally_reviewed";
/// The wire prefix of [`ReviewLevel::ReviewedAsPattern`]; the node id follows the colon.
pub const REVIEWED_AS_PATTERN_PREFIX: &str = "reviewed_as_pattern:";
/// The wire name of [`ReviewLevel::EngineVerified`].
pub const ENGINE_VERIFIED: &str = "engine_verified";

impl ReviewLevel {
    /// The wire form: `personally_reviewed`, `reviewed_as_pattern:<node_id>` or
    /// `engine_verified`.
    pub fn as_wire(&self) -> String {
        match self {
            Self::PersonallyReviewed => PERSONALLY_REVIEWED.to_string(),
            Self::ReviewedAsPattern(node) => format!("{REVIEWED_AS_PATTERN_PREFIX}{node}"),
            Self::EngineVerified => ENGINE_VERIFIED.to_string(),
        }
    }

    /// The level named by `wire`, or `None` for anything else.
    pub fn parse(wire: &str) -> Option<Self> {
        match wire {
            PERSONALLY_REVIEWED => Some(Self::PersonallyReviewed),
            ENGINE_VERIFIED => Some(Self::EngineVerified),
            other => other
                .strip_prefix(REVIEWED_AS_PATTERN_PREFIX)
                .filter(|node| !node.is_empty())
                .map(|node| Self::ReviewedAsPattern(node.to_string())),
        }
    }
}

impl Serialize for ReviewLevel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_wire())
    }
}

impl<'de> Deserialize<'de> for ReviewLevel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Self::parse(&wire)
            .ok_or_else(|| serde::de::Error::custom(format!("not a review level: {wire}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(key: &str, reason: EscalationReason, severity: &str, rank: u32) -> EscalationCandidate {
        EscalationCandidate {
            key: key.to_string(),
            node_id: "rule:R".to_string(),
            reason,
            severity: severity.to_string(),
            stage2_rank: 1,
            global_rank: rank,
            slot: 0,
            escalated: false,
            over_budget_reason: None,
            why: String::new(),
        }
    }

    #[test]
    fn budget_is_ceil_lines_over_1000_times_rate_with_a_floor() {
        // 300,000 lines at 1.0 per 1,000 is 300.
        assert_eq!(EscalationBudget::compute(1.0, 20, 300_000).budget, 300);
        // A partial thousand rounds up.
        assert_eq!(EscalationBudget::compute(1.0, 20, 300_001).budget, 301);
        // The floor holds a small repo up.
        assert_eq!(EscalationBudget::compute(1.0, 20, 4_000).budget, 20);
        // The rate scales it, and 0.3 of 10 thousand is 3 and not 4 (no float drift).
        assert_eq!(EscalationBudget::compute(0.3, 0, 10_000).budget, 3);
        assert_eq!(EscalationBudget::compute(2.5, 20, 100_000).budget, 250);
        // The floor wins over a rate of zero.
        assert_eq!(EscalationBudget::compute(0.0, 7, 100_000).budget, 7);
    }

    #[test]
    fn the_reason_list_is_closed_and_ranked() {
        assert_eq!(EscalationReason::ALL.len(), 12);
        for (i, r) in EscalationReason::ALL.iter().enumerate() {
            assert_eq!(r.rank() as usize, i);
            assert_eq!(EscalationReason::parse(r.as_str()), Some(*r));
            assert_eq!(
                serde_json::to_value(r).unwrap(),
                serde_json::Value::String(r.as_str().to_string())
            );
        }
        assert_eq!(EscalationReason::parse("because_i_said_so"), None);
        assert!(
            EscalationReason::UnauthenticatedReach.rank() < EscalationReason::NeedsMoreCode.rank()
        );
        // R-3: the two severe reasons added to the list sit with the severe ones, ahead of the
        // reasons about the reviewers and the rule.
        for added in [
            EscalationReason::AuthorizationBypass,
            EscalationReason::Injection,
        ] {
            assert!(added.rank() > EscalationReason::CrossTenant.rank());
            assert!(added.rank() < EscalationReason::ConflictingVerdicts.rank());
        }
        assert_eq!(
            EscalationReason::parse("authorization_bypass"),
            Some(EscalationReason::AuthorizationBypass)
        );
        assert_eq!(
            EscalationReason::parse("injection"),
            Some(EscalationReason::Injection)
        );
    }

    #[test]
    fn the_outlier_list_includes_the_methodology_leak() {
        assert_eq!(OutlierReason::ALL.len(), 13);
        assert_eq!(
            OutlierReason::parse("methodology_leak"),
            Some(OutlierReason::MethodologyLeak)
        );
        for r in OutlierReason::ALL {
            assert_eq!(OutlierReason::parse(r.as_str()), Some(r));
            assert_eq!(
                serde_json::to_value(r).unwrap(),
                serde_json::Value::String(r.as_str().to_string())
            );
        }
    }

    #[test]
    fn only_the_severe_reasons_and_unverified_evidence_may_exceed_the_budget() {
        let set = EscalationReason::default_over_budget_set();
        // R-3: secret exposure, data loss, cross-tenant access, authorization bypass and
        // injection go past the budget at high as well as critical.
        for reason in [
            EscalationReason::SecretExposure,
            EscalationReason::DataLoss,
            EscalationReason::CrossTenant,
            EscalationReason::AuthorizationBypass,
            EscalationReason::Injection,
        ] {
            assert!(reason.may_exceed_budget("critical", &set), "{reason:?}");
            assert!(reason.may_exceed_budget("high", &set), "{reason:?}");
            assert!(!reason.may_exceed_budget("medium", &set), "{reason:?}");
            assert!(!reason.may_exceed_budget("critical", &[]), "{reason:?}");
        }
        // Unauthenticated reach stays at critical: the group reviewer gives it to anything a
        // logged-out caller touches.
        assert!(EscalationReason::UnauthenticatedReach.may_exceed_budget("critical", &set));
        assert!(!EscalationReason::UnauthenticatedReach.may_exceed_budget("high", &set));
        assert!(EscalationReason::EvidenceUnverified.may_exceed_budget("low", &set));
        assert!(!EscalationReason::ConflictingVerdicts.may_exceed_budget("critical", &set));
        // A project can narrow the set.
        assert!(!EscalationReason::SecretExposure.may_exceed_budget("critical", &[]));
    }

    #[test]
    fn unverified_evidence_may_exceed_the_budget_whatever_the_stored_set_says() {
        // A project file written before the settings route refused such a set can hold an
        // over-budget set without it (serde fills a default only for a missing key). The release
        // gate refuses the row without a decision, so it is escalated all the same.
        for stored in [vec![], vec![EscalationReason::SecretExposure]] {
            assert!(EscalationReason::EvidenceUnverified.may_exceed_budget("low", &stored));
            assert!(
                !EscalationReason::SecretExposure.may_exceed_budget("medium", &stored),
                "the other reasons still obey the set and their severity"
            );
            assert!(
                !EscalationReason::SecretExposure.may_exceed_budget("critical", &[]),
                "and a set that leaves a reason out keeps it inside the budget"
            );
        }
        let mut line = EscalationBudget::compute(1.0, 1, 1000);
        let candidate = |reason| EscalationCandidate {
            key: String::new(),
            node_id: String::new(),
            reason,
            severity: "low".to_string(),
            stage2_rank: 1,
            global_rank: 1,
            slot: 0,
            escalated: false,
            over_budget_reason: None,
            why: String::new(),
        };
        let mut candidates = vec![
            candidate(EscalationReason::OutlierInGroup),
            candidate(EscalationReason::EvidenceUnverified),
        ];
        re_mark(&mut candidates, &mut line, &[]);
        assert!(candidates[0].escalated, "inside the budget");
        assert!(
            candidates[1].escalated,
            "over the budget, kept by unverified evidence"
        );
        assert_eq!(
            candidates[1].over_budget_reason,
            Some(EscalationReason::EvidenceUnverified)
        );
    }

    #[test]
    fn re_mark_escalates_the_first_budget_and_the_d5_reasons_past_it() {
        let mut candidates = vec![
            cand("a", EscalationReason::UnauthenticatedReach, "critical", 1),
            cand("b", EscalationReason::SecretExposure, "high", 2),
            cand("c", EscalationReason::SecretExposure, "critical", 3),
            cand("d", EscalationReason::ConflictingVerdicts, "critical", 4),
            cand("e", EscalationReason::EvidenceUnverified, "low", 5),
            cand("f", EscalationReason::RuleMisfit, "low", 6),
        ];
        let mut budget = EscalationBudget::compute(1.0, 2, 0);
        re_mark(
            &mut candidates,
            &mut budget,
            &EscalationReason::default_over_budget_set(),
        );
        let escalated: Vec<&str> = candidates
            .iter()
            .filter(|c| c.escalated)
            .map(|c| c.key.as_str())
            .collect();
        // a and b inside the budget of 2; c (critical secret) and e (unverified) over it.
        assert_eq!(escalated, vec!["a", "b", "c", "e"]);
        assert_eq!(candidates[0].over_budget_reason, None);
        assert_eq!(candidates[1].over_budget_reason, None);
        assert_eq!(
            candidates[2].over_budget_reason,
            Some(EscalationReason::SecretExposure)
        );
        assert_eq!(
            candidates[4].over_budget_reason,
            Some(EscalationReason::EvidenceUnverified)
        );
        // d (a critical disagreement) and f stay below the line, visible.
        assert!(!candidates[3].escalated && !candidates[5].escalated);
        assert_eq!(budget.budget, 2);
        assert_eq!(budget.escalated, 4);
        assert_eq!(budget.over_budget, 2);
        assert_eq!(budget.candidates, 6);
    }

    #[test]
    fn re_mark_counts_slots_and_lets_a_site_share_one() {
        // Two rows at slot 1, one at slot 2, one at slot 3. A budget of 2 slots escalates the
        // three rows of slots 1 and 2; slot 3 is past it, and one of its rows may exceed.
        let mut candidates = vec![
            cand("a", EscalationReason::UnauthenticatedReach, "high", 1),
            cand("b", EscalationReason::SecretExposure, "high", 2),
            cand("c", EscalationReason::DataLoss, "high", 3),
            cand("d", EscalationReason::RuleMisfit, "high", 4),
            cand("e", EscalationReason::OutlierInGroup, "critical", 5),
        ];
        for (c, slot) in candidates.iter_mut().zip([1, 1, 2, 3, 3]) {
            c.slot = slot;
        }
        let mut budget = EscalationBudget::compute(1.0, 2, 0);
        re_mark(&mut candidates, &mut budget, &[]);
        let escalated: Vec<&str> = candidates
            .iter()
            .filter(|c| c.escalated)
            .map(|c| c.key.as_str())
            .collect();
        assert_eq!(escalated, vec!["a", "b", "c"]);
        assert_eq!(
            (budget.budget, budget.escalated, budget.candidates),
            (2, 2, 3)
        );
        assert_eq!(budget.over_budget, 0);
        // Move the data-loss row into slot 3 at critical: the whole slot goes over the budget,
        // every row of it marked.
        candidates[2].slot = 3;
        candidates[2].severity = "critical".to_string();
        let mut budget = EscalationBudget::compute(1.0, 1, 0);
        re_mark(
            &mut candidates,
            &mut budget,
            &EscalationReason::default_over_budget_set(),
        );
        let over: Vec<(&str, Option<EscalationReason>)> = candidates
            .iter()
            .filter(|c| c.escalated && c.over_budget_reason.is_some())
            .map(|c| (c.key.as_str(), c.over_budget_reason))
            .collect();
        assert_eq!(
            over,
            vec![
                ("c", Some(EscalationReason::DataLoss)),
                ("d", Some(EscalationReason::DataLoss)),
                ("e", Some(EscalationReason::DataLoss)),
            ],
            "the slot is over budget as a whole and every row says why"
        );
        assert_eq!((budget.escalated, budget.over_budget), (2, 1));
    }

    #[test]
    fn re_mark_with_a_new_rate_moves_the_line_and_clears_the_old_marks() {
        let mut candidates = vec![
            cand("a", EscalationReason::ConflictingVerdicts, "high", 1),
            cand("b", EscalationReason::OutlierInGroup, "high", 2),
            cand("c", EscalationReason::RuleMisfit, "high", 3),
        ];
        let set = EscalationReason::default_over_budget_set();
        let mut budget = EscalationBudget::compute(1.0, 1, 0);
        re_mark(&mut candidates, &mut budget, &set);
        assert_eq!(budget.escalated, 1);
        // Raise the line: the floor of 3 takes all three; lower it again and only one stays.
        let mut budget = EscalationBudget::compute(1.0, 3, 0);
        re_mark(&mut candidates, &mut budget, &set);
        assert!(candidates.iter().all(|c| c.escalated));
        assert_eq!(budget.over_budget, 0);
        let mut budget = EscalationBudget::compute(1.0, 1, 0);
        re_mark(&mut candidates, &mut budget, &set);
        assert_eq!(candidates.iter().filter(|c| c.escalated).count(), 1);
        assert!(candidates[1..]
            .iter()
            .all(|c| c.over_budget_reason.is_none()));
    }

    #[test]
    fn a_review_level_round_trips_through_its_wire_form() {
        for level in [
            ReviewLevel::PersonallyReviewed,
            ReviewLevel::ReviewedAsPattern("sub:SEC-RLS-1/3".to_string()),
            ReviewLevel::EngineVerified,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            assert_eq!(json, format!("\"{}\"", level.as_wire()));
            assert_eq!(serde_json::from_str::<ReviewLevel>(&json).unwrap(), level);
        }
        assert_eq!(ReviewLevel::parse("reviewed_as_pattern:"), None);
        assert_eq!(ReviewLevel::parse("reviewed"), None);
    }
}
