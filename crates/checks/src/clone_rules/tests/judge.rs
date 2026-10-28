//! The bounded judge input: never a whole file.

use super::*;

/// A 45-line function whose lines are all different shapes, so it is one long family.
fn long_function(name: &str) -> String {
    let mut body =
        format!("export function {name}(order, user, config) {{\n  let total = order.base;\n");
    let lines = [
        "  if (user.isPremium && order.base > 100) { total = total - order.base * 0.2; }",
        "  total = total + order.shipping * config.rate;",
        "  if (order.coupon !== null) { total = total - order.coupon.value; }",
        "  if (user.region === 'eu') { total = total * 1.2; }",
        "  while (total > 10000) { total = total / 2; }",
        "  const rounded = Math.round(total * 100) / 100;",
        "  if (rounded < 0) { return 0; }",
        "  for (const fee of config.fees) { total = total + fee.amount; }",
        "  total = total > config.cap ? config.cap : total;",
        "  if (order.gift === true && user.age < 18) { total = total - 5; }",
        "  const tax = total * config.taxRate;",
        "  if (tax > 50 || user.exempt === true) { total = total + 50; }",
        "  switch (user.tier) {",
        "    case 1:",
        "      total = total * 0.9;",
        "      break;",
        "    case 2:",
        "      total = total * 0.8;",
        "      break;",
        "    default:",
        "      total = total * 1.0;",
        "  }",
        "  if (order.items.length > 20 && user.bulk === true) { total = total * 0.95; }",
        "  total = total - (user.credit ?? 0);",
        "  if (total < config.min) { total = config.min; }",
        "  const label = user.name + ' ' + order.id;",
        "  if (label.length > 80) { total = total + 1; }",
        "  total = total + order.items.length * config.perItem;",
        "  if (config.rush === true && order.base < 50) { total = total + 9; }",
        "  const final = Math.floor(total);",
        "  if (final === 0 && order.free !== true) { return 1; }",
        "  if (user.banned === true) { return -1; }",
        "  total = final + order.tip;",
        "  if (total > 99999) { return 99999; }",
        "  while (total % 5 !== 0) { total = total + 1; }",
        "  if (order.currency !== 'usd') { total = total * config.fx; }",
        "  return total;",
    ];
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    body.push_str("}\n");
    body
}

fn long_family(n: usize) -> (Vec<(String, String)>, Candidate) {
    let all: Vec<(String, String)> = (0..n)
        .map(|i| {
            (
                format!("src/shop{i}/total.ts"),
                long_function(&format!("total{i}")),
            )
        })
        .collect();
    let list = found(CloneRule::SingleSourceLogic, &all);
    let top = list.into_iter().next().expect("a candidate");
    (all, top)
}

#[test]
fn judge_input_shows_at_most_three_members_and_thirty_lines_each() {
    let (all, c) = long_family(4);
    assert_eq!(c.member_count(), 4);
    let input = judge_input(&c, &all);
    assert_eq!(input.member_count, 4);
    assert_eq!(input.members.len(), 3, "the rest are counted, not shown");
    for member in &input.members {
        let lines = member.code.lines().count();
        assert!(lines <= MAX_JUDGE_LINES_FOR_TESTS, "{lines} lines");
        assert!(member.truncated, "the function is longer than 30 lines");
        assert!(member
            .enclosing
            .as_deref()
            .is_some_and(|e| e.starts_with("function total")));
    }
    assert_eq!(input.members[0].path, "src/shop0/total.ts");
    assert_eq!(input.rule, RULE_SINGLE_SOURCE);
    assert_eq!(input.id, c.id);
    assert!(input.agree);
    assert!(!input.decision_tokens.is_empty());
}

const MAX_JUDGE_LINES_FOR_TESTS: usize = 30;

#[test]
fn clone_judge_input_code_and_differing_strings_are_redacted() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;
    // Two copies of one decision whose constants are credential literals: the code excerpt, the
    // disagreement pair and the decision tokens all carry them unless the input is redacted.
    let a = planted(SecretShape::StripeLiveSecretKey).value;
    let b = planted(SecretShape::GitHubToken).value;
    let first = ts_discount_default(&format!("'{a}'"));
    let second = ts_discount("quoteTotal", "o", "u", "b", &format!("'{b}'"));
    let all = files(&[("src/cart.ts", &first), ("src/quote.ts", &second)]);
    let c = found(CloneRule::SingleSourceLogic, &all).remove(0);
    let json = serde_json::to_string(&judge_input(&c, &all)).unwrap();
    assert!(
        !json.contains(&a),
        "the first literal reached the judge input"
    );
    assert!(
        !json.contains(&b),
        "the second literal reached the judge input"
    );
    assert!(
        json.contains("sk_live_\u{2026}") && json.contains("ghp_\u{2026}"),
        "the redacted forms stand in: {json}"
    );
}

#[test]
fn clone_judge_input_with_no_secrets_is_unchanged_by_redaction() {
    let (all, c) = long_family(3);
    let json = serde_json::to_string(&judge_input(&c, &all)).unwrap();
    assert!(!json.contains('\u{2026}'));
}

#[test]
fn judge_input_serializes_to_bounded_json_without_whole_files() {
    let (all, c) = long_family(4);
    let json = serde_json::to_string(&judge_input(&c, &all)).unwrap();
    assert!(json.contains("\"member_count\":4"));
    assert!(!json.contains("shop3"), "the fourth member is not shown");
    assert!(json.len() < 12_000, "{} bytes", json.len());
    // The input never carries a line past the thirtieth of a member.
    assert!(
        !json.contains("return 99999"),
        "a late line of the function is not shown"
    );
    assert!(json.contains("user.isPremium"));
}

#[test]
fn judge_input_names_the_disagreements_it_was_given() {
    let a = ts_discount_default("0.2");
    let b = ts_discount("quoteTotal", "o", "u", "b", "0.15");
    let all = files(&[("src/cart.ts", &a), ("src/quote.ts", &b)]);
    let c = found(CloneRule::SingleSourceLogic, &all).remove(0);
    let input = judge_input(&c, &all);
    assert!(!input.agree);
    assert_eq!(
        (
            input.disagreements[0].a.as_str(),
            input.disagreements[0].b.as_str()
        ),
        ("0.2", "0.15")
    );
    assert_eq!(input.members[0].lines, "2-12");
}

#[test]
fn judge_input_for_a_missing_file_shows_no_code_instead_of_failing() {
    let a = ts_discount_default("0.2");
    let all = files(&[("src/cart.ts", &a), ("src/quote.ts", &a)]);
    let c = found(CloneRule::SingleSourceLogic, &all).remove(0);
    let input = judge_input(&c, &[]);
    assert_eq!(input.members.len(), 2);
    assert!(input.members.iter().all(|m| m.code.is_empty()));
}

#[test]
fn a_naming_judge_input_lists_names_and_the_module_pair() {
    let date = |name: &str| {
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
    };
    let number = |name: &str| {
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
    };
    let all = files(&[
        (
            "lib/format.ts",
            &format!("{}{}", date("formatDate"), number("formatNumber")),
        ),
        (
            "utils/format.ts",
            &format!("{}{}", date("fmtDate"), number("numberToText")),
        ),
    ]);
    let c = found(CloneRule::NamingOneConcept, &all).remove(0);
    let input = judge_input(&c, &all);
    assert_eq!(
        input.module_pair,
        Some(["lib/format.ts".to_string(), "utils/format.ts".to_string()])
    );
    assert_eq!(input.pairs.len(), 2);
    assert_eq!(input.names.len(), 4);
    assert_eq!(input.members.len(), 3);
    let json = serde_json::to_value(&input).unwrap();
    assert_eq!(json["rule"], RULE_NAMING);
    assert!(
        json.get("decision_tokens").is_none(),
        "empty lists are omitted"
    );
}

#[test]
fn a_primitive_judge_input_lists_marks_tags_and_hooks() {
    let hook = "import { useEffect } from 'react';
export function useScrollLock(locked) {
  useEffect(() => {
    if (!locked) {
      return;
    }
    const previous = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.body.style.overflow = previous;
    };
  }, [locked]);
}
";
    let all = files(&[
        ("package.json", r#"{ "dependencies": { "react": "18" } }"#),
        ("src/hooks/modal.ts", hook),
        ("src/hooks/drawer.ts", hook),
    ]);
    let c = found(CloneRule::SharedPrimitive, &all).remove(0);
    let input = judge_input(&c, &all);
    assert_eq!(input.hooks, ["useEffect"]);
    assert!(input.mechanism_marks.contains(&"document.".to_string()));
    assert_eq!(
        input.members[0].enclosing.as_deref(),
        Some("function useScrollLock")
    );
}

// ---- cost (W29 Wave C review, D7) ----

/// The widest candidate the judge can be handed: members of thirty lines, every line about two
/// hundred characters, in more files than are shown.
fn widest_family(files_n: usize) -> (Vec<(String, String)>, Candidate) {
    let line = format!(
        "  if (a.b > 1 && c.d !== null) {{ x = x + 1; }} // {}",
        "z".repeat(150)
    );
    let body: String = (0..30).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
    let all: Vec<(String, String)> = (0..files_n)
        .map(|i| {
            (
                format!("src/wide{i}.ts"),
                format!("export function f{i}(a, c) {{\n  let x = 0;\n{body}\n  return x;\n}}\n"),
            )
        })
        .collect();
    let list = found(CloneRule::SingleSourceLogic, &all);
    let top = list
        .into_iter()
        .max_by_key(|c| c.facts.token_len)
        .expect("a candidate");
    (all, top)
}

/// One candidate's serialized judge input is bounded by construction: at most 3 members x 30
/// lines x 120 characters of code (JSON escapes a newline as two bytes), plus short capped lists.
pub(super) const MAX_JUDGE_INPUT_BYTES: usize = 14_000;

#[test]
fn a_source_line_is_cut_at_one_hundred_and_twenty_characters() {
    let (all, c) = widest_family(4);
    let input = judge_input(&c, &all);
    assert_eq!(input.members.len(), 3);
    for member in &input.members {
        let longest = member
            .code
            .lines()
            .map(|l| l.chars().count())
            .max()
            .unwrap();
        assert!(longest <= 120, "{longest} characters in one line");
        assert!(member.code.lines().count() <= 30);
    }
}

#[test]
fn the_widest_judge_input_stays_inside_the_stated_per_candidate_bound() {
    let (all, c) = widest_family(4);
    let bytes = serde_json::to_string(&judge_input(&c, &all)).unwrap().len();
    assert!(
        bytes <= MAX_JUDGE_INPUT_BYTES,
        "{bytes} bytes for one candidate (about {} tokens)",
        bytes / 4
    );
    // A batch is sixteen of them: the bound the cost statement makes.
    assert!(16 * bytes <= 16 * MAX_JUDGE_INPUT_BYTES);
}
