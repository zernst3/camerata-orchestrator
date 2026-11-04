//! The per-chunk code digest, the scan system prompt and the parser that turns a scan reply into findings.

use super::*;

/// Per-call safety cap on a single digest's size (chars). A digest is built PER CHUNK
/// (see `chunk_files`), so this only bounds one chunk's line-numbered text; it sits above
/// the raw chunk-packing target so a normal chunk is never re-truncated here. Only a
/// single pathological file larger than this would clip.
pub(crate) const MAX_DIGEST_CHARS: usize = 600_000;

/// Minimum number of chars that must have been dropped from the digest cap before the
/// `[digest truncated …]` notice is appended (BUG-8). Below this threshold the drop
/// is cosmetically trivial (e.g. the last closing brace of the last file) and the
/// notice would mislead the model into thinking significant content was omitted.
/// Chosen as ≈ 5 lines × 80 chars/line.
pub(crate) const TRUNCATION_NOTICE_MIN_DROP: usize = 400;

/// Raw-bytes target when packing files into chunks. Each chunk is audited in its own model
/// call, so the WHOLE repo is covered no matter its size — a single context can't hold a
/// multi-MB repo (a 2.8M-char repo is ~700k tokens, far past a 200k window), and the old
/// single-digest path silently dropped ~90% of such a repo. ~350k raw chars line-numbers
/// to ~400k and, with the rules block + system prompt + response, stays well inside a
/// 200k-token context. Smaller chunks also keep the model's attention per file higher.
pub(super) const CHUNK_DIGEST_CHARS: usize = 350_000;

/// Number each line `NNNN| line`, so the model cites ACCURATE line numbers. Without
/// this the digest had no line markers and the model estimated by counting (and drifted —
/// 3 of 4 findings on the testbed cited the wrong line).
pub(super) fn number_lines(content: &str) -> String {
    number_lines_from(content.lines(), 1)
}

/// [`number_lines`] for a run of lines that starts at line `first` of its file (a candidate
/// excerpt keeps the file's real line numbers).
pub(super) fn number_lines_from<'a>(lines: impl Iterator<Item = &'a str>, first: usize) -> String {
    let mut s = String::new();
    for (i, line) in lines.enumerate() {
        s.push_str(&format!("{:>4}| {}\n", i + first, line));
    }
    s
}

/// The segment that follows the cached repo-map prefix: the chunk's symbols section (empty when
/// the whole map is the prefix, which already carries every symbol) and then its digest. Both
/// differ per chunk, so both sit after the cache breakpoint.
pub(super) fn chunk_segment<'a>(symbols: &str, digest: &'a str) -> std::borrow::Cow<'a, str> {
    if symbols.is_empty() {
        std::borrow::Cow::Borrowed(digest)
    } else {
        std::borrow::Cow::Owned(format!("{symbols}{digest}"))
    }
}

/// Record on the meter what one call's repo map held. `chunk` is 0-based.
pub(super) fn note_map_use(
    meter: Option<&UsageMeter>,
    repo: &str,
    group: &str,
    chunk: usize,
    s: &repo_map::SymbolsSection,
) {
    if let Some(m) = meter {
        m.record_repo_map(crate::stage_telemetry::RepoMapCall {
            repo: repo.to_string(),
            group: group.to_string(),
            chunk: chunk + 1,
            files_in_map: s.files_in_map,
            files_with_symbols: s.files_with_symbols,
            outlined_files: s.outlined_files,
        });
    }
}

/// Plan the inputs of one scan call so that no request of it is over the model's context window:
/// each chunk is one part when its digest fits what the window leaves after the fixed parts of the
/// request, else several parts that together cover every line ([`request_budget::fit_chunks`]).
/// Returns the planned chunks (a part's files), their digests and what was split. The ONE
/// planner for the real-time and the batch scan, so the two cannot disagree.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn plan_scan_inputs<'a>(
    repo: &str,
    repo_map: &RepoMap,
    chunks: &[&'a [(String, String)]],
    digests_override: Option<&[String]>,
    batches: &[&[(String, String)]],
    nonce: &crate::prompt_frame::FrameNonce,
    audit_model: Option<&str>,
    label: &str,
    meter: Option<&UsageMeter>,
) -> (
    Vec<&'a [(String, String)]>,
    Vec<String>,
    request_budget::FitReport,
) {
    let model = resolve_audit_model(audit_model);
    let system = crate::prompt_frame::with_untrusted_clause(
        audit_system_prompt_with(terse_output_of(meter)),
        nonce,
    );
    let schema_len = reply_schemas::scan_reply_schema().to_string().len();
    let rules_len = batches
        .iter()
        .map(|b| build_rules_block(b).len())
        .max()
        .unwrap_or(0);
    let max_rules = batches.iter().map(|b| b.len()).max().unwrap_or(0);
    let fixed = system.len()
        + schema_len
        + repo.len()
        + 16
        + repo_map.prefix().len()
        + repo_map.symbols_reserve_bytes()
        + rules_len;
    let budget = request_budget::digest_budget_tokens(&model, scan_max_tokens(max_rules), fixed);
    let (parts, report) = request_budget::fit_chunks(chunks, digests_override, nonce, budget);
    if report.parts_added > 0 {
        eprintln!(
            "[camerata-server] {repo} {label}: {} chunk(s) were over the model's context window \
             and were split into {} more part(s) ({} file(s) split by line ranges)",
            report.chunks_split, report.parts_added, report.files_split_by_lines
        );
        if let Some(m) = meter {
            m.record_request_splits(crate::stage_telemetry::RequestSplits {
                chunks_split: report.chunks_split as u64,
                parts_added: report.parts_added as u64,
                files_split_by_lines: report.files_split_by_lines as u64,
            });
        }
    }
    let planned = parts.iter().map(|p| p.files).collect();
    let digests = parts.into_iter().map(|p| p.digest).collect();
    (planned, digests, report)
}

/// Build a single code digest from the repo's files, capped at [`MAX_DIGEST_CHARS`].
/// Each file is LINE-NUMBERED so the model can cite exact paths + lines, and framed by
/// [`crate::prompt_frame::frame_untrusted`] under `nonce`, the run's random delimiter token, so a
/// file cannot print a header that passes for the start of another file.
///
/// # Truncation notice (BUG-8)
///
/// The notice `[digest truncated …]` is only appended when a NON-TRIVIAL amount of
/// content was dropped. If the partial slice captures all but a single short line
/// (specifically, `dropped < TRUNCATION_NOTICE_MIN_DROP` chars), the model already
/// has effectively all the code and the truncation warning would be misleading ("did
/// I miss something?"). The minimum drop threshold is intentionally conservative:
/// dropping even one method body is significant; dropping only the closing brace of
/// the last function is not.
pub fn build_digest(files: &[(String, String)], nonce: &crate::prompt_frame::FrameNonce) -> String {
    let mut out = String::new();
    let mut truncated = false;
    let mut significant_truncation = false;
    for (path, content) in files {
        // The frame's own bytes (opening and closing lines), counted against the cap like the
        // old header was.
        let frame_len = crate::prompt_frame::frame_untrusted(nonce, path, "").len();
        let numbered = number_lines(content);
        if out.len() + frame_len + numbered.len() > MAX_DIGEST_CHARS {
            // Add a partial slice of this file if there's room, then stop.
            let remaining = MAX_DIGEST_CHARS.saturating_sub(out.len() + frame_len);
            if remaining > 200 {
                let slice: String = numbered.chars().take(remaining).collect();
                out.push_str(&crate::prompt_frame::frame_untrusted(nonce, path, &slice));
                out.push('\n');
                // BUG-8: only warn when we actually dropped a meaningful chunk of
                // content. A trivially small drop (e.g. the last closing brace of
                // the last file) does not warrant a notice that may mislead the model
                // into thinking significant context is missing.
                let dropped = numbered.len().saturating_sub(remaining);
                significant_truncation = dropped >= TRUNCATION_NOTICE_MIN_DROP;
            } else {
                // Didn't add a partial slice at all — the whole file was dropped.
                significant_truncation = true;
            }
            truncated = true;
            break;
        }
        out.push_str(&crate::prompt_frame::frame_untrusted(
            nonce, path, &numbered,
        ));
        out.push('\n');
    }
    if truncated && significant_truncation {
        out.push_str("\n// [digest truncated at the size cap — audit the largest files first]\n");
    }
    out
}

/// The version of the question the scan asks the model: [`audit_system_prompt`], the pass prompt
/// layout ([`build_pass_prompt`]) and its task lines, and the rules-block format
/// ([`build_rules_block`]). It is part of the incremental cache key
/// ([`crate::scan_cache::AiScanKey`]): carried findings answered an older question once this
/// moves, so they are re-scanned instead of reused.
///
/// BUMP IT whenever any of those change what the model is asked, including a wording tweak (the
/// system prompt is pinned by a test that fails until you do). Do NOT bump it for a pure
/// scheduling or billing change (cache breakpoints, concurrency, the position of a label line),
/// which leave the answer the model gives unchanged.
///
/// 2: client content framed under a per-run random delimiter token, named in the system prompt
/// (`crate::prompt_frame`).
///
/// 3: the detail-regeneration prompt allows a fix direction to say what the fix needs, asks a
/// borderline explanation to start with "Borderline:", and lets a name joined with a slash or
/// a dot be written as its parts when a part is itself a name (a plain member access such as
/// supabase.from stays as written).
///
/// 4: retry fixes F3. The detail-regeneration reply is addressed by the opaque id alone (the
/// finding's path and line are attached by Camerata, not echoed), splits the explanation from
/// the fix direction, and the prompt lists the names the text must keep; the fix reply is
/// addressed by the same kind of opaque id instead of a batch position, and its prompt lists
/// the names the evidence lets the fix use.
///
/// 5: defects D2 and D4. The detail-regeneration reply is one entry per finding keyed by its id
/// (the schema requires every id), and the fix prompt says an omitted id is asked about once
/// more and is never sent for a convention row.
pub const AUDIT_PROMPT_SCHEMA_VERSION: u32 = 5;

/// H5: the sampling temperature asked for on every pass that decides WHICH findings exist (the
/// scan passes, batch or not, the alternatives choice, calibration and the deep security pass).
/// Sent only where the model accepts it (`LlmRequest::temperature`); the strict models ignore it
/// and the legacy ones (Haiku 4.5, the default fast tier) and future permissive ones get the
/// least random answer they offer. The fix-text and prose-lens passes keep the provider
/// default: they word findings, not choose them.
pub const AUDIT_TEMPERATURE: f32 = 0.0;

/// The system prompt: what to look for, what NOT to (the mechanical tier is already
/// covered), and the STRICT JSON schema to return. This is the prompt with the `terse_output`
/// measure OFF (the long `detail`, `proposed_rules` on every pass); [`audit_system_prompt_with`]
/// is the one implementation and takes the switch.
pub fn audit_system_prompt() -> String {
    audit_system_prompt_with(false)
}

/// The separator [`parse_ai_findings`] puts between a finding's one-line `title` and its
/// `detail` when it builds `Finding::detail`. The post-merge detail regeneration splits on the
/// same constant to keep the headline and replace the explanation, so the two cannot drift.
pub(crate) const HEADLINE_SEP: &str = " — ";

/// [`audit_system_prompt`] with the `terse_output` switch explicit.
///
/// OFF (`terse == false`): byte-identical to the prompt before rank 4 (the incremental cache's
/// pin test hashes it). ON: the output schema asks for a ONE-sentence `detail` and carries no
/// `proposed_rules` array; the advisory pass adds that array back through its own instruction
/// line ([`pass_task_line`]), which sits in the uncached suffix.
///
/// # Why one prompt for every pass, and not a second one for the non-advisory passes
///
/// The system prompt leads the prompt-cache prefix. If the advisory pass (a chunk's first
/// rule-batch) and its sibling passes carried different system prompts, the siblings could not
/// read the cache entry the advisory pass wrote (the whole `warm_schedule` saving) because the
/// first block of the prefix would differ. So the terse prompt is the same for all passes and
/// the one difference between them lives in the instruction line.
pub fn audit_system_prompt_with(terse: bool) -> String {
    let base = r#"You are a senior software architect performing a CONFORMANCE audit of an existing codebase for Camerata.

The user message lists the rules the project has ADOPTED, each as `- [RULE-ID] directive`. Your job is to check the code against EACH adopted rule and report EVERY place the code violates it.

How to work — enumerate rule × file, exhaustively:
1. Take each adopted `[RULE-ID]` in turn.
2. For that rule, walk EVERY file in the digest and check whether that file violates it.
3. Emit one finding per concrete violation SITE (a rule violated in three files is three findings; violated twice in one file is two), and set `rule` to the EXACT adopted RULE-ID (e.g. "ARCH-STRICT-LAYERING-1"), copied verbatim — not a paraphrase, not a kebab name.
Do not stop after the first violation of a rule, and do not stop after the first file. A rule with no violations anywhere simply produces no findings — do not invent one.

RECALL OVER PRECISION. This is a discovery audit and a human architect reviews every finding before anything is enforced, so the cost of a borderline false positive is tiny and the cost of a missed real violation is high. When you are unsure whether something violates a rule, REPORT IT (use severity "low" and say it's borderline in `detail`). Do not stay silent to seem precise. Do not cap yourself at a handful — if there are thirty violations, return thirty.

SEVERITY. Use "low"/"medium"/"high" for the normal range (a debatable architectural preference is "low" or "medium"; a concrete, demonstrable security or correctness break is "high"). Reserve "critical" — it must stay RARE — for a violation that clears ALL of: (1) it is concretely exploitable, not theoretical, and (2) a competent attacker or a single bad input reaches real, serious impact quickly. Qualifying classes: unauthenticated (or unauthorized) access to a privileged, destructive, or financial operation; a credential or secret exposed to clients or committed to the repo; PII or equivalently sensitive data readable or writable with no access control on an internet-exposed surface; remote code execution or injection with a real, reachable path (not just an untrusted-input pattern that happens to be sanitized elsewhere). Do not use "critical" for a bad-but-contained bug, a missing best practice, or anything you would call "high" out of general alarm — when in doubt between "high" and "critical", use "high".

CRITICAL — do NOT invent rule names that duplicate adopted rules. Before you set `rule`, check whether the violation is already covered by one of the adopted `[RULE-ID]`s above. If it is, you MUST use that exact adopted RULE-ID — even if you would have phrased the issue differently. A controller reaching into the database directly is `ARCH-STRICT-LAYERING-1`, not "controller-direct-db" or "handler-bypasses-repo"; a handler panicking on a DB error is `ARCH-STRUCTURED-ERRORS-1`, not "panic-on-db-error". Inventing a new name for a violation an adopted rule already covers is the single worst failure mode of this audit — it produces triplicate findings that all mean the same thing.

Flagging novel issues (issues no adopted rule covers) is GATED by this pass's instruction line. ONLY when that line says to "ALSO flag any other genuine issues" may you report something outside the adopted rules — and then set `rule` to a short kebab name (e.g. "auth-on-write-paths"), reserved strictly for genuinely-novel issues (if any adopted rule fits, use the adopted id instead). When the instruction line says to check ONLY the listed rules, report nothing outside them.

CROSS-FILE REASONING IS YOUR MANDATE. Every other automated check in this pipeline (the deterministic floor, every external static-analysis tool) reads ONE FILE AT A TIME — you are the only layer with the REPO MAP and multiple file bodies at once, so tracing an invariant ACROSS files is work no other layer can do. Three mechanisms to actively look for, by name, regardless of which specific rule (if any) they end up filed under:
- CONFIG-TO-HANDLER: a platform/feature flag or config value set in one file (verification disabled, a check short-circuited, a debug/bypass mode left on) that a handler or privileged code path in ANOTHER file reads and trusts, with no independent check of its own.
- POLICY-TO-CALL-SITE: a permissive access/data policy (a wide-open rule, a default-allow, an overly broad grant) declared in one place, and code elsewhere that relies on that policy as its only protection.
- CREDENTIAL-TO-USAGE: a secret or credential defined or loaded in one file and consumed by code in one or more OTHER files — trace where it is actually used, not just where it is declared.
If deciding one of these needs the body of a file you don't have, use `needs_files` (see below) rather than guessing or staying silent about it.

COMPENSATING CONTROLS — before flagging a missing check on any path, ask: is there ANY compensating check elsewhere on this same path — a signature or HMAC verification, a shared-secret comparison, a session-derived identity check, an ownership predicate, a role/permission gate? If you can find one (in the file at hand, the repo map, or another file body you have), do NOT flag it — a report that ignores a real compensating control is itself a false positive. Only flag the path when you looked and found none.

BACKSTOP ROLE — a missing rule is never a reason to withhold a real finding. When your own reasoning (especially the cross-file mechanisms above) turns up a genuine security-relevant observation that doesn't fit any adopted rule, report it exactly like any other novel issue — subject to the SAME instruction-line gate described above, never silenced just because nothing named it in advance.

DO NOT report: hardcoded secrets, secrets embedded in URLs, raw SQL string concatenation, or path-escape writes — a separate deterministic scanner already covers those precisely. Do not report pure style/formatting nits.

Each line in the digest is prefixed with its line number as `NNNN| `. Cite that exact number in `line` — do not estimate.

WATCH FOR INDIRECTION before flagging something MISSING. When a loop renders items via a HELPER CALL (e.g. `for row in rows { data_tr(row, …) }`), an attribute the rule wants — a `key`, a CSS class, an error handler — is very often set INSIDE that helper, not at the call site. Do NOT flag "missing key"/"missing X" on the call site without checking the called function's body. If that body is in the digest, read it; if it's elsewhere, request it via `needs_files` and defer — never assume it's missing just because it's not inline at the loop. (This is a real false-positive class: row renderers extracted into helpers that DO set the key.)

Cross-file context: you have the REPO MAP (every file + its public symbols) but only SOME file bodies in this pass. If judging a rule needs the actual BODY of a file that is in the map but NOT included below (e.g. you must read a repository's implementation, or a type defined elsewhere, to decide), do NOT guess and do NOT stay silent — list EVERY file path involved in that deferred judgment (the file under suspicion AND the files it depends on) in `needs_files`. A follow-up pass will include those bodies together so you can decide then.

For `code`, copy the offending source text VERBATIM from the digest — the exact characters of the line you're flagging, not a paraphrase. A deterministic post-step locates the true line by finding this text in the file, so an exact copy gives an exact line; a paraphrase makes the line approximate. Keep it to the single most relevant line (or short span). Still set `line` to your best estimate as a fallback.

CAPTURES (optional, only when applicable). Some adopted directives above name a placeholder in angle brackets — e.g. "...naming the bucket: your `<bucket>` bucket is public" or "...your `<table>` table has no Row Level Security". When the directive you matched contains such a placeholder AND you can identify the concrete real-world object this specific violation is about, add an optional `"captures"` object to that finding mapping the BARE token name (no angle brackets, e.g. `"bucket"`, `"table"`, `"function-name"`, `"view"`, `"matview"`, `"schema"`) to the actual object name (e.g. `"invoices"`, `"profiles"`, `"charge_membership"`) — so the report can name the real object instead of a generic placeholder. Omit `captures` entirely (or leave it empty) whenever the directive has no such placeholder, or you cannot pin down the concrete object — never guess a name you are not confident is correct.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {
      "path": "relative/file/path",
      "line": 0,
      "severity": "critical|high|medium|low",
      "rule": "EXACT adopted RULE-ID, or a short-kebab-name for an unlisted issue",
      "title": "one-line statement of the specific violation here",
      "code": "the EXACT offending source line, copied verbatim from the digest",
      "detail": "why it's a problem and what the fix direction is",
      "captures": {"bucket": "the-real-object-name"}
    }
  ],
  "proposed_rules": [
    {
      "name": "short-kebab-name (only for issues NOT covered by an adopted rule)",
      "title": "the rule to enforce going forward",
      "rationale": "why this rule, grounded in the findings",
      "severity": "critical|high|medium|low",
      "enforcement": "mechanical|review"
    }
  ],
  "needs_files": ["relative/path/you/need/the/body/of.rs"]
}
If the code genuinely conforms everywhere, return {"findings": [], "proposed_rules": [], "needs_files": []}. Every finding must point at real code at a real line."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE;
    if !terse {
        return base;
    }
    let base = replace_expected(
        &base,
        r#""detail": "why it's a problem and what the fix direction is","#,
        r#""detail": "ONE sentence, at most 25 words: why this code violates the rule, naming any other file or symbol the judgment depends on","#,
    );
    let base = replace_expected(&base, AUDIT_PROPOSED_RULES_SCHEMA, "");
    let base = replace_expected(
        &base,
        r#"return {"findings": [], "proposed_rules": [], "needs_files": []}."#,
        r#"return {"findings": [], "needs_files": []}."#,
    );
    replace_expected(
        &base,
        AUDIT_OUTPUT_SHAPE_LEAD,
        &format!("{TERSE_DETAIL_PARAGRAPH}\n\n{AUDIT_OUTPUT_SHAPE_LEAD}"),
    )
}

/// The sentence that opens the output-shape section of both scan system prompts. The terse
/// variants insert their short-detail paragraph in front of it.
pub(super) const AUDIT_OUTPUT_SHAPE_LEAD: &str =
    "Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:";

/// The `proposed_rules` array of the full output schema, exactly as it appears in
/// [`audit_system_prompt`] (the terse prompt removes it).
pub(super) const AUDIT_PROPOSED_RULES_SCHEMA: &str = r#"  "proposed_rules": [
    {
      "name": "short-kebab-name (only for issues NOT covered by an adopted rule)",
      "title": "the rule to enforce going forward",
      "rationale": "why this rule, grounded in the findings",
      "severity": "critical|high|medium|low",
      "enforcement": "mechanical|review"
    }
  ],
"#;

/// What the terse scan prompts tell the model about `detail`. The full explanation is written
/// later, for the findings that survive the merge, from the title, this sentence and the code
/// around the line, so everything the judgment rests on has to be in those three places.
pub(super) const TERSE_DETAIL_PARAGRAPH: &str = "SHORT DETAIL. Write `detail` as ONE sentence of at most 25 words: the specific reason this code violates the rule, naming every OTHER file or symbol the judgment depends on (a flag read in another file, the helper whose body decides it). If the case is borderline, begin the sentence with \"Borderline:\". Do not write fix advice or background: a separate step writes the full explanation, for the findings that survive deduplication, from your title, this sentence and the code around the line. So whatever makes this a violation must be in the title, in `code`, or in this one sentence.";

/// The instruction an ADVISORY pass adds to its line under `terse_output`, restoring the
/// `proposed_rules` array the terse system prompt left out. Later passes are told not to report
/// novel issues, so they have nothing to propose and never see this.
pub(super) const ADVISORY_PROPOSED_RULES_INSTRUCTION: &str = "In THIS pass only, ALSO add a top-level `proposed_rules` array to your JSON, one entry per genuinely novel issue you flagged: [{\"name\": \"short-kebab-name\", \"title\": \"the rule to enforce going forward\", \"rationale\": \"why this rule, grounded in the findings\", \"severity\": \"critical|high|medium|low\", \"enforcement\": \"mechanical|review\"}]. Use an empty array when you propose none.";

/// `text` with the first `from` replaced by `to`. The tests pin that each replacement applies;
/// a miss here would silently leave the full prompt in place, which only costs the saving,
/// never correctness.
pub(super) fn replace_expected(text: &str, from: &str, to: &str) -> String {
    debug_assert!(text.contains(from), "terse prompt: missing `{from}`");
    text.replacen(from, to, 1)
}

/// Pull the first balanced-looking JSON object out of a model response (tolerates
/// markdown fences or stray prose around it).
pub(crate) fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Map a model-invented rule name (already uppercased + hyphenated) onto the canonical
/// adopted corpus rule it actually means, for the few families the model keeps re-inventing:
/// panics → structured-errors, direct-DB / own-pool / bypasses-repo → strict-layering,
/// secret-in-URL → no-secrets-in-URL. Returns the canonical id ONLY when that rule is
/// actually adopted by this project, so a project without the rule never gets a phantom id
/// (and the location merge still collapses the duplicates regardless). Patterns are kept
/// narrow to avoid mislabeling a genuinely-novel issue.
pub(super) fn canonical_adopted_rule(
    norm: &str,
    adopted: &std::collections::HashSet<String>,
) -> Option<String> {
    let has = |s: &str| norm.contains(s);
    let candidate = if has("SECRET") && has("URL") {
        "ARCH-NO-SECRETS-IN-URL-1"
    } else if has("PANIC")
        // BUG-AI-2: narrow the match so rules that merely MENTION "PANIC" in a non-panic
        // context (e.g. "PREVENT-PANICKING-AUTH-CHECK", "LOG-PANIC-RECOVERY-1") are not
        // falsely canonicalized to ARCH-STRUCTURED-ERRORS-1. Only the specific invented
        // names the model repeatedly emits for actual panic-at-callsite violations qualify.
        && (has("HANDLER") || has("UNWRAP") || has("UNHANDLED") || has("ON-ERROR")
            || has("PROPAGAT") || has("UNWIND") || has("BUBBL"))
    {
        "ARCH-STRUCTURED-ERRORS-1"
    } else if (has("DIRECT") && (has("DB") || has("DATABASE")))
        || (has("BYPASS") && has("REPO"))
        || (has("OWN") && has("POOL"))
        || (has("CREATES") && has("POOL"))
    {
        "ARCH-STRICT-LAYERING-1"
    } else {
        return None;
    };
    adopted.contains(candidate).then(|| candidate.to_string())
}

/// Placeholder token names an AI-emitted finding's optional `captures` object may map to —
/// the same finite domain-token vocabulary the corpus's authored `directive`/`remediation`
/// text uses (see `onboard::architectural::capture_token_for` and
/// `report_export::generic_placeholder_filler` for the deterministic-checker and
/// report-rendering sides of the same vocabulary), minus `path`/`file` — those two are always
/// filled from `Finding.path` (see `report_export::instantiate_remediation`'s resolution
/// order) and never need a model-supplied override. Restricting to this allowlist means a
/// model that emits a garbage or hallucinated key just has that key silently dropped — it can
/// never smuggle an arbitrary token into the report's `Finding` wire type via `captures`.
pub(super) const KNOWN_AI_CAPTURE_TOKENS: &[&str] = &[
    "table",
    "function-name",
    "function",
    "bucket",
    "view",
    "matview",
    "schema",
    "policy",
];

/// A capture value's length cap: a token should be a bare object name (a table, bucket, or
/// function name), never a paragraph. Generous enough that it never clips a real identifier;
/// this only exists as defense against a model that stuffs prose into the value.
pub(super) const MAX_CAPTURE_VALUE_LEN: usize = 200;

/// How many captures a single finding may carry. A rule's remediation text never uses more
/// than one or two domain tokens — this is a ceiling against a malformed/adversarial blob,
/// not a realistic limit any well-formed response would ever approach.
pub(super) const MAX_CAPTURES_PER_FINDING: usize = 8;

/// Parse one finding's OPTIONAL `captures` object from the model's raw JSON — e.g.
/// `{"bucket": "invoices"}` maps the corpus rule's `<bucket>` remediation token to the real
/// object this finding is about (see `report_export::instantiate_remediation`, the consumer).
///
/// Deliberately fail-soft at every step, since `captures` is pure enrichment (an empty map
/// just means the report's generic fallback wording is used, per `generic_placeholder_filler`)
/// and must never cost the finding itself:
/// - `captures` absent, `null`, or not a JSON object -> empty map.
/// - a key outside [`KNOWN_AI_CAPTURE_TOKENS`] -> that entry is skipped, not fatal.
/// - a value that isn't a JSON string -> that entry is skipped, not fatal.
/// - an empty/whitespace-only value, or one longer than [`MAX_CAPTURE_VALUE_LEN`] -> skipped.
/// - more than [`MAX_CAPTURES_PER_FINDING`] well-formed entries -> extras beyond the cap are
///   dropped (this can only matter for adversarial/malformed input; a real rule directive
///   never uses more than a couple of tokens).
pub(super) fn parse_finding_captures(
    f: &serde_json::Value,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(obj) = f["captures"].as_object() else {
        return out;
    };
    for (key, value) in obj {
        if out.len() >= MAX_CAPTURES_PER_FINDING {
            break;
        }
        if !KNOWN_AI_CAPTURE_TOKENS.contains(&key.as_str()) {
            continue;
        }
        let Some(s) = value.as_str() else {
            continue;
        };
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed.chars().count() > MAX_CAPTURE_VALUE_LEN {
            continue;
        }
        out.insert(key.clone(), trimmed.to_string());
    }
    out
}

/// The canonical severity label for a model-written one (M3): trimmed and case-folded, with the
/// common synonyms mapped (`severe` and `major` to high, `moderate` to medium, `minor` and
/// `informational` to low). A label that is none of these keeps `current`, so an odd word never
/// quietly turns into a rating.
pub(crate) fn parse_model_severity(raw: &str, current: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "critical" | "crit" | "blocker" => "critical",
        "high" | "severe" | "major" => "high",
        "medium" | "moderate" | "med" => "medium",
        "low" | "minor" | "informational" | "info" | "note" => "low",
        _ => return current.to_string(),
    }
    .to_string()
}

/// A model-written rule label as a rule id candidate (M4): surrounding brackets, quotes and
/// backticks removed, anything after a `:` or an opening parenthesis dropped, uppercased, with
/// spaces and underscores turned into hyphens.
pub(super) fn normalize_model_rule_label(raw: &str) -> String {
    let t = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '[' | ']' | '`' | '"' | '\'' | '*'))
        .trim();
    let t = t.split(':').next().unwrap_or(t);
    let t = t.split(" (").next().unwrap_or(t).trim();
    t.to_ascii_uppercase().replace([' ', '_'], "-")
}

/// The adopted rule id a near-miss label means (M4): the label plus a version suffix
/// (`SEC-NO-RAW-SQL-CONCAT` for `SEC-NO-RAW-SQL-CONCAT-1`), or the longest adopted id the label
/// extends (`SEC-NO-RAW-SQL-CONCAT-1-IN-REPORTS`). Only a hyphenated label is tried.
pub(super) fn adopted_near_match(
    norm: &str,
    adopted: &std::collections::HashSet<String>,
) -> Option<String> {
    if !norm.contains('-') {
        return None;
    }
    let versioned: Vec<&String> = adopted
        .iter()
        .filter(|a| {
            a.strip_prefix(norm)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    if let [only] = versioned.as_slice() {
        return Some((*only).clone());
    }
    // The label extends an adopted id. Not when what it adds is itself a run of another adopted
    // id's words ("SEC-NO-A-1-SEC-NO-B-1": two rules named at once): that is not one rule.
    let words_of = |id: &str| format!("-{id}-");
    adopted
        .iter()
        .filter(|a| {
            norm.strip_prefix(a.as_str())
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| {
                    !rest.is_empty()
                        && !adopted
                            .iter()
                            .any(|b| words_of(b).contains(&words_of(rest)))
                })
        })
        .max_by_key(|a| a.len())
        .cloned()
}

/// The fewest characters a label needs before a typo match is tried: a short id sits within
/// two edits of too many others to mean one.
pub(super) const TYPO_MATCH_MIN_LEN: usize = 12;

/// The most edits (insert, delete, substitute) between a label and an adopted id for the label
/// to be read as a typo of it.
pub(super) const TYPO_MATCH_MAX_EDITS: usize = 2;

/// The adopted rule id a mistyped label means (R1B-3): the one adopted id within
/// [`TYPO_MATCH_MAX_EDITS`] edits of the label (`JAVASCRIPT-TYPESRIPT-NO-NON-NULL-ASSERTION-1`
/// for `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1`), judged on the id without its numeric
/// suffix, which must be identical. Two or more adopted ids that close, no
/// adopted id that close, a short or unhyphenated label or one naming a placeholder: `None`, so
/// the label keeps its `AI-` invented id and the junk filter still sees a placeholder.
pub(super) fn adopted_typo_match(
    norm: &str,
    adopted: &std::collections::HashSet<String>,
) -> Option<String> {
    if norm.len() < TYPO_MATCH_MIN_LEN
        || !norm.contains('-')
        || crate::junk_finding::is_placeholder_rule_id(norm)
    {
        return None;
    }
    // The trailing `-<digits>` names which rule of a numbered family it is (`RUST-DIOXUS-3` and
    // `RUST-DIOXUS-7` are different rules), so it is never a typo: suffixes must be identical
    // and only the stems are compared.
    let (stem, suffix) = split_numeric_suffix(norm);
    let mut close = adopted.iter().filter(|a| {
        let (a_stem, a_suffix) = split_numeric_suffix(a);
        a.len() >= TYPO_MATCH_MIN_LEN
            && a_suffix == suffix
            && a_stem.len().abs_diff(stem.len()) <= TYPO_MATCH_MAX_EDITS
            && edit_distance(stem, a_stem) <= TYPO_MATCH_MAX_EDITS
    });
    match (close.next(), close.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// Split a rule id at its trailing `-<digits>`: `("RUST-DIOXUS", "-3")`. An id with no numeric
/// suffix returns the whole id and an empty suffix.
pub(super) fn split_numeric_suffix(id: &str) -> (&str, &str) {
    match id.rfind('-') {
        Some(i) if i + 1 < id.len() && id[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            id.split_at(i)
        }
        _ => (id, ""),
    }
}

/// The Levenshtein distance between two ASCII-or-not strings, counted in characters.
pub(super) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != *cb);
            cur.push(substitute.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A model-written line number (M5): a number, a numeric string, `L42`, `line 42`, or a range such as
/// `42-47` (its first line). Anything else is 0, "no line".
pub(super) fn parse_model_line(v: &serde_json::Value) -> usize {
    if let Some(n) = v.as_u64() {
        return n as usize;
    }
    if let Some(x) = v.as_f64() {
        return if x >= 0.0 { x as usize } else { 0 };
    }
    let Some(t) = v.as_str() else {
        return 0;
    };
    let lower = t.trim().to_ascii_lowercase();
    let rest = ["lines", "line", "ln", "l"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .unwrap_or(&lower);
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '#' | '.'));
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

/// Parse a model audit response into Findings + ProposedRules in the scan's shapes.
/// Robust: malformed output yields empty vecs rather than erroring the whole scan.
pub fn parse_ai_findings(
    repo: &str,
    raw: &str,
    adopted: &std::collections::HashSet<String>,
) -> (Vec<Finding>, Vec<ProposedRule>) {
    let Some(json) = extract_json_object(raw) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Vec::new(), Vec::new());
    };

    let mut findings = Vec::new();
    let mut junk_dropped: Vec<String> = Vec::new();
    if let Some(arr) = v["findings"].as_array() {
        for f in arr {
            let rule = ["rule", "rule_id", "id"]
                .iter()
                .find_map(|k| f[*k].as_str())
                .unwrap_or("architecture");
            let mut norm = normalize_model_rule_label(rule);
            if norm.is_empty() {
                norm = "ARCHITECTURE".to_string();
            }
            // If the model cited an ADOPTED rule id, key the finding to that id directly so
            // the violation shows up under the rule the architect selected. Else try to
            // canonicalize a well-known invented name onto the adopted rule it actually
            // means (AI-HANDLER-PANICS → ARCH-STRUCTURED-ERRORS-1). Else it's a genuinely
            // AI-discovered issue beyond the ruleset (AI- provenance prefix).
            let rule_id = if adopted.contains(&norm) {
                norm
            } else if let Some(near) = adopted_near_match(&norm, adopted) {
                near
            } else if let Some(canon) = canonical_adopted_rule(&norm, adopted) {
                canon
            } else if let Some(typo) = adopted_typo_match(&norm, adopted) {
                typo
            } else {
                format!("AI-{norm}")
            };
            let severity =
                parse_model_severity(f["severity"].as_str().unwrap_or("medium"), "medium");
            // The title is the headline of `Finding::detail`, split off again by the detail
            // regeneration at the FIRST `HEADLINE_SEP`, so the title itself must never hold one.
            let title = f["title"]
                .as_str()
                .unwrap_or("")
                .trim()
                .replace(HEADLINE_SEP, " - ");
            let code = f["code"].as_str().unwrap_or("").trim().to_string();
            let detail = f["detail"].as_str().unwrap_or("").trim().to_string();
            // A10: an entry with a place and a rule but no text at all is not a finding, and it is
            // not deleted either: it is kept below, marked `empty_entry` (adopted rules included).
            let empty_entry = title.is_empty() && detail.is_empty() && code.is_empty();
            // D1, A10: a row the model filled with placeholder text (title `placeholder`, code
            // and detail `x`, a rule id naming a placeholder) is not a finding. It is KEPT,
            // marked thrown out with the reason, and kept apart from the rows that go on
            // (`audit_repo` parks it; it is never deleted). Counted below.
            let placeholder_why = if empty_entry {
                Some(crate::engine_discard::EMPTY_ENTRY)
            } else {
                crate::junk_finding::parse_gate_reason(
                    !adopted.contains(&rule_id),
                    &rule_id,
                    &title,
                    &code,
                    &detail,
                )
            };
            if let Some(why) = placeholder_why {
                junk_dropped.push(format!(
                    "{rule_id} at {}: {why}",
                    f["path"].as_str().unwrap_or("(repo)")
                ));
            }
            // `snippet` holds the VERBATIM offending line when the model gave one (matches the
            // deterministic floor, and lets the line-resolution post-step grep for it). Fall
            // back to the title when there's no code. The title is preserved by leading the
            // detail so the human still sees the one-line statement.
            let snippet = if code.is_empty() { title.clone() } else { code };
            let detail = match (title.is_empty(), detail.is_empty()) {
                (false, false) => format!("{title}{HEADLINE_SEP}{detail}"),
                (false, true) => title.clone(),
                _ => detail,
            };
            let mut finding = Finding {
                could_be_intentional: false,
                site_key: String::new(),
                group_path: Vec::new(),
                group_role: crate::review_tree::GroupRole::Primary,
                review: Default::default(),
                flagged_by: Vec::new(),
                possible_duplicate_of: None,
                repo: repo.to_string(),
                path: f["path"].as_str().unwrap_or("(repo)").to_string(),
                line: ["line", "start_line", "line_number"]
                    .iter()
                    .map(|k| parse_model_line(&f[*k]))
                    .find(|n| *n > 0)
                    .unwrap_or(0),
                rule_id,
                severity,
                snippet,
                detail,
                status: "active".to_string(),
                also_matches: Vec::new(),
                // AI-audit findings are advisory, never scan-time tool previews.
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
                // Calibration (apply_verdicts) sets these once the calibration pass runs;
                // a raw pre-calibration finding has no confidence/effort opinion yet.
                confidence: None,
                effort: None,
                // Category comes from calibration (or the heuristic in group_semantic);
                // `located` is set by review_tree::mark_located once snippets are anchored to files.
                category: None,
                located: true,
                // AI/semantic findings usually have no cheaply-known structured object at
                // match time (the model narrates the defect in prose, not a parsed
                // table/function/bucket name) — but when the model DID identify one and
                // supplied an optional `captures` object (see the CAPTURES section of
                // `audit_system_prompt`), `parse_finding_captures` maps it through so the
                // report can name the real object instead of falling back to a generic
                // placeholder. Empty (the common case) falls back exactly as before.
                captures: parse_finding_captures(f),
                // Set by `audit_repo` in a blanket post-pass once the recommendation (or
                // operator-forced pick) for this finding's rule is known — see its doc
                // comment. `None` here is the correct pre-tag state, not a gap.
                evaluated_option_id: None,
                also_locations: Vec::new(),
                // Set by `generate_fix_specifics` in a later pass; a raw pre-generation
                // finding has no fix opinion yet.
                fix_specific: None,
                // Set by `apply_severity_calibration_rules` (D5) once the calibration pass
                // runs; a raw pre-calibration finding has no floor rationale yet.
                calibration_rationale: None,
                // A5.6: a model-reported fault is a Defect at parse time. `audit_repo`'s
                // kind-assignment post-pass (see `assign_deviation_kinds`) RECLASSIFIES the
                // rows judged under a decision/default-with-exceptions rule's recommended
                // option into `Deviation` once the per-rule recommendation is known — the
                // parser itself cannot know a rule's kind, and must never guess from prose.
                kind: Some(camerata_api_types::audit::FindingKind::Defect),
            };
            // The model's `code` is its verbatim quote and its prose can repeat a value: redact
            // before the row exists anywhere else (docs/design/2026-10-23, leak path A8).
            crate::onboard::redact::redact_finding(&mut finding);
            if let Some(why) = placeholder_why {
                if empty_entry {
                    crate::engine_discard::mark(&mut finding, crate::engine_discard::EMPTY_ENTRY);
                } else {
                    crate::engine_discard::mark(
                        &mut finding,
                        &format!("{}: {why}", crate::engine_discard::PLACEHOLDER_ROW),
                    );
                }
            }
            findings.push(finding);
        }
    }
    if !junk_dropped.is_empty() {
        // Operator side only: the client report never mentions a row the model should not have
        // written.
        eprintln!(
            "[camerata-server] {repo}: threw out {} junk finding(s) at scan parsing (kept, marked): {}",
            junk_dropped.len(),
            junk_dropped.join("; ")
        );
    }

    let mut proposed = Vec::new();
    if let Some(arr) = v["proposed_rules"].as_array() {
        for r in arr {
            let name = r["name"].as_str().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            let id = format!("AI-{}", name.to_ascii_uppercase().replace(' ', "-"));
            // Both mechanical and architectural tiers are CI-tier deterministic checks.
            let mechanical = matches!(
                r["enforcement"].as_str(),
                Some("mechanical") | Some("architectural")
            );
            let title = r["title"].as_str().unwrap_or(name).trim().to_string();
            // How many AI findings this rule's name accounts for.
            let finding_count = findings.iter().filter(|f| f.rule_id == id).count();
            proposed.push(ProposedRule {
                id,
                title,
                // AI-discovered architectural rules are human-judged, not auto-mechanical.
                kind: if mechanical {
                    "mechanical".to_string()
                } else {
                    "review".to_string()
                },
                // Architectural guidance partitions to CONVENTIONS.md (structured).
                enforcement: "structured".to_string(),
                options: Vec::new(),
                default_option: None,
                // AI-discovered → AI-designed; un-grounded until the grounding pass.
                verification: "draft".to_string(),
                sources: Vec::new(),
                decision_question: None,
                decision_why: None,
                scope: "repo-local".to_string(),
                domain: "architecture".to_string(),
                // Inherently semantic -> enforced at the cross-agent integration tier
                // (an AI-assisted pre-PR check), not the line-level content gate.
                enforcement_point: "integration".to_string(),
                repos: vec![repo.to_string()],
                placement: "project (AI-assisted integration check)".to_string(),
                finding_count,
                recommended: true,
                // AI-discovered rules are always draft (un-grounded), never auto-recommended.
                is_auto_recommended: false,
            });
        }
    }

    (findings, proposed)
}
