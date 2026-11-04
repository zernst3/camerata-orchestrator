//! Zero-width stripping, dispositions, severity ceilings and floors.

use super::*;

/// H2: a client-tooling note is not a tool failure. The opt-in note and the skip note each
/// reach the report in their own words, never as "the ... checks did not fully run".
#[test]
fn client_tooling_notes_reach_the_report_in_their_own_words() {
    let notes = vec![
        crate::onboard::CoverageNote {
            tool_failed: false,
            repo: None,
            tool: "clippy".to_string(),
            message: crate::scan_tools::client_tooling_skipped_message("cargo clippy", 2),
        },
        crate::onboard::CoverageNote {
            tool_failed: false,
            repo: None,
            tool: crate::scan_tools::CLIENT_TOOLING_NOTE_TOOL.to_string(),
            message: "cargo clippy ran with the operator's opt-in to run this repository's \
                          own tooling."
                .to_string(),
        },
    ];
    let lines = tool_coverage_disclosures(&notes);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("was not run")), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("opt-in to run")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("did not fully run")),
        "{lines:?}"
    );
}

/// Like `wire`, but for an `Ignored` disposition a REAL client has actually confirmed.
fn confirmed_wire(reason: &str) -> DispositionWire {
    DispositionWire {
        state: "Ignored".to_string(),
        reason: reason.to_string(),
        bucket: String::new(),
        confirmed_by_client: true,
    }
}

// ── finding_key is the shared site key, and the UI computes the same bytes ──────

/// The key is `repo\0path\0fingerprint(rule, snippet)\0ordinal`: no line, no rule id in the
/// clear. An unkeyed row gets ordinal 0.
#[test]
fn finding_key_is_the_site_key_with_no_line_in_it() {
    let f = finding("RULE-X", "src/a.rs", 7, "high");
    let expected =
        camerata_api_types::site_key::site_key("owner/repo", "src/a.rs", "RULE-X", &f.snippet, 0);
    assert_eq!(finding_key(&f), expected);
    let mut moved = f.clone();
    moved.line = 700;
    assert_eq!(finding_key(&moved), finding_key(&f));
    // A keyed row reports the key it was given.
    let mut keyed = f.clone();
    keyed.site_key = "assigned".to_string();
    assert_eq!(finding_key(&keyed), "assigned");
}

/// The server's `Finding` and the UI's `FindingView` must name every row identically, or a
/// disposition the UI keyed would never be found here. The UI row is built the way the wire
/// builds it (the `Finding` serialised, then read as a `FindingView`), after the keys were
/// assigned the way an inspection assigns them, over a set that includes a repeated line and
/// two advisories on one package version.
#[test]
fn ui_and_server_site_key_agree_byte_for_byte() {
    let mut rows = vec![
        finding("RULE-X", "src/a.rs", 7, "high"),
        finding("RULE-X", "src/a.rs", 40, "high"),
        finding("RULE-Y", "src/b.rs", 3, "low"),
    ];
    for advisory in ["RUSTSEC-2021-0001", "RUSTSEC-2020-0071"] {
        let mut dep = finding(crate::dep_audit::DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
        dep.snippet = "time@0.1.45".to_string();
        dep.captures.insert(
            crate::dep_audit::ADVISORY_CAPTURE.to_string(),
            advisory.to_string(),
        );
        rows.push(dep);
    }
    // The two RULE-X rows share a snippet, so they are a repeat; the advisories share one too.
    let shared = rows[0].snippet.clone();
    rows[1].snippet = shared;
    crate::inspections::backfill_site_keys(&mut rows);

    let keys: std::collections::BTreeSet<String> = rows.iter().map(finding_key).collect();
    assert_eq!(keys.len(), rows.len(), "every row has its own key");
    for f in &rows {
        let wire = serde_json::to_value(f).unwrap();
        let view: camerata_ui_core::triage::FindingView = serde_json::from_value(wire).unwrap();
        assert_eq!(
            camerata_ui_core::triage::finding_key(&view),
            finding_key(f),
            "UI and server disagree for {} at line {}",
            f.rule_id,
            f.line
        );
    }
    // The advisory whose id sorts first gets ordinal 0 whatever order the rows arrived in.
    let by_advisory = |id: &str| {
        rows.iter()
            .find(|f| f.captures.get("advisory").map(String::as_str) == Some(id))
            .unwrap()
    };
    assert!(finding_key(by_advisory("RUSTSEC-2020-0071")).ends_with("\u{0}0"));
    assert!(finding_key(by_advisory("RUSTSEC-2021-0001")).ends_with("\u{0}1"));
    // And a row the server never keyed reads the same on both sides.
    let unkeyed = finding("RULE-Z", "src/z.rs", 1, "low");
    let view: camerata_ui_core::triage::FindingView =
        serde_json::from_value(serde_json::to_value(&unkeyed).unwrap()).unwrap();
    assert_eq!(
        camerata_ui_core::triage::finding_key(&view),
        finding_key(&unkeyed)
    );
}

/// Every consumer of a final report sees keyed rows, so two sites that differ only by an
/// ordinal (a repeated line) are two keys in the export, never one buried under the other.
#[test]
fn sealing_a_report_keys_every_row_and_tells_repeats_apart() {
    let mut a = finding("RULE-X", "src/a.rs", 7, "high");
    let mut b = a.clone();
    a.snippet = "same()".to_string();
    b.snippet = "same()".to_string();
    b.line = 90;
    let report = report_with(vec![b.clone(), a.clone()], vec!["RULE-X"]);
    let sealed = report.seal();
    let keys: Vec<String> = sealed.findings.iter().map(finding_key).collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    assert!(sealed.findings.iter().all(|f| !f.site_key.is_empty()));
    // Line order decides the ordinal: the later line is ordinal 1.
    let later = sealed.findings.iter().find(|f| f.line == 90).unwrap();
    assert!(finding_key(later).ends_with("\u{0}1"));
}

// ── C4-R3 defect 3: zero-width/BOM code points never survive into JSON/xlsx fields ──

#[test]
fn strip_zero_width_removes_every_listed_codepoint_and_borrows_when_clean() {
    let dirty = "apps/web/\u{200B}src/\u{FEFF}a.ts\u{200C}\u{200D}\u{2060}";
    let cleaned = strip_zero_width(dirty);
    assert_eq!(cleaned, "apps/web/src/a.ts");
    for c in ZERO_WIDTH_CODEPOINTS {
        assert!(!cleaned.contains(c));
    }
    // The common case (nothing to strip) must not allocate a new string.
    assert!(matches!(
        strip_zero_width("clean/path.rs"),
        std::borrow::Cow::Borrowed(_)
    ));
}

#[test]
fn sanitize_finding_text_strips_zero_width_from_path_snippet_detail_and_captures() {
    let mut f = finding("SEC-1", "apps/\u{200B}web/a.ts", 1, "high");
    f.snippet = "const x = 1\u{FEFF};".to_string();
    f.detail = "secret\u{200D} found".to_string();
    f.captures
        .insert("table".to_string(), "pro\u{200C}files".to_string());

    let cleaned = sanitize_finding_text(&f);
    assert_eq!(cleaned.path, "apps/web/a.ts");
    assert_eq!(cleaned.snippet, "const x = 1;");
    assert_eq!(cleaned.detail, "secret found");
    assert_eq!(cleaned.captures.get("table").unwrap(), "profiles");
}

/// `build_report_json` must sanitize BEFORE anything downstream reads `path` — the
/// JSON this builds is what both the Typst template and `findings.json` ultimately render,
/// so a zero-width space planted in a scanned repo's own file name (adversarial or just an
/// odd encoding) must never reach either artifact.
#[test]
fn build_report_json_strips_zero_width_characters_from_finding_paths() {
    let f = finding("SEC-1", "apps/\u{200B}web/\u{FEFF}a.ts", 1, "critical");
    let report = report_with(vec![f], vec!["SEC-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let serialized = serde_json::to_string(&json).unwrap();
    for c in ZERO_WIDTH_CODEPOINTS {
        assert!(
            !serialized.contains(c),
            "build_report_json's output must never contain U+{:04X}",
            c as u32
        );
    }
    assert!(serialized.contains("apps/web/a.ts"));
}

/// Same invariant, the xlsx/`findings.json` path (`xlsx_export::partition_rows`, exercised
/// here via `build_findings_export`) — a separate implementation from `build_report_json`,
/// so it needs its own regression rather than relying on the PDF-JSON test above.
#[test]
fn build_findings_export_strips_zero_width_characters_from_finding_paths() {
    let f = finding("SEC-1", "apps/\u{200B}web/\u{FEFF}a.ts", 1, "critical");
    let report = report_with(vec![f], vec!["SEC-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &json,
        &HashMap::new(),
    );
    let serialized = serde_json::to_string(&export).unwrap();
    for c in ZERO_WIDTH_CODEPOINTS {
        assert!(
            !serialized.contains(c),
            "build_findings_export's output must never contain U+{:04X}",
            c as u32
        );
    }
    assert!(serialized.contains("apps/web/a.ts"));
}

/// REG-5 golden-export gate: scan `field` for `<alnum>. <lowercase-or-digit>` — the
/// signature of a space wrongly inserted after a period embedded in an identifier, dotted
/// call, filename, version string, or abbreviation (`client. auth. method()`,
/// `settings. toml`, `0001_migration. sql:12`, `e. g.`, `v1. 2. 3`) — EXCLUDING a match
/// immediately preceded by a known abbreviation (`e.g.`/`i.e.`/`etc.`/`vs.`), which is
/// correct, un-corrupted English (e.g. "...accepts unsafe values, e.g. eval.") and must not
/// be flagged as a false positive. Returns the first offending match, if any.
fn find_period_space_corruption(field: &str) -> Option<&str> {
    let corruption =
        regex::Regex::new(r"[A-Za-z0-9]\. [a-z0-9]").expect("static regex must compile");
    for m in corruption.find_iter(field) {
        // The matched alnum is a single ASCII byte, so the period sits at `m.start() + 1`.
        let period_idx = m.start() + 1;
        if !ends_with_known_abbreviation(&field[..=period_idx]) {
            return Some(m.as_str());
        }
    }
    None
}

/// Pull a named string field off a `serde_json::Value` object, if present and non-null.
fn json_str_field<'a>(obj: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(|v| v.as_str())
}

/// Every headline/detail/snippet/fix-shaped field on one `CuratedSiteJson`/`FindingRefJson`/
/// `GridTagJson`/`ThreeThingsItemJson`/`FindingRow`-shaped JSON object — the full set of
/// client-facing per-finding text fields across the PDF/JSON and xlsx/findings.json shapes.
const FINDING_TEXT_FIELD_KEYS: [&str; 5] = [
    "headline",
    "detail",
    "snippet",
    "fix",
    "fix_for_this_finding",
];

fn assert_no_corruption_in_finding_fields(obj: &serde_json::Value, artifact: &str) {
    for key in FINDING_TEXT_FIELD_KEYS {
        if let Some(s) = json_str_field(obj, key) {
            assert!(
                find_period_space_corruption(s).is_none(),
                "{artifact}'s `{key}` field contains a period-space corruption artifact \
                     ({:?}): {s:?}",
                find_period_space_corruption(s)
            );
        }
    }
    // `findings.json`'s `FindingRow` names the primary-fix column `fix_specific`, not
    // `fix_for_this_finding` — same field shape, different wire name.
    if let Some(s) = json_str_field(obj, "fix_specific") {
        assert!(
            find_period_space_corruption(s).is_none(),
            "{artifact}'s `fix_specific` field contains a period-space corruption artifact \
                 ({:?}): {s:?}",
            find_period_space_corruption(s)
        );
    }
}

/// REG-5 golden-export gate: no client-facing per-finding text field (headline, detail,
/// snippet, fix/fix_specific) in ANY of the three artifacts (the PDF/JSON's
/// `AuditReportJson`, and `findings.json`/the xlsx's `FindingsExport`) may ever contain the
/// period-space corruption pattern — scoped to the FINDING fields specifically (never the
/// static narrative/methodology/disclaimer prose, which legitimately contains sentences
/// that start with a digit — "...produced 1 candidate finding. 1 is curated for action..."
/// — a shape this regex cannot distinguish from corruption and was never meant to police).
/// This is a DURABLE regression gate, not a one-off fixture check: the synthetic finding
/// below exercises the full real pipeline — `ai_audit::group_semantic` (where the
/// corruption was actually introduced, in `split_into_sentences`/
/// `strip_cross_reference_sentences`'s split-then-rejoin) feeding into `build_report_json`
/// and `xlsx_export::build_findings_export` — so any FUTURE change anywhere in that chain
/// that reintroduces a naive "split on every period" step will fail this test loudly,
/// regardless of which function it lands in.
#[test]
fn no_export_artifact_ever_contains_a_space_inserted_inside_an_identifier_or_abbreviation() {
    let mut f = finding(
        "SEC-REG5-IDENTIFIER-HEAVY-1",
        "src/api/invoices.ts",
        42,
        "critical",
    );
    f.detail = "The call to client.auth.method() in config/settings.toml bypasses the \
                     session check. See 0001_migration.sql:12 for the original schema, e.g. the \
                     v1.2.3 release notes at https://x.y/z document the same gap. Anyone with \
                     the anon key can read and write every row."
        .to_string();

    // Run the finding through the SAME merge pass the real pipeline applies before a
    // report is ever built — this is where REG-5 actually lived.
    let merged = crate::review_tree::test_support::cluster_heads(vec![f], &[]);

    let report = report_with(merged, vec!["SEC-REG5-IDENTIFIER-HEAVY-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let json_value = serde_json::to_value(&json).unwrap();

    let mut checked_any = false;
    for bucket in [
        "do_now",
        "do_next",
        "plan",
        "accepted",
        "informational",
        "held",
    ] {
        for item in json_value["matrix"][bucket]
            .as_array()
            .into_iter()
            .flatten()
        {
            assert_no_corruption_in_finding_fields(item, "AuditReportJson.matrix");
            checked_any = true;
        }
    }
    for item in json_value["three_things"]["items"]
        .as_array()
        .into_iter()
        .flatten()
    {
        assert_no_corruption_in_finding_fields(item, "AuditReportJson.three_things");
        checked_any = true;
    }
    for row in json_value["priority_grid"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for cell in row["cells"].as_array().into_iter().flatten() {
            for item in cell["findings"].as_array().into_iter().flatten() {
                assert_no_corruption_in_finding_fields(item, "AuditReportJson.priority_grid");
                checked_any = true;
            }
        }
    }
    for group_key in ["curated_findings", "held_for_review_findings"] {
        for group in json_value[group_key].as_array().into_iter().flatten() {
            for site in group["sites"].as_array().into_iter().flatten() {
                assert_no_corruption_in_finding_fields(site, "AuditReportJson sites");
                checked_any = true;
            }
        }
    }
    assert!(
        checked_any,
        "the fixture finding must reach at least one checked section for this gate to mean \
             anything"
    );
    // Sanity check the fixture's identifiers actually survived verbatim into the artifact.
    let json_serialized = serde_json::to_string(&json).unwrap();
    assert!(
        json_serialized.contains("client.auth.method()")
            && json_serialized.contains("settings.toml")
            && json_serialized.contains("0001_migration.sql:12")
            && json_serialized.contains("v1.2.3"),
        "fixture identifiers must survive into the report untouched for this gate to mean \
             anything: {json_serialized}"
    );

    let findings_export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &json,
        &HashMap::new(),
    );
    let findings_value = serde_json::to_value(&findings_export).unwrap();
    let mut checked_any_rows = false;
    for row in findings_value["findings"].as_array().into_iter().flatten() {
        assert_no_corruption_in_finding_fields(row, "FindingsExport.findings");
        checked_any_rows = true;
    }
    assert!(
        checked_any_rows,
        "the fixture finding must reach findings.json/the xlsx export for this gate to mean \
             anything"
    );
    let findings_serialized = serde_json::to_string(&findings_export).unwrap();
    assert!(
        findings_serialized.contains("client.auth.method()")
            && findings_serialized.contains("settings.toml")
            && findings_serialized.contains("0001_migration.sql:12")
            && findings_serialized.contains("v1.2.3"),
        "fixture identifiers must survive into the findings export untouched for this gate \
             to mean anything: {findings_serialized}"
    );
}

// ── FP exclusion ───────────────────────────────────────────────────────────

#[test]
fn false_positive_findings_are_excluded_and_counted_once() {
    let f1 = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "high");
    let f2 = finding("SEC-NO-HARDCODED-SECRETS-1", "b.rs", 2, "high");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&f1),
        wire("FalsePositive", "generated fixture", ""),
    );
    let report = report_with(vec![f1, f2], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(json.methodology.candidates_reviewed, 2);
    assert_eq!(json.methodology.excluded_false_positive, 1);
    assert_eq!(json.executive_summary.excluded_false_positive, 1);
    // Only the non-FP finding survives into curated findings.
    let total_sites: usize = json.curated_findings.iter().map(|g| g.sites.len()).sum();
    assert_eq!(total_sites, 1);
    assert_eq!(json.curated_findings[0].sites[0].path, "b.rs");
}

// ── Ignored -> accepted risk ───────────────────────────────────────────────

#[test]
fn ignored_finding_is_accepted_risk_with_reason_when_client_confirmed() {
    let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&f), confirmed_wire("accepted for now"));
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Accepted risk: accepted for now"
    );
    assert_eq!(json.matrix.accepted.len(), 1);
    assert_eq!(json.executive_summary.accepted, 1);
}

// ── Item 4 regression: never fabricate a client disposition ───────────────

#[test]
fn ignored_finding_without_client_confirmation_needs_client_confirmation() {
    // The DEFAULT (confirmed_by_client absent/false) must NEVER read as if a client
    // confirmed anything, no matter how confident the auditor's own reason sounds — a
    // security-literate reader who spots an invented client disposition discards the
    // whole document.
    let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&f),
        wire("Ignored", "looks like defense-in-depth only", ""),
    );
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Needs client confirmation (reviewer's proposed rationale: looks like defense-in-depth only)"
    );
    assert_eq!(
        json.matrix.accepted.len(),
        1,
        "still an accepted-bucket disposition"
    );
}

#[test]
fn ignored_finding_without_reason_or_confirmation_is_plain_needs_client_confirmation() {
    let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&f), wire("Ignored", "", ""));
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Needs client confirmation"
    );
}

// ── TechDebt Now/Later -> do-now / planned ─────────────────────────────────

#[test]
fn tech_debt_now_and_later_route_to_matrix_do_now_and_plan() {
    let now_f = finding("ARCH-1", "a.rs", 1, "low");
    let later_f = finding("ARCH-1", "b.rs", 2, "low");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&now_f), wire("TechDebt", "", "Now"));
    dispositions.insert(finding_key(&later_f), wire("TechDebt", "", "Later"));
    let report = report_with(vec![now_f, later_f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(json.matrix.do_now.len(), 1);
    assert_eq!(json.matrix.plan.len(), 1);
    assert_eq!(json.matrix.do_now[0].path, "a.rs");
    assert_eq!(json.matrix.plan[0].path, "b.rs");
}

// ── Unresolved -> open, severity×effort quadrant ───────────────────────────

#[test]
fn unresolved_high_severity_low_effort_is_do_now() {
    let mut f = finding("SEC-1", "a.rs", 1, "critical");
    f.effort = Some("low".to_string());
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(json.matrix.do_now.len(), 1);
    assert_eq!(json.executive_summary.open, 1);
    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Open (recommended: Do now)"
    );
}

#[test]
fn unresolved_high_severity_unknown_effort_is_do_next_not_do_now() {
    // No calibration ran (effort = None) — must NOT be treated as low-effort.
    let f = finding("SEC-1", "a.rs", 1, "high");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.do_next.len(), 1);
    assert_eq!(json.matrix.do_now.len(), 0);
}

#[test]
fn unresolved_low_severity_is_plan() {
    let f = finding("STYLE-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.plan.len(), 1);
}

// ── M3 regression: an uncalibrated CRITICAL must never sit in "Do next" ───

#[test]
fn unresolved_critical_with_no_effort_is_do_now_not_do_next() {
    // The deterministic floor / preview findings never get a calibrated effort estimate
    // (effort = None). Before M3 this fell into "Do next" alongside a merely-high finding
    // — the worst finding in the report could sit un-actioned. A critical must be Do now
    // regardless of effort.
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "critical");
    assert_eq!(
        f.effort, None,
        "test fixture must not carry an effort estimate"
    );
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.do_now.len(), 1, "{:?}", json.matrix);
    assert_eq!(json.matrix.do_next.len(), 0);
    assert!(
        !json.three_things.items.is_empty(),
        "the three-things box must not be empty when a critical do_now finding exists"
    );
}

#[test]
fn unresolved_high_effort_medium_is_still_do_next_not_downgraded() {
    // A "high" (not critical) finding with unknown effort keeps the pre-existing
    // conservative behavior: Do next, never Do now, never silently downgraded to Plan.
    let f = finding("ARCH-1", "a.rs", 1, "high");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.do_next.len(), 1);
    assert_eq!(json.matrix.do_now.len(), 0);
    assert_eq!(json.matrix.plan.len(), 0);
}

// ── M7 regression: Unresolved disposition names its actual matrix bucket ───

#[test]
fn unresolved_disposition_label_names_do_next_bucket() {
    let f = finding("ARCH-1", "a.rs", 1, "high");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Open (recommended: Do next)"
    );
}

#[test]
fn unresolved_disposition_label_names_plan_bucket() {
    let f = finding("STYLE-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Open (recommended: Plan)"
    );
}

// ── S10 regression: severity casing drift must not silently demote to "low" ────

#[test]
fn severity_casing_drift_is_normalized_not_silently_demoted_to_low() {
    let f = finding("SEC-1", "a.rs", 1, "Critical"); // upstream casing drift
    let report = report_with(vec![f], vec!["SEC-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    // Must land in `do_now` (critical), never counted as `low` in the scorecard.
    assert_eq!(json.matrix.do_now.len(), 1, "{:?}", json.matrix);
    assert_eq!(json.scorecard.rows[0].critical, 1);
    assert_eq!(json.scorecard.rows[0].low, 0);
    assert_eq!(json.curated_findings[0].sites[0].severity, "critical");
}

// ── Pass-1 W3 (A5.2/A5.6): the deviation ceiling + the kind on the PDF wire ──────────

/// A deviation-kind finding under `rule_id` — exactly what
/// `ai_audit::assign_deviation_kinds` emits for a rule decided to a non-violates option.
fn deviation_finding(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
    use camerata_api_types::audit::{DeviationKind, FindingKind};
    let mut f = finding(rule_id, path, line, severity);
    f.kind = Some(FindingKind::Deviation(DeviationKind {
        deviating_sites: 2,
        assessed_sites: Some(47),
        convention_option_id: Some("default".to_string()),
        convention_label: "Default".to_string(),
    }));
    f
}

/// Same single-rule corpus as [`ruleset_with_max_severity`], but authoring the A5.2
/// `deviation_ceiling` escape hatch instead of the E2 `max_severity` ceiling.
fn ruleset_with_deviation_ceiling(rule_id: &str, ceiling: Option<&str>) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(rule_id, "test-fixture");
    // max_severity "critical" = effectively no E2 clamp; isolate the deviation clamp.
    rule.max_severity = Some("critical".to_string());
    rule.deviation_ceiling = ceiling.map(str::to_string);
    rule.default_option = Some("default".to_string());
    rule.options = vec![camerata_rules::RuleOption {
        id: "default".to_string(),
        label: "Default".to_string(),
        directive: "do the right thing".to_string(),
        why: "because".to_string(),
        remediation: Some("Fix the thing.".to_string()),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }];
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

/// req A5.2: a consistency deviation caps at `medium` BY DEFAULT (no authored ceiling) —
/// the forward-drift-cost default — while a Defect under the SAME rule at the same
/// severity is untouched, and a legacy row with NO kind is never treated as a deviation.
/// FAILS BEFORE THE FIX: `clamp_deviation_severity` did not exist; a deviation-kind
/// finding arriving at `high` exported `high` unconditionally.
#[test]
fn req_a5_2_deviation_severity_caps_at_medium_by_default() {
    let corpus = ruleset_with_deviation_ceiling("DEV-RULE-1", None);
    let dev = deviation_finding("DEV-RULE-1", "a.rs", 1, "high");
    assert_eq!(
        clamp_deviation_severity("high".to_string(), &dev, Some(&corpus)),
        "medium"
    );
    // Never a floor: a low deviation stays low.
    assert_eq!(
        clamp_deviation_severity("low".to_string(), &dev, Some(&corpus)),
        "low"
    );
    // A Defect under the same rule is untouched.
    let mut defect = finding("DEV-RULE-1", "a.rs", 2, "high");
    defect.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    assert_eq!(
        clamp_deviation_severity("high".to_string(), &defect, Some(&corpus)),
        "high"
    );
    // A legacy row (no kind) is NOT assumed to be a deviation.
    let legacy = finding("DEV-RULE-1", "a.rs", 3, "high");
    assert_eq!(
        clamp_deviation_severity("high".to_string(), &legacy, Some(&corpus)),
        "high"
    );
    // No corpus at all -> the default ceiling still applies to a deviation (the cap is a
    // pipeline default, not a corpus privilege).
    assert_eq!(
        clamp_deviation_severity("critical".to_string(), &dev, None),
        "medium"
    );
}

/// req A5.2 escape hatch: a rule authoring `deviation_ceiling = "high"` lets a genuinely
/// problematic deviation reach `high` — and the ceiling still caps `critical`.
/// FAILS BEFORE THE FIX: no `deviation_ceiling` corpus field existed to author.
#[test]
fn req_a5_2_authored_deviation_ceiling_lets_a_deviation_reach_high() {
    let corpus = ruleset_with_deviation_ceiling("DEV-RULE-1", Some("high"));
    let dev = deviation_finding("DEV-RULE-1", "a.rs", 1, "high");
    assert_eq!(
        clamp_deviation_severity("high".to_string(), &dev, Some(&corpus)),
        "high",
        "the authored escape hatch must let a deviation reach high"
    );
    assert_eq!(
        clamp_deviation_severity("critical".to_string(), &dev, Some(&corpus)),
        "high",
        "the authored ceiling is still a ceiling"
    );
}

/// req A5.2 END-TO-END through `build_report_json`: a deviation arriving at `high` lands
/// in the artifact at `medium` (matrix row AND curated site agree), while a defect under
/// the same rule keeps `high`. FAILS BEFORE THE FIX: both exported `high`.
#[test]
fn req_a5_2_report_json_caps_a_deviation_at_medium_but_not_a_defect() {
    let corpus = ruleset_with_deviation_ceiling("DEV-RULE-1", None);
    let dev = deviation_finding("DEV-RULE-1", "a.rs", 1, "high");
    let mut defect = finding("DEV-RULE-1", "b.rs", 2, "high");
    defect.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    let report = report_with(vec![dev, defect], vec!["DEV-RULE-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let sev_of = |path: &str| -> String {
        [
            &json.matrix.do_now,
            &json.matrix.do_next,
            &json.matrix.plan,
            &json.matrix.accepted,
            &json.matrix.informational,
            &json.matrix.held,
        ]
        .iter()
        .flat_map(|v| v.iter())
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("no matrix row for {path}"))
        .severity
        .clone()
    };
    assert_eq!(sev_of("a.rs"), "medium", "deviation capped");
    assert_eq!(sev_of("b.rs"), "high", "defect untouched");
}

/// req A5.6: the PDF's own data rows carry the kind LOSSLESSLY — a deviation matrix row
/// serializes the flat `"kind": "deviation"` discriminator plus the full structured
/// sentence (numerator/denominator/convention), and a legacy row with no kind serializes
/// the explicit `"unspecified"` tag, never `"defect"` and never a missing key.
/// FAILS BEFORE THE FIX: `FindingRefJson` had no kind field at all.
#[test]
fn req_a5_6_report_json_rows_carry_kind_and_legacy_renders_unspecified() {
    let dev = deviation_finding("DEV-RULE-1", "a.rs", 1, "high");
    let legacy = finding("LEGACY-RULE-1", "b.rs", 2, "high");
    let report = report_with(vec![dev, legacy], vec!["DEV-RULE-1", "LEGACY-RULE-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let v = serde_json::to_value(&json.matrix).expect("matrix serializes");
    let rows: Vec<&serde_json::Value> = [
        "do_now",
        "do_next",
        "plan",
        "accepted",
        "informational",
        "held",
    ]
    .iter()
    .flat_map(|b| v[*b].as_array().into_iter().flatten())
    .collect();
    let row_for = |path: &str| -> &serde_json::Value {
        rows.iter()
            .find(|r| r["path"] == path)
            .unwrap_or_else(|| panic!("no serialized matrix row for {path}"))
    };
    let dev_row = row_for("a.rs");
    assert_eq!(dev_row["kind"], "deviation");
    assert_eq!(dev_row["deviating_sites"], 2);
    assert_eq!(dev_row["assessed_sites"], 47);
    assert_eq!(dev_row["convention_label"], "Default");
    let legacy_row = row_for("b.rs");
    assert_eq!(
        legacy_row["kind"], "unspecified",
        "an absent kind must surface explicitly, never silently"
    );
    assert_ne!(legacy_row["kind"], "defect");
    // The curated sites carry it too.
    let curated = serde_json::to_value(&json.curated_findings).expect("curated serializes");
    let kinds: Vec<String> = curated
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["sites"].as_array().into_iter().flatten())
        .map(|s| s["kind"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        kinds.iter().any(|k| k == "deviation") || kinds.iter().any(|k| k == "unspecified"),
        "curated sites must serialize a kind key: {kinds:?}"
    );
}

// ── E2 (the audit findings tracker): a rule's `max_severity`
// ceiling, applied LAST — never exceeded by any report, never raising a lower severity ──

/// Every site severity a report actually exported, scanning BOTH top-level row sections.
///
/// W5-Q4, ruled by Zach on 2026-10-11 (*"yes, widen these tests"*): a severity test that
/// reads only `curated_findings` is structurally BLIND to the rules whose severity bound is
/// most emphatically authored. `is_informational` routes the `ARCH-OBSERVABILITY-*` class
/// out of every action bucket by rule IDENTITY, so its group renders under
/// `held_for_review_findings` and `curated_findings[0]` *panics* rather than failing
/// cleanly. Every pre-existing `req_e2_*` test had that shape. This helper is the fix, and
/// it matches the one `tests/real_corpus_severity_ceiling_e2e.rs` already uses.
fn exported_site_severities(json: &AuditReportJson) -> Vec<String> {
    json.curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter())
        .map(|s| s.severity.clone())
        .collect()
}

/// [`exported_site_severities`] for the single-row case, asserting the row was exported
/// exactly once — so a silently DROPPED row can never read as a passing clamp or raise.
fn exported_site_severity(json: &AuditReportJson) -> String {
    let severities = exported_site_severities(json);
    assert_eq!(
        severities.len(),
        1,
        "expected exactly one exported site; a dropped row is not a severity result: \
             {severities:?}"
    );
    severities[0].clone()
}

/// A single-rule corpus with `max_severity` set AND a real, authored default option — so
/// the finding has a resolvable fix and is NOT diverted to the `held` bucket by the C5-8
/// "action-tier row with no fix to show at all" gate, which would otherwise make
/// `curated_findings` empty for reasons unrelated to what this test is proving.
fn ruleset_with_max_severity(rule_id: &str, max_severity: &str) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(rule_id, "test-fixture");
    rule.max_severity = Some(max_severity.to_string());
    rule.default_option = Some("default".to_string());
    rule.options = vec![camerata_rules::RuleOption {
        id: "default".to_string(),
        label: "Default".to_string(),
        directive: "do the right thing".to_string(),
        why: "because".to_string(),
        remediation: Some("Fix the thing.".to_string()),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }];
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

/// req_e2: a rule declaring `max_severity = "high"` never exports `critical`, even when
/// the finding's own (pre-export) severity is already `"critical"` — standing in for "a
/// severity floor would otherwise raise it" (D5's floor runs upstream in `ai_audit.rs`,
/// before a finding ever reaches this export layer; a finding arriving here already at
/// `critical` is exactly what a floor-raise looks like by the time this layer sees it).
///
/// FAILS BEFORE THE FIX: before `Rule::max_severity`/`clamp_to_rule_max_severity` existed,
/// there was no corpus field to read a ceiling from at all — this exact finding would have
/// exported as `critical` unconditionally (see the sibling
/// `severity_casing_drift_is_normalized_not_silently_demoted_to_low` test just above, which
/// pins today's un-clamped `critical` behavior for a rule with no ceiling).
#[test]
fn req_e2_max_severity_clamps_a_critical_finding_down_to_the_declared_ceiling() {
    let corpus = ruleset_with_max_severity("CEILING-1", "high");
    let f = finding("CEILING-1", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec!["CEILING-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    // W5-Q4: both sections, via `exported_site_severities` — see its doc comment. The
    // asserted value is unchanged.
    assert_eq!(
        exported_site_severity(&json),
        "high",
        "a max_severity=\"high\" rule must never export critical"
    );
    assert_eq!(json.scorecard.rows[0].critical, 0, "{:?}", json.scorecard);
    assert_eq!(
        json.matrix.do_now.len(),
        0,
        "critical-only do_now must be empty: {:?}",
        json.matrix
    );
}

/// req_e2: the ceiling is a CEILING, not a floor — a finding already BELOW the declared
/// `max_severity` must never be raised up to it.
#[test]
fn req_e2_max_severity_never_raises_a_lower_severity_up_to_the_ceiling() {
    let corpus = ruleset_with_max_severity("CEILING-1", "high");
    let f = finding("CEILING-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec!["CEILING-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    // W5-Q4: both sections — see `exported_site_severities`. Asserted value unchanged.
    assert_eq!(
        exported_site_severity(&json),
        "low",
        "max_severity must never RAISE a severity that already ranks below it"
    );
}

/// req_e2, pure unit coverage of the clamp function itself (independent of the full
/// `build_report_json` pipeline): no ceiling declared is a no-op.
#[test]
fn req_e2_clamp_to_rule_max_severity_is_a_noop_when_the_rule_has_no_ceiling() {
    let mut rule = camerata_rules::bare_rule("NO-CEILING-1", "test-fixture");
    rule.max_severity = None;
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    assert_eq!(
        clamp_to_rule_max_severity("critical".to_string(), "NO-CEILING-1", Some(&set)),
        "critical"
    );
}

/// req_e2, pure unit coverage: an unknown corpus (`None`) or an unknown rule id both
/// degrade to a no-op rather than panicking or fabricating a ceiling.
#[test]
fn req_e2_clamp_to_rule_max_severity_degrades_gracefully_with_no_corpus_or_unknown_rule() {
    assert_eq!(
        clamp_to_rule_max_severity("critical".to_string(), "ANY-RULE-1", None),
        "critical"
    );
    let set = ruleset_with_max_severity("CEILING-1", "high");
    assert_eq!(
        clamp_to_rule_max_severity("critical".to_string(), "SOME-OTHER-RULE-1", Some(&set)),
        "critical"
    );
}

// ── W8 / W5-Q2: a rule's `min_severity` FLOOR — the structured mirror of the ceiling.
// Zach, 2026-10-11: "Floor, it's the minimum rating." ──────────────────────────────────

/// [`ruleset_with_max_severity`]'s floor counterpart: a single-rule corpus authoring
/// `min_severity` (and optionally `max_severity`), with a real default option so the row
/// is not diverted by the C5-8 no-fix gate for reasons unrelated to severity.
fn ruleset_with_min_severity(
    rule_id: &str,
    min_severity: &str,
    max_severity: Option<&str>,
) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(rule_id, "test-fixture");
    rule.min_severity = Some(min_severity.to_string());
    rule.max_severity = max_severity.map(str::to_string);
    rule.default_option = Some("default".to_string());
    rule.options = vec![camerata_rules::RuleOption {
        id: "default".to_string(),
        label: "Default".to_string(),
        directive: "do the right thing".to_string(),
        why: "because".to_string(),
        remediation: Some("Fix the thing.".to_string()),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }];
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

/// A `Defect`-kind finding — what every accusing producer actually emits
/// (`ai_audit::parse_findings`, `apply_alternative_decision`, `dep_audit`, the
/// deterministic floor). The floor declines a row with NO kind by design, so a floor test
/// built on a bare `finding()` would be testing the decline path, not the raise.
fn defect_finding(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
    let mut f = finding(rule_id, path, line, severity);
    f.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    f
}

fn exported_with(corpus: &camerata_rules::RuleSet, f: Finding) -> AuditReportJson {
    let rule_id = f.rule_id.clone();
    let report = report_with(vec![f], vec![&rule_id]);
    build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.seal(), &HashMap::new()),
        Some(corpus),
        &empty_opts(),
    )
}

/// req_w8: a rule declaring `min_severity = "high"` never exports anything BELOW high —
/// the exact mirror of `req_e2_max_severity_clamps_a_critical_finding_down_to_the_declared_
/// ceiling`, in the opposite direction.
///
/// FAILS BEFORE THE FIX: `Rule::min_severity` and `raise_to_rule_min_severity` did not
/// exist, so there was no corpus field to author a minimum in and a `low` finding exported
/// `low` unconditionally — the sibling
/// `req_e2_max_severity_never_raises_a_lower_severity_up_to_the_ceiling` pins exactly that
/// un-raised behaviour for a rule with no floor.
#[test]
fn req_w8_min_severity_raises_a_finding_up_to_the_declared_floor() {
    let corpus = ruleset_with_min_severity("FLOOR-1", "high", None);
    let json = exported_with(&corpus, defect_finding("FLOOR-1", "a.rs", 1, "low"));
    assert_eq!(
        exported_site_severity(&json),
        "high",
        "a min_severity=\"high\" rule must never export below high"
    );
}

/// req_w8: the floor is a FLOOR, not a ceiling — a finding already ABOVE it is untouched.
/// The mirror of the ceiling's own never-raises test, and the assertion that stops a
/// mis-signed rank comparison from reading as a pass.
#[test]
fn req_w8_min_severity_never_lowers_a_severity_already_above_the_floor() {
    let corpus = ruleset_with_min_severity("FLOOR-1", "medium", None);
    let json = exported_with(&corpus, defect_finding("FLOOR-1", "a.rs", 1, "critical"));
    assert_eq!(
        exported_site_severity(&json),
        "critical",
        "min_severity must never LOWER a severity that already ranks above it"
    );
}

/// req_w8: the floor NEVER touches an informational kind. A `ConventionObserved` row tells
/// the client "your codebase already does this"; raising it to `critical` would both lie
/// and, via `is_informational`'s severity test, re-route a compliment into an action
/// bucket. This is the single most dangerous thing a floor could do, so it is asserted on
/// the function directly rather than only through the export.
#[test]
fn req_w8_the_floor_never_applies_to_an_informational_kind() {
    use camerata_api_types::audit::{
        ConventionObservedKind, FindingKind, NotAssessedKind, NotAssessedReason,
    };
    let corpus = ruleset_with_min_severity("FLOOR-1", "critical", None);
    let mut observed = finding("FLOOR-1", "a.rs", 1, "info");
    observed.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "default".to_string(),
        convention_label: "Default".to_string(),
    }));
    assert_eq!(
        raise_to_rule_min_severity("info".to_string(), &observed, Some(&corpus)),
        "info",
        "a convention-observed row must never be raised by a defect floor"
    );
    let mut not_assessed = finding("FLOOR-1", "a.rs", 2, "info");
    not_assessed.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::ToolFailed,
        reason_detail: "the tool failed".to_string(),
    }));
    assert_eq!(
        raise_to_rule_min_severity("info".to_string(), &not_assessed, Some(&corpus)),
        "info",
        "an honesty row must never be raised by a defect floor"
    );
}

/// req_w8: an ABSENT kind is excluded too. W3's standing rule is that nothing may silently
/// default to `Defect`; treating an unkinded legacy row as eligible for a defect floor
/// would be exactly that. Same decline `clamp_deviation_severity` makes for the same
/// reason.
#[test]
fn req_w8_the_floor_never_applies_to_a_row_with_no_kind() {
    let corpus = ruleset_with_min_severity("FLOOR-1", "critical", None);
    let legacy = finding("FLOOR-1", "a.rs", 1, "low");
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &legacy, Some(&corpus)),
        "low",
        "an absent kind must not be assumed to be a Defect"
    );
}

/// req_w8: the floor cannot defeat the C4-R1 null-location safety net. A finding with an
/// empty path AND `line == 0` established nothing concrete to rate, and
/// `downgrade_unlocated_above_informational` forces that shape to `info`. The floor
/// declines it at its own precondition, so the guarantee holds from ANY position in the
/// chain and on any surface.
#[test]
fn req_w8_the_floor_cannot_defeat_the_unlocated_downgrade() {
    let corpus = ruleset_with_min_severity("FLOOR-1", "critical", None);
    let mut unlocated = defect_finding("FLOOR-1", "", 0, "low");
    unlocated.path = String::new();
    unlocated.line = 0;
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &unlocated, Some(&corpus)),
        "low",
        "a floor must not manufacture a critical for a finding with no location"
    );
    let json = exported_with(&corpus, unlocated);
    assert_eq!(
        exported_site_severity(&json),
        "info",
        "C4-R1 must still demote the unlocated row to info"
    );
}

/// req_w8: `min_severity` and `max_severity` set to the SAME value pin the rule at exactly
/// that severity from both sides — which is what several rules' prose actually authors
/// ("a high-severity finding for review, not an automatic critical"). The corpus-load gate
/// deliberately permits the equal case; this proves it behaves.
#[test]
fn req_w8_a_floor_equal_to_the_ceiling_pins_the_rule_from_both_sides() {
    let corpus = ruleset_with_min_severity("PINNED-1", "high", Some("high"));
    assert_eq!(
        exported_site_severity(&exported_with(
            &corpus,
            defect_finding("PINNED-1", "a.rs", 1, "critical")
        )),
        "high",
        "the ceiling must still clamp a critical down"
    );
    assert_eq!(
        exported_site_severity(&exported_with(
            &corpus,
            defect_finding("PINNED-1", "b.rs", 2, "low")
        )),
        "high",
        "the floor must still raise a low up"
    );
}

/// req_w8: the A5.2 deviation ceiling still binds over a floor. A rule authoring
/// `min_severity = "high"` with no `deviation_ceiling` raises a `Deviation` row to high and
/// then caps it back at the `medium` default — deviations describe forward drift cost, and
/// A5.2's default is not overridable by a defect-oriented floor.
#[test]
fn req_w8_a_deviation_row_is_still_bounded_by_the_a5_2_deviation_ceiling() {
    // `max_severity = "critical"` isolates the deviation clamp exactly as
    // `ruleset_with_deviation_ceiling` does; the rule authors NO `deviation_ceiling`, so
    // A5.2's `medium` default applies.
    let corpus = ruleset_with_min_severity("DEV-FLOOR-1", "high", Some("critical"));
    let dev = deviation_finding("DEV-FLOOR-1", "a.rs", 1, "low");
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &dev, Some(&corpus)),
        "high",
        "a deviation is an accusing kind, so the floor does reach it"
    );
    let json = exported_with(&corpus, dev);
    assert_eq!(
        exported_site_severity(&json),
        "medium",
        "A5.2's default deviation ceiling must still cap the raised severity"
    );
}

/// req_w8, pure unit coverage mirroring
/// `req_e2_clamp_to_rule_max_severity_degrades_gracefully_with_no_corpus_or_unknown_rule`:
/// absent corpus, unknown rule, and a rule with no floor all degrade to a no-op rather
/// than panicking or fabricating a floor.
#[test]
fn req_w8_raise_to_rule_min_severity_degrades_gracefully() {
    let f = defect_finding("ANY-RULE-1", "a.rs", 1, "low");
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &f, None),
        "low",
        "no corpus is a no-op"
    );
    let set = ruleset_with_min_severity("FLOOR-1", "high", None);
    assert_eq!(
        raise_to_rule_min_severity("low".to_string(), &f, Some(&set)),
        "low",
        "an unknown rule id is a no-op"
    );
    let no_floor = ruleset_with_max_severity("NO-FLOOR-1", "critical");
    assert_eq!(
        raise_to_rule_min_severity(
            "low".to_string(),
            &defect_finding("NO-FLOOR-1", "a.rs", 1, "low"),
            Some(&no_floor)
        ),
        "low",
        "a rule declaring no floor is a no-op"
    );
}

// ── Item 1: defect headline, not the rule's invariant title ────────────────
