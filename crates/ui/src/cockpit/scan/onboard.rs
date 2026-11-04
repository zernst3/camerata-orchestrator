//! The onboarding view and the greenfield form and result.

use super::*;

/// Which onboarding path the user is setting up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::cockpit) enum OnboardPath {
    /// Install governance into an EXISTING repo (scan → propose → audit → arm).
    Brownfield,
    /// Scaffold a NEW repo with the rules baked in from commit zero.
    Greenfield,
}

/// The repo-onboarding ENTRY POINT: bring a repo new to Camerata under
/// governance. Brownfield (existing repo) and greenfield (new repo) are the two
/// paths. This is distinct from a story's Investigation phase — onboarding sets
/// up the REPO's rules + CI gate; Investigation is per-STORY refinement.
///
/// Connection-gated and honest: the scan/audit/arm engine runs against GitHub, so
/// the actionable steps light up once a GitHub token is connected. Until then it
/// explains exactly what each step will do.
/// The outcome of trying to derive a repo from a navigated-to folder.
pub(in crate::cockpit) enum RepoDetect {
    /// The user cancelled the dialog.
    Cancelled,
    /// Derived `owner/repo` AND the local folder it lives in (recorded as the repo's path).
    /// `onboarded_in` names the project that already onboarded this repo, if any (#50): onboarding
    /// is one-time, so the caller blocks a re-onboard and routes the user to the workspace.
    Found {
        repo: String,
        path: String,
        onboarded_in: Option<String>,
        /// What is limited for a folder that is not on GitHub (it can be inspected, but push and
        /// pull requests are GitHub-only). `None` for a GitHub repo.
        note: Option<String>,
    },
    /// Couldn't derive one — carries a human reason for a toast.
    Failed(String),
}

/// Let the user NAVIGATE to a local repo folder; derive its `owner/repo` from the git
/// origin remote (server-side).
pub(in crate::cockpit) async fn detect_local_repo() -> RepoDetect {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_title("Choose a local repo folder")
        .pick_folder()
        .await
    else {
        return RepoDetect::Cancelled;
    };
    let path = folder.path().to_string_lossy().to_string();
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/git/detect-repo", crate::bff_base()))
        .json(&serde_json::json!({ "path": path }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return RepoDetect::Failed(format!("couldn't reach the local server ({e})")),
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return RepoDetect::Failed("unexpected response from the server".to_string()),
    };
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        match v.get("repo").and_then(|r| r.as_str()) {
            Some(r) => RepoDetect::Found {
                repo: r.to_string(),
                path,
                onboarded_in: v
                    .get("onboarded_project")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string()),
                note: v
                    .get("note")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string()),
            },
            None => RepoDetect::Failed("no repo in the response".to_string()),
        }
    } else {
        RepoDetect::Failed(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("could not detect a repo in that folder")
                .to_string(),
        )
    }
}

/// The result of a greenfield scaffold call, as returned by `POST /api/onboard/greenfield`.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(in crate::cockpit) struct GreenfieldScaffoldResult {
    pub ok: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub files_written: Vec<String>,
    #[serde(default)]
    pub commit_sha: String,
    #[serde(default)]
    pub message: String,
}

/// Resolve the adopted directive for a corpus rule: uses the default option's
/// directive, falling back to the rule title when options are absent or the default
/// is unset. Mirrors the resolve logic in the brownfield apply path.
pub(in crate::cockpit) fn resolve_gf_directive(r: &ProposedRuleView) -> String {
    if r.options.is_empty() {
        return r.title.clone();
    }
    r.default_option
        .as_ref()
        .and_then(|oid| r.options.iter().find(|o| &o.id == oid))
        .map(|o| o.directive.clone())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| r.title.clone())
}

/// Call `POST /api/onboard/greenfield` with the given name, local directory path,
/// and selected arm rules. Resolves each `ProposedRuleView` into an `ArmRuleReq`
/// (directive resolved from the default option). Returns `None` on network failure.
pub(in crate::cockpit) async fn scaffold_greenfield_api(
    name: &str,
    dest_path: &str,
    rules: &[ProposedRuleView],
) -> Option<GreenfieldScaffoldResult> {
    // Resolve each corpus rule to its ArmRuleReq shape (id + resolved directive).
    let arm_rules: Vec<ArmRuleReq> = rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .map(|r| ArmRuleReq {
            id: r.id.clone(),
            title: r.title.clone(),
            directive: resolve_gf_directive(r),
            option: r.default_option.clone(),
            enforcement: r.enforcement.clone(),
            scope: "repo-local".to_string(),
            repos: vec![name.to_string()],
        })
        .collect();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/greenfield", crate::bff_base()))
        .json(&serde_json::json!({
            "name": name,
            "path": dest_path,
            "rules": arm_rules,
        }))
        .send()
        .await
        .ok()?
        .json::<GreenfieldScaffoldResult>()
        .await
        .ok()
}

#[component]
pub(in crate::cockpit) fn OnboardView(connection: Option<ProviderView>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut path = use_signal(|| OnboardPath::Brownfield);
    let mut repo = use_signal(String::new);
    // The scan result lives in app-scope context so it survives leaving + returning to this
    // view (a running audit job keeps going server-side; the scan shouldn't vanish).
    let mut scan = use_context::<Signal<Option<ScanReportView>>>();
    let mut scanning = use_signal(|| false);
    let connected = connection.as_ref().map(|c| c.live).unwrap_or(false);
    // The part of the project's deleted-source block that touches the repos listed to scan.
    let scan_scope: Vec<String> = repo()
        .lines()
        .flat_map(|l| l.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let scan_blocked = crate::source_ui::use_scan_block()
        .and_then(|b| camerata_ui_core::readiness::scan_block_for(&b, &scan_scope));

    // Saved inspections of the open project: an inspection the app lost (it was closed, crashed
    // or the machine died) or that ended with a part left is offered here, on app start and when
    // a project is opened, with "Run what remains". Any interruption should cost minutes, never
    // the run. The plan preview, the resume and the Bombe are the ones the incomplete-scan banner
    // uses; this is the way in from a screen that has no report yet.
    let mut saved_epoch = use_signal(|| 0u32);
    let saved_res = use_resource(move || async move {
        let _ = saved_epoch();
        super::saved_inspections::fetch_saved_inspections().await
    });
    let saved: Vec<camerata_ui_core::saved_inspections::SavedInspection> =
        saved_res.read().clone().unwrap_or_default();
    let mut saved_plan = use_signal(|| Option::<(String, ResumePlanView)>::None);
    let mut saved_busy = use_signal(|| false);
    let mut saved_error = use_signal(|| Option::<String>::None);
    let mut active_audit_job = use_context::<Signal<Option<String>>>();
    // Captured here, in the render body, as `poll_job` requires (see BOMBE-SILENT-MISS there).
    let loading_count = use_context::<crate::loading::LoadingCount>();
    let saved_for_resume = saved.clone();
    let on_saved_plan = EventHandler::new(move |run_id: String| {
        saved_error.set(None);
        spawn(async move {
            let plan = match resume_plan(&run_id).await {
                Ok(plan) => plan,
                Err(why) => ResumePlanView {
                    text: String::new(),
                    refusal: Some(why),
                },
            };
            saved_plan.set(Some((run_id, plan)));
        });
    });
    let on_saved_resume = EventHandler::new(move |run_id: String| {
        let repos = saved_for_resume
            .iter()
            .find(|r| r.id == run_id)
            .map(|r| r.repos.clone())
            .unwrap_or_default();
        saved_plan.set(None);
        saved_error.set(None);
        saved_busy.set(true);
        spawn(async move {
            // The Bombe runs from the start request to the end of the job; the scan results
            // mount while it runs and pick the job up from `active_audit_job` (as they do for a
            // scan started before the view was left), so the report appears there.
            let result = run_what_remains(
                loading_count,
                || async { resume_start(&run_id).await },
                |jid| async move {
                    active_audit_job.set(Some(jid.clone()));
                    repo.set(repos.join("\n"));
                    if let Some(report) = scan_repos(&repos).await {
                        scan.set(Some(report));
                    }
                    super::saved_inspections::wait_for_audit_job(&jid).await;
                },
            )
            .await;
            saved_busy.set(false);
            saved_epoch += 1;
            if let Err(why) = result {
                saved_error.set(Some(format!("Cannot resume now: {why}")));
            }
        });
    });

    // Greenfield-specific state: the new repo's name, the local directory to create,
    // the selected corpus rules to bake in, the in-progress flag, and the scaffold result.
    // These signals are mutated inside the GreenfieldForm child component — the parent
    // holds them in scope so they survive path switches.
    let gf_name = use_signal(String::new);
    let gf_path = use_signal(String::new);
    // Set of corpus rule ids the user selected to bake into the new repo.
    let gf_selected_ids = use_signal(std::collections::BTreeSet::<String>::new);
    let gf_scaffolding = use_signal(|| false);
    let gf_result = use_signal(|| Option::<GreenfieldScaffoldResult>::None);
    // Load corpus rules for the greenfield picker (the full library, no scan needed).
    let corpus_res = use_resource(fetch_corpus_rules);
    let corpus_rules: Vec<ProposedRuleView> =
        corpus_res.read().clone().flatten().unwrap_or_default();

    // RESTORE a saved onboarding draft on first mount (issue #27): if there's no live scan
    // but a draft exists on disk, bring its scan back so the architect resumes exactly where
    // they left off — no re-scan. ScanResults then rehydrates its own selection/dispositions/
    // audit from the same draft.
    use_future(move || async move {
        if scan.peek().is_none() {
            if let Some(draft) = load_onboarding_draft().await {
                // Rehydrate the repos textarea too — otherwise it reloads empty (showing the
                // placeholder) even though the scan + rules restored, which reads as "the repos
                // were lost." The repo set is exactly the scanned repos.
                if repo.peek().trim().is_empty() {
                    repo.set(draft.scan.repos.join("\n"));
                }
                scan.set(Some(draft.scan));
            }
        }
    });

    let brownfield_cls = if path() == OnboardPath::Brownfield {
        "onboard-path on"
    } else {
        "onboard-path"
    };
    let greenfield_cls = if path() == OnboardPath::Greenfield {
        "onboard-path on"
    } else {
        "onboard-path"
    };

    // The flow steps differ slightly by path; both are gated on a connection.
    let steps: &[(&str, &str)] = match path() {
        OnboardPath::Brownfield => &[
            ("Point at the repo(s)", "Name the existing owner/repo(s) your token can reach, or browse to a local folder."),
            ("Scan + propose per-repo rules", "Camerata detects each repo's stack and proposes a starter ruleset per repo — you review, you don't author from scratch."),
            ("Pick rules", "Select rules per repo (project-level rules apply to all). Click a rule to read its options and choose an alternative."),
            ("Audit (optional) + triage", "Optionally scan the code against your selected rules + the security floor, then triage findings (Unresolved / Ignored / Tech debt). Not required to finish onboarding."),
            ("Add rules to repo(s)", "Write the governance files onto a camerata/onboard-governance branch in each local clone and push it — no PR (Open governance PR separately). Applying marks the repo onboarded."),
            ("Wire mechanical rules into CI", "The final step: add the selected mechanical rules to each repo's existing CI as enforced lint gates."),
        ],
        OnboardPath::Greenfield => &[
            ("Name the new repo", "Camerata scaffolds it with the rules baked in from commit zero."),
            ("Pick the starter ruleset", "Start from the corpus defaults for your stack; edit and approve."),
            ("Scaffold + arm", "Create the repo with CONVENTIONS.md/AGENTS.md, the CI gate, and the gate config already in place — governed from the first commit."),
        ],
    };

    rsx! {
        div { class: "onboard",
            div { class: "onboard-head",
                p { class: "onboard-title", "Onboard repos into governance" }
                p { class: "onboard-sub", "Bring a repo new to Camerata under the gate. This sets up the REPO's rules and CI enforcement — separate from a story's Investigation phase, which refines one piece of work." }
                // The inspection's start screen carries the "Guided inspection" switch.
                crate::walkthrough::GuidedSwitch {}
            }

            // Path chooser.
            div { class: "onboard-paths",
                button {
                    class: "{brownfield_cls}",
                    onclick: move |_| path.set(OnboardPath::Brownfield),
                    span { class: "onboard-path-h", "Brownfield" }
                    span { class: "onboard-path-d", "Install governance into an existing repo." }
                }
                button {
                    class: "{greenfield_cls}",
                    onclick: move |_| path.set(OnboardPath::Greenfield),
                    span { class: "onboard-path-h", "Greenfield" }
                    span { class: "onboard-path-d", "Scaffold a new repo, governed from commit zero." }
                }
            }

            // No GitHub gate: onboarding reads LOCAL code only. A token is only needed LATER
            // (development time) to push the governance branch / open a PR — surface that when
            // it's missing, but never block onboarding on it.
            if !connected {
                div { class: "onboard-note",
                    "Onboarding works on your local repo folders — no GitHub connection needed here. (A token is only needed later, to push the governance branch and open a PR.)"
                }
            }

            // ── Brownfield path: browse existing repos + scan ─────────────────
            if path() == OnboardPath::Brownfield {
                // A saved inspection to finish comes before starting another.
                if scan().is_none() {
                    super::saved_inspections::SavedInspectionsPanel {
                        saved: saved.clone(),
                        plan: saved_plan(),
                        busy: saved_busy(),
                        error: saved_error(),
                        on_plan: Some(on_saved_plan),
                        on_resume: Some(on_saved_resume),
                    }
                }
                div { class: "onboard-repo-block",
                    label { class: "onboard-repo-label", "Repositories — browse to each repo's local folder (a feature often spans several)" }
                    {
                        let names: Vec<String> = repo()
                            .lines()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        rsx! {
                            if names.is_empty() {
                                p { class: "onboard-repos-empty", "No repos yet — browse to a local repo folder to add one." }
                            } else {
                                div { class: "onboard-repos-list",
                                    for name in names {
                                        {
                                            let name_rm = name.clone();
                                            rsx! {
                                                div { class: "onboard-repo-chip", key: "{name}",
                                                    span { class: "onboard-repo-chip-name", "{name}" }
                                                    button {
                                                        class: "onboard-repo-chip-x",
                                                        title: "Remove",
                                                        onclick: move |_| {
                                                            let kept: Vec<String> = repo()
                                                                .lines()
                                                                .map(|s| s.trim().to_string())
                                                                .filter(|s| !s.is_empty() && s != &name_rm)
                                                                .collect();
                                                            repo.set(kept.join("\n"));
                                                        },
                                                        "\u{2715}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "btn-edit-sm onboard-browse",
                        "data-tour": camerata_ui_core::walkthrough::target::REPO_ADD,
                        onclick: move |_| {
                            spawn(async move {
                                match detect_local_repo().await {
                                    RepoDetect::Cancelled => {}
                                    // #50: block re-onboarding a repo that's already onboarded.
                                    RepoDetect::Found { repo: found, onboarded_in: Some(project), .. } => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{found} is already onboarded (project \u{201c}{project}\u{201d}). Onboarding is one-time, so add it to your workspace to work on it, instead of re-onboarding."));
                                    }
                                    RepoDetect::Found { repo: found, path: folder, note, .. } => {
                                        let saved = set_repo_path(&found, &folder).await;
                                        let mut cur = repo();
                                        let exists = cur.split([',', '\n']).any(|s| s.trim() == found);
                                        if exists {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{found} is already in the list."));
                                        } else {
                                            if !cur.trim().is_empty() && !cur.ends_with('\n') {
                                                cur.push('\n');
                                            }
                                            cur.push_str(&found);
                                            repo.set(cur);
                                            if saved {
                                                let limit = note.map(|n| format!(" {n}")).unwrap_or_default();
                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Added {found} ({folder}).{limit}"));
                                            } else {
                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Added {found}, but couldn't record its local path."));
                                            }
                                        }
                                    }
                                    RepoDetect::Failed(msg) => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Couldn't read that folder: {msg}. It must be a local git repository."));
                                    }
                                }
                            });
                        },
                        "Browse for a local repo folder\u{2026}"
                    }
                    p { class: "scan-section-sub", "Scans only the currently checked-out branch of each repo. Switch branches first if you want a different branch scanned." }
                    ScanReposButton {
                        repos: scan_scope.clone(),
                        scanning: scanning(),
                        blocked: scan_blocked.clone(),
                        on_scan: move |repos: Vec<String>| {
                            scanning.set(true);
                            spawn(async move {
                                clear_onboarding_draft().await;
                                scan.set(scan_repos(&repos).await);
                                scanning.set(false);
                            });
                        },
                    }
                }
            }

            // ── Greenfield path: name + directory + starter ruleset ────────────
            if path() == OnboardPath::Greenfield {
                GreenfieldForm {
                    gf_name,
                    gf_path,
                    gf_selected_ids,
                    gf_scaffolding,
                    gf_result,
                    corpus_rules: corpus_rules.clone(),
                    toasts,
                }
            }

            // Brownfield-only: scan results + flow steps.
            if path() == OnboardPath::Brownfield {
                // Scan results: the audit findings + proposed-rules tables (chorale).
                if let Some(report) = scan() {
                    {
                        // Prefer the explicit server-set `blocked` flag; the shape heuristic is
                        // kept only as a back-compat fallback for any report serialized before the
                        // flag existed (it never sets `blocked`, so `false || heuristic`).
                        let looks_blocked = report.blocked || scan_report_looks_blocked(&report);
                        rsx! {
                            if report.gated || looks_blocked {
                                div { class: "onboard-gate",
                                    span { class: "onboard-gate-dot" }
                                    div {
                                        p { class: "onboard-gate-h",
                                            if report.gated { "Scan not run" } else { "Scan blocked" }
                                        }
                                        p { class: "onboard-gate-b", "{report.message.clone().unwrap_or_default()}" }
                                        if looks_blocked {
                                            p { class: "onboard-gate-hint",
                                                "Add an Anthropic API key in Settings, or enable \u{201c}Allow Claude CLI (personal subscription)\u{201d} for this project, to run this scan."
                                            }
                                        }
                                    }
                                }
                            } else {
                                {
                                    // Key by the SCAN's identity (repo set + proposed-rule count) so a
                                    // RE-SCAN remounts ScanResults/ProposedRulesTable with fresh rows and
                                    // a fresh "recommended -> selected" pass.
                                    let scan_key = format!(
                                        "{}|{}",
                                        report.repos.join(","),
                                        report.proposed_rules.len()
                                    );
                                    rsx! { ScanResults { key: "{scan_key}", report } }
                                }
                            }
                        }
                    }
                }

                // The flow (shown until a scan has run).
                if scan().is_none() {
                    div { class: "onboard-steps",
                        for (i , (h , b)) in steps.iter().enumerate() {
                            div { class: "onboard-step",
                                span { class: "onboard-step-n", "{i + 1}" }
                                div {
                                    p { class: "onboard-step-h", "{h}" }
                                    p { class: "onboard-step-b", "{b}" }
                                }
                            }
                        }
                    }
                }
            }

            // Greenfield: flow steps (shown until scaffolding, handled inside GreenfieldForm).
            if path() == OnboardPath::Greenfield && gf_result().is_none() && !gf_scaffolding() {
                div { class: "onboard-steps",
                    for (i , (h , b)) in steps.iter().enumerate() {
                        div { class: "onboard-step",
                            span { class: "onboard-step-n", "{i + 1}" }
                            div {
                                p { class: "onboard-step-h", "{h}" }
                                p { class: "onboard-step-b", "{b}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The brownfield "Scan repos" start button. Scanning reads the repos' source, so it is off for
/// any listed repo whose source was deleted from this machine, with the way back beside it.
#[component]
pub(super) fn ScanReposButton(
    repos: Vec<String>,
    scanning: bool,
    blocked: Option<camerata_ui_core::readiness::ScanBlock>,
    on_scan: EventHandler<Vec<String>>,
) -> Element {
    rsx! {
        if let Some(block) = blocked.clone() {
            crate::source_ui::ScanBlockNotice { block }
        }
        button {
            class: "onboard-cta",
            disabled: repos.is_empty() || scanning || blocked.is_some(),
            onclick: {
                let repos = repos.clone();
                move |_| {
                    if repos.is_empty() {
                        return;
                    }
                    on_scan.call(repos.clone());
                }
            },
            if scanning { "Scanning\u{2026}" } else { "Scan repos" }
        }
    }
}

/// Greenfield onboarding form: name the new repo, pick a local directory, select
/// starter rules from the corpus, and scaffold the repo with governance baked in
/// from commit zero.
///
/// Reuses the same arm emit path as brownfield apply — the governance files emitted
/// here are identical in structure to what a brownfield onboarding would write.
#[allow(clippy::too_many_arguments)]
#[component]
pub(in crate::cockpit) fn GreenfieldForm(
    mut gf_name: Signal<String>,
    mut gf_path: Signal<String>,
    mut gf_selected_ids: Signal<std::collections::BTreeSet<String>>,
    mut gf_scaffolding: Signal<bool>,
    mut gf_result: Signal<Option<GreenfieldScaffoldResult>>,
    corpus_rules: Vec<ProposedRuleView>,
    toasts: Signal<Vec<crate::toast::Toast>>,
) -> Element {
    let can_scaffold = !gf_name().trim().is_empty() && !gf_path().trim().is_empty();
    // Split corpus into recommended (suggested for the greenfield starter set) and the rest.
    let (recommended, available): (Vec<_>, Vec<_>) =
        corpus_rules.iter().partition(|r| r.recommended);

    rsx! {
        div { class: "gf-form",
            // Step 1: name the repo.
            div { class: "gf-field",
                label { class: "gf-label", "New repo name" }
                input {
                    class: "gf-input",
                    r#type: "text",
                    placeholder: "my-project",
                    value: "{gf_name}",
                    oninput: move |e| {
                        gf_name.set(e.value());
                        // Clear a prior scaffold result when the name changes.
                        gf_result.set(None);
                    },
                }
                p { class: "gf-hint", "Used as the initial commit label. You can connect a GitHub remote later." }
            }

            // Step 2: choose the local directory.
            div { class: "gf-field",
                label { class: "gf-label", "Local directory" }
                div { class: "gf-dir-row",
                    span { class: "gf-dir-path",
                        if gf_path().is_empty() {
                            span { class: "gf-dir-empty", "No directory chosen yet" }
                        } else {
                            "{gf_path()}"
                        }
                    }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            spawn(async move {
                                let Some(folder) = rfd::AsyncFileDialog::new()
                                    .set_title("Choose the PARENT folder — Camerata creates the repo directory inside it")
                                    .pick_folder()
                                    .await
                                else {
                                    return;
                                };
                                let parent = folder.path().to_string_lossy().to_string();
                                let name = gf_name.peek().trim().to_string();
                                let dest = if name.is_empty() {
                                    parent.clone()
                                } else {
                                    format!("{}/{}", parent.trim_end_matches('/'), name)
                                };
                                gf_path.set(dest);
                                gf_result.set(None);
                            });
                        },
                        "Choose parent folder\u{2026}"
                    }
                }
                p { class: "gf-hint", "Camerata creates a new directory here for the repo. The directory must not already exist." }
            }

            // Step 3: starter ruleset picker.
            div { class: "gf-field",
                label { class: "gf-label", "Starter ruleset" }
                p { class: "gf-hint", "Select the rules to bake in from the first commit. Recommended rules are pre-ticked. You can change these after onboarding." }
                if corpus_rules.is_empty() {
                    p { class: "gf-hint", "Loading corpus rules\u{2026}" }
                } else {
                    div { class: "gf-rules-list",
                        // Recommended rules first.
                        if !recommended.is_empty() {
                            p { class: "gf-rules-group-h", "Recommended" }
                            for rule in &recommended {
                                {
                                    let rid = rule.id.clone();
                                    let rid2 = rid.clone();
                                    let checked = gf_selected_ids().contains(&rid);
                                    rsx! {
                                        label { class: "gf-rule-row", key: "{rid}",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut ids = gf_selected_ids();
                                                    if ids.contains(&rid2) { ids.remove(&rid2); } else { ids.insert(rid2.clone()); }
                                                    gf_selected_ids.set(ids);
                                                },
                                            }
                                            span { class: "gf-rule-id", "{rule.id}" }
                                            span { class: "gf-rule-title", " \u{2014} {rule.title}" }
                                            if !rule.domain.is_empty() && rule.domain != "*" {
                                                span { class: "gf-rule-domain", " [{rule.domain}]" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Available (not pre-ticked) rules.
                        if !available.is_empty() {
                            p { class: "gf-rules-group-h", "Available" }
                            for rule in &available {
                                {
                                    let rid = rule.id.clone();
                                    let rid2 = rid.clone();
                                    let checked = gf_selected_ids().contains(&rid);
                                    rsx! {
                                        label { class: "gf-rule-row", key: "{rid}",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut ids = gf_selected_ids();
                                                    if ids.contains(&rid2) { ids.remove(&rid2); } else { ids.insert(rid2.clone()); }
                                                    gf_selected_ids.set(ids);
                                                },
                                            }
                                            span { class: "gf-rule-id", "{rule.id}" }
                                            span { class: "gf-rule-title", " \u{2014} {rule.title}" }
                                            if !rule.domain.is_empty() && rule.domain != "*" {
                                                span { class: "gf-rule-domain", " [{rule.domain}]" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Step 4: scaffold CTA.
            button {
                class: "onboard-cta",
                disabled: !can_scaffold || gf_scaffolding(),
                onclick: move |_| {
                    let name = gf_name().trim().to_string();
                    let dest = gf_path().trim().to_string();
                    if name.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Enter a name for the new repo.");
                        return;
                    }
                    if dest.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Choose a directory for the new repo.");
                        return;
                    }
                    // Resolve the selected corpus rules from the full list.
                    let ids = gf_selected_ids();
                    let selected: Vec<ProposedRuleView> = corpus_rules
                        .iter()
                        .filter(|r| ids.contains(&r.id))
                        .cloned()
                        .collect();
                    gf_scaffolding.set(true);
                    gf_result.set(None);
                    spawn(async move {
                        let result = scaffold_greenfield_api(&name, &dest, &selected).await;
                        gf_scaffolding.set(false);
                        gf_result.set(result);
                    });
                },
                if gf_scaffolding() { "Scaffolding\u{2026}" } else { "Scaffold repo" }
            }

            // Step 5: result.
            if let Some(result) = gf_result() {
                GreenfieldResultView { result }
            }
        }
    }
}

/// Displays the outcome of a greenfield scaffold: success with file list and commit
/// sha, or an error message.
#[component]
pub(in crate::cockpit) fn GreenfieldResultView(result: GreenfieldScaffoldResult) -> Element {
    if result.ok {
        rsx! {
            div { class: "gf-result gf-result-ok",
                p { class: "gf-result-h", "Repo scaffolded" }
                p { class: "gf-result-msg", "{result.message}" }
                div { class: "gf-result-files",
                    p { class: "gf-result-files-h", "Files committed:" }
                    ul {
                        for f in &result.files_written {
                            li { class: "gf-result-file", "{f}" }
                        }
                    }
                }
                if !result.commit_sha.is_empty() {
                    p { class: "gf-result-sha", "Initial commit: {result.commit_sha}" }
                }
                p { class: "gf-result-path", "Location: {result.path}" }
                p { class: "gf-result-next",
                    "The repo is governed from the first commit. Connect it to a remote \
                     (e.g. create a GitHub repo and add it as origin) whenever you are ready."
                }
            }
        }
    } else {
        rsx! {
            div { class: "gf-result gf-result-err",
                p { class: "gf-result-h", "Scaffold failed" }
                p { class: "gf-result-msg", "{result.message}" }
            }
        }
    }
}
