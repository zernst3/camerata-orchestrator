//! Family formation: what is found, what is listed, and what is reported once.

use super::*;

/// The token count of the only item of `source`.
fn item_len(lang: SourceLang, source: &str) -> u32 {
    let items = tokens(lang, source, TokenMode::Structural);
    assert_eq!(
        items.len(),
        1,
        "fixture must have exactly one item with tokens"
    );
    u32::try_from(items[0].tokens.len()).unwrap()
}

#[test]
fn two_copies_under_renamed_locals_are_one_family() {
    let a = ts_discount("applyDiscount", "order", "user", "base", "0.2");
    let b = ts_discount("quoteTotal", "cart", "account", "amount", "0.2");
    let found = families_of(&files(&[("src/cart.ts", &a), ("src/quote.ts", &b)]));
    assert_eq!(found.len(), 1, "{found:#?}");
    let family = &found[0];
    assert_eq!(family.distinct_files, 2);
    assert_eq!(member_files(family), ["src/cart.ts", "src/quote.ts"]);
    assert_eq!(family.token_len, item_len(SourceLang::TypeScript, &a));
    assert!(!family.house_pattern);
    for member in &family.members {
        assert_eq!(member.token_len, family.token_len);
        assert_eq!(member.item, 0);
        assert_eq!(member.token_start, 0);
    }
}

#[test]
fn member_lines_are_the_first_and_last_token_lines() {
    let a = ts_discount_default("0.2");
    let found = families_of(&files(&[("a.ts", &a), ("b.ts", &a)]));
    let member = &found[0].members[0];
    // Line 1 is the import, which contributes no tokens; the function runs lines 2 to 12.
    assert_eq!((member.start_line, member.end_line), (2, 12));
}

#[test]
fn copies_that_differ_in_properties_literals_and_operator_families_still_match() {
    let a = ts_discount_default("0.2");
    let b = a
        .replace("0.2", "0.15")
        .replace("order.coupon", "order.voucher")
        .replace("base > 100", "base >= 250");
    let found = families_of(&files(&[("a.ts", &a), ("b.ts", &b)]));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].token_len, item_len(SourceLang::TypeScript, &a));
}

#[test]
fn members_of_three_copies_are_all_listed_and_sorted_by_file() {
    let a = ts_discount_default("0.2");
    let found = families_of(&files(&[
        ("src/c.ts", &a),
        ("src/a.ts", &a),
        ("src/b.ts", &a),
    ]));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(
        member_files(&found[0]),
        ["src/a.ts", "src/b.ts", "src/c.ts"]
    );
    assert_eq!(found[0].distinct_files, 3);
}

#[test]
fn a_python_status_ladder_copied_across_modules_is_a_family() {
    let found = families_of(&files(&[
        ("billing/status.py", &py_status("derive_status", "order")),
        ("reports/status.py", &py_status("order_state", "o")),
    ]));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].distinct_files, 2);
}

#[test]
fn a_rust_fee_computation_in_two_modules_is_a_family() {
    let found = families_of(&files(&[
        ("src/billing/fee.rs", &rs_fee("fee", "order")),
        ("src/quotes/fee.rs", &rs_fee("quote_fee", "o")),
    ]));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].distinct_files, 2);
}

#[test]
fn a_jsx_block_copied_between_components_is_a_family() {
    let component = |name: &str, label: &str| {
        format!(
            "export function {name}({{ loading, onRetry, items }}) {{
  if (loading) {{
    return (
      <div className=\"spinner\" role=\"status\">
        <Spinner size={{24}} />
        <span>{label}</span>
        <button onClick={{() => onRetry(items.length)}}>Retry</button>
      </div>
    );
  }}
  return null;
}}
"
        )
    };
    let found = families_of(&files(&[
        ("components/Menu.tsx", &component("Menu", "Loading")),
        ("components/List.tsx", &component("List", "Please wait")),
    ]));
    assert_eq!(found.len(), 1, "{found:#?}");
}

#[test]
fn structurally_different_code_is_not_a_family() {
    let ladder = "export function tier(n) {
  if (n > 100) { return 'gold'; }
  else if (n > 50) { return 'silver'; }
  else if (n > 10) { return 'bronze'; }
  return 'none';
}
";
    let switch = "export function tier(n) {
  switch (true) {
    case n > 100: return 'gold';
    case n > 50: return 'silver';
    case n > 10: return 'bronze';
    default: return 'none';
  }
}
";
    assert!(families_of(&files(&[("a.ts", ladder), ("b.ts", switch)])).is_empty());
}

#[test]
fn twin_same_file_repetition_is_not_a_family() {
    let one = ts_discount("first", "order", "user", "base", "0.2");
    let two = ts_discount("second", "cart", "account", "amount", "0.2");
    let body = |s: &str| s.replacen("import { money } from './money';\n", "", 1);
    let source = format!(
        "import {{ money }} from './money';\n{}{}",
        body(&one),
        body(&two)
    );
    let index = index_of(&files(&[("src/only.ts", &source)]));
    assert!(index.families.is_empty(), "{:#?}", index.families);
    assert_eq!(index.files_read, 1);
}

#[test]
fn same_file_members_of_a_cross_file_family_are_listed() {
    let one = ts_discount("first", "order", "user", "base", "0.2");
    let two = ts_discount("second", "cart", "account", "amount", "0.2");
    let body = |s: &str| s.replacen("import { money } from './money';\n", "", 1);
    let doubled = format!("{}{}", body(&one), body(&two));
    let found = families_of(&files(&[("a.ts", &doubled), ("b.ts", &one)]));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].members.len(), 3);
    assert_eq!(found[0].distinct_files, 2);
    assert_eq!(member_files(&found[0]), ["a.ts", "a.ts", "b.ts"]);
    let items: Vec<u32> = found[0].members.iter().map(|m| m.item).collect();
    assert_eq!(items, [0, 1, 0]);
}

#[test]
fn a_shared_prefix_is_a_second_family_and_a_full_copy_subsumes_nothing_smaller() {
    let full = ts_discount_default("0.2");
    // `c.ts` shares only the first part of the function, then goes its own way.
    let partial = full.replace(
        "  return base + order.shipping;\n",
        "  return Math.max(base, order.floor) * order.currency.rate + order.tax;\n",
    );
    let found = families_of(&files(&[
        ("a.ts", &full),
        ("b.ts", &full),
        ("c.ts", &partial),
    ]));
    let sizes: Vec<(u32, usize)> = found
        .iter()
        .map(|f| (f.token_len, f.members.len()))
        .collect();
    let full_len = item_len(SourceLang::TypeScript, &full);
    assert!(sizes.contains(&(full_len, 2)), "{sizes:?}");
    assert!(
        sizes
            .iter()
            .any(|(len, members)| *len < full_len && *members == 3),
        "{sizes:?}"
    );
}

#[test]
fn a_sub_family_with_no_more_members_than_its_container_is_dropped() {
    let a = ts_discount_default("0.2");
    let found = families_of(&files(&[("a.ts", &a), ("b.ts", &a), ("c.ts", &a)]));
    // One family of three. The shorter windows of the same three spans are subsumed, not listed.
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].members.len(), 3);
}

#[test]
fn families_are_sorted_by_key_and_stable_across_runs() {
    let a = ts_discount_default("0.2");
    let p = py_status("status", "order");
    let input = files(&[("a.ts", &a), ("b.ts", &a), ("a.py", &p), ("b.py", &p)]);
    let first = index_of(&input);
    let second = index_of(&input);
    assert_eq!(first, second);
    assert_eq!(first.families.len(), 2);
    let keys: Vec<u64> = first.families.iter().map(|f| f.key).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
}

#[test]
fn the_family_key_depends_only_on_the_shared_tokens() {
    let a = ts_discount("one", "order", "user", "base", "0.2");
    let b = ts_discount("two", "cart", "who", "amount", "0.99");
    let first = families_of(&files(&[("x/a.ts", &a), ("x/b.ts", &b)]));
    let second = families_of(&files(&[("y/c.ts", &a), ("y/d.ts", &b), ("y/e.ts", &a)]));
    assert_eq!(first[0].key, second[0].key);
}

#[test]
fn twin_type_declarations_and_imports_contribute_no_tokens() {
    let head = "import { Order } from './types';
import { money } from './money';
export interface Quote { total: number; lines: number }
export type Tier = 'gold' | 'silver';
export enum Mode { A, B }
";
    let one = format!("{head}export function f(a) {{ return a.x + 1; }}\n");
    let two =
        format!("{head}export function g(a, b, c) {{ if (a) {{ return b * c; }} return 0; }}\n");
    assert!(families_of(&files(&[("a.ts", &one), ("b.ts", &two)])).is_empty());
}

#[test]
fn twin_tiny_clone_below_the_index_floor_is_not_indexed() {
    let tiny = "export function f(a, b) { return a.x > 1 ? b.y : 0; }\n";
    assert!(families_of(&files(&[("a.ts", tiny), ("b.ts", tiny)])).is_empty());
}

#[test]
fn a_clone_just_above_the_floor_is_indexed_and_reports_its_length() {
    // About 45 tokens: above the 40-token index floor, below a rule's 60-token threshold.
    let mid = |n: &str| {
        format!(
            "export function {n}(order, user) {{
  if (user.isPremium && order.total > 100) {{
    return order.total * 0.2;
  }}
  return order.total - order.discount + order.fee;
}}
"
        )
    };
    let found = families_of(&files(&[("a.ts", &mid("a")), ("b.ts", &mid("b"))]));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        (40..60).contains(&found[0].token_len),
        "{}",
        found[0].token_len
    );
}

#[test]
fn min_tokens_option_moves_the_floor() {
    let src = ts_discount_default("0.2");
    let len = item_len(SourceLang::TypeScript, &src);
    let input = files(&[("a.ts", &src), ("b.ts", &src)]);
    let raised = CloneOptions {
        min_tokens: len + 1,
        ..CloneOptions::default()
    };
    assert!(detect(&input, &raised).families.is_empty());
    let exact = CloneOptions {
        min_tokens: len,
        ..CloneOptions::default()
    };
    assert_eq!(detect(&input, &exact).families.len(), 1);
}

#[test]
fn twin_cross_language_copies_are_not_grouped() {
    // The same token stream in two dialects: a call with a long argument list.
    let args = "a + 1,\n b * 2 - c,\n g(d, e) < h,\n [i, j],\n a.b.c,\n m[0] + n[1],\n p.q * 3,\n r > s,\n t - u,\n v + 1,\n w * 2,\n x.y - 4";
    let js = format!("result = combine({args})\n");
    let py = format!("result = combine({args})\n");
    let as_ts = structural_item_ids(SourceLang::JavaScript, &js);
    let as_py = structural_item_ids(SourceLang::Python, &py);
    assert_eq!(
        as_ts, as_py,
        "the fixture must tokenize identically in both dialects"
    );
    assert!(as_ts.len() >= 40);
    // Within one language they are a family; across languages they are not.
    assert_eq!(
        families_of(&files(&[("a.js", &js), ("b.js", &js)])).len(),
        1
    );
    assert_eq!(
        families_of(&files(&[("a.py", &py), ("b.py", &py)])).len(),
        1
    );
    assert!(families_of(&files(&[("a.js", &js), ("a.py", &py)])).is_empty());
}

#[test]
fn typescript_tsx_and_javascript_copies_are_one_language_family() {
    let src = "export function f(order, user) {
  const total = order.price * order.quantity + order.shipping - order.discount;
  if (user.isPremium && total > 100) { return total * 0.9; }
  return total;
}
";
    let found = families_of(&files(&[("a.js", src), ("b.tsx", src), ("c.ts", src)]));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].distinct_files, 3);
}

fn structural_item_ids(lang: SourceLang, source: &str) -> Vec<u32> {
    tokens(lang, source, TokenMode::Structural)
        .into_iter()
        .flat_map(|i| i.tokens.into_iter().map(|t| t.id))
        .collect()
}

#[test]
fn twin_data_table_window_is_not_fingerprinted() {
    let table = |name: &str| {
        let rows: Vec<String> = (0..24).map(|i| format!("'k{i}', {i}")).collect();
        format!(
            "export function {name}() {{ return [{}]; }}\n",
            rows.join(", ")
        )
    };
    assert!(families_of(&files(&[("a.ts", &table("a")), ("b.ts", &table("b"))])).is_empty());
}

#[test]
fn a_clone_with_data_in_it_is_still_found_when_logic_dominates() {
    let src = "export function price(order, user) {
  const base = order.price * order.quantity;
  if (user.tier === 'gold' && base > 100) { return base * 0.8; }
  if (user.tier === 'silver' && base > 50) { return base * 0.9; }
  return base - order.discount + order.fee;
}
";
    assert_eq!(
        families_of(&files(&[("a.ts", src), ("b.ts", src)])).len(),
        1
    );
}

#[test]
fn twin_tiny_vocabulary_sequences_are_not_a_family() {
    // Long but made of five kinds of token: `$ID ( $ID , ... ) ;`.
    let call = |n: &str| {
        let args = vec!["a"; 30].join(", ");
        format!("export function {n}() {{ f({args}); }}\n")
    };
    assert!(families_of(&files(&[("a.ts", &call("a")), ("b.ts", &call("b"))])).is_empty());
}

#[test]
fn a_block_repeated_inside_one_function_never_overlaps_itself() {
    let block = "  if (user.isPremium && order.total > 100) {
    total = order.total * 0.2 + order.fee - order.discount;
    log(total, order.id);
  }
";
    let repeated = format!(
        "export function f(order, user) {{\n  let total = 0;\n{block}{block}{block}  return total;\n}}\n"
    );
    let single = format!(
        "export function g(order, user) {{\n  let total = 0;\n{block}  return total;\n}}\n"
    );
    let found = families_of(&files(&[("a.ts", &repeated), ("b.ts", &single)]));
    assert!(!found.is_empty(), "the block is shared");
    for family in &found {
        let mut spans: Vec<&CodeSpan> = family.members.iter().collect();
        spans.sort_by_key(|s| (s.file.clone(), s.item, s.token_start));
        for pair in spans.windows(2) {
            if pair[0].file == pair[1].file && pair[0].item == pair[1].item {
                assert!(
                    pair[0].token_start + pair[0].token_len <= pair[1].token_start,
                    "overlapping members in {family:#?}"
                );
            }
        }
    }
}

#[test]
fn twin_house_pattern_in_a_quarter_of_files_is_flagged() {
    let skeleton = |n: usize| {
        format!(
            "export async function handler{n}(req, res) {{
  const session = await req.getSession();
  if (session.status === 401 || session.expired > 30) {{
    return res.status(401).json({{ error: 'unauthorized' }});
  }}
  return res.status(200).json(await load(req.query.id));
}}
"
        )
    };
    let filler = |n: usize| {
        format!(
            "export function unique{n}(a) {{ return a.v{n} + {n} * (a.w{n} - {n}); }}\nexport function other{n}(b) {{ return [b.x{n}, b.y{n}].length > {n} ? 'm{n}' : null; }}\n"
        )
    };
    let corpus = |sharing: usize| {
        let mut input = Vec::new();
        for n in 0..20 {
            let text = if n < sharing { skeleton(n) } else { filler(n) };
            input.push((format!("src/f{n:02}.ts"), text));
        }
        input
    };
    let six = families_of(&corpus(6));
    assert_eq!(six.len(), 1, "{six:#?}");
    assert_eq!(six[0].distinct_files, 6);
    assert!(six[0].house_pattern, "6 of 20 files is more than a quarter");
    let five = families_of(&corpus(5));
    assert_eq!(five[0].distinct_files, 5);
    assert!(
        !five[0].house_pattern,
        "5 of 20 is exactly a quarter, not more"
    );
    let four = families_of(&corpus(4));
    assert!(
        !four[0].house_pattern,
        "fewer than five files is never a house pattern"
    );
}

#[test]
fn a_sequence_in_more_than_fifty_files_is_dropped_and_counted() {
    let src = ts_discount_default("0.2");
    let many: Vec<(String, String)> = (0..60)
        .map(|n| (format!("src/m{n:02}.ts"), src.clone()))
        .collect();
    let index = index_of(&many);
    assert!(index.families.is_empty(), "{:#?}", index.families);
    assert!(index.boilerplate_buckets_skipped > 0);
    assert!(!index.truncated);
    let at_the_limit: Vec<(String, String)> = (0..50)
        .map(|n| (format!("src/m{n:02}.ts"), src.clone()))
        .collect();
    let index = index_of(&at_the_limit);
    assert_eq!(index.families.len(), 1);
    assert_eq!(index.boilerplate_buckets_skipped, 0);
}

#[test]
fn max_members_option_moves_the_ubiquity_limit() {
    let src = ts_discount_default("0.2");
    let input: Vec<(String, String)> = (0..8)
        .map(|n| (format!("src/m{n}.ts"), src.clone()))
        .collect();
    let strict = CloneOptions {
        max_members: 5,
        ..CloneOptions::default()
    };
    let index = detect(&input, &strict);
    assert!(index.families.is_empty());
    assert!(index.boilerplate_buckets_skipped > 0);
}

#[test]
fn files_read_counts_indexed_files_only() {
    let src = ts_discount_default("0.2");
    let input = files(&[
        ("a.ts", &src),
        ("b.ts", &src),
        ("README.md", "# hi"),
        ("a.test.ts", &src),
    ]);
    let index = index_of(&input);
    assert_eq!(index.files_read, 2);
    assert_eq!(index.skipped.len(), 2);
}

#[test]
fn duplicate_inputs_are_one_input() {
    let src = ts_discount_default("0.2");
    let once = index_of(&files(&[("a.ts", &src), ("b.ts", &src)]));
    let twice = index_of(&files(&[("a.ts", &src), ("a.ts", &src), ("b.ts", &src)]));
    assert_eq!(once, twice);
}

#[test]
fn twin_malformed_and_empty_inputs_never_panic() {
    let input = files(&[
        ("empty.ts", ""),
        ("blank.py", "\n\n\n"),
        ("broken.rs", "fn f( {"),
        ("broken.ts", "function ((( {{{"),
        ("broken.py", "def f(:\n\t  x ="),
        ("binary.ts", "\u{0}\u{1}\u{2}"),
        ("weird.tsx", "<<<>>>"),
    ]);
    let index = index_of(&input);
    assert!(index.families.is_empty());
    assert!(index
        .skipped
        .iter()
        .any(|(p, r)| p == "broken.rs" && *r == SkipReason::ParseFailed));
    assert!(!index.truncated);
}

#[test]
fn an_empty_corpus_is_an_empty_index() {
    assert_eq!(index_of(&[]), CloneIndex::default());
}
