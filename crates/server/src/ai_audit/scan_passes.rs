//! The scan passes: chunking, the pass prompt, one audit pass, reply salvage and retry, and the real-time and batch runners.

use super::*;

/// Partition `files` into contiguous chunks each whose RAW size is at most `budget` bytes,
/// so each chunk's digest fits a single model context and the WHOLE repo gets audited. A
/// file larger than `budget` becomes its own chunk (its digest then clips at the per-call
/// cap). The repo is never partially dropped — every file lands in exactly one chunk.
pub(super) fn chunk_files(files: &[(String, String)], budget: usize) -> Vec<&[(String, String)]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut acc = 0usize;
    for (i, (path, content)) in files.iter().enumerate() {
        let sz = path.len() + content.len() + 32; // ≈ header + content
        if acc > 0 && acc + sz > budget {
            chunks.push(&files[start..i]);
            start = i;
            acc = 0;
        }
        acc += sz;
    }
    if start < files.len() {
        chunks.push(&files[start..]);
    }
    chunks
}

/// Coarse total-time backstop for the deep tier's no-feedback calls (a bounded 8,192-token
/// reply). The scan, calibration, alternatives, detail and fix stages do not use it: they
/// stream, which shows progress, so only silence fails them. Streaming calls do
/// NOT use this — they self-bound on an idle/stall timeout inside the transport, which
/// scales with repo size (a big scan keeps streaming and never trips it; only a true hang
/// does). Set high so it never kills legitimate work. `CAMERATA_LLM_MAX_SECS` (default 600).
pub(super) fn total_backstop() -> std::time::Duration {
    let secs = std::env::var("CAMERATA_LLM_MAX_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

/// The `needs_files` paths a pass asked for (file bodies it needs co-resident to judge a
/// cross-file rule). Drives the bounded resolution round. Robust to missing/garbled output.
pub(super) fn parse_needs_files(raw: &str) -> Vec<String> {
    let Some(json) = extract_json_object(raw) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["needs_files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Compute the API cache-control breakpoints (byte offsets into the full prompt) for one
/// audit pass, given where each of the two cacheable static segments ends.
///
/// GAP-3 (batch-mode cache cost): the audit prompt is built as
/// `{repo_map_segment}{chunk_segment}{varying task_line + rules}` — see
/// [`run_passes`]/[`run_passes_batch`]'s prompt builders, which are the only callers. The
/// repo-map segment is now DELIBERATELY FIRST because it's byte-IDENTICAL across every
/// chunk in the scan (it never embeds the chunk index or the chunk's digest), so caching it
/// lets chunk 2, 3, ... of the SAME scan cache-HIT on it instead of re-billing the (often
/// large) repo map on every chunk — something the OLD ordering (chunk index embedded before
/// the repo map) could never do, since that made the "static" prefix differ at ~byte 30 on
/// every chunk.
///
/// The chunk segment (the digest) is stable across THIS chunk's rule-batches only, so
/// its breakpoint is worth adding when there's more than one rule-batch (`n_b > 1`) — that's
/// the existing parallel-mode win (a chunk's digest re-read across rule-batch 2, 3, ...).
/// When `n_b <= 1` (batch mode's default: `BATCH_RULE_BATCH_SIZE = usize::MAX`, i.e. every
/// rule in one rule-batch), that segment is written to the cache exactly once and NEVER
/// read back — Anthropic bills a ~1.25x write premium on a 5m cache entry that buys
/// nothing, so this drops the second breakpoint entirely and folds the chunk segment into
/// the uncached suffix instead of paying to cache something that's read zero times.
///
/// The repo-map breakpoint is kept unconditionally (independent of `n_b`) since it re-reads
/// across CHUNKS, a dimension `n_b` says nothing about.
pub(super) fn cache_breakpoints_for_pass(
    repo_map_prefix_len: usize,
    static_prefix_len: usize,
    n_b: usize,
) -> Vec<usize> {
    let mut bp = vec![repo_map_prefix_len];
    if n_b > 1 {
        bp.push(static_prefix_len);
    }
    bp
}

/// Build the full prompt AND its API cache-control breakpoints for one (chunk, rule-batch)
/// audit pass. Shared VERBATIM by `run_passes` (parallel mode) and `run_passes_batch` (batch
/// mode) so the two scan modes structurally cannot drift on prompt shape or cache-breakpoint
/// placement — see [`cache_breakpoints_for_pass`] for the caching rationale.
///
/// Prompt shape (cache-aware ordering, shared content first):
///   `Repository: {repo}\n\n{repo_map}` (IDENTICAL across every chunk)
///   + `{digest}\n\n` (stable across this chunk's rule-batches AND across routing groups)
///   + `({label} {ci+1}/{n_c})\n\n{task_line}\n\n{rules_block}` (varies per pass, never cached)
///
/// The group label and chunk index used to head the digest segment, inside the cached prefix.
/// `label` differs per routing group (`pass[all]`, `pass[web]`), so a second group scanning the
/// same files could never read the first group's digest even though the digest bytes were
/// identical (`SPIKE_TOKEN_EFFICIENCY_RESULTS.md`, "Why cache writes far exceed reads", fact 4).
/// It now leads the uncached suffix. This is a MOVE of one line, nothing added or removed:
/// `cache_schedule_tests::the_prompt_carries_exactly_the_same_parts_as_before_only_the_label_line_moved`
/// pins it against the previous layout.
///
/// # Why the rules block is not cached
///
/// The digest leads and the rules trail because the digest is the larger share and is the one
/// reused the most: a chunk's digest (up to 350K chars) is re-read by every one of its
/// rule-batches, while a rule-batch's rules (a few thousand tokens) could be re-read only across
/// chunks, and only if the rules LED the prompt, which would also move the rules away from the
/// code they are checked against. A breakpoint after the rules would cache a prefix no other
/// request shares. Rules-first is therefore not built; see the telemetry before revisiting.
///
/// Returns `(prompt, cache_breakpoints)`.
///
/// This is the prompt with the `label_after_prefix` efficiency measure ON (every test and the
/// reference layout use it); [`build_pass_prompt_with`] is the one implementation and takes the
/// switch.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn build_pass_prompt(
    repo: &str,
    repo_map: &str,
    label: &str,
    ci: usize,
    n_c: usize,
    digest: &str,
    n_b: usize,
    task_line: &str,
    rules_block: &str,
) -> (String, Vec<usize>) {
    build_pass_prompt_with(
        true,
        repo,
        repo_map,
        label,
        ci,
        n_c,
        digest,
        n_b,
        task_line,
        rules_block,
    )
}

/// [`build_pass_prompt`] with the `label_after_prefix` switch explicit. ON (the default): the
/// label line leads the uncached suffix. OFF: the label line sits between the repo map and the
/// digest, inside the cached digest segment, exactly as before rank 3 (so routing groups over
/// the same files can no longer share a cached digest). Same bytes either way, rearranged.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_pass_prompt_with(
    label_after_prefix: bool,
    repo: &str,
    repo_map: &str,
    label: &str,
    ci: usize,
    n_c: usize,
    digest: &str,
    n_b: usize,
    task_line: &str,
    rules_block: &str,
) -> (String, Vec<usize>) {
    let repo_map_prefix = format!("Repository: {repo}\n\n{repo_map}");
    let label_line = format!("({label} {}/{n_c})\n\n", ci + 1);
    if !label_after_prefix {
        let digest_segment = format!("{label_line}{digest}\n\n");
        let static_prefix = format!("{repo_map_prefix}{digest_segment}");
        let cache_breakpoints =
            cache_breakpoints_for_pass(repo_map_prefix.len(), static_prefix.len(), n_b);
        return (
            format!("{static_prefix}{task_line}\n\n{rules_block}"),
            cache_breakpoints,
        );
    }
    let digest_segment = format!("{digest}\n\n");
    let static_prefix = format!("{repo_map_prefix}{digest_segment}");
    let cache_breakpoints =
        cache_breakpoints_for_pass(repo_map_prefix.len(), static_prefix.len(), n_b);
    let prompt = format!("{static_prefix}{label_line}{task_line}\n\n{rules_block}");
    (prompt, cache_breakpoints)
}

/// The instruction line that closes a scan pass's prompt. ONE definition for the real-time
/// passes, the batch items and the pre-flight sample, so the three can never disagree about what
/// a pass asks. `advisory` adds the "also flag other genuine issues" task (first rule-batch of a
/// chunk only). `terse` is the `terse_output` switch: the terse system prompt carries no
/// `proposed_rules` array, so the advisory line (the only pass that may propose rules) adds it
/// back here, in the uncached suffix, where it cannot disturb the shared cache prefix. With
/// `terse` off the line is exactly what it always was.
pub(super) fn pass_task_line(advisory: bool, bi: usize, n_b: usize, terse: bool) -> String {
    if advisory {
        let line = format!("── Check the code above against the ADOPTED rules below (batch {}/{n_b}); ALSO flag any other genuine issues NOT covered by an adopted rule. Use the REPO MAP for cross-file context. ──", bi + 1);
        if terse {
            format!("{line}\n{ADVISORY_PROPOSED_RULES_INSTRUCTION}")
        } else {
            line
        }
    } else {
        format!("── Check the code above against ONLY the ADOPTED rules below (batch {}/{n_b}). Do NOT report issues outside these rules — a separate pass already covers novel findings. Use the REPO MAP for cross-file context. ──", bi + 1)
    }
}

/// The request the exact-token sample counts for a pass whose prompt and cache layout are
/// already built: the audit system prompt plus the prompt, split at the same breakpoints the
/// real call uses. ONE builder for the batch path's sample and the pre-flight sample.
pub(super) fn sample_count_request(
    prompt: &str,
    cache_breakpoints: &[usize],
    terse: bool,
    nonce: &crate::prompt_frame::FrameNonce,
) -> LlmRequest {
    LlmRequest::new(prompt.to_string())
        .with_system(crate::prompt_frame::with_untrusted_clause(
            audit_system_prompt_with(terse),
            nonce,
        ))
        .with_cache_breakpoints(cache_breakpoints.to_vec())
}

/// A representative scan pass for a repo: the first (chunk, rule-batch) pair, built exactly as
/// the scan builds it (redacted files, repo map, digest, rules block, cache layout), so
/// `count_tokens` on it is the real input size of one pass. Used by the pre-flight estimate in
/// `camerata inspect`.
pub struct ScanSample {
    pub request: LlmRequest,
    /// Chunks times rule-batches for the main scan (the real pass count, before routing).
    pub passes: usize,
}

/// The [`ScanSample`] for `files` under `selected`; `None` when there are no files.
pub fn representative_scan_sample(
    repo: &str,
    files: &[(String, String)],
    selected: &[(String, String)],
    mode: ScanMode,
    efficiency: crate::efficiency::EfficiencyConfig,
) -> Option<ScanSample> {
    if files.is_empty() {
        return None;
    }
    let model_files = crate::onboard::redact::files_for_model(files);
    // A token of the real length, so the counted size is the real size.
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding_files(&[&model_files]);
    let repo_map = RepoMap::new(&model_files, &nonce, repo_map::REPO_TREE_TOKEN_BUDGET);
    let chunks = chunk_files(&model_files, CHUNK_DIGEST_CHARS);
    let first_chunk = chunks.first()?;
    let (_, batch_size) = mode.tuning(efficiency);
    let empty: &[(String, String)] = &[];
    let batches: Vec<&[(String, String)]> = if selected.is_empty() {
        vec![empty]
    } else {
        selected.chunks(batch_size.max(1)).collect()
    };
    let first_digest = build_digest(first_chunk, &nonce);
    let digest = chunk_segment(
        &repo_map.symbols_for(first_chunk, &nonce).text,
        &first_digest,
    );
    let (prompt, bps) = build_pass_prompt_with(
        efficiency.label_after_prefix,
        repo,
        repo_map.prefix(),
        "pass",
        0,
        chunks.len(),
        &digest,
        batches.len(),
        &pass_task_line(true, 0, batches.len(), efficiency.terse_output),
        &build_rules_block(batches[0]),
    );
    Some(ScanSample {
        request: sample_count_request(&prompt, &bps, efficiency.terse_output, &nonce),
        passes: chunks.len() * batches.len(),
    })
}

/// The request one scan pass sends, built from its prompt. Its own function so the pass can ask
/// whether the run store already holds a reply to exactly this request before the spend limit
/// admits it ([`LlmPort::replays`]).
pub(super) fn build_audit_request(
    audit_model: Option<&str>,
    prompt: String,
    cache_breakpoints: Vec<usize>,
    nonce: &crate::prompt_frame::FrameNonce,
    meter: Option<&UsageMeter>,
    n_rules: usize,
) -> LlmRequest {
    let mut req = with_stage_effort(
        LlmRequest::new(prompt)
            .with_system(crate::prompt_frame::with_untrusted_clause(
                audit_system_prompt_with(terse_output_of(meter)),
                nonce,
            ))
            .with_max_tokens(scan_max_tokens(n_rules))
            .with_temperature(AUDIT_TEMPERATURE)
            .with_response_schema(reply_schemas::scan_reply_schema())
            .with_model(stage_model(audit_model)),
        Stage::ScanPass,
    );
    if !cache_breakpoints.is_empty() {
        req = req.with_cache_breakpoints(cache_breakpoints);
    }
    req
}

/// One audit pass: build the request, run it (streaming into the transcript when feedback
/// is present), and parse out findings + proposed rules + any `needs_files` request. Shared
/// by the primary chunk loop and the resolution round so neither duplicates the call logic.
///
/// `cache_breakpoints` — byte offsets into `prompt` marking the end of each cacheable
/// segment (see [`cache_breakpoints_for_pass`] for how callers compute these), forwarded to
/// [`LlmRequest::with_cache_breakpoints`]. On the API backend this tells the provider to
/// cache each segment and re-read it cheaply on every subsequent call that shares it. The
/// CLI backend ignores this (no-op). Pass an empty slice to disable caching (default).
#[allow(clippy::too_many_arguments)]
pub(super) async fn audit_pass(
    llm: &dyn LlmPort,
    audit_model: Option<&str>,
    prompt: String,
    cache_breakpoints: Vec<usize>,
    nonce: &crate::prompt_frame::FrameNonce,
    repo: &str,
    adopted: &std::collections::HashSet<String>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    session: &str,
    meter: Option<&UsageMeter>,
    // How many rules the prompt holds; the reply's output cap scales with it ([`scan_max_tokens`]).
    n_rules: usize,
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)> {
    let req = build_audit_request(
        audit_model,
        prompt,
        cache_breakpoints,
        nonce,
        meter,
        n_rules,
    );
    let resp = match send_audit_request(llm, req.clone(), feedback, session, meter).await {
        Ok(r) => r,
        Err(e) => {
            meter_failed(meter, Stage::ScanPass, &e);
            return Err(e);
        }
    };
    if let Some(m) = meter {
        m.record_stage(Stage::ScanPass, &resp);
    }
    let ScanReply {
        resp,
        malformed_retries,
        truncation_retried,
        stopped_by_cap,
    } = retry_unusable_scan_reply(
        llm,
        &req,
        resp,
        feedback,
        session,
        meter,
        MALFORMED_REPLY_RETRIES,
    )
    .await;
    // The spend limit stopped a retry the reply needed: the pass has no usable answer, and it
    // says why rather than parsing a cut-off or malformed reply as "no findings".
    if stopped_by_cap {
        return Err(retry_blocked_error(meter));
    }
    // A reply that is still not JSON is a FAILED pass, never "no findings": parsing it would
    // return an empty list indistinguishable from a clean chunk. The caller records it as a
    // failed pass naming the files it did not review.
    if !resp.truncated && scan_output_is_malformed(&resp.text) {
        anyhow::bail!("{}", malformed_reply_reason(1 + malformed_retries));
    }
    // A reply STILL cut off at the output limit after the retry is not a clean pass: the part
    // that never arrived may hold findings. The complete findings that did arrive are kept and
    // the pass is reported as failed, naming the files it did not finish (H1).
    if resp.truncated {
        let attempts = 1 + usize::from(truncation_retried) + malformed_retries;
        return Err(PartialPassReply::from_cut_off(repo, &resp.text, adopted, attempts).into());
    }
    let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
    let needs = parse_needs_files(&resp.text);
    Ok((f, p, needs))
}

/// The failure reason for a scan reply that was still cut off at the output limit after
/// `attempts` sends in all, with `recovered` complete findings salvaged from the part that
/// arrived.
pub(crate) fn cut_off_reply_reason(attempts: Option<usize>, recovered: usize) -> String {
    let sent = match attempts {
        Some(1) => " after 1 attempt".to_string(),
        Some(n) => format!(" after {n} attempts"),
        None => String::new(),
    };
    format!(
        "the model's reply was cut off at the output limit{sent}; {recovered} finding{} \
         recovered, the remainder of this pass was not reviewed",
        if recovered == 1 { "" } else { "s" }
    )
}

/// A scan pass whose reply was cut off at the output limit and stayed cut off: the complete
/// findings that arrived (`findings`), and the reason the pass is nevertheless a FAILED one. It
/// travels as the `Err` of a pass so every pass runner keeps the recovered findings AND records
/// the failure; nothing that arrived is dropped and the unreviewed remainder is never read as
/// "no findings".
#[derive(Debug)]
pub(super) struct PartialPassReply {
    pub(super) findings: Vec<Finding>,
    pub(super) proposed: Vec<ProposedRule>,
    pub(super) needs: Vec<String>,
    reason: String,
}

impl PartialPassReply {
    /// Salvage what a cut-off reply holds (see [`salvage_complete_findings`]).
    pub(super) fn from_cut_off(
        repo: &str,
        text: &str,
        adopted: &std::collections::HashSet<String>,
        attempts: usize,
    ) -> Self {
        let (findings, proposed) = salvage_complete_findings(repo, text, adopted);
        let reason = cut_off_reply_reason(Some(attempts), findings.len());
        Self {
            findings,
            proposed,
            needs: Vec::new(),
            reason,
        }
    }
}

impl std::fmt::Display for PartialPassReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for PartialPassReply {}

/// Recover every COMPLETE finding object from a reply cut off mid-way: the `{...}` entries of
/// the `findings` array that closed before the output limit hit. The last, unfinished object and
/// anything after it are not guessed at. The recovered objects go through the ordinary
/// [`parse_ai_findings`], so they are shaped, redacted and typed exactly like a whole reply's.
pub(super) fn salvage_complete_findings(
    repo: &str,
    text: &str,
    adopted: &std::collections::HashSet<String>,
) -> (Vec<Finding>, Vec<ProposedRule>) {
    let valid = complete_array_objects(text, "findings");
    if valid.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let synthetic = format!("{{\"findings\":[{}]}}", valid.join(","));
    parse_ai_findings(repo, &synthetic, adopted)
}

/// The COMPLETE `{...}` objects of the array under `key` in a reply that may have been cut off
/// mid-way, each valid JSON on its own: the shared salvage walker (the scan's findings and the
/// detail regeneration's entries both use it). The last, unfinished object and anything after it
/// are not guessed at. Anchored at the first `{`: prose before it that happens to contain the
/// key is not the key, and the key must open the object or follow a comma. Objects are parsed
/// one at a time, so a complete-looking object that is not valid JSON is skipped on its own and
/// does not cost the others. Strings and escapes are tracked, so a brace inside a string does
/// not move an object boundary.
pub(crate) fn complete_array_objects<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    let Some(obj_start) = text.find('{') else {
        return Vec::new();
    };
    let json = &text[obj_start..];
    let quoted = format!("\"{key}\"");
    let Some(key_at) = json
        .match_indices(quoted.as_str())
        .map(|(i, _)| i)
        .find(|&i| {
            json[..i]
                .trim_end()
                .chars()
                .last()
                .is_some_and(|c| c == '{' || c == ',')
        })
    else {
        return Vec::new();
    };
    let after = &json[key_at + quoted.len()..];
    let Some(open) = after.find('[') else {
        return Vec::new();
    };
    // Only whitespace and a colon may sit between the key and its array.
    if !after[..open].chars().all(|c| c.is_whitespace() || c == ':') {
        return Vec::new();
    }
    let body = &after[open + 1..];
    let bytes = body.as_bytes();
    let mut objects: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                let start = i;
                let (mut depth, mut in_str, mut esc) = (0usize, false, false);
                let mut end = None;
                while i < bytes.len() {
                    let b = bytes[i];
                    if in_str {
                        if esc {
                            esc = false;
                        } else if b == b'\\' {
                            esc = true;
                        } else if b == b'"' {
                            in_str = false;
                        }
                    } else if b == b'"' {
                        in_str = true;
                    } else if b == b'{' {
                        depth += 1;
                    } else if b == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i);
                            break;
                        }
                    }
                    i += 1;
                }
                match end {
                    Some(e) => {
                        objects.push(&body[start..=e]);
                        i = e + 1;
                    }
                    // The object never closed: the cut-off point. Nothing after it is read.
                    None => break,
                }
            }
            b']' => break,
            _ => i += 1,
        }
    }
    objects
        .into_iter()
        .filter(|o| serde_json::from_str::<serde_json::Value>(o).is_ok())
        .collect()
}

/// The COMPLETE `"key": {...}` members of the object under `key` in a reply that may have been
/// cut off mid-way, as `(member name, the member's object text)`: the keyed twin of
/// [`complete_array_objects`], for a reply that holds one property per id. The last, unfinished
/// member and anything after it are not guessed at. Strings and escapes are tracked, so a brace
/// or a quote inside a string does not move a boundary. Each member's text is valid JSON.
pub(crate) fn complete_keyed_objects<'a>(text: &'a str, key: &str) -> Vec<(String, &'a str)> {
    let Some(obj_start) = text.find('{') else {
        return Vec::new();
    };
    let json = &text[obj_start..];
    let quoted = format!("\"{key}\"");
    let Some(key_at) = json
        .match_indices(quoted.as_str())
        .map(|(i, _)| i)
        .find(|&i| {
            json[..i]
                .trim_end()
                .chars()
                .last()
                .is_some_and(|c| c == '{' || c == ',')
        })
    else {
        return Vec::new();
    };
    let after = &json[key_at + quoted.len()..];
    let Some(open) = after.find('{') else {
        return Vec::new();
    };
    // Only whitespace and a colon may sit between the key and its object.
    if !after[..open].chars().all(|c| c.is_whitespace() || c == ':') {
        return Vec::new();
    }
    let body = &after[open + 1..];
    let bytes = body.as_bytes();
    let mut members: Vec<(String, &str)> = Vec::new();
    let mut i = 0usize;
    'members: while i < bytes.len() {
        // Skip to the next member name (a string), or stop at the closing brace of the object.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'"' {
            break;
        }
        let name_start = i + 1;
        let mut j = name_start;
        let mut esc = false;
        while j < bytes.len() {
            if esc {
                esc = false;
            } else if bytes[j] == b'\\' {
                esc = true;
            } else if bytes[j] == b'"' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let Ok(name) = serde_json::from_str::<String>(&body[name_start - 1..=j]) else {
            break;
        };
        i = j + 1;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'{' {
            break;
        }
        let start = i;
        let (mut depth, mut in_str, mut esc) = (0usize, false, false);
        while i < bytes.len() {
            let b = bytes[i];
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if b == b'"' {
                in_str = true;
            } else if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    let text = &body[start..=i];
                    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
                        members.push((name, text));
                    }
                    i += 1;
                    continue 'members;
                }
            }
            i += 1;
        }
        // The member never closed: the cut-off point. Nothing after it is read.
        break;
    }
    members
}

/// Why a pass failed when its spend limit stopped a retry the reply needed (the reply was cut
/// off or malformed, and sending it again would have gone past the limit).
pub(crate) const SPEND_LIMIT_BEFORE_RETRY: &str =
    "spend limit reached before the reply could be retried";

/// The failure reason for a scan reply that is still not JSON after `attempts` sends in all
/// (the first plus any retries). States the attempt count so the operator sees how many calls
/// were spent.
pub(crate) fn malformed_reply_reason(attempts: usize) -> String {
    format!(
        "the model's reply was not valid JSON after {attempts} {}",
        if attempts == 1 { "attempt" } else { "attempts" }
    )
}

/// What [`retry_unusable_scan_reply`] hands back.
pub(super) struct ScanReply {
    /// The reply the pass should use.
    resp: crate::llm::LlmResponse,
    /// How many malformed-reply retries were sent.
    malformed_retries: usize,
    /// True when the truncation retry was sent (the first reply was cut off and the retry ran).
    truncation_retried: bool,
    /// True when the spend limit stopped a retry the reply needed (the reply is still cut off or
    /// malformed). The caller decides what that means for its pass.
    stopped_by_cap: bool,
}

/// THE RETRY STEP every scan pass uses: a real-time pass from [`audit_pass`] and a batch item
/// from [`run_passes_batch`] alike. `resp` is the reply `req` got, already recorded on the meter.
///
/// 1. TRUNCATION RETRY. A reply cut off at `max_tokens` is recovered by the same request with a
///    higher cap, which returns the whole answer where the cut-off one parses to nothing and the
///    chunk's findings are lost. At most one retry, and a retry that itself fails leaves the
///    first response in place.
/// 2. MALFORMED-REPLY RETRY (H4). A complete reply (not cut off) that holds no JSON object the
///    parser can read is asked again with [`MALFORMED_REPLY_NOTE`] added to the prompt (never
///    the identical request: a retry after a content failure changes the request), at most
///    `malformed_retries_allowed` times (a batch item passes zero: its reply is reported as a
///    failed pass instead). A transport error on a retry ends the retries and leaves the
///    malformed reply in place.
///
/// Every retry is one more billed call, so each checks the spend limit before sending: with the
/// limit reached the retry is not sent and `stopped_by_cap` is set.
pub(super) async fn retry_unusable_scan_reply(
    llm: &dyn LlmPort,
    req: &LlmRequest,
    mut resp: crate::llm::LlmResponse,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    session: &str,
    meter: Option<&UsageMeter>,
    malformed_retries_allowed: usize,
) -> ScanReply {
    // The limit refuses a retry only if the retry would be a FRESH send: one the run store holds
    // a reply for is answered from the store and costs nothing.
    let cap_reached =
        |next: &LlmRequest| meter.is_some_and(UsageMeter::retry_blocked) && !llm.replays(next);
    let mut wasted_counted = false;
    let mut stopped_by_cap = false;
    let mut truncation_retried = false;
    // The retry must CHANGE the request: it raises the cap. A reply already cut off at the runaway
    // ceiling has no higher cap to ask for, and the same request again would be paid for twice
    // and cut off the same way, so there it is not sent (the pass is reported as cut off).
    let retry_cap = req
        .max_tokens
        .saturating_mul(2)
        .min(AGGREGATE_MAX_TOKENS_CEILING);
    if resp.truncated && efficiency_of(meter).truncation_retry && retry_cap > req.max_tokens {
        if cap_reached(&req.clone().with_max_tokens(retry_cap)) {
            stopped_by_cap = true;
        } else {
            truncation_retried = true;
            if let Some(m) = meter {
                m.record_wasted(Stage::ScanPass, &resp);
            }
            wasted_counted = true;
            match send_audit_request(
                llm,
                req.clone().with_max_tokens(retry_cap),
                feedback,
                session,
                meter,
            )
            .await
            {
                Ok(retry) => {
                    if let Some(m) = meter {
                        m.record_stage(Stage::ScanPass, &retry);
                    }
                    resp = retry;
                    wasted_counted = false;
                }
                // The retry failed (possibly refused, and billed): the first reply stands.
                Err(e) => meter_failed(meter, Stage::ScanPass, &e),
            }
        }
    }
    let mut malformed_retries = 0usize;
    while !stopped_by_cap
        && !resp.truncated
        && scan_output_is_malformed(&resp.text)
        && malformed_retries < malformed_retries_allowed
    {
        // Never the same request again: the retry says what was wrong with the reply. Built
        // from the ORIGINAL request each time, so the note appears once whatever the count.
        let mut retry_req = req.clone();
        retry_req.prompt.push_str("\n\n");
        retry_req.prompt.push_str(MALFORMED_REPLY_NOTE);
        if cap_reached(&retry_req) {
            stopped_by_cap = true;
            break;
        }
        malformed_retries += 1;
        if let Some(m) = meter {
            m.record_wasted(Stage::ScanPass, &resp);
        }
        match send_audit_request(llm, retry_req, feedback, session, meter).await {
            Ok(retry) => {
                if let Some(m) = meter {
                    m.record_stage(Stage::ScanPass, &retry);
                }
                resp = retry;
            }
            Err(e) => {
                meter_failed(meter, Stage::ScanPass, &e);
                wasted_counted = true;
                break;
            }
        }
    }
    // Whatever reply the pass ends up using: if it is still unusable (cut off again, or not JSON
    // at all) its tokens bought nothing, and the telemetry says so.
    if !wasted_counted && (resp.truncated || scan_output_is_malformed(&resp.text)) {
        if let Some(m) = meter {
            m.record_wasted(Stage::ScanPass, &resp);
        }
    }
    ScanReply {
        resp,
        malformed_retries,
        truncation_retried,
        stopped_by_cap,
    }
}

/// One scan-pass request: streamed into the transcript when feedback is present, else bounded
/// by the coarse total backstop. Shared by the first attempt and the truncation retry.
pub(super) async fn send_audit_request(
    llm: &dyn LlmPort,
    req: LlmRequest,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    session: &str,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<crate::llm::LlmResponse> {
    // Never send what the model's window cannot take (M1).
    request_budget::ensure_request_fits(&req)?;
    // What this request could cost joins the spend limit's reserve before it is sent.
    note_request(meter, &req);
    if let Some((store, key)) = feedback {
        // Streaming: the idle/stall timeout lives inside the transport, so this scales with
        // repo size and only a genuine hang (no output for the idle window) aborts. No
        // total-time cap here — a big scan should be allowed to stream as long as it needs.
        let mut on_delta = |t: &str| store.append_output_raw(key, session, t);
        llm.complete_streaming(req, &mut on_delta).await
    } else {
        // Streamed as well, with no listener: the idle window governs here too. A fixed total
        // backstop around a reply that may legitimately run for minutes (a large `max_tokens`)
        // would fail a healthy call and pay for it twice; only silence is a failure.
        let mut on_delta = |_: &str| {};
        llm.complete_streaming(req, &mut on_delta).await
    }
}

/// Whether a scan reply holds no JSON object `parse_ai_findings` could read, i.e. the pass
/// produced nothing usable. (A reply of `{"findings":[]}` is a well-formed "nothing found", not
/// this.)
pub(super) fn scan_output_is_malformed(text: &str) -> bool {
    extract_json_object(text)
        .is_none_or(|json| serde_json::from_str::<serde_json::Value>(json).is_err())
}

/// The public symbols a file defines (Rust items + TS/JS exports), for the repo map. A
/// cheap line scan — no parser — capped so the map stays compact.
pub(super) fn extract_public_symbols(content: &str) -> Vec<String> {
    const RUST_KW: &[&str] = &[
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "pub fn ",
    ];
    const JSTS_KW: &[&str] = &[
        "export class ",
        "export interface ",
        "export type ",
        "export function ",
        "export const ",
    ];
    let ident = |rest: &str| -> Option<String> {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    let mut syms = Vec::new();
    for line in content.lines() {
        let t = line.trim_start();
        for kw in RUST_KW.iter().chain(JSTS_KW) {
            if let Some(rest) = t.strip_prefix(kw) {
                if let Some(name) = ident(rest) {
                    if !syms.contains(&name) {
                        syms.push(name);
                    }
                }
            }
        }
        if syms.len() >= 12 {
            break;
        }
    }
    syms
}

/// A compact map of the WHOLE repo — every file path plus the public symbols it defines —
/// injected into EVERY chunk. Naive file-chunking otherwise loses cross-file context: a
/// layering rule needs to know which dirs are repositories vs services across files, and a
/// "this type is defined elsewhere" finding needs to know the type exists in another file
/// not in this pass. The map gives every chunk that architecture without every file body,
/// so chunking doesn't reintroduce cross-file misses. (Bodies still only appear in their
/// own chunk — a rule needing the full body of a type in another chunk is the known limit.)
#[cfg(test)]
pub(super) fn build_repo_map(
    files: &[(String, String)],
    nonce: &crate::prompt_frame::FrameNonce,
) -> String {
    let listing: String = files
        .iter()
        .map(|(path, content)| repo_map_line(path, content))
        .collect();
    repo_map::whole_map_text(&listing, nonce)
}

/// One file's line of the repo map: its path and the public symbols it defines. ONE definition
/// for the map every scan pass reads and for the detail regeneration's view of a file the
/// finding names.
pub(super) fn repo_map_line(path: &str, content: &str) -> String {
    let syms = extract_public_symbols(content);
    if syms.is_empty() {
        format!("  {path}\n")
    } else {
        format!("  {path}  [{}]\n", syms.join(", "))
    }
}

/// Max concurrent LLM calls + rules-per-batch for the parallel mode. Tunable.
pub(super) const PARALLEL_CONCURRENCY: usize = 6;
pub(super) const RULE_BATCH_SIZE: usize = 15;

/// Rules-per-batch for `ScanMode::Batch` with `batch_default` on (rank 2): the same 15 rules per
/// prompt the parallel path uses, which is the prompt shape the fixtures were graded on. One
/// prompt per chunk holding EVERY rule shares one 8192-token output cap across all of them, so a
/// chunk with many violations truncates (SPIKE_TOKEN_EFFICIENCY_RESULTS.md, item 1).
///
/// `CAMERATA_BATCH_RULE_BATCH_SIZE` (a `usize`) overrides it at runtime in either switch state.
pub(super) const BATCH_RULE_BATCH_SIZE: usize = RULE_BATCH_SIZE;

/// Rules-per-batch for `ScanMode::Batch` with `batch_default` off: every rule in ONE item per
/// chunk, the pre-rank-2 shape (BUG-6).
pub(super) const LEGACY_BATCH_RULE_BATCH_SIZE: usize = usize::MAX;

/// How the semantic (LLM) audit executes — the SPEED/SCALE knob, orthogonal to model tier
/// (quality) and rule selection (coverage). The free deterministic floor is unaffected; it
/// runs the same in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// One call per file-chunk, ALL rules at once, chunks one after another. Simplest,
    /// gentlest on rate limits — the debug/fallback floor.
    Sequential,
    /// Rule-batches × file-chunks run CONCURRENTLY (capped). The default efficient floor:
    /// wall-clock is the slowest batch, not the sum of all calls.
    Parallel,
    /// Submit ALL (chunk × rule-batch) requests as a SINGLE Anthropic Message Batch
    /// (POST /v1/messages/batches), wait for it to complete, then reassemble. Costs 50%
    /// less on all tokens vs. real-time calls. Requires the `api` backend + key. Best for
    /// large scans where latency is acceptable in exchange for cost savings.
    ///
    /// Implementation: `run_passes_batch` compiles the cartesian product of chunks ×
    /// rule-batches into `BatchItem`s with deterministic `custom_id`s (`c{ci}-b{bi}`),
    /// submits via `Llm::submit_batch`, polls until `processing_status == "ended"`, fetches
    /// results, reassembles by `custom_id`, and feeds each response into the same
    /// `parse_ai_findings` + dedup/merge/calibrate tail as the parallel path.
    Batch,
}

impl ScanMode {
    /// The values [`Self::parse`] accepts, for its error message. `auto` (or nothing) asks for
    /// the run's default, which [`resolve_scan_mode`] decides.
    pub const WIRE_VALUES: &'static [&'static str] = &["auto", "parallel", "sequential", "batch"];

    /// Returns `(max_concurrent_calls, rules_per_batch)` under the run's efficiency switches.
    ///
    /// For real-time modes (`Sequential`, `Parallel`) this controls actual API parallelism. For
    /// `Batch` the concurrency value is unused (the API schedules the items); only
    /// `rules_per_batch` matters: [`BATCH_RULE_BATCH_SIZE`] with `batch_default` on,
    /// [`LEGACY_BATCH_RULE_BATCH_SIZE`] with it off.
    ///
    /// `CAMERATA_BATCH_RULE_BATCH_SIZE` overrides the Batch rule batch size at runtime.
    pub(super) fn tuning(self, eff: crate::efficiency::EfficiencyConfig) -> (usize, usize) {
        match self {
            ScanMode::Sequential => (1, usize::MAX),
            ScanMode::Parallel => (PARALLEL_CONCURRENCY, RULE_BATCH_SIZE),
            ScanMode::Batch => {
                let default = if eff.batch_default {
                    BATCH_RULE_BATCH_SIZE
                } else {
                    LEGACY_BATCH_RULE_BATCH_SIZE
                };
                let batch_rule_size = std::env::var("CAMERATA_BATCH_RULE_BATCH_SIZE")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(default);
                (PARALLEL_CONCURRENCY, batch_rule_size)
            }
        }
    }

    /// Parse the wire value. `Ok(None)` = no mode was asked for (absent, blank or `auto`), so
    /// the run's default applies ([`resolve_scan_mode`]). An unknown value is an ERROR naming
    /// the choices, never a silent fallback: a typo such as `batchh` must not quietly run the
    /// scan at twice the price.
    pub fn parse(s: Option<&str>) -> Result<Option<Self>, String> {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("auto") => Ok(None),
            Some("parallel") => Ok(Some(ScanMode::Parallel)),
            Some("sequential") => Ok(Some(ScanMode::Sequential)),
            Some("batch") => Ok(Some(ScanMode::Batch)),
            Some(other) => Err(format!(
                "unknown scan mode `{other}`; the scan modes are: {}",
                Self::WIRE_VALUES.join(", ")
            )),
        }
    }
}

/// Run a set of file-chunks × rule-batches as passes, up to `concurrency` at once, and
/// aggregate their findings / proposed rules / needs_files. Each pass registers its OWN
/// transcript agent (so parallel streams don't clobber each other) and finalizes its own
/// status. Shared by the main and resolution rounds. `concurrency == 1` => sequential.
///
/// `advisory_disabled` — when `true`, the advisory "flag novel issues beyond the adopted rules"
/// task is suppressed in EVERY batch of this call (even `bi==0`). This is the routing-safe
/// switch: language-scoped groups set this to `true` so the advisory pass runs in exactly one
/// place (the cross-cutting `All` group), preventing the same novel issue from being re-flagged
/// under N independently-invented names across N language groups.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_passes(
    llm: &dyn LlmPort,
    repo: &str,
    repo_map: &RepoMap,
    nonce: &crate::prompt_frame::FrameNonce,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    chunks: &[&[(String, String)]],
    // Rank 8: the candidate digests that replace the chunks' whole-file digests (one per chunk),
    // for a candidate pass set (`candidate_passes`); `None` builds the whole-file digests.
    digests_override: Option<&[String]>,
    batches: &[&[(String, String)]],
    concurrency: usize,
    label: &str,
    session_prefix: &str,
    meter: Option<&UsageMeter>,
    advisory_disabled: bool,
) -> PassesOut {
    // Chunks over the model's window become several parts that are all sent (M1).
    let (planned_chunks, digests, fit) = plan_scan_inputs(
        repo,
        repo_map,
        chunks,
        digests_override,
        batches,
        nonce,
        audit_model,
        label,
        meter,
    );
    let chunks: &[&[(String, String)]] = &planned_chunks;
    if fit.parts_added > 0 {
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, fit.parts_added * batches.len());
        }
    }
    // Per chunk, the symbols of its files and their neighbours (after the cache breakpoint).
    let symbols: Vec<repo_map::SymbolsSection> = chunks
        .iter()
        .map(|c| repo_map.symbols_for(c, nonce))
        .collect();
    let n_c = chunks.len();
    let n_b = batches.len();
    // Chunk-major work order, scheduled WARM-THEN-FAN-OUT: a chunk is one cache-prefix group
    // (its rule-batches all share the repo map and the chunk's digest), so its first rule-batch
    // runs and returns before its siblings start and read the cache entry it wrote. Without
    // this the first `concurrency` rule-batches of a chunk all went out together and ALL missed
    // the cache and wrote the same prefix at 1.25x (SPIKE_TOKEN_EFFICIENCY_RESULTS.md, item 3).
    // Scheduling only: every (chunk, rule-batch) pair still runs exactly once with the same
    // prompt.
    let jobs: Vec<crate::warm_schedule::ScheduledJob<(usize, usize)>> = (0..n_c)
        .flat_map(|c| {
            (0..n_b).map(move |b| crate::warm_schedule::ScheduledJob {
                group: c,
                payload: (c, b),
            })
        })
        .collect();
    // `None` = the spend cap refused the pass (it is recorded on the meter as NOT RUN).
    type PassOut = (
        usize,
        usize,
        Option<anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)>>,
    );
    let eff = efficiency_of(meter);
    // Set while the end-of-run second try is running, so a retried pass does not count twice in
    // the job's progress.
    let second_try = std::sync::atomic::AtomicBool::new(false);
    let run_one = |(ci, bi): (usize, usize)| {
        let digest = chunk_segment(&symbols[ci].text, &digests[ci]);
        let sym = &symbols[ci];
        let batch = batches[bi];
        let second_try = &second_try;
        async move {
            let rb = build_rules_block(batch);
            // ADVISORY RUNS ONCE PER CHUNK, not once per rule-batch. The "flag novel
            // issues beyond the adopted rules" task only depends on the code (the whole
            // chunk is visible every pass), not on which rule-batch this is — so asking
            // for it in all N batches just re-derives the SAME novel issue under N
            // independently-invented names (one `.expect()` → AI-HANDLER-PANICS +
            // AI-HANDLER-UNHANDLED-PANIC + AI-HANDLER-PANICS-ON-ERROR). Gate it to the
            // first batch of each chunk; later batches check ONLY their adopted rules.
            //
            // ROUTING INTERACTION: when `advisory_disabled` is set (language-scoped groups),
            // the advisory pass is suppressed in every batch, not just later ones. The
            // cross-cutting All group keeps advisory enabled so novel issues are surfaced
            // exactly once — against every file — with no per-language re-flagging.
            let advisory = !advisory_disabled && bi == 0;
            let task_line = pass_task_line(advisory, bi, n_b, terse_output_of(meter));
            // PROMPT ORDER IS CACHE-AWARE, with the SHARED content leading, then the
            // PER-CHUNK content, then the PER-BATCH (varying) content trailing — built by
            // `build_pass_prompt` (shared verbatim with `run_passes_batch` so the two
            // scan modes can't drift):
            //
            //   Repository: {repo}\n\n{repo_map}   <- IDENTICAL across every chunk in scan
            //   ({label} n/n_c)\n\n{digest}\n\n     <- stable across this chunk's rule-batches
            //   {task_line}\n\n{rb}                 <- varies every rule-batch, never cached
            //
            // The repo-map segment is deliberately free of the chunk index/label — that's
            // what the OLD ordering got wrong (the label sat in front of the repo map, so
            // the "static" prefix actually differed at ~byte 30 on every chunk and could
            // never cache-hit across chunks). Leading with the byte-identical repo map
            // instead means chunk 2, 3, ... of the SAME scan can cache-HIT it instead of
            // re-billing the repo map on every chunk. Bonus: rules landing last = most
            // recent context = strongest rule-following (unchanged).
            //
            // CACHING: `cache_breakpoints_for_pass` marks a breakpoint after the repo-map
            // segment (re-reads across chunks) and, when there's more than one rule-batch,
            // a second one after the chunk (label+digest) segment (re-reads across this
            // chunk's rule-batches — the existing parallel-mode win). The CLI backend
            // ignores these breakpoints entirely.
            let (prompt, cache_bps) = build_pass_prompt_with(
                eff.label_after_prefix,
                repo,
                repo_map.prefix(),
                label,
                ci,
                n_c,
                &digest,
                n_b,
                &task_line,
                &rb,
            );
            // SPEND CAP: asked before anything is sent, and held until the pass (its truncation
            // retry included) has finished, so a pass is never cut off mid-response. A refused
            // pass is recorded as NOT RUN, never skipped silently. A pass the run store already
            // holds a reply for is not a send at all: it is answered from the store whatever the
            // run's spend, so a resume past its limit still replays what it paid for.
            let replayed = llm.replays(&build_audit_request(
                audit_model,
                prompt.clone(),
                cache_bps.clone(),
                nonce,
                meter,
                batch.len(),
            ));
            let _permit = if replayed {
                None
            } else {
                match admit_pass(meter).await {
                    Err(CapReached) => {
                        // The end-of-round second try of a pass that already failed: the failure
                        // stands as its record, so the refusal is not recorded a second time.
                        if second_try.load(std::sync::atomic::Ordering::SeqCst) {
                            return (ci, bi, None);
                        }
                        if let Some(m) = meter {
                            m.record_not_run(crate::cost_cap::NotRunPass {
                                repo: repo.to_string(),
                                group: label.to_string(),
                                chunk: ci + 1,
                                n_chunks: n_c,
                                rule_batch: bi + 1,
                                n_rule_batches: n_b,
                                rule_ids: batch.iter().map(|(id, _)| id.clone()).collect(),
                                files: chunks[ci].iter().map(|(path, _)| path.clone()).collect(),
                                class: camerata_llm::failure::FailureClass::SpendLimit,
                            });
                        }
                        if let Some((jstore, jid)) = job {
                            jstore.inc_not_run(jid, 1);
                        }
                        return (ci, bi, None);
                    }
                    Ok(permit) => permit,
                }
            };
            note_map_use(meter, repo, label, ci, sym);
            let session = format!("{session_prefix}-c{ci}-b{bi}");
            if let Some((store, key)) = feedback {
                store.register(
                    key,
                    crate::transcript::AgentTranscript {
                        session_id: session.clone(),
                        role: format!("{label} {}/{n_c} · rules {}/{n_b} — {repo}", ci + 1, bi + 1),
                        prompt: prompt.clone(),
                        output: String::new(),
                        status: "running".to_string(),
                    },
                );
            }
            let r = audit_pass(
                llm,
                audit_model,
                prompt,
                cache_bps,
                nonce,
                repo,
                adopted,
                feedback,
                &session,
                meter,
                batch.len(),
            )
            .await;
            if let Some((store, key)) = feedback {
                store.set_status(key, &session, if r.is_ok() { "done" } else { "blocked" });
            }
            // Stream this pass's findings + progress into the job (live preview) as it
            // completes — so a Mode-3 poller sees findings appear incrementally. A failed
            // pass still counts toward `done` so the progress bar can reach 100%.
            if let Some((jstore, jid)) = job {
                match &r {
                    Ok((f, _, _)) => jstore.add_findings(jid, f.clone()),
                    Err(e) => {
                        if let Some(partial) = e.downcast_ref::<PartialPassReply>() {
                            jstore.add_findings(jid, partial.findings.clone());
                        }
                    }
                }
                if !second_try.load(std::sync::atomic::Ordering::SeqCst) {
                    jstore.inc_done(jid, 1);
                }
            }
            (ci, bi, Some(r))
        }
    };
    let mut results: Vec<PassOut> = if eff.warm_schedule {
        crate::warm_schedule::run_warm_then_fan_out(jobs, concurrency, &run_one).await
    } else {
        crate::warm_schedule::run_unscheduled(jobs, concurrency, &run_one).await
    };

    // ── END-OF-RUN SECOND TRY ────────────────────────────────────────────────────────────
    // Every pass that failed on the transport or on its content (after its own retries) is set
    // aside while the rest of the round runs, then tried ONCE more here, when the provider has
    // had the whole round to recover. The same admission and spend checks apply (`run_one` asks
    // first), and a stopped run sends nothing. An auth or billing failure is not tried again:
    // waiting does not fix it. A pass that fails again keeps both tries on its record.
    let mut prior_attempts: std::collections::HashMap<(usize, usize), Vec<AttemptRecord>> =
        std::collections::HashMap::new();
    let mut second_tried: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();
    let retry_list: Vec<(usize, usize)> = results
        .iter()
        .filter_map(|(ci, bi, r)| match r {
            Some(Err(e)) if classify_pass_error(e).is_retried_at_end() => Some((*ci, *bi)),
            _ => None,
        })
        .collect();
    if !retry_list.is_empty() && !meter.is_some_and(UsageMeter::outage_stopped) {
        for (ci, bi, r) in &results {
            if let (true, Some(Err(e))) = (retry_list.contains(&(*ci, *bi)), r) {
                let first = PassFailure::named(String::new(), e).caused_by(e);
                prior_attempts.insert((*ci, *bi), first.attempts);
            }
        }
        second_try.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut retry_jobs = retry_list.clone();
        retry_jobs.sort_unstable();
        let second_jobs: Vec<crate::warm_schedule::ScheduledJob<(usize, usize)>> = retry_jobs
            .into_iter()
            .map(|(c, b)| crate::warm_schedule::ScheduledJob {
                group: c,
                payload: (c, b),
            })
            .collect();
        let retried =
            crate::warm_schedule::run_unscheduled(second_jobs, concurrency, &run_one).await;
        for (ci, bi, r) in retried {
            // Refused at admission (the spend limit, an outage stop): the first failure stands.
            let Some(r) = r else { continue };
            let Some(slot) = results.iter_mut().find(|(c, b, _)| *c == ci && *b == bi) else {
                continue;
            };
            // A first reply that was cut off keeps the complete findings it carried unless the
            // second try did better or failed the same way.
            let keep_first = matches!(&slot.2, Some(Err(e)) if e.downcast_ref::<PartialPassReply>().is_some())
                && matches!(&r, Err(e2) if e2.downcast_ref::<PartialPassReply>().is_none());
            second_tried.insert((ci, bi));
            if !keep_first {
                slot.2 = Some(r);
            }
        }
    }

    let mut findings = Vec::new();
    let mut proposed = Vec::new();
    let mut requested = std::collections::HashSet::new();
    let mut ok = 0usize;
    let mut failures: Vec<PassFailure> = Vec::new();
    for (ci, bi, r) in results {
        match r {
            Some(Ok((f, p, needs))) => {
                findings.extend(f);
                proposed.extend(p);
                requested.extend(needs);
                ok += 1;
            }
            Some(Err(e)) => {
                let mut failure = PassFailure::at(label, ci, n_c, bi, n_b, &e)
                    .with_files(chunks[ci].len())
                    .with_file_names(chunks[ci].iter().map(|(path, _)| path))
                    .caused_by(&e)
                    .with_model(audit_model);
                if second_tried.contains(&(ci, bi)) {
                    // Both tries on the record, the first one's first and the second one marked.
                    let mut merged = prior_attempts.remove(&(ci, bi)).unwrap_or_default();
                    merged.extend(failure.attempts.drain(..).map(|mut a| {
                        a.second_try = true;
                        a
                    }));
                    failure.attempts = merged;
                }
                // A reply cut off at the output limit keeps the complete findings that arrived.
                match e.downcast::<PartialPassReply>() {
                    Ok(partial) => {
                        failures.push(failure.with_recovered(partial.findings.len()));
                        findings.extend(partial.findings);
                        proposed.extend(partial.proposed);
                        requested.extend(partial.needs);
                    }
                    Err(_) => failures.push(failure),
                }
            }
            // Refused by the spend cap: already recorded as NOT RUN on the meter.
            None => {}
        }
    }
    (findings, proposed, requested, ok, failures)
}

/// How many CONSECUTIVE poll failures `poll_batch_until_ended` tolerates before giving up.
/// A single failed poll (transient network blip, a 5xx that outlasted `count_tokens`'s own
/// retry budget, etc.) must never kill an hours-long batch after the tokens are already
/// spent — see the function doc for the full rationale. Resets to 0 on any successful poll.
pub(super) const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 5;

/// Poll a submitted batch on a fixed interval until `processing_status == "ended"`,
/// tolerating up to `max_consecutive_failures` CONSECUTIVE poll failures before giving up.
/// Each individual `poll_once` call already retries transient HTTP failures internally
/// (see `Llm::poll_batch_status` / `crate::retry`); this is the OUTER safety net for when
/// even that budget is exhausted repeatedly — a batch can run for hours, so one bad polling
/// window (a longer network outage, a burst of 5xx past the inner retry's 3-attempt cap)
/// must not discard tokens that have already been spent. The counter resets to 0 on every
/// successful poll, so it only fires on a genuine STREAK of failures, not a "few bad polls
/// scattered across an otherwise-healthy multi-hour run."
///
/// `poll_once` is injected (rather than calling `Llm::poll_batch_status` directly) so this
/// is unit-testable with a fake that fails a controlled number of times — no live batch, no
/// network, no mock HTTP server required. `on_status` is called on every SUCCESSFUL poll
/// (including the final `ended` one) so the caller can log/report progress without this
/// function knowing anything about logging.
pub(super) async fn poll_batch_until_ended<F, Fut>(
    mut poll_once: F,
    poll_interval: std::time::Duration,
    max_consecutive_failures: u32,
    mut on_status: impl FnMut(&crate::llm::BatchStatus),
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<crate::llm::BatchStatus>>,
{
    let mut consecutive_failures = 0u32;
    loop {
        tokio::time::sleep(poll_interval).await;
        match poll_once().await {
            Ok(status) => {
                consecutive_failures = 0;
                let ended = status.processing_status == "ended";
                on_status(&status);
                if ended {
                    return Ok(());
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                eprintln!(
                    "[camerata-server] batch poll failed ({consecutive_failures}/\
                     {max_consecutive_failures} consecutive failures): {e}"
                );
                if consecutive_failures >= max_consecutive_failures {
                    return Err(e.context(format!(
                        "batch poll failed {consecutive_failures} times consecutively — giving up \
                         (the batch may still be running server-side; check its status via the \
                         Anthropic console before resubmitting)"
                    )));
                }
            }
        }
    }
}

/// Batch execution mode (#61): compile ALL (chunk × rule-batch) pairs into Anthropic Message
/// Batch items, submit, poll to completion, then reassemble by `custom_id`.
///
/// ADVANTAGES vs. parallel: 50% discount on all input + output tokens; no per-call
/// rate-limit pressure; the API schedules + parallelizes internally. TRADE-OFF: latency is
/// asynchronous — the batch typically completes in seconds to a few minutes for small scans,
/// but up to 24h for very large ones.
///
/// CAP ENFORCEMENT: the Anthropic batch API accepts up to 100k requests per batch;
/// [`batch_mode::submit_round`] splits larger work into sub-batches.
///
/// RANK 2 (`batch_default` on; off reproduces the pre-rank-2 path exactly):
///
/// * RESUBMISSION RULE: every item that errored, expired or is missing from the results is
///   resubmitted ONCE, all of them together in one follow-up batch (never real time: the
///   follow-up keeps the discount and passes through the same spend gate as the first
///   submission). An item that fails again is a [`PassFailure`], the same shape a failed
///   real-time pass gets, so the export says the inspection is incomplete.
/// * A failed submit fails only that sub-batch's items (nothing was accepted, so they are
///   resubmitted like any other failure) instead of discarding replies already paid for. A
///   failed poll or fetch AFTER a successful submit is retried a bounded number of times and,
///   if it still fails, disclosed as failed passes naming the batch id; the batch is never
///   resubmitted, because it may still be running and billed.
/// * An item cut off at the output limit gets [`retry_unusable_scan_reply`], the real-time
///   passes' own truncation retry (sent in real time, admitted under the spend limit).
///
/// FALLBACK: without an API key this returns an error so the caller can say so.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_passes_batch(
    llm: &dyn batch_mode::BatchApi,
    repo: &str,
    repo_map: &RepoMap,
    nonce: &crate::prompt_frame::FrameNonce,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    chunks: &[&[(String, String)]],
    // Rank 8: as `run_passes`' `digests_override`.
    digests_override: Option<&[String]>,
    batches: &[&[(String, String)]],
    label: &str,
    meter: Option<&UsageMeter>,
    // BUG-AI-1: honor advisory_disabled in batch mode the same way run_passes does.
    // When true, the "flag novel issues beyond adopted rules" task is suppressed for
    // every (chunk, batch) pair, so batch mode does not re-introduce duplicate novel
    // findings when called from a language-scoped routing group.
    advisory_disabled: bool,
) -> anyhow::Result<PassesOut> {
    use batch_mode::{submit_round, BatchWork, ItemFailure, RoundContext};

    let eff = efficiency_of(meter);

    if !llm.has_api_key() {
        anyhow::bail!(
            "batch mode requires the Api backend with an Anthropic API key; \
             switch this project's backend to Api and add an Anthropic API key in the app \
             (Settings, Chat backend, API) or set ANTHROPIC_API_KEY, or use parallel mode"
        );
    }

    let model = resolve_audit_model(audit_model);

    let (planned_chunks, digests, _fit) = plan_scan_inputs(
        repo,
        repo_map,
        chunks,
        digests_override,
        batches,
        nonce,
        audit_model,
        label,
        meter,
    );
    let chunks: &[&[(String, String)]] = &planned_chunks;
    let symbols: Vec<repo_map::SymbolsSection> = chunks
        .iter()
        .map(|c| repo_map.symbols_for(c, nonce))
        .collect();
    let n_c = chunks.len();
    let n_b = batches.len();

    // The full cartesian product of (chunk, rule-batch) items, each with the request it sends.
    let mut work: Vec<BatchWork> = Vec::with_capacity(n_c * n_b);
    for (ci, digest) in digests.iter().enumerate() {
        let digest = chunk_segment(&symbols[ci].text, digest);
        for (bi, batch) in batches.iter().enumerate() {
            let rb = build_rules_block(batch);
            // BUG-AI-1: mirror run_passes semantics — advisory fires only on bi==0 AND
            // only when advisory_disabled is false.
            let advisory = !advisory_disabled && bi == 0;
            let task_line = pass_task_line(advisory, bi, n_b, terse_output_of(meter));
            // Same `build_pass_prompt` builder as `run_passes` — see the comment there and on
            // `cache_breakpoints_for_pass`. With one rule-batch per chunk (`batch_default` off,
            // or a small rule set) the chunk-segment breakpoint is dropped (it would be written
            // once and never re-read) and only the repo-map breakpoint stays, which re-reads
            // across the submission's other chunks.
            let (prompt, cache_bps) = build_pass_prompt_with(
                eff.label_after_prefix,
                repo,
                repo_map.prefix(),
                label,
                ci,
                n_c,
                &digest,
                n_b,
                &task_line,
                &rb,
            );
            note_map_use(meter, repo, label, ci, &symbols[ci]);
            let req = with_stage_effort(
                LlmRequest::new(prompt)
                    .with_system(crate::prompt_frame::with_untrusted_clause(
                        audit_system_prompt_with(terse_output_of(meter)),
                        nonce,
                    ))
                    .with_max_tokens(scan_max_tokens(batch.len()))
                    .with_model(model.clone())
                    .with_cache_breakpoints(cache_bps)
                    .with_response_schema(reply_schemas::scan_reply_schema())
                    .with_temperature(AUDIT_TEMPERATURE),
                Stage::ScanPass,
            );
            // Never submit what the model's window cannot take (M1).
            request_budget::ensure_request_fits(&req)?;
            work.push(BatchWork {
                custom_id: format!("c{ci}-b{bi}"),
                ci,
                bi,
                req,
            });
        }
    }

    // GAP-4: best-effort EXACT input-token count for the first (chunk, rule-batch) pair, via
    // `count_tokens` (free; Anthropic does not bill it). Logged, and used for the spend gate's
    // projection. Skipped gracefully on any failure.
    //
    // SPEND CAP (batch): a submitted batch cannot be cancelled cheaply, so the cap is checked
    // BEFORE every submission (`submit_round`), the resubmission included. The sample is only
    // counted when the spend so far plus the caller's pre-flight estimate (no network needed)
    // has not already refused the run.
    let hint_refuses = meter.is_some_and(|m| m.batch_refusal_reason(Some(0.0)).is_some());
    let mut per_item_usd: Option<f64> = None;
    if !hint_refuses {
        if let Some(first) = work.first() {
            let sample_req = sample_count_request(
                &first.req.prompt,
                &first.req.cache_breakpoints,
                terse_output_of(meter),
                nonce,
            );
            if let Some(exact) = llm.count_input_tokens(&sample_req, &model).await {
                eprintln!(
                    "[camerata-server] batch mode: exact input-token count for chunk {} rule-batch \
                     {} = {exact} tokens (via count_tokens; {} total (chunk,batch) pairs this run)",
                    first.ci,
                    first.bi,
                    work.len()
                );
                // One item's input side at the batch price, sized like the sample and with no
                // cache credit: a projection of what a submission commits, not a quote (output
                // is not projected). No cache write is projected, so the TTL does not move it.
                per_item_usd = crate::llm::input_cost_usd(
                    &model,
                    exact,
                    crate::llm::PriceBasis::batch(crate::llm::BATCH_CACHE_TTL),
                );
            }
        }
        // No exact projection (the count failed, or the model is unpriced): fall back to the
        // shared estimator, spread over the items. Still `None` when it cannot price either,
        // and the gate then refuses rather than guessing.
        if per_item_usd.is_none() && !work.is_empty() {
            per_item_usd = batch_estimate_usd(&model, chunks, batches, terse_output_of(meter))
                .map(|total| total / work.len() as f64);
        }
    }

    // Tell the job the total pass count so the progress bar can be pre-seeded.
    if let Some((jstore, jid)) = job {
        jstore.add_total(jid, work.len());
    }

    let ctx = RoundContext {
        api: llm,
        repo,
        label,
        model: &model,
        n_c,
        n_b,
        chunks,
        batches,
        job,
        meter,
        per_item_usd,
        max_body_bytes: batch_mode::max_batch_body_bytes(),
        fatal_errors: !eff.batch_default,
    };
    let all: Vec<&BatchWork> = work.iter().collect();
    let mut round = submit_round(&ctx, &all).await?;

    // RESUBMISSION (rank 2): every item that came back without a reply goes out once more, all
    // together in one follow-up batch. Only an item the API ANSWERED with an error (or left out
    // of its results) is resubmitted: a sub-batch whose poll or fetch failed after a successful
    // submit is NOT (see `submit_round`), because its items may still be running and paid for.
    if eff.batch_default {
        let failed: Vec<&BatchWork> = work
            .iter()
            .filter(|w| !round.refused.contains(&w.custom_id))
            .filter(|w| match round.results.get(&w.custom_id) {
                Some(Ok(_)) => false,
                Some(Err(failure)) => failure.resubmittable,
                // Left out of the results the API returned.
                None => true,
            })
            .collect();
        if !failed.is_empty() {
            eprintln!(
                "[camerata-server] batch mode: {} item(s) errored, expired or went missing; \
                 resubmitting them once in a follow-up batch",
                failed.len()
            );
            let mut retry = submit_round(&ctx, &failed).await?;
            for w in &failed {
                let first = match round.results.get(&w.custom_id) {
                    Some(Err(first)) => first.reason.clone(),
                    _ => "missing from the batch results".to_string(),
                };
                if retry.refused.contains(&w.custom_id) {
                    // The follow-up was refused by the spend limit: the item is NOT RUN on the
                    // meter (the cap's own disclosure), and this note says it had already
                    // failed once, so the gap is not read as a pass that never started.
                    if let Some(m) = meter {
                        m.record_note(format!(
                            "batch item {} (chunk {}, rule-batch {}) failed in the first \
                             submission ({first}) and its resubmission was not run: the spend \
                             limit was reached",
                            w.custom_id,
                            w.ci + 1,
                            w.bi + 1
                        ));
                    }
                    continue;
                }
                // A second failure that was a model refusal (billed) is a content failure: the
                // real-time second try does not send the item a third time.
                let (again, second_tryable) = match retry.results.remove(&w.custom_id) {
                    Some(Ok(resp)) => {
                        round.results.insert(w.custom_id.clone(), Ok(resp));
                        continue;
                    }
                    Some(Err(again)) => (again.reason, again.second_tryable),
                    None => ("missing from the batch results again".to_string(), true),
                };
                round.results.insert(
                    w.custom_id.clone(),
                    Err(ItemFailure {
                        reason: format!("{first}; resubmitted once and failed again: {again}"),
                        resubmittable: false,
                        second_tryable,
                    }),
                );
            }
            round.refused.extend(retry.refused);
        }
    }

    // END-OF-RUN SECOND TRY. An item that is still without a reply after the follow-up batch is
    // sent once more, in real time, through the same admission and spend checks (a stopped run
    // sends nothing). An item whose batch was accepted is NOT sent again: it may be running and
    // billed. A reply from the second try is used exactly like a batch reply; a failure keeps
    // both tries on the record.
    let mut second_try_errors: Vec<(&BatchWork, anyhow::Error)> = Vec::new();
    if eff.batch_default && !meter.is_some_and(UsageMeter::outage_stopped) {
        let realtime = llm.realtime();
        let again: Vec<&BatchWork> = work
            .iter()
            .filter(|w| !round.refused.contains(&w.custom_id))
            .filter(|w| match round.results.get(&w.custom_id) {
                Some(Ok(_)) => false,
                Some(Err(failure)) => failure.second_tryable,
                None => true,
            })
            .collect();
        for w in again {
            let Ok(_permit) = admit_pass(meter).await else {
                continue;
            };
            let session = format!("audit-{repo}-batch-{}-second-try", w.custom_id);
            match send_audit_request(realtime, w.req.clone(), None, &session, meter).await {
                Ok(resp) => {
                    if let Some(m) = meter {
                        m.record_stage(Stage::ScanPass, &resp);
                    }
                    round.results.insert(w.custom_id.clone(), Ok(resp));
                }
                Err(e) => {
                    meter_failed(meter, Stage::ScanPass, &e);
                    second_try_errors.push((w, e));
                }
            }
        }
    }

    // Each reply goes through the scan pass's own post-processing. With `batch_default` on a
    // reply cut off at the output limit gets the shared truncation retry, sent in real time
    // and admitted under the spend limit like a real-time pass; these run concurrently.
    let mut replies: Vec<(&BatchWork, crate::llm::LlmResponse)> = Vec::new();
    let mut results = crate::llm::BatchResults::default();
    for w in &work {
        if round.refused.contains(&w.custom_id) {
            continue;
        }
        match round.results.remove(&w.custom_id) {
            Some(Ok(resp)) => replies.push((w, resp)),
            Some(Err(why)) => {
                results.errors.insert(w.custom_id.clone(), why.reason);
            }
            // Not in the results at all: `reassemble_batch_passes` reports it as missing.
            None => {}
        }
    }
    let replies: Vec<(&BatchWork, crate::llm::LlmResponse)> = if eff.batch_default {
        use futures::stream::StreamExt;
        let realtime = llm.realtime();
        // Collected first so the stream's type names no closure (a `Send` bound on the spawned
        // scan future cannot be proven through a borrowing closure inside the combinator).
        let retries: Vec<_> = replies
            .into_iter()
            .map(|(w, resp)| async move {
                if !(resp.truncated && eff.truncation_retry) {
                    // No retry for this reply (complete but not JSON, or cut off with the
                    // retry switched off): it bought nothing, and the telemetry says so.
                    if resp.truncated || scan_output_is_malformed(&resp.text) {
                        if let Some(m) = meter {
                            m.record_wasted(Stage::ScanPass, &resp);
                        }
                    }
                    return (w, resp);
                }
                // SPEND CAP: the retry is one more real-time call. Refused, the cut-off reply is
                // used as it came (and counted as wasted), exactly as with the retry switched off.
                let _permit = match admit_pass(meter).await {
                    Ok(permit) => permit,
                    Err(CapReached) => {
                        if let Some(m) = meter {
                            m.record_wasted(Stage::ScanPass, &resp);
                            m.record_note(format!(
                                "the truncation retry of batch item {} was not run: {}",
                                w.custom_id,
                                crate::cost_cap::cause_for(m.cost_unknown())
                            ));
                        }
                        return (w, resp);
                    }
                };
                let session = format!("audit-{repo}-batch-{}", w.custom_id);
                // A batch item gets the truncation retry only: a reply that is complete but not
                // JSON is reported by the reassembly as a failed pass naming its files.
                let resp =
                    retry_unusable_scan_reply(realtime, &w.req, resp, None, &session, meter, 0)
                        .await
                        .resp;
                (w, resp)
            })
            .collect();
        futures::stream::iter(retries)
            .buffer_unordered(PARALLEL_CONCURRENCY)
            .collect()
            .await
    } else {
        replies
    };
    for (w, resp) in replies {
        results.responses.insert(w.custom_id.clone(), resp);
    }

    // The replies were metered as they arrived (`submit_round`, so the resubmission's spend gate
    // counted them), so the shared reassembly gets no meter: recording again would bill twice.
    let pairs: Vec<(usize, usize)> = work.iter().map(|w| (w.ci, w.bi)).collect();
    let chunk_files: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
    // Every reply was staged as it arrived, so the reassembly stages nothing. With batch_default
    // on, every unusable reply was already counted wasted above: by the retry step, or by the
    // branch that sends no retry. Only the legacy path, which has neither, needs it here.
    let wasted_meter = if eff.batch_default { None } else { meter };
    let mut out = reassemble_batch_passes(
        repo,
        label,
        n_c,
        n_b,
        &pairs,
        &chunk_files,
        &results,
        &round.refused,
        adopted,
        wasted_meter,
        true,
        job,
    );
    // A second try that failed too: the pass keeps its class and both tries.
    for (w, e) in &second_try_errors {
        let name = format!("{label} {}/{n_c} \u{b7} rules {}/{n_b}", w.ci + 1, w.bi + 1);
        if let Some(failure) = out.4.iter_mut().find(|f| f.pass == name) {
            let mut trail = StageTrail::default();
            trail.note_unusable(&failure.error, false, Vec::new());
            trail.note_error(e, true);
            failure.class = trail.class();
            failure.attempts = trail.attempts;
        }
    }
    Ok(out)
}

/// Turn a finished batch's per-item outcomes into the shared pass result: each (chunk,
/// rule-batch) pair's answer is parsed, and each pair WITHOUT one becomes a [`PassFailure`]
/// carrying the per-item reason the API gave (`errors`), so a model refusal reads as a refusal
/// rather than a generic "missing from results". Pure apart from the optional meter/job sinks,
/// so the failure naming is unit-testable without a live batch. `meter` records each reply and
/// each billed failure; a caller that already metered them as they arrived passes `None`.
#[allow(clippy::too_many_arguments)]
pub(super) fn reassemble_batch_passes(
    repo: &str,
    label: &str,
    n_c: usize,
    n_b: usize,
    pairs: &[(usize, usize)],
    chunk_files: &[usize],
    results: &crate::llm::BatchResults,
    refused_ids: &std::collections::HashSet<String>,
    adopted: &std::collections::HashSet<String>,
    meter: Option<&UsageMeter>,
    replies_already_metered: bool,
    job: Option<(&crate::jobs::JobStore, &str)>,
) -> PassesOut {
    let mut findings = Vec::new();
    let mut proposed = Vec::new();
    let mut requested = std::collections::HashSet::new();
    let mut ok = 0usize;
    let mut failures: Vec<PassFailure> = Vec::new();
    // A caller that metered each reply as it arrived still needs the replies that bought nothing
    // counted as wasted; it must not stage them a second time.
    let stage = |m: &UsageMeter, resp: &crate::llm::LlmResponse| {
        if !replies_already_metered {
            m.record_stage(Stage::ScanPass, resp);
        }
    };

    for &(ci, bi) in pairs {
        let custom_id = format!("c{ci}-b{bi}");
        if refused_ids.contains(&custom_id) {
            // Refused by the spend cap before submission: recorded as NOT RUN on the meter.
            if let Some((jstore, jid)) = job {
                jstore.inc_not_run(jid, 1);
            }
            continue;
        }
        match results.responses.get(&custom_id) {
            // A malformed batch answer (complete, not JSON) cannot be retried inside a finished
            // batch; it is a failed pass naming its files, never "no findings" (H4).
            Some(resp) if !resp.truncated && scan_output_is_malformed(&resp.text) => {
                if let Some(m) = meter {
                    stage(m, resp);
                    m.record_wasted(Stage::ScanPass, resp);
                }
                failures.push(
                    PassFailure::at(
                        label,
                        ci,
                        n_c,
                        bi,
                        n_b,
                        &"the model's reply was not valid JSON (a batch item cannot be retried)",
                    )
                    .with_files(chunk_files.get(ci).copied().unwrap_or(0)),
                );
                if let Some((jstore, jid)) = job {
                    jstore.inc_done(jid, 1);
                }
            }
            // A batch item still cut off at the output limit (after the one retry a batch item
            // gets, or with no retry on the legacy path) is a failed pass naming its files, and
            // the complete findings that arrived are kept (H2). It never reads as "no findings".
            Some(resp) if resp.truncated => {
                if let Some(m) = meter {
                    stage(m, resp);
                    m.record_wasted(Stage::ScanPass, resp);
                }
                let (f, p) = salvage_complete_findings(repo, &resp.text, adopted);
                let reason = cut_off_reply_reason(None, f.len());
                failures.push(
                    PassFailure::at(label, ci, n_c, bi, n_b, &reason)
                        .with_files(chunk_files.get(ci).copied().unwrap_or(0))
                        .with_recovered(f.len()),
                );
                if let Some((jstore, jid)) = job {
                    jstore.add_findings(jid, f.clone());
                    jstore.inc_done(jid, 1);
                }
                findings.extend(f);
                proposed.extend(p);
            }
            Some(resp) => {
                if let Some(m) = meter {
                    stage(m, resp);
                }
                let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
                let needs = parse_needs_files(&resp.text);
                findings.extend(f.clone());
                proposed.extend(p);
                requested.extend(needs);
                // Stream findings into the job for incremental preview.
                if let Some((jstore, jid)) = job {
                    jstore.add_findings(jid, f);
                    jstore.inc_done(jid, 1);
                }
                ok += 1;
            }
            None => {
                // The item failed or was not in the result set. Prefer the per-item reason
                // the API gave (a refusal reads as a refusal) over a generic "missing".
                // A failed item that was still billed (a refusal) counts toward the spend cap.
                if let (Some(m), Some(billed)) = (meter, results.billed.get(&custom_id)) {
                    stage(m, billed);
                }
                let reason = results
                    .errors
                    .get(&custom_id)
                    .cloned()
                    .unwrap_or_else(|| format!("batch item {custom_id} missing from results"));
                eprintln!(
                    "[camerata-server] batch item {custom_id} (chunk {ci}, rule-batch {bi}) \
                     failed: {reason}"
                );
                // Classed from the reason (a fetch or submit failure is transport, a refusal is
                // content) with one record of it, so the failure report says what happened and
                // an accepted batch's id (in the reason) is there for a later resume to fetch.
                failures.push(
                    PassFailure::at(label, ci, n_c, bi, n_b, &reason)
                        .with_files(chunk_files.get(ci).copied().unwrap_or(0))
                        .caused_by(&anyhow::anyhow!("{reason}")),
                );
                // Still count as done so the progress bar can reach 100%.
                if let Some((jstore, jid)) = job {
                    jstore.inc_done(jid, 1);
                }
            }
        }
    }

    (findings, proposed, requested, ok, failures)
}
