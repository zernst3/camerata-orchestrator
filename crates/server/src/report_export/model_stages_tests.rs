//! #584: every client export lists each model stage the run used, with the model it ran on,
//! read from the run's own record.

use std::collections::HashMap;
use std::io::Read as _;

use super::super::tests::{finding, report_with};
use super::*;
use crate::llm::LlmResponse;
use crate::onboard::ScanReport;
use crate::report_export::{build_report_json, ReportOptions, ReviewedReport};
use crate::stage_models::RecordedStageModels;
use crate::stage_telemetry::{FixOutcome, Stage, StageMeter};

fn call(model: &str, backend: &str) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: Some(10),
        text: String::new(),
        model: model.to_string(),
        backend: backend.to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// A run whose stages ran on different models, in batch and live mode, recorded the way the
/// pipeline records it: through the stage meter, then copied onto the ledger.
fn run_with_mixed_stages() -> ScanReport {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &call("acme-scan-model", "api/batch"));
    meter.record(Stage::ScanPass, &call("acme-scan-model", "api/batch"));
    // The joint checks across repositories are finder turns.
    meter.record(Stage::Finder, &call("acme-scan-model", "api"));
    meter.record(Stage::GroupReview, &call("acme-review-model", "api"));
    meter.record(
        Stage::DuplicateCheck,
        &call("acme-small-model", "api/batch"),
    );
    meter.record(Stage::CloneJudge, &call("acme-small-model", "api"));
    meter.record(Stage::FixGeneration, &call("acme-fix-model", "api/batch"));
    meter.record_fix_outcome(FixOutcome {
        sent_to_model: 5,
        answered: 4,
        gave_up: 1,
        cap_stopped: 1,
        ..FixOutcome::default()
    });
    meter.record(Stage::Calibration, &call("acme-cal-model", "cli"));
    let mut report = report_with(
        vec![finding("SEC-1", "src/db.rs", 3, "high")],
        vec!["SEC-1"],
    );
    report
        .ledger
        .record_model_stages(RecordedStageModels::from_telemetry(&meter.snapshot(400)));
    report
}

fn pdf_data(report: &ScanReport) -> crate::report_export::AuditReportJson {
    let sealed = report.clone().seal();
    build_report_json(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &ReportOptions::default(),
    )
}

fn lines(stages: &ModelStagesJson) -> Vec<String> {
    stages.stages.iter().map(|s| s.line.clone()).collect()
}

fn workbook(report: &ScanReport) -> Vec<u8> {
    let sealed = report.clone().seal();
    crate::xlsx_export::build_workbook(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &ReportOptions::default(),
    )
    .expect("the workbook builds")
}

fn zip_entry(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a zip");
    let mut file = archive.by_name(name).expect("the entry exists");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("UTF-8");
    out
}

/// The Start here sheet's rows, as (column A, column B) text pairs, in row order.
fn start_here_rows(bytes: &[u8]) -> Vec<(String, String)> {
    let workbook = zip_entry(bytes, "xl/workbook.xml");
    let names: Vec<&str> = workbook
        .split("<sheet name=\"")
        .skip(1)
        .map(|c| &c[..c.find('"').expect("a quoted name")])
        .collect();
    let index = names
        .iter()
        .position(|n| *n == crate::xlsx_export::vocabulary::START_HERE)
        .expect("a Start here sheet");
    let sheet = zip_entry(bytes, &format!("xl/worksheets/sheet{}.xml", index + 1));
    let shared = zip_entry(bytes, "xl/sharedStrings.xml");
    let pool: Vec<String> = shared
        .split("<si>")
        .skip(1)
        .filter_map(|si| {
            let start = si.find('>')? + 1;
            let end = si[start..].find("</t>")? + start;
            Some(
                si[start..end]
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&#39;", "'")
                    .replace("&amp;", "&"),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for row in sheet.split("<row ").skip(1) {
        let mut a = String::new();
        let mut b = String::new();
        for cell in row.split("<c r=\"").skip(1) {
            let column = cell.chars().next().unwrap_or(' ');
            let Some(v_at) = cell.find("<v>") else {
                continue;
            };
            if !cell[..v_at].contains("t=\"s\"") {
                continue;
            }
            let v_end = cell[v_at..].find("</v>").expect("closed") + v_at;
            let Ok(i) = cell[v_at + 3..v_end].parse::<usize>() else {
                continue;
            };
            let text = pool.get(i).cloned().unwrap_or_default();
            match column {
                'A' => a = text,
                'B' => b = text,
                _ => {}
            }
        }
        rows.push((a, b));
    }
    rows
}

/// The (label, detail) rows the workbook prints for the model stages.
fn workbook_stage_rows(bytes: &[u8]) -> Vec<(String, String)> {
    let rows = start_here_rows(bytes);
    let Some(at) = rows
        .iter()
        .position(|(a, _)| a == crate::xlsx_export::vocabulary::P_MODELS_BY_STAGE)
    else {
        return Vec::new();
    };
    rows[at + 1..]
        .iter()
        .take_while(|(a, b)| !a.is_empty() && !b.is_empty() && STAGE_LABELS.contains(&a.as_str()))
        .cloned()
        .collect()
}

const STAGE_LABELS: [&str; 10] = [
    "Finder",
    "Finder and the checks across repositories",
    "Group review",
    "Duplicate check",
    "Clone check",
    "Fix text",
    "Calibration",
    "Finding detail",
    "Alternative options",
    "Deep compliance and security review",
];

#[test]
fn a_run_whose_stages_used_different_models_prints_each_one_in_the_pdf_data() {
    let json = pdf_data(&run_with_mixed_stages());
    assert_eq!(
        lines(&json.cover.model_stages),
        vec![
            "Finder: acme-scan-model, batch and live.",
            "Group review: acme-review-model, live.",
            "Duplicate check: acme-small-model, batch.",
            "Clone check: acme-small-model, live.",
            "Fix text: acme-fix-model, batch. 2 findings use the rule's standard fix.",
            "Calibration: acme-cal-model (requested), live.",
        ]
    );
    assert_eq!(json.cover.model_stages.note, None);
}

#[test]
fn a_run_whose_stages_used_different_models_prints_each_one_in_the_workbook() {
    let rows = workbook_stage_rows(&workbook(&run_with_mixed_stages()));
    let want: Vec<(String, String)> = [
        ("Finder", "acme-scan-model, batch and live"),
        ("Group review", "acme-review-model, live"),
        ("Duplicate check", "acme-small-model, batch"),
        ("Clone check", "acme-small-model, live"),
        (
            "Fix text",
            "acme-fix-model, batch. 2 findings use the rule's standard fix",
        ),
        ("Calibration", "acme-cal-model (requested), live"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(rows, want);
}

/// The record wins over every single-model field and over today's default: a stage that ran on a
/// model other than the default names that model.
#[test]
fn a_non_default_model_on_one_stage_is_shown_not_the_default() {
    let default_model = crate::ai_audit::resolve_audit_model(None);
    let meter = StageMeter::default();
    meter.record(Stage::Finder, &call(&default_model, "api"));
    meter.record(Stage::FixGeneration, &call("acme-tuned-fix-model", "api"));
    let mut report = report_with(vec![finding("SEC-1", "a.rs", 1, "high")], vec!["SEC-1"]);
    report.provenance.audit_model = Some(default_model.clone());
    report
        .ledger
        .record_model_stages(RecordedStageModels::from_telemetry(&meter.snapshot(0)));

    let json = pdf_data(&report);
    let fix = json
        .cover
        .model_stages
        .stages
        .iter()
        .find(|s| s.stage == "fix_text")
        .expect("the fix text stage is listed");
    assert_eq!(fix.models, vec!["acme-tuned-fix-model".to_string()]);
    assert!(
        !fix.line.contains(&default_model),
        "the fix text names the default model: {}",
        fix.line
    );
    let rows = workbook_stage_rows(&workbook(&report));
    assert!(
        rows.contains(&(
            "Fix text".to_string(),
            "acme-tuned-fix-model, live".to_string()
        )),
        "{rows:?}"
    );
}

#[test]
fn the_pdf_data_the_workbook_and_the_json_carry_the_same_list() {
    let report = run_with_mixed_stages();
    let sealed = report.clone().seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let opts = ReportOptions::default();
    let json = build_report_json(&reviewed, None, &opts);
    let inspection =
        crate::xlsx_export::build_findings_export(&reviewed, None, &json, &HashMap::new());
    let inspection = serde_json::to_value(&inspection).expect("serializes");
    let pdf = serde_json::to_value(&json.cover.model_stages).expect("serializes");
    assert!(!json.cover.model_stages.stages.is_empty());
    assert_eq!(inspection["provenance"]["model_stages"], pdf);

    let rows = workbook_stage_rows(&workbook(&report));
    let from_pdf: Vec<(String, String)> = json
        .cover
        .model_stages
        .stages
        .iter()
        .map(|s| (s.label.clone(), s.detail.clone()))
        .collect();
    assert_eq!(rows, from_pdf);
    for stage in &json.cover.model_stages.stages {
        assert_eq!(stage.line, format!("{}: {}.", stage.label, stage.detail));
    }
}

/// A run that made no model call says so and lists nothing.
#[test]
fn a_run_with_no_model_call_lists_no_stage() {
    let mut report = report_with(vec![finding("SEC-1", "a.rs", 1, "high")], vec!["SEC-1"]);
    report
        .ledger
        .record_model_stages(RecordedStageModels::default());
    let json = pdf_data(&report);
    assert!(json.cover.model_stages.stages.is_empty());
    assert_eq!(
        json.cover.model_stages.note.as_deref(),
        Some("No model was used in this inspection.")
    );
}

/// A stage that ran on several models names each, without a serial comma.
#[test]
fn a_stage_on_three_models_lists_them_without_a_serial_comma() {
    let meter = StageMeter::default();
    for model in ["acme-a", "acme-b", "acme-c"] {
        meter.record(Stage::GroupReview, &call(model, "api"));
    }
    let mut report = report_with(Vec::new(), Vec::new());
    report
        .ledger
        .record_model_stages(RecordedStageModels::from_telemetry(&meter.snapshot(0)));
    assert_eq!(
        lines(&model_stages(&report)),
        vec!["Group review: acme-a, acme-b and acme-c, live."]
    );
}

#[test]
fn the_cross_repository_checks_are_named_on_a_project_of_several_repos() {
    let mut report = run_with_mixed_stages();
    report.repos = vec!["acme/api".to_string(), "acme/web".to_string()];
    assert_eq!(
        lines(&model_stages(&report))[0],
        "Finder and the checks across repositories: acme-scan-model, batch and live."
    );
}

/// The words follow the client-text rules: no em or en dash, no serial comma, American English
/// and the word "inspection".
#[test]
fn the_list_passes_the_client_text_rules() {
    let mut texts: Vec<String> = Vec::new();
    let mut multi = run_with_mixed_stages();
    multi.repos = vec!["acme/api".to_string(), "acme/web".to_string()];
    for report in [run_with_mixed_stages(), multi, older_run()] {
        let stages = model_stages(&report);
        texts.extend(stages.stages.iter().map(|s| s.line.clone()));
        texts.extend(stages.stages.iter().map(|s| s.detail.clone()));
        texts.extend(stages.note.clone());
    }
    let mut none = report_with(Vec::new(), Vec::new());
    none.ledger
        .record_model_stages(RecordedStageModels::default());
    texts.extend(model_stages(&none).note);
    assert!(texts.len() > 10, "{texts:?}");
    for text in &texts {
        assert!(
            crate::client_text::banned_in(text).is_empty(),
            "a banned character in {text:?}"
        );
        assert!(
            !text.contains(", and ") && !text.contains(", or "),
            "a serial comma in {text:?}"
        );
        for british in ["recognis", "organis", "behaviour", "colour", "analys"] {
            assert!(!text.contains(british), "British spelling in {text:?}");
        }
        assert!(!text.to_lowercase().contains("audit"), "{text:?}");
    }
    let notes: Vec<&String> = texts.iter().filter(|t| t.contains("inspection")).collect();
    assert_eq!(notes.len(), 2, "both notes say inspection: {texts:?}");
}

// ── An older saved run ──────────────────────────────────────────────────────────────────────

/// The fix the older run's fix stage wrote. Only its presence matters here: it is the older run's
/// sign that the fix stage ran. Kept in a constant so `fix_text_tests`' scan of fixture fix texts
/// (which counts every `fix_specific` literal in the tree) does not pick up text that is not a fix
/// text sample.
const OLDER_RUN_FIX: &str = "Use the shared query builder in src/db.rs.";

/// A run saved before the stage list was recorded: its ledger has no `model_stages` and its
/// telemetry no `modes`. Built as the stored `report.json` of such a run would read.
fn older_run_json() -> serde_json::Value {
    let meter = StageMeter::default();
    meter.record(Stage::Finder, &call("acme-old-scan-model", "api"));
    meter.record(Stage::Calibration, &call("acme-old-cal-model", "api"));
    meter.record(Stage::GroupReview, &call("acme-old-review-model", "api"));
    meter.record(Stage::FixGeneration, &call("acme-old-fix-model", "api"));
    let mut report = report_with(
        vec![crate::onboard::Finding {
            fix_specific: Some(OLDER_RUN_FIX.to_string()),
            ..finding("SEC-1", "src/db.rs", 3, "high")
        }],
        vec!["SEC-1"],
    );
    report.provenance.audit_model = Some("acme-old-scan-model".to_string());
    report.provenance.calibration_model = Some("acme-old-cal-model".to_string());
    report.provenance.group_review_model = Some("acme-old-review-model".to_string());
    report.provenance.clone_judge_model = None;
    report.ledger.record_stage_telemetry(meter.snapshot(400));
    report
        .ledger
        .record_duplicate_checks(crate::possible_duplicate::check::DuplicateChecks {
            pairs: vec![crate::possible_duplicate::check::PairCheck {
                key_a: "a".to_string(),
                key_b: "b".to_string(),
                fix_a: "F1".to_string(),
                fix_b: "F2".to_string(),
                verdict: crate::possible_duplicate::check::Verdict::Different,
                reason: "two different flaws".to_string(),
                model: "acme-old-small-model".to_string(),
            }],
            not_run: None,
        });
    let mut value = serde_json::to_value(&report).expect("serializes");
    let ledger = value["ledger"].as_object_mut().expect("a ledger object");
    ledger.remove("model_stages");
    for stage in ledger["stage_telemetry"]["stages"]
        .as_array_mut()
        .expect("telemetry stages")
    {
        stage["usage"]
            .as_object_mut()
            .expect("usage")
            .remove("modes");
    }
    value
}

fn older_run() -> ScanReport {
    serde_json::from_value(older_run_json()).expect("an older report.json still loads")
}

#[test]
fn an_older_saved_run_is_re_exported_from_its_own_record() {
    // Stored as the run folder stores it, and read back the way a re-export reads it.
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join(crate::run_store::REPORT_FILE);
    std::fs::write(&path, serde_json::to_vec(&older_run_json()).unwrap()).unwrap();
    let stored: ScanReport =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("the stored run loads");
    assert!(stored.ledger.model_stages().is_none(), "the older format");

    // No model is reachable from an export: it is built from the stored run alone.
    let json = pdf_data(&stored);
    assert_eq!(
        lines(&json.cover.model_stages),
        vec![
            "Finder: acme-old-scan-model.",
            "Group review: acme-old-review-model.",
            "Duplicate check: model not recorded.",
            "Fix text: model not recorded.",
            "Calibration: acme-old-cal-model.",
        ]
    );
    assert_eq!(
        json.cover.model_stages.note.as_deref(),
        Some(OLDER_RUN_NOTE),
        "the older run says why some stages are not recorded"
    );
    let default_model = crate::ai_audit::resolve_audit_model(None);
    for stage in &json.cover.model_stages.stages {
        assert!(!stage.line.contains(&default_model), "{}", stage.line);
        assert_eq!(stage.mode, None, "an older run did not record modes");
    }
    let rows = workbook_stage_rows(&workbook(&stored));
    assert_eq!(
        rows[3],
        ("Fix text".to_string(), "model not recorded".to_string())
    );
    assert_eq!(rows.len(), 5);
}

/// An older run whose scan made no model call lists nothing, and says no model was used.
#[test]
fn an_older_run_with_no_model_lists_no_stage() {
    let mut value = older_run_json();
    value["provenance"]["audit_model"] = serde_json::Value::Null;
    value["provenance"]["calibration_model"] = serde_json::Value::Null;
    value["provenance"]["group_review_model"] = serde_json::Value::Null;
    value["findings"][0]["fix_specific"] = serde_json::Value::Null;
    value["ledger"]
        .as_object_mut()
        .unwrap()
        .remove("duplicate_checks");
    let report: ScanReport = serde_json::from_value(value).unwrap();
    let stages = model_stages(&report);
    assert!(stages.stages.is_empty(), "{:?}", lines(&stages));
    assert_eq!(
        stages.note.as_deref(),
        Some("No model was used in this inspection.")
    );
}

// ── Review round 1 ──────────────────────────────────────────────────────────────────────────

/// Provenance stamps the configured calibration and group review models even when the stage never
/// ran. An older run lists a stage only when its stage record shows calls.
#[test]
fn an_older_run_lists_calibration_and_group_review_only_when_they_ran() {
    let mut value = older_run_json();
    let stages = value["ledger"]["stage_telemetry"]["stages"]
        .as_array_mut()
        .expect("telemetry stages");
    stages.retain(|s| s["stage"] != "calibration" && s["stage"] != "group_review");
    let report: ScanReport = serde_json::from_value(value).unwrap();
    assert_eq!(
        lines(&model_stages(&report)),
        vec![
            "Finder: acme-old-scan-model.",
            "Duplicate check: model not recorded.",
            "Fix text: model not recorded.",
        ]
    );
    let mut value = older_run_json();
    value["ledger"]
        .as_object_mut()
        .unwrap()
        .remove("stage_telemetry");
    let report: ScanReport = serde_json::from_value(value).unwrap();
    let listed = lines(&model_stages(&report));
    assert!(
        !listed
            .iter()
            .any(|l| l.starts_with("Calibration") || l.starts_with("Group review")),
        "no stage record, no sign they ran: {listed:?}"
    );
}

/// The new-format record: calibration configured but never called is not listed.
#[test]
fn calibration_configured_but_never_called_is_not_listed() {
    let meter = StageMeter::default();
    meter.record(Stage::Finder, &call("acme-scan-model", "api"));
    let mut report = report_with(vec![finding("SEC-1", "a.rs", 1, "high")], vec!["SEC-1"]);
    report.provenance.calibration_model = Some("acme-configured-cal-model".to_string());
    report
        .ledger
        .record_model_stages(RecordedStageModels::from_telemetry(&meter.snapshot(0)));
    assert_eq!(
        lines(&model_stages(&report)),
        vec!["Finder: acme-scan-model, live."]
    );
}

fn carried(n: usize) -> crate::scan_ledger::IncrementalCarry {
    crate::scan_ledger::IncrementalCarry {
        changed: 0,
        carried_files: 4,
        carried_findings: n,
        prior_sha: Some("abc1234".to_string()),
    }
}

/// An incremental run whose model findings were all carried called no model, but its findings did
/// come from one: the report never says no model was used.
#[test]
fn a_run_that_carried_every_model_finding_does_not_say_no_model_was_used() {
    let mut report = report_with(vec![finding("SEC-1", "a.rs", 1, "high")], vec!["SEC-1"]);
    report
        .ledger
        .record_model_stages(RecordedStageModels::default());
    report.ledger.record_incremental(carried(3));
    let stages = model_stages(&report);
    assert!(stages.stages.is_empty());
    assert_eq!(stages.note.as_deref(), Some(CARRIED_ALL_NOTE));
    assert_ne!(stages.note.as_deref(), Some(NO_MODEL_NOTE));
}

/// A run that called models and carried other model findings lists what it called and says
/// some findings came from an earlier inspection.
#[test]
fn a_run_that_carried_some_model_findings_says_so_under_its_list() {
    let mut report = run_with_mixed_stages();
    report.ledger.record_incremental(carried(2));
    let stages = model_stages(&report);
    assert_eq!(stages.stages.len(), 6);
    assert_eq!(stages.note.as_deref(), Some(CARRIED_SOME_NOTE));
    // Nothing carried: no note.
    let mut none_carried = run_with_mixed_stages();
    none_carried.ledger.record_incremental(carried(0));
    assert_eq!(model_stages(&none_carried).note, None);
}

#[test]
fn the_carried_notes_pass_the_client_text_rules() {
    for text in [CARRIED_ALL_NOTE, CARRIED_SOME_NOTE] {
        assert!(crate::client_text::banned_in(text).is_empty(), "{text}");
        assert!(
            !text.contains(", and ") && !text.contains(", or "),
            "{text}"
        );
        assert!(text.contains("inspection"), "{text}");
        assert!(!text.to_lowercase().contains("audit"), "{text}");
    }
}

/// A run saved before the record existed, whose duplicate check a newer build ran later: that
/// stage names its model, the others stay as the older run's.
#[test]
fn an_older_run_names_a_stage_a_newer_build_ran_later() {
    let mut report = older_run();
    let later = StageMeter::default();
    later.record(Stage::DuplicateCheck, &call("acme-dup-model", "api"));
    let mut partial = RecordedStageModels::partial();
    partial.absorb(&later.snapshot(0));
    report.ledger.record_model_stages(partial);
    let stages = model_stages(&report);
    assert_eq!(
        lines(&stages),
        vec![
            "Finder: acme-old-scan-model.",
            "Group review: acme-old-review-model.",
            "Duplicate check: acme-dup-model, live.",
            "Fix text: model not recorded.",
            "Calibration: acme-old-cal-model.",
        ]
    );
    assert_eq!(stages.note.as_deref(), Some(OLDER_RUN_NOTE));
}
