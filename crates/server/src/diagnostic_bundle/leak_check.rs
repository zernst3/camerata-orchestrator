//! The last guard (design section 2): before anything is written, search the bundle and its
//! preview for what must never be in them, whatever the decision table said.
//!
//! - any `camerata_secrets::detect_secrets` match;
//! - any repo name, repo folder name or home folder name;
//! - any path segment or file stem of 4 or more characters seen in the three inputs (not under
//!   `--named-paths`, where the bundle shows paths on purpose).
//!
//! One hit refuses the write and names the key path, never the value (for a hit on a map key,
//! the parent path and `<key>`).
//!
//! Words are compared whole, lower-cased, split on anything that is not a letter or digit, the
//! same way the skeleton splits. A segment that is itself an engine word (a vocabulary word, a
//! field name of the bundle, an enum value) is not searched for: the bundle holds those words by
//! design, and a client folder called `content` or `error` says nothing about the client that
//! the engine's own word does not. A repo, folder or home name of more than one word is also
//! searched for whole, as a substring.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::OnceLock;

use serde_json::Value;

use super::decisions::{enums, DecisionTable};
use super::sanitize::{looks_like_path, vocab, RepoInfo};

/// The shortest segment searched for.
pub const MIN_SEGMENT_CHARS: usize = 4;

/// What a hit was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeakKind {
    /// A secret shape.
    Secret,
    /// A repo name, a repo folder name or the home folder name.
    RepoName,
    /// A path segment or file stem from the inputs.
    PathSegment,
}

impl LeakKind {
    fn label(self) -> &'static str {
        match self {
            Self::Secret => "a secret",
            Self::RepoName => "a repo, folder or home name",
            Self::PathSegment => "a client path segment",
        }
    }
}

/// A refused write: where the hit was and what kind it was. The hit itself is not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeakRefusal {
    /// The key path in the bundle (`run.repos[0].dir`), or `preview line N`.
    pub key_path: String,
    pub kind: LeakKind,
}

impl fmt::Display for LeakRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the leak check refused the bundle: {} at {}",
            self.kind.label(),
            self.key_path
        )
    }
}

/// What the bundle must not hold, gathered from the inputs.
#[derive(Debug, Clone, Default)]
pub struct LeakInputs {
    /// Repo, folder and home names, whole (lower case, more than one word).
    names: BTreeSet<String>,
    /// Their single words.
    name_words: BTreeSet<String>,
    /// Path segment and stem words.
    segment_words: BTreeSet<String>,
    /// Under `--named-paths`, the repo-relative paths the bundle shows on purpose: cut out of a
    /// text before its names are searched (a file called `acme-logo.svg` in the repo `acme-web`
    /// is a path the operator asked to see). Never cut before the secret search.
    shown_paths: Vec<String>,
}

/// The words a bundle holds by design.
fn engine_words() -> &'static BTreeSet<String> {
    static WORDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let mut w: BTreeSet<String> = vocab().iter().cloned().collect();
        let mut add = |s: &str| w.extend(words(s));
        for name in DecisionTable::standard().field_names() {
            add(&name);
        }
        for list in enums::ALL {
            for v in *list {
                add(v);
            }
        }
        for extra in super::BUNDLE_WORDS {
            add(extra);
        }
        w
    })
}

/// The lower-cased alphanumeric words of `s`.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn searchable(word: &str) -> bool {
    word.chars().count() >= MIN_SEGMENT_CHARS
        && !word.chars().all(|c| c.is_ascii_digit())
        && !engine_words().contains(word)
}

impl LeakInputs {
    /// Gather from the raw inputs, the run's repos, the operator's home folder name and the
    /// paths the walk met. `engine_terms` are engine-made values the bundle shows as they are
    /// (the shipped rule ids, the engine version): their words are not searched for.
    pub fn collect(
        inputs: &[&Value],
        repos: &[RepoInfo],
        home_folder: Option<&str>,
        seen_paths: &BTreeSet<String>,
        named_paths: bool,
        engine_terms: &BTreeSet<String>,
        shown_paths: &BTreeSet<String>,
    ) -> Self {
        let mut me = Self::default();
        if named_paths {
            me.shown_paths = shown_paths.iter().map(|p| p.to_lowercase()).collect();
            // Longest first, so a path is cut whole before a shorter one inside it.
            me.shown_paths.sort_by_key(|p| std::cmp::Reverse(p.len()));
        }
        let engine: BTreeSet<String> = engine_terms.iter().flat_map(|t| words(t)).collect();
        let searchable = |w: &String| searchable(w) && !engine.contains(w);
        let mut names: Vec<&str> = Vec::new();
        for r in repos {
            names.push(&r.name);
            names.push(r.folder_name());
        }
        if let Some(h) = home_folder {
            names.push(h);
        }
        for name in names {
            let lower = name.trim().to_lowercase();
            let parts = words(&lower);
            if parts.len() > 1 && lower.chars().count() >= MIN_SEGMENT_CHARS {
                me.names.insert(lower.clone());
            }
            me.name_words.extend(parts.into_iter().filter(searchable));
        }
        if !named_paths {
            let mut paths: BTreeSet<String> = seen_paths.clone();
            for r in repos {
                paths.insert(r.dir.clone());
            }
            for input in inputs {
                strings_of(input, &mut |s| {
                    for token in s.split_whitespace() {
                        if looks_like_path(token) {
                            paths.insert(token.to_string());
                        }
                    }
                });
            }
            for p in paths {
                for segment in p.split(['/', '\\']) {
                    // A segment's words cover its stem: `charge.ts` gives `charge` and `ts`.
                    me.segment_words
                        .extend(words(segment).into_iter().filter(searchable));
                }
            }
        }
        me
    }

    fn hit(&self, text: &str) -> Option<LeakKind> {
        if !camerata_secrets::detect_secrets(text).is_empty() {
            return Some(LeakKind::Secret);
        }
        let mut lower = text.to_lowercase();
        for shown in &self.shown_paths {
            if lower.contains(shown.as_str()) {
                lower = lower.replace(shown.as_str(), " ");
            }
        }
        if self.names.iter().any(|n| lower.contains(n.as_str())) {
            return Some(LeakKind::RepoName);
        }
        let ws = words(&lower);
        if ws.iter().any(|w| self.name_words.contains(w)) {
            return Some(LeakKind::RepoName);
        }
        if ws.iter().any(|w| self.segment_words.contains(w)) {
            return Some(LeakKind::PathSegment);
        }
        None
    }
}

fn strings_of(v: &Value, f: &mut impl FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|v| strings_of(v, f)),
        Value::Object(m) => {
            for (k, v) in m {
                f(k);
                strings_of(v, f);
            }
        }
        _ => {}
    }
}

/// Search `bundle` (every key and every string) and `preview` (every line). The first hit
/// refuses.
pub fn leak_check(bundle: &Value, preview: &str, inputs: &LeakInputs) -> Result<(), LeakRefusal> {
    check_value("", bundle, inputs)?;
    for (i, line) in preview.lines().enumerate() {
        if let Some(kind) = inputs.hit(line) {
            return Err(LeakRefusal {
                key_path: format!("preview line {}", i + 1),
                kind,
            });
        }
    }
    // No pass over the serialized JSON as one text: every string and every key is searched on
    // its own above, which is what a reader of the bundle sees, while the serialized text
    // joins the bundle's own field names into `"name":value` pairs that read as named secret
    // literals (`"secret_searched":3`).
    Ok(())
}

fn check_value(path: &str, v: &Value, inputs: &LeakInputs) -> Result<(), LeakRefusal> {
    match v {
        Value::String(s) => match inputs.hit(s) {
            Some(kind) => Err(LeakRefusal {
                key_path: path.to_string(),
                kind,
            }),
            None => Ok(()),
        },
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                check_value(&format!("{path}[{i}]"), v, inputs)?;
            }
            Ok(())
        }
        Value::Object(m) => {
            for (k, v) in m {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if let Some(kind) = inputs.hit(k) {
                    // The parent path only: the key is what leaked, and it must not be printed.
                    let parent = if path.is_empty() {
                        "<key>".to_string()
                    } else {
                        format!("{path}.<key>")
                    };
                    return Err(LeakRefusal {
                        key_path: format!("{parent} (the key)"),
                        kind,
                    });
                }
                check_value(&child, v, inputs)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
