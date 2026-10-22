//! The set of files that are alive by definition, kept as exact files, folder subtrees and
//! whole packages so one root predicate can name "everything under this folder" without
//! enumerating it.

use std::collections::{BTreeSet, HashSet};

/// Whether `path` lies in `dir`'s subtree. The empty dir is the repository root, i.e. everything.
pub(super) fn is_inside(path: &str, dir: &str) -> bool {
    dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

#[derive(Debug, Default)]
pub(super) struct RootSet {
    files: HashSet<String>,
    /// Every file in these folders' subtrees.
    dirs: BTreeSet<String>,
    /// Every file whose NEAREST `package.json` sits in one of these folders (a nested package
    /// is not covered by its parent's flag).
    packages: BTreeSet<String>,
}

impl RootSet {
    pub(super) fn insert_file(&mut self, path: String) {
        self.files.insert(path);
    }

    pub(super) fn insert_dir(&mut self, dir: String) {
        self.dirs.insert(dir);
    }

    pub(super) fn insert_package(&mut self, dir: String) {
        self.packages.insert(dir);
    }

    /// Whether `path` is named directly or lies under a rooted folder.
    pub(super) fn contains(&self, path: &str) -> bool {
        self.files.contains(path) || self.dirs.iter().any(|d| is_inside(path, d))
    }

    /// Whether a file whose nearest package sits in `package_dir` is rooted as a package member.
    pub(super) fn contains_package(&self, package_dir: &str) -> bool {
        self.packages.contains(package_dir)
    }
}
