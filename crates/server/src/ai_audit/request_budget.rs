//! How large a model request may be (Fable engine review item M1, item 2).
//!
//! Nothing used to check a prompt against the model's context window, so a big chunk, a big
//! repo map or a small-window model sent a request the API refused. Now every request is
//! estimated before it is sent ([`ensure_request_fits`]) and the chunks are planned so that none
//! is over the limit to begin with ([`fit_chunks`]): a chunk whose digest would not fit is split
//! into smaller chunks that are all sent, and a single file too large on its own is split by
//! line ranges with overlap ([`LINE_OVERLAP`]). Nothing is dropped.
//!
//! The estimate is conservative (see [`CHARS_PER_TOKEN_ESTIMATE`]): it overestimates, so a request
//! judged to fit fits. No token-counting API call is made.

use super::repo_map::{estimate_tokens, CHARS_PER_TOKEN_ESTIMATE};
use super::*;

/// The window assumed for a model the registry does not know: the smallest Claude window, so an
/// unknown model is never sent more than the smallest one takes.
pub(super) const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 200_000;

/// Lines two neighbouring parts of one split file share, so a defect that spans the cut is whole
/// in at least one of them.
pub(super) const LINE_OVERLAP: usize = 40;

/// The smallest digest budget a plan will work with, in estimated tokens. A window so small that
/// the fixed parts of a request leave less than this cannot be served; the request then fails
/// loudly at [`ensure_request_fits`] instead of being cut into dust.
pub(super) const MIN_DIGEST_BUDGET_TOKENS: usize = 1_000;

/// Bytes set aside for the pass's task line, label line and framing, which the planner does not
/// build: generous, so the plan leaves room.
pub(super) const PROMPT_FRAMING_RESERVE_BYTES: usize = 4_000;

/// The context window of `model` in tokens, or [`DEFAULT_CONTEXT_WINDOW_TOKENS`] for a model the
/// registry does not list.
pub(super) fn context_window_tokens(model: &str) -> usize {
    crate::model_registry::context_window_for(model).map_or(DEFAULT_CONTEXT_WINDOW_TOKENS, |w| {
        usize::try_from(w).unwrap_or(usize::MAX)
    })
}

/// The most input tokens a request to `model` may carry: its context window minus the output it
/// may write. The reserve is the model's registry `max_output`; for a model the registry lists
/// without one, the request's own `max_tokens`.
pub(super) fn input_limit_tokens(model: &str, request_max_tokens: u32) -> usize {
    let reserve = crate::model_registry::max_output_for(model)
        .map_or(request_max_tokens as usize, |n| n as usize);
    context_window_tokens(model).saturating_sub(reserve)
}

/// The bytes of a request that count against the window: the prompt, the system prompt and the
/// reply schema (a structured-output request carries it).
pub(super) fn request_bytes(req: &LlmRequest) -> usize {
    req.prompt.len()
        + req.system.as_ref().map_or(0, String::len)
        + req
            .response_schema
            .as_ref()
            .map_or(0, |schema| schema.to_string().len())
}

/// A request that does not fit the model's window; never sent.
#[derive(Debug)]
pub(crate) struct RequestTooLarge {
    pub estimated_tokens: usize,
    pub limit_tokens: usize,
    pub model: String,
}

impl std::fmt::Display for RequestTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the request (about {} tokens) does not fit the {} token input limit of model {}, so \
             it was not sent",
            self.estimated_tokens, self.limit_tokens, self.model
        )
    }
}

impl std::error::Error for RequestTooLarge {}

/// Check `req` against its model's window before it is sent. An oversized request is an error
/// and is never sent.
pub(super) fn ensure_request_fits(req: &LlmRequest) -> Result<(), RequestTooLarge> {
    let model = resolve_audit_model(Some(req.model.as_str()));
    let limit = input_limit_tokens(&model, req.max_tokens);
    let estimated = estimate_tokens(request_bytes(req));
    if estimated > limit {
        return Err(RequestTooLarge {
            estimated_tokens: estimated,
            limit_tokens: limit,
            model,
        });
    }
    Ok(())
}

/// The digest budget in estimated tokens for a request whose other parts total `fixed_bytes`:
/// what is left of the model's input limit. Never below [`MIN_DIGEST_BUDGET_TOKENS`].
pub(super) fn digest_budget_tokens(
    model: &str,
    request_max_tokens: u32,
    fixed_bytes: usize,
) -> usize {
    input_limit_tokens(model, request_max_tokens)
        .saturating_sub(estimate_tokens(fixed_bytes + PROMPT_FRAMING_RESERVE_BYTES))
        .max(MIN_DIGEST_BUDGET_TOKENS)
}

/// One planned pass input: the files its digest covers and the digest itself.
pub(super) struct FitPart<'a> {
    pub files: &'a [(String, String)],
    pub digest: String,
}

/// What [`fit_chunks`] had to do.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FitReport {
    /// Chunks that were split into more than one part.
    pub chunks_split: usize,
    /// Parts added by splitting (parts out minus chunks in).
    pub parts_added: usize,
    /// Single files split by line ranges.
    pub files_split_by_lines: usize,
}

/// Plan the inputs of a call: each chunk becomes one part when its digest fits `budget_tokens`
/// (the digest is then exactly what it was before), else several parts that together cover every
/// line of every file. `digests_override` is a candidate pass's per-chunk digest: used while it
/// fits, else the chunk's whole files are planned instead (a superset, so nothing is lost).
pub(super) fn fit_chunks<'a>(
    chunks: &[&'a [(String, String)]],
    digests_override: Option<&[String]>,
    nonce: &crate::prompt_frame::FrameNonce,
    budget_tokens: usize,
) -> (Vec<FitPart<'a>>, FitReport) {
    let budget_bytes = (budget_tokens * CHARS_PER_TOKEN_ESTIMATE).min(MAX_DIGEST_CHARS - 1_000);
    let mut report = FitReport::default();
    let mut parts: Vec<FitPart<'a>> = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let before = parts.len();
        let supplied = digests_override.and_then(|d| d.get(i));
        match supplied {
            Some(d) if d.len() <= budget_bytes => parts.push(FitPart {
                files: chunk,
                digest: d.clone(),
            }),
            _ => split_to_fit(chunk, nonce, budget_bytes, &mut parts, &mut report),
        }
        if parts.len() - before > 1 {
            report.chunks_split += 1;
            report.parts_added += parts.len() - before - 1;
        }
    }
    (parts, report)
}

fn split_to_fit<'a>(
    files: &'a [(String, String)],
    nonce: &crate::prompt_frame::FrameNonce,
    budget_bytes: usize,
    out: &mut Vec<FitPart<'a>>,
    report: &mut FitReport,
) {
    let digest = build_digest(files, nonce);
    if digest.len() <= budget_bytes {
        out.push(FitPart { files, digest });
        return;
    }
    if files.len() > 1 {
        let (left, right) = files.split_at(files.len() / 2);
        split_to_fit(left, nonce, budget_bytes, out, report);
        split_to_fit(right, nonce, budget_bytes, out, report);
        return;
    }
    // One file that does not fit on its own: line ranges with overlap.
    let (path, content) = &files[0];
    report.files_split_by_lines += 1;
    for digest in line_range_digests(path, content, nonce, budget_bytes) {
        out.push(FitPart { files, digest });
    }
}

/// The digests of one file split into line ranges that each fit `budget_bytes`, neighbours
/// overlapping by [`LINE_OVERLAP`] lines, every part numbered with the file's real line numbers.
/// A single line longer than a part is cut into segments that keep its line number.
fn line_range_digests(
    path: &str,
    content: &str,
    nonce: &crate::prompt_frame::FrameNonce,
    budget_bytes: usize,
) -> Vec<String> {
    let frame = crate::prompt_frame::frame_untrusted(nonce, path, "").len();
    // Room for the part's own note line as well.
    let cap = budget_bytes.saturating_sub(frame + 240).max(512);
    let max_segment = (cap / 4).max(64);
    // (line number, text) with over-long lines cut into segments at char boundaries.
    let mut segments: Vec<(usize, &str)> = Vec::new();
    let total_lines = content.lines().count();
    for (i, line) in content.lines().enumerate() {
        let mut rest = line;
        loop {
            if rest.len() <= max_segment {
                segments.push((i + 1, rest));
                break;
            }
            let mut cut = max_segment;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            segments.push((i + 1, &rest[..cut]));
            rest = &rest[cut..];
        }
    }
    let seg_bytes = |(no, text): &(usize, &str)| no.to_string().len().max(4) + 3 + text.len();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    while start < segments.len() {
        let mut end = start;
        let mut used = 0usize;
        while end < segments.len() {
            let b = seg_bytes(&segments[end]);
            if used + b > cap && end > start {
                break;
            }
            used += b;
            end += 1;
        }
        ranges.push((start, end));
        if end >= segments.len() {
            break;
        }
        // The next part starts LINE_OVERLAP lines back, but always moves forward.
        let mut next = end;
        let overlap_line = segments[end - 1].0.saturating_sub(LINE_OVERLAP);
        while next > start + 1 && segments[next - 1].0 > overlap_line {
            next -= 1;
        }
        start = next.max(start + 1);
    }
    let n_parts = ranges.len();
    ranges
        .into_iter()
        .enumerate()
        .map(|(k, (a, b))| {
            let first = segments[a].0;
            let last = segments[b - 1].0;
            let mut body = format!(
                "// [part {} of {n_parts} of this file: lines {first} to {last} of {total_lines}; \
                 neighbouring parts overlap by {LINE_OVERLAP} lines]\n",
                k + 1
            );
            for seg in &segments[a..b] {
                body.push_str(&format!("{:>4}| {}\n", seg.0, seg.1));
            }
            let mut out = crate::prompt_frame::frame_untrusted(nonce, path, &body);
            out.push('\n');
            out
        })
        .collect()
}
