//! Inspection findings become units of work (roadmap 2c, gap 6), in the console.
//!
//! Two pieces:
//!
//! - [`RemediationPlanPanel`], mounted under the export panel on the inspection results. It
//!   posts the operator's current triage to `POST /api/projects/:id/remediation/plan`, shows the
//!   preview (one unit per violated rule, split and ordered by the planner) and, on a second
//!   click, creates the units (`.../remediation/create`). Confirmed findings become work; false
//!   positives and ignored findings never do; tech debt only when ticked; unresolved findings are
//!   left out until the operator confirms them.
//! - [`FindingsSourceSection`], mounted in a unit's Development panel when the unit came from
//!   findings. It lists the findings the unit closes with what the latest delta retest said
//!   about each, and offers "Rescan changes for {repo}".
//!
//! The wire shapes are the canonical `camerata_api_types::uow` ones; the plan and create
//! responses are small structs here. The text builders are pure so they are unit-tested without
//! a VirtualDom.

use std::collections::HashMap;

use super::*;

pub(super) use camerata_api_types::uow::{
    FindingsSource as FindingsSourceView, RetestEvidence as RetestEvidenceView,
    SourceFinding as SourceFindingView, UowSource as UowSourceView,
};

/// One planned unit in the preview.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(super) struct PlanUnitView {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub rule_ids: Vec<String>,
    #[serde(default)]
    pub area: Option<String>,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub findings: Vec<SourceFindingView>,
    #[serde(default)]
    pub files: usize,
    /// Indexes of the units (in this list) this one should land after.
    #[serde(default)]
    pub depends_on_index: Vec<usize>,
    #[serde(default)]
    pub already_covered_by: Option<String>,
    /// The covering unit as the console names it (`uow-3`).
    #[serde(default)]
    pub already_covered_label: Option<String>,
}

/// How many findings the plan left out, and why.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(super) struct SkippedView {
    #[serde(default)]
    pub false_positive: usize,
    #[serde(default)]
    pub ignored: usize,
    #[serde(default)]
    pub tech_debt: usize,
    #[serde(default)]
    pub unresolved: usize,
}

/// The `POST .../remediation/plan` response.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(super) struct PlanView {
    #[serde(default)]
    pub inspection_id: String,
    #[serde(default)]
    pub units: Vec<PlanUnitView>,
    #[serde(default)]
    pub skipped: SkippedView,
    #[serde(default)]
    pub notes: Vec<String>,
    /// The client-engagement lock's text when these units cannot run yet.
    #[serde(default)]
    pub development_lock: Option<String>,
}

/// The `POST .../remediation/create` response.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(super) struct CreatedView {
    #[serde(default)]
    pub created: Vec<serde_json::Value>,
    #[serde(default)]
    pub development_lock: Option<String>,
}

impl PlanView {
    /// How many units a click on "Create" would make.
    pub(super) fn new_units(&self) -> usize {
        self.units
            .iter()
            .filter(|u| u.already_covered_by.is_none())
            .count()
    }
}

/// "Create 1 unit of work" or "Create 3 units of work".
pub(super) fn create_label(n: usize) -> String {
    if n == 1 {
        "Create 1 unit of work".to_string()
    } else {
        format!("Create {n} units of work")
    }
}

/// The toast after creating: "Created 1 unit of work." or "Created 3 units of work." The unit
/// list is the console's own and refreshes when the console is opened, so the toast says to open it.
pub(super) fn created_text(n: usize) -> String {
    let what = if n == 1 {
        "1 unit of work".to_string()
    } else {
        format!("{n} units of work")
    };
    format!(
        "Created {what}. Open the console to run {}.",
        if n == 1 { "it" } else { "them" }
    )
}

/// The status column: "new", or "already uow-N" for a unit the findings already belong to.
pub(super) fn status_text(unit: &PlanUnitView) -> String {
    match (&unit.already_covered_label, &unit.already_covered_by) {
        (Some(label), _) => format!("already {label}"),
        (None, Some(id)) => format!("already {id}"),
        (None, None) => "new".to_string(),
    }
}

/// The After column: the rows (1-based) this unit should land after, or a dash.
pub(super) fn after_text(unit: &PlanUnitView) -> String {
    if unit.depends_on_index.is_empty() {
        return "none".to_string();
    }
    unit.depends_on_index
        .iter()
        .map(|i| format!("#{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The line under the preview that says what was left out.
pub(super) fn skipped_text(s: &SkippedView) -> String {
    format!(
        "Skipped: {} false positive, {} ignored, {} tech debt (tick Include tech debt to add them), {} unresolved.",
        s.false_positive, s.ignored, s.tech_debt, s.unresolved
    )
}

/// The request body both routes take: the operator's current triage and the tech debt tick.
pub(super) fn request_body(
    dispositions: &HashMap<String, Disposition>,
    include_tech_debt: bool,
) -> serde_json::Value {
    serde_json::json!({
        "dispositions": dispositions,
        "include_tech_debt": include_tech_debt,
    })
}

async fn post_remediation<T: serde::de::DeserializeOwned>(
    project_id: &str,
    action: &str,
    dispositions: &HashMap<String, Disposition>,
    include_tech_debt: bool,
) -> Result<T, String> {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/remediation/{action}",
            crate::bff_base(),
            enc_seg(project_id)
        ))
        .json(&request_body(dispositions, include_tech_debt))
        .send()
        .await
        .map_err(|_| "Could not reach the server to plan the remediation work.".to_string())?;
    if !resp.status().is_success() {
        return Err(
            super::uow::rejection_message(resp, "The remediation work was not planned").await,
        );
    }
    resp.json::<T>()
        .await
        .map_err(|_| "The server's answer could not be read.".to_string())
}

/// `POST /api/projects/:id/remediation/plan`: the preview. Creates nothing.
pub(super) async fn fetch_plan(
    project_id: &str,
    dispositions: &HashMap<String, Disposition>,
    include_tech_debt: bool,
) -> Result<PlanView, String> {
    post_remediation(project_id, "plan", dispositions, include_tech_debt).await
}

/// `POST /api/projects/:id/remediation/create`: one native unit per planned group.
pub(super) async fn create_units(
    project_id: &str,
    dispositions: &HashMap<String, Disposition>,
    include_tech_debt: bool,
) -> Result<CreatedView, String> {
    post_remediation(project_id, "create", dispositions, include_tech_debt).await
}

/// The remediation panel under the export panel on the inspection results.
#[component]
pub(super) fn RemediationPlanPanel(
    project_id: String,
    dispositions: Signal<HashMap<String, Disposition>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut include_tech_debt = use_signal(|| false);
    let mut plan = use_signal(|| None::<PlanView>);
    let mut busy = use_signal(|| false);

    let planning_id = project_id.clone();
    let create_id = project_id;
    rsx! {
        div { class: "audit-export-panel remediation-panel",
            p { class: "section-label", "Remediation work" }
            p { class: "section-hint",
                "Confirmed findings become units of work in the development console, one per rule. False positives and ignored findings are never included. Unresolved findings are left out until you confirm them."
            }
            div { class: "audit-model-row",
                label { class: "audit-model-label", r#for: "remediation-tech-debt", "Include tech debt" }
                input {
                    id: "remediation-tech-debt",
                    r#type: "checkbox",
                    checked: include_tech_debt(),
                    onchange: move |e| {
                        include_tech_debt.set(e.checked());
                        // A different tick is a different plan.
                        plan.set(None);
                    },
                }
            }
            button {
                class: "btn-run",
                disabled: busy(),
                onclick: move |_| {
                    let pid = planning_id.clone();
                    let triage = dispositions.read().clone();
                    let tech_debt = include_tech_debt();
                    busy.set(true);
                    spawn(async move {
                        match fetch_plan(&pid, &triage, tech_debt).await {
                            Ok(p) => plan.set(Some(p)),
                            Err(message) => {
                                plan.set(None);
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, message);
                            }
                        }
                        busy.set(false);
                    });
                },
                "Plan remediation work"
            }
            if let Some(p) = plan() {
                if p.units.is_empty() {
                    p { class: "section-hint",
                        "No confirmed findings to plan. Confirm findings in the review first."
                    }
                } else {
                    table { class: "governance-events-table remediation-plan-table",
                        thead {
                            tr {
                                th { "Unit" }
                                th { "Repository" }
                                th { "Branch" }
                                th { "Rules" }
                                th { "Severity" }
                                th { "Findings" }
                                th { "Files" }
                                th { "After" }
                                th { "Status" }
                            }
                        }
                        tbody {
                            for (index , u) in p.units.iter().cloned().enumerate() {
                                tr { key: "{index}", class: "governance-event-row",
                                    td { "#{index + 1} {u.title}" }
                                    td { "{u.repo}" }
                                    td { "{u.branch}" }
                                    td { {u.rule_ids.join(", ")} }
                                    td { "{u.severity}" }
                                    td { "{u.findings.len()}" }
                                    td { "{u.files}" }
                                    td { {after_text(&u)} }
                                    td { {status_text(&u)} }
                                }
                            }
                        }
                    }
                    for note in p.notes.iter() {
                        p { class: "section-hint", "{note}" }
                    }
                }
                p { class: "section-hint", {skipped_text(&p.skipped)} }
                if let Some(lock) = p.development_lock.clone() {
                    p { class: "section-hint remediation-lock-notice", "{lock}" }
                }
                if p.new_units() > 0 {
                    button {
                        class: "btn-run",
                        disabled: busy(),
                        onclick: move |_| {
                            let pid = create_id.clone();
                            let triage = dispositions.read().clone();
                            let tech_debt = include_tech_debt();
                            busy.set(true);
                            spawn(async move {
                                match create_units(&pid, &triage, tech_debt).await {
                                    Ok(done) => {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Info,
                                            created_text(done.created.len()),
                                        );
                                        // The units exist now; the preview would call them all covered.
                                        plan.set(None);
                                    }
                                    Err(message) => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, message);
                                    }
                                }
                                busy.set(false);
                            });
                        },
                        {create_label(p.new_units())}
                    }
                }
            }
        }
    }
}

// ── The unit's own view of the findings it closes ───────────────────────────────

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// What the latest delta retest said about the finding with `fingerprint`: "gone at {sha}",
/// "still present at {sha}", "not rechecked", or "not retested yet" before any retest. A finding
/// the retest lists as both gone and present (two findings sharing a fingerprint) reads as
/// still present.
pub(super) fn finding_status(fingerprint: &str, retests: &[RetestEvidenceView]) -> String {
    let Some(latest) = retests.last() else {
        return "not retested yet".to_string();
    };
    let sha = short_sha(&latest.to_sha);
    let has = |list: &[String]| list.iter().any(|f| f == fingerprint);
    if has(&latest.still_open) {
        match latest.moved.get(fingerprint) {
            Some(path) => format!("still present at {sha}, moved to {path}"),
            None => format!("still present at {sha}"),
        }
    } else if has(&latest.resolved) {
        format!("gone at {sha}")
    } else if has(&latest.not_rechecked) {
        "not rechecked".to_string()
    } else {
        "not retested yet".to_string()
    }
}

/// One line of the retest history.
pub(super) fn retest_line(r: &RetestEvidenceView) -> String {
    let total = r.resolved.len() + r.still_open.len() + r.not_rechecked.len();
    let mut line = format!(
        "Delta retest at {}: {} of {total} findings gone, {} still present.",
        short_sha(&r.to_sha),
        r.resolved.len(),
        r.still_open.len()
    );
    if !r.moved.is_empty() {
        line.push_str(&format!(" {} moved to another file.", r.moved.len()));
    }
    if !r.not_rechecked.is_empty() {
        line.push_str(&format!(" {} not rechecked.", r.not_rechecked.len()));
    }
    if r.ai_incomplete {
        line.push_str(" The AI review did not finish, so semantic findings were not rechecked.");
    }
    if r.rules_changed {
        line.push_str(" The rule selection changed since the inspection.");
    }
    line
}

/// The project a rescan runs against: the unit's own, else the active one.
pub(super) fn rescan_project_id(
    unit_project: Option<&str>,
    active: Option<String>,
) -> Option<String> {
    unit_project
        .filter(|p| !p.trim().is_empty())
        .map(str::to_string)
        .or(active)
}

/// The findings a unit closes, their retest status, the retest history and the rescan button.
#[component]
pub(super) fn FindingsSourceSection(
    source: FindingsSourceView,
    retests: Vec<RetestEvidenceView>,
    repo: String,
    story_id: String,
    /// The project the unit belongs to (`UnitOfWork::project_id`). The rescan runs against it,
    /// not against whichever project is active; a unit that records none falls back to the
    /// active project.
    #[props(default)]
    project_id: Option<String>,
    mut uow_refresh: Signal<u32>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let project = use_resource(super::fetch_active_project);
    let mut busy = use_signal(|| false);
    let rescan_label = format!("Rescan changes for {repo}");
    let retest_count = retests.len();

    rsx! {
        div { class: "uow-dev-section findings-source-section",
            p { class: "uow-dev-section-h", "Findings this unit closes" }
            p { class: "section-hint",
                "From inspection {source.inspection_id}. A finding counts as closed when the next delta retest no longer reports it."
            }
            table { class: "governance-events-table",
                thead {
                    tr {
                        th { "File" }
                        th { "Line" }
                        th { "Rule" }
                        th { "Severity" }
                        th { "Finding" }
                        th { "Retest" }
                    }
                }
                tbody {
                    for (index , f) in source.findings.iter().cloned().enumerate() {
                        tr { key: "{index}", class: "governance-event-row",
                            td { "{f.path}" }
                            td { "{f.line}" }
                            td { "{f.rule_id}" }
                            td { "{f.severity}" }
                            td { "{f.fingerprint}" }
                            td { {finding_status(&f.fingerprint, &retests)} }
                        }
                    }
                }
            }
            if !repo.is_empty() {
                button {
                    class: "btn-run",
                    disabled: busy(),
                    onclick: {
                        let repo = repo.clone();
                        let story_id = story_id.clone();
                        let unit_project = project_id.clone();
                        move |_| {
                            let active = project.read().clone().flatten().map(|p| p.id);
                            let Some(project_id) = rescan_project_id(unit_project.as_deref(), active) else {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Select a project before rescanning.",
                                );
                                return;
                            };
                            let (repo, story_id) = (repo.clone(), story_id.clone());
                            busy.set(true);
                            spawn(async move {
                                // The rescan is an audit job and this task polls it to its verdict
                                // (up to five minutes): hold the Bombe for all of it.
                                let _guard = crate::loading::LoadingGuard::new();
                                match crate::source_ui::rescan_changes(&project_id, &repo).await {
                                    Ok(crate::source_ui::RescanStarted::NothingChanged) => {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Info,
                                            "Nothing has changed since the inspection, so there is nothing to rescan.",
                                        );
                                    }
                                    Ok(crate::source_ui::RescanStarted::Job { header, .. }) => {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Info,
                                            format!("{header} started. This unit updates when it finishes."),
                                        );
                                        // The retest runs as a job; poll the unit until its verdict
                                        // lands. The button is free again after the first look, so
                                        // a long retest does not hold it.
                                        for round in 0..60 {
                                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                            if round == 0 {
                                                busy.set(false);
                                            }
                                            let seen = super::uow::fetch_uow(&story_id)
                                                .await
                                                .map_or(0, |u| u.retests.len());
                                            if seen > retest_count {
                                                uow_refresh += 1;
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Error,
                                            e.message,
                                        );
                                    }
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "{rescan_label}"
                }
            }
            if !retests.is_empty() {
                p { class: "uow-dev-section-h", "Retest history" }
                for (index , r) in retests.iter().cloned().enumerate().rev() {
                    p { key: "{index}", class: "section-hint",
                        "{r.recorded}: {retest_line(&r)}"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(covered: Option<&str>) -> PlanUnitView {
        PlanUnitView {
            title: "Fix A-1".into(),
            already_covered_by: covered.map(|_| "uow-x".to_string()),
            already_covered_label: covered.map(str::to_string),
            ..PlanUnitView::default()
        }
    }

    #[test]
    fn wording_matches_the_spec() {
        assert_eq!(create_label(1), "Create 1 unit of work");
        assert_eq!(create_label(3), "Create 3 units of work");
        assert_eq!(status_text(&unit(None)), "new");
        assert_eq!(status_text(&unit(Some("uow-4"))), "already uow-4");
        let skipped = SkippedView {
            false_positive: 2,
            ignored: 1,
            tech_debt: 3,
            unresolved: 4,
        };
        assert_eq!(
            skipped_text(&skipped),
            "Skipped: 2 false positive, 1 ignored, 3 tech debt (tick Include tech debt to add them), 4 unresolved."
        );
        let mut u = unit(None);
        assert_eq!(after_text(&u), "none");
        u.depends_on_index = vec![0, 2];
        assert_eq!(after_text(&u), "#1, #3");
    }

    #[test]
    fn a_finding_reads_its_status_from_the_latest_retest() {
        let first = RetestEvidenceView {
            to_sha: "1111111aaaa".into(),
            still_open: vec!["fp-a".into()],
            ..RetestEvidenceView::default()
        };
        let latest = RetestEvidenceView {
            to_sha: "2222222bbbb".into(),
            resolved: vec!["fp-a".into()],
            still_open: vec!["fp-b".into()],
            not_rechecked: vec!["fp-c".into()],
            rules_changed: true,
            ..RetestEvidenceView::default()
        };
        let both = [first.clone(), latest.clone()];
        assert_eq!(finding_status("fp-a", &both), "gone at 2222222");
        assert_eq!(finding_status("fp-b", &both), "still present at 2222222");
        assert_eq!(finding_status("fp-c", &both), "not rechecked");
        assert_eq!(finding_status("fp-a", &[]), "not retested yet");
        assert_eq!(finding_status("fp-z", &both), "not retested yet");
        assert_eq!(
            retest_line(&latest),
            "Delta retest at 2222222: 1 of 3 findings gone, 1 still present. 1 not rechecked. The rule selection changed since the inspection."
        );
        assert_eq!(
            retest_line(&RetestEvidenceView {
                to_sha: "abc".into(),
                resolved: vec!["a".into()],
                still_open: vec!["b".into(), "c".into()],
                ..RetestEvidenceView::default()
            }),
            "Delta retest at abc: 1 of 3 findings gone, 2 still present."
        );
    }

    #[test]
    fn a_moved_or_unrechecked_finding_says_so() {
        let mut moved = std::collections::BTreeMap::new();
        moved.insert("fp-m".to_string(), "src/new.rs".to_string());
        let latest = RetestEvidenceView {
            to_sha: "2222222bbbb".into(),
            still_open: vec!["fp-m".into()],
            moved,
            ai_incomplete: true,
            not_rechecked: vec!["fp-s".into()],
            ..RetestEvidenceView::default()
        };
        let all = [latest.clone()];
        assert_eq!(
            finding_status("fp-m", &all),
            "still present at 2222222, moved to src/new.rs"
        );
        assert_eq!(
            retest_line(&latest),
            "Delta retest at 2222222: 0 of 2 findings gone, 1 still present. 1 moved to another file. 1 not rechecked. The AI review did not finish, so semantic findings were not rechecked."
        );
    }

    #[test]
    fn the_toast_after_creating_is_singular_for_one_unit() {
        assert_eq!(
            created_text(1),
            "Created 1 unit of work. Open the console to run it."
        );
        assert_eq!(
            created_text(3),
            "Created 3 units of work. Open the console to run them."
        );
    }

    #[test]
    fn a_rescan_runs_against_the_units_own_project() {
        assert_eq!(
            rescan_project_id(Some("proj-unit"), Some("proj-active".into())).as_deref(),
            Some("proj-unit")
        );
        assert_eq!(
            rescan_project_id(None, Some("proj-active".into())).as_deref(),
            Some("proj-active")
        );
        assert_eq!(
            rescan_project_id(Some(" "), Some("proj-active".into())).as_deref(),
            Some("proj-active")
        );
        assert_eq!(rescan_project_id(None, None), None);
    }

    #[test]
    fn the_wording_has_no_em_dash() {
        for text in [
            create_label(2),
            skipped_text(&SkippedView::default()),
            retest_line(&RetestEvidenceView {
                rules_changed: true,
                ..RetestEvidenceView::default()
            }),
        ] {
            assert!(!text.contains('\u{2014}'), "{text}");
        }
    }

    fn app() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        let dispositions = use_signal(HashMap::new);
        rsx! {
            RemediationPlanPanel { project_id: "p1".to_string(), dispositions }
        }
    }

    #[test]
    fn the_panel_renders_its_title_hint_checkbox_and_plan_button() {
        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Remediation work"), "{html}");
        assert!(html.contains("Plan remediation work"), "{html}");
        assert!(html.contains("Include tech debt"), "{html}");
        assert!(html.contains("remediation-tech-debt"), "{html}");
        assert!(
            html.contains(
                "Confirmed findings become units of work in the development console, one per rule."
            ),
            "{html}"
        );
        assert!(
            !html.contains("Create "),
            "no create button before a plan: {html}"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn plan_panel_posts_the_current_triage_and_renders_the_preview_counts() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/remediation/plan"))
            .and(body_partial_json(serde_json::json!({
                "include_tech_debt": true,
                "dispositions": { "acme/api\u{0}A-1\u{0}src/a.rs\u{0}3\u{0}x": { "state": "Confirmed" } }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "inspection_id": "insp-1",
                "units": [
                    { "title": "Fix A-1", "repo": "acme/api", "branch": "camerata/remediation/a-1",
                      "rule_ids": ["A-1"], "severity": "high", "files": 1,
                      "findings": [ { "path": "src/a.rs", "line": 3, "rule_id": "A-1" } ],
                      "depends_on_index": [], "already_covered_by": null },
                    { "title": "Fix B-2", "repo": "acme/api", "branch": "camerata/remediation/b-2",
                      "rule_ids": ["B-2"], "severity": "medium", "files": 1, "findings": [],
                      "depends_on_index": [0], "already_covered_by": "uow-q",
                      "already_covered_label": "uow-7" }
                ],
                "skipped": { "false_positive": 1, "ignored": 2, "tech_debt": 0, "unresolved": 3 },
                "notes": [],
                "development_lock": "This is a client engagement."
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut triage = HashMap::new();
        triage.insert(
            "acme/api\u{0}A-1\u{0}src/a.rs\u{0}3\u{0}x".to_string(),
            Disposition {
                state: camerata_ui_core::triage::TriageState::Confirmed,
                ..Disposition::default()
            },
        );
        let plan = fetch_plan("p1", &triage, true).await.expect("a plan");
        assert_eq!(plan.units.len(), 2);
        assert_eq!(plan.new_units(), 1, "the covered unit is not created");
        assert_eq!(status_text(&plan.units[1]), "already uow-7");
        assert_eq!(after_text(&plan.units[1]), "#1");
        assert_eq!(
            skipped_text(&plan.skipped),
            "Skipped: 1 false positive, 2 ignored, 0 tech debt (tick Include tech debt to add them), 3 unresolved."
        );
        assert_eq!(
            plan.development_lock.as_deref(),
            Some("This is a client engagement.")
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn create_button_names_the_unit_count_and_shows_the_lock_notice_when_present() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/remediation/create"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "created": [ { "uow_id": "uow-a" }, { "uow_id": "uow-b" } ],
                "development_lock": "Opt in to client tooling in Settings."
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let done = create_units("p1", &HashMap::new(), false)
            .await
            .expect("created");
        assert_eq!(done.created.len(), 2);
        assert_eq!(create_label(done.created.len()), "Create 2 units of work");
        assert_eq!(
            done.development_lock.as_deref(),
            Some("Opt in to client tooling in Settings.")
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_refused_plan_surfaces_the_servers_sentence() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": "This project has no recorded inspection. Run an audit first."
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let err = fetch_plan("p1", &HashMap::new(), false).await.unwrap_err();
        assert_eq!(
            err,
            "This project has no recorded inspection. Run an audit first."
        );
    }

    #[test]
    fn the_findings_section_lists_each_finding_with_its_retest_status() {
        fn section() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            let uow_refresh = use_signal(|| 0u32);
            let source = FindingsSourceView {
                inspection_id: "insp-1".into(),
                findings: vec![SourceFindingView {
                    path: "src/db.rs".into(),
                    line: 42,
                    rule_id: "A-1".into(),
                    severity: "high".into(),
                    fingerprint: "fp-1".into(),
                    ..SourceFindingView::default()
                }],
                ..FindingsSourceView::default()
            };
            let retests = vec![RetestEvidenceView {
                to_sha: "abcdef0123".into(),
                resolved: vec!["fp-1".into()],
                recorded: "2026-10-28T10:00:00Z".into(),
                ..RetestEvidenceView::default()
            }];
            rsx! {
                FindingsSourceSection {
                    source,
                    retests,
                    repo: "acme/api".to_string(),
                    story_id: "uow-1".to_string(),
                    project_id: Some("proj-1".to_string()),
                    uow_refresh,
                }
            }
        }
        let mut vdom = VirtualDom::new(section);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Findings this unit closes"), "{html}");
        assert!(html.contains("src/db.rs"), "{html}");
        assert!(html.contains("gone at abcdef0"), "{html}");
        assert!(html.contains("Rescan changes for acme/api"), "{html}");
        assert!(
            html.contains("Delta retest at abcdef0: 1 of 1 findings gone, 0 still present."),
            "{html}"
        );
    }
}
