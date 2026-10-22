//! Loads the import graph cannot follow, as sources of liveness.
//!
//! - `G9` computed loads (`import(name)`, `require(name)`, `require.context(dir)`,
//!   `import.meta.glob(pattern)`, Python `import_module(name)` and `pkgutil` discovery, Rust
//!   `include_dir!`): the literal head of the argument, when it has one and it names a folder in
//!   the scan, roots every file under that folder; with no usable head, the loader could name
//!   anything in its package, so the whole package is rooted.
//! - `G10` loader calls with a literal path (`readdirSync('./handlers')`, `glob("plugins/*.py")`):
//!   the folder the literal names is rooted. Scan lists that are not loader calls (a Tailwind
//!   `content` glob, a tsconfig `include`) are deliberately NOT honoured: they would mark every
//!   file in `src/` alive and silence the rule.
//! - Glob strings with a literal part (`entities: ['src/**/*.entity.ts']`, `'**/*.module.ts'`,
//!   `'**/handlers/*.ts'`): every file matching the whole pattern is rooted. A glob that is only
//!   an extension list under a literal prefix (`./src/**/*.{js,ts,tsx}`) is the Tailwind and
//!   tsconfig `include` shape and stays ignored, or the rule would be silent in every such repo.

use std::collections::{BTreeSet, HashSet};

use crate::arch_checker::glob_match;
use crate::extract::resolver::{directory_of, normalize_join};
use crate::extract::{self, ComputedLoad};

use super::manifests::ManifestDirs;
use super::roots::RootSet;
use super::{family_of, Family};

/// Calls whose first argument is a path the program reads files from.
const LOADER_NAMES: &[&str] = &[
    "require.context",
    "import.meta.glob",
    "globSync",
    "fastGlob",
    "readdirSync",
    "readdir",
    "opendir",
    "iter_modules",
    "walk_packages",
    "listdir",
    "include_dir",
    "pin_all_from",
    "read_dir",
    "iterdir",
    "rglob",
    "glob",
    "sync",
    "walk",
    "fg",
    "dir",
];

/// Loader names that take their path as a bare command argument, with no parentheses
/// (`pin_all_from "app/javascript/controllers"` in a Rails importmap).
const PARENLESS_LOADERS: &[&str] = &["pin_all_from"];

const CARGO_MANIFEST_DIR: &str = "$CARGO_MANIFEST_DIR/";

/// Every ancestor folder of every file in the scan.
fn directory_set(files: &[(String, String)]) -> HashSet<String> {
    let mut dirs = HashSet::new();
    for (path, _) in files {
        let mut dir = directory_of(path);
        while !dir.is_empty() && dirs.insert(dir.to_string()) {
            dir = directory_of(dir);
        }
    }
    dirs
}

pub(super) fn dynamic_roots(files: &[(String, String)], manifests: &ManifestDirs) -> RootSet {
    let dirs = directory_set(files);
    let mut roots = RootSet::default();
    for (path, content) in files {
        if let Some(lang) = extract::lang_for_path(path) {
            let family = family_of(path).unwrap_or(Family::Ecma);
            for load in extract::computed_loads(lang, content) {
                computed_load_roots(&mut roots, path, family, &load, manifests, &dirs);
            }
        }
        literal_directory_roots(&mut roots, path, content, manifests, &dirs);
    }
    glob_string_roots(&mut roots, files, manifests);
    roots
}

/// Cap on brace expansions of one pattern (`{a,b}{c,d}{e,f}...`).
const MAX_BRACE_EXPANSIONS: usize = 64;

/// `pattern` with every `{a,b}` group expanded (no nesting). An unbalanced brace is left as is.
pub(super) fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}').map(|i| open + i) else {
        return vec![pattern.to_string()];
    };
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    let mut out = Vec::new();
    for alt in pattern[open + 1..close].split(',') {
        for rest in expand_braces(tail) {
            if out.len() >= MAX_BRACE_EXPANSIONS {
                return out;
            }
            out.push(format!("{head}{alt}{rest}"));
        }
    }
    out
}

fn has_literal(segment: &str) -> bool {
    segment
        .chars()
        .any(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn has_wildcard(segment: &str) -> bool {
    segment.contains(['*', '?', '{', '['])
}

/// Whether a glob string names a SET OF FILES by something other than their extension: the
/// basename has a literal part beyond its extension list (`*.entity.ts`, `*.module.ts`,
/// `*.resolver.{ts,js}`), or a literal folder follows a wildcard (`**/handlers/*.ts`). A literal
/// folder PREFIX with a pure extension list (`./src/**/*.{js,ts}`, Tailwind's `content`,
/// tsconfig's `include`) is not meaningful: it would mark everything under `src/` alive.
pub(super) fn is_meaningful_glob(pattern: &str) -> bool {
    if pattern.starts_with('!')
        || pattern.contains(char::is_whitespace)
        || pattern.len() > 200
        || !pattern.contains('*')
    {
        return false;
    }
    let segments: Vec<&str> = pattern.split('/').collect();
    let Some((basename, dirs)) = segments.split_last() else {
        return false;
    };
    // The basename minus its extension list: `*.entity.ts` gives `*.entity`; `*.{ts,js}` gives `*`.
    let stem = match basename.rfind('.') {
        Some(dot) if !basename[dot..].contains('*') => &basename[..dot],
        _ => match basename.find(".{") {
            Some(dot) => &basename[..dot],
            None => basename,
        },
    };
    let stem_is_meaningful = has_wildcard(basename) && has_literal(stem);
    let first_wild = dirs.iter().position(|s| has_wildcard(s));
    let dir_below_wildcard = first_wild.is_some_and(|i| {
        dirs[i + 1..]
            .iter()
            .any(|s| !has_wildcard(s) && has_literal(s) && *s != "." && *s != "..")
    });
    stem_is_meaningful || dir_below_wildcard
}

/// Quoted strings of `content` that are meaningful globs.
fn glob_strings(content: &str) -> Vec<&str> {
    if !content.contains('*') {
        return Vec::new();
    }
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'\'' | b'"' | b'`') {
            let quote = bytes[i];
            let end = bytes[i + 1..]
                .iter()
                .position(|&b| b == quote || b == b'\n')
                .map(|n| i + 1 + n);
            let Some(end) = end else { break };
            if bytes[end] == quote {
                let literal = &content[i + 1..end];
                if is_meaningful_glob(literal) {
                    out.push(literal);
                }
                i = end + 1;
            } else {
                i = end;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Files that a meaningful glob string anywhere in the scan matches are rooted. The pattern is
/// resolved from the repository root and from the nearest manifest folder of the file holding it.
/// `tsconfig*.json` and `jsconfig*.json` are not read: their lists mostly say which files NOT to
/// build.
fn glob_string_roots(roots: &mut RootSet, files: &[(String, String)], manifests: &ManifestDirs) {
    let mut patterns: BTreeSet<(String, String)> = BTreeSet::new();
    for (path, content) in files {
        let base_name = path.rsplit('/').next().unwrap_or(path);
        if (base_name.starts_with("tsconfig") || base_name.starts_with("jsconfig"))
            && base_name.ends_with(".json")
        {
            continue;
        }
        for literal in glob_strings(content) {
            let trimmed = literal.trim_start_matches("./").trim_start_matches('/');
            // A pattern with no folder part matches at any depth, like a gitignore entry.
            let anchored = if trimmed.contains('/') {
                trimmed.to_string()
            } else {
                format!("**/{trimmed}")
            };
            for base in [manifests.nearest(path), ""] {
                for expanded in expand_braces(&anchored) {
                    patterns.insert((base.to_string(), expanded));
                }
            }
        }
    }
    if patterns.is_empty() {
        return;
    }
    for (path, _) in files {
        let matched = patterns.iter().any(|(base, pattern)| {
            let rel = if base.is_empty() {
                Some(path.as_str())
            } else {
                path.strip_prefix(base.as_str())
                    .and_then(|r| r.strip_prefix('/'))
            };
            rel.is_some_and(|rel| glob_match(pattern, rel))
        });
        if matched {
            roots.insert_file(path.clone());
        }
    }
}

/// The folder `rel` names when read from `base`, if the scan has such a folder.
///
/// A `rel` that names `base` itself (`""`, `.`, `..`) only counts when `allow_base` is set: a
/// computed load whose prefix is `./` really does load from the caller's folder, but a literal
/// like `./handlers` cut at its last `/` leaves `.`, which must not root the whole folder.
fn existing_dir(base: &str, rel: &str, allow_base: bool, dirs: &HashSet<String>) -> Option<String> {
    let names_base = rel.trim_matches(|c| c == '.' || c == '/').is_empty();
    if names_base && !allow_base {
        return None;
    }
    let joined = normalize_join(base, rel);
    let exists = dirs.contains(&joined) || (joined.is_empty() && allow_base);
    exists.then_some(joined)
}

fn computed_load_roots(
    roots: &mut RootSet,
    caller: &str,
    family: Family,
    load: &ComputedLoad,
    manifests: &ManifestDirs,
    dirs: &HashSet<String>,
) {
    let whole_package = manifests.nearest(caller).to_string();
    // A Python load names a dotted module, not a path; a load with no literal head names
    // anything. Either could reach any file of the package.
    let Some(prefix) = load.prefix.as_deref().filter(|_| family != Family::Python) else {
        roots.insert_dir(whole_package);
        return;
    };
    let caller_dir = directory_of(caller);
    let manifest_dir = manifests.nearest(caller);
    let (rest, bases): (&str, Vec<&str>) = if family == Family::Rust {
        // Rust directory macros read relative to the crate (`$CARGO_MANIFEST_DIR/..`).
        match prefix.strip_prefix(CARGO_MANIFEST_DIR) {
            Some(rest) => (rest, vec![manifest_dir]),
            None => (prefix, vec![caller_dir, manifest_dir]),
        }
    } else if (prefix.starts_with('.') || prefix.starts_with('/')) && prefix.contains('/') {
        (prefix, vec![caller_dir])
    } else {
        // A bare specifier (`@/plugins/`, `plugin-`) is an alias or a package: unknowable here.
        roots.insert_dir(whole_package);
        return;
    };
    // The load can reach any file below the prefix cut at its last `/`.
    let dir_part = rest.rfind('/').map_or("", |i| &rest[..i]);
    for base in bases {
        if let Some(dir) = existing_dir(base, dir_part, true, dirs) {
            roots.insert_dir(dir);
        }
    }
}

/// Loader calls whose first argument is a string literal holding a `/`.
fn literal_directory_roots(
    roots: &mut RootSet,
    caller: &str,
    content: &str,
    manifests: &ManifestDirs,
    dirs: &HashSet<String>,
) {
    if !content.contains('/') {
        return;
    }
    for literal in loader_literals(content) {
        // A directory is the whole head (`./handlers`); only when the whole head is not a
        // folder is it a file prefix (`./plugins/index`) and its parent the folder. Cutting a
        // head that already names a folder would root the folder's parent as well.
        let whole = literal.trim_end_matches('/');
        let cut = whole.rfind('/').map_or("", |i| &whole[..i]);
        for base in [directory_of(caller), manifests.nearest(caller), ""] {
            if let Some(dir) = existing_dir(base, whole, false, dirs) {
                roots.insert_dir(dir);
            } else if let Some(dir) = existing_dir(base, cut, false, dirs) {
                roots.insert_dir(dir);
            }
        }
    }
}

fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The literal head (text before any wildcard or interpolation) of the string literal that is
/// the first argument of each loader call in `content`, for literals containing a `/`.
fn loader_literals(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    for name in LOADER_NAMES {
        if !content.contains(name) {
            continue;
        }
        for (start, _) in content.match_indices(name) {
            if start > 0 && is_identifier_byte(bytes[start - 1]) {
                continue;
            }
            let mut i = start + name.len();
            // `dir!(` (a macro) and `fn (` with spaces are both calls.
            if bytes.get(i) == Some(&b'!') {
                i += 1;
            }
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if bytes.get(i) == Some(&b'(') {
                i += 1;
            } else if !PARENLESS_LOADERS.contains(name) {
                continue;
            }
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            let Some(&quote) = bytes.get(i) else { continue };
            if !matches!(quote, b'\'' | b'"' | b'`') {
                continue;
            }
            let body_start = i + 1;
            let Some(len) =
                content[body_start..].find(|c: char| c as u32 == u32::from(quote) || c == '\n')
            else {
                continue;
            };
            let literal = &content[body_start..body_start + len];
            if !literal.contains('/') {
                continue;
            }
            let head_end = literal
                .find(['*', '{', '[', '?', '$'])
                .unwrap_or(literal.len());
            out.push(literal[..head_end].to_string());
        }
    }
    out
}
