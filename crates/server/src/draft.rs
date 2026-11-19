//! Onboarding drafts: the in-flight onboarding state (scan + audit + per-repo rule
//! selection + triage dispositions) persisted to disk so a brownfield onboarding survives
//! an app restart — the architect does NOT have to re-scan to keep testing the post-scan
//! features.
//!
//! Drafts are keyed by PROJECT id: each project can have its own in-progress onboarding,
//! so opening project B never clobbers project A's draft (the UI shows a "continue or start
//! over" prompt when a project with a draft is opened). Each draft is an opaque JSON blob
//! whose shape the UI owns. A crash mid-SCAN is still unrecoverable (the scan hasn't
//! produced a draft yet); everything after the scan is sticky.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The reserved key in the draft map that holds, per project, when its draft was last discarded
/// (Start over, a new scan, completion). A finished inspection older than that belongs to the
/// discarded draft and is never adopted into a new one. Project ids never take this form.
const CLEARED_KEY: &str = "_cleared_at";

/// `path` with `suffix` appended to its file name (`onboarding-draft.json` -> `.json.bak`).
fn sibling(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Whether `bytes` are a draft map this build can read.
fn parses_as_map(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .is_some_and(|s| serde_json::from_str::<HashMap<String, serde_json::Value>>(s).is_ok())
}

/// Write `json` to `path` through a temp file and a rename, so a crash mid-write leaves the old
/// file whole, never half of the new one. The file's previous content is first kept as
/// `<path>.bak` (one previous copy, replaced on each write), but only when that content is a
/// draft map this build can read: an unreadable file is never copied over the last good `.bak`.
fn write_with_backup(path: &std::path::Path, json: &str) -> std::io::Result<()> {
    if let Ok(current) = std::fs::read(path) {
        if parses_as_map(&current) {
            let _ = std::fs::write(sibling(path, ".bak"), &current);
        }
    }
    let tmp = sibling(path, ".tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Persists per-project onboarding drafts to `<data_dir>/camerata/onboarding-draft.json`
/// (a `{ project_id: draft }` map), with an in-memory mirror so a session without a
/// resolvable data dir still works in-run. `Clone` is a shallow handle (shared state) so
/// it can live in the axum `AppState`.
#[derive(Clone, Default)]
pub struct DraftStore {
    path: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    /// Set when the file on disk could not be read at start and was set aside; the operator is
    /// told once, on the next read of the draft ([`Self::take_unreadable_notice`]).
    notice: Arc<Mutex<Option<String>>>,
}

impl DraftStore {
    /// In-memory only (tests / no data dir).
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`. A file this build cannot read (not UTF-8, not a
    /// draft map such as the old bare-draft shape, or a read error) is never dropped: it is
    /// copied aside to a timestamped `.unreadable-<seconds>` file, the operator is told, and the
    /// store starts empty.
    pub fn at(path: PathBuf) -> Self {
        let mut notice = None;
        let mem = match std::fs::read(&path) {
            Ok(bytes) => match std::str::from_utf8(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    serde_json::from_str::<HashMap<String, serde_json::Value>>(s)
                        .map_err(|e| e.to_string())
                }) {
                Ok(map) => map,
                Err(why) => {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let aside = sibling(&path, &format!(".unreadable-{secs}"));
                    notice = Some(Self::aside_notice(
                        &path,
                        &aside,
                        &why,
                        std::fs::write(&aside, &bytes),
                    ));
                    HashMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                // The file exists but cannot be read (permissions, I/O error): nothing is
                // written over it until the operator is told.
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let aside = sibling(&path, &format!(".unreadable-{secs}"));
                notice = Some(Self::aside_notice(
                    &path,
                    &aside,
                    &e.to_string(),
                    std::fs::copy(&path, &aside).map(|_| ()),
                ));
                HashMap::new()
            }
        };
        Self {
            path: Some(Arc::new(path)),
            mem: Arc::new(Mutex::new(mem)),
            notice: Arc::new(Mutex::new(notice)),
        }
    }

    /// The operator-facing words for a draft file that was set aside, also printed to stderr.
    fn aside_notice(
        path: &std::path::Path,
        aside: &std::path::Path,
        why: &str,
        copied: std::io::Result<()>,
    ) -> String {
        eprintln!(
            "[camerata-server] {} could not be read ({why}); set aside at {}",
            path.display(),
            aside.display()
        );
        let kept = match copied {
            Ok(()) => format!("A copy was kept untouched at {}.", aside.display()),
            Err(e) => format!(
                "A copy could not be made ({e}); the file itself at {} was left as it was.",
                path.display()
            ),
        };
        format!(
            "The saved onboarding draft file could not be read, so no earlier review was restored. \
             {kept} The draft saved before the last save, when there was one, is the recovery \
             copy at {}. Nothing was deleted.",
            sibling(path, ".bak").display()
        )
    }

    /// Why no draft could be restored, when the draft file on disk was unreadable at start and
    /// nothing has been saved since. Taking it clears it: the operator is told once, and what
    /// they do next (start again, save) is not blocked by a message that keeps coming back.
    pub fn take_unreadable_notice(&self) -> Option<String> {
        self.notice.lock().ok().and_then(|mut n| n.take())
    }

    /// Best-effort flush of the whole map to disk; the in-memory mirror is authoritative. The
    /// unreadable-file notice is dropped only once a write has succeeded.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(map) = self.mem.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*map) {
            if write_with_backup(p.as_ref().as_path(), &s).is_ok() {
                if let Ok(mut n) = self.notice.lock() {
                    *n = None;
                }
            }
        }
    }

    /// The saved draft for `project_id`, or None when nothing is in progress for it.
    pub fn load(&self, project_id: &str) -> Option<serde_json::Value> {
        self.mem
            .lock()
            .ok()
            .and_then(|m| m.get(project_id).cloned())
    }

    /// Record, inside `map`, that `project_id`'s draft was discarded at `at`.
    fn stamp_cleared(map: &mut HashMap<String, serde_json::Value>, project_id: &str, at: &str) {
        let entry = map
            .entry(CLEARED_KEY.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(o) = entry.as_object_mut() {
            o.insert(project_id.to_string(), serde_json::json!(at));
        }
    }

    /// Drop `project_id`'s draft and write the file; when the write fails nothing is dropped and
    /// the error is returned. The project's discard stamp goes too (this is the project's
    /// deletion, which must leave nothing of it behind).
    pub fn clear_checked(&self, project_id: &str) -> std::io::Result<()> {
        let mut m = self
            .mem
            .lock()
            .map_err(|_| std::io::Error::other("draft store mutex poisoned"))?;
        let mut kept = m.clone();
        kept.remove(project_id);
        if let Some(o) = kept.get_mut(CLEARED_KEY).and_then(|v| v.as_object_mut()) {
            o.remove(project_id);
        }
        if let Some(p) = &self.path {
            let json = serde_json::to_string(&kept).map_err(std::io::Error::other)?;
            write_with_backup(p.as_ref().as_path(), &json)?;
        }
        *m = kept;
        Ok(())
    }

    /// The size in bytes of `project_id`'s saved draft as stored (0 when there is none).
    pub fn stored_bytes(&self, project_id: &str) -> u64 {
        self.load(project_id)
            .and_then(|v| serde_json::to_vec(&v).ok())
            .map_or(0, |b| b.len() as u64)
    }

    /// Replace `project_id`'s draft.
    pub fn save(&self, project_id: &str, v: serde_json::Value) {
        if let Ok(mut m) = self.mem.lock() {
            m.insert(project_id.to_string(), v);
        }
        self.flush();
    }

    /// Put a finished run's report into `project_id`'s draft as its `audit`, so the review the
    /// run produced is reopenable even when no screen was mounted to receive it. `report` is
    /// built only when the run is going to be adopted. Adopted when ALL hold:
    /// - the draft exists and was scanned over exactly `repos` (same repos, same order);
    /// - the run finished after the draft was last discarded (a draft with no discard stamp,
    ///   saved before stamps existed, takes any run);
    /// - the draft has no audit, or its audit was itself adopted from an earlier run and this
    ///   run is newer. An audit the operator's own screen saved is never replaced.
    ///
    /// Returns whether the draft changed.
    pub fn adopt_audit(
        &self,
        project_id: &str,
        repos: &[String],
        finished_at: &str,
        report: impl FnOnce() -> serde_json::Value,
    ) -> bool {
        let changed = {
            let Ok(mut m) = self.mem.lock() else {
                return false;
            };
            let stamp = m
                .get(CLEARED_KEY)
                .and_then(|c| c.get(project_id))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(draft) = m.get_mut(project_id).and_then(|d| d.as_object_mut()) else {
                return false;
            };
            let same_scan = draft
                .get("scan")
                .and_then(|s| s.get("repos"))
                .and_then(|r| r.as_array())
                .is_some_and(|r| {
                    r.len() == repos.len()
                        && r.iter()
                            .zip(repos)
                            .all(|(a, b)| a.as_str() == Some(b.as_str()))
                });
            if !same_scan {
                return false;
            }
            if let Some(stamp) = stamp {
                match (instant(finished_at), instant(&stamp)) {
                    (Some(done), Some(cleared)) if done > cleared => {}
                    _ => return false,
                }
            }
            let audit = draft.get("audit").filter(|a| !a.is_null());
            if let Some(audit) = audit {
                let adopted = draft
                    .get("audit_adopted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let held_at = audit
                    .get("provenance")
                    .and_then(|p| p.get("finished_at"))
                    .and_then(|v| v.as_str())
                    .and_then(instant);
                let newer = match (instant(finished_at), held_at) {
                    (Some(done), Some(held)) => done > held,
                    (Some(_), None) => true,
                    _ => false,
                };
                if !(adopted && newer) {
                    return false;
                }
            }
            draft.insert("audit".to_string(), report());
            draft.insert("audit_adopted".to_string(), serde_json::json!(true));
            true
        };
        if changed {
            self.flush();
        }
        changed
    }

    /// Drop `project_id`'s draft (onboarding completed, or the architect started fresh), and
    /// remember when: an inspection that finished before now is not this project's next draft.
    pub fn clear(&self, project_id: &str) {
        self.clear_at(
            project_id,
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
    }

    /// [`Self::clear`] at a stated moment (RFC 3339).
    pub fn clear_at(&self, project_id: &str, at: &str) {
        if let Ok(mut m) = self.mem.lock() {
            m.remove(project_id);
            Self::stamp_cleared(&mut m, project_id, at);
        }
        self.flush();
    }
}

/// An RFC 3339 timestamp as an instant, whatever its offset or fraction.
fn instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_project_round_trip() {
        let store = DraftStore::new();
        assert!(store.load("p1").is_none());

        store.save("p1", serde_json::json!({"scan": 1}));
        store.save("p2", serde_json::json!({"scan": 2}));
        // Each project keeps its own draft; one does not clobber the other.
        assert_eq!(store.load("p1"), Some(serde_json::json!({"scan": 1})));
        assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 2})));

        // Clearing one leaves the other intact.
        store.clear("p1");
        assert!(store.load("p1").is_none());
        assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 2})));
    }
}
