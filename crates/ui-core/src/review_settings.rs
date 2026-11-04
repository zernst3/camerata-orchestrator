//! The review settings lane's logic (review UI spec section 9), with no rendering framework.
//!
//! Everything the lane decides lives here, one function per judgment: the budget preview, each
//! field's validation and its hint, whether Apply is allowed, what Enter and Esc do in a field,
//! and the sentences the screen shows. The screen (`camerata-ui`'s `cockpit/review_settings.rs`)
//! only chooses words and pixels.
//!
//! The budget formula is not written here: [`budget_for`] calls
//! `camerata_api_types::review::EscalationBudget::compute`, the same function the server marks
//! the escalation line with, so the preview cannot drift from the server's number.

use camerata_api_types::review::{EscalationBudget, EscalationReason};
use camerata_api_types::review_view::{
    EscalationBudgetOutcome, EscalationBudgetReq, ReviewSettingsView, SettingsLimits,
    StageModelsView,
};

use crate::models::{chat_model_groups, ModelOption, ModelsResp};

/// The three typed fields of the lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Rate,
    Floor,
    Sample,
}

/// What the lane shows in its fields: the text as typed, so a half-typed or wrong value stays on
/// screen with its hint instead of snapping back.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsDraft {
    pub rate: String,
    pub floor: String,
    pub sample: String,
    pub reasons: Vec<EscalationReason>,
}

/// Why Apply (or the sample size save) is or is not available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyState {
    /// Valid and different from what is saved.
    Ready,
    /// Valid and the same as what is saved: nothing to apply.
    Unchanged,
    /// A field holds a value the server would refuse; carries that field's hint.
    Invalid(String),
    /// A write is in flight.
    Saving,
    /// A run is in flight, so the settings cannot change.
    RunInFlight,
}

impl ApplyState {
    pub fn enabled(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// What a key press in a field does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAction {
    Apply,
    Revert,
    Nothing,
}

/// `max(floor, ceil(lines / 1000 * rate))`: the number of escalations a run may carry. This is
/// the server's own function, so the preview and the line the server marks cannot disagree.
pub fn budget_for(lines: usize, rate: f32, floor: usize) -> usize {
    EscalationBudget::compute(rate, floor, lines).budget
}

/// The rate as it is typed: `1.0`, `0.5`, `2.25`.
pub fn format_rate(rate: f32) -> String {
    let text = rate.to_string();
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

/// `300000` as `300,000`.
pub fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The reason that is always escalated (design D5) and so always ticked.
const LOCKED_REASON: EscalationReason = EscalationReason::EvidenceUnverified;

/// `reasons` once each, with the locked reason added, in the closed list's priority order.
fn with_locked(reasons: &[EscalationReason]) -> Vec<EscalationReason> {
    let mut all = reasons.to_vec();
    all.push(LOCKED_REASON);
    in_priority_order(&all)
}

/// `reasons` once each, in the closed list's priority order.
fn in_priority_order(reasons: &[EscalationReason]) -> Vec<EscalationReason> {
    EscalationReason::ALL
        .into_iter()
        .filter(|r| reasons.contains(r))
        .collect()
}

impl SettingsDraft {
    /// The fields as saved.
    pub fn from_view(view: &ReviewSettingsView) -> Self {
        Self {
            rate: format_rate(view.escalation.rate_per_kloc),
            floor: view.escalation.floor.to_string(),
            sample: view.engine_check.sample_size_per_run.to_string(),
            reasons: with_locked(&view.escalation.over_budget_reasons),
        }
    }

    /// The saved fields with the formula replaced by `req` (a draft the reviewer has typed).
    pub fn with_req(view: &ReviewSettingsView, req: &EscalationBudgetReq) -> Self {
        Self {
            rate: format_rate(req.rate_per_kloc),
            floor: req.floor.to_string(),
            reasons: in_priority_order(&req.over_budget_reasons),
            ..Self::from_view(view)
        }
    }
}

fn rate_hint(limits: &SettingsLimits) -> String {
    format!(
        "The rate must be between 0 and {} escalations per 1,000 lines.",
        limits.rate_max
    )
}

fn floor_hint(limits: &SettingsLimits) -> String {
    format!(
        "The floor must be a whole number between 0 and {}.",
        group_thousands(limits.floor_max)
    )
}

fn sample_hint_bounds(limits: &SettingsLimits) -> String {
    format!(
        "The sample size must be a whole number between 0 and {}.",
        group_thousands(limits.sample_max)
    )
}

fn parse_rate(text: &str, limits: &SettingsLimits) -> Result<f32, String> {
    match text.trim().parse::<f32>() {
        Ok(v) if v.is_finite() && (0.0..=limits.rate_max).contains(&v) => Ok(v),
        _ => Err(rate_hint(limits)),
    }
}

fn parse_whole(text: &str, max: usize, hint: String) -> Result<usize, String> {
    match text.trim().parse::<usize>() {
        Ok(v) if v <= max => Ok(v),
        _ => Err(hint),
    }
}

fn parse_floor(text: &str, limits: &SettingsLimits) -> Result<usize, String> {
    parse_whole(text, limits.floor_max, floor_hint(limits))
}

/// Check one field against the limits: its value is fine (`None`) or the hint to show in red.
pub fn field_hint(field: Field, draft: &SettingsDraft, limits: &SettingsLimits) -> Option<String> {
    match field {
        Field::Rate => parse_rate(&draft.rate, limits).err(),
        Field::Floor => parse_floor(&draft.floor, limits).err(),
        Field::Sample => sample_size(draft, limits).err(),
    }
}

/// The formula request the draft describes, when its rate and floor are valid. The rate is
/// checked first, so a draft with two bad fields reports the rate.
pub fn escalation_req(
    draft: &SettingsDraft,
    limits: &SettingsLimits,
) -> Result<EscalationBudgetReq, String> {
    let rate_per_kloc = parse_rate(&draft.rate, limits)?;
    let floor = parse_floor(&draft.floor, limits)?;
    Ok(EscalationBudgetReq {
        rate_per_kloc,
        floor,
        // Evidence unverified is always escalated, so the request always carries it.
        over_budget_reasons: with_locked(&draft.reasons),
    })
}

/// The sample size the draft describes, when it is valid.
pub fn sample_size(draft: &SettingsDraft, limits: &SettingsLimits) -> Result<usize, String> {
    parse_whole(&draft.sample, limits.sample_max, sample_hint_bounds(limits))
}

/// Whether the formula in `draft` is the one saved in `view`.
fn formula_is_saved(view: &ReviewSettingsView, req: &EscalationBudgetReq) -> bool {
    req.rate_per_kloc == view.escalation.rate_per_kloc
        && req.floor == view.escalation.floor
        && req.over_budget_reasons == in_priority_order(&view.escalation.over_budget_reasons)
}

/// Whether the formula's Apply is available. A run in flight is the reason shown first, then a
/// write in flight, then a bad value, then nothing to apply.
pub fn formula_apply_state(
    view: &ReviewSettingsView,
    draft: &SettingsDraft,
    saving: bool,
    run_in_flight: bool,
) -> ApplyState {
    if run_in_flight {
        return ApplyState::RunInFlight;
    }
    if saving {
        return ApplyState::Saving;
    }
    match escalation_req(draft, &view.limits) {
        Err(hint) => ApplyState::Invalid(hint),
        Ok(req) if formula_is_saved(view, &req) => ApplyState::Unchanged,
        Ok(_) => ApplyState::Ready,
    }
}

/// Whether the sample size can be saved.
pub fn sample_apply_state(
    view: &ReviewSettingsView,
    draft: &SettingsDraft,
    saving: bool,
    run_in_flight: bool,
) -> ApplyState {
    if run_in_flight {
        return ApplyState::RunInFlight;
    }
    if saving {
        return ApplyState::Saving;
    }
    match sample_size(draft, &view.limits) {
        Err(hint) => ApplyState::Invalid(hint),
        Ok(n) if n == view.engine_check.sample_size_per_run => ApplyState::Unchanged,
        Ok(_) => ApplyState::Ready,
    }
}

/// What `key` (a DOM key name) does in a field when its Apply is `apply`: Enter applies when it
/// can, Esc always reverts the field.
pub fn field_action(key: &str, apply: &ApplyState) -> FieldAction {
    match key {
        "Enter" if apply.enabled() => FieldAction::Apply,
        "Escape" => FieldAction::Revert,
        _ => FieldAction::Nothing,
    }
}

/// `draft` with `field` back at its saved value.
pub fn revert_field(
    draft: &SettingsDraft,
    view: &ReviewSettingsView,
    field: Field,
) -> SettingsDraft {
    let saved = SettingsDraft::from_view(view);
    let mut out = draft.clone();
    match field {
        Field::Rate => out.rate = saved.rate,
        Field::Floor => out.floor = saved.floor,
        Field::Sample => out.sample = saved.sample,
    }
    out
}

/// Whether `reason` is ticked and cannot be changed: evidence unverified is always escalated
/// (design D5), so the lane shows it checked and disabled.
pub fn reason_locked(reason: EscalationReason) -> bool {
    reason == LOCKED_REASON
}

/// Whether the answer to the preview `answered` still describes the draft: the draft must still
/// be a valid unsaved formula, and the same one. A slow earlier reply must not overwrite the
/// preview of a newer draft, nor reappear after the draft turned invalid or was reverted.
pub fn preview_is_current(
    view: &ReviewSettingsView,
    draft: &SettingsDraft,
    answered: &EscalationBudgetReq,
) -> bool {
    preview_request(view, draft).as_ref() == Some(answered)
}

/// `draft` with `reason` ticked or unticked. The locked reason stays ticked.
pub fn toggle_reason(draft: &SettingsDraft, reason: EscalationReason) -> SettingsDraft {
    if reason_locked(reason) {
        return SettingsDraft {
            reasons: with_locked(&draft.reasons),
            ..draft.clone()
        };
    }
    let mut reasons = draft.reasons.clone();
    if let Some(at) = reasons.iter().position(|r| *r == reason) {
        reasons.remove(at);
    } else {
        reasons.push(reason);
    }
    SettingsDraft {
        reasons: with_locked(&reasons),
        ..draft.clone()
    }
}

/// The reasons the lane offers a checkbox for: the five that may exceed the budget by default,
/// plus any other reason the project's saved set already holds (so it can be unticked).
pub fn reason_choices(view: &ReviewSettingsView) -> Vec<EscalationReason> {
    let mut shown = EscalationReason::default_over_budget_set();
    shown.extend(view.escalation.over_budget_reasons.iter().copied());
    in_priority_order(&shown)
}

/// A reason as the checkbox says it.
pub fn reason_label(reason: EscalationReason) -> &'static str {
    match reason {
        EscalationReason::CrossTenant => "cross tenant",
        EscalationReason::EvidenceUnverified => "evidence unverified",
        other => other.phrase(),
    }
}

/// The note under the reason checkboxes: which of them need critical severity.
pub fn reasons_note() -> &'static str {
    "Every reason except evidence unverified counts only at critical severity."
}

/// The proposed request to preview on the server, when the draft is valid and not yet saved.
pub fn preview_request(
    view: &ReviewSettingsView,
    draft: &SettingsDraft,
) -> Option<EscalationBudgetReq> {
    let req = escalation_req(draft, &view.limits).ok()?;
    (!formula_is_saved(view, &req)).then_some(req)
}

/// The budget line under the formula: counted lines and what the draft gives. A draft that
/// cannot be applied claims no budget and says so.
pub fn budget_line(counted_lines: usize, draft: &SettingsDraft, limits: &SettingsLimits) -> String {
    let lines = group_thousands(counted_lines);
    match escalation_req(draft, limits) {
        Ok(req) => format!(
            "Counted lines {lines} \u{2192} budget {}",
            budget_for(counted_lines, req.rate_per_kloc, req.floor)
        ),
        Err(_) => format!("Counted lines {lines}, budget not computed"),
    }
}

fn candidates(n: usize) -> String {
    if n == 1 {
        "1 candidate".to_string()
    } else {
        format!("{n} candidates")
    }
}

/// The sentence for a preview or an applied budget change. It always says no model is called.
pub fn effect_sentence(outcome: &EscalationBudgetOutcome) -> String {
    let (above, below) = (outcome.moved_above, outcome.moved_below);
    if outcome.applied {
        let moved = match (above, below) {
            (0, 0) => "No candidate crossed the line.".to_string(),
            (a, 0) => format!("{} moved above the line.", candidates(a)),
            (0, b) => format!("{} moved below the line.", candidates(b)),
            (a, b) => format!(
                "{} moved above the line and {b} moved below it.",
                candidates(a)
            ),
        };
        format!("{moved} No model call was made.")
    } else {
        let would = match (above, below) {
            (0, 0) => "No candidate would cross the line.".to_string(),
            (a, 0) => format!("{} would move above the line.", candidates(a)),
            (0, b) => format!("{} would move below the line.", candidates(b)),
            (a, b) => format!(
                "{} would move above the line and {b} would move below it.",
                candidates(a)
            ),
        };
        format!("{would} Applying makes no model call.")
    }
}

/// The sample size hint: what 0 does, the suggested number and the keys.
pub fn sample_hint(suggested: usize) -> String {
    format!(
        "0 turns the lane off; suggested {suggested} for early engagements. Enter saves, Esc reverts."
    )
}

/// The four stages whose model the lane sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Scan,
    Calibration,
    GroupReview,
    Fix,
}

impl Stage {
    pub const ALL: [Stage; 4] = [
        Stage::Scan,
        Stage::Calibration,
        Stage::GroupReview,
        Stage::Fix,
    ];

    /// The `step` the step-models route takes for this stage.
    pub fn step_key(self) -> &'static str {
        match self {
            Stage::Scan => "audit",
            Stage::Calibration => "calibration",
            Stage::GroupReview => "group_review",
            Stage::Fix => "fix",
        }
    }

    /// The row label.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Scan => "Scan model",
            Stage::Calibration => "Calibration (stage 1)",
            Stage::GroupReview => "Group review (stage 2)",
            Stage::Fix => "Fix model",
        }
    }

    /// The short note beside the select.
    pub fn hint(self) -> &'static str {
        match self {
            Stage::Scan | Stage::Fix => "",
            Stage::Calibration => "per site, batched",
            Stage::GroupReview => "one call per rule group, real time",
        }
    }

    /// The model the saved settings hold for this stage (empty: not set).
    pub fn current(self, models: &StageModelsView) -> &str {
        match self {
            Stage::Scan => &models.scan,
            Stage::Calibration => &models.calibration,
            Stage::GroupReview => &models.group_review,
            Stage::Fix => &models.fix,
        }
    }

    /// Whether the select offers a "(not set)" first option: only the fix model has a run-time
    /// default that an explicit pick would override.
    pub fn can_be_unset(self) -> bool {
        self == Stage::Fix
    }
}

/// What a person can do in the lane, as the screen reports it to its host.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsEvent {
    /// A field's text changed.
    Typed(Field, String),
    /// A key was pressed in a field (a DOM key name).
    Key(Field, String),
    /// A reason checkbox was clicked.
    ToggleReason(EscalationReason),
    /// The Apply button.
    Apply,
    /// A stage's model select changed.
    Model(Stage, String),
}

/// The option groups for a stage's select: the registry's models, with the stage's current model
/// always among them so the select never shows a model that is not there.
pub fn model_groups(models: &Option<ModelsResp>, current: &str) -> Vec<(String, Vec<ModelOption>)> {
    let mut groups = chat_model_groups(models, current);
    let listed = groups
        .iter()
        .any(|(_, options)| options.iter().any(|m| m.id == current));
    if !current.trim().is_empty() && !listed {
        groups.push((
            "Current".to_string(),
            vec![ModelOption {
                label: current.to_string(),
                id: current.to_string(),
                provider: String::new(),
            }],
        ));
    }
    groups
}

#[cfg(test)]
mod tests;
