//! ARCH-SINGLE-SOURCE-LOGIC-1: positive tests, then safe twins.

use super::*;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

fn one(files: &[(String, String)]) -> Candidate {
    let mut list = found(RULE, files);
    assert_eq!(list.len(), 1, "expected exactly one candidate: {list:#?}");
    list.remove(0)
}

#[test]
fn premium_discount_written_twice_with_different_rates_is_a_disagreeing_candidate() {
    let cart = ts_discount_default("0.2");
    let quote = ts_discount("quoteTotal", "o", "u", "b", "0.15");
    let c = one(&files(&[("src/cart.ts", &cart), ("src/quote.ts", &quote)]));
    assert!(!c.facts.agree);
    assert_eq!(
        c.facts.disagreements.len(),
        1,
        "{:?}",
        c.facts.disagreements
    );
    let d = &c.facts.disagreements[0];
    assert_eq!((d.a.as_str(), d.b.as_str()), ("0.2", "0.15"));
    assert_eq!(d.path_a, "src/cart.ts");
    assert_eq!(d.path_b, "src/quote.ts");
    assert_eq!((d.line_a, d.line_b), (5, 5));
    assert!(c.facts.token_len >= 60, "{}", c.facts.token_len);
    assert_eq!(
        c.id,
        format!(
            "{}:{:016x}:{}",
            RULE_SINGLE_SOURCE, c.family.key, c.family.token_len
        )
    );
    let RuleFacts::Logic {
        decision_tokens, ..
    } = &c.facts.rule
    else {
        panic!("logic facts expected");
    };
    assert!(
        decision_tokens.contains(&"0.2".to_string()),
        "{decision_tokens:?}"
    );
    assert!(
        decision_tokens.contains(&"isPremium".to_string()),
        "{decision_tokens:?}"
    );
}

#[test]
fn identical_copies_under_renamed_locals_agree() {
    let a = ts_discount("applyDiscount", "order", "user", "base", "0.2");
    let b = ts_discount("quoteTotal", "cart", "account", "amount", "0.2");
    let c = one(&files(&[("src/cart.ts", &a), ("src/quote.ts", &b)]));
    assert!(c.facts.agree);
    assert!(c.facts.disagreements.is_empty());
}

fn title_block(operator: &str) -> String {
    format!(
        "export function pageTitle(post, site) {{
  const raw = post.title {operator} post.slug;
  const trimmed = raw.trim();
  if (trimmed.length > 60) {{
    return trimmed.slice(0, 57) + '...';
  }}
  if (trimmed.length === 0 && site.fallback !== null) {{
    return site.fallback;
  }}
  return trimmed + ' | ' + site.name;
}}
"
    )
}

#[test]
fn nullish_versus_or_is_a_named_disagreement() {
    let c = one(&files(&[
        ("src/a.ts", &title_block("??")),
        ("src/b.ts", &title_block("||")),
    ]));
    assert!(c.facts.token_len >= 60);
    assert_eq!(
        c.facts.disagreements.len(),
        1,
        "{:?}",
        c.facts.disagreements
    );
    assert_eq!(c.facts.disagreements[0].a, "??");
    assert_eq!(c.facts.disagreements[0].b, "||");
}

#[test]
fn property_name_difference_is_a_named_disagreement() {
    let with = |field: &str| {
        format!(
            "export function fee(order, rate) {{
  const amount = order.{field} * rate;
  if (amount > 500 && order.express === true) {{
    return amount + order.handling;
  }}
  if (order.region === 'eu') {{
    return amount * 1.2;
  }}
  if (order.region === 'us' && amount < 50) {{
    return amount + 4;
  }}
  return amount;
}}
"
        )
    };
    let c = one(&files(&[
        ("src/a.ts", &with("total")),
        ("src/b.ts", &with("subtotal")),
    ]));
    assert_eq!(
        c.facts.disagreements.len(),
        1,
        "{:?}",
        c.facts.disagreements
    );
    assert_eq!(c.facts.disagreements[0].a, "total");
    assert_eq!(c.facts.disagreements[0].b, "subtotal");
}

#[test]
fn three_files_are_all_listed_and_sorted() {
    let src = ts_discount_default("0.2");
    let c = one(&files(&[
        ("src/c.ts", &src),
        ("src/a.ts", &src),
        ("src/b.ts", &src),
    ]));
    let paths: Vec<&str> = c.facts.sites.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["src/a.ts", "src/b.ts", "src/c.ts"]);
    assert_eq!(c.member_count(), 3);
}

#[test]
fn python_status_derivation_copied_across_modules_is_a_candidate() {
    let c = one(&files(&[
        ("billing/status.py", &py_status("derive_status", "order")),
        ("reports/status.py", &py_status("order_state", "o")),
    ]));
    assert!(c.facts.agree);
    assert_eq!(c.member_count(), 2);
}

#[test]
fn rust_fee_computation_in_two_modules_is_a_candidate() {
    let c = one(&files(&[
        ("src/billing/fee.rs", &rs_fee("fee", "order")),
        ("src/quotes/fee.rs", &rs_fee("quote_fee", "o")),
    ]));
    assert!(c.facts.agree);
}

#[test]
fn ranking_puts_disagreeing_families_first() {
    let list = found(
        RULE,
        &files(&[
            ("src/a.ts", &ts_discount_default("0.2")),
            ("src/b.ts", &ts_discount("other", "o", "u", "b", "0.2")),
            ("src/c.ts", &ts_shipping("ship", "20", "1.5")),
            ("src/d.ts", &ts_shipping("freight", "25", "1.5")),
        ]),
    );
    assert_eq!(list.len(), 2, "{list:#?}");
    assert!(
        !list[0].facts.agree,
        "the disagreeing family is ranked first"
    );
    assert!(list[1].facts.agree);
    assert_eq!((list[0].rank, list[1].rank), (0, 1));
}

// ---- safe twins ----

#[test]
fn twin_client_factory_boilerplate_without_a_decision_is_not_a_candidate() {
    let factory = "export function buildClient(config) {
  const client = createClient(config.url, config.key);
  client.setOptions(config.options);
  client.attach(config.logger);
  client.attach(config.metrics);
  client.attach(config.tracer);
  client.connect(config.timeout);
  client.register(config.handlers);
  client.register(config.hooks);
  return client;
}
";
    let input = files(&[("src/a.ts", factory), ("src/b.ts", factory)]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
}

fn retry_wrapper() -> &'static str {
    "export async function withRetry(operation, log) {
  try {
    const result = await operation.run();
    log.info('ok');
    return result;
  } catch (e) {
    if (e instanceof NetworkError && e.retryable === true) {
      log.warn('retrying ' + e.message);
      throw new RetryableError(e.code);
    }
    log.error(e);
    throw e;
  }
}
"
}

#[test]
fn twin_error_wrapper_skeleton_is_not_a_candidate() {
    let input = files(&[("src/a.ts", retry_wrapper()), ("src/b.ts", retry_wrapper())]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_python_except_clause_is_not_a_decision() {
    let body = "def call_with_retry(client, log):
    try:
        result = client.fetch()
        log.info('ok')
        return result
    except Exception as e:
        if isinstance(e, NetworkError) and e.retryable == True:
            log.warning('retrying ' + e.message)
            raise RetryableError(e.code)
        log.error(e)
        raise
";
    let input = files(&[("svc/a.py", body), ("svc/b.py", body)]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_test_arrange_blocks_are_never_read() {
    let src = ts_discount_default("0.2");
    let tests = files(&[("src/cart.test.ts", &src), ("src/quote.test.ts", &src)]);
    assert!(found(RULE, &tests).is_empty());
    let source = files(&[("src/cart.ts", &src), ("src/quote.ts", &src)]);
    assert_eq!(
        found(RULE, &source).len(),
        1,
        "the same copies in source are found"
    );
}

fn composed(options: &str, import: &str) -> String {
    format!(
        "{import}
export function priceFor(cart, user) {{
  const base = cart.subtotal * cart.quantity;
  if (user.isPremium && cart.coupon !== null) {{
    return applyDiscount(base, {options});
  }}
  if (base > 100 || cart.vip === true) {{
    return applyDiscount(base + cart.shipping, {options});
  }}
  return base;
}}
"
    )
}

#[test]
fn composed_variant_with_its_own_decisions_around_the_helper_is_a_candidate() {
    // W29 Wave C review, D2: both copies call the same imported helper, but the premium and
    // coupon tests and the base arithmetic are written around the call in each copy, and the
    // call is a small part of the sequence. The decision is the copies' own.
    let imported = "import { applyDiscount } from './pricing';";
    let input = files(&[
        (
            "src/cart.ts",
            &composed("{ rate: 0.2, floor: 10, label: 'premium' }", imported),
        ),
        (
            "src/quote.ts",
            &composed("{ rate: 0.15, floor: 5, label: 'quote' }", imported),
        ),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    let c = one(&input);
    assert!(!c.facts.agree);
}

/// A function whose body is nearly all one call to an imported helper and its options.
fn call_dominated(options: &str, import: &str) -> String {
    format!(
        "{import}
export function quoteFor(cart) {{
  const base = cart.subtotal * cart.quantity;
  return buildQuote(base, {options});
}}
"
    )
}

const BUILD_QUOTE_IMPORT: &str = "import { buildQuote } from './pricing';";
const LONG_OPTIONS: &str = "{ rate: 0.2, floor: 10, cap: 500, label: 'premium', tiers: [1, 2, 3], region: 'eu', rounding: 'up', currency: 'usd' }";

#[test]
fn twin_composed_twin_whose_call_and_options_dominate_is_not_a_candidate() {
    let input = files(&[
        (
            "src/cart.ts",
            &call_dominated(LONG_OPTIONS, BUILD_QUOTE_IMPORT),
        ),
        (
            "src/quote.ts",
            &call_dominated(&LONG_OPTIONS.replace("0.2", "0.15"), BUILD_QUOTE_IMPORT),
        ),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn composed_twin_with_a_short_call_is_a_candidate_when_a_decision_sits_outside_it() {
    // The positive sibling of the dominance twin: the same helper, but the call covers a small
    // share of the sequence and a decision is written outside its arguments.
    let body = |import: &str| {
        let mut text = format!("{import}\nexport function run(a, b, c) {{\n");
        for _ in 0..12 {
            text.push_str("  const d = a;\n");
        }
        text.push_str("  const kind = a > 1 ? 'big' : 'small';\n  return buildQuote(b, c);\n}\n");
        text
    };
    let input = files(&[
        ("src/a.ts", &body(BUILD_QUOTE_IMPORT)),
        ("src/b.ts", &body(BUILD_QUOTE_IMPORT)),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert_eq!(found(RULE, &input).len(), 1);
}

#[test]
fn twin_composed_twin_whose_every_decision_is_inside_the_call_arguments_is_not_a_candidate() {
    // The call covers well under half of the sequence, but every operator, literal and property
    // of the sequence is inside its argument list: the decision belongs to the call's options.
    let body = |import: &str| {
        let mut text = format!("{import}\nexport function run(a, b, c) {{\n");
        for _ in 0..14 {
            text.push_str("  const d = a;\n");
        }
        text.push_str("  return buildQuote(a * b > 1, 'x');\n}\n");
        text
    };
    let input = files(&[
        ("src/a.ts", &body(BUILD_QUOTE_IMPORT)),
        ("src/b.ts", &body(BUILD_QUOTE_IMPORT)),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn an_unrelated_imported_call_beside_the_decision_is_still_a_candidate() {
    for (callee, import) in [
        ("render", "import { render } from './view';"),
        ("auth", "import { auth } from '@/lib/auth';"),
        ("useState", "import { useState } from 'react';"),
        ("t", "import { t } from './i18n';"),
    ] {
        let source = format!(
            "{import}
export function priceFor(cart, user) {{
  const base = cart.subtotal * cart.quantity;
  if (user.isPremium && cart.coupon !== null) {{
    return base - cart.coupon.value;
  }}
  if (base > 100 || cart.vip === true) {{
    return base + cart.shipping;
  }}
  {callee}(base);
  return base;
}}
"
        );
        let input = files(&[("src/a.ts", &source), ("src/b.ts", &source)]);
        assert_eq!(
            found(RULE, &input).len(),
            1,
            "a call to `{callee}` beside the decision must not hide it"
        );
    }
}

#[test]
fn twin_helper_imported_from_different_modules_is_still_a_candidate() {
    let input = files(&[
        (
            "src/cart.ts",
            &composed(
                "{ rate: 0.2 }",
                "import { applyDiscount } from './pricing';",
            ),
        ),
        (
            "src/quote.ts",
            &composed(
                "{ rate: 0.2 }",
                "import { applyDiscount } from './quote-pricing';",
            ),
        ),
    ]);
    assert_eq!(
        found(RULE, &input).len(),
        1,
        "two different helpers is two implementations"
    );
}

#[test]
fn twin_helper_that_is_not_imported_is_still_a_candidate() {
    let input = files(&[
        ("src/cart.ts", &composed("{ rate: 0.2 }", "")),
        ("src/quote.ts", &composed("{ rate: 0.2 }", "")),
    ]);
    assert_eq!(found(RULE, &input).len(), 1);
}

fn handler_skeleton() -> &'static str {
    "export async function handler(req, res) {
  const token = req.headers.authorization;
  const session = await lookup(token);
  if (session.status === 401 || session.expired === true) {
    res.status(401).json({ error: 'unauthorized', code: 401 });
    return;
  }
  res.status(200).json({ ok: true, user: session.user });
}
"
}

fn filler(n: usize) -> String {
    let mut body = format!("export function filler{n}(a) {{\n");
    for j in 0..(4 + n) {
        body.push_str(&format!("  a.step{j}(a.value);\n"));
    }
    body.push_str("}\n");
    body
}

#[test]
fn twin_house_pattern_in_a_quarter_of_files_is_skipped() {
    let mut pairs: Vec<(String, String)> = (0..6)
        .map(|n| {
            (
                format!("src/routes/r{n}.ts"),
                handler_skeleton().to_string(),
            )
        })
        .collect();
    pairs.extend((0..14).map(|n| (format!("src/other/f{n}.ts"), filler(n))));
    let family = index_of(&pairs)
        .families
        .into_iter()
        .find(|f| f.members.iter().any(|m| m.file.contains("routes")))
        .expect("the skeleton is a family");
    assert!(family.house_pattern, "6 of 20 files is a house pattern");
    assert!(found(RULE, &pairs).is_empty());

    // The same skeleton in 2 of 20 files is two copies of one decision, not a house pattern.
    let mut few: Vec<(String, String)> = (0..2)
        .map(|n| {
            (
                format!("src/routes/r{n}.ts"),
                handler_skeleton().to_string(),
            )
        })
        .collect();
    few.extend((0..18).map(|n| (format!("src/other/f{n}.ts"), filler(n))));
    assert_eq!(found(RULE, &few).len(), 1);
}

#[test]
fn twin_same_file_repetition_is_not_a_family() {
    let src = ts_discount("first", "order", "user", "base", "0.2")
        + &ts_discount("second", "order", "user", "base", "0.2");
    assert!(found(RULE, &files(&[("src/cart.ts", &src)])).is_empty());
}

#[test]
fn twin_generated_vendored_minified_and_migration_files_are_skipped() {
    let src = ts_discount_default("0.2");
    let generated = format!("// @generated by codegen\n{src}");
    for other in [
        ("vendor/pricing/discount.ts", src.clone()),
        ("src/discount.generated.ts", src.clone()),
        ("src/discount.ts", generated),
        ("src/discount.min.js", src.clone()),
        ("db/migrations/002_discount.ts", src.clone()),
    ] {
        let input = files(&[("src/cart.ts", &src), (other.0, &other.1)]);
        assert!(
            found(RULE, &input).is_empty(),
            "{} must not count as a copy",
            other.0
        );
    }
    let control = files(&[("src/cart.ts", &src), ("src/discount.ts", &src)]);
    assert_eq!(found(RULE, &control).len(), 1);
}

#[test]
fn twin_data_table_window_is_not_fingerprinted() {
    let table = "export const RATES = {
  alpha: { code: 'a1', rate: 1.5, days: 3, label: 'one' },
  beta: { code: 'b2', rate: 2.5, days: 4, label: 'two' },
  gamma: { code: 'c3', rate: 3.5, days: 5, label: 'three' },
  delta: { code: 'd4', rate: 4.5, days: 6, label: 'four' },
  epsilon: { code: 'e5', rate: 5.5, days: 7, label: 'five' },
};
";
    let input = files(&[("src/a.ts", table), ("src/b.ts", table)]);
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_cross_language_copies_are_not_grouped() {
    let input = files(&[
        ("src/cart.ts", &ts_discount_default("0.2")),
        ("app/cart.py", &py_status("derive_status", "order")),
    ]);
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_type_declarations_and_imports_contribute_no_tokens() {
    let shell = |body: &str| {
        format!(
            "import {{ a, b, c }} from './deps';
import type {{ Order, User, Cart, Quote }} from './types';
export interface Totals {{ subtotal: number; tax: number; shipping: number; total: number; }}
export type Mode = 'cart' | 'quote' | 'invoice';
{body}
"
        )
    };
    let input = files(&[
        ("src/a.ts", &shell("export const one = 1;")),
        ("src/b.ts", &shell("export const two = 'x';")),
    ]);
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_tiny_clone_below_threshold_is_not_a_candidate() {
    let small = "export function clamp(v, lo, hi) {
  if (v < lo) {
    return lo;
  }
  if (v > hi) {
    return hi;
  }
  return v * 1 + lo.min;
}
";
    let input = files(&[("src/a.ts", small), ("src/b.ts", small)]);
    let index = index_of(&input);
    assert_eq!(index.families.len(), 1, "the index floor of 40 finds it");
    assert!(index.families[0].token_len < SINGLE_SOURCE_MIN_TOKENS);
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_malformed_and_empty_inputs_never_panic() {
    let junk = [
        "",
        "function (((",
        "export default {{{{",
        "if (a ? b : c",
        "\u{0}\u{1}\u{2}",
        "def f(:\n  if x ==",
        "fn main( { match x {",
        "<<<<<<< HEAD\nconst a = 1;\n=======\nconst a = 2;\n>>>>>>> branch\n",
    ];
    for (i, text) in junk.iter().enumerate() {
        let input = files(&[
            (&format!("src/a{i}.ts"), text),
            (&format!("src/b{i}.ts"), text),
            (&format!("src/c{i}.py"), text),
            (&format!("src/d{i}.rs"), text),
        ]);
        let _ = candidates(&input, &armed(&CloneRule::ALL));
    }
    let empty: Vec<(String, String)> = Vec::new();
    let set = candidates(&empty, &armed(&CloneRule::ALL));
    assert!(set.by_rule.values().all(Vec::is_empty));
}

// ---- which differences are decision differences (W29 Wave C review, severity rule) ----

/// `(a, b, decision)` of every disagreement between two sources.
fn flags(a: &str, b: &str, pa: &str, pb: &str) -> Vec<(String, String, bool)> {
    let c = one(&files(&[(pa, a), (pb, b)]));
    c.facts
        .disagreements
        .iter()
        .map(|d| (d.a.clone(), d.b.clone(), d.decision))
        .collect()
}

fn ts_flags(from: &str, to: &str) -> Vec<(String, String, bool)> {
    let a = ts_discount_default("0.2");
    let b = a.replace(from, to);
    assert_ne!(a, b, "the replacement must change the source");
    flags(&a, &b, "src/a.ts", "src/b.ts")
}

#[test]
fn a_number_or_operator_difference_is_a_decision_difference() {
    assert_eq!(
        ts_flags("base * 0.2", "base * 0.15"),
        [("0.2".to_string(), "0.15".to_string(), true)]
    );
    assert_eq!(
        ts_flags("base > 100", "base >= 100"),
        [(">".to_string(), ">=".to_string(), true)]
    );
    assert_eq!(
        ts_flags("return base - 10;", "return base - 12;"),
        [("10".to_string(), "12".to_string(), true)]
    );
}

#[test]
fn a_property_inside_the_decision_test_is_a_decision_difference() {
    assert_eq!(
        ts_flags("if (user.isPremium)", "if (user.isMember)"),
        [("isPremium".to_string(), "isMember".to_string(), true)]
    );
    assert_eq!(
        ts_flags("order.coupon !== null", "order.voucher !== null"),
        [("coupon".to_string(), "voucher".to_string(), true)]
    );
}

#[test]
fn a_property_outside_any_decision_test_is_not_a_decision_difference() {
    assert_eq!(
        ts_flags("base + order.shipping", "base + order.freight"),
        [("shipping".to_string(), "freight".to_string(), false)]
    );
}

#[test]
fn a_python_property_in_an_if_test_is_a_decision_difference_and_a_string_is_not() {
    let a = py_status("status", "order");
    let b = a.replace("order.state == 'refunded'", "order.phase == 'refunded'");
    assert_eq!(
        flags(&a, &b, "svc/a.py", "svc/b.py"),
        [("state".to_string(), "phase".to_string(), true)]
    );
    let c = a
        .replace("'closed'", "'CLOSED'")
        .replace("'paid'", "'PAID'");
    let found = flags(&a, &c, "svc/a.py", "svc/c.py");
    assert!(!found.is_empty());
    assert!(found.iter().all(|(_, _, decision)| !decision), "{found:?}");
}

#[test]
fn a_callee_a_string_or_an_action_type_is_not_a_decision_difference() {
    let a = redux_like("ADD_TODO", "REMOVE_TODO");
    let b = redux_like("ADD_NOTE", "REMOVE_NOTE");
    let found = flags(&a, &b, "src/a.js", "src/b.js");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|(_, _, decision)| !decision), "{found:?}");

    let callee = |name: &str| {
        format!(
            "export function quote(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = {name}(base, 0.2);
    return base - discount;
  }}
  if (base > 100 && order.coupon !== null) {{
    return base - 10;
  }}
  return base + order.shipping;
}}
"
        )
    };
    assert_eq!(
        flags(
            &callee("computeDiscount"),
            &callee("computeSurcharge"),
            "src/a.ts",
            "src/b.ts"
        ),
        [(
            "computeDiscount".to_string(),
            "computeSurcharge".to_string(),
            false
        )]
    );
}

/// A reducer over two action types; the type strings are the only difference between slices.
pub(super) fn redux_like(add: &str, remove: &str) -> String {
    format!(
        "const initial = {{ items: [], loading: false }};
export function reducer(state = initial, action) {{
  switch (action.type) {{
    case '{add}':
      return {{ ...state, items: [...state.items, action.payload] }};
    case '{remove}':
      return {{ ...state, items: state.items.filter((i) => i.id !== action.payload.id) }};
    case 'LOADING':
      return {{ ...state, loading: action.payload === true }};
    default:
      return state;
  }}
}}
"
    )
}

#[test]
fn a_callee_difference_makes_the_copies_disagree() {
    let make = |name: &str| {
        format!(
            "export function quote(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = {name}(base, 0.2);
    return base - discount;
  }}
  if (base > 100 && order.coupon !== null) {{
    return base - 10;
  }}
  return base + order.shipping;
}}
"
        )
    };
    let c = one(&files(&[
        ("src/a.ts", &make("computeDiscount")),
        ("src/b.ts", &make("computeSurcharge")),
    ]));
    assert!(!c.facts.agree, "the callee differs");
    assert_eq!(c.facts.disagreements.len(), 1);
    let same = one(&files(&[
        ("src/a.ts", &make("computeDiscount")),
        ("src/b.ts", &make("computeDiscount")),
    ]));
    assert!(same.facts.agree);
}

#[test]
fn the_candidate_carries_every_members_decision_tokens_as_a_sorted_multiset() {
    let a = ts_discount_default("0.2");
    let b = ts_discount("quoteTotal", "o", "u", "b", "0.15");
    let c = one(&files(&[("src/a.ts", &a), ("src/b.ts", &b)]));
    let RuleFacts::Logic {
        member_decision_tokens,
        decision_tokens,
    } = &c.facts.rule
    else {
        panic!("logic facts expected");
    };
    let mut sorted = member_decision_tokens.clone();
    sorted.sort();
    assert_eq!(member_decision_tokens, &sorted, "sorted");
    assert_eq!(
        member_decision_tokens.len(),
        2 * decision_tokens.len(),
        "every member contributes its own tokens"
    );
    for rate in ["0.2", "0.15"] {
        assert_eq!(
            member_decision_tokens.iter().filter(|t| *t == rate).count(),
            1
        );
    }
    // Independent of the order the files were given in and of their paths.
    let swapped = one(&files(&[("src/z.ts", &a), ("src/y.ts", &b)]));
    let RuleFacts::Logic {
        member_decision_tokens: again,
        ..
    } = &swapped.facts.rule
    else {
        panic!("logic facts expected");
    };
    assert_eq!(again, member_decision_tokens);
}

#[test]
fn a_variable_used_where_the_other_copy_uses_a_different_one_is_not_a_rename() {
    // `apart from variable names` must be true: a consistent rename is one thing, but `base`
    // in one copy where the other copy has `order` (while also using `base` elsewhere) is a
    // different computation under the same token shape.
    let a = ts_discount_default("0.2");
    let b = a.replace("return base - 10;", "return order - 10;");
    let c = one(&files(&[("src/a.ts", &a), ("src/b.ts", &b)]));
    assert!(!c.facts.agree);
    assert_eq!(
        c.facts.disagreements.len(),
        1,
        "{:?}",
        c.facts.disagreements
    );
    let d = &c.facts.disagreements[0];
    assert_eq!((d.a.as_str(), d.b.as_str()), ("base", "order"));
    assert!(!d.decision);
    assert_eq!((d.line_a, d.line_b), (9, 9));
    // A consistent rename of every use is still agreement.
    let renamed = ts_discount("quoteTotal", "o", "u", "b", "0.2");
    assert!(
        one(&files(&[("src/a.ts", &a), ("src/c.ts", &renamed)]))
            .facts
            .agree
    );
}
