//! `worktracker-demo` -- Tier-1 enterprise flow over the in-process NativeProvider.
//!
//! This shows a real Product Owner participating from their board, governed.
//! The SAME flow runs against Jira, Azure DevOps, or GitHub Issues by swapping
//! the provider; the adapters exist and implement the same WorkItemProvider trait.
//!
//! Sections:
//!
//!   1. INTAKE FROM THE BOARD     -- seed and ingest a CanonicalStory
//!   2. CLARIFY-BRIDGE            -- post questions to the PO's board; PO answers
//!   3. GOVERNED EXECUTION        -- narrated; write provenance and sign-off back
//!   4. LOOP AVOIDANCE            -- updatable_fields + ExpectedEchoTable guards
//!   5. SUMMARY                   -- WORKTRACKER-DEMO: PASS

use camerata_worktracker::{
    apply_inbound, updatable_fields, CanonicalStory, ClarifyBridge, ExpectedEchoTable, ExternalRef,
    FeatureStatus, FeatureStatusReport, GateOutcome, GateResult, InboundDisposition, InboundKind,
    InboundWorkItemEvent, NativeProvider, PrLink, PrStatus, Provider, SignOff, SyncPolicy,
    WorkItemProvider,
};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build the ExternalRef that identifies the demo story on the (native) board.
pub fn demo_ref() -> ExternalRef {
    ExternalRef {
        provider: Provider::Native,
        external_id: "FEAT-SSO-1".to_string(),
        container: None,
        url: "native://stories/FEAT-SSO-1".to_string(),
        revision: None,
    }
}

/// Build the seed CanonicalStory for the demo feature.
pub fn demo_story() -> CanonicalStory {
    CanonicalStory {
        id: "FEAT-SSO-1".to_string(),
        external_ref: Some(demo_ref()),
        title: "Add SSO to the admin portal".to_string(),
        description: "The security team requires SAML 2.0 SSO for all admin logins. \
            The admin portal currently uses password auth only."
            .to_string(),
        status: FeatureStatus::Intake,
        created_by: "po@enterprise.example".to_string(),
    }
}

/// Build the FeatureStatusReport that represents the governed execution result
/// written back to the board.
pub fn signed_off_report() -> FeatureStatusReport {
    FeatureStatusReport {
        status: FeatureStatus::SignedOff,
        pr_links: vec![
            PrLink {
                repo: "enterprise/admin-portal".to_string(),
                url: "https://github.com/enterprise/admin-portal/pull/412".to_string(),
                title: "feat: add SAML 2.0 SSO to admin login".to_string(),
                status: PrStatus::Merged,
            },
            PrLink {
                repo: "enterprise/admin-portal".to_string(),
                url: "https://github.com/enterprise/admin-portal/pull/413".to_string(),
                title: "test: SSO integration tests".to_string(),
                status: PrStatus::Merged,
            },
        ],
        gate_results: vec![GateResult {
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            result: GateOutcome::Pass,
            message: Some("No hardcoded credentials or secrets found in the diff.".to_string()),
        }],
        sign_off: Some(SignOff {
            by: "architect@enterprise.example".to_string(),
            at: "2026-06-14T14:00:00Z".to_string(),
        }),
        provenance_url: "https://camerata.local/provenance/FEAT-SSO-1".to_string(),
    }
}

// ── main demo entry-point ─────────────────────────────────────────────────────

/// Run the full Tier-1 enterprise flow over the in-process NativeProvider.
pub async fn run_worktracker_demo() -> anyhow::Result<()> {
    println!("== Camerata WORKTRACKER-DEMO: Tier-1 enterprise flow (NativeProvider) ==");
    println!();

    // ── 1. INTAKE FROM THE BOARD ──────────────────────────────────────────────
    println!("── 1. INTAKE FROM THE BOARD ──");

    let provider = NativeProvider::new();
    let story = demo_story();

    println!("  feature:    {}", story.title);
    println!("  created by: {}", story.created_by);
    println!(
        "  description: {}",
        story.description.lines().next().unwrap_or("")
    );

    // Seed the story as though it arrived from the enterprise board.
    provider.seed_story(story);

    let ingested = provider.ingest_story(&demo_ref()).await?;
    println!("  ingested:   \"{}\"", ingested.title);
    println!("  status:     {:?}", ingested.status);
    println!();

    // ── 2. CLARIFY-BRIDGE: the PO participates from their board ──────────────
    println!("── 2. CLARIFY-BRIDGE (PO answers from their board) ──");
    println!("  NOTE: the PO can ANSWER but never trigger execution. The architect");
    println!("  reviews the answer locally and runs the governed agents.");
    println!();

    let bridge = ClarifyBridge::new(&provider, demo_ref());

    let questions = vec![
        "Which SAML IdP vendors must be supported at launch (Okta, Azure AD, both)?".to_string(),
        "Should existing password sessions be preserved during the migration or cut over immediately?".to_string(),
    ];

    // Post the questions to the PO's board item.
    let pending = bridge.ask(&questions, None).await?;
    println!(
        "  posted {} question(s) as comment {}",
        questions.len(),
        pending.comment_id
    );
    for q in &pending.questions {
        println!("    Q: {q}");
    }

    // Simulate the PO replying on their board (in a real integration, this would
    // arrive as an inbound Commented webhook or poll row).
    provider.inject_answer(
        demo_ref(),
        "Both Okta and Azure AD are required from day one. \
         Existing sessions should remain valid for 30 days, then cut over.",
    );
    println!("  (PO replied on their board)");

    // Poll for the PO's answer.
    let (answers, _cursor) = bridge.poll_answers(pending.since_cursor.as_deref()).await?;
    let po_answer = answers
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no PO answer found in the poll"))?;

    println!("  PO answer: \"{}\"", po_answer.body);
    println!("  occurred:  {}", po_answer.occurred_at);
    println!();

    // ── 3. GOVERNED EXECUTION (narrated) ─────────────────────────────────────
    println!("── 3. GOVERNED EXECUTION (architect-gated) ──");
    println!("  The architect reviews the PO's answer locally. Governed agents run");
    println!("  through the Camerata gate (SEC-NO-HARDCODED-SECRETS-1 + others).");
    println!("  No agent can write outside the gate; the PO cannot trigger this step.");
    println!();

    let report = signed_off_report();

    // Push the provenance trail (PR links, gate results, sign-off) back to the board.
    provider.push_status(&demo_ref(), &report).await?;
    println!(
        "  pushed status report: {:?} ({} PR(s), {} gate result(s), sign-off: {})",
        report.status,
        report.pr_links.len(),
        report.gate_results.len(),
        report
            .sign_off
            .as_ref()
            .map(|s| s.by.as_str())
            .unwrap_or("none"),
    );
    for pr in &report.pr_links {
        println!("    PR: [{}] {}", pr.repo, pr.title);
    }
    for gate in &report.gate_results {
        println!(
            "    gate: {} -> {:?}{}",
            gate.rule_id,
            gate.result,
            gate.message
                .as_deref()
                .map(|m| format!(" ({m})"))
                .unwrap_or_default(),
        );
    }

    // Re-ingest to confirm the status moved.
    let after = provider.ingest_story(&demo_ref()).await?;
    println!("  status after push: {:?}  (was Intake)", after.status);
    assert_eq!(
        after.status,
        FeatureStatus::SignedOff,
        "status must be SignedOff after push"
    );
    println!();

    // ── 4. LOOP AVOIDANCE ────────────────────────────────────────────────────
    println!("── 4. LOOP AVOIDANCE: two independent guards ──");

    // Guard 1: per-field direction via SyncPolicy + updatable_fields.
    let greenfield = SyncPolicy::greenfield();
    let brownfield = SyncPolicy::brownfield();
    let gf_fields = updatable_fields(&greenfield);
    let bf_fields = updatable_fields(&brownfield);

    println!("  Guard 1 -- per-field direction (SyncPolicy):");
    println!(
        "    greenfield (ours is source of truth): updatable from tracker = {:?}",
        gf_fields
    );
    println!(
        "    brownfield (tracker is authoritative): updatable from tracker = {:?}",
        bf_fields
    );
    println!("    Greenfield: zero tracker-authoritative fields -- Camerata owns everything.");
    println!("    Brownfield: tracker owns title, description, status. Provenance is always ours.");
    println!();

    // Also demonstrate apply_inbound respects the policy.
    let mut story_copy = demo_story();
    // Greenfield: a tracker event must not overwrite anything.
    let event_gf = InboundWorkItemEvent {
        reference: demo_ref(),
        kind: InboundKind::Updated,
        title: Some("Tracker-renamed title".to_string()),
        description: Some("Tracker-overwritten description.".to_string()),
        status: Some(FeatureStatus::Executing),
        body: None,
        delivery_id: "evt-gf-001".to_string(),
        is_echo: false,
        occurred_at: "2026-06-14T15:00:00Z".to_string(),
    };
    let applied_gf = apply_inbound(&greenfield, &mut story_copy, &event_gf);
    println!(
        "    apply_inbound with greenfield policy: applied fields = {:?}",
        applied_gf
    );
    println!(
        "    title still \"{}\" (tracker change ignored)",
        story_copy.title
    );
    println!();

    // Guard 2: echo suppression via ExpectedEchoTable.
    println!("  Guard 2 -- echo suppression (ExpectedEchoTable):");

    let mut echo_table = ExpectedEchoTable::new();

    // Camerata writes to the tracker and records the expected echo.
    echo_table.record_write(demo_ref(), "rev-sso-v7", "2026-06-14T14:00:00Z");

    // The same write bounces back from the tracker (echo, revision matches).
    let echo_event = InboundWorkItemEvent {
        reference: ExternalRef {
            revision: Some("rev-sso-v7".to_string()),
            ..demo_ref()
        },
        kind: InboundKind::Updated,
        title: None,
        description: None,
        status: Some(FeatureStatus::SignedOff),
        body: None,
        delivery_id: "delivery-echo-001".to_string(),
        is_echo: false,
        occurred_at: "2026-06-14T14:01:00Z".to_string(),
    };

    // Same delivery redelivered by the tracker (duplicate, delivery id already seen).
    let redelivery = InboundWorkItemEvent {
        delivery_id: "delivery-echo-001".to_string(),
        ..echo_event.clone()
    };

    // A genuinely new event from another user or pipeline step.
    let fresh_event = InboundWorkItemEvent {
        reference: demo_ref(),
        kind: InboundKind::Commented,
        title: None,
        description: None,
        status: None,
        body: Some("QA sign-off confirmed by the security team.".to_string()),
        delivery_id: "delivery-fresh-002".to_string(),
        is_echo: false,
        occurred_at: "2026-06-14T14:30:00Z".to_string(),
    };

    let echo_class = echo_table.classify_inbound(&echo_event);
    let dup_class = echo_table.classify_inbound(&redelivery);
    let fresh_class = echo_table.classify_inbound(&fresh_event);

    println!("    recorded outbound write -> rev-sso-v7");
    println!("    echo bounce-back:  classify_inbound = {:?}", echo_class);
    println!("    redelivery:        classify_inbound = {:?}", dup_class);
    println!(
        "    new external event: classify_inbound = {:?}",
        fresh_class
    );

    assert_eq!(
        echo_class,
        InboundDisposition::Echo,
        "bounce-back must be Echo"
    );
    assert_eq!(
        dup_class,
        InboundDisposition::Duplicate,
        "replay must be Duplicate"
    );
    assert_eq!(
        fresh_class,
        InboundDisposition::Fresh,
        "new event must be Fresh"
    );

    println!();

    // ── 5. SUMMARY ───────────────────────────────────────────────────────────
    println!("── SUMMARY ──");
    println!("  A real Product Owner participated through their board (native in this demo).");
    println!("  The PO answered the clarifying questions on the board.");
    println!("  The PO did NOT and could NOT trigger execution.");
    println!("  The architect reviewed the answer and ran governed agents through the gate.");
    println!("  Provenance (PR links, gate pass, sign-off) was written back to the board.");
    println!("  Status moved: Intake -> SignedOff.");
    println!("  Loop avoidance: Guard 1 (per-field SyncPolicy) + Guard 2 (echo table)");
    println!("  prevented any sync war.");
    println!();
    println!("  Provider-neutrality: the SAME flow runs against Jira, Azure DevOps, or");
    println!("  GitHub Issues by swapping the provider. The adapters exist and implement");
    println!("  the same WorkItemProvider trait; core never imports provider-specific code.");
    println!();
    println!("WORKTRACKER-DEMO: PASS");

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── signed_off_report shape ───────────────────────────────────────────────

    #[test]
    fn signed_off_report_has_expected_shape() {
        let report = signed_off_report();
        assert_eq!(report.status, FeatureStatus::SignedOff);
        assert_eq!(report.pr_links.len(), 2, "expect exactly two PR links");
        assert_eq!(
            report.gate_results.len(),
            1,
            "expect exactly one gate result"
        );
        assert_eq!(
            report.gate_results[0].result,
            GateOutcome::Pass,
            "gate must be a pass"
        );
        assert!(report.sign_off.is_some(), "sign-off must be present");
        assert_eq!(
            report.sign_off.as_ref().unwrap().by,
            "architect@enterprise.example"
        );
    }

    #[test]
    fn demo_story_starts_as_intake() {
        let story = demo_story();
        assert_eq!(story.status, FeatureStatus::Intake);
        assert!(!story.title.is_empty());
        assert!(story.external_ref.is_some());
    }

    // ── push_status moves the story to SignedOff ──────────────────────────────

    #[tokio::test]
    async fn status_moves_to_signed_off_after_push() {
        let provider = NativeProvider::new();
        provider.seed_story(demo_story());

        // Intake -> SignedOff via push_status.
        let report = signed_off_report();
        provider.push_status(&demo_ref(), &report).await.unwrap();

        let after = provider.ingest_story(&demo_ref()).await.unwrap();
        assert_eq!(after.status, FeatureStatus::SignedOff);
    }

    // ── clarify bridge round-trip ─────────────────────────────────────────────

    #[tokio::test]
    async fn clarify_bridge_round_trip() {
        let provider = NativeProvider::new();
        provider.seed_story(demo_story());

        let bridge = ClarifyBridge::new(&provider, demo_ref());
        let questions = vec!["Which IdP vendors?".to_string()];

        // Inject the PO's answer before asking, so the poll finds it immediately.
        provider.inject_answer(demo_ref(), "Okta and Azure AD, both required.");

        let answer = bridge.ask_and_await(&questions, None, 3).await.unwrap();

        assert!(answer.is_some(), "PO answer must be returned");
        let body = answer.unwrap().body;
        assert!(body.contains("Okta"), "answer must contain IdP name");
    }

    // ── echo classification ───────────────────────────────────────────────────

    #[test]
    fn echo_table_classifications_are_correct() {
        let mut table = ExpectedEchoTable::new();
        table.record_write(demo_ref(), "rev-x1", "2026-06-14T00:00:00Z");

        // Echo: revision matches our recorded write.
        let echo_ev = InboundWorkItemEvent {
            reference: ExternalRef {
                revision: Some("rev-x1".to_string()),
                ..demo_ref()
            },
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-echo".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T00:01:00Z".to_string(),
        };
        assert_eq!(table.classify_inbound(&echo_ev), InboundDisposition::Echo);

        // Duplicate: same delivery id already seen.
        assert_eq!(
            table.classify_inbound(&echo_ev),
            InboundDisposition::Duplicate
        );

        // Fresh: new delivery id, no recorded write.
        let fresh_ev = InboundWorkItemEvent {
            reference: demo_ref(),
            kind: InboundKind::Commented,
            title: None,
            description: None,
            status: None,
            body: Some("Fresh external event.".to_string()),
            delivery_id: "d-fresh".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T00:02:00Z".to_string(),
        };
        assert_eq!(table.classify_inbound(&fresh_ev), InboundDisposition::Fresh);
    }

    // ── updatable_fields contract ─────────────────────────────────────────────

    #[test]
    fn greenfield_policy_has_no_updatable_fields() {
        let fields = updatable_fields(&SyncPolicy::greenfield());
        assert!(
            fields.is_empty(),
            "greenfield: ours is source of truth, nothing is tracker-authoritative"
        );
    }

    #[test]
    fn brownfield_policy_has_all_three_updatable_fields() {
        let fields = updatable_fields(&SyncPolicy::brownfield());
        assert_eq!(fields.len(), 3);
        assert!(fields.contains(&"title"));
        assert!(fields.contains(&"description"));
        assert!(fields.contains(&"status"));
    }

    // ── full end-to-end: the demo itself must complete without error ──────────

    #[tokio::test]
    async fn worktracker_demo_runs_without_error() {
        run_worktracker_demo()
            .await
            .expect("worktracker-demo must not error");
    }
}
