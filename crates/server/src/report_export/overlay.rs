//! The reviewer overlay (review-tree design section 5, amendments A1, A2b, A10 and A12).
//!
//! The sealed report is the engine's answer and never changes. What the reviewer did on top of it
//! (decisions, edits, findings added, sites merged or moved to another sub-group, findings put
//! back after the engine threw them out) is stored beside it and applied here, at read time and
//! at every export:
//!
//! ```text
//! ReviewedReport = apply_overlay(sealed, decisions, edits, added)
//! ```
//!
//! The overlay runs after the seal and before the release gate. Every exporter, the release gate
//! and the review screen's inspection fetch read the [`ReviewedReport`] and nothing else, so they
//! cannot disagree about which rows exist or what they say.
//!
//! # What it does, in order
//!
//! 1. A thrown-out finding the reviewer restored returns to the kept rows
//!    ([`crate::onboard::ScanReport::reinstate_restored`]), marked
//!    [`crate::engine_discard::ENGINE_RESTORED_CAPTURE`] and held until it is decided.
//! 2. Added findings join the rows with `provenance: reviewer` in `captures`.
//! 3. Edits are applied in the order they were written; the later edit on a field wins. An edit
//!    whose site is gone, or whose rule and snippet no longer match the fingerprint it was made
//!    against, is STALE: it is returned beside its old value and not applied (D10).
//! 4. A merge ([`DecisionState::MergedInto`]) marks the merged row with a pointer; the row stays.
//! 5. The review tree is rebuilt when a change could move a row, and a split moves sites to a
//!    sub-group of their own as a post-pass over the built tree.
//!
//! A row with an applied edit carries `reviewer_edited` and the ids of its edits in every export.
//! Those ids are the only trace of an edit in client output: the reason and the reviewer's name
//! stay internal.

use std::collections::{HashMap, HashSet};

use camerata_api_types::decisions::{
    effective_by_key, restored_keys, restored_undecided_keys, Decision, DecisionState,
};
use camerata_api_types::edits::{Edit, EditField};
use camerata_api_types::review::ReviewLevel;

use super::DispositionWire;
use crate::decisions::StoreDecisions;
use crate::edits::AddedFinding;
use crate::engine_discard::RestoredKeys;
use crate::onboard::{Finding, SealedReport};
use crate::release_gate::evidence::ReviewerDecisions;
use crate::review_tree::{NodeLevel, ReviewTree, TextSource, TreeNode};

/// The `captures` key holding the ids of the edits applied to a row, comma separated, in the
/// order they were applied. A bookkeeping key.
pub const REVIEWER_EDITED_CAPTURE: &str = "reviewer_edited";
/// The `captures` key listing which fields those edits changed (the fields' wire names, comma
/// separated). The exporters read it so a reviewer's severity, headline, detail, category or
/// fix type is not overwritten by a corpus template or a bound. A bookkeeping key.
pub const REVIEWER_FIELDS_CAPTURE: &str = "reviewer_edited_fields";
/// The reviewer's headline, when they edited it.
pub const REVIEWER_HEADLINE_CAPTURE: &str = "reviewer_headline";
/// The reviewer's detail, when they edited it.
pub const REVIEWER_DETAIL_CAPTURE: &str = "reviewer_detail";
/// The `captures` key that says who produced a row. A bookkeeping key.
pub const PROVENANCE_CAPTURE: &str = "provenance";
/// The [`PROVENANCE_CAPTURE`] value of a finding the reviewer added.
pub const REVIEWER_PROVENANCE: &str = "reviewer";
/// The `captures` key on a merged row: the site key of the row it was merged into.
pub const MERGED_INTO_CAPTURE: &str = "merged_into";
/// The `captures` key on a merged row: the client-facing pointer ("Merged into src/b.rs:20").
pub const MERGED_LABEL_CAPTURE: &str = "merged_into_label";

/// Every capture the overlay writes. Listed once so the engine's merge stages treat them as
/// bookkeeping (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`) and the tests can check none leaks.
pub const OVERLAY_CAPTURE_KEYS: [&str; 8] = [
    REVIEWER_EDITED_CAPTURE,
    REVIEWER_FIELDS_CAPTURE,
    REVIEWER_HEADLINE_CAPTURE,
    REVIEWER_DETAIL_CAPTURE,
    PROVENANCE_CAPTURE,
    MERGED_INTO_CAPTURE,
    MERGED_LABEL_CAPTURE,
    crate::engine_discard::ENGINE_RESTORED_CAPTURE,
];

/// One edit that was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEdit {
    pub id: String,
    pub key: String,
    pub field: EditField,
}

/// One edit that was not applied because its site is gone or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleEditNote {
    /// The edit, with `stale` set.
    pub edit: Edit,
    /// What the site's field holds now, so the reviewer can compare it with `edit.old`. Blank
    /// when the site is gone.
    pub current_value: String,
}

/// A finding the reviewer added whose site key an engine row now holds (a rerun found the same
/// place). The engine's row is shown; the reviewer's text is kept here, never dropped and never
/// applied silently, so the UI can offer to reapply it as edits or discard it.
#[derive(Debug, Clone, PartialEq)]
pub struct SupersededNote {
    /// The finding as the reviewer added it (headline, detail and fix in `finding`, and `reason`).
    pub added: AddedFinding,
    /// The site key of the engine row that took its key.
    pub engine_key: String,
}

/// One merge: the merged row, the row it points to and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeNote {
    pub key: String,
    pub target: String,
    pub reason: String,
}

/// The sealed report with the reviewer's overlay applied. The report inside is a COPY; the sealed
/// report it came from is unchanged.
#[derive(Debug, Clone)]
pub struct ReviewedReport {
    report: SealedReport,
    dispositions: HashMap<String, DispositionWire>,
    decided: StoreDecisions,
    records: Vec<Decision>,
    history: Vec<Decision>,
    applied: Vec<AppliedEdit>,
    stale: Vec<StaleEditNote>,
    superseded: Vec<SupersededNote>,
    merges: Vec<MergeNote>,
    /// How closely each row was reviewed, by site key (design 3B item 3). Computed in
    /// [`apply_overlay`] and nowhere else; a row with no entry is held for a ruling and cannot
    /// ship to a client.
    levels: HashMap<String, ReviewLevel>,
    /// Dispositions that came from an export request rather than the store (an older client
    /// while the store is empty). They answer the gate's question too.
    legacy: bool,
    /// What the Engine Check store holds about this run, attached by the caller that has the
    /// store ([`Self::with_engine_check`]). Nothing recorded holds nothing.
    engine_check: crate::engine_check::EngineCheckEvidence,
}

impl ReviewedReport {
    /// The rows an export reads: the sealed set with the overlay applied.
    pub fn report(&self) -> &SealedReport {
        &self.report
    }

    /// What the Engine Check store holds about this run: the release gate holds every site the
    /// Engine Check said No to until it is corrected.
    pub fn engine_check(&self) -> &crate::engine_check::EngineCheckEvidence {
        &self.engine_check
    }

    /// This report with the Engine Check's evidence attached. Every caller that holds the store
    /// attaches it before the gate or the read model looks at the report.
    #[must_use]
    pub fn with_engine_check(mut self, evidence: crate::engine_check::EngineCheckEvidence) -> Self {
        self.engine_check = evidence;
        self
    }

    /// The reviewer's decisions as the exporters read them, by site key.
    pub fn dispositions(&self) -> &HashMap<String, DispositionWire> {
        &self.dispositions
    }

    /// The live decision records the overlay was applied with (revoked ones are left out).
    pub fn records(&self) -> &[Decision] {
        &self.records
    }

    /// Every decision record the overlay was given, revoked ones included. The release gate
    /// reads it to tell a site whose record was undone from a site that never had one.
    pub fn all_records(&self) -> &[Decision] {
        &self.history
    }

    /// The edits that were applied, in the order they were applied.
    pub fn applied_edits(&self) -> &[AppliedEdit] {
        &self.applied
    }

    /// The edits that were NOT applied because their site is gone or changed.
    pub fn stale_edits(&self) -> &[StaleEditNote] {
        &self.stale
    }

    /// The reviewer-added findings an engine row took the key of. Not in the rows; kept here.
    pub fn superseded(&self) -> &[SupersededNote] {
        &self.superseded
    }

    /// The merges applied.
    pub fn merges(&self) -> &[MergeNote] {
        &self.merges
    }

    /// How closely `f` was reviewed. `None` for a row held for a ruling it has not had (evidence
    /// not verified, junk text, put back by the reviewer): such a row is not `engine_verified`
    /// and nobody has reviewed it, so the claim is not made, and the release gate refuses it
    /// until it is decided.
    pub fn review_level(&self, f: &Finding) -> Option<&ReviewLevel> {
        self.levels.get(&f.site_key)
    }

    /// The overlay of nothing: the sealed rows with `dispositions` as the decisions. The one
    /// way a caller that holds no stores (the headless `camerata inspect`, the sample generator,
    /// a test) goes through the overlay.
    pub fn unreviewed(
        sealed: &SealedReport,
        dispositions: &HashMap<String, DispositionWire>,
    ) -> Self {
        apply_overlay(sealed, &[], &[], &[]).with_legacy_dispositions(dispositions.clone())
    }

    /// This report with `dispositions` (an older client's export request) as the decisions. Used
    /// only while the decision store holds no record for the project.
    #[must_use]
    pub fn with_legacy_dispositions(
        mut self,
        dispositions: HashMap<String, DispositionWire>,
    ) -> Self {
        self.dispositions = dispositions;
        self.legacy = true;
        // The levels are worked out again here, in this module, with the older client's decisions
        // among the evidence of who looked at what.
        self.levels = compute_levels(
            self.report.as_scan_report(),
            &self.records,
            &self.applied,
            Some(&self.dispositions),
        );
        self
    }
}

impl ReviewerDecisions for ReviewedReport {
    fn has_decision(&self, finding: &Finding) -> bool {
        self.decided.has_decision(finding)
            || (self.legacy && self.dispositions.has_decision(finding))
    }
}

/// The sealed report with the escalation line marked again from the project's CURRENT settings,
/// over the stored candidates: no model call, no candidate added or dropped. The budget, the
/// candidates' marks and the rows' escalation columns are rewritten on the copy
/// ([`crate::review_tree::ReviewTree::re_mark`]); the stored inspection is untouched.
///
/// This is THE place the line moves. The review screens and every export load the report through
/// it (`review_of` and `reviewed_for_export`, before [`apply_overlay`]), so the queue the reviewer
/// works from and the escalations, review levels and PDF a client receives are marked by the same
/// line. A report with no tree is returned as it is.
pub fn re_mark_line(
    sealed: &SealedReport,
    settings: &camerata_api_types::project::ReviewSettings,
) -> SealedReport {
    sealed.overlaid(|report| {
        let code_lines = report.code_lines;
        let Some(tree) = report.tree.as_mut() else {
            return;
        };
        let counted_lines = Some(tree.budget.counted_lines)
            .filter(|n| *n > 0)
            .unwrap_or(code_lines);
        tree.re_mark(
            &crate::review_tree::RankSettings {
                rate_per_kloc: settings.escalation_rate_per_kloc,
                floor: settings.escalation_floor,
                counted_lines,
                over_budget_reasons: settings.over_budget_reasons.clone(),
            },
            &mut report.findings,
        );
    })
}

/// Apply the reviewer's overlay to a sealed report (design 5.1). `decisions` may include revoked
/// records; they are ignored. `edits` may include discarded ones; they are ignored. The sealed
/// report is not changed.
pub fn apply_overlay(
    sealed: &SealedReport,
    decisions: &[Decision],
    edits: &[Edit],
    added: &[AddedFinding],
) -> ReviewedReport {
    let records: Vec<Decision> = decisions
        .iter()
        .filter(|d| d.is_active())
        .cloned()
        .collect();
    let restored = RestoredKeys::from_keys(restored_keys(&records));
    let edits: Vec<&Edit> = edits.iter().filter(|e| e.is_active()).collect();

    let mut applied: Vec<AppliedEdit> = Vec::new();
    let mut stale: Vec<StaleEditNote> = Vec::new();
    let mut merges: Vec<MergeNote> = Vec::new();
    let mut superseded: Vec<SupersededNote> = Vec::new();

    let report = sealed.overlaid(|r| {
        let reinstated = r.reinstate_restored(&restored);
        // A row a rerun brought back (never thrown out this time) is still the reviewer's
        // restore: it carries the mark that holds it until it is decided.
        for f in r.findings.iter_mut().filter(|f| restored.covers(f)) {
            f.captures
                .entry(crate::engine_discard::ENGINE_RESTORED_CAPTURE.to_string())
                .or_insert_with(|| "restored by the reviewer".to_string());
        }
        let (added_rows, taken) = add_findings(&mut r.findings, added);
        superseded = taken;

        let outcome = apply_edits(&mut r.findings, &edits);
        applied = outcome.applied;
        stale = outcome.stale;
        let multi_repo = r.repos.len() > 1;
        merges = mark_merges(&mut r.findings, &records, multi_repo);

        // The tree is rebuilt only when a change could have moved a row: building it again for
        // an overlay of nothing would change a report the reviewer never touched.
        if reinstated > 0 || added_rows > 0 || outcome.moves_rows {
            r.attach_tree();
        }
        if !outcome.splits.is_empty() {
            if let Some(tree) = r.tree.as_mut() {
                apply_splits(tree, &mut r.findings, &outcome.splits);
            }
        }
        // The ledger's escalation block is worked out again from the tree being exported, not
        // copied from what stage 2 recorded: a row thrown out after stage 2 is no longer one of
        // the escalations (wave 5 follow-up).
        if let (Some(block), Some(tree)) = (r.ledger.escalation().cloned(), r.tree.as_ref()) {
            let derived = block.derived_from_tree(tree, &r.findings);
            r.ledger.record_escalation(derived);
        }
    });

    let levels = compute_levels(report.as_scan_report(), &records, &applied, None);
    ReviewedReport {
        report,
        dispositions: crate::decisions::wire_dispositions(&records),
        decided: StoreDecisions::from_records(&records),
        records,
        history: decisions.to_vec(),
        applied,
        stale,
        superseded,
        merges,
        levels,
        legacy: false,
        engine_check: crate::engine_check::EngineCheckEvidence::default(),
    }
}

// ── how closely each row was reviewed ─────────────────────────────────────────────────────

/// The review level of every row (design 3B item 3), the ONE place it is worked out.
///
/// - [`ReviewLevel::PersonallyReviewed`]: a site-scope decision, an applied edit, or a decision of
///   any kind on a row the group reviewer escalated.
/// - [`ReviewLevel::ReviewedAsPattern`]: a decision the row inherited from a tree node; the node
///   is the one the reviewer decided.
/// - [`ReviewLevel::EngineVerified`]: no decision, and nothing holds the row.
///
/// A row held for a ruling it has not had (evidence not verified, junk text, put back by the
/// reviewer) has no level: it is not engine verified, and nobody has reviewed it. `legacy` is an
/// older client's dispositions; each is a site-scope ruling.
fn compute_levels(
    report: &crate::onboard::ScanReport,
    records: &[Decision],
    applied: &[AppliedEdit],
    legacy: Option<&HashMap<String, DispositionWire>>,
) -> HashMap<String, ReviewLevel> {
    use camerata_api_types::decisions::Scope;
    use camerata_api_types::review::EscalationState;

    let effective = effective_by_key(records);
    let node_of: HashMap<&str, &str> = records
        .iter()
        .filter_map(|d| match &d.scope {
            Scope::Node(node) => Some((d.id.as_str(), node.as_str())),
            Scope::Site => None,
        })
        .collect();
    let edited: HashSet<&str> = applied.iter().map(|e| e.key.as_str()).collect();
    let mut out = HashMap::with_capacity(report.findings.len());
    for f in &report.findings {
        let key = f.site_key.as_str();
        let record = effective.get(key).copied();
        let legacy_decided = legacy.is_some_and(|d| d.has_decision(f));
        let level = if let Some(parent) = record.and_then(|d| d.inherited_from.as_deref()) {
            let escalated = f.review.escalation_state == EscalationState::Escalated;
            if escalated || edited.contains(key) {
                Some(ReviewLevel::PersonallyReviewed)
            } else {
                match node_of.get(parent) {
                    Some(node) => Some(ReviewLevel::ReviewedAsPattern((*node).to_string())),
                    // An inherited record whose node record is missing is not a pattern the
                    // reviewer decided: the release gate reports it. The site still has a ruling.
                    None => Some(ReviewLevel::PersonallyReviewed),
                }
            }
        } else if record.is_some() || legacy_decided || edited.contains(key) {
            Some(ReviewLevel::PersonallyReviewed)
        } else if is_held_for_a_ruling(f) {
            None
        } else {
            Some(ReviewLevel::EngineVerified)
        };
        if let Some(level) = level {
            out.insert(f.site_key.clone(), level);
        }
    }
    out
}

/// Whether a mark on the row holds it until the reviewer rules on it.
fn is_held_for_a_ruling(f: &Finding) -> bool {
    [
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE,
        crate::junk_finding::JUNK_CONTENT_CAPTURE,
        crate::engine_discard::ENGINE_RESTORED_CAPTURE,
    ]
    .iter()
    .any(|capture| f.captures.contains_key(*capture))
}

// ── added findings ────────────────────────────────────────────────────────────────────────

/// Add the reviewer's findings to `rows`. A finding whose key a row already has (the engine
/// found it too, since, or it was added twice) is not added again; the engine's case is returned
/// as a [`SupersededNote`] so nothing the reviewer wrote is lost. A finding with no key yet gets
/// the normal one. Returns how many were added and the superseded ones.
fn add_findings(rows: &mut Vec<Finding>, added: &[AddedFinding]) -> (usize, Vec<SupersededNote>) {
    let engine: HashSet<String> = rows.iter().map(|f| f.site_key.clone()).collect();
    let mut have = engine.clone();
    let mut count = 0;
    let mut superseded = Vec::new();
    for a in added {
        let mut f = a.finding.clone();
        f.captures.insert(
            PROVENANCE_CAPTURE.to_string(),
            REVIEWER_PROVENANCE.to_string(),
        );
        if f.site_key.is_empty() {
            f.site_key = free_site_key(&f, &have);
        } else if have.contains(&f.site_key) {
            // An engine row took the key (a rerun found the same place): the engine's row is
            // shown and the reviewer's finding is reported beside it, never dropped. A finding
            // added twice is only a duplicate.
            if engine.contains(&f.site_key) {
                superseded.push(SupersededNote {
                    engine_key: f.site_key.clone(),
                    added: a.clone(),
                });
            }
            continue;
        }
        f.group_path.clear();
        have.insert(f.site_key.clone());
        rows.push(f);
        count += 1;
    }
    (count, superseded)
}

/// The first site key for `f` (lowest ordinal) that is not in `taken`.
fn free_site_key(f: &Finding, taken: &HashSet<String>) -> String {
    (0..)
        .map(|ordinal| {
            camerata_api_types::site_key::site_key(
                &f.repo, &f.path, &f.rule_id, &f.snippet, ordinal,
            )
        })
        .find(|k| !taken.contains(k))
        .unwrap_or_default()
}

/// The site key a finding added to a report holding `rows` gets: the normal key, with the lowest
/// ordinal no row has. Used when the finding is added, so its key is stable from then on.
pub fn key_for_added(rows: &[Finding], f: &Finding) -> String {
    let taken: HashSet<String> = rows.iter().map(|r| r.site_key.clone()).collect();
    free_site_key(f, &taken)
}

// ── edits ─────────────────────────────────────────────────────────────────────────────────

/// One site moved to a sub-group by a Subgroup edit.
#[derive(Debug, Clone)]
struct SplitMove {
    key: String,
    label: String,
}

struct EditOutcome {
    applied: Vec<AppliedEdit>,
    stale: Vec<StaleEditNote>,
    splits: Vec<SplitMove>,
    /// An applied edit changed what decides a row's place in the tree.
    moves_rows: bool,
}

fn apply_edits(rows: &mut [Finding], edits: &[&Edit]) -> EditOutcome {
    let index: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, f)| (f.site_key.clone(), i))
        .collect();
    // The fingerprint an edit was made against is the SEALED row's, so it is taken before any
    // edit changes the rule.
    let sealed_fp: Vec<String> = rows
        .iter()
        .map(|f| camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet))
        .collect();

    let mut out = EditOutcome {
        applied: Vec::new(),
        stale: Vec::new(),
        splits: Vec::new(),
        moves_rows: false,
    };
    let mut ids: HashMap<usize, Vec<String>> = HashMap::new();
    let mut fields: HashMap<usize, Vec<&'static str>> = HashMap::new();

    for e in edits {
        let hit = index
            .get(&e.key)
            .copied()
            .filter(|&i| sealed_fp[i] == e.fingerprint_at_edit);
        let Some(i) = hit else {
            let mut edit = (*e).clone();
            edit.stale = true;
            out.stale.push(StaleEditNote {
                current_value: index
                    .get(&e.key)
                    .map(|&i| current_value(&rows[i], e.field))
                    .unwrap_or_default(),
                edit,
            });
            continue;
        };
        let f = &mut rows[i];
        match e.field {
            EditField::Subgroup => out.splits.push(SplitMove {
                key: e.key.clone(),
                label: e.new.trim().to_string(),
            }),
            field => {
                apply_field(f, field, &e.new);
                if matches!(field, EditField::RuleId | EditField::Category) {
                    out.moves_rows = true;
                    f.group_path.clear();
                    f.group_role = crate::review_tree::GroupRole::Primary;
                }
                if matches!(field, EditField::Severity | EditField::Kind) {
                    out.moves_rows = true;
                }
            }
        }
        ids.entry(i).or_default().push(e.id.clone());
        let list = fields.entry(i).or_default();
        if !list.contains(&e.field.wire_name()) {
            list.push(e.field.wire_name());
        }
        out.applied.push(AppliedEdit {
            id: e.id.clone(),
            key: e.key.clone(),
            field: e.field,
        });
    }
    for (i, edit_ids) in ids {
        let f = &mut rows[i];
        f.captures
            .insert(REVIEWER_EDITED_CAPTURE.to_string(), edit_ids.join(","));
        f.captures.insert(
            REVIEWER_FIELDS_CAPTURE.to_string(),
            fields.remove(&i).unwrap_or_default().join(","),
        );
    }
    out
}

/// What `field` holds on `f` now, in the text an edit's `old` and `new` use.
pub fn current_value(f: &Finding, field: EditField) -> String {
    match field {
        EditField::Headline => crate::ai_audit::finding_headline(f),
        EditField::Detail => split_detail(&f.detail).1.to_string(),
        EditField::Fix => f.fix_specific.clone().unwrap_or_default(),
        EditField::FixType => {
            if f.captures
                .get(super::FIX_SOURCE_CAPTURE)
                .is_some_and(|v| v == super::STANDARD_FIX)
                || f.fix_specific.is_none()
            {
                super::STANDARD_FIX_LABEL.to_string()
            } else {
                super::TAILORED_FIX_LABEL.to_string()
            }
        }
        EditField::Severity => f.severity.clone(),
        EditField::RuleId => f.rule_id.clone(),
        EditField::Kind => serde_json::to_string(&f.kind).unwrap_or_default(),
        EditField::Category => f.category.clone().unwrap_or_default(),
        EditField::Subgroup => f.group_path.get(2).cloned().unwrap_or_default(),
    }
}

/// The title and the explanation of a detail (the text before and after the headline separator;
/// a detail with no separator is all explanation).
fn split_detail(detail: &str) -> (&str, &str) {
    match detail.split_once(crate::ai_audit::HEADLINE_SEP) {
        Some((title, body)) => (title, body),
        None => ("", detail),
    }
}

fn apply_field(f: &mut Finding, field: EditField, new: &str) {
    let new = new.trim();
    match field {
        EditField::Headline => {
            let title = new.replace(crate::ai_audit::HEADLINE_SEP, " - ");
            let body = split_detail(&f.detail).1.to_string();
            f.detail = format!("{title}{}{body}", crate::ai_audit::HEADLINE_SEP);
            f.captures
                .insert(REVIEWER_HEADLINE_CAPTURE.to_string(), title);
        }
        EditField::Detail => {
            let title = split_detail(&f.detail).0.to_string();
            f.detail = if title.is_empty() {
                new.to_string()
            } else {
                format!("{title}{}{new}", crate::ai_audit::HEADLINE_SEP)
            };
            f.captures
                .insert(REVIEWER_DETAIL_CAPTURE.to_string(), new.to_string());
        }
        EditField::Fix => {
            f.fix_specific = Some(new.to_string());
            // Written by the reviewer for this code: not the rule's standard guidance.
            f.captures.remove(super::FIX_SOURCE_CAPTURE);
        }
        EditField::FixType => {
            if new == super::STANDARD_FIX_LABEL {
                f.captures.insert(
                    super::FIX_SOURCE_CAPTURE.to_string(),
                    super::STANDARD_FIX.to_string(),
                );
            } else {
                f.captures.remove(super::FIX_SOURCE_CAPTURE);
            }
        }
        EditField::Severity => f.severity = new.to_ascii_lowercase(),
        EditField::RuleId => f.rule_id = new.to_string(),
        EditField::Kind => {
            if let Some(kind) = crate::edits::parse_kind(new) {
                f.kind = Some(kind);
            }
        }
        EditField::Category => {
            f.category = (!new.is_empty()).then(|| new.to_string());
        }
        // Applied to the tree, not the row.
        EditField::Subgroup => {}
    }
}

// ── the exporters' reading of an edit ─────────────────────────────────────────────────────

/// Whether the reviewer's edit of `field` is on this row. The exporters check it before they
/// let a corpus template, a bound or a rule's own category overwrite the reviewer's value.
pub(crate) fn reviewer_edited_field(f: &Finding, field: EditField) -> bool {
    f.captures
        .get(REVIEWER_FIELDS_CAPTURE)
        .is_some_and(|list| list.split(',').any(|n| n == field.wire_name()))
}

/// The reviewer's headline or detail for this row, when they edited it.
pub(crate) fn reviewer_text(f: &Finding, field: EditField) -> Option<&str> {
    let key = match field {
        EditField::Headline => REVIEWER_HEADLINE_CAPTURE,
        EditField::Detail => REVIEWER_DETAIL_CAPTURE,
        _ => return None,
    };
    f.captures.get(key).map(String::as_str)
}

/// Whether any edit of the reviewer's is applied to this row.
pub fn is_reviewer_edited(f: &Finding) -> bool {
    f.captures.contains_key(REVIEWER_EDITED_CAPTURE)
}

/// The ids of the edits applied to this row, in the order they were applied.
pub fn edit_ids_of(f: &Finding) -> Vec<String> {
    f.captures
        .get(REVIEWER_EDITED_CAPTURE)
        .map(|ids| ids.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

/// The site key of the row this one was merged into, when the reviewer merged it.
pub(crate) fn merged_target(f: &Finding) -> Option<&str> {
    f.captures.get(MERGED_INTO_CAPTURE).map(String::as_str)
}

/// The pointer a merged row exports with ("Merged into src/b.rs:20"), when the row was merged.
pub(crate) fn merged_label(f: &Finding) -> Option<&str> {
    f.captures.get(MERGED_LABEL_CAPTURE).map(String::as_str)
}

// ── merges ────────────────────────────────────────────────────────────────────────────────

/// Mark every row the reviewer merged into another with a pointer to it. No row is removed.
fn mark_merges(rows: &mut [Finding], records: &[Decision], multi_repo: bool) -> Vec<MergeNote> {
    let effective = effective_by_key(records);
    let places: HashMap<String, String> = rows
        .iter()
        .map(|f| {
            let place = if f.line > 0 {
                format!("{}:{}", f.path, f.line)
            } else {
                f.path.clone()
            };
            // A project of several repos names the repo: `src/a.rs:10` is a place in each.
            let place = if multi_repo && !f.repo.is_empty() {
                format!("{place} in {}", f.repo)
            } else {
                place
            };
            (f.site_key.clone(), place)
        })
        .collect();
    let mut notes = Vec::new();
    for f in rows.iter_mut() {
        let Some(d) = effective.get(f.site_key.as_str()) else {
            continue;
        };
        let DecisionState::MergedInto(target) = &d.state else {
            continue;
        };
        let label = match places.get(target) {
            Some(place) => format!("Merged into {place}"),
            None => "Merged into another finding".to_string(),
        };
        f.captures
            .insert(MERGED_INTO_CAPTURE.to_string(), target.clone());
        f.captures.insert(MERGED_LABEL_CAPTURE.to_string(), label);
        notes.push(MergeNote {
            key: f.site_key.clone(),
            target: target.clone(),
            reason: d.reason.clone(),
        });
    }
    notes
}

/// The keys of the rows held for a ruling of their own that have no decision yet because the
/// reviewer restored them after the engine threw them out.
pub fn restored_undecided(records: &[Decision]) -> Vec<String> {
    let mut keys: Vec<String> = restored_undecided_keys(records)
        .into_iter()
        .map(str::to_string)
        .collect();
    keys.sort();
    keys
}

// ── splits ────────────────────────────────────────────────────────────────────────────────

/// Move the split sites to sub-groups of their own, as a post-pass over the built tree. One new
/// sub-group per (rule node, name): its members are the moved sites, it keeps the kind of the
/// sub-group they left, and its cause is its label (the reviewer's reason stays on the edit
/// record and reaches no output). A sub-group left with no site is
/// removed. No site is lost or added.
fn apply_splits(tree: &mut ReviewTree, rows: &mut [Finding], moves: &[SplitMove]) {
    // The later split of a site wins.
    let mut last: HashMap<&str, &SplitMove> = HashMap::new();
    for m in moves {
        last.insert(m.key.as_str(), m);
    }
    let mut keys: Vec<&str> = last.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        let m = last[key];
        let Some(from_at) = tree
            .nodes
            .iter()
            .position(|n| n.level == NodeLevel::Subgroup && n.members.iter().any(|k| k == key))
        else {
            continue;
        };
        let from = tree.nodes[from_at].clone();
        let Some(parent) = from.parent.clone() else {
            continue;
        };
        let rule_part = parent.strip_prefix("rule:").unwrap_or(&parent).to_string();
        let id = format!("sub:{rule_part}/split:{}", slug(&m.label));
        if id == from.id {
            continue;
        }
        if let Some(n) = tree.nodes.iter_mut().find(|n| n.id == from.id) {
            n.members.retain(|k| k != key);
            n.outliers.retain(|o| o.key != key);
        }
        let moved_outliers: Vec<_> = from
            .outliers
            .iter()
            .filter(|o| o.key == key)
            .cloned()
            .collect();
        match tree.nodes.iter_mut().find(|n| n.id == id) {
            Some(existing) => {
                existing.members.push(key.to_string());
                existing.members.sort();
                existing.outliers.extend(moved_outliers);
            }
            None => {
                let at = tree
                    .nodes
                    .iter()
                    .rposition(|n| n.parent.as_deref() == Some(parent.as_str()))
                    .map_or(from_at + 1, |p| p + 1);
                tree.nodes.insert(
                    at,
                    TreeNode {
                        id: id.clone(),
                        level: NodeLevel::Subgroup,
                        parent: Some(parent.clone()),
                        label: m.label.clone(),
                        rule_ids: from.rule_ids.clone(),
                        kind: from.kind,
                        // The label, never the reviewer's reason: a sub-group's cause is read by whoever reads
                        // the tree (and exported), and the reason stays on the edit record.
                        cause: Some(m.label.clone()),
                        members: vec![key.to_string()],
                        outliers: moved_outliers,
                        summary: None,
                        summary_source: TextSource::Reviewer,
                        escalations: Vec::new(),
                        flags: Vec::new(),
                    },
                );
            }
        }
        if let Some(row) = rows.iter_mut().find(|f| f.site_key == key) {
            if row.group_path.len() >= 3 {
                row.group_path[2].clone_from(&id);
            }
        }
        // A sub-group the split emptied goes; the rule above it stays.
        tree.nodes
            .retain(|n| !(n.id == from.id && n.members.is_empty()));
    }
}

/// A node-id-safe form of a sub-group name.
fn slug(label: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in label.chars() {
        if c.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.extend(c.to_lowercase());
        } else {
            dash = true;
        }
    }
    if out.is_empty() {
        "group".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests;
