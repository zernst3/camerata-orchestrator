//! The Layer-3 reviewer's findings for a unit of work (gap 4), shown in the Development panel.
//!
//! Every finding the reviewer reports is recorded on the unit, so none is dropped silently: it
//! is fixed by a later pass, accepted by the architect with a reason, or still open. An open
//! HIGH or MEDIUM finding blocks sign-off. This section lists them and lets the architect accept
//! an open one with a reason (`POST /api/uow/:story_id/review-findings/:index/accept`).

use super::uow::rejection_message;
use super::*;
use camerata_api_types::uow::ReviewFindingStatus;

/// One recorded reviewer finding as the console reads it (the canonical wire shape).
pub(super) use camerata_api_types::uow::RecordedReviewFinding as RecordedReviewFindingView;

/// The prompt shown when the architect accepts a finding.
const ACCEPT_PROMPT: &str = "Why is this finding acceptable as is?";

/// CSS class for a finding's row: HIGH reads as critical, MEDIUM as a warning, LOW plain.
pub(super) fn severity_row_class(severity: &str) -> &'static str {
    match severity {
        "high" => "governance-event-row severity-critical",
        "medium" => "governance-event-row severity-warn",
        _ => "governance-event-row",
    }
}

/// The status column text.
pub(super) fn status_label(status: &ReviewFindingStatus) -> String {
    match status {
        ReviewFindingStatus::Open => "Open".to_string(),
        ReviewFindingStatus::Fixed { run_id, iteration } if run_id.is_empty() => {
            format!("Fixed (pass {iteration})")
        }
        ReviewFindingStatus::Fixed { run_id, iteration } => {
            format!("Fixed in run {run_id}, pass {iteration}")
        }
        ReviewFindingStatus::Accepted { by, reason, .. } => {
            format!("Accepted by {by}: {reason}")
        }
    }
}

/// How many findings still block sign-off (open and HIGH or MEDIUM).
pub(super) fn blocking_count(findings: &[RecordedReviewFindingView]) -> usize {
    findings.iter().filter(|f| f.blocks_sign_off()).count()
}

/// `POST /api/uow/:story_id/review-findings/:index/accept` with the architect's reason.
/// `Err(message)` carries the server's reason (blank reason, finding already settled) or a
/// could-not-reach text.
pub(super) async fn accept_review_finding(
    story_id: &str,
    index: usize,
    reason: &str,
) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/review-findings/{index}/accept",
            crate::bff_base(),
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "reason": reason }))
        .send()
        .await
        .map_err(|_| "Could not reach the server to accept the finding.".to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(rejection_message(resp, "The finding was not accepted").await)
    }
}

/// The reviewer-findings table with an "Accept with reason" action per open row. Renders
/// nothing when the unit has no recorded findings.
#[component]
pub(super) fn ReviewFindingsSection(
    story_id: String,
    findings: Vec<RecordedReviewFindingView>,
    mut uow_refresh: Signal<u32>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The row being accepted (its index) and the reason typed so far.
    let mut accepting = use_signal(|| None::<usize>);
    let mut reason = use_signal(String::new);
    let mut busy = use_signal(|| false);

    if findings.is_empty() {
        return rsx! {};
    }
    let blocking = blocking_count(&findings);

    rsx! {
        div { class: "uow-dev-section review-findings-section",
            p { class: "uow-dev-section-h", "Reviewer findings" }
            if blocking > 0 {
                p { class: "section-hint",
                    "{blocking} reviewer finding(s) are still open. Fix them or record a decision on each before signing off."
                }
            } else {
                p { class: "section-hint",
                    "No high or medium reviewer finding is open."
                }
            }
            table { class: "governance-events-table",
                thead {
                    tr {
                        th { "Severity" }
                        th { "Finding" }
                        th { "Scenario" }
                        th { "Rule" }
                        th { "Where" }
                        th { "Status" }
                    }
                }
                tbody {
                    for (index , f) in findings.iter().cloned().enumerate() {
                        tr {
                            key: "{index}",
                            class: severity_row_class(&f.severity),
                            td { {f.severity.to_uppercase()} }
                            td { "{f.title}" }
                            td { "{f.scenario}" }
                            td { "{f.rule.clone().unwrap_or_default()}" }
                            td { "{f.location.clone().unwrap_or_default()}" }
                            td {
                                {status_label(&f.status)}
                                if matches!(f.status, ReviewFindingStatus::Open) {
                                    if accepting() == Some(index) {
                                        div { class: "review-finding-accept",
                                            label { "{ACCEPT_PROMPT}" }
                                            input {
                                                r#type: "text",
                                                class: "addressee-input",
                                                value: "{reason}",
                                                disabled: busy(),
                                                oninput: move |e| reason.set(e.value()),
                                            }
                                            button {
                                                class: "btn-run",
                                                disabled: busy() || reason().trim().is_empty(),
                                                onclick: {
                                                    let story_id = story_id.clone();
                                                    move |_| {
                                                        let (story_id, why) = (story_id.clone(), reason());
                                                        busy.set(true);
                                                        spawn(async move {
                                                            match accept_review_finding(&story_id, index, &why).await {
                                                                Ok(()) => {
                                                                    crate::toast::push_toast(
                                                                        toasts,
                                                                        crate::toast::ToastKind::Info,
                                                                        "Reviewer finding accepted.",
                                                                    );
                                                                    accepting.set(None);
                                                                    reason.set(String::new());
                                                                    uow_refresh += 1;
                                                                }
                                                                Err(message) => {
                                                                    crate::toast::push_toast(
                                                                        toasts,
                                                                        crate::toast::ToastKind::Error,
                                                                        message,
                                                                    );
                                                                }
                                                            }
                                                            busy.set(false);
                                                        });
                                                    }
                                                },
                                                "Accept"
                                            }
                                            button {
                                                class: "btn-run",
                                                disabled: busy(),
                                                onclick: move |_| {
                                                    accepting.set(None);
                                                    reason.set(String::new());
                                                },
                                                "Cancel"
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "btn-run",
                                            onclick: move |_| {
                                                reason.set(String::new());
                                                accepting.set(Some(index));
                                            },
                                            "Accept with reason"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: &str, status: ReviewFindingStatus) -> RecordedReviewFindingView {
        RecordedReviewFindingView {
            run_id: "r".into(),
            iteration: 0,
            severity: severity.into(),
            title: "t".into(),
            scenario: String::new(),
            rule: None,
            location: None,
            status,
        }
    }

    #[test]
    fn only_open_high_and_medium_findings_are_counted_as_blocking() {
        let findings = vec![
            finding("high", ReviewFindingStatus::Open),
            finding("medium", ReviewFindingStatus::Open),
            finding("low", ReviewFindingStatus::Open),
            finding(
                "high",
                ReviewFindingStatus::Fixed {
                    run_id: "r1".into(),
                    iteration: 1,
                },
            ),
        ];
        assert_eq!(blocking_count(&findings), 2);
    }

    #[test]
    fn severity_picks_the_row_class() {
        assert!(severity_row_class("high").contains("severity-critical"));
        assert!(severity_row_class("medium").contains("severity-warn"));
        assert_eq!(severity_row_class("low"), "governance-event-row");
    }

    #[test]
    fn status_reads_in_plain_words() {
        assert_eq!(status_label(&ReviewFindingStatus::Open), "Open");
        assert_eq!(
            status_label(&ReviewFindingStatus::Fixed {
                run_id: "run-7".into(),
                iteration: 2
            }),
            "Fixed in run run-7, pass 2"
        );
        // A record written before the run id was kept still reads.
        assert_eq!(
            status_label(&ReviewFindingStatus::Fixed {
                run_id: String::new(),
                iteration: 2
            }),
            "Fixed (pass 2)"
        );
        assert_eq!(
            status_label(&ReviewFindingStatus::Accepted {
                by: "architect".into(),
                reason: "tracked".into(),
                at: "t".into()
            }),
            "Accepted by architect: tracked"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn accept_posts_the_reason_to_the_unit_route() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/uow/acme%2Fapi%237/review-findings/2/accept"))
            .and(body_json(
                serde_json::json!({ "reason": "tracked elsewhere" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        assert!(accept_review_finding("acme/api#7", 2, "tracked elsewhere")
            .await
            .is_ok());
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_refused_accept_surfaces_the_servers_reason() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_json(serde_json::json!({ "reason": "That reviewer finding is already fixed or accepted." })),
            )
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let err = accept_review_finding("S-1", 0, "x").await.unwrap_err();
        assert_eq!(err, "That reviewer finding is already fixed or accepted.");
    }
}
