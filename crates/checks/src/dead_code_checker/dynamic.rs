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

use std::collections::HashSet;

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
    "read_dir",
    "iterdir",
    "rglob",
    "glob",
    "sync",
    "walk",
    "fg",
    "dir",
];

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
    roots
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
        // A directory is either the whole head (`./handlers`) or the head cut at its last `/`
        // (`./plugins/index`).
        let whole = literal.trim_end_matches('/');
        let cut = whole.rfind('/').map_or("", |i| &whole[..i]);
        for base in [directory_of(caller), manifests.nearest(caller), ""] {
            for rel in [whole, cut] {
                if let Some(dir) = existing_dir(base, rel, false, dirs) {
                    roots.insert_dir(dir);
                }
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
            if bytes.get(i) != Some(&b'(') {
                continue;
            }
            i += 1;
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
