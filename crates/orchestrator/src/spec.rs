//! The living spec: `SPEC.md` in a scaffolded project's repo working dir.
//!
//! See `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`, the
//! "Architect-orchestrator design (decided 2026-07-10)" section: "A living spec per
//! app (like budget-tracker's SPEC.md), updated after each accepted change: the
//! model's memory, contradiction detector, checkpoint-diff source, and the PO's
//! 'what does my app do now.' Highest-leverage single artifact."
//!
//! This module owns THREE things, deliberately layered so the hard-to-get-wrong part
//! (parsing/rendering the file's structure) is pure and unit-tested, and the part that
//! actually touches disk is a thin, dumb edge:
//! - [`LivingSpec`]: the in-memory shape — a plain-words summary, an assumptions
//!   ledger (the "assume-and-declare instead of asking" pattern), and a keep-point-
//!   style change log.
//! - [`LivingSpec::parse`] / [`LivingSpec::render`]: pure, round-trippable text <->
//!   struct conversions. No I/O, no `Utc::now()` call inside — every timestamped
//!   entry is constructed with a CALLER-supplied [`chrono::DateTime<Utc>`]
//!   (RUST-PURE-STATE-TRANSITIONS-1), so tests are fully deterministic.
//! - [`read_or_init`] / [`write`]: the thin filesystem edge. Plain `std::fs` (this
//!   crate deliberately carries no tokio dependency — see the crate's module doc);
//!   a SPEC.md is a few KB of text, and this codebase already reads small sidecar
//!   files this way from inside async code (e.g. `dev_implement_run`'s
//!   `read_first_escalation_request` / `read_memory_proposals`).
//!
//! # Tolerant of a pre-existing, hand-written SPEC.md
//! An EXISTING scaffolded project (the reference apps, budget-tracker / itinerary-app)
//! may already ship a `SPEC.md` that predates this canonical 3-section format. Rather
//! than discard it, [`LivingSpec::parse`] falls back to treating the WHOLE file as the
//! summary when none of the expected `## ` section headers are found, so the first
//! turn's spec update never destroys pre-existing content — it just starts writing in
//! the canonical format from that point on.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

/// The filename this module reads/writes in a project's repo working dir.
pub const SPEC_FILENAME: &str = "SPEC.md";

const HEADING_SUMMARY: &str = "## What this app does";
const HEADING_ASSUMPTIONS: &str = "## Assumptions & decisions";
const HEADING_CHANGES: &str = "## Change log";

/// The placeholder summary a brand-new (or never-yet-summarized) spec starts with.
/// Never silently treated as a real summary by callers — it is deliberately
/// recognizable prose, not an empty string, so a rendered fresh SPEC.md reads as
/// "nothing captured yet" rather than looking broken.
pub const NO_SUMMARY_YET: &str =
    "(no summary yet — the orchestrator fills this in as changes land)";

/// One entry in the assumptions ledger: something the orchestrator declared instead
/// of asking a clarifying question (the plan doc's "assume-and-declare" pattern).
#[derive(Debug, Clone, PartialEq)]
pub struct AssumptionEntry {
    pub at: DateTime<Utc>,
    pub text: String,
}

/// One entry in the keep-point-style change log: a plain-words summary of one
/// accepted change, with the timestamp it landed.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeLogEntry {
    pub at: DateTime<Utc>,
    pub summary: String,
}

/// The living spec for one project: what it does, in plain words, plus its
/// decisions/assumptions ledger and its change history. This is the ENTIRE state of
/// `SPEC.md`, round-tripped through [`LivingSpec::parse`] / [`LivingSpec::render`].
#[derive(Debug, Clone, PartialEq)]
pub struct LivingSpec {
    /// Plain-words description of what the app does today. Updated as changes land
    /// (a later phase may have the LLM rewrite this from the change log; today it
    /// carries forward unchanged unless a caller explicitly replaces it).
    pub summary: String,
    pub assumptions: Vec<AssumptionEntry>,
    pub changes: Vec<ChangeLogEntry>,
}

impl LivingSpec {
    /// A brand-new spec with the given summary and empty ledgers.
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            assumptions: Vec::new(),
            changes: Vec::new(),
        }
    }

    /// The default fresh spec (no project-specific summary known yet).
    pub fn fresh() -> Self {
        Self::new(NO_SUMMARY_YET)
    }

    /// Append one change-log entry. Pure (returns a modified copy... actually mutates
    /// `self` in place and returns it, builder-style) — chainable.
    pub fn with_change(mut self, summary: impl Into<String>, at: DateTime<Utc>) -> Self {
        self.changes.push(ChangeLogEntry {
            at,
            summary: summary.into(),
        });
        self
    }

    /// Append one declared assumption. Builder-style, chainable.
    pub fn with_assumption(mut self, text: impl Into<String>, at: DateTime<Utc>) -> Self {
        self.assumptions.push(AssumptionEntry {
            at,
            text: text.into(),
        });
        self
    }

    /// Replace the summary (e.g. after the orchestrator re-describes the app in
    /// light of a landed change). Builder-style, chainable.
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Render the canonical Markdown text for this spec. Always emits all three
    /// sections (even when a ledger is empty, it emits a "(none yet)" placeholder
    /// line) so the file's shape is stable and diffable across turns.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# App Spec\n\n");
        out.push_str(HEADING_SUMMARY);
        out.push_str("\n\n");
        out.push_str(self.summary.trim());
        out.push_str("\n\n");

        out.push_str(HEADING_ASSUMPTIONS);
        out.push_str("\n\n");
        if self.assumptions.is_empty() {
            out.push_str("(none yet)\n");
        } else {
            for a in &self.assumptions {
                out.push_str(&format!("- {}: {}\n", format_date(a.at), a.text.trim()));
            }
        }
        out.push('\n');

        out.push_str(HEADING_CHANGES);
        out.push_str("\n\n");
        if self.changes.is_empty() {
            out.push_str("(none yet)\n");
        } else {
            for c in &self.changes {
                out.push_str(&format!("- {}: {}\n", format_date(c.at), c.summary.trim()));
            }
        }
        out
    }

    /// Parse a `SPEC.md`'s text back into a [`LivingSpec`]. Tolerant fallback: when
    /// NONE of the three canonical `## ` headings are found (a pre-existing,
    /// hand-written spec that predates this format), the whole trimmed text becomes
    /// the summary and both ledgers start empty — nothing is discarded, the file just
    /// migrates to the canonical format on the next [`Self::render`].
    pub fn parse(text: &str) -> Self {
        let has_any_heading = [HEADING_SUMMARY, HEADING_ASSUMPTIONS, HEADING_CHANGES]
            .iter()
            .any(|h| text.contains(h));
        if !has_any_heading {
            let trimmed = text.trim();
            return if trimmed.is_empty() {
                Self::fresh()
            } else {
                Self::new(trimmed.to_string())
            };
        }

        let summary = section_body(text, HEADING_SUMMARY)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| NO_SUMMARY_YET.to_string());
        let assumptions = section_body(text, HEADING_ASSUMPTIONS)
            .map(|body| parse_bullets(&body))
            .unwrap_or_default()
            .into_iter()
            .map(|(at, text)| AssumptionEntry { at, text })
            .collect();
        let changes = section_body(text, HEADING_CHANGES)
            .map(|body| parse_bullets(&body))
            .unwrap_or_default()
            .into_iter()
            .map(|(at, text)| ChangeLogEntry { at, summary: text })
            .collect();

        Self {
            summary,
            assumptions,
            changes,
        }
    }
}

/// Format a timestamp as a plain calendar date (`YYYY-MM-DD`) — the change log /
/// assumptions ledger are meant to read like a human diary, not a machine log.
fn format_date(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

/// Extract the body text between a `## <heading>` line and the NEXT `## ` heading (or
/// EOF), trimmed. Returns `None` if `heading` does not appear at all.
fn section_body(text: &str, heading: &str) -> Option<String> {
    let start = text.find(heading)? + heading.len();
    let rest = &text[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// Parse a section body's bullet lines (`- <date>: <text>`) into `(timestamp, text)`
/// pairs, in file order. A line whose leading token does not parse as `YYYY-MM-DD`
/// falls back to `at = 1970-01-01T00:00:00Z` and keeps the ENTIRE line (minus the
/// leading `- `) as the text — nothing is dropped even when a line was hand-edited
/// into a shape this parser does not expect. A "(none yet)" placeholder line (or any
/// non-bullet line) is skipped.
fn parse_bullets(body: &str) -> Vec<(DateTime<Utc>, String)> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("- ")?;
            match rest.split_once(": ") {
                Some((date_part, text)) if parse_date(date_part).is_some() => {
                    Some((parse_date(date_part).expect("checked Some above"), text.to_string()))
                }
                _ => Some((epoch(), rest.to_string())),
            }
        })
        .collect()
}

fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let d = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
    Some(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?))
}

fn epoch() -> DateTime<Utc> {
    Utc.timestamp_opt(0, 0).single().expect("epoch is a valid timestamp")
}

// ─── the thin filesystem edge ───────────────────────────────────────────────────

/// The path `SPEC.md` lives at within a project's repo working dir.
pub fn spec_path(repo_dir: &std::path::Path) -> std::path::PathBuf {
    repo_dir.join(SPEC_FILENAME)
}

/// Read `SPEC.md` from `repo_dir` and parse it, or return [`LivingSpec::fresh`] when
/// the file does not exist yet (a project's FIRST orchestrator turn). Any OTHER I/O
/// error (permissions, not-a-directory, ...) is an honest error — never silently
/// treated as "file absent."
pub fn read_or_init(repo_dir: &std::path::Path) -> anyhow::Result<LivingSpec> {
    match std::fs::read_to_string(spec_path(repo_dir)) {
        Ok(text) => Ok(LivingSpec::parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LivingSpec::fresh()),
        Err(e) => Err(anyhow::anyhow!("reading {}: {e}", spec_path(repo_dir).display())),
    }
}

/// Render and write `spec` to `SPEC.md` in `repo_dir`. `repo_dir` must already exist
/// (this never creates the project directory itself — that is the scaffolder's job).
pub fn write(repo_dir: &std::path::Path, spec: &LivingSpec) -> anyhow::Result<()> {
    let path = spec_path(repo_dir);
    std::fs::write(&path, spec.render())
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))
}

// ─────────────────────────────────────────────────────────────────────────────────
// Tests (ORCH-NEW-PATH-TESTS-1)
// ─────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(y, m, d)
                .expect("valid test date")
                .and_hms_opt(0, 0, 0)
                .expect("valid test time"),
        )
    }

    // ── render ───────────────────────────────────────────────────────────────

    #[test]
    fn render_fresh_spec_has_all_three_sections_and_placeholders() {
        let text = LivingSpec::fresh().render();
        assert!(text.contains(HEADING_SUMMARY));
        assert!(text.contains(HEADING_ASSUMPTIONS));
        assert!(text.contains(HEADING_CHANGES));
        assert!(text.contains(NO_SUMMARY_YET));
        // Two "(none yet)" placeholders: assumptions + changes.
        assert_eq!(text.matches("(none yet)").count(), 2);
    }

    #[test]
    fn render_includes_change_and_assumption_lines_with_dates() {
        let spec = LivingSpec::new("A budget tracker.")
            .with_change("Added a $0 input validation guard.", dt(2026, 7, 1))
            .with_assumption("Assumed USD as the only currency.", dt(2026, 7, 2));
        let text = spec.render();
        assert!(text.contains("- 2026-07-01: Added a $0 input validation guard."));
        assert!(text.contains("- 2026-07-02: Assumed USD as the only currency."));
    }

    // ── parse: round trip ────────────────────────────────────────────────────

    #[test]
    fn parse_render_round_trips_summary_and_ledgers() {
        let spec = LivingSpec::new("A budget tracker.")
            .with_change("Added a $0 input validation guard.", dt(2026, 7, 1))
            .with_change("Added a monthly rollover view.", dt(2026, 7, 3))
            .with_assumption("Assumed USD as the only currency.", dt(2026, 7, 2));
        let text = spec.render();
        let parsed = LivingSpec::parse(&text);
        assert_eq!(parsed, spec);
    }

    #[test]
    fn parse_empty_spec_after_render_round_trips() {
        let spec = LivingSpec::fresh();
        let parsed = LivingSpec::parse(&spec.render());
        assert_eq!(parsed, spec);
    }

    // ── parse: tolerant fallback for a pre-existing hand-written spec ───────

    #[test]
    fn parse_freeform_legacy_spec_preserves_content_as_summary() {
        let legacy = "# Budget Tracker\n\nA simple household budget app.\n\nBuilt with Dioxus.";
        let parsed = LivingSpec::parse(legacy);
        assert_eq!(parsed.summary, legacy.trim());
        assert!(parsed.assumptions.is_empty());
        assert!(parsed.changes.is_empty());
    }

    #[test]
    fn parse_empty_text_yields_fresh_spec() {
        let parsed = LivingSpec::parse("");
        assert_eq!(parsed, LivingSpec::fresh());
    }

    #[test]
    fn parse_whitespace_only_text_yields_fresh_spec() {
        let parsed = LivingSpec::parse("   \n\n  ");
        assert_eq!(parsed, LivingSpec::fresh());
    }

    // ── parse: malformed bullet lines are preserved, not dropped ────────────

    #[test]
    fn parse_bullet_without_date_prefix_falls_back_to_epoch_and_keeps_text() {
        let text = format!(
            "# App Spec\n\n{HEADING_SUMMARY}\n\nSomething.\n\n{HEADING_ASSUMPTIONS}\n\n\
             - hand-edited note with no date\n\n{HEADING_CHANGES}\n\n(none yet)\n"
        );
        let parsed = LivingSpec::parse(&text);
        assert_eq!(parsed.assumptions.len(), 1);
        assert_eq!(parsed.assumptions[0].text, "hand-edited note with no date");
        assert_eq!(parsed.assumptions[0].at, epoch());
    }

    #[test]
    fn parse_none_yet_placeholder_yields_empty_ledger() {
        let text = format!(
            "# App Spec\n\n{HEADING_SUMMARY}\n\nSomething.\n\n{HEADING_ASSUMPTIONS}\n\n\
             (none yet)\n\n{HEADING_CHANGES}\n\n(none yet)\n"
        );
        let parsed = LivingSpec::parse(&text);
        assert!(parsed.assumptions.is_empty());
        assert!(parsed.changes.is_empty());
    }

    // ── builders ─────────────────────────────────────────────────────────────

    #[test]
    fn with_summary_replaces_summary_only() {
        let spec = LivingSpec::new("old").with_summary("new");
        assert_eq!(spec.summary, "new");
    }

    // ── filesystem edge ──────────────────────────────────────────────────────

    #[test]
    fn read_or_init_returns_fresh_when_file_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = read_or_init(dir.path()).expect("read_or_init");
        assert_eq!(spec, LivingSpec::fresh());
    }

    #[test]
    fn write_then_read_or_init_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = LivingSpec::new("A budget tracker.")
            .with_change("Added a $0 input validation guard.", dt(2026, 7, 1));
        write(dir.path(), &spec).expect("write");
        assert!(spec_path(dir.path()).exists());
        let read_back = read_or_init(dir.path()).expect("read_or_init");
        assert_eq!(read_back, spec);
    }

    #[test]
    fn write_fails_honestly_when_repo_dir_does_not_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        let err = write(&missing, &LivingSpec::fresh()).expect_err("must fail honestly");
        assert!(err.to_string().contains("SPEC.md") || err.to_string().contains("writing"));
    }
}
