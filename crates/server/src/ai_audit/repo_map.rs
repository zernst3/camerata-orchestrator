//! The repo map, scoped for scale (Fable engine review item M1).
//!
//! Until this module the map was one line per file, with up to 12 symbols, and every scan,
//! alternatives and resolution call carried all of it. At 5,000 files that is 100k to 400k tokens
//! per call before the digest, and nothing checked the prompt against the context window.
//!
//! A [`RepoMap`] is now one of three shapes, chosen once per run from the repo's own size:
//!
//! * [`MapShape::Whole`]: the whole map as it always was (every path with its symbols), when it is
//!   small enough ([`WHOLE_MAP_TOKEN_BUDGET`]). Byte-identical to the old map, so a small run does
//!   not change. It leads the prompt and is the cached prefix, as before.
//! * [`MapShape::Tree`]: the complete FILE TREE, paths only, rendered by directory. It is byte
//!   identical on every call of the run and leads the prompt, so it sits in the cached static
//!   prefix and costs a cache read after the first call. Symbols are not in it: the chunk's own
//!   files and their neighbours get them in a separate section ([`RepoMap::symbols_for`]) that
//!   sits AFTER the cache breakpoint, because it differs per chunk.
//! * [`MapShape::Outline`]: the tree's fallback, a per-directory outline with file counts, used
//!   only when even the path-only tree exceeds [`REPO_TREE_TOKEN_BUDGET`]. It is a coverage
//!   limitation: a chunk can no longer name a file it has not been given. The run records it
//!   ([`TreeFallback`]) and the report discloses it; it is never silent.
//!
//! The complete tree matters because the map is how a chunk learns another file exists and asks
//! for it through `needs_files` (the resolution round). Hiding paths would stop a chunk of
//! controllers from naming the exact command file it needs.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::repo_map_line;

/// Bytes per token the size estimates assume. Real code runs near 3.5 to 4 bytes per token, so 3
/// overestimates: a request judged to fit by this figure fits. The estimate is a constant, never
/// a token-counting API call.
pub(super) const CHARS_PER_TOKEN_ESTIMATE: usize = 3;

/// The estimated tokens of `bytes` bytes of prompt text. Rounds up.
pub(super) const fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(CHARS_PER_TOKEN_ESTIMATE)
}

/// The whole old-style map (every path with its symbols) is used, byte for byte, when it is no
/// larger than this many estimated tokens. About 330 files with the usual symbols, so every
/// fixture and most small repos keep the prompt they always had.
pub(super) const WHOLE_MAP_TOKEN_BUDGET: usize = 20_000;

/// The path-only file tree is sent in full while it is no larger than this many estimated
/// tokens. At about 25 bytes per file (a directory-grouped name plus its indent and newline) that
/// is room for roughly 23,000 files, so a 20,000-file repo still gets every path. It is read from
/// the cache after the first call of a run. Above it the map falls back to a directory outline.
pub(super) const REPO_TREE_TOKEN_BUDGET: usize = 200_000;

/// The largest the symbols section of one call may be, in estimated tokens: the symbols of the
/// call's chunk files and their neighbours, clipped in priority order when a directory is huge.
pub(super) const SYMBOLS_TOKEN_BUDGET: usize = 20_000;

/// The tree budget for a model with `window` tokens of context: [`REPO_TREE_TOKEN_BUDGET`], but
/// never more than a quarter of the window, so the tree always leaves room for the code.
pub(super) fn tree_budget_for_window(window: usize) -> usize {
    REPO_TREE_TOKEN_BUDGET.min(window / 4)
}

/// What the map looks like for this run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MapShape {
    Whole,
    Tree,
    Outline,
}

/// A run's record that the file tree fell back to a directory outline (see [`MapShape::Outline`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeFallback {
    /// Files in the repo.
    pub files: usize,
    /// The estimated tokens the path-only tree would have taken.
    pub tree_tokens: usize,
    /// The budget it exceeded.
    pub budget: usize,
}

impl TreeFallback {
    /// The Coverage sentence the report carries (client wording: no dashes).
    pub(crate) fn disclosure(&self) -> String {
        format!(
            "reduced to a per-directory outline: the repository has {} files, and the list of \
             every file path (about {} tokens) is larger than the {} tokens a model call carries, \
             so each call saw an outline with file counts instead of every path. A pass could \
             name only the files it was given and their neighbours when it asked for another \
             file, so a defect that spans files far apart may not have been found.",
            self.files, self.tree_tokens, self.budget
        )
    }
}

/// One file of the map.
struct Entry {
    path: String,
    /// The file's old-style map line: `  path  [symbols]\n`.
    line: String,
}

/// The repo map of one run. Built once; each call asks it for its own symbols section.
pub(crate) struct RepoMap {
    shape: MapShape,
    /// The text that leads every prompt: the whole map, the tree or the outline.
    prefix: String,
    entries: Vec<Entry>,
    by_path: HashMap<String, usize>,
    /// Directory (empty for the root) to the entries in it.
    dirs: HashMap<String, Vec<usize>>,
    /// Per entry: the files it imports, and the files that import it (best effort).
    imports: Vec<Vec<usize>>,
    importers: Vec<Vec<usize>>,
    fallback: Option<TreeFallback>,
}

/// What one call's symbols section holds.
pub(crate) struct SymbolsSection {
    /// The section text; empty when the whole map is the prefix (it already has every symbol).
    pub text: String,
    /// Files with a symbols line in this call (the chunk's files and their neighbours).
    pub files_with_symbols: usize,
    /// Files in the repo.
    pub files_in_map: usize,
    /// Files the prefix does not name one by one (all of them in an outline, else none).
    pub outlined_files: usize,
}

impl RepoMap {
    /// A prefix-only map, for a test or a caller with a ready-made map text: no tree, no
    /// symbols section.
    #[cfg(test)]
    pub(crate) fn whole_text(text: impl Into<String>) -> Self {
        Self {
            shape: MapShape::Whole,
            prefix: text.into(),
            entries: Vec::new(),
            by_path: HashMap::new(),
            dirs: HashMap::new(),
            imports: Vec::new(),
            importers: Vec::new(),
            fallback: None,
        }
    }

    /// The map of `files`. `tree_budget` is the path-only tree's token budget
    /// ([`tree_budget_for_window`]).
    pub(crate) fn new(
        files: &[(String, String)],
        nonce: &crate::prompt_frame::FrameNonce,
        tree_budget: usize,
    ) -> Self {
        let entries: Vec<Entry> = files
            .iter()
            .map(|(path, content)| Entry {
                path: path.clone(),
                line: repo_map_line(path, content),
            })
            .collect();
        let by_path: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        let mut dirs: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            dirs.entry(dir_of(&e.path).to_string()).or_default().push(i);
        }
        let whole_listing: String = entries.iter().map(|e| e.line.as_str()).collect();
        let whole = whole_map_text(&whole_listing, nonce);
        let (shape, prefix, fallback) = if estimate_tokens(whole.len()) <= WHOLE_MAP_TOKEN_BUDGET {
            (MapShape::Whole, whole, None)
        } else {
            let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
            let tree = tree_text(&tree_listing(&paths), nonce);
            let tree_tokens = estimate_tokens(tree.len());
            if tree_tokens <= tree_budget {
                (MapShape::Tree, tree, None)
            } else {
                let outline = outline_text(&paths, tree_budget, nonce);
                (
                    MapShape::Outline,
                    outline,
                    Some(TreeFallback {
                        files: entries.len(),
                        tree_tokens,
                        budget: tree_budget,
                    }),
                )
            }
        };
        let (imports, importers) = if shape == MapShape::Whole {
            (Vec::new(), Vec::new())
        } else {
            import_graph(files, &by_path)
        };
        Self {
            shape,
            prefix,
            entries,
            by_path,
            dirs,
            imports,
            importers,
            fallback,
        }
    }

    /// The text that leads every prompt of the run. Identical on every call.
    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The most bytes a call's symbols section can take: reserved when the request size is
    /// planned. Zero when the whole map is the prefix (it has no separate symbols section).
    pub(crate) fn symbols_reserve_bytes(&self) -> usize {
        if self.shape == MapShape::Whole {
            0
        } else {
            SYMBOLS_TOKEN_BUDGET * CHARS_PER_TOKEN_ESTIMATE
        }
    }

    /// Set when the tree fell back to an outline: the run must record and disclose it.
    pub(crate) fn tree_fallback(&self) -> Option<&TreeFallback> {
        self.fallback.as_ref()
    }

    /// The symbols section for a call over `chunk`: the public symbols (up to 12 per file) of
    /// the chunk's files and their neighbours, namely the files in the same directory and the
    /// files they import or that import them. Placed after the cache breakpoint by the caller,
    /// because it differs per chunk.
    pub(crate) fn symbols_for(
        &self,
        chunk: &[(String, String)],
        nonce: &crate::prompt_frame::FrameNonce,
    ) -> SymbolsSection {
        if self.shape == MapShape::Whole {
            return SymbolsSection {
                text: String::new(),
                files_with_symbols: self.entries.len(),
                files_in_map: self.entries.len(),
                outlined_files: 0,
            };
        }
        let budget_bytes = SYMBOLS_TOKEN_BUDGET * CHARS_PER_TOKEN_ESTIMATE;
        let mut chosen: Vec<usize> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        let mut used = 0usize;
        // A chunk file the map was not built from still gets its own line, from its own content.
        let mut extra: Vec<String> = Vec::new();
        let mut take = |i: usize, chosen: &mut Vec<usize>, used: &mut usize| -> bool {
            if seen.contains(&i) {
                return true;
            }
            let len = self.entries[i].line.len();
            if *used + len > budget_bytes {
                return false;
            }
            *used += len;
            seen.insert(i);
            chosen.push(i);
            true
        };
        let chunk_idx: Vec<Option<usize>> = chunk
            .iter()
            .map(|(p, _)| self.by_path.get(p).copied())
            .collect();
        // Tier 0: the chunk's own files.
        for ((path, content), idx) in chunk.iter().zip(&chunk_idx) {
            match idx {
                Some(i) => {
                    take(*i, &mut chosen, &mut used);
                }
                None => {
                    let line = repo_map_line(path, content);
                    if used + line.len() <= budget_bytes {
                        used += line.len();
                        extra.push(line);
                    }
                }
            }
        }
        // Tier 1: files they import and files that import them.
        'imports: for i in chunk_idx.iter().flatten() {
            for &j in self.imports[*i].iter().chain(&self.importers[*i]) {
                if !take(j, &mut chosen, &mut used) {
                    break 'imports;
                }
            }
        }
        // Tier 2: files in the same directory.
        let mut dirs_done: HashSet<&str> = HashSet::new();
        'dirs: for ((path, _), _) in chunk.iter().zip(&chunk_idx) {
            let dir = dir_of(path);
            if !dirs_done.insert(dir) {
                continue;
            }
            if let Some(members) = self.dirs.get(dir) {
                for &j in members {
                    if !take(j, &mut chosen, &mut used) {
                        break 'dirs;
                    }
                }
            }
        }
        chosen.sort_unstable();
        let mut listing = String::new();
        for i in &chosen {
            listing.push_str(&self.entries[*i].line);
        }
        for line in &extra {
            listing.push_str(line);
        }
        let with_symbols = chosen.len() + extra.len();
        let text = if listing.is_empty() {
            String::new()
        } else {
            let mut out = String::from(
                "SYMBOLS — the public symbols of the files in this pass and of their neighbours \
                 (the files in the same directory, and the files they import or that import \
                 them). Every file's path is in the REPO MAP above:\n",
            );
            out.push_str(&crate::prompt_frame::frame_untrusted(
                nonce, "symbols", &listing,
            ));
            out.push('\n');
            out
        };
        SymbolsSection {
            text,
            files_with_symbols: with_symbols,
            files_in_map: self.entries.len(),
            outlined_files: if self.shape == MapShape::Outline {
                self.entries.len()
            } else {
                0
            },
        }
    }
}

/// The directory of a repo-relative path (empty for a file at the root).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The old whole map around an already-built `listing` (one `repo_map_line` per file). The ONE
/// definition of its header, shared with `build_repo_map` so the two cannot drift.
pub(super) fn whole_map_text(listing: &str, nonce: &crate::prompt_frame::FrameNonce) -> String {
    let mut out = String::from(
        "REPO MAP — every file in the repo and the public symbols it defines. Only SOME file \
         bodies appear in THIS pass; use this map for cross-file architectural context (which \
         directories are repositories vs services vs controllers, where a named type lives):\n",
    );
    // Paths and symbol names come from the client's files: framed like a file body.
    out.push_str(&crate::prompt_frame::frame_untrusted(
        nonce, "repo map", listing,
    ));
    out.push('\n');
    out
}

/// The path-only tree, grouped by directory: the directory path once (`dir/`, `./` for the root),
/// then its file names, indented. A file's path is its directory followed by its name.
fn tree_listing(paths: &[&str]) -> String {
    let mut by_dir: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for p in paths {
        let (dir, name) = p.rsplit_once('/').unwrap_or(("", p));
        by_dir.entry(dir).or_default().push(name);
    }
    let mut out = String::new();
    for (dir, names) in &mut by_dir {
        names.sort_unstable();
        if dir.is_empty() {
            out.push_str("./\n");
        } else {
            out.push_str(dir);
            out.push_str("/\n");
        }
        for n in names.iter() {
            out.push_str("  ");
            out.push_str(n);
            out.push('\n');
        }
    }
    out
}

fn tree_text(listing: &str, nonce: &crate::prompt_frame::FrameNonce) -> String {
    let mut out = String::from(
        "REPO MAP — the complete file tree of the repo, grouped by directory: a directory line \
         (`./` is the repo root) and under it the names of the files in it. A file's path is its \
         directory followed by its name. Only SOME file bodies appear in THIS pass; use this map \
         for cross-file architectural context (which directories are repositories vs services vs \
         controllers) and to name any other file you need to read. The public symbols of the \
         files in this pass and their neighbours follow the map:\n",
    );
    out.push_str(&crate::prompt_frame::frame_untrusted(
        nonce, "repo map", listing,
    ));
    out.push('\n');
    out
}

/// The directory outline: each directory and how many files it holds, rolled up to fewer path
/// components until it fits `budget` tokens. At depth 0 only the totals remain.
fn outline_text(
    paths: &[&str],
    budget_tokens: usize,
    nonce: &crate::prompt_frame::FrameNonce,
) -> String {
    let header = "REPO MAP — the repo is too large to list every file. This is an outline: each \
                  directory and how many files it holds (`./` is the repo root). Only SOME file \
                  bodies appear in THIS pass. The public symbols of the files in this pass and \
                  of their neighbours, with their exact paths, follow the map; another file can \
                  be named only if its path is known from there or from the code:\n";
    let dirs: Vec<&str> = paths.iter().map(|p| dir_of(p)).collect();
    let max_depth = dirs
        .iter()
        .map(|d| {
            if d.is_empty() {
                0
            } else {
                d.split('/').count()
            }
        })
        .max()
        .unwrap_or(0);
    let budget_bytes = budget_tokens * CHARS_PER_TOKEN_ESTIMATE;
    for depth in (1..=max_depth).rev() {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for d in &dirs {
            let key = if d.is_empty() {
                "./".to_string()
            } else {
                let parts: Vec<&str> = d.split('/').take(depth).collect();
                format!("{}/", parts.join("/"))
            };
            *counts.entry(key).or_insert(0) += 1;
        }
        let mut listing = String::new();
        for (dir, n) in &counts {
            listing.push_str(&format!(
                "  {dir}  ({n} file{})\n",
                if *n == 1 { "" } else { "s" }
            ));
        }
        if listing.len() <= budget_bytes {
            let mut out = String::from(header);
            out.push_str(&crate::prompt_frame::frame_untrusted(
                nonce, "repo map", &listing,
            ));
            out.push('\n');
            return out;
        }
    }
    let mut out = String::from(header);
    let total = format!("  {} files in the repo\n", paths.len());
    out.push_str(&crate::prompt_frame::frame_untrusted(
        nonce, "repo map", &total,
    ));
    out.push('\n');
    out
}

// ── Imports (best effort) ────────────────────────────────────────────────────────────────────

/// For every file, the files it imports and the files that import it, resolved against the
/// repo's own paths. Best effort and cheap: a relative import in the languages below (JavaScript
/// and TypeScript, Python, Rust `mod`). An import it cannot resolve is simply absent; the
/// same-directory neighbours still apply.
fn import_graph(
    files: &[(String, String)],
    by_path: &HashMap<String, usize>,
) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let mut imports: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    let mut importers: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    for (i, (path, content)) in files.iter().enumerate() {
        let mut found: Vec<usize> = Vec::new();
        for candidates in import_candidates(path, content) {
            if let Some(j) = candidates.iter().find_map(|c| by_path.get(c)) {
                if *j != i && !found.contains(j) {
                    found.push(*j);
                }
            }
        }
        for j in &found {
            importers[*j].push(i);
        }
        imports[i] = found;
    }
    (imports, importers)
}

/// Each import of a file as the list of repo paths it could mean, best first.
fn import_candidates(path: &str, content: &str) -> Vec<Vec<String>> {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    let dir = dir_of(path);
    let mut out = Vec::new();
    match ext {
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "vue" | "svelte" => {
            for line in content.lines() {
                if !(line.contains("import") || line.contains("require") || line.contains("from")) {
                    continue;
                }
                if let Some(spec) = js_specifier(line) {
                    if spec.starts_with("./") || spec.starts_with("../") {
                        out.push(js_candidates(dir, spec));
                    }
                }
            }
        }
        "py" => {
            for line in content.lines() {
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("from ") {
                    let module = rest.split_whitespace().next().unwrap_or("");
                    if module.starts_with('.') {
                        out.push(py_candidates(dir, module));
                    }
                }
            }
        }
        "rs" => {
            let stem = path
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".rs"))
                .unwrap_or("");
            for line in content.lines() {
                let t = line.trim_start();
                let t = t
                    .strip_prefix("pub(crate) ")
                    .or_else(|| t.strip_prefix("pub "))
                    .unwrap_or(t);
                if let Some(rest) = t.strip_prefix("mod ") {
                    if let Some(name) = rest.strip_suffix(';') {
                        let name = name.trim();
                        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            let mut c = vec![
                                join(dir, &format!("{name}.rs")),
                                join(dir, &format!("{name}/mod.rs")),
                            ];
                            if !matches!(stem, "mod" | "lib" | "main") {
                                c.push(join(dir, &format!("{stem}/{name}.rs")));
                            }
                            out.push(c);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// The quoted module specifier of an import / require / dynamic import line.
fn js_specifier(line: &str) -> Option<&str> {
    for marker in ["from ", "require(", "import(", "import "] {
        if let Some(at) = line.find(marker) {
            let rest = line[at + marker.len()..].trim_start();
            let quote = rest.chars().next()?;
            if quote == '\'' || quote == '"' || quote == '`' {
                let body = &rest[1..];
                return body.find(quote).map(|end| &body[..end]);
            }
        }
    }
    None
}

fn js_candidates(dir: &str, spec: &str) -> Vec<String> {
    let base = normalize(&join(dir, spec));
    let mut c = vec![base.clone()];
    for ext in [
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "vue", "svelte",
    ] {
        c.push(format!("{base}.{ext}"));
    }
    for ext in ["ts", "tsx", "js", "jsx"] {
        c.push(format!("{base}/index.{ext}"));
    }
    c
}

fn py_candidates(dir: &str, module: &str) -> Vec<String> {
    let dots = module.chars().take_while(|c| *c == '.').count();
    let mut base = dir.to_string();
    for _ in 1..dots {
        base = dir_of(&base).to_string();
    }
    let rest = module[dots..].replace('.', "/");
    let target = if rest.is_empty() {
        base
    } else {
        join(&base, &rest)
    };
    vec![format!("{target}.py"), join(&target, "__init__.py")]
}

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// `a/b/../c/./d` to `a/c/d`. A path that climbs out of the repo keeps its `..` (it matches
/// nothing).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}
