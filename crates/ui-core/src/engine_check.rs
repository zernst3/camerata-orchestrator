//! The Engine Check lane's judgments (review UI spec sections 7 and 11; design A2b, A9 and A10).
//!
//! The Engine Check is the architect's own quality control of the engine: one card per sampled
//! row, one question ("Did the engine get this right?"), Yes or No, and a No opens the correction
//! in place. Everything the lane decides is a pure function here with a unit test (spec rule 2):
//! the card flow, the requests it makes, what a card stands in (pending, held, corrected, carried
//! from an earlier run), the header sentences and the honest labels on every rate. The view in
//! `camerata-ui` chooses words and pixels and calls these.
//!
//! # Honest labels (design A10 item 6)
//!
//! An Engine Check rate is the error rate "among reported findings", and the thrown-out rate is
//! "real problems among thrown-out findings". The check cannot see problems the engine missed
//! entirely, so no string here uses the word that claims it can (a source-scan test in
//! `camerata-ui` keeps it that way).

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{EditField, NewEditReq};
use camerata_api_types::review_view::{
    CategoryAllocation, CorrectionRef, EngineCheckCard, EngineCheckError, EngineCheckLane,
    EngineCheckVerdict, EngineCheckVerdictReq, EscalationStatus, HoldReason, SampleStratum,
    StratumRate, SAMPLE_CATEGORY_FLOOR, SMALL_CATEGORY_DRAWN,
};

use crate::tree::export::Door;
use crate::tree::{outlier_phrase, thousands};
use crate::triage::{FocusMove, KeyMods, PaneFocus};

// ── The closed list ───────────────────────────────────────────────────────────────────────

/// The words for one "what was wrong" choice.
pub fn error_label(e: EngineCheckError) -> &'static str {
    match e {
        EngineCheckError::NotARealIssue => "Not a real issue",
        EngineCheckError::WrongSeverity => "Wrong severity",
        EngineCheckError::DuplicateOfAnotherRow => "Duplicate of another row",
        EngineCheckError::ShouldHaveBeenEscalated => "Should have been escalated",
        EngineCheckError::ShouldNotHaveBeenEscalated => "Should not have been escalated",
        EngineCheckError::WrongGrouping => "Wrong grouping",
        EngineCheckError::WordingOrFixWrong => "Wording or fix wrong",
        EngineCheckError::GroupSummaryDoesNotDescribe => {
            "Group summary does not describe this finding"
        }
        EngineCheckError::ThrownOutRealProblem => "Thrown out, but a real problem",
    }
}

/// The digit that picks a choice from the open list (`1` to `8`).
pub fn error_digit(e: EngineCheckError) -> Option<char> {
    EngineCheckError::ALL
        .iter()
        .position(|x| *x == e)
        .and_then(|i| char::from_digit(u32::try_from(i + 1).ok()?, 10))
}

/// The choice a digit picks.
pub fn error_for_digit(key: &str) -> Option<EngineCheckError> {
    let mut chars = key.chars();
    let digit = chars.next()?.to_digit(10)?;
    if chars.next().is_some() || digit == 0 {
        return None;
    }
    EngineCheckError::ALL
        .get(usize::try_from(digit).ok()? - 1)
        .copied()
}

// ── What a card stands in ─────────────────────────────────────────────────────────────────

/// Where a card stands: the states the lane draws (spec section 7, "States"), with the two a
/// thrown-out row can be in apart from a kept row's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardState {
    /// A fresh card, no verdict.
    Pending,
    /// Yes: the engine got it right.
    Yes,
    /// A No with no correction yet. The row is held from the client export. `carried_from` is the
    /// run the No was given on, when it was given on an earlier one.
    Held {
        error: Option<EngineCheckError>,
        carried_from: Option<String>,
    },
    /// A No on a thrown-out row that nothing has acted on, when the gate's hold is not on the card
    /// (the server holds such a row, so a card it has read reads [`CardState::Held`]): the row is
    /// still thrown out.
    ThrownOut,
    /// A thrown-out row the reviewer put back: it is back in review and held until it is decided.
    Restored,
    /// A No, and a correction or a later decision or edit stands behind it.
    Corrected { error: Option<EngineCheckError> },
}

/// Whether `later` is strictly after `earlier` (RFC 3339), the rule the release gate uses: a time
/// that cannot be read, or the same instant, is not later, so a doubt never reads as corrected.
fn strictly_later(later: &str, earlier: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(later),
        chrono::DateTime::parse_from_rfc3339(earlier),
    ) {
        (Ok(later), Ok(earlier)) => later > earlier,
        _ => false,
    }
}

/// Whether something corrected the No: the verdict names a correction, or a decision or an edit
/// was made on the row after the verdict (the gate's own test, see `uncorrected_keys`).
fn corrected_since(card: &EngineCheckCard, v: &EngineCheckVerdict) -> bool {
    v.correction.is_some()
        || card
            .site
            .decision
            .as_ref()
            .is_some_and(|d| strictly_later(&d.decided_at, &v.at))
        || card.site.edits.iter().any(|e| strictly_later(&e.at, &v.at))
}

/// The state of a card, read from the verdict, from whether the gate holds the row and from
/// whether anything corrected it. A No is "corrected" only when something did; the absence of a
/// hold proves nothing (a thrown-out row with an uncorrected No is held too, but a card that
/// does not carry the hold must still not read as corrected).
pub fn card_state(card: &EngineCheckCard) -> CardState {
    match &card.verdict {
        None => CardState::Pending,
        Some(v) if v.right => CardState::Yes,
        Some(v) => {
            let held = |error| CardState::Held {
                error,
                carried_from: v.carried_from.clone(),
            };
            match card.site.held {
                Some(HoldReason::EngineCheckNoWithoutCorrection) => held(v.wrong),
                Some(HoldReason::RestoredWithoutDecision) => CardState::Restored,
                _ if corrected_since(card, v) => CardState::Corrected { error: v.wrong },
                _ if card.stratum == SampleStratum::ThrownOut => CardState::ThrownOut,
                _ => held(v.wrong),
            }
        }
    }
}

/// The line a card states its state in.
pub fn card_state_text(state: &CardState) -> String {
    match state {
        CardState::Pending => "No verdict yet".to_string(),
        CardState::Yes => "Yes: the engine got this right".to_string(),
        CardState::Held {
            carried_from: Some(_),
            error,
        } => with_error(
            "No from an earlier run, still uncorrected: held from export until corrected",
            *error,
        ),
        CardState::Held {
            carried_from: None,
            error,
        } => with_error("No: held from export until corrected", *error),
        CardState::ThrownOut => "No: still thrown out, put it back to correct".to_string(),
        CardState::Restored => "No: put back into review, held until you decide it".to_string(),
        CardState::Corrected { error } => {
            with_error("No, corrected: the corrected version is what ships", *error)
        }
    }
}

fn with_error(text: &str, error: Option<EngineCheckError>) -> String {
    match error {
        Some(e) => format!("{text} ({})", error_label(e).to_lowercase()),
        None => text.to_string(),
    }
}

/// What was done to correct a No, in words, for the card's correction summary (spec section 7:
/// "the card shows the correction summary"): the ruling the row now has with its reason, and each
/// edit made, when the verdict names them or they were made after it. Empty when nothing was.
pub fn correction_lines(card: &EngineCheckCard) -> Vec<String> {
    let Some(v) = &card.verdict else {
        return Vec::new();
    };
    if v.right {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let named_decision = match &v.correction {
        Some(CorrectionRef::Decision(id)) => Some(id.as_str()),
        _ => None,
    };
    if let Some(d) =
        card.site.decision.as_ref().filter(|d| {
            named_decision == Some(d.id.as_str()) || strictly_later(&d.decided_at, &v.at)
        })
    {
        let label = crate::tree::decision_label(&d.state);
        let reason = d.reason.trim();
        lines.push(if reason.is_empty() {
            label.to_string()
        } else {
            format!("{label}: {reason}")
        });
    }
    let named_edit = match &v.correction {
        Some(CorrectionRef::Edit(id)) => Some(id.as_str()),
        _ => None,
    };
    for e in card
        .site
        .edits
        .iter()
        .filter(|e| named_edit == Some(e.id.as_str()) || strictly_later(&e.at, &v.at))
    {
        let change = format!("{}: {} to {}", field_label(e.field), e.old, e.new);
        let reason = e.reason.trim();
        lines.push(if reason.is_empty() {
            change
        } else {
            format!("{change} ({reason})")
        });
    }
    lines
}

/// Whether this card has been answered on THIS run. A No carried from an earlier run is not an
/// answer given here: the card still wants one.
pub fn answered(card: &EngineCheckCard) -> bool {
    card.verdict
        .as_ref()
        .is_some_and(|v| v.carried_from.is_none())
}

/// Cards whose row is held from export by an Engine Check No with no correction: the shell's Held
/// count includes these, and the export panel lists them.
pub fn held_count(lane: &EngineCheckLane) -> usize {
    lane.cards
        .iter()
        .filter(|c| matches!(card_state(c), CardState::Held { .. }))
        .count()
}

/// The card an export-panel door asks for, if the lane has it.
pub fn card_for_door(lane: &EngineCheckLane, door: &Door) -> Option<usize> {
    let Door::EngineCard { key } = door else {
        return None;
    };
    lane.cards.iter().position(|c| c.site.site_key == *key)
}

// ── The card's two panels ─────────────────────────────────────────────────────────────────

/// `high · real yes · effort low` for the Stage 1 line of "What the engine did".
pub fn stage1_text(card: &EngineCheckCard) -> String {
    let s = &card.stage1;
    if s.real.is_empty() {
        return "Stage 1 did not read this row".to_string();
    }
    let mut parts = vec![s.severity.clone(), format!("real {}", s.real)];
    if let Some(effort) = s.effort.as_deref().filter(|e| !e.is_empty()) {
        parts.push(format!("effort {effort}"));
    }
    parts.join(" \u{b7} ")
}

/// Where the engine placed the row: `Authorization > SEC-RLS-1 > sub 1`.
pub fn placed_text(card: &EngineCheckCard) -> String {
    card.placement
        .node_path
        .iter()
        .map(|c| c.label.as_str())
        .collect::<Vec<_>>()
        .join(" > ")
}

/// What the escalation did with the row.
pub fn escalation_text(status: &EscalationStatus) -> String {
    match status {
        EscalationStatus::NotCandidate => "not a candidate".to_string(),
        EscalationStatus::BelowLine { rank } => {
            format!(
                "a candidate below the line (rank {})",
                thousands(*rank as usize)
            )
        }
        EscalationStatus::Escalated { reason, rank } => format!(
            "escalated for {} (rank {})",
            reason.phrase(),
            thousands(*rank as usize)
        ),
        EscalationStatus::OverBudget { reason, rank } => format!(
            "escalated past the budget for {} (rank {})",
            reason.phrase(),
            thousands(*rank as usize)
        ),
    }
}

/// `eslint/no-anon-select (src/a.ts:14)` for each source collapsed into the row; "none" when the
/// row collapsed nothing.
pub fn collapsed_text(card: &EngineCheckCard) -> String {
    if card.flagged_by.is_empty() {
        return "none".to_string();
    }
    card.flagged_by
        .iter()
        .map(|f| match f.tool.as_deref().filter(|t| !t.is_empty()) {
            Some(tool) => format!("{tool}/{} ({}:{})", f.rule_id, f.path, f.line),
            None => format!("{} ({}:{})", f.rule_id, f.path, f.line),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The duplicate link, or "none".
pub fn duplicate_text(card: &EngineCheckCard) -> String {
    match &card.duplicate_link {
        Some(d) => format!("{} at {}:{}", d.rule_id, d.path, d.line),
        None => "none".to_string(),
    }
}

/// The outlier marks, or "none".
pub fn outlier_text(card: &EngineCheckCard) -> String {
    if card.outliers.is_empty() {
        return "none".to_string();
    }
    card.outliers
        .iter()
        .map(|o| {
            if o.why.is_empty() {
                outlier_phrase(o.reason).to_string()
            } else {
                format!("{}: {}", outlier_phrase(o.reason), o.why)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The review flags, or "none".
pub fn flags_text(card: &EngineCheckCard) -> String {
    if card.review_flags.is_empty() {
        "none".to_string()
    } else {
        card.review_flags.join("; ")
    }
}

/// The stratum chip on a thrown-out card.
pub fn stratum_label(stratum: SampleStratum) -> Option<&'static str> {
    match stratum {
        SampleStratum::Kept => None,
        SampleStratum::ThrownOut => Some("thrown out by the engine"),
    }
}

/// The question over a card, in its own words for each stratum.
pub fn question(card: &EngineCheckCard) -> &'static str {
    match card.stratum {
        SampleStratum::Kept => "Did the engine get this right?",
        SampleStratum::ThrownOut => "Was the engine right to throw this out?",
    }
}

// ── The lane header ───────────────────────────────────────────────────────────────────────

/// The title line under the lane tab.
pub const SUBTITLE: &str = "quality control of the engine, internal only, never in a client export";

/// What the lane says when the sample size is 0.
pub const OFF_TEXT: &str = "Engine Check is off: the sample size is 0, so no rows are drawn. Set a sample size in Settings to turn it on.";

/// What the lane says when it is on and the run has no card.
pub const NO_CARDS: &str = "No rows to check in this run: the sample drew none.";

/// Categories with at least the floor of unread rows: the ones the floor binds.
fn floor_categories(lane: &EngineCheckLane) -> usize {
    lane.allocation
        .iter()
        .filter(|a| a.unread >= SAMPLE_CATEGORY_FLOOR)
        .count()
}

/// The sample line. Normally `Sample 50 of this run's 27,900 unread rows (setting: 50)`. When
/// the per-category floor puts the sample above the setting, the floor wins and the line says so
/// (ruling 2 of 2026-11-04): `180 cards: at least 3 from each of 60 categories, above your
/// sample size of 50`.
pub fn sample_line(lane: &EngineCheckLane) -> String {
    let categories = floor_categories(lane);
    if lane.setting > 0 && lane.drawn > lane.setting && categories > 0 {
        return format!(
            "{} cards: at least {SAMPLE_CATEGORY_FLOOR} from each of {} categories, above your sample size of {}",
            thousands(lane.drawn),
            thousands(categories),
            thousands(lane.setting)
        );
    }
    format!(
        "Sample {} of this run's {} unread rows (setting: {})",
        thousands(lane.drawn),
        thousands(lane.unread_total),
        thousands(lane.setting)
    )
}

/// How the sample was shared out: shown beside the sample line unless the floor line already says it.
pub fn allocation_note(lane: &EngineCheckLane) -> Option<String> {
    let floor_binds = lane.setting > 0 && lane.drawn > lane.setting && floor_categories(lane) > 0;
    let mut parts: Vec<String> = Vec::new();
    if !floor_binds {
        parts.push(format!(
            "Allocated by unread share, at least {SAMPLE_CATEGORY_FLOOR} per category"
        ));
    }
    let thrown = lane
        .cards
        .iter()
        .filter(|c| c.stratum == SampleStratum::ThrownOut)
        .count();
    if thrown > 0 {
        parts.push(format!(
            "{} of the {} rows the engine threw out are in the sample",
            thousands(thrown),
            thousands(lane.thrown_out_total)
        ));
    }
    (!parts.is_empty()).then(|| parts.join(". "))
}

/// `Authorization 22 · Secret exposure 3 · Data loss 9`: the categories that drew cards.
pub fn allocation_line(lane: &EngineCheckLane) -> String {
    lane.allocation
        .iter()
        .filter(|a| a.drawn > 0)
        .map(|a| format!("{} {}", a.category, thousands(a.drawn)))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// A rate and its margin: `13% ± 12`. A small category adds `, indicative only`.
pub fn rate_text(rate: Option<f64>, margin: Option<f64>, indicative: bool) -> String {
    let (Some(rate), Some(margin)) = (rate, margin) else {
        return "no verdicts yet".to_string();
    };
    let base = format!("{rate:.0}% \u{b1} {margin:.0}");
    if indicative {
        format!("{base}, indicative only")
    } else {
        base
    }
}

/// The hover text of one category: its draw, its checks and its rate.
pub fn category_tip(a: &CategoryAllocation) -> String {
    format!(
        "{}: {} drawn of {} unread; {} checked, {} wrong; error rate among reported findings {}",
        a.category,
        thousands(a.drawn),
        thousands(a.unread),
        thousands(a.verdicts),
        thousands(a.wrong),
        rate_text(a.rate_percent, a.margin_percent, a.indicative_only)
    )
}

/// How many cards have an answer given on this run, of how many were drawn.
pub fn verdicts_line(lane: &EngineCheckLane) -> String {
    let done = lane.cards.iter().filter(|c| answered(c)).count();
    format!(
        "Verdicts {} of {}",
        thousands(done),
        thousands(lane.cards.len())
    )
}

/// Every card has an answer given on this run.
pub fn all_verdicts_in(lane: &EngineCheckLane) -> bool {
    !lane.cards.is_empty() && lane.cards.iter().all(answered)
}

/// One rate line: its label, its text and the stratum it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLine {
    pub stratum: SampleStratum,
    pub label: &'static str,
    pub text: String,
}

fn stratum_rate_text(s: &StratumRate) -> String {
    if s.verdicts == 0 {
        return format!("no verdicts yet ({} drawn)", thousands(s.drawn));
    }
    format!(
        "{} ({} of {} checked)",
        rate_text(
            s.rate_percent,
            s.margin_percent,
            s.verdicts < SMALL_CATEGORY_DRAWN
        ),
        thousands(s.wrong),
        thousands(s.verdicts)
    )
}

/// The rates, one line per stratum that drew a card, never pooled: the kept rows' "error rate
/// among reported findings" and the thrown-out rows' "real problems among thrown-out findings"
/// (design A10 items 2 and 6).
pub fn rate_lines(lane: &EngineCheckLane) -> Vec<RateLine> {
    lane.strata
        .iter()
        .filter(|s| s.drawn > 0)
        .map(|s| RateLine {
            stratum: s.stratum,
            label: match s.stratum {
                SampleStratum::Kept => "Error rate among reported findings",
                SampleStratum::ThrownOut => "Real problems among thrown-out findings",
            },
            text: stratum_rate_text(s),
        })
        .collect()
}

/// `Card 32 of 50`.
pub fn card_position(index: usize, count: usize) -> String {
    format!("Card {} of {}", index + 1, count)
}

// ── The lane's own state ──────────────────────────────────────────────────────────────────

/// What kind of correction a choice opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionKind {
    /// Mark the row a false positive (reason).
    FalsePositive,
    /// Change the severity (select, reason).
    Severity,
    /// Merge into another row (target picker, reason).
    Merge,
    /// Escalate: a site decision that says the reviewer needed it.
    Escalate,
    /// De-escalate: a site decision that says the reviewer did not.
    Deescalate,
    /// Move to another sub-group (name, reason).
    Subgroup,
    /// Edit the headline, the detail or the fix (field, text, reason).
    Text,
    /// Edit the group summary (text, reason).
    Summary,
    /// Put back a row the engine threw out (reason).
    Restore,
}

/// The correction a choice opens.
pub fn kind_of(e: EngineCheckError) -> CorrectionKind {
    match e {
        EngineCheckError::NotARealIssue => CorrectionKind::FalsePositive,
        EngineCheckError::WrongSeverity => CorrectionKind::Severity,
        EngineCheckError::DuplicateOfAnotherRow => CorrectionKind::Merge,
        EngineCheckError::ShouldHaveBeenEscalated => CorrectionKind::Escalate,
        EngineCheckError::ShouldNotHaveBeenEscalated => CorrectionKind::Deescalate,
        EngineCheckError::WrongGrouping => CorrectionKind::Subgroup,
        EngineCheckError::WordingOrFixWrong => CorrectionKind::Text,
        EngineCheckError::GroupSummaryDoesNotDescribe => CorrectionKind::Summary,
        EngineCheckError::ThrownOutRealProblem => CorrectionKind::Restore,
    }
}

/// The severities the select offers.
pub const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "info"];

/// The fields the wording correction can edit.
pub const TEXT_FIELDS: [EditField; 3] = [EditField::Headline, EditField::Detail, EditField::Fix];

/// A correction being filled in: what the form holds before it is committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub error: EngineCheckError,
    pub kind: CorrectionKind,
    /// For a [`CorrectionKind::Text`] edit, the field being edited.
    pub field: EditField,
    /// The severity, the merge target's site key, the sub-group name or the text, by kind.
    pub value: String,
    pub reason: String,
}

impl Correction {
    /// The form for `error` on `card`, filled with what the row has now.
    pub fn start(error: EngineCheckError, card: &EngineCheckCard) -> Self {
        let kind = kind_of(error);
        let field = EditField::Headline;
        let value = match kind {
            CorrectionKind::Severity => card.site.severity.clone(),
            CorrectionKind::Merge => card
                .duplicate_link
                .as_ref()
                .map(|d| d.key.clone())
                .unwrap_or_default(),
            CorrectionKind::Subgroup => card.placement.subgroup_label.clone(),
            CorrectionKind::Text => field_text(card, field),
            CorrectionKind::Summary => card.placement.summary.clone().unwrap_or_default(),
            CorrectionKind::FalsePositive
            | CorrectionKind::Escalate
            | CorrectionKind::Deescalate
            | CorrectionKind::Restore => String::new(),
        };
        Self {
            error,
            kind,
            field,
            value,
            reason: String::new(),
        }
    }

    /// Switch the field a wording correction edits; the text starts from the row's own.
    pub fn with_field(mut self, field: EditField, card: &EngineCheckCard) -> Self {
        self.field = field;
        self.value = field_text(card, field);
        self
    }

    /// The heading of the form.
    pub fn title(&self) -> &'static str {
        match self.kind {
            CorrectionKind::FalsePositive => "Mark it a false positive",
            CorrectionKind::Severity => "Change the severity",
            CorrectionKind::Merge => "Merge it into another row",
            CorrectionKind::Escalate => "Escalate it: you needed to see this row",
            CorrectionKind::Deescalate => {
                "Take it out of the queue: you did not need to see this row"
            }
            CorrectionKind::Subgroup => "Move it to another sub-group",
            CorrectionKind::Text => "Edit its wording",
            CorrectionKind::Summary => "Edit the group summary",
            CorrectionKind::Restore => "Put it back into review",
        }
    }

    /// Whether the correction needs a reason before it is committed. Escalating and
    /// de-escalating are rulings that need none.
    pub fn needs_reason(&self) -> bool {
        !matches!(
            self.kind,
            CorrectionKind::Escalate | CorrectionKind::Deescalate
        )
    }
}

/// The text of one editable field of the row.
pub fn field_text(card: &EngineCheckCard, field: EditField) -> String {
    match field {
        EditField::Detail => card.site.detail.clone(),
        EditField::Fix => card.site.fix.clone(),
        _ => card.site.headline.clone(),
    }
}

/// The word for a field in the select.
pub fn field_label(field: EditField) -> &'static str {
    match field {
        EditField::Headline => "Headline",
        EditField::Detail => "Detail",
        EditField::Fix => "Fix",
        EditField::Severity => "Severity",
        EditField::Subgroup => "Sub-group",
        EditField::Summary => "Summary",
        EditField::FixType => "Fix type",
        EditField::RuleId => "Rule id",
        EditField::Kind => "Kind",
        EditField::Category => "Category",
    }
}

/// What a committed correction posts.
#[derive(Debug, Clone, PartialEq)]
pub enum CorrectionRequest {
    Decision(NewDecisionReq),
    Edit(NewEditReq),
}

/// The correction as a request, or the words for what is missing. Nothing is posted by this; it
/// is the host that posts and then records the verdict with the correction's id.
pub fn correction_request(
    c: &Correction,
    card: &EngineCheckCard,
    run_id: &str,
) -> Result<CorrectionRequest, String> {
    let key = card.site.site_key.as_str();
    let reason = c.reason.trim();
    let value = c.value.trim();
    let need_reason = |what: &str| -> Result<(), String> {
        if reason.is_empty() {
            Err(format!("A reason is required to {what}."))
        } else {
            Ok(())
        }
    };
    let decision = |state: DecisionState, needed_me: Option<bool>, reason: &str| {
        CorrectionRequest::Decision(NewDecisionReq {
            scope: Scope::Site,
            key: key.to_string(),
            state,
            reason: reason.to_string(),
            decided_by: String::new(),
            needed_me,
            sample_verdict: None,
            except: Vec::new(),
            run_id: run_id.to_string(),
            decided_at: String::new(),
        })
    };
    let edit = |field: EditField, new: &str, edit_key: &str| {
        CorrectionRequest::Edit(NewEditReq {
            key: edit_key.to_string(),
            field,
            new: new.to_string(),
            reason: reason.to_string(),
            by: String::new(),
            run_id: run_id.to_string(),
        })
    };
    match c.kind {
        CorrectionKind::FalsePositive => {
            need_reason("call this row a false positive")?;
            Ok(decision(DecisionState::FalsePositive, None, reason))
        }
        CorrectionKind::Restore => {
            need_reason("put back a finding the engine threw out")?;
            Ok(decision(DecisionState::EngineRestored, None, reason))
        }
        CorrectionKind::Severity => {
            if !SEVERITIES.contains(&value) {
                return Err("Pick a severity.".to_string());
            }
            if value == card.site.severity {
                return Err(format!(
                    "The severity is already {value}. Pick the one it should be."
                ));
            }
            need_reason("change the severity")?;
            Ok(edit(EditField::Severity, value, key))
        }
        CorrectionKind::Merge => {
            if value.is_empty() {
                return Err("Pick the row this one duplicates.".to_string());
            }
            if value == key {
                return Err("A row cannot be merged into itself.".to_string());
            }
            need_reason("merge this row into another")?;
            Ok(decision(
                DecisionState::MergedInto(value.to_string()),
                None,
                reason,
            ))
        }
        CorrectionKind::Escalate => {
            keep_ruling(card, reason).map(|(state, reason)| decision(state, Some(true), &reason))
        }
        CorrectionKind::Deescalate => {
            keep_ruling(card, reason).map(|(state, reason)| decision(state, Some(false), &reason))
        }
        CorrectionKind::Subgroup => {
            if value.is_empty() {
                return Err("Name the sub-group to move this row to.".to_string());
            }
            if value == card.placement.subgroup_label {
                return Err("The row is already in that sub-group.".to_string());
            }
            need_reason("move this row to another sub-group")?;
            Ok(edit(EditField::Subgroup, value, key))
        }
        CorrectionKind::Text => {
            if value.is_empty() {
                return Err(format!(
                    "The {} cannot be empty.",
                    field_label(c.field).to_lowercase()
                ));
            }
            if value == field_text(card, c.field).trim() {
                return Err(format!(
                    "The {} is unchanged. Edit it, or pick another correction.",
                    field_label(c.field).to_lowercase()
                ));
            }
            need_reason("edit this row's wording")?;
            Ok(edit(c.field, value, key))
        }
        CorrectionKind::Summary => {
            let Some(node) = card.placement.node_path.last() else {
                return Err("This row has no sub-group whose summary can be edited.".to_string());
            };
            if value.is_empty() {
                return Err("The summary cannot be empty.".to_string());
            }
            if card.placement.summary.as_deref().map(str::trim) == Some(value) {
                return Err("The summary is unchanged.".to_string());
            }
            need_reason("edit the group summary")?;
            Ok(edit(EditField::Summary, value, &node.id))
        }
    }
}

/// The ruling an escalate or de-escalate correction posts: the one the row already has, with its
/// reason when the correction gives none, or Confirmed when the row has none. Escalating or
/// de-escalating says whether the reviewer needed the row; it never replaces a ruling (Ignored,
/// False positive, tech debt) the reviewer did not choose to change.
fn keep_ruling(card: &EngineCheckCard, reason: &str) -> Result<(DecisionState, String), String> {
    let (state, kept_reason) = match &card.site.decision {
        Some(d)
            if matches!(
                d.state,
                DecisionState::Confirmed
                    | DecisionState::Ignored
                    | DecisionState::FalsePositive
                    | DecisionState::TechDebtNow
                    | DecisionState::TechDebtLater
            ) =>
        {
            (d.state.clone(), d.reason.clone())
        }
        _ => (DecisionState::Confirmed, String::new()),
    };
    let reason = if reason.is_empty() {
        kept_reason.as_str()
    } else {
        reason
    };
    if matches!(state, DecisionState::Ignored | DecisionState::FalsePositive)
        && reason.trim().is_empty()
    {
        return Err("A reason is required to keep this ruling.".to_string());
    }
    Ok((state, reason.to_string()))
}

/// The correction a recorded id stands for.
pub fn correction_ref(request: &CorrectionRequest, id: &str) -> CorrectionRef {
    match request {
        CorrectionRequest::Decision(_) => CorrectionRef::Decision(id.to_string()),
        CorrectionRequest::Edit(_) => CorrectionRef::Edit(id.to_string()),
    }
}

/// The verdict a Yes posts.
pub fn yes_request() -> EngineCheckVerdictReq {
    EngineCheckVerdictReq {
        right: true,
        wrong: None,
        correction: None,
        by: String::new(),
    }
}

/// The verdict a No posts: what was wrong, and the correction when there is one. A No with
/// `None` holds the row from the client export until it is corrected.
pub fn no_request(
    wrong: EngineCheckError,
    correction: Option<CorrectionRef>,
) -> EngineCheckVerdictReq {
    EngineCheckVerdictReq {
        right: false,
        wrong: Some(wrong),
        correction,
        by: String::new(),
    }
}

/// The merge targets a card offers: the other rows of its sub-group, the link the engine found
/// first. `siblings` are `(site key, label)`; the card's own row is left out.
pub fn merge_targets(
    card: &EngineCheckCard,
    siblings: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(d) = &card.duplicate_link {
        out.push((
            d.key.clone(),
            format!("{} at {}:{} (the engine's link)", d.rule_id, d.path, d.line),
        ));
    }
    for (key, label) in siblings {
        if *key != card.site.site_key && !out.iter().any(|(k, _)| k == key) {
            out.push((key.clone(), label.clone()));
        }
    }
    out
}

/// Where the lane is in the card flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Looking at the card.
    Viewing,
    /// The closed list of what was wrong is open; `highlighted` is the entry Enter picks.
    Picking { highlighted: usize },
    /// A choice was made and its correction is being filled in.
    Correcting(Correction),
}

/// The lane's own state: which card, which step of its flow, and what went wrong last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneState {
    pub card: usize,
    pub phase: Phase,
    /// A write is in flight.
    pub busy: bool,
    /// The last write's refusal, in the server's words.
    pub error: Option<String>,
}

impl Default for LaneState {
    fn default() -> Self {
        Self {
            card: 0,
            phase: Phase::Viewing,
            busy: false,
            error: None,
        }
    }
}

impl LaneState {
    /// The No still waiting for its correction, if the reviewer is mid-correction: leaving the
    /// card (Esc, Next, Prev) stores it with no correction, which holds the row from export.
    pub fn pending_no(&self) -> Option<EngineCheckError> {
        match &self.phase {
            Phase::Correcting(c) => Some(c.error),
            _ => None,
        }
    }

    /// Go to card `to` (clamped to the lane). The flow starts again on the new card.
    pub fn go(&mut self, to: usize, count: usize) {
        self.card = to.min(count.saturating_sub(1));
        self.phase = Phase::Viewing;
        self.error = None;
    }

    /// The next card, stopping at the last.
    pub fn next(&mut self, count: usize) {
        self.go(self.card + 1, count);
    }

    /// The previous card, stopping at the first.
    pub fn prev(&mut self, count: usize) {
        self.go(self.card.saturating_sub(1), count);
    }

    /// `N`: open the closed list of what was wrong.
    pub fn open_list(&mut self) {
        self.phase = Phase::Picking { highlighted: 0 };
        self.error = None;
    }

    /// Move the highlight in the open list.
    pub fn move_highlight(&mut self, dir: FocusMove) {
        if let Phase::Picking { highlighted } = &mut self.phase {
            let last = EngineCheckError::ALL.len() - 1;
            *highlighted = match dir {
                FocusMove::Down => (*highlighted + 1).min(last),
                FocusMove::Up => highlighted.saturating_sub(1),
            };
        }
    }

    /// Pick a choice from the open list: the correction for it opens on `card`.
    pub fn pick(&mut self, error: EngineCheckError, card: &EngineCheckCard) {
        self.phase = Phase::Correcting(Correction::start(error, card));
        self.error = None;
    }

    /// Enter in the open list picks the highlighted entry.
    pub fn pick_highlighted(&mut self, card: &EngineCheckCard) {
        if let Phase::Picking { highlighted } = self.phase {
            if let Some(e) = EngineCheckError::ALL.get(highlighted) {
                self.pick(*e, card);
            }
        }
    }

    /// Esc: close the list or the form. Returns the No that must now be stored with no
    /// correction when a correction was being filled in.
    pub fn cancel(&mut self) -> Option<EngineCheckError> {
        let pending = self.pending_no();
        self.phase = Phase::Viewing;
        self.error = None;
        pending
    }

    /// Point the lane at the card for `door`, if the lane has it.
    pub fn focus_door(&mut self, lane: &EngineCheckLane, door: &Door) -> bool {
        match card_for_door(lane, door) {
            Some(i) => {
                self.go(i, lane.cards.len());
                true
            }
            None => false,
        }
    }
}

// ── Keys ──────────────────────────────────────────────────────────────────────────────────

/// Which step of the flow the keys are read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Viewing,
    Picking,
    Correcting,
}

impl From<&Phase> for KeyPhase {
    fn from(p: &Phase) -> Self {
        match p {
            Phase::Viewing => Self::Viewing,
            Phase::Picking { .. } => Self::Picking,
            Phase::Correcting(_) => Self::Correcting,
        }
    }
}

/// What a key press asks for in the Engine Check lane (spec section 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCommand {
    Prev,
    Next,
    Yes,
    No,
    /// `1` to `8` in the open list.
    Pick(EngineCheckError),
    Highlight(FocusMove),
    /// Enter in the open list.
    PickHighlighted,
    /// Enter in a correction: commit it.
    Commit,
    /// Esc: close the list, or leave the correction (the No is stored uncorrected).
    Cancel,
    /// `G`: go to the card's node in the tree.
    GoToNode,
    /// `M`: open the merge correction.
    Merge,
}

/// The Engine Check lane's key map as a pure function. Same scoping as every lane: the keys fire
/// only while the lane's region holds focus, never in a text field, never with Ctrl, Alt or Cmd.
pub fn engine_command(
    focus: PaneFocus,
    key: &str,
    mods: KeyMods,
    phase: KeyPhase,
) -> Option<EngineCommand> {
    if focus != PaneFocus::Cursor || mods.ctrl || mods.alt || mods.meta {
        return None;
    }
    match (phase, key) {
        (KeyPhase::Viewing, "ArrowDown") => return Some(EngineCommand::Next),
        (KeyPhase::Viewing, "ArrowUp") => return Some(EngineCommand::Prev),
        (KeyPhase::Picking, "ArrowDown") => return Some(EngineCommand::Highlight(FocusMove::Down)),
        (KeyPhase::Picking, "ArrowUp") => return Some(EngineCommand::Highlight(FocusMove::Up)),
        (KeyPhase::Picking, "Enter") => return Some(EngineCommand::PickHighlighted),
        (KeyPhase::Correcting, "Enter") => return Some(EngineCommand::Commit),
        (KeyPhase::Picking | KeyPhase::Correcting, "Escape") => return Some(EngineCommand::Cancel),
        _ => {}
    }
    if key.chars().count() != 1 {
        return None;
    }
    match phase {
        KeyPhase::Picking => error_for_digit(key).map(EngineCommand::Pick),
        KeyPhase::Viewing => match key.to_ascii_lowercase().as_str() {
            "y" => Some(EngineCommand::Yes),
            "n" => Some(EngineCommand::No),
            "g" => Some(EngineCommand::GoToNode),
            "m" => Some(EngineCommand::Merge),
            _ => None,
        },
        KeyPhase::Correcting => None,
    }
}

/// Whether the lane's keys will fire under this focus (the armed or off indicator).
pub fn engine_keys_armed(focus: PaneFocus) -> bool {
    engine_command(focus, "y", KeyMods::none(), KeyPhase::Viewing).is_some()
}

/// The indicator's text. It names only keys that act.
pub fn keys_hint(armed: bool, phase: KeyPhase) -> &'static str {
    if !armed {
        return match phase {
            KeyPhase::Correcting => {
                "Keys are off while you type. Enter commits, Esc leaves the card as it is."
            }
            _ => "Keys are off. Click the card (or Tab into it) to arm them.",
        };
    }
    match phase {
        KeyPhase::Viewing => {
            "Keys: Y yes \u{b7} N no \u{b7} \u{2191}\u{2193} previous or next card \u{b7} M merge \u{b7} G go to the node"
        }
        KeyPhase::Picking => {
            "Keys: 1 to 8 pick what was wrong \u{b7} \u{2191}\u{2193} then Enter \u{b7} Esc cancel"
        }
        KeyPhase::Correcting => "Keys: Enter commit the correction \u{b7} Esc leave it uncorrected",
    }
}

// ── The key in the address ────────────────────────────────────────────────────────────────

/// A site key as one URL path segment. A key holds NUL bytes and slashes (`repo NUL path NUL ...`),
/// so every byte is percent-encoded and the server's path extractor reads it back whole.
pub fn path_segment(key: &str) -> String {
    use std::fmt::Write as _;
    key.bytes()
        .fold(String::with_capacity(key.len() * 3), |mut out, b| {
            let _ = write!(out, "%{b:02X}");
            out
        })
}

#[cfg(test)]
mod tests;
