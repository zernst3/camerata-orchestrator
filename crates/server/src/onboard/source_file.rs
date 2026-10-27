//! Whole-file source read for the in-app code viewer: backs `GET /api/onboard/source-file`
//! (`WORK_ORDER_REVIEW_SPEED.md` A.2 and A.3).
//!
//! The finding modal shows the enclosing block of one finding; the viewer shows the FILE, so a
//! reviewer reads the code around a finding without leaving the app. Everything that decides
//! where code may be read from, and every status word, is shared with `finding-context`, not
//! copied:
//!
//! - **where** (inspected commit, working tree, or nothing because the source was deleted):
//!   [`crate::client_source::read::source_for_read`], the one resolver (it also refuses a repo
//!   name that is not a plain `owner/name` a project owns);
//! - **path jail** and the git object read: [`super::finding_context`]'s
//!   `resolve_contained_path`, `is_plain_relative_path` and `read_git_blob_at` (which shows regular
//!   files only: a symlink or submodule at the commit is `file_missing`);
//! - **statuses and their reasons**: [`FindingContextOutcome`], the one vocabulary, with the
//!   same source labels (`read::labelled`) and the same semantics. Only the content body is
//!   local to this module;
//! - **redaction**: [`super::finding_context::redact_block`], which calls
//!   `camerata_secrets::redact_str`. Every line leaves this module redacted, and the text that is
//!   highlighted and parsed for the enclosing block is the redacted text, so no raw line is ever
//!   held past the redaction step.
//!
//! # Statuses
//!
//! | `status` | Meaning | Carries lines |
//! |---|---|---|
//! | `ok` | Read from the inspected commit (`source: "git"`), or from the working tree when the inspection recorded no commit (`source: "working_tree"`) | yes |
//! | `commit_unavailable` | The recorded commit is not in this clone; the working tree is shown (`source: "working_tree"`) when the file is there | when the file is there |
//! | `source_deleted` | The client source was deleted on purpose; nothing is read, `deleted_at` carries the date | no |
//! | `file_missing` | No such regular file | no |
//! | `not_utf8` / `too_large` | Refused (binary, or above [`MAX_VIEWER_BYTES`], or too slow to prepare) | no |
//! | `path_traversal` | The path escapes the repo; the only non-200 | no |
//!
//! The viewer keys its amber banner on `source == "working_tree"`.
//!
//! A file up to `MAX_FILE_BYTES` (400 KB) is returned whole. A larger one, up to 4 MB, is
//! returned as an 800-line window centred on `around`, with `window` saying which lines; only
//! the window (plus [`HIGHLIGHT_CONTEXT_LINES`] lines before it) is highlighted.
//!
//! # Never blocks the runtime, never hangs
//!
//! The CPU work (redaction, enclosing block, highlighting) runs on the blocking pool under a
//! timeout ([`RenderLimits`]). The redacted lines are prepared first; if the second stage (block
//! and highlight) is slow, the response degrades to plain lines with `highlight: "skipped"` and
//! no `block`. If even the first stage is slow the file is reported `too_large`.
//!
//! Read-only by construction: a GET, no filesystem write, and the only subprocesses are the
//! read-only `git cat-file` and `git ls-tree` in `finding_context`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use camerata_checks::extract::enclosing::{enclosing_block, EnclosingBlock};
use camerata_checks::extract::highlight::{
    highlight_lang_for_path, highlight_with_budget, HighlightStatus, TokenClass, DEFAULT_BUDGET,
};

use super::files::MAX_FILE_BYTES;
use super::finding_context::{
    is_plain_relative_path, language_label, read_git_blob_at, read_working_tree_file, redact_block,
    resolve_contained_path, FindingContextOutcome, GitReadError, VerifiedCommit,
};
use crate::client_source::read::{labelled, no_store, source_for_read, SourceRead};
use crate::AppState;

/// Above this a file is refused outright (`too_large`).
pub const MAX_VIEWER_BYTES: usize = 4 * 1024 * 1024;

/// Lines in the window returned for a file above [`MAX_FILE_BYTES`].
pub const WINDOW_LINES: usize = 800;

/// Lines before a window that are highlighted with it, so a string or comment that began just
/// above the window is usually still lexed correctly.
pub const HIGHLIGHT_CONTEXT_LINES: usize = 200;

/// How long each CPU stage may take before the response degrades.
#[derive(Debug, Clone, Copy)]
pub struct RenderLimits {
    /// Reading, UTF-8, redaction and windowing. Over this the file is `too_large`.
    pub prepare: Duration,
    /// Enclosing block and highlighting. Over this the lines are returned plain.
    pub decorate: Duration,
    /// Test seam: how long each CPU stage sleeps before working, so a test can prove the
    /// timeouts degrade the response without a genuinely slow input. It travels WITH the limits
    /// of the one call that sets it (production is always zero), so it cannot slow any other
    /// test, unlike the process-global it replaces.
    pub stage_delay: Duration,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            prepare: Duration::from_secs(5),
            decorate: Duration::from_secs(2),
            stage_delay: Duration::ZERO,
        }
    }
}

/// Query of `GET /api/onboard/source-file`.
#[derive(Debug, serde::Deserialize)]
pub struct SourceFileParams {
    /// `owner/repo`.
    pub repo: String,
    /// Repo-relative path, as on the finding.
    pub path: String,
    /// The inspected commit (40 hex characters). Defaults to the commit the project's latest
    /// inspection recorded for the repo.
    #[serde(default)]
    pub sha: Option<String>,
    /// 1-based line to centre a window on and to resolve the enclosing block for.
    #[serde(default)]
    pub around: Option<usize>,
    /// The project the finding belongs to; defaults to the active project, then any project
    /// that has the repo.
    #[serde(default)]
    pub project: Option<String>,
}

/// Why a file could not be turned into a viewer response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    FileMissing,
    NotUtf8,
    TooLarge,
    PathTraversal,
    /// Git only: the commit is not in this clone.
    CommitUnavailable,
    /// Too many reads of client code were running.
    Busy,
}

impl From<Refused> for FindingContextOutcome {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::FileMissing => FindingContextOutcome::FileMissing,
            Refused::NotUtf8 => FindingContextOutcome::NotUtf8,
            Refused::TooLarge => FindingContextOutcome::TooLargeToView,
            Refused::PathTraversal => FindingContextOutcome::PathTraversal,
            Refused::CommitUnavailable => FindingContextOutcome::CommitUnavailable,
            Refused::Busy => FindingContextOutcome::Busy,
        }
    }
}

/// One run of a line: `(class index or None for plain, text)`.
pub(crate) type Segment = (Option<usize>, String);

/// A file read, redacted, windowed and highlighted: everything the response body carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rendered {
    pub language: String,
    pub total_lines: usize,
    /// 1-based inclusive line range actually returned (`to < from` for an empty file).
    pub from: usize,
    pub to: usize,
    pub highlight: HighlightStatus,
    pub block: Option<EnclosingBlock>,
    pub lines: Vec<Vec<Segment>>,
}

/// Where one read comes from.
#[derive(Clone, Copy)]
pub enum Origin<'a> {
    /// The file as it is in the working tree now (the scan ran on a folder with no git history).
    WorkingTree(&'a Path),
    /// The file as it was at commit `sha`, through git's object store, so a finding keeps showing
    /// the code the inspection saw even after the checkout moved on. The commit is checked to
    /// exist on every read.
    Git { dir: &'a Path, sha: &'a str },
    /// The same, for a commit already checked ([`VerifiedCommit::verify`]), so a caller reading
    /// many files of one commit checks it once.
    Verified(&'a VerifiedCommit),
}

impl Origin<'_> {
    fn dir(&self) -> &Path {
        match self {
            Origin::WorkingTree(dir) | Origin::Git { dir, .. } => dir,
            Origin::Verified(commit) => &commit.dir,
        }
    }
}

/// The raw bytes of `rel_path` from `origin`, at most `cap` of them: the path jail first, then
/// the working-tree file or the committed blob. The ONE place a code-showing endpoint turns a
/// `(origin, path)` into bytes ([`read_rendered`] and the batch context endpoint both call it).
/// A working-tree file larger than `cap` is refused before it is read into memory.
pub(crate) async fn read_bytes(
    origin: Origin<'_>,
    rel_path: &str,
    cap: usize,
) -> Result<Vec<u8>, Refused> {
    let Some(abs_path) = resolve_contained_path(origin.dir(), rel_path) else {
        return Err(Refused::PathTraversal);
    };
    match origin {
        Origin::WorkingTree(_) => {
            // The size is checked first so an oversized file is never read into memory.
            match tokio::fs::metadata(&abs_path).await {
                Ok(meta) if meta.is_file() && meta.len() > cap as u64 => {
                    return Err(Refused::TooLarge)
                }
                Ok(meta) if meta.is_file() => {}
                _ => return Err(Refused::FileMissing),
            }
            read_working_tree_file(&abs_path)
                .await
                .map_err(|_| Refused::FileMissing)
        }
        Origin::Git { dir, sha } => {
            // No process runs for a path that is refused.
            ensure_plain(rel_path)?;
            let commit = VerifiedCommit::verify(dir, sha).await.map_err(refused_of)?;
            read_git_blob_at(&commit, rel_path, cap)
                .await
                .map_err(refused_of)
        }
        Origin::Verified(commit) => {
            ensure_plain(rel_path)?;
            read_git_blob_at(commit, rel_path, cap)
                .await
                .map_err(refused_of)
        }
    }
}

/// A path that is safe to put after `<sha>:` (see `is_plain_relative_path`), else the traversal
/// refusal.
fn ensure_plain(rel_path: &str) -> Result<(), Refused> {
    if is_plain_relative_path(rel_path) {
        Ok(())
    } else {
        Err(Refused::PathTraversal)
    }
}

/// A failed git object read as the refusal the viewer and the context endpoints share.
fn refused_of(error: GitReadError) -> Refused {
    match error {
        GitReadError::CommitUnavailable => Refused::CommitUnavailable,
        GitReadError::NotFound => Refused::FileMissing,
        GitReadError::TooLarge => Refused::TooLarge,
        GitReadError::Busy => Refused::Busy,
    }
}

/// Read, redact, window and highlight `rel_path` from `origin`. The path jail runs before any
/// read. Never panics, never blocks the runtime, never hangs (see the module docs).
pub(crate) async fn read_rendered(
    origin: Origin<'_>,
    rel_path: &str,
    around: Option<usize>,
    limits: RenderLimits,
) -> Result<Rendered, Refused> {
    let bytes = read_bytes(origin, rel_path, MAX_VIEWER_BYTES).await?;

    let path = rel_path.to_string();
    let prepared = {
        let path = path.clone();
        tokio::time::timeout(
            limits.prepare,
            tokio::task::spawn_blocking(move || {
                stage_sleep(limits.stage_delay);
                prepare(bytes, &path, around)
            }),
        )
        .await
    };
    let prepared = match prepared {
        Ok(Ok(result)) => result?,
        // Too slow, or the worker panicked: nothing to show.
        Ok(Err(_)) | Err(_) => return Err(Refused::TooLarge),
    };

    let shared = Arc::new(prepared);
    let job = {
        let (shared, path) = (Arc::clone(&shared), path);
        tokio::task::spawn_blocking(move || {
            stage_sleep(limits.stage_delay);
            decorate(&shared, &path, around, DEFAULT_BUDGET)
        })
    };
    let decorated = match tokio::time::timeout(limits.decorate, job).await {
        Ok(Ok(decorated)) => decorated,
        // Block and highlight are an extra: plain lines, no block.
        Ok(Err(_)) | Err(_) => plain_decoration(&shared),
    };
    Ok(Rendered {
        language: shared.language.clone(),
        total_lines: shared.lines.len(),
        from: shared.from,
        to: shared.to,
        highlight: decorated.highlight,
        block: decorated.block,
        lines: decorated.lines,
    })
}

/// The redacted lines and the window, before any parsing.
#[derive(Debug)]
pub(crate) struct Prepared {
    language: String,
    /// Every line of the file, redacted.
    lines: Vec<String>,
    from: usize,
    to: usize,
}

/// What parsing adds to a [`Prepared`].
#[derive(Debug)]
pub(crate) struct Decorated {
    block: Option<EnclosingBlock>,
    highlight: HighlightStatus,
    lines: Vec<Vec<Segment>>,
}

/// Sleep `delay` on the blocking-pool thread before a CPU stage works (zero in production).
fn stage_sleep(delay: Duration) {
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
}

/// The text of a file: refused above `cap` bytes, and when it is not UTF-8. The ONE place bytes
/// become text for display ([`redacted_lines`] and finding-context's `describe` both call it).
pub(crate) fn decode(bytes: Vec<u8>, cap: usize) -> Result<String, Refused> {
    if bytes.len() > cap {
        return Err(Refused::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| Refused::NotUtf8)
}

/// Size cap, UTF-8, and redaction of a whole file: every line of it, redacted. Pure CPU; run on
/// the blocking pool. The ONE place a file's text is redacted for display ([`prepare`] and the
/// batch context endpoint both call it), so a multi-line key body is caught wherever a caller
/// goes on to cut.
///
/// The file is redacted as one text (a multi-line key body is only recognisable as a block),
/// then re-split. The `\n` count is preserved, so line numbers hold.
pub(crate) fn redacted_lines(bytes: Vec<u8>, cap: usize) -> Result<Vec<String>, Refused> {
    let source = decode(bytes, cap)?;
    let raw_lines: Vec<String> = source.lines().map(str::to_string).collect();
    drop(source);
    Ok(redact_block(&raw_lines))
}

/// Size, UTF-8, redaction and windowing. Pure CPU; run on the blocking pool.
pub(crate) fn prepare(
    bytes: Vec<u8>,
    rel_path: &str,
    around: Option<usize>,
) -> Result<Prepared, Refused> {
    let whole = bytes.len() <= MAX_FILE_BYTES;
    let lines = redacted_lines(bytes, MAX_VIEWER_BYTES)?;

    let total_lines = lines.len();
    let (from, to) = if whole || total_lines == 0 {
        (1, total_lines)
    } else {
        window_around(around.unwrap_or(1), total_lines)
    };
    Ok(Prepared {
        language: language_label(rel_path),
        lines,
        from,
        to,
    })
}

/// The enclosing block of `around` and the highlighted segments of the window. Pure CPU; run on
/// the blocking pool.
///
/// `highlight_budget` is the highlighter's own wall-clock allowance (production passes
/// [`DEFAULT_BUDGET`]); past it the highlight is abandoned and the lines come back plain
/// (`Skipped`). It is a parameter, not a constant, so a test that asserts the highlighted
/// outcome can pass a budget no loaded machine can exceed instead of racing a 250 ms clock.
pub(crate) fn decorate(
    prepared: &Prepared,
    rel_path: &str,
    around: Option<usize>,
    highlight_budget: Duration,
) -> Decorated {
    let Prepared {
        lines, from, to, ..
    } = prepared;
    let (from, to) = (*from, *to);
    let block = around.and_then(|line| enclosing_block(rel_path, &lines.join("\n"), line));

    // Highlight the window with some leading context, not the whole file.
    let whole = from == 1 && to == lines.len();
    let hl_from = if whole {
        1
    } else {
        from.saturating_sub(HIGHLIGHT_CONTEXT_LINES).max(1)
    };
    let hl_text = lines
        .get(hl_from.saturating_sub(1)..to.min(lines.len()))
        .map(|slice| slice.join("\n"))
        .unwrap_or_default();
    let highlighted = highlight_with_budget(
        highlight_lang_for_path(rel_path),
        &hl_text,
        highlight_budget,
    );

    let mut out = Vec::with_capacity(to.saturating_sub(from) + 1);
    for n in from..=to {
        let Some(line) = lines.get(n - 1) else { break };
        let spans = n
            .checked_sub(hl_from)
            .and_then(|i| highlighted.lines.get(i))
            .map_or(&[][..], Vec::as_slice);
        out.push(segments(line, spans));
    }
    Decorated {
        block,
        highlight: highlighted.status,
        lines: out,
    }
}

/// The degraded decoration: plain lines, no block.
fn plain_decoration(prepared: &Prepared) -> Decorated {
    let lines = (prepared.from..=prepared.to)
        .filter_map(|n| prepared.lines.get(n - 1))
        .map(|line| vec![(None, line.clone())])
        .collect();
    Decorated {
        block: None,
        highlight: HighlightStatus::Skipped,
        lines,
    }
}

/// The [`WINDOW_LINES`]-line window centred on `around`, clamped to the file.
fn window_around(around: usize, total_lines: usize) -> (usize, usize) {
    let around = around.clamp(1, total_lines);
    let to = (around + WINDOW_LINES / 2 - 1).min(total_lines);
    let from = (to + 1).saturating_sub(WINDOW_LINES).max(1);
    let to = (from + WINDOW_LINES - 1).min(total_lines);
    (from, to)
}

/// Cut `line` into `(class, text)` segments from its spans; gaps are plain (`None`).
fn segments(line: &str, spans: &[(TokenClass, std::ops::Range<usize>)]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut at = 0;
    for (class, range) in spans {
        let (Some(gap), Some(text)) = (line.get(at..range.start), line.get(range.clone())) else {
            // A span that does not fit this line (it cannot: the line is the text that was
            // highlighted) is dropped rather than risk a mis-cut character.
            continue;
        };
        if !gap.is_empty() {
            out.push((None, gap.to_string()));
        }
        out.push((Some(class.index()), text.to_string()));
        at = range.end;
    }
    if at < line.len() || out.is_empty() {
        out.push((None, line.get(at..).unwrap_or_default().to_string()));
    }
    out
}

fn highlight_label(status: HighlightStatus) -> &'static str {
    match status {
        HighlightStatus::Highlighted => "ok",
        HighlightStatus::Plain => "plain",
        HighlightStatus::Skipped => "skipped",
    }
}

#[derive(serde::Serialize)]
struct WindowBody {
    from: usize,
    to: usize,
    total_lines: usize,
}

#[derive(serde::Serialize)]
struct BlockBody {
    start_line: usize,
    end_line: usize,
    kind: &'static str,
}

/// The response body of a file that has lines to show.
#[derive(serde::Serialize)]
struct ContentBody<'a> {
    status: &'static str,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_differs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    language: &'a str,
    total_lines: usize,
    window: WindowBody,
    highlight: &'static str,
    classes: [&'static str; 10],
    block: Option<BlockBody>,
    lines: &'a [Vec<Segment>],
}

impl<'a> ContentBody<'a> {
    fn new(rendered: &'a Rendered, status: &'static str, source: &'static str) -> Self {
        Self {
            status,
            source,
            commit: None,
            head_differs: None,
            reason: None,
            language: &rendered.language,
            total_lines: rendered.total_lines,
            window: WindowBody {
                from: rendered.from,
                to: rendered.to,
                total_lines: rendered.total_lines,
            },
            highlight: highlight_label(rendered.highlight),
            classes: TokenClass::CLASS_NAMES,
            block: rendered.block.map(|b| BlockBody {
                start_line: b.start_line,
                end_line: b.end_line,
                kind: b.kind.label(),
            }),
            lines: &rendered.lines,
        }
    }
}

/// Path traversal is the only outcome that is not a 200.
fn status_of(outcome: &FindingContextOutcome) -> StatusCode {
    if *outcome == FindingContextOutcome::PathTraversal {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    }
}

fn respond(status: StatusCode, body: impl serde::Serialize) -> Response {
    no_store((status, Json(body)).into_response())
}

/// A refusal in the shared vocabulary, labelled with where it was read from.
fn refusal(refused: Refused, source: &str, git: Option<(&str, bool)>) -> Response {
    let outcome = FindingContextOutcome::from(refused);
    respond(status_of(&outcome), labelled(&outcome, source, git))
}

/// `GET /api/onboard/source-file`: the whole file (or an 800-line window) at the inspected
/// commit, redacted and highlighted, or the reason it cannot be shown. See the module docs.
/// Always `Cache-Control: no-store`.
///
/// Invariant: a deleted source reads nothing (the resolver answers before any path is built) and
/// is never shown from the working tree.
pub async fn source_file(state: &AppState, params: SourceFileParams) -> Response {
    source_file_with(state, params, RenderLimits::default()).await
}

/// [`source_file`] with explicit render limits (tests shrink them).
pub async fn source_file_with(
    state: &AppState,
    params: SourceFileParams,
    limits: RenderLimits,
) -> Response {
    let read = match source_for_read(
        state,
        params.project.as_deref(),
        &params.repo,
        params.sha.as_deref(),
    )
    .await
    {
        Ok(read) => read,
        Err(refusal) => return no_store(refusal.into_response()),
    };
    let (around, path) = (params.around, params.path.as_str());
    match read {
        SourceRead::Deleted {
            deleted_at,
            record_id,
        } => respond(
            StatusCode::OK,
            FindingContextOutcome::SourceDeleted {
                deleted_at,
                record_id,
            }
            .to_json(),
        ),
        // Not linked / not on disk: indistinguishable from "file missing" to the UI.
        SourceRead::Unresolved { .. } => {
            respond(StatusCode::OK, FindingContextOutcome::FileMissing.to_json())
        }
        // No commit is known for this inspection: the working tree, labelled.
        SourceRead::WorkingTree { dir } => {
            match read_rendered(Origin::WorkingTree(&dir), path, around, limits).await {
                Ok(rendered) => respond(
                    StatusCode::OK,
                    ContentBody::new(&rendered, "ok", "working_tree"),
                ),
                Err(refused) => refusal(refused, "working_tree", None),
            }
        }
        SourceRead::Git {
            dir,
            sha,
            head_differs,
        } => match read_rendered(
            Origin::Git {
                dir: &dir,
                sha: &sha,
            },
            path,
            around,
            limits,
        )
        .await
        {
            Ok(rendered) => {
                let mut body = ContentBody::new(&rendered, "ok", "git");
                body.commit = Some(&sha);
                body.head_differs = Some(head_differs);
                respond(StatusCode::OK, body)
            }
            Err(Refused::CommitUnavailable) => {
                // The one labelled fallback, and only while the folder exists (the resolver
                // checked): the working tree, loudly. The status stays `commit_unavailable`.
                let outcome = FindingContextOutcome::CommitUnavailable;
                match read_rendered(Origin::WorkingTree(&dir), path, around, limits).await {
                    Ok(rendered) => {
                        let mut body =
                            ContentBody::new(&rendered, "commit_unavailable", "working_tree");
                        body.reason = outcome.to_json()["reason"].as_str().map(str::to_string);
                        respond(StatusCode::OK, body)
                    }
                    Err(_) => respond(StatusCode::OK, labelled(&outcome, "working_tree", None)),
                }
            }
            Err(refused) => refusal(refused, "git", Some((&sha, head_differs))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The highlighter's budget in these tests: effectively unbounded. Production uses a
    /// 250 ms wall-clock budget, which a debug build under a loaded parallel test run can
    /// exceed (that is what made `a_file_between_the_two_caps_is_windowed_around_the_line`
    /// flaky: the highlight came back `Skipped`), so a test that asserts the highlighted outcome
    /// must not race the clock. The over-budget behaviour has its own deterministic tests.
    const TEST_HIGHLIGHT_BUDGET: Duration = Duration::from_secs(600);

    /// `prepare` + `decorate` without the runtime, as one value.
    fn render(
        bytes: Vec<u8>,
        path: &str,
        around: Option<usize>,
    ) -> Result<(Prepared, Decorated), Refused> {
        let prepared = prepare(bytes, path, around)?;
        let decorated = decorate(&prepared, path, around, TEST_HIGHLIGHT_BUDGET);
        Ok((prepared, decorated))
    }

    fn text(decorated: &Decorated, n: usize) -> String {
        decorated.lines[n].iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn window_is_800_lines_centred_and_clamped() {
        assert_eq!(window_around(1, 5000), (1, 800));
        assert_eq!(window_around(2500, 5000), (2100, 2899));
        assert_eq!(window_around(5000, 5000), (4201, 5000));
        assert_eq!(window_around(99_999, 5000), (4201, 5000));
        assert_eq!(window_around(0, 5000), (1, 800));
        assert_eq!(window_around(10, 300), (1, 300));
    }

    #[test]
    fn a_small_file_is_whole_with_a_block_and_highlight() {
        let src = "// c\nfn f() {\n    let s = \"x\";\n}\n";
        let (p, d) = render(src.as_bytes().to_vec(), "src/lib.rs", Some(3)).unwrap();
        assert_eq!((p.from, p.to, p.lines.len()), (1, 4, 4));
        assert_eq!(p.language, "rust");
        assert_eq!(d.highlight, HighlightStatus::Highlighted);
        let block = d.block.unwrap();
        assert_eq!((block.start_line, block.end_line), (2, 4));
        assert_eq!(block.kind.label(), "function");
        // Segment texts always re-join to the line.
        for n in 0..4 {
            assert_eq!(text(&d, n), src.lines().nth(n).unwrap());
        }
        assert!(d.lines[2]
            .iter()
            .any(|(c, t)| *c == Some(TokenClass::String.index()) && t == "\"x\""));
    }

    #[test]
    fn a_file_between_the_two_caps_is_windowed_around_the_line() {
        let body: String = (1..=40_000).map(|n| format!("let v{n} = {n};\n")).collect();
        assert!(body.len() > MAX_FILE_BYTES && body.len() < MAX_VIEWER_BYTES);
        let (p, d) = render(body.into_bytes(), "src/big.rs", Some(10_000)).unwrap();
        assert_eq!(p.lines.len(), 40_000);
        assert_eq!((p.from, p.to), (9_600, 10_399));
        assert_eq!(d.lines.len(), 800);
        assert_eq!(text(&d, 0), "let v9600 = 9600;");
        // Only the window and its leading context were highlighted, and it still lines up.
        assert_eq!(d.highlight, HighlightStatus::Highlighted);
        assert!(d.lines[0].iter().any(|(c, _)| c.is_some()));
    }

    #[test]
    fn refusals_above_the_cap_and_for_binary() {
        let big = vec![b'x'; MAX_VIEWER_BYTES + 1];
        assert_eq!(prepare(big, "a.rs", None).unwrap_err(), Refused::TooLarge);
        assert_eq!(
            prepare(vec![0xFF, 0xFE, 0x00], "a.rs", None).unwrap_err(),
            Refused::NotUtf8
        );
    }

    #[test]
    fn an_empty_file_and_an_out_of_range_line_do_not_panic() {
        let (p, d) = render(Vec::new(), "a.rs", Some(7)).unwrap();
        assert_eq!((p.lines.len(), d.lines.len()), (0, 0));
        assert!(d.block.is_none());
        let (_, d) = render(b"fn f() {}\n".to_vec(), "a.rs", Some(99)).unwrap();
        assert!(d.block.is_none());
        assert_eq!(d.lines.len(), 1);
    }

    #[test]
    fn segments_cover_the_line_exactly() {
        let line = "let é = 1; // ünï";
        let spans = vec![
            (TokenClass::Keyword, 0..3),
            (TokenClass::Number, 9..10),
            (TokenClass::Comment, 12..line.len()),
        ];
        let segs = segments(line, &spans);
        let joined: String = segs.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(joined, line);
        assert!(segments("", &[])
            .iter()
            .all(|(c, t)| c.is_none() && t.is_empty()));
    }

    /// The over-budget path, deterministically: a zero budget ALWAYS abandons the highlight, so
    /// the lines come back plain and intact with the status `Skipped`, whatever the machine load.
    #[test]
    fn a_zero_highlight_budget_always_skips_and_keeps_the_text() {
        let src = "fn f() {\n    let s = \"x\";\n}\n";
        let prepared = prepare(src.as_bytes().to_vec(), "src/lib.rs", Some(2)).unwrap();
        let d = decorate(&prepared, "src/lib.rs", Some(2), Duration::ZERO);
        assert_eq!(d.highlight, HighlightStatus::Skipped);
        for n in 0..3 {
            assert_eq!(text(&d, n), src.lines().nth(n).unwrap());
        }
        assert!(d
            .lines
            .iter()
            .all(|segs| segs.iter().all(|(class, _)| class.is_none())));
    }

    #[test]
    fn the_degraded_decoration_is_plain_and_has_no_block() {
        let prepared = prepare(b"fn f() {}\nlet x = 1;\n".to_vec(), "a.rs", Some(1)).unwrap();
        let d = plain_decoration(&prepared);
        assert_eq!(d.highlight, HighlightStatus::Skipped);
        assert!(d.block.is_none());
        assert_eq!(
            d.lines,
            vec![
                vec![(None, "fn f() {}".to_string())],
                vec![(None, "let x = 1;".to_string())]
            ]
        );
    }

    #[test]
    fn every_refusal_maps_into_the_shared_status_vocabulary() {
        for (refused, status) in [
            (Refused::FileMissing, "file_missing"),
            (Refused::NotUtf8, "not_utf8"),
            (Refused::TooLarge, "too_large"),
            (Refused::PathTraversal, "path_traversal"),
            (Refused::CommitUnavailable, "commit_unavailable"),
            (Refused::Busy, "unavailable"),
        ] {
            let json = FindingContextOutcome::from(refused).to_json();
            assert_eq!(json["status"], status);
            assert!(json["reason"].as_str().is_some_and(|r| !r.is_empty()));
        }
    }

    /// A.15: the viewer's modules have no write path and start no process of their own. (The
    /// needles are assembled so this test does not match itself.)
    #[test]
    fn the_viewer_modules_have_no_write_path() {
        let needles: Vec<String> = [
            ["fs::wri", "te"],
            ["remove", "_"],
            ["re", "name"],
            ["create", "_dir"],
            ["Open", "Options"],
            ["File::cre", "ate"],
            ["Command", "::new"],
            ["set_", "permissions"],
        ]
        .iter()
        .map(|parts| parts.concat())
        .collect();
        for (name, text) in [
            ("source_file.rs", include_str!("source_file.rs")),
            (
                "finding_context_batch.rs",
                include_str!("finding_context_batch.rs"),
            ),
            (
                "highlight.rs",
                include_str!("../../../checks/src/extract/highlight.rs"),
            ),
            (
                "nesting.rs",
                include_str!("../../../checks/src/extract/nesting.rs"),
            ),
        ] {
            let production = text.split("#[cfg(test)]").next().unwrap();
            for needle in &needles {
                assert!(
                    !production.contains(needle.as_str()),
                    "{name} uses {needle}"
                );
            }
        }
    }
}
