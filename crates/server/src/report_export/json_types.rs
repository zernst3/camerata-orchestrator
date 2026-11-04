//! The serde shapes of the report JSON (cover, summary, matrix, sites, groups, methodology)
//! and the small helpers that sit beside them.

use super::*;

/// Real pluralization for a count + noun pair (`"1 site"` / `"3 sites"`), replacing the
/// `"N thing(s)"` CLI-ism that reads as sloppy in a flagship client PDF.
pub(crate) fn noun(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// Cap a finding's snippet so one pathological finding (a multi-hundred-line diff, a giant
/// minified blob) cannot claim unbounded page real estate in the PDF. ~12 lines / ~800 chars,
/// whichever is hit first; appends a `(truncated)` marker so the cut is honest, never silent.
pub(crate) const SNIPPET_MAX_LINES: usize = 12;
pub(crate) const SNIPPET_MAX_CHARS: usize = 800;

pub(crate) fn cap_snippet(snippet: &str) -> String {
    let mut truncated = false;
    let mut out: String = {
        let mut lines: Vec<&str> = snippet.lines().collect();
        if lines.len() > SNIPPET_MAX_LINES {
            lines.truncate(SNIPPET_MAX_LINES);
            truncated = true;
        }
        lines.join("\n")
    };
    if out.chars().count() > SNIPPET_MAX_CHARS {
        out = out.chars().take(SNIPPET_MAX_CHARS).collect();
        truncated = true;
    }
    if truncated {
        out.push_str("\n... (truncated)");
    }
    out
}

// ── Output shape ─────────────────────────────────────────────────────────────────

/// One audited repo's git identity for the cover page. Mirrors `onboard::AuditedRef` plus
/// a pre-truncated `short_sha` (the template has no string-slicing primitive worth trusting
/// for this, so the serializer does it once here).
#[derive(Debug, Clone, Serialize)]
pub struct AuditedRefJson {
    pub repo: String,
    pub sha: Option<String>,
    pub short_sha: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
}

/// Cover-page stat strip (nice-to-have): a one-line severity/disposition summary so the
/// story starts on page 1 instead of the cover being ~60% dead space until "Executive
/// summary". Counts are over `code_findings` (matches the scorecard's own severity totals)
/// plus the dependency-snapshot row count.
#[derive(Debug, Clone, Serialize)]
pub struct CoverStatsJson {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub accepted: usize,
    pub dependency_advisories: usize,
}

/// P6 (2026-09-29): the cover's real "code volume" figure — non-blank source lines (never
/// characters; a raw char count reads as noise to a non-engineer reader) broken out by
/// language, straight from [`crate::onboard::ScanReport::code_lines`]/`code_lines_by_language`.
/// `CoverJson::code_volume` is `None` (never `Some` with a zero `lines`) whenever
/// `code_lines == 0` — see [`build_report_json`]'s construction of it — so the template can
/// gate the whole row on presence rather than ever rendering "0 characters"/"0 lines".
#[derive(Debug, Clone, Serialize)]
pub struct CodeVolumeJson {
    pub lines: usize,
    /// Sorted by line count descending (see `onboard::finalize_language_breakdown`). May be
    /// empty even when `lines > 0` (every scanned file had an unrecognized extension) — the
    /// template must render the total either way and only additionally list languages when
    /// this is non-empty.
    pub by_language: Vec<crate::onboard::LanguageVolume>,
    /// Each repo's own line count, when the project has more than one repo. Absent from the JSON
    /// otherwise, so a one-repo `findings.json` is byte for byte what it always was; the template
    /// guards for the key. Never merged into the total above without saying which repo holds
    /// what.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub per_repo: Vec<CodeVolumeRepoJson>,
}

/// One repo's share of the code volume.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodeVolumeRepoJson {
    pub repo: String,
    pub files: usize,
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverJson {
    pub repos: Vec<String>,
    pub files_scanned: usize,
    pub files_excluded: usize,
    pub code_chars: usize,
    /// See [`CodeVolumeJson`]'s doc comment: `None` whenever no real code volume is known
    /// (0 lines), so the template hides the "Code volume" row entirely instead of rendering a
    /// zero. Never render `d.cover.code_chars` directly in the template for this reason — it
    /// carries the identical "zero means hide, not print 0" problem this field was added to fix.
    pub code_volume: Option<CodeVolumeJson>,
    pub audited_refs: Vec<AuditedRefJson>,
    pub audit_model: Option<String>,
    pub calibration_model: Option<String>,
    /// The model backend (`api` / `cli`) and provider that ran the inspection, e.g.
    /// `api (anthropic)`, from [`ReportOptions::model`]. `None` when the caller recorded none.
    pub model_backend: Option<String>,
    pub camerata_version: String,
    /// The git commit the exporting engine was built from ([`crate::ENGINE_COMMIT`]). `None`
    /// for a build outside a git checkout. Rendered in the PDF methodology and carried in
    /// `findings.json`'s `provenance`.
    pub engine_commit: Option<String>,
    /// The rule-library lock hash ([`camerata_rules::corpus_version`]: the digest over every
    /// rule's id, version and content hash, exactly the content of `CORPUS_LOCK.toml`). The
    /// same value as `manifest.json`'s `inspection.corpus_version` and the README's "Corpus
    /// version" line. `None` when the export ran without a loaded corpus.
    pub corpus_version: Option<String>,
    /// `%Y-%m-%d %H:%M UTC` (M5) — of when this PDF was generated (NOT the scan time —
    /// that's `audited_refs`/provenance; this is "as-of" for the export itself).
    pub generated_at: String,
    pub client_name: String,
    pub project_title: String,
    pub prepared_by: String,
    /// The resolved auditing-agency brand ([`resolve_brand`]'s output, already folded through
    /// `ReportOptions::apply_env_defaults` by the time this JSON is built) — `None` means no
    /// brand resolved at all; the template must render [`NEUTRAL_COVER_TITLE`] with no agency
    /// name and no "Camerata" literal on the cover. See the Branding section of
    /// `docs/design/2026-09-13_audit-deliverable-review-fixes.md`.
    pub brand: Option<String>,
    pub stats: CoverStatsJson,
    /// H11: set when this run re-audited only the changed files and carried the earlier
    /// inspection's other findings forward ("12 files re-audited this run, 3 findings carried
    /// from the inspection at abc1234."). `None` for a full scan, and the cover then prints
    /// nothing extra.
    pub incremental_note: Option<String>,
    /// Set only on an internal export of a run that did not complete: the line printed on the
    /// cover and carried in `findings.json`. Absent otherwise, so a complete run's output is
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_incomplete: Option<String>,
}

/// H11: the cover sentence for an incremental run. Plain words; each count is the ledger's.
pub fn incremental_cover_note(carry: &crate::scan_ledger::IncrementalCarry) -> String {
    let files = |n: usize| {
        if n == 1 {
            "1 file".to_string()
        } else {
            format!("{n} files")
        }
    };
    let findings = if carry.carried_findings == 1 {
        "1 finding".to_string()
    } else {
        format!("{} findings", carry.carried_findings)
    };
    let source = match carry.prior_sha.as_deref().map(str::trim) {
        Some(sha) if !sha.is_empty() => {
            let short: String = sha.chars().take(7).collect();
            format!("the inspection at {short}")
        }
        _ => "an earlier inspection".to_string(),
    };
    format!(
        "{} re-audited this run. {} not re-audited; {} carried from {source}.",
        files(carry.changed),
        files(carry.carried_files),
        findings
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutiveSummaryJson {
    /// Deterministic template narrative, or the client's `executive_summary_override`
    /// verbatim when supplied. Never an LLM call either way.
    pub narrative: String,
    pub is_override: bool,
    pub candidates_reviewed: usize,
    pub excluded_false_positive: usize,
    pub curated_total: usize,
    /// P5 / C5-4: findings held out of the curated action tiers for a human reviewer's judgment
    /// call — `matrix.informational.len() + matrix.held.len()`, NOT `informational.len()`
    /// alone. The two buckets are both "held for review" for this reconciling purpose but
    /// deliberately distinct destinations: `informational` is the severity-capped (≤ medium)
    /// conventions-to-consider appendix, while `held` (C5-4) is the calibration hedge
    /// (`needs_review`/`confidence == "needs-review"`) at ANY severity, including high/critical
    /// — see [`crate::report_export::is_held_for_review`]'s doc comment for why that split
    /// exists and the prior regression (a high/critical hedge silently NOT counted here because
    /// it stayed in `do_now`/`do_next`) this field's current formula fixes. This is the
    /// previously-invisible "other 23" in the reconciling count
    /// `candidates_reviewed == curated_total + held_for_review + excluded_false_positive
    /// + dependency_advisories`. Always shown in the narrative, in BOTH review states.
    pub held_for_review: usize,
    /// P5: `DEP_AUDIT_RULE_ID` findings that survived the false-positive filter — carved into
    /// their own `dependency_snapshot` table, but still a real piece of `candidates_reviewed`'s
    /// reconciliation (see `held_for_review`'s doc comment).
    pub dependency_advisories: usize,
    pub do_now: usize,
    pub do_next: usize,
    pub plan: usize,
    pub accepted: usize,
    pub open: usize,
    /// W6: one explicit sentence per audit pass that FAILED or timed out this run (e.g. the
    /// alternative-recommendation pass hitting the CLI hang timeout) — see
    /// `failed_pass_disclosures`'s doc comment. Empty on the happy path. Rendered in the
    /// summary so a reader never has to infer a gap from an absence; the SAME strings also
    /// render in `MethodologyJson::failed_passes` for the more technical section.
    #[serde(default)]
    pub failed_passes: Vec<String>,
}

/// Item 7: "If you only do three things this week" — a half-page box right after the
/// executive summary. A buyer pricing remediation reads "these two criticals are about four
/// hours of work total" as the sentence that converts anxiety into a purchase order; a bare
/// finding list does not do that job.
#[derive(Debug, Clone, Serialize)]
pub struct ThreeThingsItemJson {
    pub headline: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    /// A rough, LABELED-as-rough hour estimate (e.g. `"2 to 4 hours"`), never a bare number
    /// dressed up as precise — see `effort_hours_bounds`.
    pub hours_label: String,
    /// FIX 7 (2026-09-13 review): one FACTUAL sentence of what this exposure MEANS in plain
    /// business terms (e.g. "A public bucket serves any object to anyone who has, or can
    /// guess, its URL..."), never fear-selling. `None` when no impact sentence is authored yet
    /// for this rule — the template must omit the line gracefully rather than render a blank
    /// one. See [`business_impact_for_rule`].
    pub impact: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreeThingsJson {
    pub items: Vec<ThreeThingsItemJson>,
    /// e.g. `"roughly 6 to 12 hours total (rough estimate)"`, or a note that some items have
    /// no effort estimate yet and were left out of the sum (never silently guessed).
    pub total_hours_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryRowJson {
    pub category: String,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    /// `"Clean"` | `"Attention"` | `"Action-needed"` — a status CHIP, never a letter grade.
    pub status: String,
    /// Rules in this category that were actually audited this run (from
    /// `provenance.audited_rule_ids`), so "checked vs found" is visible.
    pub audited_rules: usize,
    /// Of the audited rules in this category, how many produced ZERO real (non-FP)
    /// findings — the category's own "clean rule" count.
    pub clean_rules: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScorecardJson {
    pub rows: Vec<CategoryRowJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingRefJson {
    pub rule_id: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub severity: String,
    /// The DEFECT at this specific object ("profiles table has no RLS: all member PII
    /// publicly readable and writable with the anon key."), never the rule's own invariant
    /// title ("Every table... has RLS enabled") — read cold, the invariant sounds like a
    /// clean bill of health. See `defect_headline`.
    pub headline: String,
    /// Carried through so item 7's "If you only do three things this week" box can map effort
    /// to a rough hour estimate without re-joining back to the original `Finding`.
    pub effort: Option<String>,
    /// C3-6: this finding's own [`effective_bucket`] result (`"do_now"` | `"do_next"` |
    /// `"plan"` | `"accepted"` | `"informational"` | `"held"` (C5-4)) — which `MatrixJson` vec
    /// this ref lives in is already implied by position, but the explicit field lets every consumer
    /// (`xlsx_export`, a cross-artifact test) assert the label directly rather than
    /// re-deriving or inferring it from array membership.
    pub bucket: String,
    /// Calibration-review fix: whether this finding's own text or calibration justification
    /// records an exploit precondition (`ai_audit::mentions_exploit_precondition`) — e.g.
    /// "exploitation requires forging a session cookie". `0` for an unconditional finding,
    /// `1` when a precondition is recorded. Used ONLY as a tie-break in the "do now" ranking
    /// ([`build_report_json`]'s `do_now_sorted`) so an unconditional critical always outranks
    /// a critical whose note records a precondition — never to change severity or drop/
    /// re-bucket the finding itself.
    pub precondition_count: usize,
    /// Secondary tie-break for the "do now" ranking, after severity and
    /// `precondition_count`: `0` for a clear/confident finding (`confidence == Some("high")`,
    /// or a finding calibration never scored at all — the deterministic floor/preview tier,
    /// which was never in doubt to begin with) and `1` for `needs-review`. Lower ranks first.
    pub confidence_rank: u8,
    /// C6-B2 design point 3: every credential/secret root-cause identity token
    /// (`ai_audit::root_cause_credential_identities`) this finding's own text names — empty when
    /// none were found. Used ONLY by the "three things this week" top-3 selector
    /// (`build_report_json`'s `top3_do_now`) as an independent, defense-in-depth distinctness
    /// check: a row that shares a non-empty token with an ALREADY-SELECTED row is skipped in
    /// favor of the next-ranked row, so three "do this week" slots are never spent on three sites
    /// of ONE leaked credential. This is a safety net, not the primary fix — the primary fix is
    /// the merge pass itself (`ai_audit::backfill_credential_identity` feeding the pre-existing
    /// `shared_captured_object` cross-file merge signal) collapsing those sites into ONE row
    /// before the report is ever built; this field only catches the case where the merge pass
    /// conservatively left two rows distinct. Never used for bucket placement, severity, or any
    /// other decision — an empty set (the common case: no detected credential) never blocks
    /// selection, and every skipped row still ships normally wherever `do_now`/the matrix already
    /// placed it; nothing is dropped.
    pub root_cause_keys: Vec<String>,
    /// Pass-1 W3 (A5.6): what this row IS — flattened
    /// [`camerata_api_types::audit::KindOnWire`], so the serialized row carries a flat
    /// `"kind"` discriminator (plus the variant's payload fields as siblings) on EVERY row;
    /// an absent kind renders the explicit `"unspecified"` tag, never a silent `"defect"`.
    #[serde(flatten)]
    pub kind: camerata_api_types::audit::KindOnWire,
}

/// The severity×effort action matrix — the money page. Cell membership is driven by the
/// auditor's OWN disposition where one exists (a `TechDebt{Now}` finding is placed in
/// `do_now` regardless of its computed severity/effort quadrant — the human call wins over
/// the heuristic), and by severity×effort only for findings still `Unresolved`/open. See
/// [`matrix_bucket`].
#[derive(Debug, Clone, Serialize, Default)]
pub struct MatrixJson {
    pub do_now: Vec<FindingRefJson>,
    pub do_next: Vec<FindingRefJson>,
    pub plan: Vec<FindingRefJson>,
    pub accepted: Vec<FindingRefJson>,
    /// Bug 4's "Conventions to consider" appendix: absence-type stance/architecture notes,
    /// testing-style deviations in a repo with no test corpus, and any `info`-severity finding.
    /// VISIBLE (over-tell preserved) but deliberately OUTSIDE the do_now/do_next/plan action
    /// tiers and excluded from `curated_total` — a report appendix, not a work item. A critical
    /// or high finding is NEVER placed here (see `is_informational`). C5-4: a calibration hedge
    /// (`needs-review`) is NOT one of this bucket's signals any more, at ANY severity — see
    /// `held` below, the bucket that now owns that signal.
    #[serde(default)]
    pub informational: Vec<FindingRefJson>,
    /// C5-4: findings the calibrator itself flagged debatable (`needs_review` /
    /// `confidence == "needs-review"`), held out of every action tier (do_now/do_next/plan) AT
    /// ANY SEVERITY — unlike `informational`, there is NO severity ceiling here: a high/critical
    /// row the calibrator doubts is precisely the row a client must see called out as unresolved
    /// doubt, not silently kept in an action tier next to findings nobody doubts. See
    /// `is_held_for_review` for the gate and its doc comment for the regression this bucket
    /// closes (a prior cycle folded this signal into `informational`, whose hard "critical/high
    /// is never informational" invariant then kept a high/critical hedge stuck in `do_now`/
    /// `do_next`, contradicting its own exported `needs_review: true`).
    #[serde(default)]
    pub held: Vec<FindingRefJson>,
}

// ── FIX 6: the real severity x effort priority grid ─────────────────────────────

/// One finding placed in a [`GridCellJson`] — just enough to render a short, readable tag in a
/// crowded cell (never the full curated-findings prose; that lives in its own section).
#[derive(Debug, Clone, Serialize)]
pub struct GridTagJson {
    pub rule_id: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub headline: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GridCellJson {
    pub findings: Vec<GridTagJson>,
}

/// One severity row of the grid, with one cell per [`PriorityGridJson::columns`] entry
/// (same length, same order — the template zips them positionally rather than looking a
/// column up by name).
#[derive(Debug, Clone, Serialize)]
pub struct GridRowJson {
    pub severity: String,
    pub cells: Vec<GridCellJson>,
}

/// FIX 6 (2026-09-13 review) — the ACTUAL severity-rows x effort-columns priority grid,
/// replacing the old 4-bucket list ("Do now" / "Do next" / "Plan" / "Accepted" rendered as 4
/// unrelated boxes, not a 2-D placement) that used to live under this section's heading. Built
/// by [`build_priority_grid`] from the SAME `do_now`/`do_next`/`plan` partition
/// `MatrixJson` already computed — never a second, independent bucketing pass. Accepted and
/// Informational findings are explicitly OUT OF SCOPE for this grid (an accepted-risk item or
/// an appendix note isn't a prioritization decision); their counts are surfaced as a footnote
/// instead of silently vanishing.
#[derive(Debug, Clone, Serialize)]
pub struct PriorityGridJson {
    /// Effort column keys present this run (`"low"` | `"medium"` | `"high"` | `"unscoped"`),
    /// in that fixed order, filtered to the ones at least one row actually uses.
    pub columns: Vec<String>,
    /// Severity rows present this run (`"critical"` | `"high"` | `"medium"` | `"low"`), in
    /// that fixed order, filtered to the ones with at least one finding.
    pub rows: Vec<GridRowJson>,
    pub accepted_count: usize,
    pub informational_count: usize,
}

/// Normalize an OPTIONAL calibrated effort into one of the grid's 4 fixed column keys. `None`
/// (a deterministic-floor/preview finding never gets a calibrated effort estimate — see
/// `effort_hours_bounds`) and any value outside `low`/`medium`/`high` both land in
/// `"unscoped"` rather than silently dropping the finding from the grid.
pub(crate) fn grid_effort_key(effort: Option<&str>) -> &'static str {
    match effort {
        Some("low") => "low",
        Some("medium") => "medium",
        Some("high") => "high",
        _ => "unscoped",
    }
}

/// FIX 6 — project the `do_now`/`do_next`/`plan` findings (Accepted and Informational are OUT
/// OF SCOPE, see [`PriorityGridJson`]'s doc comment) into the real 2-D grid. Rows/columns are
/// fixed orderings but only emitted when at least one finding uses them, so a small scan's
/// grid stays small instead of always rendering a fixed 4x4 of mostly-empty cells.
pub(crate) fn build_priority_grid(matrix: &MatrixJson) -> PriorityGridJson {
    const SEVERITY_ORDER: [&str; 4] = ["critical", "high", "medium", "low"];
    const EFFORT_ORDER: [&str; 4] = ["low", "medium", "high", "unscoped"];

    let action_findings: Vec<&FindingRefJson> = matrix
        .do_now
        .iter()
        .chain(matrix.do_next.iter())
        .chain(matrix.plan.iter())
        .collect();

    let present_severities: Vec<&str> = SEVERITY_ORDER
        .iter()
        .copied()
        .filter(|sev| action_findings.iter().any(|f| f.severity == *sev))
        .collect();
    let present_efforts: Vec<&str> = EFFORT_ORDER
        .iter()
        .copied()
        .filter(|eff| {
            action_findings
                .iter()
                .any(|f| grid_effort_key(f.effort.as_deref()) == *eff)
        })
        .collect();

    let rows = present_severities
        .iter()
        .map(|sev| {
            let cells = present_efforts
                .iter()
                .map(|eff| {
                    let mut findings: Vec<GridTagJson> = action_findings
                        .iter()
                        .filter(|f| {
                            f.severity == *sev && grid_effort_key(f.effort.as_deref()) == *eff
                        })
                        .map(|f| GridTagJson {
                            rule_id: f.rule_id.clone(),
                            repo: f.repo.clone(),
                            path: f.path.clone(),
                            line: f.line,
                            headline: f.headline.clone(),
                        })
                        .collect();
                    findings.sort_by(|a, b| {
                        (&a.repo, &a.path, a.line).cmp(&(&b.repo, &b.path, b.line))
                    });
                    GridCellJson { findings }
                })
                .collect();
            GridRowJson {
                severity: sev.to_string(),
                cells,
            }
        })
        .collect();

    PriorityGridJson {
        columns: present_efforts.into_iter().map(String::from).collect(),
        rows,
        accepted_count: matrix.accepted.len(),
        informational_count: matrix.informational.len(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CitationSourceJson {
    pub title: String,
    pub url: String,
    pub linter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CitationJson {
    /// `"grounded"` (corpus-cited) | `"preview"` (deterministic tool, no corpus citation) |
    /// `"related"` (AI-tier, a real standard attached by keyword match, not a rule that was
    /// checked; see [`RELATED_STANDARD_LABEL_PREFIX`]) | `"advisory"` (AI-tier, no corpus entry
    /// at all).
    pub kind: String,
    /// One-line, template-ready label (e.g. "AI-advisory, model-inferred.").
    pub label: String,
    pub sources: Vec<CitationSourceJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedSiteJson {
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub snippet: String,
    pub detail: String,
    pub severity: String,
    pub effort: Option<String>,
    pub confidence: Option<String>,
    pub disposition: String,
    /// C3-6: the SAME [`effective_bucket`] value `disposition`'s label was derived from (see
    /// `disposition_label`'s `bucket` parameter) — carried as its own field so a test (or a
    /// future template revision) can compare this site's bucket against its `FindingRefJson`
    /// counterpart in `matrix` directly, instead of parsing it back out of the label prose.
    pub bucket: String,
    /// C4-P2: this SITE's own resolved citation ([`citation_for_finding`]) — NOT necessarily
    /// identical to the parent [`CuratedGroupJson::citation`] shown once at the group header
    /// (which is that same call applied to the group's FIRST site only). Exists so a consumer
    /// needing the per-finding citation never has to assume group membership implies a shared
    /// citation; the cross-artifact equality gate compares this against `findings.json`'s
    /// `FindingRow::citation_label`/`citation_urls` for the same finding.
    pub citation: CitationJson,
    /// Raw rule ids absorbed into this finding during P1 clustering — an INTERNAL/traceability
    /// field (registry cross-reference, test assertions) never rendered as-is in any
    /// client-facing artifact. See [`also_matches_titles`] for the human-readable form the
    /// template actually renders.
    pub also_matches: Vec<String>,
    /// C4-P4 (residual defect 2): the human-readable TITLES of `also_matches`, resolved via
    /// [`also_matches_titles`] — this is what the PDF's "Also violates:" line renders. Never the
    /// raw ids above; an id with no resolvable corpus title is simply absent here.
    pub also_matches_titles: Vec<String>,
    /// The defect at THIS object (see `defect_headline`) — the template renders this as the
    /// bold per-finding heading; the group's own rule id + invariant title (`CuratedGroupJson`)
    /// is demoted to a smaller subtitle line for registry traceability.
    pub headline: String,
    /// The rule's AUTHORED, client-facing remediation text (see [`resolve_fix`]) — rendered as
    /// its own "Fix:" line in the template, distinct from `detail`'s explanation of the
    /// violation. Placeholder tokens (`<table>`, `<function-name>`, `<bucket>`, …) have already
    /// been substituted with this finding's own `path`/`captures` (or a readable generic when
    /// unfilled) — nothing downstream needs to re-resolve tokens. `None` (never a fabricated
    /// sentence, and NEVER the rule's `directive` — see [`resolve_fix`]'s doc comment) when the
    /// rule's default option has no authored `remediation` yet: the template must omit the Fix
    /// block entirely in that case rather than render an empty line.
    pub fix: Option<String>,
    /// **P2 (2026-09-29): the PRIMARY fix.** A codebase-specific remediation for THIS
    /// finding — names the real file/symbol/column from the finding's own evidence, and
    /// points at the repo's own correct pattern elsewhere when one exists. Generated at scan
    /// time by [`crate::ai_audit::generate_fix_specifics`] (this layer stays pure/
    /// synchronous — it only reads `Finding::fix_specific`, never calls a model) with a
    /// bounded validate-and-regenerate self-check (identifier grounding + non-contradiction;
    /// see that function's doc comment). The template renders this ABOVE `fix` — the rule's
    /// generic authored remediation above is now SECONDARY context only, never the lead line.
    /// `None` when fix-generation never ran (a deterministic-only scan makes no model calls
    /// at all) or gave up after retries — the template must render `fix` alone in that case,
    /// never a bare "Fix:" label with nothing after it. C3-1b: a fix-generation failure is a
    /// PIPELINE gap, not a confidence signal — it never hedges the finding or moves it out of
    /// `do_now`; `fix` (the rule's own authored remediation) still renders as the primary Fix
    /// line, so the row never ships with no fix at all.
    pub fix_for_this_finding: Option<String>,
    /// D8: the label printed beside this site's fix, [`TAILORED_FIX_LABEL`] or
    /// [`STANDARD_FIX_LABEL`] (see [`fix_type_label`]); empty when the site carries no fix text.
    /// The same value `findings.json` and the datasets carry in `fix_type`.
    pub fix_type: String,
    /// Pass-1 W7 (C5): every evidence site a merge ABSORBED into this row
    /// (`Finding::also_locations`, verbatim) — carried in the PDF's own data so the three
    /// artifacts describe the same site multiplicity. Before W7 this reached NO artifact:
    /// a row representing three detected sites exported as one line at one `path:line`, so
    /// an over-merge was observationally identical to a detection miss (tracker C5). The
    /// template renders it from [`Self::also_sites`], which adds the rule titles; this field
    /// keeps the verbatim records so the report JSON is complete.
    pub also_locations: Vec<crate::onboard::MergedLocation>,
    /// H6: the same absorbed sites, ready to print: location plus the human title of the rule
    /// each was flagged under (never the raw rule id). The PDF renders these as "Also found at".
    /// Empty for an un-merged row.
    pub also_sites: Vec<AlsoSiteJson>,
    /// Members of a merge that were flagged at THIS row's own place (or at a place already
    /// listed in `also_sites`), as finished sentences: "Also flagged here as <headline>
    /// (<severity>)". They are the same place, so they are not counted as another site.
    pub also_flagged_here: Vec<String>,
    /// For a leaked credential that another repo of the project also flags under the same name:
    /// one finished sentence naming those repos. Display only: the rows stay separate, because
    /// each service's owner rotates their own. Empty for every other row.
    pub same_credential_note: String,
    /// M2/M8: one finished sentence for every bound that moved this row's severity ("Severity
    /// lowered from high to info: no file or line was identified for this finding."). The
    /// severity shown is the adjusted one; this says what it was and why it changed. Empty
    /// when nothing moved it, and the PDF then prints nothing.
    pub severity_adjustments: Vec<String>,
    /// Pass-1 W7 (C5): total DISTINCT places this row represents: its own `path:line` plus every
    /// `also_locations` entry at a different place. A member absorbed at the row's own line is
    /// not another site. `1` for an un-merged row.
    pub site_count: usize,
    /// WO8: whether the reviewer edited any field of this site's row. The PDF prints nothing for
    /// it; the data carries it so the PDF's rows agree with `findings.json`'s.
    pub reviewer_edited: bool,
    /// The ids of the edits applied to this site's row, in the order they were applied.
    pub edit_ids: Vec<String>,
    /// Pass-1 W3 (A5.6): what this site's row IS — same flattened always-present kind wire
    /// as [`FindingRefJson::kind`], so the PDF's own data carries the taxonomy losslessly
    /// too (the PDF renders what it already renders; Pass 2/3 own presentation).
    #[serde(flatten)]
    pub kind: camerata_api_types::audit::KindOnWire,
}

/// H6: one absorbed evidence site as the PDF prints it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AlsoSiteJson {
    pub repo: String,
    pub path: String,
    pub line: usize,
    /// The corpus title of the rule this site was flagged under; `None` when the id has no
    /// corpus entry (an AI-invented id), so a raw id is never printed.
    pub title: Option<String>,
    /// The absorbed finding's own headline, when it carried one: what is printed when the rule
    /// has no corpus title.
    pub headline: Option<String>,
    /// The absorbed finding's own severity, only when it outranks the row's.
    pub severity: Option<String>,
    /// True for a site that is only a consequence of the row's root cause.
    pub consequence: bool,
}

/// The `Finding::captures` key an incremental run writes on a finding it carried forward from the
/// earlier inspection: the commit it was last found at.
pub const CARRIED_FROM_CAPTURE: &str = "carried_from";

/// H11: the commit of the earlier inspection `f` was carried forward from, when this run did not
/// re-audit its file. `None` for a row found this run.
pub fn carried_from(f: &Finding) -> Option<String> {
    f.captures
        .get(CARRIED_FROM_CAPTURE)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The `Finding::captures` key the fix stage writes, with [`STANDARD_FIX`], on a finding whose
/// model-written fix was turned down by the checks in every round and which ships the rule's own
/// authored fix instead (D8). Absent: the fix, if any, is the model's, written for this code.
pub const FIX_SOURCE_CAPTURE: &str = "fix_source";
/// The [`FIX_SOURCE_CAPTURE`] value of a finding that ships the rule's own authored fix.
pub const STANDARD_FIX: &str = "standard";
/// The `Finding::captures` key holding why the checks turned the model's fix down, for the
/// operator's failure report only. Never read by an exporter.
pub const FIX_REJECTION_CAPTURE: &str = "fix_rejection";

/// Mark `f` as shipping the rule's own authored fix with no rejection to report: the fix was
/// never asked of the model (the no-code pre-filter), so there are no reasons to keep.
pub(crate) fn mark_standard_fix_source(f: &mut Finding) {
    f.captures
        .insert(FIX_SOURCE_CAPTURE.to_string(), STANDARD_FIX.to_string());
}

/// Mark `f` as shipping the rule's own authored fix, keeping `why` (the checks' reasons, secrets
/// scrubbed and capped) for the operator's report.
pub(crate) fn mark_standard_fix(f: &mut Finding, why: &str) {
    f.captures
        .insert(FIX_SOURCE_CAPTURE.to_string(), STANDARD_FIX.to_string());
    let why = camerata_llm::failure::scrub_message(
        &camerata_secrets::redact_str(why),
        camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
    );
    f.captures.insert(FIX_REJECTION_CAPTURE.to_string(), why);
}

/// The `Finding::captures` key the detail stage writes, with [`SCAN_DETAIL`], on a finding whose
/// regenerated explanation was turned down in every round for inventing an exploit precondition
/// and which ships the scan's own statement plus the model's accepted fix direction instead
/// (R1-1). Never read by an exporter.
pub const DETAIL_SOURCE_CAPTURE: &str = "detail_source";
/// The [`DETAIL_SOURCE_CAPTURE`] value of a finding that ships the scan's own statement.
pub const SCAN_DETAIL: &str = "scan";
/// The `Finding::captures` key holding why the checks turned the regenerated explanation down,
/// for the operator's failure report only. Never read by an exporter.
pub const DETAIL_REJECTION_CAPTURE: &str = "detail_rejection";

/// Mark `f` as shipping the scan's own statement (and the accepted fix direction) as its detail,
/// keeping `why` (the checks' reasons, secrets scrubbed and capped) for the operator's report.
pub(crate) fn mark_scan_detail(f: &mut Finding, why: &str) {
    f.captures
        .insert(DETAIL_SOURCE_CAPTURE.to_string(), SCAN_DETAIL.to_string());
    let why = camerata_llm::failure::scrub_message(
        &camerata_secrets::redact_str(why),
        camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
    );
    f.captures.insert(DETAIL_REJECTION_CAPTURE.to_string(), why);
}

/// What every export prints for a fix written for this code that passed the checks.
pub const TAILORED_FIX_LABEL: &str = "Tailored fix";
/// What every export prints for the rule's own guidance (the fix of a row whose model-written fix
/// did not pass the checks, or that the fix stage never wrote one for). Neutral client wording.
pub const STANDARD_FIX_LABEL: &str = "Standard fix";

/// The label every export prints beside a finding's fix (D8): [`TAILORED_FIX_LABEL`] when the fix
/// was written for this code and passed the checks, [`STANDARD_FIX_LABEL`] when the fix is the
/// rule's own guidance, and empty when the row carries no fix text at all. One definition, read
/// by the PDF data, `findings.json`, every dataset and the workbook.
///
/// `specific_is_authored` is true when the finding's `fix_specific`, as the export prints it, is
/// the same text as the rule's own authored fix. An old saved run, an old scan cache or a
/// carried-forward row has no [`FIX_SOURCE_CAPTURE`] mark, and an authored fix an earlier build
/// wrote into `fix_specific` is still a Standard fix.
pub fn fix_type_label(
    f: &Finding,
    has_specific: bool,
    has_standard: bool,
    specific_is_authored: bool,
) -> &'static str {
    let standard_marked = specific_is_authored
        || f.captures
            .get(FIX_SOURCE_CAPTURE)
            .is_some_and(|v| v == STANDARD_FIX);
    if has_specific && !standard_marked {
        TAILORED_FIX_LABEL
    } else if has_specific || has_standard {
        STANDARD_FIX_LABEL
    } else {
        ""
    }
}

/// The absorbed members of a row, split by place: those at a DIFFERENT place from the row and
/// from each other (the "other sites"), and those at a place already named (the row's own, or
/// an earlier member's), flagged with whether that place is the row's own.
pub(crate) struct AbsorbedSplit<'a> {
    pub other_sites: Vec<&'a crate::onboard::MergedLocation>,
    pub same_place: Vec<(&'a crate::onboard::MergedLocation, bool)>,
}

/// See [`AbsorbedSplit`]. `own` is the row's own place ([`crate::onboard::site_place`]).
pub(crate) fn split_absorbed<'a>(
    own: crate::onboard::SitePlace,
    locs: &'a [crate::onboard::MergedLocation],
) -> AbsorbedSplit<'a> {
    let mut seen: std::collections::HashSet<crate::onboard::SitePlace> =
        std::collections::HashSet::new();
    seen.insert(own.clone());
    let mut split = AbsorbedSplit {
        other_sites: Vec::new(),
        same_place: Vec::new(),
    };
    for l in locs {
        let place = l.place();
        if seen.insert(place.clone()) {
            split.other_sites.push(l);
        } else {
            split.same_place.push((l, place == own));
        }
    }
    split
}

/// The sentence for a member absorbed at a place already named: "Also flagged here as
/// <label> (<severity>)" at the row's own place, "Also flagged at path:line as ..." at another
/// member's. `label` is the member's headline, else the rule's corpus title; with neither it
/// reads "under another rule". Never a raw rule id.
pub(crate) fn flagged_sentence(
    l: &crate::onboard::MergedLocation,
    at_own_place: bool,
    corpus_title: Option<&str>,
) -> String {
    let label = Some(l.headline.trim())
        .filter(|h| !h.is_empty())
        .or(corpus_title.filter(|t| !t.trim().is_empty()));
    let place = if at_own_place {
        "here".to_string()
    } else {
        format!("at {}:{}", l.path, l.line)
    };
    let rating = if l.severity.trim().is_empty() {
        String::new()
    } else {
        format!(" ({})", l.severity.trim())
    };
    match label {
        Some(h) => format!("Also flagged {place} as {h}{rating}"),
        None => format!("Also flagged {place} under another rule{rating}"),
    }
}

/// H6: the absorbed sites of `f` that are at a different place, titled from the corpus, with
/// the member's own headline when the corpus has no title and its severity when it outranks the
/// row's.
pub(crate) fn also_sites_for(
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<AlsoSiteJson> {
    let row_rank = severity_rank(&crate::severity_bounds::bounded_severity(f, corpus));
    split_absorbed(f.place(), &f.also_locations)
        .other_sites
        .into_iter()
        .map(|l| AlsoSiteJson {
            repo: l.repo.clone(),
            path: l.path.clone(),
            line: l.line,
            title: corpus
                .and_then(|c| c.get_by_id(&l.rule_id))
                .map(|r| r.title.clone()),
            headline: Some(l.headline.trim().to_string()).filter(|h| !h.is_empty()),
            severity: Some(normalize_severity(&l.severity))
                .filter(|_| !l.severity.trim().is_empty())
                .filter(|s| severity_rank(s) < row_rank),
            consequence: l.consequence,
        })
        .collect()
}

/// The "Also flagged here" sentences of `f` (see [`flagged_sentence`]).
pub(crate) fn also_flagged_here_for(
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<String> {
    split_absorbed(f.place(), &f.also_locations)
        .same_place
        .into_iter()
        .map(|(l, own)| {
            let title = corpus
                .and_then(|c| c.get_by_id(&l.rule_id))
                .map(|r| r.title.as_str());
            flagged_sentence(l, own, title)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedGroupJson {
    pub rule_id: String,
    pub title: String,
    pub citation: CitationJson,
    pub sites: Vec<CuratedSiteJson>,
    /// For a project of several repos: the repos this rule was found in, sorted. Empty for a
    /// project of one repo (the group then reads as it always has).
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthyRuleJson {
    pub rule_id: String,
    pub title: String,
    pub citation: CitationJson,
    /// For a project of several repos: the repos where the rule was checked and is clean. A rule
    /// that did not apply to a repo is not clean there, so it is named in
    /// [`Self::not_applicable_in`] and never here. Both are empty for a one-repo project.
    pub clean_in: Vec<String>,
    /// For a project of several repos: the repos the rule does not apply to.
    pub not_applicable_in: Vec<String>,
}

/// W1: one rule the ledger confirms did NOT run this scan, with the real reason — the
/// "excluded from this audit" section. Never a rule that merely wasn't SELECTED (this is
/// populated only from `ScanLedger::excluded_rules`, which only ever carries rules the
/// pipeline actually considered and then could not run).
#[derive(Debug, Clone, Serialize)]
pub struct ExcludedRuleJson {
    pub rule_id: String,
    pub title: String,
    /// Why this rule did not run this scan (e.g. "declares mechanical/architectural
    /// enforcement but has no wired detector", "AI/semantic review not requested this run").
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhatsHealthyJson {
    /// Audited rules with zero real (non-FP) findings this run, capped (S6) at
    /// `WHATS_HEALTHY_CAP` and sorted grounded-citation rules first.
    pub rules: Vec<HealthyRuleJson>,
    /// How many MORE zero-finding rules exist beyond `rules` (0 when nothing was cut) — S6's
    /// "N further rules verified clean this run" summary line, so capping never silently
    /// drops a rule from the report, it just stops listing them individually.
    pub further_clean_count: usize,
    /// True when the dependency pass ran and found nothing — a §6 positive per the design
    /// doc ("Clean = a §6 positive").
    pub dependency_clean: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyFindingJson {
    /// `<package>@<version>`.
    pub package: String,
    pub advisory: String,
    pub severity: String,
    pub repo: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencySnapshotJson {
    pub rows: Vec<DependencyFindingJson>,
    /// Honesty line(s) about dep-audit coverage (e.g. osv-scanner unavailable this run).
    pub coverage_notes: Vec<String>,
    pub clean: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodologyJson {
    pub candidates_reviewed: usize,
    pub excluded_false_positive: usize,
    /// P5: mirrors `ExecutiveSummaryJson::held_for_review` — carried here too so the
    /// Methodology section's own reconciling line (§`typ` "N candidate findings...") can show
    /// it without the template reaching back into `executive_summary`.
    pub held_for_review: usize,
    /// FIX 3 (2026-09-13 review): a short, FACTUAL "what happens next" paragraph, rendered in
    /// its own section right before Methodology (`typ:397`'s heading). Authored prose, same
    /// pattern as `deterministic_note`/`ai_tier_note` below — never an LLM call, never
    /// pitch-toned framing (no urgency language, no pricing claims beyond "the rate herein").
    pub next_steps: String,
    pub deterministic_note: String,
    pub ai_tier_note: String,
    pub not_done: Vec<String>,
    pub severity_scale_note: String,
    /// W6: same disclosure list as `ExecutiveSummaryJson::failed_passes` — see
    /// `failed_pass_disclosures`'s doc comment. Rendered here too (never JUST in the
    /// summary) so the more technical Methodology section also states explicitly that a pass
    /// did not run, rather than the reader having to notice its absence from the findings.
    #[serde(default)]
    pub failed_passes: Vec<String>,
    /// H5: one plain sentence telling the reader what a "related standard" citation means (see
    /// [`RELATED_STANDARD_LABEL_PREFIX`]). Rendered in the Methodology section, the report's
    /// guide to reading its findings.
    pub citation_note: String,
}

/// The serializer's single output type — mirrors the §4 anatomy sections in
/// `docs/design/2026-07-23_brownfield-audit-report.md` in order.
#[derive(Debug, Clone, Serialize)]
pub struct AuditReportJson {
    /// The package is for the operator's own review. Not part of the PDF's data: the exporters
    /// that build from this value beside the PDF (`findings.json`) read it to decide whether the
    /// group reviewer's triage columns go in.
    #[serde(skip)]
    pub internal: bool,
    /// The sites the full PDF would print (design A14 item 4).
    pub site_total: usize,
    /// True above [`FULL_PDF_SITE_LIMIT`] sites: only the reshaped PDF is rendered. The per-site
    /// cards, the held-for-review cards and the matrix lists are empty, and
    /// [`Self::reduced_matrix`] carries the counts they would have given.
    pub reduced: bool,
    /// The matrix rows' counts by bucket, severity and kind. Filled only when [`Self::reduced`].
    pub reduced_matrix: Vec<MatrixTallyJson>,
    /// The sites the group reviewer escalated, with their decisions.
    pub escalations: EscalationsJson,
    /// One entry for each sub-group: its summary, cause, fix once and a worked example.
    pub sub_group_patterns: Vec<PatternJson>,
    /// Said in the Patterns section when the report has findings and no review tree (so no
    /// pattern), empty otherwise.
    pub sub_group_patterns_note: String,
    /// P5: `"raw"` (nobody has reviewed anything this session) or `"reviewed"` (at least one
    /// finding carries an explicit auditor disposition). Read directly by the Typst template
    /// (`d.review_state`) to gate the per-page draft banner and the Methodology section's
    /// review-claim wording — see [`ReviewState`]'s doc comment.
    pub review_state: ReviewState,
    pub cover: CoverJson,
    /// W27: whether the bundled brand kit's logo mark renders (cover and running header).
    /// True only when the resolved brand IS the kit's brand ([`brand_kit_matches`]); any other
    /// agency's brand gets the text wordmark alone, so the template never puts one agency's
    /// logo on another's report.
    pub brand_mark: bool,
    /// The confidentiality marking (2026-10-24 decision): the footer line on every PDF page and
    /// the handling note on the first. The workbook prints the same `line` in every sheet's
    /// footer ([`confidentiality_marking`] is the one producer).
    pub confidentiality: ConfidentialityJson,
    pub executive_summary: ExecutiveSummaryJson,
    pub three_things: ThreeThingsJson,
    pub scorecard: ScorecardJson,
    pub matrix: MatrixJson,
    /// FIX 6 (2026-09-13 review): the real severity x effort grid the template renders under
    /// the "Severity x effort" heading — derived from `matrix` (see [`build_priority_grid`]),
    /// not an independent bucketing. `matrix` itself is kept (still backs the exec-summary
    /// counts and the "Three things this week" box's `do_now` selection); the template no
    /// longer reads `matrix` directly for its own section.
    pub priority_grid: PriorityGridJson,
    /// C4-P4 (residual defect 5): ordered by severity desc, then bucket (do_now -> do_next ->
    /// plan -> accepted), then confidence — see [`curated_group_sort_key`] — NEVER the old
    /// BTreeMap-alphabetical-by-rule-id iteration order, which could and did land a live
    /// critical pages deep behind a run of alphabetically-earlier lows. A group qualifies for
    /// THIS list when at least one of its sites is in an action/accepted bucket (i.e. not
    /// EVERY site in the group is informational or held (C5-4)) — see
    /// [`held_for_review_findings`] for the complementary, fully-appendix groups.
    pub curated_findings: Vec<CuratedGroupJson>,
    /// C4-P4 (residual defect 5) / C5-4: a group whose sites are ALL `bucket == "informational"`
    /// or `bucket == "held"` never belongs under "Curated findings" client-facing prose (that
    /// heading promises action items) — it renders in its own "Held for review" section instead,
    /// titled to match the executive summary's own "held for a human reviewer's judgment call"
    /// framing. This is also where a high/critical calibration hedge (`held`) now lands — see
    /// `is_held_for_review`'s doc comment — rendered with its OWN true severity, not capped like
    /// an `informational` row. A MIXED group (some sites curated, some
    /// informational/held — proven possible by
    /// `matrix_and_curated_site_bucket_fields_agree_and_a_hedged_finding_is_never_an_action_bucket`'s
    /// sibling cross-artifact test) stays entirely in `curated_findings` above: splitting a
    /// single rule's sites across two top-level sections would fragment one finding's
    /// citation/title header across two places in the document for no reader benefit, so the
    /// bar for moving a GROUP here is "zero curated sites," not "any informational/held site."
    pub held_for_review_findings: Vec<CuratedGroupJson>,
    /// Where the services of a multi-repo project differ on a rule: an observation after the
    /// findings, with no severity, in no total. `None` (and nothing rendered) for one repo.
    pub patterns_across_services: Option<PatternsAcrossServicesJson>,
    pub whats_healthy: WhatsHealthyJson,
    /// W1: rules the ledger confirms did NOT run this scan, each with its real reason — the
    /// "excluded from this audit" section. Empty whenever `report.ledger` carries no rule
    /// data (pre-W1 reports / fixtures that never populate it) — see `has_rule_ledger_data`
    /// in `build_report_json`. Sorted by rule id for stable output.
    #[serde(default)]
    pub rules_not_run: Vec<ExcludedRuleJson>,
    pub dependency_snapshot: DependencySnapshotJson,
    pub methodology: MethodologyJson,
    /// H4: what this scan did not cover, by pass and by path (see [`coverage_gaps`]). Rendered
    /// in the PDF's Coverage block, the workbook's Coverage sheet and `findings.json`.
    pub coverage_gaps: CoverageGapsJson,
    pub disclaimer: String,
}
