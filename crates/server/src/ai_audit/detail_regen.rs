//! Rank 4 (`terse_output`): the full `detail` of a finding, written once for the findings that
//! survive the first merge, after a scan that only asked for a one-sentence one.
//!
//! # Where this runs, and why there
//!
//! In `audit_repo`, between `merge_by_location` and `verify_findings` (calibration). That is the
//! earliest point at which "the findings that survive" is known, and the latest point at which
//! every consumer that decides something from the free text can still be handed the full text:
//!
//! | consumer | runs | on |
//! |---|---|---|
//! | `merge_by_location` (exact `(path, line)` merge, `primary_rank`) | before | short: it reads path, line, snippet, rule id, severity, never `detail` |
//! | calibration prompt, `apply_verdicts` precondition check, D5 floor, D6 ceiling | after | full: verdicts are judged from the explanation |
//! | `merge_semantic_groups` (`semantic_pair_merges`: prose overlap, object veto) | after | full: the signals are lexical overlap and named objects in the prose |
//! | `fix_evidence_blob` (the fix self-check's grounding set) | after | full: a fix may name anything the explanation names |
//!
//! The reasons and the tests that pin each row are in `docs/TECHNICAL.md`, "Terse scan output".
//!
//! # Never short, never dropped
//!
//! A finding the regeneration could not rewrite (the call failed, the spend limit stopped the
//! stage, the model left it out twice) stays in the result with its short detail and a visible
//! [`DETAIL_INCOMPLETE_MARKER`], and the run reports a `FailedPass`, the same path every other
//! degraded stage uses. So the export can hold a short detail only with the marker beside it.

use super::*;

/// The marker appended to a finding whose full detail could not be written. Two words on
/// purpose: it reaches `description_overlap_score`, and a longer marker shared by two failed
/// findings would raise their prose overlap and could fuse them.
pub(crate) const DETAIL_INCOMPLETE_MARKER: &str = "[detail incomplete]";

/// The `FailedPass::pass` name the export's disclosure shows.
pub(crate) const DETAIL_FAILED_PASS: &str = "detail regeneration";

/// How many findings one regeneration call covers. Batched, not one call per finding: see
/// "Cost arithmetic" in `docs/TECHNICAL.md` and `batching_beats_one_call_per_finding`.
const DETAIL_BATCH_SIZE: usize = 25;

/// Output budget per finding: 2 to 4 sentences is about 100 tokens, with headroom.
const DETAIL_TOKENS_PER_FINDING: u32 = 160;

/// Lines of code either side of the finding the regeneration shows (the scan saw the whole
/// digest; this call sees only this window, so cross-file facts must already be in the short
/// detail the scan wrote). Smaller than the fix prompt's 8: this call is paid for on every
/// survivor, and the window is its main input.
const DETAIL_CONTEXT_RADIUS: usize = 6;

/// Attempts per finding: the first, and one repeat for what the first left out or failed on.
const DETAIL_ROUNDS: usize = 2;

/// The system prompt of the regeneration call.
pub(super) fn detail_regen_system_prompt() -> String {
    r#"You write the full explanation for audit findings that were reported in one short sentence. A human architect reads it to decide what to do, so it must be specific to the code shown.

For each finding below write `detail`: two to four sentences saying why THIS code is a problem and what the fix direction is.

Rules, every finding:
- Keep every fact the short statement already makes: each file name, symbol, flag and other file it depends on stays in your explanation. Never contradict it.
- Ground the rest in the evidence given for that finding (its rule, snippet and the surrounding code). NEVER state a fact about code you were not shown: you see only a few lines around the finding, not the files it mentions.
- If the statement says the case is borderline, keep saying so and say what would settle it.
- Do not repeat the statement word for word and do not restate the rule's text.
- Say WHAT is wrong and the DIRECTION of the fix. Do not mention Camerata, a scanner, the audit or any detection mechanism.
- NEVER repeat a credential value, not even partly. Name the variable and the line instead.

Return ONLY JSON, no prose:
{"details":[{"index":0,"detail":"..."}]}
One entry per finding, addressed by its [index]."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// The one-line statement at the front of `Finding::detail`: the part [`parse_ai_findings`]
/// wrote from the model's `title`, before [`HEADLINE_SEP`]. A text with no separator is a title
/// on its own (or, rarely, a detail with no title); either way the whole text is kept as the
/// headline so nothing the scan said is lost.
fn headline_of(detail: &str) -> &str {
    detail
        .split_once(HEADLINE_SEP)
        .map_or(detail, |(headline, _)| headline)
}

/// The detail a regenerated explanation produces: the finding's headline, then the explanation.
fn compose_detail(old_detail: &str, full: &str) -> String {
    let headline = headline_of(old_detail).trim();
    if headline.is_empty() {
        full.to_string()
    } else {
        format!("{headline}{HEADLINE_SEP}{full}")
    }
}

/// Put the visible incomplete marker on a finding that kept its short detail. Idempotent.
pub(crate) fn mark_detail_incomplete(f: &mut Finding) {
    if f.detail.contains(DETAIL_INCOMPLETE_MARKER) {
        return;
    }
    if f.detail.trim().is_empty() {
        f.detail = DETAIL_INCOMPLETE_MARKER.to_string();
    } else {
        f.detail = format!("{} {DETAIL_INCOMPLETE_MARKER}", f.detail.trim_end());
    }
}

/// The per-finding evidence the regeneration prompt shows, in batch order. Everything is
/// redacted: the snippet, the statement and (via the caller's model-facing file set) the code.
fn build_detail_regen_block(
    findings: &[&Finding],
    files: &[(String, String)],
    directives: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::new();
    for (i, f) in findings.iter().enumerate() {
        out.push_str(&format!(
            "[{i}] rule {} — severity {} — {}:{}\n",
            f.rule_id, f.severity, f.path, f.line
        ));
        if let Some(directive) = directives.get(&f.rule_id.to_ascii_uppercase()) {
            out.push_str(&format!(
                "Rule: {}\n",
                camerata_secrets::redact_str(directive)
            ));
        }
        out.push_str(&format!(
            "Short statement: {}\n",
            camerata_secrets::redact_str(&f.detail)
        ));
        if !f.snippet.is_empty() {
            out.push_str(&format!(
                "Code: {}\n",
                camerata_secrets::redact_str(&f.snippet)
            ));
        }
        if !f.captures.is_empty() {
            let caps = f
                .captures
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("Captured objects: {caps}\n"));
        }
        let context = fix_context_window(files, &f.path, f.line, DETAIL_CONTEXT_RADIUS);
        if !context.is_empty() {
            out.push_str(&format!("Surrounding code:\n{context}"));
        }
        out.push('\n');
    }
    out
}

/// Parse `{"details":[{"index":N,"detail":"..."}]}` into batch-local index -> text. Malformed
/// output, a missing array or a blank entry leaves that index absent: the caller treats an
/// absent index as "not written", never as an empty explanation.
fn parse_detail_regen(raw: &str) -> std::collections::HashMap<usize, String> {
    let mut out = std::collections::HashMap::new();
    let Some(json) = extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    let Some(arr) = v["details"].as_array() else {
        return out;
    };
    for entry in arr {
        let (Some(idx), Some(text)) = (entry["index"].as_u64(), entry["detail"].as_str()) else {
            continue;
        };
        let text = text.trim();
        if !text.is_empty() {
            out.insert(idx as usize, text.to_string());
        }
    }
    out
}

/// One round: the `pending` findings in bounded batches. Writes the regenerated detail into
/// `findings` and returns the indices still without one.
#[allow(clippy::too_many_arguments)]
async fn run_detail_round(
    llm: &dyn LlmPort,
    repo: &str,
    system: &str,
    model: Option<&str>,
    meter: Option<&UsageMeter>,
    files: &[(String, String)],
    directives: &std::collections::HashMap<String, String>,
    findings: &mut [Finding],
    pending: &[usize],
) -> Vec<usize> {
    let mut still_pending = Vec::new();
    for batch in pending.chunks(DETAIL_BATCH_SIZE) {
        // SPEND CAP, between batches: a refused batch stays pending (it ends up marked
        // incomplete, never dropped) and the stage is recorded as stopped part way.
        let _permit = match admit_stage(meter, repo, STAGE_DETAIL).await {
            Err(CapReached) => {
                still_pending.extend(batch.iter().copied());
                continue;
            }
            Ok(permit) => permit,
        };
        let prompt = {
            let refs: Vec<&Finding> = batch.iter().map(|&i| &findings[i]).collect();
            format!(
                "Repository: {repo}\n\nWrite the full detail for each finding below:\n\n{}",
                build_detail_regen_block(&refs, files, directives)
            )
        };
        let mut req = LlmRequest::new(prompt)
            .with_system(system.to_string())
            .with_max_tokens(scaled_max_tokens(batch.len(), DETAIL_TOKENS_PER_FINDING));
        if let Some(m) = model {
            req = req.with_model(m.to_string());
        }
        // Streaming transport, as the other aggregate stages: the CLI leg's heartbeat and
        // partial-output salvage apply, and a flat deadline does not.
        let mut on_delta = |_: &str| {};
        let resp = match llm.complete_streaming(req, &mut on_delta).await {
            Ok(r) => r,
            Err(_) => {
                still_pending.extend(batch.iter().copied());
                continue;
            }
        };
        if let Some(m) = meter {
            m.record_stage(Stage::DetailRegeneration, &resp);
        }
        let parsed = parse_detail_regen(&resp.text);
        for (pos, &idx) in batch.iter().enumerate() {
            let Some(text) = parsed.get(&pos) else {
                still_pending.push(idx);
                continue;
            };
            // The text is client-visible and may repeat the offending value: redact it.
            let full = camerata_secrets::redact_str(text).into_owned();
            findings[idx].detail = compose_detail(&findings[idx].detail, &full);
        }
    }
    still_pending
}

/// Write the full `detail` of every finding in `findings`, which the scan wrote as one short
/// sentence. `files` is the model-facing (redacted) file set; `directives` maps an uppercase
/// rule id to the directive the scan checked it against.
///
/// Never drops a finding. Returns the findings, each with its regenerated detail or, when that
/// failed, its short detail plus [`DETAIL_INCOMPLETE_MARKER`], and one `FailedPass` naming how
/// many.
pub(crate) async fn regenerate_full_details(
    llm: &dyn LlmPort,
    repo: &str,
    mut findings: Vec<Finding>,
    files: &[(String, String)],
    directives: &[(String, String)],
    model: Option<&str>,
    meter: Option<&UsageMeter>,
) -> (Vec<Finding>, Vec<FailedPass>) {
    if findings.is_empty() {
        return (findings, Vec::new());
    }
    let directives: std::collections::HashMap<String, String> = directives
        .iter()
        .map(|(id, directive)| (id.to_ascii_uppercase(), directive.clone()))
        .collect();
    let system = detail_regen_system_prompt();
    let total = findings.len();
    let mut pending: Vec<usize> = (0..total).collect();
    // The stage boundary check: stopped by the spend limit before it began, every finding keeps
    // its short detail, marked.
    let blocked = meter.is_some_and(|m| m.stage_blocked(repo, STAGE_DETAIL));
    if !blocked {
        for _ in 0..DETAIL_ROUNDS {
            if pending.is_empty() {
                break;
            }
            pending = run_detail_round(
                llm,
                repo,
                &system,
                model,
                meter,
                files,
                &directives,
                &mut findings,
                &pending,
            )
            .await;
        }
    }
    let mut failed_passes = Vec::new();
    if !pending.is_empty() {
        for &idx in &pending {
            mark_detail_incomplete(&mut findings[idx]);
        }
        failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: DETAIL_FAILED_PASS.to_string(),
            reason: format!(
                "the full explanation could not be written for {} of {total} finding(s) this \
                 run{}; they are in the report with the one-sentence detail the scan wrote and \
                 the {DETAIL_INCOMPLETE_MARKER} marker",
                pending.len(),
                if blocked {
                    " (the spend limit set for this run stopped the step before it began)"
                } else {
                    ""
                },
            ),
        });
    }
    (findings, failed_passes)
}
