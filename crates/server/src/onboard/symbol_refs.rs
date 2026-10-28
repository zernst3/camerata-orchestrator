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
//! | `commit_unavailable` | The recorded commit is not in this clone and the working tree WAS searched in its place (`source: "working_tree"`) | yes |
//! | `source_deleted` | The client source was deleted on purpose; nothing is searched | no |
//! | `file_missing` | No project owns that repo, or it has no folder | no |
//! | `unavailable` | The search did not run (git would not start, not a repository, an error exit) or every read slot stayed busy; no `source` | no, `hits: []` |
//! | `too_large` | The search took too long and was stopped | no, `hits: []` |
//!
//! Every answer carries `symbol`, `hits` and `more`. `unavailable` and `too_large` are failures,
//! never an empty list: a reader must not show "no references" for a search that did not run.
//! `commit_unavailable` is only the fallback that happened.
//!
//! An invalid symbol is a `400` `{ok: false, code: "invalid_symbol"}` before anything is read.
//!
//! # The symbol is data, never an option, never a pattern
//!
//! The symbol must be 1 to [`MAX_SYMBOL_BYTES`] bytes of letters, digits and `_ $ - . :` with at
//! least one letter, digit or `_` ([`is_valid_symbol`], shared with the UI through
//! `camerata-api-types`), so no space, quote, bracket, glob or regex operator can reach git. It
//! is passed after `-e` (a leading `-` is a pattern character, not an option) with
//! `--fixed-strings` (a `.` is a dot) and `-w` (whole words), and no shell is involved.
//!
//! # Bounded
//!
//! At most a few searches run at once ([`crate::workspace::GIT_READS`]; one more waits briefly,
//! then is told it is busy). The subprocess starts through [`crate::workspace::git_command`] (the
//! repository-redirecting and `GIT_CONFIG*` environment removed, `GIT_OPTIONAL_LOCKS=0`), is read
//! up to [`SearchLimits::max_output`] and killed beyond it (only whole records are kept: the cut
//! can fall inside a line, and half a line is never redacted and shown), runs under
//! [`SearchLimits::timeout`] with `kill_on_drop`, and at most [`MAX_HITS`] hits are returned with
//! `more: true` when there were others. Hit lines are redacted, then cut at
//! [`MAX_HIT_TEXT_CHARS`] characters. Read-only: `git grep` writes nothing and it is the only
//! subcommand run here.
//!
//! # What this endpoint reveals
//!
//! It is an existence oracle: `path:line` is returned even for a hit whose text is shown as
//! `[redacted]`, so a caller can learn that a symbol occurs on a line the redactor hides. That is
//! accepted for a search surface (the same caller can already read the file's redacted text and
//! ask which lines mention a name); the secret text itself never leaves.

use std::path::Path;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::files::is_noise_path;
use super::finding_context::{redact_block, FindingContextOutcome};
use crate::client_source::read::{labelled, no_store, source_for_read, SourceRead};
use crate::client_source::Refusal;
use crate::workspace::{git_command, GitEnv, GitReadGate, GIT_READS};
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

/// What one `git grep` produced: hits (capped) and whether there were more, or the shared
/// outcome for why nothing was searched ([`FindingContextOutcome::SearchFailed`]: it did not
/// start, not a repository, an error exit; [`SearchTooSlow`](FindingContextOutcome::SearchTooSlow);
/// [`Busy`](FindingContextOutcome::Busy): no slot for another read of client code freed in time).
type Searched = Result<(Vec<Hit>, bool), FindingContextOutcome>;

/// The limits one search runs under. Tests shrink them to reach the timeout and the output cap
/// without a huge repository.
#[derive(Debug, Clone, Copy)]
pub struct SearchLimits {
    /// How long the search may run.
    pub timeout: Duration,
    /// Bytes of `git grep` output read before the process is killed.
    pub max_output: usize,
    /// How many searches may run at once, and how long one waits for a slot.
    pub gate: &'static GitReadGate,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            timeout: SEARCH_TIMEOUT,
            max_output: MAX_OUTPUT_BYTES,
            gate: &GIT_READS,
        }
    }
}

/// Where one search runs.
#[derive(Clone, Copy)]
enum Where<'a> {
    WorkingTree(&'a Path),
    Git { dir: &'a Path, sha: &'a str },
}

/// Redact one hit line, trim it and cut it for display. A line that is key body (a base64 run,
/// plain or behind list decoration) is never shown: the per-line redactor cannot see the
/// `-----BEGIN` line that makes it a secret ([`camerata_secrets::is_key_body_line`]).
fn shown_text(raw: &str) -> String {
    if camerata_secrets::is_key_body_line(raw) {
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
/// noise paths (`extra_dirs` are the operator's `CAMERATA_SCAN_EXCLUDE_DIRS`, as in the scan),
/// redacting the text, stopping after [`MAX_HITS`] (the next one sets `more`).
fn parse_hits(
    output: &[u8],
    sha: Option<&str>,
    truncated: bool,
    extra_dirs: &[String],
) -> (Vec<Hit>, bool) {
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
        if path.is_empty() || is_noise_path(path, extra_dirs) {
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

/// `output` cut back to its last complete record: everything after the final line feed is a
/// record the cap split in two, and half a line (possibly half a secret) is never parsed.
fn whole_records(output: &[u8]) -> &[u8] {
    match output.iter().rposition(|b| *b == b'\n') {
        Some(last) => &output[..=last],
        None => &[],
    }
}

/// Run the one read-only `git grep` and read its output, bounded.
async fn git_grep(at: Where<'_>, symbol: &str, limits: SearchLimits) -> Searched {
    use tokio::io::AsyncReadExt;

    let (dir, tree) = match at {
        Where::WorkingTree(dir) => (dir, None),
        Where::Git { dir, sha } => (dir, Some(sha)),
    };
    // One of a bounded number of reads of client code at once; held until the process is gone.
    let Some(_slot) = limits.gate.enter().await else {
        return Err(FindingContextOutcome::Busy);
    };
    #[cfg(test)]
    GREP_RUNS.with(|c| c.set(c.get() + 1));
    let mut command = git_command(Some(dir), GitEnv::Isolated);
    command
        // Never fall back to searching a plain folder, never recurse into submodules, and
        // never print a column number, whatever the user's git configuration says.
        .args([
            "-c",
            "grep.fallbackToNoIndex=false",
            "-c",
            "grep.column=false",
        ])
        .args([
            "grep",
            "-z",
            "-n",
            "-w",
            "-I",
            "-F",
            "--no-color",
            "--no-column",
        ])
        .arg("--no-recurse-submodules")
        // The symbol is the pattern: after `-e`, a leading dash is part of it.
        .arg("-e")
        .arg(symbol);
    if let Some(sha) = tree {
        command.arg(sha);
    }
    command
        .arg("--")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return Err(FindingContextOutcome::SearchFailed);
    };
    let Some(stdout) = child.stdout.take() else {
        return Err(FindingContextOutcome::SearchFailed);
    };

    let extra_dirs = super::files::extra_exclude_dirs();
    let run = async {
        let mut bytes = Vec::new();
        let cap = limits.max_output as u64 + 1;
        if stdout.take(cap).read_to_end(&mut bytes).await.is_err() {
            return Err(FindingContextOutcome::SearchFailed);
        }
        let truncated = bytes.len() > limits.max_output;
        if truncated {
            // Past the cap: the rest is never read, and the process is killed below. The cut
            // can fall inside a record, so only whole records are kept.
            bytes.truncate(limits.max_output);
            let _ = child.kill().await;
            let kept = whole_records(&bytes).to_vec();
            return Ok(parse_in_pool(kept, tree, true, extra_dirs).await);
        }
        match child.wait().await {
            // 0: matches, 1: none.
            Ok(status) if matches!(status.code(), Some(0 | 1)) => {
                Ok(parse_in_pool(bytes, tree, false, extra_dirs).await)
            }
            _ => Err(FindingContextOutcome::SearchFailed),
        }
    };
    tokio::time::timeout(limits.timeout, run)
        .await
        .unwrap_or(Err(FindingContextOutcome::SearchTooSlow))
}

/// [`parse_hits`] on the blocking pool (it redacts up to [`MAX_HITS`] lines).
async fn parse_in_pool(
    bytes: Vec<u8>,
    sha: Option<&str>,
    truncated: bool,
    extra_dirs: Vec<String>,
) -> (Vec<Hit>, bool) {
    let sha = sha.map(str::to_string);
    tokio::task::spawn_blocking(move || parse_hits(&bytes, sha.as_deref(), truncated, &extra_dirs))
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

/// An answer that searched nothing: `outcome` in the shared vocabulary, with the same `symbol`,
/// empty `hits` and `more: false` every answer here carries so a reader never has to special-case
/// a missing field. `source` labels it when the outcome belongs to a read from one.
fn nothing_found(outcome: &FindingContextOutcome, source: Option<&str>, symbol: &str) -> Response {
    let mut json = match source {
        Some(source) => labelled(outcome, source, None),
        None => outcome.to_json(),
    };
    if let Some(map) = json.as_object_mut() {
        map.insert("symbol".to_string(), serde_json::json!(symbol));
        map.insert("hits".to_string(), serde_json::json!([]));
        map.insert("more".to_string(), serde_json::json!(false));
    }
    respond(json)
}

/// `GET /api/onboard/symbol-refs`: where a symbol is used, searched at the inspected commit (or
/// labelled as the working tree), noise paths dropped, every line redacted, at most
/// [`MAX_HITS`]. See the module docs. Always `Cache-Control: no-store`.
///
/// Invariant: a deleted source runs no process and reads nothing.
pub async fn symbol_refs(state: &AppState, params: SymbolRefsParams) -> Response {
    symbol_refs_with(state, params, SearchLimits::default()).await
}

/// [`symbol_refs`] with explicit limits (tests shrink them).
pub async fn symbol_refs_with(
    state: &AppState,
    params: SymbolRefsParams,
    limits: SearchLimits,
) -> Response {
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
        SourceRead::WorkingTree { dir } => {
            let (hits, more) = match git_grep(Where::WorkingTree(&dir), symbol, limits).await {
                Ok(found) => found,
                Err(outcome) => return nothing_found(&outcome, None, symbol),
            };
            respond(Body {
                status: "ok",
                source: "working_tree",
                commit: None,
                head_differs: None,
                reason: None,
                symbol,
                hits: &hits,
                more,
            })
        }
        SourceRead::Git {
            dir,
            sha,
            head_differs,
        } => {
            // Same test `read_git_blob_at` makes before it reads.
            if !crate::client_source::git::commit_exists(&dir, &sha).await {
                // The one labelled fallback, and only while the folder exists (the resolver
                // checked) and the search ran: the working tree, loudly. The status stays
                // `commit_unavailable`. A search that did not run is `unavailable`, not this.
                let (hits, more) = match git_grep(Where::WorkingTree(&dir), symbol, limits).await {
                    Ok(found) => found,
                    Err(outcome) => return nothing_found(&outcome, None, symbol),
                };
                let outcome = FindingContextOutcome::CommitUnavailable;
                return respond(Body {
                    status: "commit_unavailable",
                    source: "working_tree",
                    commit: None,
                    head_differs: None,
                    reason: outcome.to_json()["reason"].as_str().map(str::to_string),
                    symbol,
                    hits: &hits,
                    more,
                });
            }
            let at = Where::Git {
                dir: &dir,
                sha: &sha,
            };
            let (hits, more) = match git_grep(at, symbol, limits).await {
                Ok(found) => found,
                Err(outcome) => return nothing_found(&outcome, None, symbol),
            };
            respond(Body {
                status: "ok",
                source: "git",
                commit: Some(&sha),
                head_differs: Some(head_differs),
                reason: None,
                symbol,
                hits: &hits,
                more,
            })
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
        let (hits, more) = parse_hits(&out, Some("S"), false, &[]);
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
        let (hits, more) = parse_hits(&many, None, false, &[]);
        assert_eq!((hits.len(), more), (MAX_HITS, true));
        let exact: Vec<u8> = (1..=MAX_HITS)
            .flat_map(|n| format!("a.rs\0{n}\0foo\n").into_bytes())
            .collect();
        assert!(
            !parse_hits(&exact, None, false, &[]).1,
            "exactly the cap is not more"
        );
        assert!(
            parse_hits(&exact, None, true, &[]).1,
            "a truncated read is more"
        );
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
        // Decorated body lines (a YAML list item, a JSON array element) and a short padded
        // final line are key body too.
        for decorated in [
            format!("  - \"{body}\""),
            format!("\"{body}\","),
            "      - kQ==".to_string(),
        ] {
            assert_eq!(shown_text(&decorated), "[redacted]", "{decorated}");
        }
    }

    #[test]
    fn the_operators_extra_excluded_dirs_drop_hits_as_the_scan_does() {
        let out = b"a/vendored_by_me/x.rs\x001\x00foo\nsrc/y.rs\x002\x00foo\n";
        let (hits, _) = parse_hits(out, None, false, &["vendored_by_me".to_string()]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/y.rs");
        let (hits, _) = parse_hits(out, None, false, &[]);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn a_cut_record_is_dropped_whole_never_parsed_in_part() {
        assert_eq!(
            whole_records(b"a\0\x01\0foo\nb\0\x02\0ba"),
            b"a\0\x01\0foo\n"
        );
        assert_eq!(whole_records(b"a\0\x01\0foo\n"), b"a\0\x01\0foo\n");
        assert_eq!(whole_records(b"no newline at all"), b"");
        assert_eq!(whole_records(b""), b"");
    }

    #[test]
    fn nothing_searched_is_never_an_empty_list() {
        for (outcome, status) in [
            (FindingContextOutcome::SearchFailed, "unavailable"),
            (FindingContextOutcome::SearchTooSlow, "too_large"),
            (FindingContextOutcome::Busy, "unavailable"),
        ] {
            let json = outcome.to_json();
            assert_eq!(json["status"], status);
            assert!(json["reason"].as_str().is_some_and(|r| !r.is_empty()));
            assert!(json.get("source").is_none());
        }
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
        // No process is built here: the one shared builder starts it.
        assert!(!production.contains(&["Command", "::new"].concat()));
        assert_eq!(production.matches("git_command(").count(), 1);
        assert!(production.contains("\"grep\""));
        assert!(!production.contains("\"checkout\"") && !production.contains("\"fetch\""));
    }

    /// Every module that reads client code starts git through `workspace::git_command` (which
    /// removes the repository-redirecting variables), none builds the command itself.
    #[test]
    fn git_is_started_in_one_place_for_every_code_read() {
        let production =
            |text: &'static str| text.split("#[cfg(test)]\nmod tests").next().unwrap_or(text);
        let spawn = ["Command", "::new(\"git\")"].concat();
        for (name, text) in [
            ("symbol_refs.rs", include_str!("symbol_refs.rs")),
            ("finding_context.rs", include_str!("finding_context.rs")),
            ("source_file.rs", include_str!("source_file.rs")),
        ] {
            assert!(
                !production(text).contains(&spawn),
                "{name} builds its own git command"
            );
        }
        assert_eq!(
            production(include_str!("../workspace.rs"))
                .matches(&spawn)
                .count(),
            1,
            "workspace.rs builds git in exactly one place"
        );
    }
}
