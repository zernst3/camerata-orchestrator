//! Phase 4: families and subsumption (spec section 4), and the house-pattern flag (section 6, B1).
//!
//! A maximal pair's two spans have identical token ids by construction, so they carry the same
//! key (FNV-1a of the ids) and the same length. A family is the set of distinct spans sharing
//! `(key, token_len)`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::index::{MatchedPair, RawSpan};
use super::skip::LangFamily;
use super::{CloneFamily, CodeSpan};

/// A family is a house pattern when it is in at least this many files...
const HOUSE_MIN_FILES: u32 = 5;

/// ...and in more than this share (a quarter) of the indexed files of its language family.
const HOUSE_SHARE_DENOMINATOR: u64 = 4;

/// A family before it is turned into the public type.
struct Draft {
    key: u64,
    len: u32,
    /// Sorted by `(file, item, start)`, with no two overlapping.
    spans: Vec<RawSpan>,
}

impl Draft {
    fn distinct_files(&self) -> u32 {
        let mut files: Vec<u32> = self.spans.iter().map(|s| s.file).collect();
        files.dedup();
        u32::try_from(files.len()).unwrap_or(u32::MAX)
    }
}

pub(super) fn build_families(
    pairs: Vec<MatchedPair>,
    paths: &[&str],
    family_of_file: &[Option<LangFamily>],
) -> Vec<CloneFamily> {
    let drafts = drafts_from(pairs);
    let by_item = index_by_item(&drafts);
    let mut indexed_per_family: HashMap<LangFamily, u64> = HashMap::new();
    for family in family_of_file.iter().flatten() {
        *indexed_per_family.entry(*family).or_default() += 1;
    }

    let mut out: Vec<CloneFamily> = Vec::new();
    for (position, draft) in drafts.iter().enumerate() {
        if is_subsumed(position, &drafts, &by_item) {
            continue;
        }
        let mut spans = draft.spans.clone();
        spans.sort_by_key(|s| (s.file, s.start_line, s.start, s.item));
        let distinct_files = draft.distinct_files();
        let house_pattern = spans
            .first()
            .and_then(|s| family_of_file.get(s.file as usize).copied().flatten())
            .is_some_and(|family| {
                let in_family = indexed_per_family.get(&family).copied().unwrap_or(0);
                distinct_files >= HOUSE_MIN_FILES
                    && u64::from(distinct_files) * HOUSE_SHARE_DENOMINATOR > in_family
            });
        out.push(CloneFamily {
            key: draft.key,
            token_len: draft.len,
            members: spans
                .iter()
                .map(|s| CodeSpan {
                    file: paths
                        .get(s.file as usize)
                        .map_or_else(String::new, |p| (*p).to_string()),
                    item: s.item,
                    token_start: s.start,
                    token_len: s.len,
                    start_line: s.start_line,
                    end_line: s.end_line,
                })
                .collect(),
            distinct_files,
            house_pattern,
        });
    }
    out.sort_by(|a, b| {
        (
            a.key,
            a.token_len,
            a.members.first().map(|m| (&m.file, m.start_line)),
        )
            .cmp(&(
                b.key,
                b.token_len,
                b.members.first().map(|m| (&m.file, m.start_line)),
            ))
    });
    out
}

/// Group the maximal pairs' spans by `(key, len)`. A family needs two distinct files; spans of
/// one family that overlap each other (a periodic sequence matched at several shifts) are
/// reduced to the earliest, so no member is cited twice over the same tokens.
fn drafts_from(pairs: Vec<MatchedPair>) -> Vec<Draft> {
    let mut groups: BTreeMap<(u64, u32), Vec<RawSpan>> = BTreeMap::new();
    for pair in pairs {
        let group = groups.entry((pair.key, pair.len)).or_default();
        group.push(pair.a);
        group.push(pair.b);
    }
    let mut drafts = Vec::new();
    for ((key, len), mut spans) in groups {
        spans.sort_unstable();
        spans.dedup();
        let mut kept: Vec<RawSpan> = Vec::with_capacity(spans.len());
        for span in spans {
            let overlaps = kept.last().is_some_and(|last| {
                last.file == span.file
                    && last.item == span.item
                    && last.start + last.len > span.start
            });
            if !overlaps {
                kept.push(span);
            }
        }
        let draft = Draft {
            key,
            len,
            spans: kept,
        };
        if draft.spans.len() >= 2 && draft.distinct_files() >= 2 {
            drafts.push(draft);
        }
    }
    drafts
}

/// `(file, item)` to the `(start, end, family)` of every span in it.
type ByItem = HashMap<(u32, u32), Vec<(u32, u32, usize)>>;

fn index_by_item(drafts: &[Draft]) -> ByItem {
    let mut by_item: ByItem = HashMap::new();
    for (position, draft) in drafts.iter().enumerate() {
        for span in &draft.spans {
            by_item.entry((span.file, span.item)).or_default().push((
                span.start,
                span.start + span.len,
                position,
            ));
        }
    }
    by_item
}

/// Whether some other family `F'` says everything this one says: `F'` is longer, has at least as
/// many members, and every member of this family lies inside a member of `F'`.
fn is_subsumed(position: usize, drafts: &[Draft], by_item: &ByItem) -> bool {
    let family = &drafts[position];
    let Some(first) = family.spans.first() else {
        return false;
    };
    let inside = |span: &RawSpan, other: usize| {
        by_item.get(&(span.file, span.item)).is_some_and(|entries| {
            entries.iter().any(|&(start, end, owner)| {
                owner == other && start <= span.start && span.start + span.len <= end
            })
        })
    };
    let candidates: BTreeSet<usize> = by_item
        .get(&(first.file, first.item))
        .into_iter()
        .flatten()
        .filter(|&&(start, end, owner)| {
            owner != position
                && start <= first.start
                && first.start + first.len <= end
                && drafts[owner].len > family.len
                && drafts[owner].spans.len() >= family.spans.len()
        })
        .map(|&(_, _, owner)| owner)
        .collect();
    candidates
        .into_iter()
        .any(|other| family.spans.iter().all(|span| inside(span, other)))
}
