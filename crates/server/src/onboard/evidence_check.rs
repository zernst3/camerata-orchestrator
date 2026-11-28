//! M5: quoted evidence must exist.
//!
//! The model quotes a line of code with every finding it writes, and nothing used to open the
//! file and look. `resolve_finding_lines` corrects a line from a snippet that is in the file, but
//! a snippet found nowhere shipped anyway: a client's developers could be handed a quoted line
//! that does not exist.
//!
//! [`verify_evidence`] runs over a repo's finished rows, before they join the report. For every
//! row with a snippet it opens the file from the scanned tree and asks whether the snippet,
//! whitespace-normalised, occurs within [`EVIDENCE_WINDOW_LINES`] lines of the row's line. A row
//! where it does not is marked with the [`EVIDENCE_UNVERIFIED_CAPTURE`] capture. The row is never
//! dropped and its text is never changed: the mark holds it for review
//! (`report_export::is_held_for_review`), and the release gate refuses to export it without a
//! reviewer decision (`release_gate::evidence`).
//!
//! A row that cites a file that is not there is held too, even one exempt from the quote check
//! (no line, no quote, an informational kind): the exemption is for the quote, never for the
//! file. A model-written row on a file that exists but was not among the admitted files is held
//! as well, because the disk fallback is for deterministic rows only (#546).
//!
//! The mark is a `captures` key rather than a new field of `Finding`, the way the fix and detail
//! source marks are: it is a fact about this row in this run, and a new field would touch every
//! place a `Finding` is written out.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::onboard::Finding;
use crate::scan_ledger::EvidenceCheck;

/// How many lines either side of a row's cited line its snippet may sit. The model counts lines
/// badly, so a snippet a line or two off is the usual honest error; further than this, the quote
/// is not at the place the row says it is.
pub const EVIDENCE_WINDOW_LINES: usize = 3;

/// The `captures` key that marks a row whose quoted snippet was not found near its line. The value
/// says why, in a sentence. Absent on a verified or exempt row. A bookkeeping key
/// (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`), so two rows carrying the same sentence are not read as
/// the same object.
pub const EVIDENCE_UNVERIFIED_CAPTURE: &str = "evidence_unverified";

/// The `captures` key that lists which absorbed sites (`Finding::also_locations`) had a snippet
/// that was not found near their own line, one `path:line` per line of the value. Present exactly
/// when a site failed; the row also carries [`EVIDENCE_UNVERIFIED_CAPTURE`], which holds it.
pub const EVIDENCE_UNVERIFIED_SITES_CAPTURE: &str = "evidence_unverified_sites";

/// The `captures` key the model-row parser writes on every row the model wrote, at birth. The
/// evidence check reads it so that a row whose calibration failed (no confidence) is still known to
/// be model-written. A bookkeeping key (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`).
pub const MODEL_WRITTEN_CAPTURE: &str = "model_written";

/// The [`EVIDENCE_UNVERIFIED_CAPTURE`] value of a row whose own snippet verified but an absorbed
/// site's did not.
const ABSORBED_SITE_REASON: &str =
    "an absorbed site's quoted snippet was not found near its own line";

/// The [`EVIDENCE_UNVERIFIED_CAPTURE`] value of a row whose absorbed place cites a file that is
/// missing, or that a model-written row cited without the file having been admitted.
const PLACE_FILE_REASON: &str =
    "an absorbed site cites a file that is missing, was not among the admitted files, or is in \
     another repository and could not be checked";

/// The largest file the check will open from disk when it was not among the scanned files (for
/// example a file over the scan's own size cap, or an unchanged file on an incremental run).
const MAX_EVIDENCE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The kinds of row the check does not apply to. This is the one list: [`exemption`] decides from
/// it and nothing else does. Every exemption is a row with no quoted line of code to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exempt {
    /// The row has no snippet at all (a coverage row, a file-level finding).
    NoSnippet,
    /// The row cites no line (line 0): it is about the file or the project, so the snippet is a
    /// description, not a quote, and there is no line to look near.
    NoCitedLine,
    /// A convention summary, a no-convention statement or a not-assessed row. These state a
    /// pattern across the repo, not a defect at a line.
    Informational,
    /// A dependency advisory: its snippet is `package@version`, which is read from a lockfile by
    /// a tool, not quoted by a model.
    DependencyAdvisory,
    /// A row from an outside tool whose snippet is the tool's own message (the scan has no code
    /// line to quote for a SARIF result, so the message stands in). The tool located the row;
    /// its message is a description that is not expected to occur in the file.
    ToolMessage,
}

impl Exempt {
    /// Every exempt kind.
    pub const ALL: [Exempt; 5] = [
        Exempt::NoSnippet,
        Exempt::NoCitedLine,
        Exempt::Informational,
        Exempt::DependencyAdvisory,
        Exempt::ToolMessage,
    ];

    /// A short name for the kind, for logs and tests.
    pub fn label(self) -> &'static str {
        match self {
            Exempt::NoSnippet => "no snippet",
            Exempt::NoCitedLine => "no cited line",
            Exempt::Informational => "convention summary or not-assessed row",
            Exempt::DependencyAdvisory => "dependency advisory",
            Exempt::ToolMessage => "tool message in place of a quote",
        }
    }
}

/// Which exempt kind `f` is, or `None` when the check applies to it.
pub fn exemption(f: &Finding) -> Option<Exempt> {
    if f.rule_id == crate::dep_audit::DEP_AUDIT_RULE_ID {
        return Some(Exempt::DependencyAdvisory);
    }
    if f.kind
        .as_ref()
        .is_some_and(camerata_api_types::audit::FindingKind::is_informational)
    {
        return Some(Exempt::Informational);
    }
    if f.snippet.trim().is_empty() {
        return Some(Exempt::NoSnippet);
    }
    if snippet_is_tool_message(f) {
        return Some(Exempt::ToolMessage);
    }
    if f.line == 0 {
        return Some(Exempt::NoCitedLine);
    }
    None
}

/// Whether `f` is a row of an outside tool whose snippet is the tool's message: the scan builds a
/// preview row's explanation from the message, then quotes the head of the same message as the
/// snippet, so the snippet is a prefix of the detail. A snippet the tool really quoted from the
/// file is not a prefix of its explanation, and is held to the file like any other.
fn snippet_is_tool_message(f: &Finding) -> bool {
    f.preview && f.preview_tool.is_some() && f.detail.trim_start().starts_with(f.snippet.trim())
}

/// Whitespace-normalised: every run of whitespace is one space, ends trimmed.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `path` with leading `./` segments and doubled slashes removed, so `./src//a.ts` finds
/// `src/a.ts`. Nothing else is changed: a case variant is a different path and stays held.
fn clean_path(path: &str) -> String {
    let mut out = path;
    while let Some(rest) = out.strip_prefix("./") {
        out = rest;
    }
    let mut cleaned = String::with_capacity(out.len());
    let mut last_slash = false;
    for c in out.chars() {
        if c == '/' && last_slash {
            continue;
        }
        last_slash = c == '/';
        cleaned.push(c);
    }
    cleaned
}

/// One file's text with the byte offset of each line, so a window of lines is a slice.
struct IndexedFile<'a> {
    text: Cow<'a, str>,
    starts: Vec<usize>,
}

impl<'a> IndexedFile<'a> {
    fn new(text: Cow<'a, str>) -> Self {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { text, starts }
    }

    /// The text of lines `from..to` (0-based, `to` exclusive), clamped to the file.
    fn lines(&self, from: usize, to: usize) -> &str {
        let n = self.starts.len();
        let from = from.min(n);
        let to = to.min(n);
        if from >= to {
            return "";
        }
        let start = self.starts[from];
        let end = if to >= n {
            self.text.len()
        } else {
            self.starts[to]
        };
        &self.text[start..end]
    }
}

/// Whether `snippet` occurs, whitespace-normalised, within [`EVIDENCE_WINDOW_LINES`] lines of the
/// 1-based `line` in `content`. A snippet that spans several lines may start anywhere in the
/// window. The window text is also compared after the same secret redaction the snippet went
/// through, because a row's snippet is redacted before it is stored and the file is not.
pub fn snippet_near_line(content: &str, snippet: &str, line: usize) -> bool {
    let file = IndexedFile::new(Cow::Borrowed(content));
    snippet_near_line_indexed(&file, snippet, line)
}

fn snippet_near_line_indexed(file: &IndexedFile<'_>, snippet: &str, line: usize) -> bool {
    let needle = normalise(snippet);
    if needle.is_empty() || line == 0 {
        return false;
    }
    let snippet_lines = snippet.trim().lines().count().max(1);
    let from = line.saturating_sub(1 + EVIDENCE_WINDOW_LINES);
    let to = line - 1 + EVIDENCE_WINDOW_LINES + snippet_lines;
    let window = file.lines(from, to);
    if normalise(window).contains(&needle) {
        return true;
    }
    // Two redactions: the client one (every secret and a documentation key masked) and the model
    // one (a documentation key left readable). A snippet that mixes a masked real secret with a
    // readable documentation key, as an older stored row does, only matches the second.
    let for_client = crate::onboard::redact::redact_visible_for_client(window);
    if normalise(&for_client).contains(&needle) {
        return true;
    }
    let for_model = crate::onboard::redact::redact_visible(window);
    normalise(&for_model).contains(&needle)
}

/// The rule ids the scan's deterministic passes emit: the content floor, the architectural
/// checkers and the outside tools. A row is deterministic only when one of these produced it; every
/// other row without a deterministic mark is treated as model-written, so a row nothing identifies
/// (calibration failed, an older scan's row with no birth mark) fails closed.
#[derive(Debug, Default, Clone)]
pub struct DeterministicRules(HashSet<String>);

impl DeterministicRules {
    /// No rule is known to be deterministic. Rows are still deterministic by their own structure
    /// (a tool preview, a dependency advisory, a runtime row, a reviewer's row).
    pub fn none() -> Self {
        Self::default()
    }

    /// These rule ids.
    pub fn of<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(ids.into_iter().map(Into::into).collect())
    }

    /// The rules the scan ledger records under a deterministic tier (the floor, the architectural
    /// engine, an outside tool). A rule recorded under the semantic tier is not among them, even
    /// when a checker exists for it: the model judged it.
    pub fn from_ledger(ledger: &crate::scan_ledger::ScanLedger) -> Self {
        use crate::scan_ledger::RuleTier;
        Self(
            ledger
                .rules()
                .filter(|e| {
                    matches!(
                        e.tier,
                        RuleTier::Deterministic | RuleTier::Architectural | RuleTier::ExternalTool
                    )
                })
                .map(|e| e.rule_id.clone())
                .collect(),
        )
    }

    fn contains(&self, rule_id: &str) -> bool {
        self.0.contains(rule_id)
    }
}

/// Whether the row itself says a non-model producer made it: an outside tool's preview row, the
/// dependency scanner, the runtime audit, or a row the reviewer wrote by hand.
fn is_structurally_deterministic(f: &Finding) -> bool {
    (f.preview && f.preview_tool.is_some())
        || f.rule_id == crate::dep_audit::DEP_AUDIT_RULE_ID
        || f.rule_id == crate::runtime_audit::EOL_RULE_ID
        || f.rule_id == crate::runtime_audit::VULN_RULE_ID
        || f.captures
            .get(crate::report_export::overlay::PROVENANCE_CAPTURE)
            .is_some_and(|v| v == crate::report_export::overlay::REVIEWER_PROVENANCE)
}

/// Where the check reads files from: the scanned files in memory first, then the repo folder on
/// disk for a file the scan did not hold (over the size cap, or unchanged on an incremental run).
struct Sources<'a> {
    files: HashMap<&'a str, &'a str>,
    root: Option<&'a Path>,
    cache: HashMap<String, Option<IndexedFile<'a>>>,
    det: &'a DeterministicRules,
}

impl<'a> Sources<'a> {
    fn new(
        files: &'a [(String, String)],
        root: Option<&'a Path>,
        det: &'a DeterministicRules,
    ) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, c)| (p.as_str(), c.as_str()))
                .collect(),
            root,
            cache: HashMap::new(),
            det,
        }
    }

    fn file(&mut self, path: &str) -> Option<&IndexedFile<'a>> {
        if !self.cache.contains_key(path) {
            let loaded = self.load(path);
            self.cache.insert(path.to_string(), loaded);
        }
        self.cache.get(path).and_then(Option::as_ref)
    }

    /// Whether `path` is one of the scanned files held in memory: the files this run admitted.
    fn admitted(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.files.contains_key(clean_path(path).as_str())
    }

    /// Whether the file exists: among the scanned files, or a regular file inside the repo folder
    /// (of any size, since this only asks whether it is there).
    fn exists(&self, path: &str) -> bool {
        self.admitted(path) || self.disk_path(path).is_some()
    }

    /// The path of a regular file inside the repo folder that `path` names, or `None`.
    fn disk_path(&self, path: &str) -> Option<std::path::PathBuf> {
        let root = self.root?;
        let rel = Path::new(path);
        // A path the row names is data from the model: never follow it out of the repo folder.
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
        let full = root.join(rel);
        // Never through a link: the file may be outside the repository folder.
        crate::onboard::files::is_regular_file_inside(root, &full).then_some(full)
    }

    fn load(&self, path: &str) -> Option<IndexedFile<'a>> {
        if let Some(text) = self
            .files
            .get(path)
            .or_else(|| self.files.get(clean_path(path).as_str()))
        {
            return Some(IndexedFile::new(Cow::Borrowed(*text)));
        }
        let full = self.disk_path(path)?;
        let meta = std::fs::metadata(&full).ok()?;
        if !meta.is_file() || meta.len() > MAX_EVIDENCE_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(&full).ok()?;
        Some(IndexedFile::new(Cow::Owned(
            String::from_utf8_lossy(&bytes).into_owned(),
        )))
    }
}

/// Check every row in `findings` and mark the unverified ones. Rows are kept in order and never
/// dropped. The only fields touched are the [`EVIDENCE_UNVERIFIED_CAPTURE`] capture (cleared again
/// on a row that now verifies) and, for a row whose snippet is not near its line, the `line`, which
/// takes the existing `resolve_finding_lines` correction when the snippet is on one line of the
/// file.
///
/// `files` are the scanned files of the repo and `root` its folder, for a file not among them.
/// Returns the counts for the scan ledger.
pub fn verify_evidence(
    findings: &mut [Finding],
    files: &[(String, String)],
    root: Option<&Path>,
) -> EvidenceCheck {
    verify_evidence_with(findings, files, root, &DeterministicRules::none())
}

/// [`verify_evidence`] told which rule ids the scan's deterministic passes emit. Rows of those
/// rules (and rows deterministic by their own structure) keep the disk fallback for a file the
/// scan did not admit; every other row is held there.
pub fn verify_evidence_with(
    findings: &mut [Finding],
    files: &[(String, String)],
    root: Option<&Path>,
    det: &DeterministicRules,
) -> EvidenceCheck {
    let mut sources = Sources::new(files, root, det);
    let mut counts = EvidenceCheck::default();
    for f in findings.iter_mut() {
        let row_exempt = exemption(f);
        let by_kind = matches!(
            row_exempt,
            Some(Exempt::Informational | Exempt::DependencyAdvisory)
        );
        let mut failed_sites = if by_kind {
            counts.site_exempt += f.also_locations.len();
            Vec::new()
        } else {
            verify_sites(f, &mut sources, &mut counts)
        };
        // Every absorbed place that cites a file gets the file check, whatever its line, quote or
        // kind: a place on a missing file, or (on a model-written row) on a file that was not
        // admitted, holds the row.
        let place_files_failed = place_file_failures(f, &sources);
        for place in place_files_failed {
            if !failed_sites.contains(&place) {
                failed_sites.push(place);
            }
        }
        let sites_checked = !by_kind
            && f.also_locations
                .iter()
                .any(|s| site_is_checkable(s, &f.repo));
        // An exempt row has no quote to look for, but a file it cites that does not exist is
        // held all the same: the exemption never lets a missing file pass unexamined.
        let primary_why = if row_exempt.is_some() {
            file_problem(&f.path, is_model_written(f, sources.det), &sources)
        } else {
            primary_unverified_reason(f, &mut sources)
        };
        // A row exempt for its own snippet (none, or no cited line) is still checked when it
        // carries absorbed sites that have snippets of their own, or cites a missing file.
        if row_exempt.is_some()
            && primary_why.is_none()
            && !sites_checked
            && failed_sites.is_empty()
        {
            counts.exempt += 1;
            continue;
        }
        counts.checked += 1;
        let why = primary_why.or_else(|| {
            (!failed_sites.is_empty()).then(|| {
                if place_files_were_the_cause(f, &sources) {
                    PLACE_FILE_REASON.to_string()
                } else {
                    ABSORBED_SITE_REASON.to_string()
                }
            })
        });
        if failed_sites.is_empty() {
            f.captures.remove(EVIDENCE_UNVERIFIED_SITES_CAPTURE);
        } else {
            f.captures.insert(
                EVIDENCE_UNVERIFIED_SITES_CAPTURE.to_string(),
                failed_sites.join("\n"),
            );
        }
        match why {
            Some(why) => {
                counts.unverified += 1;
                f.captures
                    .insert(EVIDENCE_UNVERIFIED_CAPTURE.to_string(), why);
            }
            None => {
                counts.verified += 1;
                f.captures.remove(EVIDENCE_UNVERIFIED_CAPTURE);
            }
        }
    }
    counts
}

/// `path:line` for each absorbed place whose file is missing, or, for a model-written row (or a
/// place a model wrote), was not admitted. A model-written row's place in another repository is
/// held as one that could not be checked.
fn place_file_failures(f: &Finding, sources: &Sources<'_>) -> Vec<String> {
    let row_model = is_model_written(f, sources.det);
    f.also_locations
        .iter()
        .filter(|s| {
            let model = row_model || s.rule_id.starts_with("AI-");
            if s.repo.is_empty() || s.repo == f.repo {
                file_problem(&s.path, model, sources).is_some()
            } else {
                // A place in another repository: its files are not in hand, so a model-written
                // row's place cannot be checked and is held. A deterministic row's is unchanged.
                model && !crate::onboard::placeholder_paths::is_placeholder(&s.path)
            }
        })
        .map(|s| format!("{}:{}", s.path, s.line))
        .collect()
}

/// Whether some absorbed place of `f` fails the file check, so the row's reason names the file
/// and not the snippet.
fn place_files_were_the_cause(f: &Finding, sources: &Sources<'_>) -> bool {
    !place_file_failures(f, sources).is_empty()
}

/// Why a cited file stops a row: it does not exist, or `model_written` says a model cited it and
/// it was not among the admitted files. `None` when the path names no file (empty, or a
/// placeholder such as `(repo)`) or the file is fine. A finding about an absence (no lockfile, no
/// security policy) cites a path that does not exist on purpose, and is held for a reviewer
/// decision here, never dropped.
fn file_problem(path: &str, model_written: bool, sources: &Sources<'_>) -> Option<String> {
    if crate::onboard::placeholder_paths::is_placeholder(path) {
        return None;
    }
    if !sources.exists(path) {
        return Some(missing_file_text(path));
    }
    if model_written && !sources.admitted(path) {
        return Some(not_admitted_text(path));
    }
    None
}

/// The held reason for a cited file that is not in the scanned tree.
fn missing_file_text(path: &str) -> String {
    format!("the file {path} was not found in the scanned tree")
}

/// The held reason for a model-written row on a file that exists but was not admitted.
fn not_admitted_text(path: &str) -> String {
    format!(
        "the file {path} was not among the files this inspection admitted, so a model-written \
         finding on it cannot be checked against its quote"
    )
}

/// Whether a model wrote `f`, failing closed. In order: a row deterministic by its own structure
/// (a tool preview, the dependency scanner, the runtime audit, a reviewer's row) is not; then a row
/// marked at birth, one the model run tagged `semantic` (older stored rows carry this), an `AI-`
/// row or an agent finder row is; every other row is model-written unless the ledger lists its rule
/// under a deterministic tier. Confidence never decides: an unrated deterministic row carries one,
/// and a model row whose calibration failed does not.
fn is_model_written(f: &Finding, det: &DeterministicRules) -> bool {
    if is_structurally_deterministic(f) {
        return false;
    }
    f.captures.contains_key(MODEL_WRITTEN_CAPTURE)
        || f.captures
            .get(crate::onboard::PRODUCED_TIER_CAPTURE)
            .is_some_and(|t| t == "semantic")
        || f.rule_id.starts_with("AI-")
        || crate::possible_false_positive::is_agent_row(f)
        || !det.contains(&f.rule_id)
}

/// Why the row's own snippet is unverified, or `None` when it is near its line.
fn primary_unverified_reason(f: &mut Finding, sources: &mut Sources<'_>) -> Option<String> {
    // The disk fallback is for deterministic rows on files the scan left out (a secrets row on a
    // file over the read limit). A model only quotes the files it was given, so a model-written
    // row on a file that was not admitted cannot have quoted it.
    if is_model_written(f, sources.det) && sources.exists(&f.path) && !sources.admitted(&f.path) {
        return Some(not_admitted_text(&f.path));
    }
    match sources.file(&f.path) {
        None => Some(missing_file_text(&f.path)),
        Some(file) => {
            // A snippet already near the cited line is left exactly where the producer put it
            // (a detector may cite the line after the one it quotes). Only a snippet that is not
            // near gets the existing line correction, and is then looked for again.
            let near = snippet_near_line_indexed(file, &f.snippet, f.line);
            if !near {
                crate::ai_audit::resolve_finding_line(f, &file.text);
            }
            (!near && !snippet_near_line_indexed(file, &f.snippet, f.line)).then(|| {
                format!(
                    "the quoted snippet was not found within {EVIDENCE_WINDOW_LINES} lines of line {}",
                    f.line
                )
            })
        }
    }
}

/// Whether an absorbed site is one the check applies to: it quotes code (a snippet and a line,
/// and it is not a consequence whose snippet describes an impact) in the row's own repo, whose
/// files are the ones in hand.
fn site_is_checkable(site: &crate::onboard::MergedLocation, row_repo: &str) -> bool {
    !site.consequence && site.line != 0 && !site.snippet.trim().is_empty() && site.repo == row_repo
}

/// Check every absorbed site of `f` against its own path and line, counting them in `counts`.
/// Returns `path:line` for each site whose snippet was not found.
fn verify_sites(
    f: &mut Finding,
    sources: &mut Sources<'_>,
    counts: &mut EvidenceCheck,
) -> Vec<String> {
    let mut failed = Vec::new();
    let row_repo = f.repo.clone();
    for site in &mut f.also_locations {
        if !site_is_checkable(site, &row_repo) {
            counts.site_exempt += 1;
            continue;
        }
        counts.site_checked += 1;
        let found = match sources.file(&site.path) {
            None => false,
            Some(file) => {
                let near = snippet_near_line_indexed(file, &site.snippet, site.line);
                if near {
                    true
                } else {
                    site.line = crate::ai_audit::resolve_line(&file.text, &site.snippet, site.line);
                    snippet_near_line_indexed(file, &site.snippet, site.line)
                }
            }
        };
        if found {
            counts.site_verified += 1;
        } else {
            counts.site_unverified += 1;
            failed.push(format!("{}:{}", site.path, site.line));
        }
    }
    failed
}

/// [`verify_evidence`] over one repo's finished rows, with its counts added to the scan's ledger.
/// The one call the scan makes.
pub fn verify_and_record(
    ledger: &mut crate::scan_ledger::ScanLedger,
    findings: &mut [Finding],
    files: &[(String, String)],
    root: Option<&Path>,
) {
    let det = DeterministicRules::from_ledger(ledger);
    let counts = verify_evidence_with(findings, files, root, &det);
    if counts.unverified > 0 {
        tracing::warn!(
            unverified = counts.unverified,
            checked = counts.checked,
            "{} finding(s) quote evidence that was not found near the cited line; held for review",
            counts.unverified
        );
    }
    ledger.record_evidence_check(counts);
}

#[cfg(test)]
mod tests;
