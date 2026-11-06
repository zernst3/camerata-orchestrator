//! The deterministic severity floor and ceiling rules applied after the model has calibrated a finding.

use super::*;

// ── D5: severity calibration rules of thumb ──────────────────────────────────────────────
//
// the codebase-inspection hardening notes, D5. The calibration MODEL already
// carries these rules of thumb in its own system prompt (`verify_system_prompt`, above), but a
// model's application of a rule of thumb is, by construction, a per-run judgment call — the same
// finding phrased identically twice is not guaranteed the same verdict twice. This section is
// the DETERMINISTIC backstop: a pure post-calibration pass that re-derives the floor severity
// from the finding's own text and enforces it regardless of what the model said, recording WHY
// on the finding (`Finding::calibration_rationale`) so the calibrated severity is auditable, not
// a coin flip. It only ever RAISES severity to a floor — it never lowers a severity the model (or
// the deterministic origin) already assigned above the floor.

/// Phrases that mark a finding as unauthenticated access to a protected resource/operation, or a
/// path to full-account compromise (takeover/impersonation) — the plan's "Critical" class.
/// Deliberately phrase-based (not a single keyword) so a finding merely mentioning the word
/// "authentication" in passing (e.g. "the authentication middleware also logs...") doesn't
/// trip the floor — every phrase here asserts the ABSENCE of auth or a completed compromise.
pub(super) const UNAUTHENTICATED_OR_FULL_COMPROMISE_PHRASES: &[&str] = &[
    "unauthenticated",
    "without authentication",
    "without any authentication",
    "no authentication",
    "no authentication required",
    "no authentication is required",
    "missing authentication",
    "not authenticated",
    "without logging in",
    "without needing to log in",
    "no login required",
    "requires no login",
    "full account compromise",
    "full account takeover",
    "complete account takeover",
    "complete account compromise",
    "account takeover",
    "take over any account",
    "take over any user's account",
    "impersonate any user",
    "assume any user's identity",
];

/// Phrases marking an AUTHENTICATED cross-tenant/cross-user READ — the plan's "High by default"
/// class. Requires BOTH a cross-tenant/cross-user phrase AND a read-shaped verb so a finding
/// about a cross-tenant WRITE (already its own, typically higher, class) or an unrelated mention
/// of "another organization" doesn't spuriously floor to High via this rule.
pub(super) const CROSS_TENANT_PHRASES: &[&str] = &[
    "cross-tenant",
    "cross tenant",
    "cross-organization",
    "cross organization",
    "cross-org",
    "another organization's",
    "other organizations'",
    "another tenant's",
    "other tenants'",
    "another user's",
    "other users'",
    "belonging to another user",
    "belonging to a different organization",
    "data from another account",
];
pub(super) const READ_VERBS: &[&str] = &[
    "read", "view", "fetch", "access", "download", "list", "see", "retrieve",
];

/// Data-class phrases that escalate an authenticated cross-tenant read from High to Critical:
/// payment data, credentials/secrets, and sensitive PII — the plan's explicit escalation list.
pub(super) const PAYMENT_DATA_PHRASES: &[&str] = &[
    "payment",
    "credit card",
    "card number",
    "cvv",
    "bank account",
    "routing number",
    "ach transfer",
    "stripe secret",
];
pub(super) const CREDENTIAL_DATA_PHRASES: &[&str] = &[
    "password",
    "api key",
    "secret key",
    "private key",
    "access token",
    "session token",
    "credential",
    "oauth token",
    "refresh token",
];
pub(super) const SENSITIVE_PII_PHRASES: &[&str] = &[
    "social security",
    "ssn",
    "passport number",
    "driver's license",
    "medical record",
    "health record",
    "government id",
];

/// Hedge phrases that speculate a deeper layer (RLS, or defense-in-depth generally) already
/// covers a finding — the exact rationalization D3/D5 forbid as a reason to under-rate an
/// access-control or injection finding. Requires the text to mention RLS/row-level-security AT
/// ALL, AND one of these hedges, so a finding that CONFIRMS (not speculates) a mitigating RLS
/// policy is unaffected — this only catches speculative language ("probably", "likely", ...).
pub(super) const RLS_HEDGE_PHRASES: &[&str] = &[
    "probably",
    "likely",
    "presumably",
    "should prevent",
    "should mitigate",
    "should cover",
    "should catch",
    "may prevent",
    "may mitigate",
    "might prevent",
    "could prevent",
    "defense in depth",
    "defense-in-depth",
    "is likely covered",
    "is probably covered",
];

/// The lowercased text a D5 floor rule scans: `detail` + `snippet` + `category`, concatenated —
/// the model's phrasing can land in the summary prose (`detail`) or, for a deterministic/floor
/// finding, in the description-shaped `snippet`. Case-folded once so every phrase table above can
/// stay lowercase.
pub(super) fn calibration_floor_scan_text(f: &Finding) -> String {
    format!(
        "{} {} {}",
        f.detail,
        f.snippet,
        f.category.as_deref().unwrap_or("")
    )
    .to_ascii_lowercase()
}

/// W8 (Q4): the inflections a D5 phrase-table entry may carry and still be the SAME word.
/// Every table above is written in the BARE form ("read", "credential", "medical record") but
/// the text being scanned is a model's English prose, where the same word arrives inflected
/// ("reads", "accessed", "listing", "credentials", "accessible"). Whole-word-only matching
/// would therefore have silenced most genuine escalations, which is a worse failure than the
/// substring bug it fixes — so the END boundary admits exactly this closed set, and nothing
/// else. Each entry earns its place: `"s"`/`"es"` (plurals and third-person), `"ed"`/`"ing"`
/// (participles), `"able"`/`"ible"` (`readable`, `accessible`).
///
/// Deliberately ABSENT, each for a demonstrated false positive a looser set would readmit:
/// `"y"` would readmit `read` -> `ready`; `"en"` would readmit `list` -> `listen`; `"er"`
/// would readmit `list` -> `lister`; bare `"d"` is handled separately below rather than listed
/// here, because unconditionally it readmits `see` -> `seed`.
pub(super) const PHRASE_INFLECTION_SUFFIXES: &[&str] = &["s", "es", "ed", "ing", "able", "ible"];

/// Whether a bare `"d"` may follow `phrase` as an inflection. English forms the past tense of
/// an `e`-final verb by appending `d` alone (`retrieve` -> `retrieved`), so the suffix is real
/// and dropping it would lose genuine matches. It is gated on a stem of 4+ characters —
/// the same structural threshold W7/B9's [`dotted_run_is_identifier`] uses, and for the same
/// reason: a 3-character stem is too short to carry an inflection unambiguously. That gate is
/// what keeps `see` -> `seed` (a noun, and a test fixture word) out while keeping
/// `retrieve` -> `retrieved` in.
pub(super) fn phrase_admits_bare_d_suffix(phrase: &str) -> bool {
    phrase.len() >= 4 && phrase.ends_with('e')
}

/// Whether `c` is part of a word for boundary purposes — Unicode-aware `is_alphanumeric` plus
/// `_`, i.e. the usual `\w` class. A hyphen is deliberately NOT a word character: the phrase
/// tables already contain hyphenated entries (`"cross-tenant"`, `"defense-in-depth"`) that must
/// match as written, and a hyphen in prose separates words rather than joining them.
pub(super) fn is_phrase_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// W8 (Q4): whether `text` contains `phrase` as a WORD rather than as a bare substring.
///
/// The D5 floor tables are matched against the MODEL'S PROSE, and they contain one-syllable
/// entries (`"read"`, `"view"`, `"list"`, `"see"`, `"rls"`) that are substrings of common,
/// entirely unrelated English words. Under the previous `text.contains(phrase)` test,
/// al`read`y, re`view`, allow`list`, `each` transfer, `password`less and signed-u`rls` all
/// tripped the floor, manufacturing High and Critical severities out of ordinary prose — the
/// measured case being a finding that floored to High on "another user's ... already ..." and
/// to Critical once the word "credential" appeared anywhere in the same text.
///
/// Same fix shape as W7/B9's [`dotted_run_is_identifier`]: a structural property, not a
/// blacklist of observed words. A match counts when it begins at a word boundary AND ends at
/// one, where "ends at one" also admits a single inflection from the closed
/// [`PHRASE_INFLECTION_SUFFIXES`] set (see its doc comment for why whole-word-only would be
/// the worse bug). Plain `char` boundary tests rather than a regex: the scan text has been
/// `to_ascii_lowercase`d but may still hold non-ASCII, and `regex::bytes`' Unicode-by-default
/// mode is a trap this does not need to step near.
///
/// Every phrase table is matched through here; none of them changed.
pub(super) fn contains_phrase_as_word(text: &str, phrase: &str) -> bool {
    if phrase.is_empty() {
        return false;
    }
    for (start, _) in text.match_indices(phrase) {
        // Leading boundary: the character immediately before the match must not be a word
        // character. This alone kills al`read`y / re`view` / allow`list` / `each` transfer /
        // signed-u`rls`.
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(is_phrase_word_char)
        {
            continue;
        }
        let rest = &text[start + phrase.len()..];
        match rest.chars().next() {
            // End of text, or a non-word character: an exact word match.
            None => return true,
            Some(c) if !is_phrase_word_char(c) => return true,
            // Otherwise the match runs into more word characters. It still counts, but only
            // when what follows is exactly one admitted inflection AND that inflection itself
            // ends at a boundary — so `access` matches `accessed` but `password` does not
            // match `passwordless`.
            Some(_) => {
                let admitted = PHRASE_INFLECTION_SUFFIXES
                    .iter()
                    .copied()
                    .chain(phrase_admits_bare_d_suffix(phrase).then_some("d"))
                    .any(|suffix| {
                        rest.strip_prefix(suffix).is_some_and(|after| {
                            after.chars().next().is_none_or(|c| !is_phrase_word_char(c))
                        })
                    });
                if admitted {
                    return true;
                }
            }
        }
    }
    false
}

pub(super) fn mentions_unauthenticated_or_full_compromise(text: &str) -> bool {
    UNAUTHENTICATED_OR_FULL_COMPROMISE_PHRASES
        .iter()
        .any(|p| contains_phrase_as_word(text, p))
}

pub(super) fn mentions_authenticated_cross_tenant_read(text: &str) -> bool {
    CROSS_TENANT_PHRASES
        .iter()
        .any(|p| contains_phrase_as_word(text, p))
        && READ_VERBS.iter().any(|v| contains_phrase_as_word(text, v))
}

pub(super) fn mentions_escalating_data_class(text: &str) -> bool {
    PAYMENT_DATA_PHRASES
        .iter()
        .chain(CREDENTIAL_DATA_PHRASES)
        .chain(SENSITIVE_PII_PHRASES)
        .any(|p| contains_phrase_as_word(text, p))
}

pub(super) fn mentions_speculative_rls_hedge(text: &str) -> bool {
    // `"rls"` was the worst offender of the whole section: as a bare substring it matched
    // `urls`, so ANY access-control finding mentioning signed URLs alongside an ordinary hedge
    // word was floored to High without row-level security appearing in the text at all.
    let mentions_rls = contains_phrase_as_word(text, "rls")
        || contains_phrase_as_word(text, "row-level security")
        || contains_phrase_as_word(text, "row level security");
    mentions_rls
        && RLS_HEDGE_PHRASES
            .iter()
            .any(|h| contains_phrase_as_word(text, h))
}

/// Whether `f` is in the access-control / injection family D3 already treats as a class that
/// must never be softened by "RLS probably contains it" reasoning: RLS policy, authorization, or
/// injection (query-grammar injection included). Checks the finding's OWN `category` first (set
/// by calibration or an earlier merge pass), falling back to `categorize_rule_id` over its rule
/// id — reusing the same word-boundary-matched categorization D5's own Part B hardened, rather
/// than re-deriving a separate notion of "is this access-control".
pub(super) fn is_access_control_or_injection_class(f: &Finding) -> bool {
    let cat = f
        .category
        .clone()
        .or_else(|| categorize_rule_id(&f.rule_id));
    matches!(
        cat.as_deref(),
        Some("rls-policy") | Some("authorization") | Some("injection")
    )
}

/// Whether the text-based floors may look at `f` at all. A phrase in a message is evidence of a
/// defect only when the row IS about a security concern: a security rule of the corpus, a
/// security category, or a row the model invented (`AI-`), whose message is the whole account of
/// what it found. A row of a convention rule (structured errors, layering, middleware order) that
/// says "an unauthenticated caller gets a 401" is describing its own context, and the word must
/// not raise it to Critical.
pub(super) fn floor_may_apply(f: &Finding) -> bool {
    finding_class(f) == FindingClass::Security || f.rule_id.starts_with("AI-")
}

/// The deterministic D5 severity floor for ONE finding: `None` when no rule of thumb applies,
/// else the floor severity plus the rationale to record. Order matters — unauthenticated/full-
/// compromise is checked first (it is the highest floor and the two conditions are mutually
/// exclusive by construction: an unauthenticated finding is never ALSO the "authenticated
/// cross-tenant" case), then the authenticated-cross-tenant-read floor, then the exploit-
/// precondition cap (which can only LOWER a Critical floor to High, never raise or lower
/// anything else), then the RLS-hedge guard (which can only RAISE whatever floor is already
/// computed, never lower it — see the combine step in `apply_severity_calibration_rule`).
pub(super) fn severity_calibration_floor(f: &Finding) -> Option<(&'static str, String)> {
    if !floor_may_apply(f) {
        return None;
    }
    let text = calibration_floor_scan_text(f);
    let mut floor = if mentions_unauthenticated_or_full_compromise(&text) {
        Some((
            "critical",
            "Severity floor: unauthenticated access or a path to full-account compromise is \
             always Critical."
                .to_string(),
        ))
    } else if mentions_authenticated_cross_tenant_read(&text) {
        if mentions_escalating_data_class(&text) {
            Some((
                "critical",
                "Severity floor: an authenticated cross-tenant read escalates to Critical — the \
                 exposed data class (payment, credentials/secrets, or sensitive PII) crosses the \
                 escalation threshold."
                    .to_string(),
            ))
        } else {
            Some((
                "high",
                "Severity floor: an authenticated cross-tenant read is High by default, not \
                 Critical, absent an escalating data class."
                    .to_string(),
            ))
        }
    } else {
        None
    };

    // Exploit-precondition cap (cycle-5 regression fix): the two branches above derive a
    // Critical floor purely from the finding's own unauthenticated/data-class vocabulary —
    // neither one looks at whether that SAME text also records an exploit precondition (forging
    // a cookie or token, tampering with a cookie, knowing a secret path, riding a victim's
    // session). `apply_verdicts`'s upward-raise guard already blocks exactly this on the
    // MODEL-verdict path (see `mentions_exploit_precondition`'s doc comment), but this
    // deterministic floor runs AFTER `apply_verdicts` in `verify_findings` and re-derives
    // Critical independently from the finding's own text — so it was silently re-raising exactly
    // what the verdict-path guard had just blocked (two High-severity planted defects whose own
    // text named a forged-cookie/known-path precondition both still came out Critical via this
    // floor, not via the model verdict). Mirror the same guard here: when THIS floor computed
    // Critical, a recorded precondition caps it at High instead. This can only LOWER the floor's
    // own proposed value, never a finding's already-assigned severity: a finding that doesn't
    // trip the floor at all is simply `None` and untouched, and `apply_severity_calibration_rule`
    // below only ever RAISES toward the floor it is given — so capping the floor at High can only
    // stop a RAISE, never demote a finding already calibrated to Critical by some other path.
    let floor_is_critical = matches!(floor, Some((sev, _)) if sev == "critical");
    if floor_is_critical && mentions_exploit_precondition(&text) {
        floor = Some((
            "high",
            "Severity floor capped at High: the finding's own text records an exploit \
             precondition (token/cookie forgery or tampering, knowledge of a secret path, or \
             riding a victim's session) — the same guard that blocks an upward model-verdict \
             raise to Critical in `apply_verdicts` applies to this deterministic floor too, so \
             it cannot re-raise what that guard already capped."
                .to_string(),
        ));
    }

    // The RLS-hedge guard: an access-control/injection finding must never rank below High
    // because the calibration reasoning speculates a deeper layer (RLS) probably already
    // covers it — defense-in-depth failing at the boundary the finding actually names still
    // stands (ties to D3). This can only RAISE the floor already computed above, never lower it.
    if is_access_control_or_injection_class(f) && mentions_speculative_rls_hedge(&text) {
        let hedge_floor = (
            "high",
            "Severity floor: an access-control/injection finding is not under-rated on an \"RLS \
             probably contains it\" rationale — defense-in-depth failing at the boundary still \
             stands."
                .to_string(),
        );
        floor = Some(match floor {
            Some((sev, reason)) if severity_rank(sev) >= severity_rank(hedge_floor.0) => {
                (sev, reason)
            }
            _ => hedge_floor,
        });
    }
    floor
}

/// Apply the D5 severity floor to one finding: raises `severity` to the floor when the finding's
/// current severity ranks below it, and ALWAYS records the rationale when a rule of thumb
/// touched the finding (even when the floor didn't change anything — the architect can see the
/// rule considered it and found the existing severity already sufficient). Never lowers a
/// severity that already ranks at or above the floor.
pub(super) fn apply_severity_calibration_rule(mut f: Finding) -> Finding {
    if let Some((floor_sev, reason)) = severity_calibration_floor(&f) {
        if severity_rank(&f.severity) < severity_rank(floor_sev) {
            let before = f.severity.clone();
            f.severity = floor_sev.to_string();
            record_severity_adjustment(&mut f, &before, floor_sev, &reason);
        }
        f.calibration_rationale = Some(reason);
    }
    f
}

/// Apply the D5 severity floor to a whole finding set — the deterministic pass run after
/// `apply_verdicts` in `verify_findings` (and safe to run over ANY finding
/// set, including deterministic-floor findings that never see the LLM calibration pass at all,
/// since it derives everything from the finding's own text rather than the model's verdict).
pub fn apply_severity_calibration_rules(findings: Vec<Finding>) -> Vec<Finding> {
    findings
        .into_iter()
        .map(apply_severity_calibration_rule)
        .collect()
}

// ── D6: the severity CEILING — a downward calibration direction (2026-09-30 cycle-2
// queue-hardening, R1/R2). D5's floor above only ever RAISES severity. Two classes need the
// opposite correction, and neither one ever drops a finding — both stay in the queue, re-routed
// and re-badged:
//   - R1: a finding whose OWN text self-hedges "this is probably intentional, just confirm" is
//     capped to Low + flagged `needs-review`, which routes it to the `held` bucket (C5-4;
//     `report_export::is_held_for_review`) instead of an action bucket.
//   - R2: a browser-mediated CORS/cross-origin header misconfiguration (exploitable only through
//     a victim's browser + session, never a direct unauthenticated fetch) is clamped to exactly
//     Medium, bidirectionally — lowering an inflated Critical/High AND raising a re-buried
//     Low/Info. Re-burying it at Low was the prior failure just as much as inflating it to
//     Critical was.
// Both rules derive entirely from the finding's own text (like the floor), so this pass is safe
// to run over ANY finding set, and must run immediately after `apply_severity_calibration_rules`
// everywhere that runs (today: the one production call site in `verify_findings`, plus tests).

/// Phrases identifying a browser-mediated CORS / cross-origin header misconfiguration (R2).
/// Deliberately specific to cross-origin vocabulary (not just "credentials", which appears in
/// countless unrelated findings) so this never fires outside the actual CORS class.
pub(super) const CORS_MISCONFIG_PHRASES: &[&str] = &[
    "cors",
    "cross-origin",
    "cross origin",
    "access-control-allow-origin",
    "access-control-allow-credentials",
    "reflected origin",
    "reflects the origin",
    "reflects any origin",
    "reflects the request's origin",
    "echoes the origin",
    "echoed origin",
    "echoes back the origin",
];

/// Whether `f`'s own text (detail/snippet/category) names the browser-mediated CORS/cross-origin
/// misconfiguration class R2 targets.
pub(super) fn mentions_cors_misconfig(text: &str) -> bool {
    CORS_MISCONFIG_PHRASES.iter().any(|p| text.contains(p))
}

/// Self-hedged "probably intentional / confirmation-only" phrases (R1) — the finding's OWN
/// analysis concluding the pattern is likely fine and only needs a human to confirm intent,
/// rather than a confirmed defect.
pub(super) const SELF_HEDGE_CONFIRMATION_PHRASES: &[&str] = &[
    "likely intentional",
    "appears intentional",
    "appears to be intentional",
    "probably intentional",
    "may be intentional",
    "might be intentional",
    "could be intentional",
    "if this is intentional",
    "if this behavior is intentional",
    "confirm whether",
    "confirmation only",
    "intended behavior",
];

/// Negation guards for the R1 phrase set above — "not intentional" / "clearly not intended"
/// mean the OPPOSITE conclusion (a confirmed defect, not "probably fine"), so these must NEVER
/// be swept up by the self-hedge phrases even though some share the word "intentional"/"intended".
pub(super) const SELF_HEDGE_NEGATION_PHRASES: &[&str] = &[
    "not intentional",
    "not intended",
    "isn't intentional",
    "isn't intended",
];

/// Whether `f`'s own text is a self-hedged "probably intentional, just confirm" conclusion (R1),
/// with the negation guard applied first.
pub(super) fn mentions_self_hedged_confirmation_only(text: &str) -> bool {
    if SELF_HEDGE_NEGATION_PHRASES.iter().any(|p| text.contains(p)) {
        return false;
    }
    SELF_HEDGE_CONFIRMATION_PHRASES
        .iter()
        .any(|p| text.contains(p))
}

// ── D7 (C6-A3): two more severity-ceiling classes, same shape as D6's R1/R2 — derived entirely
// from the finding's own text, never dropping a finding, safe over ANY finding set. Added to
// close a rubric-instability gap observed across two independent benchmark repos: the SAME two
// classes kept drifting to an inflated severity run-to-run.
//
// R3 — open/unvalidated redirect. The exploit boundary this polices: a redirect to an
// attacker-chosen destination only harms the user who follows it — on its own the attacker gains
// nothing more than a referral, so it is Medium. It becomes High exactly when the finding's own
// text says the redirect is CHAINED to credential capture (the destination can read or
// exfiltrate a token, cookie, or credential) — only then does the attacker walk away with
// something usable. It is NEVER Critical from the redirect mechanism alone, chained or not —
// that would require the SAME finding to also describe an unauthenticated path to a privileged
// operation, which is D5's job, not this ceiling's.
//
// R4 — a privileged/role/admin check enforced ONLY in client-visible (UI/browser) code, with no
// independent server-side re-check. The exploit boundary: a client-side control is not itself the
// security boundary — anything a client enforces, a client can be made to skip — so the real
// exposure is bounded by what the SERVER allows an authenticated caller to do once that gate is
// bypassed. An authenticated privilege escalation confined to a single resource/operation is a
// HIGH finding, not Critical; Critical is reserved for an unauthenticated reach or a system- or
// account-wide compromise, neither of which this class states by itself. Cap-only (like R1, it
// never raises a lower severity) — the absent server-side check is itself a real defect worth
// keeping wherever the model or the floor already put it, just never above High.
//
// Both R3 and R4 are gated against the D5 UNAUTHENTICATED/full-compromise floor phrases — unlike
// R2 (CORS), which is authoritative because that vocabulary and the floor's are mutually
// exclusive by construction, a redirect or a client-side check CAN legitimately co-occur in the
// same finding's prose as an unauthenticated-access/full-compromise statement, and when it does
// that Critical was earned through the floor's mechanism, not this ceiling's — so neither R3 nor
// R4 may demote it. This is the BLOCKS-UP-ONLY requirement: these two ceilings only ever cap a
// severity that their OWN class would otherwise have produced, never a severity legitimately
// assigned by a different rule.

/// Phrases naming an open or unvalidated redirect — the destination is attacker-controlled or
/// otherwise unchecked, as opposed to a redirect to a fixed, trusted, hardcoded location.
pub(super) const OPEN_REDIRECT_PHRASES: &[&str] = &[
    "open redirect",
    "open-redirect",
    "unvalidated redirect",
    "redirect target is not validated",
    "redirect url is not validated",
    "redirect destination is not validated",
    "does not validate the redirect",
    "redirect parameter is not restricted",
    "redirects to an attacker-controlled",
    "redirects to an arbitrary",
    "redirects to any external",
    "redirects to any url",
    "arbitrary redirect",
    "unvalidated return url",
    "unchecked redirect",
];

/// Verbs marking the redirect destination as actively capturing something a victim carries —
/// paired with a credential-object phrase (reusing [`CREDENTIAL_DATA_PHRASES`], the same
/// vocabulary D5's cross-tenant escalation branch already uses) so an unrelated mention of
/// "token"/"session" elsewhere in the same finding's prose doesn't spuriously chain it.
pub(super) const CREDENTIAL_CAPTURE_VERBS: &[&str] = &[
    "captures",
    "capture",
    "steals",
    "exfiltrates",
    "exfiltrate",
    "harvests",
    "intercepts",
    "leaks",
    "carries",
    "forwards",
    "appends",
    "includes",
    "sends",
    "transmits",
];

pub(super) fn mentions_open_redirect(text: &str) -> bool {
    OPEN_REDIRECT_PHRASES.iter().any(|p| text.contains(p))
}

/// Whether `text` chains an open redirect to credential capture — BOTH a credential-object
/// phrase AND a capture-shaped verb must be present (the same AND-pair shape as
/// [`mentions_authenticated_cross_tenant_read`]'s phrase+verb pairing above).
pub(super) fn mentions_redirect_chained_to_credential_capture(text: &str) -> bool {
    CREDENTIAL_DATA_PHRASES.iter().any(|c| text.contains(c))
        && CREDENTIAL_CAPTURE_VERBS.iter().any(|v| text.contains(v))
}

/// Phrases marking the control's ENFORCEMENT LOCATION as client-visible/UI-only with no
/// independent server-side check — Group A of the R4 AND-pair, mirroring the
/// [`CROSS_TENANT_PHRASES`] + [`READ_VERBS`] AND-pair shape above (a location signal plus a
/// privilege-type signal, not either alone).
pub(super) const CLIENT_ONLY_ENFORCEMENT_PHRASES: &[&str] = &[
    "client-side only",
    "client side only",
    "only on the client",
    "only in the client",
    "only in the ui",
    "ui-only",
    "ui only",
    "only enforced in the browser",
    "only enforced client-side",
    "browser-only check",
    "no server-side check",
    "no server-side enforcement",
    "not enforced on the server",
    "not re-verified on the server",
    "not re-checked on the server",
    "server never verifies",
    "server does not verify",
    "server does not re-check",
    "server-side check is missing",
    "only hides the button",
    "only hides the ui",
    "disabled client-side",
];

/// Phrases marking the control as a privilege/role/admin gate — Group B of the R4 AND-pair.
/// Deliberately bigram/trigram phrases, not the bare word "admin" (which appears in countless
/// unrelated findings — an admin dashboard, an admin route — with no bearing on this class), so
/// the pair only fires on an actual privilege-gate mention.
pub(super) const PRIVILEGE_ROLE_GATE_PHRASES: &[&str] = &[
    "admin check",
    "admin flag",
    "admin role",
    "is an admin",
    "as an admin",
    "admin access",
    "admin privileges",
    "role check",
    "role field",
    "user role",
    "user's role",
    "permission check",
    "elevated privileges",
    "privilege escalation",
    "isadmin",
    "is_admin",
];

/// Whether `text` describes a privilege/role/admin gate whose enforcement location is
/// client-visible/UI-only — BOTH an enforcement-location phrase AND a privilege-type phrase.
pub(super) fn mentions_client_only_privilege_gate(text: &str) -> bool {
    CLIENT_ONLY_ENFORCEMENT_PHRASES
        .iter()
        .any(|p| text.contains(p))
        && PRIVILEGE_ROLE_GATE_PHRASES.iter().any(|p| text.contains(p))
}

/// Apply the R1-R4 severity ceiling to ONE finding. R2 (CORS) is checked first and, when it
/// matches, is AUTHORITATIVE for that finding — it overrides whatever the D5 floor already did
/// (including a floor-critical), because a browser-mediated CORS misconfiguration is never the
/// "direct unauthenticated exposure" class the floor polices, even if the finding's prose happens
/// to also brush against floor vocabulary. A finding that does NOT mention CORS is completely
/// untouched by this branch, so a genuinely unauthenticated-exposure finding keeps the floor's
/// Critical rating unchanged.
///
/// R3 (redirect) and R4 (client-only privilege gate) are checked next, each gated against the D5
/// unauthenticated/full-compromise floor phrases first (see the D7 section doc comment for why —
/// unlike CORS, these two CAN legitimately co-occur with a floor-earned Critical in the same
/// finding's prose, and must never demote it). R3 is a full bidirectional clamp like R2; R4 is
/// cap-only like R1. R1 is checked last.
pub(super) fn apply_severity_ceiling_rule(f: Finding) -> Finding {
    let before = f.severity.clone();
    let mut out = apply_severity_ceiling_rule_inner(f);
    if out.severity != before {
        let why = out
            .calibration_rationale
            .clone()
            .unwrap_or_else(|| "a severity ceiling applied".to_string());
        let after = out.severity.clone();
        record_severity_adjustment(&mut out, &before, &after, &why);
    }
    out
}

/// The text of `f` as its PRODUCER wrote it: `detail` up to the first calibration marker
/// (`[calibrated: ...]` / `[needs review: ...]`, which `apply_verdicts` appends and which carry
/// the calibration model's own one-line reason), plus `snippet` and `category`, lowercased. R1
/// reads this, so a reason such as "confirm whether" added by the calibration step is not taken
/// for the finding's own hedge (H7).
pub(super) fn producer_text(f: &Finding) -> String {
    let cut = ["[calibrated", "[needs review"]
        .iter()
        .filter_map(|m| f.detail.find(m))
        .min()
        .unwrap_or(f.detail.len());
    format!(
        "{} {} {}",
        &f.detail[..cut],
        f.snippet,
        f.category.as_deref().unwrap_or("")
    )
    .to_ascii_lowercase()
}

/// Whether `f` names the CORS class itself: a rule id with a `CORS` word (or `CROSS-ORIGIN`), or
/// a category naming CORS. R2 clamps only these (M1); CORS vocabulary in the prose of an
/// unrelated finding no longer moves its severity.
pub(super) fn names_cors_class(f: &Finding) -> bool {
    let id = f.rule_id.to_ascii_uppercase();
    id.split(['-', '_']).any(|w| w == "CORS")
        || id.contains("CROSS-ORIGIN")
        || f.category
            .as_deref()
            .is_some_and(|c| c.to_ascii_lowercase().contains("cors"))
}

pub(super) fn apply_severity_ceiling_rule_inner(mut f: Finding) -> Finding {
    // W31 — checked FIRST, and cap-only.
    //
    // A row the engine flagged as possibly deliberate cannot be told apart from a deliberate
    // choice by reading code: nothing in a repository separates a catalog-photo bucket that
    // is public on purpose from a user-document bucket that is public by accident. The
    // engine already knew that and said so in `detail` prose, where no severity pass, ranker
    // or renderer can read it — so three such rows shipped at HIGH, held, each carrying text
    // undercutting its own severity.
    //
    // The cap removes that contradiction by construction instead of asking a renderer to
    // notice it. It runs before every other rule here because it is a statement about
    // CONFIDENCE rather than about the fault's class, and a confidence cap that could be
    // overridden by a class-based ceiling would not be a cap.
    //
    // Cap, never clamp: an `info` row stays `info`. Raising the severity of a row we are
    // explicitly unsure about would be inventing certainty in the other direction. The
    // authored severity is not lost — a curator who marks the row a real violation restores
    // it, which is the point of keeping the engine's read and the curator's decision on
    // separate axes.
    if f.could_be_intentional && severity_rank(&f.severity) > severity_rank("low") {
        let was = f.severity.clone();
        f.severity = "low".to_string();
        f.calibration_rationale = Some(format!(
            "Severity cap: this state may be intentional, and the repository alone cannot \
             show whether it is. Capped from {was} to low pending a reviewer's call; it \
             returns to {was} if confirmed as a real violation."
        ));
        return f;
    }
    // D8 (owner-mandated opinion-class cap): the ARCH-OBSERVABILITY-* convention class
    // (a swallowed error logged nowhere, inconsistent unstructured logging alongside an
    // adopted structured logger, missing correlation/request id propagation) is SEMANTIC
    // and repo-relative by the owner's own framing — never a universal defect like the R1-R4
    // classes below, which all derive from the finding's TEXT. This one derives from the
    // finding's RULE IDENTITY instead (`crate::is_observability_convention_class`), checked
    // FIRST and unconditionally: it is authoritative over every other ceiling/floor result,
    // clamping to exactly Low bidirectionally (like R2's CORS clamp, not a cap-only ceiling
    // like R1/R4) — an inflated Critical/High from this class is exactly as wrong as a
    // silently-buried Info, since both misrepresent an opinionated convention. Capping the
    // severity STRING here is only HALF of the structural guarantee the owner asked for; the
    // other half — keeping this class out of every action bucket regardless of severity — is
    // `report_export::is_informational`'s matching rule-identity check, which holds even if
    // this pass somehow did not run first. See `crate::is_observability_convention_class`'s
    // doc comment for the full two-part design.
    if crate::is_observability_convention_class(&f.rule_id) {
        f.severity = "low".to_string();
        f.calibration_rationale = Some(
            "Severity ceiling: this is the observability/under-logging convention class \
             (a swallowed error logged nowhere, inconsistent unstructured logging alongside \
             an adopted structured logger, or missing correlation/request id propagation) — \
             a repo-relative judgment call the owner designated as never a universal defect. \
             Capped to exactly Low and routed out of every action bucket, regardless of any \
             other calibrated severity."
                .to_string(),
        );
        return f;
    }

    // W24 severity anchor: a test-coverage rule is never CRITICAL.
    //
    // Critical in this report means an unauthenticated exposure or a full compromise. A
    // missing test is real work and belongs in the findings, but it was never the thing an
    // attacker exploited, and ranking it beside "anyone can read every row" spends the
    // client's alarm on the wrong row. Cap-only at high, so a genuinely serious gap still
    // outranks ordinary debt; the anchor only removes the top rung.
    if crate::is_test_coverage_class(&f.rule_id) && f.severity == "critical" {
        f.severity = "high".to_string();
        f.calibration_rationale = Some(
            "Severity ceiling: this is a test-coverage rule. Critical is reserved for an \
             unauthenticated exposure or a full compromise; missing coverage is real work \
             but was never the exploited thing, so it is capped at High rather than ranked \
             beside a live exposure."
                .to_string(),
        );
        return f;
    }

    let text = calibration_floor_scan_text(&f);
    let floor_earned_critical = mentions_unauthenticated_or_full_compromise(&text);

    // R2 runs after the floor check and only for a finding that NAMES the CORS class (M1): a
    // floor-earned critical is never clamped, and CORS words in another rule's prose never
    // move its rating.
    if names_cors_class(&f) && !floor_earned_critical && mentions_cors_misconfig(&text) {
        f.severity = "medium".to_string();
        f.calibration_rationale = Some(
            "Severity ceiling: a browser-mediated CORS/cross-origin misconfiguration requires a \
             victim's browser and an active session to exploit — it is neither a direct \
             unauthenticated exposure (Critical) nor safe to leave buried (Low). Calibrated to \
             exactly Medium."
                .to_string(),
        );
        return f;
    }

    if mentions_open_redirect(&text) && !floor_earned_critical {
        if mentions_redirect_chained_to_credential_capture(&text) {
            f.severity = "high".to_string();
            f.calibration_rationale = Some(
                "Severity ceiling: an open/unvalidated redirect chained to credential capture — \
                 the destination can read or exfiltrate a token, cookie, or credential — lets an \
                 attacker walk away with something usable. Calibrated to exactly High."
                    .to_string(),
            );
        } else {
            f.severity = "medium".to_string();
            f.calibration_rationale = Some(
                "Severity ceiling: an open/unvalidated redirect, on its own, only harms the user \
                 who follows it and hands the attacker nothing — not High or Critical unless \
                 chained to credential capture. Calibrated to exactly Medium."
                    .to_string(),
            );
        }
        return f;
    }

    if mentions_client_only_privilege_gate(&text) && !floor_earned_critical {
        if severity_rank(&f.severity) > severity_rank("high") {
            f.severity = "high".to_string();
        }
        f.calibration_rationale = Some(
            "Severity ceiling: a privileged/role check enforced only in client-visible (UI/\
             browser) code, with no independent server-side re-check, is never by itself the \
             basis for Critical — the client-side control is not the security boundary. An \
             authenticated privilege escalation confined to a single resource/operation is High \
             at most; Critical is reserved for an unauthenticated reach or a system- or \
             account-wide compromise. Calibrated to High at most."
                .to_string(),
        );
    }

    // R1 reads the producer's own words only and never caps a floor-earned critical (H7): an
    // unauthenticated delete whose calibration reason says "confirm whether" stays critical.
    if !floor_earned_critical && mentions_self_hedged_confirmation_only(&producer_text(&f)) {
        f.confidence = Some("needs-review".to_string());
        f.needs_review = true;
        if severity_rank(&f.severity) > severity_rank("low") {
            f.severity = "low".to_string();
        }
        f.calibration_rationale = Some(
            "Severity ceiling: the finding's own analysis concludes the pattern is likely \
             intentional / confirmation-only — capped to Low and flagged needs-review rather \
             than left in an action bucket. It stays in the queue for a human to confirm."
                .to_string(),
        );
    }

    f
}

/// Apply the R1-R4 severity ceiling to a whole finding set — run immediately after
/// `apply_severity_calibration_rules` (the D5 floor) everywhere that runs. Safe over ANY finding
/// set (deterministic-floor findings included), since all four ceiling rules derive entirely
/// from the finding's own text rather than the model's verdict.
pub fn apply_severity_ceiling_rules(findings: Vec<Finding>) -> Vec<Finding> {
    findings
        .into_iter()
        .map(apply_severity_ceiling_rule)
        .collect()
}
