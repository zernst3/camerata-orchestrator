//! Tests: where a `key:` makes Dioxus 0.7.9 remount a component and where it does not.
//!
//! The differ reads `key` only when it compares the children of a list (a `for`, an iterator, or
//! any dynamic slot such as `{ rsx! { .. } }`, `if` and `match`, which hold their element as a
//! one-item list). A keyed component that is the ROOT of a component's `rsx!`, or a plain child
//! of an element, is the same scope re-rendered with new props: its hooks keep their state. The
//! viewer relied on the second reading and showed the first finding's state for every finding
//! (2026-11-20). These tests pin the rule the UI code now follows: a keyed host that must start
//! clean is rendered inside a list (`for k in [key]`). A keyed component nested inside an
//! element is a plain child even when that element sits in an `if` (onboard.rs was the example).

use dioxus::prelude::*;
use std::cell::{Cell, RefCell};

thread_local! {
    static MOUNTS: Cell<usize> = const { Cell::new(0) };
    static KEY: RefCell<Option<Signal<u32>>> = const { RefCell::new(None) };
}

/// A stateful child: its id is taken once, when it mounts.
#[component]
fn Probe(label: u32) -> Element {
    let id = use_hook(|| {
        MOUNTS.with(|m| {
            m.set(m.get() + 1);
            m.get()
        })
    });
    rsx! { span { "p{id}:{label}" } }
}

#[component]
fn AsRoot(key_value: u32) -> Element {
    rsx! { Probe { key: "{key_value}", label: key_value } }
}

#[component]
fn AsPlainChild(key_value: u32) -> Element {
    rsx! { div { Probe { key: "{key_value}", label: key_value } } }
}

#[component]
fn InABlock(key_value: u32) -> Element {
    rsx! {
        div {
            {
                let k = key_value;
                rsx! { Probe { key: "{k}", label: k } }
            }
        }
    }
}

#[component]
fn InAMatchArm(key_value: u32) -> Element {
    rsx! {
        div {
            match Some(key_value) {
                Some(k) => rsx! { Probe { key: "{k}", label: k } },
                None => rsx! {},
            }
        }
    }
}

#[component]
fn InAListOfOne(key_value: u32) -> Element {
    rsx! {
        div {
            for k in [key_value] {
                Probe { key: "{k}", label: k }
            }
        }
    }
}

#[component]
fn InAnIteratorAtTheRoot(key_value: u32) -> Element {
    rsx! {
        {std::iter::once(rsx! { Probe { key: "{key_value}", label: key_value } })}
    }
}

fn mounts_after_changing_the_key(app: fn() -> Element) -> usize {
    MOUNTS.with(|m| m.set(0));
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    let key = KEY.with(|k| k.borrow().expect("the harness ran"));
    vdom.in_runtime(|| {
        let mut key = key;
        key.set(2);
    });
    let _ = vdom.render_immediate_to_vec();
    MOUNTS.with(Cell::get)
}

macro_rules! harness {
    ($name:ident, $comp:ident) => {
        fn $name() -> Element {
            let key = use_signal(|| 1_u32);
            KEY.with(|k| *k.borrow_mut() = Some(key));
            rsx! { $comp { key_value: key() } }
        }
    };
}
harness!(root_app, AsRoot);
harness!(plain_child_app, AsPlainChild);
harness!(block_app, InABlock);
harness!(match_app, InAMatchArm);
harness!(list_app, InAListOfOne);
harness!(iterator_app, InAnIteratorAtTheRoot);

#[test]
fn a_lone_key_on_a_root_or_a_plain_child_does_not_remount() {
    assert_eq!(mounts_after_changing_the_key(root_app), 1);
    assert_eq!(mounts_after_changing_the_key(plain_child_app), 1);
}

#[test]
fn a_key_inside_a_list_a_block_or_a_match_arm_remounts() {
    assert_eq!(mounts_after_changing_the_key(list_app), 2);
    assert_eq!(mounts_after_changing_the_key(block_app), 2);
    assert_eq!(mounts_after_changing_the_key(match_app), 2);
    assert_eq!(mounts_after_changing_the_key(iterator_app), 2);
}
