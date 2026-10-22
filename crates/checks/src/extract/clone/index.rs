//! [`detect`]: phases 1 and 2 of the pipeline (streaming fingerprints, buckets), and the driver
//! that runs phases 3 and 4 ([`super::verify`], [`super::families`]).
//!
//! Spec section 4. Winnowing (Schleimer, Wilkerson and Aiken, 2003) keeps the minimum hash of
//! every window of `W = T - K + 1` consecutive K-gram hashes, so any two spans that share `T`
//! consecutive tokens share at least one kept fingerprint, while only about `2 / (W + 1)` of the
//! K-grams are kept.

use std::collections::{HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::cache::Source;
use super::families::build_families;
use super::skip::{classify_content, classify_path, classify_tokens, LangFamily};
use super::verify::Verifier;
use super::{
    token_id, tokenize, CloneIndex, CloneOptions, SkipReason, Token, TokenClass, TokenMode, K,
};
use crate::extract::paths::emitted_js_paths;
use crate::extract::SourceLang;

/// Bytes one fingerprint costs in the index (spec section 4).
const FINGERPRINT_BYTES: usize = 16;

/// Multiplier of the polynomial rolling hash: odd, so it is invertible modulo 2^64.
const BASE: u64 = 0x9e37_79b9_7f4a_7c15;

/// A window whose tokens are at least this many tenths data (strings, numbers, literals and the
/// punctuation of a table) is a table, not logic, and is not fingerprinted (D10).
const DATA_TENTHS: usize = 6;

/// Stack for each phase-1 worker thread.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Files scanned per parallel batch. A batch bounds how many fingerprints exist beyond the cap.
const BATCH: usize = 256;

/// The most candidate pairs produced for one verification round (a bucket is never split, so a
/// round can run over by one bucket's pairs).
const ROUND_CANDIDATES: usize = 100_000;

/// One kept fingerprint: where a winnowed K-gram hash occurs. Exactly 16 bytes (spec section 4):
/// the K-gram's place in its file is one token offset across all the file's items, not an
/// `(item, index)` pair, and phase 3 turns it back into the pair once the file is tokenized
/// again. Ordered so a sort groups equal hashes and is total (no two fingerprints compare
/// equal), which makes every later step independent of the order the files arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Fingerprint {
    pub(super) hash: u64,
    pub(super) file: u32,
    /// Token offset of the K-gram from the start of the file's first item's stream.
    pub(super) position: u32,
}

/// One occurrence of a matched sequence, with the lines of its first and last token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct RawSpan {
    pub(super) file: u32,
    pub(super) item: u32,
    pub(super) start: u32,
    pub(super) len: u32,
    pub(super) start_line: u32,
    pub(super) end_line: u32,
}

/// A maximal pair of equal Structural sequences (phase 3's output).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct MatchedPair {
    pub(super) key: u64,
    pub(super) len: u32,
    pub(super) a: RawSpan,
    pub(super) b: RawSpan,
}

/// What phase 1 learned about one file.
enum Scan {
    Skipped(SkipReason),
    Indexed {
        lang: SourceLang,
        prints: Vec<Fingerprint>,
    },
}

/// The hash parameters derived from [`CloneOptions`].
#[derive(Debug, Clone, Copy)]
pub(super) struct HashParams {
    /// The winnowing guarantee length `T`, at least `K`.
    pub(super) min_tokens: usize,
    /// `W = T - K + 1`.
    pub(super) window: usize,
}

impl HashParams {
    pub(super) fn new(opts: &CloneOptions) -> Self {
        let min_tokens = (opts.min_tokens as usize).max(K);
        Self {
            min_tokens,
            window: min_tokens - K + 1,
        }
    }
}

/// Find the clone families across `files` (`(path, content)` pairs).
///
/// Pure and deterministic: the result depends only on the multiset of `(path, content)` pairs,
/// never on their order, the number of CPUs, or the clock. Never panics; a file that cannot be
/// tokenized is skipped with [`SkipReason::ParseFailed`].
pub fn detect(files: &[(String, String)], opts: &CloneOptions) -> CloneIndex {
    // Path order, with content as the tiebreak, so a shuffled input indexes identically.
    let mut inputs: Vec<&(String, String)> = files.iter().collect();
    inputs.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
    inputs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

    let emitted = emitted_js_paths(files);
    let params = HashParams::new(opts);

    let mut index = CloneIndex::default();
    let mut fingerprints: Vec<Fingerprint> = Vec::new();
    let mut sources: Vec<Option<Source<'_>>> = Vec::with_capacity(inputs.len());
    let mut families_of_file: Vec<Option<LangFamily>> = Vec::with_capacity(inputs.len());

    'phase1: for (batch_no, chunk) in inputs.chunks(BATCH).enumerate() {
        let base = batch_no * BATCH;
        let scans = scan_batch(chunk, base, &emitted, params);
        for (offset, scan) in scans.into_iter().enumerate() {
            let (path, content) = (&chunk[offset].0, &chunk[offset].1);
            match scan {
                Scan::Skipped(reason) => {
                    index.skipped.push((path.clone(), reason));
                    sources.push(None);
                    families_of_file.push(None);
                }
                Scan::Indexed { lang, prints } => {
                    let bytes = (fingerprints.len() + prints.len()) * FINGERPRINT_BYTES;
                    if bytes > opts.max_index_bytes {
                        index.truncated = true;
                        break 'phase1;
                    }
                    fingerprints.extend(prints);
                    index.files_read += 1;
                    sources.push(Some(Source {
                        lang,
                        content: content.as_str(),
                    }));
                    families_of_file.push(Some(LangFamily::of(lang)));
                }
            }
        }
    }

    fingerprints.sort_unstable();
    let max_pairs = opts.max_pairs as usize;
    let mut buckets = Buckets::new(&fingerprints, opts.max_members as usize);
    let mut verifier = Verifier::new(&sources, params.min_tokens);
    // Candidates are produced and verified in rounds, so the pair cap bounds the extensions
    // actually attempted (a fingerprint inside an already-matched region costs nothing) and the
    // candidates never all exist at once. A round is at most `ROUND_CANDIDATES`, and no more
    // than the cap itself, so a small cap is honored at about its own size.
    let round_size = max_pairs.clamp(1, ROUND_CANDIDATES);
    loop {
        let round = buckets.next_round(round_size);
        if round.is_empty() {
            break;
        }
        if verifier.performed() >= max_pairs {
            // The cap is spent. It only cut the work short if a candidate is left that is not
            // inside a region already matched: those cost nothing and are skipped.
            if verifier.any_uncovered(&round) {
                index.truncated = true;
                break;
            }
            continue;
        }
        verifier.run_round(&round);
    }
    index.boilerplate_buckets_skipped = buckets.boilerplate_skipped();
    let pairs = verifier.finish();
    drop(fingerprints);
    let paths: Vec<&str> = inputs.iter().map(|(p, _)| p.as_str()).collect();
    index.families = build_families(pairs, &paths, &families_of_file);
    index
}

/// Phase 1 for one batch of files, in parallel. The result is in input order whatever the
/// scheduling, so the thread count never shows in the output.
fn scan_batch(
    chunk: &[&(String, String)],
    base: usize,
    emitted: &HashSet<&str>,
    params: HashParams,
) -> Vec<Scan> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(chunk.len())
        .max(1);
    if threads == 1 {
        return chunk
            .iter()
            .enumerate()
            .map(|(i, input)| scan_guarded(base + i, input, emitted, params))
            .collect();
    }
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, Scan)> = Vec::with_capacity(chunk.len());
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .filter_map(|_| {
                // A tokenizer on a hostile file can be deep; give each worker a stack that is
                // not the platform's small default for spawned threads.
                std::thread::Builder::new()
                    .stack_size(WORKER_STACK_BYTES)
                    .spawn_scoped(scope, || {
                        let mut local = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(input) = chunk.get(i) else {
                                break;
                            };
                            local.push((i, scan_guarded(base + i, input, emitted, params)));
                        }
                        local
                    })
                    .ok()
            })
            .collect();
        for worker in workers {
            if let Ok(local) = worker.join() {
                done.extend(local);
            }
        }
    });
    let mut by_index: Vec<Option<Scan>> = (0..chunk.len()).map(|_| None).collect();
    for (i, scan) in done {
        by_index[i] = Some(scan);
    }
    // A file no worker reported (no thread could be spawned) is scanned here, so the thread
    // count can never change which files are indexed.
    by_index
        .into_iter()
        .enumerate()
        .map(|(i, scan)| scan.unwrap_or_else(|| scan_guarded(base + i, chunk[i], emitted, params)))
        .collect()
}

/// [`scan_file`], with a panic anywhere in a parser turned into a `ParseFailed` skip: this
/// module promises never to panic, and a grammar bug on one hostile file must not take the scan
/// down.
fn scan_guarded(
    file: usize,
    input: &(String, String),
    emitted: &HashSet<&str>,
    params: HashParams,
) -> Scan {
    catch_unwind(AssertUnwindSafe(|| scan_file(file, input, emitted, params)))
        .unwrap_or(Scan::Skipped(SkipReason::ParseFailed))
}

fn scan_file(
    file: usize,
    input: &(String, String),
    emitted: &HashSet<&str>,
    params: HashParams,
) -> Scan {
    let (path, content) = (input.0.as_str(), input.1.as_str());
    let lang = match classify_path(path, emitted) {
        Ok(lang) => lang,
        Err(reason) => return Scan::Skipped(reason),
    };
    if let Some(reason) = classify_content(content) {
        return Scan::Skipped(reason);
    }
    let Some(items) = tokenize(lang, content, TokenMode::Structural) else {
        return Scan::Skipped(SkipReason::ParseFailed);
    };
    if let Some(reason) = classify_tokens(content, &items) {
        return Scan::Skipped(reason);
    }
    let file = u32::try_from(file).unwrap_or(u32::MAX);
    let salt = LangFamily::of(lang).salt();
    let data = DataIds::new();
    let mut prints = Vec::new();
    // Where the current item's first token sits in the file's whole token sequence.
    let mut offset = 0u32;
    for item in &items {
        if item.tokens.len() >= params.min_tokens {
            item_fingerprints(&item.tokens, salt, params.window, &data, |index, hash| {
                prints.push(Fingerprint {
                    hash,
                    file,
                    position: offset + index,
                });
            });
        }
        offset = offset.saturating_add(u32::try_from(item.tokens.len()).unwrap_or(u32::MAX));
    }
    Scan::Indexed { lang, prints }
}

/// The ids of the punctuation that makes a window look like a table (D10).
pub(super) struct DataIds([u32; 6]);

impl DataIds {
    pub(super) fn new() -> Self {
        Self([",", ":", "[", "]", "{", "}"].map(|p| token_id(TokenClass::Punct, p)))
    }

    /// Whether a token is data: a string, number or literal, or table punctuation.
    pub(super) fn is_data(&self, token: &Token) -> bool {
        match token.class {
            TokenClass::Str | TokenClass::Num | TokenClass::Lit => true,
            TokenClass::Punct => self.0.contains(&token.id),
            _ => false,
        }
    }
}

/// Whether `data` of `len` tokens is a table by the D10 ratio.
pub(super) fn is_data_heavy(data: usize, len: usize) -> bool {
    data * 10 >= len * DATA_TENTHS
}

/// Hash every K-gram of one item with a rolling hash, then winnow. `keep(index, hash)` is called
/// for each selected K-gram, in position order, once per selected position.
pub(super) fn item_fingerprints(
    tokens: &[Token],
    salt: u64,
    window: usize,
    data: &DataIds,
    mut keep: impl FnMut(u32, u64),
) {
    if tokens.len() < K {
        return;
    }
    let count = tokens.len() - K + 1;
    let top = BASE.wrapping_pow(u32::try_from(K - 1).unwrap_or(0));
    let value = |t: &Token| u64::from(t.id) + 1;
    let mut hash = 0u64;
    let mut data_count = 0usize;
    for token in &tokens[..K] {
        hash = hash.wrapping_mul(BASE).wrapping_add(value(token));
        data_count += usize::from(data.is_data(token));
    }
    let mut hashes: Vec<u64> = Vec::with_capacity(count);
    for i in 0..count {
        hashes.push(if is_data_heavy(data_count, K) {
            u64::MAX
        } else {
            finalize(hash ^ salt)
        });
        if i + 1 < count {
            let (out, incoming) = (&tokens[i], &tokens[i + K]);
            hash = hash
                .wrapping_sub(value(out).wrapping_mul(top))
                .wrapping_mul(BASE)
                .wrapping_add(value(incoming));
            data_count =
                data_count + usize::from(data.is_data(incoming)) - usize::from(data.is_data(out));
        }
    }
    winnow(&hashes, window, |index, hash| {
        keep(u32::try_from(index).unwrap_or(u32::MAX), hash);
    });
}

/// SplitMix64's finalizer: spreads the polynomial hash so "minimum in a window" is an unbiased
/// choice. Never returns `u64::MAX`, which marks a K-gram that must not be selected.
fn finalize(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)).min(u64::MAX - 1)
}

/// Winnow: of every window of `window` consecutive hashes keep the minimum, the rightmost on a
/// tie, and report each selected position once. A K-gram hash of `u64::MAX` is never selected.
pub(super) fn winnow(hashes: &[u64], window: usize, mut select: impl FnMut(usize, u64)) {
    let window = window.max(1);
    let mut deque: VecDeque<usize> = VecDeque::new();
    let mut last = usize::MAX;
    for (i, &hash) in hashes.iter().enumerate() {
        while deque.back().is_some_and(|&b| hashes[b] >= hash) {
            deque.pop_back();
        }
        deque.push_back(i);
        if i + 1 < window {
            continue;
        }
        let window_start = i + 1 - window;
        while deque.front().is_some_and(|&f| f < window_start) {
            deque.pop_front();
        }
        if let Some(&chosen) = deque.front() {
            if chosen != last {
                last = chosen;
                if hashes[chosen] != u64::MAX {
                    select(chosen, hashes[chosen]);
                }
            }
        }
    }
}

/// Phase 2: walks the sorted fingerprints bucket by bucket (a bucket is a run of equal hashes)
/// and turns each surviving bucket into candidate pairs, a round at a time, in a deterministic
/// order.
struct Buckets<'a> {
    fingerprints: &'a [Fingerprint],
    start: usize,
    max_members: usize,
    boilerplate: u32,
}

impl<'a> Buckets<'a> {
    fn new(fingerprints: &'a [Fingerprint], max_members: usize) -> Self {
        Self {
            fingerprints,
            start: 0,
            max_members,
            boilerplate: 0,
        }
    }

    /// Buckets dropped as framework or house patterns, among those walked so far.
    fn boilerplate_skipped(&self) -> u32 {
        self.boilerplate
    }

    /// The candidate pairs of the next buckets, until at least `limit` pairs are collected or the
    /// buckets run out (a bucket is never split). Sorted by file pair, so verification re-reads
    /// each file as rarely as it can. Empty once nothing is left.
    fn next_round(&mut self, limit: usize) -> Vec<(Fingerprint, Fingerprint)> {
        let mut candidates: Vec<(Fingerprint, Fingerprint)> = Vec::new();
        while self.start < self.fingerprints.len() && candidates.len() < limit {
            let hash = self.fingerprints[self.start].hash;
            let mut end = self.start + 1;
            while end < self.fingerprints.len() && self.fingerprints[end].hash == hash {
                end += 1;
            }
            let bucket = &self.fingerprints[self.start..end];
            self.start = end;
            self.add_bucket(bucket, &mut candidates);
        }
        candidates.sort_unstable_by_key(|(x, y)| (x.file, y.file, x.position, y.position));
        candidates
    }

    fn add_bucket(&mut self, bucket: &[Fingerprint], out: &mut Vec<(Fingerprint, Fingerprint)>) {
        if bucket.len() < 2 {
            return;
        }
        let mut distinct_files = 1usize;
        for pair in bucket.windows(2) {
            if pair[0].file != pair[1].file {
                distinct_files += 1;
            }
        }
        // A bucket from one file is not a sharing problem for any of the three rules.
        if distinct_files < 2 {
            return;
        }
        // A sequence in more than `max_members` files is a framework or house pattern. The
        // member-count bound is an addition: one bucket with hundreds of fingerprints across a
        // few files would otherwise spend the whole pair budget on one pattern.
        if distinct_files > self.max_members || bucket.len() > self.max_members.saturating_mul(4) {
            self.boilerplate += 1;
            return;
        }
        for (a, x) in bucket.iter().enumerate() {
            for y in &bucket[a + 1..] {
                // Two K-grams of one file closer than `K` apart overlap each other, and (a
                // K-gram lies inside one item) are therefore in the same item: they cannot be
                // two non-overlapping spans.
                if x.file == y.file && ((y.position - x.position) as usize) < K {
                    continue;
                }
                out.push((*x, *y));
            }
        }
    }
}
