//! Stage 1 calibration, the per-site first reviewer (review tree design 4.1).
//!
//! Extracted from `ai_audit.rs` unchanged; the stage 1 rewrite follows in later commits.

use super::*;

/// The system prompt for the calibration pass. This pass does NOT drop findings — it
/// recalibrates severity for the app's real context and flags low-confidence ones. An
/// earlier version was a skeptic that REFUTED (dropped) findings, but it never receives
/// the code (see `verify_findings`), so it was guessing — and dropping real, low-impact
/// violations the architect wanted to see was a direct cause of "the audit missed
/// obvious violations." Discovery is recall-first; the human triages, the tool does not
/// pre-censor.
pub fn verify_system_prompt() -> String {
    r#"You are calibrating an automated audit's findings before a human architect triages them.
You do NOT decide whether to KEEP a finding — the architect does. Every finding is kept.

Judge each finding ON ITS OWN MERITS. The model that scanned the code may have been confident
or assertive — that does NOT carry over. Calibration is where humility lives: a higher-tier
scan tends to over-assert on debatable points, and your job is to put the nuance back.

For EACH finding, do two things:
- Assign a CALIBRATED severity (critical/high/medium/low) for this app's real-world context,
  using this rubric:
  * "critical" is RARE. Only assign it when the finding clears ALL of: concretely (not
    theoretically) exploitable, AND a competent attacker or a single bad input reaches serious
    impact quickly. Qualifying classes: unauthenticated/unauthorized access to a privileged,
    destructive, or financial operation; a credential/secret exposed to clients or committed to
    the repo; PII or equivalently sensitive data readable/writable with no access control on an
    internet-exposed surface; remote code execution or injection with a real, reachable path.
    When in doubt between "high" and "critical", use "high" — do not inflate an ordinary high
    into critical out of general alarm, and never assign "critical" to a debatable preference.
  * RULES OF THUMB (apply these consistently — the SAME shape of finding must get the SAME
    severity every run, not a per-run judgment call):
    - Unauthenticated access to ANY protected resource or operation, OR a path to full-account
      compromise (takeover, impersonation, assuming any user's identity) — ALWAYS "critical".
    - An AUTHENTICATED user reading another tenant's/user's data they should not see (cross-
      tenant read) is "high" by default — NOT "critical" — unless the data class itself escalates
      it: payment data, credentials/secrets, or PII of the sensitive kind (SSN, health records,
      government ID). Only then does a cross-tenant READ clear the critical bar.
    - Do NOT downgrade an access-control or injection finding because "RLS probably covers it" or
      similar defense-in-depth speculation — a check failing at the boundary the finding actually
      names is real regardless of what a deeper layer might also do. If you invoke RLS (or any
      other layer) as a mitigating factor, it must be a CONFIRMED fact from the evidence you were
      given, not a guess, and it does not excuse a boundary that has already failed.
    - An open or unvalidated redirect (the destination is attacker-controlled or otherwise
      unchecked) is "medium" by default, NOT "high" — on its own it only harms the user who
      follows it and hands the attacker nothing. It becomes "high" exactly when the finding says
      the redirect is CHAINED to credential capture (the destination can read or exfiltrate a
      token, cookie, or credential) — then the attacker walks away with something usable. It is
      never "critical" from the redirect mechanism alone, chained or not — that would require an
      UNAUTHENTICATED path to a privileged operation, which is a different finding.
    - A privileged/role/admin check enforced ONLY in client-visible (UI/browser) code, with no
      independent check on the server, is "high" AT MOST, never "critical" — the client-side
      control is not the security boundary; a client can always be made to skip what only the
      client enforces. Judge it by what the SERVER allows an authenticated caller to do once that
      gate is bypassed. An authenticated privilege escalation confined to a single resource or
      operation is "high"; "critical" is reserved for an unauthenticated reach or a system- or
      account-wide compromise, neither of which a client-side-only gate states by itself.
  * A concrete, demonstrable SECURITY or CORRECTNESS break that does NOT clear the critical bar
    above (injection needing extra steps to reach, missing auth on a write path with contained
    blast radius, data loss/corruption, a real but non-catastrophic exploit) is "high".
  * A DEBATABLE ARCHITECTURAL PREFERENCE — a "valid pattern but not the one this rule prefers"
    call, a layering/structure/abstraction opinion, an over-engineering/YAGNI note on a small
    codebase, a stylistic or convention preference — is NEVER "critical" or "high". Cap it at
    "medium", usually "low". These are preferences a reasonable team could disagree on, not
    violations.
  * A real but low-impact issue is "low", not removed.
- Set confidence: "low" when the finding is a debatable preference (per above), is theoretical,
  is likely over-flagged, or you cannot tell it is real without seeing more code; "high" only
  for clear, concrete violations. Confidence "low" flags the finding for the architect's review —
  it is ADVICE, not a deletion. When in doubt between a violation and a preference, treat it as a
  preference: low confidence, capped severity.
- Estimate remediation EFFORT for a developer to fix this ONE finding, given only what you can
  see (the path/snippet/detail) — "low" (a one-line/local change: rename, add a check, swap a
  call), "medium" (touches a few call sites or needs a small new helper/test), or "high" (a
  structural change: new abstraction, cross-file rework, a migration). When you cannot tell,
  default to "medium" rather than guessing an extreme.
- Assign a semantic CATEGORY from this closed set (pick the single best fit; omit the field if
  none clearly applies): authorization, authentication, secret-exposure, injection,
  transport-security, rls-policy, resource-exposure, input-validation, error-handling,
  arch-conformance, testing-style, performance. This groups the same defect flagged under
  different rule names; it does NOT change severity.

Do NOT deduplicate, and do NOT cross-reference other findings — no "same as [N]", "duplicate
of [N]", "as index N", "index N", "row N", or ANY pointer to another finding by index/row.
Deduplication already happened upstream; your `reason` is one line about THIS finding's
severity/confidence only, with no reference to any other finding.

A finding whose rule id you don't recognize as a named convention is NOT automatically less
credible — it is the audit's BACKSTOP catching a genuine issue that no named rule happened to
cover. Judge it purely on the evidence in its own `detail`/`snippet`, exactly like any other
finding: do not discount its severity or confidence for being unnamed, and do not inflate it
either. The absence of a rule id is not evidence of anything.

Return ONLY JSON, no prose:
{"verdicts":[{"index":0,"severity":"critical|high|medium|low","confidence":"high|low","effort":"low|medium|high","category":"authorization|authentication|secret-exposure|injection|transport-security|rls-policy|resource-exposure|input-validation|error-handling|arch-conformance|testing-style|performance","reason":"one line"}]}
One verdict per finding, addressed by its [index]."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// Remove cross-finding dedup pointers like "same as [6]" / "duplicate of [10]" (and a
/// trailing bare "[12]") from a calibration reason, case-insensitively, then tidy leftover
/// separators. These indices are batch-local and unreliable; the merge relationship is
/// already structural (rule_id + path + line + also_matches), so the prose is pure noise.
pub(super) fn strip_dedup_pointers(reason: &str) -> String {
    // Char-indexed (UTF-8 safe). ASCII-lowercase per char keeps a 1:1 index alignment.
    let chars: Vec<char> = reason.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let starts = |at: usize, pat: &str| -> bool {
        let pc: Vec<char> = pat.chars().collect();
        at + pc.len() <= lower.len() && lower[at..at + pc.len()] == pc[..]
    };
    // After a phrase, consume optional separators + a pointer token ([..] | #N | N).
    // Returns Some(end) when a number token was consumed, else None.
    let skip_number = |from: usize| -> Option<usize> {
        let mut j = from;
        while j < chars.len() && matches!(chars[j], ' ' | '#' | ':') {
            j += 1;
        }
        if j < chars.len() && chars[j] == '[' {
            let mut k = j + 1;
            while k < chars.len() && chars[k] != ']' {
                k += 1;
            }
            return (k < chars.len() && k > j + 1).then_some(k + 1);
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            return Some(j);
        }
        None
    };
    // (phrase, requires_a_following_number). The `index`/`row` family REQUIRES a number so
    // legitimate prose ("add an index on (a, b)") is untouched while pointers ("as index 6",
    // "index 9", "row 3") are stripped. The same-as/duplicate family is always a pointer.
    let patterns: &[(&str, bool)] = &[
        ("same as", false),
        ("duplicate of", false),
        ("duplicates", false),
        ("dup of", false),
        ("as index", true),
        ("see index", true),
        ("cf index", true),
        ("index", true),
        ("row", true),
    ];
    let mut keep = String::with_capacity(reason.len());
    let mut i = 0;
    while i < chars.len() {
        let mut next = None;
        for (pat, needs_num) in patterns {
            if starts(i, pat) {
                let after = i + pat.chars().count();
                if *needs_num {
                    if let Some(end) = skip_number(after) {
                        next = Some(end);
                        break;
                    }
                    // phrase present but no number -> not a pointer; try other patterns
                } else {
                    next = Some(skip_number(after).unwrap_or(after));
                    break;
                }
            }
        }
        match next {
            Some(end) => i = end,
            None => {
                keep.push(chars[i]);
                i += 1;
            }
        }
    }
    // Tidy: collapse double spaces and strip leftover leading/trailing separators.
    let tidied = keep.replace("  ", " ");
    tidied
        .trim()
        .trim_matches(|c: char| matches!(c, ';' | ',' | '.' | '-' | ':') || c.is_whitespace())
        .to_string()
}

/// Phrases that mark a finding's own text (its authored `detail`/`snippet`) or the
/// calibration model's own verdict reason as recording an EXPLOIT PRECONDITION — some
/// additional fact that must hold (forging a cookie, holding another credential, a specific
/// attacker capability) before the vulnerability is actually reachable, as opposed to an
/// unconditional exposure exploitable as-is. Deliberately phrase-based and conservative
/// (clear precondition phrasing, not any use of the word "if") — a finding that merely
/// contains "if" somewhere in an unrelated sentence must not trip this.
///
/// CRITICAL GUARDRAIL (calibration review, the cycle-2 queue-hardening notes,
/// extended cycle-5): this predicate is used ONLY to (a) block an upward calibration move in
/// [`apply_verdicts`], (b) cap the deterministic D5 floor's critical escalation in
/// [`severity_calibration_floor`], and (c) break ties in the "do now" ranking in
/// `report_export`. It must NEVER be used to LOWER a severity — a genuine critical whose
/// justification merely mentions a mitigation must never be buried by this logic. See the call
/// sites' own doc comments for the never-demote invariant.
pub(super) const EXPLOIT_PRECONDITION_PHRASES: &[&str] = &[
    "requires ",
    "contingent on",
    "depends on",
    "only if",
    "assuming ",
    "provided that",
    "an attacker must",
];

/// Negation guard for the "requires " phrase specifically: "requires no authentication" (or
/// "requires no login" — already its own floor phrase, see
/// [`UNAUTHENTICATED_OR_FULL_COMPROMISE_PHRASES`]) NEGATES the requirement. It describes an
/// UNCONDITIONAL exposure — no credential needed at all — which is the semantic OPPOSITE of a
/// precondition that gates exploitation behind forging/holding something. Without this guard, a
/// genuinely unauthenticated finding phrased as "the endpoint requires no authentication" would
/// be misread as recording a precondition and incorrectly capped away from Critical. Scoped to
/// "requires " because none of the other phrases above share this negation ambiguity.
pub(super) const REQUIRES_NEGATION_SUFFIX: &str = "no ";

/// Whether `lower` (already-lowercased) contains the "requires " precondition phrase in a
/// NON-negated form — i.e. an occurrence of "requires " not immediately followed by "no "
/// ([`REQUIRES_NEGATION_SUFFIX`]). Scans every occurrence (not just the first) so text with both
/// a negated occurrence ("...requires no login...") and a genuine one elsewhere ("...requires
/// forging a session cookie...") still registers the genuine one.
pub(super) fn contains_non_negated_requires(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(pos) = rest.find("requires ") {
        let after = &rest[pos + "requires ".len()..];
        if !after.starts_with(REQUIRES_NEGATION_SUFFIX) {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether `text` records an exploit precondition per [`EXPLOIT_PRECONDITION_PHRASES`].
/// Case-insensitive; lowercases internally so every caller (the calibration passes here and
/// `report_export`'s ranking tie-break) can pass raw, unprocessed text. The "requires " phrase
/// is negation-guarded (see [`contains_non_negated_requires`]) so "requires no authentication"
/// reads as unconditional, not as a precondition.
pub(crate) fn mentions_exploit_precondition(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if contains_non_negated_requires(&lower) {
        return true;
    }
    EXPLOIT_PRECONDITION_PHRASES
        .iter()
        .filter(|p| **p != "requires ")
        .any(|p| lower.contains(*p))
}

/// Apply the calibration verdicts: recalibrate severity and annotate confidence/reason.
/// NEVER drops a finding — recall-first discovery hands every finding to the architect.
/// Robust: unparseable verdicts keep all findings as-is.
///
/// UPWARD-CALIBRATION GUARDRAIL: when applying the model's verdict would RAISE a finding's
/// severity above its authored/base severity (the severity it carried entering this call)
/// AND the finding's own text or the verdict's own reason records an exploit precondition
/// (see [`mentions_exploit_precondition`]), the raise is refused and the base severity is
/// kept — a High finding whose note says exploitation "requires forging a session cookie"
/// does not get waved through to Critical and take a do-now slot from an unconditional
/// critical. This can ONLY hold severity at its base value or let the raise through; it
/// never lowers a severity below what it already was (an explicit downgrade verdict with no
/// bearing on this guardrail continues to apply exactly as before).
pub fn apply_verdicts(raw: &str, findings: Vec<Finding>) -> Vec<Finding> {
    // A reply that parses whole is read whole. One that does not (cut off at the output limit
    // mid-verdict) still holds every verdict that was written out in full: those are read one by
    // one ([`answered_verdicts`]) and the findings it never reached keep what they had.
    let whole = extract_json_object(raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .filter(|v| v["verdicts"].is_array());
    let v = match whole {
        Some(v) => v,
        None => {
            let salvaged = answered_verdicts(raw, findings.len());
            if salvaged.is_empty() {
                return findings;
            }
            serde_json::json!({ "verdicts": salvaged })
        }
    };
    let Some(arr) = v["verdicts"].as_array() else {
        return findings;
    };
    let mut out = Vec::new();
    for (i, mut f) in findings.into_iter().enumerate() {
        if let Some(verdict) = arr.iter().find(|x| x["index"].as_u64() == Some(i as u64)) {
            if let Some(sev) = verdict["severity"].as_str() {
                // An unrecognised label keeps the finding's current severity (M3): it must not
                // be read as a verdict of "medium".
                let candidate = parse_model_severity(sev, &f.severity);
                let candidate = candidate.as_str();
                // Raising = moving to a MORE severe label. This module's local
                // `severity_rank` ranks `critical` highest (4) down to unrecognized/`low`
                // (0/1), so "raise" means the candidate's rank EXCEEDS the base severity's.
                // The base severity is `f.severity` as it enters this branch — untouched so
                // far this iteration.
                let would_raise = severity_rank(candidate) > severity_rank(&f.severity);
                let raw_reason = verdict["reason"].as_str().unwrap_or("");
                let precondition_text = format!("{} {} {raw_reason}", f.detail, f.snippet);
                if would_raise && mentions_exploit_precondition(&precondition_text) {
                    // BLOCKED: keep the authored/base severity. Never demotes — a verdict
                    // that does not propose a raise (equal or a genuine downgrade) is
                    // untouched by this branch and falls through to the assignment below
                    // exactly as before.
                    f.calibration_rationale = Some(format!(
                        "Calibration held at {} (the model proposed {candidate}): the \
                         finding's own text or the model's own reasoning records an exploit \
                         precondition, which can only block an upward calibration, never \
                         justify one.",
                        f.severity
                    ));
                } else if candidate != f.severity {
                    let before = std::mem::replace(&mut f.severity, candidate.to_string());
                    let why = strip_dedup_pointers(raw_reason.trim());
                    let why = camerata_secrets::redact_str(&why).into_owned();
                    let why = if why.trim().is_empty() {
                        "the calibration step's verdict".to_string()
                    } else {
                        format!("the calibration step's verdict: {}", why.trim())
                    };
                    record_severity_adjustment(&mut f, &before, candidate, &why);
                }
            }
            let low_conf = verdict["confidence"].as_str() == Some("low");
            // Structured confidence (Part 1 §3): promoted out of the string-embedded
            // `[needs review: reason]` detail suffix below. `needs_review` is also set
            // here — previously calibration never touched it for AI findings (only
            // `classify_repo_findings`'s in_test path did), so the UI's structured
            // "needs review" filter silently missed every AI-flagged finding.
            //
            // C4-P2: `needs_review` is set UNCONDITIONALLY from `low_conf` (never only on the
            // `true` branch) — a finding that enters this call already carrying a stale
            // `needs_review = true` (e.g. a prior THOROUGH-mode consensus vote, or a verdict
            // re-application) must have it CLEARED when this verdict says `high`, or the
            // exported `confidence`/`needs_review` fields silently desync: confidence says
            // "high" but the structured flag still reads hedged. The two fields are a
            // biconditional (`confidence == "needs-review"` iff `needs_review`) by construction
            // here — see `report_export::is_hedged`, which additionally self-heals any
            // desync this guard misses (a belt-and-suspenders defense, not a substitute for it).
            f.confidence = Some(if low_conf { "needs-review" } else { "high" }.to_string());
            f.needs_review = low_conf;
            // Structured effort (Part 1 §3): the calibration verdict schema now emits it
            // alongside severity/confidence. Only accept the three known values — a
            // mis-shaped or missing field leaves `effort` at its prior value (None for a
            // fresh AI finding) rather than recording a guess.
            if let Some(eff) = verdict["effort"].as_str() {
                if matches!(eff, "low" | "medium" | "high") {
                    f.effort = Some(eff.to_string());
                }
            }
            // Semantic category (Bug 3): the calibration verdict may classify the finding into
            // the closed taxonomy that drives the cross-family merge pass. Only a KNOWN value is
            // accepted; anything mis-shaped/absent leaves `category` for the rule-id heuristic in
            // `group_semantic` to fill (fail-through, per design §1a step 3).
            if let Some(cat) = verdict["category"].as_str() {
                if is_known_category(cat) {
                    f.category = Some(cat.to_string());
                }
            }
            // Strip any cross-finding dedup pointers ("same as [6]", "duplicate of [10]") the
            // model still volunteers: the indices are batch-local and wrong, the relationship
            // is already encoded structurally (rule_id + path + line + also_matches), and a
            // wrong English pointer in a data cell is worse than none — it ships in the CSV.
            let reason = strip_dedup_pointers(verdict["reason"].as_str().unwrap_or("").trim());
            // The calibration model's reason is appended to `detail`; redact it first.
            let reason = camerata_secrets::redact_str(&reason).into_owned();
            let reason = reason.trim();
            if low_conf || !reason.is_empty() {
                let tag = if low_conf {
                    "needs review"
                } else {
                    "calibrated"
                };
                // The `[needs review]`/`[calibrated]` detail tag is KEPT for one release for UI
                // back-compat (`split_needs_review`, ui-core/src/rules.rs still regex-parses
                // it) — the structured `confidence`/`needs_review` fields above are additive,
                // not a replacement, until the UI reads them directly.
                f.detail = if reason.is_empty() {
                    format!("{} [{tag}]", f.detail)
                } else {
                    format!("{} [{tag}: {reason}]", f.detail)
                };
            }
        }
        out.push(f); // never dropped
    }
    out
}

/// Per-finding token estimate for the CALIBRATION pass's response — each verdict is a
/// compact JSON object (`{"index","severity","confidence","reason"}`) with a one-sentence
/// `reason`, so a modest per-item budget covers it comfortably.
pub(super) const VERIFY_TOKENS_PER_FINDING: u32 = 150;

/// How many findings fit in ONE calibration call. Chosen so a FULL chunk already needs
/// more than [`AGGREGATE_MAX_TOKENS_FLOOR`] (`CALIBRATION_BATCH_SIZE *
/// VERIFY_TOKENS_PER_FINDING` > 4096) — the exact shape of the truncation bug this fixes —
/// while staying well short of [`AGGREGATE_MAX_TOKENS_CEILING`]. A chunk whose call fails
/// or times out passes through uncalibrated (see the `votes.is_empty()` branch below) —
/// NEVER dropped — so a smaller chunk size only costs calibration precision on failure,
/// never completeness.
pub(super) const CALIBRATION_BATCH_SIZE: usize = 40;

/// `max_tokens` for one calibration call over `n_findings` findings: the per-verdict text budget
/// plus the calibration stage's thinking allowance.
pub(crate) fn calibration_max_tokens(n_findings: usize) -> u32 {
    staged_max_tokens(
        n_findings,
        VERIFY_TOKENS_PER_FINDING,
        camerata_ui_core::scan::CALIBRATION_EFFORT,
    )
}

/// The verdict objects a calibration reply holds IN FULL, one per finding index below `n` (the
/// first of a repeated index wins), read one by one with [`complete_array_objects`]. The last,
/// unfinished object of a reply cut off at the output limit is not guessed at. Empty for a reply
/// that holds no `verdicts` array.
pub(super) fn answered_verdicts(raw: &str, n: usize) -> Vec<serde_json::Value> {
    let mut seen = std::collections::BTreeSet::new();
    complete_array_objects(raw, "verdicts")
        .into_iter()
        .filter_map(|object| serde_json::from_str::<serde_json::Value>(object).ok())
        .filter(|v| {
            v["index"]
                .as_u64()
                .is_some_and(|i| (i as usize) < n && seen.insert(i))
        })
        .collect()
}

/// The first calibration call over a chunk: its reply and the `max_tokens` it was sent with.
struct CalibrationCall<'a> {
    reply: &'a crate::llm::LlmResponse,
    cap: u32,
}

/// A calibration reply cut off at the output limit, made whole: the verdicts it wrote out in full,
/// then ONE re-ask of just the findings it did not reach (the same prompt over those findings
/// alone, a smaller request with its own cap), merged back under the original indices as
/// `{"verdicts":[...]}`. A reply that is not cut off is `first.text` unchanged. The re-ask is never
/// the same request again: when the reply answered NOTHING the findings are the same, so the
/// re-ask raises the cap to twice `first_cap` (the cap the first call was sent with) within the
/// runaway ceiling, as the scan's truncation retry does; a first cap already at the ceiling has
/// nothing to change, and then no re-ask is sent.
///
/// The cut-off call is recorded as wasted by the share of the findings it left unanswered (all of
/// it when it answered none). The re-ask goes through the spend limit like any retry, and a re-ask
/// that fails or is itself cut off keeps what it did answer: the findings still unanswered pass
/// through uncalibrated, as they always did, and the stage never drops one.
async fn reask_unanswered_verdicts(
    llm: &dyn LlmPort,
    meter: Option<&UsageMeter>,
    calibration_model: Option<&str>,
    system: &str,
    chunk: &[Finding],
    first: CalibrationCall<'_>,
    prompt_for: &(dyn Fn(&[&Finding]) -> String + Sync),
) -> String {
    let CalibrationCall {
        reply: first,
        cap: first_cap,
    } = first;
    if !first.truncated {
        return first.text.clone();
    }
    let n = chunk.len();
    let mut answered = answered_verdicts(&first.text, n);
    let have: std::collections::BTreeSet<usize> = answered
        .iter()
        .filter_map(|v| v["index"].as_u64().map(|i| i as usize))
        .collect();
    let missing: Vec<usize> = (0..n).filter(|i| !have.contains(i)).collect();
    if let Some(m) = meter {
        if answered.is_empty() {
            m.record_wasted(Stage::Calibration, first);
        } else if !missing.is_empty() {
            m.record_wasted_share(
                Stage::Calibration,
                first,
                missing.len() as f64 / n.max(1) as f64,
            );
        }
    }
    // The re-ask's cap: sized for the findings left when some were answered (a smaller, different
    // request); twice the first cap when none were (the same findings, so the cap is what changes).
    let reask_cap = if answered.is_empty() {
        let raised = first_cap
            .saturating_mul(2)
            .min(AGGREGATE_MAX_TOKENS_CEILING);
        (raised > first_cap).then_some(raised)
    } else {
        Some(calibration_max_tokens(missing.len()))
    };
    let reask_cap =
        reask_cap.filter(|_| !missing.is_empty() && !meter.is_some_and(UsageMeter::retry_blocked));
    if let Some(cap) = reask_cap {
        let items: Vec<&Finding> = missing.iter().map(|&i| &chunk[i]).collect();
        let req = with_stage_effort(
            LlmRequest::new(prompt_for(&items))
                .with_system(system.to_string())
                .with_max_tokens(cap)
                .with_temperature(AUDIT_TEMPERATURE)
                .with_response_schema(reply_schemas::calibration_reply_schema(items.len()))
                .with_model(stage_model(calibration_model)),
            Stage::Calibration,
        );
        note_request(meter, &req);
        let mut on_delta = |_: &str| {};
        match llm.complete_streaming(req, &mut on_delta).await {
            Ok(resp) => {
                if let Some(m) = meter {
                    m.record_stage(Stage::Calibration, &resp);
                }
                let got = answered_verdicts(&resp.text, items.len());
                if resp.truncated {
                    if let Some(m) = meter {
                        if got.is_empty() {
                            m.record_wasted(Stage::Calibration, &resp);
                        } else if got.len() < items.len() {
                            m.record_wasted_share(
                                Stage::Calibration,
                                &resp,
                                (items.len() - got.len()) as f64 / items.len() as f64,
                            );
                        }
                    }
                }
                for mut verdict in got {
                    // The re-ask numbered its findings from 0: put back the originals' indices.
                    let local = verdict["index"].as_u64().unwrap_or(0) as usize;
                    verdict["index"] = serde_json::json!(missing[local]);
                    answered.push(verdict);
                }
            }
            Err(e) => meter_failed(meter, Stage::Calibration, &e),
        }
    }
    serde_json::json!({ "verdicts": answered }).to_string()
}

/// Run the skeptic pass over a repo's AI findings (a fresh, reasoning-based perspective —
/// deliberately NOT re-sent the whole digest, so it judges exploitability/context, not
/// code minutiae). Graceful: on any model failure the findings pass through unchanged.
///
/// `feedback`, when present, records this pass into the transcript the same way the scan
/// passes do (`audit_pass`/`run_prose_lens`): register the agent with its REAL generated
/// prompt up front, append each vote's raw response as it lands, then set a terminal
/// status. This is what makes the cockpit's "calibrating N findings" entry show an actual
/// prompt/output instead of "no output captured" — a transcript-recording failure (lock
/// poisoning etc., handled inside `TranscriptStore` itself) can never affect calibration,
/// which stays best-effort exactly as before.
///
/// LARGE finding sets are split into [`CALIBRATION_BATCH_SIZE`]-sized chunks, each its own
/// streamed call (see the "AGGREGATE-PASS TUNING" section above) — a chunk that fails only
/// loses ITS findings' calibration (they pass through uncalibrated, never dropped), not the
/// whole scan's. The single-chunk case (the common one) is byte-for-byte the same prompt
/// and transcript session id this function always used.
///
/// C3-1b: this pass is also the ONLY source of the `effort` (hour-estimation) field, so it
/// returns a second `Vec<FailedPass>` alongside the (never-dropped) findings — a single
/// `FailedPass{pass: "hour estimation", ..}` entry when the pass produced NO usable effort
/// estimate for ANY finding in this call (every chunk's every attempt failed, or every
/// response came back without a parseable `effort`), so the export can disclose the gap
/// explicitly (`report_export::failed_pass_disclosures`) instead of silently rendering every
/// row as if "no estimate" were a settled answer. A PARTIAL failure (some findings estimated,
/// some not) is not disclosed here — only a genuine run-wide miss — the per-finding rendering
/// (`effort: None` -> "not estimated this run" in the Typst template) already covers the
/// individual-row case honestly.
#[allow(clippy::too_many_arguments)]
pub async fn verify_findings(
    llm: &dyn LlmPort,
    repo: &str,
    findings: Vec<Finding>,
    calibration_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
    thorough: bool,
    // One repo-shape sentence — detected stack + code-file count (e.g. "This is a Next.js repo
    // with 42 code files.") — computed by the caller from signals already in hand. Gives the
    // model real context to hedge stance/YAGNI findings, instead of a hardcoded size threshold
    // deciding whether a rule fires. Empty string is fine (the paragraph still reads).
    repo_shape: &str,
) -> (Vec<Finding>, Vec<FailedPass>) {
    if findings.is_empty() {
        return (findings, Vec::new());
    }
    // One delimiter token for every calibration batch of this call (see `crate::prompt_frame`),
    // drawn so that it occurs in none of the evidence it will frame.
    let evidence = finding_evidence_texts(&findings);
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding(
        &evidence.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let system = crate::prompt_frame::with_untrusted_clause(verify_system_prompt(), &nonce);
    // The calibration prompt for `items`, labelled `[0]`, `[1]`, ... in the order given. The whole
    // chunk is sent with its own indices; a re-ask of the findings a cut-off reply did not
    // answer ([`reask_unanswered_verdicts`]) is the same prompt over just those findings.
    let calibration_prompt = |items: &[&Finding]| -> String {
        let mut prompt = format!("Repository: {repo}\n");
        // Proportionality signal (#51, Bug 4 §2b): ALWAYS given now (was thorough-only) — a
        // small/young codebase must not be held to the architecture of a large one, and this
        // feeds the informational bucketing of stance/YAGNI notes downstream. Over-engineering
        // / YAGNI notes auto-hedge to low confidence + capped severity.
        prompt.push_str(&format!(
            "{repo_shape} Judge each finding PROPORTIONALLY to the codebase's size and maturity: \
             an 'over-engineering'/'missing abstraction'/YAGNI note on a small codebase is a \
             debatable preference (low confidence, capped severity), not a violation.\n"
        ));
        prompt.push_str("\nScrutinize these findings:\n");
        for (i, f) in items.iter().enumerate() {
            // The location, the quoted code and the detail all derive from the client's files.
            prompt.push_str(&format!("[{i}] (severity {})\n", f.severity));
            prompt.push_str(&crate::prompt_frame::frame_untrusted(
                &nonce,
                &format!("finding {i}"),
                &format!(
                    "{}:{} — {} :: {}",
                    f.path,
                    f.line,
                    camerata_secrets::redact_str(&f.snippet),
                    camerata_secrets::redact_str(&f.detail)
                ),
            ));
        }
        prompt
    };
    // Split into bounded chunks (see `CALIBRATION_BATCH_SIZE`'s doc comment) — each is an
    // independent streamed call with LOCAL indices `0..chunk.len()`, so a failure in one
    // chunk never touches another's verdicts. The common case (findings.len() <=
    // CALIBRATION_BATCH_SIZE) produces exactly one chunk equal to the whole input, so the
    // prompt/session/output shape below is byte-for-byte what this function always sent.
    let owned_chunks: Vec<Vec<Finding>> = findings
        .chunks(CALIBRATION_BATCH_SIZE)
        .map(<[Finding]>::to_vec)
        .collect();
    let n_chunks = owned_chunks.len();
    // Each chunk's findings, in chunk order, so a chunk whose calls failed can be tried once more
    // at the end and its result put back where it belongs.
    let mut chunk_results: Vec<Vec<Finding>> = Vec::with_capacity(n_chunks);
    // Chunks whose every call failed in a way worth another try: (chunk index, prompt, size).
    let mut deferred: Vec<(usize, String, usize)> = Vec::new();
    let mut trail = StageTrail::default();
    // Per chunk: its own failed calls, and whether it is still without a verdict. A chunk left
    // without one after the second try is a failed part naming its findings (see below).
    let mut chunk_trails: Vec<StageTrail> = vec![StageTrail::default(); n_chunks];
    let mut uncalibrated: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    for (chunk_idx, chunk) in owned_chunks.into_iter().enumerate() {
        // SPEND CAP, between batches: once the cap is reached the remaining batches do not run.
        // Their findings pass through exactly as the scan produced them (none dropped) and the
        // stage is recorded as stopped part way.
        let _permit = match admit_stage(meter, repo, STAGE_CALIBRATION).await {
            Err(CapReached) => {
                chunk_results.push(chunk);
                continue;
            }
            Ok(permit) => permit,
        };
        let chunk_len = chunk.len();
        let prompt = calibration_prompt(&chunk.iter().collect::<Vec<_>>());
        // Calibration runs on its OWN selected model (the UI exposes it). Build a fresh
        // request per pass (LlmRequest is consumed by complete_streaming).
        let build_req = || {
            with_stage_effort(
                LlmRequest::new(prompt.clone())
                    .with_system(system.clone())
                    .with_max_tokens(calibration_max_tokens(chunk_len))
                    .with_temperature(AUDIT_TEMPERATURE)
                    .with_response_schema(reply_schemas::calibration_reply_schema(chunk_len))
                    .with_model(stage_model(calibration_model)),
                Stage::Calibration,
            )
        };

        // Register (or replace) this pass's transcript entry with the REAL prompt before the
        // first call — mirrors `audit_pass`/`run_prose_lens`'s register-then-record pattern.
        // Single-chunk runs keep the EXACT session id + role convention this function always
        // used (`audit-{repo}-calibrate`); a `-bN` suffix + batch label only appears once
        // there's more than one chunk, a shape no pre-existing caller depended on.
        let session = if n_chunks > 1 {
            format!("audit-{repo}-calibrate-b{chunk_idx}")
        } else {
            format!("audit-{repo}-calibrate")
        };
        if let Some((store, key)) = feedback {
            store.register(
                key,
                crate::transcript::AgentTranscript {
                    session_id: session.clone(),
                    role: format!(
                        "calibrating {chunk_len} findings on {} — {repo}{}",
                        calibration_model.unwrap_or("default"),
                        if n_chunks > 1 {
                            format!(" (batch {}/{n_chunks})", chunk_idx + 1)
                        } else {
                            String::new()
                        }
                    ),
                    prompt: prompt.clone(),
                    output: String::new(),
                    status: "running".to_string(),
                },
            );
        }

        // THOROUGH mode (#51): run the calibration verdict MULTIPLE times and take the
        // conservative consensus, so a single over-confident pass can't push a debatable
        // finding to HIGH. Costs ~3x the calibration tokens (opt-in). Default mode is a
        // single pass (unchanged behavior).
        let passes = if thorough { 3 } else { 1 };
        let mut votes: Vec<String> = Vec::new();
        let mut last_error: Option<anyhow::Error> = None;
        for pass_idx in 0..passes {
            // Routed through the STREAMING transport (not the flat `complete`) even when
            // there is no live transcript to feed — the CLI leg's heartbeat resets on any
            // progress and SALVAGES partial output on a late kill, instead of the flat
            // non-streaming path's fixed 300s deadline with no partial credit for a
            // near-complete run. `on_delta` is a no-op here: the transcript (when present)
            // is still recorded post-hoc below, unchanged, so `feedback = Some` behavior is
            // byte-for-byte identical to before this change.
            let mut on_delta = |_: &str| {};
            let req = build_req();
            note_request(meter, &req);
            let calibration = llm.complete_streaming(req, &mut on_delta).await;
            if let Err(e) = &calibration {
                meter_failed(meter, Stage::Calibration, e);
            }
            if let Err(e) = calibration {
                last_error = Some(e);
            } else if let Ok(resp) = calibration {
                if let Some(m) = meter {
                    m.record_stage(Stage::Calibration, &resp);
                }
                // A reply cut off at the output limit still holds every verdict written out in
                // full; the findings it never reached are asked again, alone (the telemetry counts
                // the cut-off call as wasted by the share it left unanswered).
                let text = reask_unanswered_verdicts(
                    llm,
                    meter,
                    calibration_model,
                    &system,
                    &chunk,
                    CalibrationCall {
                        reply: &resp,
                        cap: calibration_max_tokens(chunk_len),
                    },
                    &calibration_prompt,
                )
                .await;
                if let Some((store, key)) = feedback {
                    // Single-pass (the common case) records the raw response as-is, so the
                    // transcript's output is exactly the model's text. THOROUGH mode's 3
                    // votes are each appended with a pass label so no vote is left silent.
                    if passes > 1 {
                        store.append_output(
                            key,
                            &session,
                            &format!("── pass {}/{passes} ──\n{}", pass_idx + 1, resp.text),
                        );
                    } else {
                        store.append_output(key, &session, &resp.text);
                    }
                }
                votes.push(text);
            }
        }
        let chunk_calibrated = match votes.len() {
            0 => chunk, // every pass for this chunk failed — pass its findings through unchanged
            1 => apply_verdicts(&votes[0], chunk),
            _ => apply_verdicts(&consensus_verdicts(&votes, chunk_len), chunk),
        };
        if let Some((store, key)) = feedback {
            if votes.is_empty() {
                store.append_output(
                    key,
                    &session,
                    "every calibration pass failed or timed out — findings pass through \
                     unchanged.",
                );
                store.set_status(key, &session, "blocked");
            } else {
                store.set_status(key, &session, "done");
            }
        }
        // Every call for this chunk failed: set it aside for one more try once the other chunks
        // are done, when the provider has had time to recover.
        if votes.is_empty() {
            if let Some(e) = &last_error {
                trail.note_error(e, false);
                chunk_trails[chunk_idx].note_error(e, false);
                uncalibrated.insert(chunk_idx);
                if second_try_allowed(e, meter) {
                    deferred.push((chunk_idx, prompt.clone(), chunk_len));
                }
            }
        }
        chunk_results.push(chunk_calibrated);
    }
    // END-OF-RUN SECOND TRY for the chunks set aside above, through the same admission and spend
    // checks; a stopped run sends nothing.
    for (chunk_idx, prompt, _chunk_len) in deferred {
        if meter.is_some_and(UsageMeter::outage_stopped) {
            break;
        }
        let Ok(_permit) = admit_stage(meter, repo, STAGE_CALIBRATION).await else {
            // The spend limit stopped the run; it records that, so the chunk is not also a
            // failed part.
            uncalibrated.remove(&chunk_idx);
            continue;
        };
        let req = with_stage_effort(
            LlmRequest::new(prompt)
                .with_system(system.clone())
                .with_max_tokens(calibration_max_tokens(chunk_results[chunk_idx].len()))
                .with_temperature(AUDIT_TEMPERATURE)
                .with_response_schema(reply_schemas::calibration_reply_schema(
                    chunk_results[chunk_idx].len(),
                ))
                .with_model(stage_model(calibration_model)),
            Stage::Calibration,
        );
        let mut on_delta = |_: &str| {};
        note_request(meter, &req);
        match llm.complete_streaming(req, &mut on_delta).await {
            Ok(resp) => {
                if let Some(m) = meter {
                    m.record_stage(Stage::Calibration, &resp);
                }
                let chunk = std::mem::take(&mut chunk_results[chunk_idx]);
                let text = reask_unanswered_verdicts(
                    llm,
                    meter,
                    calibration_model,
                    &system,
                    &chunk,
                    CalibrationCall {
                        reply: &resp,
                        cap: calibration_max_tokens(chunk.len()),
                    },
                    &calibration_prompt,
                )
                .await;
                chunk_results[chunk_idx] = apply_verdicts(&text, chunk);
                uncalibrated.remove(&chunk_idx);
            }
            Err(e) => {
                meter_failed(meter, Stage::Calibration, &e);
                trail.note_error(&e, true);
                chunk_trails[chunk_idx].note_error(&e, true);
            }
        }
    }
    // A chunk that still has no verdict after both tries (or because the run stopped before the
    // second) leaves its findings uncalibrated. Recorded below as a failed part naming them.
    let failed_chunks: Vec<(usize, Vec<Finding>)> = uncalibrated
        .iter()
        .map(|&i| (i, chunk_results[i].clone()))
        .collect();
    let calibrated: Vec<Finding> = chunk_results.into_iter().flatten().collect();
    // C3-1b: a run-wide (not per-chunk/per-finding) disclosure — every finding calibration
    // was handed came back with NO usable effort estimate at all. Neither D5 (severity floor)
    // nor D6 (severity ceiling) below ever sets `effort`, so checking it here, before those
    // text-derived passes run, is equivalent to checking the final output and reads clearer.
    let mut estimation_failed_passes = Vec::new();
    if !calibrated.is_empty() && calibrated.iter().all(|f| f.effort.is_none()) {
        if trail.attempts.is_empty() {
            trail.note_unusable(
                "the calibration reply held no usable remediation-effort estimate",
                false,
                Vec::new(),
            );
        }
        estimation_failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: "hour estimation".to_string(),
            reason: "the calibration pass returned no usable remediation-effort estimate for \
                     any finding this run"
                .to_string(),
            files_not_reviewed: 0,
            class: trail.class(),
            files: Vec::new(),
            attempts: trail.attempts.clone(),
            scope_note: false,
        });
    } else {
        // Some findings were estimated, so the run-wide miss above does not apply, but a chunk
        // that never got its verdict is still a part that failed.
        for (idx, chunk) in failed_chunks {
            estimation_failed_passes.push(failed_calibration_chunk(
                repo,
                idx,
                n_chunks,
                &chunk,
                &chunk_trails[idx],
            ));
        }
    }
    // D5: the deterministic severity floor runs regardless of whether the LLM calibration
    // pass succeeded — it re-derives its verdict from the finding's own text, not the model's,
    // so it is exactly as available when every pass failed as when one succeeded.
    // D6: the severity ceiling (R1/R2) runs immediately after, over the SAME set, for the same
    // reason — both are text-derived and safe regardless of whether calibration ran.
    let calibrated = apply_severity_ceiling_rules(apply_severity_calibration_rules(calibrated));
    (calibrated, estimation_failed_passes)
}

/// The failed part for one calibration chunk whose calls all failed: names the findings it left
/// uncalibrated (as `path:line`, at most five spelled out) and the files they sit in, so the
/// operator can see what was not checked and the resume can run the chunk again.
pub(super) fn failed_calibration_chunk(
    repo: &str,
    chunk_idx: usize,
    n_chunks: usize,
    chunk: &[Finding],
    trail: &StageTrail,
) -> FailedPass {
    let mut files: Vec<String> = chunk.iter().map(|f| f.path.clone()).collect();
    files.sort();
    files.dedup();
    let named: Vec<String> = chunk
        .iter()
        .take(5)
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect();
    let more = chunk.len().saturating_sub(named.len());
    let pass = if n_chunks > 1 {
        format!("calibration batch {} of {n_chunks}", chunk_idx + 1)
    } else {
        "calibration".to_string()
    };
    FailedPass {
        repo: repo.to_string(),
        pass,
        reason: format!(
            "the calibration step failed for {} finding{} left without a severity check or an \
             effort estimate: {}{}",
            chunk.len(),
            if chunk.len() == 1 { "" } else { "s" },
            named.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ),
        files_not_reviewed: 0,
        class: trail.class(),
        files,
        attempts: trail.attempts.clone(),
        scope_note: false,
    }
}

/// Merge several calibration passes into one CONSERVATIVE consensus verdict set (#51 thorough
/// mode). For each finding index: severity = the majority vote across the four levels
/// (low/medium/high/critical), ties break to the LOWER severity — so "critical" only wins when
/// it is the sole vote or an outright majority, never a tie against "high" (the anti-over-
/// rotation guard: disagreement about critical resolves down, not up); confidence = "high" only
/// when EVERY pass agreed on the same severity AND none of them was itself low-confidence — any
/// disagreement (on severity, or an individual pass's own confidence) means uncertainty, which
/// is exactly what the architect should review, so it becomes "low" (needs review). effort = the
/// majority vote (ties break to "medium" — a neutral default when the passes disagree, since
/// over- and under-estimating effort are equally misleading, unlike severity's asymmetric
/// humility rule). Returns a `{"verdicts":[…]}` JSON string for `apply_verdicts`.
pub(super) fn consensus_verdicts(votes: &[String], n: usize) -> String {
    use serde_json::Value;
    // Per index: collected (severity, confidence, reason, effort) across passes.
    let mut per: Vec<Vec<(String, String, String, String)>> = vec![Vec::new(); n];
    for raw in votes {
        let Some(json) = extract_json_object(raw) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        let Some(arr) = v["verdicts"].as_array() else {
            continue;
        };
        for verdict in arr {
            let Some(idx) = verdict["index"].as_u64() else {
                continue;
            };
            let idx = idx as usize;
            if idx >= n {
                continue;
            }
            let sev = match verdict["severity"].as_str().unwrap_or("medium") {
                "critical" => "critical",
                "high" => "high",
                "low" => "low",
                _ => "medium",
            }
            .to_string();
            let conf = if verdict["confidence"].as_str() == Some("low") {
                "low"
            } else {
                "high"
            }
            .to_string();
            let reason = verdict["reason"].as_str().unwrap_or("").trim().to_string();
            let effort = match verdict["effort"].as_str().unwrap_or("medium") {
                "low" => "low",
                "high" => "high",
                _ => "medium",
            }
            .to_string();
            per[idx].push((sev, conf, reason, effort));
        }
    }
    // Severity has FOUR levels now that "critical" is a real tier; effort still has three
    // (low/medium/high — a calibration verdict never emits "critical" effort). Two separate
    // rank functions rather than one shared one, so effort's 3-slot count array never has to
    // reason about a severity-only value.
    let sev_rank = |s: &str| match s {
        "critical" => 3,
        "high" => 2,
        "medium" => 1,
        _ => 0,
    };
    let effort_rank = |s: &str| match s {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    };
    let mut verdicts = Vec::new();
    for (idx, votes_for) in per.iter().enumerate() {
        if votes_for.is_empty() {
            continue;
        }
        // Majority severity; tie breaks to the lower rank (humble). This is an ANTI-OVER-
        // ROTATION guard too: "critical" only wins a tie when it is the SOLE or MAJORITY vote
        // at the top rank — a single critical vote against an equal number of high votes
        // resolves to "high", never silently promoted.
        let mut counts = [0u32; 4]; // [low, medium, high, critical]
        for (s, _, _, _) in votes_for {
            counts[sev_rank(s)] += 1;
        }
        let max = counts.iter().copied().max().unwrap_or(0);
        // Tie-breaks to the LOWER severity (humble / conservative design): low wins over
        // medium wins over high wins over critical when vote counts are equal. This is the
        // correct behaviour documented in the comment at the top of this function; the
        // previous ordering (high first) was the opposite of the spec. Fixed by BUG-5;
        // extended to the critical tier the same way.
        let sev = if counts[0] == max {
            "low"
        } else if counts[1] == max {
            "medium"
        } else if counts[2] == max {
            "high"
        } else {
            "critical"
        };
        // Disagreement on severity, or any low-confidence vote → low confidence (needs review).
        let distinct_sevs = counts.iter().filter(|&&c| c > 0).count();
        let any_low_conf = votes_for.iter().any(|(_, c, _, _)| c == "low");
        let agreed = distinct_sevs == 1 && !any_low_conf;
        let confidence = if agreed { "high" } else { "low" };
        // Majority effort; a tie among the top vote-getters breaks to "medium" (see the
        // function doc — effort has no humility direction the way severity does, so a
        // single clear winner is used as-is, but ANY tie among the leaders is neutral).
        let mut effort_counts = [0u32; 3]; // [low, medium, high]
        for (_, _, _, e) in votes_for {
            effort_counts[effort_rank(e)] += 1;
        }
        let effort_max = effort_counts.iter().copied().max().unwrap_or(0);
        let effort_winners: Vec<usize> =
            (0..3).filter(|&i| effort_counts[i] == effort_max).collect();
        let effort = match effort_winners.as_slice() {
            [0] => "low",
            [2] => "high",
            _ => "medium", // a single "medium" winner, or any tie among the leaders
        };
        // First non-empty reason, preferring a low-confidence pass's reason.
        let reason = votes_for
            .iter()
            .find(|(_, c, r, _)| c == "low" && !r.is_empty())
            .or_else(|| votes_for.iter().find(|(_, _, r, _)| !r.is_empty()))
            .map(|(_, _, r, _)| r.clone())
            .unwrap_or_default();
        verdicts.push(serde_json::json!({
            "index": idx, "severity": sev, "confidence": confidence, "effort": effort, "reason": reason
        }));
    }
    serde_json::json!({ "verdicts": verdicts }).to_string()
}
