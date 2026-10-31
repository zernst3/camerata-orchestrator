//! Decision memory (work order B.5): "the same rule on the same shape of line gets the same
//! answer", shown to the reviewer and offered as a pre-filled reason, never applied.
//!
//! ## What it is
//!
//! [`prior_decisions`] looks at the focused finding and finds the rows ALREADY DECIDED IN THIS
//! DRAFT that share its shape: the same rule id and the same fingerprint of the offending snippet
//! (`camerata_api_types::fingerprint::fingerprint`, the very function the suppression baseline
//! commits its values from, so "same shape" means exactly what it means there: whitespace is
//! collapsed, a real edit to the code is a different shape). It answers with a [`PriorDecision`]
//! the pane and the viewer popover turn into a chip, and whose [`reason_for`] pre-fills the reason
//! prompt when the reviewer presses `I` or `F`.
//!
//! The work is done by a [`DecisionIndex`], built once from the rows and the disposition map (the
//! fingerprint is computed for DECIDED rows only) and then asked per finding; [`prior_decisions`]
//! is the one-shot form of that and [`shared_prefill`] builds one index for a whole set of cards.
//!
//! ## What it is not
//!
//! * It applies nothing. The result is text. A disposition is written only by a key press that goes
//!   through `apply_rows`, with the reviewer's own Enter on the reason prompt; a pre-filled reason
//!   is just the starting text of a field the reviewer can edit, and what is stored is whatever the
//!   field holds at Enter.
//! * It keeps nothing. There is no store: the memory is DERIVED from the draft's disposition map
//!   every time it is asked, so an undo (which restores the map) removes the memory with it, and
//!   nothing needs to be kept in step. It is scoped to this draft; cross-inspection memory would
//!   need a durable record the Q2 ruling deliberately did not create.
//! * It never offers a Confirmed or Tech debt decision as something to reuse. Those carry no
//!   reason, so there is nothing to pre-fill, and a chip saying "you confirmed this before" would
//!   only nudge the one decision the work order wants made on the evidence in front of the
//!   reviewer. They are counted only to be honest about disagreement (below).
//! * It never offers a reason across states. A reason given for Ignored ("accepted risk") is a
//!   false statement under False positive ("the tool was wrong"), so `I` is only ever offered a
//!   reason given for Ignored and `F` one given for False positive. The chip still SHOWS the prior
//!   decision whatever its state.
//!
//! ## Across repos
//!
//! The shape is `(rule_id, fingerprint)` and does not include the repo, on purpose: the same rule
//! on the same line of code across one client's repositories is usually one answer, and since the
//! memory only pre-fills text that the reviewer must still confirm with Enter, a wrong match costs
//! one edit, not a wrong decision. (A test pins this.)
//!
//! The exception is a secret-exposure finding ([`FindingView::is_secret_exposure`]). Its snippet is
//! redacted to a placeholder, so every leaked credential has the same fingerprint, and "false
//! positive: fixture key" decided in one repo would pre-fill the reason on another repo's real
//! leak. For those findings the repo is part of the shape. (Tests pin this too.)
//!
//! ## When prior decisions disagree
//!
//! The rows on a shape may have been decided differently: three ignored for one reason, one false
//! positive, two confirmed. The memory does not pick silently. Reasons are grouped on their
//! whitespace-collapsed, case-folded text, so the same words typed with different spacing or
//! capitalisation are one decision (the first one seen is kept as the text to pre-fill). The
//! primary is the most common reason-carrying decision (ties broken by state, Ignored before False
//! positive, then by the folded reason text, so the answer never depends on iteration order); the
//! chip names it with its row count, and says `and N other rows decided differently` for every
//! other decided row on the shape, whether it carries another reason or is Confirmed or Tech debt.
//! If only Confirmed or Tech debt rows exist on the shape there is no primary, so there is no chip.
//!
//! The reason text is the reviewer's own, from this draft. It leaves the app through nothing here.
//!
//! [`reason_for`]: PriorDecision::reason_for

use std::collections::{HashMap, HashSet};

use camerata_api_types::fingerprint::fingerprint;

use crate::triage::{finding_key, Disposition, FindingView, TriageState};
use crate::viewer_find::cut_chars;

/// What the chip starts with.
pub const CHIP_PREFIX: &str = "Previously on this shape: ";

/// How much of a reason the chip shows; the pre-fill always carries the whole of it.
pub const CHIP_REASON_CHARS: usize = 120;

/// The rows on a shape that were decided with one particular state and (folded) reason.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReasonGroup {
    state: TriageState,
    /// The first-seen wording, trimmed: what the chip shows and the pre-fill carries.
    reason: String,
    /// The grouping key: [`fold`] of the reason.
    folded: String,
    rows: usize,
}

/// What the draft already decided about the focused finding's shape. See the module comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriorDecision {
    /// The most common reason-carrying decision on this shape: `Ignored` or `FalsePositive`.
    pub state: TriageState,
    /// Its reason, trimmed, in the wording first seen. Empty only when the draft holds a
    /// reason-carrying state with no text (an older draft); the chip then shows the state alone.
    pub reason: String,
    /// How many rows carry that decision (the same state and the same words).
    pub rows: usize,
    /// Every OTHER decided row on the shape: a different reason, a different reason-carrying
    /// state, Confirmed or Tech debt. Zero means the draft is unanimous.
    pub others: usize,
    /// Every reason-carrying group on the shape, most common first. Feeds [`reason_for`].
    ///
    /// [`reason_for`]: PriorDecision::reason_for
    groups: Vec<ReasonGroup>,
}

impl PriorDecision {
    /// The chip text: `Previously on this shape: Ignored, 'test fixture, not shipped' (3 rows)`,
    /// plus `, and 1 other row decided differently` when the draft disagrees with itself.
    ///
    /// Plain text. The reason is whitespace-collapsed (a pasted multi-line reason stays one line)
    /// and cut at [`CHIP_REASON_CHARS`]; it is rendered as a text node by every caller, never as
    /// markup.
    pub fn chip_text(&self) -> String {
        let mut text = format!("{CHIP_PREFIX}{}", self.state.label());
        let reason = collapse(&self.reason);
        if !reason.is_empty() {
            text.push_str(&format!(", '{}'", cut_chars(&reason, CHIP_REASON_CHARS)));
        }
        text.push_str(&format!(" ({} {})", self.rows, row_noun(self.rows)));
        if self.others > 0 {
            text.push_str(&format!(
                ", and {} other {} decided differently",
                self.others,
                row_noun(self.others)
            ));
        }
        text
    }

    /// The reason to pre-fill the prompt for `state` (`Ignored` or `False positive`): the most
    /// common reason the draft already gave for THAT state on this shape. `None` when it gave none
    /// for that state (a shape only ever ignored offers nothing under `F`: an accepted-risk reason
    /// is a false statement about a tool error), when the reason is blank, or when `state` takes
    /// no reason.
    pub fn reason_for(&self, state: TriageState) -> Option<&str> {
        if !state.requires_reason() {
            return None;
        }
        self.groups
            .iter()
            .find(|g| g.state == state && !g.reason.is_empty())
            .map(|g| g.reason.as_str())
    }
}

fn row_noun(n: usize) -> &'static str {
    if n == 1 {
        "row"
    } else {
        "rows"
    }
}

/// `text` with its whitespace runs collapsed to single spaces and the ends trimmed.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What two reasons are compared on: the collapsed text, case-folded. "Test fixture" and
/// "test  fixture\n" are the same decision.
fn fold(text: &str) -> String {
    collapse(text).to_lowercase()
}

/// Ranks the reason-carrying states for a tie: Ignored before False positive.
fn state_rank(state: TriageState) -> u8 {
    match state {
        TriageState::Ignored => 0,
        _ => 1,
    }
}

/// One decided row on a shape.
#[derive(Clone, Debug)]
struct Decided {
    key: String,
    state: TriageState,
    reason: String,
}

/// Every decided row of a finding set, bucketed by shape, so each question about a finding is a
/// lookup. Build it once per set of questions: the fingerprint is computed here, for decided rows
/// only, and never again.
#[derive(Clone, Debug, Default)]
pub struct DecisionIndex {
    by_shape: HashMap<Shape, Vec<Decided>>,
}

/// What two findings must share for one's decision to be offered on the other: the rule, the
/// snippet fingerprint and, for a secret-exposure finding only, the repo. See the module comment.
type Shape = (String, String, String);

/// The shape of `f`. The repo is part of it only for a secret-exposure finding, whose snippet is a
/// redacted placeholder (the same text for every credential), so a decision about a fixture key in
/// one repo must not be offered on a real leak in another.
fn shape_of(f: &FindingView) -> Shape {
    let scope = if f.is_secret_exposure() {
        f.repo.clone()
    } else {
        String::new()
    };
    (
        f.rule_id.clone(),
        fingerprint(&f.rule_id, &f.snippet),
        scope,
    )
}

impl DecisionIndex {
    /// Index `rows` against `dispositions`. A row counts when it has a decision other than
    /// Unresolved and a snippet (a blank snippet is every blank-snippet finding of the rule, not a
    /// shape); a finding listed twice counts once; a disposition for a finding not in `rows` is not
    /// evidence about this scan. The repo is part of the shape for a secret-exposure finding only
    /// ([`shape_of`]; see the module comment).
    pub fn build(rows: &[FindingView], dispositions: &HashMap<String, Disposition>) -> Self {
        let mut seen = HashSet::new();
        let mut by_shape: HashMap<Shape, Vec<Decided>> = HashMap::new();
        for f in rows {
            let key = finding_key(f);
            let Some(d) = dispositions.get(&key) else {
                continue;
            };
            if !d.state.is_decided() || f.snippet.trim().is_empty() || !seen.insert(key.clone()) {
                continue;
            }
            by_shape.entry(shape_of(f)).or_default().push(Decided {
                key,
                state: d.state,
                reason: d.reason.trim().to_string(),
            });
        }
        Self { by_shape }
    }

    /// What was decided about `focused`'s shape, not counting `focused` itself, or `None` when no
    /// reason-carrying decision exists on it (and for a finding with no snippet).
    pub fn prior(&self, focused: &FindingView) -> Option<PriorDecision> {
        if focused.snippet.trim().is_empty() {
            return None;
        }
        let shape = shape_of(focused);
        let own = finding_key(focused);
        let mut groups: Vec<ReasonGroup> = Vec::new();
        // Decided rows on the shape that carry no reason (Confirmed, Tech debt): never a primary,
        // but part of the honest "decided differently" count.
        let mut reasonless = 0usize;
        for d in self.by_shape.get(&shape)?.iter().filter(|d| d.key != own) {
            if !d.state.requires_reason() {
                reasonless += 1;
                continue;
            }
            let folded = fold(&d.reason);
            match groups
                .iter_mut()
                .find(|g| g.state == d.state && g.folded == folded)
            {
                Some(g) => g.rows += 1,
                None => groups.push(ReasonGroup {
                    state: d.state,
                    reason: d.reason.clone(),
                    folded,
                    rows: 1,
                }),
            }
        }
        groups.sort_by(|a, b| {
            b.rows
                .cmp(&a.rows)
                .then(state_rank(a.state).cmp(&state_rank(b.state)))
                .then_with(|| a.folded.cmp(&b.folded))
        });
        let primary = groups.first()?.clone();
        let reasoned: usize = groups.iter().map(|g| g.rows).sum();
        Some(PriorDecision {
            state: primary.state,
            reason: primary.reason,
            rows: primary.rows,
            others: reasoned - primary.rows + reasonless,
            groups,
        })
    }
}

/// The decisions already made in this draft on `focused`'s shape, or `None` when there is nothing
/// worth showing. The one-shot form of [`DecisionIndex::build`] then [`DecisionIndex::prior`]; a
/// caller with several questions builds the index itself.
pub fn prior_decisions(
    rows: &[FindingView],
    dispositions: &HashMap<String, Disposition>,
    focused: &FindingView,
) -> Option<PriorDecision> {
    DecisionIndex::build(rows, dispositions).prior(focused)
}

/// The reason to pre-fill for ONE focused finding pressing `I` or `F`: what its chip offers for
/// that state, or an empty string. The pane and the viewer call this; the stack and the toolbar
/// call [`shared_prefill`].
pub fn prefill_reason(prior: Option<&PriorDecision>, state: TriageState) -> String {
    prior
        .and_then(|p| p.reason_for(state))
        .unwrap_or_default()
        .to_string()
}

/// The reason to pre-fill for a SET of rows (the stacked view's checked cards, the toolbar's
/// selection) deciding `state`: the reason every one of them would be offered, when they are all
/// offered the same one (compared on the folded text), and nothing otherwise. One row with no
/// prior, or two with different priors, empties it: pre-filling a reason that is true of some of
/// the rows would put a sentence in the export for rows it does not describe, one Enter away.
pub fn shared_prefill(
    targets: &[FindingView],
    rows: &[FindingView],
    dispositions: &HashMap<String, Disposition>,
    state: TriageState,
) -> String {
    if targets.is_empty() || !state.requires_reason() {
        return String::new();
    }
    let index = DecisionIndex::build(rows, dispositions);
    let mut shared: Option<String> = None;
    for t in targets {
        let offered = prefill_reason(index.prior(t).as_ref(), state);
        if offered.is_empty() {
            return String::new();
        }
        match &shared {
            None => shared = Some(offered),
            Some(s) if fold(s) == fold(&offered) => {}
            Some(_) => return String::new(),
        }
    }
    shared.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::{TechDebtBucket, TriageModel};
    use std::collections::HashMap;

    fn finding(rule: &str, line: usize, snippet: &str) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": line, "rule_id": rule,
            "severity": "high", "snippet": snippet, "detail": "d", "status": "active",
            "kind": "deviation", "deviating_sites": 3, "assessed_sites": 40,
            "convention_label": "URI versioning"
        }))
        .expect("fixture")
    }

    fn decided(
        rows: &[&FindingView],
        state: TriageState,
        reason: &str,
    ) -> HashMap<String, Disposition> {
        rows.iter()
            .map(|f| {
                (
                    finding_key(f),
                    Disposition {
                        state,
                        reason: reason.into(),
                        bucket: TechDebtBucket::Later,
                    },
                )
            })
            .collect()
    }

    fn merged(parts: Vec<HashMap<String, Disposition>>) -> HashMap<String, Disposition> {
        parts.into_iter().flatten().collect()
    }

    /// B.5 check 1, pure half: a row decided Ignored with a reason is remembered for the next row
    /// of the same rule and snippet.
    #[test]
    fn a_decided_row_on_the_same_shape_is_remembered() {
        let a = finding("R-1", 1, "let x = unsafe_call();");
        let b = finding("R-1", 9, "let x = unsafe_call();");
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::Ignored, "test fixture, not shipped");
        let p = prior_decisions(&rows, &d, &b).expect("a prior");
        assert_eq!(p.state, TriageState::Ignored);
        assert_eq!(p.reason, "test fixture, not shipped");
        assert_eq!((p.rows, p.others), (1, 0));
        assert_eq!(
            p.chip_text(),
            "Previously on this shape: Ignored, 'test fixture, not shipped' (1 row)"
        );
        // And it is not mutual until the other is decided too: `a` sees nothing in `b`.
        assert!(prior_decisions(&rows, &d, &a).is_none());
    }

    #[test]
    fn three_rows_read_as_rows_and_false_positive_is_remembered_too() {
        let rows: Vec<FindingView> = (1..=4).map(|n| finding("R-1", n, "same")).collect();
        let d = decided(
            &[&rows[0], &rows[1], &rows[2]],
            TriageState::FalsePositive,
            "parser misread",
        );
        let p = prior_decisions(&rows, &d, &rows[3]).unwrap();
        assert_eq!(
            p.chip_text(),
            "Previously on this shape: False positive, 'parser misread' (3 rows)"
        );
    }

    #[test]
    fn the_snippet_is_compared_by_fingerprint_so_whitespace_does_not_matter_and_an_edit_does() {
        let a = finding("R-1", 1, "let  x =\tcall();");
        let same = finding("R-1", 2, "  let x = call();  ");
        let edited = finding("R-1", 3, "let x = call2();");
        let rows = vec![a.clone(), same.clone(), edited.clone()];
        let d = decided(&[&a], TriageState::Ignored, "fixture");
        assert!(prior_decisions(&rows, &d, &same).is_some());
        assert!(
            prior_decisions(&rows, &d, &edited).is_none(),
            "a different fingerprint shows no chip"
        );
    }

    #[test]
    fn the_same_snippet_under_another_rule_is_another_shape() {
        let a = finding("R-1", 1, "same");
        let other = finding("R-2", 2, "same");
        let rows = vec![a.clone(), other.clone()];
        let d = decided(&[&a], TriageState::Ignored, "fixture");
        assert!(prior_decisions(&rows, &d, &other).is_none());
    }

    #[test]
    fn confirmed_and_tech_debt_never_produce_a_chip() {
        let a = finding("R-1", 1, "same");
        let b = finding("R-1", 2, "same");
        let rows = vec![a.clone(), b.clone()];
        for state in [TriageState::Confirmed, TriageState::TechDebt] {
            let d = decided(&[&a], state, "");
            assert!(prior_decisions(&rows, &d, &b).is_none(), "{state:?}");
            // Even if the draft carries reason text on them.
            let d = decided(&[&a], state, "some old text");
            assert!(prior_decisions(&rows, &d, &b).is_none(), "{state:?}");
        }
        // An explicit Unresolved record (a row moved back) is not a decision.
        let d = decided(&[&a], TriageState::Unresolved, "");
        assert!(prior_decisions(&rows, &d, &b).is_none());
    }

    #[test]
    fn the_focused_row_never_counts_for_itself() {
        let a = finding("R-1", 1, "same");
        let rows = vec![a.clone()];
        let d = decided(&[&a], TriageState::Ignored, "fixture");
        assert!(prior_decisions(&rows, &d, &a).is_none());
    }

    #[test]
    fn a_disposition_for_a_finding_outside_the_row_set_is_not_evidence() {
        let a = finding("R-1", 1, "same");
        let gone = finding("R-1", 2, "same");
        let rows = vec![a.clone()];
        let d = decided(&[&gone], TriageState::Ignored, "fixture");
        assert!(prior_decisions(&rows, &d, &a).is_none());
        // A row listed twice is counted once.
        let twice = vec![gone.clone(), gone.clone(), a.clone()];
        assert_eq!(prior_decisions(&twice, &d, &a).unwrap().rows, 1);
    }

    #[test]
    fn a_blank_snippet_is_not_a_shape() {
        let a = finding("R-1", 1, "");
        let b = finding("R-1", 2, "   ");
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::Ignored, "fixture");
        assert!(prior_decisions(&rows, &d, &b).is_none());
    }

    /// The disagreement rule, documented in the module comment: the most common reason-carrying
    /// decision is the primary, and every other decided row on the shape is counted out loud.
    #[test]
    fn disagreeing_decisions_are_said_not_picked_silently() {
        let rows: Vec<FindingView> = (1..=8).map(|n| finding("R-1", n, "same")).collect();
        let d = merged(vec![
            decided(
                &[&rows[0], &rows[1], &rows[2]],
                TriageState::Ignored,
                "fixture",
            ),
            decided(&[&rows[3]], TriageState::Ignored, "legacy path"),
            decided(&[&rows[4]], TriageState::FalsePositive, "parser misread"),
            decided(&[&rows[5]], TriageState::Confirmed, ""),
            decided(&[&rows[6]], TriageState::TechDebt, ""),
        ]);
        let p = prior_decisions(&rows, &d, &rows[7]).unwrap();
        assert_eq!(
            (p.state, p.reason.as_str(), p.rows),
            (TriageState::Ignored, "fixture", 3)
        );
        assert_eq!(p.others, 4, "legacy, parser misread, confirmed, tech debt");
        assert_eq!(
            p.chip_text(),
            "Previously on this shape: Ignored, 'fixture' (3 rows), and 4 other rows decided differently"
        );
    }

    #[test]
    fn one_dissenting_row_reads_as_and_1_other_row() {
        let rows: Vec<FindingView> = (1..=4).map(|n| finding("R-1", n, "same")).collect();
        let d = merged(vec![
            decided(&[&rows[0], &rows[1]], TriageState::Ignored, "fixture"),
            decided(&[&rows[2]], TriageState::Confirmed, ""),
        ]);
        let p = prior_decisions(&rows, &d, &rows[3]).unwrap();
        assert!(p
            .chip_text()
            .ends_with("(2 rows), and 1 other row decided differently"));
    }

    #[test]
    fn a_tie_is_broken_by_state_then_text_so_it_never_depends_on_order() {
        let rows: Vec<FindingView> = (1..=3).map(|n| finding("R-1", n, "same")).collect();
        let d = merged(vec![
            decided(&[&rows[0]], TriageState::FalsePositive, "aaa"),
            decided(&[&rows[1]], TriageState::Ignored, "zzz"),
        ]);
        let p = prior_decisions(&rows, &d, &rows[2]).unwrap();
        assert_eq!(
            (p.state, p.reason.as_str()),
            (TriageState::Ignored, "zzz"),
            "Ignored ranks before False positive on a tie"
        );
        // Same state: the text decides, and the row order in the slice does not.
        let rows2: Vec<FindingView> = (1..=3).map(|n| finding("R-1", n, "same")).collect();
        let d2 = merged(vec![
            decided(&[&rows2[0]], TriageState::Ignored, "b"),
            decided(&[&rows2[1]], TriageState::Ignored, "a"),
        ]);
        let mut reversed = rows2.clone();
        reversed.reverse();
        assert_eq!(
            prior_decisions(&rows2, &d2, &rows2[2]),
            prior_decisions(&reversed, &d2, &rows2[2])
        );
        assert_eq!(prior_decisions(&rows2, &d2, &rows2[2]).unwrap().reason, "a");
    }

    /// An undo restores the draft's map, and the memory is derived from the map, so it is gone.
    #[test]
    fn undo_removes_the_memory() {
        let a = finding("R-1", 1, "same");
        let b = finding("R-1", 2, "same");
        let rows = vec![a.clone(), b.clone()];
        let mut model = TriageModel::new();
        let mut entry = model
            .dispose_with_undo(std::slice::from_ref(&a), TriageState::Ignored, "fixture", 0)
            .expect("applied");
        assert!(prior_decisions(&rows, &model.dispositions, &b).is_some());
        model.undo(&mut entry, 100);
        assert!(
            prior_decisions(&rows, &model.dispositions, &b).is_none(),
            "the undone decision is no longer remembered"
        );
    }

    #[test]
    fn a_reason_is_trimmed_and_the_chip_collapses_whitespace_and_cuts_a_long_one() {
        let a = finding("R-1", 1, "same");
        let b = finding("R-1", 2, "same");
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::Ignored, "  line one\n   line two  ");
        let p = prior_decisions(&rows, &d, &b).unwrap();
        assert_eq!(p.reason, "line one\n   line two");
        assert!(
            p.chip_text().contains("'line one line two'"),
            "{}",
            p.chip_text()
        );
        let long = "word ".repeat(100);
        let d = decided(&[&a], TriageState::Ignored, &long);
        let p = prior_decisions(&rows, &d, &b).unwrap();
        assert!(p.chip_text().contains("...' (1 row)"), "{}", p.chip_text());
        assert_eq!(
            prefill_reason(Some(&p), TriageState::Ignored),
            long.trim(),
            "the pre-fill carries all of it"
        );
    }

    #[test]
    fn chip_text_has_no_em_dash_and_markup_in_a_reason_is_just_text() {
        let a = finding("R-1", 1, "same");
        let b = finding("R-1", 2, "same");
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::Ignored, "<b>bold</b> & more");
        let text = prior_decisions(&rows, &d, &b).unwrap().chip_text();
        assert!(
            text.contains("<b>bold</b> & more"),
            "carried verbatim: {text}"
        );
        assert!(!text.contains('\u{2014}'));
    }

    /// M1: a reason is only ever offered under the state it was given for.
    #[test]
    fn reason_for_is_same_state_only() {
        let rows: Vec<FindingView> = (1..=5).map(|n| finding("R-1", n, "same")).collect();
        let d = merged(vec![
            decided(&[&rows[0], &rows[1]], TriageState::Ignored, "fixture"),
            decided(&[&rows[2]], TriageState::FalsePositive, "parser misread"),
        ]);
        let p = prior_decisions(&rows, &d, &rows[4]).unwrap();
        assert_eq!(p.reason_for(TriageState::Ignored), Some("fixture"));
        assert_eq!(
            p.reason_for(TriageState::FalsePositive),
            Some("parser misread")
        );
        // Only ignored so far: the chip still shows it, but F is offered nothing (an
        // accepted-risk reason is a false statement about a tool error), and the reverse.
        let only = decided(&[&rows[0]], TriageState::Ignored, "fixture");
        let p = prior_decisions(&rows, &only, &rows[4]).unwrap();
        assert!(p.chip_text().contains("Ignored, 'fixture'"));
        assert_eq!(p.reason_for(TriageState::FalsePositive), None);
        assert_eq!(prefill_reason(Some(&p), TriageState::FalsePositive), "");
        let only_fp = decided(&[&rows[0]], TriageState::FalsePositive, "misread");
        let p = prior_decisions(&rows, &only_fp, &rows[4]).unwrap();
        assert_eq!(p.reason_for(TriageState::Ignored), None);
        // States that take no reason are never pre-filled.
        assert_eq!(p.reason_for(TriageState::Confirmed), None);
        assert_eq!(p.reason_for(TriageState::TechDebt), None);
        assert_eq!(prefill_reason(None, TriageState::Ignored), "");
        assert_eq!(prefill_reason(Some(&p), TriageState::Confirmed), "");
    }

    /// L1: the repo is not part of the shape. The same rule on the same line in another repo is
    /// remembered (it only pre-fills; the reviewer still presses Enter).
    #[test]
    fn memory_is_shared_across_repos() {
        let mut other_repo = finding("R-1", 1, "same");
        other_repo.repo = "me/web".into();
        let here = finding("R-1", 2, "same");
        assert_ne!(other_repo.repo, here.repo);
        let rows = vec![other_repo.clone(), here.clone()];
        let d = decided(&[&other_repo], TriageState::Ignored, "fixture");
        let p = prior_decisions(&rows, &d, &here).expect("remembered across repos");
        assert_eq!(p.reason, "fixture");
    }

    /// A finding with the same path in two repos (`src/index.ts` in both).
    fn in_repo(repo: &str, rule: &str, line: usize, snippet: &str) -> FindingView {
        let mut f = finding(rule, line, snippet);
        f.repo = repo.into();
        f.path = "src/index.ts".into();
        f
    }

    /// M9: a secret finding's snippet is redacted to a placeholder, so "false positive: fixture
    /// key" in repo A would pre-fill repo B's real leak. For a secret-exposure finding the repo
    /// IS part of the shape. Identified by the server's category.
    #[test]
    fn a_secret_exposure_decision_is_not_offered_in_another_repo() {
        let mut a = in_repo(
            "acme/a",
            "SEC-NO-HARDCODED-SECRETS-1",
            3,
            "key = [REDACTED]",
        );
        let mut b = in_repo(
            "acme/b",
            "SEC-NO-HARDCODED-SECRETS-1",
            3,
            "key = [REDACTED]",
        );
        a.category = Some("secret-exposure".into());
        b.category = Some("secret-exposure".into());
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::FalsePositive, "fixture key");
        assert!(
            prior_decisions(&rows, &d, &b).is_none(),
            "repo A's fixture-key decision must not reach repo B's leak"
        );
        assert!(prior_decisions(&rows, &decided(&[&b], TriageState::Ignored, "x"), &b).is_none());
    }

    /// M9: the secret rule is identified by its rule id too, because a deterministic finding
    /// carries no category.
    #[test]
    fn a_secret_rule_id_without_a_category_is_repo_scoped_too() {
        let a = in_repo(
            "acme/a",
            "SEC-NO-HARDCODED-SECRETS-1",
            3,
            "key = [REDACTED]",
        );
        let b = in_repo(
            "acme/b",
            "SEC-NO-HARDCODED-SECRETS-1",
            3,
            "key = [REDACTED]",
        );
        assert!(a.category.is_none());
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::FalsePositive, "fixture key");
        assert!(prior_decisions(&rows, &d, &b).is_none());
    }

    /// M9: inside ONE repo a secret decision is still remembered (two rows in the same repo).
    #[test]
    fn a_secret_exposure_decision_is_still_offered_within_its_repo() {
        let mut a1 = in_repo(
            "acme/a",
            "SEC-NO-HARDCODED-SECRETS-1",
            3,
            "key = [REDACTED]",
        );
        let mut a2 = in_repo(
            "acme/a",
            "SEC-NO-HARDCODED-SECRETS-1",
            9,
            "key = [REDACTED]",
        );
        a1.category = Some("secret-exposure".into());
        a2.category = Some("secret-exposure".into());
        let rows = vec![a1.clone(), a2.clone()];
        let d = decided(&[&a1], TriageState::FalsePositive, "fixture key");
        let p = prior_decisions(&rows, &d, &a2).expect("same repo remembers");
        assert_eq!(p.reason, "fixture key");
    }

    /// M9 follow-up: every credential rule keeps repos apart, not only the hardcoded-secret
    /// one. A false-positive mark on a vendor token or a PEM header in repo A must never
    /// pre-fill repo B's, with or without the server's category on the row.
    #[test]
    fn every_credential_rule_is_repo_scoped_with_or_without_a_category() {
        let cases = [
            ("SEC-NO-PRIVATE-KEY-1", "-----BEGIN PRIVATE KEY-----"),
            ("SEC-NO-VENDOR-TOKEN-1", "ghp_FAKEFAKEFAKEFAKE"),
            (
                "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
                "serviceRoleKey = [REDACTED]",
            ),
            ("SEC-NO-HARDCODED-SECRETS-1", "key = [REDACTED]"),
        ];
        for (rule, snippet) in cases {
            for category in [None, Some("secret-exposure".to_string())] {
                let mut a = in_repo("acme/a", rule, 3, snippet);
                let mut b = in_repo("acme/b", rule, 3, snippet);
                a.category = category.clone();
                b.category = category.clone();
                let rows = vec![a.clone(), b.clone()];
                let d = decided(&[&a], TriageState::FalsePositive, "fixture key");
                assert!(
                    prior_decisions(&rows, &d, &b).is_none(),
                    "{rule} ({category:?}): repo A's decision reached repo B"
                );
                // The same repo still remembers.
                let mut a2 = in_repo("acme/a", rule, 9, snippet);
                a2.category = category.clone();
                let rows = vec![a.clone(), a2.clone()];
                assert!(prior_decisions(&rows, &d, &a2).is_some(), "{rule}");
            }
        }
    }

    /// M9: a non-secret rule keeps the cross-repo memory even with the same path in both repos.
    #[test]
    fn a_non_secret_decision_still_crosses_repos_on_the_same_path() {
        let a = in_repo("acme/a", "R-1", 3, "same");
        let b = in_repo("acme/b", "R-1", 3, "same");
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::Ignored, "fixture");
        assert!(prior_decisions(&rows, &d, &b).is_some());
    }

    /// L4: the same words with different whitespace or capitalisation are one decision, not a
    /// disagreement; the first wording seen is what is shown and pre-filled.
    #[test]
    fn reasons_group_on_collapsed_case_folded_text() {
        let rows: Vec<FindingView> = (1..=4).map(|n| finding("R-1", n, "same")).collect();
        let mut d = decided(&[&rows[0]], TriageState::Ignored, "Test fixture");
        d.extend(decided(
            &[&rows[1]],
            TriageState::Ignored,
            "test   fixture\n",
        ));
        d.extend(decided(&[&rows[2]], TriageState::Ignored, " TEST FIXTURE "));
        let p = prior_decisions(&rows, &d, &rows[3]).unwrap();
        assert_eq!((p.rows, p.others), (3, 0), "{p:?}");
        assert!(!p.chip_text().contains("decided differently"));
        assert_eq!(
            p.reason_for(TriageState::Ignored)
                .unwrap()
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["test", "fixture"]
        );
        // A different state with the same words is still a different decision.
        let mut d2 = decided(&[&rows[0]], TriageState::Ignored, "test fixture");
        d2.extend(decided(
            &[&rows[1]],
            TriageState::FalsePositive,
            "test fixture",
        ));
        let p = prior_decisions(&rows, &d2, &rows[3]).unwrap();
        assert_eq!((p.rows, p.others), (1, 1));
        // And the stack rule compares on the folded text too.
        let a = finding("R-1", 10, "alpha");
        let b = finding("R-1", 11, "beta");
        let pa = finding("R-1", 12, "alpha");
        let pb = finding("R-1", 13, "beta");
        let all = vec![a.clone(), b.clone(), pa.clone(), pb.clone()];
        let mut d3 = decided(&[&pa], TriageState::Ignored, "Test Fixture");
        d3.extend(decided(&[&pb], TriageState::Ignored, "test  fixture"));
        assert_eq!(
            shared_prefill(&[a, b], &all, &d3, TriageState::Ignored).to_lowercase(),
            "test fixture"
        );
    }

    /// L3: an index answers per finding without rebuilding, and agrees with the one-shot form.
    #[test]
    fn an_index_agrees_with_the_one_shot_form_for_every_row() {
        let rows: Vec<FindingView> = (1..=6)
            .map(|n| finding("R-1", n, if n % 2 == 0 { "even" } else { "odd" }))
            .collect();
        let d = merged(vec![
            decided(&[&rows[0]], TriageState::Ignored, "fixture"),
            decided(&[&rows[1], &rows[3]], TriageState::FalsePositive, "misread"),
            decided(&[&rows[2]], TriageState::Confirmed, ""),
        ]);
        let index = DecisionIndex::build(&rows, &d);
        for f in &rows {
            assert_eq!(index.prior(f), prior_decisions(&rows, &d, f));
        }
    }

    #[test]
    fn a_reasonless_reason_state_shows_the_state_and_offers_no_text() {
        let a = finding("R-1", 1, "same");
        let b = finding("R-1", 2, "same");
        let rows = vec![a.clone(), b.clone()];
        let d = decided(&[&a], TriageState::Ignored, "   ");
        let p = prior_decisions(&rows, &d, &b).unwrap();
        assert_eq!(p.chip_text(), "Previously on this shape: Ignored (1 row)");
        assert_eq!(p.reason_for(TriageState::Ignored), None);
    }

    /// The stacked view's rule: pre-fill only when every checked card is offered the same reason.
    #[test]
    fn a_set_is_prefilled_only_when_every_row_shares_the_prior_reason() {
        // Two shapes under one rule: "alpha" rows and "beta" rows.
        let prior_alpha = finding("R-1", 1, "alpha");
        let prior_beta = finding("R-1", 2, "beta");
        let a1 = finding("R-1", 10, "alpha");
        let a2 = finding("R-1", 11, "alpha");
        let b1 = finding("R-1", 12, "beta");
        let fresh = finding("R-1", 13, "gamma");
        let rows = vec![
            prior_alpha.clone(),
            prior_beta.clone(),
            a1.clone(),
            a2.clone(),
            b1.clone(),
            fresh.clone(),
        ];
        // Same reason on both shapes: every card is offered it.
        let same = merged(vec![decided(
            &[&prior_alpha, &prior_beta],
            TriageState::Ignored,
            "fixture",
        )]);
        assert_eq!(
            shared_prefill(
                &[a1.clone(), a2.clone(), b1.clone()],
                &rows,
                &same,
                TriageState::Ignored
            ),
            "fixture"
        );
        // Different reasons: empty.
        let differ = merged(vec![
            decided(&[&prior_alpha], TriageState::Ignored, "fixture"),
            decided(&[&prior_beta], TriageState::Ignored, "legacy"),
        ]);
        assert_eq!(
            shared_prefill(
                &[a1.clone(), b1.clone()],
                &rows,
                &differ,
                TriageState::Ignored
            ),
            ""
        );
        // The same cards under one shape still pre-fill.
        assert_eq!(
            shared_prefill(
                &[a1.clone(), a2.clone()],
                &rows,
                &differ,
                TriageState::Ignored
            ),
            "fixture"
        );
        // One card with no prior at all: empty.
        assert_eq!(
            shared_prefill(
                &[a1.clone(), fresh.clone()],
                &rows,
                &same,
                TriageState::Ignored
            ),
            ""
        );
        // Nothing checked, or a state that takes no reason: empty.
        assert_eq!(shared_prefill(&[], &rows, &same, TriageState::Ignored), "");
        assert_eq!(
            shared_prefill(&[a1], &rows, &same, TriageState::TechDebt),
            ""
        );
    }
}
