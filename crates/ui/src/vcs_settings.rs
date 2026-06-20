//! VCS-gate process-rule settings panel.
//!
//! This is a NEW settings area — kept entirely separate from the cockpit story
//! and audit views (per the task COLLISION CONTROL constraint). It lets the
//! project architect toggle + tune the four VCS-action gate rules:
//!
//! - `PROCESS-COMMIT-DOC-1` — substantive body + optional story-id reference
//! - `PROCESS-CONVENTIONAL-COMMIT-1` — commit subject shape
//! - `PROCESS-BRANCH-NAMING-1` — allowed branch-name prefixes (opt-in)
//! - `PROCESS-ADO-LINK-1` — ADO ticket reference in subject/title (opt-in)
//!
//! It also exposes the bypass affordance: a text area for the required reason
//! and a test action so the architect can verify which rules would fire and
//! confirm that a bypass with their stated reason is accepted.
//!
//! No story or audit view is touched.

use dioxus::prelude::*;

// ── API types ─────────────────────────────────────────────────────────────────
//
// These mirror the serde shapes from `camerata_checks::vcs_action` and
// `camerata_server`. Kept as plain Dioxus-side structs so the UI has no compile
// dependency on the server crate.

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
struct StoryIdFormatView {
    #[serde(default)]
    prefix: String,
    #[serde(default = "default_separator_char")]
    separator: char,
    #[serde(default)]
    custom_regex: Option<String>,
}

fn default_separator_char() -> char {
    '#'
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct CommitDocConfigView {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default = "twenty")]
    min_body_chars: usize,
    #[serde(default = "yes")]
    require_story_id: bool,
    #[serde(default)]
    story_id_format: StoryIdFormatView,
}

fn yes() -> bool {
    true
}
fn twenty() -> usize {
    20
}

impl Default for CommitDocConfigView {
    fn default() -> Self {
        Self {
            enabled: true,
            min_body_chars: 20,
            require_story_id: true,
            story_id_format: StoryIdFormatView::default(),
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct ConventionalCommitConfigView {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default = "default_cc_types")]
    types: Vec<String>,
}

fn default_cc_types() -> Vec<String> {
    ["feat", "fix", "chore", "docs", "refactor", "test", "perf", "build", "ci", "style", "revert"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for ConventionalCommitConfigView {
    fn default() -> Self {
        Self {
            enabled: true,
            types: default_cc_types(),
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct BranchNamingConfigView {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_branch_prefixes")]
    prefixes: Vec<String>,
}

fn default_branch_prefixes() -> Vec<String> {
    ["feature/", "release/", "hotfix/"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for BranchNamingConfigView {
    fn default() -> Self {
        Self {
            enabled: false,
            prefixes: default_branch_prefixes(),
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct AdoLinkConfigView {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_ado_prefix")]
    prefix: String,
}

fn default_ado_prefix() -> String {
    "AB".to_string()
}

impl Default for AdoLinkConfigView {
    fn default() -> Self {
        Self {
            enabled: false,
            prefix: default_ado_prefix(),
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
struct ProcessRuleConfigView {
    #[serde(default)]
    commit_doc: CommitDocConfigView,
    #[serde(default)]
    conventional_commit: ConventionalCommitConfigView,
    #[serde(default)]
    branch_naming: BranchNamingConfigView,
    #[serde(default)]
    ado_link: AdoLinkConfigView,
}

// ── BFF calls ─────────────────────────────────────────────────────────────────

async fn fetch_process_rule_config(project_id: &str) -> Option<ProcessRuleConfigView> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/process-rule-config",
        crate::BFF_URL,
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    serde_json::from_value(v.get("process_rule_config")?.clone()).ok()
}

async fn save_process_rule_config(
    project_id: &str,
    config: &ProcessRuleConfigView,
) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/process-rule-config",
            crate::BFF_URL,
            project_id
        ))
        .json(config)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Test a bypass: POST to /vcs-gate/bypass with a Commit action and a reason.
/// Returns `Ok(Some(record_json))` on a successful bypass, `Ok(None)` when the
/// action already passes (no bypass needed), and `Err(message)` on rejection.
async fn test_bypass(
    project_id: &str,
    commit_subject: &str,
    reason: &str,
) -> Result<Option<String>, String> {
    let action = serde_json::json!({
        "kind": "commit",
        "message": commit_subject,
    });
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/vcs-gate/bypass",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "action": action, "reason": reason }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("could not parse response: {e}"))?;

    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return Err(v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string());
    }

    if v.get("bypassed").and_then(|b| b.as_bool()).unwrap_or(false) {
        let record = v
            .get("record")
            .map(|r| serde_json::to_string_pretty(r).unwrap_or_default());
        Ok(record)
    } else {
        Ok(None) // action already passed the gate; no bypass needed
    }
}

// ── Component ─────────────────────────────────────────────────────────────────

/// The VCS-gate process-rule settings panel.
///
/// Renders a toggle + tunables form for each of the four process rules, plus the
/// bypass affordance. Kept out of the cockpit story/audit views — this is a NEW
/// settings area.
#[component]
pub fn VcsGateSettings(project_id: String) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // Config loaded from the BFF (None while loading or on error).
    let mut config = use_signal(|| None::<ProcessRuleConfigView>);
    // Saving/loading state.
    let mut saving = use_signal(|| false);
    // Bypass test fields.
    let mut bypass_subject = use_signal(String::new);
    let mut bypass_reason = use_signal(String::new);
    let mut bypass_result = use_signal(|| None::<Result<Option<String>, String>>);

    // Load on mount.
    let pid = project_id.clone();
    use_effect(move || {
        let pid = pid.clone();
        spawn(async move {
            if let Some(cfg) = fetch_process_rule_config(&pid).await {
                config.set(Some(cfg));
            }
        });
    });

    let Some(ref cfg) = *config.read() else {
        return rsx! {
            div { class: "vcs-settings-loading", "Loading VCS gate settings..." }
        };
    };
    let cfg = cfg.clone();

    let pid_save = project_id.clone();
    let on_save = {
        let cfg = cfg.clone();
        move |_| {
            let cfg = cfg.clone();
            let pid = pid_save.clone();
            let toasts = toasts;
            saving.set(true);
            spawn(async move {
                let ok = save_process_rule_config(&pid, &cfg).await;
                saving.set(false);
                if ok {
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "VCS gate settings saved.");
                } else {
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Failed to save VCS gate settings.");
                }
            });
        }
    };

    let pid_bypass = project_id.clone();
    let on_test_bypass = move |_| {
        let pid = pid_bypass.clone();
        let subject = bypass_subject.read().clone();
        let reason = bypass_reason.read().clone();
        bypass_result.set(None);
        spawn(async move {
            let result = test_bypass(&pid, &subject, &reason).await;
            bypass_result.set(Some(result));
        });
    };

    rsx! {
        div { class: "vcs-settings-panel",
            h2 { class: "vcs-settings-title", "VCS Gate — Process Rule Configuration" }
            p { class: "vcs-settings-intro",
                "Configure which process rules the VCS-action gate enforces for this project. \
                 Changes take effect on the next governed commit or PR."
            }

            // ── PROCESS-COMMIT-DOC-1 ─────────────────────────────────────────
            section { class: "vcs-settings-rule-section",
                h3 { class: "vcs-settings-rule-title",
                    span { class: "vcs-settings-rule-id", "PROCESS-COMMIT-DOC-1" }
                    " — Substantive commit body"
                }
                p { class: "vcs-settings-rule-desc",
                    "Requires the commit body (lines after the subject) and the PR description to \
                     be substantive (at least the configured minimum characters) and, optionally, \
                     to include a story-id reference."
                }
                label { class: "vcs-settings-toggle",
                    input {
                        r#type: "checkbox",
                        checked: cfg.commit_doc.enabled,
                        onchange: {
                            let mut config = config.clone();
                            move |e: Event<FormData>| {
                                let checked = e.value() == "true" || e.checked();
                                if let Some(c) = config.write().as_mut() {
                                    c.commit_doc.enabled = checked;
                                }
                            }
                        }
                    }
                    " Enabled"
                }
                if cfg.commit_doc.enabled {
                    div { class: "vcs-settings-tunables",
                        label { class: "vcs-settings-label",
                            "Minimum body characters"
                            input {
                                class: "vcs-settings-input",
                                r#type: "number",
                                min: "0",
                                value: cfg.commit_doc.min_body_chars.to_string(),
                                oninput: {
                                    let mut config = config.clone();
                                    move |e: Event<FormData>| {
                                        if let Ok(n) = e.value().parse::<usize>() {
                                            if let Some(c) = config.write().as_mut() {
                                                c.commit_doc.min_body_chars = n;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        label { class: "vcs-settings-toggle",
                            input {
                                r#type: "checkbox",
                                checked: cfg.commit_doc.require_story_id,
                                onchange: {
                                    let mut config = config.clone();
                                    move |e: Event<FormData>| {
                                        let checked = e.value() == "true" || e.checked();
                                        if let Some(c) = config.write().as_mut() {
                                            c.commit_doc.require_story_id = checked;
                                        }
                                    }
                                }
                            }
                            " Require story-id reference"
                        }
                        if cfg.commit_doc.require_story_id {
                            div { class: "vcs-settings-story-id-fmt",
                                p { class: "vcs-settings-hint",
                                    "Story-id format: the gate looks for \
                                     <prefix><separator><digits> in the body. \
                                     Leave prefix empty for bare #42 (GitHub). \
                                     Use prefix=\"AB\" and separator=\"#\" for AB#123 (Azure Boards). \
                                     Use prefix=\"PROJ\" and separator=\"-\" for PROJ-42 (Jira)."
                                }
                                label { class: "vcs-settings-label",
                                    "Prefix (empty = bare reference)"
                                    input {
                                        class: "vcs-settings-input",
                                        r#type: "text",
                                        placeholder: "e.g. AB or PROJ (empty = bare #42)",
                                        value: cfg.commit_doc.story_id_format.prefix.clone(),
                                        oninput: {
                                            let mut config = config.clone();
                                            move |e: Event<FormData>| {
                                                if let Some(c) = config.write().as_mut() {
                                                    c.commit_doc.story_id_format.prefix = e.value().clone();
                                                }
                                            }
                                        }
                                    }
                                }
                                label { class: "vcs-settings-label",
                                    "Separator character"
                                    select {
                                        class: "vcs-settings-select",
                                        onchange: {
                                            let mut config = config.clone();
                                            move |e: Event<FormData>| {
                                                let sep = if e.value() == "-" { '-' } else { '#' };
                                                if let Some(c) = config.write().as_mut() {
                                                    c.commit_doc.story_id_format.separator = sep;
                                                }
                                            }
                                        },
                                        option {
                                            value: "#",
                                            selected: cfg.commit_doc.story_id_format.separator == '#',
                                            "# (hash, e.g. AB#123)"
                                        }
                                        option {
                                            value: "-",
                                            selected: cfg.commit_doc.story_id_format.separator == '-',
                                            "- (dash, e.g. PROJ-42)"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── PROCESS-CONVENTIONAL-COMMIT-1 ────────────────────────────────
            section { class: "vcs-settings-rule-section",
                h3 { class: "vcs-settings-rule-title",
                    span { class: "vcs-settings-rule-id", "PROCESS-CONVENTIONAL-COMMIT-1" }
                    " — Conventional commit shape"
                }
                p { class: "vcs-settings-rule-desc",
                    "Requires the commit subject (first line) to follow conventional-commits \
                     format: <type>(scope)!: subject."
                }
                label { class: "vcs-settings-toggle",
                    input {
                        r#type: "checkbox",
                        checked: cfg.conventional_commit.enabled,
                        onchange: {
                            let mut config = config.clone();
                            move |e: Event<FormData>| {
                                let checked = e.value() == "true" || e.checked();
                                if let Some(c) = config.write().as_mut() {
                                    c.conventional_commit.enabled = checked;
                                }
                            }
                        }
                    }
                    " Enabled"
                }
                if cfg.conventional_commit.enabled {
                    div { class: "vcs-settings-tunables",
                        label { class: "vcs-settings-label",
                            "Allowed types (comma-separated)"
                            input {
                                class: "vcs-settings-input vcs-settings-input-wide",
                                r#type: "text",
                                value: cfg.conventional_commit.types.join(", "),
                                oninput: {
                                    let mut config = config.clone();
                                    move |e: Event<FormData>| {
                                        let types: Vec<String> = e.value()
                                            .split(',')
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty())
                                            .collect();
                                        if let Some(c) = config.write().as_mut() {
                                            c.conventional_commit.types = types;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── PROCESS-BRANCH-NAMING-1 ───────────────────────────────────────
            section { class: "vcs-settings-rule-section",
                h3 { class: "vcs-settings-rule-title",
                    span { class: "vcs-settings-rule-id", "PROCESS-BRANCH-NAMING-1" }
                    " — Branch naming"
                    span { class: "vcs-settings-opt-in-badge", " opt-in" }
                }
                p { class: "vcs-settings-rule-desc",
                    "Requires new branch names to start with one of the configured prefixes. \
                     Disabled by default; opt in if your team enforces a naming convention."
                }
                label { class: "vcs-settings-toggle",
                    input {
                        r#type: "checkbox",
                        checked: cfg.branch_naming.enabled,
                        onchange: {
                            let mut config = config.clone();
                            move |e: Event<FormData>| {
                                let checked = e.value() == "true" || e.checked();
                                if let Some(c) = config.write().as_mut() {
                                    c.branch_naming.enabled = checked;
                                }
                            }
                        }
                    }
                    " Enabled"
                }
                if cfg.branch_naming.enabled {
                    div { class: "vcs-settings-tunables",
                        label { class: "vcs-settings-label",
                            "Allowed prefixes (comma-separated)"
                            input {
                                class: "vcs-settings-input vcs-settings-input-wide",
                                r#type: "text",
                                value: cfg.branch_naming.prefixes.join(", "),
                                oninput: {
                                    let mut config = config.clone();
                                    move |e: Event<FormData>| {
                                        let prefixes: Vec<String> = e.value()
                                            .split(',')
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty())
                                            .collect();
                                        if let Some(c) = config.write().as_mut() {
                                            c.branch_naming.prefixes = prefixes;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── PROCESS-ADO-LINK-1 ────────────────────────────────────────────
            section { class: "vcs-settings-rule-section",
                h3 { class: "vcs-settings-rule-title",
                    span { class: "vcs-settings-rule-id", "PROCESS-ADO-LINK-1" }
                    " — ADO ticket link"
                    span { class: "vcs-settings-opt-in-badge", " opt-in" }
                }
                p { class: "vcs-settings-rule-desc",
                    "Requires the commit subject and PR title to contain an Azure DevOps \
                     (ADO) ticket reference of the form <prefix>#<id>. Disabled by default; \
                     enable if your team uses ADO and auto-links commits/PRs to work items."
                }
                label { class: "vcs-settings-toggle",
                    input {
                        r#type: "checkbox",
                        checked: cfg.ado_link.enabled,
                        onchange: {
                            let mut config = config.clone();
                            move |e: Event<FormData>| {
                                let checked = e.value() == "true" || e.checked();
                                if let Some(c) = config.write().as_mut() {
                                    c.ado_link.enabled = checked;
                                }
                            }
                        }
                    }
                    " Enabled"
                }
                if cfg.ado_link.enabled {
                    div { class: "vcs-settings-tunables",
                        label { class: "vcs-settings-label",
                            "Ticket prefix"
                            input {
                                class: "vcs-settings-input",
                                r#type: "text",
                                placeholder: "e.g. AB",
                                value: cfg.ado_link.prefix.clone(),
                                oninput: {
                                    let mut config = config.clone();
                                    move |e: Event<FormData>| {
                                        if let Some(c) = config.write().as_mut() {
                                            c.ado_link.prefix = e.value().clone();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Save button ───────────────────────────────────────────────────
            div { class: "vcs-settings-actions",
                button {
                    class: "btn-primary",
                    disabled: *saving.read(),
                    onclick: on_save,
                    if *saving.read() { "Saving..." } else { "Save settings" }
                }
            }

            // ── Bypass affordance ──────────────────────────────────────────────
            section { class: "vcs-settings-bypass-section",
                h3 { class: "vcs-settings-rule-title", "Auditable bypass" }
                p { class: "vcs-settings-rule-desc",
                    "When a specific VCS action legitimately cannot satisfy the active rules \
                     (e.g. a machine-generated merge commit or a one-time onboarding branch), \
                     supply a non-empty reason to bypass the gate. The reason is recorded in \
                     the evidence trail. An empty reason is rejected — a reason-less bypass is \
                     itself a gate violation."
                }
                div { class: "vcs-settings-tunables",
                    label { class: "vcs-settings-label",
                        "Test commit subject (to see which rules would fire)"
                        input {
                            class: "vcs-settings-input vcs-settings-input-wide",
                            r#type: "text",
                            placeholder: "e.g. 'auto-generated merge commit'",
                            value: bypass_subject.read().clone(),
                            oninput: move |e: Event<FormData>| bypass_subject.set(e.value().clone()),
                        }
                    }
                    label { class: "vcs-settings-label",
                        "Bypass reason (required and must be non-empty)"
                        textarea {
                            class: "vcs-settings-textarea",
                            placeholder: "e.g. machine-generated merge commit from the rebase pipeline, \
                                          predates this project's conventions",
                            value: bypass_reason.read().clone(),
                            oninput: move |e: Event<FormData>| bypass_reason.set(e.value().clone()),
                        }
                    }
                    button {
                        class: "btn-secondary",
                        onclick: on_test_bypass,
                        "Test bypass"
                    }
                    if let Some(result) = bypass_result.read().as_ref() {
                        match result {
                            Ok(None) => rsx! {
                                p { class: "vcs-settings-bypass-ok",
                                    "Action already passes the gate — no bypass needed."
                                }
                            },
                            Ok(Some(record)) => rsx! {
                                p { class: "vcs-settings-bypass-ok",
                                    "Bypass accepted. Record (for evidence trail):"
                                }
                                pre { class: "vcs-settings-bypass-record", "{record}" }
                            },
                            Err(msg) => rsx! {
                                p { class: "vcs-settings-bypass-err", "{msg}" }
                            },
                        }
                    }
                }
            }
        }
    }
}
