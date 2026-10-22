//! The naming and shared-primitive rules through the seam: only their own affirmative word ships.

use camerata_api_types::audit::FindingKind;

use super::*;

const NAMING: CloneRule = CloneRule::NamingOneConcept;
const UI: CloneRule = CloneRule::SharedPrimitive;

fn money(name: &str, fallback: &str) -> String {
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

fn date(name: &str) -> String {
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

fn number(name: &str) -> String {
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

fn naming_repo(fallback_a: &str, fallback_b: &str) -> Vec<(String, String)> {
    files(&[
        ("lib/format.ts", &money("formatPrice", fallback_a)),
        ("utils/money.ts", &money("formatMoney", fallback_b)),
    ])
}

#[tokio::test]
async fn same_concept_verdict_becomes_a_low_finding_listing_both_names() {
    let all = naming_repo("USD", "USD");
    let stub = StubJudge::answering("same_concept", "the money formatter");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[NAMING])).await;
    assert_eq!(outcome.findings.len(), 1, "{outcome:#?}");
    let f = &outcome.findings[0];
    assert_eq!(f.severity, "low");
    assert_eq!(f.rule_id, NAMING.id());
    assert_eq!(f.confidence.as_deref(), Some("high"));
    assert_eq!(f.kind, Some(FindingKind::Defect));
    assert!(
        f.detail.contains("`formatPrice` in `lib/format.ts:"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("`formatMoney` in `utils/money.ts:"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("they currently agree"), "{}", f.detail);
    assert_eq!(f.also_locations.len(), 1);
    assert_eq!(
        f.captures.get("concept").map(String::as_str),
        Some("the money formatter")
    );
    assert_eq!(f.evaluated_option_id, None);
}

#[tokio::test]
async fn a_disagreement_is_named_but_the_severity_stays_low() {
    let all = naming_repo("USD", "EUR");
    let stub = StubJudge::answering("same_concept", "the money formatter");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[NAMING])).await;
    let f = &outcome.findings[0];
    assert_eq!(f.severity, "low", "the subject of the rule is the name");
    assert!(f.detail.contains("they do not agree"), "{}", f.detail);
    assert!(
        f.detail.contains("`USD`") && f.detail.contains("`EUR`"),
        "{}",
        f.detail
    );
}

#[tokio::test]
async fn different_concept_and_adapter_verdicts_produce_no_finding() {
    for word in ["different_concept", "adapter"] {
        let all = naming_repo("USD", "USD");
        let stub = StubJudge::answering(word, "");
        let outcome = judge(Some(&stub), &all, set_for(&all, &[NAMING])).await;
        assert!(outcome.findings.is_empty(), "{word}");
        assert_eq!(outcome.counts[&NAMING].rejected, 1, "{word}");
    }
}

#[tokio::test]
async fn another_rules_affirmative_word_never_ships_a_naming_finding() {
    let all = naming_repo("USD", "USD");
    for word in ["same_decision", "same_mechanism"] {
        let stub = StubJudge::answering(word, "the x");
        let outcome = judge(Some(&stub), &all, set_for(&all, &[NAMING])).await;
        assert!(outcome.findings.is_empty(), "{word}");
        assert_eq!(outcome.counts[&NAMING].unjudged, 1, "{word}");
    }
}

#[tokio::test]
async fn module_rollup_finding_lists_every_pair() {
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
    let set = set_for(&all, &[NAMING]);
    assert_eq!(
        set.by_rule[&NAMING].len(),
        1,
        "one module candidate, not two function ones"
    );
    let stub = StubJudge::answering("same_concept", "the formatting helpers");
    let outcome = judge(Some(&stub), &all, set).await;
    let f = &outcome.findings[0];
    assert!(
        f.detail
            .contains("`lib/format.ts` and `utils/format.ts` overlap"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("`formatDate` matches `fmtDate`"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("`formatNumber` matches `numberToText`"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("Keep one module"), "{}", f.detail);
    assert_eq!(
        f.also_locations.len(),
        3,
        "four function sites: one row, three also_locations"
    );
    let sent = &stub.request(0).prompt;
    assert!(
        sent.contains("\"module_pair\":[\"lib/format.ts\",\"utils/format.ts\"]"),
        "{sent}"
    );
}

const REACT: &str = r#"{ "name": "web", "dependencies": { "react": "^18.2.0" } }"#;

fn list(label: &str, class: &str) -> String {
    format!(
        "export function List({{ loading, error, rows, onRetry }}) {{
  const total = rows.length;
  return (
    <section>
      {{loading && (
        <div className=\"{class}\" role=\"status\">
          <Spinner size={{24}} />
          <span>{label}</span>
        </div>
      )}}
      {{error && <button onClick={{onRetry}}>Retry</button>}}
      {{rows.map((r) => <Row key={{r.id}} row={{r}} />)}}
    </section>
  );
}}
"
    )
}

fn ui_repo(label_b: &str, class_b: &str) -> Vec<(String, String)> {
    files(&[
        ("package.json", REACT),
        ("components/Orders.tsx", &list("Loading", "spinner")),
        ("features/filters/Invoices.tsx", &list(label_b, class_b)),
        ("app/(shop)/Carts.tsx", &list("Loading", "spinner")),
    ])
}

#[tokio::test]
async fn same_mechanism_verdict_becomes_a_low_finding_counting_components() {
    let all = ui_repo("Please wait", "busy");
    let stub = StubJudge::answering("same_mechanism", "the loading spinner");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[UI])).await;
    assert_eq!(outcome.findings.len(), 1, "{outcome:#?}");
    let f = &outcome.findings[0];
    assert_eq!(f.severity, "low");
    assert_eq!(f.rule_id, UI.id());
    assert!(
        f.detail
            .starts_with("3 components hand-write the same loading spinner:"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("components/Orders.tsx") && f.detail.contains("app/(shop)/Carts.tsx")
    );
    assert!(
        f.detail.contains("apart from variable names and strings"),
        "{}",
        f.detail
    );
    assert!(
        f.detail
            .contains("The strings that differ between the copies are"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("`Loading` and `Please wait`"),
        "{}",
        f.detail
    );
    assert!(f
        .detail
        .contains("Make it one hook or component in the shared UI module"));
    assert_eq!(f.also_locations.len(), 2);
    assert_eq!(
        f.captures.get("mechanism").map(String::as_str),
        Some("the loading spinner")
    );
    assert_eq!(f.evaluated_option_id, None);
}

#[tokio::test]
async fn different_verdict_produces_no_finding_for_a_ui_candidate() {
    let all = ui_repo("Loading", "spinner");
    let stub = StubJudge::answering("different", "");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[UI])).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(outcome.counts[&UI].rejected, 1);
}

#[tokio::test]
async fn one_scan_asks_each_armed_rule_its_own_question() {
    let mut all = discount_repo("0.2", "0.15");
    all.extend(ui_repo("Loading", "spinner"));
    let stub = StubJudge::new(|req, _| {
        // Answer with the affirmative word of whichever rule the request is about.
        let system = req.system.clone().unwrap_or_default();
        let word = if system.contains("same_mechanism") {
            "same_mechanism"
        } else if system.contains("same_concept") {
            "different_concept"
        } else {
            "same_decision"
        };
        Ok(verdicts(&asked_ids(req), word, "the thing"))
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &CloneRule::ALL)).await;
    let by_rule = |rule: CloneRule| {
        outcome
            .findings
            .iter()
            .filter(|f| f.rule_id == rule.id())
            .count()
    };
    // The component is also a repeated decision (a conditional on a field), so the single-source
    // rule has candidates in the UI files as well as the discount pair.
    assert!(by_rule(CloneRule::SingleSourceLogic) >= 1);
    assert_eq!(by_rule(CloneRule::SharedPrimitive), 1);
    assert_eq!(by_rule(CloneRule::NamingOneConcept), 0);
    assert_eq!(
        stub.call_count(),
        3,
        "one batch per armed rule with candidates"
    );
}
