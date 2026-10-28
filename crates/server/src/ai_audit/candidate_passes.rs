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
//! * the locator errors, panics, times out ([`CandidateEnv::timeout`]) or cannot parse a file of
//!   the chunk: the rule stays in the chunk's ordinary whole-file prompt (exactly the prompt it
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

/// The locator a rule's `candidates` names, run over one file.
pub(super) type LocateFn = fn(CandidateLocator, &str, &str) -> Result<FileCandidates, LocateError>;

/// The locator implementation and its time limit. Production is [`CandidateEnv::production`];
/// the tests substitute a failing or hanging locator to drive the fallback.
#[derive(Clone, Copy)]
pub(super) struct CandidateEnv {
    pub locate: LocateFn,
    pub timeout: Duration,
}

impl CandidateEnv {
    /// The `camerata-checks` locators with the production time limit.
    pub(super) fn production() -> Self {
        Self {
            locate: camerata_checks::candidates::locate,
            timeout: LOCATOR_TIMEOUT,
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
    /// One candidate digest per `chunk_ids` entry; `None` = the chunks' whole-file digests.
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
        out.push(plan_unit(unit, &locators, repo, env, meter).await);
    }
    out
}

/// The pass sets of one unit that has at least one candidate rule.
async fn plan_unit(
    unit: &RoutedUnit,
    locators: &HashMap<&str, CandidateLocator>,
    repo: &str,
    env: CandidateEnv,
    meter: Option<&UsageMeter>,
) -> Vec<PassSet> {
    let chunks = chunk_files(&unit.files, super::CHUNK_DIGEST_CHARS);
    let used: BTreeSet<CandidateLocator> = unit
        .rules
        .iter()
        .filter_map(|(id, _)| locators.get(id.as_str()).copied())
        .collect();
    let whole_bytes: Vec<usize> = chunks.iter().map(|c| build_digest(c).len()).collect();

    // Every (chunk, locator) evaluation runs at once, each on a blocking thread under the time
    // limit.
    let jobs = chunks.iter().enumerate().flat_map(|(ci, chunk)| {
        let whole = whole_bytes[ci];
        used.iter()
            .map(move |&l| async move { ((ci, l), evaluate(l, chunk, whole, env).await) })
    });
    let verdicts: BTreeMap<(usize, CandidateLocator), ChunkVerdict> =
        futures::future::join_all(jobs).await.into_iter().collect();

    if let Some(m) = meter {
        for ((ci, l), verdict) in &verdicts {
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
                eprintln!(
                    "[camerata-server] candidate prefilter: {repo} {} chunk {} [{}] falls back to \
                     whole files: {reason}",
                    unit.label,
                    ci + 1,
                    l.name()
                );
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
            chunk_ids,
            digests: None,
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

/// Run `locator` over one chunk under the time limit and decide the chunk. A panic or a timeout
/// is a fallback, exactly like a locator error.
async fn evaluate(
    locator: CandidateLocator,
    chunk: &[(String, String)],
    whole_bytes: usize,
    env: CandidateEnv,
) -> ChunkVerdict {
    let owned: Vec<(String, String)> = chunk.to_vec();
    let locate = env.locate;
    let task = tokio::task::spawn_blocking(move || {
        let located: Result<Vec<FileCandidates>, LocateError> = owned
            .iter()
            .map(|(path, content)| locate(locator, path, content))
            .collect();
        located.map(|found| decide(locator, &owned, &found, whole_bytes))
    });
    match tokio::time::timeout(env.timeout, task).await {
        Ok(Ok(Ok(verdict))) => verdict,
        Ok(Ok(Err(e))) => ChunkVerdict::Fallback(e.to_string()),
        Ok(Err(join)) => ChunkVerdict::Fallback(format!("the locator failed: {join}")),
        Err(_) => ChunkVerdict::Fallback(format!(
            "the locator did not finish within {}s",
            env.timeout.as_secs_f64()
        )),
    }
}

/// The verdict for a chunk every file of which the locator answered for.
fn decide(
    locator: CandidateLocator,
    files: &[(String, String)],
    found: &[FileCandidates],
    whole_bytes: usize,
) -> ChunkVerdict {
    let any_content = found.iter().any(|f| match f {
        FileCandidates::Sites(s) => !s.is_empty(),
        FileCandidates::NotRead => true,
    });
    if !any_content {
        return ChunkVerdict::NoSites;
    }
    let digest = candidate_digest(locator, files, found);
    let (num, den) = MAX_CANDIDATE_SHARE;
    if digest.len() * den > whole_bytes * num {
        return ChunkVerdict::NotNarrowed;
    }
    ChunkVerdict::Narrowed(digest)
}

/// The per-rule candidate section that replaces a chunk's digest: every site with
/// [`CANDIDATE_CONTEXT_LINES`] lines of context, numbered with the file's real line numbers, and
/// every file the locator does not read in full. Files with no site are omitted.
pub(super) fn candidate_digest(
    locator: CandidateLocator,
    files: &[(String, String)],
    found: &[FileCandidates],
) -> String {
    let mut out = format!(
        "// ===== CANDIDATE SITES (locator: {}) =====\n\
         // The rules below are checked against these excerpts instead of whole files. A \
         deterministic locator selected every statement in these files that can be the rules' \
         subject; each is shown with up to {CANDIDATE_CONTEXT_LINES} lines of context and the \
         file's real line numbers. Lines not shown hold no candidate site, and a file not listed \
         holds none. A file marked (whole) is one the locator does not read, so it is shown in \
         full.\n\n",
        locator.name()
    );
    for ((path, content), found) in files.iter().zip(found) {
        match found {
            FileCandidates::NotRead => {
                out.push_str(&format!("// ===== FILE: {path} (whole) =====\n"));
                out.push_str(&number_lines_from(content.lines(), 1));
                out.push('\n');
            }
            FileCandidates::Sites(sites) if !sites.is_empty() => {
                let lines: Vec<&str> = content.lines().collect();
                out.push_str(&format!("// ===== FILE: {path} (excerpts) =====\n"));
                let ranges = merged_ranges(
                    sites.iter().map(|s| (s.start_line, s.end_line)),
                    lines.len(),
                );
                let mut shown_to = 0usize;
                for (start, end) in ranges {
                    if start > shown_to + 1 {
                        out.push_str(&format!(
                            "     ... (lines {}-{} not shown)\n",
                            shown_to + 1,
                            start - 1
                        ));
                    }
                    out.push_str(&number_lines_from(
                        lines[start - 1..end].iter().copied(),
                        start,
                    ));
                    shown_to = end;
                }
                if shown_to < lines.len() {
                    out.push_str(&format!(
                        "     ... (lines {}-{} not shown)\n",
                        shown_to + 1,
                        lines.len()
                    ));
                }
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
        let d = candidate_digest(CandidateLocator::SqlRoutines, &files, &found);
        assert!(d.starts_with("// ===== CANDIDATE SITES (locator: sql_routines) ====="));
        assert!(d.contains("// ===== FILE: m.sql (excerpts) =====\n     ... (lines 1-11 not shown)\n  12| line 12\n"));
        assert!(d.contains("  29| line 29\n     ... (lines 30-40 not shown)\n"));
        assert!(!d.contains("line 11\n") && !d.contains("line 30\n"));
        assert!(!d.contains("none.sql"), "a file with no site is not listed");
        assert!(d.contains(
            "// ===== FILE: supabase/config.toml (whole) =====\n   1| [api]\n   2| schemas"
        ));
        assert!(!d.contains('\u{2014}'), "no em-dashes in prompt text");
    }
}
