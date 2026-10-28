//! Tests for the clone-rule candidate producer, grouped by rule. Every rule has positive tests
//! (a real shape is a candidate, with the stated facts) and safe twins (the shape the rule must
//! NOT report, next to a positive sibling that proves the guard, not the fixture, is what
//! rejects it).

use std::collections::HashSet;

use super::*;
use crate::extract::clone::{detect, CloneIndex, CloneOptions};

mod funnel;
mod judge;
mod naming;
mod primitive;
mod single_source;

pub(super) fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
        .collect()
}

pub(super) fn armed(rules: &[CloneRule]) -> HashSet<&'static str> {
    rules.iter().map(|r| r.id()).collect()
}

/// The ranked candidates of one rule, with only that rule armed.
pub(super) fn found(rule: CloneRule, files: &[(String, String)]) -> Vec<Candidate> {
    let set = candidates(files, &armed(&[rule]));
    set.by_rule.get(&rule).cloned().unwrap_or_default()
}

/// The detector's index of the same files, to prove a twin is rejected by the rule's filter and
/// not by the detector never seeing the shape.
pub(super) fn index_of(files: &[(String, String)]) -> CloneIndex {
    detect(files, &CloneOptions::default())
}

/// A TypeScript function applying a premium discount, about 100 tokens. Every name and the rate
/// are parameters; the structure is fixed.
pub(super) fn ts_discount(func: &str, order: &str, user: &str, base: &str, rate: &str) -> String {
    format!(
        "import {{ money }} from './money';
export function {func}({order}, {user}) {{
  const {base} = {order}.subtotal * {order}.quantity;
  if ({user}.isPremium) {{
    const discount = {base} * {rate};
    return {base} - discount;
  }}
  if ({base} > 100 && {order}.coupon !== null) {{
    return {base} - 10;
  }}
  return {base} + {order}.shipping;
}}
"
    )
}

pub(super) fn ts_discount_default(rate: &str) -> String {
    ts_discount("applyDiscount", "order", "user", "base", rate)
}

/// A second, structurally unrelated TypeScript decision (shipping cost), about 70 tokens.
pub(super) fn ts_shipping(func: &str, weight: &str, rate: &str) -> String {
    format!(
        "export function {func}(parcel, zone) {{
  let cost = parcel.weight * {rate};
  if (zone.remote === true) {{
    cost = cost + zone.surcharge;
  }}
  if (parcel.weight > {weight} && parcel.fragile !== false) {{
    cost = cost * 2;
  }}
  return cost > 500 ? 500 : cost;
}}
"
    )
}

/// A Python status derivation, an `if`/`elif` ladder over string literals.
pub(super) fn py_status(func: &str, order: &str) -> String {
    format!(
        "from enum import Enum

def {func}({order}, now):
    if {order}.state == 'paid' and {order}.shipped_at is not None:
        return 'complete'
    elif {order}.state == 'paid':
        return 'fulfilling'
    elif {order}.state == 'refunded' or {order}.total < 0:
        return 'closed'
    elif now - {order}.created_at > 30:
        return 'stale'
    else:
        return 'open'
"
    )
}

/// A Rust fee computation: a `match` over a field with numeric arms.
pub(super) fn rs_fee(func: &str, order: &str) -> String {
    format!(
        "use std::fmt;

pub fn {func}({order}: &Order) -> u64 {{
    let base = {order}.amount * {order}.quantity;
    match {order}.tier {{
        1 => base / 20,
        2 => base / 10 + 5,
        3 if base > 1000 => base / 5,
        4 => base / 4 + 20,
        5 if {order}.express => base / 3 + 40,
        _ => 0,
    }}
}}
"
    )
}
