//! Whole-file source read for the in-app code viewer: backs `GET /api/onboard/source-file`
//! (`WORK_ORDER_REVIEW_SPEED.md` A.2 and A.3).
//!
//! The finding modal shows the enclosing block of one finding; the viewer shows the FILE, so a
//! reviewer reads the code around a finding without leaving the app. Everything that decides
//! where code may be read from is shared with `finding-context`, not copied:
//!
//! - **where** (inspected commit, working tree, or nothing because the source was deleted):
//!   [`crate::client_source::read::source_for_read`], the one resolver;
//! - **path jail** and the git object read: [`super::finding_context`]'s
//!   `resolve_contained_path`, `is_plain_relative_path` and `read_git_blob`;
//! - **redaction**: [`super::finding_context::redact_block`], which calls
//!   `camerata_secrets::redact_str`. Every line leaves this module redacted, and the text that is
//!   highlighted and parsed for the enclosing block is the redacted text, so no raw line is ever
//!   held past the redaction step.
//!
//! # Statuses
//!
//! | `status` | Meaning | Carries lines |
//! |---|---|---|
//! | `ok` | Read from the inspected commit (`source: "git"`) | yes |
//! | `commit_unavailable` | No commit was recorded, or this clone lacks it; the working tree is shown (`source: "working_tree"`) | yes |
//! | `source_deleted` | The client source was deleted on purpose; nothing is read, `deleted_at` carries the date | no |
//! | `file_missing` | No such file (at the commit, or in the working tree after a fallback) | no |
//! | `not_utf8` / `too_large` | Refused (binary, or above [`MAX_VIEWER_BYTES`]) | no |
//! | `path_traversal` | The path escapes the repo; the only non-200 | no |
//!
//! A file up to `MAX_FILE_BYTES` (400 KB) is returned whole. A larger one, up to 4 MB, is
//! returned as an 800-line window centred on `around`, with `window` saying which lines.
//!
//! Read-only by construction: a GET, no filesystem write, and the only subprocess is the
//! read-only `git cat-file` in `read_git_blob`.

use std::path::Path;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use camerata_checks::extract::enclosing::{enclosing_block, EnclosingBlock};
use camerata_checks::extract::highlight::{
    highlight, highlight_lang_for_path, HighlightStatus, TokenClass,
};

use super::files::MAX_FILE_BYTES;
use super::finding_context::{
    is_plain_relative_path, language_label, read_git_blob, read_working_tree_file, redact_block,
    resolve_contained_path, FindingContextOutcome, GitReadError,
};
use crate::client_source::read::{no_store, source_for_read, SourceRead};
use crate::AppState;

/// Above this a file is refused outright (`too_large`).
pub const MAX_VIEWER_BYTES: usize = 4 * 1024 * 1024;

/// Lines in the window returned for a file above [`MAX_FILE_BYTES`].
pub const WINDOW_LINES: usize = 800;

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
}

/// A file read, redacted, windowed and highlighted: everything the response body carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rendered {
    pub language: String,
    pub total_lines: usize,
    /// 1-based inclusive line range actually returned.
    pub from: usize,
    pub to: usize,
    pub highlight: HighlightStatus,
    pub block: Option<EnclosingBlock>,
    /// Per returned line, `(class index or None for plain, text)` segments.
    pub lines: Vec<Vec<(Option<usize>, String)>>,
}

/// Where one read comes from.
#[derive(Clone, Copy)]
pub(crate) enum Origin<'a> {
    WorkingTree(&'a Path),
    Git { dir: &'a Path, sha: &'a str },
}

impl Origin<'_> {
    fn dir(&self) -> &Path {
        match self {
            Origin::WorkingTree(dir) | Origin::Git { dir, .. } => dir,
        }
    }
}

/// Read, redact, window and highlight `rel_path` from `origin`. The path jail runs before any
/// read. Never panics.
pub(crate) async fn read_rendered(
    origin: Origin<'_>,
    rel_path: &str,
    around: Option<usize>,
) -> Result<Rendered, Refused> {
    let Some(abs_path) = resolve_contained_path(origin.dir(), rel_path) else {
        return Err(Refused::PathTraversal);
    };
    let bytes = match origin {
        Origin::WorkingTree(_) => {
            // The size is checked first so an oversized file is never read into memory.
            match tokio::fs::metadata(&abs_path).await {
                Ok(meta) if meta.len() > MAX_VIEWER_BYTES as u64 => return Err(Refused::TooLarge),
                Ok(_) => {}
                Err(_) => return Err(Refused::FileMissing),
            }
            read_working_tree_file(&abs_path)
                .await
                .map_err(|_| Refused::FileMissing)?
        }
        Origin::Git { dir, sha } => {
            if !is_plain_relative_path(rel_path) {
                return Err(Refused::PathTraversal);
            }
            read_git_blob(dir, sha, rel_path, MAX_VIEWER_BYTES)
                .await
                .map_err(|e| match e {
                    GitReadError::CommitUnavailable => Refused::CommitUnavailable,
                    GitReadError::NotFound => Refused::FileMissing,
                    GitReadError::TooLarge => Refused::TooLarge,
                })?
        }
    };
    render(bytes, rel_path, around)
}

/// The pure tail of a read: size, UTF-8, redaction, window, enclosing block, highlight.
pub(crate) fn render(
    bytes: Vec<u8>,
    rel_path: &str,
    around: Option<usize>,
) -> Result<Rendered, Refused> {
    if bytes.len() > MAX_VIEWER_BYTES {
        return Err(Refused::TooLarge);
    }
    let whole = bytes.len() <= MAX_FILE_BYTES;
    let source = String::from_utf8(bytes).map_err(|_| Refused::NotUtf8)?;

    // Redact the whole file as one text (a multi-line key body is only recognisable as a
    // block), then work only on the redacted lines. The `\n` count is preserved, so line numbers
    // hold.
    let raw_lines: Vec<String> = source.lines().map(str::to_string).collect();
    drop(source);
    let lines = redact_block(&raw_lines);
    drop(raw_lines);
    let total_lines = lines.len();
    let text = lines.join("\n");

    let (from, to) = if whole || total_lines == 0 {
        (1, total_lines)
    } else {
        window_around(around.unwrap_or(1), total_lines)
    };

    let block = around.and_then(|line| enclosing_block(rel_path, &text, line));
    let highlighted = highlight(highlight_lang_for_path(rel_path), &text);

    let mut out = Vec::with_capacity(to.saturating_sub(from) + 1);
    for n in from..=to {
        let Some(line) = lines.get(n - 1) else { break };
        let spans = highlighted.lines.get(n - 1).map_or(&[][..], Vec::as_slice);
        out.push(segments(line, spans));
    }
    Ok(Rendered {
        language: language_label(rel_path),
        total_lines,
        from,
        to: if out.is_empty() {
            to
        } else {
            from + out.len() - 1
        },
        highlight: highlighted.status,
        block,
        lines: out,
    })
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
fn segments(
    line: &str,
    spans: &[(TokenClass, std::ops::Range<usize>)],
) -> Vec<(Option<usize>, String)> {
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

fn refusal_reason(refused: Refused) -> &'static str {
    match refused {
        Refused::FileMissing => "file not found at the inspected commit",
        Refused::NotUtf8 => "file is not valid UTF-8 (binary?)",
        Refused::TooLarge => "file exceeds the 4 MB viewer cap",
        Refused::PathTraversal => "requested path escapes the repo directory",
        Refused::CommitUnavailable => {
            "Inspected commit not available in this clone; showing the working tree"
        }
    }
}

fn refusal_body(refused: Refused, source: Option<&str>) -> serde_json::Value {
    let status = match refused {
        Refused::FileMissing => "file_missing",
        Refused::NotUtf8 => "not_utf8",
        Refused::TooLarge => "too_large",
        Refused::PathTraversal => "path_traversal",
        Refused::CommitUnavailable => "commit_unavailable",
    };
    let mut body = serde_json::json!({ "status": status, "reason": refusal_reason(refused) });
    if let (Some(map), Some(source)) = (body.as_object_mut(), source) {
        map.insert("source".to_string(), serde_json::json!(source));
    }
    body
}

fn highlight_label(status: HighlightStatus) -> &'static str {
    match status {
        HighlightStatus::Highlighted => "ok",
        HighlightStatus::Plain => "plain",
        HighlightStatus::Skipped => "skipped",
    }
}

/// The response body for a rendered file.
fn content_body(rendered: &Rendered, status: &str, source: &str) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "source": source,
        "language": rendered.language,
        "total_lines": rendered.total_lines,
        "window": {
            "from": rendered.from,
            "to": rendered.to,
            "total_lines": rendered.total_lines,
        },
        "highlight": highlight_label(rendered.highlight),
        "classes": TokenClass::CLASS_NAMES,
        "block": rendered.block.map(|b| serde_json::json!({
            "start_line": b.start_line,
            "end_line": b.end_line,
            "kind": b.kind.label(),
        })),
        "lines": rendered.lines,
    })
}

/// `GET /api/onboard/source-file`: the whole file (or an 800-line window) at the inspected
/// commit, redacted and highlighted, or the reason it cannot be shown. See the module docs.
/// Always `Cache-Control: no-store`.
///
/// Invariant: a deleted source reads nothing (the resolver answers before any path is built) and
/// is never shown from the working tree.
pub async fn source_file(state: &AppState, params: SourceFileParams) -> Response {
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
    let around = params.around;
    let path = params.path.as_str();
    let (status, body) = match read {
        SourceRead::Deleted {
            deleted_at,
            record_id,
        } => (
            StatusCode::OK,
            FindingContextOutcome::SourceDeleted {
                deleted_at,
                record_id,
            }
            .to_json(),
        ),
        SourceRead::Unresolved { .. } => (StatusCode::OK, refusal_body(Refused::FileMissing, None)),
        SourceRead::WorkingTree { dir } => {
            // No commit is known for this inspection: the working tree, labelled.
            match read_rendered(Origin::WorkingTree(&dir), path, around).await {
                Ok(rendered) => {
                    let mut body = content_body(&rendered, "commit_unavailable", "working_tree");
                    set_reason(
                        &mut body,
                        "No commit was recorded for this inspection; showing the working tree",
                    );
                    (StatusCode::OK, body)
                }
                Err(refused) => (
                    status_of(refused),
                    refusal_body(refused, Some("working_tree")),
                ),
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
        )
        .await
        {
            Ok(rendered) => {
                let mut body = content_body(&rendered, "ok", "git");
                if let Some(map) = body.as_object_mut() {
                    map.insert("commit".to_string(), serde_json::json!(sha));
                    map.insert("head_differs".to_string(), serde_json::json!(head_differs));
                }
                (StatusCode::OK, body)
            }
            Err(Refused::CommitUnavailable) => {
                // The one labelled fallback, and only while the folder exists (the resolver
                // checked): the working tree, loudly.
                match read_rendered(Origin::WorkingTree(&dir), path, around).await {
                    Ok(rendered) => {
                        let mut body =
                            content_body(&rendered, "commit_unavailable", "working_tree");
                        set_reason(&mut body, refusal_reason(Refused::CommitUnavailable));
                        (StatusCode::OK, body)
                    }
                    Err(refused) => (
                        status_of(refused),
                        refusal_body(refused, Some("working_tree")),
                    ),
                }
            }
            Err(refused) => (status_of(refused), refusal_body(refused, Some("git"))),
        },
    };
    no_store((status, Json(body)).into_response())
}

fn set_reason(body: &mut serde_json::Value, reason: &str) {
    if let Some(map) = body.as_object_mut() {
        map.insert("reason".to_string(), serde_json::json!(reason));
    }
}

/// Path traversal is the only outcome that is not a 200.
fn status_of(refused: Refused) -> StatusCode {
    if refused == Refused::PathTraversal {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rendered: &Rendered, n: usize) -> String {
        rendered.lines[n].iter().map(|(_, t)| t.as_str()).collect()
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
        let r = render(src.as_bytes().to_vec(), "src/lib.rs", Some(3)).unwrap();
        assert_eq!((r.from, r.to, r.total_lines), (1, 4, 4));
        assert_eq!(r.language, "rust");
        assert_eq!(r.highlight, HighlightStatus::Highlighted);
        let block = r.block.unwrap();
        assert_eq!((block.start_line, block.end_line), (2, 4));
        assert_eq!(block.kind.label(), "function");
        assert_eq!(text(&r, 2), "    let s = \"x\";");
        // Segment texts always re-join to the line.
        for n in 0..4 {
            assert_eq!(text(&r, n), src.lines().nth(n).unwrap());
        }
        assert!(r.lines[2]
            .iter()
            .any(|(c, t)| *c == Some(TokenClass::String.index()) && t == "\"x\""));
    }

    #[test]
    fn a_file_between_the_two_caps_is_windowed_around_the_line() {
        let body: String = (1..=40_000).map(|n| format!("let v{n} = {n};\n")).collect();
        assert!(body.len() > MAX_FILE_BYTES && body.len() < MAX_VIEWER_BYTES);
        let r = render(body.into_bytes(), "src/big.rs", Some(10_000)).unwrap();
        assert_eq!(r.total_lines, 40_000);
        assert_eq!((r.from, r.to), (9_600, 10_399));
        assert_eq!(r.lines.len(), 800);
        assert_eq!(text(&r, 0), "let v9600 = 9600;");
    }

    #[test]
    fn refusals_above_the_cap_and_for_binary() {
        let big = vec![b'x'; MAX_VIEWER_BYTES + 1];
        assert_eq!(render(big, "a.rs", None), Err(Refused::TooLarge));
        assert_eq!(
            render(vec![0xFF, 0xFE, 0x00], "a.rs", None),
            Err(Refused::NotUtf8)
        );
    }

    #[test]
    fn an_empty_file_and_an_out_of_range_line_do_not_panic() {
        let r = render(Vec::new(), "a.rs", Some(7)).unwrap();
        assert_eq!((r.total_lines, r.lines.len()), (0, 0));
        assert!(r.block.is_none());
        let r = render(b"fn f() {}\n".to_vec(), "a.rs", Some(99)).unwrap();
        assert!(r.block.is_none());
        assert_eq!(r.lines.len(), 1);
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
                "highlight.rs",
                include_str!("../../../checks/src/extract/highlight.rs"),
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
