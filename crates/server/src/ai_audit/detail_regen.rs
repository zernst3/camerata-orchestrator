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
//! # The reply is not trusted
//!
//! The regenerated text feeds severity decisions, so a reply is accepted per finding, and only
//! when it passes four checks (see [`accept_entry`]). It is addressed by an opaque id and echoes
//! the finding's path and line (a reply numbered from 1, or reordered, cannot land on a
//! neighbour); it must keep every path or identifier the short statement named; it must not add
//! an exploit precondition that neither the statement nor the code shown makes (the same phrase
//! list calibration's raise-block uses, [`mentions_exploit_precondition`]); and a borderline
//! statement stays borderline. An entry that fails is simply not written, exactly like one the
//! model left out.
//!
//! # Never short, never dropped
//!
//! A finding the regeneration could not rewrite (the call failed, the spend limit stopped the
//! stage, the model left it out or failed a check twice) stays in the result with its short
//! detail and a visible [`DETAIL_INCOMPLETE_MARKER`], and the run reports a `FailedPass`, the same
//! path every other degraded stage uses. So the export can hold a short detail only with the
//! marker beside it.

use super::*;
use crate::stage_telemetry::DetailRejections;

/// The marker appended to a finding whose full detail could not be written. Two words on
/// purpose: it reaches `description_overlap_score`, and a longer marker shared by two failed
/// findings would raise their prose overlap and could fuse them.
pub(crate) const DETAIL_INCOMPLETE_MARKER: &str = "[detail incomplete]";

// How many findings one regeneration call covers is `camerata_ui_core::scan::REGEN_BATCH_SIZE`
// (batched, not one call per finding: see "Cost arithmetic" in `docs/TECHNICAL.md` and
// `batching_beats_one_call_per_finding`); the rounds' sizes come from `regen_round_batch_size`.

/// Output budget per finding. Measured on a real run: a full detail with its JSON wrapping is
/// about 280 output tokens, so 160 cut off 4 of 10 calls. 450 leaves headroom for a long one.
const DETAIL_TOKENS_PER_FINDING: u32 = camerata_ui_core::scan::REGEN_TOKENS_PER_FINDING;

/// Lines of code either side of the finding the regeneration shows. Smaller than the fix prompt's
/// 8: this call is paid for on every survivor, and the window is its main input.
const DETAIL_CONTEXT_RADIUS: usize = 6;

/// Lines either side of the first occurrence of a token the statement names in ANOTHER place
/// (a flag defined in a different file, a helper whose body decides the case).
const EXCERPT_RADIUS: usize = 3;

/// The most excerpt lines one finding gets from the files its statement names.
const EXCERPT_LINE_BUDGET: usize = 20;

/// The most tokens of one statement that are looked up in the file set.
const MAX_LOOKUP_TOKENS: usize = 6;

/// Attempts per finding: the first, a repeat in half-size batches for what the first left out,
/// failed a check on, or lost to a cut-off reply, and a last round of one finding to a call for
/// any straggler. Each call still passes the spend-limit check.
const DETAIL_ROUNDS: usize = camerata_ui_core::scan::REGEN_ROUNDS;

/// The batch size of round `round` (0-based): the full batch, half of it, then one at a time.
fn round_batch_size(round: usize) -> usize {
    camerata_ui_core::scan::regen_round_batch_size(round)
}

/// The system prompt of the regeneration call.
pub(crate) fn detail_regen_system_prompt() -> String {
    r#"You write the full explanation for audit findings that were reported in one short sentence. A human architect reads it to decide what to do, so it must be specific to the code shown.

Each finding below starts with an opaque id in square brackets, then its rule and severity; its location and evidence follow inside its delimited block, starting with the line `Location: path:line`. For each finding write `detail`: two to four sentences saying why THIS code is a problem and what the fix direction is.

Rules, every finding:
- Keep every fact the short statement already makes: each file name, symbol, flag and other file it names stays in your explanation, spelled exactly as given. Never contradict it.
- Ground the rest in the evidence given for that finding (its rule, snippet, the surrounding code and the excerpts from the files it names). NEVER state a fact about code you were not shown.
- Do not add conditions the statement does not make. Do not write that exploiting it requires, depends on, is contingent on, assumes, or needs an attacker to do something unless the statement or the code shown says so.
- If the statement says the case is borderline, keep saying so and say what would settle it.
- Do not repeat the statement word for word and do not restate the rule's text.
- Say WHAT is wrong and the DIRECTION of the fix. Do not mention Camerata, a scanner, the audit or any detection mechanism.
- NEVER repeat a credential value, not even partly. Name the variable and the line instead.

Return ONLY JSON, no prose:
{"details":[{"id":"<the finding's id, copied exactly>","path":"<its path, copied exactly>","line":<its line number>,"detail":"..."}]}
One entry per finding. An entry whose id, path or line does not match its finding is discarded."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// The one-line statement at the front of `Finding::detail`: the part [`parse_ai_findings`]
/// wrote from the model's `title`, before the first [`HEADLINE_SEP`] (the parser keeps the title
/// free of one, so the first is the boundary). A text with no separator is a title on its own
/// (or, rarely, a detail with no title); either way the whole text is kept as the headline so
/// nothing the scan said is lost.
fn headline_of(detail: &str) -> &str {
    detail
        .split_once(HEADLINE_SEP)
        .map_or(detail, |(headline, _)| headline)
}

/// The scan's own explanation: everything after the headline (the whole text when there is none).
fn short_statement_of(detail: &str) -> &str {
    detail
        .split_once(HEADLINE_SEP)
        .map_or(detail, |(_, short)| short)
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

// ── tokens a statement names ────────────────────────────────────────────────────────────────

/// The kinds of token a statement can name that the regenerated text must keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NamedToken {
    pub text: String,
    /// A file path or file name (`src/config.rs`, `webhook.rs`), as opposed to a symbol.
    pub is_path: bool,
}

/// Strip the punctuation a token picks up from the sentence around it.
fn trim_token(raw: &str) -> &str {
    raw.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '"'
                | '\''
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | ';'
                | ':'
                | '.'
                | '!'
                | '?'
        )
    })
}

/// `name.ext` with a plausible extension: a stem of two or more characters, an extension of one
/// to five alphanumerics starting with a letter (`config.rs`, `app.tsx`; not `e.g`, not `3.5`).
fn looks_like_file_name(token: &str) -> bool {
    let Some((stem, ext)) = token.rsplit_once('.') else {
        return false;
    };
    stem.chars().count() >= 2
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && stem
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '.'))
}

/// `WebhookPayload`, `parseURL`: a lowercase letter followed later by an uppercase one, in a
/// token long enough to be an identifier rather than a word.
fn looks_like_camel_case(token: &str) -> bool {
    token.chars().count() >= 4
        && token.chars().all(|c| c.is_alphanumeric())
        && token
            .chars()
            .zip(token.chars().skip(1))
            .any(|(a, b)| a.is_lowercase() && b.is_uppercase())
}

/// Every path-like or identifier-like token of `text`, in order of appearance, without repeats:
/// a token containing `/`, `::`, `_`, a file extension, or CamelCase.
pub(super) fn named_tokens(text: &str) -> Vec<NamedToken> {
    let mut out: Vec<NamedToken> = Vec::new();
    for raw in text.split_whitespace() {
        let token = trim_token(raw);
        if token.is_empty() {
            continue;
        }
        let is_file = looks_like_file_name(token);
        let is_path = is_file || token.contains('/');
        let is_symbol = token.contains("::") || token.contains('_') || looks_like_camel_case(token);
        if !(is_path || is_symbol) {
            continue;
        }
        if out.iter().any(|t| t.text == token) {
            continue;
        }
        out.push(NamedToken {
            text: token.to_string(),
            is_path,
        });
    }
    out
}

// ── the evidence a finding is regenerated from ──────────────────────────────────────────────

/// The file whose path is `token` (exact, or `token` is a trailing path of it, or it is a
/// trailing path of `token`).
fn file_named<'a>(files: &'a [(String, String)], token: &str) -> Option<&'a (String, String)> {
    let token = token.trim_start_matches("./");
    files.iter().find(|(p, _)| p == token).or_else(|| {
        files
            .iter()
            .find(|(p, _)| p.ends_with(&format!("/{token}")) || token.ends_with(&format!("/{p}")))
    })
}

/// The 1-based line of the first occurrence of `symbol` in `content`.
fn first_line_with(content: &str, symbol: &str) -> Option<usize> {
    content
        .lines()
        .position(|l| l.contains(symbol))
        .map(|i| i + 1)
}

/// `radius` lines either side of `line` (1-based) of `content`, numbered, as
/// `(first_line, last_line, text)`.
fn numbered_excerpt(content: &str, line: usize, radius: usize) -> (usize, usize, String) {
    let lines: Vec<&str> = content.lines().collect();
    let idx = line.saturating_sub(1).min(lines.len().saturating_sub(1));
    let start = idx.saturating_sub(radius);
    let end = (idx + radius + 1).min(lines.len());
    let text: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}: {l}\n", start + i + 1))
        .collect();
    (start + 1, end, text)
}

/// Add a file's repo-map line to `lines` once.
fn push_map_line(lines: &mut Vec<String>, path: &str, content: &str) {
    let line = repo_map_line(path, content);
    if !lines.contains(&line) {
        lines.push(line);
    }
}

/// Excerpts from the OTHER files a finding's headline and statement name: for each path token the
/// file's repo-map line, and for each symbol token the first place it occurs (the files the
/// statement names first, then the rest) with a few lines around it. At most
/// [`EXCERPT_LINE_BUDGET`] lines, and never a place the finding's own window already shows.
/// Empty when the statement names nothing that resolves. This is what keeps a cross-file
/// finding (a flag set in one file, trusted in another) explainable on an incremental run,
/// where the file it names is not among the files scanned.
fn named_file_excerpts(f: &Finding, files: &[(String, String)]) -> String {
    let tokens = named_tokens(&f.detail);
    let named_files: Vec<&(String, String)> = tokens
        .iter()
        .filter(|t| t.is_path)
        .filter_map(|t| file_named(files, &t.text))
        .collect();
    let mut shown: Vec<(String, usize, usize)> = Vec::new();
    let mut body = String::new();
    let mut map_lines: Vec<String> = Vec::new();
    let mut budget = EXCERPT_LINE_BUDGET;
    for (path, content) in &named_files {
        push_map_line(&mut map_lines, path, content);
    }
    for token in tokens.iter().filter(|t| !t.is_path).take(MAX_LOOKUP_TOKENS) {
        if budget == 0 {
            break;
        }
        let hit = named_files
            .iter()
            .copied()
            .chain(files.iter())
            .find_map(|(p, c)| first_line_with(c, &token.text).map(|l| (p, c, l)));
        let Some((path, content, line)) = hit else {
            continue;
        };
        // The finding's own window already shows this place.
        if *path == f.path && line.abs_diff(f.line) <= DETAIL_CONTEXT_RADIUS {
            continue;
        }
        let (first, last, text) = numbered_excerpt(content, line, EXCERPT_RADIUS);
        if shown
            .iter()
            .any(|(p, a, b)| p == path && first <= *b && last >= *a)
        {
            continue;
        }
        let n = last + 1 - first;
        if n > budget {
            break;
        }
        budget -= n;
        push_map_line(&mut map_lines, path, content);
        shown.push((path.clone(), first, last));
        body.push_str(&format!("{path}:\n{text}"));
    }
    // A named file with no symbol excerpt still shows its first lines, within the budget.
    for (path, content) in &named_files {
        if budget < 4 || shown.iter().any(|(p, _, _)| p == path) {
            continue;
        }
        let (first, last, text) = numbered_excerpt(content, 1, EXCERPT_RADIUS);
        let n = last + 1 - first;
        if n > budget {
            continue;
        }
        budget -= n;
        shown.push((path.clone(), first, last));
        body.push_str(&format!("{path}:\n{text}"));
    }
    if body.is_empty() && map_lines.is_empty() {
        return String::new();
    }
    format!(
        "Files the statement names (repo map, then excerpts):\n{}{body}",
        map_lines.concat()
    )
}

/// One finding's prompt block, and the text of the code it shows (the evidence an exploit
/// precondition must be found in to be allowed).
struct Block {
    prompt: String,
    code_shown: String,
}

fn build_block(
    id: &str,
    f: &Finding,
    files: &[(String, String)],
    directives: &std::collections::HashMap<String, String>,
    nonce: &crate::prompt_frame::FrameNonce,
) -> Block {
    let mut prompt = format!("[{id}] rule {} — severity {}\n", f.rule_id, f.severity);
    if let Some(directive) = directives.get(&f.rule_id.to_ascii_uppercase()) {
        prompt.push_str(&format!(
            "Rule: {}\n",
            camerata_secrets::redact_str(directive)
        ));
    }
    // Everything that derives from the client's files (location, the short statement the scan
    // wrote from them, quoted code, captured names, the windows and excerpts) sits in one frame.
    let mut evidence = format!("Location: {}:{}\n", f.path, f.line);
    evidence.push_str(&format!(
        "Short statement: {}\n",
        camerata_secrets::redact_str(&f.detail)
    ));
    if !f.snippet.is_empty() {
        evidence.push_str(&format!(
            "Code: {}\n",
            camerata_secrets::redact_str(&f.snippet)
        ));
    }
    evidence.push_str(&captured_objects_line(f));
    let mut code_shown = f.snippet.clone();
    let window = fix_context_window(files, &f.path, f.line, DETAIL_CONTEXT_RADIUS);
    if !window.is_empty() {
        evidence.push_str(&format!("Surrounding code:\n{window}"));
        code_shown.push('\n');
        code_shown.push_str(&window);
    }
    let excerpts = named_file_excerpts(f, files);
    if !excerpts.is_empty() {
        evidence.push_str(&excerpts);
        code_shown.push('\n');
        code_shown.push_str(&excerpts);
    }
    prompt.push_str(&crate::prompt_frame::frame_untrusted(
        nonce,
        &format!("finding {id}"),
        &evidence,
    ));
    prompt.push('\n');
    Block { prompt, code_shown }
}

// ── the reply ───────────────────────────────────────────────────────────────────────────────

/// One entry of the reply, before any check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub id: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub detail: String,
}

/// A reply read entry by entry: the usable entries, and how many array members were not one
/// (no id, or no text).
#[derive(Debug, Default)]
pub(super) struct ParsedReply {
    pub entries: Vec<Entry>,
    pub unusable: usize,
}

fn entry_of(value: &serde_json::Value) -> Option<Entry> {
    let id = value["id"].as_str()?.trim().to_string();
    let detail = value["detail"].as_str()?.trim().to_string();
    if id.is_empty() || detail.is_empty() {
        return None;
    }
    Some(Entry {
        id,
        path: value["path"].as_str().map(|p| p.trim().to_string()),
        line: value["line"].as_u64(),
        detail,
    })
}

/// Parse `{"details":[{"id","path","line","detail"}]}`. Malformed output, a missing array or an
/// entry without an id or text yields nothing for it; the caller treats a missing entry as "not
/// written", never as an empty explanation.
///
/// A reply `cut_off` at the output limit is read one entry at a time with the scan's salvage
/// walker ([`complete_array_objects`]): every complete, well-formed entry is kept, the unfinished
/// last one and anything after it are not guessed at.
pub(super) fn parse_detail_reply(raw: &str, cut_off: bool) -> ParsedReply {
    let values: Vec<serde_json::Value> = if cut_off {
        complete_array_objects(raw, "details")
            .into_iter()
            .filter_map(|o| serde_json::from_str(o).ok())
            .collect()
    } else {
        let Some(json) = extract_json_object(raw) else {
            return ParsedReply::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            return ParsedReply::default();
        };
        let Some(arr) = v["details"].as_array() else {
            return ParsedReply::default();
        };
        arr.clone()
    };
    let mut out = ParsedReply::default();
    for value in &values {
        match entry_of(value) {
            Some(e) => out.entries.push(e),
            None => out.unusable += 1,
        }
    }
    out
}

/// Why a reply entry was not written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Reject {
    /// The echoed path or line is not the finding's.
    Mismatch,
    /// A path or identifier the statement named is missing from the text.
    DropsNamedToken(String),
    /// The text makes an exploit precondition that neither the statement nor the code shown makes.
    InventedPrecondition,
}

/// `path` without a leading `./` or `/` (repeated), so `./src/a.rs` and `/src/a.rs` read as
/// `src/a.rs`.
fn normalized_path(path: &str) -> &str {
    let mut p = path.trim();
    while let Some(rest) = p.strip_prefix("./").or_else(|| p.strip_prefix('/')) {
        p = rest;
    }
    p
}

/// The checks one entry must pass to be written (the id match is done by the caller, which
/// knows the batch). Returns the text to write: the model's, with `Borderline: ` put back at the
/// front when the statement was borderline and the text lost it.
pub(super) fn accept_entry(f: &Finding, entry: &Entry, code_shown: &str) -> Result<String, Reject> {
    // The path is read forgivingly (a leading "./" or "/" is not a different file); the line is
    // not: any other line is another place.
    if entry.path.as_deref().map(normalized_path) != Some(normalized_path(&f.path))
        || entry.line != Some(f.line as u64)
    {
        return Err(Reject::Mismatch);
    }
    let text = camerata_secrets::redact_str(&entry.detail).into_owned();
    let short = short_statement_of(&f.detail);
    // Every path or identifier the statement named stays. Both sides go through the same secret
    // redaction: the text is what would be written, and a name the redactor rewrites in the
    // statement is rewritten the same way in the text, so a kept secret-shaped name is not
    // mistaken for a dropped one.
    let redacted_short = camerata_secrets::redact_str(short);
    for token in named_tokens(&redacted_short) {
        if !text.contains(&token.text) {
            return Err(Reject::DropsNamedToken(token.text));
        }
    }
    // No new exploit precondition. The same phrase list as calibration's raise-block and the D5
    // cap (`mentions_exploit_precondition`): a regenerated "requires a valid session" would
    // silently stop calibration raising a critical, on a claim nobody made.
    if mentions_exploit_precondition(&text)
        && !mentions_exploit_precondition(short)
        && !mentions_exploit_precondition(code_shown)
    {
        return Err(Reject::InventedPrecondition);
    }
    // A borderline statement stays borderline, deterministically.
    let borderline = |s: &str| {
        s.trim_start()
            .to_ascii_lowercase()
            .starts_with("borderline:")
    };
    if borderline(short) && !borderline(&text) {
        return Ok(format!("Borderline: {text}"));
    }
    Ok(text)
}

/// An opaque per-finding id for one batch: not its position, so a reply numbered from 1 or
/// reordered cannot land on a neighbour. Six hex digits of a hash of the finding's place in the
/// run, re-hashed on the rare collision within the batch.
fn opaque_id(repo: &str, idx: usize, f: &Finding, taken: &[String]) -> String {
    let mut salt = 0u32;
    loop {
        let h = crate::suppression::fnv1a(&format!(
            "{repo}\u{1f}{idx}\u{1f}{}\u{1f}{}\u{1f}{salt}",
            f.path, f.line
        ));
        let id = format!("{:06x}", h & 0xff_ffff);
        if !taken.contains(&id) {
            return id;
        }
        salt += 1;
    }
}

/// One round: the `pending` findings in batches of `batch_size`. Writes the regenerated detail
/// into `findings` and returns the indices still without one. `last_error` keeps the text of the
/// most recent failed call, for the failed pass's reason; `rejections` counts why entries were
/// turned down.
#[allow(clippy::too_many_arguments)]
async fn run_detail_round(
    llm: &dyn LlmPort,
    repo: &str,
    system: &str,
    nonce: &crate::prompt_frame::FrameNonce,
    model: Option<&str>,
    meter: Option<&UsageMeter>,
    files: &[(String, String)],
    directives: &std::collections::HashMap<String, String>,
    findings: &mut [Finding],
    pending: &[usize],
    batch_size: usize,
    last_error: &mut Option<String>,
    rejections: &mut DetailRejections,
    trail: &mut super::StageTrail,
    second_try: bool,
) -> Vec<usize> {
    let mut still_pending = Vec::new();
    for batch in pending.chunks(batch_size.max(1)) {
        // SPEND CAP, between batches: a refused batch stays pending (it ends up marked
        // incomplete, never dropped) and the stage is recorded as stopped part way.
        let _permit = match admit_stage(meter, repo, STAGE_DETAIL).await {
            Err(CapReached) => {
                still_pending.extend(batch.iter().copied());
                continue;
            }
            Ok(permit) => permit,
        };
        let mut ids: Vec<String> = Vec::with_capacity(batch.len());
        let mut blocks: Vec<Block> = Vec::with_capacity(batch.len());
        for &idx in batch {
            let id = opaque_id(repo, idx, &findings[idx], &ids);
            blocks.push(build_block(&id, &findings[idx], files, directives, nonce));
            ids.push(id);
        }
        let prompt = format!(
            "Repository: {repo}\n\nWrite the full detail for each finding below:\n\n{}",
            blocks.iter().map(|b| b.prompt.as_str()).collect::<String>()
        );
        let req = LlmRequest::new(prompt)
            .with_system(system.to_string())
            .with_max_tokens(scaled_max_tokens(batch.len(), DETAIL_TOKENS_PER_FINDING))
            .with_model(super::stage_model(model));
        // Streaming transport, as the other aggregate stages: the CLI leg's heartbeat and
        // partial-output salvage apply, and a flat deadline does not.
        let mut on_delta = |_: &str| {};
        let resp = match llm.complete_streaming(req, &mut on_delta).await {
            Ok(r) => r,
            Err(e) => {
                // A refusal is billed: it counts toward the spend limit like any answered call
                // (`record_failed`), and the reason the call failed reaches the failed pass.
                meter_failed(meter, Stage::DetailRegeneration, &e);
                trail.note_error(&e, second_try);
                *last_error = Some(format!("{e:#}"));
                still_pending.extend(batch.iter().copied());
                continue;
            }
        };
        if let Some(m) = meter {
            m.record_stage(Stage::DetailRegeneration, &resp);
        }
        // A reply cut off at the output limit is still a billed, partly wasted call, counted as
        // waste as before; but every complete entry in it is kept (read one by one), and only
        // the findings it lost go to the next round.
        if resp.truncated {
            if let Some(m) = meter {
                m.record_wasted(Stage::DetailRegeneration, &resp);
            }
        }
        let parsed = parse_detail_reply(&resp.text, resp.truncated);
        // Entries that name no finding of the batch (no usable id or text, or an id that is not
        // the batch's). Each is charged below to one finding the reply did not answer, so a
        // finding's attempt is counted once, under one reason.
        let mut stray = parsed.unusable;
        let entries = parsed.entries;
        // An id the reply uses twice is ambiguous: neither entry is written.
        let used_twice: Vec<&str> = entries
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| entries.iter().filter(|e| e.id == *id).count() > 1)
            .collect();
        let mut written = vec![false; batch.len()];
        // A finding some entry of the reply was addressed to (whatever became of it).
        let mut answered = vec![false; batch.len()];
        // A finding whose id the reply used twice.
        let mut ambiguous = vec![false; batch.len()];
        for entry in &entries {
            // An id that is not this batch's is ignored, never matched by position.
            let Some(pos) = ids.iter().position(|id| *id == entry.id) else {
                stray += 1;
                continue;
            };
            answered[pos] = true;
            if used_twice.contains(&entry.id.as_str()) {
                ambiguous[pos] = true;
                continue;
            }
            let idx = batch[pos];
            match accept_entry(&findings[idx], entry, &blocks[pos].code_shown) {
                Ok(text) => {
                    findings[idx].detail = compose_detail(&findings[idx].detail, &text);
                    written[pos] = true;
                }
                Err(Reject::Mismatch) => rejections.mismatch += 1,
                Err(Reject::DropsNamedToken(_)) => rejections.drops_named_token += 1,
                Err(Reject::InventedPrecondition) => rejections.invented_precondition += 1,
            }
        }
        for (pos, &idx) in batch.iter().enumerate() {
            if !written[pos] {
                // One count per finding per attempt. An entry that failed a check was counted
                // above under its reason; the rest are counted here.
                if ambiguous[pos] {
                    rejections.missing_id += 1;
                } else if !answered[pos] {
                    if stray > 0 {
                        stray -= 1;
                        rejections.missing_id += 1;
                    } else if resp.truncated {
                        rejections.truncated += 1;
                    } else {
                        rejections.left_out += 1;
                    }
                }
                still_pending.push(idx);
            }
        }
    }
    still_pending
}

/// Write the full `detail` of every finding in `findings`, which the scan wrote as one short
/// sentence. `files` is the WHOLE model-facing (redacted) file set of the repo, not only the
/// files this run scanned, so a file the statement names is found even on an incremental run;
/// `directives` maps an uppercase rule id to the directive the scan checked it against.
///
/// Never drops a finding. Returns the findings, each with its regenerated detail or, when that
/// failed, its short detail plus [`DETAIL_INCOMPLETE_MARKER`], and one `FailedPass` naming how
/// many.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn regenerate_full_details(
    llm: &dyn LlmPort,
    repo: &str,
    mut findings: Vec<Finding>,
    files: &[(String, String)],
    directives: &[(String, String)],
    nonce: &crate::prompt_frame::FrameNonce,
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
    let system = crate::prompt_frame::with_untrusted_clause(detail_regen_system_prompt(), nonce);
    let total = findings.len();
    let mut pending: Vec<usize> = (0..total).collect();
    let mut last_error: Option<String> = None;
    let mut rejections = DetailRejections::default();
    let mut trail = super::StageTrail::default();
    // The stage boundary check: stopped by the spend limit before it began, every finding keeps
    // its short detail, marked.
    let blocked = meter.is_some_and(|m| m.stage_blocked(repo, STAGE_DETAIL));
    if !blocked {
        for round in 0..DETAIL_ROUNDS {
            if pending.is_empty() {
                break;
            }
            pending = run_detail_round(
                llm,
                repo,
                &system,
                nonce,
                model,
                meter,
                files,
                &directives,
                &mut findings,
                &pending,
                // Each round is smaller: a cut-off or a muddled long reply is the usual reason a
                // batch fails, and a smaller one is both shorter and easier to keep straight.
                round_batch_size(round),
                &mut last_error,
                &mut rejections,
                &mut trail,
                false,
            )
            .await;
        }
        // END-OF-RUN SECOND TRY: what the rounds could not write gets one more try, one finding
        // at a time (the smallest prompt), through the same admission and spend checks. A stopped
        // run sends nothing.
        if !pending.is_empty() && !meter.is_some_and(UsageMeter::outage_stopped) {
            trail.note_unusable(
                "no usable explanation was written after the stage's own rounds",
                false,
            );
            pending = run_detail_round(
                llm,
                repo,
                &system,
                nonce,
                model,
                meter,
                files,
                &directives,
                &mut findings,
                &pending,
                1,
                &mut last_error,
                &mut rejections,
                &mut trail,
                true,
            )
            .await;
            if !pending.is_empty() {
                trail.note_unusable("no usable explanation was written on the second try", true);
            }
        }
    }
    if rejections.total() > 0 {
        if let Some(m) = meter {
            m.record_detail_rejections(rejections);
        }
    }
    let mut failed_passes = Vec::new();
    if !pending.is_empty() {
        for &idx in &pending {
            mark_detail_incomplete(&mut findings[idx]);
        }
        failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: STAGE_DETAIL.to_string(),
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
            ) + &(if rejections.total() > 0 {
                // The total only, in plain words: the breakdown by reason is operator telemetry.
                let n = rejections.total();
                format!(
                    "; the model's replies fell short {n} {} across the attempts (an entry failed \
                     a check, was left out or was cut off)",
                    if n == 1 { "time" } else { "times" }
                )
            } else {
                String::new()
            }) + &last_error
                .as_deref()
                .map(|e| {
                    format!(
                        "; a regeneration call failed with: {}",
                        batch_mode::disclosable_error(e)
                    )
                })
                .unwrap_or_default(),
            files_not_reviewed: 0,
            class: trail.class(),
            files: Vec::new(),
            attempts: trail.attempts.clone(),
            scope_note: false,
        });
    }
    (findings, failed_passes)
}
