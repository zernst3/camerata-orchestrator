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

/// A project id as a file-name part: ASCII letters, digits, `_` and `-` as they are, anything
/// else (a `.`, a separator) as `%XX` per byte, so no id can name another project's file.
fn file_part(project_id: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for b in project_id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
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
    write_atomic(path, json)
}

/// Write `json` to `path` through a temp file and a rename. A file that is read-only is not
/// replaced (a rename would replace it without asking; a plain write would not).
fn write_atomic(path: &std::path::Path, json: &str) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the draft file is read-only",
        ));
    }
    let tmp = sibling(path, ".tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Persists per-project onboarding drafts, ONE FILE PER PROJECT, next to the legacy shared path
/// `<data_dir>/onboarding-draft.json`: `onboarding-draft-<project>.json`, each a
/// `{ project_id: draft, "_cleared_at": { project_id: at } }` map holding that project's keys
/// only, with its own `.bak`, `.tmp` and `.unreadable-<secs>` siblings. So a project's deletion
/// removes exactly its files. An in-memory mirror serves a session without a data dir. `Clone`
/// is a shallow handle (shared state) so it can live in the axum `AppState`.
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

    /// Persist to (and rehydrate from) the per-project files next to `path` (the legacy shared
    /// draft file's name, which also fixes the folder and the file-name stem). A file this build
    /// cannot read (not UTF-8, not a draft map such as the old bare-draft shape, or a read error)
    /// is never dropped: it is copied aside to a timestamped `.unreadable-<seconds>` file, the
    /// operator is told, and that file's drafts are not restored.
    ///
    /// A shared file from before the split is migrated here, once: each project's draft and
    /// discard stamp go to its own file (a project that already has its own file keeps that one),
    /// then the shared file is renamed to `<path>.shared-backup`, never deleted. A shared file
    /// that cannot be read is moved to its `.unreadable-<seconds>` name instead, so a second
    /// start finds nothing to redo.
    pub fn at(path: PathBuf, live_ids: &[String]) -> Self {
        let _ = live_ids;
        let mut notice = None;
        let mut mem: HashMap<String, serde_json::Value> = HashMap::new();
        let store = Self {
            path: Some(Arc::new(path.clone())),
            mem: Arc::new(Mutex::new(HashMap::new())),
            notice: Arc::new(Mutex::new(None)),
        };

        for file in store.project_files_on_disk() {
            let (map, told) = Self::load_file(&file, false);
            notice = notice.or(told);
            if let Some(map) = map {
                Self::merge(&mut mem, map, false);
            }
        }

        if path.exists() {
            let (map, told) = Self::load_file(&path, true);
            notice = notice.or(told);
            if let Some(map) = map {
                let mut ids: Vec<String> = map
                    .keys()
                    .filter(|k| k.as_str() != CLEARED_KEY)
                    .cloned()
                    .collect();
                if let Some(o) = map.get(CLEARED_KEY).and_then(|c| c.as_object()) {
                    ids.extend(o.keys().cloned());
                }
                ids.sort();
                ids.dedup();
                let had_file: Vec<bool> = ids
                    .iter()
                    .map(|id| store.project_file(id).is_some_and(|f| f.exists()))
                    .collect();
                Self::merge(&mut mem, map, true);
                if let Ok(mut m) = store.mem.lock() {
                    *m = mem.clone();
                }
                let all_written = ids
                    .iter()
                    .zip(&had_file)
                    .all(|(id, had)| *had || store.write_project(id).is_ok());
                if all_written {
                    let _ = std::fs::rename(&path, Self::backup_name(&path));
                }
            }
        }

        if let Ok(mut m) = store.mem.lock() {
            *m = mem;
        }
        if let Ok(mut n) = store.notice.lock() {
            *n = notice;
        }
        store
    }

    /// Where a shared file is kept once its drafts have their own files.
    fn backup_name(shared: &std::path::Path) -> PathBuf {
        let first = sibling(shared, ".shared-backup");
        if !first.exists() {
            return first;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        sibling(shared, &format!(".shared-backup-{secs}"))
    }

    /// Read one draft file. `None` for a file that is not there. A file this build cannot read is
    /// set aside (`move_aside`: moved to its `.unreadable-<secs>` name, else copied there) and
    /// the notice says so.
    fn load_file(
        path: &std::path::Path,
        move_aside: bool,
    ) -> (Option<HashMap<String, serde_json::Value>>, Option<String>) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let aside = sibling(path, &format!(".unreadable-{secs}"));
        match std::fs::read(path) {
            Ok(bytes) => match std::str::from_utf8(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    serde_json::from_str::<HashMap<String, serde_json::Value>>(s)
                        .map_err(|e| e.to_string())
                }) {
                Ok(map) => (Some(map), None),
                Err(why) => {
                    let kept = if move_aside && std::fs::rename(path, &aside).is_ok() {
                        Ok(())
                    } else {
                        std::fs::write(&aside, &bytes)
                    };
                    (None, Some(Self::aside_notice(path, &aside, &why, kept)))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(e) => {
                // The file exists but cannot be read (permissions, I/O error): nothing is
                // written over it until the operator is told.
                let kept = std::fs::copy(path, &aside).map(|_| ());
                (
                    None,
                    Some(Self::aside_notice(path, &aside, &e.to_string(), kept)),
                )
            }
        }
    }

    /// Fold `map` into `into`. With `keep_existing`, what `into` already holds for a project wins.
    fn merge(
        into: &mut HashMap<String, serde_json::Value>,
        map: HashMap<String, serde_json::Value>,
        keep_existing: bool,
    ) {
        for (key, value) in map {
            if key == CLEARED_KEY {
                let Some(stamps) = value.as_object() else {
                    continue;
                };
                for (id, at) in stamps {
                    let held = into.get(CLEARED_KEY).and_then(|c| c.get(id)).is_some();
                    if !(keep_existing && held) {
                        Self::stamp_cleared(into, id, at.as_str().unwrap_or_default());
                    }
                }
            } else if !(keep_existing && into.contains_key(&key)) {
                into.insert(key, value);
            }
        }
    }

    /// The file `project_id`'s draft lives in: `onboarding-draft-<project>.json` beside the shared
    /// path. `None` for a store with no folder.
    pub fn project_file(&self, project_id: &str) -> Option<PathBuf> {
        let shared = self.path.as_ref()?;
        let stem = shared.file_stem()?.to_string_lossy().to_string();
        let name = match shared.extension() {
            Some(ext) => format!("{stem}-{}.{}", file_part(project_id), ext.to_string_lossy()),
            None => format!("{stem}-{}", file_part(project_id)),
        };
        Some(shared.with_file_name(name))
    }

    /// Every per-project draft file in the folder (not their `.bak`, `.tmp` or set-aside copies).
    fn project_files_on_disk(&self) -> Vec<PathBuf> {
        let Some(shared) = &self.path else {
            return Vec::new();
        };
        let Some(dir) = shared.parent() else {
            return Vec::new();
        };
        let stem = shared
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = shared
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let (head, tail) = (format!("{stem}-"), ext);
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && p.file_name().is_some_and(|n| {
                                let n = n.to_string_lossy();
                                n.starts_with(&head) && n.ends_with(&tail)
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Everything on disk that is `project_id`'s alone: its draft file and that file's `.bak`,
    /// `.tmp` and set-aside copies. Never another project's, never the shared file.
    pub fn files_of(&self, project_id: &str) -> Vec<PathBuf> {
        let Some(file) = self.project_file(project_id) else {
            return Vec::new();
        };
        let Some(dir) = file.parent() else {
            return Vec::new();
        };
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().is_some_and(|n| {
                            let n = n.to_string_lossy();
                            n == name
                                || n == format!("{name}.bak")
                                || n == format!("{name}.tmp")
                                || n.starts_with(&format!("{name}.unreadable-"))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
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
    pub fn take_unreadable_notice(&self, _project_id: &str) -> Option<String> {
        self.notice.lock().ok().and_then(|mut n| n.take())
    }

    /// Write `project_id`'s file from the in-memory map: its draft and its discard stamp, and
    /// nothing of any other project. No file is written for a project the map holds nothing of.
    fn write_project(&self, project_id: &str) -> std::io::Result<()> {
        let Some(file) = self.project_file(project_id) else {
            return Ok(());
        };
        let json = {
            let m = self
                .mem
                .lock()
                .map_err(|_| std::io::Error::other("draft store mutex poisoned"))?;
            let mut only: HashMap<String, serde_json::Value> = HashMap::new();
            if let Some(d) = m.get(project_id) {
                only.insert(project_id.to_string(), d.clone());
            }
            if let Some(at) = m.get(CLEARED_KEY).and_then(|c| c.get(project_id)) {
                only.insert(
                    CLEARED_KEY.to_string(),
                    serde_json::json!({ project_id: at }),
                );
            }
            if only.is_empty() {
                return Ok(());
            }
            serde_json::to_string(&only).map_err(std::io::Error::other)?
        };
        write_with_backup(&file, &json)
    }

    /// Best-effort flush of `project_id`'s file; the in-memory mirror is authoritative. The
    /// unreadable-file notice is dropped only once a write has succeeded.
    fn flush(&self, project_id: &str) {
        if self.path.is_none() {
            return;
        }
        if self.write_project(project_id).is_ok() {
            if let Ok(mut n) = self.notice.lock() {
                *n = None;
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

    /// Drop `project_id`'s draft and its discard stamp, for the project's deletion: every file
    /// that is the project's alone ([`Self::files_of`]: the draft file, its `.bak`, `.tmp` and
    /// set-aside copies) is removed, and no other project's file is touched. The draft file goes
    /// last, and memory changes only after every removal succeeded, so a failure part way leaves
    /// disk and memory agreeing the draft is still there, and a retry finishes the job.
    pub fn clear_checked(&self, project_id: &str) -> std::io::Result<()> {
        let mut m = self
            .mem
            .lock()
            .map_err(|_| std::io::Error::other("draft store mutex poisoned"))?;
        let main = self.project_file(project_id);
        let mut files = self.files_of(project_id);
        files.sort_by_key(|f| Some(f) == main.as_ref());
        for f in files {
            match std::fs::remove_file(&f) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        m.remove(project_id);
        if let Some(o) = m.get_mut(CLEARED_KEY).and_then(|v| v.as_object_mut()) {
            o.remove(project_id);
        }
        Ok(())
    }

    /// Whether the store holds anything of `project_id`: its draft or its discard stamp.
    pub fn holds_project(&self, project_id: &str) -> bool {
        self.mem.lock().is_ok_and(|m| {
            m.contains_key(project_id)
                || m.get(CLEARED_KEY)
                    .and_then(|c| c.as_object())
                    .is_some_and(|o| o.contains_key(project_id))
        })
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
        self.flush(project_id);
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
            self.flush(project_id);
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
        self.flush(project_id);
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
