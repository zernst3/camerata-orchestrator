//! The review room: the small in-memory file tree one agent works in (design section 1). It is
//! built from already redacted files and serves five tools (Read, Glob, Grep, LS, Write). A path
//! that is not in the room does not exist; a write is accepted only under `_camerata/out/`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Mutex;

use super::Piece;

/// Where the agent writes its output files.
pub const OUT_DIR: &str = "_camerata/out/";
/// The most lines one Read returns.
pub const MAX_READ_LINES: usize = 2_000;
/// The share of the piece's bytes that context files may take, in percent.
pub const CONTEXT_PERCENT: usize = 30;
/// The most hits one Grep returns.
pub const MAX_GREP_HITS: usize = 200;
/// The most bytes one written file may hold.
pub const MAX_WRITE_BYTES: usize = 1_000_000;
/// The most bytes of text one Read returns (the numbered lines together). A read that would pass
/// it stops at the last whole line that fits.
pub const MAX_READ_BYTES: usize = 100_000;
/// The most distinct files the agent may write. Writing a path again replaces it and does not
/// count twice.
pub const MAX_WRITE_FILES: usize = 200;
/// The folder the room keeps for its own files. A repository file under this name (in any case) cannot be
/// served (see [`NotServed`]), so a client file can never shadow or replace a room file.
pub const RESERVED_DIR: &str = "_camerata";

/// The folder, below the root of a room on disk, that holds the repository's files (see
/// [`Room::materialize_under_code`]).
pub const CODE_DIR: &str = "code";
/// The longest line, in characters, the Read tool returns whole. Claude Code cuts a longer line.
pub const MAX_READ_LINE_CHARS: usize = 2_000;

/// Why a tool call was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    #[error("no such file: {0}")]
    NotFound(String),
    #[error("writes are accepted only under {OUT_DIR}: {0}")]
    OutsideOut(String),
    #[error("{0}")]
    Invalid(String),
}

/// What one Read returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutput {
    /// The returned lines, numbered, one per line.
    pub text: String,
    /// The first line returned (1-based).
    pub first_line: usize,
    /// How many lines came back (at most [`MAX_READ_LINES`]).
    pub lines_returned: usize,
    /// How many lines the file has.
    pub total_lines: usize,
}

/// One Grep hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// A piece file the room could not serve, and why. The file stays owed in coverage and is reported
/// as not reviewed with this reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotServed {
    pub path: String,
    pub reason: String,
}

/// What [`Room::build`] returns: the room and the piece files it could not serve.
#[derive(Debug)]
pub struct BuiltRoom {
    pub room: Room,
    /// Piece files missing from the repository set, or named like the room's own files. Never
    /// dropped quietly.
    pub not_served: Vec<NotServed>,
}

/// The room. Methods take `&self`; the output files sit behind a lock so a transport can share
/// the room across turns.
#[derive(Debug)]
pub struct Room {
    files: BTreeMap<String, String>,
    piece_files: Vec<String>,
    context_files: Vec<String>,
    /// Read-only files added for a call that needs more than the piece (the joints call).
    extra_files: Vec<String>,
    out: Mutex<BTreeMap<String, String>>,
}

/// The tree file: every path of the repository, one a line.
const TREE_PATH: &str = "_camerata/context/tree.txt";
const NEIGHBOURS_PATH: &str = "_camerata/context/neighbours.md";
const CONTEXT_LIST_PATH: &str = "_camerata/context/context-files.txt";
const CHECKLIST_PATH: &str = "_camerata/checklist.md";
/// The most outline lines the neighbours file holds on each side.
const MAX_NEIGHBOUR_LINES: usize = 400;

/// A path as the room names it: no leading `./`, no empty or `.` parts. `None` for an absolute
/// path, a path with `..`, a backslash or a NUL: those name nothing in the room.
fn normalize(path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// How many leading folders `a` and `b` share.
fn shared_folders(a: &str, b: &str) -> usize {
    let a: Vec<&str> = a.split('/').collect();
    let b: Vec<&str> = b.split('/').collect();
    let (a, b) = (&a[..a.len() - 1], &b[..b.len() - 1]);
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

impl Room {
    /// Build the room for `piece` from `repo_files` (the whole repository, already redacted) and
    /// the rendered checklist text. The files are redacted once more here, so a room built from
    /// raw text still serves no credential value.
    pub fn build(piece: &Piece, repo_files: &[(String, String)], checklist_md: &str) -> BuiltRoom {
        // Compared without regard to case: on a case-insensitive disk a client folder `_Camerata/`
        // is the room's own `_camerata/` folder.
        let reserved = |p: &str| {
            p.eq_ignore_ascii_case(RESERVED_DIR)
                || (p.len() > RESERVED_DIR.len()
                    && p.is_char_boundary(RESERVED_DIR.len())
                    && p[..RESERVED_DIR.len()].eq_ignore_ascii_case(RESERVED_DIR)
                    && p.as_bytes()[RESERVED_DIR.len()] == b'/')
        };
        // A repository file named like the room's own files is left out of the room entirely: it
        // is neither served nor listed, so it cannot shadow a room file. A piece file among them
        // stays owed and is reported below.
        let repo: Vec<(String, String)> = crate::onboard::redact::files_for_model(repo_files)
            .into_iter()
            .filter(|(p, _)| !reserved(p))
            .collect();
        let by_path: HashMap<String, usize> = repo
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (p.clone(), i))
            .collect();
        let (imports, importers) = super::super::repo_map::import_graph(&repo, &by_path);

        let mut not_served: Vec<NotServed> = Vec::new();
        let mut piece_idx: Vec<usize> = Vec::new();
        for path in &piece.files {
            if let Some(&i) = by_path.get(path) {
                piece_idx.push(i);
            } else if reserved(path) {
                not_served.push(NotServed {
                    path: path.clone(),
                    reason: format!(
                        "its path is under {RESERVED_DIR}/, which the review room keeps for its own files"
                    ),
                });
            } else {
                not_served.push(NotServed {
                    path: path.clone(),
                    reason: "the file is not in the repository set the room was built from"
                        .to_string(),
                });
            }
        }
        piece_idx.sort_unstable();
        piece_idx.dedup();
        let in_piece: BTreeSet<usize> = piece_idx.iter().copied().collect();
        let piece_bytes: usize = piece_idx.iter().map(|&i| repo[i].1.len()).sum();

        // Context files: what the piece imports, nearest folder first, within the budget.
        let mut candidates: BTreeSet<usize> = BTreeSet::new();
        for &i in &piece_idx {
            candidates.extend(imports[i].iter().copied().filter(|j| !in_piece.contains(j)));
        }
        let mut ranked: Vec<usize> = candidates.iter().copied().collect();
        ranked.sort_by_key(|&j| {
            let nearest = piece_idx
                .iter()
                .map(|&i| shared_folders(&repo[i].0, &repo[j].0))
                .max()
                .unwrap_or(0);
            (std::cmp::Reverse(nearest), repo[j].0.clone())
        });
        let budget = piece_bytes * CONTEXT_PERCENT / 100;
        let mut used = 0usize;
        let mut context_idx: Vec<usize> = Vec::new();
        for j in ranked {
            let len = repo[j].1.len();
            if used + len <= budget {
                used += len;
                context_idx.push(j);
            }
        }
        context_idx.sort_by(|&a, &b| repo[a].0.cmp(&repo[b].0));

        // The neighbours: outlines of what the piece imports and what imports the piece.
        let mut importing: BTreeSet<usize> = BTreeSet::new();
        for &i in &piece_idx {
            importing.extend(
                importers[i]
                    .iter()
                    .copied()
                    .filter(|j| !in_piece.contains(j)),
            );
        }
        let outline = |set: &BTreeSet<usize>| -> String {
            let mut text = String::new();
            let mut names: Vec<&usize> = set.iter().collect();
            names.sort_by(|a, b| repo[**a].0.cmp(&repo[**b].0));
            for &&j in names.iter().take(MAX_NEIGHBOUR_LINES) {
                text.push_str(&super::super::repo_map_line(&repo[j].0, &repo[j].1));
            }
            if names.len() > MAX_NEIGHBOUR_LINES {
                text.push_str(&format!(
                    "  ({} more not shown)\n",
                    names.len() - MAX_NEIGHBOUR_LINES
                ));
            }
            if text.is_empty() {
                text.push_str("  none\n");
            }
            text
        };
        let neighbours = format!(
            "# Neighbours of this part\n\nFiles this part imports (outline of their public names):\n{}\nFiles that import this part:\n{}",
            outline(&candidates),
            outline(&importing)
        );

        let mut tree_paths: Vec<&str> = repo.iter().map(|(p, _)| p.as_str()).collect();
        tree_paths.sort_unstable();
        let tree: String = tree_paths.iter().map(|p| format!("{p}\n")).collect();

        let context_list = if context_idx.is_empty() {
            "none\n".to_string()
        } else {
            context_idx
                .iter()
                .map(|&j| format!("{}\n", repo[j].0))
                .collect()
        };

        let mut files: BTreeMap<String, String> = BTreeMap::new();
        for &i in piece_idx.iter().chain(&context_idx) {
            files.insert(repo[i].0.clone(), repo[i].1.clone());
        }
        files.insert(TREE_PATH.to_string(), tree);
        files.insert(NEIGHBOURS_PATH.to_string(), neighbours);
        files.insert(CONTEXT_LIST_PATH.to_string(), context_list);
        files.insert(CHECKLIST_PATH.to_string(), checklist_md.to_string());
        not_served.sort_by(|a, b| a.path.cmp(&b.path));
        not_served.dedup_by(|a, b| a.path == b.path);
        BuiltRoom {
            room: Room {
                files,
                piece_files: piece_idx.iter().map(|&i| repo[i].0.clone()).collect(),
                context_files: context_idx.iter().map(|&j| repo[j].0.clone()).collect(),
                extra_files: Vec::new(),
                out: Mutex::new(BTreeMap::new()),
            },
            not_served,
        }
    }

    /// Add read-only files that are not piece files (the cards, the leads and the problem list
    /// of the joints call). Each path must be under `_camerata/`, outside the output folder, and
    /// must not replace a file the room already has. They are never owed in coverage.
    pub fn with_extra_files(mut self, extras: &[(String, String)]) -> Result<Room, RoomError> {
        for (path, text) in extras {
            let normal = normalize(path)
                .filter(|p| p.starts_with(&format!("{RESERVED_DIR}/")))
                .filter(|p| !p.starts_with(OUT_DIR))
                .ok_or_else(|| {
                    RoomError::Invalid(format!(
                        "an extra file must be under {RESERVED_DIR}/ and outside {OUT_DIR}: {path}"
                    ))
                })?;
            if self.files.contains_key(&normal) {
                return Err(RoomError::Invalid(format!(
                    "an extra file would replace a file of the room: {normal}"
                )));
            }
            self.files.insert(normal.clone(), text.clone());
            self.extra_files.push(normal);
        }
        self.extra_files.sort();
        Ok(self)
    }

    /// The extra read-only files, sorted.
    pub fn extra_files(&self) -> &[String] {
        &self.extra_files
    }

    /// The piece files the room serves with their line counts, in path order. Every caller counts
    /// lines through this one rule (the lines the Read tool numbers).
    pub fn piece_line_counts(&self) -> Vec<(String, usize)> {
        self.piece_files
            .iter()
            .map(|p| {
                let lines = self.files.get(p).map_or(0, |t| t.lines().count());
                (p.clone(), lines)
            })
            .collect()
    }

    /// The whole text of a piece file, numbered like a Read, with no line or byte limit. `None`
    /// for any file that is not a piece file. For the message that sends a file again.
    pub fn piece_file_text(&self, path: &str) -> Option<String> {
        let path = normalize(path)?;
        if !self.piece_files.contains(&path) {
            return None;
        }
        let text = self.files.get(&path)?;
        Some(
            text.lines()
                .enumerate()
                .map(|(i, line)| format!("{:>6}\t{line}\n", i + 1))
                .collect(),
        )
    }

    /// The same room with only `paths` of the piece's files (for the send-again step). Context,
    /// tree, neighbours and checklist stay.
    pub fn only(&self, paths: &[String]) -> Room {
        let keep: BTreeSet<String> = paths.iter().filter_map(|p| normalize(p)).collect();
        let piece_files: Vec<String> = self
            .piece_files
            .iter()
            .filter(|p| keep.contains(*p))
            .cloned()
            .collect();
        let mut files = self.files.clone();
        for dropped in self.piece_files.iter().filter(|p| !keep.contains(*p)) {
            files.remove(dropped);
        }
        Room {
            files,
            piece_files,
            context_files: self.context_files.clone(),
            extra_files: self.extra_files.clone(),
            out: Mutex::new(BTreeMap::new()),
        }
    }

    /// The piece's own files: the ones the agent owes a full read.
    pub fn piece_files(&self) -> &[String] {
        &self.piece_files
    }

    /// The context files (outside the piece, shown in full).
    pub fn context_files(&self) -> &[String] {
        &self.context_files
    }

    fn out_lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The text of a served file, if the room has it.
    fn text_of(&self, path: &str) -> Option<String> {
        let path = normalize(path)?;
        if let Some(t) = self.files.get(&path) {
            return Some(t.clone());
        }
        self.out_lock().get(&path).cloned()
    }

    /// Every path the room serves, sorted (the output files included).
    pub fn paths(&self) -> Vec<String> {
        let mut all: BTreeSet<String> = self.files.keys().cloned().collect();
        all.extend(self.out_lock().keys().cloned());
        all.into_iter().collect()
    }

    /// Whether `path` names a file of the room.
    pub fn contains(&self, path: &str) -> bool {
        self.text_of(path).is_some()
    }

    /// Read `count` lines from `first_line` (1-based). At most [`MAX_READ_LINES`] come back.
    pub fn read(
        &self,
        path: &str,
        first_line: usize,
        count: usize,
    ) -> Result<ReadOutput, RoomError> {
        let text = self
            .text_of(path)
            .ok_or_else(|| RoomError::NotFound(path.to_string()))?;
        let first_line = first_line.max(1);
        let total_lines = text.lines().count();
        let take = count.min(MAX_READ_LINES);
        let mut out = String::new();
        let mut returned = 0usize;
        let mut cut_by_bytes = false;
        for (i, line) in text.lines().enumerate().skip(first_line - 1).take(take) {
            let numbered = format!("{:>6}\t{line}\n", i + 1);
            if out.len() + numbered.len() > MAX_READ_BYTES {
                cut_by_bytes = true;
                break;
            }
            out.push_str(&numbered);
            returned += 1;
        }
        if cut_by_bytes {
            // The lines not returned are not read: the trace counts only `returned`.
            out.push_str(&format!(
                "(output stopped at the {MAX_READ_BYTES} byte limit; read again from line {} for the rest)\n",
                first_line + returned
            ));
        }
        Ok(ReadOutput {
            text: out,
            first_line,
            lines_returned: returned,
            total_lines,
        })
    }

    fn glob_options() -> glob::MatchOptions {
        glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        }
    }

    /// The paths matching a glob pattern, sorted. A pattern with no `/` also matches a file name
    /// in any folder.
    pub fn glob(&self, pattern: &str) -> Vec<String> {
        let Some(pattern) = normalize(pattern) else {
            return Vec::new();
        };
        let Ok(pat) = glob::Pattern::new(&pattern) else {
            return Vec::new();
        };
        let opts = Self::glob_options();
        let bare = !pattern.contains('/');
        self.paths()
            .into_iter()
            .filter(|p| {
                pat.matches_with(p, opts)
                    || (bare && pat.matches_with(p.rsplit('/').next().unwrap_or(p), opts))
            })
            .collect()
    }

    /// Search the room's files line by line for a regular expression. `glob` limits the files.
    pub fn grep(&self, pattern: &str, glob: Option<&str>) -> Result<Vec<GrepHit>, RoomError> {
        let re = regex::Regex::new(pattern)
            .map_err(|e| RoomError::Invalid(format!("not a valid pattern: {e}")))?;
        let paths = match glob {
            Some(g) => self.glob(g),
            None => self.paths(),
        };
        let mut hits = Vec::new();
        'files: for path in paths {
            let Some(text) = self.text_of(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    hits.push(GrepHit {
                        path: path.clone(),
                        line: i + 1,
                        text: line.to_string(),
                    });
                    if hits.len() >= MAX_GREP_HITS {
                        break 'files;
                    }
                }
            }
        }
        Ok(hits)
    }

    /// The immediate children of a directory (a folder has a trailing slash).
    pub fn ls(&self, dir: &str) -> Result<Vec<String>, RoomError> {
        let dir = normalize(dir).ok_or_else(|| RoomError::NotFound(dir.to_string()))?;
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut children: BTreeSet<String> = BTreeSet::new();
        for path in self.paths() {
            if let Some(rest) = path.strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((folder, _)) => children.insert(format!("{folder}/")),
                    None => children.insert(rest.to_string()),
                };
            }
        }
        if children.is_empty() && !dir.is_empty() {
            return Err(RoomError::NotFound(dir));
        }
        Ok(children.into_iter().collect())
    }

    /// Write an output file. Accepted only under [`OUT_DIR`].
    pub fn write(&self, path: &str, text: &str) -> Result<(), RoomError> {
        let normal = normalize(path).ok_or_else(|| RoomError::OutsideOut(path.to_string()))?;
        if !normal.starts_with(OUT_DIR) || normal.len() == OUT_DIR.len() {
            return Err(RoomError::OutsideOut(path.to_string()));
        }
        if text.len() > MAX_WRITE_BYTES {
            return Err(RoomError::Invalid(format!(
                "a file may hold at most {MAX_WRITE_BYTES} bytes"
            )));
        }
        let mut out = self.out_lock();
        if !out.contains_key(&normal) && out.len() >= MAX_WRITE_FILES {
            return Err(RoomError::Invalid(format!(
                "at most {MAX_WRITE_FILES} files may be written"
            )));
        }
        out.insert(normal, text.to_string());
        Ok(())
    }

    /// The files written so far: path to text.
    pub fn outputs(&self) -> BTreeMap<String, String> {
        self.out_lock().clone()
    }

    /// The lines of each served file that are longer than [`MAX_READ_LINE_CHARS`] characters:
    /// path to the 1-based line numbers. The command line tool cuts such a line, so a Read that
    /// returned it did not show it whole.
    pub fn long_lines(&self) -> BTreeMap<String, Vec<usize>> {
        let mut out = BTreeMap::new();
        for path in self.paths() {
            let Some(text) = self.text_of(&path) else {
                continue;
            };
            let long: Vec<usize> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| l.chars().count() > MAX_READ_LINE_CHARS)
                .map(|(i, _)| i + 1)
                .collect();
            if !long.is_empty() {
                out.insert(path, long);
            }
        }
        out
    }

    /// Where a path of the room lands on disk under [`Room::materialize_under_code`]: the room's
    /// own files (`_camerata/...`) stay at the root and every other file goes under `code/`.
    pub fn disk_path(path: &str) -> String {
        if path.starts_with(&format!("{RESERVED_DIR}/")) {
            path.to_string()
        } else {
            format!("{CODE_DIR}/{path}")
        }
    }

    /// Write the room to `dir` for the command line transport, the repository's files under
    /// `dir/code/` and the room's own files (`_camerata/`) at `dir`. The session runs with `dir`
    /// as its working directory, so a client's own `.claude/` folder or `.mcp.json`, now under
    /// `code/`, is read like any other file and never loads as project settings, hooks or an MCP
    /// server. Returns the room paths written.
    pub fn materialize_under_code(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        let mut written = Vec::new();
        for path in self.paths() {
            let Some(text) = self.text_of(&path) else {
                continue;
            };
            let target = dir.join(Self::disk_path(&path));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, text)?;
            written.push(path);
        }
        std::fs::create_dir_all(dir.join(OUT_DIR))?;
        Ok(written)
    }

    /// Write the room to `dir` for the command line transport. Returns the paths written.
    pub fn materialize(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        let mut written = Vec::new();
        for path in self.paths() {
            let Some(text) = self.text_of(&path) else {
                continue;
            };
            let target = dir.join(&path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, text)?;
            written.push(path);
        }
        std::fs::create_dir_all(dir.join(OUT_DIR))?;
        Ok(written)
    }
}
