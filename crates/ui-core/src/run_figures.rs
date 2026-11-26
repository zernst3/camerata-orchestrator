//! The Run figures panel's model (GitHub #293, with #61's batch status): an operator-only,
//! metrics-only summary of a finished inspection run, built from two sources and nothing else.
//!
//! * [`ReportFigures`]: the part of the saved report the app already holds after a run. Scope
//!   (lines, files, languages, files read) and the per-stage figures on the report's ledger
//!   (tokens, calls, models, busy time, first and last call times, batch wait).
//! * [`RunRecordFigures`]: what only the run's folder holds (`GET /api/inspections/:id/figures`):
//!   the sessions' wall-clock time and where each batch stage's batches stand.
//!
//! Metrics only, by construction: the report part deserializes counts, timings and model ids and
//! no other field, so a path, a language name, a finding or a line of code never enters this
//! model. A figure the run did not record is `None` and reads "not recorded", never zero.
//! Nothing here is ever part of the client package.

use std::collections::BTreeMap;

use serde::Deserialize;

pub use camerata_api_types::run_figures::{BatchStageStatus, RunRecordFigures};

/// The words a figure the run did not record reads as.
pub const NOT_RECORDED: &str = "not recorded";

/// The panel's heading line, also the first line of the copied text.
pub const COPY_HEADING: &str = "Run figures (operator only, never part of the client package)";

// ── The report part ──────────────────────────────────────────────────────────────────────────

/// The part of a saved report the panel reads. Every field is optional: a report saved before a
/// figure existed leaves it `None`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ReportFigures {
    /// Non-blank source lines in the files read.
    #[serde(default)]
    pub code_lines: Option<u64>,
    /// One entry per language. Only the count of entries is read; the name is never kept.
    #[serde(default)]
    pub code_lines_by_language: Option<Vec<LanguageLines>>,
    /// The files the inspection read.
    #[serde(default)]
    pub files_scanned: Option<u64>,
    #[serde(default)]
    pub ledger: Option<LedgerFigures>,
}

/// One language's line count. The language name is not deserialized.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct LanguageLines {
    #[serde(default)]
    pub lines: u64,
}

/// The ledger blocks the panel reads.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct LedgerFigures {
    #[serde(default)]
    pub input: Option<InputFigures>,
    #[serde(default)]
    pub stage_telemetry: Option<TelemetryFigures>,
}

/// What the inspection was fed: only the skipped counts (never the named skipped files).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct InputFigures {
    #[serde(default)]
    pub skipped_totals: Option<SkippedTotals>,
}

/// Files not read, per reason. [`Self::total`] counts the same reasons the engine's own total
/// does (a pattern that removed nothing is not a file).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct SkippedTotals {
    #[serde(default)]
    pub oversize: u64,
    #[serde(default)]
    pub beyond_file_cap: u64,
    #[serde(default)]
    pub not_text: u64,
    #[serde(default)]
    pub excluded_by_agreement: u64,
    #[serde(default)]
    pub long_line: u64,
    #[serde(default)]
    pub generated: u64,
    #[serde(default)]
    pub generated_unsearched: u64,
    #[serde(default)]
    pub not_inspected_type: u64,
    #[serde(default)]
    pub not_inspected_versions_only: u64,
}

impl SkippedTotals {
    /// Files not read, for any reason.
    pub fn total(&self) -> u64 {
        self.oversize
            + self.beyond_file_cap
            + self.not_text
            + self.excluded_by_agreement
            + self.long_line
            + self.generated
            + self.generated_unsearched
            + self.not_inspected_type
            + self.not_inspected_versions_only
    }
}

/// The per-stage figures on the ledger.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct TelemetryFigures {
    #[serde(default)]
    pub stages: Vec<StageRecord>,
}

/// One stage's row.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct StageRecord {
    /// The stage as the engine saves it (`scan_pass`, `calibration`, ...).
    pub stage: String,
    #[serde(default)]
    pub usage: StageUsage,
}

/// What one stage spent. Each field is `None` when the saved record has no such key.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct StageUsage {
    #[serde(default)]
    pub calls: Option<u64>,
    /// Billed input, cache reads and writes included (the engine's convention).
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    /// Calls per model id.
    #[serde(default)]
    pub models: Option<BTreeMap<String, u64>>,
    #[serde(default)]
    pub busy_ms: Option<u64>,
    #[serde(default)]
    pub first_call_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_call_unix_ms: Option<u64>,
    #[serde(default)]
    pub batch_wait_ms: Option<u64>,
}

impl StageUsage {
    /// From the first call's start to the last call's end. `None` when the stage recorded no
    /// timed call.
    pub fn wall_clock_ms(&self) -> Option<u64> {
        match (self.first_call_unix_ms, self.last_call_unix_ms) {
            (Some(first), Some(last)) if last >= first => Some(last - first),
            _ => None,
        }
    }

    /// The calls' time in flight, added up. Recorded only when a timed call was (a stage of
    /// untimed calls saves zero, which is not a measurement).
    pub fn busy_time_ms(&self) -> Option<u64> {
        self.first_call_unix_ms.and(self.busy_ms)
    }
}

// ── Stages and roles ─────────────────────────────────────────────────────────────────────────

/// The agent role a stage's calls belong to. A grouping the panel makes over the saved stages:
/// the agents that find problems, the ones that judge what was found, and the ones that write
/// it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentRole {
    Finder,
    Reviewer,
    Writer,
    Other,
}

impl AgentRole {
    pub const ALL: [AgentRole; 4] = [
        AgentRole::Finder,
        AgentRole::Reviewer,
        AgentRole::Writer,
        AgentRole::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AgentRole::Finder => "Finder",
            AgentRole::Reviewer => "Reviewer",
            AgentRole::Writer => "Writer",
            AgentRole::Other => "Other",
        }
    }
}

/// The saved stage names in pipeline order, with the panel's word for each and its role.
const STAGES: [(&str, &str, AgentRole); 10] = [
    ("scan_pass", "Scan", AgentRole::Finder),
    ("finder", "Finder", AgentRole::Finder),
    ("deep_tier", "Deep tier", AgentRole::Finder),
    ("calibration", "Calibration", AgentRole::Reviewer),
    ("group_review", "Group review", AgentRole::Reviewer),
    ("duplicate_check", "Duplicate check", AgentRole::Reviewer),
    ("clone_judge", "Clone judge", AgentRole::Reviewer),
    ("alternatives", "Alternatives", AgentRole::Writer),
    ("detail_regeneration", "Detail", AgentRole::Writer),
    ("fix_generation", "Fix", AgentRole::Writer),
];

/// The panel's word for a saved stage name. An unknown name reads as `Other`.
pub fn stage_label(stage: &str) -> &'static str {
    STAGES
        .iter()
        .find(|(name, _, _)| *name == stage)
        .map_or("Other", |(_, label, _)| label)
}

/// The role a saved stage belongs to.
pub fn stage_role(stage: &str) -> AgentRole {
    STAGES
        .iter()
        .find(|(name, _, _)| *name == stage)
        .map_or(AgentRole::Other, |(_, _, role)| *role)
}

fn stage_rank(stage: &str) -> usize {
    STAGES
        .iter()
        .position(|(name, _, _)| *name == stage)
        .unwrap_or(STAGES.len())
}

// ── Formatting ───────────────────────────────────────────────────────────────────────────────

/// `48,213`: a whole number with thousands separators.
pub fn count(n: u64) -> String {
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

/// `1h 02m 03s`, `4m 05s`, `45s`, `350 ms`.
pub fn duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms} ms");
    }
    let secs = (ms + 500) / 1_000;
    let (h, m, s) = (secs / 3_600, (secs % 3_600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// `94.3%`.
pub fn percent(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

fn or_not_recorded(value: Option<String>) -> String {
    value.unwrap_or_else(|| NOT_RECORDED.to_string())
}

/// `a`, `a and b`, `a, b and c`: no serial comma.
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// The sum of the recorded values: `None` when none was recorded.
fn sum_recorded(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.fold(None, |acc, v| match (acc, v) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    })
}

// ── The panel ────────────────────────────────────────────────────────────────────────────────

/// The panel's views. Buttons switch between them; each is the panel's one table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FiguresView {
    #[default]
    Summary,
    Stages,
    Roles,
    Models,
    Batches,
}

impl FiguresView {
    pub const ALL: [FiguresView; 5] = [
        FiguresView::Summary,
        FiguresView::Stages,
        FiguresView::Roles,
        FiguresView::Models,
        FiguresView::Batches,
    ];

    /// The button's words.
    pub fn label(self) -> &'static str {
        match self {
            FiguresView::Summary => "Summary",
            FiguresView::Stages => "Stages",
            FiguresView::Roles => "Agent roles",
            FiguresView::Models => "Models",
            FiguresView::Batches => "Batches",
        }
    }

    /// A stable name for tests and the click-through (`data-test`).
    pub fn key(self) -> &'static str {
        match self {
            FiguresView::Summary => "summary",
            FiguresView::Stages => "stages",
            FiguresView::Roles => "roles",
            FiguresView::Models => "models",
            FiguresView::Batches => "batches",
        }
    }
}

/// One view's table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FiguresTable {
    pub headers: Vec<&'static str>,
    pub rows: Vec<Vec<String>>,
    /// The one row shown when `rows` is empty.
    pub empty: &'static str,
}

/// Everything the panel shows, worked out once from its two sources.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunFigures {
    report: ReportFigures,
    /// `None` when the run's folder could not be read (no saved run, or the server did not
    /// answer): every figure it holds reads "not recorded".
    record: Option<RunRecordFigures>,
}

/// One row of a stage-like table: the figures summed over some stages.
#[derive(Debug, Clone, Default)]
struct Sums {
    calls: Option<u64>,
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    cached: Option<u64>,
}

impl Sums {
    fn of<'a>(usages: impl Iterator<Item = &'a StageUsage> + Clone) -> Self {
        Self {
            calls: sum_recorded(usages.clone().map(|u| u.calls)),
            tokens_in: sum_recorded(usages.clone().map(|u| u.input_tokens)),
            tokens_out: sum_recorded(usages.clone().map(|u| u.output_tokens)),
            cached: sum_recorded(usages.map(|u| u.cache_read_input_tokens)),
        }
    }

    fn cells(&self) -> [String; 4] {
        [
            or_not_recorded(self.calls.map(count)),
            or_not_recorded(self.tokens_in.map(count)),
            or_not_recorded(self.tokens_out.map(count)),
            or_not_recorded(self.cached.map(count)),
        ]
    }

    /// Cache reads as a share of billed input (which includes them). `None` with no input, or
    /// when reads exceed it (a wrong figure, not a measurement).
    fn cache_hit_share(&self) -> Option<f64> {
        match (self.cached, self.tokens_in) {
            (Some(read), Some(input)) if input > 0 && read <= input => {
                Some(read as f64 / input as f64)
            }
            _ => None,
        }
    }
}

impl RunFigures {
    pub fn new(report: ReportFigures, record: Option<RunRecordFigures>) -> Self {
        Self { report, record }
    }

    /// The stages that made calls, in pipeline order.
    fn stages(&self) -> Vec<&StageRecord> {
        let mut stages: Vec<&StageRecord> = self
            .report
            .ledger
            .as_ref()
            .and_then(|l| l.stage_telemetry.as_ref())
            .map(|t| t.stages.iter().collect())
            .unwrap_or_default();
        stages.sort_by_key(|s| stage_rank(&s.stage));
        stages
    }

    fn telemetry_recorded(&self) -> bool {
        self.report
            .ledger
            .as_ref()
            .is_some_and(|l| l.stage_telemetry.is_some())
    }

    fn totals(&self) -> Sums {
        if !self.telemetry_recorded() {
            return Sums::default();
        }
        let stages = self.stages();
        let sums = Sums::of(stages.iter().map(|s| &s.usage));
        // A run whose ledger holds the block with no stage made no model call: zero is recorded.
        if stages.is_empty() {
            return Sums {
                calls: Some(0),
                tokens_in: Some(0),
                tokens_out: Some(0),
                cached: Some(0),
            };
        }
        sums
    }

    /// Calls per model id over every stage, model-id order.
    fn models(&self) -> BTreeMap<String, (u64, Vec<&'static str>)> {
        let mut out: BTreeMap<String, (u64, Vec<&'static str>)> = BTreeMap::new();
        for s in self.stages() {
            for (model, calls) in s.usage.models.iter().flatten() {
                let entry = out.entry(model.clone()).or_default();
                entry.0 += calls;
                let label = stage_label(&s.stage);
                if !entry.1.contains(&label) {
                    entry.1.push(label);
                }
            }
        }
        out
    }

    fn files_not_read(&self) -> Option<u64> {
        self.report
            .ledger
            .as_ref()
            .and_then(|l| l.input.as_ref())
            .and_then(|i| i.skipped_totals.as_ref())
            .map(SkippedTotals::total)
    }

    /// Files in scope: the files read and the files not read.
    fn files_in_scope(&self) -> Option<u64> {
        Some(self.report.files_scanned? + self.files_not_read()?)
    }

    fn coverage(&self) -> Option<f64> {
        let in_scope = self.files_in_scope()?;
        (in_scope > 0).then(|| self.report.files_scanned.unwrap_or(0) as f64 / in_scope as f64)
    }

    /// The Summary view's figures, as (figure, value) pairs.
    pub fn summary(&self) -> Vec<(&'static str, String)> {
        let totals = self.totals();
        let models = self.models();
        let wall = self.record.as_ref().and_then(|r| r.wall_clock_ms);
        let sessions = self
            .record
            .as_ref()
            .filter(|r| r.wall_clock_ms.is_some())
            .map(|r| r.sessions);
        vec![
            (
                "Lines in scope",
                or_not_recorded(self.report.code_lines.map(count)),
            ),
            (
                "Files in scope",
                or_not_recorded(self.files_in_scope().map(count)),
            ),
            (
                "Languages in scope",
                or_not_recorded(
                    self.report
                        .code_lines_by_language
                        .as_ref()
                        .map(|l| count(l.len() as u64)),
                ),
            ),
            (
                "Files read",
                or_not_recorded(self.report.files_scanned.map(count)),
            ),
            (
                "Coverage",
                or_not_recorded(self.coverage().map(|c| {
                    format!(
                        "{} ({} of {} files read)",
                        percent(c),
                        count(self.report.files_scanned.unwrap_or(0)),
                        count(self.files_in_scope().unwrap_or(0))
                    )
                })),
            ),
            ("Wall clock", or_not_recorded(wall.map(duration))),
            (
                "Sessions",
                or_not_recorded(sessions.map(u64::from).map(count)),
            ),
            ("Model calls", or_not_recorded(totals.calls.map(count))),
            ("Tokens in", or_not_recorded(totals.tokens_in.map(count))),
            ("Tokens out", or_not_recorded(totals.tokens_out.map(count))),
            ("Tokens cached", or_not_recorded(totals.cached.map(count))),
            (
                "Cache hit share",
                or_not_recorded(totals.cache_hit_share().map(percent)),
            ),
            (
                "Models used",
                or_not_recorded((self.telemetry_recorded()).then(|| count(models.len() as u64))),
            ),
        ]
    }

    /// One view's table.
    pub fn table(&self, view: FiguresView) -> FiguresTable {
        match view {
            FiguresView::Summary => FiguresTable {
                headers: vec!["Figure", "Value"],
                rows: self
                    .summary()
                    .into_iter()
                    .map(|(figure, value)| vec![figure.to_string(), value])
                    .collect(),
                empty: NOT_RECORDED,
            },
            FiguresView::Stages => FiguresTable {
                headers: vec![
                    "Stage",
                    "Wall clock",
                    "Busy time",
                    "Model calls",
                    "Tokens in",
                    "Tokens out",
                    "Tokens cached",
                ],
                rows: self
                    .stages()
                    .into_iter()
                    .map(|s| {
                        let u = &s.usage;
                        let mut row = vec![
                            stage_label(&s.stage).to_string(),
                            or_not_recorded(u.wall_clock_ms().map(duration)),
                            or_not_recorded(u.busy_time_ms().map(duration)),
                        ];
                        row.extend(Sums::of(std::iter::once(u)).cells());
                        row
                    })
                    .collect(),
                empty: "Stage figures: not recorded",
            },
            FiguresView::Roles => {
                let stages = self.stages();
                FiguresTable {
                    headers: vec![
                        "Agent role",
                        "Stages",
                        "Model calls",
                        "Tokens in",
                        "Tokens out",
                        "Tokens cached",
                    ],
                    rows: AgentRole::ALL
                        .iter()
                        .filter_map(|role| {
                            let mine: Vec<&StageRecord> = stages
                                .iter()
                                .copied()
                                .filter(|s| stage_role(&s.stage) == *role)
                                .collect();
                            if mine.is_empty() {
                                return None;
                            }
                            let labels: Vec<&str> =
                                mine.iter().map(|s| stage_label(&s.stage)).collect();
                            let mut row = vec![role.label().to_string(), and_list(&labels)];
                            row.extend(Sums::of(mine.iter().map(|s| &s.usage)).cells());
                            Some(row)
                        })
                        .collect(),
                    empty: "Agent role figures: not recorded",
                }
            }
            FiguresView::Models => FiguresTable {
                headers: vec!["Model", "Model calls", "Stages"],
                rows: self
                    .models()
                    .into_iter()
                    .map(|(model, (calls, stages))| vec![model, count(calls), and_list(&stages)])
                    .collect(),
                empty: "Models used: not recorded",
            },
            FiguresView::Batches => {
                let waits: BTreeMap<&str, Option<u64>> = self
                    .stages()
                    .into_iter()
                    .map(|s| (s.stage.as_str(), s.usage.batch_wait_ms))
                    .collect();
                FiguresTable {
                    headers: vec!["Stage", "Submitted", "Processing", "Ended", "Batch wait"],
                    rows: self
                        .record
                        .iter()
                        .flat_map(|r| r.batches.iter())
                        .map(|b| {
                            let wait = waits
                                .get(b.stage.as_str())
                                .copied()
                                .flatten()
                                .filter(|ms| *ms > 0);
                            vec![
                                stage_label(&b.stage).to_string(),
                                count(u64::from(b.submitted)),
                                count(u64::from(b.processing)),
                                count(u64::from(b.ended)),
                                or_not_recorded(wait.map(duration)),
                            ]
                        })
                        .collect(),
                    empty: if self.record.is_some() {
                        "No stage of this run ran in batches."
                    } else {
                        "Batch status: not recorded"
                    },
                }
            }
        }
    }

    /// The plain-text summary the Copy button copies: the heading, then every figure of every
    /// view, one per line, in the order the views list them.
    pub fn copy_text(&self) -> String {
        let mut lines = vec![COPY_HEADING.to_string()];
        for (figure, value) in self.summary() {
            lines.push(format!("{figure}: {value}"));
        }
        for view in [
            FiguresView::Stages,
            FiguresView::Roles,
            FiguresView::Models,
            FiguresView::Batches,
        ] {
            let table = self.table(view);
            if table.rows.is_empty() {
                lines.push(table.empty.to_string());
                continue;
            }
            for row in &table.rows {
                let Some((name, rest)) = row.split_first() else {
                    continue;
                };
                let prefix = match view {
                    FiguresView::Stages => format!("{name} stage"),
                    FiguresView::Roles => format!("{name} role"),
                    FiguresView::Models => format!("Model {name}"),
                    FiguresView::Batches => format!("{name} batches"),
                    FiguresView::Summary => name.clone(),
                };
                for (header, value) in table.headers.iter().skip(1).zip(rest) {
                    lines.push(format!("{prefix}, {}: {value}", header.to_lowercase()));
                }
            }
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

#[cfg(test)]
#[path = "run_figures_tests.rs"]
mod tests;
