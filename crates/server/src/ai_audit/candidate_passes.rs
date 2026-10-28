//! Token-efficiency rank 8: the candidate prefilter. A rule that names a deterministic locator
//! (`candidates` in its TOML, `camerata_rules::Candidates`) is sent, per chunk, only the sites
//! that locator finds (each with [`CANDIDATE_CONTEXT_LINES`] lines of context) plus the repo map,
//! instead of the chunk's whole files.
//!
//! # Where it sits
//!
//! Routing ([`super::runnable_units`]) decides which files a rule is sent; this module decides
//! what a chunk of those files LOOKS like to the rule. It turns each [`super::RoutedUnit`] into
//! the [`PassSet`]s that run, and both execution paths (real-time [`super::run_routed_passes`]
//! and batch [`super::run_routed_passes_batch`]) run exactly those, so they send the same
//! prompts.
//!
//! # The false-negative guard
//!
//! Narrowing is decided per (chunk, locator), and every doubt resolves to whole files:
//!
//! * the locator errors, panics, times out ([`CandidateEnv::timeout`]), is handed more text than
//!   [`LOCATOR_MAX_INPUT_BYTES`] or cannot parse a file of the chunk: the rule stays in the chunk's ordinary whole-file prompt (exactly the prompt it
//!   gets with the switch off) and the fallback is recorded in telemetry;
//! * a file the locator does not read (not its language, e.g. `supabase/config.toml` for a SQL
//!   locator) is never "no candidates": it is carried WHOLE inside the candidate prompt;
//! * when the sites plus the whole-carried files come to more than half the whole digest, the
//!   rule also stays in the whole-file prompt (a separate prompt would cost more than it saves);
//! * the locator found no site in a chunk it read completely: no pass is sent for the rule
//!   there, because a locator's filter is a superset of the rule's subject (see
//!   `camerata_checks::candidates`), so there is nothing for the rule to judge.
//!
//! The advisory "flag novel issues" pass never runs on a candidate prompt: it stays on the
//! whole-file prompt of its chunk. A chunk whose every rule was narrowed away still gets its
//! whole-file advisory pass when its unit runs advisory.
//!
//! # Switch off
//!
//! With `candidates_prefilter` off, or for a unit with no candidate rule, a unit is ONE pass set
//! identical to what rank 7 ran (same label, session prefix, chunks, rules, advisory), so the
//! prompts are byte-identical.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camerata_checks::candidates::{CandidateLocator, FileCandidates, LocateError};

use super::{build_digest, chunk_files, number_lines_from, RoutedUnit, UsageMeter};
use crate::scan_routing::AuditRule;
use crate::stage_telemetry::{CandidateFallback, CandidateOutcome};

/// A unit's `(id, directive)` pairs.
type RulePairs = Vec<(String, String)>;

/// Lines of context shown on each side of a candidate site.
pub(super) const CANDIDATE_CONTEXT_LINES: usize = 8;

/// A candidate prompt is used only when its code section is at most this share of the chunk's
/// whole digest (numerator, denominator). Above it, the rule stays in the whole-file prompt.
const MAX_CANDIDATE_SHARE: (usize, usize) = (1, 2);

/// How long one (chunk, locator) evaluation may take before the chunk falls back to whole files.
const LOCATOR_TIMEOUT: Duration = Duration::from_secs(20);

/// Most file text (bytes) one (chunk, locator) evaluation is handed. A chunk above it falls back
/// to whole files without running the locator at all, so no input can pin a thread for long.
const LOCATOR_MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// The locator a rule's `candidates` names, run over one file. The flag is raised when the scan
/// stops waiting (the time limit passed): a locator must poll it and return early, because a
/// blocking thread cannot be killed from outside.
pub(super) type LocateFn =
    fn(CandidateLocator, &str, &str, &AtomicBool) -> Result<FileCandidates, LocateError>;

/// The locator implementation and its time limit. Production is [`CandidateEnv::production`];
/// the tests substitute a failing or hanging locator to drive the fallback.
#[derive(Clone, Copy)]
pub(super) struct CandidateEnv {
    pub locate: LocateFn,
    pub timeout: Duration,
    /// Chunks with more file text than this are not located (see [`LOCATOR_MAX_INPUT_BYTES`]).
    pub max_input_bytes: usize,
}

impl CandidateEnv {
    /// The `camerata-checks` locators with the production time limit.
    pub(super) fn production() -> Self {
        Self {
            locate: camerata_checks::candidates::locate_until,
            timeout: LOCATOR_TIMEOUT,
            max_input_bytes: LOCATOR_MAX_INPUT_BYTES,
        }
    }
}

/// One `run_passes` / `run_passes_batch` call: a set of a unit's chunks, the rules sent with
/// them, and (for a candidate set) the candidate digests that replace the whole-file ones.
pub(super) struct PassSet {
    /// The group label the prompt carries (`pass[scoped]`, or `pass[scoped] candidates[...]`).
    pub label: String,
    /// The transcript session prefix (unique per set within a scan).
    pub session_prefix: String,
    /// Indices into the unit's chunk list ([`chunk_files`] of the unit's files).
    pub chunk_ids: Vec<usize>,
    /// One digest per `chunk_ids` entry: the candidate digests of a candidate set, or the
    /// already-built whole-file digests of a whole-file set in a unit that located candidates.
    /// `None` = the pass call builds the chunks' whole-file digests itself.
    pub digests: Option<Vec<String>>,
    /// The `(id, directive)` pairs, in plan order. Empty only for an advisory-only set.
    pub rules: Vec<(String, String)>,
    pub advisory_disabled: bool,
}

impl PassSet {
    /// The rule-batches this set sends with each chunk. An empty rule list (an advisory-only
    /// set) is one empty batch, which is how a pass carries no adopted rule.
    pub(super) fn batches(&self, batch_size: usize) -> Vec<&[(String, String)]> {
        if self.rules.is_empty() {
            return vec![&[]];
        }
        self.rules.chunks(batch_size.max(1)).collect()
    }

    /// Passes this set sends: chunks x rule-batches.
    pub(super) fn pass_count(&self, batch_size: usize) -> usize {
        self.chunk_ids.len() * self.batches(batch_size).len()
    }

    /// This set's chunks out of its unit's chunk list.
    pub(super) fn chunks_of<'a>(
        &self,
        unit_chunks: &[&'a [(String, String)]],
    ) -> Vec<&'a [(String, String)]> {
        self.chunk_ids.iter().map(|&i| unit_chunks[i]).collect()
    }
}

/// The set a unit runs when nothing is narrowed: exactly the rank-7 call.
fn whole_unit_set(unit: &RoutedUnit, repo: &str, chunk_count: usize) -> PassSet {
    PassSet {
        label: unit.label.clone(),
        session_prefix: format!("audit-{repo}-g{}", unit.index),
        chunk_ids: (0..chunk_count).collect(),
        digests: None,
        rules: unit.rules.clone(),
        advisory_disabled: unit.advisory_disabled,
    }
}

/// What one (chunk, locator) evaluation decided.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChunkVerdict {
    Narrowed(String),
    NoSites,
    NotNarrowed,
    Fallback(String),
}

impl ChunkVerdict {
    fn outcome(&self) -> CandidateOutcome {
        match self {
            Self::Narrowed(_) => CandidateOutcome::Narrowed,
            Self::NoSites => CandidateOutcome::NoSites,
            Self::NotNarrowed => CandidateOutcome::NotNarrowed,
            Self::Fallback(_) => CandidateOutcome::Fallback,
        }
    }

    /// Does the rule stay in the chunk's whole-file prompt?
    fn keeps_whole(&self) -> bool {
        matches!(self, Self::NotNarrowed | Self::Fallback(_))
    }
}

/// The pass sets for every unit, in unit order. `selected` carries each rule's `candidates`;
/// with `prefilter` off every unit is its single [`whole_unit_set`].
pub(super) async fn plan_pass_sets(
    units: &[RoutedUnit],
    selected: &[AuditRule],
    repo: &str,
    prefilter: bool,
    env: CandidateEnv,
    nonce: &crate::prompt_frame::FrameNonce,
    meter: Option<&UsageMeter>,
) -> Vec<Vec<PassSet>> {
    let locators: HashMap<&str, CandidateLocator> = if prefilter {
        selected
            .iter()
            .filter_map(|r| r.candidates.map(|c| (r.id.as_str(), c.locator)))
            .collect()
    } else {
        HashMap::new()
    };
    let mut out = Vec::with_capacity(units.len());
    for unit in units {
        let chunk_count = chunk_files(&unit.files, super::CHUNK_DIGEST_CHARS).len();
        let has_candidates = unit
            .rules
            .iter()
            .any(|(id, _)| locators.contains_key(id.as_str()));
        if !has_candidates {
            out.push(vec![whole_unit_set(unit, repo, chunk_count)]);
            continue;
        }
        out.push(plan_unit(unit, &locators, repo, env, nonce, meter).await);
    }
    out
}

/// The pass sets of one unit that has at least one candidate rule.
async fn plan_unit(
    unit: &RoutedUnit,
    locators: &HashMap<&str, CandidateLocator>,
    repo: &str,
    env: CandidateEnv,
    nonce: &crate::prompt_frame::FrameNonce,
    meter: Option<&UsageMeter>,
) -> Vec<PassSet> {
    let chunks = chunk_files(&unit.files, super::CHUNK_DIGEST_CHARS);
    let used: BTreeSet<CandidateLocator> = unit
        .rules
        .iter()
        .filter_map(|(id, _)| locators.get(id.as_str()).copied())
        .collect();
    // Each chunk's whole-file digest, built ONCE: its length is the baseline the saving is
    // measured against, and the whole-file sets reuse the text instead of building it again in
    // `run_passes`.
    let whole_digests: Vec<String> = chunks.iter().map(|c| build_digest(c, nonce)).collect();
    let whole_bytes: Vec<usize> = whole_digests.iter().map(String::len).collect();

    // Every (chunk, locator) evaluation runs at once, each on a blocking thread under the time
    // limit.
    let jobs = chunks.iter().enumerate().flat_map(|(ci, chunk)| {
        let whole = whole_bytes[ci];
        used.iter()
            .map(move |&l| async move { ((ci, l), evaluate(l, chunk, whole, env, nonce).await) })
    });
    let verdicts: BTreeMap<(usize, CandidateLocator), ChunkVerdict> =
        futures::future::join_all(jobs).await.into_iter().collect();

    for ((ci, l), verdict) in &verdicts {
        // A fallback is stated whether or not a meter is attached: a scan that quietly sent whole
        // files where it promised excerpts is a cost surprise nobody can find.
        if let ChunkVerdict::Fallback(reason) = verdict {
            eprintln!(
                "[camerata-server] candidate prefilter: {repo} {} chunk {} [{}] falls back to \
                 whole files: {reason}",
                unit.label,
                ci + 1,
                l.name()
            );
        }
        if let Some(m) = meter {
            let sent = match verdict {
                ChunkVerdict::Narrowed(d) => d.len(),
                ChunkVerdict::NoSites => 0,
                ChunkVerdict::NotNarrowed | ChunkVerdict::Fallback(_) => whole_bytes[*ci],
            };
            for (id, _) in unit
                .rules
                .iter()
                .filter(|(id, _)| locators.get(id.as_str()) == Some(l))
            {
                m.record_candidate(
                    id,
                    l.name(),
                    verdict.outcome(),
                    whole_bytes[*ci] as u64,
                    sent as u64,
                );
            }
            if let ChunkVerdict::Fallback(reason) = verdict {
                m.record_candidate_fallback(CandidateFallback {
                    repo: repo.to_string(),
                    group: unit.label.clone(),
                    chunk: ci + 1,
                    locator: l.name().to_string(),
                    reason: reason.clone(),
                });
            }
        }
    }

    // Whole-file sets: per chunk, the rules that stay whole (no locator, or a locator that did
    // not narrow this chunk), grouped so chunks with the same rule list share one call.
    // (the rules that stay whole, the chunks that share exactly that list)
    let mut whole_groups: Vec<(RulePairs, Vec<usize>)> = Vec::new();
    for ci in 0..chunks.len() {
        let rules: RulePairs = unit
            .rules
            .iter()
            .filter(|(id, _)| {
                locators
                    .get(id.as_str())
                    .is_none_or(|l| verdicts[&(ci, *l)].keeps_whole())
            })
            .cloned()
            .collect();
        // A chunk with no whole-file rule left still needs its advisory pass when the unit runs
        // advisory: that pass is the only one that reads whole files for novel issues.
        if rules.is_empty() && unit.advisory_disabled {
            continue;
        }
        match whole_groups.iter_mut().find(|(r, _)| *r == rules) {
            Some((_, ids)) => ids.push(ci),
            None => whole_groups.push((rules, vec![ci])),
        }
    }
    let mut sets: Vec<PassSet> = whole_groups
        .into_iter()
        .enumerate()
        .map(|(k, (rules, chunk_ids))| PassSet {
            label: unit.label.clone(),
            session_prefix: if k == 0 {
                format!("audit-{repo}-g{}", unit.index)
            } else {
                format!("audit-{repo}-g{}-w{k}", unit.index)
            },
            digests: Some(
                chunk_ids
                    .iter()
                    .map(|&ci| whole_digests[ci].clone())
                    .collect(),
            ),
            chunk_ids,
            rules,
            advisory_disabled: unit.advisory_disabled,
        })
        .collect();

    // Candidate sets: per locator, the chunks it narrowed, with that locator's rules.
    for (li, l) in used.iter().enumerate() {
        let (chunk_ids, digests): (Vec<usize>, Vec<String>) = (0..chunks.len())
            .filter_map(|ci| match &verdicts[&(ci, *l)] {
                ChunkVerdict::Narrowed(d) => Some((ci, d.clone())),
                _ => None,
            })
            .unzip();
        if chunk_ids.is_empty() {
            continue;
        }
        sets.push(PassSet {
            label: format!("{} candidates[{}]", unit.label, l.name()),
            session_prefix: format!("audit-{repo}-g{}-k{li}", unit.index),
            chunk_ids,
            digests: Some(digests),
            rules: unit
                .rules
                .iter()
                .filter(|(id, _)| locators.get(id.as_str()) == Some(l))
                .cloned()
                .collect(),
            // The advisory pass reads whole files; it never runs on excerpts.
            advisory_disabled: true,
        });
    }
    sets
}

/// Run `locator` over one chunk under the time limit and decide the chunk. A panic, a timeout or
/// an oversized chunk is a fallback, exactly like a locator error.
///
/// The work runs on a blocking thread, which cannot be killed, so two things keep a pathological
/// file from pinning the blocking pool: a chunk with more than [`CandidateEnv::max_input_bytes`]
/// of text is never handed to the locator, and when the time limit passes the cancel flag the
/// locator polls is raised, so the thread stops within a few thousand characters instead of
/// running to the end of the file.
async fn evaluate(
    locator: CandidateLocator,
    chunk: &[(String, String)],
    whole_bytes: usize,
    env: CandidateEnv,
    nonce: &crate::prompt_frame::FrameNonce,
) -> ChunkVerdict {
    let input_bytes: usize = chunk.iter().map(|(_, content)| content.len()).sum();
    if input_bytes > env.max_input_bytes {
        return ChunkVerdict::Fallback(format!(
            "the chunk holds {input_bytes} bytes of text, over the {} byte limit for a locator",
            env.max_input_bytes
        ));
    }
    let owned: Vec<(String, String)> = chunk.to_vec();
    let locate = env.locate;
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let nonce = nonce.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut found = Vec::with_capacity(owned.len());
        for (path, content) in &owned {
            if worker_cancel.load(Ordering::Relaxed) {
                return Err(LocateError("cancelled: the time limit passed".to_string()));
            }
            found.push(locate(locator, path, content, &worker_cancel)?);
        }
        Ok(decide(locator, &owned, &found, whole_bytes, &nonce))
    });
    match tokio::time::timeout(env.timeout, task).await {
        Ok(Ok(Ok(verdict))) => verdict,
        Ok(Ok(Err(e))) => ChunkVerdict::Fallback(e.to_string()),
        Ok(Err(join)) => ChunkVerdict::Fallback(format!("the locator failed: {join}")),
        Err(_) => {
            cancel.store(true, Ordering::Relaxed);
            ChunkVerdict::Fallback(format!(
                "the locator did not finish within {}s",
                env.timeout.as_secs_f64()
            ))
        }
    }
}

/// The verdict for a chunk every file of which the locator answered for.
fn decide(
    locator: CandidateLocator,
    files: &[(String, String)],
    found: &[FileCandidates],
    whole_bytes: usize,
    nonce: &crate::prompt_frame::FrameNonce,
) -> ChunkVerdict {
    let any_content = found.iter().any(|f| match f {
        FileCandidates::Sites(s) => !s.is_empty(),
        FileCandidates::NotRead => true,
    });
    if !any_content {
        return ChunkVerdict::NoSites;
    }
    let digest = candidate_digest(locator, files, found, nonce);
    let (num, den) = MAX_CANDIDATE_SHARE;
    if digest.len() * den > whole_bytes * num {
        return ChunkVerdict::NotNarrowed;
    }
    ChunkVerdict::Narrowed(digest)
}

/// The per-rule candidate section that replaces a chunk's digest: every site with
/// [`CANDIDATE_CONTEXT_LINES`] lines of context, numbered with the file's real line numbers, and
/// every file the locator does not read in full. Files with no site are omitted. Each file's
/// text sits in its own frame under the run's `nonce`, like a whole-file digest.
pub(super) fn candidate_digest(
    locator: CandidateLocator,
    files: &[(String, String)],
    found: &[FileCandidates],
    nonce: &crate::prompt_frame::FrameNonce,
) -> String {
    let mut out = format!(
        "// ===== CANDIDATE SITES (locator: {}) =====\n\
         // The rules below are checked against these excerpts instead of whole files. A \
         deterministic locator selected every statement in these files that can be the rules' \
         subject; each is shown with up to {CANDIDATE_CONTEXT_LINES} lines of context and the \
         file's real line numbers. Lines not shown hold no candidate site, and a file not listed \
         holds none. A file the locator does not read is shown in full.\n\n",
        locator.name()
    );
    for ((path, content), found) in files.iter().zip(found) {
        match found {
            FileCandidates::NotRead => {
                out.push_str(&crate::prompt_frame::frame_untrusted(
                    nonce,
                    path,
                    &number_lines_from(content.lines(), 1),
                ));
                out.push('\n');
            }
            FileCandidates::Sites(sites) if !sites.is_empty() => {
                let lines: Vec<&str> = content.lines().collect();
                let mut body = String::new();
                let ranges = merged_ranges(
                    sites.iter().map(|s| (s.start_line, s.end_line)),
                    lines.len(),
                );
                let mut shown_to = 0usize;
                for (start, end) in ranges {
                    if start > shown_to + 1 {
                        body.push_str(&format!(
                            "     ... (lines {}-{} not shown)\n",
                            shown_to + 1,
                            start - 1
                        ));
                    }
                    body.push_str(&number_lines_from(
                        lines[start - 1..end].iter().copied(),
                        start,
                    ));
                    shown_to = end;
                }
                if shown_to < lines.len() {
                    body.push_str(&format!(
                        "     ... (lines {}-{} not shown)\n",
                        shown_to + 1,
                        lines.len()
                    ));
                }
                out.push_str(&crate::prompt_frame::frame_untrusted(nonce, path, &body));
                out.push('\n');
            }
            FileCandidates::Sites(_) => {}
        }
    }
    out
}

/// Sites widened by the context and clamped to `1..=total`, sorted, with overlapping or touching
/// ranges merged. Empty when the file has no line.
fn merged_ranges(sites: impl Iterator<Item = (usize, usize)>, total: usize) -> Vec<(usize, usize)> {
    if total == 0 {
        return Vec::new();
    }
    let mut ranges: Vec<(usize, usize)> = sites
        .map(|(s, e)| {
            let start = s.saturating_sub(CANDIDATE_CONTEXT_LINES).clamp(1, total);
            let end = e
                .max(s)
                .saturating_add(CANDIDATE_CONTEXT_LINES)
                .clamp(start, total);
            (start, end)
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if s <= *last_end + 1 => *last_end = (*last_end).max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use camerata_checks::candidates::SiteRange;

    #[test]
    fn ranges_take_context_clamp_and_merge() {
        assert_eq!(merged_ranges([(10, 12)].into_iter(), 100), vec![(2, 20)]);
        assert_eq!(merged_ranges([(1, 1)].into_iter(), 5), vec![(1, 5)]);
        // 10..12 +- 8 = 2..20 and 29..30 +- 8 = 21..38 touch, so they merge.
        assert_eq!(
            merged_ranges([(29, 30), (10, 12)].into_iter(), 100),
            vec![(2, 38)]
        );
        assert_eq!(
            merged_ranges([(10, 10), (40, 41)].into_iter(), 100),
            vec![(2, 18), (32, 49)]
        );
        assert!(merged_ranges([(1, 1)].into_iter(), 0).is_empty());
        // A site past the end (a stale line) is clamped, never a panic.
        assert_eq!(merged_ranges([(90, 95)].into_iter(), 10), vec![(10, 10)]);
    }

    #[test]
    fn the_digest_shows_sites_with_real_line_numbers_and_whole_unread_files() {
        let sql: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let files = vec![
            ("m.sql".to_string(), sql),
            ("none.sql".to_string(), "select 1;\n".to_string()),
            (
                "supabase/config.toml".to_string(),
                "[api]\nschemas = [\"public\"]\n".to_string(),
            ),
        ];
        let found = vec![
            FileCandidates::Sites(vec![SiteRange {
                start_line: 20,
                end_line: 21,
            }]),
            FileCandidates::Sites(Vec::new()),
            FileCandidates::NotRead,
        ];
        let nonce = crate::prompt_frame::FrameNonce::fixed("tok");
        let d = candidate_digest(CandidateLocator::SqlRoutines, &files, &found, &nonce);
        assert!(d.starts_with("// ===== CANDIDATE SITES (locator: sql_routines) ====="));
        assert!(d.contains(
            "<<<UNTRUSTED tok \"m.sql\">>>\n     ... (lines 1-11 not shown)\n  12| line 12\n"
        ));
        assert!(d.contains(
            "  29| line 29\n     ... (lines 30-40 not shown)\n<<<END UNTRUSTED tok>>>\n"
        ));
        assert!(!d.contains("line 11\n") && !d.contains("line 30\n"));
        assert!(!d.contains("none.sql"), "a file with no site is not listed");
        assert!(
            d.contains("<<<UNTRUSTED tok \"supabase/config.toml\">>>\n   1| [api]\n   2| schemas")
        );
        assert!(
            !d.contains("// ===== FILE:"),
            "no forgeable file header is left"
        );
        assert!(!d.contains('\u{2014}'), "no em-dashes in prompt text");
    }
}
