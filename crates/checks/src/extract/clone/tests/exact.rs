//! `exact_diff`: whether the members of a found family agree, and the token pair and lines that
//! say so when they do not.

use super::*;

/// The Exact-mode disagreements between the first two members of the only family found.
fn disagreements(
    lang: SourceLang,
    path_a: &str,
    a: &str,
    path_b: &str,
    b: &str,
) -> Vec<Disagreement> {
    let found = families_of(&files(&[(path_a, a), (path_b, b)]));
    assert_eq!(found.len(), 1, "fixture must form one family: {found:#?}");
    let family = &found[0];
    assert_eq!(family.members.len(), 2);
    let (first, second) = (&family.members[0], &family.members[1]);
    assert_eq!(first.file, path_a, "members are sorted by path");
    let a_items = tokens(lang, a, TokenMode::Exact);
    let b_items = tokens(lang, b, TokenMode::Exact);
    exact_diff(
        &a_items[first.item as usize],
        first,
        &b_items[second.item as usize],
        second,
    )
}

fn pair(d: &Disagreement) -> (&str, &str) {
    (d.a.as_str(), d.b.as_str())
}

#[test]
fn copies_under_renamed_locals_agree() {
    let a = ts_discount("applyDiscount", "order", "user", "base", "0.2");
    let b = ts_discount("quoteTotal", "cart", "account", "amount", "0.2");
    assert!(disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b).is_empty());
}

#[test]
fn a_rate_difference_is_a_named_disagreement_with_both_lines() {
    let a = ts_discount_default("0.2");
    let b = ts_discount_default("0.15");
    let found = disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("0.2", "0.15"));
    assert_eq!((found[0].line_a, found[0].line_b), (5, 5));
}

#[test]
fn lines_are_per_file() {
    let a = ts_discount_default("0.2");
    let b = format!(
        "// padding\n// padding\n// padding\n{}",
        ts_discount_default("0.15")
    );
    let found = disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].line_a, found[0].line_b), (5, 8));
}

#[test]
fn a_nullish_versus_or_difference_is_named() {
    let make = |op: &str| {
        format!(
            "export function label(item, user) {{
  const name = item.title {op} item.slug;
  if (user.locale === 'fr' && item.count > 1) {{
    return name + 's' + item.suffix;
  }}
  return name.trim().toLowerCase() + user.separator;
}}
"
        )
    };
    let found = disagreements(
        SourceLang::TypeScript,
        "a.ts",
        &make("??"),
        "b.ts",
        &make("||"),
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("??", "||"));
    assert_eq!((found[0].line_a, found[0].line_b), (2, 2));
}

#[test]
fn a_property_name_difference_is_a_named_disagreement() {
    let a = ts_discount_default("0.2");
    let b = a.replace("order.coupon", "order.voucher");
    let found = disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("coupon", "voucher"));
}

#[test]
fn a_comparison_operator_difference_is_a_named_disagreement() {
    let a = ts_discount_default("0.2");
    let b = a.replace("base > 100", "base >= 100");
    let found = disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), (">", ">="));
    assert_eq!(found[0].line_a, 8);
}

#[test]
fn a_string_case_difference_is_a_named_disagreement() {
    let a = py_status("status", "order");
    let b = a.replace("'closed'", "'CLOSED'");
    let found = disagreements(SourceLang::Python, "a.py", &a, "b.py", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("closed", "CLOSED"));
}

#[test]
fn several_differences_are_listed_in_token_order() {
    let a = ts_discount_default("0.2");
    let b = a
        .replace("0.2", "0.15")
        .replace("coupon", "voucher")
        .replace("- 10", "- 12");
    let found = disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b);
    let pairs: Vec<(&str, &str)> = found.iter().map(pair).collect();
    assert_eq!(
        pairs,
        [("0.2", "0.15"), ("coupon", "voucher"), ("10", "12")]
    );
    let positions: Vec<u32> = found.iter().map(|d| d.position).collect();
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(positions, sorted);
}

#[test]
fn a_rust_match_arm_difference_is_a_named_disagreement() {
    let a = rs_fee("fee", "order");
    let b = a.replace("base / 20", "base / 25");
    let found = disagreements(SourceLang::Rust, "a.rs", &a, "b.rs", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("20", "25"));
}

#[test]
fn a_formatting_only_difference_agrees() {
    let a = ts_discount_default("0.2");
    let b = a
        .replace("0.2", ".20")
        .replace("'", "\"")
        .replace("{\n", "{ \n");
    assert!(disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b).is_empty());
}

#[test]
fn spans_of_different_length_are_not_reported_as_agreeing() {
    let a = ts_discount_default("0.2");
    let items = tokens(SourceLang::TypeScript, &a, TokenMode::Exact);
    let full = CodeSpan {
        file: "a.ts".into(),
        item: 0,
        token_start: 0,
        token_len: 10,
        start_line: 2,
        end_line: 2,
    };
    let longer = CodeSpan {
        token_len: 12,
        ..full.clone()
    };
    let found = exact_diff(&items[0], &full, &items[0], &longer);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0].position, 10);
    assert_eq!(found[0].a, "");
    assert!(!found[0].b.is_empty());
}

#[test]
fn a_span_past_its_item_is_a_disagreement_not_a_panic() {
    let a = ts_discount_default("0.2");
    let items = tokens(SourceLang::TypeScript, &a, TokenMode::Exact);
    let outside = CodeSpan {
        file: "a.ts".into(),
        item: 0,
        token_start: u32::try_from(items[0].tokens.len()).unwrap() - 2,
        token_len: 5,
        start_line: 1,
        end_line: 1,
    };
    let found = exact_diff(&items[0], &outside, &items[0], &outside);
    assert_eq!(found.len(), 3, "{found:#?}");
}

#[test]
fn structural_tokens_diff_by_id_when_no_text_was_recorded() {
    let a = tokens(
        SourceLang::TypeScript,
        &ts_discount_default("0.2"),
        TokenMode::Structural,
    );
    let b = tokens(
        SourceLang::TypeScript,
        &ts_discount_default("0.2").replace("coupon", "voucher"),
        TokenMode::Structural,
    );
    // Structural collapses the property name, so these agree.
    let span = CodeSpan {
        file: "a.ts".into(),
        item: 0,
        token_start: 0,
        token_len: u32::try_from(a[0].tokens.len()).unwrap(),
        start_line: 2,
        end_line: 12,
    };
    assert!(exact_diff(&a[0], &span, &b[0], &span).is_empty());
    // Two genuinely different token streams are reported, with the id in hex as the text.
    let other = tokens(
        SourceLang::TypeScript,
        "export function f() { return 1 + 2; }\n",
        TokenMode::Structural,
    );
    let found = exact_diff(&a[0], &span, &other[0], &span);
    assert!(!found.is_empty());
    assert!(found[0].a.starts_with('#') || found[0].a.is_empty());
}

#[test]
fn span_tokens_returns_the_tokens_of_the_span() {
    let items = tokens(
        SourceLang::TypeScript,
        &ts_discount_default("0.2"),
        TokenMode::Structural,
    );
    let span = CodeSpan {
        file: "a.ts".into(),
        item: 0,
        token_start: 3,
        token_len: 4,
        start_line: 1,
        end_line: 1,
    };
    assert_eq!(span_tokens(&items[0], &span), &items[0].tokens[3..7]);
    let beyond = CodeSpan {
        token_start: 100_000,
        ..span
    };
    assert!(span_tokens(&items[0], &beyond).is_empty());
}

// ---- callee names (W29 Wave C review, D3) ----
//
// A free identifier collapses in Exact mode (a rename of a local is not a disagreement), but the
// name of the function a call invokes is a fact about WHAT the code does: two copies that call
// `computeDiscount` and `computeSurcharge` do not agree. The family search runs in Structural
// mode, which is unchanged, so such copies are still found as one family.

fn discount_with_callee(callee: &str) -> String {
    format!(
        "export function quote(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = {callee}(base, 0.2);
    return base - discount;
  }}
  if (base > 100 && order.coupon !== null) {{
    return base - 10;
  }}
  return base + order.shipping;
}}
"
    )
}

#[test]
fn a_different_callee_is_a_named_disagreement_and_the_family_is_still_found() {
    let a = discount_with_callee("computeDiscount");
    let b = discount_with_callee("computeSurcharge");
    let found = disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("computeDiscount", "computeSurcharge"));
    assert_eq!((found[0].line_a, found[0].line_b), (4, 4));
}

#[test]
fn a_renamed_local_and_a_renamed_declaration_still_agree() {
    let a = discount_with_callee("computeDiscount");
    let b = a
        .replace("function quote", "function priceIt")
        .replace("order", "cart")
        .replace("base", "amount")
        .replace("discount", "cut");
    // `computeDiscount` contains `Discount` with a capital D, which the lower-case replace
    // leaves alone, so the callee text is unchanged.
    assert!(disagreements(SourceLang::TypeScript, "a.ts", &a, "b.ts", &b).is_empty());
}

#[test]
fn a_different_constructor_after_new_is_a_named_disagreement() {
    let make = |class: &str| {
        format!(
            "export function build(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium && base > 100) {{
    return new {class}(base, 0.2);
  }}
  if (order.coupon !== null) {{
    return new {class}(base, 0.1);
  }}
  return base + order.shipping;
}}
"
        )
    };
    let found = disagreements(
        SourceLang::TypeScript,
        "a.ts",
        &make("PremiumPrice"),
        "b.ts",
        &make("GuestPrice"),
    );
    // One per `new` (the producer reports the repeated pair once).
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(found
        .iter()
        .all(|d| pair(d) == ("PremiumPrice", "GuestPrice")));
}

#[test]
fn a_python_callee_difference_is_a_named_disagreement() {
    let make = |callee: &str| {
        format!(
            "def quote(order, user):
    base = order.subtotal * order.quantity
    if user.is_premium and base > 100:
        return {callee}(base, 0.2)
    elif order.coupon is not None:
        return base - 10
    return base + order.shipping
"
        )
    };
    let found = disagreements(
        SourceLang::Python,
        "a.py",
        &make("compute_discount"),
        "b.py",
        &make("compute_surcharge"),
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("compute_discount", "compute_surcharge"));
}

#[test]
fn a_rust_callee_difference_is_a_named_disagreement_but_a_renamed_fn_is_not() {
    let a = rs_fee("fee", "order").replace("base / 20", "scale(base, 20)");
    let b = a.replace("scale(", "rescale(");
    let found = disagreements(SourceLang::Rust, "a.rs", &a, "b.rs", &b);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(pair(&found[0]), ("scale", "rescale"));
    // The declaration `fn fee(` is followed by `(` too, but it declares: a rename of it is not a
    // disagreement.
    let renamed = a.replace("pub fn fee(", "pub fn charge(");
    assert!(disagreements(SourceLang::Rust, "a.rs", &a, "b.rs", &renamed).is_empty());
}
