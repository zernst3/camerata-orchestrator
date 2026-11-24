//! The diagnostic bundle: when a client run fails, one command writes a sanitized JSON of engine
//! facts (counts, enums, rule ids, hashed paths, word-skeleton messages) that a developer can
//! fix the bug from WITHOUT ever seeing client code.
//!
//! Design: `docs/design` diagnostic bundle, revision 2 (owner-approved). This module is build
//! steps 1, 3 and 4: the pure re-typing functions ([`sanitize`]), the decision table and its
//! walker ([`decisions`]), the shape probe ([`shape`]), the last guard ([`leak_check`]), the
//! preview ([`preview`]), and [`build`] and [`write`]. The error pointer (step 2) is not built:
//! its `origin` and `cause_origin` fields have no decision, so they are dropped and listed under
//! `undecided` until it is.
//!
//! # Nothing is copied, everything is re-typed
//!
//! The bundle is built from three operator files only, read as `serde_json::Value`: `run.json`
//! ([`crate::run_store::RunRecord`]), the integrity ledger ([`crate::scan_ledger::ScanLedger`],
//! which holds the stage telemetry) and `failures.json`
//! ([`crate::failure_report::FailureReport`]). It never opens `calls/`, `batches/`, checkpoints,
//! finder rooms, saved inspections or exports. The one exception is the shape probe, which
//! measures the files of a failed part in the checkout and keeps numbers and yes-or-no values.
//!
//! Every path is hashed with a salt made for the bundle and never written ([`sanitize::BundleSalt`]);
//! the operator's lookup file (token to real path) goes beside the run, never beside the bundle.
//! Before anything is written, [`leak_check`] searches the bundle and its preview for secrets,
//! repo and home names and client path segments, and one hit refuses the write.
//!
//! Camerata sends nothing anywhere: the operator decides whether to pass the file on.

pub mod decisions;
pub mod leak_check;
pub mod preview;
pub mod sanitize;
pub mod shape;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use decisions::{DecisionTable, Walker};
use leak_check::{leak_check, LeakInputs, LeakRefusal};
use sanitize::{BundleSalt, PathTokens, RepoInfo};

/// The layout version of a bundle.
pub const BUNDLE_FORMAT: u32 = 1;

/// The words a bundle and its preview hold by their own layout, which the leak check does not
/// count as a client's.
pub const BUNDLE_WORDS: &[&str] = &[
    "bundle id made_at engine_version os arch named_paths format coverage leaves kept dropped",
    "rejected undecided rejected_values leak_check passed refused absent shapes status measured",
    "unavailable skipped files path present bytes lines longest_line line_ending utf8 bom",
    "ends_with_newline repo tree_changed piece rule quoted secret key other",
    "camerata diagnostic engine on made named paths yes no run attempt origin parts part kind",
    "calls done not failures none failure report in message resolved attempts at backend model",
    "http error type exit wait longest ending final newline text values dropped decision decided",
    "but the value did fit leak check probed not_probed",
    std::env::consts::OS,
    std::env::consts::ARCH,
    std::env::consts::FAMILY,
];

/// How a bundle is built.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Show real repo-relative paths instead of tokens (`--named-paths`). Secrets and code are
    /// stripped all the same.
    pub named_paths: bool,
    /// Run the shape probe on the files of each failed part (`--no-shapes` turns it off).
    pub shapes: bool,
    /// The rule ids of the shipped corpus: shown as they are. Any other id is hashed.
    pub known_rule_ids: BTreeSet<String>,
    /// The operator's home folder name, for the leak check.
    pub home_folder: Option<String>,
    /// The run's folder (`runs/<run-id>/`), where the lookup file goes. `None`: no lookup file.
    pub lookup_dir: Option<PathBuf>,
}

/// A built bundle, not yet written. Holds no salt: the salt dies with [`build`].
pub struct DiagnosticBundle {
    id: String,
    json: Value,
    preview: String,
    /// Token to real path, for the operator only. Never part of the bundle.
    lookup: BTreeMap<String, String>,
    lookup_dir: Option<PathBuf>,
    leak_inputs: LeakInputs,
    leak_result: Result<(), LeakRefusal>,
}

impl std::fmt::Debug for DiagnosticBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The lookup holds real paths: never in a debug print.
        f.debug_struct("DiagnosticBundle")
            .field("id", &self.id)
            .field("leak_result", &self.leak_result)
            .finish_non_exhaustive()
    }
}

impl DiagnosticBundle {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The bundle as it would be written.
    pub fn json(&self) -> &Value {
        &self.json
    }

    pub fn preview(&self) -> &str {
        &self.preview
    }

    /// The leak check's verdict on this bundle. [`write`] refuses an `Err`.
    pub fn leak_result(&self) -> &Result<(), LeakRefusal> {
        &self.leak_result
    }
}

/// Where a bundle was written.
#[derive(Debug, Clone)]
pub struct WrittenBundle {
    pub json: PathBuf,
    pub preview: PathBuf,
    /// The operator's lookup file, when the run's folder was known and a path was hashed.
    pub lookup: Option<PathBuf>,
    /// Plain notes for the operator (a folder others can read).
    pub notes: Vec<String>,
}

/// Why nothing was written.
#[derive(Debug)]
pub enum WriteError {
    /// The leak check found something; nothing was written.
    Leak(LeakRefusal),
    Io(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Leak(r) => write!(f, "{r}; nothing was written"),
            Self::Io(e) => write!(f, "could not write the bundle: {e}"),
        }
    }
}

impl std::error::Error for WriteError {}

/// `<data dir>/camerata/diagnostics`: outside the state directory, so a cleanup of state never
/// takes a bundle, and outside every repo.
pub fn default_out_dir() -> Option<PathBuf> {
    crate::app_dirs::camerata_dir().map(|d| d.join("diagnostics"))
}

/// The operator's home folder name, for the leak check.
pub fn home_folder_name() -> Option<String> {
    dirs::home_dir()
        .and_then(|h| h.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| !n.is_empty())
}

/// A fresh bundle id: the time made plus 8 random hex characters, so ids sort by time.
fn bundle_id(now: chrono::DateTime<chrono::Utc>) -> String {
    let mut bytes = [0u8; 4];
    getrandom::getrandom(&mut bytes)
        .expect("the operating system random source is unavailable; refusing to name a bundle");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{hex}", now.format("%Y%m%dT%H%M%SZ"))
}

/// The run's repos, as `run.json` names them.
fn repos_of(run: &Value) -> Vec<RepoInfo> {
    run.get("repos")
        .and_then(Value::as_array)
        .map(|repos| {
            repos
                .iter()
                .map(|r| RepoInfo {
                    name: r
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    dir: r
                        .get("dir")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build a bundle from the three operator files. `ledger` and `failures` are `None` when the
/// file does not exist; the bundle then says `absent`.
pub fn build(
    run: &Value,
    ledger: Option<&Value>,
    failures: Option<&Value>,
    opts: &BuildOptions,
) -> DiagnosticBundle {
    build_with(
        run,
        ledger,
        failures,
        opts,
        &DecisionTable::standard(),
        BundleSalt::generate(),
    )
}

/// [`build`] with an explicit table and salt, for the tests.
pub(crate) fn build_with(
    run: &Value,
    ledger: Option<&Value>,
    failures: Option<&Value>,
    opts: &BuildOptions,
    table: &DecisionTable,
    salt: BundleSalt,
) -> DiagnosticBundle {
    let now = chrono::Utc::now();
    let id = bundle_id(now);
    let repos = repos_of(run);
    // Under `--named-paths` a first walk collects the paths met in typed `Path` leaves, so the
    // real walk shows a word of a text only when it is one of them, wherever it comes first.
    let typed = if opts.named_paths {
        let mut first = PathTokens::new(&salt, &repos, true);
        let mut w = Walker::new(table, &mut first, &opts.known_rule_ids);
        w.walk("run", run);
        if let Some(l) = ledger {
            w.walk("ledger", l);
        }
        if let Some(f) = failures {
            w.walk("failures", f);
        }
        first.typed().clone()
    } else {
        BTreeSet::new()
    };
    let mut paths = PathTokens::new(&salt, &repos, opts.named_paths).with_typed(typed);
    let mut walker = Walker::new(table, &mut paths, &opts.known_rule_ids);

    let mut run_out = walker.walk("run", run);
    add_repo_facts(&mut run_out, run, &walker);
    let ledger_out = ledger.map_or(json!("absent"), |l| walker.walk("ledger", l));
    let mut failures_out = failures.map_or(json!("absent"), |f| walker.walk("failures", f));
    let undecided: Vec<String> = walker.undecided.iter().cloned().collect();
    let rejected: Vec<String> = walker.rejected.iter().cloned().collect();
    let coverage = walker.coverage;
    if let Some(raw) = failures {
        add_shapes(&mut failures_out, raw, &repos, opts.shapes, walker.paths());
    }

    let mut bundle = Map::new();
    bundle.insert(
        "bundle".into(),
        json!({
            "id": id,
            "format": BUNDLE_FORMAT,
            "made_at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "engine_version": crate::run_store::engine_version(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "named_paths": opts.named_paths,
        }),
    );
    bundle.insert("run".into(), run_out);
    bundle.insert("ledger".into(), ledger_out);
    bundle.insert("failures".into(), failures_out);
    bundle.insert(
        "coverage".into(),
        serde_json::to_value(coverage).unwrap_or(Value::Null),
    );
    bundle.insert("undecided".into(), json!(undecided));
    bundle.insert("rejected_values".into(), json!(rejected));
    bundle.insert("leak_check".into(), json!("passed"));
    let mut json = Value::Object(bundle);

    let inputs: Vec<&Value> = [Some(run), ledger, failures]
        .into_iter()
        .flatten()
        .collect();
    let mut engine_terms = opts.known_rule_ids.clone();
    engine_terms.insert(crate::run_store::engine_version());
    // The run's model ids are the operator's settings, not the client's.
    model_ids(run, &mut engine_terms);
    if let Some(l) = ledger {
        model_ids(l, &mut engine_terms);
    }
    if let Some(f) = failures {
        model_ids(f, &mut engine_terms);
    }
    let leak_inputs = LeakInputs::collect(
        &inputs,
        &repos,
        opts.home_folder.as_deref(),
        paths.seen(),
        opts.named_paths,
        &engine_terms,
        paths.shown(),
    );
    let mut preview = preview::render(&json);
    let leak_result = leak_check(&json, &preview, &leak_inputs);
    if leak_result.is_err() {
        json["leak_check"] = json!("refused");
        preview = preview::render(&json);
    }
    // The salt dies with this function: nothing it returns holds it.
    let lookup = paths.lookup().clone();
    DiagnosticBundle {
        id,
        json,
        preview,
        lookup,
        lookup_dir: opts.lookup_dir.clone(),
        leak_inputs,
        leak_result,
    }
}

/// Every model id an input names: a string under a key `model` or ending `_model`, a value of
/// `models` (except lists, which hold the client's own patterns), and the keys of `models`.
fn model_ids(v: &Value, out: &mut BTreeSet<String>) {
    let mut add = |s: &str| {
        if decisions::model_id_ok(s) {
            out.insert(s.to_string());
        }
    };
    fn walk(v: &Value, add: &mut impl FnMut(&str)) {
        if let Value::Object(m) = v {
            for (k, v) in m {
                match v {
                    // The role keys (`audit`, `fix`, ...) name a model only inside `models`,
                    // which the next arm covers: elsewhere a `fix` can hold the client's text.
                    Value::String(s) if k == "model" || k.ends_with("_model") => add(s),
                    Value::Object(inner) if k == "models" => {
                        inner.keys().for_each(|k| add(k));
                        for v in inner.values() {
                            if let Value::String(s) = v {
                                add(s);
                            }
                        }
                    }
                    _ => {}
                }
                walk(v, add);
            }
        } else if let Value::Array(a) = v {
            a.iter().for_each(|v| walk(v, add));
        }
    }
    walk(v, &mut add);
}

/// Each repo of the run gets its label and whether its tree changed during the run, in place
/// of its name, folder, commit and tree digests.
fn add_repo_facts(run_out: &mut Value, run: &Value, walker: &Walker<'_, '_>) {
    let (Some(out), Some(raw)) = (
        run_out.get_mut("repos").and_then(Value::as_array_mut),
        run.get("repos").and_then(Value::as_array),
    ) else {
        return;
    };
    for (o, r) in out.iter_mut().zip(raw) {
        let Some(o) = o.as_object_mut() else { continue };
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        o.insert("repo".into(), json!(walker.repo(name)));
        let start = r.get("tree_at_start").and_then(Value::as_str);
        let end = r.get("tree_at_end").and_then(Value::as_str);
        o.insert(
            "tree_changed".into(),
            json!(matches!((start, end), (Some(s), Some(e)) if s != e)),
        );
    }
}

/// Each still-failing entry gets the shapes of its files: `measured`, `unavailable` (the
/// checkout is gone) or `skipped` (`--no-shapes`). A resolved entry is history and gets none.
fn add_shapes(
    failures_out: &mut Value,
    raw: &Value,
    repos: &[RepoInfo],
    probe_on: bool,
    paths: &mut PathTokens<'_>,
) {
    let (Some(out), Some(raw)) = (
        failures_out
            .get_mut("entries")
            .and_then(Value::as_array_mut),
        raw.get("entries").and_then(Value::as_array),
    ) else {
        return;
    };
    let mut budget = shape::MAX_PROBED_FILES;
    for (o, r) in out.iter_mut().zip(raw) {
        if !r.get("resolved_at").is_none_or(Value::is_null) {
            continue;
        }
        let Some(o) = o.as_object_mut() else { continue };
        let files: Vec<String> = r
            .get("files")
            .and_then(Value::as_array)
            .map(|f| {
                f.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let repo_name = r.get("repo").and_then(Value::as_str).unwrap_or("");
        let ordinal = sanitize::repo_ordinal(repos, repo_name);
        let dir = ordinal
            .and_then(|n| repos.get(n as usize - 1))
            .map(|r| PathBuf::from(&r.dir))
            .filter(|d| d.is_dir());
        let shapes = match (probe_on, dir) {
            (false, _) => json!({"status": "skipped"}),
            (true, None) => json!({"status": "unavailable"}),
            (true, Some(dir)) => {
                let take = files.len().min(budget);
                budget -= take;
                let measured = shape::probe(&dir, &files[..take]);
                let rows: Vec<Value> = files[..take]
                    .iter()
                    .zip(measured)
                    .map(|(f, s)| {
                        let mut row = serde_json::to_value(&s).unwrap_or(Value::Null);
                        if let Some(m) = row.as_object_mut() {
                            m.retain(|_, v| !v.is_null());
                            let mut with_path = Map::new();
                            with_path
                                .insert("path".into(), json!(paths.leaf(f, ordinal.unwrap_or(0))));
                            with_path.extend(std::mem::take(m));
                            *m = with_path;
                        }
                        row
                    })
                    .collect();
                json!({"status": "measured", "files": rows, "not_probed": files.len() - take})
            }
        };
        o.insert("shapes".into(), shapes);
    }
}

/// Write `bundle` into `out_dir` (created private, mode 0700, when it does not exist) as
/// `camerata-diagnostic-<id>.json` and `camerata-diagnostic-<id>.preview.txt`, and the lookup
/// file into the run's folder. Refused, writing nothing, when the leak check found anything.
pub fn write(bundle: &DiagnosticBundle, out_dir: &Path) -> Result<WrittenBundle, WriteError> {
    let is_default = default_out_dir().as_deref() == Some(out_dir);
    write_in(bundle, out_dir, is_default)
}

/// [`write`], told whether `out_dir` is the default folder (which is made private every time).
pub(crate) fn write_in(
    bundle: &DiagnosticBundle,
    out_dir: &Path,
    is_default: bool,
) -> Result<WrittenBundle, WriteError> {
    if let Err(r) = &bundle.leak_result {
        return Err(WriteError::Leak(r.clone()));
    }
    // Once more, on exactly what is about to be written.
    leak_check(&bundle.json, &bundle.preview, &bundle.leak_inputs).map_err(WriteError::Leak)?;
    let io = |e: std::io::Error| WriteError::Io(e.to_string());
    let mut notes = Vec::new();
    // A folder this creates is private to the operator (0700). The default folder is made
    // private every time. An existing folder named with `--out` keeps its permissions
    // (changing the operator's own folder would surprise), and a note says when others can
    // read it. The files themselves are always 0600.
    let existed = out_dir.is_dir();
    if !existed {
        crate::run_store::create_private_dir(out_dir).map_err(io)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if is_default {
            std::fs::set_permissions(out_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(io)?;
        } else if existed {
            let mode = std::fs::metadata(out_dir).map_err(io)?.permissions().mode();
            if mode & 0o044 != 0 {
                notes.push(
                    "the output folder is readable by other users; the bundle files are \
                     private (0600), but their names and times are visible"
                        .to_string(),
                );
            }
        }
    }
    let stem = format!("camerata-diagnostic-{}", bundle.id);
    let json_path = out_dir.join(format!("{stem}.json"));
    let preview_path = out_dir.join(format!("{stem}.preview.txt"));
    let text =
        serde_json::to_vec_pretty(&bundle.json).map_err(|e| WriteError::Io(e.to_string()))?;
    // The lookup first: a bundle whose tokens cannot be translated is not left behind.
    let lookup = match &bundle.lookup_dir {
        Some(dir) if dir.is_dir() && !bundle.lookup.is_empty() => {
            let path = dir.join(format!("diagnostic-lookup-{}.json", bundle.id));
            let text = serde_json::to_vec_pretty(&bundle.lookup)
                .map_err(|e| WriteError::Io(e.to_string()))?;
            crate::run_store::write_atomic(&path, &text).map_err(io)?;
            Some(path)
        }
        _ => None,
    };
    crate::run_store::write_atomic(&json_path, &text).map_err(io)?;
    crate::run_store::write_atomic(&preview_path, bundle.preview.as_bytes()).map_err(io)?;
    Ok(WrittenBundle {
        json: json_path,
        preview: preview_path,
        lookup,
        notes,
    })
}
