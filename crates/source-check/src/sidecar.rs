//! The sidecar: what the last run learned about each cited URL. It lives beside the corpus, never
//! inside a rule file (the corpus is a protected area and nothing here edits a rule).
//!
//! It is also what a later export reads, so it is self-sufficient: the last definitive link state,
//! the last time the URL was checked, and (for a URL with an anchor) the section hash, a capped
//! excerpt of the text for the next diff, and the review state of a changed section.
//!
//! A changed section is never silently re-baselined. The old baseline stays, the new text waits
//! in [`SourceRecord::pending`], and the section is reported as changed on every run until a
//! person accepts it ([`SourceHashes::accept`]), which records who and when.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Bumped when the file's shape changes incompatibly.
pub const SIDECAR_SCHEMA_VERSION: u32 = 1;
/// Longest section text kept for the next diff. The hash always covers the full text.
pub const EXCERPT_CAP_CHARS: usize = 12_000;

/// The definitive health of a cited URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// Answered 2xx directly.
    Ok,
    /// Answered 2xx after a redirect.
    Redirected,
    /// The server said the page is gone.
    Dead,
    /// No usable answer this run (never stored: an unreachable run leaves the record alone).
    Unreachable,
}

/// What the last definitive check recorded about one cited URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Date (YYYY-MM-DD) of the last check that got a definitive answer.
    pub last_checked: String,
    /// The link state at that check.
    pub link: LinkState,
    /// Where a redirect ended up, when `link` is `redirected`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirected_to: Option<String>,
    /// `sha256:<hex>` of the cited section's normalised text, for a URL with an anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_hash: Option<String>,
    /// The section's normalised text, capped, kept so the next change can be shown as a diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_excerpt: Option<String>,
    /// A change seen since the baseline that nobody has accepted yet. While this is set the
    /// baseline above is the last ACCEPTED text and the section stays flagged on every run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingChange>,
    /// Who accepted the most recent change, and when. `None` until a change is first accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<Acceptance>,
}

/// New section text that differs from the accepted baseline and has not been reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingChange {
    /// `sha256:<hex>` of the new normalised text.
    pub hash: String,
    /// The new text, capped.
    pub excerpt: String,
    /// Date (YYYY-MM-DD) of the first run that saw the section differ from the baseline.
    pub first_seen: String,
}

/// A recorded acceptance of a changed section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    /// Date (YYYY-MM-DD) the change was accepted.
    pub on: String,
    /// Who accepted it, as given to the accept command.
    pub by: String,
}

/// Which changed sections to accept.
#[derive(Debug, Clone, Copy)]
pub enum AcceptTarget<'a> {
    /// Exactly these cited URLs (anchor included). Each must have a pending change.
    Urls(&'a [String]),
    /// Every section with a pending change (for a report that was reviewed in full).
    All,
}

/// The whole sidecar file: records keyed by the URL exactly as a rule cites it (fragment kept).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHashes {
    /// See [`SIDECAR_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// One record per cited web URL, sorted by URL so the file diffs cleanly.
    pub entries: BTreeMap<String, SourceRecord>,
}

impl Default for SourceHashes {
    fn default() -> Self {
        Self {
            schema_version: SIDECAR_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl SourceHashes {
    /// Read the sidecar at `path`. A missing file is the first run and yields an empty sidecar; a
    /// file that exists but does not parse is an error (never silently reset, which would erase
    /// every baseline).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let parsed: Self = serde_json::from_str(&text)
                    .with_context(|| format!("parsing sidecar {}", path.display()))?;
                if parsed.schema_version != SIDECAR_SCHEMA_VERSION {
                    anyhow::bail!(
                        "sidecar {} has schema_version {} but this build reads {}",
                        path.display(),
                        parsed.schema_version,
                        SIDECAR_SCHEMA_VERSION
                    );
                }
                Ok(parsed)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading sidecar {}", path.display())),
        }
    }

    /// Accept pending changes: the pending text becomes the baseline, the pending marker clears,
    /// and `by` / `today` are recorded. Returns the accepted URLs. For [`AcceptTarget::Urls`] it
    /// is all or nothing: if any URL is unknown or has no pending change, nothing is accepted and
    /// the error names each one.
    pub fn accept(
        &mut self,
        target: AcceptTarget<'_>,
        today: &str,
        by: &str,
    ) -> Result<Vec<String>> {
        let urls: Vec<String> = match target {
            AcceptTarget::All => self
                .entries
                .iter()
                .filter(|(_, r)| r.pending.is_some())
                .map(|(u, _)| u.clone())
                .collect(),
            AcceptTarget::Urls(list) => {
                let bad: Vec<&str> = list
                    .iter()
                    .filter(|u| self.entries.get(*u).is_none_or(|r| r.pending.is_none()))
                    .map(String::as_str)
                    .collect();
                if !bad.is_empty() {
                    anyhow::bail!(
                        "nothing to accept for: {} (not in the sidecar, or no unreviewed change). \
                         Nothing was accepted.",
                        bad.join(", ")
                    );
                }
                list.to_vec()
            }
        };
        for url in &urls {
            if let Some(record) = self.entries.get_mut(url) {
                if let Some(pending) = record.pending.take() {
                    record.section_hash = Some(pending.hash);
                    record.section_excerpt = Some(pending.excerpt);
                    record.accepted = Some(Acceptance {
                        on: today.to_string(),
                        by: by.to_string(),
                    });
                }
            }
        }
        Ok(urls)
    }

    /// Write the sidecar to `path` as pretty JSON with a trailing newline.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(path, text).with_context(|| format!("writing sidecar {}", path.display()))
    }
}

/// `crates/rules/CORPUS_SOURCE_HASHES.json`, the sidecar beside the corpus and its other
/// generated records (`CORPUS_FRESHNESS.toml`, `CORPUS_LOCK.toml`).
pub fn sidecar_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rules/CORPUS_SOURCE_HASHES.json"
    ))
}

/// `text` cut to at most [`EXCERPT_CAP_CHARS`] characters on a character boundary.
pub fn cap_excerpt(text: &str) -> String {
    text.chars().take(EXCERPT_CAP_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SourceHashes {
        let mut s = SourceHashes::default();
        s.entries.insert(
            "https://example.com/a#sec".to_string(),
            SourceRecord {
                last_checked: "2026-10-27".to_string(),
                link: LinkState::Redirected,
                redirected_to: Some("https://example.com/b".to_string()),
                section_hash: Some("sha256:abc".to_string()),
                section_excerpt: Some("Some text.".to_string()),
                pending: Some(PendingChange {
                    hash: "sha256:def".to_string(),
                    excerpt: "Some new text.".to_string(),
                    first_seen: "2026-10-01".to_string(),
                }),
                accepted: Some(Acceptance {
                    on: "2026-09-01".to_string(),
                    by: "zach".to_string(),
                }),
            },
        );
        s.entries.insert(
            "https://example.com/plain".to_string(),
            SourceRecord {
                last_checked: "2026-10-27".to_string(),
                link: LinkState::Ok,
                redirected_to: None,
                section_hash: None,
                section_excerpt: None,
                pending: None,
                accepted: None,
            },
        );
        s
    }

    #[test]
    fn round_trips_through_disk_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        let original = sample();
        original.save(&path).expect("save");
        assert_eq!(SourceHashes::load(&path).expect("load"), original);
    }

    #[test]
    fn saved_form_is_stable_and_newline_terminated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        sample().save(&path).expect("save");
        let first = std::fs::read_to_string(&path).expect("read");
        SourceHashes::load(&path)
            .expect("load")
            .save(&path)
            .expect("resave");
        assert_eq!(first, std::fs::read_to_string(&path).expect("read"));
        assert!(first.ends_with("}\n"));
    }

    #[test]
    fn accept_promotes_pending_to_baseline_and_records_who_and_when() {
        let mut s = sample();
        let url = "https://example.com/a#sec".to_string();
        let done = s
            .accept(
                AcceptTarget::Urls(std::slice::from_ref(&url)),
                "2026-11-02",
                "pat",
            )
            .expect("accept");
        assert_eq!(done, vec![url.clone()]);
        let r = &s.entries[&url];
        assert_eq!(r.section_hash.as_deref(), Some("sha256:def"));
        assert_eq!(r.section_excerpt.as_deref(), Some("Some new text."));
        assert!(r.pending.is_none());
        assert_eq!(
            r.accepted,
            Some(Acceptance {
                on: "2026-11-02".to_string(),
                by: "pat".to_string()
            })
        );
    }

    #[test]
    fn accept_by_url_is_all_or_nothing() {
        let mut s = sample();
        let before = s.clone();
        let err = s
            .accept(
                AcceptTarget::Urls(&[
                    "https://example.com/a#sec".to_string(),
                    "https://example.com/plain".to_string(),
                ]),
                "2026-11-02",
                "pat",
            )
            .expect_err("plain has no pending change");
        assert!(err.to_string().contains("https://example.com/plain"));
        assert_eq!(s, before, "a failed accept changes nothing");
    }

    #[test]
    fn accept_all_takes_every_pending_change_and_nothing_else() {
        let mut s = sample();
        let done = s
            .accept(AcceptTarget::All, "2026-11-02", "pat")
            .expect("accept");
        assert_eq!(done, vec!["https://example.com/a#sec".to_string()]);
        assert!(s.entries["https://example.com/plain"].accepted.is_none());
        // Nothing pending any more: accept-all is now a no-op, not an error.
        assert!(s
            .accept(AcceptTarget::All, "2026-11-03", "pat")
            .expect("accept")
            .is_empty());
    }

    #[test]
    fn missing_file_is_an_empty_first_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loaded = SourceHashes::load(&dir.path().join("nope.json")).expect("load");
        assert!(loaded.entries.is_empty());
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_silent_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        std::fs::write(&path, "{ not json").expect("write");
        assert!(SourceHashes::load(&path).is_err());
    }

    #[test]
    fn excerpt_cap_respects_char_boundaries() {
        let long = "é".repeat(EXCERPT_CAP_CHARS + 5);
        assert_eq!(cap_excerpt(&long).chars().count(), EXCERPT_CAP_CHARS);
    }
}
