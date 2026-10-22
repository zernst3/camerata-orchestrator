//! The memory and work caps, the token cache, and the winnowing and verification internals.

use std::sync::Arc;

use super::super::cache::{Source, TokenCache};
use super::super::index::{is_data_heavy, item_fingerprints, winnow, DataIds, HashParams};
use super::super::verify::extend;
use super::*;

/// `count` copies of the discount function, one per file.
fn clones(count: usize) -> Vec<(String, String)> {
    let src = ts_discount_default("0.2");
    (0..count)
        .map(|n| (format!("src/c{n:02}.ts"), src.clone()))
        .collect()
}

/// The least value in `0..=high` (a multiple of `step`) for which `ok` holds, given that `ok` is
/// monotone: false below some point, true from it on.
fn least_passing(high: usize, step: usize, ok: impl Fn(usize) -> bool) -> usize {
    assert!(ok(high), "the upper bound must pass");
    let (mut low, mut high) = (0, high / step);
    while low < high {
        let mid = (low + high) / 2;
        if ok(mid * step) {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    low * step
}

#[test]
fn the_index_cap_stops_indexing_and_sets_truncated() {
    let input = clones(8);
    let unbounded = CloneOptions {
        max_index_bytes: usize::MAX,
        ..CloneOptions::default()
    };
    let full = detect(&input, &unbounded);
    assert!(!full.truncated);
    assert_eq!(full.files_read, 8);

    let tiny = CloneOptions {
        max_index_bytes: 16,
        ..CloneOptions::default()
    };
    let none = detect(&input, &tiny);
    assert!(none.truncated);
    assert_eq!(none.files_read, 0);
    assert!(none.families.is_empty());

    // The smallest cap that holds everything: one byte fewer truncates.
    let needed = least_passing(1_000_000, 16, |cap| {
        !detect(
            &input,
            &CloneOptions {
                max_index_bytes: cap,
                ..CloneOptions::default()
            },
        )
        .truncated
    });
    assert!(needed > 16);
    let exact = detect(
        &input,
        &CloneOptions {
            max_index_bytes: needed,
            ..CloneOptions::default()
        },
    );
    assert_eq!(exact, full);
    let short = detect(
        &input,
        &CloneOptions {
            max_index_bytes: needed - 16,
            ..CloneOptions::default()
        },
    );
    assert!(short.truncated);
    assert!(short.files_read < 8);
}

#[test]
fn a_truncated_index_is_correct_for_what_it_found() {
    let input = clones(8);
    let full = index_of(&input);
    let needed = least_passing(1_000_000, 16, |cap| {
        !detect(
            &input,
            &CloneOptions {
                max_index_bytes: cap,
                ..CloneOptions::default()
            },
        )
        .truncated
    });
    let partial = detect(
        &input,
        &CloneOptions {
            max_index_bytes: needed - 16,
            ..CloneOptions::default()
        },
    );
    assert!(partial.truncated);
    for family in &partial.families {
        let whole = full
            .families
            .iter()
            .find(|f| f.key == family.key && f.token_len == family.token_len)
            .expect("a truncated family exists in the full index");
        for member in &family.members {
            assert!(whole.members.contains(member), "{member:?}");
        }
    }
}

#[test]
fn the_pair_cap_stops_verification_and_sets_truncated() {
    // Ten different shapes, each copied into two files: ten separate regions to extend.
    let input: Vec<(String, String)> = (0..10)
        .flat_map(|shape| {
            let extra = "  log(base, order.id);\n".repeat(shape + 1);
            let src = ts_discount_default("0.2").replace(
                "  return base + order.shipping;\n",
                &format!("{extra}  return base + order.shipping;\n"),
            );
            [
                (format!("a/s{shape}.ts"), src.clone()),
                (format!("b/s{shape}.ts"), src),
            ]
        })
        .collect();
    let full = index_of(&input);
    assert!(!full.truncated);
    assert!(full.families.len() >= 10);

    let capped = detect(
        &input,
        &CloneOptions {
            max_pairs: 3,
            ..CloneOptions::default()
        },
    );
    assert!(capped.truncated);
    assert!(capped.families.len() < full.families.len());
    for family in &capped.families {
        assert!(
            full.families
                .iter()
                .any(|f| f.key == family.key && f.members == family.members),
            "a capped family exists in the full index"
        );
    }

    // More cap never truncates what less cap did not, and an untruncated index is the full one.
    let mut last_truncated = true;
    for cap in [0u32, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 1000, 1_000_000] {
        let result = detect(
            &input,
            &CloneOptions {
                max_pairs: cap,
                ..CloneOptions::default()
            },
        );
        assert!(!result.truncated || last_truncated, "cap {cap}");
        last_truncated = result.truncated;
        if !result.truncated {
            assert_eq!(result, full, "cap {cap}");
        }
    }
    assert!(!last_truncated);
}

#[test]
fn fingerprints_inside_an_already_matched_region_do_not_spend_the_pair_cap() {
    // Two identical files share dozens of fingerprints, but one region: one extension is all it
    // costs, so a cap of one holds the whole index.
    let input = clones(2);
    let full = index_of(&input);
    assert_eq!(full.families.len(), 1);
    let one = detect(
        &input,
        &CloneOptions {
            max_pairs: 1,
            ..CloneOptions::default()
        },
    );
    assert!(!one.truncated);
    assert_eq!(one, full);
}

#[test]
fn a_zero_pair_cap_finds_nothing_and_says_so() {
    let capped = detect(
        &clones(3),
        &CloneOptions {
            max_pairs: 0,
            ..CloneOptions::default()
        },
    );
    assert!(capped.truncated);
    assert!(capped.families.is_empty());
}

#[test]
fn an_input_with_no_candidates_is_never_truncated_by_a_zero_cap() {
    let capped = detect(
        &files(&[("a.ts", "export function f(a) { return a; }\n")]),
        &CloneOptions {
            max_pairs: 0,
            max_index_bytes: 0,
            ..CloneOptions::default()
        },
    );
    assert!(!capped.truncated);
}

#[test]
fn a_bucket_with_hundreds_of_members_is_boilerplate_even_across_few_files() {
    // Four files, each holding the same function 60 times: more than 4 * 50 fingerprints per
    // bucket, but only four distinct files.
    let body = |n: usize| {
        ts_discount("f", "order", "user", "base", "0.2")
            .replacen("import { money } from './money';\n", "", 1)
            .replace("function f(", &format!("function f{n}("))
    };
    let big: String = (0..60).map(body).collect();
    let input = files(&[
        ("a.ts", &big),
        ("b.ts", &big),
        ("c.ts", &big),
        ("d.ts", &big),
    ]);
    let index = index_of(&input);
    assert!(index.boilerplate_buckets_skipped > 0);
}

// -- the token cache ---------------------------------------------------------------------------

fn sources(texts: &[&'static str]) -> Vec<Source<'static>> {
    texts
        .iter()
        .map(|content| Source {
            lang: SourceLang::TypeScript,
            content,
        })
        .collect()
}

const A: &str = "export function a(x) { return x + 1; }\n";
const B: &str = "export function b(x) { return x * 2; }\n";
const C: &str = "export function c(x) { return x - 3; }\n";

#[test]
fn the_cache_holds_at_most_its_entry_limit() {
    let srcs = sources(&[A, B, C]);
    let mut cache = TokenCache::with_limits(2, usize::MAX);
    for (i, source) in srcs.iter().enumerate() {
        assert!(cache.get(u32::try_from(i).unwrap(), source).is_some());
    }
    assert_eq!(cache.len(), 2);
}

#[test]
fn the_cache_evicts_the_least_recently_used_file() {
    let srcs = sources(&[A, B, C]);
    let mut cache = TokenCache::with_limits(2, usize::MAX);
    let a1 = cache.get(0, &srcs[0]).unwrap();
    let _b = cache.get(1, &srcs[1]).unwrap();
    // Touch `a` so `b` is the oldest, then add `c`.
    let a2 = cache.get(0, &srcs[0]).unwrap();
    assert!(Arc::ptr_eq(&a1, &a2), "a hit returns the cached stream");
    let _c = cache.get(2, &srcs[2]).unwrap();
    let a3 = cache.get(0, &srcs[0]).unwrap();
    assert!(Arc::ptr_eq(&a1, &a3), "`a` survived");
    let b2 = cache.get(1, &srcs[1]).unwrap();
    assert!(
        !Arc::ptr_eq(&_b, &b2),
        "`b` was evicted and tokenized again"
    );
    assert_eq!(*_b, *b2, "re-tokenizing gives the same stream");
}

#[test]
fn the_cache_is_bounded_by_bytes_too() {
    let srcs = sources(&[A, B, C]);
    let mut probe = TokenCache::with_limits(8, usize::MAX);
    probe.get(0, &srcs[0]).unwrap();
    let one_file = probe.bytes();
    assert!(one_file > 0);
    let mut cache = TokenCache::with_limits(8, one_file * 2);
    for (i, source) in srcs.iter().enumerate() {
        cache.get(u32::try_from(i).unwrap(), source).unwrap();
        assert!(cache.bytes() <= one_file * 2);
    }
    assert_eq!(cache.len(), 2);
}

#[test]
fn a_file_larger_than_the_whole_budget_is_used_once_and_not_kept() {
    let srcs = sources(&[A]);
    let mut cache = TokenCache::with_limits(8, 1);
    let first = cache.get(0, &srcs[0]).unwrap();
    assert!(!first.items.is_empty());
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.bytes(), 0);
}

#[test]
fn the_cache_returns_none_for_a_file_that_does_not_tokenize() {
    let bad = [Source {
        lang: SourceLang::Rust,
        content: "fn broken( {",
    }];
    let mut cache = TokenCache::with_budget(64 * 1024 * 1024);
    assert!(cache.get(0, &bad[0]).is_none());
    assert_eq!(cache.len(), 0);
}

// -- winnowing ---------------------------------------------------------------------------------

fn selected(hashes: &[u64], window: usize) -> Vec<(usize, u64)> {
    let mut out = Vec::new();
    winnow(hashes, window, |i, h| out.push((i, h)));
    out
}

#[test]
fn a_window_of_one_selects_every_position() {
    let hashes = [5, 3, 9, 3, 7];
    let picked: Vec<usize> = selected(&hashes, 1).iter().map(|(i, _)| *i).collect();
    assert_eq!(picked, [0, 1, 2, 3, 4]);
}

#[test]
fn winnowing_takes_the_rightmost_minimum_of_each_window() {
    // Windows of 3: [4,2,2] [2,2,9] [2,9,1]; the minima are the rightmost 2 (index 2), the same
    // 2 again, then 1 at index 5.
    let hashes = [4, 2, 2, 9, 9, 1];
    let picked: Vec<(usize, u64)> = selected(&hashes, 3);
    assert_eq!(picked, [(2, 2), (5, 1)]);
}

#[test]
fn a_position_is_reported_once_however_many_windows_choose_it() {
    let picked = selected(&[9, 1, 8, 8, 8, 8], 3);
    assert_eq!(picked.iter().filter(|(i, _)| *i == 1).count(), 1);
}

#[test]
fn a_hash_of_u64_max_is_never_selected() {
    let hashes = [u64::MAX, u64::MAX, u64::MAX, 7, u64::MAX, u64::MAX];
    let picked = selected(&hashes, 2);
    assert!(picked.iter().all(|(_, h)| *h != u64::MAX));
    assert_eq!(picked, [(3, 7)]);
    assert!(selected(&[u64::MAX; 10], 3).is_empty());
}

#[test]
fn every_window_has_a_selected_minimum() {
    let mut rng = Rng(7);
    for _ in 0..200 {
        let len = 30 + rng.below(100);
        let window = 1 + rng.below(25);
        let hashes: Vec<u64> = (0..len).map(|_| rng.next() % 50).collect();
        let picked = selected(&hashes, window);
        for start in 0..=(len - window) {
            let slice = &hashes[start..start + window];
            let min = *slice.iter().min().unwrap();
            assert!(
                picked
                    .iter()
                    .any(|(i, h)| *i >= start && *i < start + window && *h == min),
                "window {start}..{} of {hashes:?} (w={window}) has no selected minimum",
                start + window
            );
        }
    }
}

#[test]
fn winnowing_density_is_close_to_two_over_window_plus_one() {
    let mut rng = Rng(11);
    let hashes: Vec<u64> = (0..50_000).map(|_| rng.next() >> 1).collect();
    let window = 21;
    let kept = selected(&hashes, window).len() as f64 / hashes.len() as f64;
    let expected = 2.0 / (window as f64 + 1.0);
    assert!(
        (kept - expected).abs() < 0.02,
        "kept {kept}, expected about {expected}"
    );
}

fn fabricated(ids: &[u32]) -> Vec<Token> {
    ids.iter()
        .map(|id| Token {
            id: *id,
            class: TokenClass::Ident,
            line: 1,
            raw: 0..0,
        })
        .collect()
}

fn fingerprint_hashes(ids: &[u32], window: usize) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    item_fingerprints(&fabricated(ids), 0, window, &DataIds::new(), |i, h| {
        out.push((i, h))
    });
    out
}

#[test]
fn equal_k_grams_hash_alike_wherever_they_sit() {
    let mut rng = Rng(3);
    for _ in 0..50 {
        let shared: Vec<u32> = (0..K).map(|_| (rng.next() % 1000) as u32).collect();
        let mut ids: Vec<u32> = (0..30).map(|_| (rng.next() % 1000) as u32).collect();
        let first = ids.len();
        ids.extend(&shared);
        ids.extend((0..17).map(|_| (rng.next() % 1000) as u32));
        let second = ids.len();
        ids.extend(&shared);
        let all = fingerprint_hashes(&ids, 1);
        let at = |position: usize| all.iter().find(|(i, _)| *i as usize == position).unwrap().1;
        assert_eq!(at(first), at(second));
    }
}

#[test]
fn spans_sharing_the_guarantee_length_share_a_fingerprint() {
    // The winnowing guarantee (spec section 4): any two spans of `T = 40` equal tokens have at
    // least one selected fingerprint in common, wherever they sit and whatever surrounds them.
    let t = 40;
    let window = t - K + 1;
    let mut rng = Rng(99);
    for case in 0..300 {
        let alphabet = if case % 2 == 0 { 40 } else { 4000 };
        let shared_len = t + rng.below(30);
        let shared: Vec<u32> = (0..shared_len)
            .map(|_| (rng.next() % alphabet) as u32)
            .collect();
        let build = |rng: &mut Rng| {
            let before = rng.below(60);
            let after = rng.below(60);
            let mut ids: Vec<u32> = (0..before)
                .map(|_| (rng.next() % alphabet) as u32)
                .collect();
            ids.extend(&shared);
            ids.extend((0..after).map(|_| (rng.next() % alphabet) as u32));
            ids
        };
        let (a, b) = (build(&mut rng), build(&mut rng));
        let ha: Vec<u64> = fingerprint_hashes(&a, window)
            .into_iter()
            .map(|(_, h)| h)
            .collect();
        let hb: Vec<u64> = fingerprint_hashes(&b, window)
            .into_iter()
            .map(|(_, h)| h)
            .collect();
        assert!(
            ha.iter().any(|h| hb.contains(h)),
            "case {case}: no shared fingerprint for a shared run of {shared_len}"
        );
    }
}

#[test]
fn unrelated_sequences_share_no_fingerprint() {
    let mut rng = Rng(5);
    let a: Vec<u32> = (0..400).map(|_| rng.next() as u32).collect();
    let b: Vec<u32> = (0..400).map(|_| rng.next() as u32).collect();
    let ha: Vec<u64> = fingerprint_hashes(&a, 21)
        .into_iter()
        .map(|(_, h)| h)
        .collect();
    let hb: Vec<u64> = fingerprint_hashes(&b, 21)
        .into_iter()
        .map(|(_, h)| h)
        .collect();
    assert!(ha.iter().all(|h| !hb.contains(h)));
}

#[test]
fn data_heavy_windows_are_not_fingerprinted() {
    let strings: Vec<Token> = (0..60)
        .map(|i| Token {
            id: i,
            class: TokenClass::Str,
            line: 1,
            raw: 0..0,
        })
        .collect();
    let mut out = Vec::new();
    item_fingerprints(&strings, 0, 21, &DataIds::new(), |i, h| out.push((i, h)));
    assert!(out.is_empty());
    assert!(is_data_heavy(12, 20));
    assert!(!is_data_heavy(11, 20));
}

// -- verification ------------------------------------------------------------------------------

#[test]
fn a_hash_collision_is_rejected_by_comparing_tokens() {
    let a: Vec<u32> = (0..100).collect();
    let mut b = a.clone();
    b[50] = 9999;
    let (a, b) = (fabricated(&a), fabricated(&b));
    // The K-grams at 30 cover position 50, where the streams differ: the fingerprints "collided".
    assert!(extend(&a, &b, 30, 30, false).is_none());
    // A K-gram that really is equal extends to the first difference on each side.
    let found = extend(&a, &b, 5, 5, false).expect("equal K-gram");
    assert_eq!((found.a_start, found.b_start, found.len), (0, 0, 50));
    let right = extend(&a, &b, 55, 55, false).expect("equal K-gram");
    assert_eq!((right.a_start, right.len), (51, 49));
}

#[test]
fn extension_stops_at_the_item_boundary_on_both_sides() {
    let a = fabricated(&(0..50).collect::<Vec<u32>>());
    let b = fabricated(&(0..50).collect::<Vec<u32>>());
    let found = extend(&a, &b, 10, 10, false).unwrap();
    assert_eq!((found.a_start, found.len), (0, 50));
    let shifted = fabricated(&(10..50).collect::<Vec<u32>>());
    let found = extend(&a, &shifted, 15, 5, false).unwrap();
    assert_eq!((found.a_start, found.b_start, found.len), (10, 0, 40));
}

#[test]
fn a_periodic_match_inside_one_item_is_trimmed_to_one_period() {
    // Three repeats of a 25-token block: the first and second repeat match, and extending the
    // match would run into the third; the trimmed match is one block and does not overlap.
    let block: Vec<u32> = (0..25).collect();
    let ids: Vec<u32> = block.iter().chain(&block).chain(&block).copied().collect();
    let tokens = fabricated(&ids);
    let found = extend(&tokens, &tokens, 3, 28, true).unwrap();
    assert_eq!(
        found.len, 25,
        "one period, not the 50 the overlap would allow"
    );
    assert_eq!(found.a_start + found.len, found.b_start);
}

#[test]
fn the_winnowing_parameters_guarantee_a_floor_no_rule_threshold_undercuts() {
    // The detection floor T is the longest shared run winnowing is guaranteed to find:
    // T = W + K - 1. The smallest threshold any consumer rule applies is 40 tokens
    // (UI-SHARED-PRIMITIVE-1; the two logic rules use 60), so T must not exceed it, or a run a
    // rule would act on could be missed.
    const SMALLEST_RULE_THRESHOLD: usize = 40;
    let options = CloneOptions::default();
    let params = HashParams::new(&options);
    assert_eq!(K, 30);
    assert_eq!(params.window, 11);
    assert_eq!(params.min_tokens, params.window + K - 1);
    assert_eq!(params.min_tokens, options.min_tokens as usize);
    assert!(params.min_tokens <= SMALLEST_RULE_THRESHOLD);
    assert!(K <= params.min_tokens, "a K-gram must fit inside the floor");
    // A smaller requested floor is clamped up to K, keeping T = W + K - 1 with W = 1.
    let low = HashParams::new(&CloneOptions {
        min_tokens: 5,
        ..CloneOptions::default()
    });
    assert_eq!((low.min_tokens, low.window), (K, 1));
}

#[test]
fn the_default_caps_hold_the_twenty_thousand_file_profile() {
    // The synthetic 20,000-file repository of `perf.rs` needs 340,622 extensions and indexes
    // without truncating under 96 MiB; the defaults must keep covering it.
    let options = CloneOptions::default();
    assert!(options.max_pairs >= 340_622);
    assert!(options.max_index_bytes >= 96 * 1024 * 1024);
}

#[test]
fn a_fingerprint_is_exactly_sixteen_bytes() {
    // The index budget counts 16 bytes per fingerprint; the struct must really be that size.
    assert_eq!(std::mem::size_of::<super::super::index::Fingerprint>(), 16);
}
