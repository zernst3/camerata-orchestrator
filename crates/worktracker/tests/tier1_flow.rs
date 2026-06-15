#![allow(clippy::unwrap_used)]
//! Tier-1 end-to-end composition test: a real Product Owner participates from their
//! board through the WorkItemProvider port, the same way for any provider.
//!
//! This drives the whole enterprise flow over the in-process NativeProvider (the
//! greenfield "ours is source of truth" case) and doubles as executable
//! documentation of how the adapters compose:
//!
//!   ingest a story -> clarify with the PO (post questions, poll the answer) ->
//!   write provenance / gate results / PR links / sign-off back -> the status moves.
//!
//! The Jira / Azure DevOps / GitHub adapters implement the same `WorkItemProvider`
//! trait, so this exact flow runs against a real board by swapping the provider.

use camerata_worktracker::{
    CanonicalStory, ClarifyBridge, ExternalRef, FeatureStatus, FeatureStatusReport, GateOutcome,
    GateResult, NativeProvider, PrLink, PrStatus, Provider, SignOff, WorkItemProvider,
};

fn reference() -> ExternalRef {
    ExternalRef {
        provider: Provider::Native,
        external_id: "FEAT-42".to_string(),
        container: None,
        url: "native://FEAT-42".to_string(),
        revision: None,
    }
}

fn seeded_story() -> CanonicalStory {
    CanonicalStory {
        id: "FEAT-42".to_string(),
        external_ref: Some(reference()),
        title: "Add CSV export to the reports page".to_string(),
        description: "The finance team wants to export the monthly report.".to_string(),
        status: FeatureStatus::Intake,
        created_by: "po@example.com".to_string(),
    }
}

#[tokio::test]
async fn tier1_flow_ingest_clarify_then_write_status_back() {
    let provider = NativeProvider::new();

    // 1. The story originates on the (native) board; the orchestrator ingests it.
    provider.seed_story(seeded_story());
    let ingested = provider.ingest_story(&reference()).await.unwrap();
    assert_eq!(ingested.title, "Add CSV export to the reports page");
    assert_eq!(ingested.status, FeatureStatus::Intake);

    // 2. The lead engineer needs a product clarification; the bridge carries it to
    //    the PO and back. (The PO can answer here; they never trigger execution.)
    let bridge = ClarifyBridge::new(&provider, reference());
    provider.inject_answer(
        reference(),
        "Comma-separated, one row per booking, with a header row",
    );
    let answer = bridge
        .ask_and_await(
            &["For the export, what format and shape do you want?".to_string()],
            None,
            3,
        )
        .await
        .unwrap();
    let answer = answer.expect("the PO answered on the board");
    assert!(answer.body.contains("Comma-separated"));
    // The question really landed on the board.
    assert!(provider
        .posted_questions()
        .iter()
        .any(|(_, qs)| qs.iter().any(|q| q.contains("what format"))));

    // 3. The governed agents run locally (out of scope here); the orchestrator
    //    writes the minimum-credible trail back onto the work item: PR links, gate
    //    results, and the human sign-off. These fields are ALWAYS ours.
    let report = FeatureStatusReport {
        status: FeatureStatus::SignedOff,
        pr_links: vec![PrLink {
            repo: "acme/reports".to_string(),
            url: "https://github.com/acme/reports/pull/7".to_string(),
            title: "Add CSV export".to_string(),
            status: PrStatus::Merged,
        }],
        gate_results: vec![GateResult {
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            result: GateOutcome::Pass,
            message: None,
        }],
        sign_off: Some(SignOff {
            by: "po@example.com".to_string(),
            at: "2026-06-14T05:00:00Z".to_string(),
        }),
        provenance_url: "https://camerata.local/provenance/FEAT-42".to_string(),
    };
    provider.push_status(&reference(), &report).await.unwrap();

    // 4. The board now reflects the new status (native is source of truth here).
    let after = provider.ingest_story(&reference()).await.unwrap();
    assert_eq!(after.status, FeatureStatus::SignedOff);
}
