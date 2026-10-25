//! Realtime Database rules: FIREBASE-RTDB-RULES-OPEN-1 (8) and the RTDB half of
//! FIREBASE-RULES-AUTH-ONLY-1 (3, which Firebase's insecure-rules page lists for all three
//! products and which this rule family hands to rule 3 for the same cascade logic).
//!
//! `.read` and `.write` cascade: a shallower grant takes precedence over a deeper rule, so a node
//! that sets `true` makes every node under it readable or writable no matter what the children
//! say. A grant is therefore reported once, at the node that sets it, and a child that sets
//! `false` under a granting ancestor is reported as ineffective ("improperly inherited rules").

use super::grants::{finding, is_public_word, severity_rank, whereabouts, Located};
use super::rtdb::{Key, RtdbFile, RtdbFlat, RtdbNode, RtdbRule};
use super::rules_model::Cond;
use super::{RULE_AUTH_ONLY, RULE_RTDB_OPEN};
use crate::arch_checker::{
    ArchViolation, ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM,
};

/// A Realtime Database rules file that read cleanly.
pub(crate) struct LoadedRtdb {
    pub path: String,
    pub label: Option<String>,
    pub config_unreadable: bool,
    pub parsed: RtdbFile,
}

impl Located for LoadedRtdb {
    fn path(&self) -> &str {
        &self.path
    }
    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
    fn config_unreadable(&self) -> bool {
        self.config_unreadable
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Breadth {
    Open,
    AuthOnly,
}

impl Breadth {
    fn of(cond: &Cond) -> Option<Breadth> {
        if cond.is_true() {
            Some(Breadth::Open)
        } else if cond.is_auth_only() {
            Some(Breadth::AuthOnly)
        } else {
            None
        }
    }

    fn rule(self) -> &'static str {
        match self {
            Breadth::Open => RULE_RTDB_OPEN,
            Breadth::AuthOnly => RULE_AUTH_ONLY,
        }
    }
}

/// One grant a node sets itself.
struct Grant<'a> {
    key: Key,
    breadth: Breadth,
    rule: &'a RtdbRule,
    severity: &'static str,
    grade: ViolationGrade,
}

fn is_root(node: &RtdbNode) -> bool {
    node.path == "/"
}

fn is_public(node: &RtdbNode) -> bool {
    node.path.split('/').any(is_public_word)
}

/// The grant `node` sets for `key`, with the severity and grade the design assigns it.
fn grant_of(node: &RtdbNode, key: Key) -> Option<Grant<'_>> {
    let rule = node.rule(key)?;
    let breadth = Breadth::of(&rule.cond)?;
    let root = is_root(node);
    let (severity, grade) = match (key, breadth, root) {
        (Key::Write, Breadth::Open, true) => (SEVERITY_CRITICAL, ViolationGrade::Defect),
        (Key::Write, _, _) => (SEVERITY_HIGH, ViolationGrade::Defect),
        (Key::Read, _, true) => (SEVERITY_HIGH, ViolationGrade::Defect),
        (Key::Read, _, false) if is_public(node) => return None,
        (Key::Read, _, false) => (SEVERITY_MEDIUM, ViolationGrade::Review),
    };
    Some(Grant {
        key,
        breadth,
        rule,
        severity,
        grade,
    })
}

/// All findings for one Realtime Database rules file.
pub(crate) fn check(file: &LoadedRtdb) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    for flat in file.parsed.flatten() {
        out.append(&mut grants_at(file, &flat));
        out.append(&mut shadowed_denials(file, &flat));
    }
    out
}

fn grants_at(file: &LoadedRtdb, flat: &RtdbFlat<'_>) -> Vec<ArchViolation> {
    let node = flat.node;
    let write = grant_of(node, Key::Write);
    let read = grant_of(node, Key::Read);
    // A node that grants both with the same breadth, the write at least as severe as the read,
    // is one finding: the read is a clause of the write.
    let merged = matches!((&write, &read), (Some(w), Some(r))
        if w.breadth == r.breadth && severity_rank(w.severity) >= severity_rank(r.severity));
    let mut out = Vec::new();
    for grant in [&write, &read].into_iter().flatten() {
        if merged && grant.key == Key::Read {
            continue;
        }
        out.push(report(file, node, grant, merged));
    }
    out
}

fn report(file: &LoadedRtdb, node: &RtdbNode, grant: &Grant<'_>, merged: bool) -> ArchViolation {
    let where_ = whereabouts(file, grant.rule.line);
    let path = &node.path;
    let key = grant.key.as_str();
    let value = &grant.rule.raw;
    let what = match (grant.key, merged) {
        (Key::Write, true) => "read and write",
        (Key::Write, false) => "write to",
        (Key::Read, _) => "read",
    };
    let target = if is_root(node) {
        "the whole database".to_string()
    } else {
        format!("every node under `{path}`")
    };
    let keys = if merged {
        ".read and .write".to_string()
    } else {
        key.to_string()
    };
    let body = match (grant.breadth, grant.grade, is_root(node)) {
        (Breadth::Open, ViolationGrade::Review, _) => format!(
            "{where_} sets `{key}` to `{value}` on `{path}`, so anyone, signed in or not, can read {target}. If `{path}` is meant to be public, no change is needed; if it holds email addresses, phone numbers or anything a user expects to be private, scope the read to the owner."
        ),
        (Breadth::Open, _, _) => format!(
            "{where_} sets {keys} to `{value}` on `{path}`, so anyone on the internet, signed in or not, can {what} {target}."
        ),
        (Breadth::AuthOnly, ViolationGrade::Review, _) => format!(
            "{where_} sets `{key}` to `{value}` on `{path}`, so any signed-in user can read {target}. If this data is shared among all users by design, no change is needed; if it holds per-user data, scope reads to the owner."
        ),
        (Breadth::AuthOnly, _, true) => format!(
            "{where_} sets {keys} to `{value}` at the root, so any signed-in user can {what} {target}. Anyone who can create an account, including with anonymous or email sign-in, has full access to all users' data."
        ),
        (Breadth::AuthOnly, _, false) => format!(
            "{where_} sets {keys} to `{value}` on `{path}`, so any signed-in user can {what} {target}, not only their own."
        ),
    };
    finding(
        grant.breadth.rule(),
        file,
        grant.rule.line,
        Some(node.path.clone()),
        grant.severity,
        grant.grade,
        &body,
    )
}

/// A node whose `false` is overridden by a granting ancestor ("improperly inherited rules").
fn shadowed_denials(file: &LoadedRtdb, flat: &RtdbFlat<'_>) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    for key in [Key::Read, Key::Write] {
        let Some(denial) = flat.node.rule(key).filter(|r| r.cond.is_false()) else {
            continue;
        };
        // The shallowest ancestor (not this node) whose grant cascades down to it.
        let Some(parent) = flat
            .ancestors
            .iter()
            .copied()
            .find_map(|a| grant_of_any(a, key).map(|g| (a, g)))
        else {
            continue;
        };
        let (ancestor, grant) = parent;
        let ancestor_value = &grant.rule.raw;
        let body = format!(
            "{} sets `{}` to `false` on `{}`, but `{}` grants `{}: {}` (line {}). Rules at a child path can only grant more access, never less, so the denial has no effect.",
            whereabouts(file, denial.line),
            key.as_str(),
            flat.node.path,
            ancestor.path,
            key.as_str(),
            ancestor_value,
            grant.rule.line
        );
        out.push(finding(
            RULE_RTDB_OPEN,
            file,
            denial.line,
            Some(flat.node.path.clone()),
            grant.severity,
            ViolationGrade::Defect,
            &body,
        ));
    }
    out
}

/// Like [`grant_of`] but without the public-path silence: a denial under a public grant is still
/// ineffective, and the fact is checkable from the two lines.
fn grant_of_any(node: &RtdbNode, key: Key) -> Option<Grant<'_>> {
    let rule = node.rule(key)?;
    let breadth = Breadth::of(&rule.cond)?;
    let severity = match (key, breadth, is_root(node)) {
        (Key::Write, Breadth::Open, true) => SEVERITY_CRITICAL,
        (Key::Write, _, _) | (Key::Read, _, true) => SEVERITY_HIGH,
        (Key::Read, _, false) => SEVERITY_MEDIUM,
    };
    Some(Grant {
        key,
        breadth,
        rule,
        severity,
        grade: ViolationGrade::Defect,
    })
}
