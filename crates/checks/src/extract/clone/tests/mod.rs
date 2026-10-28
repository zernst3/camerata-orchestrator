//! Tests for the clone detector, grouped by the part of the pipeline they pin down. Each guard in
//! the spec's trap list (D1 to D10, B1 to B3) has a safe-twin test; each positive sibling proves
//! the same fixture without the guard's trigger is still found.

use super::*;

mod caps;
mod detect;
mod exact;
mod props;
mod skips;
mod tokenize;

#[cfg(test)]
mod perf;

pub(super) fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
        .collect()
}

/// A small deterministic generator (SplitMix64) for the property tests. The workspace has no
/// property-testing dependency; a fixed seed per case keeps every failure reproducible.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n` must be positive).
    pub(super) fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    pub(super) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub(super) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }

    /// A fresh identifier that is never a keyword (it ends in digits).
    pub(super) fn ident(&mut self) -> String {
        const WORDS: &[&str] = &[
            "alpha", "beta", "gamma", "delta", "cart", "quote", "basket", "client", "acct", "unit",
            "line", "ledger", "tally", "sum", "price", "basis", "subject", "target",
        ];
        format!("{}{}", self.pick(WORDS), self.below(10_000))
    }
}

pub(super) fn index_of(files: &[(String, String)]) -> CloneIndex {
    detect(files, &CloneOptions::default())
}

/// The families of the default-options index.
pub(super) fn families_of(files: &[(String, String)]) -> Vec<CloneFamily> {
    index_of(files).families
}

/// The paths of a family's members, in member order.
pub(super) fn member_files(family: &CloneFamily) -> Vec<&str> {
    family.members.iter().map(|m| m.file.as_str()).collect()
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

/// The same function with the default names.
pub(super) fn ts_discount_default(rate: &str) -> String {
    ts_discount("applyDiscount", "order", "user", "base", rate)
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
        _ => 0,
    }}
}}
"
    )
}
