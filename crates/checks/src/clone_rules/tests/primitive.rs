//! UI-SHARED-PRIMITIVE-1: positive tests, then safe twins.

use super::*;

const RULE: CloneRule = CloneRule::SharedPrimitive;

const REACT: &str = r#"{ "name": "web", "dependencies": { "react": "^18.2.0" } }"#;
const SOLID: &str = r#"{ "name": "web", "dependencies": { "solid-js": "^1.8.0" } }"#;

/// A repo with a React package at its root plus the given component files.
fn react_repo(components: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut all = vec![("package.json".to_string(), REACT.to_string())];
    all.extend(files(components));
    all
}

fn one(files: &[(String, String)]) -> Candidate {
    let mut list = found(RULE, files);
    assert_eq!(list.len(), 1, "expected exactly one candidate: {list:#?}");
    list.remove(0)
}

fn marks(c: &Candidate) -> &[String] {
    let RuleFacts::Primitive {
        mechanism_marks, ..
    } = &c.facts.rule
    else {
        panic!("primitive facts expected");
    };
    mechanism_marks
}

const OUTSIDE_CLICK: &str = "  useEffect(() => {
    function onDown(event) {
      if (ref.current && !ref.current.contains(event.target)) {
        onClose();
      }
    }
    document.addEventListener('mousedown', onDown);
    return () => {
      document.removeEventListener('mousedown', onDown);
    };
  }, [ref, onClose]);
";

fn menu() -> String {
    format!(
        "import {{ useEffect, useRef, useState }} from 'react';
export function Menu({{ items, onClose }}) {{
  const ref = useRef(null);
  const [active, setActive] = useState(0);
  const count = items.length;
{OUTSIDE_CLICK}  return <ul ref={{ref}}>{{items.map((i) => <li key={{i.id}}>{{i.label}}</li>)}}</ul>;
}}
"
    )
}

fn popover() -> String {
    format!(
        "import {{ useEffect, useRef, useState }} from 'react';
export function Popover({{ anchor, onClose, children }}) {{
  const ref = useRef(null);
  const [pos, setPos] = useState(null);
  const label = anchor.title;
{OUTSIDE_CLICK}  return <div ref={{ref}} className=\"popover\">{{children}}</div>;
}}
"
    )
}

fn dropdown() -> String {
    format!(
        "import {{ useEffect, useRef }} from 'react';
export function Dropdown({{ options, onClose }}) {{
  const ref = useRef(null);
  const first = options[0];
{OUTSIDE_CLICK}  return <select ref={{ref}}>{{options.map((o) => <option key={{o}}>{{o}}</option>)}}</select>;
}}
"
    )
}

#[test]
fn outside_click_effect_in_three_components_is_a_candidate() {
    let list = found(
        RULE,
        &react_repo(&[
            ("src/components/Menu.tsx", &menu()),
            ("src/components/Popover.tsx", &popover()),
            ("features/filters/Dropdown.tsx", &dropdown()),
        ]),
    );
    // The effect is shared by all three; Menu and Popover also share their prologue, which is
    // a second, partially overlapping family. The three-place family ranks first.
    let c = &list[0];
    assert_eq!(c.member_count(), 3, "{list:#?}");
    let m = marks(c);
    for expected in [
        "useEffect",
        "addEventListener",
        "removeEventListener",
        "ref.current",
        "document.",
        "conditional",
    ] {
        assert!(m.iter().any(|x| x == expected), "{expected} in {m:?}");
    }
    assert!(c.facts.token_len >= 40);
    assert_eq!(
        c.id,
        format!(
            "{}:{:016x}:{}",
            RULE_SHARED_PRIMITIVE, c.family.key, c.family.token_len
        )
    );
}

fn spinner(label: &str, class: &str, retry: &str) -> String {
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
      {{error && <button onClick={{onRetry}}>{retry}</button>}}
      {{rows.map((r) => <Row key={{r.id}} row={{r}} />)}}
    </section>
  );
}}
"
    )
}

#[test]
fn conditional_spinner_block_in_two_components_is_a_candidate() {
    let c = one(&react_repo(&[
        ("src/a/Orders.tsx", &spinner("Loading", "spinner", "Retry")),
        (
            "src/b/Invoices.tsx",
            &spinner("Loading", "spinner", "Retry"),
        ),
    ]));
    let m = marks(&c);
    assert!(m.iter().any(|x| x == "conditional"), "{m:?}");
    assert!(
        m.iter().any(|x| x == "onRetry") || m.iter().any(|x| x == "onClick"),
        "{m:?}"
    );
    let RuleFacts::Primitive { jsx_tags, .. } = &c.facts.rule else {
        panic!();
    };
    assert!(jsx_tags.contains(&"Spinner".to_string()), "{jsx_tags:?}");
}

#[test]
fn empty_state_with_retry_handler_is_a_candidate() {
    let empty = |text: &str, other: &str| {
        format!(
            "export function Results({{ items, onRetry, {other} }}) {{
  return (
    <main>
      {{items.length === 0 && (
        <div className=\"empty\">
          <p>{text}</p>
          <button onClick={{onRetry}}>Try again</button>
        </div>
      )}}
    </main>
  );
}}
"
        )
    };
    let c = one(&react_repo(&[
        ("src/a/Results.tsx", &empty("Nothing here yet", "a")),
        ("src/b/Hits.tsx", &empty("Nothing here yet", "b")),
    ]));
    assert!(marks(&c).iter().any(|x| x == "onClick"), "{:?}", marks(&c));
}

#[test]
fn custom_hook_body_duplicated_across_files_is_a_candidate() {
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
    let c = one(&react_repo(&[
        ("src/hooks/modal.ts", hook),
        ("src/hooks/drawer.ts", hook),
    ]));
    let RuleFacts::Primitive { hooks, .. } = &c.facts.rule else {
        panic!();
    };
    assert_eq!(hooks, &["useEffect".to_string()]);
}

#[test]
fn jsx_file_under_react_package_is_in_scope() {
    let c = one(&react_repo(&[
        ("src/a/Orders.jsx", &spinner("Loading", "spinner", "Retry")),
        (
            "src/b/Invoices.jsx",
            &spinner("Loading", "spinner", "Retry"),
        ),
    ]));
    assert_eq!(c.member_count(), 2);
}

#[test]
fn differing_labels_and_classnames_still_match_and_are_named() {
    let c = one(&react_repo(&[
        ("src/a/Orders.tsx", &spinner("Loading", "spinner", "Retry")),
        (
            "src/b/Invoices.tsx",
            &spinner("Please wait", "busy", "Try again"),
        ),
    ]));
    let RuleFacts::Primitive {
        differing_strings, ..
    } = &c.facts.rule
    else {
        panic!();
    };
    let pairs: Vec<(&str, &str)> = differing_strings
        .iter()
        .map(|d| (d.a.as_str(), d.b.as_str()))
        .collect();
    assert!(pairs.contains(&("spinner", "busy")), "{pairs:?}");
    assert!(pairs.contains(&("Loading", "Please wait")), "{pairs:?}");
    assert!(pairs.contains(&("Retry", "Try again")), "{pairs:?}");
}

// ---- safe twins ----

#[test]
fn twin_static_layout_without_a_mechanism_is_not_a_candidate() {
    let footer = |n: &str| {
        format!(
            "export function Footer{n}() {{
  return (
    <footer className=\"site-footer\">
      <div className=\"columns\">
        <section><h4>Company</h4><a href=\"/about\">About</a><a href=\"/jobs\">Jobs</a><a href=\"/press\">Press</a></section>
        <section><h4>Support</h4><a href=\"/help\">Help</a><a href=\"/status\">Status</a><a href=\"/contact\">Contact</a></section>
      </div>
    </footer>
  );
}}
"
        )
    };
    let input = react_repo(&[
        ("src/a/Footer.tsx", &footer("A")),
        ("src/b/Footer.tsx", &footer("B")),
    ]);
    assert!(
        !index_of(&input).families.is_empty(),
        "the detector sees it"
    );
    assert!(found(RULE, &input).is_empty());
}

fn modal_use(name: &str, extra: &str) -> String {
    format!(
        "import Modal from '../Modal';
export function {name}({{ open, onClose, title }}) {{
{extra}  return (
    <Modal open={{open}} onClose={{onClose}} title={{title}} size=\"lg\" closable footer={{null}} backdrop>
      <p>Are you sure you want to continue with this action today</p>
    </Modal>
  );
}}
"
    )
}

#[test]
fn twin_single_imported_component_with_props_is_composed() {
    let components: Vec<(String, String)> = (0..5)
        .map(|n| {
            (
                format!("src/dialogs/Dialog{n}.tsx"),
                modal_use(&format!("Dialog{n}"), ""),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = components
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let input = react_repo(&refs);
    assert!(
        !index_of(&input).families.is_empty(),
        "the detector sees it"
    );
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_wrapper_with_its_own_conditional_around_an_imported_component_is_still_a_candidate() {
    let own_logic = "  const [seen, setSeen] = useState(false);
  useEffect(() => {
    if (open && !seen) {
      window.setTimeout(() => setSeen(true), 300);
    }
    return () => {
      window.clearTimeout(timer);
    };
  }, [open, seen]);
";
    let input = react_repo(&[
        ("src/dialogs/A.tsx", &modal_use("A", own_logic)),
        ("src/dialogs/B.tsx", &modal_use("B", own_logic)),
    ]);
    let c = one(&input);
    assert!(
        marks(&c)
            .iter()
            .any(|x| x == "useEffect" || x == "conditional"),
        "{:?}",
        marks(&c)
    );
}

#[test]
fn twin_stories_and_tests_are_skipped() {
    let a = spinner("Loading", "spinner", "Retry");
    for path in [
        "src/a/Orders.stories.tsx",
        "src/a/Orders.test.tsx",
        "src/a/__tests__/Orders.tsx",
    ] {
        let input = react_repo(&[("src/b/Invoices.tsx", &a), (path, &a)]);
        assert!(found(RULE, &input).is_empty(), "{path}");
    }
}

#[test]
fn twin_package_without_react_family_dependency_is_out_of_scope() {
    let a = spinner("Loading", "spinner", "Retry");
    let input = files(&[
        ("package.json", SOLID),
        ("src/a/Orders.tsx", &a),
        ("src/b/Invoices.tsx", &a),
    ]);
    assert_eq!(index_of(&input).families.len(), 1, "the detector sees it");
    assert!(found(RULE, &input).is_empty());
    // No manifest at all is also out of scope: the rule would be guessing.
    let bare = files(&[("src/a/Orders.tsx", &a), ("src/b/Invoices.tsx", &a)]);
    assert!(found(RULE, &bare).is_empty());
}

#[test]
fn twin_nearest_manifest_decides_in_a_monorepo() {
    let a = spinner("Loading", "spinner", "Retry");
    let input = files(&[
        ("package.json", REACT),
        ("apps/solid/package.json", SOLID),
        ("apps/solid/src/Orders.tsx", &a),
        ("apps/solid/src/Invoices.tsx", &a),
        ("apps/web/package.json", REACT),
        ("apps/web/src/Orders.tsx", &a),
        ("apps/web/src/Invoices.tsx", &a),
    ]);
    let list = found(RULE, &input);
    assert_eq!(list.len(), 1, "{list:#?}");
    assert!(list[0]
        .facts
        .sites
        .iter()
        .all(|s| s.path.starts_with("apps/web/")));
}

#[test]
fn twin_vue_and_svelte_files_are_never_read() {
    let sfc = "<template>
  <div v-if=\"loading\" class=\"spinner\"><span>Loading</span><button @click=\"retry\">Retry</button></div>
</template>
<script>
export default { methods: { retry() { if (this.loading === false) { this.$emit('retry', 1); } } } };
</script>
";
    for ext in ["vue", "svelte"] {
        let input = react_repo(&[
            (&format!("src/a/Orders.{ext}"), sfc),
            (&format!("src/b/Invoices.{ext}"), sfc),
        ]);
        assert!(found(RULE, &input).is_empty(), "{ext}");
    }
}

#[test]
fn twin_below_forty_tokens_is_not_a_candidate() {
    let small = |n: &str| {
        format!(
            "export function Close{n}({{ onClose }}) {{
  return <button onClick={{onClose}} aria-label=\"Close\">x</button>;
}}
"
        )
    };
    let input = react_repo(&[
        ("src/a/Close.tsx", &small("A")),
        ("src/b/Close.tsx", &small("B")),
    ]);
    assert!(
        index_of(&input).families.is_empty(),
        "under the detector floor"
    );
    assert!(found(RULE, &input).is_empty());
}

#[test]
fn twin_non_component_logic_in_a_react_package_is_not_a_ui_unit() {
    let util = |name: &str| {
        format!(
            "export function {name}(order, rate) {{
  const amount = order.total * rate;
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
    let input = react_repo(&[
        ("src/lib/a.ts", &util("feeA")),
        ("src/lib/b.ts", &util("feeB")),
    ]);
    assert!(
        !index_of(&input).families.is_empty(),
        "the detector sees it"
    );
    assert!(
        found(RULE, &input).is_empty(),
        "no JSX and no hook: not a UI mechanism"
    );
}

#[test]
fn a_mechanism_in_forty_components_is_a_candidate_and_in_forty_one_is_not() {
    let src = spinner("Loading", "spinner", "Retry");
    let copies = |n: usize| -> Vec<(String, String)> {
        let mut all = vec![("package.json".to_string(), REACT.to_string())];
        all.extend((0..n).map(|i| (format!("src/screens/s{i}/List.tsx"), src.clone())));
        all
    };
    let forty = found(RULE, &copies(40));
    assert_eq!(forty.len(), 1);
    assert_eq!(forty[0].member_count(), 40);
    assert!(
        found(RULE, &copies(41)).is_empty(),
        "past 40 places it is a convention, not a copy"
    );
}
