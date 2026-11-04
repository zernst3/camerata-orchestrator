//! The site detail's judgments (review UI spec sections 3, 8 and 11; design section 5).
//!
//! The detail pane is the right pane for a focused site in every lane. It edits a field, adds a
//! finding, merges a row into another, splits it to a sub-group, records the client's
//! acknowledgement and deals with what a rerun left stale. Every judgment it makes is a pure
//! function here, with a unit test (spec rule 2); the view in `camerata-ui` chooses words and
//! pixels and the controller performs the I/O.
//!
//! - fields: which fields are editable, their label and chord letter, the control each one
//!   takes, the value a row holds now, and the lines an edited or stale field draws;
//! - requests: the [`NewEditReq`], decision, [`AddFindingReq`] and client-ack bodies, each built
//!   only when its required reason (or who) is there;
//! - the open form: [`DetailMode`] and [`reduce`], the state of whichever form is open;
//! - lines: the decision line (stale, new since the group decision, merged), the held line, the
//!   client line and the stage lines;
//! - [`bridge`]: a site as the old [`FindingView`](crate::triage::FindingView), so the code
//!   viewer and the stacked view (built for the flat Queue) open on a site of the tree.

pub mod bridge;

use std::collections::BTreeMap;

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{AddFindingReq, ClientAckReq, EditField, FindingAdded, NewEditReq};
use camerata_api_types::review::{DisagreementField, EscalationState, ReviewLevel};
use camerata_api_types::review_view::{DecisionView, EditView, SiteRow, TreeLevel};

use crate::tree::state::{TreeIndex, TreeState};
use crate::tree::{decision_label, kind_tag_of, marker_chips, outlier_phrase, short_date, Chip};
use crate::triage::UNDO_WINDOW_MS;

// ── The editable fields ───────────────────────────────────────────────────────────────────

/// The fields a site's own detail edits, in the order spec section 3 lists them. The node
/// summary is the ninth field of [`EditField::ALL`]; it is edited at the node (WO10).
pub const SITE_FIELDS: [EditField; 9] = [
    EditField::Headline,
    EditField::Detail,
    EditField::Fix,
    EditField::FixType,
    EditField::Severity,
    EditField::RuleId,
    EditField::Kind,
    EditField::Category,
    EditField::Subgroup,
];

/// The severities a reviewer can set (spec section 3). The server also takes `info`, which only
/// an informational row carries, so a row that has it keeps it in its own select.
pub const SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];

/// The two fix labels the exports print.
pub const FIX_TYPES: [&str; 2] = ["Tailored fix", "Standard fix"];

/// The closed category taxonomy the engine groups by (the server's `KNOWN_CATEGORIES`). The
/// server takes any label here, so this list is the select's suggestions and a drift from the
/// engine's list changes what is offered and nothing else.
pub const CATEGORIES: [&str; 12] = [
    "authorization",
    "authentication",
    "secret-exposure",
    "injection",
    "transport-security",
    "rls-policy",
    "resource-exposure",
    "input-validation",
    "error-handling",
    "arch-conformance",
    "testing-style",
    "performance",
];

/// The field's name on the screen.
pub fn field_label(field: EditField) -> &'static str {
    match field {
        EditField::Headline => "Headline",
        EditField::Detail => "Detail",
        EditField::Fix => "Fix",
        EditField::FixType => "Fix type",
        EditField::Severity => "Severity",
        EditField::RuleId => "Rule",
        EditField::Kind => "Kind",
        EditField::Category => "Category",
        EditField::Subgroup => "Sub-group",
        EditField::Summary => "Summary",
    }
}

/// The letter that picks the field after `E` (spec section 8: `H`, `D`, `X` fix, `S` severity,
/// `R` rule, `K` kind, `G` category, `U` sub-group). The spec gives the fix type no letter; `T`
/// is free after `E` (it is Tech debt only on a bare site), so the fix type is `E` then `T`.
/// A summary is edited at its node, so it has none here.
pub fn chord_letter(field: EditField) -> Option<char> {
    match field {
        EditField::Headline => Some('H'),
        EditField::Detail => Some('D'),
        EditField::Fix => Some('X'),
        EditField::FixType => Some('T'),
        EditField::Severity => Some('S'),
        EditField::RuleId => Some('R'),
        EditField::Kind => Some('K'),
        EditField::Category => Some('G'),
        EditField::Subgroup => Some('U'),
        EditField::Summary => None,
    }
}

/// The field a key picks after `E`, in either case.
pub fn field_for_chord(key: &str) -> Option<EditField> {
    let mut chars = key.chars();
    let c = chars.next()?.to_ascii_uppercase();
    if chars.next().is_some() {
        return None;
    }
    SITE_FIELDS
        .into_iter()
        .find(|f| chord_letter(*f) == Some(c))
}

/// The line shown while the pane waits for the field's letter.
pub fn chord_hint() -> String {
    let letters = SITE_FIELDS
        .iter()
        .filter_map(|f| chord_letter(*f).map(|c| format!("{c} {}", field_label(*f).to_lowercase())))
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    format!("Edit which field? {letters} \u{b7} Esc cancels")
}

/// One choice of a select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlOption {
    pub value: String,
    pub label: String,
}

impl ControlOption {
    fn same(value: &str) -> Self {
        Self {
            value: value.to_string(),
            label: value.to_string(),
        }
    }
}

/// The edit control a field takes (spec section 3, "Control").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Single-line text.
    Line,
    /// Multi-line text.
    Multi,
    /// A closed list.
    Select(Vec<ControlOption>),
    /// A list of suggestions and free text (the rule id: corpus ids plus any other).
    Combo(Vec<ControlOption>),
    /// The sub-group is changed by a split: a select of the rule's sub-groups plus a new one.
    Split,
    /// The summary of a node, edited at the node head (WO10's `summary_edit_request`).
    NodeSummary,
}

/// What the host knows about the row's surroundings, for the controls and the lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetailContext {
    /// The labels of the breadcrumb: category, rule, sub-group.
    pub crumbs: Vec<String>,
    /// The rule ids the run holds, for the rule select (the tree's rule nodes).
    pub rule_ids: Vec<String>,
    /// The labels of the other sub-groups under the row's rule, for a split.
    pub subgroups: Vec<String>,
    /// The state of the group decision a new-since-decision row is new since.
    pub node_decision: Option<DecisionState>,
    /// The sites the host has loaded, for the merge picker: `(site key, path, line, rule id)`.
    pub pool: Vec<PoolSite>,
}

/// A site the merge picker can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSite {
    pub key: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    /// The row is itself merged into another, so it is not a target.
    pub merged: bool,
}

impl PoolSite {
    /// A pool entry for `row`.
    pub fn of(row: &SiteRow) -> Self {
        Self {
            key: row.site_key.clone(),
            path: row.path.clone(),
            line: row.line,
            rule_id: row.rule_id.clone(),
            merged: row.merged_into.is_some(),
        }
    }
}

/// What the lane gives the detail about the row's surroundings, from the tree and the sites it
/// has loaded. `node` is the sub-group the row sits under.
pub fn context_for(
    index: &TreeIndex,
    state: &TreeState,
    node: &str,
    row: &SiteRow,
) -> DetailContext {
    let crumbs = index
        .breadcrumb(node)
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    // Every rule of the run, once, as its id.
    let mut rule_ids: Vec<String> = Vec::new();
    let mut stack: Vec<&str> = index.roots().map(|n| n.id.as_str()).collect();
    while let Some(id) = stack.pop() {
        let Some(n) = index.node(id) else { continue };
        if n.thrown_out {
            continue;
        }
        if n.level == TreeLevel::Rule {
            let rule = n.id.strip_prefix("rule:").unwrap_or(&n.id).to_string();
            if !rule_ids.contains(&rule) {
                rule_ids.push(rule);
            }
        } else if n.level == TreeLevel::Category {
            stack.extend(index.children(id).map(|c| c.id.as_str()));
        }
    }
    rule_ids.sort();
    let subgroups = index
        .parent_of(node)
        .into_iter()
        .flat_map(|parent| index.children(parent))
        .filter(|n| n.level == TreeLevel::Subgroup && n.id != node && !n.thrown_out)
        .map(|n| n.label.clone())
        .collect();
    let node_decision = row
        .new_since_decision
        .as_ref()
        .and_then(|n| index.node(&n.node_id))
        .and_then(|n| n.decision.as_ref())
        .map(|d| d.state.clone());
    let pool = state
        .loaded
        .values()
        .flat_map(|l| l.rows.iter())
        .map(PoolSite::of)
        .collect();
    DetailContext {
        crumbs,
        rule_ids,
        subgroups,
        node_decision,
        pool,
    }
}

/// The control `field` takes on `row`.
pub fn control_for(field: EditField, row: &SiteRow, ctx: &DetailContext) -> Control {
    match field {
        EditField::Headline => Control::Line,
        EditField::Detail | EditField::Fix => Control::Multi,
        EditField::FixType => {
            Control::Select(FIX_TYPES.iter().map(|f| ControlOption::same(f)).collect())
        }
        EditField::Severity => {
            let mut options: Vec<ControlOption> =
                SEVERITIES.iter().map(|s| ControlOption::same(s)).collect();
            if !SEVERITIES.contains(&row.severity.as_str()) && !row.severity.is_empty() {
                options.insert(0, ControlOption::same(&row.severity));
            }
            Control::Select(options)
        }
        EditField::RuleId => {
            let mut ids: Vec<String> = Vec::new();
            for id in std::iter::once(&row.rule_id).chain(&ctx.rule_ids) {
                if !id.is_empty() && !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
            Control::Combo(ids.iter().map(|i| ControlOption::same(i)).collect())
        }
        EditField::Kind => Control::Select(kind_options(row)),
        EditField::Category => {
            let mut options: Vec<ControlOption> =
                CATEGORIES.iter().map(|c| ControlOption::same(c)).collect();
            if let Some(current) = row.category.as_deref().filter(|c| !c.is_empty()) {
                if !CATEGORIES.contains(&current) {
                    options.insert(0, ControlOption::same(current));
                }
            }
            options.insert(
                0,
                ControlOption {
                    value: String::new(),
                    label: "(none)".to_string(),
                },
            );
            Control::Select(options)
        }
        EditField::Subgroup => Control::Split,
        EditField::Summary => Control::NodeSummary,
    }
}

/// The kinds the select offers. The server takes a bare tag only for a kind that carries no
/// numbers, so `defect` is the one a reviewer can move a row to; a row that already is a
/// deviation lists it as its current value.
fn kind_options(row: &SiteRow) -> Vec<ControlOption> {
    let current = kind_tag_of(row);
    let mut options = Vec::new();
    if current != "defect" {
        options.push(ControlOption {
            value: current.to_string(),
            label: format!("{current} (current)"),
        });
    }
    options.push(ControlOption::same("defect"));
    options
}

/// The value `field` holds on `row` now.
pub fn current_value(row: &SiteRow, field: EditField, ctx: &DetailContext) -> String {
    match field {
        EditField::Headline => row.headline.clone(),
        EditField::Detail => row.detail.clone(),
        EditField::Fix => row.fix.clone(),
        EditField::FixType => row.fix_type.clone(),
        EditField::Severity => row.severity.clone(),
        EditField::RuleId => row.rule_id.clone(),
        EditField::Kind => kind_tag_of(row).to_string(),
        EditField::Category => row.category.clone().unwrap_or_default(),
        EditField::Subgroup => ctx
            .crumbs
            .get(2)
            .cloned()
            .or_else(|| {
                row.stage2
                    .as_ref()
                    .map(|s| s.subgroup_label.clone())
                    .filter(|l| !l.is_empty())
            })
            .or_else(|| row.group_path.get(2).cloned())
            .unwrap_or_default(),
        EditField::Summary => String::new(),
    }
}

/// What a field line draws: the value, the edit that is applied to it, and a stale edit beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldState {
    pub field: EditField,
    pub label: &'static str,
    pub value: String,
    /// The latest edit of this field that applies (`mark-edited`).
    pub applied: Option<EditView>,
    /// The latest edit of this field that no longer applies (`mark-stale`).
    pub stale: Option<EditView>,
}

/// The state of `field` on `row`.
pub fn field_state(row: &SiteRow, field: EditField, ctx: &DetailContext) -> FieldState {
    let of_field = || row.edits.iter().filter(move |e| e.field == field);
    FieldState {
        field,
        label: field_label(field),
        value: current_value(row, field, ctx),
        applied: of_field().rfind(|e| !e.stale).cloned(),
        stale: of_field().rfind(|e| e.stale).cloned(),
    }
}

/// The hover on an edited field: the value it replaced, the reason, who and when (spec section 8).
pub fn edit_tip(edit: &EditView) -> String {
    let when = short_date(&edit.at);
    let mut tip = format!("Was: {}", shown(&edit.old));
    if !edit.reason.is_empty() {
        tip.push_str(&format!("\nWhy: {}", edit.reason));
    }
    if !edit.by.is_empty() {
        tip.push_str(&format!("\nBy {}", edit.by));
        if !when.is_empty() {
            tip.push_str(&format!(" on {when}"));
        }
    }
    tip
}

fn shown(value: &str) -> &str {
    if value.is_empty() {
        "(empty)"
    } else {
        value
    }
}

/// The second line under a stale edit's field: the reviewer's text, shown and not applied.
pub fn stale_edit_line(edit: &EditView) -> String {
    format!("Your edit (stale): {}", edit.new)
}

/// The note that says what a stale edit was against, for its tooltip.
pub fn stale_edit_tip(edit: &EditView) -> String {
    format!(
        "You changed \u{201c}{}\u{201d} to \u{201c}{}\u{201d}. The row has changed since, so nothing was applied.",
        edit.old, edit.new
    )
}

// ── The edit request ──────────────────────────────────────────────────────────────────────

/// Why an edit was not posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditRefusal {
    ReasonRequired,
    Unchanged,
    Invalid(String),
}

impl EditRefusal {
    /// The words the form shows.
    pub fn message(&self) -> String {
        match self {
            Self::ReasonRequired => "An edit needs a reason. Say what was wrong.".to_string(),
            Self::Unchanged => "That is the value it already has.".to_string(),
            Self::Invalid(why) => why.clone(),
        }
    }
}

/// What is wrong with `new` as the value of `field`, in words; `None` when it is a value the
/// field can take. Mirrors the server's own validation, which refuses the same values.
pub fn value_problem(field: EditField, new: &str) -> Option<String> {
    let new = new.trim();
    let name = field_label(field);
    match field {
        EditField::Headline
        | EditField::Detail
        | EditField::Fix
        | EditField::RuleId
        | EditField::Summary => new.is_empty().then(|| format!("{name} cannot be empty.")),
        EditField::Subgroup => new
            .is_empty()
            .then(|| "A sub-group needs a name.".to_string()),
        EditField::Severity => {
            let ok = SEVERITIES.contains(&new.to_ascii_lowercase().as_str())
                || new.eq_ignore_ascii_case("info");
            (!ok).then(|| "Severity must be critical, high, medium or low.".to_string())
        }
        EditField::FixType => (!FIX_TYPES.contains(&new))
            .then(|| "The fix type must be Tailored fix or Standard fix.".to_string()),
        EditField::Kind => {
            let ok = new.starts_with('{') || new == "defect";
            (!ok).then(|| {
                "A row can be moved to the kind defect. A deviation needs the convention numbers the engine measured, so it cannot be set by hand."
                    .to_string()
            })
        }
        EditField::Category => None,
    }
}

/// The edit that sets `field` of `row` to `new`, with the reason it needs.
pub fn edit_request(
    row: &SiteRow,
    field: EditField,
    new: &str,
    reason: &str,
    ctx: &DetailContext,
    run_id: &str,
) -> Result<NewEditReq, EditRefusal> {
    let new = new.trim();
    // The value it already has is no change, even when it could not be typed (a deviation).
    if new == current_value(row, field, ctx).trim() {
        return Err(EditRefusal::Unchanged);
    }
    if let Some(why) = value_problem(field, new) {
        return Err(EditRefusal::Invalid(why));
    }
    if reason.trim().is_empty() {
        return Err(EditRefusal::ReasonRequired);
    }
    Ok(NewEditReq {
        key: row.site_key.clone(),
        field,
        new: new.to_string(),
        reason: reason.trim().to_string(),
        by: String::new(),
        run_id: run_id.to_string(),
    })
}

/// The undo bar's words after an edit.
pub fn edit_done_message(field: EditField) -> String {
    format!("{} edited.", field_label(field))
}

// ── Split to a sub-group ──────────────────────────────────────────────────────────────────

/// Where a split sends the site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitChoice {
    /// A sub-group of the rule that already exists, by label.
    Existing(String),
    /// A new sub-group, named in the form.
    New,
}

/// The sub-group name a split posts, or why it cannot.
pub fn split_label(choice: &SplitChoice, new_label: &str) -> Result<String, String> {
    match choice {
        SplitChoice::Existing(label) => Ok(label.clone()),
        SplitChoice::New => {
            let label = new_label.trim();
            if label.is_empty() {
                Err("A new sub-group needs a label.".to_string())
            } else {
                Ok(label.to_string())
            }
        }
    }
}

/// The undo bar's words after a split, naming where the site went.
pub fn split_done_message(label: &str) -> String {
    format!("Moved 1 site to \u{201c}{label}\u{201d}. Both sub-groups have a new member count.")
}

// ── Merge into another site ───────────────────────────────────────────────────────────────

/// One site the merge picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCandidate {
    pub key: String,
    /// `path:line`.
    pub label: String,
    pub rule_id: String,
    /// "possible duplicate" for the row the engine linked.
    pub note: Option<String>,
}

/// How many sites the picker lists at most.
pub const MERGE_LIST_MAX: usize = 12;

/// The sites `row` can be merged into: the possible-duplicate link first, then the loaded sites
/// whose `path:line` or rule id contains `query` (every loaded site, nearest first, when the
/// query is empty). Never the row itself, and never a row that is itself merged away.
pub fn merge_candidates(row: &SiteRow, ctx: &DetailContext, query: &str) -> Vec<MergeCandidate> {
    let mut out: Vec<MergeCandidate> = Vec::new();
    if let Some(dup) = row.possible_duplicate_of.as_ref() {
        if dup.key != row.site_key {
            let known = ctx.pool.iter().find(|p| p.key == dup.key);
            if !known.is_some_and(|p| p.merged) {
                out.push(MergeCandidate {
                    key: dup.key.clone(),
                    label: format!("{}:{}", dup.path, dup.line),
                    rule_id: dup.rule_id.clone(),
                    note: Some("possible duplicate".to_string()),
                });
            }
        }
    }
    let query = query.trim().to_lowercase();
    for site in &ctx.pool {
        if out.len() >= MERGE_LIST_MAX {
            break;
        }
        if site.key == row.site_key || site.merged || out.iter().any(|c| c.key == site.key) {
            continue;
        }
        let label = format!("{}:{}", site.path, site.line);
        let matches = query.is_empty()
            || label.to_lowercase().contains(&query)
            || site.rule_id.to_lowercase().contains(&query);
        if matches {
            out.push(MergeCandidate {
                key: site.key.clone(),
                label,
                rule_id: site.rule_id.clone(),
                note: None,
            });
        }
    }
    out
}

/// Why a merge was not posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRefusal {
    NoTarget,
    ReasonRequired,
    Itself,
}

impl MergeRefusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::NoTarget => "Pick the site to merge into.",
            Self::ReasonRequired => "A merge needs a reason. Say why these are one problem.",
            Self::Itself => "A site cannot be merged into itself.",
        }
    }
}

/// The decision that merges `row` into `target`: a site-scope `MergedInto` with a required
/// reason. The row stays in the tree with a pointer; it never disappears.
pub fn merge_request(
    row: &SiteRow,
    target: Option<&str>,
    reason: &str,
    run_id: &str,
) -> Result<NewDecisionReq, MergeRefusal> {
    let target = target
        .filter(|t| !t.is_empty())
        .ok_or(MergeRefusal::NoTarget)?;
    if target == row.site_key {
        return Err(MergeRefusal::Itself);
    }
    if reason.trim().is_empty() {
        return Err(MergeRefusal::ReasonRequired);
    }
    Ok(decision_request(
        row,
        DecisionState::MergedInto(target.to_string()),
        reason,
        run_id,
    ))
}

/// The undo bar's words after a merge.
pub fn merge_done_message(label: &str) -> String {
    format!("Merged into {label}. The row stays in the tree.")
}

fn decision_request(
    row: &SiteRow,
    state: DecisionState,
    reason: &str,
    run_id: &str,
) -> NewDecisionReq {
    NewDecisionReq {
        scope: Scope::Site,
        key: row.site_key.clone(),
        state,
        reason: reason.trim().to_string(),
        decided_by: String::new(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: run_id.to_string(),
        decided_at: String::new(),
    }
}

// ── Add a finding ─────────────────────────────────────────────────────────────────────────

/// One field of the add form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddField {
    Repo,
    Path,
    Line,
    RuleId,
    Kind,
    Severity,
    Headline,
    Detail,
    Fix,
    Reason,
}

/// Where the snippet of the add form stands. It is read from the source at the line and shown
/// read-only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Snippet {
    #[default]
    NotLoaded,
    Loading,
    /// The text of the line.
    Loaded(String),
    /// The source could not give the line, with the reason.
    Failed(String),
}

/// The add form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddDraft {
    pub repo: String,
    pub path: String,
    pub line: String,
    pub rule_id: String,
    pub kind: String,
    pub severity: String,
    pub headline: String,
    pub detail: String,
    pub fix: String,
    pub reason: String,
    pub snippet: Snippet,
    pub error: Option<String>,
}

impl AddDraft {
    /// A form prefilled from the focused site (or from the line the code viewer is on).
    pub fn at(repo: &str, path: &str, line: usize, rule_id: &str) -> Self {
        Self {
            repo: repo.to_string(),
            path: path.to_string(),
            line: if line == 0 {
                String::new()
            } else {
                line.to_string()
            },
            rule_id: rule_id.to_string(),
            kind: "defect".to_string(),
            severity: "medium".to_string(),
            headline: String::new(),
            detail: String::new(),
            fix: String::new(),
            reason: String::new(),
            snippet: Snippet::NotLoaded,
            error: None,
        }
    }

    /// A form prefilled from `row`: its repo, file and line, and its rule.
    pub fn from_row(row: &SiteRow) -> Self {
        Self::at(&row.repo, &row.path, row.line, &row.rule_id)
    }

    /// The line number, when the text in the field is one.
    pub fn line_number(&self) -> Option<usize> {
        self.line.trim().parse::<usize>().ok().filter(|n| *n > 0)
    }

    fn set(&mut self, field: AddField, text: String) {
        let slot = match field {
            AddField::Repo => &mut self.repo,
            AddField::Path => &mut self.path,
            AddField::Line => &mut self.line,
            AddField::RuleId => &mut self.rule_id,
            AddField::Kind => &mut self.kind,
            AddField::Severity => &mut self.severity,
            AddField::Headline => &mut self.headline,
            AddField::Detail => &mut self.detail,
            AddField::Fix => &mut self.fix,
            AddField::Reason => &mut self.reason,
        };
        *slot = text;
        // Where the finding is has changed, so the snippet shown is for another line.
        if matches!(field, AddField::Repo | AddField::Path | AddField::Line) {
            self.snippet = Snippet::NotLoaded;
        }
    }
}

/// The request that adds a finding, or what is missing from the form, in words. The server runs
/// the evidence check; this only refuses what the server would refuse as incomplete.
pub fn add_request(draft: &AddDraft) -> Result<AddFindingReq, String> {
    let missing = |what: &str| Err(format!("A finding needs {what}."));
    if draft.repo.trim().is_empty() || draft.path.trim().is_empty() {
        return missing("a repo and a file path");
    }
    let Some(line) = draft.line_number() else {
        return missing("a line number");
    };
    if draft.rule_id.trim().is_empty() {
        return missing("a rule id");
    }
    if draft.headline.trim().is_empty() || draft.detail.trim().is_empty() {
        return missing("a headline and an explanation");
    }
    let snippet = match &draft.snippet {
        Snippet::Loaded(text) if !text.trim().is_empty() => text.clone(),
        Snippet::Loading => return Err("The code at that line is still loading.".to_string()),
        Snippet::Failed(why) => return Err(why.clone()),
        _ => return Err("Load the code at that line first: the finding quotes it.".to_string()),
    };
    if draft.reason.trim().is_empty() {
        return Err(
            "A finding the engine missed needs a reason. Say why you added it.".to_string(),
        );
    }
    let kind = serde_json::from_value(serde_json::json!({ "kind": draft.kind.trim() }))
        .map_err(|_| "Pick what the row is: defect.".to_string())?;
    Ok(AddFindingReq {
        repo: draft.repo.trim().to_string(),
        path: draft.path.trim().to_string(),
        line,
        rule_id: draft.rule_id.trim().to_string(),
        severity: draft.severity.trim().to_ascii_lowercase(),
        snippet,
        headline: draft.headline.trim().to_string(),
        detail: draft.detail.trim().to_string(),
        fix: draft.fix.trim().to_string(),
        kind: Some(kind),
        category: None,
        reason: draft.reason.trim().to_string(),
        by: String::new(),
    })
}

/// What the evidence check said about an added finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddedOutcome {
    /// The check found the snippet near the line.
    Verified,
    /// The check did not find it: the row is in, and held from every export until it is decided.
    /// Carries the gate's reason.
    Held(String),
}

/// The check's answer to an added finding.
pub fn added_outcome(done: &FindingAdded) -> AddedOutcome {
    if done.evidence_verified {
        AddedOutcome::Verified
    } else {
        AddedOutcome::Held(refusal_text(&done.evidence_note))
    }
}

/// The gate's reason in a sentence a reviewer can act on.
pub fn refusal_text(note: &str) -> String {
    let note = note.trim();
    if note.is_empty() {
        "The evidence check did not find the quoted code at that line.".to_string()
    } else {
        note.to_string()
    }
}

/// What the pane says after an added finding: where it is and, when the check held it, why.
pub fn added_message(outcome: &AddedOutcome) -> String {
    match outcome {
        AddedOutcome::Verified => {
            "Finding added. It is in its node with your edit mark.".to_string()
        }
        AddedOutcome::Held(why) => format!(
            "Finding added, but the evidence check held it: {why} It stays out of every export until you decide it."
        ),
    }
}

/// The lines of the snippet the form shows read-only, from the source's window.
pub fn snippet_at_line(start_line: usize, lines: &[String], line: usize) -> Option<String> {
    let at = line.checked_sub(start_line)?;
    lines.get(at).cloned().filter(|l| !l.trim().is_empty())
}

// ── Client acknowledgement ────────────────────────────────────────────────────────────────

/// Where a row stands with the client's acknowledgement (spec section 8, "Client").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckState {
    /// Only Ignored rows have a client line.
    NotApplicable,
    /// Ignored, and the client has not confirmed: "Needs client confirmation".
    Needed { decision_id: String },
    /// The client accepted the risk.
    Acknowledged {
        by: String,
        at: String,
        note: String,
    },
}

/// The client line of `row`.
pub fn ack_state(row: &SiteRow) -> AckState {
    match &row.decision {
        Some(d) if d.state == DecisionState::Ignored => match &d.confirmed_by_client {
            Some(ack) => AckState::Acknowledged {
                by: ack.by.clone(),
                at: ack.at.clone(),
                note: ack.note.clone(),
            },
            None => AckState::Needed {
                decision_id: d.id.clone(),
            },
        },
        _ => AckState::NotApplicable,
    }
}

/// The client line in words: `Accepted risk: <reason>` once acknowledged (exactly as the
/// exports print it), else `Needs client confirmation`.
pub fn ack_line(row: &SiteRow) -> Option<String> {
    match ack_state(row) {
        AckState::NotApplicable => None,
        AckState::Needed { .. } => Some("Needs client confirmation".to_string()),
        AckState::Acknowledged { .. } => {
            let reason = row
                .decision
                .as_ref()
                .map(|d| d.reason.trim().to_string())
                .unwrap_or_default();
            Some(if reason.is_empty() {
                "Accepted risk".to_string()
            } else {
                format!("Accepted risk: {reason}")
            })
        }
    }
}

/// Who acknowledged it and when, for the line under an acknowledged row.
pub fn ack_detail(row: &SiteRow) -> Option<String> {
    let AckState::Acknowledged { by, at, note } = ack_state(row) else {
        return None;
    };
    let when = short_date(&at);
    let mut text = format!("Confirmed by {by}");
    if !when.is_empty() {
        text.push_str(&format!(" on {when}"));
    }
    if !note.trim().is_empty() {
        text.push_str(&format!(": {}", note.trim()));
    }
    Some(text)
}

/// The decision id the acknowledgement is recorded on, only for a row that can have one.
pub fn ack_target(row: &SiteRow) -> Option<String> {
    match ack_state(row) {
        AckState::Needed { decision_id } => Some(decision_id),
        _ => None,
    }
}

/// Why an acknowledgement was not posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckRefusal {
    NotIgnored,
    WhoRequired,
}

impl AckRefusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotIgnored => "Only an Ignored row can be acknowledged by the client.",
            Self::WhoRequired => "Say who at the client acknowledged it.",
        }
    }
}

/// The acknowledgement of `row`: `(decision id, request)`. Refused for a row that is not
/// Ignored (or already acknowledged) and when nobody is named.
pub fn client_ack_request(
    row: &SiteRow,
    by: &str,
    note: &str,
) -> Result<(String, ClientAckReq), AckRefusal> {
    let id = ack_target(row).ok_or(AckRefusal::NotIgnored)?;
    if by.trim().is_empty() {
        return Err(AckRefusal::WhoRequired);
    }
    Ok((
        id,
        ClientAckReq {
            by: by.trim().to_string(),
            note: note.trim().to_string(),
        },
    ))
}

// ── Stale, moved and new since decision ───────────────────────────────────────────────────

/// A button on the decision line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineAction {
    Reapply,
    Discard,
}

impl LineAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reapply => "Reapply",
            Self::Discard => "Discard",
        }
    }
}

/// How the decision line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTone {
    /// A decision stands.
    Decided,
    /// Nobody has decided it.
    Unresolved,
    /// Unresolved beside a decision that no longer applies, or a member the group decision did
    /// not cover.
    Warning,
}

/// The Decision line of the detail (spec section 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionLine {
    pub text: String,
    pub tone: LineTone,
    /// The decision came from a node and not from the reviewer looking at this site.
    pub inherited: bool,
    /// What the line offers: Reapply and Discard beside a stale decision, else nothing.
    pub actions: Vec<LineAction>,
}

/// The decision line of `row`.
pub fn decision_line(row: &SiteRow, ctx: &DetailContext) -> DecisionLine {
    if let Some(d) = &row.decision {
        let text = match (&d.state, &row.merged_into) {
            (DecisionState::MergedInto(_), Some(m)) => format!("Merged into {}", m.label),
            (state, _) => decision_label(state).to_string(),
        };
        let reason = d.reason.trim();
        return DecisionLine {
            text: if reason.is_empty() || matches!(d.state, DecisionState::MergedInto(_)) {
                text
            } else {
                format!("{text} \u{b7} \u{201c}{reason}\u{201d}")
            },
            tone: LineTone::Decided,
            inherited: d.inherited_from.is_some(),
            actions: Vec::new(),
        };
    }
    if let Some(stale) = &row.stale_decision {
        return DecisionLine {
            text: stale_decision_text(stale),
            tone: LineTone::Warning,
            inherited: false,
            actions: vec![LineAction::Reapply, LineAction::Discard],
        };
    }
    if let Some(n) = &row.new_since_decision {
        let verb = match &ctx.node_decision {
            Some(state) => verb_for(state),
            None => "decided",
        };
        let date = short_date(&n.decided_at);
        let on = if date.is_empty() {
            String::new()
        } else {
            format!(" on {date}")
        };
        return DecisionLine {
            text: format!(
                "Unresolved \u{b7} new since you {verb} this group{on} ({} {})",
                n.covered,
                if n.covered == 1 { "other" } else { "others" }
            ),
            tone: LineTone::Warning,
            inherited: false,
            actions: Vec::new(),
        };
    }
    DecisionLine {
        text: "Unresolved".to_string(),
        tone: LineTone::Unresolved,
        inherited: false,
        actions: Vec::new(),
    }
}

fn verb_for(state: &DecisionState) -> &'static str {
    match state {
        DecisionState::Confirmed => "confirmed",
        DecisionState::Ignored => "ignored",
        DecisionState::FalsePositive => "marked false positive",
        DecisionState::TechDebtNow | DecisionState::TechDebtLater => "saved as tech debt",
        DecisionState::MergedInto(_) | DecisionState::EngineRestored => "decided",
    }
}

/// `Unresolved · stale: was Confirmed on 11-04, "policy exists upstream"`.
pub fn stale_decision_text(stale: &DecisionView) -> String {
    let date = short_date(&stale.decided_at);
    let on = if date.is_empty() {
        String::new()
    } else {
        format!(" on {date}")
    };
    let reason = stale.reason.trim();
    let because = if reason.is_empty() {
        String::new()
    } else {
        format!(", \u{201c}{reason}\u{201d}")
    };
    format!(
        "Unresolved \u{b7} stale: was {}{on}{because}",
        decision_label(&stale.state)
    )
}

/// The decision that reapplies a stale one to `row`: the same state and the same reason,
/// recorded now as a new record. Nothing applies until the reviewer asks.
pub fn reapply_decision_request(row: &SiteRow, run_id: &str) -> Option<NewDecisionReq> {
    let stale = row.stale_decision.as_ref()?;
    // A merge's target is a site key of the old run; it cannot be carried over.
    if matches!(stale.state, DecisionState::MergedInto(_)) {
        return None;
    }
    Some(decision_request(
        row,
        stale.state.clone(),
        &stale.reason,
        run_id,
    ))
}

/// The undo bar's words after a reapplied decision.
pub fn reapply_done_message(row: &SiteRow) -> String {
    let state = row
        .stale_decision
        .as_ref()
        .map_or("decision", |d| decision_label(&d.state));
    format!("{state} reapplied.")
}

/// The red line of a held row (spec section 8, "States").
pub fn held_line(row: &SiteRow) -> Option<String> {
    row.held
        .map(|h| format!("Held from export: {}. Decide it to release.", h.phrase()))
}

// ── The head and the stage lines ──────────────────────────────────────────────────────────

/// The chips of the detail's head: the row's markers with the detail's fuller words (spec
/// section 8: `ESC unauthenticated reach #12`, `outlier: different object`), then a chip for a
/// finding the reviewer added.
pub fn head_chips(row: &SiteRow) -> Vec<Chip> {
    let mut chips: Vec<Chip> = marker_chips(row)
        .into_iter()
        .map(|mut chip| {
            if chip.class.starts_with("mark-esc") {
                let s2 = row.stage2.as_ref();
                if let Some(reason) = s2.and_then(|s| s.reason) {
                    chip.label = format!("{} {}", chip.label, reason.phrase());
                }
                if let Some(rank) = s2.and_then(|s| s.global_rank) {
                    chip.label = format!("{} #{rank}", chip.label);
                }
            } else if chip.class == "mark-outlier" {
                chip.label = format!("outlier: {}", chip.label);
            }
            chip
        })
        .collect();
    if row.added_by_reviewer && !chips.iter().any(|c| c.class == "mark-edited") {
        chips.push(Chip {
            class: "mark-edited",
            label: "\u{270e} added".to_string(),
            tip: "The reviewer added this finding; the engine did not report it.".to_string(),
        });
    }
    chips
}

/// How closely the row was reviewed, in words; `None` for a row held for a ruling.
pub fn review_level_text(level: Option<&ReviewLevel>) -> Option<String> {
    level.map(|l| match l {
        ReviewLevel::PersonallyReviewed => "review level: personally reviewed".to_string(),
        ReviewLevel::ReviewedAsPattern(_) => "review level: reviewed as a pattern".to_string(),
        ReviewLevel::EngineVerified => "review level: engine verified".to_string(),
    })
}

/// The note written after the severity when stage 2 disagrees with stage 1. Stage 1's value is
/// never overwritten: the disagreement is written beside it (spec section 8).
pub fn severity_note(row: &SiteRow) -> Option<String> {
    let s2 = row.stage2.as_ref()?;
    let d = s2
        .disagreements
        .iter()
        .find(|d| d.field == DisagreementField::Severity)?;
    let why = d.why.trim();
    Some(if why.is_empty() {
        format!("stage 1 said {}; stage 2 disagrees: {}", d.stage1, d.mine)
    } else {
        format!(
            "stage 1 said {}; stage 2 disagrees: {}, \u{201c}{why}\u{201d}",
            d.stage1, d.mine
        )
    })
}

/// The Stage 1 line: `high · real yes · effort low · "the key is a literal"`; `None` when stage 1
/// did not read the site.
pub fn stage1_line(row: &SiteRow) -> Option<String> {
    let real = match row.stage1.real.as_str() {
        "" => return None,
        other => other,
    };
    let mut parts = vec![row.stage1.severity.clone(), format!("real {real}")];
    if let Some(effort) = row.stage1.effort.as_deref().filter(|e| !e.is_empty()) {
        parts.push(format!("effort {effort}"));
    }
    if !row.stage1.reason.trim().is_empty() {
        parts.push(format!("\u{201c}{}\u{201d}", row.stage1.reason.trim()));
    }
    Some(parts.join(" \u{b7} "))
}

/// The Stage 2 line: `escalated, unauthenticated reach, rank 12 · why "..." · outlier: different
/// object "..."`; `None` when the group reviewer said nothing about the site.
pub fn stage2_line(row: &SiteRow) -> Option<String> {
    let s2 = row.stage2.as_ref()?;
    let mut parts: Vec<String> = Vec::new();
    let state = match s2.state {
        EscalationState::Escalated => Some("escalated"),
        EscalationState::BelowTheLine => Some("below the line"),
        EscalationState::NotACandidate => None,
    };
    if let Some(state) = state {
        let mut head = state.to_string();
        if let Some(reason) = s2.reason {
            head.push_str(&format!(", {}", reason.phrase()));
        }
        if let Some(rank) = s2.global_rank {
            head.push_str(&format!(", rank {rank}"));
        }
        parts.push(head);
        if !s2.why.trim().is_empty() {
            parts.push(format!("why \u{201c}{}\u{201d}", s2.why.trim()));
        }
    }
    if let Some(reason) = s2.outlier_reason {
        let mut text = format!("outlier: {}", outlier_phrase(reason));
        if !s2.outlier_why.trim().is_empty() {
            text.push_str(&format!(" \u{201c}{}\u{201d}", s2.outlier_why.trim()));
        }
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The "Also flagged by" line: `eslint/no-secret (web/env.ts:3) · semgrep/hardcoded-key (...)`;
/// `None` when nothing else flagged the site.
pub fn flagged_by_line(row: &SiteRow) -> Option<String> {
    if row.flagged_by.is_empty() {
        return None;
    }
    Some(
        row.flagged_by
            .iter()
            .map(|f| {
                let who = match &f.tool {
                    Some(tool) if !tool.is_empty() => format!("{tool}/{}", f.rule_id),
                    _ => f.rule_id.clone(),
                };
                format!("{who} ({}:{})", f.path, f.line)
            })
            .collect::<Vec<_>>()
            .join(" \u{b7} "),
    )
}

/// The possible-duplicate line: `SEC-KEY-2 at web/env.ts:3`, with the engine's hint.
pub fn duplicate_line(row: &SiteRow) -> Option<(String, String)> {
    let d = row.possible_duplicate_of.as_ref()?;
    Some((
        format!("{} at {}:{}", d.rule_id, d.path, d.line),
        d.hint.clone(),
    ))
}

// ── The open form ─────────────────────────────────────────────────────────────────────────

/// A field edit that is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDraft {
    pub field: EditField,
    pub value: String,
    pub reason: String,
    pub error: Option<String>,
}

/// The merge picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeDraft {
    pub query: String,
    pub target: Option<String>,
    pub reason: String,
    pub error: Option<String>,
}

/// The split picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitDraft {
    pub choice: SplitChoice,
    pub new_label: String,
    pub reason: String,
    pub error: Option<String>,
}

/// The client acknowledgement form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckDraft {
    pub by: String,
    pub note: String,
    pub error: Option<String>,
}

/// What the detail pane is doing besides showing the site.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DetailMode {
    #[default]
    Viewing,
    /// `E` was pressed and the pane waits for the field's letter.
    PickField,
    Edit(EditDraft),
    Merge(MergeDraft),
    Split(SplitDraft),
    Add(Box<AddDraft>),
    Ack(AckDraft),
}

impl DetailMode {
    /// A form is open (so the tree's keys are off while the reviewer types in it).
    pub fn is_form(&self) -> bool {
        !matches!(self, Self::Viewing | Self::PickField)
    }

    /// The error the open form shows.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Edit(d) => d.error.as_deref(),
            Self::Merge(d) => d.error.as_deref(),
            Self::Split(d) => d.error.as_deref(),
            Self::Add(d) => d.error.as_deref(),
            Self::Ack(d) => d.error.as_deref(),
            Self::Viewing | Self::PickField => None,
        }
    }

    /// Show `message` in the open form.
    pub fn with_error(mut self, message: String) -> Self {
        match &mut self {
            Self::Edit(d) => d.error = Some(message),
            Self::Merge(d) => d.error = Some(message),
            Self::Split(d) => d.error = Some(message),
            Self::Add(d) => d.error = Some(message),
            Self::Ack(d) => d.error = Some(message),
            Self::Viewing | Self::PickField => {}
        }
        self
    }
}

/// What the reviewer did in the pane that changes the form (not a write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailAction {
    /// `E`: wait for the field's letter.
    PickField,
    BeginEdit(EditField),
    BeginMerge,
    BeginSplit,
    /// Open the add form; the line comes from the focused site, or from the code viewer.
    BeginAdd,
    BeginAck,
    /// The main value of the open form: the field's text, the merge search, the new sub-group's
    /// label or who acknowledged.
    Text(String),
    /// The reason (or the acknowledgement's note).
    Reason(String),
    /// A pick in a list: a merge target's key.
    Pick(String),
    Split(SplitChoice),
    Add(AddField, String),
    Cancel,
}

/// The form after `action`. Writing and loading are the controller's; this only changes what is
/// typed and picked. `row` is the focused site.
pub fn reduce(
    mode: DetailMode,
    action: &DetailAction,
    row: &SiteRow,
    ctx: &DetailContext,
) -> DetailMode {
    match action {
        DetailAction::PickField => DetailMode::PickField,
        DetailAction::BeginEdit(field) => {
            if *field == EditField::Subgroup {
                return reduce(mode, &DetailAction::BeginSplit, row, ctx);
            }
            if *field == EditField::Summary {
                return mode;
            }
            DetailMode::Edit(EditDraft {
                field: *field,
                value: current_value(row, *field, ctx),
                reason: String::new(),
                error: None,
            })
        }
        DetailAction::BeginMerge => DetailMode::Merge(MergeDraft {
            query: String::new(),
            target: row
                .possible_duplicate_of
                .as_ref()
                .map(|d| d.key.clone())
                .filter(|k| *k != row.site_key),
            reason: String::new(),
            error: None,
        }),
        DetailAction::BeginSplit => DetailMode::Split(SplitDraft {
            choice: ctx
                .subgroups
                .first()
                .map_or(SplitChoice::New, |l| SplitChoice::Existing(l.clone())),
            new_label: String::new(),
            reason: String::new(),
            error: None,
        }),
        DetailAction::BeginAdd => DetailMode::Add(Box::new(AddDraft::from_row(row))),
        DetailAction::BeginAck => match ack_state(row) {
            AckState::Needed { .. } => DetailMode::Ack(AckDraft {
                by: String::new(),
                note: String::new(),
                error: None,
            }),
            _ => mode,
        },
        DetailAction::Cancel => DetailMode::Viewing,
        DetailAction::Text(text) => match mode {
            DetailMode::Edit(d) => DetailMode::Edit(EditDraft {
                value: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Merge(d) => DetailMode::Merge(MergeDraft {
                query: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Split(d) => DetailMode::Split(SplitDraft {
                new_label: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Ack(d) => DetailMode::Ack(AckDraft {
                by: text.clone(),
                error: None,
                ..d
            }),
            other => other,
        },
        DetailAction::Reason(text) => match mode {
            DetailMode::Edit(d) => DetailMode::Edit(EditDraft {
                reason: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Merge(d) => DetailMode::Merge(MergeDraft {
                reason: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Split(d) => DetailMode::Split(SplitDraft {
                reason: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Ack(d) => DetailMode::Ack(AckDraft {
                note: text.clone(),
                error: None,
                ..d
            }),
            DetailMode::Add(mut d) => {
                d.set(AddField::Reason, text.clone());
                d.error = None;
                DetailMode::Add(d)
            }
            other => other,
        },
        DetailAction::Pick(key) => match mode {
            DetailMode::Merge(d) => DetailMode::Merge(MergeDraft {
                target: Some(key.clone()),
                error: None,
                ..d
            }),
            other => other,
        },
        DetailAction::Split(choice) => match mode {
            DetailMode::Split(d) => DetailMode::Split(SplitDraft {
                choice: choice.clone(),
                error: None,
                ..d
            }),
            other => other,
        },
        DetailAction::Add(field, text) => match mode {
            DetailMode::Add(mut d) => {
                d.set(*field, text.clone());
                d.error = None;
                DetailMode::Add(d)
            }
            other => other,
        },
    }
}

/// What a commit writes. The controller posts it and, on success, shows `done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailWrite {
    Edit {
        req: NewEditReq,
        done: String,
    },
    Decision {
        req: NewDecisionReq,
        done: String,
    },
    Add(AddFindingReq),
    Ack {
        decision_id: String,
        req: ClientAckReq,
    },
}

/// The write the open form makes, or the words that say why it cannot yet. Nothing is posted
/// from a form that fails here, so a missing reason never reaches the server.
pub fn commit(
    mode: &DetailMode,
    row: &SiteRow,
    ctx: &DetailContext,
    run_id: &str,
) -> Result<DetailWrite, String> {
    match mode {
        DetailMode::Viewing | DetailMode::PickField => {
            Err("Nothing is open to commit.".to_string())
        }
        DetailMode::Edit(d) => {
            let req = edit_request(row, d.field, &d.value, &d.reason, ctx, run_id)
                .map_err(|e| e.message())?;
            Ok(DetailWrite::Edit {
                done: edit_done_message(d.field),
                req,
            })
        }
        DetailMode::Merge(d) => {
            let req = merge_request(row, d.target.as_deref(), &d.reason, run_id)
                .map_err(|e| e.message().to_string())?;
            let label = ctx
                .pool
                .iter()
                .find(|p| Some(&p.key) == d.target.as_ref())
                .map(|p| format!("{}:{}", p.path, p.line))
                .or_else(|| {
                    row.possible_duplicate_of
                        .as_ref()
                        .filter(|dup| Some(&dup.key) == d.target.as_ref())
                        .map(|dup| format!("{}:{}", dup.path, dup.line))
                })
                .unwrap_or_else(|| "the other site".to_string());
            Ok(DetailWrite::Decision {
                done: merge_done_message(&label),
                req,
            })
        }
        DetailMode::Split(d) => {
            let label = split_label(&d.choice, &d.new_label)?;
            let req = edit_request(row, EditField::Subgroup, &label, &d.reason, ctx, run_id)
                .map_err(|e| match e {
                    EditRefusal::Unchanged => "The site is already in that sub-group.".to_string(),
                    other => other.message(),
                })?;
            Ok(DetailWrite::Edit {
                done: split_done_message(&label),
                req,
            })
        }
        DetailMode::Add(d) => add_request(d).map(DetailWrite::Add),
        DetailMode::Ack(d) => {
            let (decision_id, req) =
                client_ack_request(row, &d.by, &d.note).map_err(|e| e.message().to_string())?;
            Ok(DetailWrite::Ack { decision_id, req })
        }
    }
}

// ── The undo bar ──────────────────────────────────────────────────────────────────────────

/// What the undo bar reverses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoAction {
    /// Discard the edit with this id ("Undo edit").
    DiscardEdit(String),
    /// Revoke the decision with this id (a merge, a reapplied decision).
    RevokeDecision(String),
}

/// The five second undo bar of the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailUndo {
    pub action: UndoAction,
    pub message: String,
    pub armed_at_ms: u64,
}

impl DetailUndo {
    pub fn new(action: UndoAction, message: String, now_ms: u64) -> Self {
        Self {
            action,
            message,
            armed_at_ms: now_ms,
        }
    }

    /// The button's label.
    pub fn button(&self) -> &'static str {
        match self.action {
            UndoAction::DiscardEdit(_) => "Undo edit",
            UndoAction::RevokeDecision(_) => "Undo",
        }
    }

    /// Whole seconds left in the window at `now_ms`, or `None` once it has closed.
    pub fn seconds_left(&self, now_ms: u64) -> Option<u64> {
        let spent = now_ms.saturating_sub(self.armed_at_ms);
        (spent < UNDO_WINDOW_MS).then(|| (UNDO_WINDOW_MS - spent).div_ceil(1000))
    }
}

// ── Keys ──────────────────────────────────────────────────────────────────────────────────

/// A key on a focused site that opens a form (spec section 11, "Detail").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKey {
    /// `E`: edit a field (then its letter).
    Edit,
    /// `M`: merge into.
    Merge,
    /// `A`: add a finding.
    Add,
    /// `U`: split to a sub-group.
    Split,
}

/// The detail key a press is, in either case.
pub fn detail_key(key: &str) -> Option<DetailKey> {
    match key.to_ascii_lowercase().as_str() {
        "e" => Some(DetailKey::Edit),
        "m" => Some(DetailKey::Merge),
        "a" => Some(DetailKey::Add),
        "u" => Some(DetailKey::Split),
        _ => None,
    }
}

/// What a press means while the pane waits for a field's letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordPress {
    Field(EditField),
    Cancel,
    /// Not a field letter: the press is ignored and the pane keeps waiting.
    Ignore,
}

/// The press after `E`.
pub fn chord_press(key: &str) -> ChordPress {
    if key == "Escape" {
        return ChordPress::Cancel;
    }
    field_for_chord(key).map_or(ChordPress::Ignore, ChordPress::Field)
}

/// A summary of a row's edits by field, oldest first, for the export checks and the tests.
pub fn applied_by_field(row: &SiteRow) -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    for e in row.edits.iter().filter(|e| !e.stale) {
        out.insert(e.field.wire_name(), e.new.clone());
    }
    out
}

#[cfg(test)]
mod tests;
