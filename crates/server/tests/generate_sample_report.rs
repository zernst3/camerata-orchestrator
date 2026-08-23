//! Regenerates the flagship demo files under `sample-report/` from the REAL report
//! pipeline (`report_export::build_report_json` -> `report_export::compile_pdf` /
//! `xlsx_export::build_workbook`, the exact embedded template + workbook serializer shipped
//! in the binary) — never hand-typed. As of the product-export pass (2026-07-27), this also
//! emits `camerata-sample-audit-findings.xlsx` (the real workbook) and
//! `camerata-sample-audit.zip` (the real PDF + real xlsx + a README.txt, zipped the same way
//! `POST /api/projects/:id/product-export` does — the zip-assembly glue itself is inlined
//! here rather than calling the server's private handler helpers, but the PDF and xlsx BYTES
//! inside it are the exact same serializer output a live export would produce).
//!
//! Why this exists (2026-07-26 audit-report-refinements pass, "demo honesty" north star):
//! the sample PDF used to be materially nicer than what the serializer could actually
//! produce — several sample strings were unreachable by the real code (fabricated
//! candidate/exclusion counts that didn't reconcile, hand-written category labels the
//! scorecard's `category_for` fallback could never emit, a hand-written narrative while
//! `is_override` claimed `false`). This test closes that gap the other direction: instead of
//! hand-editing `sample-report/data.json` to look nicer, it builds a realistic
//! `onboard::ScanReport` fixture (hand-constructed, like the unit tests in
//! `report_export.rs` — NOT run through the real scanner; only the SERIALIZER + COMPILER are
//! "real" here) and runs it through the exact same code path a live export uses. Whatever
//! comes out — narrative wording, category names, the "N further rules verified clean"
//! line, the reconciled candidate/exclusion counts — is provably reproducible by the code
//! next to it, not an aspirational hand-edit.
//!
//! `#[ignore]`d: this WRITES files into the repo (`sample-report/data.json` +
//! `sample-report/camerata-sample-audit.pdf`) rather than asserting anything, so it must not
//! run as part of the normal `cargo test` gate. Run explicitly:
//!
//!   cargo test -p camerata-server --test generate_sample_report -- --ignored --nocapture
//!
//! After running this, regenerate the preview PNGs from `sample-report/audit_report.typ`
//! (which must be kept byte-identical to `crates/server/templates/audit_report.typ` — see
//! that file's own header) via:
//!
//!   typst compile sample-report/audit_report.typ "sample-report/page-{p}.png" --ppi 130

use std::collections::HashMap;
use std::path::Path;

use camerata_server::dep_audit::DEP_AUDIT_RULE_ID;
use camerata_server::onboard::{AuditedRef, CoverageNote, Finding, ScanProvenance, ScanReport};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};
use camerata_server::xlsx_export;

fn finding(rule_id: &str, repo: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        ..Finding::default()
    }
}

#[tokio::test]
#[ignore = "writes sample-report/ fixtures on demand; not part of the normal test gate"]
async fn regenerate_sample_report() {
    // ═══ The fixture: a two-repo Supabase-backed portal, hand-built like the report_export
    // unit tests' `report_with()` helper — NOT run through the real scanner. Only the
    // downstream serializer + Typst compiler are "real" here (see module doc comment). ═══
    const PORTAL: &str = "harbor-portal";
    const EDGE: &str = "harbor-edge-functions";

    // ── 6 real (non-FP) code findings, hitting all four matrix buckets ──────────────────
    let mut rls_enabled = finding(
        "SUPABASE-RLS-ENABLED-1",
        PORTAL,
        "supabase/migrations/20240301_init.sql",
        14,
        "critical",
    );
    rls_enabled.effort = Some("low".to_string());
    rls_enabled.confidence = Some("high".to_string());
    rls_enabled.snippet = "create table public.profiles (\n  id uuid primary key references auth.users,\n  full_name text,\n  email text,\n  phone text\n);".to_string();
    // Item 1: `detail`'s FIRST SENTENCE is the punchy, defect-first headline the report's
    // serializer mechanically lifts out (`defect_headline`) — the owner's own target voice
    // ("profiles table has no RLS: all member PII publicly readable and writable with the
    // anon key.") — with the remainder of the paragraph supplying the supporting evidence.
    rls_enabled.detail = "The profiles table has no RLS: all member PII is publicly readable and writable with the anon key. No migration in the timeline ever enables Row Level Security on it, verified by replaying supabase/migrations in order. The anon (public) API key ships in the frontend bundle, so this is exploitable by anyone on the internet with no authentication. No evidence of RLS in the repository is not the same as RLS being disabled in production; confirm against the live database before remediation, since dashboard changes are not visible to a repository scan.".to_string();

    let mut key_service_role = finding(
        "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
        PORTAL,
        "apps/web/.env.production",
        6,
        "critical",
    );
    key_service_role.effort = Some("low".to_string());
    key_service_role.confidence = Some("high".to_string());
    key_service_role.snippet = "NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY=sb_secret_[redacted]".to_string();
    key_service_role.detail = "The service_role key is shipped to every browser: it bypasses all Row Level Security and grants full read and write access to every table. A service_role secret is assigned to a NEXT_PUBLIC_ environment variable, which Next.js inlines into the client bundle at build time. Anyone who opens browser developer tools obtains this key. Rotate it immediately, remove it from client-delivered code, and move privileged operations to a server-side route.".to_string();

    let mut auth_edge_jwt = finding(
        "SUPABASE-AUTH-EDGE-JWT-1",
        EDGE,
        "supabase/config.toml",
        22,
        "high",
    );
    auth_edge_jwt.effort = Some("medium".to_string());
    auth_edge_jwt.confidence = Some("needs-review".to_string());
    auth_edge_jwt.snippet = "[functions.charge-membership]\nverify_jwt = false".to_string();
    auth_edge_jwt.detail = "The charge-membership edge function has JWT verification disabled: any anonymous caller on the internet can invoke the payment path. Its body does not perform its own authorization check either. Re-enable verify_jwt, or add an explicit authenticated-user check inside the handler before any privileged work.".to_string();

    let mut storage_public = finding(
        "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
        PORTAL,
        "supabase/migrations/20240412_storage.sql",
        3,
        "high",
    );
    // Judgment call: effort is "low" (not "medium") here — flipping `public = true` to
    // `false` plus adding an owner-scoped storage policy is a same-day, scoped fix. This
    // puts all 3 do-now items (this bucket + the two RLS/key criticals) in the "If you only
    // do three things this week" box together, matching the blast-radius sentence below,
    // which draws on all three.
    storage_public.effort = Some("low".to_string());
    storage_public.confidence = Some("high".to_string());
    storage_public.snippet = "insert into storage.buckets (id, name, public)\nvalues ('member-documents', 'member-documents', true);".to_string();
    storage_public.detail = "The member-documents storage bucket is public: every uploaded document is downloadable by anyone with, or guessing, the URL. No authentication and no access policy gate object retrieval. If this bucket holds private member documents, make it private and add owner-scoped access policies.".to_string();

    let mut rls_permissive = finding(
        "SUPABASE-RLS-PERMISSIVE-TRUE-1",
        PORTAL,
        "supabase/migrations/20240520_policies.sql",
        31,
        "medium",
    );
    rls_permissive.effort = Some("low".to_string());
    rls_permissive.confidence = Some("needs-review".to_string());
    rls_permissive.snippet = "create policy \"messages are readable\"\n  on public.messages for select\n  using (true);".to_string();
    rls_permissive.detail = "The messages table's read policy grants access to everyone: using (true) lets every anon and authenticated caller read every row. If messages are meant to be private between members, scope the policy to the owning user, for example using (auth.uid() = sender_id).".to_string();

    // Long, real-looking path (a Next.js route group directory) — exercises the M8 path-wrap
    // hardening in situ rather than just in a unit test.
    let mut auth_getsession = finding(
        "SUPABASE-AUTH-GETSESSION-SERVER-1",
        PORTAL,
        "apps/web/app/(dashboard)/settings/billing/components/UpdatePaymentMethodForm.tsx",
        112,
        "high",
    );
    auth_getsession.effort = Some("medium".to_string());
    auth_getsession.confidence = Some("high".to_string());
    auth_getsession.snippet = "const { data: { session } } = await supabase.auth.getSession()".to_string();
    auth_getsession.detail = "The billing settings page trusts an unverified session cookie: a forged cookie could impersonate any user at this layer. Server-side code reads getSession(), which returns the client's unverified cookie JWT rather than validating it against the auth server.".to_string();

    // ── 3 findings dispositioned FalsePositive (excluded, counted once in methodology) ──
    let fp_sql = finding("SEC-NO-RAW-SQL-CONCAT-1", PORTAL, "scripts/report.rs", 40, "critical");
    let fp_secret = finding("SEC-NO-HARDCODED-SECRETS-1", EDGE, "supabase/functions/_shared/config.ts", 5, "critical");
    let fp_url = finding("ARCH-NO-SECRETS-IN-URL-1", PORTAL, "apps/web/lib/analytics.ts", 18, "critical");

    // ── 1 dependency finding (carved into its own §7 lane, not counted in curated_total) ──
    let mut dep_next = finding(
        DEP_AUDIT_RULE_ID,
        PORTAL,
        "package-lock.json",
        0,
        "medium",
    );
    dep_next.snippet = "next@14.1.0".to_string();
    dep_next.detail = "Next.js SSRF via image optimization (GHSA-fr5h-rqp8-mj6g). Fixed in 14.1.1.".to_string();

    let all_findings = vec![
        rls_enabled.clone(),
        key_service_role.clone(),
        auth_edge_jwt,
        storage_public,
        rls_permissive.clone(),
        auth_getsession.clone(),
        fp_sql,
        fp_secret,
        fp_url,
        dep_next,
    ];
    // M1 (demo honesty): every count on the page is DERIVED from this same array by the real
    // serializer — 10 reviewed, 3 excluded as false positives, 6 real code findings + 1
    // dependency advisory make up the remaining 7. Nothing is hand-typed independently.
    assert_eq!(all_findings.len(), 10);

    // ── Dispositions: one Ignored (NOT client-confirmed), rest left Unresolved (the FPs are ──
    // keyed here too). Item 4 (never fabricate a client disposition): this fixture represents
    // a realistic PRE-ENGAGEMENT external scan of an OSS-style repo — there is no client yet
    // to have confirmed anything, so `confirmed_by_client` is `false` on every entry (the
    // false-positive dispositions are the auditor's own triage call and don't route through
    // this flag at all; it only shapes the `Ignored` rendering).
    let mut dispositions: HashMap<String, DispositionWire> = HashMap::new();
    dispositions.insert(
        report_export::finding_key(&fn_by_rule(&all_findings, "SEC-NO-RAW-SQL-CONCAT-1")),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "internal reporting script, not a user-facing query path".to_string(),
            bucket: String::new(),
            confirmed_by_client: false,
        },
    );
    dispositions.insert(
        report_export::finding_key(&fn_by_rule(&all_findings, "SEC-NO-HARDCODED-SECRETS-1")),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "test fixture constant, not a live credential".to_string(),
            bucket: String::new(),
            confirmed_by_client: false,
        },
    );
    dispositions.insert(
        report_export::finding_key(&fn_by_rule(&all_findings, "ARCH-NO-SECRETS-IN-URL-1")),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the token in the URL is a public, non-secret analytics write-key".to_string(),
            bucket: String::new(),
            confirmed_by_client: false,
        },
    );
    dispositions.insert(
        report_export::finding_key(&auth_getsession),
        DispositionWire {
            state: "Ignored".to_string(),
            // The auditor's OWN proposed rationale — never invented client dialogue ("team
            // confirmed..."). No client conversation has happened yet, so
            // `confirmed_by_client: false` renders this as "Needs client confirmation
            // (auditor's proposed rationale: ...)", not as an accepted disposition.
            reason: "this looks like a defense-in-depth redirect only; primary authorization \
                     appears to run through RLS at the data layer, but that has not been \
                     confirmed with the client yet"
                .to_string(),
            bucket: String::new(),
            confirmed_by_client: false,
        },
    );

    // ── What's healthy: 22 audited rules, 6 have real findings -> 16 healthy, exercising ──
    // the S6 cap (10 shown + "6 further rules verified clean").
    let audited_rule_ids = vec![
        "SUPABASE-RLS-ENABLED-1",
        "SUPABASE-RLS-PERMISSIVE-TRUE-1",
        "SUPABASE-RLS-NO-POLICY-1",
        "SUPABASE-RLS-POLICY-DISABLED-1",
        "SUPABASE-RLS-USER-METADATA-1",
        "SUPABASE-RLS-VIEW-INVOKER-1",
        "SUPABASE-RLS-INITPLAN-1",
        "SUPABASE-AUTH-EDGE-JWT-1",
        "SUPABASE-AUTH-GETSESSION-SERVER-1",
        "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1",
        "SUPABASE-AUTH-USERS-EXPOSED-1",
        "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
        "SEC-NO-SECRET-FILE-1",
        "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
        "SUPABASE-STORAGE-OBJECT-POLICY-1",
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-RAW-SQL-CONCAT-1",
        "ARCH-NO-SECRETS-IN-URL-1",
        "SEC-NO-PRIVATE-KEY-1",
        "SEC-NO-DISABLED-TLS-1",
        "SEC-NO-UNSAFE-DESERIALIZATION-1",
        "SUPABASE-FUNC-SEARCH-PATH-1",
    ];
    assert_eq!(audited_rule_ids.len(), 22);

    let report = ScanReport {
        repos: vec![PORTAL.to_string(), EDGE.to_string()],
        stacks: Vec::new(),
        files_scanned: 214,
        test_file_count: 0,
        files_excluded: 1188,
        code_chars: 486_203,
        excluded_mechanical_rules: Vec::new(),
        findings: all_findings,
        proposed_rules: Vec::new(),
        gated: false,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: vec![
            CoverageNote {
                tool: "osv-scanner".to_string(),
                message: "Dependency scan covers the npm ecosystem via package-lock.json. No manifest was found for other ecosystems.".to_string(),
            },
            CoverageNote {
                tool: "osv-scanner".to_string(),
                message: "Advisory data reflects the scan timestamp; re-run before remediation sign-off.".to_string(),
            },
        ],
        provenance: ScanProvenance {
            audited_refs: vec![
                AuditedRef {
                    repo: PORTAL.to_string(),
                    sha: Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".to_string()),
                    branch: Some("main".to_string()),
                    dirty: false,
                },
                AuditedRef {
                    repo: EDGE.to_string(),
                    sha: Some("9f8e7d6c5b4a39281706f5e4d3c2b1a098765432".to_string()),
                    branch: Some("main".to_string()),
                    dirty: true,
                },
            ],
            audit_model: Some("anthropic/claude-opus-4.8".to_string()),
            calibration_model: Some("anthropic/claude-sonnet-4.6".to_string()),
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "sample-fixture".to_string(),
            audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
            camerata_version: "0.4.0".to_string(),
            osv_scanner_version: Some("1.9.0".to_string()),
            started_at: "2026-07-26T15:30:00Z".to_string(),
            finished_at: "2026-07-26T15:42:00Z".to_string(),
        },
    };

    // ═══ Load the REAL bundled rule corpus so citations/domains/titles resolve for real ═══
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(corpus_errors.is_empty(), "corpus must load cleanly: {corpus_errors:?}");

    // No `executive_summary_override` — the narrative shown is `default_narrative`'s REAL
    // output, not hand-written prose (this is the S7 self-consistency fix: the old sample
    // had `is_override: false` while showing hand-written narrative text).
    let opts = ReportOptions {
        client_name: "Harbor Community Foundation".to_string(),
        project_title: "Harbor Member Portal".to_string(),
        prepared_by: "Zachary Ernst".to_string(),
        executive_summary_override: None,
    };

    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // Sanity: the reconciliation M1 was fixing.
    assert_eq!(json.methodology.candidates_reviewed, 10);
    assert_eq!(json.methodology.excluded_false_positive, 3);
    assert_eq!(json.executive_summary.curated_total, 6);
    assert_eq!(json.dependency_snapshot.rows.len(), 1);
    assert!(!json.executive_summary.is_override);

    // ═══ Write sample-report/data.json (pretty, reproducible byte-for-byte from this test) ═══
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample-report");
    let data_json = serde_json::to_string_pretty(&json).expect("serialize AuditReportJson");
    std::fs::write(repo_root.join("data.json"), data_json + "\n").expect("write sample-report/data.json");

    // ═══ Build the REAL Excel workbook (the xlsx sibling of the PDF, same data pass) ═══
    let xlsx = xlsx_export::build_workbook(&report, &dispositions, Some(&corpus), &opts)
        .expect("build_workbook must succeed for the sample fixture");
    assert_eq!(&xlsx[0..2], b"PK", "the workbook must be a valid xlsx (zip) container");
    std::fs::write(repo_root.join("camerata-sample-audit-findings.xlsx"), &xlsx)
        .expect("write sample-report/camerata-sample-audit-findings.xlsx");
    // Sanity-check the xlsx structure: it must open as a zip and carry a workbook.xml with
    // the sheets the product export promises (never just "non-empty bytes").
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx.clone()))
            .expect("the generated xlsx must be a valid zip archive");
        let mut workbook_xml = String::new();
        {
            use std::io::Read;
            archive
                .by_name("xl/workbook.xml")
                .expect("xlsx must contain xl/workbook.xml")
                .read_to_string(&mut workbook_xml)
                .expect("xl/workbook.xml must be readable UTF-8");
        }
        for expected in [
            "name=\"Index\"",
            "name=\"All Findings\"",
            "name=\"Dependencies\"",
            "name=\"False Positives\"",
            "name=\"Coverage\"",
        ] {
            assert!(
                workbook_xml.contains(expected),
                "sample workbook missing expected sheet {expected}: {workbook_xml}"
            );
        }
    }
    eprintln!(
        "regenerate_sample_report: wrote {} bytes to sample-report/camerata-sample-audit-findings.xlsx",
        xlsx.len()
    );

    // ═══ Build + write the REAL findings.json (machine-readable sibling of the xlsx) ═══
    // Same `report`/`dispositions`/`corpus` inputs as the xlsx above, plus the ALREADY-BUILT
    // `json` (`AuditReportJson`) for provenance/summary reuse — see
    // `xlsx_export::build_findings_export`'s doc comment. Both a standalone file (for Zach
    // to eyeball) and the zip entry below come from this one call.
    let findings_export =
        xlsx_export::build_findings_export(&report, &dispositions, Some(&corpus), &json);
    assert_eq!(
        findings_export.findings.len(),
        6,
        "findings.json must carry exactly the 6 non-FP code findings (matches curated_total)"
    );
    let findings_json_bytes =
        serde_json::to_vec_pretty(&findings_export).expect("serialize findings.json");
    std::fs::write(
        repo_root.join("camerata-sample-audit-findings.json"),
        &findings_json_bytes,
    )
    .expect("write sample-report/camerata-sample-audit-findings.json");
    eprintln!(
        "regenerate_sample_report: wrote {} bytes to \
         sample-report/camerata-sample-audit-findings.json",
        findings_json_bytes.len()
    );

    // ═══ Compile the REAL embedded template against it and write the PDF ═══
    if !typst_on_path() {
        eprintln!(
            "regenerate_sample_report: typst not on PATH, wrote data.json + the xlsx but \
             skipped the PDF compile and the zip"
        );
        return;
    }
    let pdf = report_export::compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed for the sample fixture");
    std::fs::write(repo_root.join("camerata-sample-audit.pdf"), &pdf)
        .expect("write sample-report/camerata-sample-audit.pdf");
    eprintln!(
        "regenerate_sample_report: wrote {} bytes to sample-report/camerata-sample-audit.pdf",
        pdf.len()
    );

    // ═══ Zip PDF + xlsx + findings.json + README.txt — mirrors ═══
    // POST /api/projects/:id/product-export's assembly (that handler's own zip-building
    // helpers are private to camerata-server, so this inlines the same four-entry Deflate
    // zip directly over the REAL pdf/xlsx/json bytes built above; nothing here re-derives
    // report content).
    let readme = format!(
        "Camerata Audit — Product Export (sample)\n\
         =========================================\n\
         \n\
         This ZIP contains three artifacts derived from the SAME audit scan:\n\
         \n\
         camerata-sample-audit.pdf\n\
         \x20 The curated NARRATIVE report — cover, executive summary, category scorecard,\n\
         \x20 severity x effort matrix, curated findings with citations and recommended\n\
         \x20 fixes, what's healthy, dependency snapshot, and methodology.\n\
         \n\
         camerata-sample-audit-findings.xlsx\n\
         \x20 The COMPLETE working dataset — every finding as its own row, a per-category\n\
         \x20 sheet, a Dependencies sheet, a Coverage sheet, and a False Positives sheet\n\
         \x20 with the auditor's exclusion reasons.\n\
         \n\
         findings.json\n\
         \x20 The MACHINE-READABLE version of the xlsx's \"All Findings\" data, wrapped with\n\
         \x20 the report's provenance and summary counts.\n\
         \n\
         Repos audited: {}\n\
         Generated: {}\n\
         \n\
         {}\n",
        json.cover.repos.join(", "),
        json.cover.generated_at,
        report_export::AUDIT_REPORT_DISCLAIMER,
    );
    let zip_bytes = {
        use std::io::Write;
        let buf = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("camerata-sample-audit.pdf", options).expect("zip entry: pdf");
        writer.write_all(&pdf).expect("zip write: pdf");
        writer
            .start_file("camerata-sample-audit-findings.xlsx", options)
            .expect("zip entry: xlsx");
        writer.write_all(&xlsx).expect("zip write: xlsx");
        writer.start_file("findings.json", options).expect("zip entry: findings.json");
        writer.write_all(&findings_json_bytes).expect("zip write: findings.json");
        writer.start_file("README.txt", options).expect("zip entry: readme");
        writer.write_all(readme.as_bytes()).expect("zip write: readme");
        writer.finish().expect("finish zip").into_inner()
    };
    assert_eq!(&zip_bytes[0..2], b"PK", "the product-export zip itself must be a valid zip");
    std::fs::write(repo_root.join("camerata-sample-audit.zip"), &zip_bytes)
        .expect("write sample-report/camerata-sample-audit.zip");
    eprintln!(
        "regenerate_sample_report: wrote {} bytes to sample-report/camerata-sample-audit.zip",
        zip_bytes.len()
    );
}

fn fn_by_rule(findings: &[Finding], rule_id: &str) -> Finding {
    findings
        .iter()
        .find(|f| f.rule_id == rule_id)
        .unwrap_or_else(|| panic!("fixture must contain a finding for {rule_id}"))
        .clone()
}

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}
