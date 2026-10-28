//! Phase 3: verification and maximal extension (spec section 4).
//!
//! A candidate pair says "these two K-grams hash alike". This phase re-tokenizes both files,
//! rejects a hash collision by comparing the tokens, and extends the match left and right while
//! the tokens stay equal and both cursors stay inside their items. What comes out is a MAXIMAL
//! matched pair, which is the only thing a family is built from.
//!
//! Candidates arrive in rounds. A [`Verifier`] keeps what one round learned (which stretches of
//! which file pairs are already matched, and the tokenized files each worker holds) for the
//! next, so a clone region met again in a later round is recognized without being extended again.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::cache::{Source, TokenCache, MAX_BYTES};
use super::index::{is_data_heavy, DataIds, Fingerprint, MatchedPair, RawSpan};
use super::{Token, K};

/// Fewest distinct token kinds in a shared sequence (spec section 3): a window of
/// `$ID , $ID , $ID` has two kinds, and no rule wants it.
const MIN_DISTINCT_KINDS: usize = 8;

/// Stack for each verification worker thread (see the same constant in `index`).
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Key of the diagonal a candidate lies on: both files and the offset between the two cursors.
type Diagonal = (u32, u32, i64);

/// The stretches of file A (in file-wide token coordinates) already matched on each diagonal. A
/// later fingerprint inside one is the same maximal pair, so it is skipped, not extended again.
type Covered = HashMap<Diagonal, Vec<(u32, u32)>>;

/// What verifying one group of candidates (all sharing their first file) produced.
#[derive(Default)]
struct GroupOutcome {
    pairs: Vec<MatchedPair>,
    covered: Covered,
    /// Candidates that reached an extension attempt (not skipped as already matched).
    performed: usize,
}

/// Verifies rounds of candidate pairs and accumulates the maximal pairs.
pub(super) struct Verifier<'a> {
    sources: &'a [Option<Source<'a>>],
    min_tokens: usize,
    /// One token cache per worker; the 64 MiB budget is split among them.
    caches: Vec<TokenCache>,
    /// The matched stretches found so far, by the first file of the pairs they belong to.
    covered: HashMap<u32, Covered>,
    pairs: Vec<MatchedPair>,
    performed: usize,
}

impl<'a> Verifier<'a> {
    pub(super) fn new(sources: &'a [Option<Source<'a>>], min_tokens: usize) -> Self {
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        Self {
            sources,
            min_tokens,
            caches: (0..threads)
                .map(|_| TokenCache::with_budget(MAX_BYTES / threads))
                .collect(),
            covered: HashMap::new(),
            pairs: Vec::new(),
            performed: 0,
        }
    }

    /// How many candidates have been extended so far (the quantity `max_pairs` bounds).
    pub(super) fn performed(&self) -> usize {
        self.performed
    }

    /// Whether any of `candidates` is outside every region matched so far, so verifying it would
    /// cost an extension. Reads only the matched regions; tokenizes nothing.
    pub(super) fn any_uncovered(&self, candidates: &[(Fingerprint, Fingerprint)]) -> bool {
        candidates
            .iter()
            .any(|(x, y)| !is_covered(self.covered.get(&x.file), x, y))
    }

    /// The maximal pairs found, sorted and de-duplicated.
    pub(super) fn finish(mut self) -> Vec<MatchedPair> {
        self.pairs.sort_unstable();
        self.pairs.dedup();
        self.pairs
    }

    /// Verify one round. `candidates` is sorted by `(first file, second file, ..)`. Candidates
    /// are verified in groups that share their first file, in parallel: a group's work reads
    /// only its own bookkeeping, and the pairs are sorted at the end, so the number of workers
    /// never shows in the output.
    pub(super) fn run_round(&mut self, candidates: &[(Fingerprint, Fingerprint)]) {
        let groups = groups_by_first_file(candidates);
        let jobs: Vec<Mutex<Option<Covered>>> = groups
            .iter()
            .map(|group| {
                let file = candidates[group.start].0.file;
                Mutex::new(Some(self.covered.remove(&file).unwrap_or_default()))
            })
            .collect();
        let next = AtomicUsize::new(0);
        let (sources, min_tokens) = (self.sources, self.min_tokens);
        let work = |cache: TokenCache| {
            let mut worker = Worker {
                cache,
                data: DataIds::new(),
            };
            let mut done: Vec<(usize, GroupOutcome)> = Vec::new();
            loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let (Some(group), Some(job)) = (groups.get(at), jobs.get(at)) else {
                    break;
                };
                let covered = job
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take())
                    .unwrap_or_default();
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    worker.verify_group(&candidates[group.clone()], covered, sources, min_tokens)
                }));
                done.push((at, outcome.unwrap_or_default()));
            }
            (worker.cache, done)
        };

        let mut outcomes: Vec<(usize, GroupOutcome)> = Vec::new();
        let caches = std::mem::take(&mut self.caches);
        let workers = caches.len().min(groups.len()).max(1);
        let mut spare = caches;
        if workers == 1 {
            let cache = spare
                .pop()
                .unwrap_or_else(|| TokenCache::with_budget(MAX_BYTES));
            let (cache, done) = work(cache);
            spare.push(cache);
            outcomes.extend(done);
        } else {
            let lent: Vec<TokenCache> = spare.drain(..workers).collect();
            std::thread::scope(|scope| {
                let handles: Vec<_> = lent
                    .into_iter()
                    .filter_map(|cache| {
                        std::thread::Builder::new()
                            .stack_size(WORKER_STACK_BYTES)
                            .spawn_scoped(scope, || work(cache))
                            .ok()
                    })
                    .collect();
                for handle in handles {
                    if let Ok((cache, done)) = handle.join() {
                        spare.push(cache);
                        outcomes.extend(done);
                    }
                }
            });
            // If no worker thread could be spawned, the groups are still owed.
            if next.load(Ordering::Relaxed) == 0 {
                let (cache, done) = work(TokenCache::with_budget(MAX_BYTES / workers));
                spare.push(cache);
                outcomes.extend(done);
            }
        }
        self.caches = spare;
        // A worker that panicked while holding its cache lost it; keep the pool full.
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        while self.caches.len() < threads {
            self.caches
                .push(TokenCache::with_budget(MAX_BYTES / threads));
        }

        outcomes.sort_by_key(|(at, _)| *at);
        for (at, outcome) in outcomes {
            self.performed += outcome.performed;
            self.pairs.extend(outcome.pairs);
            self.covered
                .insert(candidates[groups[at].start].0.file, outcome.covered);
        }
    }
}

fn diagonal_of(x: &Fingerprint, y: &Fingerprint) -> Diagonal {
    (
        x.file,
        y.file,
        i64::from(x.position) - i64::from(y.position),
    )
}

/// Whether the K-gram at `x`, paired with the one at `y`, lies inside a region already matched.
fn is_covered(covered: Option<&Covered>, x: &Fingerprint, y: &Fingerprint) -> bool {
    covered
        .and_then(|regions| regions.get(&diagonal_of(x, y)))
        .is_some_and(|ranges| {
            ranges
                .iter()
                .any(|&(lo, hi)| lo <= x.position && x.position as usize + K <= hi as usize)
        })
}

/// The index ranges of `candidates` that share a first file.
fn groups_by_first_file(candidates: &[(Fingerprint, Fingerprint)]) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0;
    for (at, (first, _)) in candidates.iter().enumerate() {
        if first.file != candidates[start].0.file {
            groups.push(start..at);
            start = at;
        }
    }
    if start < candidates.len() {
        groups.push(start..candidates.len());
    }
    groups
}

/// One verification thread: its own token cache and the tables it reads.
struct Worker {
    cache: TokenCache,
    data: DataIds,
}

impl Worker {
    fn verify_group(
        &mut self,
        group: &[(Fingerprint, Fingerprint)],
        mut covered: Covered,
        sources: &[Option<Source<'_>>],
        min_tokens: usize,
    ) -> GroupOutcome {
        let mut outcome = GroupOutcome::default();
        for (x, y) in group {
            if is_covered(Some(&covered), x, y) {
                continue;
            }
            let diagonal = diagonal_of(x, y);
            let Some((a_source, b_source)) = source_pair(sources, x.file, y.file) else {
                continue;
            };
            let (Some(a_file), Some(b_file)) = (
                self.cache.get(x.file, a_source),
                self.cache.get(y.file, b_source),
            ) else {
                continue;
            };
            let (Some((item_a, at_a)), Some((item_b, at_b))) =
                (a_file.locate(x.position), b_file.locate(y.position))
            else {
                continue;
            };
            outcome.performed += 1;
            let (a_item, b_item) = (&a_file.items[item_a], &b_file.items[item_b]);
            let same_item = x.file == y.file && item_a == item_b;
            let Some(found) = extend(&a_item.tokens, &b_item.tokens, at_a, at_b, same_item) else {
                continue;
            };
            // The matched range in file-wide coordinates, so a later fingerprint inside it is
            // recognized without tokenizing anything.
            let file_start = x.position as usize - at_a + found.a_start;
            covered
                .entry(diagonal)
                .or_default()
                .push((file_start as u32, (file_start + found.len) as u32));
            if found.len < min_tokens {
                continue;
            }
            let a_tokens = &a_item.tokens[found.a_start..found.a_start + found.len];
            let b_tokens = &b_item.tokens[found.b_start..found.b_start + found.len];
            if distinct_kinds(a_tokens) < MIN_DISTINCT_KINDS {
                continue;
            }
            let data_count = a_tokens.iter().filter(|t| self.data.is_data(t)).count();
            if is_data_heavy(data_count, found.len) {
                continue;
            }
            outcome.pairs.push(MatchedPair {
                key: family_key(a_tokens),
                len: found.len as u32,
                a: span(x.file, item_a as u32, found.a_start, a_tokens),
                b: span(y.file, item_b as u32, found.b_start, b_tokens),
            });
        }
        outcome.covered = covered;
        outcome
    }
}

fn source_pair<'s, 'a>(
    sources: &'s [Option<Source<'a>>],
    a: u32,
    b: u32,
) -> Option<(&'s Source<'a>, &'s Source<'a>)> {
    let a = sources.get(a as usize)?.as_ref()?;
    let b = sources.get(b as usize)?.as_ref()?;
    Some((a, b))
}

/// A verified, maximal, non-overlapping match between two token slices.
pub(super) struct Extension {
    pub(super) a_start: usize,
    pub(super) b_start: usize,
    pub(super) len: usize,
}

/// Whether two tokens are the same normalized token.
fn same(a: &Token, b: &Token) -> bool {
    a.id == b.id && a.class == b.class
}

/// Verify the K-grams at token `ta` of `a` and `tb` of `b` really are equal, then extend the
/// match as far as it goes. `same_item` says `a` and `b` are one item, whose two matched spans
/// must not overlap. `None` when the hashes collided or the extension is empty.
pub(super) fn extend(
    a: &[Token],
    b: &[Token],
    ta: usize,
    tb: usize,
    same_item: bool,
) -> Option<Extension> {
    if ta + K > a.len() || tb + K > b.len() {
        return None;
    }
    if !(0..K).all(|k| same(&a[ta + k], &b[tb + k])) {
        return None;
    }
    let (mut lo_a, mut lo_b) = (ta, tb);
    while lo_a > 0 && lo_b > 0 && same(&a[lo_a - 1], &b[lo_b - 1]) {
        lo_a -= 1;
        lo_b -= 1;
    }
    let (mut hi_a, mut hi_b) = (ta + K, tb + K);
    while hi_a < a.len() && hi_b < b.len() && same(&a[hi_a], &b[hi_b]) {
        hi_a += 1;
        hi_b += 1;
    }
    let mut len = hi_a - lo_a;
    // Two spans of one item must not overlap each other. A periodic sequence matches itself
    // shifted by one period; the match is trimmed to one period (the spec discards it; keeping
    // the non-overlapping part finds a repeated block the spec would miss, and the claim, two
    // identical non-overlapping sequences, stays true).
    if same_item && lo_a < lo_b {
        len = len.min(lo_b - lo_a);
    }
    if len == 0 {
        return None;
    }
    Some(Extension {
        a_start: lo_a,
        b_start: lo_b,
        len,
    })
}

fn distinct_kinds(tokens: &[Token]) -> usize {
    tokens.iter().map(|t| t.id).collect::<HashSet<u32>>().len()
}

/// FNV-1a (64-bit) of the token ids, in order.
fn family_key(tokens: &[Token]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for token in tokens {
        for byte in token.id.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn span(file: u32, item: u32, start: usize, tokens: &[Token]) -> RawSpan {
    RawSpan {
        file,
        item,
        start: start as u32,
        len: tokens.len() as u32,
        start_line: tokens.first().map_or(0, |t| t.line),
        end_line: tokens.last().map_or(0, |t| t.line),
    }
}
