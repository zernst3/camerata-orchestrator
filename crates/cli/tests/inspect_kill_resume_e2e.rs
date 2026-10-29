#![allow(clippy::unwrap_used)]
//! Run to completion, interrupted runs: the REAL `camerata inspect` binary is killed (SIGKILL) part
//! way through a run, then resumed. Zach, 2026-10-28, after a laptop crash: "any interruption
//! should cost minutes, never the run".
//!
//! The model is a fake `claude` on PATH (the CLI backend, `--internal`), so the real binary, its
//! real transport and the real run store are all exercised with no network and no spend. The fake
//! answers each request as a pure function of the request, logs every call, and can be slowed so a
//! kill lands part way. A fake `typst` in front of the real one lets the second test kill the
//! process during the export.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import hashlib, json, os, re, sys, time
args = sys.argv[1:]
prompt = args[args.index("-p") + 1] if "-p" in args else ""
system = args[args.index("--system-prompt") + 1] if "--system-prompt" in args else ""
if os.environ.get("CAMERATA_FAKE_WARM"):
    sys.exit(0)
digest = hashlib.sha256((system + "\n" + prompt).encode()).hexdigest()[:16]
log = os.environ.get("FAKE_LOG")
def note(what):
    if log:
        with open(log, "a") as f:
            f.write(what + "\t" + digest + "\n")
note("start")
time.sleep(float(os.environ.get("FAKE_DELAY", "0")))
labels = re.findall(r'^<<<UNTRUSTED \S+ "([^"]*)">>>$', prompt, re.M)
if "CONFORMANCE audit" in system:
    findings = [
        {"path": p, "line": 1, "severity": "medium", "rule": "unchecked-input",
         "title": "Input is not checked in " + p, "code": "// FIXTURE",
         "detail": p + " passes input on without checking it."}
        for p in labels if p != "repo map"
    ]
    out = {"findings": findings, "proposed_rules": [], "needs_files": []}
elif "calibrating an automated audit" in system:
    n = len([l for l in prompt.splitlines() if l.startswith("[") and "(severity" in l])
    out = {"verdicts": [{"index": i, "severity": "medium", "confidence": "high",
                         "reason": "checked", "effort": "low"} for i in range(n)]}
elif "ONE concrete, codebase-specific fix" in system:
    n = len([l for l in labels if l.startswith("finding ")])
    out = {"fixes": [{"index": i, "fix": "Check the input before it is used."} for i in range(n)]}
else:
    out = {"findings": [], "proposed_rules": [], "needs_files": [], "verdicts": [],
           "fixes": [], "recommendations": []}
note("done")
print(json.dumps({"type": "result", "is_error": False, "result": json.dumps(out),
                  "total_cost_usd": 0.01,
                  "usage": {"input_tokens": 1000, "output_tokens": 100}}))
"#;

/// In front of the real `typst`: announces that the export began, then waits a while (so a kill
/// lands during the export), then runs the real one.
const FAKE_TYPST: &str = r#"#!/bin/sh
if [ -n "$TYPST_MARK" ]; then
  touch "$TYPST_MARK"
  sleep 20
fi
exec "$REAL_TYPST" "$@"
"#;

/// Both gates fail the test under CI when the program is missing, so a broken CI image can never
/// turn these tests into quiet skips.
fn python_and_typst_available(test: &str) -> bool {
    camerata_test_env::python3_on_path_for(test) && camerata_test_env::typst_on_path_for(test)
}

fn write_script(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stage_fixture(dest: &Path) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    fn copy(src: &Path, dest: &Path) {
        std::fs::create_dir_all(dest).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dest.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    copy(&root, dest);
    // As a real repository ignores its build output: the scan's own build step leaves it behind.
    std::fs::write(dest.join(".gitignore"), "target/\nCargo.lock\n").unwrap();
    let g = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "kill-e2e@camerata.local"]);
    g(&["config", "user.name", "Kill E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

struct World {
    _root: tempfile::TempDir,
    bin_dir: PathBuf,
    state: PathBuf,
    repo: PathBuf,
    log: PathBuf,
}

fn world(root: &tempfile::TempDir, name: &str) -> World {
    let base = root.path().join(name);
    let bin_dir = base.join("bin");
    let state = base.join("state");
    let repo = base.join("repo");
    for d in [&bin_dir, &state, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_script(&bin_dir, "claude", FAKE_CLAUDE);
    // Warm both fakes once: the OS checks a new executable the first time it runs.
    let _ = Command::new(bin_dir.join("claude"))
        .env("CAMERATA_FAKE_WARM", "1")
        .status();
    stage_fixture(&repo);
    World {
        _root: tempfile::tempdir().unwrap(),
        log: base.join("calls.log"),
        bin_dir,
        state,
        repo,
    }
}

fn inspect(w: &World, export: &Path, extra: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_camerata"));
    cmd.args([
        "inspect",
        "--internal",
        "--backend",
        "cli",
        "--realtime",
        "--full",
    ])
    .args(["--client", "Example Client Ltd", "--repo-name", "acme/one"])
    .args([
        "--efficiency",
        "warm_schedule=off",
        "--efficiency",
        "terse_output=off",
    ])
    .arg("--repo")
    .arg(&w.repo)
    .arg("--export")
    .arg(export)
    .args(extra);
    let path = format!(
        "{}:{}",
        w.bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path)
        .env("CAMERATA_STATE_DIR", &w.state)
        .env("CAMERATA_DISABLE_DEP_AUDIT", "1")
        .env("FAKE_LOG", &w.log)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

fn resume_cmd(w: &World, export: &Path, envs: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_camerata"));
    cmd.args(["inspect", "--resume", "latest", "--export"])
        .arg(export);
    let path = format!(
        "{}:{}",
        w.bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path)
        .env("CAMERATA_STATE_DIR", &w.state)
        .env("CAMERATA_DISABLE_DEP_AUDIT", "1")
        .env("FAKE_LOG", &w.log)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

fn log_lines(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn calls_stored(state: &Path) -> usize {
    std::fs::read_dir(state.join("camerata/runs"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|run| {
            std::fs::read_dir(run.path().join("calls"))
                .map(|d| d.count())
                .unwrap_or(0)
        })
        .sum()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !ready() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(90),
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        out.insert(file.name().to_string(), bytes);
    }
    out
}

const RUN_METADATA: &[&str] = &[
    "generated_at",
    "sha256",
    "short_sha",
    "size_bytes",
    "bytes",
    "commit",
    "sha",
    "head",
    "branch",
    "dir",
    "scanned_at",
    "created_at",
    "started_at",
    "finished_at",
    "completed_at",
    "timestamp",
    "model_sessions",
    "actual_usage",
    "usage",
    "cost_usd",
    "cost",
    "duration_ms",
    "elapsed_ms",
    "stage_telemetry",
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "calls",
];

fn strip(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            map.retain(|k, _| !RUN_METADATA.contains(&k.as_str()));
            map.values_mut().for_each(strip);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
        _ => {}
    }
}

/// The findings, manifest content, README and workbook sheets of an export, without the run's own
/// metadata (times, commits, costs). The PDF is compiled from the same data.
fn comparable(path: &Path) -> BTreeMap<String, String> {
    let short = regex::Regex::new(r"acme-one-[0-9a-f]{7}([.-])").unwrap();
    let hash40 = regex::Regex::new(r"[0-9a-f]{40}").unwrap();
    let when = regex::Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2})?Z?( UTC)?").unwrap();
    let on_main = regex::Regex::new(r"\b[0-9a-f]{7} on main").unwrap();
    let mut out = BTreeMap::new();
    for (name, bytes) in entries(path) {
        let name = short.replace(&name, "acme-one-<c>$1").into_owned();
        if name.ends_with(".pdf") {
            continue;
        }
        if name.ends_with(".json") {
            let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            strip(&mut v);
            let text = serde_json::to_string_pretty(&v).unwrap();
            out.insert(
                name,
                short.replace_all(&text, "acme-one-<c>$1").into_owned(),
            );
        } else if name.ends_with(".xlsx") {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
            for i in 0..archive.len() {
                let mut file = archive.by_index(i).unwrap();
                let inner = file.name().to_string();
                if inner.starts_with("docProps") {
                    continue;
                }
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
                let text = when.replace_all(&text, "<time>").into_owned();
                out.insert(
                    format!("{name}!{inner}"),
                    on_main.replace_all(&text, "<c> on main").into_owned(),
                );
            }
        } else {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let text = when.replace_all(&text, "<time>").into_owned();
            let text = hash40.replace_all(&text, "<c>").into_owned();
            out.insert(
                name,
                short.replace_all(&text, "acme-one-<c>$1").into_owned(),
            );
        }
    }
    out
}

fn assert_same(resumed: &Path, baseline: &Path) {
    let (a, b) = (comparable(resumed), comparable(baseline));
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (name, text) in &a {
        let other = &b[name];
        let first = text
            .lines()
            .zip(other.lines())
            .enumerate()
            .find(|(_, (x, y))| x != y);
        assert!(
            text == other,
            "{name} differs between the resumed and the uninterrupted run: {first:?}"
        );
    }
}

fn baseline_export(root: &tempfile::TempDir) -> PathBuf {
    let w = world(root, "baseline");
    let export = root.path().join("baseline.zip");
    let status = inspect(&w, &export, &[], &[]).status().unwrap();
    assert!(status.success(), "the uninterrupted run completes");
    export
}

/// SIGKILL part way through the model calls; resume; the export equals the uninterrupted run's and
/// no stored call was sent again.
#[test]
#[serial_test::serial(process_env)]
fn a_run_killed_during_the_model_calls_resumes_to_the_uninterrupted_export() {
    if !python_and_typst_available("a_run_killed_during_the_model_calls") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let baseline = baseline_export(&root);

    let w = world(&root, "killed");
    let export = root.path().join("resumed.zip");
    let mut child = inspect(&w, &export, &[], &[("FAKE_DELAY", "0.6")])
        .spawn()
        .unwrap();
    wait_for("some model calls to be stored", || {
        calls_stored(&w.state) >= 3
    });
    child.kill().unwrap(); // SIGKILL: no destructor runs, the lock stays behind
    child.wait().unwrap();
    assert!(!export.exists(), "nothing was exported");

    // The run is Interrupted and says what remains.
    {
        let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &w.state);
        let runs = camerata_server::run_store::list_runs();
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(
            runs[0].state,
            camerata_server::run_store::RunState::Interrupted
        );
        assert!(!runs[0].remaining.is_empty(), "{runs:?}");
    }
    let sent_before = log_lines(&w.log);
    let stored_before = calls_stored(&w.state);
    assert!(stored_before >= 3);

    let out = resume_cmd(&w, &export, &[]).output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Resume: what remains"), "{stdout}");
    assert!(export.exists());
    assert_same(&export, &baseline);

    // Done parts were not sent again. A call in flight at the kill may be (it was never stored),
    // so allow the few that were running at once.
    let done_before: std::collections::HashSet<String> = sent_before
        .iter()
        .filter(|(k, _)| k == "done")
        .map(|(_, d)| d.clone())
        .collect();
    let started_after: Vec<String> = log_lines(&w.log)
        .into_iter()
        .skip(sent_before.len())
        .filter(|(k, _)| k == "start")
        .map(|(_, d)| d)
        .collect();
    let resent = started_after
        .iter()
        .filter(|d| done_before.contains(*d))
        .count();
    assert!(
        resent <= 4,
        "{resent} of {} calls finished before the kill were sent again",
        done_before.len()
    );
    assert!(
        started_after.len()
            < sent_before.iter().filter(|(k, _)| k == "start").count() + started_after.len(),
        "sanity"
    );
}

/// SIGKILL during the export (after every model call is stored): the resume sends no model call
/// at all and the export is the uninterrupted run's.
#[test]
#[serial_test::serial(process_env)]
fn a_run_killed_during_the_export_resumes_without_a_single_model_call() {
    if !python_and_typst_available("a_run_killed_during_the_export") {
        return;
    }
    let real_typst = Command::new("which")
        .arg("typst")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let baseline = baseline_export(&root);

    let w = world(&root, "killed-in-export");
    write_script(&w.bin_dir, "typst", FAKE_TYPST);
    let mark = root.path().join("export-began");
    let export = root.path().join("resumed.zip");
    let mut child = inspect(
        &w,
        &export,
        &[],
        &[
            ("TYPST_MARK", mark.to_str().unwrap()),
            ("REAL_TYPST", &real_typst),
        ],
    )
    .spawn()
    .unwrap();
    wait_for("the export to begin", || mark.exists());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!export.exists());
    let sent_before = log_lines(&w.log).len();
    assert!(sent_before > 0, "every model call ran before the export");

    let out = resume_cmd(&w, &export, &[("REAL_TYPST", &real_typst)])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        log_lines(&w.log).len(),
        sent_before,
        "the resume sent no model call: merging and export are recomputed from the stored results"
    );
    assert_same(&export, &baseline);
}
