//! ARCH-NAMING-ONE-CONCEPT-1: positive tests, then safe twins.

use super::*;
use crate::clone_rules::names::{concept_words, has_two_different_concepts};

const RULE: CloneRule = CloneRule::NamingOneConcept;

fn ts_money(name: &str, fallback: &str) -> String {
    format!(
        "export function {name}(amount, currency) {{
  const code = currency ?? '{fallback}';
  const fixed = Math.round(amount * 100) / 100;
  if (fixed < 0) {{
    return '-' + code + ' ' + Math.abs(fixed).toFixed(2);
  }}
  if (fixed === 0 && code !== null) {{
    return code + ' free';
  }}
  return code + ' ' + fixed.toFixed(2);
}}
"
    )
}

fn ts_date(name: &str) -> String {
    format!(
        "export function {name}(value, locale) {{
  const date = new Date(value);
  if (isNaN(date.getTime())) {{
    return '';
  }}
  const day = date.getDate() < 10 ? '0' + date.getDate() : date.getDate();
  const month = date.getMonth() + 1;
  return day + '/' + month + '/' + date.getFullYear() + (locale ?? '');
}}
"
    )
}

fn ts_number(name: &str) -> String {
    format!(
        "export function {name}(value, digits) {{
  if (value === null || value === undefined) {{
    return '0';
  }}
  const places = digits > 6 ? 6 : digits;
  const scaled = Math.round(value * Math.pow(10, places)) / Math.pow(10, places);
  return scaled.toFixed(places).replace(',', '.');
}}
"
    )
}

fn one(files: &[(String, String)]) -> Candidate {
    let mut list = found(RULE, files);
    assert_eq!(list.len(), 1, "expected exactly one candidate: {list:#?}");
    list.remove(0)
}

fn names_of(c: &Candidate) -> Vec<String> {
    let RuleFacts::Naming { names, .. } = &c.facts.rule else {
        panic!("naming facts expected");
    };
    names
        .iter()
        .map(|n| n.name.clone().unwrap_or_default())
        .collect()
}

#[test]
fn two_functions_with_identical_bodies_and_different_nouns_are_a_candidate() {
    let c = one(&files(&[
        ("lib/format.ts", &ts_money("formatPrice", "USD")),
        ("utils/money.ts", &ts_money("formatMoney", "USD")),
    ]));
    assert!(c.facts.token_len >= 60, "{}", c.facts.token_len);
    assert!(c.facts.agree);
    assert_eq!(names_of(&c), ["formatPrice", "formatMoney"]);
    let RuleFacts::Naming { module, .. } = &c.facts.rule else {
        panic!();
    };
    assert!(module.is_none());
}

#[test]
fn python_helpers_under_two_names_are_a_candidate() {
    let body = |name: &str| {
        format!(
            "def {name}(user):
    first = (user.first_name or '').strip()
    last = (user.last_name or '').strip()
    if first and last:
        return first + ' ' + last
    if first:
        return first
    if last:
        return last
    return user.email.split('@')[0]
"
        )
    };
    let c = one(&files(&[
        ("accounts/names.py", &body("to_display_name")),
        ("profiles/labels.py", &body("display_label")),
    ]));
    assert_eq!(names_of(&c), ["to_display_name", "display_label"]);
}

#[test]
fn rust_free_functions_under_two_names_are_a_candidate() {
    let body = |name: &str| {
        format!(
            "pub fn {name}(items: &[Item], tax: u64) -> u64 {{
    let mut total = 0;
    for item in items {{
        if item.active && item.qty > 0 {{
            total += item.price * item.qty;
        }}
    }}
    if tax > 0 {{
        total += total * tax / 100;
    }}
    total
}}
"
        )
    };
    let c = one(&files(&[
        ("src/cart/sum.rs", &body("cart_total")),
        ("src/basket/sum.rs", &body("basket_amount")),
    ]));
    assert_eq!(names_of(&c), ["basket_amount", "cart_total"]);
}

#[test]
fn two_cloned_pairs_between_two_files_roll_up_to_one_module_candidate() {
    let lib = format!("{}{}", ts_date("formatDate"), ts_number("formatNumber"));
    let utils = format!("{}{}", ts_date("fmtDate"), ts_number("numberToText"));
    let c = one(&files(&[
        ("lib/format.ts", &lib),
        ("utils/format.ts", &utils),
    ]));
    let RuleFacts::Naming { module, .. } = &c.facts.rule else {
        panic!();
    };
    let module = module.as_ref().expect("a module roll-up");
    assert_eq!(
        module.files,
        ["lib/format.ts".to_string(), "utils/format.ts".to_string()]
    );
    assert_eq!(module.pairs.len(), 2);
    let pairs: Vec<(String, String)> = module
        .pairs
        .iter()
        .map(|p| {
            (
                p.a.name.clone().unwrap_or_default(),
                p.b.name.clone().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            ("formatDate".to_string(), "fmtDate".to_string()),
            ("formatNumber".to_string(), "numberToText".to_string())
        ]
    );
    assert_eq!(c.member_count(), 4);
}

#[test]
fn a_single_cloned_pair_between_two_files_is_not_rolled_up() {
    let lib = format!("{}{}", ts_date("formatDate"), ts_number("formatNumber"));
    let utils = format!("{}{}", ts_date("fmtDate"), ts_number("formatNumber"));
    let list = found(
        RULE,
        &files(&[("lib/format.ts", &lib), ("utils/format.ts", &utils)]),
    );
    assert_eq!(list.len(), 1, "{list:#?}");
    let RuleFacts::Naming { module, .. } = &list[0].facts.rule else {
        panic!();
    };
    assert!(
        module.is_none(),
        "one differently named pair is a function candidate"
    );
}

#[test]
fn body_disagreement_is_carried_into_the_naming_facts() {
    let c = one(&files(&[
        ("lib/format.ts", &ts_money("formatPrice", "USD")),
        ("utils/money.ts", &ts_money("formatMoney", "EUR")),
    ]));
    assert!(!c.facts.agree);
    assert_eq!(c.facts.disagreements.len(), 1);
    assert_eq!(c.facts.disagreements[0].a, "USD");
    assert_eq!(c.facts.disagreements[0].b, "EUR");
    assert_eq!(c.facts.disagreements[0].line_a, 2);
}

#[test]
fn name_words_ignore_verbs_separators_and_case() {
    assert_eq!(concept_words("getUser"), concept_words("fetch_user"));
    assert_eq!(concept_words("userName"), concept_words("user_name"));
    assert_eq!(concept_words("formatPrice"), concept_words("format_price"));
    assert_ne!(concept_words("formatPrice"), concept_words("formatMoney"));
    assert_eq!(
        concept_words("buildReport"),
        concept_words("makeReportHelper")
    );
    assert_eq!(
        concept_words("parseHTTPResponse"),
        ["http", "parse", "response"]
    );
    // A name that is only a verb keeps it, so `get` and `set` stay different.
    assert_ne!(concept_words("get"), concept_words("set"));
    assert!(has_two_different_concepts(["formatPrice", "formatMoney"]));
    assert!(!has_two_different_concepts([
        "getUser",
        "fetchUser",
        "get_user"
    ]));
    assert!(!has_two_different_concepts([
        "default",
        "<anonymous>",
        "formatPrice"
    ]));
}

// ---- safe twins ----

#[test]
fn twin_same_name_in_two_modules_is_not_a_naming_candidate() {
    let input = files(&[
        ("lib/format.ts", &ts_money("formatPrice", "USD")),
        ("utils/money.ts", &ts_money("formatPrice", "USD")),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_verb_only_difference_is_not_a_candidate() {
    let input = files(&[
        ("lib/user.ts", &ts_money("getPrice", "USD")),
        ("utils/user.ts", &ts_money("fetchPrice", "USD")),
    ]);
    assert_eq!(index_of(&input).families.len(), 1);
    assert!(found(RULE, &input).is_empty());
}

fn mapper_body(call: &str) -> String {
    format!(
        "(user) {{
  const base = {call}(user);
  if (user.active === true && user.role !== 'guest') {{
    base.tier = user.plan ?? 'free';
  }}
  const label = user.first + ' ' + user.last;
  if (label.length > 40 || user.email === null) {{
    return {{ ...base, label: label.slice(0, 40) }};
  }}
  return {{ ...base, label }};
}}
"
    )
}

#[test]
fn twin_wrapper_that_calls_the_other_function_is_not_a_candidate() {
    let mapper = format!("export function mapUser{}", mapper_body("convert"));
    let wrapper = |import: &str| {
        format!(
            "{import}
export function toApiUser{}",
            mapper_body("mapUser")
        )
    };
    let wrapped = files(&[
        ("src/mapper.ts", &mapper),
        (
            "src/api.ts",
            &wrapper("import { mapUser } from './mapper';"),
        ),
    ]);
    assert_eq!(index_of(&wrapped).families.len(), 1, "the detector sees it");
    assert!(
        found(RULE, &wrapped).is_empty(),
        "a wrapper over the other member is no copy"
    );
    // The same two bodies without the import are two implementations under two names.
    let loose = files(&[("src/mapper.ts", &mapper), ("src/api.ts", &wrapper(""))]);
    assert_eq!(found(RULE, &loose).len(), 1);
}

#[test]
fn twin_partial_body_overlap_below_ninety_percent_is_not_a_naming_candidate() {
    let middle = "  const fixed = Math.round(amount * 100) / 100;
  if (fixed < 0 && code !== null) {
    return '-' + code + ' ' + Math.abs(fixed).toFixed(2);
  }
  if (fixed === 0 || code === undefined) {
    return code + ' free';
  }
  const label = code + ' ' + fixed.toFixed(2);
  return label.length > 12 ? label.slice(0, 12) : label;
";
    let a = format!(
        "export function renderPrice(amount, code) {{
  const prefix = code ?? 'USD';
  if (amount === undefined || amount === null) {{
    return prefix;
  }}
  const unit = amount > 1000 ? 'k' : '';
{middle}}}
"
    );
    let b = format!(
        "export function renderCost(amount, code) {{
  const region = code.region ?? 'EU';
  while (amount > 1e6 && region !== 'XX') {{
    amount = amount / 10;
  }}
  let steps = 0;
{middle}}}
"
    );
    let input = files(&[("src/a.ts", &a), ("src/b.ts", &b)]);
    let index = index_of(&input);
    assert_eq!(
        index.families.len(),
        1,
        "the shared middle is a family: {index:#?}"
    );
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_default_exports_do_not_count_as_different_names() {
    let page = |name: &str| {
        let head = if name.is_empty() {
            "export default function () {".to_string()
        } else {
            format!("export default function {name}() {{")
        };
        format!(
            "{head}
  const fixed = Math.round(1 * 100) / 100;
  if (fixed < 0 && typeof window !== 'undefined') {{
    return '-' + window.name + ' ' + Math.abs(fixed).toFixed(2);
  }}
  if (fixed === 0 || window.name === undefined) {{
    return window.name + ' free';
  }}
  const label = window.name + ' ' + fixed.toFixed(2);
  return label.length > 12 ? label.slice(0, 12) : label;
}}
"
        )
    };
    let input = files(&[
        ("app/a/page.tsx", &page("")),
        ("app/b/page.tsx", &page("Landing")),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(
        found(RULE, &input).is_empty(),
        "an anonymous default export is not a chosen name"
    );
}

#[test]
fn twin_emitted_js_twin_is_ignored() {
    let ts = ts_money("formatPrice", "USD");
    let input = files(&[
        ("src/format.ts", &ts),
        ("src/format.js", &ts_money("formatMoney", "USD")),
    ]);
    assert!(
        found(RULE, &input).is_empty(),
        "a committed emit beside its .ts is not a copy"
    );
}

#[test]
fn twin_test_generated_vendored_files_are_skipped() {
    let a = ts_money("formatPrice", "USD");
    let b = ts_money("formatMoney", "USD");
    for other in [
        "lib/format.test.ts",
        "vendor/money.ts",
        "src/money.generated.ts",
    ] {
        let input = files(&[("lib/format.ts", &a), (other, &b)]);
        assert!(found(RULE, &input).is_empty(), "{other}");
    }
    assert_eq!(
        found(
            RULE,
            &files(&[("lib/format.ts", &a), ("utils/money.ts", &b)])
        )
        .len(),
        1
    );
}
