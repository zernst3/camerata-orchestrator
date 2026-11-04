//! Tests for merge.

use super::*;

// ── P1: deduplication + cross-tier merge ────────────────────────────────────────────
// the codebase-inspection hardening notes, P1. Synthetic findings only —
// never keyed to any one benchmark's rule/file/table names.

#[test]
fn p1_security_row_heads_the_pair_at_one_location_regardless_of_severity() {
    // The canonical P1 regression, at the EXACT same location (both rows cite the same
    // middleware line). The structural row here is even HIGHER severity, to prove class
    // beats severity. Nothing is folded: both rows stay, the security row heads the pair
    // and the structural row links to it.
    let code = "app.use(cors())";
    let files = vec![("middleware.ts".to_string(), code.to_string())];
    let structural = site_finding("ARCH-MIDDLEWARE-FIRST-1", "middleware.ts", 12, "high", code);
    let security = site_finding(
        "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS",
        "middleware.ts",
        12,
        "medium",
        code,
    );
    assert_eq!(
        head_row(vec![structural.clone(), security.clone()]).rule_id,
        "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS",
        "the security finding heads the pair, never hidden behind a structural row"
    );
    let rows = crate::review_tree::collapse_and_group(vec![structural, security], &files);
    assert_eq!(
        rows.len(),
        2,
        "both rows cite the same code location and both stay"
    );
    assert_eq!(
        crate::review_tree::test_support::link_pairs(&rows),
        vec![(
            "ARCH-MIDDLEWARE-FIRST-1".to_string(),
            "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS".to_string(),
            "same_line".to_string()
        )]
    );
}

#[test]
fn p1_semantic_group_shared_object_security_heads_the_cluster() {
    // Cross-file, cross-class cluster via a shared captured object: a Hygiene structural
    // finding and a Security finding both name the SAME table, in different files.
    // Security heads the cluster even though the structural finding is deterministic AND
    // more severe, and each row keeps its own severity.
    let mut structural = site_finding("ARCH-MIDDLEWARE-FIRST-1", "router.ts", 5, "critical", "");
    structural
        .captures
        .insert("table".to_string(), "profiles".to_string());
    let mut security = site_finding("AI-RLS-MISSING-ON-PROFILES", "schema.sql", 40, "low", "");
    security.category = Some("rls-policy".to_string());
    security
        .captures
        .insert("table".to_string(), "profiles".to_string());
    let groups = clusters(vec![structural, security], &[]);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        head_of(&groups[0]).rule_id,
        "AI-RLS-MISSING-ON-PROFILES",
        "security heads it even though it is lower severity and the other side is deterministic"
    );
    let severities: Vec<&str> = groups[0].iter().map(|f| f.severity.as_str()).collect();
    assert_eq!(severities, vec!["critical", "low"], "no severity is moved");
}

#[test]
fn p1_primary_tiebreak_severity_wins_within_same_class() {
    // Same class (both Hygiene, both invented AI- names, no calibrated confidence) —
    // severity alone decides, in isolation from the other keys.
    let low = site_finding("AI-HYGIENE-LOW", "f.rs", 10, "low", "");
    let high = site_finding("AI-HYGIENE-HIGH", "f.rs", 10, "high", "");
    let merged = head_row(vec![low, high]);
    assert_eq!(merged.rule_id, "AI-HYGIENE-HIGH");
}

#[test]
fn p1_primary_tiebreak_confidence_wins_within_same_class_and_severity() {
    // Same class (neither rule id nor category is security-flavored), same severity, and
    // the SAME origin tier (both confidence.is_some() ⇒ both AdoptedAi) — confidence alone
    // decides: a "needs-review"-flagged finding must not win over an equally-severe,
    // clearer one.
    let mut needs_review = site_finding("RULE-A", "f.rs", 10, "high", "");
    needs_review.confidence = Some("needs-review".to_string());
    let mut clear = site_finding("RULE-B", "f.rs", 10, "high", "");
    clear.confidence = Some("high".to_string());
    let merged = head_row(vec![needs_review, clear]);
    assert_eq!(merged.rule_id, "RULE-B");
}

#[test]
fn p1_primary_tiebreak_specificity_wins_when_severity_and_confidence_tie() {
    // Same class, same severity, same confidence (both None — neither ran through
    // calibration) — specificity (deterministic beats an invented AI- name) decides.
    let deterministic = site_finding("RULE-DETERMINISTIC", "f.rs", 10, "medium", "");
    let invented = site_finding("AI-INVENTED-NAME", "f.rs", 10, "medium", "");
    let merged = head_row(vec![deterministic, invented]);
    assert_eq!(merged.rule_id, "RULE-DETERMINISTIC");
}

#[test]
fn p1_cross_file_shared_captured_object_shares_a_cluster() {
    // A config-flag finding in one file and the handler finding that reads that SAME flag
    // in another file are one root cause: the general "same object across files" signal,
    // independent of category or line proximity. They share a cluster and stay two rows;
    // the deterministic config-flag finding heads it (origin outranks severity) and each
    // row keeps its own severity and its own site.
    let mut flag = site_finding("CONFIG-FLAG-DEBUG-MODE", "config.rs", 5, "medium", "");
    flag.captures
        .insert("flag".to_string(), "debug_mode".to_string());
    let mut handler = site_finding("AI-HANDLER-TRUSTS-DEBUG-FLAG", "handler.rs", 80, "high", "");
    handler
        .captures
        .insert("flag".to_string(), "debug_mode".to_string());
    let groups = clusters(vec![flag, handler], &[]);
    assert_eq!(
        groups.len(),
        1,
        "cross-file findings sharing a captured object cluster"
    );
    assert_eq!(groups[0].len(), 2);
    let head = head_of(&groups[0]);
    assert_eq!(head.rule_id, "CONFIG-FLAG-DEBUG-MODE");
    assert_eq!(
        head.severity, "medium",
        "the head keeps its own grounded severity"
    );
    let handler = groups[0].iter().find(|f| f.path == "handler.rs").unwrap();
    assert_eq!((handler.line, handler.severity.as_str()), (80, "high"));
    assert!(
        handler.located,
        "both sides cite real, independently-actionable code"
    );
}

#[test]
fn p1_consequence_member_stays_its_own_row_in_the_root_cause_cluster() {
    // Root cause: a deterministic RLS-missing finding on the table's own policy site.
    let mut root = site_finding("SEC-RLS-MISSING", "policy.sql", 5, "high", "");
    root.captures
        .insert("table".to_string(), "profiles".to_string());
    // Consequence: an AI finding describing that a page is exposed ONLY because of the
    // missing policy above: no independently-fixable code of its own (`located = false`).
    let mut consequence = site_finding(
        "AI-PAGE-EXPOSED-VIA-MISSING-RLS",
        "page.tsx",
        40,
        "medium",
        "",
    );
    consequence
        .captures
        .insert("table".to_string(), "profiles".to_string());
    consequence.located = false;
    let groups = clusters(vec![root, consequence], &[]);
    assert_eq!(groups.len(), 1, "they share a root cause");
    assert_eq!(
        groups[0].len(),
        2,
        "the consequence is still a row of its own"
    );
    assert_eq!(head_of(&groups[0]).rule_id, "SEC-RLS-MISSING");
    let consequence = groups[0].iter().find(|f| f.path == "page.tsx").unwrap();
    assert!(
        !consequence.located,
        "it is still marked as having no fix of its own"
    );
}

#[test]
fn p1_an_unrelated_rule_is_never_in_another_cluster() {
    // Two findings form a genuine cluster (same category, adjacent lines); a third, wholly
    // unrelated finding (different file, different category, no shared object) is never
    // in the cluster's rows. One deterministic + one AI-invented so the AI+AI corroboration
    // guard does not apply (this test is about cluster ISOLATION).
    let mut a = site_finding("AUTHZ-MISSING-CHECK", "h.rs", 10, "high", "");
    a.category = Some("authorization".to_string());
    let mut b = site_finding("AI-AUTHZ-MISSING-2", "h.rs", 12, "medium", "");
    b.category = Some("authorization".to_string());
    let unrelated = site_finding("PERF-N-PLUS-ONE-1", "other.rs", 500, "low", "");
    let groups = clusters(vec![a, b, unrelated], &[]);
    assert_eq!(groups.len(), 2, "the unrelated finding is its own cluster");
    let pair = groups
        .iter()
        .find(|g| g.iter().any(|f| f.rule_id == "AI-AUTHZ-MISSING-2"))
        .expect("the a/b cluster must exist");
    assert!(
        pair.iter().all(|f| f.rule_id != "PERF-N-PLUS-ONE-1"),
        "an unrelated rule is never linked into the cluster: {pair:?}"
    );
    let alone = groups
        .iter()
        .find(|g| g.iter().any(|f| f.rule_id == "PERF-N-PLUS-ONE-1"))
        .unwrap();
    assert_eq!(alone.len(), 1);
}

#[test]
fn p1_merge_never_drops_a_distinct_root_cause() {
    // Two GENUINELY distinct defects, each duplicated across two rule families (4 raw
    // findings total). Merging must collapse each duplicate pair into one row WITHOUT
    // losing either distinct defect — output must be exactly 2, never 1 (over-merged) or 4
    // (under-merged).
    let mut det1 = site_finding("RLS-MISSING", "a.sql", 10, "high", "");
    det1.category = Some("rls-policy".to_string());
    let mut ai1 = site_finding("AI-RLS-GAP", "a.sql", 12, "medium", "");
    ai1.category = Some("rls-policy".to_string());

    // Categories left unset so both backfill to "injection" via the rule-id heuristic.
    let det2 = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "b.ts", 40, "critical", "");
    let ai2 = site_finding("AI-SQL-INJECTION", "b.ts", 41, "high", "");

    let out = cluster_heads(vec![det1, ai1, det2, ai2], &[]);
    assert_eq!(
        out.len(),
        2,
        "two distinct defects survive as exactly two rows — nothing dropped, nothing over-merged"
    );
}

// ── MERGE (cycle-2 queue-hardening, the cycle-2 queue-hardening notes) ────
// Gap (ii): two deterministic findings sharing a captured object (same root cause — e.g. two
// secret-detectors naming the SAME committed secret) were blocked from ever merging by the
// unconditional det+det guard. Synthetic findings only, fixture-independent.

#[test]
fn merge_gap_ii_two_deterministic_findings_sharing_captured_object_cluster() {
    // Two DIFFERENT deterministic secret-detectors independently flag the SAME committed
    // secret value in the same file — same root cause, not two distinct defects. The
    // relaxed det+det guard (only blocks when there is NO shared captured object) must let
    // this merge, proving gap (ii) is fixed.
    let mut det_a = site_finding("SEC-SECRET-SCANNER-A-1", "src/config.ts", 10, "high", "");
    det_a
        .captures
        .insert("secret".to_string(), "sk_live_abc123xyz".to_string());
    let mut det_b = site_finding(
        "SEC-SECRET-SCANNER-B-1",
        "src/config.ts",
        10,
        "critical",
        "",
    );
    det_b
        .captures
        .insert("value".to_string(), "sk_live_abc123xyz".to_string());

    let groups = clusters(vec![det_a, det_b], &[]);
    assert_eq!(
        groups.len(),
        1,
        "two det findings naming the SAME captured secret are one cluster: {groups:?}"
    );
    let ids = crate::review_tree::test_support::cluster_rule_ids(&groups[0]);
    assert!(
        ids.contains("SEC-SECRET-SCANNER-A-1") && ids.contains("SEC-SECRET-SCANNER-B-1"),
        "both rows are in the cluster: {ids:?}"
    );
}

#[test]
fn merge_gap_ii_two_deterministic_findings_with_no_shared_object_stay_distinct() {
    // Two deterministic findings in the same file with NO shared captured object and
    // non-overlapping descriptions are genuinely different defects — the relaxed guard must
    // NOT open the door to merging det+det pairs in general, only the shared-object case.
    let mut det_a = site_finding("SEC-A-1", "src/config.ts", 10, "high", "");
    det_a.detail =
        "Missing input validation on the signup form allows arbitrary payloads.".to_string();
    let mut det_b = site_finding("SEC-B-1", "src/config.ts", 11, "high", "");
    det_b.detail = "Outbound webhook requests do not verify the TLS certificate chain.".to_string();

    let out = cluster_heads(vec![det_a, det_b], &[]);
    assert_eq!(
        out.len(),
        2,
        "distinct det+det defects with no shared object must stay two rows: {out:?}"
    );
}

// ── C6-B2: evidence-chain / root-cause merge signal ─────────────────────────────────────
// ONE leaked credential, named/used across 3+ files (the holder, the module constructing a
// client from it, and its call sites), must collapse into ONE row — see
// `root_cause_credential_identities`/`backfill_credential_identity`'s doc comments.

#[test]
fn c6b2_leaked_credential_chain_across_four_findings_three_files_merges_into_one() {
    // The holder (deterministic secret-detector), the module that constructs a client from
    // the SAME secret, and two downstream call sites that use that client — each one's own
    // text names the identical credential identifier, in a different file, under different
    // rule families, far apart in line number. All four are ONE defect (delete the secret +
    // rotate it once), not four.
    let mut holder = site_finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        "src/config.ts",
        5,
        "critical",
        "",
    );
    holder.detail = "STRIPE_SECRET_KEY is committed in plaintext in this config file.".to_string();
    holder.effort = Some("high".to_string());

    let mut constructor = site_finding(
        "AI-STRIPE-CLIENT-FROM-LEAKED-KEY",
        "src/payments/stripeClient.ts",
        212,
        "critical",
        "",
    );
    constructor.detail =
        "Constructs a Stripe client directly from the committed STRIPE_SECRET_KEY constant."
            .to_string();
    constructor.effort = Some("high".to_string());

    let mut call_site_a = site_finding(
        "AI-PAYMENT-CALL-USES-LEAKED-KEY-1",
        "src/payments/charge.ts",
        40,
        "critical",
        "",
    );
    call_site_a.detail =
        "Calls stripe.charges.create using the client built from STRIPE_SECRET_KEY.".to_string();
    call_site_a.effort = Some("high".to_string());

    let mut call_site_b = site_finding(
        "AI-PAYMENT-CALL-USES-LEAKED-KEY-2",
        "src/payments/refund.ts",
        900,
        "critical",
        "",
    );
    call_site_b.detail =
        "Issues a refund via the same client built from STRIPE_SECRET_KEY.".to_string();
    call_site_b.effort = Some("high".to_string());

    let groups = clusters(vec![holder, constructor, call_site_a, call_site_b], &[]);
    assert_eq!(
        groups.len(),
        1,
        "one leaked credential referenced by 4 findings across 3+ files is ONE cluster: {groups:?}"
    );
    let rows = &groups[0];
    assert_eq!(rows.len(), 4, "all four are still rows");
    let head = head_of(rows);
    assert_eq!(
        head.rule_id, "SEC-NO-HARDCODED-SECRETS-1",
        "the deterministic holder finding heads the cluster"
    );
    let ids = crate::review_tree::test_support::cluster_rule_ids(rows);
    for expected in [
        "SEC-NO-HARDCODED-SECRETS-1",
        "AI-STRIPE-CLIENT-FROM-LEAKED-KEY",
        "AI-PAYMENT-CALL-USES-LEAKED-KEY-1",
        "AI-PAYMENT-CALL-USES-LEAKED-KEY-2",
    ] {
        assert!(
            ids.contains(expected),
            "missing rule id {expected} in {ids:?}"
        );
    }
    let paths: std::collections::HashSet<&str> = rows.iter().map(|l| l.path.as_str()).collect();
    for expected_path in [
        "src/payments/stripeClient.ts",
        "src/payments/charge.ts",
        "src/payments/refund.ts",
    ] {
        assert!(
            paths.contains(expected_path),
            "missing location {expected_path} in {paths:?}"
        );
    }
    // Every row keeps its own estimate; nothing sums them.
    assert!(rows.iter().all(|r| r.effort == Some("high".to_string())));
}

#[test]
fn c6b2_two_different_leaked_credentials_stay_two_rows() {
    // Safe twin: TWO DIFFERENT leaked credentials, each independently committed/used, must
    // NOT cross-merge just because both are "a leaked secret" — the root-cause signal
    // requires the SAME concrete identifier, never merely "both are critical".
    let mut stripe_holder = site_finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        "src/config.ts",
        5,
        "critical",
        "",
    );
    stripe_holder.detail =
        "STRIPE_SECRET_KEY is committed in plaintext in this config file.".to_string();

    let mut sendgrid_holder = site_finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        "src/email/config.ts",
        9,
        "critical",
        "",
    );
    sendgrid_holder.detail =
        "SENDGRID_API_KEY is committed in plaintext in this config file.".to_string();

    let out = cluster_heads(vec![stripe_holder, sendgrid_holder], &[]);
    assert_eq!(
        out.len(),
        2,
        "two DIFFERENT leaked credentials must stay two rows, never cross-merged on \
             \"both are a leaked secret\" alone: {out:?}"
    );
}

#[test]
fn c6b2_backfill_never_overwrites_an_existing_capture() {
    // The credential backfill must be purely additive: a finding that already carries a
    // `captures` entry for an unrelated purpose (e.g. a table name from
    // `backfill_captured_object`) must keep that entry intact AND still gain the credential
    // token under its own key.
    let mut f = site_finding("SEC-MIXED-1", "src/handler.ts", 10, "high", "");
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    f.detail = "Reads DB_PASSWORD to connect before querying profiles.".to_string();
    backfill_credential_identity(&mut f);
    assert_eq!(
        f.captures.get("table").map(String::as_str),
        Some("profiles"),
        "the pre-existing capture must survive untouched"
    );
    assert_eq!(
        f.captures.get("DB_PASSWORD").map(String::as_str),
        Some("DB_PASSWORD"),
        "the credential token is additionally recorded under its own key"
    );
}

// Gap (i): det+AI describing one defect a few lines apart with near-identical prose, in
// different categories, previously missed both semantic signals.

#[test]
fn merge_gap_i_det_and_ai_adjacent_lines_overlapping_description_different_category_cluster() {
    // A deterministic secret-detector and an AI finding, three lines apart, in DIFFERENT
    // categories (so signal (a) never fires) but describing the SAME defect in near-identical
    // prose. The description-overlap signal clusters them: two rows, one cluster.
    let mut det = site_finding("SEC-HARDCODED-API-KEY-1", "src/config.ts", 10, "high", "");
    det.detail =
        "A live Stripe secret key is hardcoded in this file and committed to the repository."
            .to_string();
    det.category = Some("secrets-hygiene".to_string());

    let mut ai = site_finding("AI-STRIPE-KEY-EXPOSED", "src/config.ts", 12, "medium", "");
    ai.detail = "A live stripe secret key is hardcoded in this file and committed to the \
                     repository, risking compromise."
        .to_string();
    ai.category = Some("credential-exposure".to_string());

    let groups = clusters(vec![det, ai], &[]);
    assert_eq!(
        groups.len(),
        1,
        "near-identical prose on adjacent lines is one cluster: {groups:?}"
    );
    let ids = crate::review_tree::test_support::cluster_rule_ids(&groups[0]);
    assert!(
        ids.contains("SEC-HARDCODED-API-KEY-1") && ids.contains("AI-STRIPE-KEY-EXPOSED"),
        "both rows are in the cluster: {ids:?}"
    );
    let lines = crate::review_tree::test_support::cluster_lines(&groups[0]);
    assert!(
        lines.contains(&10) && lines.contains(&12),
        "both lines kept: {lines:?}"
    );
}

#[test]
fn merge_gap_i_sink_and_call_site_in_different_files_stay_two_rows() {
    // PRESERVE (design "Do NOT break"): a sink finding and a call-site finding describing
    // the SAME overarching vulnerability, but in TWO DIFFERENT files, with genuinely
    // overlapping prose. The new description-overlap signal is deliberately same-path-only,
    // so this must NOT merge even though the text is near-identical — proving the signal
    // never crosses files.
    let mut sink = site_finding(
        "SEC-SQL-INJECTION-SINK-1",
        "src/db/sink.ts",
        50,
        "critical",
        "",
    );
    sink.detail = "User-controlled input flows into a raw SQL query without \
                       parameterization, enabling SQL injection."
        .to_string();
    let mut call_site = site_finding(
        "AI-SQL-INJECTION-CALL-SITE",
        "src/api/handler.ts",
        20,
        "high",
        "",
    );
    call_site.detail = "User-controlled input flows into a raw SQL query without \
                             parameterization, enabling SQL injection at this call site."
        .to_string();

    let out = cluster_heads(vec![sink, call_site], &[]);
    assert_eq!(
        out.len(),
        2,
        "the sink+call-site pair across two files must stay two rows: {out:?}"
    );
}

// ── MERGE (cycle-3 queue-hardening): one defect shipping as 2-3 rows ────────────────────
// Synthetic findings only, fixture-independent (observed-failures 1-3 from the cycle-3
// triage, never reproduced against the actual scan output).

#[test]
fn merge_gap_i_near_identical_prose_clusters_beyond_window_distance() {
    // Observed failure 1: an unauthenticated-export defect shipped as two critical rows 9
    // lines apart in the same file, outside SEMANTIC_MERGE_WINDOW (5), so only the
    // description-overlap signal can pair them. Realistically paraphrased prose (see the
    // threshold's doc comment for the score).
    let mut det = site_finding(
        "SEC-UNAUTH-EXPORT-1",
        "src/export/handler.ts",
        10,
        "critical",
        "audit export endpoint missing authentication check",
    );
    det.detail = "the export handler returns the full audit report with no auth check \
                       so any caller can read it"
        .to_string();
    let mut ai = site_finding(
        "AI-EXPORT-NO-AUTH-CHECK",
        "src/export/handler.ts",
        19,
        "critical",
        "audit export route missing auth validation",
    );
    ai.detail = "this export handler returns the full audit report without an auth check \
                      so any user can read it"
        .to_string();

    let groups = clusters(vec![det, ai], &[]);
    assert_eq!(
        groups.len(),
        1,
        "near-identical prose 9 lines apart (outside the window) is still one cluster: {groups:?}"
    );
    let ids = crate::review_tree::test_support::cluster_rule_ids(&groups[0]);
    assert!(
        ids.contains("SEC-UNAUTH-EXPORT-1") && ids.contains("AI-EXPORT-NO-AUTH-CHECK"),
        "both rows are in the cluster: {ids:?}"
    );
    let lines = crate::review_tree::test_support::cluster_lines(&groups[0]);
    assert!(
        lines.contains(&10) && lines.contains(&19),
        "both lines kept: {lines:?}"
    );
}

#[test]
fn merge_cycle3_security_class_heads_the_cluster_and_no_severity_is_moved() {
    // Observed failure 2: a cross-tenant-read defect shipped as a correct SECURITY rule at
    // LOW ("borderline") plus a generic rule at CRITICAL — the generic row must never shadow
    // the security finding. `primary_rank` ranks `FindingClass` (Security > Hygiene) ahead
    // of severity, so the security rule must win the primary slot even though its OWN
    // severity is lower; `merge_semantic_group` keeps `max_sev` independent of which side won
    // primary, so the merged row's severity must still be "critical", not the security row's
    // own "low". Clustered via a shared captured object (design point 2b, general and
    // line/category-independent) so the test exercises class+severity selection in
    // isolation, not the line-window signal.
    let mut sec_low = site_finding(
        "AUTH-MISSING-TENANT-CHECK-1",
        "api/orders.rs",
        15,
        "low",
        "org id not verified against the session tenant",
    );
    sec_low
        .captures
        .insert("table".to_string(), "orders".to_string());

    let mut generic_critical = site_finding(
        "ARCH-RESOURCE-LEAK-GENERIC-1",
        "api/orders.rs",
        400,
        "critical",
        "generic resource-handling smell flagged on the same table",
    );
    generic_critical
        .captures
        .insert("table".to_string(), "orders".to_string());

    assert_eq!(
        finding_class(&sec_low),
        FindingClass::Security,
        "sanity: the AUTH-prefixed rule must classify Security"
    );
    assert_eq!(
        finding_class(&generic_critical),
        FindingClass::Hygiene,
        "sanity: the generic ARCH rule must classify Hygiene, not inherit Security"
    );

    let groups = clusters(vec![sec_low, generic_critical], &[]);
    assert_eq!(groups.len(), 1, "the pair shares a cluster: {groups:?}");
    assert_eq!(groups[0].len(), 2, "and stays two rows");
    assert_eq!(
        head_of(&groups[0]).rule_id,
        "AUTH-MISSING-TENANT-CHECK-1",
        "the SECURITY rule heads the cluster even at lower severity"
    );
    let severities: Vec<&str> = groups[0].iter().map(|f| f.severity.as_str()).collect();
    assert_eq!(
        severities,
        vec!["low", "critical"],
        "each row keeps its own severity: the generic critical is still a critical row"
    );
}

#[test]
fn merge_cycle3_same_rule_id_twice_adjacent_is_one_cluster_of_two_rows() {
    // Observed failure 3: the same rule id firing twice on adjacent lines for one response
    // must collapse to one row. Deliberately NO shared category (an invented AI- id with no
    // taxonomy mapping — categorize_rule_id backfills both to None) and NO prose overlap
    // (the two detail strings share essentially no words), so signals (a) and (c) are both
    // unavailable — only the new same-rule-id-adjacent signal can catch this.
    let mut first = site_finding(
        "AI-WIDGET-MISCONFIG-REPEAT-1",
        "src/widgets/panel.rs",
        100,
        "high",
        "",
    );
    first.detail =
        "A background retry loop hammers the queue with no backoff policy at all.".to_string();
    let mut second = site_finding(
        "AI-WIDGET-MISCONFIG-REPEAT-1",
        "src/widgets/panel.rs",
        102,
        "high",
        "",
    );
    second.detail =
        "Totally unrelated phrasing on a second pass, written differently by the model."
            .to_string();
    assert!(
            description_overlap_score(&first, &second) < DESCRIPTION_OVERLAP_THRESHOLD,
            "sanity: this pair must NOT merge via description overlap, isolating the same-rule-id signal"
        );
    assert_eq!(
        categorize_rule_id(&first.rule_id),
        None,
        "sanity: this invented rule id must have no taxonomy mapping, isolating the \
             same-rule-id signal from signal (a)'s same-category path"
    );

    let groups = clusters(vec![first.clone(), second.clone()], &[]);
    assert_eq!(
        groups.len(),
        1,
        "the same rule id firing twice on adjacent lines is one cluster: {groups:?}"
    );
    assert_eq!(groups[0].len(), 2, "both rows are kept");
    assert_eq!(groups[0][0].rule_id, "AI-WIDGET-MISCONFIG-REPEAT-1");
    let lines = crate::review_tree::test_support::cluster_lines(&groups[0]);
    assert!(
        lines.contains(&100) && lines.contains(&102),
        "both sites kept: {lines:?}"
    );
    // Two rows with no snippet prove nothing about being the same SITE, so the mechanical
    // collapse leaves both.
    assert_eq!(collapse_located(vec![first, second], &[]).len(), 2);
}

#[test]
fn merge_c4_same_rule_same_severity_in_different_files_stays_two_rows() {
    // C4-P1 regression: the N-site collapse arm (same rule id + same severity at any distance)
    // must be SAME-FILE only. The same architectural rule tripping a Rust handler and a TS
    // handler are two DISTINCT defects (two separate places to fix) and must stay two rows.
    // Collapsing them cross-file was an over-merge that broke the handler-no-db / strict-
    // layering e2e tests. Different paths, so no same-file signal (window/construct/overlap)
    // can fire — this isolates the same-rule-adjacent same-path guard specifically.
    let mut rs = site_finding(
        "ARCH-HANDLER-NO-DB-REPEAT-1",
        "src/routes/orders.rs",
        2,
        "high",
        "",
    );
    rs.detail = "The Rust route handler holds a database handle and queries it inline.".to_string();
    let mut ts = site_finding(
        "ARCH-HANDLER-NO-DB-REPEAT-1",
        "src/routes/orders.ts",
        2,
        "high",
        "",
    );
    ts.detail =
        "Separately, the TypeScript route reaches the data layer without delegating.".to_string();
    let out = cluster_heads(vec![rs, ts], &[]);
    assert_eq!(
        out.len(),
        2,
        "the same rule firing in two DIFFERENT files is two distinct defects, never an N-site \
             collapse: {out:?}"
    );
}

// ── C4-P1: defect-shaped merge redesign ─────────────────────────────────────────────────
// Synthetic findings only — one defect shipping as 2-3 rows (same handler/different rules,
// cross-file root cause, N same-rule sites, stale headline cross-references).

#[test]
fn p1_c4_same_sink_security_clusters_despite_different_categories() {
    // Design point 1: a boundary-validation rule and a privileged-key-bypass rule flagged on
    // the SAME handler body, with DIFFERENT categories — signal (a) alone would never fire.
    // REG-3 tightened the same-construct-security override to additionally require the pair
    // be about the SAME defect (same category class, or a shared captured object) — so this
    // fixture now also gives both findings what actually links them in reality: the SAME
    // `debug_mode` flag is what the validation rule says is under-checked and what the
    // bypass rule says is trusted to skip the privileged-key check. That shared captured
    // object is the legitimate root-cause link the override now requires; without it (see
    // `p1_c4_same_construct_pair_with_unrelated_category_and_no_object_stays_two_rows`,
    // just below) the override must NOT fire. Because they share a construct
    // (`same_construct`), one side is security-tier, AND they share that captured object,
    // they must still merge into one row with the security rule as primary.
    let content = "fn handler() {\n let x = input();\n if debug_mode {\n return bypass_check(x);\n }\n ok(x)\n}\n";
    let files = vec![("h.rs".to_string(), content.to_string())];
    let mut validation = site_finding("ARCH-BOUNDARY-VALIDATION-1", "h.rs", 2, "high", "");
    validation.category = Some("error-handling".to_string());
    validation
        .captures
        .insert("flag".to_string(), "debug_mode".to_string());
    let mut bypass = site_finding("AUTH-PRIVILEGED-KEY-BYPASS-1", "h.rs", 4, "critical", "");
    bypass.category = Some("authorization".to_string());
    bypass
        .captures
        .insert("flag".to_string(), "debug_mode".to_string());
    assert_eq!(
        finding_class(&validation),
        FindingClass::Hygiene,
        "sanity: the plain validation rule is not itself security-tier"
    );
    assert_eq!(
        finding_class(&bypass),
        FindingClass::Security,
        "sanity: the bypass rule is security-tier"
    );

    let groups = clusters(vec![validation, bypass], &files);
    assert_eq!(
        groups.len(),
        1,
        "same-handler validation + bypass rules cluster despite different categories: {groups:?}"
    );
    assert_eq!(
        head_of(&groups[0]).rule_id,
        "AUTH-PRIVILEGED-KEY-BYPASS-1",
        "the security-tier rule heads the cluster"
    );
    let lines = crate::review_tree::test_support::cluster_lines(&groups[0]);
    assert!(
        lines.contains(&2) && lines.contains(&4),
        "both evidence sites are kept: {lines:?}"
    );
}

// ── REG-3: same-construct-security override tightened to require a same-defect link ────

#[test]
fn p1_c4_same_construct_pair_with_unrelated_category_and_no_object_stays_two_rows() {
    // REG-3 (the bug this test pins): before the fix, `same_construct_security_override`
    // fired whenever one side of a same-construct pair was security-tier, with NO
    // requirement the two findings be related at all. That absorbed a genuinely unrelated,
    // merely co-located rule into the security finding's `also_matches`, where it vanished
    // from its own category's Coverage count and read as "clean" — exactly the shape that
    // would mis-score a planted defect as a miss on a blind hold-out. An injection finding
    // and an unrelated pagination-convention finding, co-located in the SAME handler, with
    // DIFFERENT categories and no shared captured object, must stay TWO rows.
    let content = "fn handler() {\n let q = raw_sql(id);\n let page = rows.skip(n).take(k);\n}\n";
    let files = vec![("h.rs".to_string(), content.to_string())];
    let mut injection = site_finding("SEC-SQL-INJECTION-SINK-1", "h.rs", 2, "critical", "");
    injection.detail = "User-controlled input flows into a raw SQL query.".to_string();
    let mut pagination = site_finding("PERF-PAGINATION-MISSING-1", "h.rs", 3, "medium", "");
    pagination.detail = "This handler returns every row with no pagination.".to_string();
    assert_eq!(
        finding_class(&injection),
        FindingClass::Security,
        "sanity: the injection rule is security-tier"
    );
    assert_eq!(
        finding_class(&pagination),
        FindingClass::Hygiene,
        "sanity: the pagination rule is not itself security-tier"
    );
    assert_ne!(
        categorize_rule_id(&injection.rule_id),
        categorize_rule_id(&pagination.rule_id),
        "sanity: the two rules are genuinely different defect classes"
    );

    let out = cluster_heads(vec![injection, pagination], &files);
    assert_eq!(
        out.len(),
        2,
        "an injection finding and an unrelated pagination finding co-located in one \
             construct, with different categories and no shared object, must stay two rows, \
             each routed to its own category: {out:?}"
    );
    let ids: std::collections::HashSet<&str> = out.iter().map(|f| f.rule_id.as_str()).collect();
    assert!(
        ids.contains("SEC-SQL-INJECTION-SINK-1") && ids.contains("PERF-PAGINATION-MISSING-1"),
        "neither finding may be absorbed into the other's also_matches: {out:?}"
    );
    assert!(
        out.iter().all(|f| f.also_matches.is_empty()),
        "neither row may carry the other as an also_matches member: {out:?}"
    );
}

#[test]
fn p1_c4_same_construct_security_override_still_clusters_on_matching_category() {
    // PRESERVE: the legitimate case the override exists for — two same-construct findings
    // that genuinely share a defect class (here, both corpus rule ids fall into the
    // `categorize_rule_id` "authorization" class via the `BYPASS`/`RBAC` tokens, even though
    // neither Finding carries an explicit `.category`) — must still merge, with merge
    // PRIMACY untouched by the REG-3 tightening: this only narrows WHICH pairs merge, never
    // WHO wins once they do. A deterministic rule id and an invented AI- id, same construct,
    // same inferred category, both security-tier (the class-match alone would be enough even
    // without the pre-existing "at least one side security" condition): still one row,
    // deterministic primary (class ties broken by origin specificity — C5-1 unchanged).
    let content =
            "fn handler() {\n check_rbac(user);\n if bypass_role_check(user) {\n grant_all(user);\n }\n}\n";
    let files = vec![("h.rs".to_string(), content.to_string())];
    let det = site_finding("AUTHZ-RBAC-CHECK-INCOMPLETE-1", "h.rs", 2, "high", "");
    let ai = site_finding("AI-PRIVILEGE-BYPASS-GRANTS-ALL", "h.rs", 4, "critical", "");
    assert_eq!(
        categorize_rule_id(&det.rule_id).as_deref(),
        Some("authorization"),
        "sanity: the deterministic rule id's inferred class is authorization"
    );
    assert_eq!(
        categorize_rule_id(&ai.rule_id).as_deref(),
        Some("authorization"),
        "sanity: the invented AI- id's inferred class is ALSO authorization, via the \
             `BYPASS` token, even with no explicit `.category` of its own"
    );

    let groups = clusters(vec![det, ai], &files);
    assert_eq!(
            groups.len(),
            1,
            "same construct + same inferred category + one side security-tier still cluster: {groups:?}"
        );
    assert_eq!(
        head_of(&groups[0]).rule_id,
        "AUTHZ-RBAC-CHECK-INCOMPLETE-1",
        "primacy (deterministic beats invented AI within the same class) is unaffected"
    );
    assert_eq!(groups[0].len(), 2, "both rows are kept");
}

#[test]
fn p1_c4_same_construct_security_override_still_clusters_on_shared_object() {
    // PRESERVE: the other legitimate link the override accepts — a shared captured object —
    // continues to work exactly as before, independent of category. A deterministic finding
    // and an adopted-AI finding (confidence set, non-`AI-` id) name the SAME captured table,
    // same construct, different categories, one security-tier: still one row, deterministic
    // primary.
    let content = "fn handler() {\n validate(payments);\n leak_to_log(payments);\n}\n";
    let files = vec![("h.rs".to_string(), content.to_string())];
    let mut det = site_finding("ARCH-VALIDATION-INCOMPLETE-1", "h.rs", 2, "medium", "");
    det.category = Some("input-validation".to_string());
    det.captures
        .insert("table".to_string(), "payments".to_string());
    let mut ai = site_finding("SEC-SENSITIVE-DATA-LOGGED-1", "h.rs", 3, "critical", "");
    ai.category = Some("resource-exposure".to_string());
    ai.confidence = Some("high".to_string());
    ai.captures
        .insert("table".to_string(), "payments".to_string());
    assert_ne!(
        det.category, ai.category,
        "sanity: the two findings have different categories"
    );

    let groups = clusters(vec![det, ai], &files);
    assert_eq!(
        groups.len(),
        1,
        "same construct + shared captured object + one side security-tier still cluster \
             despite different categories: {groups:?}"
    );
    assert_eq!(
        head_of(&groups[0]).rule_id,
        "ARCH-VALIDATION-INCOMPLETE-1",
        "primacy (deterministic beats adopted-AI within the same class) is unaffected"
    );
    assert_eq!(groups[0].len(), 2, "both rows are kept");
}

#[test]
fn p1_c4_same_construct_distinct_objects_stays_two_rows() {
    // Over-merge guard: the same-construct/security-override signal must NOT fuse two
    // findings in one handler that visibly name DIFFERENT structural objects — that is two
    // distinct defects, not one, even with one side security-tier.
    let content = "fn handler() {\n let a = orders.find();\n let b = profiles.find();\n}\n";
    let files = vec![("h.rs".to_string(), content.to_string())];
    let mut security = site_finding("AUTH-ORDERS-LEAK-1", "h.rs", 2, "high", "orders.find()");
    security.category = Some("authorization".to_string());
    let mut hygiene = site_finding(
        "ARCH-PROFILES-SMELL-1",
        "h.rs",
        3,
        "medium",
        "profiles.find()",
    );
    hygiene.category = Some("arch-conformance".to_string());

    let out = cluster_heads(vec![security, hygiene], &files);
    assert_eq!(
        out.len(),
        2,
        "two defects naming different objects in one handler must stay two rows: {out:?}"
    );
}

#[test]
fn p1_c4_cross_file_policy_and_query_cluster_via_backfilled_object() {
    // Design point 2: a permissive-policy finding and the query finding that relies on that
    // SAME policy, in a DIFFERENT file, must merge into one row at the policy, with the
    // query's site preserved in also_locations — even though NEITHER finding's own detector
    // populated `captures` (the general case). `group_semantic`'s backfill must
    // extract the shared object from each finding's own prose for the cross-file
    // shared-captured-object signal to fire.
    let mut policy = site_finding(
        "SUPABASE-RLS-PERMISSIVE-POLICY-1",
        "supabase/policies.sql",
        12,
        "high",
        "\"invoices_read_all\"",
    );
    policy.detail = "The \"invoices_read_all\" policy grants unrestricted read access.".to_string();
    let mut query = site_finding(
        "AI-QUERY-RELIES-ON-PERMISSIVE-POLICY",
        "src/api/invoices.ts",
        30,
        "medium",
        "\"invoices_read_all\"",
    );
    query.detail =
        "This query relies on the \"invoices_read_all\" policy to scope access.".to_string();
    assert!(
        policy.captures.is_empty() && query.captures.is_empty(),
        "sanity: neither detector populated captures itself"
    );

    let groups = clusters(vec![policy, query], &[]);
    assert_eq!(
        groups.len(),
        1,
        "policy + relying query share a cluster via the backfilled captured object: {groups:?}"
    );
    assert_eq!(groups[0].len(), 2, "and stay two rows");
    assert_eq!(
        head_of(&groups[0]).rule_id,
        "SUPABASE-RLS-PERMISSIVE-POLICY-1",
        "the policy finding heads the cluster"
    );
    let query_row = groups[0]
        .iter()
        .find(|f| f.path == "src/api/invoices.ts")
        .unwrap();
    assert_eq!(query_row.rule_id, "AI-QUERY-RELIES-ON-PERMISSIVE-POLICY");
}

#[test]
fn p1_c4_n_site_cluster_same_rule_same_file_same_severity() {
    // Design point 3: the GENERAL "N location(s)" pattern (already used for the
    // arch-conformance/needs-review structural group) must also apply to a genuinely curated
    // row: one rule firing at the SAME severity three times across one file, far enough apart
    // that no window/construct signal would otherwise catch it, collapses to ONE row listing
    // every site — not three near-duplicate rows.
    let a = site_finding("ARCH-NO-DIRECT-DB-1", "svc.rs", 10, "medium", "");
    let b = site_finding("ARCH-NO-DIRECT-DB-1", "svc.rs", 200, "medium", "");
    let c = site_finding("ARCH-NO-DIRECT-DB-1", "svc.rs", 400, "medium", "");
    let groups = clusters(vec![a, b, c], &[]);
    assert_eq!(
        groups.len(),
        1,
        "three same-rule/same-file/same-severity occurrences are one cluster: {groups:?}"
    );
    assert_eq!(groups[0].len(), 3, "all three are still rows");
    let lines = crate::review_tree::test_support::cluster_lines(&groups[0]);
    assert!(
        lines.contains(&10) && lines.contains(&200) && lines.contains(&400),
        "all three evidence sites are kept: {lines:?}"
    );
}

#[test]
fn p1_c4_n_site_collapse_does_not_fuse_different_severities_or_objects() {
    // The N-site collapse signal is scoped to SAME severity, and still subject to the
    // objects_conflict veto — it must not swallow a genuinely distinct occurrence (different
    // severity AND a different named object) of the same rule into the group.
    let mut a = site_finding("RLS-MISSING-1", "s.sql", 10, "medium", "public.orders");
    a.detail = "orders has no RLS policy".to_string();
    let mut b = site_finding("RLS-MISSING-1", "s.sql", 500, "critical", "public.payments");
    b.detail = "payments has no RLS policy".to_string();
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(
        out.len(),
        2,
        "different severity + disjoint named objects must stay two rows: {out:?}"
    );
}

// ── Pass-1 W7: C4 (the veto that cannot veto) + B9 (prose abbreviations as objects) ──

/// req_b9: `structural_objects` must not register English dotted abbreviations ("e.g",
/// "i.e") as structural objects. The extractor was reading English: `e.g` was the sole
/// object on 5 rows and caused 5 of 51 `objects_conflict` vetoes. The fix is the CLASS
/// (a dotted run whose every segment is too short to be an identifier is prose, not an
/// object), never a blacklist of the one observed token.
#[test]
fn req_b9_prose_abbreviations_are_not_structural_objects() {
    let mut f = site_finding("SEC-X-1", "a.rs", 1, "high", "");
    f.detail =
        "Avoid string-built SQL, e.g. format! with user input, i.e. concatenation.".to_string();
    let objs = structural_objects(&f);
    assert!(
        !objs.contains("e.g") && !objs.contains("i.e"),
        "prose abbreviations must not register as structural objects: {objs:?}"
    );
    // A genuine dotted identifier right next to the abbreviation still registers.
    f.detail = "Avoid raw SQL, e.g. against public.orders directly.".to_string();
    let objs = structural_objects(&f);
    assert!(objs.contains("public.orders"), "{objs:?}");
    assert!(!objs.contains("e.g"), "{objs:?}");
}

/// req_b9 / req_c4: two findings about DISJOINT tables whose prose happens to share an
/// abbreviation token ("e.g") must still conflict — before W7 the shared noise token
/// made the sets non-disjoint and disabled the veto entirely.
#[test]
fn req_b9_shared_abbreviation_token_does_not_disable_the_disjointness_veto() {
    let mut a = site_finding("RLS-MISSING-1", "s.sql", 10, "medium", "db.orders");
    a.detail = "db.orders has no RLS, e.g. anyone can read it.".to_string();
    let mut b = site_finding("RLS-MISSING-1", "s.sql", 14, "medium", "db.profiles");
    b.detail = "db.profiles has no RLS, e.g. anyone can read it.".to_string();
    assert!(
        objects_conflict(&a, &b),
        "disjoint tables must conflict even when the prose shares an abbreviation: \
             a={:?} b={:?}",
        structural_objects(&a),
        structural_objects(&b)
    );
}

/// req_c4: a shared CONTAINER token (a schema prefix, a table namespace, any common
/// parent) must not defeat the veto. Before W7, `objects_conflict` required FULL
/// disjointness, so any family whose sites sit inside one named container had an inert
/// discrimination guard — the over-collapse half of the merge tension.
#[test]
fn req_c4_shared_container_token_does_not_defeat_the_disjointness_veto() {
    let mut a = site_finding("RLS-MISSING-1", "s.sql", 10, "medium", "public.orders");
    a.detail = "Table public.orders in the 'public' schema has no RLS policy.".to_string();
    let mut b = site_finding("RLS-MISSING-1", "s.sql", 14, "medium", "public.profiles");
    b.detail = "Table public.profiles in the 'public' schema has no RLS policy.".to_string();
    // Sanity: both sides really do extract the shared container token, so this test
    // exercises the defeat shape rather than accidentally testing full disjointness.
    assert!(
        structural_objects(&a).contains("public") && structural_objects(&b).contains("public"),
        "fixture must name the shared container on both sides: a={:?} b={:?}",
        structural_objects(&a),
        structural_objects(&b)
    );
    assert!(
        objects_conflict(&a, &b),
        "a shared schema token is a container, not identity evidence — orders vs \
             profiles must conflict"
    );
    // End to end: the same-rule/same-severity N-site collapse signal fires for this
    // pair, and the (now working) veto must keep them two rows.
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(
        out.len(),
        2,
        "two tables inside one shared container must stay two rows: {out:?}"
    );
}

/// req_c4: structured identity — two findings whose DETECTORS each captured a concrete
/// object, with no value in common, are about different things and must conflict, with
/// no dependence on what their prose happens to mention. This is the review's
/// prescription verbatim: detector-populated captures as the identity signal, never
/// keyword/prose adjustment.
#[test]
fn req_c4_disjoint_detector_captures_veto_the_merge() {
    let mut a = site_finding("RLS-MISSING-1", "s.sql", 10, "medium", "");
    a.captures.insert("table".to_string(), "orders".to_string());
    let mut b = site_finding("RLS-MISSING-1", "s.sql", 14, "medium", "");
    b.captures
        .insert("table".to_string(), "profiles".to_string());
    // No prose objects at all — before W7 the veto could not see the captures and the
    // empty prose sets disabled it, so the same-rule-adjacent signal fused the pair.
    assert!(structural_objects(&a).is_empty() && structural_objects(&b).is_empty());
    assert!(
        objects_conflict(&a, &b),
        "disjoint detector captures are a structured different-things proof"
    );
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(
        out.len(),
        2,
        "two sites whose detectors captured different objects must stay two rows: {out:?}"
    );
}

// ── Pass-1 W7: kind-aware merge — a Defect must never fuse with an informational row ──

/// req_w7_kind: an informational row (ConventionObserved / NoConvention / NotAssessed)
/// must never be absorbed into a Defect row by the semantic merge, and vice versa — a
/// merge that absorbs a `Defect` into a `ConventionObserved` primary (or the reverse) is
/// a correctness bug with a name now that `FindingKind` exists. The two rows make
/// different KINDS of statement; collapsing them loses one of the statements entirely.
#[test]
fn req_w7_kind_informational_row_never_fuses_with_a_defect_row_in_the_semantic_merge() {
    use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
    let mut defect = site_finding("ARCH-API-VERSIONING-1", "src/api.rs", 10, "high", "");
    defect.kind = Some(FindingKind::Defect);
    let mut conv = site_finding("ARCH-API-VERSIONING-1", "src/api.rs", 12, "info", "");
    conv.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "versioned".to_string(),
        convention_label: "URI-versioned endpoints".to_string(),
    }));
    // Same rule id, same file, 2 lines apart: signal (d) fires, both deterministic
    // (same_rule_adjacent exempts the det+det guard), no objects to conflict — before
    // the kind guard this pair merged unconditionally.
    let out = cluster_heads(vec![defect, conv], &[]);
    assert_eq!(
        out.len(),
        2,
        "a defect and an informational convention statement are different KINDS of \
             statement and must both survive: {out:?}"
    );
    let tags: std::collections::BTreeSet<&str> = out
        .iter()
        .map(|f| camerata_api_types::audit::FindingKind::tag_or_unspecified(f.kind.as_ref()))
        .collect();
    assert!(tags.contains("defect") && tags.contains("convention_observed"));
}

/// req_w7_kind: the same guard at the LOCATION merge — an informational row co-located
/// with a defect row (same path, same line, both citing real code) stays its own row.
#[test]
fn req_w7_kind_informational_row_never_fuses_with_a_defect_row_at_the_same_location() {
    use camerata_api_types::audit::{FindingKind, NoConventionKind};
    let content = "line one\nlet q = build_query(user_input);\nline three\n";
    let files = vec![("src/api.rs".to_string(), content.to_string())];
    let mut defect = site_finding(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/api.rs",
        2,
        "high",
        "let q = build_query(user_input);",
    );
    defect.kind = Some(FindingKind::Defect);
    let mut nc = site_finding(
        "ARCH-API-VERSIONING-1",
        "src/api.rs",
        2,
        "info",
        "let q = build_query(user_input);",
    );
    nc.kind = Some(FindingKind::NoConvention(NoConventionKind {
        observed_options: vec![],
    }));
    let out = collapse_located(vec![defect, nc], &files);
    assert_eq!(
        out.len(),
        2,
        "co-location must not fuse an informational statement into a defect: {out:?}"
    );
}

/// req_w7_kind: `primary_rank` is kind-aware — an actionable row (Defect/Deviation, or a
/// legacy row with no kind) always outranks an informational row for primacy, even when
/// the informational row would win on every pre-existing key (deterministic origin vs an
/// AI-tier defect). Belt-and-braces behind the merge guards above: should a mixed-kind
/// group ever form again, the defect can still never be hidden under an informational
/// primary.
#[test]
fn req_w7_kind_primary_rank_never_lets_an_informational_row_outrank_a_defect() {
    use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
    // AI-tier defect: adopted corpus rule id + calibration confidence, medium severity.
    let mut defect = site_finding("ARCH-API-VERSIONING-1", "src/api.rs", 10, "medium", "");
    defect.kind = Some(FindingKind::Defect);
    defect.confidence = Some("high".to_string());
    // Deterministic informational row: no confidence (origin ranks HIGHER), info severity.
    let mut conv = site_finding("ARCH-API-VERSIONING-1", "src/api.rs", 12, "info", "");
    conv.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "versioned".to_string(),
        convention_label: "URI-versioned endpoints".to_string(),
    }));
    assert!(
        primary_rank(&defect, 2, 1) > primary_rank(&conv, 2, 0),
        "an AI-tier Defect must outrank a deterministic informational row — before W7, \
             origin decided first and the ConventionObserved row won primacy: defect={:?} \
             conv={:?}",
        primary_rank(&defect, 2, 1),
        primary_rank(&conv, 2, 0)
    );
}

// ── Firebase review: an asserted defect is never fused into, or ranked beneath, a held row ──

/// A real adapter row: the deterministic architectural checker's `ArchViolation`, converted
/// exactly as the scan converts it, so the merge tests see the production field values.
fn adapter_row(
    grade: camerata_checks::arch_checker::ViolationGrade,
    rule: &str,
    line: usize,
    severity: &'static str,
) -> Finding {
    crate::onboard::arch_violation_to_finding(
        "o/r",
        &camerata_checks::arch_checker::ArchViolation {
            grade,
            rule_id: rule.to_string(),
            file: "firestore.rules".to_string(),
            line,
            object: Some("/posts/{id}".to_string()),
            message: "a fact".to_string(),
            severity,
        },
    )
}

/// A review-graded (held) adapter row and an asserted one on one path: the pair shares the
/// captured rules path and sits two lines apart, so before the fix the semantic merge fused
/// them and the security-class held row (AUTH-ONLY) became the primary, hiding the asserted
/// defect (OPEN) beneath a needs-review row.
#[test]
fn a_held_adapter_row_and_an_asserted_defect_on_one_path_stay_separate() {
    use camerata_checks::arch_checker::ViolationGrade::{Defect, Review};
    let held = adapter_row(Review, "FIREBASE-RULES-AUTH-ONLY-1", 5, "medium");
    let asserted = adapter_row(Defect, "FIREBASE-RULES-OPEN-1", 6, "high");
    assert!(held.needs_review && !asserted.needs_review);
    let out = cluster_heads(vec![held, asserted], &[]);
    assert_eq!(out.len(), 2, "held and asserted must both survive: {out:?}");
    let asserted_row = out
        .iter()
        .find(|f| f.rule_id == "FIREBASE-RULES-OPEN-1")
        .expect("the asserted row survives as its own primary");
    assert!(!asserted_row.needs_review);
    assert_eq!(asserted_row.severity, "high");
}

/// Two held rows on one path still fuse (the shared object is real evidence of one root
/// cause), and the result stays held: fusing must not launder either into an assertion.
#[test]
fn two_held_adapter_rows_on_one_path_still_merge_and_stay_held() {
    use camerata_checks::arch_checker::ViolationGrade::Review;
    let a = adapter_row(Review, "FIREBASE-RULES-AUTH-ONLY-1", 5, "medium");
    let b = adapter_row(Review, "FIREBASE-RULES-FIELD-VALIDATION-1", 6, "medium");
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].needs_review);
}

/// The backstop: should a mixed group ever form, an unhedged row outranks a hedged one even
/// when the hedged row wins on class (security) and the unhedged one is hygiene.
#[test]
fn primary_rank_never_lets_a_hedged_row_outrank_an_unhedged_one() {
    let mut hedged = site_finding("AI-AUTH-BYPASS-1", "a.rs", 5, "high", "");
    hedged.confidence = Some("needs-review".to_string());
    hedged.needs_review = true;
    let unhedged = site_finding("ARCH-API-VERSIONING-1", "a.rs", 6, "low", "");
    assert_eq!(finding_class(&hedged), FindingClass::Security);
    assert_eq!(finding_class(&unhedged), FindingClass::Hygiene);
    assert!(
        primary_rank(&unhedged, 2, 1) > primary_rank(&hedged, 2, 0),
        "an unhedged row must outrank a hedged one: unhedged={:?} hedged={:?}",
        primary_rank(&unhedged, 2, 1),
        primary_rank(&hedged, 2, 0)
    );
}

/// An adapter Review row is produced by a deterministic checker: its `needs-review`
/// confidence is the HELD marker, not a calibration verdict, so it is not AI-tier. A
/// calibrated AI row with the same confidence still is.
#[test]
fn an_adapter_review_row_is_deterministic_origin_not_ai() {
    use camerata_checks::arch_checker::ViolationGrade::{Defect, Review};
    let held = adapter_row(Review, "FIREBASE-RULES-AUTH-ONLY-1", 5, "medium");
    assert_eq!(finding_origin(&held), Origin::Deterministic);
    let asserted = adapter_row(Defect, "FIREBASE-RULES-OPEN-1", 6, "high");
    assert_eq!(finding_origin(&asserted), Origin::Deterministic);
    let mut calibrated = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "a.rs", 1, "high", "");
    calibrated.confidence = Some("needs-review".to_string());
    calibrated.needs_review = true;
    assert_eq!(finding_origin(&calibrated), Origin::AdoptedAi);
}

/// Two deterministic rows keep the "two distinct defects" guard when one of them is a held
/// adapter row: with nothing shared (different rules, different captured objects), they
/// stay two rows rather than being treated as an AI pair that proximity can fuse.
#[test]
fn a_held_adapter_row_keeps_the_det_det_distinct_defects_guard() {
    use camerata_checks::arch_checker::ViolationGrade::Review;
    let mut a = adapter_row(Review, "FIREBASE-RULES-AUTH-ONLY-1", 5, "medium");
    let mut b = adapter_row(Review, "FIREBASE-RULES-FIELD-VALIDATION-1", 6, "medium");
    a.captures.clear();
    b.captures.clear();
    a.snippet.clear();
    b.snippet.clear();
    a.detail = "alpha".to_string();
    b.detail = "omega".to_string();
    a.category = Some("authorization".to_string());
    b.category = Some("authorization".to_string());
    // Same category and two lines apart would fuse an AI pair only with corroboration; as
    // deterministic rows they need a shared object, the same rule, or a same-construct
    // security override, and have none.
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(out.len(), 2, "{out:?}");
}

// ── Pass-1 W7: B7 (brace-free construct blindness) + B8 (seed-only clustering) ──

/// req_b7: `same_construct` was false for ALL brace-free content, so `.env`, `.sql`,
/// `.toml`, `.yaml` could never satisfy the in-construct merge signals — a blind spot
/// over exactly the file types config and migration defects live in. For content with no
/// braces at all, the blank-line-delimited stanza is the construct.
#[test]
fn req_b7_brace_free_content_falls_back_to_the_blank_line_stanza() {
    let sql = "CREATE TABLE orders (\n  id uuid PRIMARY KEY,\n  total numeric\n);\n\
                   \n\
                   CREATE POLICY orders_read ON orders\n  USING (true);\n";
    // Lines 1-4 are one statement stanza; line 5 is blank; lines 6-7 are another.
    assert!(
        same_construct(sql, 1, 3),
        "two lines inside one brace-free statement stanza share a construct"
    );
    assert!(
        !same_construct(sql, 1, 6),
        "lines in different stanzas (across the blank line) do not"
    );
    assert!(same_construct(sql, 6, 7));
    // Brace-BEARING content keeps the pre-W7 semantics exactly: no enclosing brace block
    // means top-level, which is NOT a construct — the fallback must never apply there.
    let rust = "use std::fmt;\nconst A: u8 = 1;\nfn f() {\n    body();\n}\n";
    assert!(
        !same_construct(rust, 1, 2),
        "top-level lines in a brace-bearing file are not one construct"
    );
}

/// req_b8: clustering compared each finding against the group's SEED only, so membership
/// was non-transitive and order-dependent — 13 of 32 same-rule/same-file/same-severity
/// pairs on the measured corpus were blocked by NOTHING in the predicate, only by not
/// happening to be the seed's own pair. A finding that pair-merges with ANY member joins
/// the group.
#[test]
fn req_b8_a_finding_that_pairs_with_any_member_joins_the_cluster_not_only_the_seed() {
    // A(10, high) seeds; B(14, high) joins via A (in-window, same rule). C(18, medium)
    // pair-merges with B (in-window, same rule) but NOT with A (8 lines apart, different
    // severity — no signal fires). Seed-only clustering split C into its own row.
    let a = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 10, "high", "");
    let b = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 14, "high", "");
    let c = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 18, "medium", "");
    // Sanity: the pairwise facts this fixture is built on.
    assert!(same_cause_hint(&a, &b, None));
    assert!(same_cause_hint(&b, &c, None));
    assert!(
        !same_cause_hint(&a, &c, None),
        "A and C alone must NOT pair-merge, or this test is not exercising B8"
    );
    let groups = clusters(vec![a, b, c], &[]);
    assert_eq!(
        groups.len(),
        1,
        "C pairs with member B and joins the cluster: {groups:?}"
    );
    assert_eq!(groups[0].len(), 3, "all three are rows");
}

/// req_c4 (non-regression): a genuinely SHARED full object is still identity evidence —
/// the container discount must never split two findings that really do name the same
/// table, and a shared detector capture still merges as before.
#[test]
fn req_c4_genuinely_shared_object_still_merges() {
    let mut a = site_finding("RLS-MISSING-1", "s.sql", 10, "medium", "public.orders");
    a.detail = "public.orders has no RLS.".to_string();
    let mut b = site_finding("RLS-MISSING-1", "s.sql", 14, "medium", "public.orders");
    b.detail = "public.orders is writable with the anon key.".to_string();
    assert!(
        !objects_conflict(&a, &b),
        "the identical table on both sides is shared identity, never a conflict"
    );
    let out = cluster_heads(vec![a, b], &[]);
    assert_eq!(
        out.len(),
        1,
        "same rule, same table, 4 lines apart: one row: {out:?}"
    );
}

// ── P1: finding_class corpus-verified false-positive/negative regressions ──────────────
// These pin the EXACT bugs found by cross-checking `SECURITY_RULE_TOKENS` against every
// real rule id in `crates/rules/principles/**` (not a synthetic id) — a naive substring
// scan misclassified real corpus rules in both directions. Real ids used deliberately: the
// point is that THESE SPECIFIC ids, which exist in the corpus today, classify correctly.

#[test]
fn p1_finding_class_rce_substring_inside_resource_is_not_security() {
    // "RCE" (remote code execution) must not match merely because "RESOURCE" or
    // "INTERCEPTORS" happens to contain the letters r-c-e as a substring.
    let f = site_finding("ARCH-RESOURCE-LIFECYCLE-1", "a.rs", 1, "medium", "");
    assert_eq!(finding_class(&f), FindingClass::Hygiene);
    let f2 = site_finding("JAVA-RESOURCE-MANAGEMENT-1", "a.java", 1, "medium", "");
    assert_eq!(finding_class(&f2), FindingClass::Hygiene);
    let f3 = site_finding(
        "JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1",
        "a.ts",
        1,
        "medium",
        "",
    );
    assert_eq!(finding_class(&f3), FindingClass::Hygiene);
}

#[test]
fn p1_finding_class_dependency_injection_is_not_injection_vulnerability() {
    // "Constructor injection" (a DI pattern) must not classify as Security just because it
    // contains the word "injection" — real SQL/command injection rules are caught via "SQL"
    // or an explicit injection-vulnerability token, not bare "INJECT".
    let f = site_finding(
        "JAVA-SPRING-CONSTRUCTOR-INJECTION-1",
        "a.java",
        1,
        "medium",
        "",
    );
    assert_eq!(finding_class(&f), FindingClass::Hygiene);
    let f2 = site_finding(
        "CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1",
        "a.cs",
        1,
        "medium",
        "",
    );
    assert_eq!(finding_class(&f2), FindingClass::Hygiene);
}

#[test]
fn p1_finding_class_database_session_di_is_not_auth_session() {
    // "FastAPI DI session" is a database-session lifecycle rule, not an authentication
    // session — bare "SESSION" must not promote it to Security.
    let f = site_finding("PYTHON-FASTAPI-DI-SESSION-1", "a.py", 1, "medium", "");
    assert_eq!(finding_class(&f), FindingClass::Hygiene);
}

#[test]
fn p1_finding_class_real_security_rules_still_classify_security() {
    // The fixes above must not have thrown out real coverage: every one of these IS a
    // security rule in the corpus and must still classify as Security.
    for rule_id in [
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-RAW-SQL-CONCAT-1",
        "SUPABASE-RLS-ENABLED-1",
        "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1",
        "SUPABASE-EXPOSURE-SCHEMAS-1",
        "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
        "ARCH-NO-SECRETS-IN-URL-1",
        "ARCH-FETCH-THEN-AUTHORIZE-1",
        "ARCH-SERVER-AUTHZ-1",
        "CSHARP-ASPNETCORE-CORS-EXPLICIT-1",
        "GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1",
        "JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1",
        "JAVA-SPRING-METHOD-SECURITY-1",
        "CICD-CODEQL-SECURITY-SCAN-1",
    ] {
        let f = site_finding(rule_id, "a.rs", 1, "medium", "");
        assert_eq!(
            finding_class(&f),
            FindingClass::Security,
            "{rule_id} must classify as Security"
        );
    }
}

#[test]
fn p1_finding_class_generic_hygiene_rules_stay_hygiene() {
    // A spot check of genuinely structural/style/testing/process rules across several
    // stacks — none of these should ever classify as Security.
    for rule_id in [
        "ARCH-MIDDLEWARE-FIRST-1",
        "ARCH-CURSOR-PAGINATION-1",
        "ARCH-MONOLITH-FIRST-1",
        "JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1",
        "RUST-TESTING-1",
        "TESTING-PYRAMID-1",
        "GO-TESTING-TABLE-DRIVEN-T-RUN-1",
        "SQL-DB-NPLUSONE-1",
        "RUST-SQLX-CONNECTION-POOL-SIZED-1",
    ] {
        let f = site_finding(rule_id, "a.rs", 1, "medium", "");
        assert_eq!(
            finding_class(&f),
            FindingClass::Hygiene,
            "{rule_id} must classify as Hygiene"
        );
    }
}

/// The floor's private-key and vendor-token rules get NO category from the rule-id map, and
/// the Supabase service-role rule keeps `authorization`. A `secret-exposure` category on
/// them would be a merge key, and two credentials a few lines apart (a private key under a
/// Stripe key) would fuse into one row. The cockpit's decision memory keeps their repos
/// apart by rule id (`FindingView::is_secret_exposure`), so it needs no category here.
#[test]
fn floor_credential_rules_carry_no_secret_exposure_merge_category() {
    for id in ["SEC-NO-PRIVATE-KEY-1", "SEC-NO-VENDOR-TOKEN-1"] {
        assert_eq!(categorize_rule_id(id), None, "{id}");
    }
    assert_eq!(
        categorize_rule_id("SUPABASE-KEY-SERVICE-ROLE-CLIENT-1").as_deref(),
        Some("authorization")
    );
    // The hardcoded-secret rule is categorized, as it always was.
    assert_eq!(
        categorize_rule_id("SEC-NO-HARDCODED-SECRETS-1").as_deref(),
        Some("secret-exposure")
    );
}

// ── D5 pt.2: categorize_rule_id word-boundary matching ────────────────────────────────
//
// Mirrors the p1_finding_class_* tests above: `categorize_rule_id` had the SAME raw-
// substring bug `finding_class`'s `SECURITY_RULE_TOKENS` was fixed for (commit f80ad83),
// never propagated to this function. Cross-checked against every real rule id in
// `crates/rules/principles/**`.

/// THE bug this fix exists for: bare "ARCH" must not match merely because "SEARCH" (from
/// "SEARCH-PATH") happens to contain the letters a-r-c-h as a substring.
/// `SUPABASE-FUNC-SEARCH-PATH-1` (D1 — SECURITY DEFINER without a pinned search_path) must
/// not be miscategorized `arch-conformance`.
#[test]
fn categorize_rule_id_arch_substring_inside_search_path_is_not_arch_conformance() {
    // "SEARCH-PATH" spells S-E-ARCH: the letters "ARCH" sit inside "SEARCH" (its "SE-ARCH"
    // shape) as a pure substring artifact, not a standalone hyphen-delimited word — a raw
    // substring scan for "ARCH" matches it anyway. Word-boundary matching must not.
    assert_ne!(
        categorize_rule_id("SUPABASE-FUNC-SEARCH-PATH-1").as_deref(),
        Some("arch-conformance"),
        "ARCH must not match inside SEARCH-PATH via substring"
    );
    // Contrast: a rule id where ARCH genuinely IS its own standalone word must still match.
    assert_eq!(
        categorize_rule_id("ARCH-STRICT-LAYERING-1").as_deref(),
        Some("arch-conformance"),
        "ARCH as a real standalone word must still categorize as arch-conformance"
    );
}

/// "RCE" (remote code execution) must not match merely because "RESOURCE" or
/// "INTERCEPTORS" contains the letters r-c-e as a substring — none of these are injection
/// concerns, the same corpus false positives `finding_class` was fixed for.
#[test]
fn categorize_rule_id_rce_substring_inside_resource_or_interceptors_is_not_injection() {
    for rule_id in [
        "ARCH-RESOURCE-LIFECYCLE-1",
        "JAVA-RESOURCE-MANAGEMENT-1",
        "JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1",
        "GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1",
    ] {
        assert_ne!(
            categorize_rule_id(rule_id).as_deref(),
            Some("injection"),
            "{rule_id} must not categorize as injection via bare RCE"
        );
    }
}

/// Bare "INJECT" must not match a dependency-injection PATTERN rule — "constructor
/// injection" is a DI pattern, not an injection vulnerability. The real injection-
/// vulnerability shapes the corpus uses are explicit compounds (RAW-SQL, PARAMETERIZED,
/// SQL-INJECTION, ...), not the bare word.
#[test]
fn categorize_rule_id_dependency_injection_pattern_is_not_injection_vulnerability() {
    for rule_id in [
        "JAVA-SPRING-CONSTRUCTOR-INJECTION-1",
        "CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1",
        "JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1",
    ] {
        assert_ne!(
            categorize_rule_id(rule_id).as_deref(),
            Some("injection"),
            "{rule_id} is a DI pattern rule, not an injection vulnerability"
        );
    }
}

/// Bare "SESSION" must not promote a DATABASE-session lifecycle rule to `authentication` —
/// `PYTHON-FASTAPI-DI-SESSION-1` is about a FastAPI DB-session dependency, not an auth
/// session. The real auth-session case is caught via "GETSESSION" (a distinct, unambiguous
/// compound word), which must keep working.
#[test]
fn categorize_rule_id_database_session_is_not_authentication() {
    assert_ne!(
        categorize_rule_id("PYTHON-FASTAPI-DI-SESSION-1").as_deref(),
        Some("authentication"),
        "a DB-session lifecycle rule must not categorize as authentication"
    );
    assert_eq!(
        categorize_rule_id("SUPABASE-AUTH-GETSESSION-SERVER-1").as_deref(),
        Some("authentication"),
        "a real auth-session rule (GETSESSION) must still categorize as authentication"
    );
}

/// Positive cases: real, unambiguous corpus ids for EVERY category must still categorize
/// correctly after the word-boundary fix — the fix must not have thrown out real coverage.
#[test]
fn categorize_rule_id_positive_cases_across_every_category_still_match() {
    for (rule_id, expected) in [
        ("SUPABASE-RLS-ENABLED-1", "rls-policy"),
        ("SEC-NO-DISABLED-TLS-1", "transport-security"),
        ("SEC-NO-HARDCODED-SECRETS-1", "secret-exposure"),
        ("SEC-NO-RAW-SQL-CONCAT-1", "injection"),
        ("SEC-NO-QUERY-GRAMMAR-INJECTION-1", "injection"),
        ("SEC-NO-UNSAFE-DESERIALIZATION-1", "injection"),
        ("PYTHON-FLASK-PARAMETERIZED-SQL-1", "injection"),
        ("SUPABASE-AUTH-GETSESSION-SERVER-1", "authentication"),
        ("SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1", "authorization"),
        ("JAVASCRIPT-EXPRESS-VALIDATE-INPUT-1", "input-validation"),
        ("RUST-NO-UNWRAP-1", "error-handling"),
        (
            "JAVASCRIPT-EXPRESS-CENTRAL-ERROR-HANDLER-1",
            "error-handling",
        ),
        ("PYTHON-FLASK-ERROR-HANDLERS-1", "error-handling"),
        ("RUST-TESTING-1", "testing-style"),
        ("SQL-DB-NPLUSONE-1", "performance"),
        ("SUPABASE-STORAGE-PUBLIC-BUCKET-1", "resource-exposure"),
        ("ARCH-STRICT-LAYERING-1", "arch-conformance"),
        ("ARCH-MONOLITH-FIRST-1", "arch-conformance"),
    ] {
        assert_eq!(
            categorize_rule_id(rule_id).as_deref(),
            Some(expected),
            "{rule_id} must categorize as {expected}"
        );
    }
}

/// `RUST-SQLX-CONNECTION-POOL-SIZED-1` and its SQLX-family siblings must NOT categorize as
/// `injection` — that was a pre-existing bug independent of the ARCH/SEARCH-PATH shape (bare
/// "SQL" matching the "SQLX" word via prefix), the same class of false positive the corpus
/// audit was asked to surface. `SQL-DB-NPLUSONE-1` is a performance rule, not injection,
/// for the same reason.
#[test]
fn categorize_rule_id_sqlx_family_and_nplusone_are_not_injection() {
    for rule_id in [
        "RUST-SQLX-CONNECTION-POOL-SIZED-1",
        "RUST-SQLX-COMPILE-CHECKED-QUERIES-1",
        "RUST-SQLX-MIGRATIONS-CHECKED-IN-1",
        "RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1",
        "SQL-DB-NPLUSONE-1",
    ] {
        assert_ne!(
            categorize_rule_id(rule_id).as_deref(),
            Some("injection"),
            "{rule_id} must not categorize as injection via bare SQL matching SQLX/SQL-DB"
        );
    }
}

/// Corpus-verification test pinning the exact real ids named in the task as having
/// motivated this fix — a regression here means the fix was reverted or narrowed.
#[test]
fn categorize_rule_id_corpus_verification_pins_the_motivating_ids() {
    assert_ne!(
        categorize_rule_id("SUPABASE-FUNC-SEARCH-PATH-1").as_deref(),
        Some("arch-conformance")
    );
    assert_ne!(
        categorize_rule_id("PYTHON-FASTAPI-DI-SESSION-1").as_deref(),
        Some("authentication")
    );
    assert_ne!(
        categorize_rule_id("JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1").as_deref(),
        Some("injection")
    );
}
