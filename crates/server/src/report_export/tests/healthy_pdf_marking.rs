//! What's healthy, dependency snapshots, coverage disclosures, executive summary,
//! PDF compilation, the marking and held-for-review buckets.

use super::*;

#[test]
fn whats_healthy_is_audited_rules_minus_rules_with_findings() {
    let f = finding("SEC-1", "a.rs", 1, "high");
    let report = report_with(vec![f], vec!["SEC-1", "SEC-2", "SEC-3"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let healthy_ids: Vec<&str> = json
        .whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert_eq!(healthy_ids, vec!["SEC-2", "SEC-3"]);
}

#[test]
fn whats_healthy_recovers_a_rule_whose_only_finding_was_a_false_positive() {
    // The tool flagged SEC-1 once, but the auditor proved it wrong (FalsePositive) —
    // the rule is honestly "clean" this run: excluded findings don't count against it.
    let f = finding("SEC-1", "a.rs", 1, "high");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&f),
        wire("FalsePositive", "not exploitable", ""),
    );
    let report = report_with(vec![f], vec!["SEC-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    let healthy_ids: Vec<&str> = json
        .whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert_eq!(healthy_ids, vec!["SEC-1"]);
}

// ── Dependency carve-out ───────────────────────────────────────────────────

#[test]
fn dep_audit_findings_are_carved_out_of_code_sections_into_dependency_snapshot() {
    let mut f = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
    f.snippet = "time@0.3.20".to_string();
    f.detail = "RUSTSEC-2023-0001: potential segfault (affects time@0.3.20)".to_string();
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert!(
        json.curated_findings.is_empty(),
        "dep findings must not appear in curated findings"
    );
    assert_eq!(
        json.matrix.do_now.len()
            + json.matrix.do_next.len()
            + json.matrix.plan.len()
            + json.matrix.accepted.len(),
        0
    );
    assert_eq!(json.dependency_snapshot.rows.len(), 1);
    assert_eq!(json.dependency_snapshot.rows[0].package, "time@0.3.20");
    assert!(!json.dependency_snapshot.clean);
}

#[test]
fn dependency_snapshot_clean_when_no_dep_findings() {
    let report = report_with(vec![], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(json.dependency_snapshot.clean);
    assert!(json.whats_healthy.dependency_clean);
}

#[test]
fn dependency_coverage_note_surfaces_osv_scanner_failures_only() {
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "osv-scanner".to_string(),
            message: "dependency audit did not run: network unavailable".to_string(),
        },
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "ruff".to_string(),
            message: "ruff not installed".to_string(),
        },
    ];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.dependency_snapshot.coverage_notes.len(), 1);
    assert!(json.dependency_snapshot.coverage_notes[0].contains("network unavailable"));
}

/// req_g1 (W7): a NON-dependency tool's coverage gap reaches the client artifacts.
/// Before W7, only `osv-scanner` notes were exported anywhere; a wholesale eslint/
/// semgrep/ruff/clippy failure reached the UI and the ledger but no PDF/xlsx/JSON — a
/// whole tool tier could fail and the client's artifacts said nothing (the ESLint
/// failure was invisible to a client even when disclosed internally).
#[test]
fn req_g1_non_osv_tool_coverage_gap_is_disclosed_in_summary_and_methodology() {
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "eslint".to_string(),
            message: "Could not preview 12 rule(s) with eslint: spawn ENOENT at \
                          /internal/path/node_modules/.bin/eslint. It enforces once wired \
                          into CI."
                .to_string(),
        },
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "eslint".to_string(),
            message: "Could not preview 3 rule(s) with eslint: exit code 2.".to_string(),
        },
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "osv-scanner".to_string(),
            message: "dependency audit did not run: network unavailable".to_string(),
        },
    ];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let eslint_lines = |notes: &[String]| {
        notes
            .iter()
            .filter(|n| n.contains("eslint"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let summary_lines = eslint_lines(&json.executive_summary.failed_passes);
    let methodology_lines = eslint_lines(&json.methodology.failed_passes);
    assert_eq!(
        summary_lines.len(),
        1,
        "exactly ONE concise line per affected tool (G3: concise and useful, never an \
             exhaustive dump): {:?}",
        json.executive_summary.failed_passes
    );
    assert_eq!(methodology_lines, summary_lines);
    // G3: client language, never the notes' internal diagnostics.
    assert!(
        !summary_lines[0].contains("ENOENT") && !summary_lines[0].contains("/internal/path"),
        "the disclosure must not dump internal diagnostics: {:?}",
        summary_lines[0]
    );
    // The osv note stays where it already lived (the dependency snapshot), not duplicated
    // into the tool lines.
    assert!(
        !json
            .executive_summary
            .failed_passes
            .iter()
            .any(|n| n.contains("osv-scanner")),
        "osv-scanner notes belong to the dependency snapshot, not the tool lines: {:?}",
        json.executive_summary.failed_passes
    );
}

/// req_w13 (G3): a rule whose own corpus file would not parse gets ONE concise,
/// client-facing line — with a count, because "how much of the rule library was missing"
/// is the only quantity a client can act on — and NEVER the file path or the `RulesError`
/// text, both of which name the reviewer's filesystem and TOML internals.
///
/// FAILS BEFORE THE FIX: `tool_coverage_disclosures` had no arm for
/// `RULE_CORPUS_COVERAGE_TOOL`, so two such notes rendered as "The rule-corpus
/// static-analysis checks did not fully run this scan" — a sentence that names a tool the
/// client does not have, says nothing about how many rules were missing, and reads as a
/// tool failure rather than a rule-library gap. (And upstream of that, nothing produced
/// the notes at all: the whole channel is W13's.)
#[test]
fn req_w13_the_client_facing_corpus_load_line_is_one_concise_line_with_a_count() {
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: crate::RULE_CORPUS_COVERAGE_TOOL.to_string(),
            message: "/Users/auditor/corpus/supabase/rls/a.toml did not parse, so rule \
                          SUPABASE-RLS-ENABLED-1 was not loaded and did not run: TOML parse \
                          error in a.toml: unknown variant `architectual`"
                .to_string(),
        },
        CoverageNote {
            tool_failed: false,
            repo: None,
            tool: crate::RULE_CORPUS_COVERAGE_TOOL.to_string(),
            message: "/Users/auditor/corpus/universal/b.toml did not parse, so rule \
                          SEC-NO-RAW-SQL-CONCAT-1 was not loaded and did not run: TOML parse \
                          error in b.toml: expected a value"
                .to_string(),
        },
    ];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let lines: Vec<&String> = json
        .executive_summary
        .failed_passes
        .iter()
        .filter(|n| n.contains("rule library"))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "G3: exactly ONE line for the whole scan, never one per broken file: {:?}",
        json.executive_summary.failed_passes
    );
    let line = lines[0];
    assert_eq!(
        line,
        "2 rules in the rule library could not be loaded this scan and did not run; \
             anything they would have reported is not reflected in this report.",
        "the exact client-facing wording is pinned: it ships in a client PDF"
    );
    // The same line, verbatim, in the methodology — a reader must hit the identical
    // statement wherever they look, which is the W7 shape this reuses.
    assert!(json.methodology.failed_passes.contains(line));

    // G3's prohibition: never the raw diagnostics.
    for leak in [
        "/Users/auditor",
        ".toml",
        "TOML parse error",
        "architectual",
        "unknown variant",
        "rule-corpus",
    ] {
        assert!(
            !line.contains(leak),
            "the client line must not carry {leak:?}: {line}"
        );
    }
    // No em/en dash (house style for this deliverable — see `AUDIT_REPORT_DISCLAIMER`).
    assert!(
        !line.contains('\u{2014}') && !line.contains('\u{2013}'),
        "{line}"
    );
}

/// Item 24: a Firebase repository's report states what Firebase coverage means, in the one
/// written sentence, once, in both the executive summary and the methodology, with no dash.
#[test]
fn the_firebase_scope_sentence_reaches_the_summary_and_methodology_verbatim() {
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![CoverageNote {
        tool_failed: false,
        repo: None,
        tool: crate::FIREBASE_SCOPE_COVERAGE_TOOL.to_string(),
        message: "operator note".to_string(),
    }];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let line = crate::FIREBASE_SCOPE_SENTENCE;
    assert_eq!(
        json.executive_summary
            .failed_passes
            .iter()
            .filter(|n| n.as_str() == line)
            .count(),
        1,
        "{:?}",
        json.executive_summary.failed_passes
    );
    assert!(json.methodology.failed_passes.iter().any(|n| n == line));
    assert!(
        !line.contains('\u{2014}') && !line.contains('\u{2013}'),
        "{line}"
    );
    assert!(
        !line.contains("static-analysis"),
        "not the generic tool wording"
    );
}

/// req_w13: the line pluralizes honestly. One broken rule file is "1 rule", not
/// "1 rule(s)" — the same real-pluralization bar the rest of this report holds (S8).
#[test]
fn req_w13_the_client_facing_corpus_load_line_pluralizes_correctly() {
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![CoverageNote {
        tool_failed: false,
        repo: None,
        tool: crate::RULE_CORPUS_COVERAGE_TOOL.to_string(),
        message: "x.toml did not parse".to_string(),
    }];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(
        json.executive_summary
            .failed_passes
            .iter()
            .any(|n| n.starts_with("1 rule in the rule library")),
        "expected singular '1 rule', got: {:?}",
        json.executive_summary.failed_passes
    );
}

// ── Executive summary override ─────────────────────────────────────────────

#[test]
fn executive_summary_override_is_used_verbatim() {
    let report = report_with(vec![], vec![]);
    let mut opts = empty_opts();
    opts.executive_summary_override = Some("Custom hand-written summary.".to_string());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &opts,
    );
    assert_eq!(
        json.executive_summary.narrative,
        "Custom hand-written summary."
    );
    assert!(json.executive_summary.is_override);
}

#[test]
fn executive_summary_default_narrative_when_no_override() {
    // S7: a zero-candidate scan gets its own honest sentence, not a mechanical
    // "0 candidate finding(s) were reviewed... 0 do now, 0 do next..." run-on.
    let report = report_with(vec![], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(!json.executive_summary.is_override);
    assert_eq!(
        json.executive_summary.narrative,
        "The scan surfaced no problems to review in this run."
    );
}

#[test]
fn executive_summary_default_narrative_pluralizes_correctly() {
    // S8: real pluralization ("1 candidate finding", not "1 candidate finding(s)").
    let f = finding("SEC-1", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(
        json.executive_summary.narrative.contains("1 problem."),
        "expected singular 'finding', got: {:?}",
        json.executive_summary.narrative
    );
    assert!(
        !json.executive_summary.narrative.contains("finding(s)"),
        "must not contain the CLI-ism '(s)': {:?}",
        json.executive_summary.narrative
    );
}

// ── C4-R3: four renderer defects, verified against the REAL compiled PDF's extracted
// text (not just "it compiled") — a smoke check that only asserts `%PDF` magic bytes is
// blind to exactly these bugs (a doubled banner, a stray "None", an embedded U+200B, an
// unfilled placeholder are all still a valid, compiling PDF). `pdf_extract` gives these
// tests the same read on the artifact a human (or a grep/copy-paste workflow) would get.

/// The long path shared by every C4-R3 test below: long enough (well past `breakable()`'s
/// default 40-char chunk size) to force a wrap in both the curated-finding site line and
/// the narrower severity×effort grid cell.
const C4_R3_LONG_PATH: &str = "apps/web/src/components/very/deeply/nested/directory/\
        structure/that/goes/on/ForeverLongComponentFileName.tsx";

/// Builds a two-finding, multi-page RAW report that exercises all four C4-R3 defects at
/// once: a long path (zero-width-space wrap bug), an empty severity×effort cell (the
/// "None" bug — this report deliberately has findings in only ONE of the two
/// severity/effort combinations it surfaces, so the other cell in the grid is empty), the
/// real corpus's `SEC-NO-UNSAFE-HTML-SINK-1` remediation (the unfilled-placeholder bug,
/// whose authored text contains a literal `<img ...>` HTML example), and a RAW
/// (unreviewed) review state spanning several pages (the draft-banner-doubling bug).
async fn c4_r3_report_and_pdf() -> (AuditReportJson, Vec<u8>) {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut long_path_finding =
        finding("SEC-NO-UNSAFE-HTML-SINK-1", C4_R3_LONG_PATH, 42, "critical");
    long_path_finding.effort = Some("low".to_string());
    long_path_finding.confidence = Some("high".to_string());

    let mut other_finding = finding("SEC-NO-HARDCODED-SECRETS-1", "b.rs", 1, "low");
    other_finding.effort = Some("high".to_string());

    let report = report_with(
        vec![long_path_finding, other_finding],
        vec!["SEC-NO-UNSAFE-HTML-SINK-1", "SEC-NO-HARDCODED-SECRETS-1"],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(
        json.review_state,
        ReviewState::Raw,
        "report must be RAW to exercise the draft banner"
    );
    assert!(
        json.priority_grid
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.findings.is_empty())),
        "the grid must contain at least one empty cell to exercise the 'None' bug"
    );

    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed for the C4-R3 regression report");
    (json, pdf)
}

/// Defect 1: the per-page draft banner must render EXACTLY ONCE per page — never doubled
/// into "DRAFTDRAFT: ..." by a redundant short echo elsewhere on the same or an adjacent
/// page. Every occurrence of the bare word "DRAFT" anywhere in the document must be part
/// of the one full banner sentence; a standalone "DRAFT" (the old footer echo) is exactly
/// what let it collide with the next page's banner.
#[tokio::test]
async fn compile_pdf_renders_the_draft_banner_exactly_once_never_doubled() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_renders_the_draft_banner_exactly_once_never_doubled: \
                 typst not on PATH"
        );
        return;
    }
    let (_, pdf) = c4_r3_report_and_pdf().await;
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    assert!(
        !text.contains("DRAFTDRAFT"),
        "the draft banner must never render doubled: {text:?}"
    );
    let bare_draft_count = text.matches("DRAFT").count();
    let full_banner_count = text
        .matches("DRAFT: not yet reviewed by a human reviewer")
        .count();
    assert!(
        full_banner_count >= 1,
        "the draft banner must render at least once: {text:?}"
    );
    assert_eq!(
        bare_draft_count, full_banner_count,
        "every occurrence of the word DRAFT must be part of the one full banner sentence, \
             never a standalone echo elsewhere on the page: {text:?}"
    );
}

/// Defect 2: an empty severity×effort matrix cell must render as blank — never the
/// literal string "None" (a Rust `Option`/Typst `none` stringified instead of omitted) or
/// "null". The whole document is checked, not just the matrix section, since neither
/// string is legitimate ANYWHERE in this report's authored prose.
#[tokio::test]
async fn compile_pdf_never_renders_the_literal_none_for_an_empty_matrix_cell() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_never_renders_the_literal_none_for_an_empty_matrix_cell: \
                 typst not on PATH"
        );
        return;
    }
    let (_, pdf) = c4_r3_report_and_pdf().await;
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    assert!(
        !text.contains("None"),
        "no cell may render the literal word None: {text:?}"
    );
    assert!(
        !text.contains("null"),
        "no cell may render the literal word null: {text:?}"
    );
}

/// Defect 3: wrapping a long path/snippet/package must never splice a zero-width space (or
/// any other zero-width/BOM code point) into the rendered text — that invisible character
/// used to survive into the PDF's copy/search/grep layer and corrupt an otherwise-exact
/// path. The long path in `c4_r3_report_and_pdf` is long enough to force a wrap.
#[tokio::test]
async fn compile_pdf_never_embeds_a_zero_width_character_when_wrapping_a_long_path() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_never_embeds_a_zero_width_character_when_wrapping_a_long_path: \
                 typst not on PATH"
        );
        return;
    }
    let (_, pdf) = c4_r3_report_and_pdf().await;
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    for c in ZERO_WIDTH_CODEPOINTS {
        assert!(
            !text.contains(c),
            "rendered text must never contain U+{:04X}: {text:?}",
            c as u32
        );
    }
    // A genuine visual line wrap is expected to introduce ORDINARY whitespace at the break
    // point (a real PDF reader/extractor represents any two-line text that way — that's not
    // the bug) — collapsing all whitespace must still reconstruct the exact original path
    // with NOTHING else spliced in. This is the real discriminator from the old bug: the
    // zero-width space survived even collapsing whitespace, because it is not whitespace.
    let collapsed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        collapsed.contains(C4_R3_LONG_PATH),
        "wrapping the long path must never introduce anything other than incidental \
             whitespace at the break point: reconstructed={collapsed:?}"
    );
}

/// Defect 4: an authored remediation's literal angle-bracketed EXAMPLE (the real corpus's
/// `SEC-NO-UNSAFE-HTML-SINK-1` remediation shows `<img src=x onerror=alert(1)>` as prose,
/// not a placeholder) must render verbatim — never swallowed by the generic-placeholder
/// filler into a nonsense sentence like "a payload like the affected resource renders as
/// inert text".
#[tokio::test]
async fn the_fix_text_never_swallows_a_literal_html_example_into_a_generic_placeholder() {
    if which_typst().is_none() {
        eprintln!("skipping the_fix_text_never_swallows_a_literal_html_example: typst not on PATH");
        return;
    }
    let (json, _pdf) = c4_r3_report_and_pdf().await;
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "SEC-NO-UNSAFE-HTML-SINK-1")
        .expect("the HTML-sink finding must survive triage into curated findings");
    // The PDF prints no per-place fix text; the text is carried by the dataset the workbook and
    // the CSV files are built from, and that is where the example must survive.
    let site = &group.sites[0];
    let fix = format!(
        "{} {}",
        site.fix.clone().unwrap_or_default(),
        site.fix_for_this_finding.clone().unwrap_or_default()
    );
    assert!(
        !fix.contains("a payload like the affected resource"),
        "the literal <img ...> example must not be replaced by the generic filler: {fix:?}"
    );
    assert!(
        fix.contains("payload like") && fix.contains("renders as inert text"),
        "the authored remediation sentence must still be in the fix text: {fix:?}"
    );
}

/// UTF8-BREAK-1: `breakable()` used to chunk a path/snippet by raw BYTE offset
/// (`s.slice(i, e)`), and Typst hard-errors the WHOLE compile when a slice index lands
/// mid-character — so any finding whose path, snippet, or package name contained a
/// multi-byte character (an accented Latin letter, a CJK character, an emoji) crashed the
/// entire PDF export with NO deliverable produced at all. `UNIT` mixes all three; the path
/// repeats it 5x and the snippet 8x specifically because that lands a UTF-8 continuation
/// byte exactly at byte offset 40 (the path's default chunk size) and 70 (the snippet's
/// explicit `n: 70`) — i.e. this is a literal repro of the byte-boundary crash, not just
/// "contains some multi-byte text somewhere".
#[tokio::test]
async fn compile_pdf_survives_multi_byte_characters_in_path_and_snippet() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_survives_multi_byte_characters_in_path_and_snippet: \
                 typst not on PATH"
        );
        return;
    }
    const UNIT: &str = "café日本語😀";
    let multibyte_path = format!("apps/{}/file.tsx", UNIT.repeat(5));
    let multibyte_snippet = format!(
        "const secretToken = '{}'; // trailing comment to extend the line further for padding",
        UNIT.repeat(8)
    );

    let mut f = finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        &multibyte_path,
        10,
        "critical",
    );
    f.snippet = multibyte_snippet.clone();
    let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings.len(),
        1,
        "the multi-byte finding must survive triage into curated findings"
    );
    assert_eq!(json.curated_findings[0].sites[0].path, multibyte_path);
    assert_eq!(json.curated_findings[0].sites[0].snippet, multibyte_snippet);

    let pdf = compile_pdf(&json).await.expect(
        "compile_pdf must not crash on a multi-byte path/snippet — breakable() must chunk \
             by character cluster, never by raw byte offset",
    );
    assert!(
        pdf.starts_with(b"%PDF"),
        "output must start with the PDF magic bytes"
    );

    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    assert!(
        !text.contains('\u{FFFD}'),
        "no UTF-8 replacement character may appear in the extracted text: {text:?}"
    );
    // Same discriminator the C4-R3 zero-width-space test uses: a real visual line wrap
    // introduces only ORDINARY whitespace at the break point, so collapsing all whitespace
    // must still reconstruct every multi-byte run byte-for-byte, with nothing dropped,
    // reordered, or replaced.
    let collapsed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        collapsed.contains(&UNIT.repeat(5)),
        "the path's accented/CJK/emoji characters must survive intact: \
             reconstructed={collapsed:?}"
    );
    // The snippet is not printed (the PDF holds no code), so it must not be in the text.
    assert!(
        !collapsed.contains(&UNIT.repeat(8)),
        "the PDF printed a code snippet: reconstructed={collapsed:?}"
    );
}

// ── End-to-end PDF compile (typst-present-only) ────────────────────────────

/// Compiles a small report to a real PDF via the bundled template. Skips gracefully
/// (prints a note, does not fail) when `typst` is not on PATH — CI environments
/// without Typst installed must never hard-fail on this test.
#[tokio::test]
async fn compile_pdf_produces_a_real_pdf_when_typst_is_present() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_produces_a_real_pdf_when_typst_is_present: typst not on PATH"
        );
        return;
    }
    let report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "critical",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let mut opts = empty_opts();
    opts.client_name = "Acme Corp".to_string();
    opts.project_title = "Acme Backend Audit".to_string();
    opts.prepared_by = "Camerata".to_string();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &opts,
    );

    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed when typst is present");
    assert!(!pdf.is_empty(), "PDF bytes must be non-empty");
    assert!(
        pdf.starts_with(b"%PDF"),
        "output must start with the PDF magic bytes"
    );
}

/// W27: the bundled fonts alone render the report. Compiled with system fonts IGNORED, so a
/// machine that happens to have Inter or JetBrains Mono installed cannot mask a missing or
/// misnamed bundle file: Typst warns "unknown font family" for a family it cannot find, and
/// the PDF's font dictionaries name every embedded face.
#[tokio::test]
async fn the_bundled_fonts_alone_render_the_report_in_inter_and_jetbrains_mono() {
    if which_typst().is_none() {
        eprintln!("skipping the_bundled_fonts_alone_render_the_report: typst not on PATH");
        return;
    }
    let report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "critical",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let mut opts = empty_opts();
    opts.client_name = "Acme Corp".to_string();
    opts.brand = BRAND_KIT_NAME.to_string();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &opts,
    );
    assert!(json.brand_mark, "the kit's own brand renders the mark");

    let (pdf, warnings) = run_typst(
        &render_data_json(&json).expect("serialize"),
        FontSources::BundledOnly,
    )
    .await
    .expect("the report compiles from the bundled fonts alone");
    assert!(
        !warnings.contains("unknown font family"),
        "a bundled family is missing: {warnings}"
    );
    let text = String::from_utf8_lossy(&pdf);
    for face in ["Inter-Regular", "Inter-SemiBold", "JetBrainsMono-Regular"] {
        assert!(
            text.contains(&format!("+{face}")),
            "the PDF does not embed {face}"
        );
    }
}

/// Confidentiality (2026-10-24 decision): the marking names the client and the SCAN date
/// (not the wall clock), consent belongs to the brand when there is one and else to the
/// preparer, and a blank client never prints "prepared for .".
#[test]
fn the_confidentiality_marking_names_the_client_the_scan_date_and_who_consents() {
    let mut provenance = ScanProvenance {
        finished_at: "2026-10-21T09:30:00Z".to_string(),
        ..ScanProvenance::default()
    };
    let mut opts = empty_opts();
    opts.client_name = "  Example Client Ltd ".to_string();
    opts.brand = "Cantus Works".to_string();
    let m = confidentiality_marking(&provenance, &opts);
    assert_eq!(
        m.line,
        "Confidential, prepared for Example Client Ltd on 21 October 2026"
    );
    assert_eq!(
        m.handling_note,
        "This report is confidential and was prepared for Example Client Ltd only. Please \
             do not forward it outside Example Client Ltd without the written consent of \
             Cantus Works."
    );

    opts.brand = String::new();
    opts.prepared_by = "A. Reviewer".to_string();
    assert!(confidentiality_marking(&provenance, &opts)
        .handling_note
        .ends_with("written consent of A. Reviewer."));

    opts.client_name = "   ".to_string();
    provenance.finished_at = "2026-03-02T00:00:00Z".to_string();
    let blank = confidentiality_marking(&provenance, &opts);
    assert_eq!(blank.line, "Confidential, prepared on 2 March 2026");
    assert!(!blank.line.contains("prepared for"));
    for text in [&blank.line, &blank.handling_note, &m.line, &m.handling_note] {
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{text}"
        );
    }
}

#[test]
fn a_client_name_must_be_one_short_line_and_present_to_export() {
    assert_eq!(
        validate_client_name("  Example Client Ltd  ").as_deref(),
        Ok("Example Client Ltd")
    );
    assert_eq!(validate_client_name("").as_deref(), Ok(""));
    assert!(validate_client_name("Example\nClient").is_err());
    assert!(validate_client_name(&"x".repeat(CLIENT_NAME_MAX_CHARS + 1)).is_err());
    assert!(validate_client_name(&"x".repeat(CLIENT_NAME_MAX_CHARS)).is_ok());

    let mut opts = empty_opts();
    opts.client_name = " ".to_string();
    let refused = require_client_name(&opts).expect_err("blank blocks the export");
    assert!(
        refused.to_string().contains("inspection setup"),
        "{refused}"
    );
    opts.client_name = "Example Client Ltd".to_string();
    assert_eq!(
        require_client_name(&opts).as_deref(),
        Ok("Example Client Ltd")
    );
}

/// Invisible characters are not part of a client's name: they are stripped where the name is
/// saved, so the manifest, the workbook and the PDF can never print different strings, and a
/// name made only of them is blank (and refused with the message that says so).
#[test]
fn zero_width_characters_are_stripped_from_the_client_name() {
    assert_eq!(
        validate_client_name("Ex\u{200B}ample\u{FEFF} Client\u{2060} Ltd\u{200D}").as_deref(),
        Ok("Example Client Ltd")
    );
    assert_eq!(
        validate_client_name("\u{200B}  \u{200C}\u{2060}").as_deref(),
        Ok("")
    );
    let mut opts = empty_opts();
    opts.client_name = "\u{200B}\u{200D}".to_string();
    let refused = require_client_name(&opts).expect_err("an invisible name is a blank name");
    assert!(
        refused.to_string().contains("Enter the client name"),
        "the refusal says the name is missing: {refused}"
    );
    // The marking prints the same string the setup saved.
    opts.client_name = "Acme\u{200B} Ltd".to_string();
    assert_eq!(
        confidentiality_marking(&ScanProvenance::default(), &opts)
            .line
            .split(" on ")
            .next(),
        Some("Confidential, prepared for Acme Ltd")
    );
}

/// The workbook footer holds 255 characters, with each `&` written twice, so a name made of
/// ampersands has the least room. The bound is measured on the real footer, with the longest
/// date the marking can print, and refused with a message that says what to shorten.
#[test]
fn a_client_name_that_would_overflow_the_workbook_footer_is_refused() {
    assert!(validate_client_name(&"x".repeat(CLIENT_NAME_MAX_CHARS)).is_ok());
    // 80 characters with several ampersands is a realistic long name.
    let long = format!("{} & Sons & Daughters & Partners", "A".repeat(49));
    assert!(long.chars().count() <= 80);
    assert!(validate_client_name(&long).is_ok());

    let footer_len = |name: &str| {
        let line = marking_line(name, WORST_MARKING_DATE);
        crate::xlsx_export::sheet_footer(&line)
            .encode_utf16()
            .count()
    };
    let widest_accepted = (1..=CLIENT_NAME_MAX_CHARS)
        .take_while(|n| validate_client_name(&"&".repeat(*n)).is_ok())
        .last()
        .expect("a short name of ampersands is accepted");
    assert!(widest_accepted < CLIENT_NAME_MAX_CHARS, "the bound bites");
    assert!(footer_len(&"&".repeat(widest_accepted)) <= EXCEL_FOOTER_MAX_CHARS);
    assert!(
        footer_len(&"&".repeat(widest_accepted + 1)) > EXCEL_FOOTER_MAX_CHARS,
        "the bound is exact, not merely safe"
    );
    let refused = validate_client_name(&"&".repeat(CLIENT_NAME_MAX_CHARS))
        .expect_err("a hundred ampersands cannot fit a footer");
    assert!(refused.to_string().contains("footer"), "{refused}");
    assert!(refused.to_string().contains("Shorten"), "{refused}");
}

/// The longest date the marking can print is the one the bound is measured with.
#[test]
fn the_worst_marking_date_is_the_longest_the_marking_can_print() {
    use chrono::TimeZone;
    let longest = (1..=12u32)
        .flat_map(|month| {
            (1..=28u32).map(move |day| {
                chrono::Utc
                    .with_ymd_and_hms(2026, month, day, 0, 0, 0)
                    .unwrap()
                    .format("%-d %B %Y")
                    .to_string()
            })
        })
        .map(|date| date.chars().count())
        .max()
        .unwrap();
    // Days 29 to 31 add no width over 28 (both are two digits).
    assert_eq!(WORST_MARKING_DATE.chars().count(), longest);
}

/// W27: the logo mark belongs to the bundled brand kit, so it renders for that brand only.
/// Another agency's brand, or no brand, never gets it.
#[test]
fn the_brand_mark_renders_only_for_the_brand_kit_brand() {
    assert!(brand_kit_matches(Some("Cantus Works")));
    assert!(brand_kit_matches(Some("  cantus works ")));
    assert!(!brand_kit_matches(Some("Another Agency")));
    assert!(!brand_kit_matches(None));
}

// ── req_det: two exports of ONE report are byte-identical ─────────────────

/// req_det: compiling the SAME report twice, across a second boundary, must produce
/// byte-identical PDFs.
///
/// This is the property the product needs in order to claim two deliverables ARE the same
/// report. Without it the claim is unverifiable: you cannot diff two PDFs and say anything.
///
/// FAILS BEFORE THE CHANGE: `compile_pdf` set neither `SOURCE_DATE_EPOCH` nor
/// `--creation-timestamp`, so typst stamped the wall clock into five places — the Info
/// dict's `/CreationDate` and `/ModDate`, the XMP packet's `xmp:CreateDate`,
/// `xmp:ModifyDate`, `xmpMM:InstanceID` and `xmpMM:DocumentID`, and the trailer's `/ID`.
/// Red output was `the two PDFs must be byte-identical … first difference at byte 10029`
/// inside the XMP packet, with the two windows showing `08:49:30` and `08:49:32`.
///
/// The sleep is deliberate and is why this test is trustworthy: without it the two
/// compiles usually land in the same second and the OLD code passes, which is precisely
/// how this defect survived — a test that is usually green teaches you to rerun, not to
/// look.
#[tokio::test]
async fn req_det_two_compiles_of_one_report_are_byte_identical() {
    if which_typst().is_none() {
        eprintln!(
            "skipping req_det_two_compiles_of_one_report_are_byte_identical: typst not on PATH"
        );
        return;
    }
    let report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "critical",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let first = compile_pdf(&json)
        .await
        .expect("first compile must succeed");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let second = compile_pdf(&json)
        .await
        .expect("second compile must succeed");

    assert!(first.starts_with(b"%PDF") && !first.is_empty());
    let first_diff = first
        .iter()
        .zip(&second)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| first.len().min(second.len()));
    assert_eq!(
        first,
        second,
        "two compiles of ONE report must be byte-identical; lengths {} vs {}, first \
             difference at byte {first_diff}",
        first.len(),
        second.len()
    );
}

/// req_det: the PDF's embedded creation timestamp is the SCAN time — not a constant, and
/// not the moment the bytes were serialized. A client reading the file's metadata must be
/// told when the audit ran.
///
/// FAILS BEFORE THE CHANGE: the Info dict carried the wall clock at compile time, which
/// for this fixture (a scan stamped `2026-07-23T00:05:00Z`) is years off.
#[tokio::test]
async fn req_det_pdf_creation_timestamp_is_the_scan_time() {
    if which_typst().is_none() {
        eprintln!("skipping req_det_pdf_creation_timestamp_is_the_scan_time: typst not on PATH");
        return;
    }
    let report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "critical",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    // `report_with` stamps finished_at = 2026-07-23T00:05:00Z; the cover string is
    // minute-precision, so the pinned instant is 00:05:00 of that day.
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.cover.generated_at, "2026-07-23 00:05 UTC");

    let pdf = compile_pdf(&json).await.expect("compile must succeed");
    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("D:20260723000500"),
        "the PDF must carry the SCAN instant as its creation date, not the wall clock"
    );
}

/// W6: the real Typst template must compile cleanly with a non-empty `failed_passes` list
/// — exercising the new disclosure block in BOTH the summary and the methodology section,
/// not just the gated-off "empty list" path every other `compile_pdf_*` test above covers.
#[tokio::test]
async fn compile_pdf_renders_the_failed_pass_disclosure_block() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_renders_the_failed_pass_disclosure_block: typst not on PATH"
        );
        return;
    }
    let mut report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "critical",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.failed_passes = vec![crate::ai_audit::FailedPass {
        repo: "acme/widgets".to_string(),
        pass: "rule-alternative recommendations".to_string(),
        reason: "Claude CLI timed out after 300s (no output).".to_string(),
        files_not_reviewed: 0,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    }];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.executive_summary.failed_passes.len(), 1);
    assert_eq!(json.methodology.failed_passes.len(), 1);

    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed with a failed-pass disclosure present");
    assert!(pdf.starts_with(b"%PDF"));
}

/// P2: the real Typst template must compile cleanly with `fix_for_this_finding` SET
/// (the new primary-fix branch), alongside a generic `fix` from the corpus — exercising
/// the "both present" path this pass added, not just the pre-existing "neither present"
/// path the test above already covers.
#[tokio::test]
async fn compile_pdf_renders_fix_specific_and_generic_fix_together() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_renders_fix_specific_and_generic_fix_together: typst not \
                 on PATH"
        );
        return;
    }
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    f.fix_specific = Some(
        "Enable RLS on `profiles` directly (see supabase/migrations/2.sql for the pattern \
             this repo already uses on `orders`)."
            .to_string(),
    );
    let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert!(json.curated_findings[0].sites[0]
        .fix_for_this_finding
        .is_some());
    assert!(json.curated_findings[0].sites[0].fix.is_some());
    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed with both fix_for_this_finding and fix set");
    assert!(pdf.starts_with(b"%PDF"));
}

// ── C4-P4 residual defect 5: curated ordering + held-for-review separation ─────────

/// Data test: the curated groups of the report data must be ordered by
/// severity desc (never alphabetically by rule id — the fixture deliberately gives the LOW
/// finding a rule id that sorts alphabetically FIRST, so a passing test can only mean the
/// severity sort actually fired), and an entirely-informational finding must render under
/// their own list, never among the curated groups. (The PDF prints neither list: it is a summary.)
#[tokio::test]
async fn curated_findings_are_severity_ordered_and_informational_rows_are_held_for_review_separately(
) {
    if which_typst().is_none() {
        eprintln!("skipping curated_findings_are_severity_ordered_...: typst not on PATH");
        return;
    }
    let mut low = finding("AAA-LOW-COSMETIC-1", "a.rs", 1, "low");
    low.detail = "A low severity cosmetic logging format issue was observed here. This is \
                      not urgent."
        .to_string();
    let mut critical = finding("ZZZ-CRITICAL-AUTH-1", "z.rs", 2, "critical");
    critical.detail = "A critical authentication bypass allows any unauthenticated user \
                            full admin access. Immediate action is required."
        .to_string();
    let mut info = finding("MMM-STYLE-NOTE-1", "m.rs", 3, "info");
    info.detail = "A minor style preference about variable naming conventions was observed \
                       in this file."
        .to_string();

    let report = report_with(vec![low, critical, info], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    // JSON-level contract: 2 genuinely curated groups (ordered critical-first despite the
    // alphabetical trap), 1 fully-informational group moved out to its own list.
    assert_eq!(
        json.curated_findings.len(),
        2,
        "{:?}",
        json.curated_findings
    );
    assert_eq!(json.held_for_review_findings.len(), 1);
    assert_eq!(
        json.curated_findings[0].sites[0].severity, "critical",
        "the first curated finding must be the max severity present, not the \
             alphabetically-first rule id: {:?}",
        json.curated_findings
    );
    assert_eq!(json.curated_findings[1].sites[0].severity, "low");
    assert_eq!(
        json.held_for_review_findings[0].sites[0].bucket,
        "informational"
    );
    assert!(json
        .curated_findings
        .iter()
        .all(|g| g.sites.iter().all(|s| s.bucket != "informational")));

    // The PDF lists no group, so there is no section order to check there: the order of the
    // data is what the workbook and the CSV files are built from.
}

// ── C6-B1: a held-bucket row counted but never rendered (fixed above in the
// `by_rule`-population loop) ──────────────────────────────────────────────────────────

/// Reproduces the exact client-facing shape of the bug: six criticals, one of them a
/// hedged (`needs_review: true`), uncited AI-tier finding that — before this fix — was
/// tallied into `matrix.held`/the cover strip/the category scorecard via the independent
/// `code_findings` pass but never got a `CuratedSiteJson` at all, because the `by_rule`
/// loop `continue`d past any `is_uncited_ai_finding` row before building one (stale
/// reasoning left over from before C5-7 — see that loop's updated doc comment). The
/// client counts six criticals on the cover and finds five in the PDF; the missing one is
/// a critical. This test pins both halves: the row is counted, AND it has a real,
/// independently discoverable site block, AND that block actually renders in the "Held
/// for review" section of the compiled PDF.
#[tokio::test]
async fn hedged_uncited_critical_in_held_bucket_is_counted_and_renders_in_held_for_review() {
    if which_typst().is_none() {
        eprintln!("skipping hedged_uncited_critical_in_held_bucket_...: typst not on PATH");
        return;
    }
    let mut hedge = finding(
        "AI-CUSTOM-NOVEL-DEFECT-1",
        "apps/api/src/risky.ts",
        7,
        "critical",
    );
    hedge.detail = "zzqy-hedged-uncited-critical: a genuinely novel defect class the model \
                         flagged with low confidence, naming none of the known grounded \
                         vocabulary."
        .to_string();
    hedge.needs_review = true;

    // Five ordinary criticals that render normally, so the fixture matches the client's
    // own report exactly: six criticals counted, one of six missing from the PDF body.
    let mut findings: Vec<Finding> = (1..=5)
        .map(|n| {
            let mut f = finding(
                &format!("SEC-TEST-ORDINARY-CRIT-{n}"),
                &format!("apps/api/src/ordinary_{n}.ts"),
                n,
                "critical",
            );
            f.detail = format!("zzqy-ordinary-critical-{n}: an uncontested critical finding.");
            f
        })
        .collect();
    findings.push(hedge);

    let report = report_with(findings, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    // ── Counted ──
    assert_eq!(
        json.cover.stats.critical, 6,
        "the cover must count all six criticals, hedged row included: {:?}",
        json.cover.stats
    );
    assert_eq!(
        json.matrix.held.len(),
        1,
        "the hedged, uncited row must sit in the severity-unbounded held bucket: {:?}",
        json.matrix.held
    );
    assert_eq!(json.matrix.held[0].severity, "critical");
    assert_eq!(json.matrix.held[0].rule_id, "AI-CUSTOM-NOVEL-DEFECT-1");

    // ── Has a rendered-eligible site ──
    let held_group = json
        .held_for_review_findings
        .iter()
        .find(|g| g.rule_id == "AI-CUSTOM-NOVEL-DEFECT-1")
        .expect(
            "the hedged, uncited critical finding must have its own CuratedGroupJson in \
                 held_for_review_findings, not merely a matrix.held tally",
        );
    assert_eq!(held_group.sites.len(), 1);
    assert_eq!(held_group.sites[0].severity, "critical");
    assert_eq!(held_group.sites[0].bucket, "held");
    assert!(
        json.curated_findings
            .iter()
            .all(|g| g.rule_id != "AI-CUSTOM-NOVEL-DEFECT-1"),
        "the uncited gate must still keep this row OUT of curated_findings: {:?}",
        json.curated_findings
    );

    // ── The PDF still counts it ──
    // The PDF prints no row, so the held critical is counted on the cover and nowhere else in the
    // PDF; its row is in the workbook and the release gate holds the groups to the matrix.
    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed for the hedged-uncited-critical fixture");
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    assert!(
        text.contains("6 critical"),
        "the cover must count all six criticals: {text:?}"
    );
}

/// The cover's critical tally must equal the number of critical-severity site BLOCKS
/// actually built across `curated_findings` + `held_for_review_findings` combined — never
/// a count computed independently of what gets a rendered block. Six criticals, split
/// across every destination a critical row can reach (two ordinary curated do_now rows,
/// two calibration-hedged held rows, two uncited-AI held rows), all six render.
#[tokio::test]
async fn cover_critical_count_equals_the_number_of_critical_blocks_actually_rendered() {
    let mut findings = Vec::new();
    for n in 1..=2 {
        let mut f = finding(
            &format!("SEC-CURATED-CRIT-{n}"),
            &format!("a{n}.rs"),
            n,
            "critical",
        );
        f.detail = format!("zzqy-curated-critical-{n}");
        findings.push(f);
    }
    for n in 1..=2 {
        let mut f = finding(
            &format!("ARCH-HEDGE-CRIT-{n}"),
            &format!("b{n}.rs"),
            n,
            "critical",
        );
        f.detail = format!("zzqy-hedged-critical-{n}");
        f.needs_review = true;
        findings.push(f);
    }
    for n in 1..=2 {
        let mut f = finding(
            &format!("AI-UNCITED-CRIT-{n}"),
            &format!("c{n}.rs"),
            n,
            "critical",
        );
        f.detail = format!(
            "zzqy-uncited-critical-{n}: no grounded citation vocabulary anywhere in this \
                 sentence."
        );
        findings.push(f);
    }

    let report = report_with(findings, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let rendered_critical_blocks = json
        .curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| &g.sites)
        .filter(|s| s.severity == "critical")
        .count();

    assert_eq!(json.cover.stats.critical, 6, "{:?}", json.cover.stats);
    assert_eq!(
        rendered_critical_blocks, 6,
        "every one of the six critical rows must have an actual rendered site block"
    );
    assert_eq!(
        json.cover.stats.critical, rendered_critical_blocks,
        "the cover's critical tally must equal the number of critical blocks actually \
             rendered, never a count computed independently of rendering"
    );

    if which_typst().is_none() {
        eprintln!(
            "skipping PDF leg of cover_critical_count_equals_the_number_of_critical_blocks_actually_rendered: \
                 typst not on PATH"
        );
        return;
    }
    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed for the six-critical fixture");
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    assert!(text.contains("6 critical"), "{text:?}");
}

/// Every (bucket x severity) combination the report pipeline can actually produce must
/// emit a real, independently findable site block — curated at critical/high/medium/low
/// (never info, which is always informational by `is_informational`'s hard rule), held at
/// EVERY severity including critical/high (C5-4: severity-unbounded by design), and
/// informational only at medium/low/info (never critical/high: `is_informational`'s other
/// hard invariant). `informational x critical` and `informational x high` are not
/// included below because they are IMPOSSIBLE combinations by design, not untested ones —
/// seprately pinned by `critical_and_high_are_never_informational`. One shared report, one
/// shared `build_report_json` call, one shared PDF compile — twelve independent
/// assertions, so a renderer change that silently drops any ONE combination fails exactly
/// that assertion rather than a vague aggregate count.
#[tokio::test]
async fn every_bucket_severity_combination_emits_a_rendered_block() {
    let severities = ["critical", "high", "medium", "low"];
    let mut findings = Vec::new();
    let mut expectations: Vec<(&'static str, &'static str, String)> = Vec::new();

    // curated x {critical, high, medium, low}
    for sev in severities {
        let tag = format!("zzqy-curated-{sev}");
        let mut f = finding(
            &format!("SEC-COMBO-CURATED-{}", sev.to_ascii_uppercase()),
            &format!("curated-{sev}.rs"),
            1,
            sev,
        );
        f.detail = tag.clone();
        findings.push(f);
        expectations.push(("curated", sev, tag));
    }

    // held x {critical, high, medium, low, info} — a calibration hedge, severity-unbounded.
    for sev in ["critical", "high", "medium", "low", "info"] {
        let tag = format!("zzqy-held-{sev}");
        let mut f = finding(
            &format!("ARCH-COMBO-HELD-{}", sev.to_ascii_uppercase()),
            &format!("held-{sev}.rs"),
            1,
            sev,
        );
        f.detail = tag.clone();
        f.needs_review = true;
        findings.push(f);
        expectations.push(("held", sev, tag));
    }

    // informational x {medium, low} — the §2d testing-style gate (this fixture's
    // `test_file_count` is 0, below `MIN_STYLE_CORPUS_FILES`).
    for sev in ["medium", "low"] {
        let tag = format!("zzqy-informational-{sev}");
        let mut f = finding(
            &format!("ARCH-COMBO-INFO-{}", sev.to_ascii_uppercase()),
            &format!("info-{sev}.rs"),
            1,
            sev,
        );
        f.detail = tag.clone();
        f.category = Some("testing-style".to_string());
        findings.push(f);
        expectations.push(("informational", sev, tag));
    }

    // informational x info — the plain `info`-severity rule, no hedge needed.
    {
        let tag = "zzqy-informational-info".to_string();
        let mut f = finding("ARCH-COMBO-INFO-INFO", "info-info.rs", 1, "info");
        f.detail = tag.clone();
        findings.push(f);
        expectations.push(("informational", "info", tag));
    }

    let report = report_with(findings, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    for (bucket, sev, tag) in &expectations {
        let (group_list, group_list_name) = match *bucket {
            "curated" => (&json.curated_findings, "curated_findings"),
            _ => (&json.held_for_review_findings, "held_for_review_findings"),
        };
        let site = group_list
            .iter()
            .flat_map(|g| &g.sites)
            .find(|s| s.detail == *tag)
            .unwrap_or_else(|| {
                panic!(
                    "no rendered site found for ({bucket}, {sev}) in {group_list_name}: \
                         tag {tag:?} — curated_findings={:?}, held_for_review_findings={:?}",
                    json.curated_findings, json.held_for_review_findings
                )
            });
        assert_eq!(
            site.severity, *sev,
            "the site found for tag {tag:?} must keep its real severity"
        );
        let expected_site_bucket = if *bucket == "curated" {
            // Any action tier counts — do_now/do_next/plan all mean "genuinely curated".
            assert!(
                !matches!(site.bucket.as_str(), "informational" | "held"),
                "tag {tag:?} expected to be curated (an action tier), got bucket {:?}",
                site.bucket
            );
            continue;
        } else {
            *bucket
        };
        assert_eq!(
            site.bucket, expected_site_bucket,
            "tag {tag:?} expected site bucket {expected_site_bucket:?}, got {:?}",
            site.bucket
        );
    }
}

// ── Regression guard: the backtick-hash interpolation bug class ───────────
//
// Three separate spots in the template used to write `` `#value` `` (a value
// interpolation immediately inside backtick-delimited raw text). Typst does NOT
// evaluate `#...` inside backticks — it renders the literal characters `#value` — so
// the cover's short SHA, branch name, and a curated-finding site's path all rendered as
// the LITERAL TEXT `#r.short_sha` / `#r.branch` / `#site.path` instead of their values.
// The fix is `#raw(value)` (evaluate first, THEN render as raw/monospace). This test
// pins the shipped template against the exact bug class returning, ever.
#[test]
fn shipped_template_has_no_backtick_hash_interpolation_bug() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(
        !template.contains("`#"),
        "the shipped audit_report.typ template contains a backtick immediately followed \
             by '#' — Typst renders `#value` inside backtick-delimited raw text LITERALLY \
             instead of interpolating it. Use `#raw(value)` (evaluate, then render as raw) \
             instead of `` `#value` `` (see the M6/M8 fixes in \
             docs/design/2026-07-26_audit-report-refinements.md)."
    );
}

/// Item 6 regression guard: the category scorecard renders as a compact heat-grid (color
/// intensity keyed to severity counts, numbers kept), the ONE visual addition the owner
/// approved — no funnels/bar charts/gauges/scatter plots. Pins the template helper names
/// so a future edit can't silently drop the heat-grid back to plain numeric cells (or add
/// an unapproved chart type) without this test flagging it.
#[test]
fn shipped_template_renders_the_scorecard_as_a_heat_grid_and_adds_no_other_chart_types() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(
        template.contains("heat_bg") && template.contains("heat_cell"),
        "the scorecard's heat-grid coloring helpers are missing from the shipped template"
    );
    for banned in ["chart(", "bar-chart", "gauge(", "funnel", "scatter"] {
        assert!(
            !template.contains(banned),
            "found a banned decorative chart primitive ({banned:?}) — the owner's ruling \
                 is the scorecard heat-grid and the severity x effort matrix are the ONLY \
                 visuals; anything else reads as marketing."
        );
    }
}

// ── Item 2 (Bug 4): informational bucketing predicate ─────────────────────────
//
// `is_informational` is the single gate that diverts a low-signal OPEN finding out of the
// do_now/do_next/plan action tiers into the visible-but-advisory `informational` appendix.
// These pin the three independent signals (C5-4 moved the former needs-review signal to its
// own severity-unbounded `is_held_for_review` gate — see `needs_review_confidence_is_held_not_informational`
// below) AND the two hard invariants (never critical/high, never a dispositioned finding)
// that keep real defects in the action tiers.

/// The `info` severity tier is informational by definition — nothing below `low` is an
/// action item (e.g. the unexposed-schema RLS note from I3).
#[test]
fn info_severity_is_always_informational() {
    let f = finding("SOME-RULE-1", "a.rs", 1, "info");
    assert!(is_informational(
        &f,
        Disposition::Unresolved,
        "info",
        None,
        0
    ));
}

/// The load-bearing invariant: a critical or high finding is NEVER auto-demoted to the
/// appendix, no matter which other signals fire. A buyer must see every real defect in the
/// action tiers.
#[test]
fn critical_and_high_are_never_informational() {
    // Even with every other informational signal set (testing-style, needs-review,
    // not-located), a high/critical stays an action item.
    let mut f = finding("ARCH-SERVICE-DI-1", "a.rs", 1, "high");
    f.category = Some("testing-style".to_string());
    f.confidence = Some("needs-review".to_string());
    f.located = false;
    assert!(
        !is_informational(&f, Disposition::Unresolved, "high", None, 0),
        "a high finding must never be auto-informational"
    );
    assert!(
        !is_informational(&f, Disposition::Unresolved, "critical", None, 0),
        "a critical finding must never be auto-informational"
    );
}

/// §2d — a `testing-style` note is informational only when the repo has too few test files
/// to have a house style to deviate FROM. At/above the corpus threshold it's a real note.
#[test]
fn testing_style_is_gated_on_test_corpus_size() {
    let mut f = finding("STYLE-TEST-1", "a.rs", 1, "low");
    f.category = Some("testing-style".to_string());
    // Below the threshold: no corpus to deviate from → informational.
    assert!(is_informational(
        &f,
        Disposition::Unresolved,
        "low",
        None,
        MIN_STYLE_CORPUS_FILES - 1
    ));
    // At/above the threshold: a genuine style deviation → stays an action item.
    assert!(!is_informational(
        &f,
        Disposition::Unresolved,
        "low",
        None,
        MIN_STYLE_CORPUS_FILES
    ));
}

/// C5-4: a finding the calibrator itself flagged `needs-review` is held out of action tiers
/// via `is_held_for_review` — a SEPARATE gate from `is_informational` now (the former §2c
/// signal), with NO severity ceiling, so `is_informational` itself must say `false` for it
/// at every severity (it is no longer one of `is_informational`'s own signals at all).
#[test]
fn needs_review_confidence_is_held_not_informational() {
    let mut f = finding("SOME-RULE-1", "a.rs", 1, "medium");
    f.confidence = Some("needs-review".to_string());
    assert!(
        is_held_for_review(&f, Disposition::Unresolved),
        "a needs-review finding must be held for review"
    );
    assert!(
        !is_informational(&f, Disposition::Unresolved, "medium", None, 0),
        "needs-review is no longer one of is_informational's own signals (C5-4)"
    );
    // A high-confidence finding is neither held nor informational — a normal action item.
    f.confidence = Some("high".to_string());
    assert!(!is_held_for_review(&f, Disposition::Unresolved));
    assert!(!is_informational(
        &f,
        Disposition::Unresolved,
        "medium",
        None,
        0
    ));
}

/// A finding the auditor has EXPLICITLY dispositioned (accepted / tech-debt / FP) keeps its
/// own destination — the predicate only ever re-buckets an OPEN (`Unresolved`) row.
#[test]
fn dispositioned_findings_are_never_re_bucketed() {
    let f = finding("SOME-RULE-1", "a.rs", 1, "info");
    // Even an `info`-severity finding, which is informational when open, is left alone once
    // an auditor has ruled on it.
    assert!(!is_informational(
        &f,
        Disposition::BaselineAccepted,
        "info",
        None,
        0
    ));
    assert!(!is_informational(
        &f,
        Disposition::TechDebtLater,
        "info",
        None,
        0
    ));
}

/// §2a — an ABSENCE-type (`located == false`) `structured` stance rule in the universal /
/// framework layer is a "the project hasn't adopted X" convention, not a concrete defect.
/// Uses the REAL corpus (the predicate reads the rule's enforcement + domain), and the real
/// stance rule `ARCH-SERVICE-DI-1` (domain `api-layer`, enforcement `structured`).
#[tokio::test]
async fn absence_type_structured_stance_rule_is_informational() {
    let corpus = camerata_rules::load_corpus(&camerata_rules::corpus_path())
        .await
        .expect("bundled corpus must load");
    // Sanity: the rule exists and is the shape §2a keys on.
    let rule = corpus
        .get_by_id("ARCH-SERVICE-DI-1")
        .expect("ARCH-SERVICE-DI-1 must exist in the bundled corpus");
    assert_eq!(
        rule.enforcement,
        camerata_rules::EnforcementKind::Structured
    );
    assert_eq!(rule.domain, "api-layer");

    let mut f = finding("ARCH-SERVICE-DI-1", "src/svc.rs", 1, "medium");
    // Absence-type: the "snippet" is a description, not code located in the file.
    f.located = false;
    assert!(
        is_informational(&f, Disposition::Unresolved, "medium", Some(&corpus), 100),
        "an absence-type structured stance note is informational"
    );

    // A PRESENCE-type violation of the same rule (real code located) IS a defect → action.
    f.located = true;
    assert!(
        !is_informational(&f, Disposition::Unresolved, "medium", Some(&corpus), 100),
        "a located (presence-type) violation of the stance rule stays an action item"
    );

    // And a high-severity instance is never demoted, absence-type or not.
    f.located = false;
    assert!(
        !is_informational(&f, Disposition::Unresolved, "high", Some(&corpus), 100),
        "severity still overrides the stance-layer signal"
    );
}

// ── D6 ceiling interaction (2026-09-30 cycle-2 queue-hardening) ────────────────
//
// R1 (`ai_audit::apply_severity_ceiling_rule`) re-routes a self-hedged finding to
// needs-review + Low severity; R2 clamps a browser-mediated CORS misconfiguration to
// exactly Medium. These pin the OUTPUT side here: that the R1 shape is what
// `is_held_for_review` keys on (C5-4: the hedge signal now lives there, not
// `is_informational`), and that R2's medium landing spot buckets into "plan" — never
// "do_now" — so it can never displace a genuine critical from the top action tier.

/// An R1-shaped finding (severity capped to Low, confidence flagged needs-review by the D6
/// ceiling) routes to the `held` bucket (C5-4), out of every action bucket.
#[test]
fn r1_shaped_low_needs_review_finding_is_held_for_review() {
    let mut f = finding("AI-HEDGE-1", "a.rs", 1, "low");
    f.confidence = Some("needs-review".to_string());
    assert!(
        is_held_for_review(&f, Disposition::Unresolved),
        "an R1-shaped (low + needs-review) finding must be held for review"
    );
    assert_eq!(
        effective_bucket(&f, Disposition::Unresolved, "low", None, 0, None),
        "held",
        "it must route to the held bucket, not an action tier"
    );
}

/// A medium-severity CORS finding (the R2 ceiling's landing severity) buckets into "plan" —
/// never "do_now" — so it can never displace a genuine critical finding from the top tier.
#[test]
fn r2_medium_cors_finding_lands_in_plan_not_do_now() {
    let bucket = matrix_bucket(Disposition::Unresolved, "medium", None);
    assert_eq!(
        bucket, "plan",
        "a medium finding (R2's landing severity) belongs in Plan"
    );
    assert_ne!(
        bucket, "do_now",
        "a medium finding must never displace a critical in do_now"
    );
}

// ── D8 ceiling (owner-mandated observability/under-logging convention-class cap) ──────
//
// Capability 2: under-logging / observability thoroughness is SEMANTIC and opinion-shaped
// by the owner's own framing, so it must be capped structurally — a finding from this
// class must NEVER occupy an action bucket (do_now/do_next/plan) on its own, regardless of
// what severity is PROPOSED for it (by a model, by the D5 floor, by anything). The two
// tests below are the bidirectional pin the task calls for: the gate fires for every
// ARCH-OBSERVABILITY-* id at every proposed severity (including critical), and a genuine,
// non-observability rule id at the SAME proposed severities reaches its normal action
// bucket unchanged — proving this is a rule-IDENTITY gate, not a global neutering of high/
// critical severities.

/// Every `ARCH-OBSERVABILITY-*` rule, at every severity band a model could possibly
/// propose (including critical), routes to `"informational"` — never an action bucket.
#[test]
fn observability_convention_class_never_reaches_an_action_bucket_at_any_proposed_severity() {
    for rule_id in [
        "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
        "ARCH-OBSERVABILITY-LOG-CONSISTENCY-1",
        "ARCH-OBSERVABILITY-CORRELATION-ID-1",
    ] {
        for proposed_severity in ["critical", "high", "medium", "low", "info"] {
            let mut f = finding(rule_id, "a.rs", 1, proposed_severity);
            // Even a "low-effort" hint must not help it sneak into do_now via the
            // high+low-effort matrix cell — the gate must win regardless of effort too.
            f.effort = Some("low".to_string());
            let bucket = effective_bucket(
                &f,
                Disposition::Unresolved,
                proposed_severity,
                None,
                0,
                None,
            );
            assert_eq!(
                bucket, "informational",
                "{rule_id} at proposed severity {proposed_severity} must route to \
                     informational, got {bucket}"
            );
            assert!(
                !matches!(bucket, "do_now" | "do_next" | "plan"),
                "{rule_id} at proposed severity {proposed_severity} must never occupy an \
                     action bucket, got {bucket}"
            );
        }
    }
}

/// Bidirectional control for the test above: a genuine (non-observability) rule id DOES
/// reach its normal action bucket at the severities that earn one, proving the D8 gate is
/// scoped to rule IDENTITY rather than a hidden global change to bucket placement.
#[test]
fn non_observability_rule_still_reaches_its_action_bucket_at_matching_proposed_severities() {
    let critical = finding("SEC-SOME-REAL-DEFECT-1", "a.rs", 1, "critical");
    assert_eq!(
        effective_bucket(
            &critical,
            Disposition::Unresolved,
            "critical",
            None,
            0,
            None
        ),
        "do_now",
        "a genuine critical finding must still reach do_now"
    );

    let mut high_low_effort = finding("SEC-SOME-REAL-DEFECT-1", "a.rs", 1, "high");
    high_low_effort.effort = Some("low".to_string());
    assert_eq!(
        effective_bucket(
            &high_low_effort,
            Disposition::Unresolved,
            "high",
            None,
            0,
            None
        ),
        "do_now",
        "a genuine high+low-effort finding must still reach do_now"
    );

    let medium = finding("SEC-SOME-REAL-DEFECT-1", "a.rs", 1, "medium");
    assert_eq!(
        effective_bucket(&medium, Disposition::Unresolved, "medium", None, 0, None),
        "plan",
        "a genuine medium finding must still reach plan"
    );
}

/// End-to-end at the `ai_audit` calibration-pipeline level: `apply_severity_ceiling_rules`
/// (D8) forces an ARCH-OBSERVABILITY-* finding's severity to exactly `"low"` regardless of
/// whatever severity it carried going in — including a model proposing critical — never
/// raising OR lowering any OTHER rule's severity in the same pass.
#[test]
fn d8_ceiling_forces_observability_findings_to_low_regardless_of_input_severity() {
    for input_severity in ["critical", "high", "medium", "info"] {
        let f = finding(
            "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
            "a.rs",
            1,
            input_severity,
        );
        let out = crate::ai_audit::apply_severity_ceiling_rules(vec![f]);
        assert_eq!(
            out[0].severity, "low",
            "input severity {input_severity} must be forced to low"
        );
    }

    // Control: an unrelated rule's severity is untouched by this pass.
    let untouched = finding("SEC-SOME-REAL-DEFECT-1", "a.rs", 1, "critical");
    let out = crate::ai_audit::apply_severity_ceiling_rules(vec![untouched]);
    assert_eq!(out[0].severity, "critical");
}

/// End-to-end at the `build_report_json` level: an `info`-severity finding lands in the
/// `informational` appendix, NEVER in an action tier, and the curated four-bucket invariant
/// `do_now + do_next + plan + accepted == curated_total` still holds exactly (informational
/// is excluded from `curated_total`).
#[test]
fn build_report_json_routes_info_to_appendix_and_preserves_the_invariant() {
    let action = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 10, "critical");
    let advisory = finding("SOME-RULE-1", "b.rs", 20, "info");
    let report = report_with(
        vec![action, advisory],
        vec!["SEC-NO-HARDCODED-SECRETS-1", "SOME-RULE-1"],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.matrix.informational.len(),
        1,
        "the info finding is in the appendix"
    );
    assert_eq!(json.matrix.informational[0].path, "b.rs");
    assert_eq!(
        json.matrix.do_now.len(),
        1,
        "the critical is an action item"
    );

    // No info-severity finding may appear in any action tier.
    for tier in [&json.matrix.do_now, &json.matrix.do_next, &json.matrix.plan] {
        assert!(
            tier.iter().all(|r| r.severity != "info"),
            "no info-severity finding may land in an action tier"
        );
    }

    // The self-checking curated partition still balances, appendix excluded.
    let curated = json.executive_summary.curated_total;
    let summed = json.matrix.do_now.len()
        + json.matrix.do_next.len()
        + json.matrix.plan.len()
        + json.matrix.accepted.len();
    assert_eq!(
        summed, curated,
        "do_now+do_next+plan+accepted must equal curated_total"
    );
    assert_eq!(
        curated, 1,
        "only the critical is curated; the info note is appendix-only"
    );
}

/// C4-R1: the pipeline-wide null-location safety net. A synthetic `critical` finding with
/// an EMPTY path and `line == 0` — the exact shape the Supabase RLS over-firing regression
/// produced (a deterministic rule asserting a falsehood with no real evidence behind it) —
/// must be DOWNGRADED to `info` (never dropped) by `build_report_json`, landing in the
/// `informational` appendix rather than `do_now`. A normally-located critical finding in
/// the SAME report is completely unaffected, proving this is a location-shaped gate, not a
/// blanket demotion of the rule or severity.
#[test]
fn build_report_json_downgrades_an_unlocated_above_informational_finding_but_never_drops_it() {
    let located_critical = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 10, "critical");
    let unlocated_critical = finding("SUPABASE-RLS-POLICY-DISABLED-1", "", 0, "critical");
    let report = report_with(
        vec![located_critical, unlocated_critical],
        vec![
            "SEC-NO-HARDCODED-SECRETS-1",
            "SUPABASE-RLS-POLICY-DISABLED-1",
        ],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    // Still present — downgraded, not dropped.
    assert_eq!(
        json.matrix.informational.len(),
        1,
        "the unlocated critical must still ship, re-bucketed to informational: {:?}",
        json.matrix.informational
    );
    assert_eq!(
        json.matrix.informational[0].rule_id,
        "SUPABASE-RLS-POLICY-DISABLED-1"
    );
    assert_eq!(
        json.matrix.informational[0].severity, "info",
        "the empty-path/line-0 finding's severity must be downgraded to info"
    );

    // The normally-located critical is completely unaffected — this is a location-shaped
    // gate, not a general severity cap.
    assert_eq!(
        json.matrix.do_now.len(),
        1,
        "the located critical stays an action item"
    );
    assert_eq!(json.matrix.do_now[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1");
    assert_eq!(json.matrix.do_now[0].severity, "critical");

    // No action tier may ever contain the unlocated finding.
    for tier in [&json.matrix.do_now, &json.matrix.do_next, &json.matrix.plan] {
        assert!(
            tier.iter()
                .all(|r| r.rule_id != "SUPABASE-RLS-POLICY-DISABLED-1"),
            "the unlocated finding must never land in an action tier: {tier:?}"
        );
    }
}

/// P7 end-to-end: N occurrences of one needs-review STRUCTURAL rule are no longer folded into
/// one row. Every occurrence stays a row, grouped under one rule node of the review tree,
/// and every one of them is HELD (C5-4: a needs-review row), never in a curated action tier,
/// while a genuine critical finding in the same report is untouched.
#[test]
fn structural_needs_review_occurrences_all_stay_rows_held_and_in_one_rule_node() {
    let mut occ1 = finding("ARCH-SOME-PREFERENCE-1", "a.rs", 10, "medium");
    occ1.category = Some("arch-conformance".to_string());
    occ1.confidence = Some("needs-review".to_string());
    let mut occ2 = finding("ARCH-SOME-PREFERENCE-1", "b.rs", 20, "medium");
    occ2.category = Some("arch-conformance".to_string());
    occ2.confidence = Some("needs-review".to_string());
    let mut occ3 = finding("ARCH-SOME-PREFERENCE-1", "c.rs", 30, "medium");
    occ3.category = Some("arch-conformance".to_string());
    occ3.confidence = Some("needs-review".to_string());
    let critical = finding("SEC-NO-HARDCODED-SECRETS-1", "d.rs", 1, "critical");

    // Run the SAME grouping pass the real scan pipeline runs before build_report.
    let grouped = crate::review_tree::collapse_and_group(vec![occ1, occ2, occ3, critical], &[]);
    assert_eq!(
        grouped.len(),
        4,
        "nothing is folded away: three structural occurrences and one critical: {grouped:?}"
    );
    assert_eq!(
        grouped
            .iter()
            .filter(
                |f| f.group_path.get(1).map(String::as_str) == Some("rule:ARCH-SOME-PREFERENCE-1")
            )
            .count(),
        3,
        "the three occurrences share one rule node"
    );

    let report = report_with(
        grouped,
        vec!["ARCH-SOME-PREFERENCE-1", "SEC-NO-HARDCODED-SECRETS-1"],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.matrix.held.len(),
        3,
        "every structural occurrence is its own held row: {:?}",
        json.matrix.held
    );
    assert!(json
        .matrix
        .held
        .iter()
        .all(|r| r.rule_id == "ARCH-SOME-PREFERENCE-1"));
    assert!(
        json.matrix.informational.is_empty(),
        "a needs-review row never routes to informational (C5-4): {:?}",
        json.matrix.informational
    );
    // None of the three original locations leak into an action tier as separate rows.
    for tier in [&json.matrix.do_now, &json.matrix.do_next, &json.matrix.plan] {
        assert!(
            tier.iter().all(|r| r.rule_id != "ARCH-SOME-PREFERENCE-1"),
            "the grouped rule must never appear in a curated action tier: {tier:?}"
        );
    }
    // The critical finding is unaffected — still curated, still do_now.
    assert_eq!(json.matrix.do_now.len(), 1);
    assert_eq!(json.matrix.do_now[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1");
}

// ── C3-6: one bucket computation, read consistently everywhere in the JSON ─────────

/// `FindingRefJson::bucket` (the matrix entry) and `CuratedSiteJson::bucket` (the SAME
/// finding's held-for-review row) must read the SAME value — both come from exactly one
/// `effective_bucket` call per finding now (previously two independent `is_informational`
/// and `matrix_bucket` call sites in this file, which happened to agree today but had no
/// structural guarantee against drifting apart). A hedged (`needs-review`) finding is the
/// sharpest case: it must read `"held"` (C5-4) in BOTH places, never an action bucket — and
/// it still ships (over-tell, never dropped), just consistently re-bucketed.
///
/// C4-P4 (residual defect 5, 2026-10-01) / C5-4: this finding's whole group is now ENTIRELY
/// held (its only site), so it moved out of `curated_findings` into
/// `held_for_review_findings` — see `AuditReportJson::curated_findings`'s doc comment. The
/// bucket-agreement assertions below are unchanged; only which top-level list the group
/// lives in changed (and the bucket label is `"held"`, not `"informational"` — C5-4 split
/// the two).
#[test]
fn matrix_and_curated_site_bucket_fields_agree_and_a_hedged_finding_is_never_an_action_bucket() {
    let mut hedged = finding("SOME-HEDGED-RULE-1", "a.rs", 1, "medium");
    hedged.confidence = Some("needs-review".to_string());
    // A non-`None` `confidence` makes `is_ai_tier` true (it treats any calibrated finding
    // as AI-tier — see that function's doc comment); without a preview tool or grounded
    // citation, the SEPARATE P3 "uncited AI finding" gate would exclude this row from
    // `curated_findings`/`held_for_review_findings` entirely rather than routing it to the
    // held bucket. Giving it a preview tool keeps its citation `"preview"` (not
    // `"advisory"`), isolating THIS test to the hedge/confidence signal this test is
    // actually about.
    hedged.preview_tool = Some("clippy".to_string());
    let report = report_with(vec![hedged], vec!["SOME-HEDGED-RULE-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(json.matrix.held.len(), 1);
    assert_eq!(json.matrix.held[0].bucket, "held");
    assert!(
        json.matrix.informational.is_empty(),
        "a needs-review row never routes to informational (C5-4): {:?}",
        json.matrix.informational
    );
    for tier in [&json.matrix.do_now, &json.matrix.do_next] {
        assert!(
            tier.is_empty(),
            "a hedged finding must never land in an action bucket: {tier:?}"
        );
    }
    assert!(
        json.curated_findings
            .iter()
            .all(|g| g.rule_id != "SOME-HEDGED-RULE-1"),
        "an entirely-held finding must never render under Curated findings: {:?}",
        json.curated_findings
    );

    let group = json
        .held_for_review_findings
        .iter()
        .find(|g| g.rule_id == "SOME-HEDGED-RULE-1")
        .expect("the hedged finding still ships, as a held row in held_for_review_findings");
    assert_eq!(group.sites.len(), 1, "the finding must not be dropped");
    assert_eq!(
        group.sites[0].bucket, "held",
        "the curated site's own bucket field must read held too"
    );
    assert_eq!(
        group.sites[0].bucket, json.matrix.held[0].bucket,
        "matrix and curated-site bucket fields must never disagree for the same finding"
    );
}

// ── Branding (2026-09-13 review): precedence + neutral fallback ────────────────

// ── The summary's claim about preview tools matches the ledger ─────────────────────────────

const NO_PREVIEW_TOOL_CLAIM: &str = "had no scan-time preview tool available";

fn failed_pass_lines(report: &ScanReport) -> Vec<String> {
    build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .executive_summary
    .failed_passes
}

/// A selected rule no preview tool is wired for is judged by the model, and the ledger records it
/// as run. The summary used to tell the client such rules were "not reflected in this report".
#[test]
fn a_rule_the_ledger_shows_as_run_is_not_called_unchecked_for_want_of_a_preview_tool() {
    use crate::scan_ledger::RuleTier;
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![
        crate::scan_tools::unrouted_note("JAVASCRIPT-A-1"),
        crate::scan_tools::unrouted_note("JAVASCRIPT-B-1"),
    ];
    for rule in ["JAVASCRIPT-A-1", "JAVASCRIPT-B-1"] {
        report
            .ledger
            .record_rule("owner/repo", rule, RuleTier::Semantic, true, None, 5, 0);
    }
    let lines = failed_pass_lines(&report);
    assert!(
        !lines.iter().any(|l| l.contains(NO_PREVIEW_TOOL_CLAIM)),
        "the ledger shows both rules ran: {lines:?}"
    );
}

#[test]
fn a_rule_the_ledger_does_not_show_as_run_keeps_the_no_preview_tool_line() {
    use crate::scan_ledger::RuleTier;
    let mut report = report_with(vec![], vec![]);
    report.coverage_notes = vec![
        crate::scan_tools::unrouted_note("JAVASCRIPT-A-1"),
        crate::scan_tools::unrouted_note("JAVASCRIPT-C-1"),
    ];
    report.ledger.record_rule(
        "owner/repo",
        "JAVASCRIPT-A-1",
        RuleTier::Semantic,
        true,
        None,
        5,
        0,
    );
    let lines = failed_pass_lines(&report);
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains(NO_PREVIEW_TOOL_CLAIM))
            .count(),
        1,
        "one rule is on no ledger entry as run, so the line stays: {lines:?}"
    );
}
