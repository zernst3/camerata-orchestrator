//! The mention guard (`G8`, the master guard). A file is "mentioned" when its name appears as a
//! whole word in the text of any other file. Every literal reference to a file (an import
//! specifier the resolver dropped, a `<script src>`, a CI command, a README line) contains its
//! name, so a mention keeps a file alive even when no import edge was resolved.

use std::collections::HashMap;

/// Basenames that name their directory rather than themselves.
const INDEX_LIKE: &[&str] = &[
    "index.ts",
    "index.tsx",
    "index.js",
    "index.jsx",
    "index.mjs",
    "index.cjs",
    "mod.rs",
    "lib.rs",
    "main.rs",
    "mod.ts",
    "__init__.py",
];

/// Keys too generic to search for: treated as mentioned without looking.
const STOPLIST: &[&str] = &[
    "index",
    "utils",
    "util",
    "types",
    "type",
    "helpers",
    "helper",
    "constants",
    "const",
    "config",
    "main",
    "app",
    "api",
    "lib",
    "core",
    "common",
    "shared",
    "base",
    "data",
    "model",
    "models",
    "service",
    "services",
    "test",
    "tests",
    "src",
    "client",
    "server",
    "store",
    "hooks",
    "hook",
    "routes",
    "route",
    "pages",
    "page",
    "components",
    "component",
    "schema",
    "errors",
    "error",
    "logger",
    "auth",
    "user",
    "users",
    "db",
    "handler",
    "handlers",
    "actions",
    "state",
    "styles",
    "theme",
    "setup",
];

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The text a file is searched for: its basename minus the final extension
/// (`foo.service.ts` gives `foo.service`); for an index-like basename, the parent directory's
/// name instead (`src/billing/index.ts` gives `billing`).
pub(super) fn key_of(path: &str) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let stem = file.rfind('.').map_or(file, |dot| &file[..dot]);
    if INDEX_LIKE.contains(&file.to_ascii_lowercase().as_str()) {
        let parent = dir.rsplit('/').next().unwrap_or("");
        if !parent.is_empty() {
            return parent.to_string();
        }
    }
    stem.to_string()
}

/// Keys that are short, generic or not searchable are treated as mentioned: the file is never
/// reported. Being wrong here only ever hides a finding.
pub(super) fn is_always_mentioned(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    key.len() < 3
        || !lower.bytes().next().is_some_and(is_word_byte)
        || STOPLIST.contains(&lower.as_str())
}

/// For each key, the indexes (into the file slice it was built from) of every file whose text
/// mentions it. Built in one pass over the files, however many keys there are.
pub(super) struct MentionIndex {
    by_key: HashMap<String, Vec<usize>>,
}

impl MentionIndex {
    pub(super) fn build(files: &[(String, String)], keys: &[String]) -> Self {
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        // Group the keys by their leading word, so a file is tokenized once and each word is
        // looked up once.
        let mut by_first_word: HashMap<String, Vec<String>> = HashMap::new();
        for key in keys {
            let lower = key.to_ascii_lowercase();
            let first: String = lower
                .bytes()
                .take_while(|b| is_word_byte(*b))
                .map(char::from)
                .collect();
            if first.is_empty() {
                continue;
            }
            by_key.entry(lower.clone()).or_default();
            let group = by_first_word.entry(first).or_default();
            if !group.contains(&lower) {
                group.push(lower);
            }
        }
        for (idx, (_, content)) in files.iter().enumerate() {
            let lower = content.to_ascii_lowercase();
            let bytes = lower.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if !is_word_byte(bytes[i]) {
                    i += 1;
                    continue;
                }
                let mut j = i;
                while j < bytes.len() && is_word_byte(bytes[j]) {
                    j += 1;
                }
                if let Some(group) = by_first_word.get(&lower[i..j]) {
                    for key in group {
                        let end = i + key.len();
                        let whole = lower[i..].starts_with(key.as_str())
                            && bytes.get(end).is_none_or(|b| !is_word_byte(*b));
                        if whole {
                            if let Some(list) = by_key.get_mut(key) {
                                if list.last() != Some(&idx) {
                                    list.push(idx);
                                }
                            }
                        }
                    }
                }
                i = j;
            }
        }
        Self { by_key }
    }

    /// Indexes of the files mentioning `key` (empty for a key that was not indexed).
    pub(super) fn mentioners(&self, key: &str) -> &[usize] {
        self.by_key
            .get(&key.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }
}
