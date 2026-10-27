//! The detail pane's code context (work order B.3): the Dioxus side. Every judgment is in
//! `camerata_ui_core::pane_context`; this file owns the clock, the request and the signals.
//!
//! The pane renders the stored snippet at once. [`use_pane_context`] watches which finding the
//! cursor is on; once the cursor has rested there for `SETTLE_MS` it asks
//! `POST /api/onboard/finding-context/batch` for that one finding (a batch of one), keeps the
//! answer in the viewer's cache (`ViewerCache`, the pane's lane) and hands the pane the window
//! to draw beneath the snippet. Nothing it can fail at changes what the pane drew before:
//! every refusal, a failed request, a stale answer is `None`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use camerata_ui_core::pane_context::{
    accept, BatchRequest, BatchResponse, ContextTarget, PaneContext, SettleGate, SETTLE_MS,
};
use camerata_ui_core::viewer::ViewerCache;
use dioxus::prelude::*;

/// `POST /api/onboard/finding-context/batch` for one finding. `None` when the server cannot be
/// reached or does not answer with JSON; a refusal is an answer whose entries are not `ok`.
pub(super) async fn fetch_context(
    target: &ContextTarget,
    project: Option<&str>,
) -> Option<BatchResponse> {
    #[cfg(test)]
    REQUESTS.with(|c| c.set(c.get() + 1));
    let items = [target.item.clone()];
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/onboard/finding-context/batch",
            crate::bff_base()
        ))
        .json(&BatchRequest {
            project,
            items: &items,
        })
        .send()
        .await
        .ok()?;
    resp.json::<BatchResponse>().await.ok()
}

#[cfg(test)]
thread_local! {
    /// Test seam: how many context requests this thread has started.
    pub(super) static REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The context window to draw under the focused finding's snippet, or `None` (draw nothing new).
///
/// `target` is the focused finding's request, or `None` when nothing is focused. A window already
/// held (a whole file the viewer read, or an earlier answer) is returned at once and costs no
/// request; otherwise one request goes out when the cursor settles, and the answer is shown only
/// while its finding is still the focused one.
pub(super) fn use_pane_context(
    target: Memo<Option<ContextTarget>>,
    project_id: String,
) -> Option<PaneContext> {
    let cache = try_use_context::<Signal<ViewerCache>>();
    // A monotonic clock, like the undo window's: the settle time is time since mount.
    let epoch = use_hook(Instant::now);
    let mut gate = use_signal(SettleGate::new);
    let mut settled = use_signal(|| Option::<ContextTarget>::None);

    // Every change of the focused finding is told to the gate, and asked about again SETTLE_MS
    // later. A burst of moves leaves one pending key, so only the last timer finds it due.
    use_effect(move || {
        let key = target.read().as_ref().map(|t| t.finding_key.clone());
        let now = epoch.elapsed().as_millis() as u64;
        gate.write().moved(key.as_deref(), now);
        if key.is_none() {
            return;
        }
        spawn(async move {
            tokio::time::sleep(Duration::from_millis(SETTLE_MS)).await;
            let now = epoch.elapsed().as_millis() as u64;
            let due = gate.write().due(now);
            if let Some(key) = due {
                if let Some(t) = target.peek().clone().filter(|t| t.finding_key == key) {
                    settled.set(Some(t));
                }
            }
        });
    });

    let project = Some(project_id).filter(|p| !p.is_empty());
    let fetched = use_resource(move || {
        let t = settled();
        let project = project.clone();
        async move {
            let t = t?;
            // Held already (the viewer's file, or an earlier answer): no request.
            if cache.is_some_and(|c| {
                c.peek()
                    .context(&t.cache, t.item.expect.as_deref())
                    .is_some()
            }) {
                return None;
            }
            let response = fetch_context(&t, project.as_deref()).await?;
            let window = accept(&t, &response)?;
            if let Some(mut cache) = cache {
                cache
                    .write()
                    .put_context(t.cache.clone(), Arc::new(window.clone()));
            }
            Some((t.finding_key, window))
        }
    });

    let t = target.read().clone()?;
    if let Some(held) = cache.and_then(|c| c.peek().context(&t.cache, t.item.expect.as_deref())) {
        return Some((*held).clone());
    }
    // An answer for a finding the cursor has left is dropped here, not drawn.
    let answer = fetched.read().clone();
    match answer {
        Some(Some((key, window))) if key == t.finding_key => Some(window),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! B.3 acceptance: the stored snippet is there at once, the surrounding code fills in
    //! beneath it, three moves inside 200 ms issue one request, a stale answer is dropped, and any
    //! status but `ok` leaves the pane byte-identical to what it was.

    use std::cell::RefCell;
    use std::time::Duration;

    use camerata_ui_core::pane_context::BatchAnswer;
    use camerata_ui_core::triage::{FindingView, KeyMods, PaneFocus, TriageState};
    use camerata_ui_core::viewer::{FileKey, SourceFileView, ViewerCache, WindowView};
    use dioxus::prelude::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::super::scan::{DetailPaneRow, FindingDetailPane};
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn finding(line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": line, "rule_id": "R-1",
            "severity": "high", "snippet": format!("code at {line}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    /// The four findings the harness moves between.
    fn findings() -> Vec<FindingView> {
        [10, 20, 30, 40].into_iter().map(finding).collect()
    }

    /// The pane for `row` and `context`, props only.
    #[component]
    fn PaneHarness(row: DetailPaneRow, context: Option<PaneContext>) -> Element {
        rsx! {
            FindingDetailPane {
                row: Some(row),
                focus: PaneFocus::Elsewhere,
                prompt: None::<TriageState>,
                prompt_reason: String::new(),
                undo: None,
                on_key: Callback::new(|_: (String, KeyMods)| {}),
                on_focus: Callback::new(|_: PaneFocus| {}),
                on_prompt_reason: Callback::new(|_: String| {}),
                on_prompt_commit: Callback::new(|()| {}),
                on_prompt_cancel: Callback::new(|()| {}),
                on_undo: Callback::new(|()| {}),
                on_dispose: Callback::new(|_: TriageState| {}),
                on_open_detail: Callback::new(|()| {}),
                context,
            }
        }
    }

    fn render_pane(context: Option<PaneContext>) -> String {
        let row = DetailPaneRow::from_finding(&finding(10), None, None, 0, 4);
        let mut vdom = VirtualDom::new_with_props(PaneHarness, PaneHarnessProps { row, context });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    fn target(line: usize) -> ContextTarget {
        ContextTarget::for_finding(&finding(line), Some(SHA)).expect("target")
    }

    fn ok_answer(t: &ContextTarget) -> BatchAnswer {
        BatchAnswer {
            status: "ok".into(),
            source: "git".into(),
            key: Some(t.finding_key.clone()),
            repo: t.item.repo.clone(),
            path: t.item.path.clone(),
            line: t.item.line,
            start_line: t.item.line - 1,
            violation_line: t.item.line,
            lines: vec![
                "above".into(),
                format!("code at {}", t.item.line),
                "below".into(),
            ],
            matches_snippet: true,
        }
    }

    #[test]
    fn the_snippet_is_there_and_the_context_fills_in_beneath_it_with_the_violation_marked() {
        let t = target(10);
        let ctx = accept(
            &t,
            &BatchResponse {
                items: vec![ok_answer(&t)],
            },
        )
        .expect("accepted");
        let html = render_pane(Some(ctx));
        let snippet = html.find("pane-snippet").expect("the stored snippet");
        let context = html.find("data-pane-context").expect("the context block");
        assert!(snippet < context, "the context sits beneath the snippet");
        assert!(html.contains("above") && html.contains("below"), "{html}");
        // Only the violation line carries the highlight class.
        assert_eq!(html.matches("finding-ctx-line-hit").count(), 1, "{html}");
        let hit = html.find("finding-ctx-line-hit").unwrap();
        assert!(html[hit..].contains(">10<") && html[hit..].contains("code at 10"));
    }

    /// B.3 check 2: any non-ok status leaves the pane identical to today's rendering.
    #[test]
    fn every_non_ok_answer_renders_the_pane_byte_identical_to_today() {
        let baseline = render_pane(None);
        assert!(!baseline.contains("data-pane-context"));
        let t = target(10);
        for status in [
            "commit_unavailable",
            "source_deleted",
            "file_missing",
            "not_utf8",
            "too_large",
            "line_gone",
            "path_traversal",
            "unavailable",
        ] {
            // A non-ok entry carries no lines; this one does, as a hostile server might send.
            let mut a = ok_answer(&t);
            a.status = status.into();
            let accepted = accept(&t, &BatchResponse { items: vec![a] });
            assert_eq!(render_pane(accepted), baseline, "{status}");
        }
        // The working tree is not the inspected commit: also today's pane.
        let mut working = ok_answer(&t);
        working.source = "working_tree".into();
        assert_eq!(
            render_pane(accept(
                &t,
                &BatchResponse {
                    items: vec![working]
                }
            )),
            baseline
        );
    }

    // ── The mounted hook ────────────────────────────────────────────────────────────────

    #[derive(Clone, Copy)]
    struct Probe {
        idx: Signal<usize>,
        cache: Signal<ViewerCache>,
    }

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    }

    fn app() -> Element {
        let idx = use_signal(|| 0_usize);
        let cache = use_context_provider(|| Signal::new(ViewerCache::new()));
        PROBE.with(|p| *p.borrow_mut() = Some(Probe { idx, cache }));
        let target = use_memo(move || ContextTarget::for_finding(&findings()[idx()], Some(SHA)));
        let context = use_pane_context(target, "p1".to_string());
        let row = DetailPaneRow::from_finding(&findings()[idx()], None, None, idx(), 4);
        rsx! { PaneHarness { row, context } }
    }

    /// The batch endpoint, faked: answers every item `status` (an `ok` answer carries the line
    /// and one either side) after `delay`.
    struct Answerer {
        status: &'static str,
        delay: Duration,
    }

    impl Respond for Answerer {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let items: Vec<serde_json::Value> = body["items"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let line = item["line"].as_u64().unwrap();
                    let mut a = serde_json::json!({
                        "status": self.status, "index": index, "key": item["key"],
                        "repo": item["repo"], "path": item["path"], "line": line,
                    });
                    if self.status == "ok" {
                        a["source"] = "git".into();
                        a["start_line"] = (line - 1).into();
                        a["violation_line"] = line.into();
                        a["lines"] =
                            serde_json::json!(["above", format!("code at {line}"), "below"]);
                        a["matches_snippet"] = true.into();
                    }
                    a
                })
                .collect();
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "items": items }))
                .set_delay(self.delay)
        }
    }

    async fn serve(status: &'static str, delay_ms: u64) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/finding-context/batch"))
            .respond_with(Answerer {
                status,
                delay: Duration::from_millis(delay_ms),
            })
            .mount(&server)
            .await;
        server
    }

    /// Let timers, requests and effects run for about `ms`.
    async fn pump(vdom: &mut VirtualDom, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while tokio::time::Instant::now() < until {
            let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
            let _ = vdom.render_immediate_to_vec();
        }
    }

    fn move_to(vdom: &VirtualDom, to: usize) {
        let probe = PROBE.with(|p| p.borrow().expect("probe"));
        vdom.in_runtime(|| {
            let mut idx = probe.idx;
            idx.set(to);
        });
    }

    /// The POST bodies the fake server received: the other side of the request seam.
    async fn posted(server: &MockServer) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|q| q.method.as_str() == "POST")
            .map(|q| serde_json::from_slice(&q.body).unwrap())
            .collect()
    }

    /// B.3 check 1: moving the cursor three rows within 200 ms issues one request, the snippet
    /// is visible immediately and the surrounding lines appear beneath it.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn three_moves_inside_200ms_issue_one_request_and_the_context_fills_in() {
        let server = serve("ok", 0).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        REQUESTS.with(|c| c.set(0));

        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        let first = dioxus_ssr::render(&vdom);
        assert!(first.contains("code at 10"), "the snippet is there at once");
        assert!(!first.contains("data-pane-context"), "and nothing else yet");

        pump(&mut vdom, 20).await;
        move_to(&vdom, 1);
        pump(&mut vdom, 60).await;
        move_to(&vdom, 2);
        pump(&mut vdom, 60).await;
        move_to(&vdom, 3);
        let _ = vdom.render_immediate_to_vec();
        let before = dioxus_ssr::render(&vdom);
        assert!(
            before.contains("code at 40") && !before.contains("data-pane-context"),
            "the snippet is immediate on a move; the context is not"
        );
        pump(&mut vdom, 600).await;

        assert_eq!(
            REQUESTS.with(std::cell::Cell::get),
            1,
            "one request, not four"
        );
        let bodies = posted(&server).await;
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert_eq!(body["project"], "p1");
        assert_eq!(body["items"].as_array().unwrap().len(), 1, "a batch of one");
        assert_eq!(
            body["items"][0]["line"], 40,
            "for where the cursor came to rest"
        );
        assert_eq!(body["items"][0]["sha"], SHA);
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("data-pane-context"), "{html}");
        assert!(html.contains("above") && html.contains("below"));
    }

    /// The cache is the viewer's: an answer is kept, so going back to a finding asks nothing.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_finding_seen_before_is_answered_from_the_cache_without_a_request() {
        let server = serve("ok", 0).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        REQUESTS.with(|c| c.set(0));
        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        pump(&mut vdom, 500).await;
        assert_eq!(REQUESTS.with(std::cell::Cell::get), 1);
        let probe = PROBE.with(|p| p.borrow().expect("probe"));
        assert_eq!(
            probe.cache.peek().context_len(),
            1,
            "kept in the viewer's cache"
        );

        move_to(&vdom, 1);
        pump(&mut vdom, 400).await;
        assert_eq!(REQUESTS.with(std::cell::Cell::get), 2);
        move_to(&vdom, 0);
        let _ = vdom.render_immediate_to_vec();
        let at_once = dioxus_ssr::render(&vdom);
        assert!(
            at_once.contains("data-pane-context"),
            "a cached window needs no wait"
        );
        pump(&mut vdom, 400).await;
        assert_eq!(REQUESTS.with(std::cell::Cell::get), 2, "and no request");
    }

    /// The viewer and the pane share: a whole file the viewer holds answers the pane at once,
    /// with the lines of the pane's finding (never another finding's), and costs no request.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_file_the_viewer_holds_answers_the_pane_without_a_request() {
        let server = serve("ok", 0).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        REQUESTS.with(|c| c.set(0));
        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        let probe = PROBE.with(|p| p.borrow().expect("probe"));
        // A 60 line file whose line n reads `code at n`, as the viewer would have cached it.
        let file = SourceFileView {
            status: "ok".into(),
            source: "git".into(),
            total_lines: 60,
            window: Some(WindowView {
                from: 1,
                to: 60,
                total_lines: 60,
            }),
            lines: (1..=60)
                .map(|n| vec![(None, format!("code at {n}"))])
                .collect(),
            ..SourceFileView::default()
        };
        vdom.in_runtime(|| {
            let mut cache = probe.cache;
            cache.write().put(
                FileKey {
                    repo: "me/api".into(),
                    sha: SHA.into(),
                    path: "src/a.rs".into(),
                },
                std::sync::Arc::new(file),
            );
        });
        let _ = vdom.render_immediate_to_vec();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("data-pane-context"), "{html}");
        assert!(
            html.contains("code at 7") && html.contains("code at 13"),
            "{html}"
        );
        pump(&mut vdom, 400).await;
        assert_eq!(
            REQUESTS.with(std::cell::Cell::get),
            0,
            "the held file answered"
        );
        assert_eq!(
            probe.cache.peek().context_len(),
            0,
            "and nothing was written"
        );
    }

    /// An answer for a finding the cursor has left is dropped, however late it arrives.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_stale_answer_for_a_finding_the_cursor_left_is_dropped() {
        let server = serve("ok", 500).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        // Finding 10 settles at ~150 ms and its answer is due at ~650 ms.
        pump(&mut vdom, 250).await;
        move_to(&vdom, 1);
        // Finding 20 settles at ~400 ms; its own answer is due at ~900 ms.
        pump(&mut vdom, 500).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("code at 20"), "the pane is on finding 20");
        assert!(
            !html.contains("data-pane-context"),
            "finding 10's answer must not be drawn under finding 20: {html}"
        );
        pump(&mut vdom, 800).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("data-pane-context"),
            "finding 20's own answer: {html}"
        );
        let hit = html.find("finding-ctx-line-hit").unwrap();
        assert!(html[hit..].contains("code at 20"));
    }

    /// B.3 check 2, on the mounted pane: after the request has been made and answered with any
    /// status but `ok`, the pane is byte-identical to the one that never asked.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_refusal_leaves_the_mounted_pane_byte_identical() {
        for status in [
            "file_missing",
            "commit_unavailable",
            "source_deleted",
            "unavailable",
        ] {
            let server = serve(status, 0).await;
            let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
            REQUESTS.with(|c| c.set(0));
            let mut vdom = VirtualDom::new(app);
            vdom.rebuild_in_place();
            let before = dioxus_ssr::render(&vdom);
            pump(&mut vdom, 500).await;
            assert_eq!(
                REQUESTS.with(std::cell::Cell::get),
                1,
                "{status}: it did ask"
            );
            assert_eq!(dioxus_ssr::render(&vdom), before, "{status}");
        }
    }

    /// A server that is not there is a refusal too.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_unreachable_server_leaves_the_pane_as_it_was() {
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", "http://127.0.0.1:1");
        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        let before = dioxus_ssr::render(&vdom);
        pump(&mut vdom, 500).await;
        assert_eq!(dioxus_ssr::render(&vdom), before);
    }
}
