use super::*;
use crate::arch_checker::{ViolationGrade, SEVERITY_MEDIUM};

fn run(pairs: &[(&str, &str)]) -> Vec<ArchViolation> {
    let files: Vec<(String, String)> = pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    EffectCleanupChecker.check(&RepoView {
        spec: "test/repo",
        files: &files,
    })
}

fn one(path: &str, src: &str) -> Vec<ArchViolation> {
    run(&[(path, src)])
}

// ── positives: an effect starts something and returns nothing ─────────────

const INTERVAL_NO_CLEANUP: &str = "\
import { useEffect, useState } from 'react';

export function Clock() {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    setInterval(() => setNow(Date.now()), 1000);
  }, []);
  return <span>{now}</span>;
}
";

#[test]
fn an_interval_started_by_an_effect_with_no_cleanup_is_reported() {
    let vs = one("src/Clock.tsx", INTERVAL_NO_CLEANUP);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.rule_id, RULE_EFFECT_CLEANUP);
    assert_eq!(v.grade, ViolationGrade::Defect);
    assert_eq!(v.severity, SEVERITY_MEDIUM);
    assert_eq!(v.file, "src/Clock.tsx");
    assert_eq!(v.line, 5, "attributed to the effect call");
    assert_eq!(v.object.as_deref(), Some("setInterval"));
    assert!(v.message.contains("`Clock`"), "{}", v.message);
    assert!(v.message.contains("line 6"), "{}", v.message);
    assert!(v.message.contains("cleanup"), "{}", v.message);
}

#[test]
fn a_window_listener_added_by_an_effect_with_no_cleanup_is_reported() {
    let src = "\
import React from 'react';

export const Layout = () => {
  React.useEffect(() => {
    const onResize = () => console.log(window.innerWidth);
    window.addEventListener('resize', onResize);
  }, []);
  return null;
};
";
    let vs = one("src/Layout.jsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("addEventListener"));
    assert!(vs[0].message.contains("`Layout`"), "{}", vs[0].message);
}

#[test]
fn a_realtime_channel_subscribed_by_an_effect_with_no_cleanup_is_reported() {
    let src = "\
import { useEffect } from 'react';
import { supabase } from '@/lib/supabase';

export function useOrderFeed(onChange) {
  useEffect(() => {
    supabase
      .channel('orders')
      .on('postgres_changes', { event: '*', schema: 'public', table: 'orders' }, onChange)
      .subscribe();
  }, [onChange]);
}
";
    let vs = one("src/hooks/useOrderFeed.ts", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("subscribe"));
    assert!(
        vs[0].message.contains("`useOrderFeed`"),
        "{}",
        vs[0].message
    );
}

#[test]
fn an_early_bare_return_is_not_a_cleanup() {
    // The guard returns nothing; the path that subscribes returns nothing either.
    let src = "\
import { useEffect } from 'react';
export function Presence({ user, socketUrl }) {
  useEffect(() => {
    if (!user) return;
    const ws = new WebSocket(socketUrl);
    ws.onmessage = (e) => console.log(e.data);
  }, [user, socketUrl]);
  return null;
}
";
    let vs = one("src/Presence.jsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("WebSocket"));
}

#[test]
fn use_layout_effect_and_an_aliased_import_are_read() {
    let src = "\
import { useLayoutEffect as useLE } from 'react';
export function Tooltip() {
  useLE(() => {
    document.addEventListener('scroll', reposition);
  });
  return null;
}
";
    let vs = one("src/Tooltip.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
}

#[test]
fn an_auth_state_listener_with_no_cleanup_is_reported() {
    let src = "\
import { useEffect } from 'react';
export function AuthGate({ client, setUser }) {
  useEffect(() => {
    client.auth.onAuthStateChange((_event, session) => setUser(session?.user ?? null));
  }, [client, setUser]);
  return null;
}
";
    let vs = one("src/AuthGate.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("onAuthStateChange"));
}

#[test]
fn two_leaking_effects_are_two_findings_in_line_order() {
    let src = "\
import { useEffect } from 'react';
export function Dashboard() {
  useEffect(() => {
    setInterval(poll, 5000);
  }, []);
  useEffect(() => {
    window.addEventListener('focus', poll);
  }, []);
  return null;
}
";
    let vs = one("src/Dashboard.tsx", src);
    assert_eq!(vs.len(), 2, "{vs:#?}");
    assert_eq!((vs[0].line, vs[1].line), (3, 6));
}

// ── safe twins: the look-alikes that must stay quiet ───────────────────────

#[test]
fn safe_twin_an_interval_that_is_cleared_by_the_returned_cleanup() {
    let src = "\
import { useEffect, useState } from 'react';

export function Clock() {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  return <span>{now}</span>;
}
";
    assert!(one("src/Clock.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_listener_removed_by_the_returned_cleanup() {
    let src = "\
import React from 'react';
export const Layout = () => {
  React.useEffect(() => {
    const onResize = () => console.log(window.innerWidth);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
  return null;
};
";
    assert!(one("src/Layout.jsx", src).is_empty());
}

#[test]
fn safe_twin_a_channel_removed_by_the_returned_cleanup() {
    let src = "\
import { useEffect } from 'react';
import { supabase } from '@/lib/supabase';
export function useOrderFeed(onChange) {
  useEffect(() => {
    const channel = supabase
      .channel('orders')
      .on('postgres_changes', { event: '*', schema: 'public', table: 'orders' }, onChange)
      .subscribe();
    return () => {
      supabase.removeChannel(channel);
    };
  }, [onChange]);
}
";
    assert!(one("src/hooks/useOrderFeed.ts", src).is_empty());
}

#[test]
fn safe_twin_a_returned_unsubscribe_function_or_member() {
    let src = "\
import { useEffect } from 'react';
export function A({ store, client, setUser }) {
  useEffect(() => {
    const unsubscribe = store.subscribe(render);
    return unsubscribe;
  }, [store]);
  useEffect(() => {
    const { data } = client.auth.onAuthStateChange((_e, s) => setUser(s?.user));
    return data.subscription.unsubscribe;
  }, [client, setUser]);
  return null;
}
";
    assert!(one("src/A.tsx", src).is_empty());
}

#[test]
fn safe_twin_an_expression_bodied_effect_returns_what_subscribe_returns() {
    let src = "\
import { useEffect } from 'react';
export function B({ store }) {
  useEffect(() => store.subscribe(render), [store]);
  return null;
}
";
    assert!(one("src/B.tsx", src).is_empty());
}

#[test]
fn safe_twin_work_started_only_inside_a_nested_callback_is_not_read() {
    // The start sits in a callback the effect hands to something else; whether that callback
    // runs, and when, is not a fact the tree states.
    let src = "\
import { useEffect } from 'react';
export function C({ loader }) {
  useEffect(() => {
    loader.ready().then(() => {
      setInterval(tick, 1000);
    });
  }, [loader]);
  return null;
}
";
    assert!(one("src/C.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_one_shot_timeout_is_left_to_the_semantic_read() {
    let src = "\
import { useEffect } from 'react';
export function Toast({ onDone }) {
  useEffect(() => {
    setTimeout(onDone, 3000);
  }, [onDone]);
  return null;
}
";
    assert!(one("src/Toast.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_listener_on_an_element_the_effect_created_itself() {
    let src = "\
import { useEffect } from 'react';
export function Preload({ src, setReady }) {
  useEffect(() => {
    const img = new Image();
    img.addEventListener('load', () => setReady(true));
    img.src = src;
  }, [src, setReady]);
  return null;
}
";
    assert!(one("src/Preload.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_once_listener_and_a_self_stopping_interval() {
    let src = "\
import { useEffect } from 'react';
export function D() {
  useEffect(() => {
    window.addEventListener('load', start, { once: true });
  }, []);
  useEffect(() => {
    const id = setInterval(() => {
      if (done()) clearInterval(id);
    }, 100);
  }, []);
  return null;
}
";
    assert!(one("src/D.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_handle_kept_in_a_ref_for_another_cleanup() {
    let src = "\
import { useEffect, useRef } from 'react';
export function E() {
  const timer = useRef(null);
  useEffect(() => {
    timer.current = setInterval(tick, 1000);
  }, []);
  useEffect(() => () => clearInterval(timer.current), []);
  return null;
}
";
    assert!(one("src/E.tsx", src).is_empty());
}

#[test]
fn safe_twin_a_use_effect_that_does_not_come_from_react() {
    let src = "\
import { useEffect } from './my-effects';
export function F() {
  useEffect(() => {
    setInterval(tick, 1000);
  }, []);
  return null;
}
";
    assert!(one("src/F.tsx", src).is_empty());
}

#[test]
fn safe_twin_an_async_effect_callback_is_not_read() {
    // An async callback returns a promise, so React receives no cleanup either way; that shape
    // belongs to the unhandled-rejection and effect rules' semantic read, not to this proof.
    let src = "\
import { useEffect } from 'react';
export function G() {
  useEffect(async () => {
    setInterval(tick, 1000);
  }, []);
  return null;
}
";
    assert!(one("src/G.tsx", src).is_empty());
}

#[test]
fn test_story_and_non_ecmascript_files_are_never_read() {
    for path in [
        "src/Clock.test.tsx",
        "src/Clock.stories.tsx",
        "src/__tests__/Clock.tsx",
        "src/Clock.py",
        "node_modules/x/Clock.tsx",
    ] {
        assert!(one(path, INTERVAL_NO_CLEANUP).is_empty(), "{path}");
    }
}

#[test]
fn malformed_input_degrades_to_no_findings() {
    for src in [
        "",
        "useEffect(",
        "import { useEffect } from 'react'; useEffect(() => { setInterval(",
        "\u{0}\u{1}\u{2}",
    ] {
        let _ = one("src/x.tsx", src);
    }
}

// ── round-1 review: handle hand-offs, extra starts and pinned choices ─────

#[test]
fn safe_twin_a_handle_bound_to_a_const_then_stored_in_a_ref() {
    let src = "\
import { useEffect, useRef } from 'react';
export function E() {
  const timer = useRef(null);
  useEffect(() => {
    const id = setInterval(tick, 1000);
    timer.current = id;
  }, []);
  useEffect(() => () => clearInterval(timer.current), []);
  return null;
}
";
    let vs = one("src/E.tsx", src);
    assert!(vs.is_empty(), "{vs:#?}");
}

#[test]
fn safe_twin_a_connection_bound_to_a_const_then_passed_to_a_setter() {
    let src = "\
import { useEffect, useState } from 'react';
export function S({ url }) {
  const [socket, setSocket] = useState(null);
  useEffect(() => {
    const ws = new WebSocket(url);
    setSocket(ws);
  }, [url]);
  useEffect(() => () => socket?.close(), [socket]);
  return null;
}
";
    let vs = one("src/S.tsx", src);
    assert!(vs.is_empty(), "{vs:#?}");
}

#[test]
fn safe_twin_a_handle_assigned_to_a_variable_declared_outside_the_callback() {
    let src = "\
import { useEffect } from 'react';
export function T() {
  let timerId;
  useEffect(() => {
    timerId = setInterval(tick, 1000);
  }, []);
  useEffect(() => () => clearInterval(timerId), []);
  return null;
}
";
    let vs = one("src/T.tsx", src);
    assert!(vs.is_empty(), "{vs:#?}");
}

#[test]
fn a_handle_bound_to_a_const_and_never_used_again_is_still_reported() {
    let src = "\
import { useEffect } from 'react';
export function U() {
  useEffect(() => {
    const id = setInterval(tick, 1000);
  }, []);
  return null;
}
";
    let vs = one("src/U.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
}

#[test]
fn the_checkers_defects_need_review() {
    assert!(
        EffectCleanupChecker.defects_need_review(),
        "findings are review-only until the checker has run on real repositories"
    );
}

#[test]
fn a_null_guard_before_a_subscription_with_no_cleanup_is_reported() {
    let src = "\
import { useEffect } from 'react';
export function G({ user, client }) {
  useEffect(() => {
    if (!user) return null;
    client.channel('x').subscribe();
  }, [user, client]);
  return null;
}
";
    let vs = one("src/G.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("subscribe"));
}

#[test]
fn safe_twin_a_valued_conditional_return_keeps_the_checker_quiet() {
    // Documented choice: any return that hands React a value silences the checker, even on one
    // branch. Whether the other branch also needs a cleanup is the semantic read's call.
    let src = "\
import { useEffect } from 'react';
export function V({ on }) {
  useEffect(() => {
    const id = setInterval(tick, 1000);
    if (on) return () => clearTimeout(0);
    window.addEventListener('resize', f);
  }, [on]);
  return null;
}
";
    let vs = one("src/V.tsx", src);
    assert!(vs.is_empty(), "{vs:#?}");
}

#[test]
fn an_event_source_opened_by_an_effect_with_no_cleanup_is_reported() {
    let src = "\
import { useEffect } from 'react';
export function F({ url }) {
  useEffect(() => {
    new EventSource(url);
  }, [url]);
  return null;
}
";
    let vs = one("src/F.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("EventSource"));
}

#[test]
fn a_firestore_snapshot_listener_with_no_cleanup_is_reported() {
    let src = "\
import { useEffect } from 'react';
import { onSnapshot } from 'firebase/firestore';
export function Orders({ q, setRows }) {
  useEffect(() => {
    onSnapshot(q, (snap) => setRows(snap.docs));
  }, [q]);
  return null;
}
";
    let vs = one("src/Orders.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("onSnapshot"));
}

#[test]
fn firebase_auth_listeners_with_no_cleanup_are_reported() {
    for call in ["onAuthStateChanged", "onIdTokenChanged"] {
        let src = format!(
            "\
import {{ useEffect }} from 'react';
import {{ {call} }} from 'firebase/auth';
export function Gate({{ auth, setUser }}) {{
  useEffect(() => {{
    {call}(auth, (u) => setUser(u));
  }}, [auth]);
  return null;
}}
"
        );
        let vs = one("src/Gate.tsx", &src);
        assert_eq!(vs.len(), 1, "{call}: {vs:#?}");
        assert_eq!(vs[0].object.as_deref(), Some(call));
    }
}

#[test]
fn a_window_set_interval_with_no_cleanup_is_reported() {
    let src = "\
import { useEffect } from 'react';
export function W() {
  useEffect(() => {
    window.setInterval(tick, 1000);
  }, []);
  return null;
}
";
    let vs = one("src/W.tsx", src);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert_eq!(vs[0].object.as_deref(), Some("setInterval"));
}
