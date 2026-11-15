//! The operator's rewrite advisory: an internal note that says whether a run's numbers are worth
//! a fix-or-rewrite talk, with the numbers behind it.
//!
//! Zach's ruling: whether a client fixes or rewrites is the architect's decision alone. No client
//! file ever states or hints at it. This module gives the operator the numbers and nothing more.
//!
//! OPERATOR ONLY. [`write`] puts `<stem>.rewrite_advisory.json` and `.md` next to the run's
//! ledger in the state directory, beside the failure report, and appends the run's raw numbers to
//! a history file there so the starting thresholds can be measured later. Nothing here is ever
//! added to an export: the type is outside `AuditReportJson` and `ProductPackage`, the export
//! request carries no advisory field, and the release gate refuses any package that carries one
//! of the advisory's names (`camerata_api_types::rewrite_advisory`).
//!
//! # The signals
//!
//! All four come from data a finished run already holds; no model is called.
//!
//! * **A. Fix hours against code size**: the low and high ends of every Fix's effort band
//!   ([`crate::report_export::effort_hours_bounds`]), summed and divided by the counted lines, as
//!   hours per 1,000 lines.
//! * **B. Structural share**: the share of the Fix hours on Fixes with a row on a rule whose
//!   enforcement tier is architectural, or in the review tree's architecture category.
//! * **C. Areas without a settled habit**: per review-tree category, the rules assessed for a
//!   convention and those that departed from it or had none.
//! * **D. One cause repeated widely and expensively**: the places of the largest high-effort Fix,
//!   and of the three largest medium or high Fixes, as a share of all places.
//!
//! Two or more over their thresholds fire the advisory; one alone only lists its number. Every
//! comparison is in whole numbers, so a value exactly at a threshold is never over it. A Fix's
//! effort is worked out the way the Fix plan does it: the most common effort of each of its
//! sub-groups' rows, then the largest of those.

mod text;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use camerata_api_types::audit::FindingKind;
use serde::{Deserialize, Serialize};

use crate::onboard::{Finding, ScanReport};
use crate::review_tree::{FixIndex, FixPlace};

pub use text::render_md;

// ── The thresholds: STARTING GUESSES, not measured ──────────────────────────────────────────
//
// Every number below is a first guess, written down so it can be tested and later measured
// against the history file `write` keeps. None of them is calibrated.

/// A: over this many hours of Fix effort per 1,000 counted lines, at the low end of the bands.
pub const FIX_HOURS_PER_KLOC_OVER: u64 = 15;
/// B: over this share (percent) of the Fix hours on structural rules.
pub const STRUCTURAL_SHARE_OVER_PERCENT: u64 = 40;
/// C: an area is unsettled when over this share (percent) of its assessed rules departed or had
/// no convention.
pub const UNSETTLED_RULES_OVER_PERCENT: u64 = 30;
/// C: over this share (percent) of the assessed areas unsettled.
pub const UNSETTLED_AREAS_OVER_PERCENT: u64 = 50;
/// D: one high-effort Fix covering over this share (percent) of all places.
pub const ONE_CAUSE_OVER_PERCENT: u64 = 25;
/// D: the [`TOP_CAUSES`] largest medium or high Fixes covering over this share (percent).
pub const TOP_CAUSES_OVER_PERCENT: u64 = 50;
/// D: how many of the largest medium or high Fixes are taken together.
pub const TOP_CAUSES: usize = 3;
/// How many signals over their thresholds fire the advisory.
pub const SIGNALS_TO_FIRE: usize = 2;
/// Guard: fewer counted lines than this is not enough to say.
pub const MIN_CODE_LINES: usize = 2_000;
/// Guard: over this share (percent) of the Fixes without an effort estimate is not enough to say.
pub const MAX_UNESTIMATED_PERCENT: u64 = 20;
/// The review tree's architecture category (`cat:arch-conformance`, shown as "Architecture" by
/// [`crate::report_export::category_display_label`]).
pub const ARCHITECTURE_CATEGORY_KEY: &str = "arch-conformance";

// ── The shapes ──────────────────────────────────────────────────────────────────────────────

/// The document as it is saved and served: the advisory under one distinctive key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvisoryDocument {
    pub rewrite_advisory: RewriteAdvisory,
}

/// What came of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Two or more signals over their thresholds.
    Fired,
    /// Fewer than two.
    NotFired,
    /// A guard held: the numbers are kept, the text says only why there is not enough to say.
    NotEnoughToSay,
}

/// Why there is not enough to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardReason {
    /// Fewer than [`MIN_CODE_LINES`] counted lines.
    FewLines,
    /// The run has no review tree, so it has no Fixes.
    NoReviewTree,
    /// The run did not run to completion (the detail names the cause, in a fixed sentence).
    IncompleteRun,
    /// Over [`MAX_UNESTIMATED_PERCENT`] of the Fixes have no effort estimate.
    ManyUnestimated,
}

/// One guard that held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub reason: GuardReason,
    /// The fixed sentence the text shows, numbers filled in.
    pub detail: String,
}

/// The Fixes the signals count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixCount {
    /// Fixes with at least one place that is a problem.
    pub counted: usize,
    /// Of those, the Fixes with no effort estimate. Left out of every hour and of signal D.
    pub without_estimate: usize,
}

/// Signal A.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixHours {
    /// The low ends of the bands, summed.
    pub low_hours: u64,
    /// The high ends, summed.
    pub high_hours: u64,
    /// `low_hours` per 1,000 counted lines, to one decimal.
    pub low: f64,
    /// `high_hours` per 1,000 counted lines, to one decimal.
    pub high: f64,
    pub threshold: u64,
    pub over: bool,
}

/// Signal B.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Structural {
    pub low_hours: u64,
    pub high_hours: u64,
    pub structural_low_hours: u64,
    pub structural_high_hours: u64,
    /// The structural share of the low-end hours, to one decimal. What trips the signal.
    pub percent: f64,
    pub threshold: u64,
    pub over: bool,
}

/// One review-tree category assessed for a convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Area {
    /// The category as the review tree shows it.
    pub category: String,
    /// Rules with a convention row, a departure or a no-convention row in this category.
    pub assessed_rules: usize,
    /// Of those, rules that departed or had no convention.
    pub unsettled_rules: usize,
    /// Over [`UNSETTLED_RULES_OVER_PERCENT`] of the assessed rules unsettled.
    pub unsettled: bool,
}

/// Signal C.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsettled {
    /// Categories with at least one assessed rule.
    pub assessed: usize,
    /// Of those, the unsettled ones.
    pub unsettled: usize,
    /// Every assessed category, sorted by name.
    pub areas: Vec<Area>,
    /// Percent of the assessed areas.
    pub threshold: u64,
    /// Percent of an area's assessed rules.
    pub threshold_rules: u64,
    pub over: bool,
}

/// One Fix's share of the places.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CauseShare {
    /// The Fix id.
    pub fix: String,
    pub places: usize,
    pub percent: f64,
}

/// The largest medium or high Fixes taken together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopCauses {
    /// Their Fix ids, largest first.
    pub fixes: Vec<String>,
    /// The places they cover between them, each once.
    pub places: usize,
    pub percent: f64,
}

/// Signal D.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatCause {
    /// The places of every counted Fix with an estimate, each once.
    pub total_places: usize,
    /// The high-effort Fix with the most places, when there is one.
    pub largest_high: Option<CauseShare>,
    pub top_three: TopCauses,
    /// Percent, for one high-effort Fix.
    pub threshold_one: u64,
    /// Percent, for the top three medium or high Fixes.
    pub threshold_three: u64,
    pub over: bool,
}

/// The four signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    pub fix_hours_per_kloc: FixHours,
    pub structural_share: Structural,
    pub unsettled_areas: Unsettled,
    pub repeat_cause_share: RepeatCause,
}

impl Signals {
    /// How many are over their thresholds.
    pub fn over(&self) -> usize {
        [
            self.fix_hours_per_kloc.over,
            self.structural_share.over,
            self.unsettled_areas.over,
            self.repeat_cause_share.over,
        ]
        .iter()
        .filter(|o| **o)
        .count()
    }
}

/// The context line: never a signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// Fixes whose worst place is critical or high.
    pub critical_high_fixes: usize,
    /// Those per 1,000 counted lines, to one decimal.
    pub per_kloc: f64,
}

/// The guards' limits, saved with each run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub min_code_lines: usize,
    pub max_unestimated_percent: u64,
    pub signals_to_fire: usize,
}

/// The advisory of one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewriteAdvisory {
    /// The saved run, or empty when the run was not saved.
    pub run_id: String,
    /// When the run finished (from the report, so the same run gives the same bytes).
    pub finished_at: String,
    pub outcome: Outcome,
    /// How many signals are over their thresholds (kept when a guard holds).
    pub signals_over: usize,
    pub code_lines: usize,
    /// Files a failed part of the run did not review: the numbers are understated by them.
    pub files_not_reviewed: usize,
    /// The guards that held; empty unless the outcome is [`Outcome::NotEnoughToSay`].
    pub guards: Vec<Guard>,
    pub fixes: FixCount,
    pub rewrite_signals: Signals,
    pub context: Context,
    pub limits: Limits,
    /// The text the operator reads, one fixed sentence per line, numbers filled in.
    pub lines: Vec<String>,
}

impl RewriteAdvisory {
    /// The saved form, pretty-printed. The same run gives the same bytes.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&AdvisoryDocument {
            rewrite_advisory: self.clone(),
        })
        .unwrap_or_else(|_| "{}".to_string())
    }
}

// ── The computation ─────────────────────────────────────────────────────────────────────────

/// Whether a row says something is wrong at its place (a defect, a departure, or a row with no
/// kind), as the Fix merge counts places.
fn is_problem(f: &Finding) -> bool {
    matches!(
        f.kind,
        None | Some(FindingKind::Defect) | Some(FindingKind::Deviation(_))
    )
}

/// The review-tree category key of a row (`arch-conformance`), from its tree path, else from its
/// rule id the way the tree places it.
fn category_key(f: &Finding) -> String {
    if let Some(cat) = f.group_path.first() {
        return cat.strip_prefix("cat:").unwrap_or(cat).to_string();
    }
    crate::ai_audit::categorize_rule_id(&f.rule_id)
        .or_else(|| f.category.clone())
        .unwrap_or_else(|| crate::review_tree::UNCATEGORISED.to_string())
}

fn is_structural(f: &Finding, corpus: Option<&camerata_rules::RuleSet>) -> bool {
    category_key(f) == ARCHITECTURE_CATEGORY_KEY
        || corpus
            .and_then(|c| c.get_by_id(&f.rule_id))
            .is_some_and(|r| r.enforcement == camerata_rules::EnforcementKind::Architectural)
}

fn severity_rank(severity: &str) -> u8 {
    match severity.trim().to_ascii_lowercase().as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

fn effort_rank(effort: &str) -> u8 {
    match effort {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// `part / whole` as a percent to one decimal (0 when `whole` is 0).
fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    one_decimal(part as f64 * 100.0 / whole as f64)
}

fn one_decimal(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// `part` over `limit` percent of `whole`, in whole numbers (exactly at the limit is not over).
fn over_percent(part: u64, whole: u64, limit: u64) -> bool {
    whole > 0 && part * 100 > limit * whole
}

/// What one Fix contributes.
struct FixTally {
    id: String,
    places: BTreeSet<FixPlace>,
    /// The Fix's effort (`low`, `medium`, `high`) or empty for no estimate.
    effort: String,
    worst: u8,
    structural: bool,
}

/// The Fixes of `report` that have a problem place, with their effort, worst severity and
/// whether they are structural. Sorted by Fix id.
fn tally_fixes(report: &ScanReport, corpus: Option<&camerata_rules::RuleSet>) -> Vec<FixTally> {
    let index = FixIndex::new(report.tree.as_ref(), &report.findings);
    // Per Fix: the efforts of each sub-group's rows, the worst severity, structural or not.
    struct Rows<'a> {
        by_sub: BTreeMap<&'a str, Vec<String>>,
        worst: u8,
        structural: bool,
    }
    let mut rows: BTreeMap<String, Rows<'_>> = BTreeMap::new();
    for f in report.findings.iter().filter(|f| is_problem(f)) {
        let fix = index.fix_id_of(f);
        if fix.is_empty() {
            continue;
        }
        let effort =
            crate::report_export::resolve_effort(f, &f.rule_id, corpus, None).unwrap_or_default();
        let entry = rows.entry(fix).or_insert_with(|| Rows {
            by_sub: BTreeMap::new(),
            worst: u8::MAX,
            structural: false,
        });
        let sub = f.group_path.get(2).map_or("", String::as_str);
        entry.by_sub.entry(sub).or_default().push(effort);
        entry.worst = entry.worst.min(severity_rank(&f.severity));
        entry.structural |= is_structural(f, corpus);
    }
    index
        .fixes()
        .iter()
        .filter(|fx| !fx.places.is_empty())
        .filter_map(|fx| {
            let r = rows.get(&fx.id)?;
            let effort = r
                .by_sub
                .values()
                .map(|efforts| {
                    crate::xlsx_export::effort_majority(efforts.iter().map(String::as_str))
                })
                .max_by_key(|e| effort_rank(e))
                .unwrap_or_default();
            Some(FixTally {
                id: fx.id.clone(),
                places: fx.places.iter().cloned().collect(),
                effort,
                worst: r.worst,
                structural: r.structural,
            })
        })
        .collect()
}

fn fix_hours(fixes: &[&FixTally], code_lines: usize) -> FixHours {
    let (low_hours, high_hours) = hours_of(fixes.iter().copied());
    let per_kloc = |h: u64| {
        if code_lines == 0 {
            0.0
        } else {
            one_decimal(h as f64 * 1000.0 / code_lines as f64)
        }
    };
    FixHours {
        low_hours,
        high_hours,
        low: per_kloc(low_hours),
        high: per_kloc(high_hours),
        threshold: FIX_HOURS_PER_KLOC_OVER,
        // low / (lines / 1000) > T, in whole numbers.
        over: code_lines > 0 && low_hours * 1000 > FIX_HOURS_PER_KLOC_OVER * code_lines as u64,
    }
}

/// The summed low and high ends of `fixes`' bands. A Fix with no estimate adds nothing.
fn hours_of<'a>(fixes: impl Iterator<Item = &'a FixTally>) -> (u64, u64) {
    fixes.fold(
        (0, 0),
        |(lo, hi), fx| match crate::report_export::effort_hours_bounds(Some(&fx.effort)).0 {
            Some((l, h)) => (lo + u64::from(l), hi + u64::from(h)),
            None => (lo, hi),
        },
    )
}

fn structural_share(fixes: &[&FixTally]) -> Structural {
    let (low_hours, high_hours) = hours_of(fixes.iter().copied());
    let (structural_low_hours, structural_high_hours) =
        hours_of(fixes.iter().copied().filter(|fx| fx.structural));
    Structural {
        low_hours,
        high_hours,
        structural_low_hours,
        structural_high_hours,
        percent: percent(structural_low_hours, low_hours),
        threshold: STRUCTURAL_SHARE_OVER_PERCENT,
        over: over_percent(
            structural_low_hours,
            low_hours,
            STRUCTURAL_SHARE_OVER_PERCENT,
        ),
    }
}

fn unsettled_areas(report: &ScanReport) -> Unsettled {
    // Category key, then rule id, then whether the rule departed or had no convention.
    let mut by_area: BTreeMap<String, BTreeMap<&str, bool>> = BTreeMap::new();
    for f in &report.findings {
        let unsettled = match &f.kind {
            Some(FindingKind::Deviation(_)) | Some(FindingKind::NoConvention(_)) => true,
            Some(FindingKind::ConventionObserved(_)) => false,
            _ => continue,
        };
        let rule = by_area
            .entry(category_key(f))
            .or_default()
            .entry(f.rule_id.as_str())
            .or_insert(false);
        *rule |= unsettled;
    }
    let areas: Vec<Area> = by_area
        .into_iter()
        .map(|(key, rules)| {
            let assessed_rules = rules.len();
            let unsettled_rules = rules.values().filter(|u| **u).count();
            Area {
                category: crate::report_export::category_display_label(&key),
                assessed_rules,
                unsettled_rules,
                unsettled: over_percent(
                    unsettled_rules as u64,
                    assessed_rules as u64,
                    UNSETTLED_RULES_OVER_PERCENT,
                ),
            }
        })
        .collect();
    let assessed = areas.len();
    let unsettled = areas.iter().filter(|a| a.unsettled).count();
    Unsettled {
        assessed,
        unsettled,
        areas,
        threshold: UNSETTLED_AREAS_OVER_PERCENT,
        threshold_rules: UNSETTLED_RULES_OVER_PERCENT,
        over: over_percent(
            unsettled as u64,
            assessed as u64,
            UNSETTLED_AREAS_OVER_PERCENT,
        ),
    }
}

fn repeat_cause(fixes: &[&FixTally]) -> RepeatCause {
    let all: BTreeSet<&FixPlace> = fixes.iter().flat_map(|fx| fx.places.iter()).collect();
    let total = all.len();
    // Largest first; on a tie the smaller id, so the same run always names the same Fix.
    let mut sized: Vec<&&FixTally> = fixes.iter().collect();
    sized.sort_by(|a, b| b.places.len().cmp(&a.places.len()).then(a.id.cmp(&b.id)));
    let largest_high = sized
        .iter()
        .find(|fx| fx.effort == "high")
        .map(|fx| CauseShare {
            fix: fx.id.clone(),
            places: fx.places.len(),
            percent: percent(fx.places.len() as u64, total as u64),
        });
    let top: Vec<&&FixTally> = sized
        .iter()
        .filter(|fx| fx.effort == "high" || fx.effort == "medium")
        .take(TOP_CAUSES)
        .copied()
        .collect();
    let top_places: BTreeSet<&FixPlace> = top.iter().flat_map(|fx| fx.places.iter()).collect();
    let over_one = largest_high
        .as_ref()
        .is_some_and(|c| over_percent(c.places as u64, total as u64, ONE_CAUSE_OVER_PERCENT));
    let over_three = over_percent(
        top_places.len() as u64,
        total as u64,
        TOP_CAUSES_OVER_PERCENT,
    );
    RepeatCause {
        total_places: total,
        largest_high,
        top_three: TopCauses {
            fixes: top.iter().map(|fx| fx.id.clone()).collect(),
            places: top_places.len(),
            percent: percent(top_places.len() as u64, total as u64),
        },
        threshold_one: ONE_CAUSE_OVER_PERCENT,
        threshold_three: TOP_CAUSES_OVER_PERCENT,
        over: over_one || over_three,
    }
}

/// Files a failed part did not review. A disclosed scope limit is not a gap, and a part that did
/// not run because the run stopped is the incomplete-run guard, not a gap.
fn files_not_reviewed(report: &ScanReport) -> usize {
    use crate::ai_audit::FailureClass;
    let mut files: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut unnamed = 0usize;
    for f in report.failed_passes.iter().filter(|f| {
        !f.scope_note && !matches!(f.class, FailureClass::SpendLimit | FailureClass::OutageStop)
    }) {
        if f.files.is_empty() {
            unnamed += f.files_not_reviewed;
        } else {
            files.extend(f.files.iter().map(|p| (f.repo.as_str(), p.as_str())));
        }
    }
    files.len() + unnamed
}

/// The guards that hold for `report`, in a fixed order.
fn guards(report: &ScanReport, fixes: FixCount) -> Vec<Guard> {
    use crate::ai_audit::FailureClass;
    let mut out: Vec<Guard> = Vec::new();
    if report.code_lines < MIN_CODE_LINES {
        out.push(Guard {
            reason: GuardReason::FewLines,
            detail: text::few_lines(report.code_lines),
        });
    }
    if report.tree.is_none() {
        out.push(Guard {
            reason: GuardReason::NoReviewTree,
            detail: text::NO_REVIEW_TREE.to_string(),
        });
    }
    let stopped = report.ledger.is_outage_stopped()
        || report.ledger.is_cost_capped()
        || report.failed_passes.iter().any(|f| {
            !f.scope_note && matches!(f.class, FailureClass::SpendLimit | FailureClass::OutageStop)
        });
    let causes = [
        (report.gated || report.blocked, text::NO_SCAN),
        (report.ai_blocked_reason.is_some(), text::AI_NOT_RUN),
        (report.ai_error.is_some(), text::AI_FAILED),
        (
            !report.repos_not_inspected.is_empty(),
            text::REPO_NOT_INSPECTED,
        ),
        (stopped, text::RUN_STOPPED),
    ];
    for (holds, detail) in causes {
        if holds {
            out.push(Guard {
                reason: GuardReason::IncompleteRun,
                detail: detail.to_string(),
            });
        }
    }
    if over_percent(
        fixes.without_estimate as u64,
        fixes.counted as u64,
        MAX_UNESTIMATED_PERCENT,
    ) {
        out.push(Guard {
            reason: GuardReason::ManyUnestimated,
            detail: text::many_unestimated(fixes),
        });
    }
    out
}

/// The advisory of `report`. Pure: the same report and corpus give the same advisory, byte for
/// byte once serialized. Makes no model call and reads no file.
pub fn compute(report: &ScanReport, corpus: Option<&camerata_rules::RuleSet>) -> RewriteAdvisory {
    let all = tally_fixes(report, corpus);
    // An effort that is not low, medium or high adds no hours, so it is no estimate.
    let estimated: Vec<&FixTally> = all
        .iter()
        .filter(|fx| {
            crate::report_export::effort_hours_bounds(Some(&fx.effort))
                .0
                .is_some()
        })
        .collect();
    let fixes = FixCount {
        counted: all.len(),
        without_estimate: all.len() - estimated.len(),
    };
    let signals = Signals {
        fix_hours_per_kloc: fix_hours(&estimated, report.code_lines),
        structural_share: structural_share(&estimated),
        unsettled_areas: unsettled_areas(report),
        repeat_cause_share: repeat_cause(&estimated),
    };
    let critical_high_fixes = all.iter().filter(|fx| fx.worst <= 1).count();
    let context = Context {
        critical_high_fixes,
        per_kloc: if report.code_lines == 0 {
            0.0
        } else {
            one_decimal(critical_high_fixes as f64 * 1000.0 / report.code_lines as f64)
        },
    };
    let guards = guards(report, fixes);
    let signals_over = signals.over();
    let outcome = if !guards.is_empty() {
        Outcome::NotEnoughToSay
    } else if signals_over >= SIGNALS_TO_FIRE {
        Outcome::Fired
    } else {
        Outcome::NotFired
    };
    let mut advisory = RewriteAdvisory {
        run_id: report.run_id.clone().unwrap_or_default(),
        finished_at: report.provenance.finished_at.clone(),
        outcome,
        signals_over,
        code_lines: report.code_lines,
        files_not_reviewed: files_not_reviewed(report),
        guards,
        fixes,
        rewrite_signals: signals,
        context,
        limits: Limits {
            min_code_lines: MIN_CODE_LINES,
            max_unestimated_percent: MAX_UNESTIMATED_PERCENT,
            signals_to_fire: SIGNALS_TO_FIRE,
        },
        lines: Vec::new(),
    };
    advisory.lines = text::lines(&advisory);
    advisory
}

// ── On disk ─────────────────────────────────────────────────────────────────────────────────

/// Where the advisory goes for a given ledger file: next to it, same stem (as the failure
/// report). The JSON, the readable form and the history of every run's raw numbers.
pub fn paths_next_to(ledger_file: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let name = ledger_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".ledger.json").unwrap_or(&name);
    let dir = ledger_file.parent().unwrap_or_else(|| Path::new("."));
    (
        dir.join(format!("{stem}.rewrite_advisory.json")),
        dir.join(format!("{stem}.rewrite_advisory.md")),
        dir.join(format!("{stem}.rewrite_advisory.history.jsonl")),
    )
}

/// How many runs the history keeps.
const KEEP_RUNS: usize = 500;

/// Write the advisory's JSON and readable form next to `ledger_file`, replacing the last run's,
/// and add its raw numbers to the history (a run written again, a resume, replaces its own line).
/// Returns the JSON and Markdown paths.
pub fn write(
    ledger_file: &Path,
    advisory: &RewriteAdvisory,
) -> std::io::Result<(PathBuf, PathBuf)> {
    let (json_path, md_path, history_path) = paths_next_to(ledger_file);
    std::fs::write(&json_path, advisory.to_json())?;
    std::fs::write(&md_path, render_md(advisory))?;
    let line = serde_json::to_string(&AdvisoryDocument {
        rewrite_advisory: advisory.clone(),
    })
    .map_err(std::io::Error::other)?;
    let previous = std::fs::read_to_string(&history_path).unwrap_or_default();
    let mut kept: Vec<&str> = previous
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| {
            advisory.run_id.is_empty() || run_id_of(l).as_deref() != Some(advisory.run_id.as_str())
        })
        .collect();
    kept.push(&line);
    if kept.len() > KEEP_RUNS {
        kept.drain(..kept.len() - KEEP_RUNS);
    }
    let mut history = kept.join("\n");
    history.push('\n');
    std::fs::write(&history_path, history)?;
    Ok((json_path, md_path))
}

/// The run id of one history line, when it parses. A line that does not parse is kept as it is.
fn run_id_of(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value
        .get("rewrite_advisory")?
        .get("run_id")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
