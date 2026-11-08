//! Escalations and patterns JSON, the confidentiality marking, the client-name and
//! run-completeness gates, the disclaimers and the tool-coverage disclosures.

use super::*;

/// The site count above which only the reshaped PDF (summary, escalations and patterns) is
/// rendered (amendment A14 item 4). At 30,000 sites the full PDF compiled in about 22 seconds
/// (M8); the limit is the size the full PDF was measured at.
pub const FULL_PDF_SITE_LIMIT: usize = 30_000;

/// A site the group reviewer escalated, as the PDF prints it, with the decision on it. Nothing the
/// group reviewer wrote about the site (its reason, rank or words) is in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EscalatedSiteJson {
    /// The sub-group the place sits in.
    pub group: String,
    /// The Fix the place is in.
    pub fix_id: String,
    pub headline: String,
    pub severity: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    /// Every rule at the place, by id, the worst first. One entry is one place (one escalation
    /// slot), whatever the number of rules at it.
    pub rules: Vec<String>,
    /// The decision on the place, as its rows state it ("Confirmed by the reviewer ...", "Open ...").
    pub decision: String,
    pub review_level: String,
}

/// The escalations section: the sentence the ledger states and the sites above the line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EscalationsJson {
    /// "Escalated 347 of a budget of 300 (47 over, ...)", worked out from the reviewed tree. Empty
    /// when the group review did not run for this report.
    pub sentence: String,
    pub items: Vec<EscalatedSiteJson>,
}

/// One sub-group as the PDF describes it: what its sites share, the fix once and a worked example.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PatternJson {
    /// The Fix id; the All problems filter is `Fix id = <id>`.
    pub group: String,
    pub rule_id: String,
    pub rule_title: String,
    /// Every rule the Fix satisfies, by id, the worst first.
    pub rules: Vec<String>,
    pub label: String,
    pub cause: String,
    pub summary: String,
    pub severity: String,
    /// Every site of the sub-group that is not a false positive.
    pub site_count: usize,
    /// Of those, the sites still to fix, and the ones accepted as a risk.
    pub open_sites: usize,
    pub ignored_sites: usize,
    /// Sites by review level wire name.
    pub review_levels: std::collections::BTreeMap<String, usize>,
    pub escalated: usize,
    pub effort: String,
    /// The fix, written once.
    pub fix: String,
    /// The primary site's own fix for its code.
    pub worked_example: String,
    pub folders: Vec<String>,
    pub sequence: String,
    /// "N sites, see Sites sheet, filter Group = <id>": the PDF names no individual site here.
    pub pointer: String,
}

/// How many rows the matrix held in one bucket at one severity and kind. In the reshaped PDF the
/// matrix lists no site, and this is what the release gate reconciles its counts against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatrixTallyJson {
    pub bucket: String,
    pub severity: String,
    pub kind: String,
    pub count: usize,
}

/// One service's cell of a pattern row as the report prints it: the ledger's cell state plus
/// the finished label ("Violated, 3 sites", "Clean", "Not applicable: ...", "Not reviewed: ...").
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatternCellJson {
    pub repo: String,
    #[serde(flatten)]
    pub state: crate::scan_ledger::CellState,
    pub label: String,
}

/// One rule the services differ on. Carries no severity and no bucket: it is an observation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatternRowJson {
    pub rule_id: String,
    pub title: String,
    /// The fixed sentence that introduces the row ([`crate::onboard::patterns_matrix::
    /// pattern_sentence`]).
    pub statement: String,
    pub cells: Vec<PatternCellJson>,
}

/// The "Patterns across services" section: where the services of one project differ on a rule.
/// Present only for a project of more than one repo. Never part of the findings, the severity
/// totals or any count (the release gate checks that).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatternsAcrossServicesJson {
    pub repos: Vec<String>,
    /// One plain sentence on what the section is and is not.
    pub note: String,
    pub rows: Vec<PatternRowJson>,
    pub themes: Vec<crate::onboard::patterns_matrix::Theme>,
}

/// What the section says about itself: an observation, not a finding.
pub const PATTERNS_NOTE: &str = "This section compares the services of this project. It is an \
     observation, not a finding: it carries no severity and is in no total. A service where a \
     rule does not apply, or was not reviewed, is shown as such and is never counted as clean.";

/// The finished label of one cell.
pub fn pattern_cell_label(state: &crate::scan_ledger::CellState) -> String {
    use crate::scan_ledger::CellState;
    match state {
        CellState::Violated { sites } => {
            format!(
                "Violated, {sites} {}",
                if *sites == 1 { "site" } else { "sites" }
            )
        }
        CellState::Clean => "Clean".to_string(),
        CellState::NotApplicable { reason } => format!("Not applicable: {reason}"),
        CellState::NotReviewed { reason } => format!("Not reviewed: {reason}"),
    }
}

/// The patterns section for a report, or `None` for a project of one repo. A row for a rule
/// the corpus does not know (an id the model invented) is left out when a corpus is loaded:
/// the section names rules by title and never prints a raw id.
pub fn patterns_across_services_json(
    report: &crate::onboard::ScanReport,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Option<PatternsAcrossServicesJson> {
    let matrix = &report.patterns_across_services;
    if !matrix.is_present() {
        return None;
    }
    let rows = matrix
        .rows
        .iter()
        .filter_map(|row| {
            let title = match corpus {
                Some(c) => c.get_by_id(&row.rule_id)?.title.clone(),
                None => row.rule_id.clone(),
            };
            Some(PatternRowJson {
                rule_id: row.rule_id.clone(),
                statement: crate::onboard::patterns_matrix::pattern_sentence(&title),
                title,
                cells: row
                    .cells
                    .iter()
                    .map(|c| PatternCellJson {
                        repo: c.repo.clone(),
                        label: pattern_cell_label(&c.state),
                        state: c.state.clone(),
                    })
                    .collect(),
            })
        })
        .collect();
    Some(PatternsAcrossServicesJson {
        repos: matrix.repos.clone(),
        note: PATTERNS_NOTE.to_string(),
        rows,
        themes: matrix.themes.clone(),
    })
}

/// The confidentiality marking a client deliverable carries (2026-10-24 decision, recorded in
/// the private decision log).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConfidentialityJson {
    /// "Confidential, prepared for <client> on <date>": every PDF page footer and every
    /// workbook sheet footer.
    pub line: String,
    /// The first page's short handling note: who the report is for, and not to forward it
    /// outside that company without consent.
    pub handling_note: String,
}

/// The longest client name the inspection setup accepts. The name is printed in every PDF
/// footer and every workbook sheet footer, so this is the plain length cap; the workbook
/// footer's own limit (which counts each `&` twice) is checked on top of it, see
/// [`validate_client_name`].
pub const CLIENT_NAME_MAX_CHARS: usize = 100;

/// The most characters Excel keeps in one header or footer string, layout codes included.
pub(crate) const EXCEL_FOOTER_MAX_CHARS: usize = 255;

/// The widest date the marking line can carry ("%-d %B %Y": two digits, the longest month
/// name, four digits). The footer bound is measured with it, so a name accepted today still
/// fits whatever day the inspection is dated.
pub(crate) const WORST_MARKING_DATE: &str = "30 September 2026";

/// The confidentiality line for `client` (already cleaned) and `date`: the one place its
/// wording lives, shared by the marking and the footer-length bound.
pub(crate) fn marking_line(client: &str, date: &str) -> String {
    format!("Confidential, prepared for {client} on {date}")
}

/// A client name as every artifact prints it: invisible zero-width characters removed, then
/// trimmed. The one cleaning step, so the PDF, the workbook and the manifest can never print
/// different strings for the same name.
pub(crate) fn clean_client_name(raw: &str) -> String {
    strip_zero_width(raw).trim().to_string()
}

/// Why an export refused to start: the inspection setup has no usable client name. Its
/// `Display` is the plain message the operator sees (HTTP 422 body, CLI stderr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientNameRequired(pub String);

impl std::fmt::Display for ClientNameRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientNameRequired {}

/// Why an export refused to start: the report covers fewer repos than the project holds. Its
/// `Display` is the plain message the operator sees (HTTP 422 body, CLI stderr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoriesMissing {
    /// The repos the report does not cover.
    pub repos: Vec<String>,
    message: String,
}

impl std::fmt::Display for RepositoriesMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RepositoriesMissing {}

/// The refusal for a client export of a run that did not complete.
#[derive(Debug, Clone)]
pub struct RunIncomplete {
    /// The plain-language refusal, with the reasons.
    pub message: String,
    /// Why the run is incomplete (see [`crate::onboard::ScanReport::incomplete_reasons`]).
    pub reasons: Vec<String>,
    /// True when an outage stop is among the reasons (the CLI exits 5 rather than 6).
    pub outage: bool,
    /// True when the spend limit stopped the run (the CLI exits 4 rather than 6, so a script that
    /// raises the limit and runs again keeps working).
    pub cost_capped: bool,
}

impl std::fmt::Display for RunIncomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RunIncomplete {}

/// Refuse a CLIENT export of a run that did not complete: any failed part, any part that did not
/// run (spend limit or outage stop), a fatal AI error, or a repo not inspected. Nothing is built
/// and no zip is written. An internal export is allowed and is marked instead
/// ([`internal_incomplete_marking`]).
pub fn require_complete_run(
    report: &crate::onboard::ScanReport,
    opts: &ReportOptions,
) -> Result<(), RunIncomplete> {
    if opts.internal {
        return Ok(());
    }
    let reasons = report.incomplete_reasons();
    if reasons.is_empty() {
        return Ok(());
    }
    Err(RunIncomplete {
        message: format!(
            "This inspection did not run to completion, so it cannot be exported for a client: \
             {}. Run it again; nothing was written.",
            reasons.join("; ")
        ),
        reasons,
        outage: report.ledger.is_outage_stopped(),
        cost_capped: report.ledger.is_cost_capped(),
    })
}

/// The cover line of an internal export of a run that did not complete; `None` for a complete run.
pub fn internal_incomplete_marking(
    report: &crate::onboard::ScanReport,
    opts: &ReportOptions,
) -> Option<String> {
    if !opts.internal {
        return None;
    }
    let reasons = report.incomplete_reasons();
    if reasons.is_empty() {
        return None;
    }
    Some(format!(
        "INTERNAL INCOMPLETE. This inspection did not run to completion and is not for a client: {}.",
        reasons.join("; ")
    ))
}

/// Refuse to package a report that covers fewer repos than the project holds. A deliverable for
/// ten of eleven repos, read as the whole codebase, is worse than no deliverable: so a repo the
/// scan could not inspect ([`crate::onboard::ScanReport::repos_not_inspected`]) and a project
/// repo the report does not list at all both stop the export, naming the repos.
pub fn require_all_repos(
    report: &crate::onboard::ScanReport,
    opts: &ReportOptions,
) -> Result<(), RepositoriesMissing> {
    let mut missing: Vec<String> = report
        .repos_not_inspected
        .iter()
        .map(|(repo, _)| repo.clone())
        .collect();
    for repo in &opts.expected_repos {
        if !report.repos.iter().any(|r| r == repo) && !missing.contains(repo) {
            missing.push(repo.clone());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let total = opts
        .expected_repos
        .len()
        .max(report.repos.len() + missing.len());
    let reasons: Vec<String> = missing
        .iter()
        .map(
            |repo| match report.repos_not_inspected.iter().find(|(r, _)| r == repo) {
                Some((_, why)) => format!("{repo} ({why})"),
                None => format!("{repo} (not in this inspection)"),
            },
        )
        .collect();
    Err(RepositoriesMissing {
        message: format!(
            "This report covers {} of {total} repositories, so it cannot be packaged as the \
             inspection of the project. Not inspected: {}. Make each one readable and run the \
             inspection again.",
            total - missing.len(),
            reasons.join("; ")
        ),
        repos: missing,
    })
}

/// Check a client name as the inspection setup saves it: zero-width characters removed and
/// trimmed ([`clean_client_name`]), at most [`CLIENT_NAME_MAX_CHARS`] characters, no line breaks
/// or other control characters (it is one line of a page footer), and short enough that the
/// workbook footer holds the whole confidentiality line (Excel keeps 255 characters, and a
/// literal `&` is written twice). Blank is allowed here (the operator may clear the field);
/// exporting with a blank name is refused by [`require_client_name`].
pub fn validate_client_name(name: &str) -> Result<String, ClientNameRequired> {
    let name = clean_client_name(name);
    if name.chars().count() > CLIENT_NAME_MAX_CHARS {
        return Err(ClientNameRequired(format!(
            "The client name is longer than {CLIENT_NAME_MAX_CHARS} characters. Shorten it in \
             the inspection setup; it is printed in the footer of every page."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ClientNameRequired(
            "The client name contains a line break or another control character. Enter it on \
             one line in the inspection setup."
                .to_string(),
        ));
    }
    if !name.is_empty() {
        let footer = crate::xlsx_export::sheet_footer(&marking_line(&name, WORST_MARKING_DATE));
        let used = footer.encode_utf16().count();
        if used > EXCEL_FOOTER_MAX_CHARS {
            return Err(ClientNameRequired(format!(
                "The client name is too long for the workbook footer: Excel keeps \
                 {EXCEL_FOOTER_MAX_CHARS} characters there and this name needs {used} (each \
                 & counts twice). Shorten it in the inspection setup."
            )));
        }
    }
    Ok(name)
}

/// The export-time check: a valid, NON-BLANK client name, or the message that blocks the
/// export. Called by the one package assembly (`release_gate::build_product_package_with_stem`)
/// and the PDF-only route, before anything is compiled, so no deliverable can say
/// "prepared for ." (2026-10-24 decision).
pub fn require_client_name(opts: &ReportOptions) -> Result<String, ClientNameRequired> {
    let name = validate_client_name(&opts.client_name)?;
    if name.is_empty() {
        return Err(ClientNameRequired(
            "Enter the client name in the inspection setup before exporting. It fills the \
             confidentiality line on every page of the report and every sheet of the workbook."
                .to_string(),
        ));
    }
    Ok(name)
}

/// The scan date as the confidentiality line prints it: "21 October 2026". The SAME instant
/// the cover's "Generated" line uses ([`scan_instant`], falling back to now only when the scan
/// recorded no time), so the two can never name different days.
pub(crate) fn marking_date(provenance: &crate::onboard::ScanProvenance) -> String {
    scan_instant(provenance)
        .unwrap_or_else(chrono::Utc::now)
        .format("%-d %B %Y")
        .to_string()
}

/// THE producer of the confidentiality marking, shared by the PDF data ([`build_report_json`])
/// and the workbook (`xlsx_export::build_workbook`), so the PDF footer and the sheet footers
/// print the same line. A blank client name (reachable only when a caller bypasses
/// [`require_client_name`], as unit tests do) yields "Confidential, prepared on <date>" rather
/// than "prepared for .". Consent is the brand's to give when there is one, else the preparer's.
pub fn confidentiality_marking(
    provenance: &crate::onboard::ScanProvenance,
    opts: &ReportOptions,
) -> ConfidentialityJson {
    let client = clean_client_name(&opts.client_name);
    let date = marking_date(provenance);
    let consent_from = resolve_brand(opts, None).unwrap_or_else(|| resolve_prepared_by(opts, None));
    if client.is_empty() {
        return ConfidentialityJson {
            line: format!("Confidential, prepared on {date}"),
            handling_note: format!(
                "This report is confidential. Please do not forward it without the written \
                 consent of {consent_from}."
            ),
        };
    }
    ConfidentialityJson {
        line: marking_line(&client, &date),
        handling_note: format!(
            "This report is confidential and was prepared for {client} only. Please do not \
             forward it outside {client} without the written consent of {consent_from}."
        ),
    }
}

/// The disclaimer for a project of several repos: the same paragraph, speaking of the
/// repositories the cover page names rather than one repository state.
pub const AUDIT_REPORT_DISCLAIMER_MULTI: &str =
    "This report is an advisory architectural and security assessment based on static \
     analysis of the repository states identified on the cover page. It is not a \
     certification, warranty or guarantee of security. Findings reflect evidence present in \
     the inspected code at scan time and may not represent the live production systems. All \
     remediation should be validated by the client's engineering team against the live \
     environments. The preparing reviewer accepts no liability for actions taken on the basis \
     of this report.";

/// The disclaimer for a project of `repo_count` repos.
pub fn audit_report_disclaimer(repo_count: usize) -> &'static str {
    if repo_count > 1 {
        AUDIT_REPORT_DISCLAIMER_MULTI
    } else {
        AUDIT_REPORT_DISCLAIMER
    }
}

/// General-audit variant of `ai_audit::DEEP_ADVISORY_DISCLAIMER` / the deep-report export's
/// `DEEP_REPORT_ADVISORY` (`lib.rs`) — same honesty posture, worded for the standard
/// (non-deep-tier) brownfield audit this report covers. M4/S12: the calm, board-appropriate
/// paragraph, not a shouty ALL-CAPS block, and no em/en dashes.
pub const AUDIT_REPORT_DISCLAIMER: &str =
    "This report is an advisory architectural and security assessment based on static \
     analysis of the repository state identified on the cover page. It is not a \
     certification, warranty or guarantee of security. Findings reflect evidence present in \
     the inspected code at scan time and may not represent the live production system. All \
     remediation should be validated by the client's engineering team against the live \
     environment. The preparing reviewer accepts no liability for actions taken on the basis \
     of this report.";

/// W6: turn every [`crate::ai_audit::FailedPass`] this scan recorded into ONE explicit,
/// factual disclosure sentence — e.g. "Rule-alternative recommendations for owner/repo: not
/// computed — pass failed (Claude CLI timed out after 300s...)." Shared verbatim between
/// `ExecutiveSummaryJson::failed_passes` and `MethodologyJson::failed_passes` (see
/// [`build_report_json`]) so a reader hits the same honest statement whether they read the
/// summary or the methodology — never a silent gap in either. Empty input yields an empty
/// list, which both sections render as nothing (no fabricated "everything ran cleanly"
/// claim needed — the absence of a disclosure IS the claim, because every real gap here is
/// always disclosed when one exists).
/// W7 (G1/G3): one concise, client-facing disclosure line per NON-dependency tool whose
/// coverage notes record incomplete coverage this scan. Before W7, only `tool ==
/// "osv-scanner"` notes were exported (into the dependency snapshot); semgrep/eslint/ruff/
/// clippy notes reached the UI and the ledger but NO client artifact, so a whole tool tier
/// could fail and the client's PDF/xlsx/JSON said nothing — the ESLint wholesale failure
/// (C11's cost side) was invisible to a client even when disclosed internally.
///
/// G3 is the owner's constraint and this function's shape: coverage IS client-facing, but
/// **concise and useful, not exhaustive** — ONE line per affected tool, written for a client,
/// never the notes' own internal diagnostics (tool exit codes, config paths, per-rule
/// repetition). The full per-note detail stays where it already lives: the UI and the ledger.
/// Rendered via the existing `failed_passes` channel (executive summary + methodology in the
/// PDF/JSON — no template change) and as the Coverage sheet's "Tool coverage notes" section.
///
/// W13 adds a third arm for [`crate::RULE_CORPUS_COVERAGE_TOOL`] — a rule whose own `.toml`
/// would not parse, so the rule never entered the scan. It gets ONE line for the whole scan
/// (with a count, because "how much of the rule library was missing" is the only quantity a
/// client can act on) and it deliberately carries NEITHER the file path NOR the `RulesError`
/// text: both name the reviewer's filesystem and TOML internals, and both stay in the note's
/// own `message` plus the `tracing::error!` line, which no client artifact reads.
/// The sentence a client reads for selected checks no preview tool was wired for. The release
/// gate looks for it by this text, so the producer and the check cannot drift.
pub(crate) const NO_PREVIEW_TOOL_LINE: &str =
    "Some selected checks had no scan-time preview tool available this run; they are not \
     reflected in this report and enforce once wired into CI.";

/// The notes whose claim the ledger backs: an "unrouted" note names a selected rule no preview
/// tool is wired for, and says the check is not reflected in the report. A rule with no such
/// tool is judged by the model instead, and the ledger records it as run, so the claim is false
/// for it and the note is dropped from what a client reads. A rule the ledger does not show as
/// run keeps its note. Every other note passes through unchanged.
pub(crate) fn notes_the_ledger_backs(
    notes: &[crate::onboard::CoverageNote],
    ledger: &crate::scan_ledger::ScanLedger,
) -> Vec<crate::onboard::CoverageNote> {
    notes
        .iter()
        .filter(|n| match crate::scan_tools::unrouted_note_rule(n) {
            Some(rule) => !ledger.rules().any(|e| e.rule_id == rule && e.ran),
            None => true,
        })
        .cloned()
        .collect()
}

/// [`tool_coverage_disclosures`] over the notes `ledger` backs.
pub(crate) fn tool_coverage_disclosures_for(
    notes: &[crate::onboard::CoverageNote],
    ledger: &crate::scan_ledger::ScanLedger,
) -> Vec<String> {
    tool_coverage_disclosures(&notes_the_ledger_backs(notes, ledger))
}

pub(crate) fn tool_coverage_disclosures(notes: &[crate::onboard::CoverageNote]) -> Vec<String> {
    // Counted per tool, not merely collected: the W13 arm states HOW MANY rules were missing
    // from the library, which one deduplicated tool name cannot express.
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for note in notes.iter().filter(|n| n.tool != "osv-scanner") {
        *counts.entry(note.tool.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(tool, count)| {
            if tool == "unrouted" {
                // Not a tool name a client would recognize: these notes mean "a selected
                // check had no scan-time tool available at all".
                NO_PREVIEW_TOOL_LINE.to_string()
            } else if tool == crate::FIREBASE_SCOPE_COVERAGE_TOOL {
                // Not a tool either: the one written statement of what Firebase coverage is.
                crate::FIREBASE_SCOPE_SENTENCE.to_string()
            } else if tool == crate::scan_tools::CLIENT_TOOLING_NOTE_TOOL {
                // Not a tool either, and not a failure: the client's own tooling RAN, with the
                // operator's opt-in. The note's own (client-voice) text says so.
                client_tooling_text(notes, tool)
            } else if notes
                .iter()
                .any(|n| n.tool == tool && crate::scan_tools::is_client_tooling_skip(n))
            {
                // Withheld on purpose (a client engagement with no opt-in), not a tool failure:
                // the note's own text names the step and why.
                client_tooling_text(notes, tool)
            } else if crate::runtime_audit::is_runtime_note_tool(tool) {
                // Not a tool either: the runtime end-of-life and runtime vulnerability pass. Its
                // notes are already written for the reader and name what was not checked.
                every_note_text(notes, tool)
            } else if tool == crate::RULE_CORPUS_COVERAGE_TOOL {
                // Also not a tool name: these notes mean "a rule file in the library could
                // not be read, so that rule was never applied".
                format!(
                    "{} in the rule library could not be loaded this scan and did not run; \
                     anything they would have reported is not reflected in this report.",
                    noun(count, "rule", "rules"),
                )
            } else {
                format!(
                    "The {tool} static-analysis checks did not fully run this scan; \
                     anything they would have reported is not reflected in this report."
                )
            }
        })
        .collect()
}

/// Every note for `tool`, in order and deduplicated, as one disclosure line. For passes whose
/// notes are already client-voice sentences (the runtime end-of-life and vulnerability pass).
pub(crate) fn every_note_text(notes: &[crate::onboard::CoverageNote], tool: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in notes.iter().filter(|n| n.tool == tool) {
        if !seen.contains(&n.message.as_str()) {
            seen.push(n.message.as_str());
        }
    }
    seen.join(" ")
}

/// The client-tooling notes for `tool`, in order and deduplicated, as one disclosure line.
pub(crate) fn client_tooling_text(notes: &[crate::onboard::CoverageNote], tool: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in notes.iter().filter(|n| n.tool == tool) {
        if (tool == crate::scan_tools::CLIENT_TOOLING_NOTE_TOOL
            || crate::scan_tools::is_client_tooling_skip(n))
            && !seen.contains(&n.message.as_str())
        {
            seen.push(n.message.as_str());
        }
    }
    seen.join(" ")
}

pub(crate) fn failed_pass_disclosures(
    failed_passes: &[crate::ai_audit::FailedPass],
    internal: bool,
) -> Vec<String> {
    failed_passes
        .iter()
        .map(|f| {
            let mut pass = f.pass.clone();
            if let Some(first) = pass.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            // A client export is refused for a run with a failed part, so this wording reaches
            // only an internal export; the client-facing form carries no failure wording.
            if f.scope_note {
                // A disclosed limit of the scope (a tool not run), not a failed part.
                return format!("{pass} for {}: not run ({}).", f.repo, f.reason);
            }
            if !internal {
                return format!("{pass} for {}: did not complete.", f.repo);
            }
            format!(
                "{pass} for {}: not computed, pass failed ({}).",
                f.repo, f.reason
            )
        })
        .collect()
}

/// FIX 3 (2026-09-13 review) — a short, FACTUAL "what happens next" paragraph, rendered in a
/// new section before Methodology. Deliberately non-pitch: no urgency language. Three factual
/// steps, one paragraph, no em/en dashes (house style for this client deliverable — see
/// `AUDIT_REPORT_DISCLAIMER`'s doc comment).
///
/// C4-P4 (residual defect 4, 2026-10-01): this used to end step one with "at the rate set out
/// in the engagement" — a placeholder that promised a number (the design doc's public promise
/// 2: "the fix rate is written into the report before the client decides") without ever stating
/// one. A real dollar/hour billing rate genuinely isn't this report's to invent (there is no
/// such field anywhere in `ReportOptions`, and fabricating one would be worse than the
/// placeholder it replaces). The number this report CAN state honestly, because every input to
/// it is already computed for the executive-summary/methodology reconciliation a few lines
/// above this one, is the TRIAGE fix rate.
///
/// C5-6 (REG-6, 2026-10-02): the triage fix rate above used to be `(candidates_reviewed -
/// excluded_false_positive) / candidates_reviewed` — "kept" counted EVERY non-excluded
/// candidate, including rows still sitting in [`MatrixJson::held`] / `informational` with no
/// human disposition at all. On a fresh, unreviewed DRAFT export that produced a flatly false
/// "100% fix rate" sentence calling undecided held rows "real, actionable items" before a
/// reviewer had looked at a single one of them. Two independent bugs, both fixed here:
///
/// - **No percentage before a human has triaged anything.** [`ReviewState::Raw`] (see its doc
///   comment — nobody this session has dispositioned a single finding) can never print a
///   fix-rate percentage, full stop; there is no "rate" yet, only an unreviewed backlog. It
///   prints the held count instead: "Fix rate not yet established: N of M candidates are held
///   for a reviewer's decision; 0 excluded so far" — `0 excluded` is not a placeholder, it is
///   always true here, because [`ReviewState::Raw`] is derived from "no finding carries an
///   explicit this-session auditor disposition," and an excluded-as-false-positive row IS such
///   a disposition (see `build_report_json`'s partition loop).
/// - **Held rows never belong in the rate's numerator OR denominator**, even once a reviewer
///   has acted on OTHER findings. A row still sitting in `held`/`informational` was not kept as
///   "real, actionable," nor was it excluded as noise — it is neither, by construction. The
///   fix rate is now `curated_total / (curated_total + excluded_false_positive)` only: the
///   share of DECIDED candidates (curated for action vs. excluded as a false positive) that
///   were kept. The "excluding the rest as likely false positives" clause is appended only when
///   `excluded_false_positive > 0` — with nothing excluded, "excluding the rest" describes
///   nothing and is simply dropped, not forced to read "excluding 0."
///
/// A zero-candidate (clean) run has no fix rate to report; the paragraph says so plainly
/// instead of dividing by zero or rendering a percentage that implies findings existed. The
/// same "nothing to compute a rate from" sentence is reused, defensively, if a `Reviewed`
/// export somehow has zero curated AND zero excluded candidates (every candidate is held or a
/// dependency advisory) — dividing zero by zero is not a 0% or 100% fix rate, it is "not yet
/// established," identically to the draft case.
#[cfg(test)]
pub(crate) fn next_steps_note(
    review_state: ReviewState,
    candidates_reviewed: usize,
    curated_total: usize,
    held_for_review: usize,
    excluded_false_positive: usize,
) -> String {
    next_steps_note_for(
        false,
        review_state,
        candidates_reviewed,
        curated_total,
        held_for_review,
        excluded_false_positive,
    )
}

/// [`next_steps_note`] for a project of one repo (`multi_repo` false) or several, where the
/// retest covers the repositories and the codebases rather than one repository.
pub(crate) fn next_steps_note_for(
    multi_repo: bool,
    review_state: ReviewState,
    candidates_reviewed: usize,
    curated_total: usize,
    held_for_review: usize,
    excluded_false_positive: usize,
) -> String {
    let (rescan_object, introduces) = if multi_repo {
        ("repositories", "the codebases introduce")
    } else {
        ("repository", "the codebase introduces")
    };
    let not_yet_established = || {
        format!(
            "Fix rate not yet established: {held_for_review} of {} are held for a reviewer's \
             decision; 0 excluded so far.",
            noun(candidates_reviewed, "candidate", "candidates"),
        )
    };
    let fix_rate_sentence = if candidates_reviewed == 0 {
        "This run produced no candidate findings, so there is no fix rate to report.".to_string()
    } else if matches!(review_state, ReviewState::Raw) {
        not_yet_established()
    } else {
        let denominator = curated_total + excluded_false_positive;
        if denominator == 0 {
            not_yet_established()
        } else {
            let pct = (curated_total as f64 / denominator as f64) * 100.0;
            let excluding_clause = if excluded_false_positive > 0 {
                ", excluding the rest as likely false positives"
            } else {
                ""
            };
            format!(
                "This run kept {curated_total} of {} as real, actionable items (a {pct:.0}% \
                 fix rate){excluding_clause}.",
                noun(denominator, "candidate finding", "candidate findings"),
            )
        }
    };
    format!(
        "There are three steps from here. First, the do-now items above get fixed, by your own \
         team, an outside contractor, or the reviewing engineer. {fix_rate_sentence} Second, a \
         retest: the reviewing engineer re-scans the {rescan_object} once the fixes are in and \
         signs a short addendum confirming each item is closed. Third, ongoing coverage: a \
         monthly delta rescan plus on-call architect availability for anything new \
         {introduces} between engagements."
    )
}

// ── FIX 7 business-impact map (§"If you only do three things this week") ───────────
