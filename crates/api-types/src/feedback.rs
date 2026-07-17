//! `DefectReport` — the Product-Owner feedback loop's INGEST contract (the "Feedback
//! loop: auto-capture + click-to-report" section of
//! `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`).
//!
//! Both feedback sources (a scaffolded app's built-in error reporter posting
//! auto-captured runtime errors, and a human clicking an element in the preview to
//! describe an issue) produce ONE structured shape that Camerata records and surfaces
//! for the orchestrator to act on. This module holds ONLY that shape — the pure-serde
//! wire contract. The store (`camerata_persistence::feedback`) and the server endpoints
//! (`camerata_server`) that ingest/list/update it are later layers built directly on
//! top of this crate's types (unlike the governance-event mirror pattern, this crate IS
//! the canonical definition — `camerata-persistence` depends on it rather than
//! duplicating a hand-mirrored DTO).
//!
//! Conventions honored (mirrors `camerata_persistence::governance_event`):
//! - RUST-DOMAIN-4: explicit fields, no stringly-typed catch-all (the enums are closed
//!   and typed; `DefectContext::extra` is the one deliberately open escape hatch for
//!   forward-compatible structured extras).
//! - SQL-AUDIT-COLUMNS-1: `ts` (RFC3339 UTC) stamped at construction time.
//! - RUST-PURE-STATE-TRANSITIONS-1: builder setters are pure (`mut self -> Self`).
//! - ORCH-NEW-PATH-TESTS-1: unit tests included in this file.

use std::collections::BTreeMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Who/what produced the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DefectSource {
    /// The scaffolded app's built-in error reporter caught this mechanically (wasm
    /// panic hook, `window.onerror`, `unhandledrejection`, a failed-request
    /// interceptor) — no human involved yet.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// A human clicked an element in the preview and described the issue.
    #[serde(rename = "user")]
    User,
}

impl DefectSource {
    /// The stable lowercase wire/column string for this variant.
    pub fn as_str(&self) -> &'static str {
        match self {
            DefectSource::Auto => "auto",
            DefectSource::User => "user",
        }
    }

    /// Parse the stable lowercase string back to a variant. Unrecognized input falls
    /// back to the default ([`DefectSource::Auto`]) rather than erroring — this is a
    /// forward-compatible, best-effort read path (mirrors
    /// `camerata_persistence::governance_event`'s plain-string kind vocabulary).
    pub fn parse(s: &str) -> Self {
        match s {
            "user" => DefectSource::User,
            _ => DefectSource::Auto,
        }
    }
}

/// What kind of defect this report describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DefectKind {
    /// A caught runtime error/exception/rejection in the running app.
    #[serde(rename = "runtime_error")]
    RuntimeError,
    /// A human-authored bug report (click-to-report).
    #[serde(rename = "user_report")]
    UserReport,
    /// A build/compile failure surfaced back to the loop.
    #[serde(rename = "build_error")]
    BuildError,
    /// Anything else — kept open for forward compatibility.
    #[default]
    #[serde(rename = "other")]
    Other,
}

impl DefectKind {
    /// The stable lowercase wire/column string for this variant.
    pub fn as_str(&self) -> &'static str {
        match self {
            DefectKind::RuntimeError => "runtime_error",
            DefectKind::UserReport => "user_report",
            DefectKind::BuildError => "build_error",
            DefectKind::Other => "other",
        }
    }

    /// Parse the stable lowercase string back to a variant, falling back to
    /// [`DefectKind::Other`] for unrecognized input (forward-compatible read path).
    pub fn parse(s: &str) -> Self {
        match s {
            "runtime_error" => DefectKind::RuntimeError,
            "user_report" => DefectKind::UserReport,
            "build_error" => DefectKind::BuildError,
            _ => DefectKind::Other,
        }
    }
}

/// How serious the defect is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DefectSeverity {
    /// Informational — no user-visible impact (e.g. a caught-and-recovered warning).
    #[default]
    #[serde(rename = "info")]
    Info,
    /// Degraded behavior, not blocking.
    #[serde(rename = "warning")]
    Warning,
    /// A user-visible failure.
    #[serde(rename = "error")]
    Error,
    /// A severe, likely app-breaking failure.
    #[serde(rename = "critical")]
    Critical,
}

impl DefectSeverity {
    /// The stable lowercase wire/column string for this variant.
    pub fn as_str(&self) -> &'static str {
        match self {
            DefectSeverity::Info => "info",
            DefectSeverity::Warning => "warning",
            DefectSeverity::Error => "error",
            DefectSeverity::Critical => "critical",
        }
    }

    /// Parse the stable lowercase string back to a variant, falling back to
    /// [`DefectSeverity::Info`] for unrecognized input (forward-compatible read path).
    pub fn parse(s: &str) -> Self {
        match s {
            "warning" => DefectSeverity::Warning,
            "error" => DefectSeverity::Error,
            "critical" => DefectSeverity::Critical,
            _ => DefectSeverity::Info,
        }
    }
}

/// The report's lifecycle status — tracked so the orchestrator (and a human) can see
/// what has and hasn't been acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DefectStatus {
    /// Newly recorded, not yet looked at.
    #[default]
    #[serde(rename = "open")]
    Open,
    /// Seen/triaged, work not necessarily started or finished.
    #[serde(rename = "acknowledged")]
    Acknowledged,
    /// Addressed.
    #[serde(rename = "resolved")]
    Resolved,
}

impl DefectStatus {
    /// The stable lowercase wire/column string for this variant.
    pub fn as_str(&self) -> &'static str {
        match self {
            DefectStatus::Open => "open",
            DefectStatus::Acknowledged => "acknowledged",
            DefectStatus::Resolved => "resolved",
        }
    }

    /// Parse the stable lowercase string back to a variant, falling back to
    /// [`DefectStatus::Open`] for unrecognized input (forward-compatible read path).
    pub fn parse(s: &str) -> Self {
        match s {
            "acknowledged" => DefectStatus::Acknowledged,
            "resolved" => DefectStatus::Resolved,
            _ => DefectStatus::Open,
        }
    }
}

/// Structured context carried alongside a [`DefectReport`] — the "what was going on
/// when this happened" a triager (human or orchestrator) needs to act without asking
/// a follow-up question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DefectContext {
    /// The app route/URL active when the defect occurred, if known.
    #[serde(default)]
    pub route: Option<String>,
    /// A selector/description of the element the user clicked (click-to-report only).
    #[serde(default)]
    pub element: Option<String>,
    /// A captured stack trace, if any.
    #[serde(default)]
    pub stack: Option<String>,
    /// Recent console output, if captured.
    #[serde(default)]
    pub console: Option<String>,
    /// Open, forward-compatible structured extras that don't warrant a dedicated field
    /// yet — the one deliberately stringly-typed escape hatch in this otherwise closed
    /// shape.
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

/// One structured defect report — the feedback loop's shared ingest shape, whether it
/// came from the app's auto-capture reporter or a human's click-to-report.
///
/// # Constructing
///
/// Use [`DefectReport::auto`] or [`DefectReport::user`] (or the general
/// [`DefectReport::new`]), then chain the `with_*` builder setters for whichever
/// optional fields apply. `ts` is stamped `Utc::now()` at construction (mirrors
/// `camerata_persistence::GovernanceEvent`); `id` and `status` start `None`/`Open` and
/// are finalized by the store/server on ingest (the server stamps `id`/`ts`/`status`
/// fresh on every `POST /api/feedback`, regardless of what a caller sent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefectReport {
    /// Row id, assigned by the store. `None` until recorded.
    #[serde(default)]
    pub id: Option<i64>,
    /// Which app/project this report is about.
    pub project_id: String,
    /// Where the report came from.
    #[serde(default)]
    pub source: DefectSource,
    /// What kind of defect this is.
    #[serde(default)]
    pub kind: DefectKind,
    /// A short one-line summary.
    pub title: String,
    /// A longer free-text description.
    #[serde(default)]
    pub description: String,
    /// Structured context (route/element/stack/console/extras).
    #[serde(default)]
    pub context: DefectContext,
    /// How serious this is.
    #[serde(default)]
    pub severity: DefectSeverity,
    /// The report's current lifecycle status.
    #[serde(default)]
    pub status: DefectStatus,
    /// RFC3339 UTC timestamp, stamped at construction.
    #[serde(default = "now_rfc3339")]
    pub ts: String,
    /// A stable dedupe key for "the same underlying defect" (the cheapest-now /
    /// most-expensive-later fold — see the plan doc's "Usability backlog" section,
    /// "fold-in-now" items). `None` when not yet computed: a client MAY compute and
    /// send its own (the scaffolded app's auto-capture reporter does, for its own
    /// client-side rate limiting — see `crates/scaffold`'s `error-reporter.js`); when
    /// absent, the ingest server computes one from `kind` + the top stack frame +
    /// `context.route`. `#[serde(default)]` so older/minimal payloads still deserialize.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// How many times a report with this fingerprint has been seen for this project.
    /// Starts at 1 on first insert; the ingest server increments it in place instead
    /// of inserting a new row when a recent OPEN report with the same fingerprint
    /// already exists (see `camerata_persistence::feedback::FeedbackStore::bump_fingerprint`).
    /// `#[serde(default = "default_count")]` so an older/minimal payload without this
    /// field still deserializes to the sensible default of 1.
    #[serde(default = "default_count")]
    pub count: i64,
}

/// `serde(default = ...)` helper for [`DefectReport::ts`] — a deserialized report that
/// omits `ts` (e.g. a hand-built test fixture, or a minimal client payload the server
/// will re-stamp anyway) gets "now" rather than an empty string.
fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// `serde(default = ...)` helper for [`DefectReport::count`] — a deserialized report
/// that omits `count` gets the sensible default of 1 occurrence, not 0.
fn default_count() -> i64 {
    1
}

impl DefectReport {
    /// The general constructor every ergonomic helper below delegates to. `ts` is
    /// stamped `Utc::now()` at construction; `id` is `None`; `status` defaults to
    /// [`DefectStatus::Open`]; `severity` defaults to [`DefectSeverity::Info`].
    pub fn new(
        project_id: impl Into<String>,
        source: DefectSource,
        kind: DefectKind,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            project_id: project_id.into(),
            source,
            kind,
            title: title.into(),
            description: String::new(),
            context: DefectContext::default(),
            severity: DefectSeverity::default(),
            status: DefectStatus::default(),
            ts: now_rfc3339(),
            fingerprint: None,
            count: 1,
        }
    }

    /// Construct an auto-captured report (the scaffolded app's built-in error reporter).
    pub fn auto(project_id: impl Into<String>, kind: DefectKind, title: impl Into<String>) -> Self {
        Self::new(project_id, DefectSource::Auto, kind, title)
    }

    /// Construct a human-authored report (click-to-report).
    pub fn user(project_id: impl Into<String>, kind: DefectKind, title: impl Into<String>) -> Self {
        Self::new(project_id, DefectSource::User, kind, title)
    }

    /// Attach a longer free-text description (builder-style, chainable).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set the severity (builder-style, chainable).
    pub fn with_severity(mut self, severity: DefectSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Attach the active route/URL (builder-style, chainable).
    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.context.route = Some(route.into());
        self
    }

    /// Attach the clicked element selector/description (builder-style, chainable).
    pub fn with_element(mut self, element: impl Into<String>) -> Self {
        self.context.element = Some(element.into());
        self
    }

    /// Attach a captured stack trace (builder-style, chainable).
    pub fn with_stack(mut self, stack: impl Into<String>) -> Self {
        self.context.stack = Some(stack.into());
        self
    }

    /// Attach captured console output (builder-style, chainable).
    pub fn with_console(mut self, console: impl Into<String>) -> Self {
        self.context.console = Some(console.into());
        self
    }

    /// Attach one structured extra key/value (builder-style, chainable, repeatable).
    pub fn with_extra(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.extra.insert(key.into(), value.into());
        self
    }

    /// Set a client-computed fingerprint (builder-style, chainable). Used by callers
    /// that already know their own stable dedupe key (e.g. the scaffolded app's
    /// auto-capture reporter) — the ingest server honors a client-provided
    /// fingerprint instead of computing its own.
    pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.fingerprint = Some(fingerprint.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── enum defaults ────────────────────────────────────────────────────────

    #[test]
    fn enum_defaults_are_sensible() {
        assert_eq!(DefectSource::default(), DefectSource::Auto);
        assert_eq!(DefectKind::default(), DefectKind::Other);
        assert_eq!(DefectSeverity::default(), DefectSeverity::Info);
        assert_eq!(DefectStatus::default(), DefectStatus::Open);
    }

    #[test]
    fn enum_as_str_and_parse_round_trip() {
        for v in [DefectSource::Auto, DefectSource::User] {
            assert_eq!(DefectSource::parse(v.as_str()), v);
        }
        for v in [
            DefectKind::RuntimeError,
            DefectKind::UserReport,
            DefectKind::BuildError,
            DefectKind::Other,
        ] {
            assert_eq!(DefectKind::parse(v.as_str()), v);
        }
        for v in [
            DefectSeverity::Info,
            DefectSeverity::Warning,
            DefectSeverity::Error,
            DefectSeverity::Critical,
        ] {
            assert_eq!(DefectSeverity::parse(v.as_str()), v);
        }
        for v in [
            DefectStatus::Open,
            DefectStatus::Acknowledged,
            DefectStatus::Resolved,
        ] {
            assert_eq!(DefectStatus::parse(v.as_str()), v);
        }
    }

    #[test]
    fn parse_falls_back_to_default_for_unknown_string() {
        assert_eq!(DefectSource::parse("bogus"), DefectSource::Auto);
        assert_eq!(DefectKind::parse("bogus"), DefectKind::Other);
        assert_eq!(DefectSeverity::parse("bogus"), DefectSeverity::Info);
        assert_eq!(DefectStatus::parse("bogus"), DefectStatus::Open);
    }

    // ── constructors ─────────────────────────────────────────────────────────

    #[test]
    fn auto_constructor_sets_source_and_defaults() {
        let r = DefectReport::auto(
            "proj-1",
            DefectKind::RuntimeError,
            "TypeError: x is undefined",
        );
        assert_eq!(r.source, DefectSource::Auto);
        assert_eq!(r.kind, DefectKind::RuntimeError);
        assert_eq!(r.project_id, "proj-1");
        assert_eq!(r.title, "TypeError: x is undefined");
        assert!(r.id.is_none());
        assert_eq!(r.status, DefectStatus::Open);
        assert_eq!(r.severity, DefectSeverity::Info);
        assert!(r.description.is_empty());
        assert!(!r.ts.is_empty());
        assert!(r.fingerprint.is_none());
        assert_eq!(r.count, 1);
    }

    #[test]
    fn user_constructor_sets_source() {
        let r = DefectReport::user("proj-2", DefectKind::UserReport, "Button does nothing");
        assert_eq!(r.source, DefectSource::User);
        assert_eq!(r.kind, DefectKind::UserReport);
    }

    #[test]
    fn builder_setters_attach_optional_fields() {
        let r = DefectReport::user("proj-3", DefectKind::UserReport, "Broken layout")
            .with_description("The sidebar overlaps the content on mobile")
            .with_severity(DefectSeverity::Warning)
            .with_route("/dashboard")
            .with_element("button.save")
            .with_stack("at foo (app.js:1:1)")
            .with_console("warn: deprecated API")
            .with_extra("viewport", "375x812")
            .with_fingerprint("fp-abc123");

        assert_eq!(r.description, "The sidebar overlaps the content on mobile");
        assert_eq!(r.severity, DefectSeverity::Warning);
        assert_eq!(r.context.route.as_deref(), Some("/dashboard"));
        assert_eq!(r.context.element.as_deref(), Some("button.save"));
        assert_eq!(r.context.stack.as_deref(), Some("at foo (app.js:1:1)"));
        assert_eq!(r.context.console.as_deref(), Some("warn: deprecated API"));
        assert_eq!(
            r.context.extra.get("viewport").map(String::as_str),
            Some("375x812")
        );
        assert_eq!(r.fingerprint.as_deref(), Some("fp-abc123"));
    }

    // ── serde round-trip ─────────────────────────────────────────────────────

    #[test]
    fn defect_report_serde_round_trips() {
        let r = DefectReport::auto("proj-4", DefectKind::BuildError, "cargo build failed")
            .with_description("E0308 mismatched types")
            .with_severity(DefectSeverity::Critical)
            .with_stack("src/main.rs:10:5");

        let json = serde_json::to_string(&r).expect("serialize");
        let back: DefectReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, r);
    }

    #[test]
    fn enums_serialize_as_stable_lowercase_strings() {
        assert_eq!(
            serde_json::to_string(&DefectSource::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&DefectKind::RuntimeError).unwrap(),
            "\"runtime_error\""
        );
        assert_eq!(
            serde_json::to_string(&DefectSeverity::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&DefectStatus::Acknowledged).unwrap(),
            "\"acknowledged\""
        );
    }

    #[test]
    fn minimal_json_payload_fills_in_defaults() {
        // A minimal client payload (e.g. a hand-typed click-to-report body) that omits
        // every optional field must still deserialize, with sensible defaults filled in.
        let json = serde_json::json!({
            "project_id": "proj-5",
            "title": "Something broke",
        });
        let r: DefectReport = serde_json::from_value(json).expect("deserialize minimal payload");
        assert_eq!(r.project_id, "proj-5");
        assert_eq!(r.title, "Something broke");
        assert_eq!(r.source, DefectSource::Auto);
        assert_eq!(r.kind, DefectKind::Other);
        assert_eq!(r.status, DefectStatus::Open);
        assert!(r.id.is_none());
        assert!(!r.ts.is_empty());
        assert!(r.fingerprint.is_none());
        assert_eq!(r.count, 1);
    }

    // ── fingerprint + count (PART C: fingerprint + dedupe) ──────────────────

    #[test]
    fn fingerprint_and_count_default_and_round_trip() {
        let r = DefectReport::auto("proj-6", DefectKind::RuntimeError, "boom");
        assert!(r.fingerprint.is_none());
        assert_eq!(r.count, 1);

        let r = r.with_fingerprint("fp-xyz");
        assert_eq!(r.fingerprint.as_deref(), Some("fp-xyz"));

        let json = serde_json::to_string(&r).expect("serialize");
        let back: DefectReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, r);
        assert_eq!(back.fingerprint.as_deref(), Some("fp-xyz"));
        assert_eq!(back.count, 1);
    }

    #[test]
    fn count_greater_than_one_round_trips() {
        let mut r = DefectReport::auto("proj-7", DefectKind::RuntimeError, "boom");
        r.count = 3;
        let json = serde_json::to_string(&r).expect("serialize");
        let back: DefectReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.count, 3);
    }
}
