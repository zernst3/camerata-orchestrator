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
    /// The network policy refused the address (never stored, like `unreachable`).
    Refused,
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
    /// Date (YYYY-MM-DD) of the first run that found the page but not the cited anchor. Cleared
    /// when the anchor is found again. A vanished anchor lowers confidence like a dead link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_missing_since: Option<String>,
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

/// One section to accept, optionally pinned to the exact text the reviewer read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptSpec {
    /// The cited URL, anchor included.
    pub url: String,
    /// The pending hash the report printed. When set, the accept is refused if the sidecar's
    /// pending hash has moved on since (the page changed again after the review).
    pub expect_hash: Option<String>,
}

impl AcceptSpec {
    /// Accept whatever is pending for `url` right now.
    pub fn unpinned(url: &str) -> Self {
        Self {
            url: url.to_string(),
            expect_hash: None,
        }
    }

    /// Accept `url` only if its pending hash is still `hash`.
    pub fn pinned(url: &str, hash: &str) -> Self {
        Self {
            url: url.to_string(),
            expect_hash: Some(hash.to_string()),
        }
    }

    /// Parse `url#anchor` or `url#anchor=sha256:<hash>` (the form the report prints). The split
    /// is on the LAST `=sha256:`, so a URL with its own `=` in a query string is untouched.
    pub fn parse(spec: &str) -> Self {
        match spec.rfind("=sha256:") {
            Some(at) => Self::pinned(&spec[..at], &spec[at + 1..]),
            None => Self::unpinned(spec),
        }
    }
}

/// Which changed sections to accept.
#[derive(Debug, Clone, Copy)]
pub enum AcceptTarget<'a> {
    /// Exactly these sections. Each must have a pending change (and match its hash, if pinned).
    Urls(&'a [AcceptSpec]),
    /// Every section with a pending change (for a report that was reviewed in full).
    All,
}

/// One acceptance that was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedChange {
    /// The cited URL, anchor included.
    pub url: String,
    /// The hash of the text that became the baseline.
    pub hash: String,
    /// True when the caller did not pin a hash, so whatever was pending was taken on trust.
    pub unpinned: bool,
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
    /// and `by` / `today` are recorded. Returns what was accepted, hash included.
    ///
    /// For [`AcceptTarget::Urls`] it is all or nothing: if any section is unknown, has no pending
    /// change, or is pinned to a hash that is no longer the pending one (the page changed again
    /// since the report was read), nothing is accepted and the error names each problem.
    pub fn accept(
        &mut self,
        target: AcceptTarget<'_>,
        today: &str,
        by: &str,
    ) -> Result<Vec<AcceptedChange>> {
        let specs: Vec<AcceptSpec> = match target {
            AcceptTarget::All => self
                .entries
                .iter()
                .filter(|(_, r)| r.pending.is_some())
                .map(|(u, _)| AcceptSpec::unpinned(u))
                .collect(),
            AcceptTarget::Urls(list) => list.to_vec(),
        };
        let mut problems = Vec::new();
        for spec in &specs {
            match self.entries.get(&spec.url).and_then(|r| r.pending.as_ref()) {
                None => problems.push(format!(
                    "{}: not in the sidecar, or no unreviewed change",
                    spec.url
                )),
                Some(p) => {
                    if let Some(expected) = &spec.expect_hash {
                        if *expected != p.hash {
                            problems.push(format!(
                                "{}: the pending text is now {} but you reviewed {expected}; \
                                 the page changed again, read the new report first",
                                spec.url, p.hash
                            ));
                        }
                    }
                }
            }
        }
        if !problems.is_empty() {
            anyhow::bail!("nothing was accepted:\n  {}", problems.join("\n  "));
        }
        let mut done = Vec::with_capacity(specs.len());
        for spec in specs {
            if let Some(record) = self.entries.get_mut(&spec.url) {
                if let Some(pending) = record.pending.take() {
                    done.push(AcceptedChange {
                        url: spec.url.clone(),
                        hash: pending.hash.clone(),
                        unpinned: spec.expect_hash.is_none(),
                    });
                    record.section_hash = Some(pending.hash);
                    record.section_excerpt = Some(pending.excerpt);
                    record.accepted = Some(Acceptance {
                        on: today.to_string(),
                        by: by.to_string(),
                    });
                }
            }
        }
        Ok(done)
    }

    /// Write the sidecar to `path` as pretty JSON with a trailing newline, atomically: the text
    /// goes to a temp file in the same directory, is fsynced, then renamed over `path`, so a crash
    /// leaves either the old file or the new one, never a half-written baseline.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        write_atomic(path, text.as_bytes())
            .with_context(|| format!("writing sidecar {}", path.display()))
    }
}

/// Write `bytes` to `path` via a same-directory temp file, fsync, rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sidecar".to_string());
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    // Best effort: make the rename itself durable.
    if let Ok(dir_handle) = std::fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

/// An exclusive lock on the sidecar for the duration of a run or an accept, so two runs cannot
/// interleave their load-modify-save and silently overwrite each other. It is a file
/// `<sidecar>.lock` created with `create_new` and removed on drop. A second holder fails with a
/// message naming the file; a crash can leave a stale lock, which the message explains how to
/// clear.
#[derive(Debug)]
pub struct SidecarLock {
    path: PathBuf,
}

impl SidecarLock {
    /// Take the lock for the sidecar at `sidecar`.
    pub fn acquire(sidecar: &Path) -> Result<Self> {
        use std::io::Write as _;
        let mut name = sidecar
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".lock");
        let path = sidecar.with_file_name(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "pid {}", std::process::id());
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(&path).unwrap_or_default();
                anyhow::bail!(
                    "another check-sources run or accept holds {} ({}). Wait for it to finish; \
                     if no such run is active (it crashed), delete that file and retry.",
                    path.display(),
                    holder.trim()
                )
            }
            Err(e) => Err(e).with_context(|| format!("creating lock {}", path.display())),
        }
    }
}

impl Drop for SidecarLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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
                anchor_missing_since: Some("2026-10-20".to_string()),
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
                anchor_missing_since: None,
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
                AcceptTarget::Urls(&[AcceptSpec::unpinned(&url)]),
                "2026-11-02",
                "pat",
            )
            .expect("accept");
        assert_eq!(
            done,
            vec![AcceptedChange {
                url: url.clone(),
                hash: "sha256:def".to_string(),
                unpinned: true
            }]
        );
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
                    AcceptSpec::unpinned("https://example.com/a#sec"),
                    AcceptSpec::unpinned("https://example.com/plain"),
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
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].url, "https://example.com/a#sec");
        assert_eq!(done[0].hash, "sha256:def", "accept-all reports every hash");
        assert!(s.entries["https://example.com/plain"].accepted.is_none());
        // Nothing pending any more: accept-all is now a no-op, not an error.
        assert!(s
            .accept(AcceptTarget::All, "2026-11-03", "pat")
            .expect("accept")
            .is_empty());
    }

    #[test]
    fn a_pinned_accept_with_the_reviewed_hash_succeeds() {
        let mut s = sample();
        let url = "https://example.com/a#sec";
        let done = s
            .accept(
                AcceptTarget::Urls(&[AcceptSpec::pinned(url, "sha256:def")]),
                "2026-11-02",
                "pat",
            )
            .expect("accept");
        assert!(!done[0].unpinned);
    }

    #[test]
    fn a_stale_pinned_accept_is_refused_and_changes_nothing() {
        let mut s = sample();
        let before = s.clone();
        let err = s
            .accept(
                AcceptTarget::Urls(&[AcceptSpec::pinned(
                    "https://example.com/a#sec",
                    "sha256:reviewed-earlier",
                )]),
                "2026-11-02",
                "pat",
            )
            .expect_err("the page changed again after the review");
        let msg = err.to_string();
        assert!(
            msg.contains("sha256:def") && msg.contains("sha256:reviewed-earlier"),
            "{msg}"
        );
        assert_eq!(s, before);
    }

    #[test]
    fn accept_specs_parse_the_form_the_report_prints() {
        assert_eq!(
            AcceptSpec::parse("https://a.example/p#s=sha256:abc123"),
            AcceptSpec::pinned("https://a.example/p#s", "sha256:abc123")
        );
        assert_eq!(
            AcceptSpec::parse("https://a.example/p?x=1#s"),
            AcceptSpec::unpinned("https://a.example/p?x=1#s"),
            "an '=' in the query is not a pin"
        );
        assert_eq!(
            AcceptSpec::parse("https://a.example/p?x=sha256:zz#s=sha256:abc"),
            AcceptSpec::pinned("https://a.example/p?x=sha256:zz#s", "sha256:abc")
        );
    }

    #[test]
    fn save_replaces_the_file_atomically_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        sample().save(&path).expect("first save");
        SourceHashes::default().save(&path).expect("second save");
        assert!(SourceHashes::load(&path).expect("load").entries.is_empty());
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["hashes.json"], "no temp file left behind");
    }

    #[test]
    fn a_failed_save_leaves_the_old_file_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        sample().save(&path).expect("save");
        let before = std::fs::read_to_string(&path).expect("read");
        // A directory squatting on the temp name makes the write fail before the rename.
        let tmp = dir
            .path()
            .join(format!(".hashes.json.tmp.{}", std::process::id()));
        std::fs::create_dir(&tmp).expect("squat");
        assert!(SourceHashes::default().save(&path).is_err());
        assert_eq!(before, std::fs::read_to_string(&path).expect("read"));
    }

    #[test]
    fn a_second_holder_of_the_lock_fails_clearly_until_the_first_drops() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        let first = SidecarLock::acquire(&path).expect("first lock");
        let err = SidecarLock::acquire(&path).expect_err("second lock");
        let msg = err.to_string();
        assert!(
            msg.contains("hashes.json.lock") && msg.contains("another"),
            "{msg}"
        );
        drop(first);
        let again = SidecarLock::acquire(&path).expect("lock free again");
        drop(again);
        assert!(!dir.path().join("hashes.json.lock").exists());
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
