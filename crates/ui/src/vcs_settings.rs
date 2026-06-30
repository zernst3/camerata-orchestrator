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

/// Mirror of `camerata_checks::vcs_action::PrCoverage`.
///
/// Controls whether the commit-doc body/id rules also gate PR fields. Both flags
/// default to `true` (server default); the UI toggles them explicitly so a `POST`
/// that omits the `pr` block never silently resets them to `true` (BUG-2).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct PrCoverageView {
    /// Apply `PROCESS-COMMIT-DOC-1` to the PR body as well as the commit body.
    #[serde(default = "yes")]
    apply_body_rule: bool,
    /// Apply `PROCESS-ADO-LINK-1` / story-id id rules to the PR title as well.
    #[serde(default = "yes")]
    apply_id_rule: bool,
}

impl Default for PrCoverageView {
    fn default() -> Self {
        Self {
            apply_body_rule: true,
            apply_id_rule: true,
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
    /// PR-coverage toggles — must be round-tripped so a save never silently
    /// overwrites a custom value with the server default (BUG-2).
    #[serde(default)]
    pr: PrCoverageView,
}

// ── Corpus rule ids (selection gating) ──────────────────────────────────────────
//
// Each VCS-gate parameter sub-section is shown ONLY when its corresponding corpus
// rule is selected in the active project's ruleset. The rules are opt-in corpus
// rules (see `crates/rules/principles/process/`); their rich parameters live here
// in Settings, not in the rule options.

const RULE_COMMIT_DOC: &str = "PROCESS-COMMIT-DOC-1";
const RULE_CONVENTIONAL_COMMIT: &str = "PROCESS-CONVENTIONAL-COMMIT-1";
const RULE_BRANCH_NAMING: &str = "PROCESS-BRANCH-NAMING-1";
const RULE_ADO_LINK: &str = "PROCESS-ADO-LINK-1";

/// Pure predicate: is `rule_id` present in the project's selected rule ids?
///
/// `selections` is the flat list of selected corpus rule ids gathered from every
/// ruleset bucket (base selections + cross-repo + process). Visibility of each
/// VCS-gate parameter sub-section is gated on this so a project that has not
/// selected the rule does not see (or accidentally tune) its parameters.
fn vcs_rule_selected(selections: &[String], rule_id: &str) -> bool {
    selections.iter().any(|s| s == rule_id)
}

// ── BFF calls ─────────────────────────────────────────────────────────────────

/// Fetch the project's selected corpus-rule ids from its ruleset.
///
/// Returns a flat `Vec<String>` of rule ids gathered from all three ruleset
/// buckets (`selections`, `cross_repo`, `process`) so the caller does not need to
/// know which bucket a process rule landed in. `None` on a request/parse failure
/// (the panel then treats no rules as selected and shows the select-first hint).
async fn fetch_selected_rule_ids(project_id: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/ruleset",
        crate::bff_base(),
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
    let ruleset = v.get("ruleset")?;
    let mut ids = Vec::new();
    for bucket in ["selections", "cross_repo", "process"] {
        if let Some(arr) = ruleset.get(bucket).and_then(|b| b.as_array()) {
            for sel in arr {
                if let Some(rid) = sel.get("rule_id").and_then(|r| r.as_str()) {
                    ids.push(rid.to_string());
                }
            }
        }
    }
    Some(ids)
}

async fn fetch_process_rule_config(project_id: &str) -> Option<ProcessRuleConfigView> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/process-rule-config",
        crate::bff_base(),
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
            crate::bff_base(),
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
            crate::bff_base(),
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
    // The project's selected corpus-rule ids (gates which param sub-sections show).
    // `None` while loading; an empty vec means no VCS rules are selected.
    let mut selected_rules = use_signal(|| None::<Vec<String>>);
    // Saving/loading state.
    let mut saving = use_signal(|| false);
    // Bypass test fields.
    let mut bypass_subject = use_signal(String::new);
    let mut bypass_reason = use_signal(String::new);
    let mut bypass_result = use_signal(|| None::<Result<Option<String>, String>>);

    // Load on mount: both the config and the project's selected rule ids.
    let pid = project_id.clone();
    use_effect(move || {
        let pid = pid.clone();
        spawn(async move {
            if let Some(cfg) = fetch_process_rule_config(&pid).await {
                config.set(Some(cfg));
            }
            // Always resolve the selection list (empty vec on failure) so the panel
            // can render the select-first hint rather than hanging on "Loading".
            let ids = fetch_selected_rule_ids(&pid).await.unwrap_or_default();
            selected_rules.set(Some(ids));
        });
    });

    let Some(ref cfg) = *config.read() else {
        return rsx! {
            div { class: "vcs-settings-loading", "Loading VCS gate settings..." }
        };
    };
    let cfg = cfg.clone();

    // Resolve selection gating. While the selection list is still loading we treat
    // nothing as selected (the hint shows); once loaded, each rule's params show
    // only when that rule is selected.
    let selected = selected_rules.read().clone().unwrap_or_default();
    let commit_doc_selected = vcs_rule_selected(&selected, RULE_COMMIT_DOC);
    let conventional_commit_selected = vcs_rule_selected(&selected, RULE_CONVENTIONAL_COMMIT);
    let branch_naming_selected = vcs_rule_selected(&selected, RULE_BRANCH_NAMING);
    let ado_link_selected = vcs_rule_selected(&selected, RULE_ADO_LINK);
    let any_vcs_rule_selected = commit_doc_selected
        || conventional_commit_selected
        || branch_naming_selected
        || ado_link_selected;

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
                "Configure the parameters for the VCS-gate process rules this project has \
                 selected. Each rule's parameters appear here only once the rule is selected \
                 on the Rules page. These rules validate commit/branch metadata and are \
                 enforced at CI (layer 4) and at Camerata's own commit/PR gate — not at the \
                 layer-2 code gate. Changes take effect on the next governed commit or PR."
            }

            // When no VCS process rule is selected, show a single hint instead of
            // the (now-empty) parameter sections.
            if !any_vcs_rule_selected {
                p { class: "vcs-settings-empty-hint",
                    "No VCS-gate process rules are selected for this project. Select any of "
                    span { class: "vcs-settings-rule-id", "PROCESS-COMMIT-DOC-1" }
                    ", "
                    span { class: "vcs-settings-rule-id", "PROCESS-CONVENTIONAL-COMMIT-1" }
                    ", "
                    span { class: "vcs-settings-rule-id", "PROCESS-BRANCH-NAMING-1" }
                    ", or "
                    span { class: "vcs-settings-rule-id", "PROCESS-ADO-LINK-1" }
                    " on the Rules page to configure their parameters here."
                }
            }

            // ── PROCESS-COMMIT-DOC-1 ─────────────────────────────────────────
            if commit_doc_selected {
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

            } // end if commit_doc_selected

            // ── PROCESS-CONVENTIONAL-COMMIT-1 ────────────────────────────────
            if conventional_commit_selected {
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

            } // end if conventional_commit_selected

            // ── PROCESS-BRANCH-NAMING-1 ───────────────────────────────────────
            if branch_naming_selected {
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

            } // end if branch_naming_selected

            // ── PROCESS-ADO-LINK-1 ────────────────────────────────────────────
            if ado_link_selected {
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

            } // end if ado_link_selected

            // ── PR coverage (BUG-2) ──────────────────────────────────────────
            // Controls whether the commit-doc body and ADO/story-id rules also
            // apply to PR bodies and PR titles. Both are ON by default (matches
            // the server default). Exposing them in the UI prevents a round-trip
            // from silently overwriting a custom value with the server default.
            // Only relevant when one of the rules it extends to PRs is selected.
            if commit_doc_selected || ado_link_selected {
            section { class: "vcs-settings-rule-section",
                h3 { class: "vcs-settings-rule-title", "PR coverage" }
                p { class: "vcs-settings-rule-desc",
                    "Choose whether commit-body and story-id rules also gate \
                     the PR description and PR title. Disabling either here \
                     opts the project out of PR-level enforcement while keeping \
                     the commit-level rules active."
                }
                label { class: "vcs-settings-toggle",
                    input {
                        r#type: "checkbox",
                        checked: cfg.pr.apply_body_rule,
                        onchange: {
                            let mut config = config.clone();
                            move |e: Event<FormData>| {
                                let checked = e.value() == "true" || e.checked();
                                if let Some(c) = config.write().as_mut() {
                                    c.pr.apply_body_rule = checked;
                                }
                            }
                        }
                    }
                    " Apply body rule to PR description (PROCESS-COMMIT-DOC-1)"
                }
                label { class: "vcs-settings-toggle",
                    input {
                        r#type: "checkbox",
                        checked: cfg.pr.apply_id_rule,
                        onchange: {
                            let mut config = config.clone();
                            move |e: Event<FormData>| {
                                let checked = e.value() == "true" || e.checked();
                                if let Some(c) = config.write().as_mut() {
                                    c.pr.apply_id_rule = checked;
                                }
                            }
                        }
                    }
                    " Apply id rule to PR title (PROCESS-ADO-LINK-1 / story-id)"
                }
            }

            } // end if commit_doc_selected || ado_link_selected

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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Selection gating: vcs_rule_selected ────────────────────────────────────

    #[test]
    fn vcs_rule_selected_true_when_present() {
        let selections = vec![
            "SOME-OTHER-RULE-1".to_string(),
            RULE_COMMIT_DOC.to_string(),
        ];
        assert!(vcs_rule_selected(&selections, RULE_COMMIT_DOC));
    }

    #[test]
    fn vcs_rule_selected_false_when_absent() {
        let selections = vec!["SOME-OTHER-RULE-1".to_string()];
        assert!(!vcs_rule_selected(&selections, RULE_COMMIT_DOC));
    }

    #[test]
    fn vcs_rule_selected_false_on_empty() {
        let selections: Vec<String> = Vec::new();
        assert!(!vcs_rule_selected(&selections, RULE_CONVENTIONAL_COMMIT));
        assert!(!vcs_rule_selected(&selections, RULE_BRANCH_NAMING));
        assert!(!vcs_rule_selected(&selections, RULE_ADO_LINK));
    }

    #[test]
    fn vcs_rule_selected_is_exact_match_not_prefix() {
        // A near-miss id (substring / different suffix) must NOT count as selected.
        let selections = vec![
            "PROCESS-COMMIT-DOC-12".to_string(),
            "PROCESS-COMMIT-DOC".to_string(),
        ];
        assert!(!vcs_rule_selected(&selections, RULE_COMMIT_DOC));
    }

    #[test]
    fn vcs_rule_selected_each_rule_independently() {
        let selections = vec![RULE_BRANCH_NAMING.to_string()];
        assert!(vcs_rule_selected(&selections, RULE_BRANCH_NAMING));
        assert!(!vcs_rule_selected(&selections, RULE_COMMIT_DOC));
        assert!(!vcs_rule_selected(&selections, RULE_CONVENTIONAL_COMMIT));
        assert!(!vcs_rule_selected(&selections, RULE_ADO_LINK));
    }

    // ── BUG-2 regression: PrCoverageView round-trips through serde ──────────────

    /// Before BUG-2 fix: `ProcessRuleConfigView` did not include the `pr` field, so a
    /// round-trip through serde (as happens in a POST /process-rule-config body) silently
    /// dropped the field. The server deserialized the missing `pr` as `PrCoverage::default()`
    /// (`apply_body_rule = true`, `apply_id_rule = true`), overwriting any custom value.
    ///
    /// After the fix: `pr: PrCoverageView` is included in `ProcessRuleConfigView` with
    /// matching serde defaults, so a config with non-default PR coverage round-trips
    /// correctly.
    #[test]
    fn bug2_pr_coverage_round_trips_through_serde() {
        // A config with non-default PR coverage values.
        let mut cfg = ProcessRuleConfigView::default();
        cfg.pr.apply_body_rule = false;
        cfg.pr.apply_id_rule = true;

        // Serialize to JSON (what the UI sends to the server via POST).
        let json = serde_json::to_string(&cfg).expect("must serialize");

        // Verify the `pr` field is present in the JSON payload.
        assert!(
            json.contains("\"pr\""),
            "BUG-2: serialized config must include the 'pr' field; got: {json}"
        );
        assert!(
            json.contains("apply_body_rule"),
            "BUG-2: serialized config must include 'apply_body_rule'; got: {json}"
        );

        // Deserialize back (what the server does when it receives the POST body).
        let back: ProcessRuleConfigView = serde_json::from_str(&json).expect("must deserialize");

        // The non-default value must survive the round-trip.
        assert!(
            !back.pr.apply_body_rule,
            "BUG-2: apply_body_rule=false must survive a serde round-trip; \
             got apply_body_rule=true (the bug: missing field deserialized as default=true)"
        );
        assert!(
            back.pr.apply_id_rule,
            "apply_id_rule=true must survive a serde round-trip"
        );
    }

    /// Verify defaults: a freshly-deserialized config from an empty JSON object must
    /// have both PR coverage flags `true` (matching the server default `PrCoverage::default()`).
    #[test]
    fn bug2_pr_coverage_defaults_to_true_true() {
        let cfg: ProcessRuleConfigView = serde_json::from_str("{}").expect("empty obj must deser");
        assert!(cfg.pr.apply_body_rule, "apply_body_rule must default to true");
        assert!(cfg.pr.apply_id_rule, "apply_id_rule must default to true");
    }

    /// Verify that a `ProcessRuleConfigView` with explicit `pr` values serializes the
    /// `pr` block at the top level (not nested inside another sub-object).
    #[test]
    fn bug2_pr_coverage_is_top_level_field_on_config() {
        let cfg = ProcessRuleConfigView {
            pr: PrCoverageView {
                apply_body_rule: true,
                apply_id_rule: false,
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // `pr` must be a top-level key with the two boolean sub-fields.
        assert!(
            v["pr"].is_object(),
            "BUG-2: 'pr' must be a top-level object in the serialized config; got: {json}"
        );
        assert_eq!(v["pr"]["apply_body_rule"], true);
        assert_eq!(v["pr"]["apply_id_rule"], false);
    }

    // ── Tier 2: network-helper tests (wiremock) ─────────────────────────────────
    //
    // Each test points the converted helper (now reading `crate::bff_base()`) at a
    // fake BFF via the `CAMERATA_BFF_URL` seam, mounts the exact route(s) the helper
    // calls, runs it, and asserts the parsed result / request body. `CAMERATA_BFF_URL`
    // is process-global, so these set+remove it and must not run concurrently with
    // any other test that reads `bff_base()`.

    /// GET helper: gathers selected corpus-rule ids from all three ruleset buckets
    /// (`selections`, `cross_repo`, `process`) into a single flat Vec.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_selected_rule_ids_flattens_all_three_buckets() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/ruleset"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "ruleset": {
                    "selections": [{ "rule_id": "PROCESS-COMMIT-DOC-1" }],
                    "cross_repo": [{ "rule_id": "SOME-CROSS-REPO-1" }],
                    "process": [{ "rule_id": "PROCESS-BRANCH-NAMING-1" }],
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ids = super::fetch_selected_rule_ids("proj-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let ids = ids.expect("ok:true response parses to Some");
        assert!(ids.contains(&"PROCESS-COMMIT-DOC-1".to_string()));
        assert!(ids.contains(&"SOME-CROSS-REPO-1".to_string()));
        assert!(ids.contains(&"PROCESS-BRANCH-NAMING-1".to_string()));
        assert_eq!(ids.len(), 3, "exactly the three bucket entries, flattened");
    }

    /// GET helper: an `ok:false` response yields `None` (the panel then treats no rules
    /// as selected and shows the select-first hint).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_selected_rule_ids_returns_none_on_ok_false() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-2/ruleset"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": false })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ids = super::fetch_selected_rule_ids("proj-2").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ids.is_none(), "ok:false must map to None");
    }

    /// GET helper: parses the `process_rule_config` block into a `ProcessRuleConfigView`,
    /// preserving non-default values that round-trip through serde.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_process_rule_config_parses_the_config_block() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-3/process-rule-config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "process_rule_config": {
                    "commit_doc": { "enabled": true, "min_body_chars": 99, "require_story_id": false },
                    "branch_naming": { "enabled": true, "prefixes": ["feat/"] },
                    "pr": { "apply_body_rule": false, "apply_id_rule": true }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let cfg = super::fetch_process_rule_config("proj-3").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let cfg = cfg.expect("ok:true with config block parses to Some");
        assert_eq!(cfg.commit_doc.min_body_chars, 99);
        assert!(!cfg.commit_doc.require_story_id);
        assert!(cfg.branch_naming.enabled);
        assert_eq!(cfg.branch_naming.prefixes, vec!["feat/".to_string()]);
        assert!(!cfg.pr.apply_body_rule, "non-default PR coverage is parsed");
        assert!(cfg.pr.apply_id_rule);
    }

    /// GET helper: `ok:false` yields `None`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_process_rule_config_returns_none_on_ok_false() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-4/process-rule-config"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": false })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let cfg = super::fetch_process_rule_config("proj-4").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(cfg.is_none(), "ok:false must map to None");
    }

    /// POST helper: serializes the FULL config (including the `pr` block — BUG-2) to the
    /// project's process-rule-config endpoint and reports success on a 2xx.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_process_rule_config_posts_the_full_config_body() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // A config with a non-default PR-coverage value; the exact JSON must reach the BFF.
        let cfg = ProcessRuleConfigView {
            pr: PrCoverageView {
                apply_body_rule: false,
                apply_id_rule: true,
            },
            ..Default::default()
        };
        // The expected body is the helper's own serialization of `cfg`, so the matcher
        // pins the EXACT shape the UI sends (all four rule blocks + the pr block).
        let expected_body: serde_json::Value =
            serde_json::to_value(&cfg).expect("config serializes");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-5/process-rule-config"))
            .and(body_json(expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::save_process_rule_config("proj-5", &cfg).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "a 2xx response reports success");
        // `.expect(1)` + `body_json` assert (on server drop) that the helper POSTed the
        // exact full config — including the `pr` block — to the right path.
    }

    /// POST helper: a non-success status maps to `false`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_process_rule_config_returns_false_on_error_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-6/process-rule-config"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::save_process_rule_config("proj-6", &ProcessRuleConfigView::default()).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!ok, "a 500 response reports failure");
    }

    /// POST helper: a successful bypass returns `Ok(Some(record))`. Asserts the exact
    /// request body (the commit action + reason) and that the record JSON is surfaced.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn test_bypass_returns_record_when_bypassed() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/vcs-gate/bypass"))
            .and(body_json(serde_json::json!({
                "action": { "kind": "commit", "message": "auto merge" },
                "reason": "machine-generated merge commit",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "bypassed": true,
                "record": { "reason": "machine-generated merge commit", "by": "architect" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::test_bypass("proj-7", "auto merge", "machine-generated merge commit").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let record = result.expect("ok:true is not an Err");
        let record = record.expect("bypassed:true yields Some(record)");
        assert!(
            record.contains("machine-generated merge commit"),
            "the evidence record JSON is surfaced; got: {record}"
        );
    }

    /// POST helper: `bypassed:false` (the action already passes the gate) returns `Ok(None)`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn test_bypass_returns_none_when_action_already_passes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-8/vcs-gate/bypass"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "bypassed": false
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::test_bypass("proj-8", "feat: real subject", "n/a").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let inner = result.expect("ok:true is not an Err");
        assert!(inner.is_none(), "bypassed:false maps to Ok(None) — no bypass needed");
    }

    /// POST helper: `ok:false` returns `Err(message)` carrying the server's message string.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn test_bypass_returns_err_with_server_message_on_rejection() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-9/vcs-gate/bypass"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "empty reason is rejected"
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::test_bypass("proj-9", "auto merge", "").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let err = result.expect_err("ok:false must be an Err");
        assert_eq!(err, "empty reason is rejected");
    }

    // ── Tier 1: render test (dioxus-ssr) ────────────────────────────────────────
    //
    // `VcsGateSettings` reads a toast Signal from context and kicks off async fetches
    // on mount via `use_effect`. Under SSR those fetches are still pending on first
    // render, so the component renders its loading branch ("Loading VCS gate
    // settings..."). The harness must PROVIDE the toast context signal the component
    // reads with `use_context`, else it panics. We assert the loading-branch
    // structure (the only deterministic output without a live BFF).
    #[test]
    fn renders_loading_branch_with_toast_context() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            // Provide the toast context the component reads via use_context, BEFORE
            // mounting it (else use_context panics).
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            rsx! {
                super::VcsGateSettings { project_id: "proj-render".to_string() }
            }
        }

        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Loading VCS gate settings"),
            "first render (fetches pending) shows the loading branch; got: {html}"
        );
        assert!(
            html.contains("vcs-settings-loading"),
            "the loading branch carries its class; got: {html}"
        );
    }
}
