//! The content-keyed verdict cache of the clone-candidate judge.
//!
//! A verdict is cached by the CONTENT of the question, not by where the code lives: the rule, the
//! judge model, the prompt schema version, the candidate's id (a hash of the family's normalized
//! token sequence and its length), the exact-mode differences between the copies, and the
//! rule-specific facts the judge was shown. A file that is moved or renamed hits the cache; a
//! family whose tokens, differences or names changed misses it. A judge model change or a prompt
//! schema bump changes every key, so the cache is invalidated wholesale by construction.
//!
//! `unsure` is NOT cached: it is retried next scan. The cache is persisted inside the project's
//! scan manifest (`ScanManifest::clone_verdicts`, `serde(default)` so `MANIFEST_VERSION` is not
//! bumped), in the app data directory, never in the client's repository.

use std::collections::BTreeMap;

use camerata_checks::clone_rules::{Candidate, RuleFacts};
use serde::{Deserialize, Serialize};

/// Bump when the prompt, the closed vocabulary or the judge input shape changes in a way that
/// should invalidate every cached verdict.
pub const PROMPT_SCHEMA_VERSION: u32 = 1;

/// The most verdicts one project keeps. Older ones (by `judged_at`) are dropped first, so the
/// manifest cannot grow without bound as code changes.
pub const MAX_CACHED_VERDICTS: usize = 4_000;

/// One cached, definite verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedVerdict {
    /// A verdict word from the rule's closed vocabulary, never `unsure`.
    pub verdict: String,
    /// The judge's concept phrase, empty when it gave none.
    #[serde(default)]
    pub concept: String,
    /// RFC 3339 time the verdict was obtained.
    #[serde(default)]
    pub judged_at: String,
}

/// Verdicts by content key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictCache {
    #[serde(default)]
    entries: BTreeMap<String, CachedVerdict>,
}

impl VerdictCache {
    pub fn get(&self, key: &str) -> Option<&CachedVerdict> {
        self.entries.get(key)
    }

    /// Record a definite verdict. `unsure` (and the empty word) is refused: an undecided
    /// question must be asked again next scan.
    pub fn insert(&mut self, key: String, verdict: CachedVerdict) -> bool {
        if verdict.verdict.is_empty() || verdict.verdict == "unsure" {
            return false;
        }
        self.entries.insert(key, verdict);
        true
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Keep at most `max` entries, dropping the oldest (by `judged_at`, then key).
    pub fn trim(&mut self, max: usize) {
        if self.entries.len() <= max {
            return;
        }
        let mut order: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|(k, v)| (v.judged_at.clone(), k.clone()))
            .collect();
        order.sort();
        let drop = self.entries.len() - max;
        for (_, key) in order.into_iter().take(drop) {
            self.entries.remove(&key);
        }
    }
}

/// The cache key of one candidate for one judge model.
pub fn cache_key(candidate: &Candidate, judge_model: &str) -> String {
    let mut material = String::new();
    let mut push = |part: &str| {
        material.push_str(part);
        material.push('\u{1f}');
    };
    push(candidate.rule.id());
    push(judge_model);
    push(&PROMPT_SCHEMA_VERSION.to_string());
    // The id carries the hash of the family's Structural token ids and their count.
    push(&candidate.id);
    // The differences are an unordered set of text pairs: which copy sorts first depends on the
    // paths, and a moved file must still hit.
    let mut differences: Vec<(&str, &str)> = candidate
        .facts
        .disagreements
        .iter()
        .map(|d| {
            if d.a <= d.b {
                (d.a.as_str(), d.b.as_str())
            } else {
                (d.b.as_str(), d.a.as_str())
            }
        })
        .collect();
    differences.sort_unstable();
    for (a, b) in differences {
        push(a);
        push(b);
    }
    match &candidate.facts.rule {
        RuleFacts::Logic { decision_tokens } => {
            // The decision tokens are read from the first copy, which depends on the paths when
            // the copies differ; the differences above already carry that. When the copies
            // agree every copy has the same tokens, so they are content.
            if candidate.facts.agree {
                for t in decision_tokens {
                    push(t);
                }
            }
        }
        RuleFacts::Naming { names, module } => {
            for n in names {
                push(n.name.as_deref().unwrap_or(""));
            }
            if module.is_some() {
                push("module");
            }
        }
        RuleFacts::Primitive {
            mechanism_marks,
            jsx_tags,
            hooks,
            differing_strings,
        } => {
            for t in mechanism_marks.iter().chain(jsx_tags).chain(hooks) {
                push(t);
            }
            let mut strings: Vec<(&str, &str)> = differing_strings
                .iter()
                .map(|d| {
                    if d.a <= d.b {
                        (d.a.as_str(), d.b.as_str())
                    } else {
                        (d.b.as_str(), d.a.as_str())
                    }
                })
                .collect();
            strings.sort_unstable();
            for (a, b) in strings {
                push(a);
                push(b);
            }
        }
    }
    format!("{:016x}", crate::suppression::fnv1a(&material))
}
