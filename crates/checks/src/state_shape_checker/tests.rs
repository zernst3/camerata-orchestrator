use super::*;
use crate::arch_checker::ViolationGrade;

fn run(pairs: &[(&str, &str)]) -> Vec<ArchViolation> {
    let files: Vec<(String, String)> = pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    StateShapeChecker.check(&RepoView {
        spec: "test/repo",
        files: &files,
    })
}

fn one(path: &str, src: &str) -> Vec<ArchViolation> {
    run(&[(path, src)])
}

const FETCHER: &str = "\
import { useState } from 'react';

export function useOrders(api) {
  const [isLoading, setIsLoading] = useState(false);
  const [isError, setIsError] = useState(false);

  async function load() {
    setIsLoading(true);
    setIsError(false);
    try {
      await api.orders();
      setIsLoading(false);
    } catch (e) {
      setIsError(true);
      setIsLoading(false);
    }
  }
  return { isLoading, isError, load };
}
";

// ── exclusive flags: positives ─────────────────────────────────────────────

#[test]
fn two_flags_turned_on_only_while_the_other_is_turned_off_are_one_status() {
    let vs = one("src/useOrders.ts", FETCHER);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.rule_id, RULE_STATE_SHAPE);
    assert_eq!(v.grade, ViolationGrade::Defect);
    assert_eq!(v.severity, crate::arch_checker::SEVERITY_MEDIUM);
    assert_eq!(v.line, 4);
    assert_eq!(v.object.as_deref(), Some("isLoading, isError"));
    assert!(
        v.message.contains("`isLoading` and `isError`"),
        "{}",
        v.message
    );
    assert!(v.message.contains("`useOrders`"), "{}", v.message);
    assert!(v.message.contains("lines 8 and 14"), "{}", v.message);
}

#[test]
fn three_pairwise_exclusive_flags_are_one_finding() {
    let src = "\
import React from 'react';
export const Panel = () => {
  const [showA, setShowA] = React.useState(true);
  const [showB, setShowB] = React.useState(false);
  const [showC, setShowC] = React.useState(false);
  const a = () => { setShowA(true); setShowB(false); setShowC(false); };
  const b = () => { setShowB(true); setShowA(false); setShowC(false); };
  const c = () => { setShowC(true); setShowA(false); setShowB(false); };
  return <div onClick={a} onBlur={b} onFocus={c} />;
};
";
    let vs = one("src/Panel.jsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("showA, showB, showC"));
    assert!(vs[0].message.contains("3 variables"), "{}", vs[0].message);
}

#[test]
fn a_typed_boolean_state_and_an_aliased_hook_import_are_read() {
    let src = "\
import { useState as useS } from 'react';
export function Tabs() {
  const [left, setLeft] = useS<boolean>(true);
  const [right, setRight] = useS<boolean>(false);
  return (
    <div>
      <button onClick={() => { setLeft(true); setRight(false); }} />
      <button onClick={() => { setRight(true); setLeft(false); }} />
    </div>
  );
}
";
    assert_eq!(one("src/Tabs.tsx", src).len(), 1);
}

#[test]
fn switch_cases_are_statement_lists() {
    let src = "\
import { useState } from 'react';
export function useMode() {
  const [editing, setEditing] = useState(false);
  const [viewing, setViewing] = useState(true);
  function go(mode) {
    switch (mode) {
      case 'edit':
        setEditing(true);
        setViewing(false);
        break;
      default:
        setViewing(true);
        setEditing(false);
    }
  }
  return { editing, viewing, go };
}
";
    assert_eq!(one("src/useMode.js", src).len(), 1);
}

// ── exclusive flags: safe twins and traps ──────────────────────────────────

#[test]
fn safe_twin_independent_booleans_reset_together_are_not_one_status() {
    let src = "\
import { useState } from 'react';
export function Form() {
  const [open, setOpen] = useState(false);
  const [dirty, setDirty] = useState(false);
  const reset = () => { setOpen(false); setDirty(false); };
  const edit = () => { setDirty(true); setOpen(false); };
  return <form onReset={reset} onChange={edit} />;
}
";
    // `open` is never turned on, so nothing shows the two are exclusive.
    assert!(one("src/Form.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_toggled_flag_is_not_a_literal_writer() {
    let src = FETCHER.replace("setIsError(true);", "setIsError(!isError);");
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn an_updater_or_computed_value_disqualifies_the_flag() {
    for replacement in ["setIsError(e => !e);", "setIsError(Boolean(e));"] {
        let src = FETCHER.replace("setIsError(true);", replacement);
        assert!(one("src/useOrders.ts", &src).is_empty(), "{replacement}");
    }
}

#[test]
fn a_flag_turned_on_alone_could_be_true_with_the_other() {
    let src = FETCHER.replace(
        "  return { isLoading, isError, load };",
        "  const fail = () => setIsError(true);\n  return { isLoading, isError, load, fail };",
    );
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn both_set_true_together_anywhere_is_not_exclusive() {
    let src = FETCHER.replace(
        "  return { isLoading, isError, load };",
        "  const both = () => { setIsLoading(true); setIsError(true); setIsError(false); };\n  return { isLoading, isError, load, both };",
    );
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn a_pair_split_across_branches_is_unpaired() {
    let src = "\
import { useState } from 'react';
export function useX(flag) {
  const [a, setA] = useState(false);
  const [b, setB] = useState(false);
  function go() {
    if (flag) { setA(true); }
    setB(false);
  }
  function other() { setB(true); setA(false); }
  return { a, b, go, other };
}
";
    assert!(one("src/useX.js", src).is_empty());
}

#[test]
fn a_setter_passed_to_a_child_has_unknown_writers() {
    let src = FETCHER.replace(
        "  return { isLoading, isError, load };",
        "  return { isLoading, isError, load, onError: setIsError };",
    );
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn a_shadowed_setter_is_skipped() {
    let src = FETCHER.replace(
        "  return { isLoading, isError, load };",
        "  const helper = (setIsError) => setIsError(true);\n  return { isLoading, isError, load, helper };",
    );
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn flags_that_both_start_true_are_not_reported() {
    let src = FETCHER
        .replace(
            "useState(false);\n  const [isError",
            "useState(true);\n  const [isError",
        )
        .replace(
            "[isError, setIsError] = useState(false)",
            "[isError, setIsError] = useState(true)",
        );
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn non_boolean_state_is_not_a_flag() {
    let src = "\
import { useState } from 'react';
export function useY() {
  const [count, setCount] = useState(0);
  const [name, setName] = useState('');
  return { count, name, setCount, setName };
}
";
    assert!(one("src/useY.ts", src).is_empty());
}

#[test]
fn use_state_from_a_non_react_module_is_not_read() {
    let src = FETCHER.replace("from 'react'", "from './store'");
    assert!(one("src/useOrders.ts", &src).is_empty());
    let src = FETCHER.replace("import { useState } from 'react';\n", "");
    assert!(one("src/useOrders.ts", &src).is_empty());
}

#[test]
fn preact_hooks_count_as_react() {
    let src = FETCHER.replace("from 'react'", "from 'preact/hooks'");
    assert_eq!(one("src/useOrders.ts", &src).len(), 1);
}

#[test]
fn test_story_generated_and_vendored_files_are_not_read() {
    for path in [
        "src/useOrders.test.ts",
        "src/__tests__/useOrders.ts",
        "src/useOrders.stories.tsx",
        "src/generated/useOrders.ts",
        "node_modules/x/useOrders.js",
        "dist/useOrders.js",
        "src/useOrders.min.js",
    ] {
        assert!(one(path, FETCHER).is_empty(), "{path}");
    }
}

// ── derived state copied by an effect: positives ───────────────────────────

const PROFILE: &str = "\
import { useEffect, useState } from 'react';

export function Profile({ first, last }) {
  const [fullName, setFullName] = useState('');
  useEffect(() => {
    setFullName(first + ' ' + last);
  }, [first, last]);
  return <h1>{fullName}</h1>;
}
";

#[test]
fn an_effect_that_only_copies_a_pure_expression_of_its_dependencies_is_derived_state() {
    let vs = one("src/Profile.jsx", PROFILE);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.line, 5);
    assert_eq!(v.object.as_deref(), Some("fullName"));
    assert!(v.message.contains("`first + ' ' + last`"), "{}", v.message);
    assert!(v.message.contains("`first` and `last`"), "{}", v.message);
    assert!(v.message.contains("`Profile`"), "{}", v.message);
}

#[test]
fn an_expression_body_member_dependencies_and_a_pure_filter_are_derived_state() {
    let src = "\
import * as React from 'react';
export function TodoList(props: { todos: Todo[]; filter: string }) {
  const [visible, setVisible] = React.useState<Todo[]>([]);
  React.useEffect(() => setVisible(props.todos.filter((t: Todo) => t.tag === props.filter)), [props.todos, props.filter]);
  return <ul>{visible.length}</ul>;
}
";
    assert_eq!(one("src/TodoList.tsx", src).len(), 1);
}

#[test]
fn mirroring_a_prop_into_state_is_derived_state() {
    let src = "\
import { useEffect, useState } from 'react';
export function Badge({ count }) {
  const [shown, setShown] = useState(count);
  useEffect(() => { setShown(count); }, [count]);
  return <span>{shown}</span>;
}
";
    assert_eq!(one("src/Badge.jsx", src).len(), 1);
}

// ── derived state: safe twins and traps ────────────────────────────────────

#[test]
fn safe_twin_an_editable_copy_synced_from_a_prop_is_not_derived() {
    let src = PROFILE.replace(
        "  return <h1>{fullName}</h1>;",
        "  return <input value={fullName} onChange={e => setFullName(e.target.value)} />;",
    );
    assert!(one("src/Profile.jsx", &src).is_empty());
}

#[test]
fn the_updater_form_accumulates_and_is_not_derived() {
    let src = PROFILE.replace(
        "setFullName(first + ' ' + last);",
        "setFullName(prev => prev + first);",
    );
    assert!(one("src/Profile.jsx", &src).is_empty());
}

#[test]
fn a_second_statement_a_condition_or_a_cleanup_leaves_the_effect_alone() {
    for body in [
        "setFullName(first + ' ' + last);\n    track(first);",
        "if (first) setFullName(first + ' ' + last);",
        "setFullName(first + ' ' + last);\n    return () => {};",
    ] {
        let src = PROFILE.replace("setFullName(first + ' ' + last);", body);
        assert!(one("src/Profile.jsx", &src).is_empty(), "{body}");
    }
}

#[test]
fn impure_or_external_reads_are_not_derived() {
    for expr in [
        "Date.now()",
        "localStorage.getItem(first)",
        "window.innerWidth + first",
        "ref.current + first",
        "format(first, last)",
        "new Name(first, last)",
        "first + RATE",
    ] {
        let src = PROFILE.replace("first + ' ' + last", expr);
        assert!(one("src/Profile.jsx", &src).is_empty(), "{expr}");
    }
}

#[test]
fn timers_and_promises_are_not_derived() {
    for body in [
        "setTimeout(() => setFullName(first + last), 300);",
        "fetchName(first).then(setFullName);",
    ] {
        let src = PROFILE.replace("setFullName(first + ' ' + last);", body);
        assert!(one("src/Profile.jsx", &src).is_empty(), "{body}");
    }
}

#[test]
fn an_empty_dependency_array_is_a_one_time_copy() {
    let src = PROFILE.replace("[first, last]", "[]");
    assert!(one("src/Profile.jsx", &src).is_empty());
}

#[test]
fn a_dependency_missing_from_the_array_is_not_derived() {
    let src = PROFILE.replace("[first, last]", "[first]");
    assert!(one("src/Profile.jsx", &src).is_empty());
}

#[test]
fn layout_effects_reducers_and_prop_callbacks_are_not_read() {
    let layout = PROFILE.replace("useEffect", "useLayoutEffect").replace(
        "useLayoutEffect, useState",
        "useEffect, useLayoutEffect, useState",
    );
    assert!(one("src/Profile.jsx", &layout).is_empty());
    let callback = "\
import { useEffect } from 'react';
export function Sync({ value, onChange }) {
  useEffect(() => { onChange(value); }, [value]);
  return null;
}
";
    assert!(one("src/Sync.jsx", callback).is_empty());
}

#[test]
fn malformed_input_does_not_panic() {
    for src in [
        "",
        "useState(",
        "const [a, setA] = useState(false",
        "}}}{{{",
        "\u{0}",
    ] {
        let _ = one("src/x.tsx", src);
    }
}

#[test]
fn checker_contract() {
    let checker = StateShapeChecker;
    assert_eq!(checker.rule_ids(), &[RULE_STATE_SHAPE]);
    assert!(checker.advisory_coexisting());
    assert!(!checker.defects_need_review());
    assert!(checker.runs_at_write_time());
    for path in ["a/b.tsx", "a/b.jsx", "a/b.ts", "a/b.js"] {
        assert!(
            crate::arch_checker::matches_any_glob(checker.interest_globs(), path),
            "{path}"
        );
    }
}

#[test]
fn output_is_deterministic_and_free_of_dashes_and_tool_names() {
    let files = [("src/useOrders.ts", FETCHER), ("src/Profile.jsx", PROFILE)];
    let first = run(&files);
    let second = run(&[files[1], files[0]]);
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    for v in &first {
        assert!(!v.message.contains('\u{2014}') && !v.message.contains('\u{2013}'));
        assert!(!v.message.to_ascii_lowercase().contains("camerata"));
    }
}

#[test]
fn a_copy_named_as_lagging_may_mean_the_stale_render() {
    let src = PROFILE
        .replace("fullName", "prevName")
        .replace("setFullName", "setPrevName")
        .replace("setprevName", "setPrevName");
    assert!(one("src/Profile.jsx", &src).is_empty(), "{src}");
}

#[test]
fn a_mirror_that_starts_from_another_value_may_be_a_transition() {
    let src = "\
import { useEffect, useState } from 'react';
export function Fade({ open }) {
  const [visible, setVisible] = useState(false);
  useEffect(() => { setVisible(open); }, [open]);
  return <div className={visible ? 'in' : 'out'} />;
}
";
    assert!(one("src/Fade.jsx", src).is_empty());
}
