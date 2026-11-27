//! #587: every scan tool runs from Camerata's own tooling folder at its pinned version, is
//! provisioned when missing, never resolves from the user's `PATH`, and a provisioning failure
//! during the run stops the run instead of becoming a coverage note.
//!
//! No test here downloads anything: the tools are fakes served by
//! [`crate::pinned_tools::fixtures::FakeTools`], checked against pins over their real checksums.

use camerata_rules::EnforcementKind;

use super::tests::{lookup_over, rule_with, selected};
use super::*;
use crate::pinned_tools::fixtures::FakeTools;
use crate::pinned_tools::Artifact;

fn python_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("app.py"),
        "import os\nq = \"SELECT * FROM t WHERE id = \" + os.environ[\"ID\"]\n",
    )
    .unwrap();
    dir
}

/// The failure #587 reports: a Python repo, a machine with no ruff anywhere. Before the fix the
/// pass ran a bare `ruff` from `PATH` and ended as "Could not preview 3 rule(s) with ruff: No
/// such file or directory". Now ruff is provisioned into the tooling folder and the pass runs.
#[tokio::test]
async fn a_python_repo_without_ruff_on_the_machine_gets_ruff_provisioned_and_the_pass_runs() {
    let tools = FakeTools::new();
    assert!(!tools.env.is_installed(Artifact::Ruff), "no ruff yet");
    let rules = vec![rule_with(
        "PY-SQL-1",
        EnforcementKind::Mechanical,
        false,
        &["Ruff: S608"],
    )];
    let lookup = lookup_over(&rules);
    let repo = python_repo();
    let (findings, notes, attempted) = run_scan_tools(
        "acme/api",
        repo.path(),
        &[selected("PY-SQL-1")],
        &lookup,
        None,
        None,
        crate::client_lock::ClientTooling::INTERNAL,
        false,
        &tools.env,
    )
    .await
    .expect("no provisioning stop");

    assert!(attempted.contains("ruff"), "{attempted:?}");
    assert!(
        !notes.iter().any(|n| n.tool == "ruff"),
        "the ruff pass ran, so there is no ruff note: {notes:?}"
    );
    let ruff_rows: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.preview_tool.as_deref() == Some("ruff"))
        .collect();
    assert_eq!(ruff_rows.len(), 1, "{findings:?}");
    assert_eq!(ruff_rows[0].path, "app.py");
    assert!(tools.env.is_installed(Artifact::Ruff));
    assert_eq!(
        FakeTools::runs_of(&tools.entry(Artifact::Ruff)).len(),
        1,
        "the pinned ruff from the tooling folder is the one that ran"
    );
}

/// No scan tool resolves from `PATH`: with the process `PATH` EMPTY, every tool (ruff, eslint
/// on the pinned Node.js, semgrep in the pinned CPython's venv, osv-scanner) is provisioned and
/// runs, and each one was started with Camerata's child `PATH`, never the process's.
#[cfg(unix)]
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_an_empty_path_every_scan_tool_still_runs_from_the_tooling_folder() {
    let _semgrep_on = camerata_test_env::EnvGuard::remove(DISABLE_SEMGREP_ENV_VAR);
    let _dep_audit_on = camerata_test_env::EnvGuard::remove(crate::dep_audit::DISABLE_ENV_VAR);
    let _empty_path = camerata_test_env::EnvGuard::set("PATH", "");

    let tools = FakeTools::new();
    let rules = vec![
        rule_with("PY-1", EnforcementKind::Mechanical, false, &["Ruff: S608"]),
        rule_with(
            "JS-1",
            EnforcementKind::Mechanical,
            false,
            &["eslint: eqeqeq"],
        ),
        rule_with("SG-1", EnforcementKind::Mechanical, false, &["semgrep"]),
    ];
    let lookup = lookup_over(&rules);
    let repo = python_repo();
    std::fs::write(repo.path().join("app.js"), "if (a == b) {}\n").unwrap();
    std::fs::write(repo.path().join("package-lock.json"), "{}\n").unwrap();

    let (_findings, notes, attempted) = run_scan_tools(
        "acme/web",
        repo.path(),
        &[selected("PY-1"), selected("JS-1"), selected("SG-1")],
        &lookup,
        None,
        None,
        crate::client_lock::ClientTooling::INTERNAL,
        false,
        &tools.env,
    )
    .await
    .expect("no provisioning stop");
    for tool in ["ruff", "eslint", "semgrep"] {
        assert!(attempted.contains(tool), "{tool}: {attempted:?}");
        assert!(
            !notes.iter().any(|n| n.tool == tool),
            "{tool} ran cleanly with an empty PATH: {notes:?}"
        );
    }
    let (_, dep_note, _) = crate::dep_audit::run_dep_audit_listed(
        "acme/web",
        repo.path(),
        &crate::onboard::scope::RepoScope::none(),
        &tools.env,
    )
    .await
    .expect("no provisioning stop");
    assert!(
        dep_note
            .as_ref()
            .is_none_or(|n| !n.message.contains("provision")),
        "{dep_note:?}"
    );

    let programs = [
        tools.entry(Artifact::Ruff),
        tools.entry(Artifact::Node),
        crate::pinned_tools::semgrep_bin(tools.tooling()),
        tools.entry(Artifact::OsvScanner),
    ];
    for program in programs {
        let runs = FakeTools::runs_of(&program);
        assert!(!runs.is_empty(), "{} never ran", program.display());
        for seen in &runs {
            // The process PATH is empty, so an inherited PATH would read as "". Each run was
            // given Camerata's: its own runtime folders, then the OS base folders.
            assert!(!seen.is_empty(), "{} inherited the PATH", program.display());
            for dir in std::env::split_paths(seen) {
                assert!(
                    dir.starts_with(tools.tooling())
                        || crate::pinned_tools::SYSTEM_PATH_DIRS
                            .iter()
                            .any(|s| dir == std::path::Path::new(s)),
                    "{} was started with {} on its PATH",
                    program.display(),
                    dir.display()
                );
            }
        }
    }
}

/// A tool that has to be provisioned during the run and cannot be stops the run: the result is
/// a [`ToolProvisioningStop`], never a coverage note that lets the run finish as if the pass were
/// a scope limit (the run-integrity rule, #582).
#[tokio::test]
async fn a_tool_that_cannot_be_provisioned_mid_run_stops_the_run() {
    let rules = vec![rule_with(
        "PY-SQL-1",
        EnforcementKind::Mechanical,
        false,
        &["Ruff: S608"],
    )];
    let lookup = lookup_over(&rules);
    let repo = python_repo();

    let offline = FakeTools::new();
    offline
        .source
        .fail_everything("network unreachable (fixture)");
    let tampered = FakeTools::new();
    tampered.tamper(Artifact::Ruff);

    for (tools, cause) in [(&offline, "unreachable"), (&tampered, "checksum")] {
        let stop = run_scan_tools(
            "acme/api",
            repo.path(),
            &[selected("PY-SQL-1")],
            &lookup,
            None,
            None,
            crate::client_lock::ClientTooling::INTERNAL,
            false,
            &tools.env,
        )
        .await
        .expect_err("the run stops");
        assert_eq!(stop.tool, "ruff");
        assert_eq!(stop.repo, "acme/api");
        assert!(stop.reason.contains(cause), "{cause}: {}", stop.reason);
        assert!(!tools.env.is_installed(Artifact::Ruff));
    }
}

/// What was known before the run, Camerata having no build of a tool for this machine, is not
/// a stop: the list said so, the run was refused if it was a client one, and the pass is
/// disclosed as not run.
#[tokio::test]
async fn a_tool_with_no_build_for_this_machine_is_a_disclosed_note_not_a_stop() {
    let fakes = FakeTools::new();
    let pins = fakes.env.clone();
    let env = crate::pinned_tools::ToolEnv::new(fakes.tooling(), None, fakes.source.clone())
        .with_pins(move |a| pins.pins(a));
    let rules = vec![rule_with(
        "PY-SQL-1",
        EnforcementKind::Mechanical,
        false,
        &["Ruff: S608"],
    )];
    let lookup = lookup_over(&rules);
    let repo = python_repo();
    let (findings, notes, _) = run_scan_tools(
        "acme/api",
        repo.path(),
        &[selected("PY-SQL-1")],
        &lookup,
        None,
        None,
        crate::client_lock::ClientTooling::INTERNAL,
        false,
        &env,
    )
    .await
    .expect("not a stop");
    assert!(findings.is_empty());
    let note = notes
        .iter()
        .find(|n| n.tool == "ruff")
        .expect("a ruff note");
    assert!(!note.tool_failed, "{note:?}");
    assert!(
        note.message.contains("cannot be provisioned"),
        "{}",
        note.message
    );
}
