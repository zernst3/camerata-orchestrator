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
//! The mark is a `captures` key rather than a new field of `Finding`, the way the fix and detail
//! source marks are: it is a fact about this row in this run, and a new field would touch every
//! place a `Finding` is written out.

use std::borrow::Cow;
use std::collections::HashMap;
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

/// The [`EVIDENCE_UNVERIFIED_CAPTURE`] value of a row whose own snippet verified but an absorbed
/// site's did not.
const ABSORBED_SITE_REASON: &str =
    "an absorbed site's quoted snippet was not found near its own line";

/// The [`EVIDENCE_UNVERIFIED_CAPTURE`] value of a group row built from occurrences, one of which
/// failed.
const GROUPED_OCCURRENCE_REASON: &str =
    "a grouped occurrence's quoted snippet was not found near its line";

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
}

impl Exempt {
    /// Every exempt kind.
    pub const ALL: [Exempt; 4] = [
        Exempt::NoSnippet,
        Exempt::NoCitedLine,
        Exempt::Informational,
        Exempt::DependencyAdvisory,
    ];

    /// A short name for the kind, for logs and tests.
    pub fn label(self) -> &'static str {
        match self {
            Exempt::NoSnippet => "no snippet",
            Exempt::NoCitedLine => "no cited line",
            Exempt::Informational => "convention summary or not-assessed row",
            Exempt::DependencyAdvisory => "dependency advisory",
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
    if f.line == 0 {
        return Some(Exempt::NoCitedLine);
    }
    None
}

/// Whitespace-normalised: every run of whitespace is one space, ends trimmed.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
    let redacted = crate::onboard::redact::redact_visible(window);
    normalise(&redacted).contains(&needle)
}

/// Where the check reads files from: the scanned files in memory first, then the repo folder on
/// disk for a file the scan did not hold (over the size cap, or unchanged on an incremental run).
struct Sources<'a> {
    files: HashMap<&'a str, &'a str>,
    root: Option<&'a Path>,
    cache: HashMap<String, Option<IndexedFile<'a>>>,
}

impl<'a> Sources<'a> {
    fn new(files: &'a [(String, String)], root: Option<&'a Path>) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, c)| (p.as_str(), c.as_str()))
                .collect(),
            root,
            cache: HashMap::new(),
        }
    }

    fn file(&mut self, path: &str) -> Option<&IndexedFile<'a>> {
        if !self.cache.contains_key(path) {
            let loaded = self.load(path);
            self.cache.insert(path.to_string(), loaded);
        }
        self.cache.get(path).and_then(Option::as_ref)
    }

    fn load(&self, path: &str) -> Option<IndexedFile<'a>> {
        if let Some(text) = self.files.get(path) {
            return Some(IndexedFile::new(Cow::Borrowed(*text)));
        }
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
    let mut sources = Sources::new(files, root);
    let mut counts = EvidenceCheck::default();
    for f in findings.iter_mut() {
        let row_exempt = exemption(f);
        let by_kind = matches!(
            row_exempt,
            Some(Exempt::Informational | Exempt::DependencyAdvisory)
        );
        let failed_sites = if by_kind {
            counts.site_exempt += f.also_locations.len();
            Vec::new()
        } else {
            verify_sites(f, &mut sources, &mut counts)
        };
        let sites_checked = !by_kind
            && f.also_locations
                .iter()
                .any(|s| site_is_checkable(s, &f.repo));
        // A row exempt for its own snippet (none, or no cited line) is still checked when it
        // carries absorbed sites that have snippets of their own.
        if row_exempt.is_some() && !sites_checked {
            counts.exempt += 1;
            continue;
        }
        counts.checked += 1;
        let primary_why = if row_exempt.is_some() {
            None
        } else {
            primary_unverified_reason(f, &mut sources)
        };
        let why = primary_why
            .or_else(|| (!failed_sites.is_empty()).then(|| ABSORBED_SITE_REASON.to_string()));
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

/// Why the row's own snippet is unverified, or `None` when it is near its line.
fn primary_unverified_reason(f: &mut Finding, sources: &mut Sources<'_>) -> Option<String> {
    match sources.file(&f.path) {
        None => Some(format!(
            "the file {} was not found in the scanned tree",
            f.path
        )),
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

/// Carry the evidence marks of `occurrences` onto `captures`, the captures of the row built from
/// them (the structural group, whose own snippet is synthetic and cannot be checked). The check
/// runs on the occurrences before they are grouped; this puts what it found on the group so it is
/// held, and names each failing site: an occurrence whose own snippet failed is named by its
/// `path:line`, and the absorbed sites that failed are named as they were.
pub fn carry_marks_to_group(
    occurrences: &[Finding],
    captures: &mut std::collections::BTreeMap<String, String>,
) {
    let mut sites: Vec<String> = Vec::new();
    let mut add = |site: String| {
        if !sites.contains(&site) {
            sites.push(site);
        }
    };
    let mut held = false;
    for f in occurrences {
        let Some(reason) = f.captures.get(EVIDENCE_UNVERIFIED_CAPTURE) else {
            continue;
        };
        held = true;
        if reason != ABSORBED_SITE_REASON {
            add(format!("{}:{}", f.path, f.line));
        }
        if let Some(listed) = f.captures.get(EVIDENCE_UNVERIFIED_SITES_CAPTURE) {
            listed.lines().for_each(|l| add(l.to_string()));
        }
    }
    if held {
        captures.insert(
            EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
            GROUPED_OCCURRENCE_REASON.to_string(),
        );
        captures.insert(
            EVIDENCE_UNVERIFIED_SITES_CAPTURE.to_string(),
            sites.join("\n"),
        );
    }
}

/// [`verify_evidence`] over one repo's finished rows, with its counts added to the scan's ledger.
/// The one call the scan makes.
pub fn verify_and_record(
    ledger: &mut crate::scan_ledger::ScanLedger,
    findings: &mut [Finding],
    files: &[(String, String)],
    root: Option<&Path>,
) {
    let counts = verify_evidence(findings, files, root);
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
