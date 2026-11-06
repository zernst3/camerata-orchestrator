//! The rule-alternatives stage: the prompt, the reply parser and the pass that recommends one option per multi-option rule.

use super::*;

/// The text of each finding that a calibration or fix prompt frames: path, quoted code, detail and
/// the captured object names and values. A delimiter token is drawn to avoid all of it.
pub(super) fn finding_evidence_texts(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .flat_map(|f| {
            [f.path.clone(), f.snippet.clone(), f.detail.clone()]
                .into_iter()
                .chain(f.captures.iter().flat_map(|(k, v)| [k.clone(), v.clone()]))
        })
        .collect()
}

/// The ADOPTED-rules header for the audit prompt. Empty when nothing is selected (the
/// audit then falls back to a free-form investigative read).
pub(super) fn build_rules_block(selected: &[(String, String)]) -> String {
    if selected.is_empty() {
        return String::new();
    }
    let mut b = String::from(
        "The project has ADOPTED these rules — check the code against each, AND flag \
         any other genuine issues you find:\n",
    );
    for (id, directive) in selected {
        b.push_str(&format!("- [{id}] {directive}\n"));
    }
    b.push('\n');
    b
}

/// The system prompt for the dedicated alternative-recommendation pass. Separate from
/// [`audit_system_prompt`] (which checks code against ONE resolved directive per rule) —
/// this pass's only job is to PICK, per multi-option rule, the option that best fits the
/// codebase, grounded in real code evidence. It does not report violations; the main audit
/// passes do that afterward, against whichever option this pass decided (see `audit_repo`).
pub fn alternatives_system_prompt() -> String {
    r#"You are a senior software architect helping a team decide, for THIS codebase, which
alternative implementation of each adopted rule best matches its existing (or best-practice)
pattern.

The user message lists one or more rules. Each rule shows EVERY alternative option it offers
— an option id, a human label, the concrete directive that option codifies, and the rationale
for it — plus a marker for whichever option is CURRENTLY selected (the project's own choice,
the corpus default, or "none selected" when neither exists).

For EACH rule listed, first decide whether the rule's underlying CONCERN even applies to this
codebase. Some rules govern something that may simply not exist here at all (no pagination
anywhere, no API versioning anywhere, no background jobs anywhere) — if you cannot point to a
SPECIFIC file and line where the codebase either already follows this rule's concern or
concretely needs it, the rule is NOT APPLICABLE. Set `"applicable": false` and do not pick an
option for it. Only set `"applicable": true` when you can cite the exact evidence.

When the rule IS applicable, pick EXACTLY ONE option id: the one that best matches how this
codebase already does things, or — if the codebase is inconsistent — the one that is the
best-practice fit given the stack and code you can see. You are free to keep the
currently-selected option, or pick a different one; ground the choice in the actual code, not
a coin flip.

ALSO count, for each applicable rule, how many sites in the code you can see the rule's
decision applies to (e.g. how many API endpoints for a versioning rule) and report that number
as `"sites_total"`. Count only what you can actually see; if you cannot count honestly, omit
the field entirely — never guess a number.

Some rules are marked `[PURE DECISION — "no convention" is a valid answer]`. For ONLY those
rules: if the codebase genuinely uses SEVERAL of the listed options with NO clear majority, do
not force a pick — set `"no_convention": true` and report what you actually saw in
`"observed_options"`: one entry per option you saw in use, with the option id copied verbatim
from that rule's own list and, when you can count honestly, how many sites use it. A rule
without that marker must ALWAYS get a single recommended option when applicable.

CRITICAL RULE — never assert an unevidenced convention: you must NEVER say a codebase has
"adopted" a pattern, or describe ANY convention as already in use, unless you cite the EXACT
`path:line` where that is true. If you cannot cite a real file:line, either say the rule is not
applicable (see above) or, if it applies but nothing in the repo yet follows it, say so plainly
("no existing pattern; recommending X as the best-practice fit for this stack") rather than
inventing an "adopted" claim.

ALSO report that same distinction as a BOOLEAN, `"currently_followed"`, for every applicable
rule. Set it `true` ONLY when the codebase ALREADY follows your recommended option today and
you cited the file:line proving it. Set it `false` when the rule applies but the codebase does
not follow the option yet and you are recommending it as the best-practice target. This is NOT
the same question as `"applicable"`: a rule can apply precisely BECAUSE the codebase is missing
the thing it governs, and that is `"applicable": true` with `"currently_followed": false`. We
print "this codebase consistently follows X" to the client only for the `true` case, so a wrong
`true` here puts a false statement about their own code in front of them over our name. If you
are unsure, use `false`.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "recommendations": [
    {
      "rule_id": "EXACT rule id as given, copied verbatim",
      "applicable": true,
      "recommended_option_id": "EXACT option id from THAT rule's own list, copied verbatim",
      "evidence": "path/to/file.ext:42 — one short phrase on what's there",
      "recommendation_reasoning": "1-3 sentences grounded in the codebase evidence you saw",
      "sites_total": 47,
      "currently_followed": true,
      "no_convention": false,
      "observed_options": [{"option_id": "EXACT option id", "sites": 2}]
    }
  ]
}
When `"applicable"` is `false`, omit `"recommended_option_id"` and `"evidence"` (or leave them
empty) — there is nothing to recommend against for a concern that doesn't apply here.
`"sites_total"` is optional — include it only when you could honestly count. Omit
`"no_convention"`/`"observed_options"` entirely except for the one case described above (a
marked PURE-DECISION rule with no clear majority).

Include exactly one entry per rule listed, even when you are keeping the currently-selected
option or marking it not applicable. NEVER invent an option id that was not listed for that
specific rule. NEVER set `"applicable": true` without a real `"evidence"` file:line — an
unevidenced applicability claim will be discarded and treated as not applicable."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// The per-rule "every option, plus which is currently selected" block the recommendation
/// pass reads. Shared shape with what a human reviewer would see in the rule-detail modal —
/// id, label, directive, why — so the model's grounds for picking match what an architect
/// would weigh.
pub(super) fn build_alternatives_block(alternatives: &[RuleAlternatives]) -> String {
    let mut b = String::from("Rules to decide (pick ONE option id per rule):\n\n");
    for a in alternatives {
        // A5.3: only a PURE decision rule may honestly answer "no convention adopted" — a
        // default_with_exceptions rule has an externally-correct default, so the model is
        // never offered the escape for it (and the parser refuses it even if volunteered).
        //
        // W17: asks `RuleKind::offers_no_convention_escape()` rather than comparing against
        // `Decision` inline, so a newly added kind states its answer in one exhaustive match in
        // the rules crate instead of inheriting "no escape" by failing a comparison. Behaviour
        // is identical for all existing kinds; `decision_with_floor` is also withheld, because
        // the escape severs the per-site violation path and that kind names real faults.
        let marker = if a.kind.is_some_and(|k| k.offers_no_convention_escape()) {
            " [PURE DECISION — \"no convention\" is a valid answer]"
        } else {
            ""
        };
        b.push_str(&format!("- [{}]{marker}\n", a.rule_id));
        for o in &a.options {
            let marker = if a.selected_option_id.as_deref() == Some(o.id.as_str()) {
                " [CURRENTLY SELECTED]"
            } else {
                ""
            };
            b.push_str(&format!(
                "    * option id \"{}\" — {}: {}{}\n      why: {}\n",
                o.id, o.label, o.directive, marker, o.why
            ));
        }
        if a.selected_option_id.is_none() {
            b.push_str("    (no option currently selected for this rule)\n");
        }
        b.push('\n');
    }
    b
}

/// Fail-soft recommendations for every listed rule, all pointing at the currently-selected
/// (or, absent that, the first-listed) option and flagged `hallucinated` so the caller can
/// tell this was a fallback rather than a genuine model answer. Used when the model's raw
/// output for the recommendation pass could not be parsed at all.
pub(super) fn fallback_recommendations(
    alternatives: &[RuleAlternatives],
    reason: &str,
) -> Vec<RuleRecommendation> {
    alternatives
        .iter()
        .filter_map(|a| {
            let id = a
                .selected_option_id
                .clone()
                .or_else(|| a.options.first().map(|o| o.id.clone()))?;
            Some(RuleRecommendation {
                repo: String::new(),
                rule_id: a.rule_id.clone(),
                recommended_option_id: id,
                recommendation_reasoning: reason.to_string(),
                hallucinated: true,
                operator_chosen: false,
                // A total parse failure is a MODEL-PIPELINE failure, not evidence that the
                // rule's concern doesn't apply — fail open exactly as before P7 (keep checking
                // the currently-selected option) rather than silently dropping the rule.
                evidence: None,
                applicable: true,
                no_convention: false,
                observed_options: Vec::new(),
                assessed_sites: None,
                currently_followed: None,
            })
        })
        .collect()
}

/// Parse one recommendation entry's OPTIONAL `observed_options` array into validated
/// [`camerata_api_types::audit::ObservedOptionUsage`] records (Pass-1 W3, A5.3). Fail-soft
/// and structurally validating, like [`parse_finding_captures`]:
/// - absent / non-array -> empty;
/// - an entry whose `option_id` is not one of THIS rule's real options -> dropped (an
///   invented option id is never carried anywhere);
/// - duplicate option ids -> first entry wins;
/// - `label` always comes from the AUTHORED option (never model prose);
/// - `sites` only when a positive integer was given (`None` = observed, not counted).
pub(super) fn parse_observed_options(
    r: &serde_json::Value,
    options: &[RuleOptionView],
) -> Vec<camerata_api_types::audit::ObservedOptionUsage> {
    let Some(arr) = r["observed_options"].as_array() else {
        return Vec::new();
    };
    let mut out: Vec<camerata_api_types::audit::ObservedOptionUsage> = Vec::new();
    for entry in arr {
        let Some(raw_id) = entry["option_id"].as_str().map(str::trim) else {
            continue;
        };
        let Some(opt) = options.iter().find(|o| o.id == raw_id) else {
            continue;
        };
        if out.iter().any(|u| u.option_id == opt.id) {
            continue;
        }
        out.push(camerata_api_types::audit::ObservedOptionUsage {
            option_id: opt.id.clone(),
            label: opt.label.clone(),
            sites: entry["sites"]
                .as_u64()
                .filter(|n| *n > 0)
                .map(|n| n as usize),
        });
    }
    out
}

/// A non-blank `evidence` string after trimming, or `None`. Shared by the "does this entry
/// carry real evidence" check on both branches of [`parse_alternative_recommendations`].
pub(super) fn trimmed_evidence(v: &serde_json::Value) -> Option<String> {
    v["evidence"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse the recommendation pass's raw model output into one [`RuleRecommendation`] per
/// listed rule. Robust by construction: EVERY rule in `alternatives` gets exactly one
/// output entry, whether or not the model answered it validly —
/// - malformed/unparseable JSON -> every rule falls back (see [`fallback_recommendations`]);
/// - a rule missing from the model's `recommendations` array -> that rule falls back;
/// - a `recommended_option_id` that is not a real option id on that specific rule (a
///   hallucination) -> that rule falls back, flagged `hallucinated: true`;
/// - otherwise the model's pick is used as-is, `hallucinated: false`.
///
/// The fallback target is the rule's OWN `selected_option_id` (the project's chosen option,
/// else the corpus default) — never a hardcoded/arbitrary id — so a fallback still resolves
/// to something the architect already considered reasonable.
///
/// # P7 — evidence-gated applicability
/// Two checks are ORTHOGONAL to the option-id validation above:
/// - **Applicability.** The model reports `"applicable": true|false` per rule — `false` means
///   the codebase neither follows nor needs this rule's concern at all (no evidence for it
///   anywhere), and the caller (`audit_repo`) must then drop the rule from the directive set
///   actually checked so it emits zero findings, rather than checking violations against a
///   convention nothing in the repo actually established.
/// - **Evidence-gates-applicability.** A model claiming `applicable: true` WITHOUT a non-blank
///   `evidence` file:line is never trusted at face value — that is exactly the "describes a
///   convention as adopted without citing where" failure mode this pass exists to prevent. Such
///   an entry is forcibly downgraded to `applicable: false` and flagged `hallucinated: true`
///   (the model's applicability claim, not just its option id, was corrected). This check is
///   independent of whether the `recommended_option_id` itself was valid.
pub fn parse_alternative_recommendations(
    raw: &str,
    alternatives: &[RuleAlternatives],
) -> Vec<RuleRecommendation> {
    let Some(json) = extract_json_object(raw) else {
        return fallback_recommendations(
            alternatives,
            "The model returned no parseable JSON for the recommendation pass; keeping the \
             currently selected option.",
        );
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return fallback_recommendations(
            alternatives,
            "The model returned malformed JSON for the recommendation pass; keeping the \
             currently selected option.",
        );
    };
    let arr = v["recommendations"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(alternatives.len());
    for a in alternatives {
        let entry = arr
            .iter()
            .find(|r| r["rule_id"].as_str().map(str::trim) == Some(a.rule_id.as_str()));
        let real_ids: std::collections::HashSet<&str> =
            a.options.iter().map(|o| o.id.as_str()).collect();
        let fallback_id = || {
            a.selected_option_id
                .clone()
                .or_else(|| a.options.first().map(|o| o.id.clone()))
                .unwrap_or_default()
        };
        let rec = match entry {
            Some(r) => {
                let raw_id = r["recommended_option_id"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let reasoning = r["recommendation_reasoning"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // Applicability, gated on evidence — independent of the option-id check below.
                // `applicable` absent entirely (an older-shaped/malformed entry) fails OPEN
                // (treated as applicable) so a rule is never silently skipped just because the
                // model omitted a field; only an EXPLICIT `true` without evidence is corrected.
                // `applicable` ABSENT entirely (the key was never sent — an older-shaped
                // response, or a test fixture predating P7) is NOT the same as the model
                // EXPLICITLY claiming `true`: only an explicit, unevidenced `true` gets
                // corrected. An absent field fails open exactly like pre-P7 behavior (checked,
                // no evidence requirement) — the current prompt always sends the field for a
                // real model call, so this only matters for legacy/malformed output.
                let raw_applicable = r.get("applicable").and_then(|v| v.as_bool());
                let raw_evidence = trimmed_evidence(r);
                let (applicable, evidence, evidence_corrected) =
                    match (raw_applicable, raw_evidence) {
                        (None, ev) => (true, ev, false),
                        (Some(true), Some(ev)) => (true, Some(ev), false),
                        (Some(true), None) => (false, None, true),
                        (Some(false), _) => (false, None, false),
                    };
                // Pass-1 W3: the model's structured site count (`"sites_total"`) — the
                // deviation sentence's DENOMINATOR. Display honesty only; a missing or
                // non-positive count is `None`, never a fabricated number.
                let assessed_sites = r["sites_total"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .map(|n| n as usize);
                // W21/W22: does the codebase ACTUALLY follow the recommended option today, as
                // opposed to the rule's concern merely applying? `applicable` cannot answer
                // that — its own contract is "already follows it OR concretely needs it" —
                // and reading it as if it could is what shipped "the codebase consistently
                // follows X" over a repo the same row then described as having "no existing
                // pattern". Absent stays `None` and consumers must treat that as NOT
                // followed; see `RuleRecommendation::currently_followed`.
                let currently_followed = r.get("currently_followed").and_then(|v| v.as_bool());
                // Pass-1 W3 (A5.3): the no-convention outcome. STRUCTURALLY gated: honored
                // only for a pure `Decision`-kind rule (see `RuleAlternatives::kind`) AND only
                // when the claim is backed by a validated observed-usage breakdown or a real
                // evidence citation — an unbacked "no convention" from the model is corrected
                // (flagged `hallucinated`, judged against an option exactly as before), so a
                // lazy model answer can never silently drop a rule's violation coverage.
                let observed_options = parse_observed_options(r, &a.options);
                let no_convention_claimed = r
                    .get("no_convention")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // W17: `offers_no_convention_escape()` in place of an inline `== Decision`, so
                // the kind taxonomy answers this in one exhaustive match. Same reasoning and
                // same behaviour as the prompt-side marker in `build_alternatives_block`, and
                // the two now cannot drift apart.
                if applicable
                    && no_convention_claimed
                    && a.kind.is_some_and(|k| k.offers_no_convention_escape())
                    && (!observed_options.is_empty() || evidence.is_some())
                {
                    out.push(RuleRecommendation {
                        repo: String::new(),
                        rule_id: a.rule_id.clone(),
                        // Kept only so the field is never a meaningless empty string; a
                        // no-convention rule is never judged against ANY option (audit_repo
                        // drops it from the directive set and hard-filters strays).
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning: reasoning,
                        hallucinated: false,
                        operator_chosen: false,
                        evidence,
                        applicable: true,
                        no_convention: true,
                        observed_options,
                        assessed_sites,
                        currently_followed,
                    });
                    continue;
                }
                let refused_no_convention = applicable && no_convention_claimed;
                let rec = if !applicable {
                    // NOT APPLICABLE — there is no option to validate (an explicit, honest
                    // "this concern doesn't apply here" answer names no option at all, and the
                    // evidence-corrected case's `raw_id`/`raw_evidence`, if any, are moot: the
                    // whole point is nothing will be checked against this rule). Never run the
                    // option-id validation below — an empty/missing `recommended_option_id` here
                    // is EXPECTED, not a hallucination in its own right.
                    let recommendation_reasoning = if evidence_corrected {
                        format!(
                            "The model claimed this rule's concern applies to the codebase but \
                             did not cite a file:line where it does — never trusting an \
                             unevidenced \"adopted\" claim, treating as not applicable instead. \
                             Model reasoning was: {reasoning}"
                        )
                    } else {
                        reasoning
                    };
                    RuleRecommendation {
                        repo: String::new(),
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning,
                        // Only the evidence-correction case is a genuine hallucination (the
                        // model's OWN claim had to be overridden). An honest, explicit
                        // `applicable: false` is not — the model correctly declined to assert
                        // an unevidenced convention.
                        hallucinated: evidence_corrected,
                        operator_chosen: false,
                        evidence,
                        applicable,
                        no_convention: false,
                        observed_options: Vec::new(),
                        assessed_sites: None,
                        currently_followed,
                    }
                } else if !raw_id.is_empty() && real_ids.contains(raw_id.as_str()) {
                    RuleRecommendation {
                        repo: String::new(),
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: raw_id,
                        recommendation_reasoning: reasoning,
                        hallucinated: false,
                        operator_chosen: false,
                        evidence,
                        applicable,
                        no_convention: false,
                        observed_options: Vec::new(),
                        assessed_sites,
                        currently_followed,
                    }
                } else if raw_id.is_empty() {
                    RuleRecommendation {
                        repo: String::new(),
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning: "The model did not return a valid option id \
                             for this rule; keeping the currently selected option."
                            .to_string(),
                        hallucinated: true,
                        operator_chosen: false,
                        evidence,
                        applicable,
                        no_convention: false,
                        observed_options: Vec::new(),
                        assessed_sites,
                        currently_followed,
                    }
                } else {
                    RuleRecommendation {
                        repo: String::new(),
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning: format!(
                            "The model returned option id \"{raw_id}\", which is not one of \
                             this rule's real alternatives; keeping the currently selected \
                             option."
                        ),
                        hallucinated: true,
                        operator_chosen: false,
                        evidence,
                        applicable,
                        no_convention: false,
                        observed_options: Vec::new(),
                        assessed_sites,
                        currently_followed,
                    }
                };
                // Pass-1 W3 (A5.3): the model volunteered "no convention" where the outcome
                // is not available (not a pure decision rule / no backing at all). The claim
                // is CORRECTED, visibly: the rule keeps being judged against an option
                // exactly as before, and the entry is flagged so the UI can show the model's
                // answer was overridden rather than silently re-interpreted.
                let mut rec = rec;
                if refused_no_convention {
                    rec.hallucinated = true;
                    rec.recommendation_reasoning = format!(
                        "The model answered \"no convention adopted\" for a rule where that \
                         outcome is not available (only a pure decision-kind rule with an \
                         observed-usage breakdown or evidence can make that claim); judging \
                         against an option as usual instead. Model reasoning was: {}",
                        rec.recommendation_reasoning
                    );
                }
                rec
            }
            None => RuleRecommendation {
                repo: String::new(),
                rule_id: a.rule_id.clone(),
                recommended_option_id: fallback_id(),
                recommendation_reasoning: "The model did not return a recommendation for this \
                     rule; keeping the currently selected option."
                    .to_string(),
                hallucinated: true,
                operator_chosen: false,
                // Missing from the model's output entirely — a pipeline gap, not evidence the
                // concern is inapplicable. Fail open (keep checking) exactly as before P7.
                evidence: None,
                applicable: true,
                no_convention: false,
                observed_options: Vec::new(),
                assessed_sites: None,
                currently_followed: None,
            },
        };
        out.push(rec);
    }
    out
}

/// Most file chunks a rule judged "not applicable" is asked about again (M7). Beyond this the
/// judgment stands and says how many chunks it covers.
pub(super) const ALTERNATIVES_MAX_FILE_CHUNKS: usize = 4;

/// A path a model named in `needs_files` or a scan reply, in the form the scan lists files: no
/// leading `./` or `/`, forward slashes, trimmed of quotes and backticks (M6).
pub(crate) fn normalize_requested_path(raw: &str) -> String {
    let t = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '`' | '"' | '\''))
        .replace('\\', "/");
    let mut t = t.trim();
    loop {
        if let Some(rest) = t.strip_prefix("./") {
            t = rest;
        } else if let Some(rest) = t.strip_prefix('/') {
            t = rest;
        } else {
            break;
        }
    }
    t.to_string()
}

/// Run the dedicated recommendation pass: ONE LLM call deciding `recommended_option_id` +
/// `recommendation_reasoning` for every rule in `alternatives`, grounded in the repo map plus
/// a representative code digest. Deciding this ONCE (rather than once per file-chunk, as the
/// violation passes do) keeps every later violation-check for a given rule coherent — they
/// all check against the SAME option (see the design doc's "violations are always relative to
/// ONE chosen alternative" coherence principle) — and avoids re-litigating the same pick N
/// times at N times the cost.
///
/// KNOWN LIMIT: for a repo too large for one chunk, this pass sees only the FIRST size-capped
/// chunk's digest (plus the whole-repo map, which is cheap cross-file symbol context but not
/// full bodies). This is the same chunk-0 tradeoff the existing advisory ("flag novel issues")
/// pass already makes for the identical reason — see `run_passes`'s `bi == 0` gating. A rule
/// whose defining pattern lives entirely in a later chunk may get a less-informed pick; this
/// is a disclosed limitation, not a silent one.
///
/// Returns `Ok((vec![], None))` when `alternatives` is empty (nothing to decide — no call made).
///
/// A reply cut off at the output limit is not read as "no answer". The entries that closed before
/// the cut are kept ([`salvage_alternative_answers`]); the rules it never reached are asked again
/// ONCE as a smaller batch; a rule still unanswered then gets no entry (it keeps its default
/// directive) and the second value of the result is the plain-words reason the caller records
/// as a pass, naming those rules. `None` when every rule was decided.
/// Whether `raw` is a recommendations reply: a JSON object holding the `recommendations` array
/// [`parse_alternative_recommendations`] reads. The run store decides a stored reply is usable
/// with this; a transcript that merely quotes some object is not.
pub(crate) fn is_recommendations_reply(raw: &str) -> bool {
    extract_json_object(raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .is_some_and(|v| v["recommendations"].is_array())
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn recommend_alternatives(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    map_files: &[(String, String)],
    nonce: &crate::prompt_frame::FrameNonce,
    alternatives: &[RuleAlternatives],
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<(Vec<RuleRecommendation>, Option<String>)> {
    recommend_alternatives_in(
        llm,
        repo,
        files,
        map_files,
        nonce,
        alternatives,
        audit_model,
        feedback,
        meter,
        stage_batch::StageRoute::default(),
    )
    .await
}

/// [`recommend_alternatives`] on a run's route: each call goes out as a Message Batch when the
/// run's scan mode is Batch and the model seam has a batch API (chosen per call), reporting to
/// the run's job.
#[allow(clippy::too_many_arguments)]
pub(super) async fn recommend_alternatives_in(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    map_files: &[(String, String)],
    nonce: &crate::prompt_frame::FrameNonce,
    alternatives: &[RuleAlternatives],
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
    route: stage_batch::StageRoute<'_>,
) -> anyhow::Result<(Vec<RuleRecommendation>, Option<String>)> {
    if alternatives.is_empty() {
        return Ok((Vec::new(), None));
    }
    // The same map as the scan's: same files, same run token, same tree budget (the scan model's
    // window), so its leading tree is the same bytes and the same cache entry.
    let repo_map = RepoMap::new(
        map_files,
        nonce,
        repo_map::tree_budget_for_window(request_budget::context_window_tokens(
            &resolve_audit_model(audit_model),
        )),
    );
    let file_chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
    // M7: a rule judged "not applicable" on the first chunk's digest is asked again on each
    // further chunk, because the code that makes it applicable may sit in any of them. Rules
    // already judged applicable (or with no convention) are not asked again, so the common case
    // is still ONE call per rule batch. A rule is "not applicable" only when EVERY chunk says so.
    //
    // A chunk over the model's window is split (M1) into parts that each fit; the digests read are
    // the first `ALTERNATIVES_MAX_FILE_CHUNKS` of those parts, and the "judged over N of M"
    // disclosure counts parts.
    let read_chunks = &file_chunks[..file_chunks.len().min(ALTERNATIVES_MAX_FILE_CHUNKS)];
    let fixed_bytes = {
        let system =
            crate::prompt_frame::with_untrusted_clause(alternatives_system_prompt(), nonce);
        let schema_len = reply_schemas::alternatives_reply_schema().to_string().len();
        let block_len = alternatives
            .chunks(ALTERNATIVES_BATCH_SIZE)
            .map(|c| build_alternatives_block(c).len())
            .max()
            .unwrap_or(0);
        system.len()
            + schema_len
            + repo.len()
            + 16
            + repo_map.prefix().len()
            + repo_map.symbols_reserve_bytes()
            + block_len
    };
    let digest_budget = request_budget::digest_budget_tokens(
        &resolve_audit_model(audit_model),
        alternatives_max_tokens(ALTERNATIVES_BATCH_SIZE.min(alternatives.len())),
        fixed_bytes,
    );
    let (parts, fit) = request_budget::fit_chunks(read_chunks, None, nonce, digest_budget);
    if fit.parts_added > 0 {
        eprintln!(
            "[camerata-server] {repo} alternatives: {} chunk(s) were over the model's context \
             window and were split into {} more part(s)",
            fit.chunks_split, fit.parts_added
        );
        if let Some(m) = meter {
            m.record_request_splits(crate::stage_telemetry::RequestSplits {
                chunks_split: fit.chunks_split as u64,
                parts_added: fit.parts_added as u64,
                files_split_by_lines: fit.files_split_by_lines as u64,
            });
        }
    }
    let total_chunks = (file_chunks.len() + fit.parts_added).max(1);
    // Per digest, the symbols of its chunk's files and their neighbours; they sit with the digest,
    // after the cached repo-map prefix.
    let symbols: Vec<repo_map::SymbolsSection> = parts
        .iter()
        .take(ALTERNATIVES_MAX_FILE_CHUNKS)
        .map(|p| repo_map.symbols_for(p.files, nonce))
        .collect();
    let digests: Vec<String> = if parts.is_empty() {
        vec![String::new()]
    } else {
        parts
            .iter()
            .zip(&symbols)
            .take(ALTERNATIVES_MAX_FILE_CHUNKS)
            .map(|(p, sy)| chunk_segment(&sy.text, &p.digest).into_owned())
            .collect()
    };
    let chunks_read = digests.len();

    // Bounded chunks (see `ALTERNATIVES_BATCH_SIZE`'s doc comment) - each rule-batch is an
    // independent streamed call, so no single completion is asked to decide every
    // multi-option rule in the scan at once. The common case (alternatives.len() <=
    // ALTERNATIVES_BATCH_SIZE) is exactly one chunk equal to the whole input, so the
    // prompt/session/role shape below is byte-for-byte what this function always sent.
    let rule_chunks: Vec<&[RuleAlternatives]> =
        alternatives.chunks(ALTERNATIVES_BATCH_SIZE).collect();
    let n_chunks = rule_chunks.len();
    let mut recommendations = Vec::with_capacity(alternatives.len());
    let mut gaps = AlternativesGaps::default();

    for (chunk_idx, alt_chunk) in rule_chunks.into_iter().enumerate() {
        // The best answer so far for each rule of this batch, in the batch's order.
        let mut best: Vec<Option<RuleRecommendation>> = vec![None; alt_chunk.len()];
        // Rules whose MOST RECENT ask (a first send and its smaller retry included) got no
        // answer because the reply was cut off at the output limit.
        let mut unanswered_ids: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // The digests to ask, in order. A digest whose ask failed in a way worth another try is
        // asked once more at the END of this list, after the others, inside this stage, so the
        // decision exists before the scan reads it.
        let mut work: Vec<(usize, bool)> = (0..digests.len()).map(|i| (i, false)).collect();
        let mut first_failures: std::collections::HashMap<usize, anyhow::Error> =
            std::collections::HashMap::new();
        let mut next_work = 0usize;
        while next_work < work.len() {
            let (digest_idx, is_second) = work[next_work];
            next_work += 1;
            let digest = &digests[digest_idx];
            let pending: Vec<RuleAlternatives> = alt_chunk
                .iter()
                .zip(&best)
                .filter(|(_, b)| b.as_ref().is_none_or(|r| !r.applicable))
                .map(|(a, _)| a.clone())
                .collect();
            if pending.is_empty() {
                break;
            }
            // Later digests of a SPEND-CAPPED run are skipped, not sent: the answers so far stand.
            if digest_idx > 0 && meter.is_some_and(UsageMeter::retry_blocked) {
                break;
            }
            // Single-call runs keep the EXACT session id + role this function always used
            // (`audit-{repo}-alternatives`); the `-bN` suffix + batch label only appears once
            // there's more than one chunk, and `-dN` once a rule is asked on a further digest.
            let mut session = if n_chunks > 1 {
                format!("audit-{repo}-alternatives-b{chunk_idx}")
            } else {
                format!("audit-{repo}-alternatives")
            };
            if digest_idx > 0 {
                session.push_str(&format!("-d{digest_idx}"));
            }
            let role = format!(
                "recommending {} alternative(s), {repo}{}{}",
                pending.len(),
                if n_chunks > 1 {
                    format!(" (batch {}/{n_chunks})", chunk_idx + 1)
                } else {
                    String::new()
                },
                if digest_idx > 0 {
                    format!(" (file chunk {}/{})", digest_idx + 1, digests.len())
                } else {
                    String::new()
                }
            );
            if let Some(sy) = symbols.get(digest_idx) {
                note_map_use(meter, repo, "alternatives", digest_idx, sy);
            }
            let ask = AlternativesAsk {
                llm,
                repo,
                repo_map: repo_map.prefix(),
                digest,
                n_rule_batches: n_chunks,
                n_digests: digests.len(),
                nonce,
                audit_model,
                feedback,
                meter,
                route,
            };
            let resp = match ask.send(&pending, &session, &role).await {
                Ok(resp) => resp,
                Err(e) if is_second => {
                    return Err(match first_failures.remove(&digest_idx) {
                        Some(first) => error_with_both_tries(&first, e),
                        None => e,
                    });
                }
                Err(e) if second_try_allowed(&e, meter) => {
                    first_failures.insert(digest_idx, e);
                    work.push((digest_idx, true));
                    continue;
                }
                Err(e) => return Err(e),
            };
            let (mut answered, unanswered) = if resp.truncated {
                // A reply cut off at the output limit: the entries that closed before the cut
                // are answers; the rest were never reached.
                gaps.attempts = gaps.attempts.max(1);
                let (recs, rest) = salvage_alternative_answers(&resp.text, &pending);
                if recs.is_empty() {
                    if let Some(m) = meter {
                        m.record_wasted(Stage::Alternatives, &resp);
                    }
                }
                (recs, rest)
            } else {
                (
                    parse_alternative_recommendations(&resp.text, &pending),
                    Vec::new(),
                )
            };
            let mut still_unanswered: Vec<RuleAlternatives> = Vec::new();
            if !unanswered.is_empty() {
                // ONE retry of what the cut-off reply left out, as a smaller batch: the rules not
                // yet answered, and when the first reply answered none of them, half of the
                // batch at a time, so the retry is never the same size as the send that failed.
                let why_not = if !efficiency_of(meter).truncation_retry {
                    Some("the retry of a cut-off reply is switched off".to_string())
                } else {
                    None
                };
                let sizes = if unanswered.len() == pending.len() && unanswered.len() > 1 {
                    unanswered.len().div_ceil(2)
                } else {
                    unanswered.len()
                };
                for (n, group) in unanswered.chunks(sizes).enumerate() {
                    if let Some(why) = &why_not {
                        gaps.retry_note.get_or_insert_with(|| why.clone());
                        still_unanswered.extend_from_slice(group);
                        continue;
                    }
                    if meter.is_some_and(UsageMeter::retry_blocked) {
                        gaps.retry_note
                            .get_or_insert_with(|| retry_blocked_cause(meter).1.to_string());
                        still_unanswered.extend_from_slice(group);
                        continue;
                    }
                    gaps.attempts = gaps.attempts.max(2);
                    let retry_session = format!("{session}-r{n}");
                    let retry_role = format!(
                        "recommending {} alternative(s), {repo} (retry of a cut-off reply)",
                        group.len()
                    );
                    match ask.send(group, &retry_session, &retry_role).await {
                        Ok(retry) if retry.truncated => {
                            let (recs, rest) = salvage_alternative_answers(&retry.text, group);
                            if recs.is_empty() {
                                if let Some(m) = meter {
                                    m.record_wasted(Stage::Alternatives, &retry);
                                }
                            }
                            answered.extend(recs);
                            still_unanswered.extend(rest);
                        }
                        Ok(retry) => {
                            answered.extend(parse_alternative_recommendations(&retry.text, group));
                        }
                        // The failed call is already on the meter; the answers so far stand.
                        Err(e) => {
                            gaps.retry_note
                                .get_or_insert_with(|| format!("the retry did not complete: {e}"));
                            still_unanswered.extend_from_slice(group);
                        }
                    }
                }
            }
            for a in &pending {
                unanswered_ids.remove(&a.rule_id);
            }
            for a in &still_unanswered {
                unanswered_ids.insert(a.rule_id.clone());
            }
            for rec in answered {
                if let Some(at) = alt_chunk.iter().position(|a| a.rule_id == rec.rule_id) {
                    // The first answer is kept unless a later digest finds the rule applicable.
                    if best[at]
                        .as_ref()
                        .is_none_or(|b| !b.applicable && rec.applicable)
                    {
                        best[at] = Some(rec);
                    }
                }
            }
        }
        // A rule whose last ask was cut off and never answered keeps its default directive (it has
        // no entry here); one that an earlier file chunk did answer keeps that answer, and says it
        // was judged on fewer chunks.
        for (a, b) in alt_chunk.iter().zip(best.iter_mut()) {
            if !unanswered_ids.contains(&a.rule_id) {
                continue;
            }
            match b {
                None => gaps.undecided.push(a.rule_id.clone()),
                Some(rec) => {
                    rec.recommendation_reasoning.push_str(
                        " A reply for a later file chunk was cut off, so this was judged on fewer \
                         file chunks.",
                    );
                    gaps.partial.push(a.rule_id.clone());
                }
            }
        }
        // A rule still judged not applicable after reading fewer chunks than the repo has says
        // so: the judgment covers part of the code only.
        for rec in best.iter_mut().flatten() {
            if !rec.applicable && chunks_read < total_chunks {
                rec.recommendation_reasoning.push_str(&format!(
                    " Judged over {chunks_read} of {total_chunks} file chunks."
                ));
            }
        }
        recommendations.extend(best.into_iter().flatten());
    }
    Ok((recommendations, gaps.reason()))
}

/// What the recommendation pass could not settle because a reply was cut off at the output limit
/// and the one smaller retry did not finish the job. Plain data; [`Self::reason`] words it.
#[derive(Default)]
pub(super) struct AlternativesGaps {
    /// Rules no reply answered. They keep their default directive.
    undecided: Vec<String>,
    /// Rules an earlier file chunk answered whose later-chunk reply was cut off.
    partial: Vec<String>,
    /// The most sends one rule batch needed (the first, then the retry).
    attempts: usize,
    /// Why the retry was not sent or did not complete, when that is the case.
    retry_note: Option<String>,
}

impl AlternativesGaps {
    /// The sentence the run records for what was left undecided, or `None` when nothing was. No
    /// failure wording: the rules named keep their default directive and the sentence says so.
    fn reason(&self) -> Option<String> {
        if self.undecided.is_empty() && self.partial.is_empty() {
            return None;
        }
        let list = |ids: &[String]| {
            const SHOWN: usize = 8;
            let mut out = ids
                .iter()
                .take(SHOWN)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if ids.len() > SHOWN {
                out.push_str(&format!(" and {} more", ids.len() - SHOWN));
            }
            out
        };
        let plural = |n: usize| if n == 1 { "rule" } else { "rules" };
        let mut text = format!(
            "the model's reply deciding which option each rule follows was cut off at the output \
             limit after {} attempt{}",
            self.attempts.max(1),
            if self.attempts <= 1 { "" } else { "s" }
        );
        if let Some(note) = &self.retry_note {
            text.push_str(&format!(" ({note})"));
        }
        if !self.undecided.is_empty() {
            text.push_str(&format!(
                "; {} {} ({}) keep their default directive",
                self.undecided.len(),
                plural(self.undecided.len()),
                list(&self.undecided)
            ));
        }
        if !self.partial.is_empty() {
            text.push_str(&format!(
                "; {} {} ({}) judged on fewer file chunks than the repo has",
                self.partial.len(),
                plural(self.partial.len()),
                list(&self.partial)
            ));
        }
        Some(text)
    }
}

/// One call of the recommendation pass in the shape the shared batch runner takes: a single item.
pub(super) struct AlternativesCall<'a> {
    key: String,
    req: Option<LlmRequest>,
    reply: Option<crate::llm::LlmResponse>,
    feedback: Option<(&'a crate::transcript::TranscriptStore, &'a str)>,
    session: String,
}

impl stage_batch::StageItems for AlternativesCall<'_> {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.req
            .iter()
            .map(|r| (self.key.clone(), r.clone()))
            .collect()
    }

    // The stage runs its own second try.
    fn resends(&self) -> bool {
        false
    }

    fn absorb(
        &mut self,
        _key: &String,
        resp: crate::llm::LlmResponse,
    ) -> Result<(), stage_batch::ItemReject> {
        if let Some((store, key)) = self.feedback {
            store.append_output_raw(key, &self.session, &resp.text);
        }
        self.reply = Some(resp);
        Ok(())
    }

    fn retry(&self, _key: &String, _reject: &stage_batch::ItemReject) -> Option<LlmRequest> {
        None
    }
}

/// The inputs every call of the recommendation pass shares, so the first send and a retry build
/// the same request.
pub(super) struct AlternativesAsk<'a> {
    llm: &'a dyn LlmPort,
    repo: &'a str,
    repo_map: &'a str,
    digest: &'a str,
    /// How many rule batches the stage sends in all: every one of them re-sends `digest`.
    n_rule_batches: usize,
    /// How many file-chunk digests the stage asks about: every one re-sends `repo_map`.
    n_digests: usize,
    nonce: &'a crate::prompt_frame::FrameNonce,
    audit_model: Option<&'a str>,
    feedback: Option<(&'a crate::transcript::TranscriptStore, &'a str)>,
    meter: Option<&'a UsageMeter>,
    /// How the call goes out (a batch in a batch run) and the job it reports to.
    route: stage_batch::StageRoute<'a>,
}

impl AlternativesAsk<'_> {
    /// One call asking for `rules`. A transport error is recorded on the meter and returned; a
    /// reply (cut off or not) is recorded as the alternatives stage's and returned for the caller
    /// to read.
    async fn send(
        &self,
        rules: &[RuleAlternatives],
        session: &str,
        role: &str,
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        let Self {
            llm,
            repo,
            repo_map,
            digest,
            n_rule_batches,
            n_digests,
            nonce,
            audit_model,
            feedback,
            meter,
            route,
        } = *self;
        // The shared prefix (repo map, then digest) leads and the rules, which differ per batch,
        // trail, so the digest the stage re-sends with every rule batch is one cached prefix.
        let map_prefix = format!("Repository: {repo}\n\n{repo_map}");
        let static_prefix = format!("{map_prefix}{digest}\n\n");
        let cache_breakpoints = alternatives_cache_breakpoints(
            map_prefix.len(),
            static_prefix.len(),
            n_rule_batches,
            n_digests,
        );
        let prompt = format!("{static_prefix}{}", build_alternatives_block(rules));
        if let Some((store, key)) = feedback {
            store.register(
                key,
                crate::transcript::AgentTranscript {
                    session_id: session.to_string(),
                    role: role.to_string(),
                    prompt: prompt.clone(),
                    output: String::new(),
                    status: "running".to_string(),
                },
            );
        }
        let req = with_stage_effort(
            LlmRequest::new(prompt)
                .with_system(crate::prompt_frame::with_untrusted_clause(
                    alternatives_system_prompt(),
                    nonce,
                ))
                .with_max_tokens(alternatives_max_tokens(rules.len()))
                .with_temperature(AUDIT_TEMPERATURE)
                .with_response_schema(reply_schemas::alternatives_reply_schema())
                .with_cache_breakpoints(cache_breakpoints)
                .with_model(stage_model(audit_model)),
            Stage::Alternatives,
        );
        // Never send what the model's window cannot take (M1).
        request_budget::ensure_request_fits(&req)?;
        // One call through the shared batch runner: it checks the spend limit, sends through the
        // STREAMING transport EVEN WHEN there is no live transcript to feed (the CLI leg's
        // heartbeat resets on any progress and SALVAGES partial output on a late kill, instead
        // of the flat path's fixed 300s deadline), records the usage, and writes the reply down
        // in the run's checkpoint so a restart does not pay for it again.
        let key = crate::checkpoint::batch_key(session, rules.iter().map(|r| r.rule_id.as_str()));
        let mut call = AlternativesCall {
            key: key.clone(),
            req: Some(req),
            reply: None,
            feedback,
            session: session.to_string(),
        };
        let checkpoint = crate::checkpoint::stage_checkpoint(&format!("alternatives_{repo}"));
        let model = stage_model(audit_model);
        // A one-item Message Batch when the run is a batch run and the seam has a batch API
        // (chosen per call, as stage 1 does); a real-time run sends the one call as before.
        let (mode, batch_api) = stage_batch::mode_and_api(llm, route.batch, 1);
        let ctx = stage_batch::StageCtx {
            repo,
            model: &model,
            realtime: llm,
            batch: batch_api,
            meter,
            job: route.job,
            checkpoint: checkpoint.as_ref(),
            settings: stage_batch::StageSettings::default(),
        };
        let outcome = stage_batch::run_stage(Stage::Alternatives, &mut call, mode, &ctx).await;
        if let Some((store, skey)) = feedback {
            store.set_status(
                skey,
                session,
                if call.reply.is_some() {
                    "done"
                } else {
                    "blocked"
                },
            );
        }
        let Some(resp) = call.reply.take() else {
            let failed = outcome.failed.into_iter().next();
            return Err(match failed {
                Some(f) => {
                    let mut failure = camerata_llm::failure::LlmCallFailure::new(f.class, f.reason);
                    failure.attempts = f.attempts;
                    let err = anyhow::Error::new(failure);
                    match f.live_batch {
                        // The batch may still be running and billed: no second try covers it.
                        Some(batch_id) => err.context(super::failures::LiveBatchHeld(batch_id)),
                        None => err,
                    }
                }
                None => anyhow::anyhow!("the recommendation call did not run"),
            });
        };
        Ok(resp)
    }
}

/// The cache breakpoints of an alternatives request: byte offsets into its prompt, each the end
/// of a prefix a later request of the stage repeats exactly. The repo map is repeated by every
/// request over another file-chunk digest (`n_digests > 1`); the digest after it is repeated by
/// every further rule batch (`n_rule_batches > 1`), which is the saving: the stage re-sent the
/// whole digest, 25k to 40k input tokens, uncached with each rule batch. A stage that sends one
/// request over one digest repeats nothing (a cache write costs more than the plain input and
/// nothing would read it), so it carries none. The same placement as the scan's
/// ([`cache_breakpoints_for_pass`]); the schema and the system prompt are the same on every
/// request of the stage, so the prefix is one cache entry.
pub(super) fn alternatives_cache_breakpoints(
    map_prefix_len: usize,
    static_prefix_len: usize,
    n_rule_batches: usize,
    n_digests: usize,
) -> Vec<usize> {
    let mut breakpoints = Vec::new();
    if n_digests > 1 {
        breakpoints.push(map_prefix_len);
    }
    if n_rule_batches > 1 {
        breakpoints.push(static_prefix_len);
    }
    breakpoints
}

/// What a cut-off recommendation reply still answers: the recommendations for the rules in
/// `pending` whose entry closed before the cut (read by the same brace-balanced walker the scan
/// uses, [`complete_array_objects`]), and the rules it never reached. A rule the reply did not
/// reach gets NO entry here (not the fallback answer a whole reply's omission gets): the caller
/// asks again or leaves the rule at its default directive and says so.
pub(super) fn salvage_alternative_answers(
    text: &str,
    pending: &[RuleAlternatives],
) -> (Vec<RuleRecommendation>, Vec<RuleAlternatives>) {
    let mut kept: Vec<&str> = Vec::new();
    let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for object in complete_array_objects(text, "recommendations") {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(object) else {
            continue;
        };
        let Some(id) = v["rule_id"].as_str().map(str::trim) else {
            continue;
        };
        if pending.iter().any(|a| a.rule_id == id) && answered.insert(id.to_string()) {
            kept.push(object);
        }
    }
    let (done, rest): (Vec<RuleAlternatives>, Vec<RuleAlternatives>) = pending
        .iter()
        .cloned()
        .partition(|a| answered.contains(&a.rule_id));
    if kept.is_empty() {
        return (Vec::new(), rest);
    }
    let synthetic = format!("{{\"recommendations\":[{}]}}", kept.join(","));
    (parse_alternative_recommendations(&synthetic, &done), rest)
}
