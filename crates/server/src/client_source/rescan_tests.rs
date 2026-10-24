//! "Rescan changes": only what changed since the inspected commit is sent to the model, the
//! findings for the rest carry forward, and the result is recorded as a delta retest.
//!
//! The model is a fake `claude` on PATH that logs every prompt it is given and answers with a
//! canned result, so the tests can assert exactly which file bodies reached it.

use std::path::Path;
use std::time::Duration;

use camerata_test_env::EnvGuard;
use tempfile::TempDir;

use super::rescan::{rescan_changes, RescanOutcome};
use super::test_support::{fixture, install_fake_claude, Fx, REPO};
use crate::inspections::InspectionKind;
use crate::test_git::{commit_all, git, write_file};
use crate::{start_audit_job, AuditJobOptions, AuditReq};

const STABLE_MARKER: &str = "UNIQUE-STABLE-BODY-7731";
const CHANGED_MARKER_OLD: &str = "UNIQUE-CHANGED-BODY-OLD-4402";
const CHANGED_MARKER_NEW: &str = "UNIQUE-CHANGED-BODY-NEW-9158";
const RULE: &str = "ARCH-CURRENCY-CONSISTENCY-1";

/// The line the fake model flags in the stable file. The marker is NOT on it: later passes show
/// the model a finding's own line, and the marker must reach the model only if the whole file
/// body does.
const STABLE_FINDING_CODE: &str = "const base = order.subtotal + order.shippingCents;";

fn stable_ts() -> String {
    format!(
        "export function stableTotal(order: Order): number {{\n  {STABLE_FINDING_CODE}\n  return base;\n}}\n// {STABLE_MARKER}\n"
    )
}

fn changed_ts(marker: &str) -> String {
    format!(
        "export function changedTotal(order: Order): number {{\n  const tag = \"{marker}\";\n  return order.subtotal * 2;\n}}\n"
    )
}

struct Rig {
    fx: Fx,
    tools: TempDir,
    _guards: Vec<EnvGuard>,
}

impl Rig {
    fn prompts(&self) -> String {
        std::fs::read_to_string(self.tools.path().join("prompts.log")).unwrap_or_default()
    }

    fn forget_prompts(&self) {
        let _ = std::fs::remove_file(self.tools.path().join("prompts.log"));
    }
}

/// The fake model for these tests: whenever a prompt contains the STABLE file's body it "finds"
/// a violation in it.
fn install_model(dir: &Path) {
    let finding = serde_json::json!({
        "findings": [{
            "path": "src/stable.ts",
            "line": 2,
            "severity": "high",
            "rule": RULE,
            "title": "currency mixed with a shipping amount",
            "code": STABLE_FINDING_CODE,
            "detail": "the shipping amount is in another currency",
        }],
        "proposed_rules": [], "needs_files": []
    });
    // The fix-specific pass: a plain-prose fix for finding 0 that passes the self-check.
    let fix = serde_json::json!({
        "fixes": [{ "index": 0, "fix": "Convert the shipping amount into the order's currency before adding it to the subtotal." }]
    });
    install_fake_claude(dir, STABLE_MARKER, &finding, &fix);
}

/// A project of one repo with two TypeScript files committed, the rule selected, and a fake
/// `claude` on PATH. The first (full) inspection has NOT run yet.
fn rig() -> Rig {
    let tools = TempDir::new().unwrap();
    install_model(tools.path());
    let path = format!(
        "{}:{}",
        tools.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let guards = vec![
        EnvGuard::set("PATH", path),
        EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
        EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1"),
    ];
    let fx = fixture();
    write_file(&fx.checkout, "package.json", "{\"private\":true}\n");
    write_file(&fx.checkout, "src/stable.ts", &stable_ts());
    write_file(
        &fx.checkout,
        "src/changed.ts",
        &changed_ts(CHANGED_MARKER_OLD),
    );
    commit_all(&fx.checkout, "typescript sources");
    fx.state
        .projects
        .update(&fx.project_id, |p| {
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: RULE.to_string(),
                chosen_option: None,
                repos: Vec::new(),
                ..Default::default()
            });
        })
        .unwrap();
    Rig {
        fx,
        tools,
        _guards: guards,
    }
}

/// The scan the project's settings describe, built by the same code the delta uses so the two
/// share a rule fingerprint.
async fn scan_request(fx: &Fx) -> AuditReq {
    let project = fx.state.projects.get(&fx.project_id).unwrap();
    super::rescan::project_scan_request(&project, &[REPO.to_string()])
        .await
        .unwrap()
}

/// Wait for a job to finish; returns its final status.
async fn wait_for_job(fx: &Fx, job_id: &str) -> String {
    for _ in 0..1_200 {
        if let Some(job) = fx.state.jobs.get(job_id) {
            if job.status != "running" {
                return job.status;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {job_id} did not finish");
}

/// Run the first, full inspection through the same job path the scan button uses.
async fn first_inspection(rig: &Rig) -> crate::inspections::InspectionEntry {
    let job = start_audit_job(
        &rig.fx.state,
        scan_request(&rig.fx).await,
        AuditJobOptions {
            project_id: Some(rig.fx.project_id.clone()),
            ..AuditJobOptions::default()
        },
    )
    .await;
    assert_eq!(wait_for_job(&rig.fx, &job).await, "done");
    let entry = rig
        .fx
        .state
        .inspections
        .latest(&rig.fx.project_id)
        .expect("the first inspection is recorded");
    assert!(entry.sha_for(REPO).is_some());
    assert!(
        entry
            .report
            .findings
            .iter()
            .any(|f| f.path == "src/stable.ts" && f.rule_id.ends_with(RULE)),
        "the fake model found a violation in the stable file: {:?}",
        entry.report.findings
    );
    entry
}

/// Change `src/changed.ts` and commit it, then run the delta. Returns the finished job id.
async fn change_and_rescan(rig: &Rig) -> String {
    write_file(
        &rig.fx.checkout,
        "src/changed.ts",
        &changed_ts(CHANGED_MARKER_NEW),
    );
    commit_all(&rig.fx.checkout, "edit the changed file");
    rig.forget_prompts();
    let outcome = rescan_changes(&rig.fx.state, &rig.fx.project_id, REPO)
        .await
        .expect("the delta starts");
    let RescanOutcome::Started { job_id, .. } = outcome else {
        panic!("expected a job, got {outcome:?}");
    };
    assert_eq!(wait_for_job(&rig.fx, &job_id).await, "done");
    job_id
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_audits_only_files_changed_since_inspected_sha() {
    let rig = rig();
    first_inspection(&rig).await;
    assert!(
        rig.prompts().contains(STABLE_MARKER),
        "the full scan read every file"
    );

    change_and_rescan(&rig).await;

    let prompts = rig.prompts();
    assert!(
        prompts.contains(CHANGED_MARKER_NEW),
        "the changed file's body reached the model"
    );
    assert!(
        !prompts.contains(STABLE_MARKER),
        "the unchanged file's body must NOT be sent again: {}",
        prompts
            .lines()
            .filter(|l| l.contains("stable"))
            .take(5)
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(!prompts.contains(CHANGED_MARKER_OLD));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_carries_forward_ai_findings_for_unchanged_files_with_line_numbers() {
    let rig = rig();
    let base = first_inspection(&rig).await;
    let before = base
        .report
        .findings
        .iter()
        .find(|f| f.path == "src/stable.ts" && f.rule_id.ends_with(RULE))
        .cloned()
        .unwrap();

    change_and_rescan(&rig).await;

    let latest = rig.fx.state.inspections.latest(&rig.fx.project_id).unwrap();
    let carried: Vec<_> = latest
        .report
        .findings
        .iter()
        .filter(|f| f.path == "src/stable.ts" && f.rule_id.ends_with(RULE))
        .collect();
    assert_eq!(
        carried.len(),
        1,
        "carried once, not duplicated: {carried:?}"
    );
    assert_eq!(carried[0].line, before.line, "line numbers intact");
    assert_eq!(carried[0].snippet, before.snippet);
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_writes_delta_retest_entry_with_from_and_to_sha() {
    let rig = rig();
    let base = first_inspection(&rig).await;
    let from = base.sha_for(REPO).unwrap().to_string();

    change_and_rescan(&rig).await;

    let entries = rig.fx.state.inspections.list(&rig.fx.project_id);
    assert_eq!(entries.len(), 2);
    let to = git(&rig.fx.checkout, &["rev-parse", "HEAD"]);
    assert_ne!(from, to);
    assert_eq!(
        entries[0].kind,
        InspectionKind::DeltaRetest {
            base_inspection_id: base.id.clone(),
            from_sha: from,
            to_sha: to.clone(),
        }
    );
    assert_eq!(
        entries[0].sha_for(REPO),
        Some(to.as_str()),
        "the new entry covers HEAD"
    );
    assert_eq!(entries[1].kind, InspectionKind::Full, "the base is kept");
    // The review screen's current scan is the delta's result.
    assert_eq!(
        rig.fx
            .state
            .get_last_scan(&rig.fx.project_id)
            .unwrap()
            .provenance
            .audited_refs[0]
            .sha
            .as_deref(),
        Some(to.as_str())
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_falls_back_to_full_when_rules_changed() {
    let rig = rig();
    first_inspection(&rig).await;
    // The rule selection changes after the inspection: carried findings would reflect the old
    // rules, so the engine must audit EVERYTHING again.
    rig.fx
        .state
        .projects
        .update(&rig.fx.project_id, |p| {
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: "ARCH-EXACT-DECIMALS-1".to_string(),
                chosen_option: None,
                repos: Vec::new(),
                ..Default::default()
            });
        })
        .unwrap();

    change_and_rescan(&rig).await;

    let prompts = rig.prompts();
    assert!(prompts.contains(CHANGED_MARKER_NEW));
    let message = rig
        .fx
        .state
        .inspections
        .latest(&rig.fx.project_id)
        .and_then(|e| e.report.message);
    assert!(
        prompts.contains(STABLE_MARKER),
        "a changed rule set means a full re-scan, so the stable file is audited again; \
         report message: {message:?}"
    );
    let latest = rig.fx.state.inspections.latest(&rig.fx.project_id).unwrap();
    assert!(
        latest
            .report
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("rule selection changed"),
        "{:?}",
        latest.report.message
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_nothing_changed_returns_without_a_job() {
    let rig = rig();
    first_inspection(&rig).await;
    let jobs_before = rig
        .fx
        .state
        .jobs
        .has_running_for_project(&rig.fx.project_id);
    assert!(!jobs_before);

    let outcome = rescan_changes(&rig.fx.state, &rig.fx.project_id, REPO)
        .await
        .expect("allowed");

    assert!(
        matches!(outcome, RescanOutcome::NothingChanged),
        "{outcome:?}"
    );
    assert_eq!(
        rig.fx.state.inspections.list(&rig.fx.project_id).len(),
        1,
        "no new entry"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_counts_uncommitted_edits_as_changes() {
    let rig = rig();
    first_inspection(&rig).await;
    write_file(
        &rig.fx.checkout,
        "src/changed.ts",
        &changed_ts(CHANGED_MARKER_NEW),
    );
    rig.forget_prompts();

    let outcome = rescan_changes(&rig.fx.state, &rig.fx.project_id, REPO)
        .await
        .expect("allowed");

    let RescanOutcome::Started {
        job_id,
        changed_files,
        ..
    } = outcome
    else {
        panic!("a dirty tree is a change");
    };
    assert_eq!(changed_files, 1);
    assert_eq!(wait_for_job(&rig.fx, &job_id).await, "done");
    assert!(rig.prompts().contains(CHANGED_MARKER_NEW));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_changes_warns_when_inspection_was_dirty() {
    let rig = rig();
    // An inspection whose tree had uncommitted changes when it ran.
    let sha = git(&rig.fx.checkout, &["rev-parse", "HEAD"]);
    let mut report = super::test_support::report_at(REPO, &sha);
    report.provenance.audited_refs[0].dirty = true;
    report.provenance.audit_model = Some("test-model".to_string());
    rig.fx
        .state
        .set_last_scan(rig.fx.project_id.clone(), report);
    write_file(
        &rig.fx.checkout,
        "src/changed.ts",
        &changed_ts(CHANGED_MARKER_NEW),
    );
    commit_all(&rig.fx.checkout, "edit");

    let outcome = rescan_changes(&rig.fx.state, &rig.fx.project_id, REPO)
        .await
        .expect("allowed");

    let RescanOutcome::Started { job_id, notes, .. } = outcome else {
        panic!("expected a job");
    };
    assert!(
        notes
            .iter()
            .any(|n| n.contains("uncommitted changes") && n.contains("full re-inspection")),
        "{notes:?}"
    );
    assert_eq!(wait_for_job(&rig.fx, &job_id).await, "done");
    let latest = rig.fx.state.inspections.latest(&rig.fx.project_id).unwrap();
    assert!(latest
        .report
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("uncommitted"));
}

#[tokio::test]
async fn rescan_changes_refuses_on_deleted_source() {
    use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let deleted = delete_source(
        &fx.state,
        &fx.project_id,
        REPO,
        &DeleteRequest {
            confirm: REPO.to_string(),
            include_uncommitted: false,
        },
    )
    .await;
    assert!(matches!(deleted, Ok(DeleteOutcome::Done { .. })));

    let refused = rescan_changes(&fx.state, &fx.project_id, REPO)
        .await
        .unwrap_err();

    assert_eq!(refused.code, "source_deleted");
    assert!(
        refused.message.starts_with("Source deleted on "),
        "{}",
        refused.message
    );
    assert!(refused.message.contains("Re-fetch source to rescan"));
}

#[tokio::test]
async fn rescan_changes_refuses_when_there_is_no_inspection() {
    let fx = fixture();
    let refused = rescan_changes(&fx.state, &fx.project_id, REPO)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "no_inspection");
    assert!(refused.message.contains("run a scan instead"));
}

#[tokio::test]
async fn rescan_changes_refuses_when_inspected_commit_unreachable() {
    let fx = fixture();
    fx.inspect_at(&"1".repeat(40));
    let refused = rescan_changes(&fx.state, &fx.project_id, REPO)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "inspected_commit_unreachable");
    assert!(refused.message.contains("full re-inspection"));
}

#[tokio::test]
async fn rescan_changes_refuses_while_a_scan_runs_or_when_another_repo_is_unavailable() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let job = fx.state.jobs.create("audit", Some(fx.project_id.clone()));
    let busy = rescan_changes(&fx.state, &fx.project_id, REPO)
        .await
        .unwrap_err();
    assert_eq!(busy.code, "scan_in_progress");
    fx.state
        .jobs
        .finish(&job, crate::onboard::ScanReport::gated(&[REPO.to_string()]));

    // An inspection that covered a second repo whose clone is gone cannot be re-run.
    let mut report = super::test_support::report_at(REPO, &fx.head);
    report.provenance.audited_refs.push(
        super::test_support::report_at("acme/lib", &"2".repeat(40))
            .provenance
            .audited_refs[0]
            .clone(),
    );
    fx.state.set_last_scan(fx.project_id.clone(), report);
    let missing = rescan_changes(&fx.state, &fx.project_id, REPO)
        .await
        .unwrap_err();
    assert_eq!(missing.code, "source_not_present");
    assert!(missing.message.contains("acme/lib"));
}

/// The synthetic prior must agree with `partition`: an unchanged file is unchanged, a changed
/// or renamed file is changed, a deleted file's finding is not carried.
#[test]
fn synthetic_prior_fingerprints_equal_partition_fingerprints() {
    use crate::scan_cache::{partition, ScanManifest, MANIFEST_VERSION};
    use std::collections::HashSet;

    let current: Vec<(String, String)> = vec![
        ("keep.rs".into(), "same body".into()),
        ("edited.rs".into(), "new body".into()),
        ("new_name.rs".into(), "renamed body".into()),
    ];
    // Git said: edited.rs changed; old_name.rs -> new_name.rs; gone.rs deleted.
    let changed: HashSet<String> = ["edited.rs", "old_name.rs", "new_name.rs", "gone.rs"]
        .into_iter()
        .map(String::from)
        .collect();
    let mut report = super::test_support::report_at(REPO, &"a".repeat(40));
    for path in ["keep.rs", "edited.rs", "old_name.rs", "gone.rs"] {
        report.findings.push(crate::onboard::Finding {
            repo: REPO.to_string(),
            path: path.to_string(),
            rule_id: "AI-STANCE".to_string(),
            ..crate::onboard::Finding::default()
        });
    }
    // A floor finding on an unchanged file is recomputed, never carried.
    report.findings.push(crate::onboard::Finding {
        repo: REPO.to_string(),
        path: "keep.rs".to_string(),
        rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
        ..crate::onboard::Finding::default()
    });
    report.ledger.record_rule(
        "SEC-NO-HARDCODED-SECRETS-1".to_string(),
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        3,
        1,
    );
    let entry = crate::inspections::InspectionEntry::from_report(
        "p",
        report,
        crate::inspections::InspectionKind::Full,
    );

    let delta = super::rescan::repo_delta(REPO, &current, &changed, &entry);
    let mut prior = ScanManifest {
        version: MANIFEST_VERSION,
        ..ScanManifest::default()
    };
    prior.files.insert(REPO.to_string(), delta.unchanged);
    prior.findings = delta.carried;

    let part = partition(Some(&prior), REPO, &current);

    let changed_paths: HashSet<&str> = part.changed.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        changed_paths,
        HashSet::from(["edited.rs", "new_name.rs"]),
        "exactly the files git reported as changed (that still exist) are re-audited"
    );
    assert_eq!(part.unchanged_count, 1);
    let carried: Vec<(&str, &str)> = part
        .carried
        .iter()
        .map(|f| (f.path.as_str(), f.rule_id.as_str()))
        .collect();
    assert_eq!(
        carried,
        [("keep.rs", "AI-STANCE")],
        "only the unchanged file's AI finding carries; changed, renamed-from and deleted files' \
         findings drop, and the floor finding is recomputed instead"
    );
}
