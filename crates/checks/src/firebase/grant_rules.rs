//! The Firestore grant rules: OPEN (1), TEST-MODE (2), AUTH-ONLY (3) and RECURSIVE-WILDCARD (6).
//!
//! All four read the same `allow` sites. They are separate rules because each states a different
//! fact, but one allow must never be reported twice: **G7**, one owner per fact. When
//! RECURSIVE-WILDCARD fires on a broad allow it OWNS that allow, and OPEN, TEST-MODE and
//! AUTH-ONLY skip it.

use std::collections::BTreeSet;

use chrono::NaiveDate;

use super::grants::{
    changes_data, covers, creates, finding, join_words, long_date, overlap, reads, severity_rank,
    short_phrase, verbs, write_verbs, Grant, Site,
};
use super::{RULE_AUTH_ONLY, RULE_OPEN, RULE_RECURSIVE_WILDCARD, RULE_TEST_MODE};
use crate::arch_checker::{
    ArchViolation, ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM,
};
use crate::firebase::rules_parse::{Segment, Service};

/// The allows RECURSIVE-WILDCARD owns, as `(file path, allow line)`.
pub(crate) type Owned = BTreeSet<(String, usize)>;

fn is_owned(owned: &Owned, site: &Site<'_>) -> bool {
    owned.contains(&(site.file.path.clone(), site.allow.line))
}

/// The noun and the "where" of a grant: `every document in the database` at the root,
/// `every document under `/orders/{id}`` otherwise.
fn scope_phrase(site: &Site<'_>) -> String {
    let (noun, whole) = match site.file.service {
        Service::Storage => ("file", "in the bucket"),
        _ => ("document", "in the database"),
    };
    if site.root {
        format!("every {noun} {whole}")
    } else {
        format!("every {noun} under `{}`", site.display_path())
    }
}

/// What a site grants on the write side and on the read side, as `(severity, grade)`.
struct Facts {
    write: Option<&'static str>,
    read: Option<(&'static str, ViolationGrade)>,
}

/// Which breadth of grant a finding reports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Breadth {
    Open,
    AuthOnly,
}

fn facts(site: &Site<'_>, breadth: Breadth) -> Facts {
    let m = &site.allow.methods;
    let write = if changes_data(m) {
        Some(match (breadth, site.root) {
            (Breadth::Open, true) => SEVERITY_CRITICAL,
            _ => SEVERITY_HIGH,
        })
    } else if creates(m) && breadth == Breadth::Open {
        Some(SEVERITY_MEDIUM)
    } else {
        None
    };
    let read = if !reads(m) {
        None
    } else if site.root {
        Some((SEVERITY_HIGH, ViolationGrade::Defect))
    } else if site.public {
        None
    } else {
        Some((SEVERITY_MEDIUM, ViolationGrade::Review))
    };
    Facts { write, read }
}

/// The strongest write-side severity among `sites` that share this site's block, or sit in an
/// ancestor recursive-wildcard block covering it.
fn covering_write_rank(site: &Site<'_>, all: &[Site<'_>], breadth: Breadth) -> u8 {
    let v2 = site.file.parsed.version == Some(2);
    all.iter()
        .filter(|s| s.file.path == site.file.path)
        .filter(|s| match breadth {
            Breadth::Open => s.grant == Grant::Open,
            Breadth::AuthOnly => s.grant == Grant::AuthOnly,
        })
        .filter(|s| {
            std::ptr::eq(s.block, site.block)
                || (matches!(
                    s.block.effective_path.last(),
                    Some(Segment::Wildcard {
                        recursive: true,
                        ..
                    })
                ) && covers(&s.block.effective_path, &site.block.effective_path, v2))
        })
        .filter_map(|s| facts(s, breadth).write)
        .map(severity_rank)
        .max()
        .unwrap_or(0)
}

/// Rules 1 and 3 over the Firestore sites: OPEN and AUTH-ONLY.
fn breadth_rule(sites: &[Site<'_>], owned: &Owned, breadth: Breadth) -> Vec<ArchViolation> {
    let (rule, wanted) = match breadth {
        Breadth::Open => (RULE_OPEN, Grant::Open),
        Breadth::AuthOnly => (RULE_AUTH_ONLY, Grant::AuthOnly),
    };
    let mut out = Vec::new();
    for site in sites {
        if site.grant != wanted || site.file.service != Service::Firestore || is_owned(owned, site)
        {
            continue;
        }
        let f = facts(site, breadth);
        let object = Some(site.display_path());
        let where_ = site.whereabouts(site.allow.line);
        let allow = site.allow_text();
        let scope = scope_phrase(site);
        let write_rank = f.write.map(severity_rank);
        let read_rank = f.read.map(|(s, _)| severity_rank(s));
        let merged = matches!((write_rank, read_rank), (Some(w), Some(r)) if w >= r);
        let m = &site.allow.methods;

        if let Some(severity) = f.write {
            let body = match breadth {
                Breadth::Open => {
                    let verbs = if merged {
                        verbs(m, "and")
                    } else {
                        write_verbs(m, "and")
                    };
                    format!(
                        "{where_} allows anyone on the internet, signed in or not, to {verbs} {scope} (`{allow}`)."
                    )
                }
                Breadth::AuthOnly if site.root => {
                    let what = short_phrase(m);
                    format!(
                        "{where_} lets any signed-in user {what} {scope} (`{allow}`). Anyone who can create an account, including with anonymous or email sign-in, has full access to all users' data."
                    )
                }
                Breadth::AuthOnly => {
                    let verbs = if merged {
                        verbs(m, "or")
                    } else {
                        write_verbs(m, "or")
                    };
                    format!(
                        "{where_} lets any signed-in user {verbs} any document under `{}`, not only their own (`{allow}`).",
                        site.display_path()
                    )
                }
            };
            out.push(finding(
                rule,
                site.file,
                site.allow.line,
                object.clone(),
                severity,
                ViolationGrade::Defect,
                &body,
            ));
        }

        if let Some((severity, grade)) = f.read {
            if merged {
                continue;
            }
            // Silent when a write-side finding of the same or higher severity covers this path:
            // the open read is then a clause of that finding, not a second row.
            if f.write.is_none()
                && covering_write_rank(site, sites, breadth) >= severity_rank(severity)
            {
                continue;
            }
            let path = site.display_path();
            let body = match (breadth, site.root) {
                (Breadth::Open, true) => format!(
                    "{where_} allows anyone to read {scope} (`{allow}`)."
                ),
                (Breadth::Open, false) => format!(
                    "{where_} allows anyone, signed in or not, to read {scope} (`{allow}`). If `{path}` is meant to be public, no change is needed; if it holds email addresses, phone numbers or anything a user expects to be private, scope the read to the owner."
                ),
                (Breadth::AuthOnly, true) => format!(
                    "{where_} lets any signed-in user read {scope} (`{allow}`). Anyone who can create an account, including with anonymous or email sign-in, can read all users' data."
                ),
                (Breadth::AuthOnly, false) => format!(
                    "{where_} lets any signed-in user read {scope} (`{allow}`). If this collection is shared among all users by design, no change is needed; if it holds per-user data, scope reads to the owner."
                ),
            };
            out.push(finding(
                rule,
                site.file,
                site.allow.line,
                object,
                severity,
                grade,
                &body,
            ));
        }
    }
    out
}

/// FIREBASE-RULES-OPEN-1.
pub(crate) fn open(sites: &[Site<'_>], owned: &Owned) -> Vec<ArchViolation> {
    breadth_rule(sites, owned, Breadth::Open)
}

/// FIREBASE-RULES-AUTH-ONLY-1 over Firestore (Storage is owned by FIREBASE-STORAGE-RULES-OPEN-1,
/// Realtime Database by its own rule; one owner per fact).
pub(crate) fn auth_only(sites: &[Site<'_>], owned: &Owned) -> Vec<ArchViolation> {
    breadth_rule(sites, owned, Breadth::AuthOnly)
}

/// FIREBASE-RULES-TEST-MODE-1 over Firestore and Storage.
pub(crate) fn test_mode(sites: &[Site<'_>], owned: &Owned, today: NaiveDate) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    for site in sites {
        let Grant::TestMode(expiry) = site.grant else {
            continue;
        };
        if is_owned(owned, site) {
            continue;
        }
        let where_ = site.whereabouts(site.allow.line);
        let date = long_date(expiry);
        let condition = site.allow.condition.clone().unwrap_or_default();
        let object = Some(site.display_path());
        if expiry >= today {
            let severity = if site.root {
                SEVERITY_CRITICAL
            } else {
                SEVERITY_HIGH
            };
            let what = short_phrase(&site.allow.methods);
            let body = format!(
                "{where_} is the console's test-mode scaffold: it allows anyone to {what} {} until {date} (`{condition}`). Firebase's template says this rule is for getting started and leaves the app open to attackers until it expires.",
                scope_phrase(site)
            );
            out.push(finding(
                RULE_TEST_MODE,
                site.file,
                site.allow.line,
                object,
                severity,
                ViolationGrade::Defect,
                &body,
            ));
        } else {
            let body = format!(
                "{where_} is the console's test-mode scaffold with an expiry of {date}, which has passed. As written, every client request is now denied. If the app still works, the rules running in the console are not the rules in this repository; if the app does not, this is why. Either way, write real rules and deploy them."
            );
            out.push(finding(
                RULE_TEST_MODE,
                site.file,
                site.allow.line,
                object,
                SEVERITY_MEDIUM,
                ViolationGrade::Review,
                &body,
            ));
        }
    }
    out
}

/// FIREBASE-RULES-RECURSIVE-WILDCARD-1 over Firestore and Storage. Returns the findings and the
/// allows it owns (G7).
pub(crate) fn recursive_wildcard(
    sites: &[Site<'_>],
    today: NaiveDate,
) -> (Vec<ArchViolation>, Owned) {
    let mut out = Vec::new();
    let mut owned = Owned::new();
    for a in sites {
        // A recursive wildcard anywhere in the path (`/{path=**}/comments/{c}` covers every
        // `comments` subcollection, in v2); `covers` decides what each narrower rule is under.
        if !a.block.effective_path.iter().any(|s| {
            matches!(
                s,
                Segment::Wildcard {
                    recursive: true,
                    ..
                }
            )
        }) {
            continue;
        }
        let breadth = match a.grant {
            Grant::Open | Grant::AuthOnly => a.grant,
            // A scaffold whose date has passed grants nothing now: RULES-TEST-MODE reports it.
            Grant::TestMode(d) if d >= today => a.grant,
            _ => continue,
        };
        let v2 = a.file.parsed.version == Some(2);
        let mut narrower: Vec<&Site<'_>> = sites
            .iter()
            .filter(|n| n.file.path == a.file.path)
            .filter(|n| !std::ptr::eq(n.block, a.block))
            .filter(|n| covers(&a.block.effective_path, &n.block.effective_path, v2))
            .filter(|n| matches!(n.grant, Grant::Lock | Grant::Other))
            .filter(|n| overlap(&a.allow.methods, &n.allow.methods))
            .collect();
        if narrower.is_empty() {
            continue;
        }
        // The grade and severity are those of the broad grant itself.
        let Some((severity, grade)) = broad_grade(a, breadth) else {
            continue;
        };
        narrower.sort_by_key(|n| n.allow.line);
        narrower.dedup_by_key(|n| n.allow.line);
        let lines: Vec<String> = narrower.iter().map(|n| n.allow.line.to_string()).collect();
        let line_words: Vec<&str> = lines.iter().map(String::as_str).collect();
        let paths: Vec<String> = narrower
            .iter()
            .map(|n| format!("`{}`", n.display_path()))
            .collect();
        let path_words: Vec<&str> = paths.iter().map(String::as_str).collect();
        let plural = if narrower.len() == 1 { "" } else { "s" };
        let who = match breadth {
            Grant::Open => "to anyone, signed in or not,",
            Grant::AuthOnly => "to any signed-in user",
            _ => "to anyone until the scaffold expires,",
        };
        let what = short_phrase(&a.allow.methods);
        let body = format!(
            "{} grants {what} {who} on `{}`. Because access is granted when ANY matching rule allows it, this grant makes the narrower rule{plural} at line{plural} {} ({}) ineffective: a request they would deny is allowed by line {}.",
            a.whereabouts(a.allow.line),
            a.display_path(),
            join_words(&line_words, "and"),
            join_words(&path_words, "and"),
            a.allow.line
        );
        owned.insert((a.file.path.clone(), a.allow.line));
        out.push(finding(
            RULE_RECURSIVE_WILDCARD,
            a.file,
            a.allow.line,
            Some(a.display_path()),
            severity,
            grade,
            &body,
        ));
    }
    (out, owned)
}

/// The severity and grade a broad grant carries under rules 1, 2 and 3, or `None` when those
/// rules would not report it (a public read, an auth-only create).
fn broad_grade(site: &Site<'_>, breadth: Grant) -> Option<(&'static str, ViolationGrade)> {
    match breadth {
        Grant::TestMode(_) => Some((
            if site.root {
                SEVERITY_CRITICAL
            } else {
                SEVERITY_HIGH
            },
            ViolationGrade::Defect,
        )),
        Grant::Open | Grant::AuthOnly => {
            let b = if breadth == Grant::Open {
                Breadth::Open
            } else {
                Breadth::AuthOnly
            };
            let f = facts(site, b);
            match (f.write, f.read) {
                (Some(w), Some((r, g))) => {
                    if severity_rank(w) >= severity_rank(r) {
                        Some((w, ViolationGrade::Defect))
                    } else {
                        Some((r, g))
                    }
                }
                (Some(w), None) => Some((w, ViolationGrade::Defect)),
                (None, Some((r, g))) => Some((r, g)),
                (None, None) => None,
            }
        }
        _ => None,
    }
}
