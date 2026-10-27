//! Batch code context for many findings at once: backs
//! `POST /api/onboard/finding-context/batch` (`WORK_ORDER_REVIEW_SPEED.md` B.2 "Data", B.3).
//!
//! The detail pane asks for one finding (a batch of one) 150 ms after the cursor settles; the
//! stacked group view will ask for a whole rule group. Both want the stored line plus three lines
//! either side, and neither may fetch a file per finding, so this takes up to
//! [`MAX_BATCH_ITEMS`] `(repo, path, line, sha)` tuples and answers each one.
//!
//! # Nothing is re-implemented
//!
//! Every judgment is the one the single-finding and whole-file endpoints already make:
//!
//! - **where** the code may be read from (an owned `owner/name` repo, a deletion record first, a
//!   40-hex sha, the inspected commit through git objects, the working tree only when no commit
//!   was recorded): [`source_for_read`];
//! - **the path jail** and the read itself: `source_file::read_bytes`, which is
//!   `resolve_contained_path`, `is_plain_relative_path` and `read_git_blob` (so `GIT_READS` gates
//!   every git process);
//! - **redaction**: `source_file::redacted_lines`, which is [`redact_block`]. The WHOLE file is
//!   redacted as one text BEFORE any slicing, so a multi-line key block that starts above a
//!   window is caught wherever it is cut;
//! - **statuses and reasons**: [`FindingContextOutcome`], per item, labelled by
//!   [`labelled`]. `ok`, `commit_unavailable`, `source_deleted`, `file_missing`, `not_utf8`,
//!   `too_large`, `line_gone`, `path_traversal` and `unavailable` (the shared `Busy`).
//!
//! # Differences from `finding-context`, on purpose
//!
//! - **No working-tree fallback.** `commit_unavailable` carries no lines: this surface sits
//!   beside a stored snippet, and code from a different commit under a finding is a false
//!   positive, which is worse than no context.
//! - **A window, not the enclosing block.** The stored line plus [`CONTEXT_RADIUS`] lines either
//!   side (clipped to the file), no tree-sitter work.
//! - **`path_traversal` is a per-item status** in a 200 answer: one bad tuple must not blank the
//!   other 199 cards. No file is read for it.
//! - **A malformed sha** (not 40 hex) is a 400 for the whole request, as everywhere else.
//!
//! # The git read gate
//!
//! `read_git_blob` takes a `GIT_READS` slot per file and releases it when that file's blob is
//! read, so a batch never holds a slot between files. Files are read [`READ_CONCURRENCY`] at a
//! time (of the gate's 4), never all at once: 200 simultaneous reads would queue 196 of them past
//! the gate's 3 second wait and answer them `unavailable`, and a batch holding every slot would
//! starve the symbol search and the viewer. There is no hold-and-wait anywhere (a read takes one
//! slot, runs, releases), so two batches cannot deadlock; at worst a read waits its turn and, past
//! 3 seconds, that item alone answers `unavailable`.
//!
//! Each distinct `(repo, sha, path)` is read, decoded and redacted once, however many tuples cite
//! it. Read-only by construction: no write, and the only subprocesses are the read-only git
//! calls inside `read_git_blob`.

use std::collections::HashMap;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream::{self, StreamExt};

use super::files::MAX_FILE_BYTES;
use super::finding_context::{language_label, snippet_matches, FindingContextOutcome};
use super::source_file::{read_bytes, redacted_lines, Origin, Refused, RenderLimits};
use crate::client_source::read::{labelled, no_store, source_for_read, SourceRead};
use crate::client_source::Refusal;
use crate::AppState;

/// The most tuples one request may carry. More is a 400 (`too_many_items`).
pub const MAX_BATCH_ITEMS: usize = 200;

/// Lines of context either side of the stored line.
pub const CONTEXT_RADIUS: usize = 3;

/// Files read at once (see the module docs on the gate).
pub const READ_CONCURRENCY: usize = 2;

/// Body of `POST /api/onboard/finding-context/batch`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BatchParams {
    /// The project the findings belong to; defaults to the active project, then any project that
    /// has the repo (as in the single-finding endpoint).
    #[serde(default)]
    pub project: Option<String>,
    pub items: Vec<BatchItem>,
}

/// One finding to show context for.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BatchItem {
    /// `owner/repo`.
    pub repo: String,
    /// Repo-relative path, as on the finding.
    pub path: String,
    /// 1-based violation line.
    pub line: usize,
    /// The inspected commit (40 hex characters); defaults to the commit the project's latest
    /// inspection recorded for the repo.
    #[serde(default)]
    pub sha: Option<String>,
    /// The first line of the stored snippet, for the staleness check (`matches_snippet`).
    #[serde(default)]
    pub expect: Option<String>,
    /// Opaque text echoed back unchanged, so a client can match an answer to a finding.
    #[serde(default)]
    pub key: Option<String>,
}

/// Where the code of one `(repo, sha)` comes from, resolved once per request.
type Resolved = Result<SourceRead, Refusal>;

/// What the shared tail of a read produced for one file: the redacted lines, or why not.
type FileLines = Result<Vec<String>, FindingContextOutcome>;

/// `POST /api/onboard/finding-context/batch`. Always `Cache-Control: no-store`.
///
/// Answers `{ "items": [...] }`, one entry per request tuple, in request order. Each entry echoes
/// `index`, `key` (when sent), `repo`, `path`, `line` and `sha` (as sent, `""` when none), and
/// carries the shared `status` vocabulary. An `ok` entry adds `source`, `commit`, `head_differs`,
/// `start_line`, `end_line`, `violation_line`, `lines` (redacted), `language`, `kind`
/// (`"window"`) and `matches_snippet`.
pub async fn finding_context_batch(state: &AppState, params: BatchParams) -> Response {
    if params.items.len() > MAX_BATCH_ITEMS {
        return no_store(
            Refusal::bad_request(
                "too_many_items",
                format!(
                    "A batch holds at most {MAX_BATCH_ITEMS} findings; got {}.",
                    params.items.len()
                ),
            )
            .into_response(),
        );
    }

    // Resolve each distinct (repo, sha) once: the resolver spawns a git process for the head
    // commit, and a group of findings shares a repo.
    let mut resolved: HashMap<(String, Option<String>), Resolved> = HashMap::new();
    for item in &params.items {
        let key = (item.repo.clone(), item.sha.clone());
        if resolved.contains_key(&key) {
            continue;
        }
        let read = source_for_read(
            state,
            params.project.as_deref(),
            &item.repo,
            item.sha.as_deref(),
        )
        .await;
        resolved.insert(key, read);
    }
    // A malformed sha is the caller's bug, not a property of one file: refuse the request.
    if let Some(refusal) = resolved.values().find_map(|r| r.as_ref().err()) {
        return no_store(refusal.clone().into_response());
    }

    // Group the tuples by file, in first-seen order, so each file is read and redacted once.
    let mut order: Vec<FileKey> = Vec::new();
    let mut by_file: HashMap<FileKey, Vec<usize>> = HashMap::new();
    for (i, item) in params.items.iter().enumerate() {
        let key = FileKey {
            repo: item.repo.clone(),
            sha: item.sha.clone(),
            path: item.path.clone(),
        };
        by_file
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key.clone());
                Vec::new()
            })
            .push(i);
    }

    let items = &params.items;
    let resolved = &resolved;
    let answered: Vec<Vec<(usize, serde_json::Value)>> = stream::iter(order)
        .map(|file| {
            let indexes = by_file.get(&file).cloned().unwrap_or_default();
            async move {
                let Some(Ok(read)) = resolved.get(&(file.repo.clone(), file.sha.clone())) else {
                    // Unreachable: a refusal returned above. Answered closed all the same.
                    return indexes
                        .into_iter()
                        .map(|i| {
                            (
                                i,
                                answer(&items[i], i, &FindingContextOutcome::FileMissing, None),
                            )
                        })
                        .collect::<Vec<_>>();
                };
                answer_file(read, &file.path, &indexes, items).await
            }
        })
        .buffered(READ_CONCURRENCY)
        .collect()
        .await;

    let mut out: Vec<serde_json::Value> = vec![serde_json::Value::Null; params.items.len()];
    for (i, value) in answered.into_iter().flatten() {
        out[i] = value;
    }
    no_store((StatusCode::OK, Json(serde_json::json!({ "items": out }))).into_response())
}

/// A file as the batch groups it: the request's own repo, sha (as sent) and path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FileKey {
    repo: String,
    sha: Option<String>,
    path: String,
}

/// How a response entry is labelled: where the lines were read from.
enum Label<'a> {
    Git { sha: &'a str, head_differs: bool },
    WorkingTree,
}

/// Answer every tuple that cites one file: read it once, redact the whole of it, slice per tuple.
async fn answer_file(
    read: &SourceRead,
    path: &str,
    indexes: &[usize],
    items: &[BatchItem],
) -> Vec<(usize, serde_json::Value)> {
    let whole = |outcome: FindingContextOutcome, label: Option<Label<'_>>| {
        indexes
            .iter()
            .map(|&i| (i, answer(&items[i], i, &outcome, label.as_ref())))
            .collect::<Vec<_>>()
    };
    let (origin, label) = match read {
        SourceRead::Deleted {
            deleted_at,
            record_id,
        } => {
            return whole(
                FindingContextOutcome::SourceDeleted {
                    deleted_at: deleted_at.clone(),
                    record_id: record_id.clone(),
                },
                None,
            )
        }
        // Not linked / not on disk: indistinguishable from "file missing" to the UI.
        SourceRead::Unresolved { .. } => return whole(FindingContextOutcome::FileMissing, None),
        SourceRead::WorkingTree { dir } => (Origin::WorkingTree(dir), Label::WorkingTree),
        SourceRead::Git {
            dir,
            sha,
            head_differs,
        } => (
            Origin::Git { dir, sha },
            Label::Git {
                sha,
                head_differs: *head_differs,
            },
        ),
    };

    let lines = match file_lines(origin, path).await {
        Ok(lines) => lines,
        // The commit is missing: no working-tree fallback here (module docs).
        Err(outcome @ FindingContextOutcome::CommitUnavailable) => return whole(outcome, None),
        Err(outcome) => return whole(outcome, Some(label)),
    };
    let language = language_label(path);
    indexes
        .iter()
        .map(|&i| {
            let item = &items[i];
            let outcome = slice(&lines, item, &language);
            (i, answer(item, i, &outcome, Some(&label)))
        })
        .collect()
}

/// The redacted lines of one file, or the shared outcome that says why not. Reading is
/// `read_bytes` (jail, then the committed blob or the working-tree file); decoding and
/// redacting is `redacted_lines`, on the blocking pool under the viewer's own time limit.
async fn file_lines(origin: Origin<'_>, path: &str) -> FileLines {
    let bytes = read_bytes(origin, path, MAX_FILE_BYTES)
        .await
        .map_err(outcome_of)?;
    let job = tokio::task::spawn_blocking(move || redacted_lines(bytes, MAX_FILE_BYTES));
    match tokio::time::timeout(RenderLimits::default().prepare, job).await {
        Ok(Ok(result)) => result.map_err(outcome_of),
        // Too slow, or the worker panicked: nothing to show.
        Ok(Err(_)) | Err(_) => Err(FindingContextOutcome::TooLarge),
    }
}

/// A refusal in the shared vocabulary. The cap here is `finding-context`'s (400 KB), so the
/// oversize reason is its `TooLarge`, not the viewer's.
fn outcome_of(refused: Refused) -> FindingContextOutcome {
    match refused {
        Refused::TooLarge => FindingContextOutcome::TooLarge,
        other => other.into(),
    }
}

/// The window of `lines` for one tuple: the stored line and [`CONTEXT_RADIUS`] either side,
/// clipped to the file. A line outside the file is `line_gone`.
fn slice(lines: &[String], item: &BatchItem, language: &str) -> FindingContextOutcome {
    if item.line == 0 || item.line > lines.len() {
        return FindingContextOutcome::LineGone;
    }
    let start = item.line.saturating_sub(CONTEXT_RADIUS).max(1);
    let end = (item.line + CONTEXT_RADIUS).min(lines.len());
    FindingContextOutcome::Ok {
        lines: lines[start - 1..end].to_vec(),
        start_line: start,
        end_line: end,
        violation_line: item.line,
        kind: "window",
        language: language.to_string(),
        matches_snippet: snippet_matches(
            lines.get(item.line - 1).map(String::as_str),
            item.expect.as_deref(),
        ),
    }
}

/// One response entry: the outcome's own JSON body, the source label, and the echo that lets a
/// client match it to a finding.
fn answer(
    item: &BatchItem,
    index: usize,
    outcome: &FindingContextOutcome,
    label: Option<&Label<'_>>,
) -> serde_json::Value {
    let mut body = match label {
        Some(Label::Git { sha, head_differs }) => {
            labelled(outcome, "git", Some((sha, *head_differs)))
        }
        Some(Label::WorkingTree) => labelled(outcome, "working_tree", None),
        None => outcome.to_json(),
    };
    if let Some(map) = body.as_object_mut() {
        map.insert("index".to_string(), serde_json::json!(index));
        if let Some(key) = &item.key {
            map.insert("key".to_string(), serde_json::json!(key));
        }
        map.insert("repo".to_string(), serde_json::json!(item.repo));
        map.insert("path".to_string(), serde_json::json!(item.path));
        map.insert("line".to_string(), serde_json::json!(item.line));
        map.insert(
            "sha".to_string(),
            serde_json::json!(item.sha.as_deref().unwrap_or("")),
        );
    }
    body
}
