//! The read jail of the in-process agent tool loop: `Read`, `Glob`, `Grep` and `LS` see only
//! the worktree and the read folders the run declared (sibling repositories of a multi-repo
//! project), never the rest of the machine.
//!
//! # The threat
//!
//! A client engagement's dev run drives this loop over the client's repository. A model that
//! asks for `/Users/auditor/.ssh/id_ed25519`, `../other-client/src`, or a file reached through a
//! symlink the repository itself committed (`vendor -> /`) would otherwise get it, and the bytes
//! would leave the machine in the next request. The jail refuses every such read with a tool
//! error the model sees, and reads nothing outside.
//!
//! # How a path is checked
//!
//! 1. A `..` component anywhere in the requested path is refused outright (no lexical
//!    gymnastics: a read never needs one, since every root is addressable directly).
//! 2. A relative path resolves against the worktree. An absolute path is refused unless it is
//!    lexically under a jail root, before the filesystem is touched (so a refused path never
//!    reveals whether it exists).
//! 3. The path is canonicalized (every symlink resolved) and must still be under a CANONICAL
//!    jail root. This is what stops a symlink inside the worktree from pointing out of it.
//!
//! `Glob` and `Grep` apply the same check to every file they would report: a glob match or a
//! walked file whose canonical path leaves the jail is dropped, and `Grep` never follows a
//! symlinked directory (which also rules out symlink loops).
//!
//! Paths in tool output and in the read log are shown relative to the worktree root. A read
//! folder's paths are shown absolute in tool output (the model needs a path it can pass back)
//! and as `read-folder:<name>/<rel>` in the log.

use std::path::{Component, Path, PathBuf};

use super::{truncate_output, ToolInvocation, MAX_READ_OUTPUT_BYTES};

/// Walk depth limit for `Grep` (directories below this are not searched).
const GREP_MAX_DEPTH: usize = 8;

/// Most `Grep` matches returned before the walk stops.
const GREP_MAX_MATCHES: usize = 2000;

/// What the jail decided about one read-tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadVerdict {
    /// The path was inside the jail and the tool returned its output.
    Allowed,
    /// The path was outside the jail (absolute, `..`, or a symlink out); nothing was read.
    Refused,
    /// The path was inside the jail but the read failed (missing file, bad pattern, I/O error).
    Error,
}

/// The result of one read-tool call: the text handed back to the model, plus what the read log
/// records about it.
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    /// The tool result the model sees.
    pub text: String,
    /// The path the call asked for, relative to the jail when it resolved inside it, else as
    /// the model wrote it.
    pub path: String,
    pub verdict: ReadVerdict,
    /// Bytes of tool output returned to the model for an allowed read; zero otherwise.
    pub bytes: usize,
}

/// One root of the jail: the worktree (always first) or a declared read folder.
struct JailRoot {
    /// As declared (lexically normalized), for the pre-filesystem check of absolute paths.
    declared: PathBuf,
    /// Canonical form, or `None` when the folder does not exist (nothing under it is readable).
    canonical: Option<PathBuf>,
    /// `None` for the worktree; the folder's name for a read folder (used in the log).
    read_folder: Option<String>,
}

/// The set of folders an agent may read.
pub struct ReadJail {
    roots: Vec<JailRoot>,
}

/// A requested path that resolved inside the jail.
struct Resolved {
    /// Canonical absolute path (what is actually read).
    abs: PathBuf,
    /// Index of the root it is under.
    root: usize,
    /// Path relative to that root.
    rel: PathBuf,
}

enum JailError {
    /// Outside the jail: nothing was read.
    Refused(String),
    /// Inside the jail, but the filesystem said no.
    Io(String),
}

impl ReadJail {
    /// The jail for an agent whose worktree is `worktree` and whose run declared `read_dirs`.
    /// With no worktree the jail has no worktree root: every relative path is refused.
    pub fn new(worktree: Option<&Path>, read_dirs: &[PathBuf]) -> Self {
        let mut roots = Vec::new();
        if let Some(wt) = worktree {
            roots.push(JailRoot::new(wt, None));
        }
        for dir in read_dirs {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "read-folder".to_string());
            roots.push(JailRoot::new(dir, Some(name)));
        }
        Self { roots }
    }

    fn worktree(&self) -> Option<&JailRoot> {
        self.roots.first().filter(|r| r.read_folder.is_none())
    }

    /// The index of the root whose canonical form contains `canonical`, and the path under it.
    fn locate(&self, canonical: &Path) -> Option<(usize, PathBuf)> {
        self.roots.iter().enumerate().find_map(|(i, r)| {
            let root = r.canonical.as_ref()?;
            canonical
                .strip_prefix(root)
                .ok()
                .map(|rel| (i, rel.to_path_buf()))
        })
    }

    /// True when `p` (absolute) is lexically under a root, in its declared or canonical form.
    fn lexically_inside(&self, p: &Path) -> bool {
        let p = super::normalize_lexical(p);
        self.roots.iter().any(|r| {
            p.starts_with(&r.declared) || r.canonical.as_ref().is_some_and(|c| p.starts_with(c))
        })
    }

    /// Resolve a requested path to the file or folder it names inside the jail.
    fn resolve(&self, raw: &str) -> Result<Resolved, JailError> {
        let raw = if raw.trim().is_empty() { "." } else { raw };
        let requested = Path::new(raw);
        if has_parent_component(requested) {
            return Err(JailError::Refused(
                "a `..` component may leave the worktree, so it is not allowed".to_string(),
            ));
        }
        let candidate = if requested.is_absolute() {
            if !self.lexically_inside(requested) {
                return Err(JailError::Refused(
                    "the path is outside the worktree and the read folders".to_string(),
                ));
            }
            requested.to_path_buf()
        } else {
            match self.worktree().and_then(|w| w.canonical.as_ref()) {
                Some(root) => root.join(requested),
                None => {
                    return Err(JailError::Refused(
                        "this agent has no worktree to read from".to_string(),
                    ))
                }
            }
        };
        let canonical = std::fs::canonicalize(&candidate).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                JailError::Io(format!("`{raw}` does not exist"))
            } else {
                JailError::Io(format!("`{raw}`: {e}"))
            }
        })?;
        match self.locate(&canonical) {
            Some((root, rel)) => Ok(Resolved {
                abs: canonical,
                root,
                rel,
            }),
            None => Err(JailError::Refused(
                "the path resolves outside the worktree and the read folders (through a \
                 symbolic link)"
                    .to_string(),
            )),
        }
    }

    /// How a path inside the jail is shown to the MODEL: relative to the worktree root, or
    /// absolute for a read folder (so the model can pass it back to another tool).
    fn shown(&self, root: usize, rel: &Path) -> String {
        let r = &self.roots[root];
        if r.read_folder.is_none() {
            let s = rel.display().to_string();
            if s.is_empty() {
                ".".to_string()
            } else {
                s
            }
        } else {
            r.declared.join(rel).display().to_string()
        }
    }

    /// How a path inside the jail is recorded in the READ LOG: relative to the worktree root,
    /// or `read-folder:<name>/<rel>` (no host path).
    fn logged(&self, root: usize, rel: &Path) -> String {
        let r = &self.roots[root];
        let rel_s = rel.display().to_string();
        match &r.read_folder {
            None if rel_s.is_empty() => ".".to_string(),
            None => rel_s,
            Some(name) if rel_s.is_empty() => format!("read-folder:{name}"),
            Some(name) => format!("read-folder:{name}/{rel_s}"),
        }
    }

    /// The model-facing text of a refusal.
    fn refusal(&self, tool: &str, raw: &str, why: &str) -> String {
        let folders: Vec<String> = self
            .roots
            .iter()
            .filter(|r| r.read_folder.is_some())
            .map(|r| r.declared.display().to_string())
            .collect();
        let scope = if folders.is_empty() {
            "the worktree".to_string()
        } else {
            format!("the worktree and the read folders {}", folders.join(", "))
        };
        format!(
            "DENIED: {tool} refused `{raw}`: {why}. Reads are confined to {scope}; use a path \
             relative to the worktree root. Nothing was read."
        )
    }
}

impl JailRoot {
    fn new(path: &Path, read_folder: Option<String>) -> Self {
        Self {
            declared: super::normalize_lexical(path),
            canonical: std::fs::canonicalize(path).ok().filter(|c| c.is_dir()),
            read_folder,
        }
    }
}

fn has_parent_component(p: &Path) -> bool {
    p.components().any(|c| matches!(c, Component::ParentDir))
}

/// The input string of a read tool, under its primary name or an alias.
fn input_str<'a>(inv: &'a ToolInvocation, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| inv.input[*k].as_str())
}

fn allowed(text: String, path: String) -> ReadOutcome {
    let text = truncate_output(text, MAX_READ_OUTPUT_BYTES);
    ReadOutcome {
        bytes: text.len(),
        text,
        path,
        verdict: ReadVerdict::Allowed,
    }
}

fn not_allowed(text: String, path: String, verdict: ReadVerdict) -> ReadOutcome {
    ReadOutcome {
        text,
        path,
        verdict,
        bytes: 0,
    }
}

/// Turn a jail error into the outcome the model and the log see.
fn jail_error(jail: &ReadJail, tool: &str, raw: &str, e: JailError) -> ReadOutcome {
    match e {
        JailError::Refused(why) => not_allowed(
            jail.refusal(tool, raw, &why),
            raw.to_string(),
            ReadVerdict::Refused,
        ),
        JailError::Io(msg) => not_allowed(
            format!("ERROR: {tool}: {msg}"),
            raw.to_string(),
            ReadVerdict::Error,
        ),
    }
}

/// `Read`: the content of one file. Input `{file_path}` (alias `path`).
pub fn execute_read(inv: &ToolInvocation, jail: &ReadJail) -> ReadOutcome {
    let Some(raw) = input_str(inv, &["file_path", "path"]) else {
        return not_allowed(
            "ERROR: Read requires `file_path` in input".to_string(),
            String::new(),
            ReadVerdict::Error,
        );
    };
    let r = match jail.resolve(raw) {
        Ok(r) => r,
        Err(e) => return jail_error(jail, "Read", raw, e),
    };
    let logged = jail.logged(r.root, &r.rel);
    if r.abs.is_dir() {
        return not_allowed(
            format!("ERROR: Read: `{raw}` is a directory; use LS to list it"),
            logged,
            ReadVerdict::Error,
        );
    }
    match std::fs::read_to_string(&r.abs) {
        Ok(content) => allowed(content, logged),
        Err(e) => not_allowed(
            format!("ERROR reading `{raw}`: {e}"),
            logged,
            ReadVerdict::Error,
        ),
    }
}

/// `Glob`: the files matching a pattern. Input `{pattern}` (alias `glob`). A relative pattern
/// is matched under the worktree root; an absolute one only under a jail root.
pub fn execute_glob(inv: &ToolInvocation, jail: &ReadJail) -> ReadOutcome {
    let Some(pattern) = input_str(inv, &["pattern", "glob"]) else {
        return not_allowed(
            "ERROR: Glob requires `pattern` in input".to_string(),
            String::new(),
            ReadVerdict::Error,
        );
    };
    if has_parent_component(Path::new(pattern)) {
        return jail_error(
            jail,
            "Glob",
            pattern,
            JailError::Refused(
                "a `..` component may leave the worktree, so it is not allowed".to_string(),
            ),
        );
    }

    // Split the pattern into its literal leading folder (resolved through the jail) and the
    // wildcard remainder. A relative pattern's literal folder is the worktree root itself.
    let (base_raw, rest) = split_literal_prefix(pattern);
    let base = match jail.resolve(&base_raw) {
        Ok(r) => r,
        Err(e) => return jail_error(jail, "Glob", pattern, e),
    };
    let logged = if Path::new(pattern).is_absolute() {
        jail.logged(base.root, &base.rel.join(&rest))
    } else {
        pattern.to_string()
    };
    let full = if rest.as_os_str().is_empty() {
        glob::Pattern::escape(&base.abs.display().to_string())
    } else {
        format!(
            "{}/{}",
            glob::Pattern::escape(&base.abs.display().to_string()),
            rest.display()
        )
    };

    let paths = match glob::glob(&full) {
        Ok(paths) => paths,
        Err(e) => {
            return not_allowed(
                format!("ERROR: Glob pattern error `{pattern}`: {e}"),
                logged,
                ReadVerdict::Error,
            )
        }
    };
    let mut entries: Vec<String> = Vec::new();
    for p in paths.filter_map(Result::ok) {
        // Every match is re-checked: a match reached through a symlinked folder may lie
        // outside the jail even though the pattern did not.
        let Ok(canonical) = std::fs::canonicalize(&p) else {
            continue;
        };
        if jail.locate(&canonical).is_none() {
            continue;
        }
        // Show the match by where it was found (its path under the base), not where a symlink
        // inside the jail points.
        let rel = p
            .strip_prefix(&base.abs)
            .map(|r| base.rel.join(r))
            .unwrap_or_else(|_| base.rel.clone());
        entries.push(jail.shown(base.root, &rel));
    }
    if entries.is_empty() {
        return allowed(format!("No files matched pattern `{pattern}`"), logged);
    }
    allowed(entries.join("\n"), logged)
}

/// The leading components of `pattern` that hold no wildcard, as a path string (`.` when the
/// first component already has one), and the rest.
fn split_literal_prefix(pattern: &str) -> (String, PathBuf) {
    let p = Path::new(pattern);
    let mut literal = PathBuf::new();
    let mut rest = PathBuf::new();
    let mut in_rest = false;
    for comp in p.components() {
        let s = comp.as_os_str().to_string_lossy();
        if !in_rest && !s.contains(['*', '?', '[', ']', '{', '}']) {
            literal.push(comp.as_os_str());
        } else {
            in_rest = true;
            rest.push(comp.as_os_str());
        }
    }
    // A fully literal pattern names one path: match it as the (escaped) base itself.
    let literal = literal.display().to_string();
    (
        if literal.is_empty() {
            ".".to_string()
        } else {
            literal
        },
        rest,
    )
}

/// `Grep`: lines matching a regex. Input `{pattern, path}` (aliases `regex`, `directory`); the
/// path defaults to the worktree root and may name a folder or one file.
pub fn execute_grep(inv: &ToolInvocation, jail: &ReadJail) -> ReadOutcome {
    let Some(search_pattern) = input_str(inv, &["pattern", "regex"]) else {
        return not_allowed(
            "ERROR: Grep requires `pattern` in input".to_string(),
            String::new(),
            ReadVerdict::Error,
        );
    };
    let search_dir = input_str(inv, &["path", "directory"]).unwrap_or(".");
    let r = match jail.resolve(search_dir) {
        Ok(r) => r,
        Err(e) => return jail_error(jail, "Grep", search_dir, e),
    };
    let logged = jail.logged(r.root, &r.rel);
    let re = match regex::Regex::new(search_pattern) {
        Ok(re) => re,
        Err(e) => {
            return not_allowed(
                format!("ERROR: invalid grep pattern `{search_pattern}`: {e}"),
                logged,
                ReadVerdict::Error,
            )
        }
    };

    let mut findings: Vec<String> = Vec::new();
    if r.abs.is_file() {
        grep_file(&r.abs, &jail.shown(r.root, &r.rel), &re, &mut findings);
    } else {
        grep_dir(jail, &r, &r.abs, &re, &mut findings, 0);
    }
    if findings.is_empty() {
        return allowed(
            format!("No matches for `{search_pattern}` in `{search_dir}`"),
            logged,
        );
    }
    allowed(findings.join("\n"), logged)
}

/// Recursive walk under `dir` (a real folder inside the jail). Symlinked folders are never
/// followed; a symlinked file is searched only when its target is inside the jail.
fn grep_dir(
    jail: &ReadJail,
    base: &Resolved,
    dir: &Path,
    re: &regex::Regex,
    out: &mut Vec<String>,
    depth: usize,
) {
    if depth > GREP_MAX_DEPTH || out.len() > GREP_MAX_MATCHES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue; // hidden entries (e.g. .git)
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let rel = path
            .strip_prefix(&base.abs)
            .map(|r| base.rel.join(r))
            .unwrap_or_else(|_| base.rel.clone());
        let shown = jail.shown(base.root, &rel);
        if kind.is_symlink() {
            let inside_file = std::fs::canonicalize(&path)
                .ok()
                .filter(|c| c.is_file() && jail.locate(c).is_some());
            if let Some(target) = inside_file {
                grep_file(&target, &shown, re, out);
            }
        } else if kind.is_dir() {
            grep_dir(jail, base, &path, re, out, depth + 1);
        } else if kind.is_file() {
            grep_file(&path, &shown, re, out);
        }
        if out.len() > GREP_MAX_MATCHES {
            out.push(format!("... (truncated at {GREP_MAX_MATCHES} matches)"));
            return;
        }
    }
}

fn grep_file(path: &Path, shown: &str, re: &regex::Regex, out: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (lineno, line) in text.lines().enumerate() {
        if re.is_match(line) {
            out.push(format!("{shown}:{}:{line}", lineno + 1));
            if out.len() > GREP_MAX_MATCHES {
                return;
            }
        }
    }
}

/// `LS`: the entries of one folder. Input `{path}` (alias `directory`), default the worktree
/// root. Folders end in `/`, symlinks in `@`.
pub fn execute_ls(inv: &ToolInvocation, jail: &ReadJail) -> ReadOutcome {
    let raw = input_str(inv, &["path", "directory"]).unwrap_or(".");
    let r = match jail.resolve(raw) {
        Ok(r) => r,
        Err(e) => return jail_error(jail, "LS", raw, e),
    };
    let logged = jail.logged(r.root, &r.rel);
    match std::fs::read_dir(&r.abs) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    match e.file_type() {
                        Ok(t) if t.is_symlink() => format!("{name}@"),
                        Ok(t) if t.is_dir() => format!("{name}/"),
                        _ => name,
                    }
                })
                .collect();
            names.sort();
            allowed(names.join("\n"), logged)
        }
        Err(e) => not_allowed(
            format!("ERROR listing `{raw}`: {e}"),
            logged,
            ReadVerdict::Error,
        ),
    }
}
