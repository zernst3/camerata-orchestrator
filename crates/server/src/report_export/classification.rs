//! Category labels, matrix buckets, ranks, hold and hedge rules, headlines, effort
//! and the evidence gate.

use super::*;

/// The strftime format every client-facing "generated at" string uses (the PDF cover's
/// `generated_at`, the workbook Index's "Generated" row). Minute precision, explicit UTC.
/// Shared so [`generated_at_label`] and the parse back out of it in [`compile_pdf`] can never
/// drift apart.
pub(crate) const GENERATED_AT_FORMAT: &str = "%Y-%m-%d %H:%M UTC";

/// The instant this audit RAN, recovered from the scan's own provenance: `finished_at` when it
/// is a real RFC3339 instant, else `started_at`, else `None` (a report whose provenance was
/// never stamped, or was stamped with something unparseable — never a silently invented time).
///
/// Every timestamp a deliverable embeds derives from this, never from the wall clock at
/// serialization time: the PDF cover's "Generated" line, the PDF's own `SOURCE_DATE_EPOCH`
/// creation timestamp, the workbook Index's "Generated" row and the workbook's
/// `docProps/core.xml` `dcterms:created`/`dcterms:modified`.
///
/// Two reasons, and the second is the stronger one:
///   1. Reproducibility. With the clock read at serialization time, two exports of ONE report
///      were not byte-identical, so the product could not prove two deliverables were the same
///      report. (It also made `crates/cli/tests/inspect_export_equivalence.rs` flake about one
///      run in four, which is how the problem was noticed rather than what it was.)
///   2. Correctness for a client deliverable. A client reading the file's metadata wants to
///      know when the AUDIT ran. "When the bytes happened to be serialized" is an answer to a
///      question nobody asked, and it actively misleads on a re-export: a PDF regenerated
///      months later from an unchanged report claimed to have been generated months after the
///      audit it describes.
///
/// Deliberately NOT a constant. Pinning to a fixed epoch would buy reproducibility while
/// throwing away the true statement, which is the half that matters to the reader.
pub(crate) fn scan_instant(
    provenance: &crate::onboard::ScanProvenance,
) -> Option<chrono::DateTime<chrono::Utc>> {
    for raw in [&provenance.finished_at, &provenance.started_at] {
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw.trim()) {
            return Some(parsed.with_timezone(&chrono::Utc));
        }
    }
    None
}

/// The client-facing "generated at" string for a scan: [`scan_instant`] rendered in
/// [`GENERATED_AT_FORMAT`].
///
/// Falls back to the wall clock ONLY when provenance carries no parseable instant at all. That
/// case is non-reproducible by construction and is accepted rather than papered over: a report
/// with no recorded scan time has no true answer to give, and inventing a fixed one would
/// print a confident lie. Every real scan path stamps `started_at`/`finished_at`.
pub(crate) fn generated_at_label(provenance: &crate::onboard::ScanProvenance) -> String {
    scan_instant(provenance)
        .unwrap_or_else(chrono::Utc::now)
        .format(GENERATED_AT_FORMAT)
        .to_string()
}

/// A small, generic technical-acronym allowlist for cosmetic title-casing — NOT a rule
/// taxonomy, just common initialisms that should stay upper ("RLS", not "Rls") wherever they
/// show up in a category key, whether that key came from a corpus rule's lowercase `domain`
/// (`"supabase:rls"`) or a rule id's own uppercase tokens (`"SUPABASE-RLS-ENABLED-1"`).
pub(crate) const CATEGORY_ACRONYMS: &[&str] = &[
    "RLS", "SQL", "JWT", "API", "TLS", "SSL", "CSS", "UI", "URL", "URI", "CVE", "CORS", "HTML",
    "JSON", "CSP", "XSS", "CSRF", "JS", "TS", "SSR", "SSG", "CLI", "DB",
];

/// Title-case one category-key token: a known short acronym stays upper (case-insensitive
/// match); anything else becomes `Capitalized-then-lowercase` regardless of its original
/// casing. Purely cosmetic — see `category_for`.
pub(crate) fn title_case_token(tok: &str) -> String {
    let upper = tok.to_ascii_uppercase();
    if CATEGORY_ACRONYMS.contains(&upper.as_str()) {
        return upper;
    }
    let mut chars = tok.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
    }
}

/// Turn a raw category key (a corpus rule's `domain`, e.g. `"supabase:rls"`, or a rule id
/// used as a fallback, e.g. `"SUPABASE-RLS-ENABLED-1"`) into a reader-facing label: split on
/// `-`/`:`/`_`, drop pure-numeric segments, title-case up to the first two remaining tokens
/// (`"supabase:rls"` -> `"Supabase RLS"`).
pub(crate) fn prettify_category_key(key: &str) -> String {
    let tokens: Vec<&str> = key
        .split(['-', ':', '_'])
        .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
        .take(2)
        .collect();
    if tokens.is_empty() {
        // Same generic bucket the A9 ladder ends in, named once — a key with no usable
        // tokens and a rule id no source can categorize are the same situation to a reader.
        return UNCATEGORIZED_CATEGORY.to_string();
    }
    tokens
        .iter()
        .map(|t| title_case_token(t))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The ONE label a category falls back to when no real category can be established: not from
/// the corpus (the rule has no entry), not from the finding's own calibrated `category`, and
/// not from the closed semantic taxonomy. One honest generic bucket, in client language.
///
/// A9: this replaced slugifying the rule id itself. The old terminal fallback was
/// `prettify_category_key(rule_id)`, which turns `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` into
/// `"Arch No"` and `LEGACY-UNMAPPED-1` into `"Legacy Unmapped"`. Those strings reached the PDF
/// scorecard's Category column AND — because `xlsx_export` names its per-area worksheets from
/// the category — became client-facing Excel TAB names. A client opened the deliverable and
/// saw a tab called "Arch No". A rule id is internal vocabulary; no amount of title-casing
/// turns it into a category, and a per-rule mangled label is strictly worse than one honest
/// generic one because it also fragments the scorecard into bogus single-row categories.
pub(crate) const UNCATEGORIZED_CATEGORY: &str = "Other";

/// Client-facing display labels for the closed semantic taxonomy
/// (`ai_audit::KNOWN_CATEGORIES`). Only the keys whose mechanical prettification is INTERNAL
/// vocabulary are listed; every other key already reads as client language through
/// `prettify_category_key` (`injection` -> "Injection", `rls-policy` -> "RLS Policy",
/// `secret-exposure` -> "Secret Exposure", ...) and is deliberately not restated here, so this
/// table cannot drift into a second, parallel taxonomy. It RENAMES the existing taxonomy for
/// the reader; it never adds to it.
pub(crate) const CATEGORY_DISPLAY_OVERRIDES: &[(&str, &str)] = &[
    // "Arch Conformance" is our governance vocabulary; the client's word is "Architecture".
    ("arch-conformance", "Architecture"),
    // "Testing Style" names an internal rule family; the client-facing subject is testing.
    ("testing-style", "Testing"),
];

/// Render a category KEY (a corpus rule's `domain`, or a closed-taxonomy class) as the label a
/// client reads: an explicit override where the mechanical prettification would leak internal
/// vocabulary, otherwise [`prettify_category_key`].
pub(crate) fn category_display_label(key: &str) -> String {
    if let Some((_, label)) = CATEGORY_DISPLAY_OVERRIDES.iter().find(|(k, _)| *k == key) {
        return (*label).to_string();
    }
    prettify_category_key(key)
}

/// Category for the scorecard: the corpus rule's own `domain` when known, else the closed
/// semantic taxonomy the rule id's TOKENS classify into
/// (`crate::ai_audit::categorize_rule_id` — the same repo-agnostic classifier calibration
/// uses), else [`UNCATEGORIZED_CATEGORY`].
///
/// A9: what this function must never do is return the rule id itself with title case applied.
/// A rule id is an internal identifier; its first two tokens are not a category, and the
/// three-deep ladder above exists so that the only way to reach the generic bucket is for
/// every real source of a category to have genuinely come up empty. See
/// [`UNCATEGORIZED_CATEGORY`] for the client-facing damage the old fallback did.
pub(crate) fn category_for(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> String {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(rule_id)) {
        return category_display_label(&rule.domain);
    }
    if let Some(class) = crate::ai_audit::categorize_rule_id(rule_id) {
        return category_display_label(&class);
    }
    UNCATEGORIZED_CATEGORY.to_string()
}

/// Category for a specific FINDING, preferred over [`category_for`] wherever a `Finding` (not
/// just a bare rule id) is available: a calibrated finding knows its own semantic class, and
/// that first-hand answer beats anything inferred from an id.
///
/// An AI-tier finding invents its own rule id per occurrence (`"AI-SQL-INJECTION-7"`), which
/// has no corpus entry to join against. Before C4-P2 the fallback mangled that invented id's
/// own tokens into a category key (`"AI-SQL-INJECTION-7"` -> `"Ai SQL"`), scattering AI
/// findings across nonsense per-finding "categories" instead of grouping them with their real
/// defect family, and leaving the scorecard's `audited_rules` count for that bogus category at
/// 0 (nothing in `provenance.audited_rule_ids` was ever going to share a made-up key).
///
/// C4-P2 (partial mitigation): when the finding's own rule id has no corpus entry, prefer its
/// structured `category` field — the closed taxonomy calibration already classified it into
/// (see `ai_audit::apply_verdicts`'s `is_known_category` gate, which only ever writes a
/// trusted value there) — over anything derived from the rule id.
///
/// A9 (the rest of it): that mitigation only ever covered a CALIBRATED finding. A finding
/// whose `category` was never populated — a native-checker finding from `crates/checks`, an
/// AI-tier finding from a run where calibration did not write a known category, a row from a
/// report that predates the taxonomy — still fell through to `prettify_category_key(rule_id)`
/// and reached the client mangled. The tail is now [`category_for`]'s own three-deep ladder
/// (corpus domain, then the closed taxonomy by rule-id TOKENS, then one honest generic
/// bucket), so no path through this function can return a slugified rule id. The first branch
/// is unchanged: a deterministic finding with a real corpus entry is completely unaffected.
pub(crate) fn category_for_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    // The reviewer's category wins over the rule's own domain.
    let reviewer_category =
        overlay::reviewer_edited_field(finding, camerata_api_types::edits::EditField::Category);
    if let Some(rule) = corpus
        .and_then(|c| c.get_by_id(&finding.rule_id))
        .filter(|_| !reviewer_category)
    {
        return category_display_label(&rule.domain);
    }
    if let Some(cat) = finding.category.as_deref() {
        let cat = cat.trim();
        if !cat.is_empty() {
            return category_display_label(cat);
        }
    }
    category_for(&finding.rule_id, corpus)
}

/// Which matrix cell a finding lands in. The auditor's OWN disposition wins where one
/// exists (`Ignored`/`BaselineAccepted` -> Accepted; `TechDebt{Now}` -> Do now;
/// `TechDebt{Later}` -> Plan); only an `Unresolved` (still-open) finding falls back to the
/// computed severity×effort quadrant. `severity` MUST already be normalized
/// (`normalize_severity`) — a `critical` finding is ALWAYS `do_now` regardless of effort (a
/// deterministic-floor/preview finding never gets calibrated effort, and the worst finding in
/// the report must never sit un-actioned in "Do next" just because no one estimated the fix
/// time). `high` severity still needs `effort == Some("low")` for `do_now`; missing effort is
/// treated as "not low" there — conservative, so an uncalibrated high finding never gets
/// silently downgraded to a same-day fix. `medium`/`low` always land in `Plan`.
pub(crate) fn matrix_bucket(
    disposition: Disposition,
    severity: &str,
    effort: Option<&str>,
) -> &'static str {
    match disposition {
        Disposition::Ignored | Disposition::BaselineAccepted | Disposition::WaivedInline => {
            "accepted"
        }
        Disposition::TechDebtNow => "do_now",
        Disposition::TechDebtLater => "plan",
        // `Confirmed` shares `Unresolved`'s arm ON PURPOSE. The three dispositions above all
        // override the computed quadrant because each records a decision about WHEN or
        // WHETHER to fix; confirming records only that the finding is REAL, which is what the
        // quadrant already assumed. Giving it its own cell would be the UI inventing a
        // scheduling decision the auditor never made.
        Disposition::Confirmed | Disposition::Unresolved => {
            if severity == "critical" {
                return "do_now";
            }
            let low_effort = matches!(effort, Some("low"));
            match (severity == "high", low_effort) {
                (true, true) => "do_now",
                (true, false) => "do_next",
                (false, _) => "plan",
            }
        }
    }
}

/// THE single bucket computation for a finding (C3-6). Reused UNMODIFIED by all three export
/// surfaces — `build_report_json`'s matrix pass, that same function's curated-findings
/// disposition label (both in this file), and `xlsx_export::partition_rows`'s own
/// `FindingRow::bucket` — so a finding's bucket is computed exactly once and every artifact
/// renders that one answer. Before this, `matrix_bucket` alone was "the" bucket function in two
/// of those three call sites, with `is_informational`/`is_uncited_ai_finding` applied as a
/// separate caller-side gate ahead of it; the xlsx partition had NO such gate at all, so a
/// hedged (`needs-review`) or uncited-AI-tier row that the PDF/JSON correctly held out of
/// do_now/do_next/plan could still land in an action tier in the workbook — the root cause of
/// the three artifacts disagreeing on bucket counts.
///
/// This is also the fix for "bucket is a pure function of severity": folding the hedge/
/// provenance gates INTO the bucket function itself (rather than a pre-check some callers
/// remembered to apply and one didn't) means a finding's bucket is now genuinely
/// `f(severity, provenance tier, hedge state, disposition, effort)` — a hedged row, or an
/// AI-tier finding with no grounded citation, can route out of an action tier even though its
/// raw severity×effort quadrant alone would have placed it there. See `is_informational` and
/// `is_uncited_ai_finding` for the individual signals folded in here.
///
/// C5-4: [`is_held_for_review`] is checked FIRST, ahead of `is_informational` — a calibration
/// hedge routes to `"held"` at ANY severity, including critical/high, which `is_informational`'s
/// own hard invariant would otherwise keep in an action tier. See that function's doc comment
/// for the regression this closes.
///
/// C5-7 (closes the gate's `C5-5`-flagged open invariant): [`is_uncited_ai_finding`] is now
/// checked as its OWN branch, ahead of `is_informational`, rather than folded into the same
/// `|| is_uncited_ai_finding(...)` arm that fed `"informational"`. That shared arm is how a
/// high/critical uncited AI finding used to end up in `matrix.informational` carrying its real
/// (high/critical) severity — `is_uncited_ai_finding` is deliberately severity-blind (see its
/// own doc comment: "a critical, uncited finding is PRECISELY the case this gate exists to
/// catch"), but `informational` is everywhere else in this module a severity-CAPPED appendix
/// (`is_informational`'s hard "critical/high is never informational" rule), and the shared arm
/// silently exempted this one signal from that invariant. At high/critical severity an uncited
/// AI finding now routes to `"held"` instead — the same severity-unbounded "a human must decide"
/// destination `is_held_for_review` uses (here, the human decision needed is "supply or confirm
/// a citation" rather than "resolve a calibration hedge", but the destination, visibility, and
/// real-severity badge are identical). At medium/low/info it still routes to `"informational"`,
/// exactly as before — nothing changes for the severity band `is_informational`'s cap was
/// already written for.
///
/// C5-8 (R7 amended — re-route, never block): a FINAL check, after every other gate has had its
/// say, for the one case none of them cover — a row that earns an action bucket
/// (do_now/do_next/plan) on severity/effort/disposition alone, survives every other gate, but
/// has no fix to put in front of the client. Delegates entirely to
/// [`is_action_row_missing_a_fix`] — see that function's doc comment for the exact scope (in
/// particular, why an UNRECOGNIZED rule id is deliberately exempt). Shipping a recognized-but-
/// unauthored rule's row as an action item with a blank Fix block is worse than silence — it
/// reads as "we checked, here is what to do" when there is nothing to do yet. It is NOT dropped
/// (over-tell, never under-tell) and NOT a new refuse-to-export gate (this function always
/// returns a destination, never an error) — it is re-routed to `"held"`, the same
/// severity-unbounded bucket `is_held_for_review` and the C5-7 uncited-AI case both use, with
/// its own disclosed reason ("fix not generated" — a caller building this row's headline calls
/// `is_action_row_missing_a_fix` separately to know WHICH held-for-review reason applied,
/// mirroring `gate_uncited`'s identical pattern a few lines up in `build_report_json`).
/// `"accepted"` is deliberately EXEMPT: an auditor who already accepted the risk is not
/// expecting a fix at all, so a missing one there is not a gap this check needs to catch.
pub(crate) fn effective_bucket(
    finding: &Finding,
    disposition: Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
    chosen_option: Option<&str>,
) -> &'static str {
    if is_held_for_review(finding, disposition) {
        return "held";
    }
    if is_uncited_ai_finding(finding, corpus) {
        return if severity_rank(severity) <= severity_rank("high") {
            "held"
        } else {
            "informational"
        };
    }
    if is_informational(finding, disposition, severity, corpus, test_file_count) {
        return "informational";
    }
    let bucket = matrix_bucket(disposition, severity, finding.effort.as_deref());
    if is_action_row_missing_a_fix(
        finding,
        disposition,
        severity,
        corpus,
        test_file_count,
        chosen_option,
    ) {
        return "held";
    }
    // C6-B4 (amended): a missing ESTIMATE must NOT demote a row out of its action bucket.
    //
    // The fix gate above is a different case and stays: an action item with no fix of any kind is
    // unusable, so held-with-a-disclosed-reason is the honest destination. A missing effort band
    // is only a PACKAGING gap — the finding itself is confident, cited and actionable. Demoting it
    // would hide the engine's strongest rows (a deterministic security HIGH with no authored band
    // yet) out of the queue the reviewer reads first, which is the completeness failure this
    // product exists to avoid: noise is handled by merge/re-rank/re-badge/route, never by pushing a
    // real action item out of sight over a field the ESTIMATOR failed to supply. The row therefore
    // keeps its action bucket and renders an honest "not estimated this run" instead; the
    // corpus-load test (`every_deterministic_rule_option_with_remediation_has_an_authored_effort_band`)
    // is where missing bands get driven to zero, at BUILD time, where nothing is hidden from anyone.
    bucket
}

/// C5-8 (R7 amended): true when `finding` would land in an ACTION bucket
/// (do_now/do_next/plan) by severity/effort/disposition alone, survives every other
/// `effective_bucket` gate, and has no fix to show — neither a codebase-specific fix
/// (`Finding::fix_specific`) nor the rule's own authored remediation ([`resolve_fix`]).
/// [`effective_bucket`] delegates its own final check here (so the two can never drift) and a
/// caller building this row's headline calls this a SECOND time to know WHICH held-for-review
/// reason applied — the same duplication `gate_uncited`/`is_uncited_ai_finding` already accept
/// in `build_report_json`'s matrix-building loop, for the identical reason.
///
/// # Scoped to RECOGNIZED corpus rules only
/// This is the runtime safety net for the residual case P3's corpus-load gate is meant to make
/// rare: a rule the corpus KNOWS ABOUT, which should therefore already carry authored
/// remediation, somehow does not. It is deliberately NOT triggered merely because
/// `finding.rule_id` has no corpus entry at all — an unrecognized id (an AI-tier finding's own
/// invented rule id, or any rule the loaded corpus simply does not define) was never going to
/// have a rule-level fix to begin with; that is a separate, pre-existing, already-documented gap
/// (see `is_ai_tier`'s doc comment and `citation_join_still_labels_a_non_ai_tier_unknown_rule_id_as_ai_advisory`'s
/// test — e.g. `SEC-NO-RAW-SQL-CONCAT-1` has no TOML entry at all and is untouched by the P3
/// citation gate for the same reason), not a NEW one this check exists to catch. Concretely: an
/// AI-tier finding that classifies into a grounded citation class (C5-7's `AiFindingClass`) but
/// whose own invented rule id still has no corpus entry, and whose `fix_specific` generation
/// hasn't run in a given synchronous test, is UNCHANGED by this gate — exactly as it was before
/// C5-8 — because `resolve_fix` was never going to find a rule-level remediation for an invented
/// id regardless of this feature.
pub(crate) fn is_action_row_missing_a_fix(
    finding: &Finding,
    disposition: Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
    chosen_option: Option<&str>,
) -> bool {
    if is_held_for_review(finding, disposition) || is_uncited_ai_finding(finding, corpus) {
        return false;
    }
    if is_informational(finding, disposition, severity, corpus, test_file_count) {
        return false;
    }
    let bucket = matrix_bucket(disposition, severity, finding.effort.as_deref());
    if !matches!(bucket, "do_now" | "do_next" | "plan") {
        return false;
    }
    if has_any_fix(finding, corpus, chosen_option) {
        return false;
    }
    // Neither fix half exists. Only count that as the C5-8 gap when the rule is one the
    // loaded corpus actually recognizes — see this function's doc comment.
    corpus.and_then(|c| c.get_by_id(&finding.rule_id)).is_some()
}

/// C6-B4: the fix-availability gate's exact mirror for the OTHER half of a usable action row
/// — true when `finding` would land in an ACTION bucket (do_now/do_next/plan) by
/// severity/effort/disposition alone, survives every other `effective_bucket` gate, and has
/// NO remediation-effort estimate from ANY source ([`resolve_effort`]: neither the finding's
/// own calibrated `effort` nor the rule's authored band).
///
/// # Why this should rarely fire post-C6-B4.1
/// The corpus-wide test `every_deterministic_rule_option_with_remediation_has_an_authored_
/// effort_band` (in `camerata-rules`) now guarantees every deterministic (mechanical/
/// architectural) rule option that carries client-facing `remediation` ALSO carries an
/// authored `effort` band — which means a recognized rule's row almost never reaches this
/// gate in practice going forward: if it has a fix to show, [`resolve_effort`] can virtually
/// always find a number to go with it. This function exists as the SAME kind of belt-and-
/// suspenders runtime safety net [`is_action_row_missing_a_fix`] is for its own invariant
/// (a corpus-authoring gap slipping past the build-time test some other way, a non-default
/// resolved option, a corpus that failed to load at render time) — not an expected steady
/// state. The fifth-consecutive-run alarm C6-B4 exists to close is exactly this: an
/// unestimated deterministic row shipping silently in an action bucket. Now it either gets an
/// estimate or is visibly, honestly held — never silently shipped unestimated.
///
/// # Scoped to RECOGNIZED corpus rules only (mirrors the fix gate exactly)
/// Deliberately NOT triggered merely because `finding.rule_id` has no corpus entry, or no
/// corpus was loaded at all — that is the PRE-EXISTING, honestly-disclosed "not yet
/// estimated" gap the C4-P3 guardrail protects
/// (`a_row_with_no_calibration_and_no_corpus_still_exports_unrefused`: a bare finding with
/// no calibration and no corpus must still export, in its severity-driven action bucket,
/// reporting "not yet estimated" rather than vanishing). This gate only catches the NEWER,
/// narrower case: a rule the loaded corpus DOES recognize, which per C6-B4.1 should already
/// carry an authored band whenever it has a fix, somehow still has neither.
pub(crate) fn is_action_row_missing_an_estimate(
    finding: &Finding,
    disposition: Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
    chosen_option: Option<&str>,
) -> bool {
    if is_held_for_review(finding, disposition) || is_uncited_ai_finding(finding, corpus) {
        return false;
    }
    if is_informational(finding, disposition, severity, corpus, test_file_count) {
        return false;
    }
    let bucket = matrix_bucket(disposition, severity, finding.effort.as_deref());
    if !matches!(bucket, "do_now" | "do_next" | "plan") {
        return false;
    }
    if resolve_effort(finding, &finding.rule_id, corpus, chosen_option).is_some() {
        return false;
    }
    // No estimate from any source. Only count that as the C6-B4 gap when the rule is one the
    // loaded corpus actually recognizes — see this function's doc comment.
    corpus.and_then(|c| c.get_by_id(&finding.rule_id)).is_some()
}

/// Severity rank for ordering (0 = most severe, ascending). The canonical mapping for any
/// severity-based sort in this module; `xlsx_export::severity_rank` (same crate) delegates
/// here rather than keeping its own copy, so the two artifacts' tie-breaks can never drift.
pub(crate) fn severity_rank(sev: &str) -> u8 {
    camerata_rules::severity_bounds::severity_rank(sev)
}

/// C4-P4 (residual defect 5): bucket rank for ordering the "Curated findings" section — action
/// tiers lead (in the same do_now -> do_next -> plan order the severity x effort matrix and the
/// "three things" box already use), "accepted" (dispositioned risk, still curated) comes next,
/// and "informational"/"held" (C5-4) both sort last. In practice a group that reaches this
/// ranking always has at least one non-informational, non-held site (see
/// [`AuditReportJson::curated_findings`]'s doc comment on the group-eligibility rule), so the
/// `_ => 4` arm only ever matters for computing the MIN across a mixed group's sites, where a
/// same-rule informational/held companion site must never win the group's own (best) rank away
/// from its curated sibling.
pub(crate) fn curated_bucket_rank(bucket: &str) -> u8 {
    match bucket {
        "do_now" => 0,
        "do_next" => 1,
        "plan" => 2,
        "accepted" => 3,
        _ => 4, // "informational" | "held" (C5-4) or anything unrecognized.
    }
}

/// C4-P4 (residual defect 5): confidence rank for the FINAL tie-break in curated-group
/// ordering (after severity, then bucket) — a confidently-asserted finding sorts ahead of an
/// equally-severe, equally-bucketed one the calibrator flagged as debatable. Takes
/// [`CuratedSiteJson::confidence`] verbatim (already the canonical [`hedge_confidence`] value,
/// never a raw unreconciled read); an absent confidence (no calibration pass ran at all) is
/// treated as ordinary/medium rather than either extreme, since "no opinion recorded" is not
/// the same claim as "needs review."
pub(crate) fn curated_confidence_rank(confidence: Option<&str>) -> u8 {
    match confidence {
        Some("high") => 0,
        Some("medium") => 1,
        Some("low") => 2,
        Some("needs-review") => 3,
        _ => 1,
    }
}

/// C4-P4 (residual defect 5): the full sort key for one curated GROUP — the MIN (best/most
/// urgent) of `(severity_rank, curated_bucket_rank, curated_confidence_rank)` across every site
/// in the group, with the group's own `rule_id` as a final deterministic tie-break so two
/// equally-ranked groups never depend on map/vec iteration order. Using the MIN across sites
/// (rather than, say, the first site after the existing repo/path/line sort) means a mixed
/// group's own curated site always determines its position, never an informational companion
/// that happens to sort first within the group.
pub(crate) fn curated_group_sort_key(group: &CuratedGroupJson) -> (u8, u8, u8, String) {
    let best = group
        .sites
        .iter()
        .map(|s| {
            (
                severity_rank(&s.severity),
                curated_bucket_rank(&s.bucket),
                curated_confidence_rank(s.confidence.as_deref()),
            )
        })
        .min()
        .unwrap_or((u8::MAX, u8::MAX, u8::MAX));
    (best.0, best.1, best.2, group.rule_id.clone())
}

/// Provenance-tier rank for the WITHIN-bucket sort (C3-6, item 3): a deterministic/grounded
/// citation (a published standard, a real linter rule) outranks a scan-time preview tool,
/// which outranks an ungrounded AI-advisory citation — so a security-floor finding (typically
/// `"grounded"`) never prints after an architecture/advisory note (typically `"advisory"`) of
/// the same severity, even though raw severity alone can't tell them apart. 0 sorts first.
/// Takes a [`CitationJson::kind`] string (`"grounded"` | `"related"` | `"preview"` |
/// `"advisory"`); anything else is treated as the lowest tier rather than panicking on an
/// unrecognized value. H5: `"related"` keeps the rank `"grounded"` gave the same finding
/// before it was relabelled, so the honest label does not reorder anyone's report.
pub(crate) fn provenance_tier_rank(citation_kind: &str) -> u8 {
    match citation_kind {
        "grounded" | "related" => 0,
        "preview" => 1,
        _ => 2,
    }
}

// C3-1b (the cycle-2 queue-hardening notes): there used to be a
// `fix_generation_failed` gate here that read a `"[needs review: fix not generated]"` tag
// off `detail` and demoted a would-be `do_now` finding to `do_next` — conflating a PIPELINE
// failure (the fix-specific generation pass gave up) with a CONFIDENCE judgement about the
// finding. `crate::ai_audit::generate_fix_specifics` no longer writes that tag (or sets
// `needs_review`) on failure at all: it only logs to stderr and leaves `fix_specific` at
// `None`, and the row still ships a usable fix because `resolve_fix` (below) renders the
// rule's own authored remediation as `CuratedSiteJson::fix` regardless of whether
// `fix_specific` generated. Bucket placement is therefore driven ONLY by the finding's own
// severity/effort/disposition (`matrix_bucket`) and calibration's own doubt signal
// (`is_informational`'s `confidence == "needs-review"` check) — never by whether a SEPARATE
// AI pass happened to produce prose for it.

/// A style rule needs an established corpus before deviations are findings (Bug 4 §2d). A
/// repo with fewer than this many test files has no test corpus to speak of, so
/// `testing-style` deviations are conventions-to-consider, not defects.
pub(crate) const MIN_STYLE_CORPUS_FILES: usize = 3;

/// Rule DOMAINS whose `structured` (decision-shaped) rules are architectural/framework STANCE
/// rules — "the project hasn't adopted X" conventions rather than concrete defects. This is
/// the "universal or framework layer" of Bug 4 §2a, expressed over the corpus's real domain
/// vocabulary (the folder each rule lives under). Language-core domains (`rust`, `go`,
/// `python`, `java`, `csharp`, `ruby`) and security/infra-checker domains (`supabase`, `sql`,
/// `permissions`, `iac`, `ci-cd`) are deliberately ABSENT: a located violation there is a real
/// defect even in a tiny app. A whitelist (not a blacklist) so an unrecognized future domain
/// fails OPEN to over-telling — it stays in the action tier rather than being auto-demoted.
pub(crate) const STANCE_LAYER_DOMAINS: &[&str] = &[
    "universal",
    "api-layer",
    "fullstack",
    "ui",
    "javascript",
    "integration",
];

/// C5-4: whether a finding is HELD for a human reviewer's judgment call, INDEPENDENT of its own
/// severity — the calibrator's own hedge (`needs_review` / `confidence == "needs-review"`),
/// applied only to an OPEN (`Unresolved`) row (an auditor's explicit disposition — accepted /
/// tech-debt / FP — is a human call that already supersedes calibration's own doubt; see
/// `is_informational`'s identical "only ever re-bucket an OPEN row" gate for the same
/// reasoning). Routes to the `held` bucket (see [`MatrixJson::held`]), which — UNLIKE
/// `informational` — has NO severity ceiling: a high/critical row the calibrator itself flagged
/// debatable is precisely the row a client must see called out as unresolved doubt, not silently
/// left in an action tier next to findings nobody doubts.
///
/// This is the SECOND attempt at this fix (C5-4; the first, cycle-4's P2, regressed): the root
/// cause was that `is_informational`'s hard "a critical/high finding is never informational"
/// invariant was the ONLY gate checked, and `needs-review` was folded into that SAME
/// severity-capped predicate — so a high/critical hedge fell through the hard invariant and
/// stayed in `do_now`/`do_next`, contradicting its own exported `needs_review: true`, and the
/// executive summary's "held for review" count (which only ever counted the `informational`
/// appendix) silently undercounted the true number of hedged rows. Splitting the hedge gate out
/// into its OWN function, checked BEFORE `is_informational` in [`effective_bucket`], closes that
/// gap structurally: a hedge is a hedge regardless of severity, and `informational` stays
/// reserved for the three genuinely low-stakes signals `is_informational` still owns.
pub(crate) fn is_held_for_review(finding: &Finding, disposition: Disposition) -> bool {
    if disposition != Disposition::Unresolved {
        return false;
    }
    // M5: a row whose quoted snippet was not found near its line is held until the reviewer
    // decides it; the release gate also refuses to export it without that decision.
    // M4: a row whose text is real junk (an empty or placeholder explanation, the model's own
    // refusal, a JSON fragment) is held the same way, until the reviewer decides it.
    finding.needs_review
        || finding.confidence.as_deref() == Some("needs-review")
        || finding
            .captures
            .contains_key(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE)
        || finding
            .captures
            .contains_key(crate::junk_finding::JUNK_CONTENT_CAPTURE)
        // A12 item 2: put back by the reviewer after the engine threw it out, not ruled on yet.
        || finding
            .captures
            .contains_key(crate::engine_discard::ENGINE_RESTORED_CAPTURE)
}

/// Whether a finding is a "convention to consider" rather than an action item (Bug 4). Routes
/// to the `informational` matrix appendix instead of do_now/do_next/plan. NEVER true for a
/// critical/high finding or for a finding the auditor has explicitly dispositioned — the hard
/// invariant that keeps true defects in the action tiers, with ONE deliberate exception (D8,
/// below). `severity` must already be normalized. The four independent informational signals
/// (design §2a/2d + the `info` tier itself, plus D8) — the calibrator's own `needs-review`
/// hedge (former §2c) is now its OWN gate, [`is_held_for_review`], checked ahead of this one in
/// [`effective_bucket`]: a hedge routes to the `held` bucket at ANY severity, never into this
/// severity-capped appendix:
///   - D8: the `ARCH-OBSERVABILITY-*` convention class (`crate::is_observability_convention_class`),
///     checked BEFORE the hard critical/high invariant — the one rule-identity-driven signal
///     that is exempt from it, by owner mandate (see that function's doc comment),
///   - `info`-severity (e.g. the unexposed-schema RLS note from I3),
///   - `testing-style` category in a repo below the test-corpus threshold (§2d),
///   - absence-type (`located == false`) `structured` stance-layer rule at ≤ medium (§2a).
pub(crate) fn is_informational(
    finding: &Finding,
    disposition: Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
) -> bool {
    // Only ever re-bucket an OPEN row — an auditor's explicit call (accepted/tech-debt/FP)
    // keeps its own destination.
    if disposition != Disposition::Unresolved {
        return false;
    }
    // D8 (owner-mandated opinion-class cap): the ARCH-OBSERVABILITY-* convention class is
    // checked FIRST, ahead of the hard "critical/high is never informational" invariant right
    // below — deliberately, and ONLY for this one rule-IDENTITY-driven class. Every other
    // signal this function owns is a SEVERITY-DERIVED heuristic (an info-tier note, an
    // under-corpus testing-style deviation, an absence-type stance rule), which is exactly
    // what that hard invariant exists to protect — a buggy heuristic must never bury a real
    // high/critical defect. A rule-id match against an explicitly-authored, owner-mandated
    // LOW-cap class is not that kind of signal: it is an authored classification applied
    // before any severity was even assigned, not a derived guess about an already-produced
    // severity. `ai_audit`'s D8 ceiling pass (`apply_severity_ceiling_rule`) also forces this
    // class's exported severity to exactly Low in the normal pipeline; this check is the
    // structural backstop that keeps the bucket guarantee holding even if that pass somehow
    // did not run first (see `crate::is_observability_convention_class`'s doc comment for the
    // full two-part design — capping the severity string alone is not sufficient, since
    // `matrix_bucket` routes a bare "low" severity to the `"plan"` action bucket by default).
    if crate::is_observability_convention_class(&finding.rule_id) {
        return true;
    }
    // Hard invariant: a critical or high finding is never auto-informational, whatever family.
    if severity == "critical" || severity == "high" {
        return false;
    }
    // The `info` tier is informational by definition (nothing below `low` is an action item).
    if severity == "info" {
        return true;
    }
    // §2d — testing-style deviation with no test corpus to deviate FROM.
    if finding.category.as_deref() == Some("testing-style")
        && test_file_count < MIN_STYLE_CORPUS_FILES
    {
        return true;
    }
    // §2a — an absence-type structured stance-rule note (the generic-arch/style over-firing).
    if !finding.located {
        if let Some(rule) = corpus.and_then(|c| c.get_by_id(&finding.rule_id)) {
            if rule.enforcement == camerata_rules::EnforcementKind::Structured
                && STANCE_LAYER_DOMAINS.contains(&rule.domain.as_str())
            {
                return true;
            }
        }
    }
    false
}

/// Whether `f`'s own calibration justification — its `detail` (which carries any
/// `[calibrated: reason]`/`[needs review: reason]` tag `ai_audit::apply_verdicts` appends)
/// plus its `calibration_rationale` (set by the D5/D6 deterministic passes, or by the
/// upward-calibration guardrail itself) — records an exploit precondition. See
/// [`FindingRefJson::precondition_count`]'s doc comment: this is a RANKING tie-break only,
/// never a severity or bucket change.
pub(crate) fn finding_precondition_count(f: &Finding) -> usize {
    let text = format!(
        "{} {}",
        f.detail,
        f.calibration_rationale.as_deref().unwrap_or("")
    );
    usize::from(crate::ai_audit::mentions_exploit_precondition(&text))
}

/// See [`FindingRefJson::confidence_rank`]'s doc comment. Reads BOTH hedge fields (C4-P2 —
/// see [`is_held_for_review`]'s check for why) rather than `confidence` alone.
pub(crate) fn finding_confidence_rank(f: &Finding) -> u8 {
    u8::from(f.needs_review || f.confidence.as_deref() == Some("needs-review"))
}

// ── C4-P2 / C5-4: one hedge source of truth, shared by findings.json / the xlsx workbook / the
// PDF ──────────────────────────────────────────────────────────────────────────────────────
//
// Bug family (artifact-consistency hardening, see docs/plans for the originating report): the
// executive summary's "held for review" count, the structured `needs_review` boolean, and the
// `confidence` string could each tell a different story for the SAME finding — a row routed to
// the informational/held appendix for a reason OTHER than calibration doubt (an `info`-severity
// note, a testing-style deviation with no corpus, an absence-type stance rule) rendered with
// `needs_review: false` / `confidence: "high"` in `findings.json`/the workbook, even though the
// narrative text calls that same row "held for a human reviewer's judgment call" — an unhedged
// row sitting in the bucket whose entire definition is "needs a human to decide". Separately, a
// raw desync between `f.needs_review` and `f.confidence` (a verdict re-application, a merge)
// could make the two structured fields themselves disagree.
//
// The fix: ONE function decides whether a finding is hedged for EXPORT purposes, folding in
// BOTH raw calibration doubt (`f.needs_review` / `f.confidence == "needs-review"`) AND bucket
// placement (`bucket == "informational"` or `bucket == "held"` (C5-4) — being held for review IS
// a hedge, by definition of either bucket). `build_report_json` (the PDF's
// `CuratedSiteJson.confidence`) and `xlsx_export::partition_rows`
// (`FindingRow.confidence`/`FindingRow.needs_review`, which `findings.json` serializes verbatim)
// both call this ONE function with their own already-computed `bucket` (itself a single shared
// computation — see `effective_bucket`) — never re-deriving hedge state independently. This does
// NOT change bucket placement (a finding explicitly dispositioned out of `Unresolved` still
// routes via `matrix_bucket`, so a calibration-hedged row CAN legitimately sit in `"plan"`
// rather than `"held"` — see `is_held_for_review`'s "only an `Unresolved` row" gate); it only
// guarantees that WHEREVER a finding lands, its exported confidence/needs_review fields never
// contradict each other or the bucket that put it there.
pub(crate) fn is_hedged(f: &Finding, bucket: &str) -> bool {
    f.needs_review
        || f.confidence.as_deref() == Some("needs-review")
        || bucket == "informational"
        || bucket == "held"
}

/// The canonical EXPORTED confidence string for `f`, reconciled against [`is_hedged`] — never
/// `"needs-review"` unless `is_hedged` agrees, and never anything OTHER than `"needs-review"`
/// when it does. This is the one producer of the `confidence == "needs-review" ⇔ needs_review`
/// biconditional every artifact renders (`CuratedSiteJson::confidence`, `FindingRow::confidence`
/// / `FindingRow::needs_review`) — see [`is_hedged`]'s doc comment for the full rationale.
///
/// C4-P3: a deterministic-floor / RLS-replay finding never goes through the AI calibration pass
/// (`ai_audit::verify_findings`), so `f.confidence` is `None` for it FOREVER — not "not yet
/// evaluated", genuinely never evaluated, because there is no judgment call left for calibration
/// to make: the mini schema-state replay / content-rule match either holds or it does not. Left
/// as `None`, this prints "Confidence: not evaluated this run" beside a PROVEN critical (a live
/// secret, an unprotected table, a definer function with no pinned search_path) — which reads as
/// "we are not sure" next to exactly the findings a client is paying to act on first. Once a row
/// has survived [`is_hedged`] above (so R1's null-location downgrade, `is_informational`'s
/// absence/needs-review/testing-style checks, and a raw `needs_review` flag have ALL already had
/// their say — see the module-level doc comment on [`is_hedged`]), a `None`-confidence,
/// non-AI-tier row (`!is_ai_tier(f)`, i.e. origin `Deterministic` per `ai_audit::finding_origin`'s
/// own definition of that origin — `AdoptedAi`/`InventedAi` findings carry a real `confidence` or
/// are gated separately) is reported `"high"` BY CONSTRUCTION. This never fires for an AI-tier
/// finding whose confidence is `None` because ITS OWN calibration pass genuinely failed — that
/// stays `None` (the honest "not evaluated this run" gap is real there, since a model verdict was
/// actually expected and never arrived).
pub(crate) fn hedge_confidence(f: &Finding, bucket: &str) -> Option<String> {
    if is_hedged(f, bucket) {
        Some("needs-review".to_string())
    } else if f.confidence.is_none() && !is_ai_tier(f) {
        Some("high".to_string())
    } else {
        f.confidence.clone()
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finding_ref(
    f: &Finding,
    severity: &str,
    headline: String,
    bucket: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_option: Option<&str>,
) -> FindingRefJson {
    FindingRefJson {
        rule_id: f.rule_id.clone(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        severity: severity.to_string(),
        headline,
        // C4-P3: calibration's effort, else the rule's authored band — see `resolve_effort`'s
        // doc comment. Keeps the matrix table AND the "three things this week" box (which reads
        // `FindingRefJson::effort` for its hour estimate / total) in lockstep with
        // `CuratedSiteJson::effort`, so a proven do-now critical never shows an estimate in the
        // curated findings section but "not yet estimated" in the summary box above it.
        effort: resolve_effort(f, &f.rule_id, corpus, chosen_option),
        bucket: bucket.to_string(),
        precondition_count: finding_precondition_count(f),
        confidence_rank: finding_confidence_rank(f),
        root_cause_keys: crate::ai_audit::root_cause_credential_identities(f)
            .into_iter()
            .collect(),
        kind: camerata_api_types::audit::KindOnWire(f.kind.clone()),
    }
}

/// Item 1 (the biggest ask): derive a defect-first HEADLINE for one finding, distinct from
/// the rule's own invariant title. Mechanical, not hand-faked — every finding already carries
/// a `detail` string (the checker's own explanation of the violation, whether it came from the
/// deterministic floor, a preview tool, or the calibrated AI tier), and house convention is for
/// that `detail` to LEAD with the defect statement itself ("the profiles table has no RLS...")
/// before supporting evidence. Taking `detail`'s leading sentence as the headline therefore
/// generalizes beyond this one report's fixture: it works for any finding whose `detail` text
/// follows that convention, and degrades safely (falls back to the rule's own
/// title/rule_id, never an empty string) for the rare finding with no `detail` at all.
pub(crate) fn defect_headline(detail: &str, fallback: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    // C4-P4 (residual defect 1, 2026-10-01): sentence-boundary detection used to be "first '.'
    // anywhere" (falling back through ". ", then a bare trailing '.', then any bare '.'), which
    // cut a headline off mid-abbreviation any time `detail` led with "(e.g. ...)" / "i.e. ..." /
    // "etc." before its real first sentence ended — the period in the abbreviation looked
    // identical to a sentence-ending period to the old scan. `sentence_boundary` below is a
    // REAL sentence-boundary scan: a '.' only counts when it is outside an open `(...)`
    // parenthetical, not immediately preceded by a known abbreviation, and followed by
    // whitespace+an uppercase letter (or end of string). When no such boundary exists at all
    // (a single long run-on sentence with no real ending in sight), the headline is capped by
    // LENGTH with an ellipsis instead of truncating at an arbitrary/wrong character — never an
    // unbounded wall of text standing in for a "headline".
    match sentence_boundary(trimmed) {
        Some(end) => {
            let mut headline = trimmed[..end].trim().to_string();
            if !headline.ends_with('.') {
                headline.push('.');
            }
            headline
        }
        None => {
            if trimmed.chars().count() > HEADLINE_LENGTH_CAP {
                let capped: String = trimmed.chars().take(HEADLINE_LENGTH_CAP).collect();
                format!("{}...", capped.trim_end())
            } else {
                // Short enough to use whole (no ellipsis needed) — same trailing-period
                // normalization as the real-boundary branch above, for a consistent
                // headline-reads-like-a-sentence shape either way.
                let mut headline = trimmed.to_string();
                if !headline.ends_with('.') {
                    headline.push('.');
                }
                headline
            }
        }
    }
}

/// Headline length cap (characters), only ever applied when `detail` has no real sentence
/// boundary at all within that many characters — see `defect_headline`'s doc comment.
pub(crate) const HEADLINE_LENGTH_CAP: usize = 200;

/// Known sentence-internal abbreviations whose own period must never be mistaken for a
/// sentence-ending period, GENERAL across any `detail` text (not keyed to one rule/fixture) —
/// see `defect_headline`'s doc comment for the bug this closes. Matched case-insensitively
/// against the text immediately BEFORE and INCLUDING the candidate period.
pub(crate) const SENTENCE_ABBREVIATIONS: &[&str] = &["e.g.", "i.e.", "etc.", "vs."];

/// Find the end index (exclusive, just past the period) of the first REAL sentence boundary in
/// `s`, or `None` if there is no such boundary anywhere. See [`sentence_boundaries`] for the
/// full rule set (this is just its first hit) — kept as its own function because
/// [`defect_headline`] only ever wants the first boundary, not every one.
pub(crate) fn sentence_boundary(s: &str) -> Option<usize> {
    sentence_boundaries(s).first().copied()
}

/// REG-5: find every REAL sentence-boundary index (exclusive, just past the punctuation) in
/// `s` — the general scan [`sentence_boundary`] (first hit only) and
/// [`crate::ai_audit::split_into_sentences`] (every hit, to tokenize a `detail` into sentences
/// without fragmenting identifiers) both build on. A `.`/`!`/`?` is a real boundary only when
/// ALL of the following hold:
///
/// - it sits outside any backtick-delimited span (`` `...` `` is treated as fully opaque —
///   code/identifiers quoted in prose, e.g. `` `foo.bar()` ``, must never be scanned for
///   sentence punctuation at all, regardless of what they contain);
/// - it sits outside any open `(...)` parenthetical (so "(e.g. allowing X)" never splits there);
/// - for `.` specifically, it is NOT immediately preceded by a known abbreviation
///   (`SENTENCE_ABBREVIATIONS`, so a bare "i.e." / "etc." outside parens doesn't split either);
/// - it is followed, with NO whitespace required, by an uppercase letter (this is what still
///   catches a genuinely run-together sentence like "end.Start" and inserts the missing space
///   when the caller rejoins kept sentences with `" "` — the ONLY legitimate reason this
///   machinery was built), OR by whitespace then an uppercase letter, OR it is the very last
///   character in the string (a clean trailing sentence with nothing after it).
///
/// This is deliberately NOT "any `.` is a boundary" — that naive scan is REG-5: it used to treat
/// every period in `detail` (including the ones inside dotted calls, filenames, version
/// strings, and abbreviations — `client.auth.method()`, `settings.toml`,
/// `0001_migration.sql:12`, `e.g.`, `v1.2.3`) as a sentence end, fragment on it, then rejoin the
/// fragments with a forced single space — silently inserting a space after every such period
/// across headline/detail prose in the PDF, xlsx, and JSON alike (all three artifacts derive
/// from the same stored `Finding::detail`). A period is only a genuine boundary here when the
/// text immediately after it is SHAPED like the start of a new sentence (uppercase letter, or
/// end of string) — a lowercase letter or digit right after the period (the identifier/
/// filename/version/dotted-call case) never qualifies, boundary-shaped-ness alone included.
pub(crate) fn sentence_boundaries(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut paren_depth: i32 = 0;
    let mut in_backtick = false;
    for (i, c) in s.char_indices() {
        if c == '`' {
            in_backtick = !in_backtick;
            continue;
        }
        if in_backtick {
            continue;
        }
        match c {
            '(' => paren_depth += 1,
            ')' => paren_depth = (paren_depth - 1).max(0),
            '.' | '!' | '?' if paren_depth == 0 => {
                if c == '.' && ends_with_known_abbreviation(&s[..=i]) {
                    continue;
                }
                let rest_trimmed = s[i + c.len_utf8()..].trim_start();
                let boundary_shaped = rest_trimmed
                    .chars()
                    .next()
                    .map(char::is_uppercase)
                    .unwrap_or(true); // nothing after it at all: a clean trailing sentence.
                if boundary_shaped {
                    out.push(i + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn ends_with_known_abbreviation(prefix: &str) -> bool {
    let lower = prefix.to_ascii_lowercase();
    SENTENCE_ABBREVIATIONS.iter().any(|a| lower.ends_with(a))
}

/// Deterministic exec-summary narrative (never an LLM call). Overridden verbatim by
/// `ReportOptions::executive_summary_override` when the client supplies one. Special-cases
/// zero candidates (a plain "0 candidate finding(s) were reviewed..." reads as broken, not
/// clean) and uses real pluralization throughout (`noun`) rather than the "(s)" CLI-ism.
///
/// FIX 4 (2026-09-13 review, "page 3 states the top-3 three times"): this used to LEAD with a
/// blast-radius sentence composed from the top do-now findings' own headlines — the SAME 3
/// findings the "If you only do three things this week" box (immediately below, on the same
/// page) already names in full. Between that box and the (also-dropped) "Top priority items"
/// bullet list, the top 3 were stated three separate times on one page. The exec summary is
/// now curation-and-counts ONLY: the false-positive triage line, then the single one-pass
/// bucket-count partition.
///
/// # P5: review-state honesty + full reconciliation
/// `default_narrative` now takes `review_state` and branches on it — this is the fix for a
/// raw (nobody has reviewed anything this run) export whose narrative used to claim "N were
/// reviewed... 0 were dispositioned as false positives by the reviewer" no matter what, even
/// when NO reviewer had ever opened the report. See [`ReviewState`]'s doc comment for the
/// derivation.
/// - `Raw`: describes what the ENGINE did ("The engine analyzed N files and produced C
///   candidate findings..."). NEVER the phrase "reviewed by a human reviewer" or
///   "dispositioned by a human reviewer" — grep-tested (see
///   `raw_export_carries_the_draft_flag_and_a_fully_reconciling_engine_voiced_narrative`).
/// - `Reviewed`: describes the reviewer's actual dispositions, same voice as before this pass.
///
/// In BOTH states the narrative reconciles EVERY candidate finding, with each number shown:
/// `candidates_reviewed == curated_total + held_for_review + excluded_fp +
/// dependency_advisories` always holds (by construction — see `build_report_json`: every
/// finding lands in exactly one of those four buckets). This is the fix for the "41 reviewed;
/// 0 excluded... 18 curated" bug, where the other 23 needs-review rows were held out of the
/// curated set and never counted anywhere in the narrative.
#[allow(clippy::too_many_arguments)]
pub(crate) fn default_narrative(
    review_state: ReviewState,
    files_scanned: usize,
    candidates_reviewed: usize,
    excluded_fp: usize,
    curated_total: usize,
    held_for_review: usize,
    dependency_advisories: usize,
    do_now: usize,
    do_next: usize,
    plan: usize,
    accepted: usize,
) -> String {
    if candidates_reviewed == 0 {
        return "The scan surfaced no candidate findings to review in this run.".to_string();
    }
    let dep_clause = if dependency_advisories == 0 {
        String::new()
    } else {
        format!(
            " A further {} tracked separately in the dependency snapshot.",
            noun(
                dependency_advisories,
                "dependency advisory is",
                "dependency advisories are"
            ),
        )
    };
    match review_state {
        ReviewState::Raw => format!(
            "This is a draft export: no finding in this run has had a human triage pass. The \
             engine analyzed {} and produced {}. {curated_total} {} curated for action \
             ({do_now} do now, {do_next} do next, {plan} planned, {accepted} already carried \
             as accepted risk from a prior baseline or in-code waiver), {held_for_review} {} \
             held for a human reviewer's judgment call, and {excluded_fp} {} auto-excluded as \
             likely false positives.{dep_clause}",
            noun(files_scanned, "file", "files"),
            noun(
                candidates_reviewed,
                "candidate finding",
                "candidate findings"
            ),
            if curated_total == 1 { "is" } else { "are" },
            if held_for_review == 1 { "is" } else { "are" },
            if excluded_fp == 1 { "was" } else { "were" },
        ),
        ReviewState::Reviewed => format!(
            "{} were reviewed by a human reviewer; {} {} dispositioned as false positives and \
             excluded entirely from this report. Of the remaining {}, {curated_total} {} \
             curated for action ({do_now} do now, {do_next} do next, {plan} planned, \
             {accepted} accepted as risk) and {held_for_review} {} held for further review.\
             {dep_clause}",
            noun(
                candidates_reviewed,
                "candidate finding",
                "candidate findings"
            ),
            excluded_fp,
            if excluded_fp == 1 { "was" } else { "were" },
            noun(curated_total + held_for_review, "finding", "findings"),
            if curated_total == 1 { "is" } else { "are" },
            if held_for_review == 1 { "is" } else { "are" },
        ),
    }
}

/// P5: the AI-tier Methodology paragraph must never claim a human already reviewed advisory
/// findings when this export is [`ReviewState::Raw`] — the paragraph used to unconditionally
/// say "Every advisory finding is reviewed and dispositioned by a human reviewer before it
/// appears here", which is a flat fabrication on a raw, pre-review export. Neither branch
/// contains the literal phrases "reviewed by a human reviewer" or "dispositioned by a human
/// reviewer" in the `Raw` case — grep-tested alongside `default_narrative`'s own gate.
pub(crate) fn ai_tier_note_for(
    review_state: ReviewState,
    repos_not_run: usize,
    total_repos: usize,
) -> String {
    let repos = |n: usize| {
        if n == 1 {
            "1 repo".to_string()
        } else {
            format!("{n} repos")
        }
    };
    if repos_not_run > 0 && repos_not_run >= total_repos {
        // The tier ran nowhere: say so, and make no claim about a review that did not happen.
        return format!(
            "The advisory language-model tier did not run for {}; see Coverage. Every finding \
             produced in this run came from the deterministic tier.",
            repos(repos_not_run)
        );
    }
    let ran = match review_state {
        ReviewState::Reviewed => {
            "A second, advisory tier uses a calibrated language-model review for findings that \
             require semantic judgment. Every advisory finding in this export has been \
             reviewed and dispositioned by a human reviewer; lower-confidence items are marked \
             needs-review."
                .to_string()
        }
        ReviewState::Raw => {
            "A second, advisory tier uses a calibrated language-model review for findings that \
             require semantic judgment. This export has not yet had a human triage pass: every \
             advisory finding here is the engine's own calibrated output, awaiting a human \
             reviewer's judgment before any is accepted, marked tech debt, or ruled out. \
             Lower-confidence items are marked needs-review."
                .to_string()
        }
    };
    if repos_not_run == 0 {
        ran
    } else {
        format!(
            "{ran} The advisory language-model tier did not run for {}; see Coverage.",
            repos(repos_not_run)
        )
    }
}

/// C4-P3: the EXPORTED remediation-effort tier for `f`, reconciled against the rule it violates.
/// Order of preference:
/// 1. `f.effort` — the finding's OWN estimate, whether set by the AI calibration pass
///    (`ai_audit::verify_findings`) or by a deterministic detector's own sensible default at
///    finding-creation time (e.g. `onboard::audit::default_effort_for`). Never second-guessed
///    here.
/// 2. Failing that, the rule's AUTHORED effort band on the option this finding resolves against
///    (`RuleOption::effort`, set in the corpus TOML) — the rule author's floor/default estimate
///    for a finding that never went through calibration and whose detector built a bare
///    `Finding` with no per-finding default (the architectural/RLS-replay checkers —
///    `onboard::architectural::arch_violation_to_finding` sets `effort: None` unconditionally).
///    This is the fix for a PROVEN critical (an unprotected table, a definer function with no
///    pinned search_path) rendering "not estimated this run" forever just because its detector
///    never set one: the rule itself already carries a defensible default.
/// 3. `None` — genuinely no estimate available from either source. The caller's existing
///    "not yet estimated" render (`effort_hours_bounds`) and the run-wide `FailedPass{pass:
///    "hour estimation", ..}` disclosure (`ai_audit::verify_findings`) are the honest way to
///    say so; this function never fabricates a number, and — per the C4-P3 guardrail — nothing
///    downstream may ever refuse or drop a row because this returns `None`.
pub(crate) fn resolve_effort(
    f: &Finding,
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_option: Option<&str>,
) -> Option<String> {
    f.effort.clone().or_else(|| {
        corpus_rule_for(rule_id, corpus)
            .and_then(|rule| rule.resolved_option(chosen_option))
            .and_then(|opt| opt.effort.clone())
    })
}

/// Item 7's effort -> rough-hour mapping (a judgment call, documented here rather than buried
/// in a magic number): the calibration pass's three qualitative tiers translate to rough,
/// LABELED-as-rough wall-clock ranges a buyer can price against. `low` ~= a same-day, scoped
/// fix (rotate a key, flip a migration flag); `medium` ~= about a business day (touches a few
/// call sites or needs a short design pass); `high` ~= the better part of a week (a real
/// refactor or a cross-cutting change). Returns `None` for the hour bounds when effort was
/// never calibrated (a deterministic-floor/preview finding, per M3) — the label still reads
/// honestly ("not yet estimated") rather than silently guessing a number.
pub(crate) fn effort_hours_bounds(effort: Option<&str>) -> (Option<(u32, u32)>, String) {
    match effort {
        Some("low") => (Some((2, 4)), "2 to 4 hours".to_string()),
        Some("medium") => (
            Some((8, 16)),
            "1 to 2 days (about 8 to 16 hours)".to_string(),
        ),
        Some("high") => (
            Some((24, 40)),
            "3 to 5 days (about 24 to 40 hours)".to_string(),
        ),
        _ => (None, "not yet estimated".to_string()),
    }
}

/// Sum the rough hour ranges across item 7's (up to 3) do-now findings. Only sums items with a
/// calibrated effort; when one or more items have none, the total says so explicitly rather
/// than folding a guessed number into the sum.
pub(crate) fn total_hours_label(items: &[&FindingRefJson]) -> String {
    if items.is_empty() {
        return "No do-now items this run.".to_string();
    }
    let mut lo_sum = 0u32;
    let mut hi_sum = 0u32;
    let mut uncounted = 0usize;
    for f in items {
        match effort_hours_bounds(f.effort.as_deref()).0 {
            Some((lo, hi)) => {
                lo_sum += lo;
                hi_sum += hi;
            }
            None => uncounted += 1,
        }
    }
    let counted = items.len() - uncounted;
    if counted == 0 {
        return "None of these items has a calibrated effort estimate yet; ask the reviewer for \
                a rough scoping pass before pricing remediation."
            .to_string();
    }
    let mut label = format!("Roughly {lo_sum} to {hi_sum} hours total (rough estimate)");
    if uncounted > 0 {
        label.push_str(&format!(
            "; {} without an effort estimate yet {} not included in this total",
            noun(uncounted, "item", "items"),
            if uncounted == 1 { "is" } else { "are" }
        ));
    }
    label.push('.');
    label
}

/// Defect 4 (2026-10-07 grading pass) — the "evidence of violation" gate: a conservative,
/// deliberately simple NEGATION-PHRASE detector over a zero-finding rule's own
/// [`crate::ai_audit::RuleRecommendation::evidence`] text. Across three graded fixtures, a
/// multi-option/structured rule's own evidence sometimes reads as a plain description of the
/// exact violation the rule exists to catch — e.g. "three sequential writes with no
/// transaction", "an external chargeable call with no idempotency-key handling",
/// "authorization done as ad hoc checks inside business logic" — while the rule still reports
/// ZERO findings and ships under "What's healthy", because compliance was checked against
/// whichever option the model picked (which can itself be the non-compliant option the
/// evidence describes) rather than against the rule's own requirement. The engine wrote down
/// WHY the rule is violated and then told the client the rule is satisfied.
///
/// Any one of these phrases appearing is treated as "this evidence negates the rule's own
/// requirement" — deliberately conservative (a false positive costs a genuinely-healthy rule
/// one extra human look in `held`, never a dropped finding or a refused export; a false
/// negative is just the pre-existing gap this gate narrows, not a regression it introduces).
/// See [`build_report_json`]'s "What's healthy" section for the call site, and
/// [`parse_evidence_citation`] for how the matching advisory finding is anchored.
pub(crate) fn evidence_negates_requirement(evidence: &str) -> bool {
    const NEGATION_PHRASES: &[&str] = &["no ", "without ", "not ", "ad hoc", "only ", "missing"];
    let lower = evidence.to_ascii_lowercase();
    NEGATION_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

/// Best-effort split of a [`crate::ai_audit::RuleRecommendation::evidence`] citation
/// (`"path:line"` or `"path:line — short note"`, per that field's own doc comment) into its
/// path/line parts, anchoring the [`evidence_negates_requirement`] advisory finding at a real
/// location whenever the model's citation parses. Splits ONLY on an em dash or " - " (never a
/// bare hyphen) so a hyphenated path (`src/my-service/orders.rs:142`) is never mistaken for a
/// "path — note" separator.
///
/// # On the unparseable case (W21, 2026-10-19)
///
/// Returns `("", 0)` — an EMPTY path, not the evidence text.
///
/// It used to return `(evidence, 0)`, reasoned as "never drops the evidence text". The intent
/// was right and the mechanism was wrong: `path` is a client-facing FILE LOCATION rendered in
/// the PDF and the workbook, and the fallback put model prose into it. The measured result on
/// hearth was ten rows whose `path` was a sentence up to 180 characters long, e.g.
/// `"app/api/bookings/route.ts:15-39 — booking insert, payment insert, and slot update execute
/// as three separate awaits with no transaction boundary"`, shipped as a file path.
///
/// Nothing is actually lost. All three callers already put the evidence text in `snippet`
/// (and `report_export`'s gate quotes it in `detail` as well), so the text survives in the
/// fields built to hold prose. What changes is that a row which cannot be anchored now says
/// so by carrying no location, which is what `located: false` exists to express.
pub(crate) fn parse_evidence_citation(evidence: &str) -> (String, usize) {
    let trimmed = evidence.trim();
    // The note follows the place after an em dash, a spaced hyphen, a colon or a comma (the
    // house form of a client citation is `path:line: note`, and the text the models write is
    // rewritten to it at parse), so the place is what comes before the first of them.
    let head = [" \u{2014} ", " - ", ": ", ", "]
        .iter()
        .filter_map(|separator| trimmed.split_once(separator).map(|(head, _)| head))
        .min_by_key(|head| head.len())
        .unwrap_or(trimmed)
        .trim();
    if let Some((path, line_str)) = head.rsplit_once(':') {
        if let Ok(line) = line_str.trim().parse::<usize>() {
            if !path.is_empty() {
                return (path.to_string(), line);
            }
        }
    }
    (String::new(), 0)
}

/// Defect 4's injection point: for every rule the ledger would otherwise credit as healthy
/// (`ran && findings_emitted == 0`, and genuinely zero findings in `report.findings` too) whose
/// `RuleRecommendation::evidence` [`evidence_negates_requirement`], synthesize ONE advisory
/// `Finding` and append it to `report.findings` IN PLACE.
///
/// # Pass-1 W1 seal fix (2026-10-09)
///
/// This used to run INSIDE [`build_report_json`], against a CLONED report, with the clone then
/// silently shadowing that function's own `report` parameter — so the PDF's JSON saw the
/// synthetic rows, but `xlsx_export::build_workbook` / `build_findings_export` (which read the
/// ORIGINAL, un-injected `ScanReport` directly, never the exporter-local clone) never did. That
/// was the exact shape of the measured defect: `findings.json`'s own `summary` (cloned from the
/// PDF's `AuditReportJson`) described `N+K` rows while its `findings` array held only `N`.
///
/// The fix is a POSITION fix, not a logic fix: this function is now part of the SCAN pipeline,
/// not the export pipeline. It must run exactly once, after every other row-adding pass
/// (`crate::onboard::audit_repos`'s merges, `crate::merge_scan_preview`, the dependency audit)
/// and strictly BEFORE [`crate::onboard::ScanReport::seal`] — see that method's doc comment for
/// the full pipeline order. Every exporter then reads the SAME already-sealed row set, so the
/// three artifacts can never disagree on which rows exist. Records its own
/// `"evidence-gate-synthesis"` [`crate::scan_ledger::StageAccounting`] entry (an ADDITION-side
/// stage — see that type's doc comment) so the rows it adds are accounted exactly as a
/// row-removing merge stage already is, never invisible to the pipeline-integrity ledger.
///
/// A no-op (including no ledger stage — nothing to disclose) when `report.recommendations` is
/// empty, which covers the overwhelming majority of scans (deterministic-only, or no
/// multi-option rule selected this run). **Test-fixture note:** a fixture that wants to exercise
/// this gate must populate `recommendations` — the standing `export_invariants_gate` fixture in
/// this file does NOT (by design, to keep its own invariants independent of this gate), which is
/// exactly why this defect shipped undetected; see the `req_d1`/`req_d2`/`req_c1` fixtures below
/// for ones that deliberately populate it and assert `K > 0`.
pub fn apply_evidence_gate(report: &mut crate::onboard::ScanReport) {
    apply_zero_finding_evidence_check(report);
}

/// The evidence gate (Defect 4): the zero-finding half. See [`apply_evidence_gate`]'s doc
/// comment; this is its body.
pub(crate) fn apply_zero_finding_evidence_check(report: &mut crate::onboard::ScanReport) {
    if report.recommendations.is_empty() && report.recommendations_by_repo.is_empty() {
        return;
    }
    let healthy_ids: std::collections::HashSet<String> = if report.ledger.rules().next().is_some() {
        report.ledger.healthy_rule_ids().into_iter().collect()
    } else {
        report.provenance.audited_rule_ids.iter().cloned().collect()
    };
    // One recommendation per (rule, repo) when the scan kept them; the by-rule map holds only
    // the last repo's. A row the gate adds is attributed to the repo whose evidence it is, never
    // to the first repo of the project.
    let recs: Vec<crate::ai_audit::RuleRecommendation> =
        if report.recommendations_by_repo.is_empty() {
            let mut by_rule: Vec<_> = report.recommendations.values().cloned().collect();
            by_rule.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
            by_rule
        } else {
            report.recommendations_by_repo.clone()
        };
    let rule_ids_with_any_finding: std::collections::HashSet<String> = report
        .findings
        .iter()
        .flat_map(|f| std::iter::once(f.rule_id.clone()).chain(f.also_matches.iter().cloned()))
        .collect();
    let mut synthetic = Vec::new();
    for rec in &recs {
        // The id as the ledger spells it (the recommendation's is upper-cased).
        let Some(rid) = healthy_ids
            .iter()
            .find(|id| id.eq_ignore_ascii_case(&rec.rule_id))
        else {
            continue;
        };
        if rule_ids_with_any_finding.contains(rid.as_str()) {
            continue;
        }
        let Some(evidence) = rec.evidence.as_deref() else {
            continue;
        };
        if !evidence_negates_requirement(evidence) {
            continue;
        }
        let (path, line) = parse_evidence_citation(evidence);
        // W21: an unparseable citation yields an empty path, so this row is only `located`
        // when the model's evidence actually named a file. The evidence text itself is still
        // carried in `snippet` and quoted in `detail` below, so nothing is lost by refusing
        // to call a sentence a location.
        let located = !path.is_empty();
        synthetic.push(Finding {
            repo: if rec.repo.is_empty() {
                report.repos.first().cloned().unwrap_or_default()
            } else {
                rec.repo.clone()
            },
            path,
            line,
            rule_id: rid.clone(),
            severity: "medium".to_string(),
            snippet: evidence.chars().take(200).collect(),
            detail: format!(
                "This rule's own evaluation evidence reads \"{evidence}\", which appears to \
                 negate the rule's own requirement, even though the rule reported zero \
                 findings this run. Flagged here for manual confirmation rather than listed as \
                 healthy."
            ),
            needs_review: true,
            confidence: Some("needs-review".to_string()),
            located,
            // A5.6: the gate's claim is that the rule's own requirement is negated — an
            // objective-fault claim (held for review), never a consistency observation.
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            ..Finding::default()
        });
    }
    if synthetic.is_empty() {
        return;
    }
    let rows_in = report.findings.len();
    let added = synthetic.len();
    report.findings.extend(synthetic);
    let rows_out = report.findings.len();
    let mut acc = crate::scan_ledger::StageAccounting::default();
    acc.push_added("evidence_gate_synthesized", added);
    report
        .ledger
        .record_stage("evidence-gate-synthesis", rows_in, rows_out, acc);
}

/// For a rule listed under "What's healthy" in a project of several repos: the repos where it
/// was checked and is clean, and the repos it does not apply to. The list says where the rule
/// is healthy; a repo it never applied to is not one of them.
pub(crate) fn healthy_repos_for(
    report: &crate::onboard::ScanReport,
    rule_id: &str,
) -> (Vec<String>, Vec<String>) {
    use crate::scan_ledger::CellState;
    let mut clean = Vec::new();
    let mut not_applicable = Vec::new();
    for repo in &report.repos {
        match crate::onboard::patterns_matrix::cell_for(report, rule_id, repo) {
            CellState::Clean => clean.push(repo.clone()),
            CellState::NotApplicable { .. } => not_applicable.push(repo.clone()),
            CellState::Violated { .. } | CellState::NotReviewed { .. } => {}
        }
    }
    (clean, not_applicable)
}
