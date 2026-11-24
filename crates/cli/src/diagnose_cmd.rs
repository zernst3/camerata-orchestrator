//! `camerata diagnose`, the diagnostic bundle an operator sends a developer when a client run
//! fails, so the bug can be fixed without the developer ever seeing client code.
//!
//! ```text
//! camerata diagnose bundle --run <id|latest> [--ledger <path>] [--failures <path>] [--out <dir>]
//!                          [--named-paths] [--no-shapes]
//! camerata diagnose preview <bundle.json>
//! ```
//!
//! `bundle` reads three operator files only (the run's `run.json`, the integrity ledger and the
//! failure report), re-types every field through the decision table
//! ([`camerata_server::diagnostic_bundle`]), runs the leak check and writes the bundle and its
//! preview to `<data dir>/camerata/diagnostics/` (or `--out`). The lookup file (token to real
//! path) goes into the run's own folder, never beside the bundle. It prints the preview and the
//! paths. Nothing is sent anywhere.
//!
//! The ledger and failure report are files only `camerata inspect` writes, beside each other in
//! the state directory. A cockpit run keeps both inside its saved inspection instead, so for a
//! cockpit run this command finds no ledger: the bundle then holds the run record alone, unless
//! `--ledger` and `--failures` name the files.
//!
//! Exit codes: `0` written, `3` the leak check refused the bundle (nothing was written), `1` the
//! bundle could not be made (no such run, an unreadable file).
//!
//! No model call, no network.

use std::path::{Path, PathBuf};

use camerata_server::diagnostic_bundle::{self, BuildOptions, WriteError};
use camerata_server::run_store::{self, RunSummary};
use serde_json::Value;

/// Exit code for "the leak check refused the bundle".
pub const EXIT_REFUSED: i32 = 3;

/// `diagnose bundle`'s arguments, clap-free.
#[derive(Debug, Clone, Default)]
pub struct BundleArgs {
    /// A run id, or `latest`.
    pub run: String,
    pub ledger: Option<PathBuf>,
    pub failures: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub named_paths: bool,
    pub no_shapes: bool,
}

/// What `diagnose bundle` did.
#[derive(Debug)]
pub struct BundleOutcome {
    pub preview: String,
    pub json: PathBuf,
    pub preview_path: PathBuf,
    pub lookup: Option<PathBuf>,
    /// Plain notes for the operator (a file that was not found, and what that means).
    pub notes: Vec<String>,
}

impl BundleOutcome {
    /// The text meant to be read and passed on: the preview, and the reminder to read it.
    /// Printed on stdout. It holds no path of this machine and no note.
    pub fn render_preview(&self) -> String {
        let mut out = self.preview.clone();
        out.push_str(
            "\nNothing was sent anywhere. Read the preview before you pass the bundle on.\n",
        );
        out
    }

    /// The notes and the three paths, which name this machine's state folder. Printed on
    /// stderr, under a line saying they are local only.
    pub fn render_local(&self) -> String {
        let mut out = String::from("Local only, do not send:\n");
        for note in &self.notes {
            out.push_str(&format!("note: {note}\n"));
        }
        out.push_str(&format!("bundle:  {}\n", self.json.display()));
        out.push_str(&format!("preview: {}\n", self.preview_path.display()));
        match &self.lookup {
            Some(l) => out.push_str(&format!(
                "lookup:  {} (token to real path; for you only, never send it)\n",
                l.display()
            )),
            None => out.push_str("lookup:  none (no path was hashed)\n"),
        }
        out
    }
}

/// Why `diagnose bundle` did not write a bundle.
#[derive(Debug)]
pub enum BundleError {
    /// The leak check refused it.
    Refused(String),
    /// The bundle could not be made.
    Failed(String),
}

impl BundleError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Refused(_) => EXIT_REFUSED,
            Self::Failed(_) => 1,
        }
    }
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

/// Whether `id` can name a run folder: letters, digits, `.`, `_` and `-`, no `..`. The id is
/// joined to the runs folder, so anything that could climb out of it is refused.
pub fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The run `run` names. `latest` is the newest saved run of any state and origin: a bundle is
/// for the run that just went wrong, whether it ended incomplete or was started in the cockpit.
pub fn resolve_run_id(run: &str, runs: &[RunSummary]) -> Result<String, String> {
    let run = run.trim();
    if run == "latest" {
        return runs
            .first()
            .map(|r| r.run_id.clone())
            .ok_or_else(|| "there is no saved run".to_string());
    }
    if !valid_run_id(run) {
        return Err(format!(
            "`{run}` is not a run id (letters, digits, `.`, `_` and `-` only)"
        ));
    }
    Ok(run.to_string())
}

/// Where `camerata inspect` keeps the ledger of the run in `run` (its repos' folders): the one
/// repo's own ledger, or the project's ledger for several repos.
pub fn default_ledger(run: &Value) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = run
        .get("repos")?
        .as_array()?
        .iter()
        .filter_map(|r| r.get("dir").and_then(Value::as_str))
        .map(PathBuf::from)
        .collect();
    match dirs.len() {
        0 => None,
        1 => Some(camerata_server::scan_cache::state_dir::ledger_path(
            &dirs[0],
        )),
        _ => Some(camerata_server::scan_cache::state_dir::project_ledger_path(
            &dirs,
        )),
    }
}

fn read_json(path: &Path, what: &str) -> Result<Value, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("could not read the {what} at {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not a readable {what}: {e}", path.display()))
}

/// An optional input: `explicit` must exist; the default may be missing, which is noted.
fn optional_input(
    explicit: Option<&Path>,
    default: Option<PathBuf>,
    what: &str,
    notes: &mut Vec<String>,
) -> Result<(Option<Value>, Option<PathBuf>), String> {
    if let Some(path) = explicit {
        return Ok((Some(read_json(path, what)?), Some(path.to_path_buf())));
    }
    match default {
        Some(path) if path.is_file() => Ok((Some(read_json(&path, what)?), Some(path))),
        Some(path) => {
            notes.push(format!(
                "no {what} at {} (a cockpit run keeps it inside its saved inspection; name the \
                 file to include it)",
                path.display()
            ));
            Ok((None, Some(path)))
        }
        None => {
            notes.push(format!("no {what}: the run names no repo folder"));
            Ok((None, None))
        }
    }
}

/// The rule ids of the shipped corpus, shown in a bundle as they are.
///
/// Only the bundled corpus counts: `CAMERATA_CORPUS_PATH` may name a folder with rules written
/// for one client, and their ids must be hashed, not shown. A missing folder gives an empty
/// set, so every id is hashed.
async fn shipped_rule_ids() -> std::collections::BTreeSet<String> {
    let (corpus, _errors) = camerata_rules::load_corpus_lenient(std::path::Path::new(
        camerata_rules::DEFAULT_CORPUS_PATH,
    ))
    .await;
    corpus.iter().map(|r| r.id.0.clone()).collect()
}

/// `diagnose bundle`, reading the run from the user's runs folder.
pub async fn run_bundle(args: &BundleArgs) -> Result<BundleOutcome, BundleError> {
    let runs_dir = run_store::runs_dir();
    let id = resolve_run_id(&args.run, &run_store::list_runs()).map_err(BundleError::Failed)?;
    let out_dir = match &args.out {
        Some(d) => d.clone(),
        None => diagnostic_bundle::default_out_dir().ok_or_else(|| {
            BundleError::Failed(
                "this system has no per-user data folder; name one with --out".to_string(),
            )
        })?,
    };
    bundle_from(
        &runs_dir.join(&id),
        args,
        &out_dir,
        shipped_rule_ids().await,
        diagnostic_bundle::home_folder_name(),
    )
}

/// [`run_bundle`] for an explicit run folder, output folder, corpus and home folder name.
pub fn bundle_from(
    run_dir: &Path,
    args: &BundleArgs,
    out_dir: &Path,
    known_rule_ids: std::collections::BTreeSet<String>,
    home_folder: Option<String>,
) -> Result<BundleOutcome, BundleError> {
    let run_path = run_dir.join("run.json");
    if !run_path.is_file() {
        return Err(BundleError::Failed(format!(
            "there is no saved run at {}",
            run_dir.display()
        )));
    }
    let run = read_json(&run_path, "run record").map_err(BundleError::Failed)?;
    let mut notes = Vec::new();
    let (ledger, ledger_path) = optional_input(
        args.ledger.as_deref(),
        default_ledger(&run),
        "integrity ledger",
        &mut notes,
    )
    .map_err(BundleError::Failed)?;
    let failures_default = ledger_path
        .as_deref()
        .map(|l| camerata_server::failure_report::paths_next_to(l).0);
    let (failures, _) = if args.failures.is_none() && ledger.is_none() {
        (None, None)
    } else {
        optional_input(
            args.failures.as_deref(),
            failures_default,
            "failure report",
            &mut notes,
        )
        .map_err(BundleError::Failed)?
    };
    let opts = BuildOptions {
        named_paths: args.named_paths,
        shapes: !args.no_shapes,
        known_rule_ids,
        home_folder,
        lookup_dir: Some(run_dir.to_path_buf()),
    };
    let bundle = diagnostic_bundle::build(&run, ledger.as_ref(), failures.as_ref(), &opts);
    match diagnostic_bundle::write(&bundle, out_dir) {
        Ok(written) => Ok(BundleOutcome {
            preview: bundle.preview().to_string(),
            json: written.json,
            preview_path: written.preview,
            lookup: written.lookup,
            notes: {
                notes.extend(written.notes);
                notes
            },
        }),
        Err(e @ WriteError::Leak(_)) => Err(BundleError::Refused(format!(
            "{e}. A field reached the bundle that must not: report the location above (it \
             names where, never what; a map key shows as `<key>`) to the developers so the \
             decision table can be fixed"
        ))),
        Err(e) => Err(BundleError::Failed(e.to_string())),
    }
}

/// `diagnose preview`: the preview of a written bundle, rendered again from its JSON.
pub fn run_preview(bundle: &Path) -> Result<String, String> {
    let json = read_json(bundle, "diagnostic bundle")?;
    if json.pointer("/bundle/format").is_none() {
        return Err(format!(
            "{} is not a diagnostic bundle (it has no bundle header)",
            bundle.display()
        ));
    }
    Ok(diagnostic_bundle::preview::render(&json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_run_id_cannot_climb_out_of_the_runs_folder() {
        for bad in ["", "..", "../x", "a/b", "a\\b", "x..y", "/abs", "~"] {
            assert!(!valid_run_id(bad), "{bad}");
            assert!(resolve_run_id(bad, &[]).is_err(), "{bad}");
        }
        assert!(valid_run_id("20261124T101500Z-ab12cd"));
        assert_eq!(
            resolve_run_id("latest", &[]),
            Err("there is no saved run".into())
        );
    }

    fn run_record(dir: &Path) -> Value {
        json!({
            "schema": 1, "run_id": "20261124T101500Z-ab12cd", "created_at": "2026-11-24T10:15:00Z",
            "updated_at": "2026-11-24T10:15:00Z", "status": "incomplete", "origin": "cli",
            "attempt": 1, "engine_version": "0.0.0+prompt1", "project_key": "acme-web-1",
            "repos": [{"name": "acme-web", "dir": dir.to_string_lossy(), "commit": null,
                       "clean": true, "tree_at_start": "t"}],
            "models": {"audit": "model-a", "calibration": "model-a", "fix": "model-a",
                       "backend": "api", "mode": "parallel"},
            "rules_fingerprint": "f", "ai_scan_key": {"audit_model": "model-a",
            "calibration_model": "", "prompt_schema_version": 1, "directives_fingerprint": "d"},
            "nonce_seed": "00", "args": {"repo": dir.to_string_lossy()}, "plan": []
        })
    }

    #[test]
    fn a_bundle_is_written_from_explicit_files() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("acme-web");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/billing.ts"), "let a = 1;\n").unwrap();
        let run_dir = tmp.path().join("runs/20261124T101500Z-ab12cd");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("run.json"), run_record(&repo).to_string()).unwrap();
        let ledger = tmp.path().join("x.ledger.json");
        std::fs::write(&ledger, json!({"runner_notes": ["all fine"]}).to_string()).unwrap();
        let failures = tmp.path().join("x.failures.json");
        std::fs::write(
            &failures,
            json!({"generated_at": "2026-11-24T10:15:00Z", "entries": [{
                "repo": "acme-web", "part": "audit pass 1/1", "files": ["src/billing.ts"],
                "class": "transport", "attempts": [], "first_at": "", "last_at": "",
                "message": "HTTP 529 overloaded"}]})
            .to_string(),
        )
        .unwrap();
        let out = tmp.path().join("out");
        let args = BundleArgs {
            run: "20261124T101500Z-ab12cd".into(),
            ledger: Some(ledger),
            failures: None,
            out: Some(out.clone()),
            named_paths: false,
            no_shapes: false,
        };
        let done = bundle_from(
            &run_dir,
            &args,
            &out,
            Default::default(),
            Some("auditor".into()),
        )
        .expect("written");
        assert!(done.json.starts_with(&out));
        assert!(done
            .lookup
            .as_deref()
            .is_some_and(|l| l.starts_with(&run_dir)));
        let text = std::fs::read_to_string(&done.json).unwrap();
        assert!(!text.contains("acme"), "{text}");
        assert!(!text.contains("billing"));
        assert!(text.contains("\"lines\": 1"), "the shape probe ran: {text}");
        assert!(done.render_preview().contains("Leak check: passed"));
        assert!(done.render_local().contains("never send it"));
        assert_eq!(run_preview(&done.json).unwrap(), done.preview);

        // An explicit file that does not exist is an error, not a silent gap.
        let missing = BundleArgs {
            failures: Some(tmp.path().join("nope.json")),
            ..args.clone()
        };
        assert!(matches!(
            bundle_from(&run_dir, &missing, &out, Default::default(), None),
            Err(BundleError::Failed(_))
        ));
        // No ledger anywhere: the run record alone, with a note.
        let alone = BundleArgs {
            ledger: None,
            ..args
        };
        let done = bundle_from(&run_dir, &alone, &out, Default::default(), None).unwrap();
        assert!(done.notes.iter().any(|n| n.contains("integrity ledger")));
        let text = std::fs::read_to_string(&done.json).unwrap();
        assert!(text.contains("\"ledger\": \"absent\""));
    }

    /// M4: the shipped rule ids come from the bundled corpus, whatever CAMERATA_CORPUS_PATH says.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_corpus_override_does_not_widen_the_shipped_rule_ids() {
        let custom = tempfile::tempdir().unwrap();
        let sample = std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH)
            .join("csharp/csharp-ienumerable-collections-1.toml");
        let text = std::fs::read_to_string(&sample).unwrap().replace(
            "CSHARP-IENUMERABLE-COLLECTIONS-1",
            "ACME-NO-DIRECT-LEDGER-WRITES-1",
        );
        std::fs::write(custom.path().join("custom.toml"), text).unwrap();
        let bundled = shipped_rule_ids().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_CORPUS_PATH", custom.path());
        let with_override = shipped_rule_ids().await;
        assert!(!with_override.contains("ACME-NO-DIRECT-LEDGER-WRITES-1"));
        assert_eq!(with_override, bundled);
        assert!(!bundled.is_empty(), "the bundled corpus has rules");
        // An override pointing nowhere changes nothing either.
        let _gone = camerata_test_env::EnvGuard::set("CAMERATA_CORPUS_PATH", "/nonexistent/x");
        assert_eq!(shipped_rule_ids().await, bundled);
    }

    /// L8: the notes and the three paths are not in the text meant to be passed on.
    #[test]
    fn local_notes_and_paths_are_kept_apart_from_the_preview() {
        let out = BundleOutcome {
            preview: "Camerata diagnostic bundle x\nLeak check: passed\n".into(),
            json: PathBuf::from("/data/b.json"),
            preview_path: PathBuf::from("/data/b.preview.txt"),
            lookup: Some(PathBuf::from("/state/runs/r/diagnostic-lookup-x.json")),
            notes: vec!["no integrity ledger at /state/acme-web-1/x.ledger.json".into()],
        };
        let stdout = out.render_preview();
        assert!(stdout.contains("Leak check: passed"));
        for local in ["/data/b.json", "/state/", "note:", "lookup"] {
            assert!(!stdout.contains(local), "`{local}` is on stdout:\n{stdout}");
        }
        let local = out.render_local();
        assert!(local.starts_with("Local only, do not send"), "{local}");
        for part in [
            "/data/b.json",
            "/data/b.preview.txt",
            "diagnostic-lookup-x.json",
            "note:",
        ] {
            assert!(local.contains(part), "`{part}` missing:\n{local}");
        }
    }

    #[test]
    fn preview_refuses_a_file_that_is_not_a_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.json");
        std::fs::write(&p, "{}").unwrap();
        assert!(run_preview(&p).is_err());
    }
}
