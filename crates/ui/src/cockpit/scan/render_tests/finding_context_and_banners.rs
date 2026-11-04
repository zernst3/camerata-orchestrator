//! Render tests: finding context and banners.

use super::*;

// ── Finding code context: where the block came from (client source lifecycle) ──────────

fn render_context(body: serde_json::Value) -> String {
    #[derive(Clone, PartialEq, Props)]
    struct Case {
        view: FindingContextView,
    }
    fn harness(case: Case) -> Element {
        rsx! { FindingContextBlock { view: case.view } }
    }
    let view: FindingContextView = serde_json::from_value(body).expect("valid context body");
    let mut vdom = VirtualDom::new_with_props(harness, Case { view });
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

#[test]
fn finding_pane_renders_source_deleted_and_newer_source_notes() {
    // Source deleted: the note replaces the code and points at the Workspace tab.
    let deleted = render_context(serde_json::json!({
        "status": "source_deleted",
        "reason": "Source deleted on 24 Oct 2026, re-fetch to view",
        "deleted_at": "2026-10-24T17:40:19Z",
        "record_id": "del-p1-acme-app-20261024T174019Z",
    }));
    assert!(
        deleted.contains("Source deleted on 24 Oct 2026, re-fetch to view"),
        "{deleted}"
    );
    assert!(deleted.contains("Open Workspace"));
    assert!(!deleted.contains("finding-ctx-pre"), "no code is shown");

    // Read from the inspected commit while the checkout has moved on: code plus a muted note.
    let newer = render_context(serde_json::json!({
        "status": "ok",
        "source": "git",
        "commit": "3f2a9c14e0b7aaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "head_differs": true,
        "lines": ["fn a() {", "    bad();", "}"],
        "start_line": 10,
        "violation_line": 11,
        "kind": "function",
        "matches_snippet": true,
    }));
    assert!(newer.contains("finding-ctx-pre"), "{newer}");
    assert!(newer.contains("bad();"));
    assert!(
        newer.contains("References commit 3f2a9c1; source on disk is newer"),
        "{newer}"
    );

    // At the inspected commit: no note at all.
    let same = render_context(serde_json::json!({
        "status": "ok",
        "source": "git",
        "commit": "3f2a9c14e0b7aaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "head_differs": false,
        "lines": ["fn a() {", "}"],
        "start_line": 1,
        "violation_line": 1,
        "kind": "function",
        "matches_snippet": true,
    }));
    assert!(!same.contains("source on disk is newer"), "{same}");
}

fn deleted_block() -> camerata_ui_core::readiness::ScanBlock {
    camerata_ui_core::readiness::ScanBlock {
        repos: vec![("acme/app".to_string(), "2026-10-24T17:40:19Z".to_string())],
    }
}

/// The opening tag of the first button whose text is `label`.
fn button_tag_labelled(html: &str, label: &str) -> String {
    let at = html
        .find(label)
        .unwrap_or_else(|| panic!("{label:?} not rendered: {html}"));
    let start = html[..at].rfind("<button").expect("inside a button");
    let end = html[start..].find('>').expect("tag closes") + start;
    html[start..=end].to_string()
}

#[test]
fn scan_repos_button_is_off_for_a_deleted_source_and_says_why() {
    #[derive(Clone, PartialEq, Props)]
    struct Case {
        blocked: Option<camerata_ui_core::readiness::ScanBlock>,
    }
    fn harness(case: Case) -> Element {
        rsx! {
            ScanReposButton {
                repos: vec!["acme/app".to_string()],
                scanning: false,
                blocked: case.blocked,
                on_scan: move |_| {},
            }
        }
    }
    let render = |blocked| {
        let mut vdom = VirtualDom::new_with_props(harness, Case { blocked });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    };
    let blocked = render(Some(deleted_block()));
    assert!(
        button_tag_labelled(&blocked, "Scan repos").contains("disabled"),
        "{blocked}"
    );
    assert!(blocked.contains(
            "Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to rescan."
        ));
    let open = render(None);
    assert!(
        !button_tag_labelled(&open, "Scan repos").contains("disabled"),
        "{open}"
    );
    assert!(!open.contains("was deleted"));
}

#[test]
fn rescan_sticky_bar_is_disabled_with_the_reason_when_a_source_is_deleted() {
    fn harness() -> Element {
        let block = use_memo(|| Some(deleted_block()));
        use_context_provider(|| block);
        rsx! {
            RescanStickyBar { pending_count: 2usize, rescanning: false, on_rescan: move |_| {} }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    // The label also appears in the bar's caption; the button is the `btn-run` one.
    let button = button_tag_labelled(&html, "Rescan 2 overridden rules</button>");
    assert!(
        button.contains("btn-run") && button.contains("disabled"),
        "{html}"
    );
    assert!(html.contains("was deleted on 24 Oct 2026"));
}

#[test]
fn onboard_view_renders_findings_with_deleted_source_and_disabled_rescan_controls() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<String>));
        use_context_provider(crate::loading::LoadingCount::default);
        use_context_provider(|| Signal::new(CockpitView::Onboard));
        // The cockpit's shared "which sources were deleted" memo.
        let block = use_memo(|| Some(deleted_block()));
        use_context_provider(|| block);
        let report: ScanReportView = serde_json::from_value(serde_json::json!({
            "repos": ["acme/app"],
            "files_scanned": 3,
            "findings": [{
                "repo": "acme/app", "path": "src/db.rs", "line": 9,
                "rule_id": "SQL-INJECTION-1", "severity": "high",
                "snippet": "db.query(format!(..))", "detail": "interpolated SQL",
            }],
            "proposed_rules": [],
            "gated": false,
        }))
        .expect("valid report");
        use_context_provider(|| Signal::new(Some(report)));
        rsx! { OnboardView { connection: None } }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    // The review surface is still there: a deleted source does not hide it (the report,
    // its rules table and the audit panel render), it only switches the source-needing
    // controls off.
    assert!(
        html.contains("Step 2 — audit the code against your selected rules"),
        "{html}"
    );
    assert!(html.contains("findings-toolbar"));
    // Every control that needs the source says why and is off.
    assert!(html.contains("Source for acme/app was deleted on 24 Oct 2026."));
    assert!(
        button_tag_labelled(&html, "Audit code against selected rules").contains("disabled"),
        "audit"
    );
    assert!(
        button_tag_labelled(&html, "Add rules to repo(s) (branch + push)").contains("disabled"),
        "apply"
    );
    assert!(
        button_tag_labelled(&html, "Open governance PR").contains("disabled"),
        "open PR"
    );
}

#[test]
fn finding_pane_renders_the_commit_unavailable_banner_with_the_working_tree() {
    let html = render_context(serde_json::json!({
        "status": "commit_unavailable",
        "reason": "Inspected commit not available in this clone; showing the working tree",
        "source": "working_tree",
        "lines": ["fn a() {", "    bad();", "}"],
        "start_line": 1,
        "violation_line": 2,
        "kind": "function",
        "matches_snippet": false,
    }));
    assert!(html.contains("finding-ctx-banner"), "{html}");
    assert!(html.contains("Inspected commit not available in this clone; showing the working tree"));
    assert!(
        html.contains("bad();"),
        "the working tree block is still shown"
    );

    // When even the working tree has nothing, it degrades to the single reason line.
    let empty = render_context(serde_json::json!({
        "status": "commit_unavailable",
        "reason": "Inspected commit not available in this clone; showing the working tree",
        "source": "working_tree",
    }));
    assert!(!empty.contains("finding-ctx-pre"), "{empty}");
    assert!(empty.contains("Inspected commit not available"));
}

#[test]
fn the_existing_degraded_reasons_still_render_one_line() {
    let html = render_context(serde_json::json!({
        "status": "file_missing",
        "reason": "file not found — changed since scan?",
    }));
    assert!(html.contains("finding-ctx-degraded"));
    assert!(html.contains("file not found"));
}

// ── Tier-1 render: DeterministicProgress (props-only, no context) ──────────
// Mirrors the live_run.rs render-test pattern: a harness root mounts the component
// inside the VirtualDom runtime so its hooks run, then dioxus_ssr renders static HTML.
fn det_progress_harness() -> Element {
    let progress: DetProgressView = serde_json::from_value(serde_json::json!({
        "tools": [
            { "tool": "floor", "status": "done", "findings": 3 },
            { "tool": "clippy", "status": "running", "findings": 0 }
        ],
        "done": 1,
        "total": 2
    }))
    .expect("valid DetProgressView fixture");
    rsx! {
        DeterministicProgress { progress }
    }
}

#[test]
fn deterministic_progress_renders_tools_and_count() {
    let mut vdom = VirtualDom::new(det_progress_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("det-progress"),
        "root container; html=\n{html}"
    );
    assert!(html.contains("Deterministic scan"), "title; html=\n{html}");
    assert!(
        html.contains("1/2 tools"),
        "done/total count; html=\n{html}"
    );
    // det_tool_label maps "floor" → "Security floor"; "clippy" passes through.
    assert!(
        html.contains("Security floor"),
        "floor label; html=\n{html}"
    );
    assert!(html.contains("clippy"), "clippy label; html=\n{html}");
    // The done tool shows its findings count; the running tool shows its status.
    assert!(
        html.contains("3 finding(s)"),
        "done tool findings; html=\n{html}"
    );
    assert!(
        html.contains("running"),
        "running tool status; html=\n{html}"
    );
}

// ── Tier-1 render: GreenfieldResultView, success branch (props-only) ───────
fn gf_ok_harness() -> Element {
    let result: GreenfieldScaffoldResult = serde_json::from_value(serde_json::json!({
        "ok": true,
        "path": "/dest/my-proj",
        "files_written": ["CONVENTIONS.md", "AGENTS.md"],
        "commit_sha": "abc123",
        "message": "Repo created"
    }))
    .expect("valid GreenfieldScaffoldResult fixture");
    rsx! {
        GreenfieldResultView { result }
    }
}

#[test]
fn greenfield_result_ok_renders_files_and_sha() {
    let mut vdom = VirtualDom::new(gf_ok_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("gf-result-ok"),
        "success container class; html=\n{html}"
    );
    assert!(
        html.contains("Repo scaffolded"),
        "success heading; html=\n{html}"
    );
    assert!(html.contains("CONVENTIONS.md"), "first file; html=\n{html}");
    assert!(html.contains("AGENTS.md"), "second file; html=\n{html}");
    assert!(html.contains("abc123"), "commit sha; html=\n{html}");
    assert!(
        html.contains("/dest/my-proj"),
        "location path; html=\n{html}"
    );
}

// ── Tier-1 render: GreenfieldResultView, error branch (props-only) ─────────
fn gf_err_harness() -> Element {
    let result: GreenfieldScaffoldResult = serde_json::from_value(serde_json::json!({
        "ok": false,
        "message": "directory already exists"
    }))
    .expect("valid GreenfieldScaffoldResult fixture");
    rsx! {
        GreenfieldResultView { result }
    }
}

#[test]
fn greenfield_result_err_renders_failure_message() {
    let mut vdom = VirtualDom::new(gf_err_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("gf-result-err"),
        "error container class; html=\n{html}"
    );
    assert!(
        html.contains("Scaffold failed"),
        "error heading; html=\n{html}"
    );
    assert!(
        html.contains("directory already exists"),
        "error message; html=\n{html}"
    );
}

// ── Tier-1 render: DeepReportExportPanel (needs the toast context) ─────────
// It calls use_context::<Signal<Vec<Toast>>>(), so the harness MUST provide it.
// The report_md signal starts None, so we assert the static button + hint structure
// (no modal until the async fetch resolves, which never runs under SSR).
fn deep_export_harness_soc2_on() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    rsx! {
        DeepReportExportPanel { project_id: "proj-1".to_string(), soc2_enabled: true }
    }
}

#[test]
fn deep_export_panel_renders_button_and_soc2_on_hint() {
    let mut vdom = VirtualDom::new(deep_export_harness_soc2_on);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("deep-export-panel"),
        "panel container; html=\n{html}"
    );
    assert!(
        html.contains("Export deep compliance report"),
        "section label; html=\n{html}"
    );
    // soc2_enabled = true → the "also included" hint, not the "disabled" one.
    assert!(
        html.contains("SOC-2 gap analysis is also included"),
        "soc2-on hint; html=\n{html}"
    );
    // The CTA label (loading() is false initially).
    assert!(
        html.contains("Export deep report"),
        "export button label; html=\n{html}"
    );
    // No modal overlay until a report loads.
    assert!(
        !html.contains("deep-export-modal-overlay"),
        "no modal before fetch; html=\n{html}"
    );
}

fn deep_export_harness_soc2_off() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    rsx! {
        DeepReportExportPanel { project_id: "proj-1".to_string(), soc2_enabled: false }
    }
}

#[test]
fn deep_export_panel_renders_soc2_off_hint() {
    let mut vdom = VirtualDom::new(deep_export_harness_soc2_off);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("SOC-2 gap analysis is disabled for this workspace"),
        "soc2-off hint; html=\n{html}"
    );
}

// ── Tier-1 render: AiReviewBlockedBanner (compliance-gate fix) ──────────────────────────
// Regression for the silent-empty-audit bug: a `Blocked` compliance gate or a hard AI-pass
// failure must never render as a clean, empty findings table. These SSR renders (no network
// resources needed — `AiReviewBlockedBanner` takes plain `Option<String>` props) prove the
// banner text is actually in the DOM, not just present on the underlying data.

fn ai_blocked_banner_harness() -> Element {
    rsx! {
        AiReviewBlockedBanner {
            ai_blocked_reason: Some(
                "This project is API-only (CLI disabled). Add an Anthropic API key to run."
                    .to_string(),
            ),
            ai_error: None,
        }
    }
}

#[test]
fn ai_review_blocked_banner_renders_reason_and_remediation() {
    let mut vdom = VirtualDom::new(ai_blocked_banner_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("AI review did not run"),
        "heading; html=\n{html}"
    );
    assert!(
        html.contains("This project is API-only (CLI disabled). Add an Anthropic API key to run."),
        "the exact block reason must be visible verbatim; html=\n{html}"
    );
    assert!(
        html.contains("Add an Anthropic API key in Settings"),
        "actionable remediation hint; html=\n{html}"
    );
    assert!(
        !html.contains("AI review failed"),
        "must not ALSO show the error heading; html=\n{html}"
    );
}

fn ai_error_banner_harness() -> Element {
    rsx! {
        AiReviewBlockedBanner {
            ai_blocked_reason: None,
            ai_error: Some("me/api: AI audit skipped (simulated LLM unavailable (complete))".to_string()),
        }
    }
}

#[test]
fn ai_review_error_banner_renders_real_error_text() {
    let mut vdom = VirtualDom::new(ai_error_banner_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("AI review failed"), "heading; html=\n{html}");
    assert!(
            html.contains("simulated LLM unavailable"),
            "the real underlying error text must be visible verbatim, not a generic message; html=\n{html}"
        );
}

fn ai_banner_neither_set_harness() -> Element {
    rsx! {
        AiReviewBlockedBanner { ai_blocked_reason: None, ai_error: None }
    }
}

#[test]
fn ai_review_banner_renders_nothing_on_the_happy_path() {
    // Neither field set (the ordinary case — the AI review ran and produced findings, or
    // wasn't requested at all) — the banner must be entirely absent, never an empty box.
    let mut vdom = VirtualDom::new(ai_banner_neither_set_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("onboard-gate"),
        "no banner markup at all on the happy path; html=\n{html}"
    );
}

// ── The INCOMPLETE banner: a scan with passes that did not run must never look complete ──

fn failed_pass(pass: &str, reason: &str) -> FailedPassView {
    FailedPassView {
        repo: "acme/api".to_string(),
        pass: pass.to_string(),
        reason: reason.to_string(),
        ..FailedPassView::default()
    }
}

fn incomplete_harness() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![
                failed_pass(
                    "scan pass pass[all] chunk 2/3",
                    "NOT RUN: the spend limit set for this run was reached before rule-batch 2 of 4 ran",
                ),
                failed_pass("calibration (verification and hour estimates)", "NOT RUN (in part): the spend limit set for this run was reached"),
            ],
            compact: false,
        }
    }
}

#[test]
fn the_incomplete_banner_names_the_count_the_cause_and_every_pass() {
    let mut vdom = VirtualDom::new(incomplete_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Incomplete: 2 pass(es) or stage(s) did not run"),
        "{html}"
    );
    assert!(html.contains("Stopped by the spend limit"), "{html}");
    assert!(html.contains("scan pass pass[all] chunk 2/3"), "{html}");
    assert!(
        html.contains("calibration (verification and hour estimates)"),
        "{html}"
    );
    assert!(html.contains("NOT RUN"), "{html}");
    assert!(
        html.contains("does NOT mean the missing parts are clean"),
        "{html}"
    );
}

fn incomplete_compact_harness() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![failed_pass("hour estimation", "no estimate returned")],
            compact: true,
        }
    }
}

#[test]
fn the_compact_form_beside_export_says_the_export_will_carry_it_too() {
    let mut vdom = VirtualDom::new(incomplete_compact_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Incomplete: 1 pass(es) or stage(s) did not run"),
        "{html}"
    );
    assert!(html.contains("The export will say so"), "{html}");
    // A failed pass of ANY cause shows, not only the spend limit.
    assert!(!html.contains("Stopped by the spend limit"), "{html}");
}

fn failure_detail_harness() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "acme/api".to_string(),
                pass: "audit pass[all] 1/2".to_string(),
                reason: "Anthropic API HTTP 529: overloaded".to_string(),
                class: "transport".to_string(),
                files: vec!["src/a.rs".to_string()],
                attempts: vec![AttemptView {
                    at: "2026-10-28T10:00:00Z".to_string(),
                    backend: "api".to_string(),
                    model: "claude-x".to_string(),
                    http_status: Some(529),
                    error_type: Some("overloaded_error".to_string()),
                    request_id: Some("req_9".to_string()),
                    retry_after_secs: Some(30),
                    wait_secs: Some(5),
                    message: "Overloaded".to_string(),
                    ..AttemptView::default()
                }],
                scope_note: false,
            }],
            compact: false,
        }
    }
}

#[test]
fn the_failure_panel_lists_class_repo_part_files_and_every_attempt_field() {
    let mut vdom = VirtualDom::new(failure_detail_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    for needle in [
        "Class: transport",
        "acme/api",
        "audit pass[all] 1/2",
        "src/a.rs",
        "HTTP 529",
        "overloaded_error",
        "request req_9",
        "retry-after 30s",
        "waited 5s",
        "claude-x",
        "Copy as JSON",
    ] {
        assert!(html.contains(needle), "{needle} missing in {html}");
    }
    // One banner, not two.
    assert_eq!(html.matches("scan-incomplete-banner").count(), 1, "{html}");
}

fn rejected_replies_harness() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "acme/api".to_string(),
                pass: "fix-specific generation".to_string(),
                reason: "gave up".to_string(),
                class: "content".to_string(),
                attempts: vec![AttemptView {
                    at: "2026-10-30T10:00:00Z".to_string(),
                    message: "no usable fix was returned".to_string(),
                    rejections: vec![
                        "src/db.ts:2 ARCH-1: named `rotateKey`, which is not in the finding's evidence"
                            .to_string(),
                    ],
                    ..AttemptView::default()
                }],
                ..FailedPassView::default()
            }],
            compact: false,
        }
    }
}

#[test]
fn the_failure_panel_shows_why_each_reply_was_turned_down() {
    let mut vdom = VirtualDom::new(rejected_replies_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Turned down: src/db.ts:2 ARCH-1"), "{html}");
    assert!(html.contains("rotateKey"), "{html}");
    // A report written before the field existed still reads.
    let old: AttemptView = serde_json::from_str(r#"{"at":"t","message":"m"}"#).unwrap();
    assert!(old.rejections.is_empty());
    let new: AttemptView = serde_json::from_str(r#"{"at":"t","rejections":["a","b"]}"#).unwrap();
    assert_eq!(new.rejections, vec!["a".to_string(), "b".to_string()]);
}

fn failure_with_run_harness() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "acme/api".to_string(),
                pass: "fix-specific generation".to_string(),
                reason: "overloaded".to_string(),
                class: "transport".to_string(),
                ..FailedPassView::default()
            }],
            compact: false,
            run_id: Some("20261029T101500Z-ab12cd".to_string()),
        }
    }
}

fn failure_with_plan_harness() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "acme/api".to_string(),
                pass: "fix-specific generation".to_string(),
                ..FailedPassView::default()
            }],
            compact: false,
            run_id: Some("r1".to_string()),
            plan: Some(ResumePlanView {
                text: "Resume: what remains\n  - acme/api: fix text (failed)\n".to_string(),
                refusal: None,
            }),
        }
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "acme/api".to_string(),
                pass: "p".to_string(),
                ..FailedPassView::default()
            }],
            compact: false,
            run_id: Some("r2".to_string()),
            plan: Some(ResumePlanView {
                text: String::new(),
                refusal: Some("repository acme/api is at commit 1234567890 now".to_string()),
            }),
        }
    }
}

#[test]
fn run_what_remains_is_off_for_a_scan_that_was_not_saved_and_live_for_one_that_was() {
    let mut vdom = VirtualDom::new(failure_detail_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Run what remains"), "{html}");
    assert!(html.contains("disabled"), "{html}");
    assert!(html.contains("was not saved"), "{html}");

    let mut vdom = VirtualDom::new(failure_with_run_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    let button = html
        .split("scan-run-what-remains")
        .nth(1)
        .expect("the button");
    let button = button.split("</button>").next().unwrap();
    assert!(!button.contains("disabled"), "{button}");
    assert!(
        button.contains("Nothing is sent until you confirm"),
        "{button}"
    );
    assert!(!html.contains("Resume is coming"), "{html}");
}

#[test]
fn the_plan_is_shown_with_its_estimate_before_anything_starts_and_a_refusal_has_no_start() {
    let mut vdom = VirtualDom::new(failure_with_plan_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Resume: what remains"), "{html}");
    assert!(html.contains("Start: run only these parts"), "{html}");
    assert!(
        html.contains("Cannot resume now: repository acme/api is at commit"),
        "{html}"
    );
    // Only the plan without a refusal offers the start button.
    assert_eq!(html.matches("scan-resume-confirm").count(), 1, "{html}");
}

thread_local! {
    static RESUME_COUNT: std::cell::RefCell<Option<crate::loading::LoadingCount>> =
        const { std::cell::RefCell::new(None) };
    static RESUME_LOG: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn resume_harness() -> Element {
    crate::loading::provide_loading_context();
    let count = use_context::<crate::loading::LoadingCount>();
    RESUME_COUNT.with(|c| *c.borrow_mut() = Some(count));
    spawn(async move {
        // The start request and the whole job poll: the Bombe runs through both.
        let _ = run_what_remains(
            count,
            || async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                RESUME_LOG.with(|l| l.borrow_mut().push(*count.peek()));
                Ok("job-1".to_string())
            },
            |_jid| async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                RESUME_LOG.with(|l| l.borrow_mut().push(*count.peek()));
            },
        )
        .await;
    });
    rsx! { div {} }
}

/// "Run what remains" holds the Bombe from the start request to the end of the polling, the
/// way `loading.rs` tests every guard: a real spawned task with real suspensions.
#[tokio::test]
async fn run_what_remains_holds_the_bombe_from_the_start_request_to_the_end_of_the_poll() {
    RESUME_LOG.with(|l| l.borrow_mut().clear());
    let mut vdom = VirtualDom::new(resume_harness);
    vdom.rebuild_in_place();
    let count = RESUME_COUNT.with(|c| c.borrow_mut().take()).unwrap();
    assert_eq!(*count.read(), 0);
    vdom.process_events();
    assert_eq!(*count.read(), 1, "running as the resume starts");
    for _ in 0..4 {
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        vdom.process_events();
    }
    assert_eq!(
        RESUME_LOG.with(|l| l.borrow().clone()),
        vec![1, 1],
        "running at the start request and again during the poll"
    );
    assert_eq!(*count.read(), 0, "released when the polling ended");
}

fn refused_resume_harness() -> Element {
    crate::loading::provide_loading_context();
    let count = use_context::<crate::loading::LoadingCount>();
    RESUME_COUNT.with(|c| *c.borrow_mut() = Some(count));
    spawn(async move {
        let refused: Result<(), String> = run_what_remains(
            count,
            || async { Err("the repo moved on".to_string()) },
            |_jid| async {},
        )
        .await;
        assert!(refused.is_err());
    });
    rsx! { div {} }
}

#[tokio::test]
async fn a_refused_resume_releases_the_bombe() {
    let mut vdom = VirtualDom::new(refused_resume_harness);
    vdom.rebuild_in_place();
    let count = RESUME_COUNT.with(|c| c.borrow_mut().take()).unwrap();
    vdom.process_events();
    assert_eq!(
        *count.read(),
        0,
        "nothing runs, so nothing is left counting"
    );
}

#[test]
fn an_old_report_without_the_new_fields_still_loads() {
    let v: FailedPassView =
        serde_json::from_str(r#"{"repo":"a/b","pass":"p","reason":"r"}"#).unwrap();
    assert!(v.class.is_empty() && v.attempts.is_empty() && v.files.is_empty());
}

fn incomplete_none_harness() -> Element {
    rsx! {
        IncompletePassesBanner { failed_passes: Vec::new(), compact: false }
        IncompletePassesBanner { failed_passes: Vec::new(), compact: true }
    }
}

#[test]
fn a_complete_scan_shows_no_incomplete_banner() {
    let mut vdom = VirtualDom::new(incomplete_none_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("onboard-gate") && !html.contains("Incomplete"),
        "{html}"
    );
}

#[test]
fn the_report_view_keeps_the_failed_passes_the_server_sends() {
    let report: ScanReportView = serde_json::from_value(serde_json::json!({
            "files_scanned": 3, "findings": [], "proposed_rules": [], "gated": false,
            "failed_passes": [
                { "repo": "acme/api", "pass": "scan pass pass[all] chunk 1/1", "reason": "NOT RUN: the spend limit set for this run was reached" }
            ],
            "message": "INCOMPLETE: the spend limit stopped this scan; 1 scan pass(es) and 0 stage(s) did not run"
        }))
        .unwrap();
    assert_eq!(report.failed_passes.len(), 1);
    assert_eq!(
        report.failed_passes[0].pass,
        "scan pass pass[all] chunk 1/1"
    );
    assert!(report.message.as_deref().unwrap().starts_with("INCOMPLETE"));
    // A report from before the field existed still loads, with nothing to show.
    let old: ScanReportView = serde_json::from_value(serde_json::json!({
        "files_scanned": 3, "findings": [], "proposed_rules": [], "gated": false
    }))
    .unwrap();
    assert!(old.failed_passes.is_empty());
}

#[test]
fn progress_never_counts_a_refused_pass_as_done() {
    assert_eq!(
        job_progress_label(3, 6, 2, 0),
        "3/6 passes \u{b7} 2 finding(s) so far"
    );
    let stopped = job_progress_label(3, 6, 2, 3);
    assert!(stopped.starts_with("3/6 passes"), "{stopped}");
    assert!(stopped.contains("3 NOT RUN (spend limit)"), "{stopped}");
    // And the polled job carries the separate count.
    let job: JobStateView = serde_json::from_value(serde_json::json!({
        "status": "running", "done": 3, "total": 6, "not_run": 3
    }))
    .unwrap();
    assert_eq!((job.done, job.not_run), (3, 3));
}

// ── Tier-1 render: ScanJobFailedBanner (background-job silent-death fix) ───────────────
// Regression for "the scan silently dies with no error": a background job that ends
// `failed` (or that the poller loses contact with) must render a loud, visible banner —
// never just a spinner that quietly disappears with nothing else on screen.

fn scan_job_failed_banner_harness() -> Element {
    rsx! {
        ScanJobFailedBanner {
            message: Some(
                "The scan crashed unexpectedly: claude CLI (`claude`) was not found on PATH. \
                 Please retry; if it keeps happening, this is a bug worth reporting."
                    .to_string(),
            ),
        }
    }
}

#[test]
fn scan_job_failed_banner_renders_the_real_failure_text() {
    let mut vdom = VirtualDom::new(scan_job_failed_banner_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Scan failed"), "heading; html=\n{html}");
    assert!(
        html.contains("claude CLI (`claude`) was not found on PATH"),
        "the real underlying failure text must be visible verbatim, not swallowed; html=\n{html}"
    );
    assert!(
        html.contains("onboard-gate"),
        "reuses the same unmissable banner styling; html=\n{html}"
    );
}

fn scan_job_failed_banner_none_harness() -> Element {
    rsx! {
        ScanJobFailedBanner { message: None }
    }
}

#[test]
fn scan_job_failed_banner_renders_nothing_on_the_happy_path() {
    // No failure message (the ordinary case — the job is still running, or finished
    // cleanly) — the banner must be entirely absent, never an empty box.
    let mut vdom = VirtualDom::new(scan_job_failed_banner_none_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("onboard-gate"),
        "no banner markup at all on the happy path; html=\n{html}"
    );
}

// ── Tier-1 render: audit-integrated alternative recommendation ─────────────────────────
// docs/design/2026-09-22_audit-integrated-alternatives.md — Phase 2 (UI).

/// A multi-option `ProposedRuleView` fixture (2+ options), built via JSON so the required
/// fields (`id`, `title`, `kind`) are explicit and every optional field defaults sanely.
pub(super) fn proposed_rule_with_options(
    id: &str,
    title: &str,
    options: &[(&str, &str)],
) -> ProposedRuleView {
    let opts: Vec<serde_json::Value> = options
            .iter()
            .map(|(oid, label)| {
                serde_json::json!({ "id": oid, "label": label, "directive": format!("{label} directive"), "why": "" })
            })
            .collect();
    serde_json::from_value(serde_json::json!({
        "id": id, "title": title, "kind": "semantic", "options": opts
    }))
    .expect("valid ProposedRuleView fixture")
}

pub(super) fn recommendation(
    option_id: &str,
    reasoning: &str,
    hallucinated: bool,
    operator_chosen: bool,
) -> RuleRecommendationView {
    RuleRecommendationView {
        recommended_option_id: option_id.to_string(),
        recommendation_reasoning: reasoning.to_string(),
        hallucinated,
        operator_chosen,
        evidence: None,
        applicable: true,
    }
}

/// Like [`recommendation`] but with explicit `evidence`/`applicable` (P7) for the tests that
/// exercise the evidence line + not-applicable marker specifically.
pub(super) fn recommendation_with_evidence(
    option_id: &str,
    reasoning: &str,
    evidence: Option<&str>,
    applicable: bool,
) -> RuleRecommendationView {
    RuleRecommendationView {
        recommended_option_id: option_id.to_string(),
        recommendation_reasoning: reasoning.to_string(),
        hallucinated: false,
        operator_chosen: false,
        evidence: evidence.map(str::to_string),
        applicable,
    }
}

fn finding_evaluated_under(rule_id: &str, option_id: &str) -> FindingView {
    serde_json::from_value(serde_json::json!({
        "repo": "owner/repo", "path": "a.rs", "line": 1, "rule_id": rule_id,
        "severity": "medium", "snippet": "s", "detail": "",
        "evaluated_option_id": option_id
    }))
    .expect("valid FindingView fixture")
}

/// Harness: one multi-option rule WITH a finding (evaluated against "layered", AI
/// recommended) and one multi-option rule with ZERO findings (still listed — build item 2
/// of the design's UX: the review panel includes zero-violation rules so they can still be
/// overridden).
fn rule_alternatives_harness() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
    let audit = use_signal(|| None::<ScanReportView>);
    let proposed_rules = vec![
        proposed_rule_with_options(
            "ARCH-1",
            "Layering",
            &[
                ("layered", "Layered architecture"),
                ("hexagonal", "Hexagonal architecture"),
            ],
        ),
        proposed_rule_with_options(
            "ARCH-2",
            "Error handling",
            &[
                ("result", "Result-based"),
                ("exceptions", "Exception-based"),
            ],
        ),
    ];
    let mut recommendations = std::collections::HashMap::new();
    recommendations.insert(
        "ARCH-1".to_string(),
        recommendation(
            "layered",
            "The repo already separates controllers from services.",
            false,
            false,
        ),
    );
    recommendations.insert(
        "ARCH-2".to_string(),
        recommendation("result", "Fallback: no valid choice returned.", true, false),
    );
    rsx! {
        RuleAlternativesPanel {
            project_id: "proj-1".to_string(),
            proposed_rules,
            findings: vec![finding_evaluated_under("ARCH-1", "layered")],
            repos: vec!["owner/repo".to_string()],
            recommendations,
            audit,
        }
    }
}

#[test]
fn rule_alternatives_panel_renders_group_controls_for_every_multi_option_rule() {
    let mut vdom = VirtualDom::new(rule_alternatives_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("rec-panel"), "panel container; html=\n{html}");
    assert!(
        html.contains("Rule alternatives"),
        "panel heading; html=\n{html}"
    );
    // ARCH-1: has a finding, evaluated against its AI-recommended option.
    assert!(html.contains("ARCH-1"), "rule id; html=\n{html}");
    assert!(html.contains("Layering"), "rule title; html=\n{html}");
    assert!(
        html.contains("Evaluated against"),
        "evaluated-against label; html=\n{html}"
    );
    assert!(
        html.contains("Layered architecture"),
        "evaluated option label; html=\n{html}"
    );
    assert!(
        html.contains("AI recommended"),
        "AI-recommended badge; html=\n{html}"
    );
    assert!(
        html.contains("1 finding(s)"),
        "ARCH-1 finding count; html=\n{html}"
    );
    assert!(html.contains("Why?"), "Why control; html=\n{html}");
    assert!(
        html.contains("rec-select"),
        "Change dropdown; html=\n{html}"
    );
    assert!(
        html.contains("Hexagonal architecture"),
        "the non-evaluated option is still offered; html=\n{html}"
    );
    // ARCH-2: zero findings, but still listed with its own controls (build item 2 — the
    // review panel includes zero-violation rules so they can still be overridden).
    assert!(
        html.contains("ARCH-2"),
        "zero-violation rule id; html=\n{html}"
    );
    assert!(
        html.contains("Error handling"),
        "zero-violation rule title; html=\n{html}"
    );
    assert!(
        html.contains("0 findings"),
        "zero-violation finding count; html=\n{html}"
    );
    assert!(
        html.contains("Result-based"),
        "zero-violation rule's evaluated option; html=\n{html}"
    );
    // Accept alternatives is present and enabled (nothing staged yet).
    assert!(
        html.contains("Accept alternatives"),
        "accept button; html=\n{html}"
    );
    // No sticky bar yet — nothing staged.
    assert!(
        !html.contains("rec-sticky-bar"),
        "no sticky bar before any override is staged; html=\n{html}"
    );
}

/// Bounded internal scroll (46-rule regression): the panel used to grow one row per rule and
/// drag the whole page down. `.rec-rows` (the wrapper around the row list, NOT `.rec-panel`
/// itself) must declare both a `max-height` and `overflow-y: auto` so only the row list
/// scrolls while the panel head/sub, the sticky rescan bar, and the Accept button (siblings
/// rendered after `.rec-rows` closes) stay pinned. Style-source assertion — no render
/// harness computes CSS, so this is the only way to pin the rule down.
#[test]
fn rec_rows_wrapper_has_bounded_scroll() {
    let css = crate::style::GLOBAL_CSS;
    let rule = css
        .lines()
        .find(|l| l.trim_start().starts_with(".rec-rows "))
        .unwrap_or_else(|| panic!(".rec-rows rule not found in GLOBAL_CSS"));
    assert!(
        rule.contains("max-height"),
        ".rec-rows must cap its height; rule={rule}"
    );
    assert!(
        rule.contains("overflow-y: auto"),
        ".rec-rows must scroll internally; rule={rule}"
    );
    // `.rec-panel` itself (the outer shell) must NOT carry its own max-height/overflow — only
    // the inner `.rec-rows` list scrolls, so the header/sub stay outside the scroll region.
    let panel_rule = css
        .lines()
        .find(|l| l.trim_start().starts_with(".rec-panel "))
        .unwrap_or_else(|| panic!(".rec-panel rule not found in GLOBAL_CSS"));
    assert!(
        !panel_rule.contains("overflow"),
        ".rec-panel must not itself scroll; rule={panel_rule}"
    );
}

/// Uniform card layout: `.rec-row` must lay its children out in a COLUMN (header block, then
/// the "Evaluated against" line, then the Change dropdown on its own line) rather than the
/// old `flex-wrap: wrap`, which let the "Evaluated against" block land beside OR below the
/// header depending on content width. `.rec-select` must be full-width so the dropdown reads
/// as its own line rather than an inline control.
#[test]
fn rec_row_is_column_layout_and_select_is_full_width() {
    let css = crate::style::GLOBAL_CSS;
    let row_rule: String = {
        let start = css
            .find(".rec-row {")
            .expect(".rec-row rule not found in GLOBAL_CSS");
        let end = css[start..].find('}').expect("unterminated .rec-row rule") + start;
        css[start..=end].to_string()
    };
    assert!(
        row_rule.contains("flex-direction: column"),
        ".rec-row must be a column; rule={row_rule}"
    );
    assert!(
        !row_rule.contains("flex-wrap"),
        ".rec-row must not wrap children; rule={row_rule}"
    );
    let select_rule = css
        .lines()
        .find(|l| l.trim_start().starts_with(".rec-select "))
        .unwrap_or_else(|| panic!(".rec-select rule not found in GLOBAL_CSS"));
    assert!(
        select_rule.contains("width: 100%"),
        ".rec-select must be full-width; rule={select_rule}"
    );
}

/// Render-level proof that many rules (well past the small 2-row fixture above) still render
/// as uniform column cards inside the bounded `.rec-rows` wrapper — the actual shape that
/// regressed with 46 real rules, not just a CSS-source assertion.
#[test]
fn rule_alternatives_panel_renders_many_rules_inside_bounded_rows_wrapper() {
    fn many_rules_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
        let audit = use_signal(|| None::<ScanReportView>);
        let mut proposed_rules = Vec::new();
        let mut recommendations = std::collections::HashMap::new();
        for i in 0..20 {
            let id = format!("ARCH-{i}");
            proposed_rules.push(proposed_rule_with_options(
                &id,
                &format!("Rule {i}"),
                &[("a", "Option A"), ("b", "Option B")],
            ));
            recommendations.insert(id, recommendation("a", "reasoning", false, false));
        }
        rsx! {
            RuleAlternativesPanel {
                project_id: "proj-1".to_string(),
                proposed_rules,
                findings: vec![],
                repos: vec!["owner/repo".to_string()],
                recommendations,
                audit,
            }
        }
    }
    let mut vdom = VirtualDom::new(many_rules_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("rec-rows"),
        "bounded rows wrapper; html=\n{html}"
    );
    assert_eq!(
        html.matches("rec-row\"").count(),
        20,
        "one card per rule; html=\n{html}"
    );
    for i in 0..20 {
        assert!(
            html.contains(&format!("ARCH-{i}")),
            "rule {i} rendered; html=\n{html}"
        );
    }
}

/// P7 (the codebase-inspection hardening notes): each rule's card must show
/// the evidence `file:line` the pick was grounded in, and an applicability marker when the
/// model judged the rule not applicable to this repo — "a wrong pick costs a glance, not a
/// false finding".
#[test]
fn rule_alternatives_panel_renders_evidence_and_not_applicable_marker() {
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
        let audit = use_signal(|| None::<ScanReportView>);
        let proposed_rules = vec![
            proposed_rule_with_options(
                "ARCH-1",
                "Layering",
                &[
                    ("layered", "Layered architecture"),
                    ("hexagonal", "Hexagonal architecture"),
                ],
            ),
            proposed_rule_with_options(
                "ARCH-2",
                "Error handling",
                &[
                    ("result", "Result-based"),
                    ("exceptions", "Exception-based"),
                ],
            ),
        ];
        let mut recommendations = std::collections::HashMap::new();
        recommendations.insert(
            "ARCH-1".to_string(),
            recommendation_with_evidence(
                "layered",
                "The repo already separates controllers from services.",
                Some("src/api/handler.rs:42 — controller calls service.method directly"),
                true,
            ),
        );
        recommendations.insert(
            "ARCH-2".to_string(),
            recommendation_with_evidence(
                "result",
                "This rule targets a stack this repo doesn't use.",
                None,
                false,
            ),
        );
        rsx! {
            RuleAlternativesPanel {
                project_id: "proj-1".to_string(),
                proposed_rules,
                findings: vec![],
                repos: vec!["owner/repo".to_string()],
                recommendations,
                audit,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    // ARCH-1: evidenced + applicable — shows the evidence line, no "not applicable" marker
    // anywhere near it (checked precisely below via the ARCH-2-only assertion).
    assert!(
        html.contains("src/api/handler.rs:42"),
        "evidence file:line rendered; html=\n{html}"
    );
    assert!(
        html.contains("rec-evidence"),
        "evidence line uses its own class; html=\n{html}"
    );
    // ARCH-2: not applicable — marker rendered, and it has no evidence line of its own
    // (only ARCH-1 contributes an occurrence of the evidence class/text).
    assert!(
        html.contains("Not applicable to this repo"),
        "not-applicable marker rendered; html=\n{html}"
    );
    assert_eq!(
        html.matches("rec-evidence").count(),
        1,
        "only the evidenced rule (ARCH-1) renders an evidence line; html=\n{html}"
    );
}

#[test]
fn rule_alternatives_panel_renders_nothing_when_no_recommendations() {
    // No multi-option rule was part of the audit (e.g. a mechanical-only scan) — the panel
    // must render nothing, not an empty shell.
    fn harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
        let audit = use_signal(|| None::<ScanReportView>);
        rsx! {
            RuleAlternativesPanel {
                project_id: "proj-1".to_string(),
                proposed_rules: vec![],
                findings: vec![],
                repos: vec![],
                recommendations: std::collections::HashMap::new(),
                audit,
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("rec-panel"),
        "no panel shell when nothing is in scope; html=\n{html}"
    );
}

// ── Tier-1 render: RuleWhyModal (extracted so the "open" shape is directly testable) ────

fn why_modal_harness() -> Element {
    rsx! {
        RuleWhyModal {
            rule_id: "ARCH-1".to_string(),
            title: "Layering".to_string(),
            reasoning: "The repo already separates controllers from services.".to_string(),
            hallucinated: false,
            on_close: move |_| {},
        }
    }
}

#[test]
fn rule_why_modal_shows_the_reasoning() {
    let mut vdom = VirtualDom::new(why_modal_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("ARCH-1"), "rule id; html=\n{html}");
    assert!(html.contains("Layering"), "title; html=\n{html}");
    assert!(
        html.contains("Why this alternative"),
        "section label; html=\n{html}"
    );
    assert!(
        html.contains("The repo already separates controllers from services."),
        "reasoning body; html=\n{html}"
    );
    assert!(
        !html.contains("rec-hallucinated-note"),
        "no hallucination note when false; html=\n{html}"
    );
}

fn why_modal_hallucinated_harness() -> Element {
    rsx! {
        RuleWhyModal {
            rule_id: "ARCH-2".to_string(),
            title: "Error handling".to_string(),
            reasoning: "Fallback: no valid choice returned.".to_string(),
            hallucinated: true,
            on_close: move |_| {},
        }
    }
}

#[test]
fn rule_why_modal_renders_hallucinated_note() {
    let mut vdom = VirtualDom::new(why_modal_hallucinated_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("rec-hallucinated-note"),
        "hallucination note class; html=\n{html}"
    );
    assert!(
        html.contains("did not return a valid choice"),
        "hallucination explanation; html=\n{html}"
    );
}

#[test]
fn rule_why_modal_falls_back_when_no_reasoning_recorded() {
    fn harness() -> Element {
        rsx! {
            RuleWhyModal {
                rule_id: "ARCH-3".to_string(),
                title: "Naming".to_string(),
                reasoning: String::new(),
                hallucinated: false,
                on_close: move |_| {},
            }
        }
    }
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("No reasoning was recorded for this recommendation."),
        "fallback note; html=\n{html}"
    );
}

// ── Tier-1 render: RescanStickyBar ───────────────────────────────────────────────────────

fn sticky_bar_harness() -> Element {
    rsx! {
        RescanStickyBar { pending_count: 3usize, rescanning: false, on_rescan: move |_| {} }
    }
}

#[test]
fn rescan_sticky_bar_shows_the_correct_count() {
    let mut vdom = VirtualDom::new(sticky_bar_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("rec-sticky-bar"),
        "sticky bar container; html=\n{html}"
    );
    assert!(
        html.contains("Rescan 3 overridden rules"),
        "count reflected in both the label and the button; html=\n{html}"
    );
}

fn sticky_bar_rescanning_harness() -> Element {
    rsx! {
        RescanStickyBar { pending_count: 1usize, rescanning: true, on_rescan: move |_| {} }
    }
}

#[test]
fn rescan_sticky_bar_shows_rescanning_label_and_disables_the_button() {
    let mut vdom = VirtualDom::new(sticky_bar_rescanning_harness);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Rescanning"),
        "in-flight label; html=\n{html}"
    );
    assert!(
        html.contains("disabled"),
        "button disabled while rescanning; html=\n{html}"
    );
}
