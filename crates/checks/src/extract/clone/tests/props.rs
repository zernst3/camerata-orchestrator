//! Property tests. The workspace has no property-testing dependency, so each property runs over
//! a fixed number of cases drawn from a seeded generator ([`Rng`]); a failing case names its seed.

use super::*;

/// One random corpus: clone groups in three languages, unique filler, and files every guard
/// refuses.
fn random_corpus(rng: &mut Rng) -> Vec<(String, String)> {
    let mut input: Vec<(String, String)> = Vec::new();
    let copies = 2 + rng.below(3);
    for n in 0..copies {
        let (func, order, user, base) = (rng.ident(), rng.ident(), rng.ident(), rng.ident());
        let rate = format!("0.{}", 1 + rng.below(98));
        input.push((
            format!("{}/ts{n}.ts", rng.pick(&["src", "app", "lib/pricing"])),
            ts_discount(&func, &order, &user, &base, &rate),
        ));
    }
    for n in 0..(2 + rng.below(2)) {
        input.push((
            format!("billing/status{n}.py"),
            py_status(&rng.ident(), &rng.ident()),
        ));
    }
    for n in 0..(2 + rng.below(2)) {
        input.push((
            format!("crates/fees/src/f{n}.rs"),
            rs_fee(&rng.ident(), &rng.ident()),
        ));
    }
    for n in 0..rng.below(4) {
        let id = rng.ident();
        input.push((
            format!("src/unique{n}.ts"),
            format!(
                "export function {id}(a) {{\n  const t = a.v{n} * {n} + a.w - {id}.length;\n  return t > {n} ? t : -t;\n}}\n"
            ),
        ));
    }
    input.push(("README.md".to_string(), "# readme\n".to_string()));
    input.push(("src/skip.test.ts".to_string(), ts_discount_default("0.2")));
    input.push(("vendor/skip.ts".to_string(), ts_discount_default("0.2")));
    input
}

#[test]
fn the_index_does_not_depend_on_the_order_of_the_files() {
    for seed in 0..40u64 {
        let mut rng = Rng(seed);
        let corpus = random_corpus(&mut rng);
        let baseline = index_of(&corpus);
        assert!(
            !baseline.families.is_empty(),
            "seed {seed}: the corpus has clones"
        );
        for round in 0..6u64 {
            let mut shuffled = corpus.clone();
            Rng(seed * 1000 + round).shuffle(&mut shuffled);
            assert_eq!(index_of(&shuffled), baseline, "seed {seed} round {round}");
        }
        let mut reversed = corpus.clone();
        reversed.reverse();
        assert_eq!(index_of(&reversed), baseline, "seed {seed} reversed");
    }
}

#[test]
fn the_index_does_not_depend_on_which_thread_scanned_a_file() {
    // More files than one batch, so several workers share the work.
    let mut corpus: Vec<(String, String)> = Vec::new();
    for n in 0..700 {
        let mut rng = Rng(n);
        let text = if n % 7 == 0 {
            ts_discount(
                &rng.ident(),
                &rng.ident(),
                &rng.ident(),
                &rng.ident(),
                "0.2",
            )
        } else {
            format!(
                "export function f{n}(a) {{\n  const t = a.v{n} * {n} + a.w;\n  return t > {n} ? t : -t;\n}}\n"
            )
        };
        corpus.push((format!("src/file{n:04}.ts"), text));
    }
    let first = index_of(&corpus);
    let second = index_of(&corpus);
    assert_eq!(first, second);
    assert_eq!(first.files_read, 700);
    let clones = first
        .families
        .iter()
        .find(|f| f.distinct_files >= 50)
        .map(|f| f.distinct_files);
    // 100 copies is over the ubiquity limit: counted, not listed.
    assert!(clones.is_none());
    assert!(first.boilerplate_buckets_skipped > 0);
}

#[test]
fn a_clone_is_found_whatever_the_identifiers_are() {
    for seed in 0..60u64 {
        let mut rng = Rng(seed);
        let one = ts_discount(
            &rng.ident(),
            &rng.ident(),
            &rng.ident(),
            &rng.ident(),
            "0.2",
        );
        let two = ts_discount(
            &rng.ident(),
            &rng.ident(),
            &rng.ident(),
            &rng.ident(),
            "0.2",
        );
        let found = families_of(&files(&[("a/one.ts", &one), ("b/two.ts", &two)]));
        assert_eq!(found.len(), 1, "seed {seed}: ts");
        assert_eq!(found[0].members.len(), 2, "seed {seed}: ts");

        let one = py_status(&rng.ident(), &rng.ident());
        let two = py_status(&rng.ident(), &rng.ident());
        let found = families_of(&files(&[("a/one.py", &one), ("b/two.py", &two)]));
        assert_eq!(found.len(), 1, "seed {seed}: py");

        let one = rs_fee(&rng.ident(), &rng.ident());
        let two = rs_fee(&rng.ident(), &rng.ident());
        let found = families_of(&files(&[("a/one.rs", &one), ("b/two.rs", &two)]));
        assert_eq!(found.len(), 1, "seed {seed}: rs");
    }
}

#[test]
fn a_clone_is_found_whatever_the_literals_and_property_names_are() {
    for seed in 0..40u64 {
        let mut rng = Rng(seed);
        let one = ts_discount_default(&format!("0.{}", 1 + rng.below(98)));
        let two = ts_discount_default(&format!("{}.{}", rng.below(9), 1 + rng.below(98)))
            .replace(".coupon", &format!(".{}", rng.ident()))
            .replace(".shipping", &format!(".{}", rng.ident()))
            .replace(" 100", &format!(" {}", 1 + rng.below(900)));
        let found = families_of(&files(&[("a.ts", &one), ("b.ts", &two)]));
        assert_eq!(found.len(), 1, "seed {seed}");
    }
}

#[test]
fn exact_diff_names_the_differing_pair_and_its_lines() {
    for seed in 0..80u64 {
        let mut rng = Rng(seed);
        let a = ts_discount("one", "order", "user", "base", "0.2");
        // (replacement of the A source, expected A text, expected B text, expected line in A)
        let (b, want_a, want_b, line) = match rng.below(5) {
            0 => {
                let rate = format!("0.{}", 3 + rng.below(90));
                (a.replace("0.2", &rate), "0.2".to_string(), rate, 5)
            }
            1 => {
                let word = rng.ident();
                (
                    a.replace(".coupon", &format!(".{word}")),
                    "coupon".to_string(),
                    word,
                    8,
                )
            }
            2 => {
                let op = (*rng.pick(&[">=", "<", "<=", "==="])).to_string();
                (
                    a.replace("base > 100", &format!("base {op} 100")),
                    ">".to_string(),
                    op,
                    8,
                )
            }
            3 => {
                let n = 11 + rng.below(80);
                (
                    a.replace("- 10;", &format!("- {n};")),
                    "10".to_string(),
                    n.to_string(),
                    9,
                )
            }
            _ => {
                let word = rng.ident();
                (
                    a.replace(".shipping", &format!(".{word}")),
                    "shipping".to_string(),
                    word,
                    11,
                )
            }
        };
        let pad = rng.below(6);
        let b = format!("{}{b}", "// padding\n".repeat(pad));
        let found = families_of(&files(&[("a.ts", &a), ("b.ts", &b)]));
        assert_eq!(found.len(), 1, "seed {seed}");
        let family = &found[0];
        let a_items = tokens(SourceLang::TypeScript, &a, TokenMode::Exact);
        let b_items = tokens(SourceLang::TypeScript, &b, TokenMode::Exact);
        let diff = exact_diff(
            &a_items[family.members[0].item as usize],
            &family.members[0],
            &b_items[family.members[1].item as usize],
            &family.members[1],
        );
        assert_eq!(diff.len(), 1, "seed {seed}: {diff:#?}");
        assert_eq!(
            (diff[0].a.as_str(), diff[0].b.as_str()),
            (want_a.as_str(), want_b.as_str()),
            "seed {seed}"
        );
        assert_eq!(diff[0].line_a, line, "seed {seed}");
        assert_eq!(
            diff[0].line_b,
            line + u32::try_from(pad).unwrap(),
            "seed {seed}"
        );
    }
}

#[test]
fn no_family_comes_from_a_single_file() {
    for seed in 0..40u64 {
        let mut rng = Rng(seed);
        let copies = 2 + rng.below(4);
        let mut source = String::new();
        for _ in 0..copies {
            let body = ts_discount(
                &rng.ident(),
                &rng.ident(),
                &rng.ident(),
                &rng.ident(),
                "0.2",
            );
            source.push_str(&body.replacen("import { money } from './money';\n", "", 1));
        }
        let py: String = (0..copies)
            .map(|_| py_status(&rng.ident(), &rng.ident()))
            .collect();
        let index = index_of(&files(&[
            ("src/only.ts", &source),
            ("src/only.py", &py),
            ("README.md", "# nothing\n"),
        ]));
        assert!(
            index.families.is_empty(),
            "seed {seed}: {:#?}",
            index.families
        );
        assert!(!index.truncated);
    }
}

#[test]
fn caps_are_monotone_and_a_truncated_index_is_a_subset() {
    for seed in 0..12u64 {
        let mut rng = Rng(seed);
        let groups = 2 + rng.below(3);
        let mut input = Vec::new();
        for group in 0..groups {
            // Each group is a different shape (a different number of extra statements).
            let extra = "  log(base, order.id);\n".repeat(group + 1);
            for copy in 0..(2 + rng.below(3)) {
                let src = ts_discount(&rng.ident(), "order", "user", "base", "0.2").replace(
                    "  return base + order.shipping;",
                    &format!("{extra}  return base + order.shipping;"),
                );
                input.push((format!("g{group}/c{copy}.ts"), src));
            }
        }
        let full = detect(
            &input,
            &CloneOptions {
                max_index_bytes: usize::MAX,
                max_pairs: u32::MAX,
                ..CloneOptions::default()
            },
        );
        assert!(!full.truncated);
        assert!(full.families.len() >= groups, "seed {seed}");
        let mut last_truncated = true;
        for cap in [0usize, 16, 64, 160, 480, 1600, 4800, 16_000, 160_000] {
            let capped = detect(
                &input,
                &CloneOptions {
                    max_index_bytes: cap,
                    ..CloneOptions::default()
                },
            );
            // More room never truncates what less room did not.
            assert!(!capped.truncated || last_truncated, "seed {seed} cap {cap}");
            last_truncated = capped.truncated;
            assert!(capped.files_read <= full.files_read);
            if !capped.truncated {
                assert_eq!(
                    capped, full,
                    "seed {seed} cap {cap}: an untruncated index is the full one"
                );
            }
            for family in &capped.families {
                assert!(
                    full.families.iter().any(|f| f.key == family.key
                        && f.token_len == family.token_len
                        && family.members.iter().all(|m| f.members.contains(m))),
                    "seed {seed} cap {cap}: a truncated family must exist in the full index"
                );
            }
        }
        assert!(
            !last_truncated,
            "seed {seed}: the largest cap holds the corpus"
        );
        for pairs in [0u32, 1, 2, 5, 20, 100, 100_000] {
            let capped = detect(
                &input,
                &CloneOptions {
                    max_pairs: pairs,
                    ..CloneOptions::default()
                },
            );
            if capped.truncated {
                assert!(
                    capped.families.len() <= full.families.len(),
                    "seed {seed} pairs {pairs}"
                );
            } else {
                assert_eq!(capped, full, "seed {seed} pairs {pairs}");
            }
        }
    }
}

#[test]
fn equal_input_gives_an_equal_index_run_after_run() {
    let mut rng = Rng(2024);
    let corpus = random_corpus(&mut rng);
    let first = index_of(&corpus);
    for _ in 0..5 {
        assert_eq!(index_of(&corpus), first);
    }
}
