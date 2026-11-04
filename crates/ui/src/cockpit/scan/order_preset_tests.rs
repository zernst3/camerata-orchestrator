//! Tests: order preset tests.

use super::*;
use std::cell::RefCell;
use std::time::Duration;

fn finding(rule: &str, sev: &str, path: &str, line: usize) -> FindingView {
    serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": path, "line": line, "rule_id": rule, "severity": sev,
        "snippet": format!("snip-{rule}-{line}"), "detail": "d", "status": "active",
        "kind": "defect"
    }))
    .expect("fixture")
}

/// Five findings across three files and three rules, chosen so that every preset gives a
/// different order of groups.
fn rows() -> Vec<FindingView> {
    vec![
        finding("R-A", "high", "src/z.rs", 40),
        finding("R-B", "critical", "src/a.rs", 2),
        finding("R-A", "critical", "src/a.rs", 9),
        finding("R-A", "high", "src/a.rs", 1),
        finding("R-0", "low", "src/m.rs", 3),
    ]
}

#[derive(Clone, Copy)]
struct Probe {
    preset: Signal<OrderPreset>,
    request: Signal<Option<String>>,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
}

/// The table as the results view mounts it, with the preset lifted into a signal the test
/// can flip, and the finding-modal's "open in viewer" request as the way to aim the cursor.
fn app() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<FindingView>));
    let request = use_signal(|| Option::<String>::None);
    use_context_provider(|| super::super::viewer::ViewerRequest(request));
    let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let preset = use_signal(|| PRESET_AT_MOUNT.with(|p| p.get()));
    PROBE.with(|p| *p.borrow_mut() = Some(Probe { preset, request }));
    rsx! {
        FindingsTable {
            findings: rows(),
            repos: vec!["me/api".to_string()],
            descriptions: std::collections::HashMap::new(),
            triage_view: TriageState::Unresolved,
            dispositions,
            order_preset: preset,
        }
    }
}

thread_local! {
    static PRESET_AT_MOUNT: std::cell::Cell<OrderPreset> =
        const { std::cell::Cell::new(OrderPreset::Curated) };
}

fn mount(preset: OrderPreset) -> VirtualDom {
    PRESET_AT_MOUNT.with(|p| p.set(preset));
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    vdom
}

async fn pump(vdom: &mut VirtualDom, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

/// The text of each chorale group header, in the order the table shows them.
fn headers(html: &str) -> Vec<String> {
    let marker = "class=\"chorale-group-header\"";
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(marker) {
        rest = &rest[at + marker.len()..];
        let end = rest.find("</tr>").unwrap_or(rest.len());
        let text = strip_tags(&rest[..end]);
        out.push(text);
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// The first group header whose text starts with `prefix`, in the order shown.
fn position(headers: &[String], prefix: &str) -> usize {
    headers
        .iter()
        .position(|h| h.contains(prefix))
        .unwrap_or_else(|| panic!("no header with {prefix}: {headers:?}"))
}

/// B.4: each preset cuts the groups the way it says, at mount (what a reloaded draft does).
#[test]
fn each_preset_orders_and_groups_the_mounted_queue_its_own_way() {
    // Curated: rule groups, worst first.
    let h = headers(&dioxus_ssr::render(&mount(OrderPreset::Curated)));
    assert!(
        position(&h, "R-A") < position(&h, "R-B") && position(&h, "R-B") < position(&h, "R-0"),
        "{h:?}"
    );
    // By rule: rule groups in rule id order.
    let h = headers(&dioxus_ssr::render(&mount(OrderPreset::ByRule)));
    assert!(
        position(&h, "R-0") < position(&h, "R-A") && position(&h, "R-A") < position(&h, "R-B"),
        "{h:?}"
    );
    // By file: the top level is the file, in path order, and no rule heads a group yet.
    let h = headers(&dioxus_ssr::render(&mount(OrderPreset::ByFile)));
    assert_eq!(h.len(), 3, "one collapsed group per file: {h:?}");
    assert!(
        position(&h, "src/a.rs") < position(&h, "src/m.rs")
            && position(&h, "src/m.rs") < position(&h, "src/z.rs"),
        "{h:?}"
    );
    assert!(h.iter().all(|t| !t.contains("R-")), "{h:?}");
    // Critical first: no groups at all, and the rows run worst first.
    let html = dioxus_ssr::render(&mount(OrderPreset::CriticalFirst));
    assert!(headers(&html).is_empty(), "flat: no group headers");
    let at = |s: &str| {
        html.find(s)
            .unwrap_or_else(|| panic!("{s} missing: {html}"))
    };
    assert!(at("snip-R-A-9") < at("snip-R-A-1"), "critical before high");
    assert!(at("snip-R-B-2") < at("snip-R-A-40"), "critical before high");
    assert!(at("snip-R-A-40") < at("snip-R-0-3"), "high before low");
}

/// The select names every preset and shows the current one.
#[test]
fn the_select_lists_the_four_cuts_and_marks_the_current_one() {
    let html = dioxus_ssr::render(&mount(OrderPreset::ByFile));
    for p in OrderPreset::ALL {
        assert!(html.contains(&format!("value=\"{}\"", p.key())), "{html}");
        assert!(html.contains(p.label()), "{}", p.label());
    }
    let by_file = html
        .find("<option value=\"by_file\"")
        .unwrap_or_else(|| panic!("no by_file option: {html}"));
    let tag_start = by_file;
    let tag_end = by_file + html[by_file..].find('>').unwrap();
    assert!(
        html[tag_start..tag_end].contains("selected"),
        "By file is the selected option: {}",
        &html[tag_start..tag_end]
    );
    // And the bar names the order on screen.
    assert!(html.contains(&OrderPreset::ByFile.sentence()), "{html}");
    assert!(!html.contains(&OrderPreset::Curated.sentence()));
}

/// B.4 check 1: selecting a preset on the live table regroups it, and the cursor stays on
/// the same finding key through every re-cut.
#[tokio::test(flavor = "current_thread")]
async fn switching_preset_regroups_the_live_table_and_the_cursor_keeps_its_finding() {
    let mut vdom = mount(OrderPreset::Curated);
    pump(&mut vdom, 50).await;
    let probe = PROBE.with(|p| p.borrow().expect("probe"));
    // Aim the cursor at one finding in the middle of the list (what the finding modal's
    // "Open in viewer" does), which opens the groups that hold it.
    let target = rows()[2].clone();
    let want = super::super::viewer::dom_key(&finding_key(&target));
    vdom.in_runtime(|| {
        let mut request = probe.request;
        request.set(Some(finding_key(&target)));
    });
    pump(&mut vdom, 100).await;
    let key_on_pane = |html: &str| {
        let attr = "data-finding-key=\"";
        let i = html.find(attr).expect("the pane is rendered") + attr.len();
        html[i..i + html[i..].find('"').unwrap()].to_string()
    };
    let html = dioxus_ssr::render(&vdom);
    assert_eq!(
        key_on_pane(&html),
        want,
        "the cursor is on the aimed finding"
    );

    for preset in [
        OrderPreset::ByFile,
        OrderPreset::CriticalFirst,
        OrderPreset::ByRule,
        OrderPreset::Curated,
    ] {
        vdom.in_runtime(|| {
            let mut p = probe.preset;
            p.set(preset);
        });
        pump(&mut vdom, 100).await;
        let html = dioxus_ssr::render(&vdom);
        assert_eq!(
            key_on_pane(&html),
            want,
            "{preset:?}: the cursor stays on the same finding"
        );
        // The pane describes that finding, and the bar names the cut now on screen.
        assert!(html.contains("src/a.rs:9"), "{preset:?}: pane location");
        assert!(html.contains(&preset.sentence()), "{preset:?}: the bar");
        let h = headers(&html);
        match preset.grouping() {
            Grouping::Flat => assert!(h.is_empty(), "{preset:?}: {h:?}"),
            Grouping::FileThenRule => {
                assert!(h.first().is_some_and(|t| t.contains("src/")), "{h:?}");
            }
            Grouping::RuleThenFile => {
                assert!(h.first().is_some_and(|t| t.contains("R-")), "{h:?}");
            }
        }
    }
}

/// The choice is draft state, like `triage_view`: it round-trips through the saved draft,
/// and a draft saved before presets existed reads as the curated order.
#[test]
fn the_preset_is_saved_in_the_draft_and_an_old_draft_reads_as_curated() {
    let scan = serde_json::json!({
        "files_scanned": 0, "findings": [], "proposed_rules": [], "gated": false
    });
    let old: OnboardingDraft =
        serde_json::from_value(serde_json::json!({ "scan": scan })).expect("old draft");
    assert_eq!(old.order_preset, OrderPreset::Curated);

    let mut draft = old.clone();
    draft.order_preset = OrderPreset::ByFile;
    let wire = serde_json::to_value(&draft).unwrap();
    assert_eq!(wire["order_preset"], "by_file");
    let back: OnboardingDraft = serde_json::from_value(wire).unwrap();
    assert_eq!(back.order_preset, OrderPreset::ByFile);
}

/// The review log (INTERNAL, step 7a) is saved with the draft under the one key the export
/// tests know it by; a draft from before it existed reads as an empty log; a log of a shape
/// this build does not know reads as an empty log and the dispositions and view survive.
#[test]
fn the_review_log_is_saved_in_the_draft_and_an_old_or_unknown_one_reads_as_empty() {
    use camerata_ui_core::review_timer::{GroupTiming, ReviewLog};
    let scan = serde_json::json!({
        "files_scanned": 0, "findings": [], "proposed_rules": [], "gated": false
    });
    let old: OnboardingDraft =
        serde_json::from_value(serde_json::json!({ "scan": scan })).expect("old draft");
    assert_eq!(old.review_log.log, ReviewLog::default());
    assert!(old.review_log.unreadable.is_none() && !old.review_log.just_failed);

    let mut draft = old.clone();
    draft.review_log.log.review_groups.insert(
        "me/api\u{1f}R-A".to_string(),
        GroupTiming {
            opened_at_ms: 5,
            active_ms: 90_000,
            group_reviewed: true,
        },
    );
    let wire = serde_json::to_value(&draft).unwrap();
    assert_eq!(
        wire[camerata_api_types::review_timing::REVIEW_LOG_KEY]["review_groups"]["me/api\u{1f}R-A"]
            ["active_ms"],
        90_000
    );
    let back: OnboardingDraft = serde_json::from_value(wire).unwrap();
    assert_eq!(back.review_log, draft.review_log);

    for bad in [
        serde_json::json!("future"),
        serde_json::json!({ "review_groups": [1, 2] }),
        serde_json::json!(7),
    ] {
        let draft: OnboardingDraft = serde_json::from_value(serde_json::json!({
            "scan": scan, "triage_view": "TechDebt", "review_log": bad,
        }))
        .expect("a bad log never fails the draft");
        assert_eq!(draft.review_log.log, ReviewLog::default());
        assert_eq!(
            draft.review_log.unreadable.as_ref(),
            Some(&bad),
            "the original is kept, not dropped"
        );
        assert_eq!(draft.triage_view, TriageState::TechDebt);
        // And it is written back verbatim by the next save.
        let wire = serde_json::to_value(&draft).unwrap();
        assert_eq!(
            wire[camerata_api_types::review_timing::REVIEW_LOG_KEY]["review_unreadable"],
            bad
        );
    }
}

/// The HTTP transport against a stand-in server: a load reads the store, a post the server
/// refuses comes back as a refusal in the server's words (and does not stop the next key),
/// a server error counts as unreachable, and the run the load learned stamps the change.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_decision_transport_maps_refusals_and_failures_and_stamps_the_run() {
    use camerata_api_types::decisions::{
        Decision, DecisionError, DecisionRecorded, DecisionState, DecisionsResp, Scope,
    };
    use camerata_ui_core::decision_sync::{
        load_decisions, sync_decisions, DecisionTransport, OpError,
    };
    use camerata_ui_core::decisions::{Availability, DraftQueue};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let uri = "/api/projects/p1/decisions";
    Mock::given(method("GET"))
        .and(path(uri))
        .respond_with(ResponseTemplate::new(200).set_body_json(DecisionsResp {
            decisions: Vec::new(),
            standings: Vec::new(),
            migrated_from_draft: true,
            latest_run_id: "insp-9".to_string(),
        }))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(uri))
        .and(body_partial_json(serde_json::json!({"key": "k1"})))
        .respond_with(ResponseTemplate::new(422).set_body_json(DecisionError {
            ok: false,
            code: "reason_required".to_string(),
            message: "ignoring a finding needs a reason".to_string(),
        }))
        .mount(&server)
        .await;
    let recorded = |key: &str| DecisionRecorded {
        decision: Decision {
            id: format!("dec-{key}"),
            key: key.to_string(),
            state: DecisionState::Confirmed,
            reason: String::new(),
            scope: Scope::Site,
            applies_to: Vec::new(),
            except: Vec::new(),
            inherited_from: None,
            decided_by: "reviewer".to_string(),
            decided_at: "2026-11-02T10:00:00Z".to_string(),
            run_id: "insp-9".to_string(),
            engine_version: "v".to_string(),
            needed_me: None,
            sample_verdict: None,
            confirmed_by_client: None,
            revoked_at: None,
        },
        inherited_count: 0,
    };
    Mock::given(method("POST"))
        .and(path(uri))
        .and(body_partial_json(serde_json::json!({"key": "k2"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("k2")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(uri))
        .and(body_partial_json(serde_json::json!({"key": "k3"})))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let transport = HttpDecisions {
        project_id: "p1".to_string(),
    };
    let loaded = load_decisions(&transport, &DraftQueue::default()).await;
    assert_eq!(loaded.bridge.availability(), Availability::Live);
    assert_eq!(loaded.bridge.run_id(), "insp-9");

    let mut screen = std::collections::HashMap::new();
    for (k, st, why) in [
        ("k1", TriageState::Ignored, ""),
        ("k2", TriageState::Confirmed, ""),
    ] {
        screen.insert(
            k.to_string(),
            Disposition {
                state: st,
                reason: why.to_string(),
                bucket: TechDebtBucket::Later,
            },
        );
    }
    let bridge = std::cell::RefCell::new(loaded.bridge);
    bridge
        .borrow_mut()
        .observe(&screen, "insp-9", "2026-11-02T10:00:00Z");
    let out = sync_decisions(
        &bridge,
        &transport,
        || screen.clone(),
        |k| format!("{k}.rs:1"),
    )
    .await;
    assert!(out.stored_any, "k2 was stored after k1 was refused");
    assert_eq!(out.refused.len(), 1);
    let text = out.refused[0].text();
    assert!(text.contains("k1.rs:1") && text.contains("ignoring a finding needs a reason"));
    let bodies: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(|r| r.body_json().unwrap())
        .collect();
    let k2 = bodies.iter().find(|b| b["key"] == "k2").unwrap();
    assert_eq!(k2["run_id"], "insp-9");
    assert_eq!(k2["decided_at"], "2026-11-02T10:00:00Z");

    // A server error is not a refusal: the store is unreachable and the change stays queued.
    let boom = transport
        .post(&camerata_api_types::decisions::NewDecisionReq {
            scope: Scope::Site,
            key: "k3".to_string(),
            state: DecisionState::Confirmed,
            reason: String::new(),
            decided_by: String::new(),
            needed_me: None,
            sample_verdict: None,
            except: Vec::new(),
            run_id: String::new(),
            decided_at: String::new(),
        })
        .await;
    assert!(matches!(boom, Err(OpError::Unreachable(_))), "{boom:?}");
}

/// The request that asks the server for the client's export carries the dispositions and the
/// report options and nothing else, so the review log cannot ride along even by accident.
/// It carries no client name either: the server reads that from the project.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_product_export_request_carries_no_review_log() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/product-export"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"zip".to_vec()))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut dispositions = std::collections::HashMap::new();
    dispositions.insert(
        "k".to_string(),
        Disposition {
            state: TriageState::Confirmed,
            ..Disposition::default()
        },
    );
    let options = camerata_ui_core::product_export::ProductExportOptions {
        project_title: "Title".to_string(),
        prepared_by: "Me".to_string(),
        triage_log: true,
        ..Default::default()
    };
    export_product_zip("p1", &dispositions, &options)
        .await
        .expect("the export answers");
    let requests = server.received_requests().await.expect("recorded");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("json");
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["dispositions", "options"]);
    assert_eq!(
        body["options"]["triage_log"],
        serde_json::json!(true),
        "the ticked triage-log box reaches the request"
    );
    assert!(
        body["options"].get("client_name").is_none(),
        "the client name comes from the project's inspection setup, never the request"
    );
    let text = body.to_string();
    for key in camerata_api_types::review_timing::INTERNAL_ONLY_KEYS {
        assert!(!text.contains(key), "{key} in the export request: {text}");
    }
}

/// TRIPWIRE, not proof. The Queue's own CSV button exports findings, which carry no
/// review-time field; this checks the header row (no rows) and one serialized finding against
/// the deny-list, so a review-time column or field added later is caught here. The structural
/// guard is that `FindingView` has no such field and the CSV is built from it alone.
#[test]
fn the_queue_csv_carries_no_review_time_field() {
    let csv = findings_csv(&[]);
    for key in camerata_api_types::review_timing::INTERNAL_ONLY_KEYS {
        assert!(!csv.contains(key), "{key} in the Queue CSV header: {csv}");
    }
    let json = serde_json::to_string(&serde_json::json!({
        "rule_id": "R", "severity": "high", "snippet": "s", "detail": "d",
        "path": "a.rs", "line": 1
    }))
    .unwrap();
    let finding: FindingView = serde_json::from_str(&json).unwrap();
    let written = serde_json::to_string(&finding).unwrap();
    for key in camerata_api_types::review_timing::INTERNAL_ONLY_KEYS {
        assert!(!written.contains(key), "{key} on a finding: {written}");
    }
}
