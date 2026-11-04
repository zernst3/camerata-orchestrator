//! The deep report and audit report export panels and the client name setup.

use super::*;

/// Fetch and return the Markdown deep report for the active project from
/// `GET /api/projects/:id/deep-report`. Returns the Markdown string on success.
///
/// BUG 3 fix: SOC-2 inclusion is decided SERVER-SIDE via the `soc2` feature flag
/// (`export_deep_report` drives it off `state.feature_flags.soc2`). The client used
/// to send `?include_soc2=<bool>`, but the server ignored it — the query param was a
/// dead lever giving false control. It is dropped; the server owns the decision.
pub(in crate::cockpit) async fn fetch_deep_report(project_id: &str) -> Option<String> {
    let url = format!(
        "{}/api/projects/{}/deep-report",
        crate::bff_base(),
        project_id,
    );
    let resp = reqwest::get(url).await.ok()?;
    if resp.status().is_success() {
        resp.text().await.ok()
    } else {
        None
    }
}

/// The deep-report export panel: a single button that calls the export endpoint and
/// shows the resulting Markdown in a scrollable modal. The SOC-2 section is only
/// included when the `soc2` feature flag is on (Feature 5 gate).
///
/// Placed in the Onboard view after the audit findings, below the deep-tier results.
#[component]
pub(in crate::cockpit) fn DeepReportExportPanel(project_id: String, soc2_enabled: bool) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut loading = use_signal(|| false);
    let mut report_md: Signal<Option<String>> = use_signal(|| None);

    rsx! {
        div { class: "deep-export-panel",
            p { class: "section-label", "Export deep compliance report" }
            p { class: "section-hint",
                "Downloads the full deep-tier analysis as a Markdown document. \
                 Includes deep security findings and threat model."
                if soc2_enabled {
                    " SOC-2 gap analysis is also included."
                }
                if !soc2_enabled {
                    " SOC-2 gap analysis is disabled for this workspace (feature flag off)."
                }
            }
            button {
                class: "btn-run",
                disabled: loading(),
                onclick: move |_| {
                    let pid = project_id.clone();
                    loading.set(true);
                    spawn(async move {
                        let _guard = crate::loading::LoadingGuard::new();
                        // SOC-2 inclusion is feature-flag-driven server-side; no client param.
                        match fetch_deep_report(&pid).await {
                            Some(md) => report_md.set(Some(md)),
                            None => crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Error,
                                "Deep report export failed — run an audit with deep tier enabled first.",
                            ),
                        }
                        loading.set(false);
                    });
                },
                if loading() { "Exporting\u{2026}" } else { "Export deep report (Markdown)" }
            }
            if let Some(md) = report_md.read().clone() {
                div { class: "deep-export-modal-overlay",
                    onclick: move |_| report_md.set(None),
                    div { class: "deep-export-modal",
                        onclick: move |e| e.stop_propagation(),
                        div { class: "deep-export-modal-head",
                            p { class: "deep-export-modal-title", "Deep compliance report" }
                            button {
                                class: "rule-modal-close",
                                onclick: move |_| report_md.set(None),
                                "\u{00D7}"
                            }
                        }
                        textarea {
                            class: "deep-export-body",
                            readonly: true,
                            value: "{md}",
                        }
                        button {
                            class: "btn-edit-sm",
                            onclick: move |_| {
                                let md_copy = md.clone();
                                spawn(async move {
                                    let _ = save_csv("camerata-deep-report.md", md_copy).await;
                                });
                            },
                            "Save to file"
                        }
                    }
                }
            }
        }
    }
}

/// POST the product-export request (dispositions + options) and, on success, return
/// `(zip_bytes, suggested_filename)` — the filename is read off the server's
/// `Content-Disposition` header (`camerata-audit-{repo}-{shortsha}.zip`) so the save dialog
/// defaults to something meaningful; falls back to a generic name if the header is missing
/// or unparsable. On a non-2xx response, returns the server's `{ "message": "…" }` text
/// (or a generic fallback) as `Err` for the caller to toast.
///
/// This is the PRIMARY export ("instead of a PDF export... a product export" — the owner's
/// ruling): the zip contains the same curated PDF the old `/audit-report` route produces,
/// PLUS a fully-formatted Excel workbook (every finding, false positives on their own
/// sheet, a coverage sheet) and a README.txt manifest. `/api/projects/:id/audit-report`
/// itself is left in place for one release (see `docs/design/2026-07-27_product-export.md`)
/// but this panel's button no longer targets it.
pub(in crate::cockpit) async fn export_product_zip(
    project_id: &str,
    dispositions: &std::collections::HashMap<String, Disposition>,
    options: &camerata_ui_core::product_export::ProductExportOptions,
) -> Result<(Vec<u8>, String), String> {
    let url = format!(
        "{}/api/projects/{}/product-export",
        crate::bff_base(),
        project_id,
    );
    let resp = reqwest::Client::new()
        .post(url)
        // The client name is not sent: the server reads it from the project's inspection setup
        // (`Project::client_name`), so every export names the same client.
        .json(&camerata_ui_core::product_export::request_body(
            dispositions,
            options,
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let msg = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_else(|| "Product export failed.".to_string());
        return Err(msg);
    }

    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split("filename=\"").nth(1))
        .map(|s| s.trim_end_matches('"').to_string())
        .unwrap_or_else(|| "camerata-audit-export.zip".to_string());

    let bytes = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok((bytes, filename))
}

/// The inspection setup's client-name field: who the inspection is for. Saved to the project
/// when the field loses focus after an edit (`POST /api/projects/:id/client-name`); every export
/// prints it in the confidentiality line, and the export refuses while it is blank, so the
/// field says so until it is filled.
#[component]
pub(in crate::cockpit) fn ClientNameSetup(project_id: String, initial: String) -> Element {
    // The field keeps its own text while it is on screen, so it is keyed by the project it is
    // for and by the name that project has saved. Switching projects (or the saved name
    // changing under it) builds a fresh field, so one project's text can never be shown against
    // another project's id, or posted to it.
    //
    // A `key` is only honoured among siblings in a list, so the one field is rendered as a list
    // of one.
    let key = format!("{project_id}/{initial}");
    rsx! {
        for key in [key] {
            ClientNameField {
                key: "{key}",
                project_id: project_id.clone(),
                initial: initial.clone(),
            }
        }
    }
}

#[component]
pub(super) fn ClientNameField(project_id: String, initial: String) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut name = use_signal(|| initial.clone());
    let blank = name().trim().is_empty();
    rsx! {
        div {
            class: "audit-model-row inspection-client-row",
            "data-tour": camerata_ui_core::walkthrough::target::CLIENT_NAME,
            label { class: "audit-model-label", r#for: "inspection-client-name", "Client name" }
            input {
                id: "inspection-client-name",
                class: "audit-export-input",
                placeholder: "Required before export",
                value: "{name}",
                oninput: move |e| name.set(e.value()),
                onchange: move |_| {
                    let pid = project_id.clone();
                    let value = name();
                    spawn(async move {
                        match super::set_client_name(&pid, &value).await {
                            Ok(()) => crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Client name saved."),
                            Err(msg) => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, msg),
                        }
                    });
                },
            }
            if blank {
                span { class: "section-hint",
                    "Required before export. It fills the confidentiality line on every page of the report and every sheet of the workbook."
                }
            }
        }
    }
}

/// The product-export panel: client-editable report framing (all optional) + a single
/// export button. POSTs the CURRENT triage state (`dispositions.read()`, whatever it is
/// right now — including any still-Unresolved findings; a draft mid-engagement report is
/// legitimate, so this is never gated on triage completeness) and saves the returned ZIP
/// (curated PDF + full Excel workbook + README) via the same native-save-dialog idiom
/// `save_csv`/the deep-report panel use.
///
/// Placed in the Onboard view right after the triage Process step, above the deep-tier
/// panel — see `ScanResults`.
#[component]
pub(in crate::cockpit) fn AuditReportExportPanel(
    project_id: String,
    dispositions: Signal<std::collections::HashMap<String, Disposition>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut loading = use_signal(|| false);
    let mut project_title = use_signal(String::new);
    let mut prepared_by = use_signal(String::new);
    let mut summary_override = use_signal(String::new);
    let mut triage_log = use_signal(|| false);

    rsx! {
        div { class: "audit-export-panel",
            p { class: "section-label", "Export product (ZIP: PDF + Excel + JSON)" }
            p { class: "section-hint",
                "One ZIP, three artifacts from the same scan: a board-forwardable PDF (cover, \
                 executive summary, category scorecard, severity\u{00d7}effort matrix, \
                 curated findings with citations and recommended fixes, what's healthy, \
                 dependency/CVE snapshot, methodology), a fully-formatted Excel workbook — \
                 every finding, one row each, sortable and filterable, with a per-category \
                 sheet and a False Positives sheet (nothing silently dropped) — and a raw \
                 JSON export of the full report for machine consumption, plus a README \
                 explaining each file. Uses your CURRENT triage — a draft mid-engagement \
                 export (some findings still Unresolved) is fine."
            }
            div { class: "audit-export-fields",
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Project title" }
                    input {
                        class: "audit-export-input",
                        placeholder: "optional",
                        value: "{project_title}",
                        oninput: move |e| project_title.set(e.value()),
                    }
                }
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Prepared by" }
                    input {
                        class: "audit-export-input",
                        placeholder: "optional",
                        value: "{prepared_by}",
                        oninput: move |e| prepared_by.set(e.value()),
                    }
                }
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Executive summary override" }
                    textarea {
                        class: "audit-export-summary",
                        rows: "8",
                        placeholder: "Optional — leave blank for the auto-generated summary. A multi-sentence narrative that replaces the report's executive summary section verbatim.",
                        value: "{summary_override}",
                        oninput: move |e| summary_override.set(e.value()),
                    }
                }
            }
            div { class: "audit-model-row",
                label { class: "audit-model-label", r#for: "export-triage-log", "Triage log" }
                input {
                    id: "export-triage-log",
                    r#type: "checkbox",
                    checked: triage_log(),
                    onchange: move |e| triage_log.set(e.checked()),
                }
                span { class: "section-hint",
                    "Also export, per rule, what the AI pass proposed, what review kept and threw \
                     out, and why: a Triage Log sheet in the workbook and csv/triage-log.csv. Off by default."
                }
            }
            // The release gate refuses a package while a row needs a decision. When it does, the
            // button gives way to the list of those rows, each with a door to it (spec section 10).
            crate::cockpit::export_gate::ExportGateHost { project_id: project_id.clone(),
                button {
                    class: "btn-run",
                    "data-tour": camerata_ui_core::walkthrough::target::EXPORT_PACKAGE,
                    disabled: loading(),
                    onclick: move |_| {
                        let pid = project_id.clone();
                        let disp_snapshot = dispositions.read().clone();
                        let sync = try_consume_context::<DecisionSync>();
                        let pt = project_title();
                        let pb = prepared_by();
                        let so = summary_override();
                        let log = triage_log();
                        loading.set(true);
                        spawn(async move {
                            let _guard = crate::loading::LoadingGuard::new();
                            let override_opt = if so.trim().is_empty() { None } else { Some(so) };
                            // The export reads the decision store. Store what the screen has
                            // changed first; whatever is still not stored is listed in the request
                            // and the release gate refuses to export while any is. With the
                            // decision state unknown nothing can be exported at all.
                            let mut unsynced = Vec::new();
                            if let Some(sync) = sync {
                                if sync.bridge.borrow().availability()
                                    == camerata_ui_core::decisions::Availability::Unknown
                                {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Error,
                                        camerata_ui_core::decision_sync::UNKNOWN_BANNER,
                                    );
                                    loading.set(false);
                                    return;
                                }
                                sync.flush(dispositions).await;
                                unsynced = sync.bridge.borrow().unsynced(&disp_snapshot);
                            }
                            let options = camerata_ui_core::product_export::ProductExportOptions {
                                project_title: pt,
                                prepared_by: pb,
                                executive_summary_override: override_opt,
                                triage_log: log,
                                unsynced_changes: unsynced,
                            };
                            match export_product_zip(&pid, &disp_snapshot, &options).await {
                                Ok((bytes, filename)) => {
                                    if save_bytes(&filename, bytes).await {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Product export saved.");
                                    }
                                }
                                Err(msg) => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, msg),
                            }
                            loading.set(false);
                        });
                    },
                    if loading() { "Exporting\u{2026}" } else { "Export product (ZIP: PDF + Excel + JSON)" }
                }
            }
        }
    }
}
