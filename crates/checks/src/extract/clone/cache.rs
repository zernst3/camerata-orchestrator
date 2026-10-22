//! The bounded cache of tokenized files phase 3 re-reads.
//!
//! Phase 1 discards every token stream (it keeps 16 bytes per fingerprint), so verifying a
//! candidate pair means tokenizing both files again. The cache bounds that by BYTES
//! ([`MAX_BYTES`], shared among the verification workers) and, as a backstop, by entries,
//! evicting the least recently used. It changes only how long phase 3 takes, never what it
//! finds: a file tokenizes to the same stream every time.
//!
//! The spec bounds the cache at 256 files and 64 MiB. A tokenized 150-line file is about 20 KB,
//! so 256 files is 5 MB of a 64 MiB budget, and verification re-parsed files it could have kept.
//! The byte budget is the real limit, so the entry limit is only a guard against a corpus of
//! thousands of tiny files (build notes).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::{tokenize, ItemTokens, Token, TokenMode};
use crate::extract::SourceLang;

/// The most tokenized files held at once. A guard, not the budget: see the module docs.
pub(super) const MAX_ENTRIES: usize = 16_384;

/// The most bytes of tokens held at once across all workers (spec section 5).
pub(super) const MAX_BYTES: usize = 64 * 1024 * 1024;

/// One file as the cache needs it to re-tokenize.
pub(super) struct Source<'a> {
    pub(super) lang: SourceLang,
    pub(super) content: &'a str,
}

/// A tokenized file, with where each item starts in the file's whole token sequence (the
/// coordinate a [`super::index::Fingerprint`] uses).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CachedFile {
    pub(super) items: Vec<ItemTokens>,
    starts: Vec<u32>,
}

impl CachedFile {
    fn new(items: Vec<ItemTokens>) -> Self {
        let mut starts = Vec::with_capacity(items.len());
        let mut at = 0u32;
        for item in &items {
            starts.push(at);
            at = at.saturating_add(u32::try_from(item.tokens.len()).unwrap_or(u32::MAX));
        }
        Self { items, starts }
    }

    /// The item and the token index inside it that a file-wide token `position` falls on.
    pub(super) fn locate(&self, position: u32) -> Option<(usize, usize)> {
        let item = self
            .starts
            .partition_point(|&start| start <= position)
            .checked_sub(1)?;
        let index = (position - self.starts[item]) as usize;
        (index < self.items[item].tokens.len()).then_some((item, index))
    }
}

struct Entry {
    items: Arc<CachedFile>,
    bytes: usize,
    used: u64,
}

pub(super) struct TokenCache {
    entries: HashMap<u32, Entry>,
    /// Entries by last use, oldest first. Ticks are unique, so this is a total order.
    by_use: BTreeMap<u64, u32>,
    bytes: usize,
    tick: u64,
    max_entries: usize,
    max_bytes: usize,
}

impl TokenCache {
    pub(super) fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            by_use: BTreeMap::new(),
            bytes: 0,
            tick: 0,
            max_entries: max_entries.max(1),
            max_bytes,
        }
    }

    /// A cache with the default entry limit and `max_bytes` of budget.
    pub(super) fn with_budget(max_bytes: usize) -> Self {
        Self::with_limits(MAX_ENTRIES, max_bytes)
    }

    /// The Structural token stream of `file`, tokenizing it when it is not cached. `None` when
    /// the file does not tokenize (which phase 1 already accepted, so only a nondeterministic
    /// tokenizer could reach it).
    pub(super) fn get(&mut self, file: u32, source: &Source<'_>) -> Option<Arc<CachedFile>> {
        self.tick += 1;
        if let Some(entry) = self.entries.get_mut(&file) {
            self.by_use.remove(&entry.used);
            entry.used = self.tick;
            self.by_use.insert(self.tick, file);
            return Some(Arc::clone(&entry.items));
        }
        let items = Arc::new(CachedFile::new(tokenize(
            source.lang,
            source.content,
            TokenMode::Structural,
        )?));
        let bytes = footprint(&items.items);
        // A single file larger than the whole budget is used once and not kept.
        if bytes > self.max_bytes {
            return Some(items);
        }
        while self.entries.len() >= self.max_entries || self.bytes + bytes > self.max_bytes {
            if !self.evict_oldest() {
                break;
            }
        }
        self.bytes += bytes;
        self.by_use.insert(self.tick, file);
        self.entries.insert(
            file,
            Entry {
                items: Arc::clone(&items),
                bytes,
                used: self.tick,
            },
        );
        Some(items)
    }

    /// Drop the least recently used entry. `false` when the cache is already empty.
    fn evict_oldest(&mut self) -> bool {
        let Some((_, file)) = self.by_use.pop_first() else {
            return false;
        };
        if let Some(entry) = self.entries.remove(&file) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
        true
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
}

/// An estimate of the heap a tokenized file holds.
fn footprint(items: &[ItemTokens]) -> usize {
    items
        .iter()
        .map(|item| {
            std::mem::size_of::<ItemTokens>() + item.tokens.len() * std::mem::size_of::<Token>()
        })
        .sum()
}
