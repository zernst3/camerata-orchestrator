//! The rule library a run's inspection loaded, kept with the run (#560).
//!
//! Only the inspection is pinned to an engine build. Triage and export can run on any newer
//! build, but the rules in a client package must be exactly the rules the inspection ran with.
//! So the run keeps the parsed rule set it loaded, and every export and review screen reads the
//! rules from here, never from the live library (`camerata_rules::corpus_path()`).
//!
//! # Layout
//!
//! `<run dir>/rules/sha256-<hex>.json`, content addressed: the file is named by the rule digest
//! the run pinned ([`super::engine_pin::EnginePin::corpus_version`], which is
//! [`camerata_rules::corpus_version`] over the parsed rules). The file holds
//! `{ kind, format, digest, rules }`, the rules in load order in their serde form.
//!
//! # Reading back
//!
//! [`decode`] refuses, with a named [`RefusalKind`], unless every check passes:
//!
//! * the file parses, names [`SNAPSHOT_KIND`] and a format this build reads ([`SNAPSHOT_FORMAT`]);
//! * every rule reads, and reading drops nothing: each rule serialized again holds every field
//!   and value the file holds, so a field this build does not know refuses the export instead of
//!   vanishing from it (a field this build added since, with its default, is allowed);
//! * the digest recomputed over the rules read equals the digest the run pinned.
//!
//! The digest is recomputed by the running build, so a renderer, wording or format fix in a newer
//! build applies to an older run while the rule content stays the inspection's.
//!
//! The write is shared: [`super::RunStore::create`] writes it for the command line and the app
//! alike, from the same rule set the pin's digest was computed over.

use std::path::{Path, PathBuf};

use camerata_rules::{Rule, RuleSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The folder inside a run's folder that holds its rule snapshot.
pub const SNAPSHOT_DIR: &str = "rules";

/// The `kind` every snapshot file names.
pub const SNAPSHOT_KIND: &str = "camerata-rule-snapshot";

/// The snapshot layout this build writes and reads. A file of another format is refused.
pub const SNAPSHOT_FORMAT: u32 = 1;

/// The command that saves the snapshot of a run made before snapshots were kept.
pub fn backfill_command(run_id: &str) -> String {
    format!("camerata run snapshot-rules --run {run_id} --from <rule library folder>")
}

#[derive(Serialize)]
struct SnapshotOut<'a> {
    kind: &'static str,
    format: u32,
    digest: &'a str,
    rules: Vec<&'a Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotIn {
    kind: String,
    format: u32,
    digest: String,
    rules: Vec<Value>,
}

/// Why a snapshot cannot be used. Every kind refuses the export; none falls back to the live
/// library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalKind {
    /// The inspection names no saved run.
    NoRun,
    /// The run's record could not be read.
    RunUnreadable(String),
    /// The run was saved before Camerata recorded which rules it ran with.
    NoPin,
    /// The run loaded no rule library.
    NoRules,
    /// The run's rules were not saved with it (a run made before snapshots were kept).
    Missing,
    /// The snapshot file could not be read or parsed, or names a kind or format this build does
    /// not read.
    Unreadable(String),
    /// The snapshot holds a field or value this build cannot read back (named by its path).
    Dropped { rule: String, field: String },
    /// The rules read back do not have the digest the run pinned.
    DigestMismatch { pinned: String, found: String },
}

/// A refusal to use a run's rule snapshot, with the run it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRefusal {
    pub run_id: Option<String>,
    pub kind: RefusalKind,
}

impl SnapshotRefusal {
    pub fn new(run_id: Option<&str>, kind: RefusalKind) -> Self {
        Self {
            run_id: run_id.map(str::to_string),
            kind,
        }
    }

    /// A short stable code for the kind, for a JSON body.
    pub fn code(&self) -> &'static str {
        match self.kind {
            RefusalKind::NoRun => "no_run",
            RefusalKind::RunUnreadable(_) => "run_unreadable",
            RefusalKind::NoPin => "no_pin",
            RefusalKind::NoRules => "no_rules",
            RefusalKind::Missing => "missing",
            RefusalKind::Unreadable(_) => "unreadable",
            RefusalKind::Dropped { .. } => "unknown_field",
            RefusalKind::DigestMismatch { .. } => "digest_mismatch",
        }
    }
}

impl std::fmt::Display for SnapshotRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let run = self.run_id.as_deref().unwrap_or("(unknown)");
        match &self.kind {
            RefusalKind::NoRun => f.write_str(
                "This inspection has no saved run, so the rules it ran with are not known and it \
                 cannot be exported. Run the inspection again.",
            ),
            RefusalKind::RunUnreadable(why) => write!(
                f,
                "The saved run {run} of this inspection could not be read ({why}), so the rules \
                 it ran with are not known and it cannot be exported."
            ),
            RefusalKind::NoPin => write!(
                f,
                "Run {run} was saved before Camerata recorded which rules an inspection ran with, \
                 so it cannot be exported. Run the inspection again."
            ),
            RefusalKind::NoRules => write!(
                f,
                "Run {run} loaded no rule library, so there are no rules to export it with. Run \
                 the inspection again with the rule library in place."
            ),
            RefusalKind::Missing => write!(
                f,
                "The rules run {run} ran with were not saved with it, so it cannot be exported. \
                 Save them with `{}`, naming the rule library the inspection ran with, then \
                 export again.",
                backfill_command(run)
            ),
            RefusalKind::Unreadable(why) => write!(
                f,
                "The saved rules of run {run} cannot be read ({why}), so it cannot be exported."
            ),
            RefusalKind::Dropped { rule, field } => write!(
                f,
                "The saved rules of run {run} hold {field} on rule {rule}, which this build of \
                 Camerata cannot read, so it cannot be exported. Export it with a build that \
                 reads it."
            ),
            RefusalKind::DigestMismatch { pinned, found } => write!(
                f,
                "The saved rules of run {run} are not the rules it ran with (they read as {found} \
                 and the run recorded {pinned}). They may be damaged, so it cannot be exported."
            ),
        }
    }
}

impl std::error::Error for SnapshotRefusal {}

/// The file name a snapshot with `digest` (`sha256:<64 lowercase hex>`) is kept under, or `None`
/// for anything else (so a digest read from a file can never name a path outside the folder).
pub fn file_name(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    let well_formed = hex.len() == 64
        && hex
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    well_formed.then(|| format!("sha256-{hex}.json"))
}

/// The snapshot of `rules`: its digest and the file's bytes.
pub fn encode(rules: &RuleSet) -> (String, Vec<u8>) {
    let digest = camerata_rules::corpus_version(rules);
    let out = SnapshotOut {
        kind: SNAPSHOT_KIND,
        format: SNAPSHOT_FORMAT,
        digest: &digest,
        rules: rules.iter().collect(),
    };
    // A RuleSet is plain strings, numbers and lists: serializing it cannot fail.
    let bytes = serde_json::to_vec(&out).unwrap_or_default();
    (digest, bytes)
}

/// Read a snapshot back and verify it against `pinned`, the rule digest the run recorded. See
/// the module docs for every check.
pub fn decode(bytes: &[u8], pinned: &str) -> Result<RuleSet, RefusalKind> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| RefusalKind::Unreadable(format!("it does not parse: {e}")))?;
    let file: SnapshotIn = serde_json::from_value(value)
        .map_err(|e| RefusalKind::Unreadable(format!("it is not a rule snapshot: {e}")))?;
    if file.kind != SNAPSHOT_KIND {
        return Err(RefusalKind::Unreadable(format!(
            "it names the kind `{}`, not `{SNAPSHOT_KIND}`",
            file.kind
        )));
    }
    if file.format != SNAPSHOT_FORMAT {
        return Err(RefusalKind::Unreadable(format!(
            "it is in format {}, and this build reads format {SNAPSHOT_FORMAT}",
            file.format
        )));
    }
    let mut set = RuleSet::default();
    for (index, stored) in file.rules.into_iter().enumerate() {
        let id = stored
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("number {}", index + 1), str::to_string);
        let rule: Rule = serde_json::from_value(stored.clone())
            .map_err(|e| RefusalKind::Unreadable(format!("rule {id} does not read: {e}")))?;
        let reread = serde_json::to_value(&rule).unwrap_or(Value::Null);
        if let Some(field) = first_dropped(&stored, &reread, "") {
            return Err(RefusalKind::Dropped { rule: id, field });
        }
        set.push(rule);
    }
    let found = camerata_rules::corpus_version(&set);
    if found != pinned || found != file.digest {
        return Err(RefusalKind::DigestMismatch {
            pinned: pinned.to_string(),
            found,
        });
    }
    Ok(set)
}

/// The first place where `stored` holds something `reread` does not (a missing key, a list of
/// another length, a different value), named by its path; `None` when `reread` holds all of it.
/// Keys `reread` has and `stored` lacks are allowed: a field added since, at its default.
fn first_dropped(stored: &Value, reread: &Value, path: &str) -> Option<String> {
    let here = || {
        if path.is_empty() {
            "the rule itself".to_string()
        } else {
            format!("`{path}`")
        }
    };
    match (stored, reread) {
        (Value::Object(a), Value::Object(b)) => a.iter().find_map(|(key, value)| {
            let inner = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            match b.get(key) {
                None => Some(format!("`{inner}`")),
                Some(other) => first_dropped(value, other, &inner),
            }
        }),
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(here());
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (x, y))| first_dropped(x, y, &format!("{path}[{i}]")))
        }
        (x, y) if x == y => None,
        _ => Some(here()),
    }
}

/// Where a run's snapshot with `digest` lives, inside `run_dir`.
pub fn path_in(run_dir: &Path, digest: &str) -> Option<PathBuf> {
    file_name(digest).map(|name| run_dir.join(SNAPSHOT_DIR).join(name))
}

/// Write the snapshot of `rules` into `run_dir` and return its digest.
pub fn write_in(run_dir: &Path, rules: &RuleSet) -> std::io::Result<String> {
    let (digest, bytes) = encode(rules);
    let path = path_in(run_dir, &digest).ok_or_else(|| {
        std::io::Error::other(format!("the rule digest {digest} is not a sha256 digest"))
    })?;
    super::create_private_dir(&run_dir.join(SNAPSHOT_DIR))?;
    super::write_atomic(&path, &bytes)?;
    Ok(digest)
}

/// A run's verified snapshot: the rules, their digest and the file's bytes (to keep a copy
/// beside the inspection made from the run).
#[derive(Debug, Clone)]
pub struct RunSnapshot {
    pub rules: RuleSet,
    pub digest: String,
    pub bytes: Vec<u8>,
}

/// Read the snapshot of the run in `run_dir` and verify it against the run's own pinned rule
/// digest. Every failure is a named refusal.
pub fn read_run_dir(run_dir: &Path) -> Result<RunSnapshot, SnapshotRefusal> {
    let run_id = run_dir.file_name().and_then(|n| n.to_str());
    let refuse = |kind| SnapshotRefusal::new(run_id, kind);
    let record = super::load_record(run_dir)
        .map_err(|e| refuse(RefusalKind::RunUnreadable(e.to_string())))?;
    let run_id = Some(record.run_id.as_str());
    let refuse = |kind| SnapshotRefusal::new(run_id, kind);
    let pinned = record
        .pin
        .as_ref()
        .ok_or_else(|| refuse(RefusalKind::NoPin))?
        .corpus_version
        .clone();
    if pinned.trim().is_empty() {
        return Err(refuse(RefusalKind::NoRules));
    }
    let path = path_in(run_dir, &pinned).ok_or_else(|| {
        refuse(RefusalKind::Unreadable(format!(
            "the run recorded the rule digest `{pinned}`, which is not a sha256 digest"
        )))
    })?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refuse(RefusalKind::Missing))
        }
        Err(e) => return Err(refuse(RefusalKind::Unreadable(e.to_string()))),
    };
    let rules = decode(&bytes, &pinned).map_err(refuse)?;
    Ok(RunSnapshot {
        rules,
        digest: pinned,
        bytes,
    })
}

/// [`read_run_dir`] for the run `run_id` in this user's run store.
pub fn read_run(run_id: &str) -> Result<RunSnapshot, SnapshotRefusal> {
    read_run_dir(&checked_run_dir(run_id)?)
}

// ── Backfill: the snapshot of a run made before snapshots were kept ─────────────────────────

/// What [`backfill`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backfilled {
    /// The snapshot was written from the folder, whose rules have the run's pinned digest.
    Written { digest: String, rules: usize },
    /// The run already keeps a snapshot that verifies; nothing was written.
    AlreadyKept { digest: String },
}

/// Why [`backfill`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillRefusal {
    /// The run cannot take a snapshot (no record, no pin, no rule library, a bad run id).
    Run(SnapshotRefusal),
    /// The folder holds no rule library to read.
    NoLibrary { folder: String },
    /// Rule files in the folder did not load, so it is not the library the run loaded.
    Unloadable { folder: String, files: Vec<String> },
    /// The folder's rules are not the rules the run ran with.
    DigestMismatch {
        run_id: String,
        pinned: String,
        found: String,
    },
    /// The snapshot could not be written.
    Io(String),
}

impl std::fmt::Display for BackfillRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(refusal) => write!(f, "{refusal}"),
            Self::NoLibrary { folder } => write!(
                f,
                "{folder} holds no rule library (no rule files were found there). Nothing was \
                 written."
            ),
            Self::Unloadable { folder, files } => write!(
                f,
                "{} rule files in {folder} did not load ({}), so it cannot be shown to be the \
                 library the inspection ran with. Nothing was written.",
                files.len(),
                files.join("; ")
            ),
            Self::DigestMismatch {
                run_id,
                pinned,
                found,
            } => write!(
                f,
                "The rule library in that folder is not the one run {run_id} ran with: it reads \
                 as {found} and the run recorded {pinned}. Name the folder that holds the rule \
                 library of the build that ran the inspection. Nothing was written."
            ),
            Self::Io(why) => write!(f, "The snapshot could not be written: {why}"),
        }
    }
}

impl std::error::Error for BackfillRefusal {}

/// Save the snapshot of the run `run_id` from the rule library in `folder`, for a run made
/// before snapshots were kept. The folder is loaded the way an inspection loads its library and
/// its digest is computed; unless it equals the run's pinned rule digest nothing is written.
/// It never guesses: a folder with a rule file that does not load, or with no rules, is refused.
/// A run that already keeps a snapshot that verifies is left as it is.
pub async fn backfill(run_id: &str, folder: &Path) -> Result<Backfilled, BackfillRefusal> {
    let run_dir = checked_run_dir(run_id).map_err(BackfillRefusal::Run)?;
    let record = super::load_record(&run_dir).map_err(|e| {
        BackfillRefusal::Run(SnapshotRefusal::new(
            Some(run_id),
            RefusalKind::RunUnreadable(e.to_string()),
        ))
    })?;
    let refuse = |kind| BackfillRefusal::Run(SnapshotRefusal::new(Some(&record.run_id), kind));
    let pinned = record
        .pin
        .as_ref()
        .ok_or_else(|| refuse(RefusalKind::NoPin))?
        .corpus_version
        .clone();
    if pinned.trim().is_empty() {
        return Err(refuse(RefusalKind::NoRules));
    }
    if let Ok(kept) = read_run_dir(&run_dir) {
        return Ok(Backfilled::AlreadyKept {
            digest: kept.digest,
        });
    }
    let shown = folder.display().to_string();
    if !folder.is_dir() {
        return Err(BackfillRefusal::NoLibrary { folder: shown });
    }
    let (rules, errors) = camerata_rules::load_corpus_lenient(folder).await;
    if !errors.is_empty() {
        return Err(BackfillRefusal::Unloadable {
            folder: shown,
            files: errors
                .iter()
                .map(|(path, error)| format!("{}: {error}", path.display()))
                .collect(),
        });
    }
    if rules.is_empty() {
        return Err(BackfillRefusal::NoLibrary { folder: shown });
    }
    let found = camerata_rules::corpus_version(&rules);
    if found != pinned {
        return Err(BackfillRefusal::DigestMismatch {
            run_id: record.run_id.clone(),
            pinned,
            found,
        });
    }
    write_in(&run_dir, &rules).map_err(|e| BackfillRefusal::Io(e.to_string()))?;
    // Read back exactly as an export will, so a write that does not verify is said now.
    let kept = read_run_dir(&run_dir).map_err(BackfillRefusal::Run)?;
    Ok(Backfilled::Written {
        digest: kept.digest,
        rules: kept.rules.len(),
    })
}

/// The folder of the run `run_id` in this user's run store, refusing anything that is not a
/// plain run id (so a run id can never name a path outside the store).
fn checked_run_dir(run_id: &str) -> Result<PathBuf, SnapshotRefusal> {
    if run_id.trim().is_empty() || run_id.contains(['/', '\\']) || run_id.starts_with('.') {
        return Err(SnapshotRefusal::new(
            Some(run_id),
            RefusalKind::RunUnreadable("it is not a run id".to_string()),
        ));
    }
    Ok(super::runs_dir().join(run_id))
}

#[cfg(test)]
#[path = "rule_snapshot_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rule_snapshot_backfill_tests.rs"]
mod backfill_tests;
