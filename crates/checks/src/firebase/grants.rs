//! The shared model the Firestore / Storage rules read: one [`Site`] per `allow`, with its
//! condition classified, plus path helpers (root detection, public vocabulary, coverage) and the
//! message plumbing every rule uses.
//!
//! The shared false-positive guards of the design (section 6) that live here:
//!
//! - **G3** exact recognisers: [`Grant`] is `Open`, `AuthOnly` or `TestMode` only when the
//!   normalised, inlined condition matches a listed form exactly.
//! - **G4** helper functions are inlined before recognition (see `rules_model`).
//! - **G5** public vocabulary: [`Site::public`].
//! - **G6** `if false` is a lock, not a grant: [`Grant::Lock`].
//! - **G8** version semantics: [`is_root`] and [`covers`].

use std::collections::BTreeSet;

use chrono::NaiveDate;

use super::rules_model::Cond;
use super::rules_parse::{
    render_path, Allow, FlatBlock, MatchBlock, Method, RulesFile, Segment, Service,
};
use crate::arch_checker::{ArchViolation, ViolationGrade};

/// The console caveat every rules finding ends with (the RLS checker's precedent): the repository
/// file is what the next deploy makes live, but console edits never land in it.
pub(crate) const CAVEAT: &str = "This reflects the rules file in the repository. Rules edited in the Firebase console never land in the repository, so confirm against the deployed rules before treating this as settled.";

/// Added when the `firebase.json` beside a conventionally resolved file could not be read.
pub(crate) const CONFIG_UNREADABLE: &str =
    "firebase.json could not be read, so the deployed rules file could not be confirmed.";

/// Path words that mark deliberately public content (G5). A read grant on a path carrying one of
/// them is never reported by the read-side rules (a false negative by design; Firebase's own
/// examples put public files under `/public/`).
const PUBLIC_WORDS: &[&str] = &[
    "public",
    "published",
    "static",
    "assets",
    "catalog",
    "faq",
    "announcements",
    "changelog",
    "legal",
    "locales",
];

/// Where a rules file came from: enough to word a finding about it.
pub(crate) trait Located {
    fn path(&self) -> &str;
    /// The database, bucket or instance the file belongs to, when `firebase.json` named one.
    fn label(&self) -> Option<&str>;
    /// The `firebase.json` beside it could not be read, so the deployed file is unconfirmed.
    fn config_unreadable(&self) -> bool;
}

/// A Firestore or Storage rules file that read cleanly, with where it came from.
pub(crate) struct LoadedRules {
    pub path: String,
    pub label: Option<String>,
    pub config_unreadable: bool,
    pub service: Service,
    pub parsed: RulesFile,
}

impl Located for LoadedRules {
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

/// What an `allow` grants, by its condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Unconditional.
    Open,
    /// Exactly "the caller is signed in".
    AuthOnly,
    /// The `request.time < timestamp.date(...)` scaffold, with its expiry.
    TestMode(NaiveDate),
    /// `if false`: a lock, never a grant (G6).
    Lock,
    /// Anything else: not recognised, so no finding.
    Other,
}

/// One `allow` in its block, ready for the rules.
pub(crate) struct Site<'a> {
    pub file: &'a LoadedRules,
    pub block: &'a MatchBlock,
    pub allow: &'a Allow,
    pub grant: Grant,
    pub root: bool,
    pub public: bool,
}

impl Site<'_> {
    /// The block's path as a reader sees it (`/orders/{id}`).
    pub fn display_path(&self) -> String {
        display_path(&self.file.service, &self.block.effective_path)
    }

    /// `Line 7 of `firestore.rules``, plus the database or bucket when the file belongs to one.
    pub fn whereabouts(&self, line: usize) -> String {
        whereabouts(self.file, line)
    }

    /// The allow as written: `allow read, write: if true`.
    pub fn allow_text(&self) -> String {
        allow_text(self.allow)
    }
}

pub(crate) fn whereabouts(file: &impl Located, line: usize) -> String {
    match file.label() {
        Some(label) => format!("Line {line} of `{}` (the rules for `{label}`)", file.path()),
        None => format!("Line {line} of `{}`", file.path()),
    }
}

pub(crate) fn allow_text(allow: &Allow) -> String {
    let methods: Vec<&str> = allow.methods.iter().map(|m| m.as_str()).collect();
    match &allow.condition {
        Some(c) => format!("allow {}: if {c}", methods.join(", ")),
        None => format!("allow {}", methods.join(", ")),
    }
}

/// Every allow of `file` as a [`Site`].
pub(crate) fn sites<'a>(file: &'a LoadedRules) -> Vec<Site<'a>> {
    let mut out = Vec::new();
    for FlatBlock { block, scope } in file.parsed.flatten() {
        let root = is_root(&file.service, &block.effective_path);
        let public = has_public_segment(&block.effective_path);
        for allow in &block.allows {
            let cond = Cond::new(allow.condition.as_deref(), &scope);
            let grant = classify(&cond);
            out.push(Site {
                file,
                block,
                allow,
                grant,
                root,
                public,
            });
        }
    }
    out
}

fn classify(cond: &Cond) -> Grant {
    if cond.is_false() {
        Grant::Lock
    } else if cond.is_true() {
        Grant::Open
    } else if let Some(expiry) = cond.test_mode_expiry() {
        Grant::TestMode(expiry)
    } else if cond.is_auth_only() {
        Grant::AuthOnly
    } else {
        Grant::Other
    }
}

// ── methods ─────────────────────────────────────────────────────────────────────────────────

/// `read` and `write` expanded into the methods they cover.
pub(crate) fn expand(methods: &BTreeSet<Method>) -> BTreeSet<Method> {
    let mut out = BTreeSet::new();
    for m in methods {
        match m {
            Method::Read => out.extend([Method::Get, Method::List]),
            Method::Write => out.extend([Method::Create, Method::Update, Method::Delete]),
            other => {
                out.insert(*other);
            }
        }
    }
    out
}

/// The grant lets the caller change or remove existing data.
pub(crate) fn changes_data(methods: &BTreeSet<Method>) -> bool {
    let e = expand(methods);
    e.contains(&Method::Update) || e.contains(&Method::Delete)
}

pub(crate) fn creates(methods: &BTreeSet<Method>) -> bool {
    expand(methods).contains(&Method::Create)
}

pub(crate) fn reads(methods: &BTreeSet<Method>) -> bool {
    let e = expand(methods);
    e.contains(&Method::Get) || e.contains(&Method::List)
}

/// Whether two method sets share a method once `read`/`write` are expanded.
pub(crate) fn overlap(a: &BTreeSet<Method>, b: &BTreeSet<Method>) -> bool {
    let (ea, eb) = (expand(a), expand(b));
    ea.intersection(&eb).next().is_some()
}

/// `create, change and delete` (or with `or`).
pub(crate) fn write_verbs(methods: &BTreeSet<Method>, joiner: &str) -> String {
    let e = expand(methods);
    let mut verbs = Vec::new();
    if e.contains(&Method::Create) {
        verbs.push("create");
    }
    if e.contains(&Method::Update) {
        verbs.push("change");
    }
    if e.contains(&Method::Delete) {
        verbs.push("delete");
    }
    join_words(&verbs, joiner)
}

/// `read, create, change, delete`: the verbs a method set grants, reads first.
fn verb_list(methods: &BTreeSet<Method>) -> Vec<&'static str> {
    let e = expand(methods);
    let mut verbs = Vec::new();
    if e.contains(&Method::Get) || e.contains(&Method::List) {
        verbs.push("read");
    }
    if e.contains(&Method::Create) {
        verbs.push("create");
    }
    if e.contains(&Method::Update) {
        verbs.push("change");
    }
    if e.contains(&Method::Delete) {
        verbs.push("delete");
    }
    verbs
}

/// Every verb spelled out: `read, create, change and delete`.
pub(crate) fn verbs(methods: &BTreeSet<Method>, joiner: &str) -> String {
    join_words(&verb_list(methods), joiner)
}

/// `read and write` when the methods cover both fully, otherwise the verbs they do cover.
pub(crate) fn short_phrase(methods: &BTreeSet<Method>) -> String {
    let e = expand(methods);
    let full_write = [Method::Create, Method::Update, Method::Delete]
        .iter()
        .all(|m| e.contains(m));
    let any_write =
        e.contains(&Method::Create) || e.contains(&Method::Update) || e.contains(&Method::Delete);
    let read = e.contains(&Method::Get) || e.contains(&Method::List);
    match (read, full_write, any_write) {
        (true, true, _) => "read and write".to_string(),
        (false, true, _) => "write".to_string(),
        (true, false, false) if e.contains(&Method::Get) && e.contains(&Method::List) => {
            "read".to_string()
        }
        _ => verbs(methods, "and"),
    }
}

pub(crate) fn join_words(words: &[&str], joiner: &str) -> String {
    match words {
        [] => String::new(),
        [only] => (*only).to_string(),
        [head @ .., last] => format!("{} {joiner} {last}", head.join(", ")),
    }
}

// ── paths ───────────────────────────────────────────────────────────────────────────────────

/// The Firestore root `/databases/{database}/documents/{x=**}` or the Storage root
/// `/b/{bucket}/o/{x=**}` (G8): the grant covers every document or file that has a path.
pub(crate) fn is_root(service: &Service, path: &[Segment]) -> bool {
    let (first, third) = match service {
        Service::Firestore => ("databases", "documents"),
        Service::Storage => ("b", "o"),
        Service::Other(_) => return false,
    };
    matches!(
        path,
        [
            Segment::Literal(a),
            Segment::Wildcard { recursive: false, .. },
            Segment::Literal(b),
            Segment::Wildcard { recursive: true, .. },
        ] if a == first && b == third
    )
}

/// The path without the service prefix a reader never writes (`/databases/{d}/documents` or
/// `/b/{bucket}/o`), so `/{document=**}` and `/orders/{id}` read as the client wrote them.
pub(crate) fn display_path(service: &Service, path: &[Segment]) -> String {
    let (first, third) = match service {
        Service::Firestore => ("databases", "documents"),
        Service::Storage => ("b", "o"),
        Service::Other(_) => return render_path(path),
    };
    match path {
        [Segment::Literal(a), Segment::Wildcard { .. }, Segment::Literal(b), rest @ ..]
            if a == first && b == third =>
        {
            render_path(rest)
        }
        _ => render_path(path),
    }
}

/// G5: a path word that marks public content.
pub(crate) fn is_public_word(word: &str) -> bool {
    PUBLIC_WORDS.contains(&word.to_ascii_lowercase().as_str())
}

/// G5: the path carries a word that marks public content.
pub(crate) fn has_public_segment(path: &[Segment]) -> bool {
    path.iter().any(|s| match s {
        Segment::Literal(l) => is_public_word(l),
        _ => false,
    })
}

/// Whether `broad` covers `narrow`: every path `narrow` can match, `broad` also matches. A
/// recursive wildcard matches one or more segments in `v1` and zero or more in `v2` (G8).
pub(crate) fn covers(broad: &[Segment], narrow: &[Segment], v2: bool) -> bool {
    match broad.first() {
        None => narrow.is_empty(),
        Some(Segment::Wildcard {
            recursive: true, ..
        }) => {
            let min = usize::from(!v2);
            (min..=narrow.len()).any(|k| covers(&broad[1..], &narrow[k..], v2))
        }
        Some(b) => match narrow.first() {
            Some(n) => segment_covers(b, n) && covers(&broad[1..], &narrow[1..], v2),
            None => false,
        },
    }
}

fn segment_covers(broad: &Segment, narrow: &Segment) -> bool {
    match (broad, narrow) {
        (Segment::Literal(a), Segment::Literal(b)) => a == b,
        (
            Segment::Wildcard {
                recursive: false, ..
            },
            Segment::Literal(_)
            | Segment::Wildcard {
                recursive: false, ..
            },
        ) => true,
        _ => false,
    }
}

// ── findings ────────────────────────────────────────────────────────────────────────────────

/// Build a finding: the body, then the console caveat, then (when it applies) the unreadable
/// `firebase.json` sentence.
pub(crate) fn finding(
    rule: &'static str,
    file: &impl Located,
    line: usize,
    object: Option<String>,
    severity: &'static str,
    grade: ViolationGrade,
    body: &str,
) -> ArchViolation {
    let mut message = format!("{body} {CAVEAT}");
    if file.config_unreadable() {
        message.push(' ');
        message.push_str(CONFIG_UNREADABLE);
    }
    ArchViolation {
        grade,
        rule_id: rule.to_string(),
        file: file.path().to_string(),
        line,
        object,
        message,
        severity,
    }
}

/// Rank of a severity for "same severity or higher" comparisons.
pub(crate) fn severity_rank(severity: &str) -> u8 {
    match severity {
        crate::arch_checker::SEVERITY_CRITICAL => 4,
        crate::arch_checker::SEVERITY_HIGH => 3,
        crate::arch_checker::SEVERITY_MEDIUM => 2,
        _ => 1,
    }
}

/// `24 November 2026`.
pub(crate) fn long_date(date: NaiveDate) -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month = MONTHS
        .get(date.month0() as usize)
        .copied()
        .unwrap_or("January");
    format!("{} {} {}", date.day(), month, date.year())
}
