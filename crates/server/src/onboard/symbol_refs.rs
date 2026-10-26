//! Symbol references for the in-app code viewer: backs `GET /api/onboard/symbol-refs`
//! (`WORK_ORDER_REVIEW_SPEED.md` A.6, "where else is this symbol used").
//!
//! One read-only `git grep` over the inspected commit. Everything that decides where code may be
//! read from, and every status word, is shared with `source-file` and `finding-context`, not
//! copied:
//!
//! - **where** (repo must be an exact `owner/name` a project owns; the sha is 40 hex characters;
//!   a deleted source reads nothing): [`crate::client_source::read::source_for_read`];
//! - **statuses**: [`FindingContextOutcome`], with the same source labels
//!   ([`crate::client_source::read::labelled`]);
//! - **noise paths**: [`super::files::is_noise_path`], the scan's own filter;
//! - **redaction**: [`camerata_secrets::redact_str`] over every hit line before it leaves.
//!
//! # Response
//!
//! `{status, source, commit?, head_differs?, symbol, hits: [{path, line, text}], more}`.
//!
//! | `status` | Meaning | Carries `hits` |
//! |---|---|---|
//! | `ok` | Searched at the inspected commit (`source: "git"`), or the working tree when the inspection recorded no commit (`source: "working_tree"`) | yes (maybe none) |
//! | `commit_unavailable` | The recorded commit is not in this clone: the working tree is searched (`source: "working_tree"`), and when it cannot be (not a git checkout) there are no hits | when searchable |
//! | `source_deleted` | The client source was deleted on purpose; nothing is searched | no |
//! | `file_missing` | No project owns that repo, or it has no folder | no |
//! | `too_large` | The search took too long | no |
//!
//! An invalid symbol is a `400` `{ok: false, code: "invalid_symbol"}` before anything is read.
//!
//! # The symbol is data, never an option, never a pattern
//!
//! The symbol must be 1 to [`MAX_SYMBOL_BYTES`] bytes of letters, digits and `_ $ - . :` with at
//! least one letter, digit or `_` ([`is_valid_symbol`], shared with the UI through
//! `camerata-api-types`), so no space, quote, bracket, glob or regex operator can reach git. It is passed after `-e` (a leading `-` is a pattern character,
//! not an option) with `--fixed-strings` (a `.` is a dot) and `-w` (whole words), and no shell is
//! involved.
//!
//! # Bounded
//!
//! The subprocess is read up to [`MAX_OUTPUT_BYTES`] and killed beyond it, runs under
//! [`SEARCH_TIMEOUT`] with `kill_on_drop`, and at most [`MAX_HITS`] hits are returned with
//! `more: true` when there were others. Hit lines are redacted, then cut at
//! [`MAX_HIT_TEXT_CHARS`] characters. Read-only: `git grep` writes nothing, optional index locks
//! are disabled, and it is the only subcommand run here.

use std::path::Path;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::files::is_noise_path;
use super::finding_context::{redact_block, FindingContextOutcome};
use crate::client_source::read::{labelled, no_store, source_for_read, SourceRead};
use crate::client_source::Refusal;
use crate::AppState;
use camerata_api_types::symbol::{is_valid_symbol, MAX_SYMBOL_BYTES};

/// Hits returned; further hits only set `more`.
pub const MAX_HITS: usize = 200;

/// A hit line is cut to this many characters after redaction.
pub const MAX_HIT_TEXT_CHARS: usize = 240;

/// Bytes of `git grep` output read before the process is killed.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// How long the search may run.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

thread_local! {
    /// Test seam: how many searches this thread has started. Lets a test prove that a deleted
    /// or unresolved repo, or a refused request, runs no process.
    #[cfg(test)]
    pub(crate) static GREP_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Query of `GET /api/onboard/symbol-refs`.
#[derive(Debug, serde::Deserialize)]
pub struct SymbolRefsParams {
    /// `owner/repo`.
    pub repo: String,
    /// The symbol to search for (see the module docs for the accepted shape).
    pub symbol: String,
    /// The inspected commit (40 hex characters). Defaults to the commit the project's latest
    /// inspection recorded for the repo.
    #[serde(default)]
    pub sha: Option<String>,
    /// The project the finding belongs to; defaults to the active project, then any project
    /// that has the repo.
    #[serde(default)]
    pub project: Option<String>,
}

/// One reference: a code location, redacted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// The line, redacted, leading whitespace trimmed, cut at [`MAX_HIT_TEXT_CHARS`].
    pub text: String,
}

/// What one `git grep` produced.
#[derive(Debug, PartialEq, Eq)]
enum Searched {
    /// Hits (capped) and whether there were more.
    Done { hits: Vec<Hit>, more: bool },
    /// git could not search here (not a repository, unknown tree).
    Failed,
    /// Over [`SEARCH_TIMEOUT`].
    TooSlow,
}

/// Where one search runs.
#[derive(Clone, Copy)]
enum Where<'a> {
    WorkingTree(&'a Path),
    Git { dir: &'a Path, sha: &'a str },
}

/// A line that is nothing but base64 characters is key material or a blob, not code, and the
/// per-line redactor cannot see the `-----BEGIN` line that makes it a secret. It is never shown.
fn looks_like_key_body(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 40
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Redact one hit line, trim it and cut it for display.
fn shown_text(raw: &str) -> String {
    if looks_like_key_body(raw) {
        return "[redacted]".to_string();
    }
    let redacted = redact_block(&[raw.to_string()]).concat();
    let trimmed = redacted.trim_start();
    if trimmed.chars().count() > MAX_HIT_TEXT_CHARS {
        let cut: String = trimmed.chars().take(MAX_HIT_TEXT_CHARS).collect();
        format!("{cut}...")
    } else {
        trimmed.to_string()
    }
}

/// Parse `git grep -z -n` output (`[<sha>:]<path> NUL <line> NUL <text> LF`) into hits, dropping
/// noise paths, redacting the text, stopping after [`MAX_HITS`] (the next one sets `more`).
fn parse_hits(output: &[u8], sha: Option<&str>, truncated: bool) -> (Vec<Hit>, bool) {
    let prefix = sha.map(|s| format!("{s}:"));
    let mut hits = Vec::new();
    let mut more = truncated;
    for record in output.split(|b| *b == b'\n') {
        let mut parts = record.splitn(3, |b| *b == 0);
        let (Some(name), Some(line), Some(text)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(name), Ok(line)) = (std::str::from_utf8(name), std::str::from_utf8(line)) else {
            continue;
        };
        let Ok(line) = line.parse::<usize>() else {
            continue;
        };
        let path = match &prefix {
            Some(p) => name.strip_prefix(p.as_str()).unwrap_or(name),
            None => name,
        };
        if path.is_empty() || is_noise_path(path, &[]) {
            continue;
        }
        if hits.len() >= MAX_HITS {
            more = true;
            break;
        }
        // A trailing `\r` is the line ending, not code.
        let text = String::from_utf8_lossy(text);
        hits.push(Hit {
            path: path.to_string(),
            line,
            text: shown_text(text.trim_end_matches('\r')),
        });
    }
    (hits, more)
}

/// Run the one read-only `git grep` and read its output, bounded.
async fn git_grep(at: Where<'_>, symbol: &str) -> Searched {
    use tokio::io::AsyncReadExt;

    let (dir, tree) = match at {
        Where::WorkingTree(dir) => (dir, None),
        Where::Git { dir, sha } => (dir, Some(sha)),
    };
    #[cfg(test)]
    GREP_RUNS.with(|c| c.set(c.get() + 1));
    let mut command = tokio::process::Command::new("git");
    command
        .current_dir(dir)
        // Never fall back to searching a plain folder, never recurse into submodules, whatever
        // the user's git configuration says.
        .args(["-c", "grep.fallbackToNoIndex=false"])
        .args(["grep", "-z", "-n", "-w", "-I", "-F", "--no-color"])
        .arg("--no-recurse-submodules")
        // The symbol is the pattern: after `-e`, a leading dash is part of it.
        .arg("-e")
        .arg(symbol);
    if let Some(sha) = tree {
        command.arg(sha);
    }
    command
        .arg("--")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return Searched::Failed;
    };
    let Some(stdout) = child.stdout.take() else {
        return Searched::Failed;
    };

    let run = async {
        let mut bytes = Vec::new();
        let cap = MAX_OUTPUT_BYTES as u64 + 1;
        if stdout.take(cap).read_to_end(&mut bytes).await.is_err() {
            return Searched::Failed;
        }
        let truncated = bytes.len() > MAX_OUTPUT_BYTES;
        if truncated {
            // Past the cap: the rest is never read, and the process is killed below.
            bytes.truncate(MAX_OUTPUT_BYTES);
            let _ = child.kill().await;
            let (hits, more) = parse_in_pool(bytes, tree, true).await;
            return Searched::Done { hits, more };
        }
        match child.wait().await {
            // 0: matches, 1: none.
            Ok(status) if matches!(status.code(), Some(0 | 1)) => {
                let (hits, more) = parse_in_pool(bytes, tree, false).await;
                Searched::Done { hits, more }
            }
            _ => Searched::Failed,
        }
    };
    tokio::time::timeout(SEARCH_TIMEOUT, run)
        .await
        .unwrap_or(Searched::TooSlow)
}

/// [`parse_hits`] on the blocking pool (it redacts up to [`MAX_HITS`] lines).
async fn parse_in_pool(bytes: Vec<u8>, sha: Option<&str>, truncated: bool) -> (Vec<Hit>, bool) {
    let sha = sha.map(str::to_string);
    tokio::task::spawn_blocking(move || parse_hits(&bytes, sha.as_deref(), truncated))
        .await
        .unwrap_or_default()
}

#[derive(serde::Serialize)]
struct Body<'a> {
    status: &'static str,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_differs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    symbol: &'a str,
    hits: &'a [Hit],
    more: bool,
}

fn respond(body: impl serde::Serialize) -> Response {
    no_store((StatusCode::OK, Json(body)).into_response())
}

fn unavailable(symbol: &str) -> Response {
    let outcome = FindingContextOutcome::CommitUnavailable;
    let mut json = labelled(&outcome, "working_tree", None);
    if let Some(map) = json.as_object_mut() {
        map.insert("symbol".to_string(), serde_json::json!(symbol));
        map.insert("hits".to_string(), serde_json::json!([]));
        map.insert("more".to_string(), serde_json::json!(false));
    }
    respond(json)
}

fn too_slow() -> Response {
    respond(serde_json::json!({
        "status": "too_large",
        "reason": "the search took too long; try a more specific symbol",
    }))
}

/// `GET /api/onboard/symbol-refs`: where a symbol is used, searched at the inspected commit (or
/// labelled as the working tree), noise paths dropped, every line redacted, at most
/// [`MAX_HITS`]. See the module docs. Always `Cache-Control: no-store`.
///
/// Invariant: a deleted source runs no process and reads nothing.
pub async fn symbol_refs(state: &AppState, params: SymbolRefsParams) -> Response {
    if !is_valid_symbol(&params.symbol) {
        return no_store(
            Refusal {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_symbol",
                message: format!(
                    "A symbol is 1 to {MAX_SYMBOL_BYTES} letters, digits or _ $ - . : characters."
                ),
            }
            .into_response(),
        );
    }
    let symbol = params.symbol.as_str();
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
    match read {
        SourceRead::Deleted {
            deleted_at,
            record_id,
        } => respond(
            FindingContextOutcome::SourceDeleted {
                deleted_at,
                record_id,
            }
            .to_json(),
        ),
        // Not linked / not on disk: indistinguishable from "file missing" to the UI.
        SourceRead::Unresolved { .. } => respond(FindingContextOutcome::FileMissing.to_json()),
        // No commit is known for this inspection: the working tree, labelled.
        SourceRead::WorkingTree { dir } => match git_grep(Where::WorkingTree(&dir), symbol).await {
            Searched::Done { hits, more } => respond(Body {
                status: "ok",
                source: "working_tree",
                commit: None,
                head_differs: None,
                reason: None,
                symbol,
                hits: &hits,
                more,
            }),
            // Not a git checkout: there is nothing to search that git can vouch for.
            Searched::Failed => unavailable(symbol),
            Searched::TooSlow => too_slow(),
        },
        SourceRead::Git {
            dir,
            sha,
            head_differs,
        } => {
            // Same test `read_git_blob` makes before it reads.
            if !crate::client_source::git::commit_exists(&dir, &sha).await {
                // The one labelled fallback, and only while the folder exists (the resolver
                // checked): the working tree, loudly. The status stays `commit_unavailable`.
                return match git_grep(Where::WorkingTree(&dir), symbol).await {
                    Searched::Done { hits, more } => {
                        let outcome = FindingContextOutcome::CommitUnavailable;
                        respond(Body {
                            status: "commit_unavailable",
                            source: "working_tree",
                            commit: None,
                            head_differs: None,
                            reason: outcome.to_json()["reason"].as_str().map(str::to_string),
                            symbol,
                            hits: &hits,
                            more,
                        })
                    }
                    Searched::Failed => unavailable(symbol),
                    Searched::TooSlow => too_slow(),
                };
            }
            match git_grep(
                Where::Git {
                    dir: &dir,
                    sha: &sha,
                },
                symbol,
            )
            .await
            {
                Searched::Done { hits, more } => respond(Body {
                    status: "ok",
                    source: "git",
                    commit: Some(&sha),
                    head_differs: Some(head_differs),
                    reason: None,
                    symbol,
                    hits: &hits,
                    more,
                }),
                Searched::Failed => unavailable(symbol),
                Searched::TooSlow => too_slow(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_drops_noise_caps_and_flags_more() {
        let mut out = Vec::new();
        out.extend_from_slice(b"S:src/a.rs\x007\x00    let x = foo;\n");
        out.extend_from_slice(b"S:node_modules/dep/index.js\x001\x00foo\n");
        out.extend_from_slice(b"S:Cargo.lock\x002\x00foo\n");
        out.extend_from_slice(b"S:dir:with/colon.rs\x0012\x00foo()\r\n");
        let (hits, more) = parse_hits(&out, Some("S"), false);
        assert!(!more);
        assert_eq!(
            hits,
            vec![
                Hit {
                    path: "src/a.rs".into(),
                    line: 7,
                    text: "let x = foo;".into()
                },
                Hit {
                    path: "dir:with/colon.rs".into(),
                    line: 12,
                    text: "foo()".into()
                },
            ]
        );

        let many: Vec<u8> = (1..=MAX_HITS + 5)
            .flat_map(|n| format!("a.rs\0{n}\0foo\n").into_bytes())
            .collect();
        let (hits, more) = parse_hits(&many, None, false);
        assert_eq!((hits.len(), more), (MAX_HITS, true));
        let exact: Vec<u8> = (1..=MAX_HITS)
            .flat_map(|n| format!("a.rs\0{n}\0foo\n").into_bytes())
            .collect();
        assert!(
            !parse_hits(&exact, None, false).1,
            "exactly the cap is not more"
        );
        assert!(parse_hits(&exact, None, true).1, "a truncated read is more");
    }

    #[test]
    fn shown_text_is_trimmed_cut_char_safely_and_hides_key_bodies() {
        let long = format!("   {}", "é".repeat(MAX_HIT_TEXT_CHARS + 50));
        let shown = shown_text(&long);
        assert_eq!(shown.chars().count(), MAX_HIT_TEXT_CHARS + 3);
        assert!(shown.ends_with("..."));
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj";
        assert_eq!(shown_text(body), "[redacted]");
        assert_eq!(shown_text("let key = foo();"), "let key = foo();");
    }

    /// A.15: the module runs one process, `git grep`, and has no write path. (The needles are
    /// assembled so this test does not match itself.)
    #[test]
    fn the_module_has_no_write_path_and_runs_only_git_grep() {
        let production = include_str!("symbol_refs.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        for parts in [
            ["fs::wri", "te"],
            ["remove", "_"],
            ["re", "name"],
            ["create", "_dir"],
            ["Open", "Options"],
            ["File::cre", "ate"],
            ["set_", "permissions"],
            ["sh", " -c"],
        ] {
            let needle = parts.concat();
            assert!(!production.contains(&needle), "uses {needle}");
        }
        assert_eq!(
            production.matches(&["Command", "::new"].concat()).count(),
            1,
            "exactly one process is started"
        );
        assert!(production.contains("\"grep\""));
        assert!(!production.contains("\"checkout\"") && !production.contains("\"fetch\""));
    }
}
