//! The grant rules: OPEN (1), TEST-MODE (2), AUTH-ONLY (3), RECURSIVE-WILDCARD (6) and the
//! Storage grants (7).
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
use super::{
    RULE_AUTH_ONLY, RULE_OPEN, RULE_RECURSIVE_WILDCARD, RULE_STORAGE_OPEN, RULE_TEST_MODE,
};
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
pub(crate) struct Facts {
    pub write: Option<(&'static str, ViolationGrade)>,
    pub read: Option<(&'static str, ViolationGrade)>,
}

/// Which breadth of grant a finding reports.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Breadth {
    Open,
    AuthOnly,
}

/// The write-side and read-side facts of a site, by service.
///
/// Firestore: a write that changes or deletes data is high (critical at the root when open); an
/// open create alone is medium (an anonymous submission box is a real pattern that still needs
/// validation). Storage: an upload is itself a write of a file anyone can then fetch, so create
/// counts with the rest. Reads: root grants are asserted, named paths are held for review (G9, G10).
pub(crate) fn facts(site: &Site<'_>, breadth: Breadth) -> Facts {
    let m = &site.allow.methods;
    let storage = site.file.service == Service::Storage;
    let write = if changes_data(m) || (storage && creates(m)) {
        Some(match (breadth, site.root) {
            (Breadth::Open, true) => (SEVERITY_CRITICAL, ViolationGrade::Defect),
            // Firebase's own Storage example lets any signed-in user write to its `public`
            // folder, and the path vocabulary (G5) says the owner meant that content to be
            // shared: held for review, not asserted.
            (Breadth::AuthOnly, _) if site.public => (SEVERITY_MEDIUM, ViolationGrade::Review),
            _ => (SEVERITY_HIGH, ViolationGrade::Defect),
        })
    } else if creates(m) && breadth == Breadth::Open {
        Some((SEVERITY_MEDIUM, ViolationGrade::Defect))
    } else {
        None
    };
    let read = if !reads(m) {
        None
    } else if site.root {
        // Storage auth-only at the root is the shape of the old default: every file, but the
        // exposure is to account holders, one step below open.
        let severity = if storage && breadth == Breadth::AuthOnly {
            SEVERITY_MEDIUM
        } else {
            SEVERITY_HIGH
        };
        Some((severity, ViolationGrade::Defect))
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
        .map(|(severity, _)| severity_rank(severity))
        .max()
        .unwrap_or(0)
}

/// The wording inputs shared by the write and read sentences of one site.
struct Words {
    where_: String,
    allow: String,
    scope: String,
    path: String,
}

/// The sentence for a grant's write side. `merged` folds the read verb into the list.
fn write_body(site: &Site<'_>, breadth: Breadth, w: &Words, merged: bool) -> String {
    let m = &site.allow.methods;
    let (where_, allow, scope) = (&w.where_, &w.allow, &w.scope);
    let storage = site.file.service == Service::Storage;
    match (storage, breadth) {
        (false, Breadth::Open) => {
            let v = if merged {
                verbs(m, "and")
            } else {
                write_verbs(m, "and")
            };
            format!("{where_} allows anyone on the internet, signed in or not, to {v} {scope} (`{allow}`).")
        }
        (false, Breadth::AuthOnly) if site.root => {
            let what = short_phrase(m);
            format!("{where_} lets any signed-in user {what} {scope} (`{allow}`). Anyone who can create an account, including with anonymous or email sign-in, has full access to all users' data.")
        }
        (false, Breadth::AuthOnly) => {
            let v = if merged {
                verbs(m, "or")
            } else {
                write_verbs(m, "or")
            };
            format!("{where_} lets any signed-in user {v} any document under `{}`, not only their own (`{allow}`).", w.path)
        }
        (true, Breadth::Open) => {
            let v = storage_verbs(m, merged, "and");
            format!("{where_} allows anyone on the internet, signed in or not, to {v} {scope} (`{allow}`).")
        }
        (true, Breadth::AuthOnly) => {
            let v = storage_verbs(m, merged, "or");
            format!(
                "{where_} lets any signed-in user {v} any file {}, not only their own (`{allow}`).",
                if site.root {
                    "in the bucket".to_string()
                } else {
                    format!("under `{}`", w.path)
                }
            )
        }
    }
}

/// Storage names the write verbs by what a client does to a file: upload, replace, delete.
fn storage_verbs(
    methods: &BTreeSet<crate::firebase::rules_parse::Method>,
    with_read: bool,
    joiner: &str,
) -> String {
    use crate::firebase::rules_parse::Method;
    let e = super::grants::expand(methods);
    let mut v = Vec::new();
    if with_read && (e.contains(&Method::Get) || e.contains(&Method::List)) {
        v.push("read");
    }
    if e.contains(&Method::Create) {
        v.push("upload");
    }
    if e.contains(&Method::Update) {
        v.push("replace");
    }
    if e.contains(&Method::Delete) {
        v.push("delete");
    }
    join_words(&v, joiner)
}

/// The sentence for a grant's read side.
fn read_body(site: &Site<'_>, breadth: Breadth, w: &Words) -> String {
    let (where_, allow, scope, path) = (&w.where_, &w.allow, &w.scope, &w.path);
    let storage = site.file.service == Service::Storage;
    match (storage, breadth, site.root) {
        (false, Breadth::Open, true) => format!("{where_} allows anyone to read {scope} (`{allow}`)."),
        (false, Breadth::Open, false) => format!("{where_} allows anyone, signed in or not, to read {scope} (`{allow}`). If `{path}` is meant to be public, no change is needed; if it holds email addresses, phone numbers or anything a user expects to be private, scope the read to the owner."),
        (false, Breadth::AuthOnly, true) => format!("{where_} lets any signed-in user read {scope} (`{allow}`). Anyone who can create an account, including with anonymous or email sign-in, can read all users' data."),
        (false, Breadth::AuthOnly, false) => format!("{where_} lets any signed-in user read {scope} (`{allow}`). If this collection is shared among all users by design, no change is needed; if it holds per-user data, scope reads to the owner."),
        (true, Breadth::Open, _) => format!("{where_} lets anyone with the URL read {scope} (`{allow}`). Verification photos, identity documents and private uploads must not be readable this way; public assets belong under an explicit public folder with its own rule."),
        (true, Breadth::AuthOnly, true) => format!("{where_} lets any signed-in user read {scope} (`{allow}`). Anyone who can create an account, including with anonymous or email sign-in, can fetch every uploaded file."),
        (true, Breadth::AuthOnly, false) => format!("{where_} lets signed-in users read {scope} (`{allow}`). If this folder is shared among all users by design, no change is needed; if it holds per-user files, scope reads to the owner."),
    }
}

/// OPEN and AUTH-ONLY for one service: rules 1 and 3 over Firestore, rule 7 over Storage.
fn breadth_rule(
    sites: &[Site<'_>],
    owned: &Owned,
    breadth: Breadth,
    service: &Service,
) -> Vec<ArchViolation> {
    let wanted = match breadth {
        Breadth::Open => Grant::Open,
        Breadth::AuthOnly => Grant::AuthOnly,
    };
    let rule = match (service, breadth) {
        (Service::Storage, _) => RULE_STORAGE_OPEN,
        (_, Breadth::Open) => RULE_OPEN,
        (_, Breadth::AuthOnly) => RULE_AUTH_ONLY,
    };
    let mut out = Vec::new();
    for site in sites {
        if site.grant != wanted || &site.file.service != service || is_owned(owned, site) {
            continue;
        }
        let f = facts(site, breadth);
        let object = Some(site.display_path());
        let words = Words {
            where_: site.whereabouts(site.allow.line),
            allow: site.allow_text(),
            scope: scope_phrase(site),
            path: site.display_path(),
        };
        let write_rank = f.write.map(|(severity, _)| severity_rank(severity));
        let read_rank = f.read.map(|(s, _)| severity_rank(s));
        let merged = matches!((write_rank, read_rank), (Some(w), Some(r)) if w >= r);

        if let Some((severity, grade)) = f.write {
            let mut body = write_body(site, breadth, &words, merged);
            if grade == ViolationGrade::Review {
                body.push_str(&format!(
                    " The name of `{}` suggests its content is public by design; if every signed-in user is meant to be able to change it, no change is needed.",
                    words.path
                ));
            }
            out.push(finding(
                rule,
                site.file,
                site.allow.line,
                object.clone(),
                severity,
                grade,
                &body,
            ));
        }

        if let Some((severity, grade)) = f.read {
            if merged {
                continue;
            }
            // Silent when a write-side finding of the same or higher severity covers this path:
            // the read is then a clause of that finding, not a second row.
            if f.write.is_none()
                && covering_write_rank(site, sites, breadth) >= severity_rank(severity)
            {
                continue;
            }
            let body = read_body(site, breadth, &words);
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
    breadth_rule(sites, owned, Breadth::Open, &Service::Firestore)
}

/// FIREBASE-RULES-AUTH-ONLY-1 over Firestore (Storage is owned by FIREBASE-STORAGE-RULES-OPEN-1,
/// Realtime Database by its own rule reader; one owner per fact).
pub(crate) fn auth_only(sites: &[Site<'_>], owned: &Owned) -> Vec<ArchViolation> {
    breadth_rule(sites, owned, Breadth::AuthOnly, &Service::Firestore)
}

/// FIREBASE-STORAGE-RULES-OPEN-1: open and auth-only grants in Storage rules.
pub(crate) fn storage(sites: &[Site<'_>], owned: &Owned) -> Vec<ArchViolation> {
    let mut out = breadth_rule(sites, owned, Breadth::Open, &Service::Storage);
    out.append(&mut breadth_rule(
        sites,
        owned,
        Breadth::AuthOnly,
        &Service::Storage,
    ));
    out
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
                (Some((w, wg)), Some((r, g))) => {
                    if severity_rank(w) >= severity_rank(r) {
                        Some((w, wg))
                    } else {
                        Some((r, g))
                    }
                }
                (Some(write), None) => Some(write),
                (None, Some((r, g))) => Some((r, g)),
                (None, None) => None,
            }
        }
        _ => None,
    }
}
