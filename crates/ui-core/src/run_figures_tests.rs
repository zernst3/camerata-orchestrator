//! The Run figures model over the acme fixtures: the figures each source carries, the copied
//! text, "not recorded" and the batch states.

use super::*;
use crate::test_support::run_figures::{FIGURES_JSON, REPORT_JSON};

fn report() -> ReportFigures {
    serde_json::from_str(REPORT_JSON).unwrap()
}

fn record() -> RunRecordFigures {
    serde_json::from_str(FIGURES_JSON).unwrap()
}

fn figures() -> RunFigures {
    RunFigures::new(report(), Some(record()))
}

fn value(f: &RunFigures, figure: &str) -> String {
    f.summary()
        .into_iter()
        .find(|(name, _)| *name == figure)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no figure {figure}"))
}

/// The whole copied text for the acme run, one figure per line.
const ACME_COPY: &str = "\
Run figures (operator only, never part of the client package)
Lines in scope: 48,213
Files in scope: 326
Languages in scope: 3
Files read: 311
Coverage: 95.4% (311 of 326 files read)
Wall clock: 1h 02m 03s
Sessions: 2
Model calls: 166
Tokens in: 1,507,405
Tokens out: 150,227
Tokens cached: 754,322
Cache hit share: 50.0%
Models used: 2
Scan stage, wall clock: 12m 30s
Scan stage, busy time: 40m 01s
Scan stage, model calls: 120
Scan stage, tokens in: 1,234,567
Scan stage, tokens out: 98,765
Scan stage, tokens cached: 654,321
Calibration stage, wall clock: 2m 45s
Calibration stage, busy time: 5m 05s
Calibration stage, model calls: 37
Calibration stage, tokens in: 222,333
Calibration stage, tokens out: 44,455
Calibration stage, tokens cached: 100,001
Fix stage, wall clock: not recorded
Fix stage, busy time: not recorded
Fix stage, model calls: 9
Fix stage, tokens in: 50,505
Fix stage, tokens out: 7,007
Fix stage, tokens cached: 0
Finder role, stages: Scan
Finder role, model calls: 120
Finder role, tokens in: 1,234,567
Finder role, tokens out: 98,765
Finder role, tokens cached: 654,321
Reviewer role, stages: Calibration
Reviewer role, model calls: 37
Reviewer role, tokens in: 222,333
Reviewer role, tokens out: 44,455
Reviewer role, tokens cached: 100,001
Writer role, stages: Fix
Writer role, model calls: 9
Writer role, tokens in: 50,505
Writer role, tokens out: 7,007
Writer role, tokens cached: 0
Model claude-haiku-5, model calls: 16
Model claude-haiku-5, stages: Calibration and Fix
Model claude-sonnet-5, model calls: 150
Model claude-sonnet-5, stages: Scan and Calibration
Scan batches, submitted: 0
Scan batches, processing: 1
Scan batches, ended: 4
Scan batches, batch wait: 10m 40s
Calibration batches, submitted: 2
Calibration batches, processing: 0
Calibration batches, ended: 3
Calibration batches, batch wait: not recorded
Fix batches, submitted: 0
Fix batches, processing: 0
Fix batches, ended: 5
Fix batches, batch wait: not recorded
";

#[test]
fn the_copied_text_is_every_figure_one_per_line() {
    assert_eq!(figures().copy_text(), ACME_COPY);
}

#[test]
fn scope_and_tokens_come_from_the_report_and_time_and_batches_from_the_run_folder() {
    let f = figures();
    assert_eq!(value(&f, "Lines in scope"), "48,213");
    assert_eq!(value(&f, "Languages in scope"), "3");
    assert_eq!(value(&f, "Files read"), "311");
    assert_eq!(value(&f, "Wall clock"), "1h 02m 03s");
    assert_eq!(value(&f, "Tokens in"), "1,507,405");

    // Without the folder's figures, the time and the batches are not recorded; the report's
    // figures stand.
    let report_only = RunFigures::new(report(), None);
    assert_eq!(value(&report_only, "Wall clock"), NOT_RECORDED);
    assert_eq!(value(&report_only, "Sessions"), NOT_RECORDED);
    assert_eq!(value(&report_only, "Tokens in"), "1,507,405");
    let batches = report_only.table(FiguresView::Batches);
    assert!(batches.rows.is_empty());
    assert_eq!(batches.empty, "Batch status: not recorded");

    // Without the report's ledger, scope stands where the report has it and no token is made up.
    let mut bare = report();
    bare.ledger = None;
    let folder_only = RunFigures::new(bare, Some(record()));
    assert_eq!(value(&folder_only, "Wall clock"), "1h 02m 03s");
    assert_eq!(value(&folder_only, "Tokens in"), NOT_RECORDED);
    assert_eq!(value(&folder_only, "Files in scope"), NOT_RECORDED);
    assert_eq!(value(&folder_only, "Coverage"), NOT_RECORDED);
    assert_eq!(folder_only.table(FiguresView::Batches).rows.len(), 3);
}

#[test]
fn a_run_that_recorded_nothing_reads_not_recorded_never_zero() {
    let f = RunFigures::new(ReportFigures::default(), None);
    for (figure, value) in f.summary() {
        assert_eq!(value, NOT_RECORDED, "{figure}");
    }
    for view in FiguresView::ALL.iter().skip(1) {
        let table = f.table(*view);
        assert!(table.rows.is_empty(), "{view:?}");
        assert!(
            table.empty.contains(NOT_RECORDED),
            "{view:?}: {}",
            table.empty
        );
    }
    let text = f.copy_text();
    assert!(!text.contains(": 0\n"), "{text}");
    assert!(!text.contains("0%"), "{text}");
}

#[test]
fn a_run_folder_with_no_batch_says_so_and_a_saved_zero_wait_is_not_a_wait() {
    let mut no_batches = record();
    no_batches.batches.clear();
    let f = RunFigures::new(report(), Some(no_batches));
    let t = f.table(FiguresView::Batches);
    assert!(t.rows.is_empty());
    assert_eq!(t.empty, "No stage of this run ran in batches.");

    let t = figures().table(FiguresView::Batches);
    assert_eq!(
        t.headers,
        vec!["Stage", "Submitted", "Processing", "Ended", "Batch wait"]
    );
    assert_eq!(t.rows[0], vec!["Scan", "0", "1", "4", "10m 40s"]);
    assert_eq!(t.rows[1], vec!["Calibration", "2", "0", "3", NOT_RECORDED]);
    assert_eq!(t.rows[2], vec!["Fix", "0", "0", "5", NOT_RECORDED]);
}

#[test]
fn a_stage_with_no_timed_call_has_its_time_not_recorded() {
    let t = figures().table(FiguresView::Stages);
    let fix = t.rows.iter().find(|r| r[0] == "Fix").unwrap();
    assert_eq!(fix[1], NOT_RECORDED, "wall clock");
    assert_eq!(
        fix[2], NOT_RECORDED,
        "busy time, saved as 0 with no timed call"
    );
    assert_eq!(fix[3], "9");
}

#[test]
fn a_report_saved_before_a_figure_existed_leaves_it_not_recorded() {
    let mut value: serde_json::Value = serde_json::from_str(REPORT_JSON).unwrap();
    let usage = &mut value["ledger"]["stage_telemetry"]["stages"][0]["usage"];
    let usage = usage.as_object_mut().unwrap();
    usage.remove("cache_read_input_tokens");
    usage.remove("first_call_unix_ms");
    let old: ReportFigures = serde_json::from_value(value).unwrap();
    let f = RunFigures::new(old, Some(record()));
    let scan = f.table(FiguresView::Stages).rows[0].clone();
    assert_eq!(scan[0], "Scan");
    assert_eq!(scan[1], NOT_RECORDED);
    assert_eq!(scan[6], NOT_RECORDED, "cached");
}

#[test]
fn nothing_from_the_client_repository_beyond_counts_reaches_the_panel() {
    let f = figures();
    let mut shown = f.copy_text();
    for view in FiguresView::ALL {
        let t = f.table(view);
        shown.push_str(&t.headers.join("|"));
        for row in t.rows {
            shown.push_str(&row.join("|"));
        }
    }
    for leak in [
        "acme",
        "src/",
        "ledger_secret",
        "select",
        "SEC-NO-RAW",
        "Rust",
        "TypeScript",
        "SQL",
        "20261124T101500Z",
        "assets/",
    ] {
        assert!(!shown.contains(leak), "`{leak}` reached the panel");
    }
}

#[test]
fn visible_words_have_no_long_dashes() {
    let f = figures();
    let mut words = f.copy_text();
    for view in FiguresView::ALL {
        words.push_str(view.label());
        words.push_str(f.table(view).empty);
    }
    assert!(!words.contains('\u{2014}') && !words.contains('\u{2013}'));
}

#[test]
fn figures_format_as_the_panel_writes_them() {
    assert_eq!(count(0), "0");
    assert_eq!(count(999), "999");
    assert_eq!(count(1_000), "1,000");
    assert_eq!(count(1_507_405), "1,507,405");
    assert_eq!(duration(350), "350 ms");
    assert_eq!(duration(45_000), "45s");
    assert_eq!(duration(245_000), "4m 05s");
    assert_eq!(duration(3_723_000), "1h 02m 03s");
    assert_eq!(and_list(&["a"]), "a");
    assert_eq!(and_list(&["a", "b", "c"]), "a, b and c");
}

#[test]
fn every_saved_stage_has_a_role_and_a_word() {
    for (name, _, _) in STAGES {
        assert_ne!(stage_label(name), "Other", "{name}");
        assert_ne!(stage_role(name), AgentRole::Other, "{name}");
    }
    assert_eq!(stage_label("a_stage_from_later"), "Other");
}
