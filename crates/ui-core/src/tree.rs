//! The tree lane's judgments (review UI spec sections 2, 5, 10 and 11; design A6 step 2).
//!
//! Every judgment the tree screen, the review shell and the export-blocked panel make is a pure
//! function here, with a unit test (spec rule 2). The views in `camerata-ui` choose words and
//! pixels and call these.
//!
//! - this file: the pieces of one row and one node header (counts line, chips, cells) and the
//!   one place a site key or node id is made safe for an element id, an `href` or a `data-`
//!   attribute ([`dom_token`]);
//! - [`state`]: which nodes are open, which sites are loaded (500 at a time, per node) and
//!   where the cursor is;
//! - [`keys`]: what a key press means, only while the lane's region is armed;
//! - [`batch`]: the plan a batch decision makes, its sentence, its request, its undo and the
//!   summary edit;
//! - [`shell`]: the lane switcher and the progress strip;
//! - [`export`]: the export-blocked panel's rows.

pub mod batch;
pub mod export;
pub mod keys;
pub mod shell;
pub mod state;

use camerata_api_types::audit::FindingKind;
use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review::{EscalationReason, EscalationState, OutlierReason};
use camerata_api_types::review_view::{Marker, NodeCountsView, SiteRow, TreeLevel};

// ── What a screen is showing ──────────────────────────────────────────────────────────────

/// A view that may still be loading or may have failed (the empty, loading and error states of
/// spec section 5 for every screen that reads the server).
#[derive(Debug, Clone, PartialEq)]
pub enum ViewStatus<T> {
    Loading,
    /// The server's reason, in words.
    Failed(String),
    Ready(T),
}

impl<T> ViewStatus<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(v) => Some(v),
            _ => None,
        }
    }
}

// ── Keys in the DOM ───────────────────────────────────────────────────────────────────────

/// A site key or node id as a token that is safe in an element id, an `href`, a `data-`
/// attribute and a URL: lowercase hex of its bytes.
///
/// A site key is `repo NUL path NUL fingerprint NUL ordinal`. A raw NUL in an attribute is
/// invalid markup and a webview may cut the attribute at it, so no screen builds an id, an
/// attribute or a URL from a key directly. This is the one place it is encoded, and
/// [`from_dom_token`] is the one place it is decoded.
pub fn dom_token(raw: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(raw.len() * 2);
    for b in raw.bytes() {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The key or node id a [`dom_token`] was made from. `None` for text that is not a token.
pub fn from_dom_token(token: &str) -> Option<String> {
    if token.is_empty() || !token.len().is_multiple_of(2) || !token.is_ascii() {
        return None;
    }
    let bytes = (0..token.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(token.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

// ── Node headers ──────────────────────────────────────────────────────────────────────────

/// How far a node's header is indented: category at 0, rule at 16px, sub-group at 32px
/// (spec section 2, "Tree indentation").
pub fn indent_px(level: TreeLevel) -> u32 {
    match level {
        TreeLevel::Category => 0,
        TreeLevel::Rule => 16,
        TreeLevel::Subgroup => 32,
    }
}

/// `1204` as `1,204`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Which term of a node's counts line (spec section 2, "Counts line").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountKind {
    Sites,
    Escalated,
    Outliers,
    NewSinceDecision,
    Stale,
    Held,
    Decided,
}

/// How a counts term is coloured: a node with an escalation shows its count in amber, one with
/// an outlier or a new-since-decision member in orange, a held one in red, the rest in plain
/// gray (spec section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountTone {
    Plain,
    Amber,
    Orange,
    Red,
}

/// One term of the counts line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountTerm {
    pub kind: CountKind,
    pub tone: CountTone,
    pub text: String,
}

/// The terms of a node's counts line, zero terms omitted (the sites term always stays):
/// `312 sites`, `4 esc`, `2 outl`, `12 new`, `3 stale`, `1 held`, `298 decided`.
pub fn count_terms(c: &NodeCountsView) -> Vec<CountTerm> {
    let mut terms = vec![CountTerm {
        kind: CountKind::Sites,
        tone: CountTone::Plain,
        text: if c.sites == 1 {
            "1 site".to_string()
        } else {
            format!("{} sites", thousands(c.sites))
        },
    }];
    let mut push = |kind: CountKind, tone: CountTone, n: usize, word: &str| {
        if n > 0 {
            terms.push(CountTerm {
                kind,
                tone,
                text: format!("{} {word}", thousands(n)),
            });
        }
    };
    push(CountKind::Escalated, CountTone::Amber, c.escalated, "esc");
    push(CountKind::Outliers, CountTone::Orange, c.outliers, "outl");
    push(
        CountKind::NewSinceDecision,
        CountTone::Orange,
        c.new_since_decision,
        "new",
    );
    push(CountKind::Stale, CountTone::Plain, c.stale, "stale");
    push(CountKind::Held, CountTone::Red, c.held, "held");
    push(CountKind::Decided, CountTone::Plain, c.decided, "decided");
    terms
}

/// The counts line as one string: `312 sites · 4 esc · 2 outl · 12 new · 298 decided`.
pub fn counts_line(c: &NodeCountsView) -> String {
    count_terms(c)
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// Every site of the node is decided, so its header shows a green check before its label.
pub fn is_fully_decided(c: &NodeCountsView) -> bool {
    c.sites > 0 && c.decided >= c.sites
}

// ── Chips ─────────────────────────────────────────────────────────────────────────────────

/// One chip on a site row (spec section 2): its CSS class (in `style.rs` beside `.kind-chip`),
/// its label and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub class: &'static str,
    pub label: String,
    pub tip: String,
}

/// An outlier reason in words, for the `mark-outlier` chip.
pub fn outlier_phrase(reason: OutlierReason) -> &'static str {
    match reason {
        OutlierReason::SeverityRaiseHeld => "severity raise held",
        OutlierReason::DifferentObject => "different object",
        OutlierReason::DifferentConstruct => "different construct",
        OutlierReason::OnlySiteInFile => "only site in file",
        OutlierReason::Stage2Outlier => "outlier in group",
        OutlierReason::PreconditionAdded => "precondition added",
        OutlierReason::UnverifiedName => "unverified name",
        OutlierReason::FixUngrounded => "fix not grounded",
        OutlierReason::Contradiction => "contradiction",
        OutlierReason::EvidenceUnverified => "evidence unverified",
        OutlierReason::InTestCode => "in test code",
        OutlierReason::CouldBeIntentional => "could be intentional",
        OutlierReason::MethodologyLeak => "wording flag",
    }
}

/// `2026-11-04T09:00:00Z` as `11-04`; empty for text that is not a date.
pub fn short_date(stamp: &str) -> String {
    let b = stamp.as_bytes();
    let shaped = b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit);
    if shaped {
        stamp[5..10].to_string()
    } else {
        String::new()
    }
}

/// A decision's state in words (the Decision column).
pub fn decision_label(state: &DecisionState) -> &'static str {
    match state {
        DecisionState::Confirmed => "Confirmed",
        DecisionState::Ignored => "Ignored",
        DecisionState::FalsePositive => "False positive",
        DecisionState::TechDebtNow => "Tech debt, now",
        DecisionState::TechDebtLater => "Tech debt, later",
        DecisionState::MergedInto(_) => "Merged",
        DecisionState::EngineRestored => "Put back",
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The chips a row has, in the precedence the screens draw them (spec section 2, "Noise rule"):
/// held, escalated or candidate, outlier, new since decision, stale, moved, possible duplicate,
/// edited. An over-budget escalation is one `ESC` chip that says so, and not a second chip.
pub fn marker_chips(row: &SiteRow) -> Vec<Chip> {
    let over = row.markers.contains(&Marker::OverBudget);
    let mut chips = Vec::new();
    for marker in &row.markers {
        let chip = match marker {
            Marker::Held => Chip {
                class: "mark-held",
                label: "\u{1f512} held".to_string(),
                tip: row.held.map_or_else(
                    || "Held from export until it is decided.".to_string(),
                    |h| format!("Held from export: {}. Decide it to release.", h.phrase()),
                ),
            },
            Marker::Escalated => Chip {
                class: if over {
                    "mark-esc mark-over"
                } else {
                    "mark-esc"
                },
                label: if over { "ESC over" } else { "ESC" }.to_string(),
                tip: escalation_tip(row, over),
            },
            Marker::OverBudget => {
                if row.markers.contains(&Marker::Escalated) {
                    continue;
                }
                Chip {
                    class: "mark-over",
                    label: "over".to_string(),
                    tip: "Past the escalation budget, for a reason that may exceed it.".to_string(),
                }
            }
            Marker::Candidate => Chip {
                class: "mark-cand",
                label: "candidate".to_string(),
                tip: "The group reviewer wanted this escalated; it is below the line.".to_string(),
            },
            Marker::Outlier => {
                let reason = row.stage2.as_ref().and_then(|s| s.outlier_reason);
                Chip {
                    class: "mark-outlier",
                    label: reason.map_or("outlier", outlier_phrase).to_string(),
                    tip: row
                        .stage2
                        .as_ref()
                        .map(|s| s.outlier_why.clone())
                        .filter(|w| !w.is_empty())
                        .unwrap_or_else(|| "Does not share its sub-group's cause.".to_string()),
                }
            }
            Marker::NewSinceDecision => {
                let date = row
                    .new_since_decision
                    .as_ref()
                    .map(|n| short_date(&n.decided_at))
                    .filter(|d| !d.is_empty());
                Chip {
                    class: "mark-new",
                    label: date.map_or_else(
                        || "\u{25cf} new since decision".to_string(),
                        |d| format!("\u{25cf} new since {d}"),
                    ),
                    tip: row
                        .new_since_decision
                        .as_ref()
                        .map_or_else(String::new, |n| {
                            format!(
                            "A member the group decision did not cover ({} others were covered).",
                            n.covered
                        )
                        }),
                }
            }
            Marker::Stale => {
                let was = row
                    .stale_decision
                    .as_ref()
                    .map(|d| format!("stale: was {}", decision_label(&d.state)));
                Chip {
                    class: "mark-stale",
                    label: was.unwrap_or_else(|| "stale edit".to_string()),
                    tip: "An earlier decision or edit no longer applies. It is shown beside the row and nothing was applied."
                        .to_string(),
                }
            }
            Marker::Moved => Chip {
                class: "mark-moved",
                label: row
                    .moved_from
                    .as_ref()
                    .and_then(|m| m.line)
                    .map_or_else(|| "moved".to_string(), |l| format!("moved, was line {l}")),
                tip: "The code moved; the row has a new key.".to_string(),
            },
            Marker::PossibleDuplicate => {
                let dup = row.possible_duplicate_of.as_ref();
                Chip {
                    class: "mark-dup",
                    label: dup.map_or_else(
                        || "dup?".to_string(),
                        |d| format!("dup? {} at {}:{}", d.rule_id, base_name(&d.path), d.line),
                    ),
                    tip: dup.map_or_else(String::new, |d| d.hint.clone()),
                }
            }
            Marker::Edited => Chip {
                class: "mark-edited",
                label: "\u{270e} edited".to_string(),
                tip: "The reviewer changed a field of this row.".to_string(),
            },
        };
        chips.push(chip);
    }
    chips
}

fn escalation_tip(row: &SiteRow, over: bool) -> String {
    let reason = row
        .stage2
        .as_ref()
        .and_then(|s| s.reason)
        .map(EscalationReason::phrase);
    let rank = row.stage2.as_ref().and_then(|s| s.global_rank);
    let mut tip = String::from("In the escalation queue");
    if let Some(r) = reason {
        tip.push_str(&format!(" for {r}"));
    }
    if let Some(n) = rank {
        tip.push_str(&format!(", rank {n}"));
    }
    if over {
        tip.push_str(", past the budget");
    }
    tip.push('.');
    tip
}

/// The Markers column of a row: at most two chips, the rest folded into `+N` with a tooltip
/// listing them (spec section 2, "Noise rule").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerCell {
    pub shown: Vec<Chip>,
    pub folded: Vec<Chip>,
}

/// How many chips the Markers column shows before it folds the rest.
pub const MARKERS_SHOWN: usize = 2;

impl MarkerCell {
    /// `+3`, when chips are folded.
    pub fn fold_label(&self) -> Option<String> {
        (!self.folded.is_empty()).then(|| format!("+{}", self.folded.len()))
    }

    /// The folded chips' labels, one per line, for the `+N` tooltip.
    pub fn fold_tip(&self) -> String {
        self.folded
            .iter()
            .map(|c| c.label.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The Markers column of `row`.
pub fn marker_cell(row: &SiteRow) -> MarkerCell {
    let mut chips = marker_chips(row);
    let folded = chips.split_off(MARKERS_SHOWN.min(chips.len()));
    MarkerCell {
        shown: chips,
        folded,
    }
}

// ── Cells ─────────────────────────────────────────────────────────────────────────────────

/// A severity in the words a badge shows (`high` is `High`).
pub fn severity_label(severity: &str) -> String {
    let mut chars = severity.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The chorale badge colour of a severity (spec section 2).
pub fn severity_badge(severity: &str) -> &'static str {
    match severity {
        "critical" => "red",
        "high" => "orange",
        "medium" => "yellow",
        "low" => "gray",
        _ => "default",
    }
}

/// The Stage 1 column: whether stage 1 judged the finding real as one letter, then the effort.
/// `Y low`, `N`, `? high`; `unread` when stage 1 did not read the site.
pub fn stage1_cell(row: &SiteRow) -> String {
    let letter = match row.stage1.real.as_str() {
        "yes" => "Y",
        "no" => "N",
        "unclear" => "?",
        _ => return "unread".to_string(),
    };
    match row.stage1.effort.as_deref().filter(|e| !e.is_empty()) {
        Some(effort) => format!("{letter} {effort}"),
        None => letter.to_string(),
    }
}

/// The Stage 2 column: the escalation reason, else the outlier reason, in words; blank when the
/// group reviewer said nothing about the site.
pub fn stage2_cell(row: &SiteRow) -> String {
    let Some(s) = row.stage2.as_ref() else {
        return String::new();
    };
    if s.state != EscalationState::NotACandidate {
        if let Some(reason) = s.reason {
            return capitalize(reason.phrase());
        }
    }
    s.outlier_reason
        .map(|r| capitalize(outlier_phrase(r)))
        .unwrap_or_default()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How the Decision column draws its cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionTone {
    /// A decision stands: green.
    Decided,
    /// Nobody has decided it: gray.
    Unresolved,
}

/// The Decision column of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionCell {
    pub label: String,
    pub tone: DecisionTone,
    /// The decision came from a node and not from the reviewer looking at this site.
    pub inherited: bool,
}

/// The Decision column: the state label (green when decided, `Unresolved` gray), `inherited`
/// when it came from a node, and `Merged into path:line` for a merge.
pub fn decision_cell(row: &SiteRow) -> DecisionCell {
    match &row.decision {
        Some(d) => {
            let label = match (&d.state, &row.merged_into) {
                (DecisionState::MergedInto(_), Some(m)) => format!("Merged into {}", m.label),
                (state, _) => decision_label(state).to_string(),
            };
            DecisionCell {
                label,
                tone: DecisionTone::Decided,
                inherited: d.inherited_from.is_some(),
            }
        }
        None => DecisionCell {
            label: "Unresolved".to_string(),
            tone: DecisionTone::Unresolved,
            inherited: false,
        },
    }
}

/// `path:line`, the File:line column.
pub fn file_line(row: &SiteRow) -> String {
    format!("{}:{}", row.path, row.line)
}

/// The kind tag of a row, for the kind chip.
pub fn kind_tag_of(row: &SiteRow) -> &'static str {
    FindingKind::tag_or_unspecified(row.kind.as_ref())
}

/// A row is an outlier: the group reviewer or the engine says it does not belong with its
/// sub-group.
pub fn is_outlier(row: &SiteRow) -> bool {
    row.markers.contains(&Marker::Outlier)
        || row
            .stage2
            .as_ref()
            .is_some_and(|s| s.outlier_reason.is_some())
}

#[cfg(test)]
mod tests;
