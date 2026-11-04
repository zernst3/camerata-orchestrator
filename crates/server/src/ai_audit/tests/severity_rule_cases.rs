//! Tests for severity rule cases.

use super::*;

// ── D5: severity calibration rules of thumb ───────────────────────────────

fn finding_with_category(rule: &str, sev: &str, detail: &str, category: &str) -> Finding {
    Finding {
        detail: detail.to_string(),
        category: Some(category.to_string()),
        ..finding(rule, sev)
    }
}

/// An unauthenticated-access finding must floor at Critical regardless of what severity the
/// scanner/calibration model assigned it, and the rationale must be recorded.
#[test]
fn severity_floor_unauthenticated_access_is_critical() {
    let f = finding_with_detail(
        "AI-EXPORT-ENDPOINT",
        "medium",
        "The /api/export endpoint is reachable without authentication and returns every \
             user's records.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "unauthenticated access must floor to critical"
    );
    let rationale = out
        .calibration_rationale
        .expect("a calibration rationale must be recorded");
    assert!(
        rationale.to_lowercase().contains("unauthenticated")
            || rationale.to_lowercase().contains("critical"),
        "rationale must explain the unauthenticated/critical floor: {rationale}"
    );
}

/// A path to full-account compromise (not merely unauthenticated) must ALSO floor at
/// Critical — the plan's second qualifying clause for the Critical tier.
#[test]
fn severity_floor_full_account_compromise_is_critical() {
    let f = finding_with_detail(
        "AI-SESSION-FIXATION",
        "high",
        "An attacker can achieve full account takeover by replaying a pre-auth session id.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "full-account compromise must floor to critical"
    );
    assert!(out.calibration_rationale.is_some());
}

/// An AUTHENTICATED cross-tenant READ of ordinary (non-sensitive) data floors at High, NOT
/// Critical — the plan's explicit "High by default" rule, guarding against inflation.
#[test]
fn severity_floor_authenticated_cross_tenant_read_is_high_not_critical() {
    let f = finding_with_detail(
        "AI-ORG-SETTINGS-LEAK",
        "medium",
        "An authenticated user can view another organization's settings by changing the org \
             id in the URL.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "an authenticated cross-tenant read of non-sensitive data floors at High, not Critical"
    );
    let rationale = out.calibration_rationale.expect("rationale recorded");
    assert!(
        rationale.to_lowercase().contains("high"),
        "the rationale for a High floor must say so: {rationale}"
    );
    assert!(
        !rationale.to_lowercase().contains("escalates to critical"),
        "a non-escalated High floor must not use the escalation-branch wording: {rationale}"
    );
}

/// A cross-tenant read that exposes PAYMENT data escalates all the way to Critical — the
/// plan's explicit escalation class.
#[test]
fn severity_floor_cross_tenant_read_of_payment_data_escalates_to_critical() {
    let f = finding_with_detail(
        "AI-INVOICE-LEAK",
        "medium",
        "An authenticated user can view another organization's stored payment method and \
             card number.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "cross-tenant read of payment data must escalate to Critical"
    );
}

/// A cross-tenant read that exposes CREDENTIALS/secrets also escalates to Critical.
#[test]
fn severity_floor_cross_tenant_read_of_credentials_escalates_to_critical() {
    let f = finding_with_detail(
        "AI-API-KEY-LEAK",
        "low",
        "An authenticated user can fetch another tenant's stored api key from the settings \
             endpoint.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "cross-tenant read of a credential/secret must escalate to Critical"
    );
}

/// A cross-tenant read that exposes sensitive PII (SSN) also escalates to Critical.
#[test]
fn severity_floor_cross_tenant_read_of_sensitive_pii_escalates_to_critical() {
    let f = finding_with_detail(
        "AI-PROFILE-LEAK",
        "medium",
        "An authenticated user can view another user's social security number through the \
             profile API.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "cross-tenant read of sensitive PII must escalate to Critical"
    );
}

/// The floor only ever RAISES severity — a finding already calibrated ABOVE the computed
/// floor must not be lowered back down to it.
#[test]
fn severity_floor_never_lowers_an_already_higher_severity() {
    let f = finding_with_detail(
        "AI-ORG-SETTINGS-LEAK-2",
        "critical", // already critical from an earlier, more specific verdict
        "An authenticated user can view another organization's settings.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "the floor (high) must never lower a severity already above it"
    );
}

/// A finding whose text matches none of the D5 signals is left completely untouched —
/// no rationale is fabricated for a finding the rule doesn't apply to.
#[test]
fn severity_floor_does_not_touch_unrelated_findings() {
    let f = finding_with_detail(
        "ARCH-STRICT-LAYERING-1",
        "medium",
        "The controller calls the repository directly, skipping the service layer.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(out.severity, "medium", "severity must be untouched");
    assert_eq!(
        out.calibration_rationale, None,
        "no rationale is recorded when no rule of thumb applies"
    );
}

/// THE FLOOR/DETERMINISTIC-ORIGIN CASE (ties to D3): an RLS-policy finding must NOT be
/// under-rated to below High merely because the write-up hedges that RLS probably contains
/// it — defense-in-depth speculation must never excuse a boundary that has already failed.
/// This is exactly the class of finding the deterministic floor/native RLS checker produces
/// (never touched by LLM calibration at all), so the function is exercised directly over a
/// floor-shaped finding — low severity, `confidence: None`, exactly as the floor emits it.
#[test]
fn severity_floor_rls_finding_not_downgraded_on_rls_probably_contains_it_hedge() {
    let f = finding_with_category(
        "SUPABASE-RLS-NO-POLICY-1",
        "low", // e.g. a merge/consequence pass had already softened this
        "The profiles table has RLS enabled but no policy defined; row-level security \
             probably contains unauthorized access, so real-world risk is limited.",
        "rls-policy",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "an RLS-hedged access-control finding must floor at High, never stay low"
    );
    let rationale = out.calibration_rationale.expect("rationale recorded");
    assert!(
        rationale.to_lowercase().contains("rls"),
        "rationale must name the RLS-hedge guard: {rationale}"
    );
}

/// The RLS-hedge guard also covers an injection-class finding (D3's query-grammar-injection
/// family) that hedges the same way, not just rls-policy — both are access-control-adjacent.
#[test]
fn severity_floor_injection_finding_not_downgraded_on_rls_hedge() {
    let f = finding_with_category(
        "SEC-NO-QUERY-GRAMMAR-INJECTION-1",
        "medium",
        "User input is concatenated into a PostgREST .or() filter string; this is likely \
             covered by RLS on the underlying table.",
        "injection",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "an RLS-hedged injection finding must floor at High"
    );
}

/// A finding that CONFIRMS (not speculates) a mitigating RLS policy — no hedge language at
/// all — is NOT touched by the RLS-hedge guard; only speculative "probably"/"likely"/
/// "should" language trips it.
#[test]
fn severity_floor_rls_hedge_guard_does_not_fire_without_speculative_language() {
    let f = finding_with_category(
        "SUPABASE-RLS-ENABLED-1",
        "low",
        "The profiles table has RLS enabled with an owner-scoped SELECT policy confirmed in \
             the migration.",
        "rls-policy",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "low",
        "a confirmed (non-speculative) mitigation is not overridden by the hedge guard"
    );
    assert_eq!(out.calibration_rationale, None);
}

/// The RLS-hedge guard is scoped to access-control/injection findings — an unrelated
/// (e.g. testing-style) finding that happens to mention "RLS" and "probably" in passing is
/// not swept up into a High floor.
#[test]
fn severity_floor_rls_hedge_guard_does_not_fire_outside_access_control_class() {
    let f = finding_with_category(
        "TESTING-PYRAMID-1",
        "low",
        "This test module probably doesn't need the RLS fixture it imports.",
        "testing-style",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "low",
        "a non-access-control finding is unaffected by the hedge guard"
    );
}

// ── W8-Q4: the D5 floor's phrase tables must match WORDS, not bare substrings ──────────
//
// Every phrase table in the D5 floor was tested with `text.contains(phrase)`. The floor
// scans the MODEL'S PROSE, so a one-syllable entry like `"read"` or `"list"` matched
// whatever English word happened to contain it: al`read`y, re`view`, allow`list`,
// `each` transfer, `password`less, `urls` (which contains "rls"). Each false positive
// below was observed RED against the pre-fix `contains` implementation; the fix is
// `contains_phrase_as_word` (word boundaries + a closed inflection set), the same fix
// shape as W7/B9's `dotted_run_is_identifier`. The phrase tables themselves are unchanged.

/// RED BEFORE FIX: "already" contains "read", so a finding that merely mentions another
/// user's record alongside the word "already" floored to High — a fabricated cross-tenant
/// read escalation out of two ordinary English words.
#[test]
fn req_w8_q4_read_verb_does_not_match_inside_already() {
    let f = finding_with_detail(
        "ARCH-STRUCTURED-ERRORS-1",
        "medium",
        "A request for another user's record is already rejected by the guard clause, so \
             the handler never returns a row it should not.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "medium",
        "\"already\" is not the read verb \"read\": no cross-tenant-read floor may fire \
             (rationale was: {:?})",
        out.calibration_rationale
    );
    assert_eq!(
        out.calibration_rationale, None,
        "no rule of thumb applies, so no rationale may be fabricated"
    );
}

/// RED BEFORE FIX: "review" contains "view".
#[test]
fn req_w8_q4_read_verb_does_not_match_inside_review() {
    let f = finding_with_detail(
        "ARCH-NAMING-1",
        "low",
        "A code review of the handler confirmed another tenant's id is validated before \
             the query is built.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "low",
        "\"review\" is not the read verb \"view\" (rationale was: {:?})",
        out.calibration_rationale
    );
    assert_eq!(out.calibration_rationale, None);
}

/// RED BEFORE FIX: "allowlist" contains "list".
#[test]
fn req_w8_q4_read_verb_does_not_match_inside_allowlist() {
    let f = finding_with_detail(
        "ARCH-CONFIG-1",
        "low",
        "The allowlist of another organization's permitted domains is hard-coded in the \
             module rather than configured.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "low",
        "\"allowlist\" is not the read verb \"list\" (rationale was: {:?})",
        out.calibration_rationale
    );
    assert_eq!(out.calibration_rationale, None);
}

/// RED BEFORE FIX, and the headline measured case: prose containing "another user's" plus
/// "already" floored to High, and adding the bare word "credential" took the SAME prose to
/// Critical. Both escalations were manufactured by substring matching. The badged-CRITICAL
/// `ARCH-STRUCTURED-ERRORS-1` case in the audit is this bug, not a separate quirk.
#[test]
fn req_w8_q4_substring_read_verb_plus_credential_does_not_manufacture_critical() {
    let f = finding_with_detail(
        "ARCH-STRUCTURED-ERRORS-1",
        "medium",
        "The error branch for another user's record is already structured, but the \
             credential refresh path still returns a bare string.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "medium",
        "a real word (\"credential\") must not escalate a floor that a SUBSTRING \
             (\"already\" -> \"read\") fabricated in the first place (rationale was: {:?})",
        out.calibration_rationale
    );
}

/// RED BEFORE FIX: "each transfer" contains the payment phrase "ach transfer", so a GENUINE
/// High cross-tenant read was inflated to Critical by the word "each". The read verb here
/// is real, so the High floor must survive — only the data-class escalation is wrong.
#[test]
fn req_w8_q4_payment_data_class_does_not_match_ach_transfer_inside_each_transfer() {
    let f = finding_with_detail(
        "AI-LEDGER-LEAK",
        "medium",
        "An authenticated user can view another organization's ledger page, where each \
             transfer row is rendered.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "the genuine cross-tenant read still floors at High, but \"each transfer\" is not \
             the payment phrase \"ach transfer\" and must not escalate to Critical"
    );
}

/// RED BEFORE FIX: "passwordless" contains "password". A passwordless-login CONFIG is not
/// an exposed credential, so the Critical data-class escalation was unearned.
#[test]
fn req_w8_q4_credential_data_class_does_not_match_password_inside_passwordless() {
    let f = finding_with_detail(
        "AI-AUTH-CONFIG-LEAK",
        "medium",
        "An authenticated user can view another tenant's passwordless sign-in \
             configuration from the settings page.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "a passwordless-login CONFIG is not an exposed credential: the cross-tenant read \
             floors at High, not Critical"
    );
}

/// RED BEFORE FIX: "urls" contains "rls". Any access-control finding mentioning signed URLs
/// alongside an ordinary hedge word ("probably") was floored to High by the RLS-hedge guard
/// without row-level security being anywhere in the text.
#[test]
fn req_w8_q4_rls_hedge_guard_does_not_match_rls_inside_urls() {
    let f = finding_with_category(
        "AI-SIGNED-URL-TTL",
        "low",
        "The handler mints signed urls for attachments with a seven-day expiry, which is \
             probably longer than this feature needs.",
        "authorization",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "low",
        "\"urls\" is not \"rls\": the RLS-hedge guard must not fire on text that never \
             mentions row-level security (rationale was: {:?})",
        out.calibration_rationale
    );
    assert_eq!(out.calibration_rationale, None);
}

/// THE OTHER HALF, and the one that matters most: the fix must not silence the mechanism.
/// Real prose about reading another tenant's data still escalates — including the INFLECTED
/// forms real write-ups use ("accessed", "listed", "fetches", "downloaded", "viewing"),
/// which a naive whole-word-only matcher would have thrown away.
#[test]
fn req_w8_q4_genuine_cross_tenant_reads_still_floor_at_high() {
    let genuine = [
        "An authenticated user can read another tenant's invoices.",
        "Any member can view another organization's settings by changing the org id.",
        "Records belonging to another user are accessed without an owner check.",
        "The endpoint lists other tenants' projects alongside the caller's own.",
        "The loader fetches another organization's rows before filtering.",
        "Attachments belonging to another user can be downloaded by id.",
        "A member sees other users' draft documents in the sidebar.",
        "The report job is viewing another tenant's aggregate table directly.",
        "Data from another account is retrieved by the shared helper.",
    ];
    for detail in genuine {
        let out =
            apply_severity_calibration_rule(finding_with_detail("AI-TENANT-LEAK", "low", detail));
        assert_eq!(
            out.severity, "high",
            "a genuine authenticated cross-tenant read must still floor at High: {detail}"
        );
        assert!(
            out.calibration_rationale.is_some(),
            "the floor rationale must still be recorded: {detail}"
        );
    }
}

/// The genuine data-class escalations must also survive the boundary fix, in the INFLECTED
/// plural forms real write-ups overwhelmingly use ("credentials", "passwords", "medical
/// records") — the escalation list is matched by word, not by exact string.
#[test]
fn req_w8_q4_genuine_data_class_escalations_still_reach_critical() {
    let genuine = [
        "An authenticated user can read another tenant's stored passwords.",
        "Any member can view another organization's api keys and credentials.",
        "The endpoint lists other tenants' payments with full card numbers.",
        "A member can fetch another user's medical records from the export route.",
        "Rows containing another user's social security number are retrieved.",
    ];
    for detail in genuine {
        let out =
            apply_severity_calibration_rule(finding_with_detail("AI-TENANT-LEAK", "low", detail));
        assert_eq!(
            out.severity, "critical",
            "a genuine escalating data class must still reach Critical: {detail}"
        );
    }
}

/// Pins the one deliberately CONSERVATIVE consequence of the boundary fix, so it is a
/// recorded decision rather than an accident: "seen" is a genuine past participle of the
/// read verb "see", but W8-Q4 names `see`-inside-`seen` as one of the substring hits to
/// close, and `"n"` is not an inflection the closed suffix set admits (admitting it would
/// be safe in isolation — no other phrase gains a match — so this is reversible with a
/// one-word change to `PHRASE_INFLECTION_SUFFIXES` if a benchmark run argues for it).
/// Documented here, with the reversal named, rather than left silent.
#[test]
fn req_w8_q4_seen_is_conservatively_not_treated_as_the_read_verb_see() {
    let f = finding_with_detail(
        "AI-TENANT-LEAK",
        "low",
        "Another tenant's rows can be seen on the shared dashboard.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "low",
        "\"seen\" is deliberately outside the admitted inflection set (see this test's \
             doc comment for the one-word reversal)"
    );
}

/// The boundary helper itself, exercised directly over the exact pairs that motivated it —
/// so a future edit to `contains_phrase_as_word` fails here, at the unit, rather than
/// three layers up in a severity assertion.
#[test]
fn req_w8_q4_contains_phrase_as_word_unit_contract() {
    // Substring hits that must NOT match.
    for (text, phrase) in [
        ("the record was already returned", "read"),
        ("a code review of the handler", "view"),
        ("the allowlist of domains", "list"),
        ("it seems fine", "see"),
        ("the seed data script", "see"),
        ("the endpoint is ready", "read"),
        ("an event listener fires", "list"),
        ("the handler will listen on the socket", "list"),
        ("passwordless sign-in", "password"),
        ("each transfer row", "ach transfer"),
        ("signed urls expire", "rls"),
        ("the preview pane", "view"),
    ] {
        assert!(
            !contains_phrase_as_word(text, phrase),
            "{phrase:?} must not match as a bare substring of {text:?}"
        );
    }
    // Genuine word matches, bare and inflected, that MUST still match.
    for (text, phrase) in [
        ("can read the row", "read"),
        ("reads every row", "read"),
        ("reading the row", "read"),
        ("a readable export", "read"),
        ("rows are listed", "list"),
        ("listing every row", "list"),
        ("the data is accessed", "access"),
        ("publicly accessible", "access"),
        ("the loader fetches rows", "fetch"),
        ("rows are retrieved", "retrieve"),
        ("stored credentials", "credential"),
        ("medical records", "medical record"),
        ("rls is enabled", "rls"),
        ("(read)", "read"),
        ("read", "read"),
    ] {
        assert!(
            contains_phrase_as_word(text, phrase),
            "{phrase:?} must still match as a word in {text:?}"
        );
    }
}

/// `apply_severity_calibration_rules` (plural) must apply the floor across a whole finding
/// set — the shape `verify_findings` actually calls it with, after `apply_verdicts`.
#[test]
fn severity_floor_applies_across_a_finding_set_after_apply_verdicts() {
    let findings = vec![
        finding_with_detail(
            "AI-UNAUTH-EXPORT",
            "high",
            "The export endpoint requires no authentication and returns all records.",
        ),
        finding_with_detail("AI-UNRELATED", "medium", "A minor style inconsistency."),
    ];
    // Calibration itself did not escalate the unauthenticated finding (a plausible
    // under-rated model verdict) — the deterministic floor must catch it anyway.
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"high","real":"yes","reason":"no auth check"},
            {"index":1,"severity":"medium","real":"yes","reason":"style"}
        ]}"#;
    let calibrated = apply_verdicts(raw, findings);
    let floored = apply_severity_calibration_rules(calibrated);
    let unauth = floored
        .iter()
        .find(|f| f.rule_id == "AI-UNAUTH-EXPORT")
        .unwrap();
    assert_eq!(
        unauth.severity, "critical",
        "the deterministic floor overrides an under-rated model verdict"
    );
    let unrelated = floored
        .iter()
        .find(|f| f.rule_id == "AI-UNRELATED")
        .unwrap();
    assert_eq!(
        unrelated.severity, "medium",
        "an unrelated finding passes through the floor untouched"
    );
    assert_eq!(unrelated.calibration_rationale, None);
}

// ── cycle-5: the D5 floor's own critical escalation must also respect the precondition
// cap ─────────────────────────────────────────────────────────────────────────────────────
//
// Calibration review (second attempt): cycle-4's R2 blocked an upward raise in
// `apply_verdicts` when a precondition phrase was present, but TWO High-severity planted
// defects still came out Critical — a mutation gated by an unverified session that requires
// forging a cookie; a public bucket of sensitive documents that requires knowing a path.
// Root cause: `severity_calibration_floor` (D5) re-derives Critical independently from the
// finding's own unauthenticated/data-class vocabulary, runs AFTER `apply_verdicts`, and
// never consulted preconditions at all — so it silently re-raised exactly what the
// verdict-path guard had just blocked. These tests pin the fix on the floor path directly
// (OVER-RATING guard), and the paired "do-not-break" tests pin the floor's genuine Critical
// classes stay Critical when no precondition is recorded (UNDER-RATING guard).

/// OVER-RATING GUARD: a finding that trips the UNAUTHENTICATED/full-compromise floor branch
/// (normally Critical) must cap at High when its own text ALSO records an exploit
/// precondition (forging a session cookie). Base severity starts below High to prove the cap
/// is an upper bound, not merely "leave it where it was" — it still raises from Medium to
/// High, just never all the way to Critical.
#[test]
fn severity_floor_caps_unauthenticated_critical_escalation_at_high_with_precondition() {
    let f = finding_with_detail(
        "AI-MUTATION-FORGED-COOKIE",
        "medium",
        "The admin-settings mutation runs with no authentication required on the session \
             check; exploitation requires forging a session cookie to pass the check.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "an exploit precondition caps the floor's unauthenticated-branch critical \
             escalation at High, though it still raises Medium up to High"
    );
    let rationale = out.calibration_rationale.expect("rationale recorded");
    assert!(
        rationale.to_lowercase().contains("precondition"),
        "the cap must be recorded on the finding so it's auditable: {rationale}"
    );
}

/// OVER-RATING GUARD: the cross-tenant-read + escalating-data-class floor branch (normally
/// Critical) must ALSO cap at High when the finding's own text records a precondition — here,
/// an attacker needing to know a secret storage path, mirroring the "public bucket of
/// sensitive documents" planted defect from the calibration review.
#[test]
fn severity_floor_caps_cross_tenant_data_class_critical_escalation_at_high_with_precondition() {
    let f = finding_with_detail(
        "AI-PUBLIC-BUCKET-DOCS",
        "medium",
        "An authenticated user can view another organization's stored government id \
             documents in the shared bucket, though exploitation requires knowing the exact \
             storage path.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "a precondition caps the cross-tenant/sensitive-data-class critical escalation at \
             High, not Critical"
    );
}

/// END-TO-END regression pin: `apply_verdicts` blocks the model's upward raise for a
/// preconditioned finding (cycle-4 R2), and the D5 floor that runs immediately after it must
/// NOT re-raise the finding back to Critical from its own independent text scan — this is the
/// exact failure mode the calibration review caught on the second attempt.
#[test]
fn severity_floor_does_not_re_raise_what_apply_verdicts_blocked_for_precondition() {
    let findings = vec![finding_with_detail(
        "AI-MUTATION-FORGED-COOKIE-2",
        "high",
        "The admin-settings mutation runs with no authentication required on the session \
             check; exploitation requires forging a session cookie to pass the check.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"escalate to critical"}
        ]}"#;
    let calibrated = apply_verdicts(raw, findings);
    assert_eq!(
        calibrated[0].severity, "high",
        "apply_verdicts must block the model's upward raise (cycle-4 R2)"
    );
    let floored = apply_severity_calibration_rules(calibrated);
    assert_eq!(
        floored[0].severity, "high",
        "the deterministic D5 floor must not re-raise to critical what apply_verdicts \
             already capped — this was the cycle-4-to-cycle-5 regression"
    );
}

/// UNDER-RATING GUARD (do-not-break): a genuine unauthenticated exposure of sensitive data
/// with NO precondition recorded must still floor all the way to Critical — the cap must
/// never become a general brake on real unconditional criticals.
#[test]
fn severity_floor_unauthenticated_sensitive_data_exposure_stays_critical_without_precondition() {
    let f = finding_with_detail(
        "AI-UNAUTH-PII-EXPORT",
        "medium",
        "The /api/records endpoint is unauthenticated and returns every user's social \
             security number with no access control.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "a genuine unauthenticated sensitive-data exposure with no precondition must stay \
             Critical"
    );
}

/// UNDER-RATING GUARD (do-not-break, false-positive regression): "requires no
/// authentication" is NOT a precondition — it NEGATES the requirement, describing the exact
/// unconditional exposure the floor polices. Without the negation guard on the "requires "
/// phrase, this exact wording (already used by `severity_floor_applies_across_a_finding_set_
/// after_apply_verdicts` above) would be misread as a precondition and wrongly capped at
/// High instead of Critical.
#[test]
fn severity_floor_requires_no_authentication_is_not_mistaken_for_a_precondition() {
    let f = finding_with_detail(
        "AI-UNAUTH-REQUIRES-NO-AUTH",
        "medium",
        "The export endpoint requires no authentication and returns every user's records.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "critical",
        "\"requires no authentication\" negates the requirement and must still floor to \
             Critical, not be capped at High"
    );
}

/// GUARDRAIL (do-not-break): the floor-high RLS-hedge class (injection family, which covers
/// stored XSS) must stay exactly High — unaffected by the precondition cap, since the cap
/// only ever touches a Critical floor value, never a High one.
#[test]
fn severity_floor_stored_xss_injection_class_rls_hedge_stays_high_guardrail() {
    let f = finding_with_category(
        "AI-STORED-XSS-RLS-HEDGE",
        "medium",
        "Stored XSS: user-supplied HTML is rendered without sanitization; this is likely \
             covered by RLS on the underlying table.",
        "injection",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "high",
        "a stored-XSS/injection-class floor must stay High on an RLS hedge, unaffected by \
             the precondition cap"
    );
}

/// GUARDRAIL (do-not-break): a weak-token-class finding that trips none of the floor's
/// phrases stays exactly at its authored Medium — neither raised by the floor nor lowered by
/// the precondition cap (which only ever lowers a Critical FLOOR VALUE, never a finding's
/// already-assigned severity).
#[test]
fn severity_floor_weak_token_class_stays_medium_guardrail() {
    let f = finding_with_detail(
        "AI-WEAK-SESSION-TOKEN",
        "medium",
        "Session tokens are generated with a weak, predictable random source, making them \
             guessable with effort.",
    );
    let out = apply_severity_calibration_rule(f);
    assert_eq!(
        out.severity, "medium",
        "a weak-token finding that trips no floor rule stays at its authored Medium"
    );
    assert_eq!(
        out.calibration_rationale, None,
        "no floor rule applies here; the precondition cap must not fabricate a rationale \
             for a finding the floor never touched"
    );
}

// ── D6: severity ceiling — R1/R2 (2026-09-30 cycle-2 queue-hardening) ─────────
//
// The D5 floor above only ever raises severity. These pin the opposite direction: R1
// re-routes a self-hedged "probably intentional" finding to needs-review + Low (out of every
// action bucket, but never dropped); R2 clamps a browser-mediated CORS misconfiguration to
// exactly Medium in BOTH directions, without disturbing the floor's Critical rating for a
// genuinely unauthenticated-exposure finding.

const CORS_CREDENTIALS_TEXT: &str =
    "The API's CORS policy reflects the request's Origin header back verbatim and sets \
         Access-Control-Allow-Credentials: true, so any origin can make authenticated \
         cross-origin requests riding the victim's session cookies.";

/// R2, downward direction: a CORS-with-credentials finding that started at Critical (the
/// current over-rating failure) must land at exactly Medium — not left at Critical.
#[test]
fn severity_ceiling_cors_credentials_lowers_critical_to_medium() {
    let f = finding_with_detail("AI-CORS-1", "critical", CORS_CREDENTIALS_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "medium");
    assert!(out.calibration_rationale.is_some());
}

/// R2, upward direction: the SAME class starting at Low (the prior re-burial failure) must
/// ALSO land at exactly Medium — not stay at Low.
#[test]
fn severity_ceiling_cors_credentials_raises_low_to_medium() {
    let f = finding_with_detail("AI-CORS-2", "low", CORS_CREDENTIALS_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "medium");
}

/// Bidirectional guard, spelled out explicitly: the CORS class must never be observed at
/// either extreme after both passes run, whichever extreme it started at.
#[test]
fn severity_ceiling_cors_credentials_is_never_low_or_critical() {
    for start in ["critical", "high", "medium", "low", "info"] {
        let f = finding_with_detail("AI-CORS-3", start, CORS_CREDENTIALS_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_ne!(
            out.severity, "low",
            "starting severity {start:?} must not stay Low"
        );
        assert_ne!(
            out.severity, "critical",
            "starting severity {start:?} must not become Critical"
        );
        assert_eq!(out.severity, "medium");
    }
}

/// Non-interference: a genuinely unauthenticated-exposure finding (no CORS vocabulary at
/// all) must still floor to Critical after BOTH the floor and the ceiling run — the D6
/// ceiling must never reintroduce the old under-rating for a class it doesn't target.
#[test]
fn severity_ceiling_does_not_touch_unauthenticated_exposure_floor() {
    let f = finding_with_detail(
        "AI-UNAUTH-EXPORT-2",
        "medium",
        "The export endpoint requires no authentication and returns every user's records.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "critical",
        "a non-CORS unauthenticated-exposure finding keeps the floor's critical rating"
    );
}

const SELF_HEDGE_TEXT: &str =
    "This debug endpoint skips the auth middleware, but it appears intentional and is \
         likely only used for internal load-testing; confirm whether this is expected before \
         treating it as a defect.";

/// R1: a self-hedged "probably intentional / confirmation-only" finding is capped to Low
/// AND flagged needs-review — routing it to the `held` bucket (C5-4) without dropping it.
#[test]
fn severity_ceiling_self_hedged_confirmation_only_caps_to_low_and_needs_review() {
    let f = finding_with_detail("AI-HEDGE-1", "high", SELF_HEDGE_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "low");
    assert_eq!(out.confidence.as_deref(), Some("needs-review"));
    assert!(out.calibration_rationale.is_some());
}

/// Safe twin: an actionable finding at the SAME base severity whose text is NOT self-hedged
/// is left completely unchanged by the ceiling pass.
#[test]
fn severity_ceiling_non_hedged_twin_is_unchanged() {
    let f = finding_with_detail(
        "AI-HEDGE-2",
        "high",
        "This debug endpoint skips the auth middleware in production with no mitigating \
             control; it must be removed before release.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f.clone()));
    assert_eq!(out.severity, "high");
    assert_eq!(out.confidence, None);
}

/// R1 negation guard: text that explicitly says the pattern is NOT intentional (a confirmed
/// defect, not a hedge) must never be swept up by the self-hedge phrase set.
#[test]
fn severity_ceiling_self_hedge_negation_guard_does_not_downgrade() {
    let f = finding_with_detail(
        "AI-HEDGE-3",
        "high",
        "The team confirmed this is NOT intentional — it's a genuine authorization bug that \
             must be fixed.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "high",
        "negated hedge language must not downgrade severity"
    );
    assert_eq!(out.confidence, None);
}

// ── D7 (C6-A3): severity ceiling R3 (open/unvalidated redirect) and R4 (a privileged/role
// check enforced only in client-visible code) ────────────────────────────────────────────
//
// Observed across two independent benchmark repos: a client-side/open redirect was
// inflating to High, and a UI-only admin/role check was inflating to Critical, run over run
// on the SAME code. Each anchor below is pinned bidirectionally — an over-rating guard (the
// class caps at its anchor) and an under-rating guard (the regression list of classes that
// must NOT be buried by the new rules: a chained redirect, a genuine unauthenticated
// exposure, a committed secret, the weak-token class, and the floor-high
// injection/stored-XSS class).

const OPEN_REDIRECT_TEXT: &str =
    "The /login?next= handler performs an open redirect: `next` is taken straight from the \
         query string and used as the post-login redirect target with no allow-list or same-\
         origin check.";

const CHAINED_REDIRECT_TEXT: &str =
    "The OAuth callback performs an open redirect to a `return_to` URL taken from the query \
         string with no allow-list check; the redirect destination captures the session token \
         appended to the fragment, so a crafted link exfiltrates the victim's session.";

const CLIENT_ONLY_ADMIN_CHECK_TEXT: &str =
    "The `isAdmin` role check that gates the delete-organization button is enforced \
         client-side only in the React component; there is no server-side check on the DELETE \
         /orgs/:id route, so any authenticated user can send the request directly.";

/// OVER-RATING GUARD: an open-redirect finding that started Critical (the observed inflation
/// failure) must land at exactly Medium, not stay at High or Critical.
#[test]
fn severity_ceiling_open_redirect_caps_critical_at_medium() {
    let f = finding_with_detail("AI-OPEN-REDIRECT-1", "critical", OPEN_REDIRECT_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "medium");
    assert!(out.calibration_rationale.is_some());
}

/// OVER-RATING GUARD, upward direction: the SAME unchained redirect class starting at Low
/// must ALSO land at exactly Medium — not left buried at Low. Bidirectional, like R2's CORS
/// clamp.
#[test]
fn severity_ceiling_open_redirect_raises_low_to_medium() {
    let f = finding_with_detail("AI-OPEN-REDIRECT-2", "low", OPEN_REDIRECT_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "medium");
}

/// Bidirectional guard, spelled out explicitly: an UNCHAINED open redirect must never be
/// observed at either extreme after both passes run, whichever extreme it started at.
#[test]
fn severity_ceiling_open_redirect_is_never_low_high_or_critical() {
    for start in ["critical", "high", "medium", "low", "info"] {
        let f = finding_with_detail("AI-OPEN-REDIRECT-3", start, OPEN_REDIRECT_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "medium",
            "starting severity {start:?} must land at exactly Medium"
        );
    }
}

/// UNDER-RATING GUARD (do-not-break): an open redirect CHAINED to credential capture must
/// land at High, not buried at Medium — even when it started at Low or Medium.
#[test]
fn severity_ceiling_chained_redirect_raises_low_to_high() {
    let f = finding_with_detail("AI-CHAINED-REDIRECT-1", "low", CHAINED_REDIRECT_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "high",
        "a redirect chained to credential capture must reach High, not be buried at Medium"
    );
}

/// UNDER-RATING GUARD (do-not-break), over-rating half of the same chained class: a chained
/// redirect that started Critical must come DOWN to exactly High, not stay at Critical —
/// chaining earns High, not Critical, from the redirect mechanism alone.
#[test]
fn severity_ceiling_chained_redirect_caps_critical_at_high() {
    let f = finding_with_detail("AI-CHAINED-REDIRECT-2", "critical", CHAINED_REDIRECT_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "high");
}

/// NEVER-DEMOTE / BLOCKS-UP-ONLY guard: a finding whose text is BOTH an open redirect AND,
/// independently, an unauthenticated path to a privileged operation earned its Critical
/// through the D5 floor's mechanism, not the redirect's — R3 must not demote it.
#[test]
fn severity_ceiling_open_redirect_does_not_demote_unauthenticated_exposure() {
    let f = finding_with_detail(
        "AI-REDIRECT-PLUS-UNAUTH",
        "high",
        "The admin panel's /impersonate endpoint performs an open redirect to an \
             attacker-controlled URL AND requires no authentication at all, letting anyone \
             assume any user's identity.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "critical",
        "an independently-earned unauthenticated/full-compromise critical must survive the \
             redirect ceiling, not be demoted to Medium"
    );
}

/// OVER-RATING GUARD: a privileged/role check enforced only client-side (the observed
/// inflation failure) must cap at High, not stay Critical.
#[test]
fn severity_ceiling_client_only_admin_check_caps_critical_at_high() {
    let f = finding_with_detail(
        "AI-CLIENT-ADMIN-1",
        "critical",
        CLIENT_ONLY_ADMIN_CHECK_TEXT,
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "high");
    assert!(out.calibration_rationale.is_some());
}

/// Cap-only semantics (like R1): a client-only admin-check finding that started at Medium
/// must NOT be raised by R4 — the rule only ever lowers a severity above its anchor, never
/// raises one below it. The rationale is still recorded (transparency, same convention as
/// D5's floor when it "touches but doesn't change" a finding).
#[test]
fn severity_ceiling_client_only_admin_check_does_not_raise_below_high() {
    let f = finding_with_detail("AI-CLIENT-ADMIN-2", "medium", CLIENT_ONLY_ADMIN_CHECK_TEXT);
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "medium",
        "R4 caps Critical down to High; it must never raise a lower severity up to High"
    );
    assert!(
        out.calibration_rationale.is_some(),
        "the rule touched this finding (it matched the class) so a rationale is recorded \
             even though the severity itself didn't change"
    );
}

/// NEVER-DEMOTE / BLOCKS-UP-ONLY guard: a finding whose text is BOTH a client-only admin
/// check AND, independently, an unauthenticated path to full account compromise earned its
/// Critical through the D5 floor's mechanism — R4 must not demote it.
#[test]
fn severity_ceiling_client_only_admin_check_does_not_demote_unauthenticated_exposure() {
    let f = finding_with_detail(
        "AI-CLIENT-ADMIN-PLUS-UNAUTH",
        "high",
        "The admin role check is enforced client-side only with no server-side check, and \
             separately the account-recovery flow allows full account takeover with no \
             authentication required at all.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "critical",
        "an independently-earned unauthenticated/full-compromise critical must survive the \
             client-only-admin-check ceiling, not be capped to High"
    );
}

/// UNDER-RATING GUARD (do-not-break, shared regression list): a genuinely unauthenticated
/// sensitive-data exposure with no redirect/client-check vocabulary at all must still reach
/// Critical through the full floor+ceiling pipeline — R3/R4 must not become a general brake
/// on real unconditional criticals they don't target.
#[test]
fn severity_ceiling_unauthenticated_exposure_unaffected_by_new_rules() {
    let f = finding_with_detail(
        "AI-UNAUTH-EXPORT-3",
        "medium",
        "The /api/records endpoint is unauthenticated and returns every user's social \
             security number with no access control.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "critical");
}

/// UNDER-RATING GUARD (do-not-break, shared regression list): a committed secret, run
/// through the model-verdict path AND the full deterministic floor+ceiling pipeline, must
/// still land at Critical — R3/R4 have no redirect/client-check vocabulary to match here, so
/// this pins that the new rules don't accidentally interfere with an unrelated class.
#[test]
fn severity_ceiling_committed_secret_unaffected_by_new_rules() {
    let findings = vec![finding_with_detail(
        "AI-STRIPE-SECRET-COMMITTED-2",
        "high",
        "A live Stripe secret key is hardcoded in this file and committed to the repository.",
    )];
    let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","real":"yes","reason":"secret committed to version control"}
        ]}"#;
    let calibrated = apply_verdicts(raw, findings);
    let floored = apply_severity_calibration_rules(calibrated);
    let ceiled = apply_severity_ceiling_rules(floored);
    assert_eq!(ceiled[0].severity, "critical");
}

/// UNDER-RATING GUARD (do-not-break, shared regression list): the weak-token class, which
/// trips no floor or ceiling rule at all, must stay exactly at its authored Medium through
/// the full pipeline — never demoted to Low by the new rules.
#[test]
fn severity_ceiling_weak_token_class_unaffected_by_new_rules() {
    let f = finding_with_detail(
        "AI-WEAK-SESSION-TOKEN-2",
        "medium",
        "Session tokens are generated with a weak, predictable random source, making them \
             guessable with effort.",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(
        out.severity, "medium",
        "the weak-token class must stay at Medium, never buried at Low by the new rules"
    );
}

/// UNDER-RATING GUARD (do-not-break, shared regression list): a floor-high injection/stored-
/// XSS finding must stay exactly High through the full pipeline — unaffected by R3/R4, which
/// have no redirect/client-check vocabulary to match on this finding's text.
#[test]
fn severity_ceiling_stored_xss_floor_high_unaffected_by_new_rules() {
    let f = finding_with_category(
        "AI-STORED-XSS-RLS-HEDGE-2",
        "medium",
        "Stored XSS: user-supplied HTML is rendered without sanitization; this is likely \
             covered by RLS on the underlying table.",
        "injection",
    );
    let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
    assert_eq!(out.severity, "high");
}

// ── Determinism: the deterministic calibration layer must be stable run-to-run ───────────
//
// The engine has oscillated on severity for the SAME code across runs. The floor (D5) and
// ceiling (D6/D7) passes are pure functions of each finding's own text, so — unlike the
// calibration MODEL, which this test makes no claim about and cannot make deterministic —
// running them twice over an identical finding set must produce identical output: same
// severities, same calibration rationale text, same order. This is the guard for "same
// input -> same severity" for everything that does NOT depend on the model's judgment call.

/// Running the deterministic floor+ceiling pipeline twice over the SAME finding set (one
/// finding per anchor covered above, plus a couple of untouched/unrelated findings) must
/// yield identical severities, identical rationale text, and identical ordering both times.
#[test]
fn deterministic_calibration_pipeline_is_stable_across_two_runs() {
    fn build_set() -> Vec<Finding> {
        vec![
            finding_with_detail(
                "AI-UNAUTH",
                "medium",
                "The export endpoint is unauthenticated and returns every user's records.",
            ),
            finding_with_detail("AI-REDIRECT", "high", OPEN_REDIRECT_TEXT),
            finding_with_detail("AI-CHAINED-REDIRECT", "low", CHAINED_REDIRECT_TEXT),
            finding_with_detail("AI-CLIENT-ADMIN", "critical", CLIENT_ONLY_ADMIN_CHECK_TEXT),
            finding_with_detail("AI-CORS", "low", CORS_CREDENTIALS_TEXT),
            finding_with_detail("AI-HEDGE", "high", SELF_HEDGE_TEXT),
            finding_with_detail(
                "AI-WEAK-TOKEN",
                "medium",
                "Session tokens are generated with a weak, predictable random source.",
            ),
            finding_with_detail("AI-UNRELATED", "medium", "A minor style inconsistency."),
        ]
    }

    fn run(findings: Vec<Finding>) -> Vec<(String, String, Option<String>)> {
        let floored = apply_severity_calibration_rules(findings);
        let ceiled = apply_severity_ceiling_rules(floored);
        ceiled
            .into_iter()
            .map(|f| (f.rule_id, f.severity, f.calibration_rationale))
            .collect()
    }

    let run1 = run(build_set());
    let run2 = run(build_set());
    assert_eq!(
        run1, run2,
        "the deterministic floor+ceiling pipeline must produce identical severities, \
             rationale text, and ordering across two runs over the same input — this does NOT \
             (and cannot) pin the calibration MODEL itself, only this deterministic layer"
    );
}

// ── Structured confidence + effort (Part 1 §3) ────────────────────────────

/// `apply_verdicts` sets the STRUCTURED `confidence`/`needs_review` fields, derived from the
/// verdict's `real` answer. The `[needs review]` detail tag is no longer written.
#[test]
fn apply_verdicts_sets_structured_confidence_and_needs_review() {
    let findings = vec![
        finding("AI-TIMING", "medium"), // index 0 -> low confidence
        finding("AI-REAL", "high"),     // index 1 -> high confidence
    ];
    let raw = r#"{"verdicts":[
            {"index":0,"real":"unclear","effort":"low","reason":"negligible timing residual"},
            {"index":1,"severity":"high","real":"yes","effort":"high","reason":"concrete"}
        ]}"#;
    let out = apply_verdicts(raw, findings);
    let timing = out.iter().find(|f| f.rule_id == "AI-TIMING").unwrap();
    assert_eq!(timing.confidence.as_deref(), Some("needs-review"));
    assert!(
        timing.needs_review,
        "an unclear verdict must set structured needs_review"
    );
    assert_eq!(timing.effort.as_deref(), Some("low"));
    // The structured fields are the whole signal: no `[needs review: ...]` suffix is written
    // onto the client text any more.
    assert!(!timing.detail.contains("[needs review"));

    let real = out.iter().find(|f| f.rule_id == "AI-REAL").unwrap();
    assert_eq!(real.confidence.as_deref(), Some("high"));
    assert!(
        !real.needs_review,
        "a real finding in production code must not set needs_review"
    );
    assert_eq!(real.effort.as_deref(), Some("high"));
}

/// A verdict with a mis-shaped or missing `effort` value must leave `Finding.effort`
/// untouched (fail-soft — effort is advisory, never load-bearing) rather than recording a
/// guessed value.
#[test]
fn apply_verdicts_ignores_invalid_effort_value() {
    let findings = vec![finding("AI-X", "medium")];
    let raw = r#"{"verdicts":[{"index":0,"real":"yes","effort":"extreme","reason":""}]}"#;
    let out = apply_verdicts(raw, findings);
    assert_eq!(
        out[0].effort, None,
        "an unrecognized effort value must not be recorded"
    );

    let findings2 = vec![finding("AI-Y", "medium")];
    let raw2 = r#"{"verdicts":[{"index":0,"real":"yes","reason":""}]}"#;
    let out2 = apply_verdicts(raw2, findings2);
    assert_eq!(
        out2[0].effort, None,
        "an absent effort field must leave effort as None"
    );
}

/// The calibration verdict JSON schema (Part 1 §3) parses `effort` alongside
/// severity/confidence/reason — this pins the wire shape the system prompt
/// (`verify_system_prompt`) asks the model to emit.
#[test]
fn calibration_verdict_json_parses_effort_field() {
    let raw = r#"{"verdicts":[{"index":0,"severity":"high","real":"yes","effort":"medium","reason":"a clear break"}]}"#;
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let verdict = &v["verdicts"][0];
    assert_eq!(verdict["effort"], "medium");
    // Round-trips through apply_verdicts onto the Finding.
    let out = apply_verdicts(raw, vec![finding("SEC-X", "high")]);
    assert_eq!(out[0].effort.as_deref(), Some("medium"));
}

#[test]
fn apply_verdicts_fail_open_on_garbage() {
    let findings = vec![finding("AI-X", "high")];
    // Unparseable verdicts -> keep all (never silently lose a finding).
    let out = apply_verdicts("the model rambled", findings);
    assert_eq!(out.len(), 1);
}

#[test]
fn parse_keys_adopted_rule_ids_directly_and_prefixes_others() {
    let adopted: std::collections::HashSet<String> =
        ["ARCH-STRICT-LAYERING-1".to_string()].into_iter().collect();
    let raw = r#"{"findings":[
            {"path":"svc.rs","line":12,"severity":"high","rule":"ARCH-STRICT-LAYERING-1","title":"service hits db","detail":"d"},
            {"path":"x.rs","line":3,"severity":"low","rule":"some-new-smell","title":"t","detail":"d"}
        ],"proposed_rules":[]}"#;
    let (f, _r) = parse_ai_findings("me/api", raw, &adopted);
    assert_eq!(f.len(), 2);
    // An adopted id is keyed verbatim; an unlisted issue gets the AI- prefix.
    assert!(f.iter().any(|x| x.rule_id == "ARCH-STRICT-LAYERING-1"));
    assert!(f.iter().any(|x| x.rule_id == "AI-SOME-NEW-SMELL"));
}

#[test]
fn parse_garbage_yields_empty_not_error() {
    let none = std::collections::HashSet::new();
    let (f, r) = parse_ai_findings("me/api", "the model declined to answer in JSON", &none);
    assert!(f.is_empty());
    assert!(r.is_empty());
    let (f2, r2) = parse_ai_findings("me/api", "{ not valid json ]", &none);
    assert!(f2.is_empty());
    assert!(r2.is_empty());
}

#[test]
fn empty_findings_object_is_clean() {
    let none = std::collections::HashSet::new();
    let (f, r) = parse_ai_findings("me/api", r#"{"findings": [], "proposed_rules": []}"#, &none);
    assert!(f.is_empty());
    assert!(r.is_empty());
}
