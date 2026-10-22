//! The producer's shell: what is armed, what is capped, and that the output is deterministic.

use super::*;

fn discount_repo() -> Vec<(String, String)> {
    files(&[
        ("src/a.ts", &ts_discount_default("0.2")),
        ("src/b.ts", &ts_discount("other", "o", "u", "b", "0.15")),
        ("src/c.ts", &ts_shipping("ship", "20", "1.5")),
        ("src/d.ts", &ts_shipping("freight", "25", "1.5")),
        ("package.json", r#"{ "dependencies": { "react": "18" } }"#),
    ])
}

#[test]
fn an_unarmed_rule_costs_nothing_and_has_no_entry() {
    let none = candidates(&discount_repo(), &HashSet::new());
    assert!(none.by_rule.is_empty());
    assert_eq!(
        none.index,
        IndexSummary::default(),
        "the index is not even built"
    );

    let one = candidates(&discount_repo(), &armed(&[CloneRule::SingleSourceLogic]));
    assert_eq!(
        one.by_rule.keys().copied().collect::<Vec<_>>(),
        [CloneRule::SingleSourceLogic]
    );
    assert_eq!(one.index.files_read, 4);
    assert!(!one.index.truncated);
    assert!(one.index.families >= 2);
}

#[test]
fn an_armed_rule_with_nothing_to_judge_has_an_empty_entry() {
    let set = candidates(&discount_repo(), &armed(&[CloneRule::SharedPrimitive]));
    assert_eq!(
        set.by_rule.get(&CloneRule::SharedPrimitive),
        Some(&Vec::new())
    );
    assert_eq!(set.over_cap.get(&CloneRule::SharedPrimitive), Some(&0));
    assert_eq!(set.found(CloneRule::SharedPrimitive), 0);
}

#[test]
fn rule_ids_and_the_enum_agree() {
    assert_eq!(RULE_IDS.len(), CloneRule::ALL.len());
    for rule in CloneRule::ALL {
        assert!(RULE_IDS.contains(&rule.id()));
        assert_eq!(CloneRule::from_id(rule.id()), Some(rule));
        assert_eq!(rule.affirmative_word(), rule.verdict_words()[0]);
        assert!(rule.verdict_words().contains(&"unsure"));
    }
    assert_eq!(CloneRule::from_id("ARCH-DEAD-CODE-1"), None);
    assert_eq!(CloneRule::from_id("arch-single-source-logic-1"), None);
}

#[test]
fn the_output_does_not_depend_on_the_order_of_the_files() {
    let reference = candidates(&discount_repo(), &armed(&CloneRule::ALL));
    assert!(reference.found(CloneRule::SingleSourceLogic) >= 2);
    let mut reversed = discount_repo();
    reversed.reverse();
    assert_eq!(candidates(&reversed, &armed(&CloneRule::ALL)), reference);
    let mut rotated = discount_repo();
    rotated.rotate_left(2);
    assert_eq!(candidates(&rotated, &armed(&CloneRule::ALL)), reference);
    // And a second run of the same input is the same output.
    assert_eq!(
        candidates(&discount_repo(), &armed(&CloneRule::ALL)),
        reference
    );
}

#[test]
fn candidate_ids_are_stable_across_scans_and_unique() {
    let first = candidates(&discount_repo(), &armed(&[CloneRule::SingleSourceLogic]));
    let second = candidates(&discount_repo(), &armed(&[CloneRule::SingleSourceLogic]));
    let ids = |s: &CandidateSet| -> Vec<String> {
        s.by_rule[&CloneRule::SingleSourceLogic]
            .iter()
            .map(|c| c.id.clone())
            .collect()
    };
    assert_eq!(ids(&first), ids(&second));
    let mut unique = ids(&first);
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids(&first).len());
}

#[test]
fn candidates_beyond_the_cap_are_counted_and_the_ranking_keeps_the_best() {
    let base = found(
        CloneRule::SingleSourceLogic,
        &files(&[
            ("src/a.ts", &ts_discount_default("0.2")),
            ("src/b.ts", &ts_discount_default("0.2")),
        ]),
    )
    .remove(0);
    // Five candidates that differ only in id (and so in the final tie-break) and in how many
    // places they live.
    let mut list: Vec<Candidate> = (0..5)
        .map(|n| {
            let mut c = base.clone();
            c.id = format!("{}:{n:016x}:{}", RULE_SINGLE_SOURCE, c.family.token_len);
            // Each lives somewhere else, so none is contained in another.
            for (k, site) in c.facts.sites.iter_mut().enumerate() {
                site.path = format!("src/n{n}_{k}.ts");
            }
            c
        })
        .collect();
    list[3].facts.sites.push(Site {
        path: "src/z.ts".to_string(),
        start_line: 1,
        end_line: 2,
    });
    let (kept, over) = rank_and_cap(CloneRule::SingleSourceLogic, list.clone(), 3);
    assert_eq!(kept.len(), 3);
    assert_eq!(over, 2);
    assert_eq!(
        kept[0].id, list[3].id,
        "the candidate with more places ranks first"
    );
    assert_eq!(kept.iter().map(|c| c.rank).collect::<Vec<_>>(), [0, 1, 2]);
    // Under the cap nothing is over it.
    let (all, none) = rank_and_cap(CloneRule::SingleSourceLogic, list, 160);
    assert_eq!((all.len(), none), (5, 0));
}

#[test]
fn the_judge_cap_is_a_constant_of_one_hundred_and_sixty() {
    assert_eq!(CLONE_JUDGE_MAX_PER_RULE, 160);
}

#[test]
fn ui_files_counts_only_indexed_files_in_react_family_packages() {
    let react = r#"{ "dependencies": { "react": "18" } }"#;
    let solid = r#"{ "dependencies": { "solid-js": "1" } }"#;
    let component = "export function Box({ items }) {\n  return <ul>{items.map((i) => <li key={i}>{i}</li>)}</ul>;\n}\n";
    let input = files(&[
        ("apps/web/package.json", react),
        ("apps/web/src/Box.tsx", component),
        ("apps/web/src/util.ts", "export const one = 1;\n"),
        ("apps/web/src/Box.test.tsx", component),
        ("apps/solid/package.json", solid),
        ("apps/solid/src/Box.tsx", component),
        ("services/api/main.py", "print('hi')\n"),
    ]);
    let set = candidates(&input, &armed(&[CloneRule::SharedPrimitive]));
    // Box.tsx and util.ts under the React package; the test file is skipped by the detector, the
    // Solid package and the Python file are out of scope.
    assert_eq!(set.index.ui_files, 2, "{:?}", set.index);
    // Counted only when the rule is armed.
    let other = candidates(&input, &armed(&[CloneRule::SingleSourceLogic]));
    assert_eq!(other.index.ui_files, 0);
}

// ---- overlapping families (W29 Wave C review, D4) ----

/// One function whose body is the same ten-line step thirty times over: the detector finds the
/// whole function AND every periodic sub-block of it as separate families.
fn periodic_function(name: &str) -> String {
    let step = "  if (a.b > 1 && c.d !== null) { x = x + 1; }\n";
    format!(
        "export function {name}(a, c) {{\n  let x = 0;\n{}  return x;\n}}\n",
        step.repeat(30)
    )
}

fn periodic_repo() -> Vec<(String, String)> {
    files(&[
        ("src/a.ts", &periodic_function("fa")),
        ("src/b.ts", &periodic_function("fb")),
        ("src/c.ts", &periodic_function("fc")),
    ])
}

#[test]
fn one_function_copied_in_three_files_is_one_candidate_not_a_family_per_period() {
    let input = periodic_repo();
    assert!(
        index_of(&input).families.len() > 1,
        "the detector reports the periodic sub-families too"
    );
    for rule in [CloneRule::SingleSourceLogic, CloneRule::NamingOneConcept] {
        let list = found(rule, &input);
        assert!(list.len() <= 1, "{rule:?}: {list:#?}");
    }
    let single = found(CloneRule::SingleSourceLogic, &input);
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].member_count(), 3, "the whole-function family");
    let spans: Vec<u32> = single[0]
        .facts
        .sites
        .iter()
        .map(|s| s.end_line - s.start_line)
        .collect();
    assert!(spans.iter().all(|n| *n >= 30), "{spans:?}");
}

fn site(path: &str, start: u32, end: u32) -> Site {
    Site {
        path: path.to_string(),
        start_line: start,
        end_line: end,
    }
}

/// A synthetic single-source candidate living at exactly `sites`.
fn at(template: &Candidate, id: &str, token_len: u32, sites: &[Site]) -> Candidate {
    let mut c = template.clone();
    c.id = id.to_string();
    c.facts.token_len = token_len;
    c.facts.sites = sites.to_vec();
    c
}

fn template() -> Candidate {
    found(
        CloneRule::SingleSourceLogic,
        &files(&[
            ("src/a.ts", &ts_discount_default("0.2")),
            ("src/b.ts", &ts_discount_default("0.2")),
        ]),
    )
    .remove(0)
}

#[test]
fn a_candidate_wholly_inside_a_kept_candidates_sites_is_dropped_whatever_its_own_rank() {
    let base = template();
    // The periodic sub-family has MORE places, so the plain ranking would put it first.
    let sub = at(
        &base,
        "sub",
        100,
        &[
            site("a.ts", 3, 12),
            site("a.ts", 13, 22),
            site("b.ts", 3, 12),
            site("b.ts", 13, 22),
        ],
    );
    let whole = at(
        &base,
        "whole",
        300,
        &[site("a.ts", 1, 34), site("b.ts", 1, 34)],
    );
    let (kept, over) = rank_and_cap(
        CloneRule::SingleSourceLogic,
        vec![sub.clone(), whole.clone()],
        160,
    );
    assert_eq!(over, 0, "a dominated candidate is not 'over the cap'");
    assert_eq!(
        kept.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["whole"]
    );
    assert_eq!(kept[0].rank, 0);
}

#[test]
fn a_candidate_with_a_site_outside_the_kept_one_survives() {
    let base = template();
    let whole = at(
        &base,
        "whole",
        300,
        &[site("a.ts", 1, 34), site("b.ts", 1, 34)],
    );
    let partly = at(
        &base,
        "partly",
        100,
        &[site("a.ts", 3, 12), site("z.ts", 1, 10)],
    );
    let elsewhere = at(
        &base,
        "elsewhere",
        100,
        &[site("c.ts", 3, 12), site("d.ts", 3, 12)],
    );
    let (kept, _) = rank_and_cap(
        CloneRule::SingleSourceLogic,
        vec![partly, elsewhere, whole],
        160,
    );
    let mut ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["elsewhere", "partly", "whole"]);
}

#[test]
fn candidates_with_identical_coverage_keep_exactly_one() {
    let base = template();
    let sites = [site("a.ts", 1, 34), site("b.ts", 1, 34)];
    let one = at(&base, "one", 300, &sites);
    let two = at(&base, "two", 280, &sites);
    let (kept, _) = rank_and_cap(CloneRule::SingleSourceLogic, vec![two, one], 160);
    assert_eq!(kept.len(), 1);
}

#[test]
fn the_cap_counts_only_the_candidates_that_survive_the_overlap_pass() {
    let base = template();
    let mut list = vec![at(
        &base,
        "whole",
        300,
        &[site("a.ts", 1, 100), site("b.ts", 1, 100)],
    )];
    for n in 0..4u32 {
        list.push(at(
            &base,
            &format!("sub{n}"),
            50,
            &[site("a.ts", 1 + n, 10 + n), site("b.ts", 1 + n, 10 + n)],
        ));
    }
    let (kept, over) = rank_and_cap(CloneRule::SingleSourceLogic, list, 2);
    assert_eq!((kept.len(), over), (1, 0));
}
