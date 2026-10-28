//! W13 — A RULE FILE THAT WOULD NOT PARSE MUST NOT VANISH.
//!
//! # The defect
//!
//! Every production corpus load was LENIENT and threw the parse errors away:
//!
//! ```ignore
//! let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;
//! ```
//!
//! `load_corpus_lenient` returns `(RuleSet, Vec<(PathBuf, RulesError)>)` and nothing carried
//! the second element. Note the asymmetry that made this invisible for so long: several
//! `crates/server/tests/*.rs` files bind `corpus_errors` and assert it is empty. PRODUCTION
//! bound `_errs`. So a rule file with a typo'd enum value failed to parse, disappeared from
//! every scan, and NOTHING reported it — not the [`ScanLedger`], not the coverage sheet, not
//! `camerata integrity-gate`, not a log line.
//!
//! # Why that is severe in this product's own terms
//!
//! > **H1 AGREED, with a sharpening** — every rule must be CAPABLE of running. A rule that
//! > does not run because it is irrelevant to the scanned stack is CORRECT. A rule that could
//! > never run for ANY stack is a DEFECT.
//!
//! A build gate (`mechanical_gate`) exists for unreachable domains. This is a SECOND route to
//! the same outcome and it walks under the gate built to stop it: `camerata integrity-gate`'s
//! failure families (`unjustified_skip`, `dark_tier`) are all derived from ledger entries, and
//! a rule that never loaded has no ledger entry. It is not a skip. It is absent from the
//! ledger's universe, so the gate could not see it even in principle.
//!
//! Commercially: a graded sweep of four repos costs roughly $26 and four hours. A silently
//! absent rule means paying to measure a degraded engine and not knowing.
//!
//! # What this file proves, and the red-before evidence
//!
//! [`req_w13_a_rule_whose_file_failed_to_parse_vanished_from_every_surface`] is the
//! DEFECT-REPRODUCTION test, written to fail first. Run against `HEAD` before the fix it
//! reported, in order:
//!
//! - the malformed rule was absent from the loaded corpus (correct, and the premise),
//! - **`report.ledger` had NO entry for it at all** — not `ran = false`, not a skip, nothing,
//! - `report.coverage_notes` was empty,
//! - the report JSON's `rules_not_run` did not mention it,
//! - and `integrity_gate::evaluate` returned **PASS**, exit code 0, over that very scan.
//!
//! The fix routes the discarded information to the four places built for it (the ledger, the
//! gate, one concise client line, the operator log) and every assertion below then holds.
//!
//! # SYNTHETIC, never a benchmark repo
//!
//! Both halves of the fixture are invented. The repo under `tests/fixtures/
//! corpus_load_failure_repo/` is synthetic by construction (see its `Cargo.toml`), and the
//! CORPUS is assembled at test time into a `tempfile` directory from real rule files copied
//! verbatim out of the bundled corpus, with exactly one of them broken by a single-token edit.
//!
//! Copied rather than hand-written on purpose: a hand-written "rule" would only prove the
//! loader rejects a file the real corpus never contains. Taking a REAL file and typo'ing one
//! enum value reproduces the reported shape exactly.
//!
//! # No process state is mutated
//!
//! `camerata_rules::corpus_path()` reads the process-global `CAMERATA_CORPUS_PATH`, and a test
//! that set it would race every other test in the binary. W13 added
//! `split_scannable_rules_from` / `load_corpus_for_scan`, which take the directory as a
//! PARAMETER, so this file points at its temp corpus directly and nothing global moves.
//!
//! # Cost
//!
//! ZERO API SPEND and no network: `run_ai_review: false` throughout.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use camerata_server::ai_audit::ScanMode;
use camerata_server::integrity_gate::{self, IntegrityGateOptions, SkipClass};
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};
use camerata_server::scan_ledger::CORPUS_LOAD_FAILED_REASON;

/// The planted deterministic-floor rule the fixture repo violates. A REAL corpus rule with a
/// real wired detector, so the scan demonstrably ran.
const RULE_SQL_CONCAT: &str = "SEC-NO-RAW-SQL-CONCAT-1";
/// A second real rule, selected and expected to come back genuinely CLEAN — its job is to keep
/// "ran and found nothing" distinguishable from "never ran".
const RULE_SECRETS: &str = "SEC-NO-HARDCODED-SECRETS-1";
/// The rule whose FILE is deliberately broken. A real corpus rule, so the test describes a
/// real regression rather than the loader rejecting an invented file.
const RULE_BROKEN: &str = "SUPABASE-RLS-ENABLED-1";

// ─── fixture staging (per this repo's convention: each e2e file owns its own copy) ──────────

fn stage_fixture_as_git_repo(fixture_name: &str, dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture_name);
    copy_dir_recursive(&fixture_root, dest);

    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    g(&["init", "-q", "-b", "main"]);
    g(&[
        "config",
        "user.email",
        "corpus-load-failure-e2e@camerata.local",
    ]);
    g(&["config", "user.name", "Camerata W13 E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "synthetic W13 fixture"]);
}

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

// ─── the synthetic corpus ───────────────────────────────────────────────────────────────────

/// Locate the real corpus file declaring `rule_id`, returning `(file name, body)`. Searched by
/// CONTENT rather than by a hard-coded path, so a corpus reorganisation does not break this.
fn find_real_rule_file(corpus_root: &Path, rule_id: &str) -> Option<(String, String)> {
    let mut stack = vec![corpus_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "toml") {
                let body = std::fs::read_to_string(&p).ok()?;
                if body.contains(&format!("id = \"{rule_id}\"")) {
                    return Some((p.file_name()?.to_str()?.to_string(), body));
                }
            }
        }
    }
    None
}

/// Break a rule file the way the reported defect did: TYPO ONE ENUM VALUE. `enforcement`
/// deserializes into an enum, so an unknown variant is a TOML deserialize error and
/// `load_corpus_lenient` skips the whole file.
///
/// Returns `None` if nothing changed — a malformed-corpus test over an intact file would pass
/// vacuously, and that is the B10 shape this repo has already paid for once.
fn typo_one_enum_value(body: &str) -> Option<String> {
    for (good, typo) in [
        (
            "enforcement = \"architectural\"",
            "enforcement = \"architectual\"",
        ),
        (
            "enforcement = \"mechanical\"",
            "enforcement = \"mechanicle\"",
        ),
        (
            "enforcement = \"structured\"",
            "enforcement = \"structered\"",
        ),
        ("enforcement = \"prose\"", "enforcement = \"proze\""),
    ] {
        if body.contains(good) {
            return Some(body.replacen(good, typo, 1));
        }
    }
    None
}

/// A corpus directory holding the two intact rules the scan selects plus [`RULE_BROKEN`]'s
/// real file with one typo'd enum value. `None` when the bundled corpus is not checked out, so
/// this file SKIPS rather than fails, matching every other corpus-backed test here.
fn synthetic_corpus() -> Option<tempfile::TempDir> {
    let real = camerata_rules::corpus_path();
    if !real.exists() {
        return None;
    }
    let dir = tempfile::TempDir::new().unwrap();
    for id in [RULE_SQL_CONCAT, RULE_SECRETS] {
        let (name, body) = find_real_rule_file(&real, id)
            .unwrap_or_else(|| panic!("the bundled corpus must carry {id}"));
        std::fs::write(dir.path().join(name), body).unwrap();
    }
    let (name, body) = find_real_rule_file(&real, RULE_BROKEN)
        .unwrap_or_else(|| panic!("the bundled corpus must carry {RULE_BROKEN}"));
    let broken = typo_one_enum_value(&body).unwrap_or_else(|| {
        panic!("{RULE_BROKEN}'s file declares no `enforcement` value to typo; pick another rule")
    });
    std::fs::write(dir.path().join(name), broken).unwrap();
    Some(dir)
}

/// The two intact rules, and nothing else. SHAPE A, the dominant production shape: a project's
/// selection is derived FROM the corpus (`propose_corpus_rules` -> `curated_rule_selection`),
/// so a rule whose file would not parse is never in it. Before the fix the rule was therefore
/// wholly ABSENT from the ledger.
fn selection_without_the_broken_rule() -> Vec<SelectedRule> {
    vec![
        SelectedRule {
            id: RULE_SQL_CONCAT.to_string(),
            directive: "no raw SQL built by string concatenation".to_string(),
            repos: Vec::new(),
        },
        SelectedRule {
            id: RULE_SECRETS.to_string(),
            directive: "no hardcoded secrets".to_string(),
            repos: Vec::new(),
        },
    ]
}

/// SHAPE B: the selection DOES name the broken rule. Reachable because selections are
/// PERSISTED per project while the corpus moves underneath them, so yesterday's valid
/// selection can name today's unparseable file. This shape is the more damning one: before the
/// fix, `audit_repos` recorded the rule with `skip_reason: "AI/semantic review not requested
/// this run"` — an affirmative claim of a JUSTIFIED scope choice for a rule that did not
/// exist, which the integrity gate classified as `ScopedOut` and PASSED.
fn selection_including_the_broken_rule() -> Vec<SelectedRule> {
    let mut v = selection_without_the_broken_rule();
    v.push(SelectedRule {
        id: RULE_BROKEN.to_string(),
        directive: "row-level security is enabled on every table".to_string(),
        repos: Vec::new(),
    });
    v
}

// ─── the scan, assembled exactly as the production handlers assemble it ─────────────────────

/// Everything one scan produced: the report (with its ledger and coverage notes), the report
/// JSON, the workbook, and the corpus-load failures the loader reported.
struct Scan {
    report_json: report_export::AuditReportJson,
    json_value: serde_json::Value,
    workbook: Vec<u8>,
    ledger: camerata_server::scan_ledger::ScanLedger,
    coverage_note_tools: BTreeSet<String>,
    coverage_note_messages: Vec<String>,
    load_failure_paths: Vec<PathBuf>,
}

/// Drive the REAL scan pipeline over the fixture with the malformed corpus, in the SAME order
/// `onboard_audit` / `onboard_audit_start` / `camerata inspect` do it:
/// `split_scannable_rules_from` -> `audit_repos` -> `record_corpus_load_failures` ->
/// `apply_evidence_gate` -> `seal` -> the exporters.
///
/// `record_corpus_load_failures` sits exactly where all three production entry points put it:
/// immediately after `audit_repos` returns, i.e. as soon as a `ScanReport` (and therefore a
/// `ScanLedger`) exists to record into.
async fn scan_with_corpus(corpus_dir: &Path, repo_dir: &Path, selected: Vec<SelectedRule>) -> Scan {
    let (_scannable, excluded_mechanical, _preview, corpus, load_failures) =
        camerata_server::split_scannable_rules_from(corpus_dir, selected.clone()).await;
    let load_failure_paths = load_failures.iter().map(|f| f.path.clone()).collect();

    let sources = vec![("e2e/w13".to_string(), repo_dir.to_path_buf())];
    let (mut report, _manifest) = onboard::audit_repos(
        &camerata_server::credentials::MemoryCredentialStore::new(),
        &sources,
        &selected,
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        false,
        false,
        false, // run_ai_review — ZERO API SPEND
        true,  // run_deterministic
        None,
        camerata_server::llm::BackendResolution::Api,
        corpus.as_ref(),
        &HashMap::new(),
    )
    .await;
    report.excluded_mechanical_rules = excluded_mechanical;
    camerata_server::record_corpus_load_failures(&mut report, &load_failures);

    report_export::apply_evidence_gate(&mut report);
    let ledger = report.ledger.clone();
    let coverage_note_tools = report
        .coverage_notes
        .iter()
        .map(|n| n.tool.clone())
        .collect();
    let coverage_note_messages = report
        .coverage_notes
        .iter()
        .map(|n| n.message.clone())
        .collect();

    let sealed = report.seal();
    let dispositions: HashMap<String, DispositionWire> = HashMap::new();
    let opts = ReportOptions::default();
    let report_json =
        report_export::build_report_json(&sealed, &dispositions, corpus.as_ref(), &opts);
    let json_value = serde_json::to_value(&report_json).unwrap();
    let workbook = camerata_server::xlsx_export::build_workbook(
        &sealed,
        &dispositions,
        corpus.as_ref(),
        &opts,
    )
    .expect("the workbook must build");

    Scan {
        report_json,
        json_value,
        workbook,
        ledger,
        coverage_note_tools,
        coverage_note_messages,
        load_failure_paths,
    }
}

/// Every string in the workbook's shared-strings pool — every cell a client can read.
fn workbook_strings(bytes: &[u8]) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a valid xlsx zip");
    let mut xml = String::new();
    {
        use std::io::Read as _;
        let mut f = zip
            .by_name("xl/sharedStrings.xml")
            .expect("xlsx must carry a shared-strings pool");
        f.read_to_string(&mut xml).unwrap();
    }
    xml.split("<t")
        .skip(1)
        .filter_map(|chunk| {
            let start = chunk.find('>')? + 1;
            let end = chunk.find("</t>")?;
            if end < start {
                return None;
            }
            Some(
                chunk[start..end]
                    .replace("&amp;", "&")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&#39;", "'"),
            )
        })
        .collect()
}

// ═══ ANTI-VACUITY ═══════════════════════════════════════════════════════════════════════════

/// req_w13: the fixture is load-bearing. Every assertion in this file is meaningless if the
/// scan produced nothing or if the "malformed" file actually parsed, so this fails loudly and
/// separately first. (B10 is the cautionary case: a pre-existing gate asserted over a fixture
/// in which the feature it guarded COULD NOT FIRE.)
#[tokio::test]
async fn req_w13_the_malformed_corpus_fixture_is_load_bearing_and_not_vacuous() {
    let Some(corpus_dir) = synthetic_corpus() else {
        eprintln!("corpus unavailable; skipping req_w13 e2e");
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    stage_fixture_as_git_repo("corpus_load_failure_repo", repo.path());
    let scan = scan_with_corpus(
        corpus_dir.path(),
        repo.path(),
        selection_without_the_broken_rule(),
    )
    .await;

    // The scan genuinely ran: the planted violation fired.
    assert!(
        scan.ledger.fired_rule_ids().contains(RULE_SQL_CONCAT),
        "the planted violation must have fired, proving the floor ran over this fixture: {:#?}",
        scan.ledger.rules().collect::<Vec<_>>()
    );
    // And something is honestly recorded as clean, so "did not run" stays distinguishable
    // from "ran and found nothing".
    assert!(
        scan.ledger.rule(RULE_SECRETS).is_some(),
        "the clean rule must be RECORDED one way or the other, never simply absent"
    );
    // The loader really did reject exactly one file, and it is the one we broke.
    assert_eq!(
        scan.load_failure_paths.len(),
        1,
        "the typo must break exactly one file: {:?}",
        scan.load_failure_paths
    );
    assert!(
        scan.load_failure_paths[0].ends_with("supabase-rls-enabled-1.toml"),
        "{:?}",
        scan.load_failure_paths
    );
    // The report has a rules-not-run section, so the export assertions below are not over an
    // empty artifact.
    assert!(
        !scan.report_json.rules_not_run.is_empty(),
        "the `rules_not_run` section must exist for the disclosure assertions to mean anything"
    );
}

// ═══ THE DEFECT, SHAPE A — the rule was wholly ABSENT ═══════════════════════════════════════

/// req_w13: THE REQUIREMENT. A rule whose own `.toml` would not parse is recorded in the
/// ledger, disclosed to the client, FAILS `camerata integrity-gate`, and surfaces in full to
/// an operator.
///
/// SHAPE A is the dominant production shape: the selection is derived FROM the corpus, so a
/// rule that left the corpus is never selected and nothing anywhere mentions it.
///
/// # FAILS BEFORE THE FIX — verified by running an equivalent probe against `HEAD` (`a9a3a07`)
///
/// At that commit the scan path's corpus load was `load_corpus_lenient(&p).await.0`, so:
///
/// 1. `scan.ledger.rule(RULE_BROKEN)` was `None`. Not `ran = false`. Not a skip with a reason.
///    NOTHING. A rule that never loaded is not even a skip — it is absent from the ledger's
///    universe, which is exactly why the gate's `unjustified_skip` / `dark_tier` families
///    could not see it even in principle.
/// 2. `scan.coverage_note_tools` was empty, so no client line existed.
/// 3. `report_json.rules_not_run` never mentioned the rule.
/// 4. `integrity_gate::evaluate(..)` returned PASS with exit code 0 over the very scan in
///    which a rule had silently left the corpus.
#[tokio::test]
async fn req_w13_a_rule_whose_file_failed_to_parse_is_disclosed_and_fails_the_gate() {
    let Some(corpus_dir) = synthetic_corpus() else {
        eprintln!("corpus unavailable; skipping req_w13 e2e");
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    stage_fixture_as_git_repo("corpus_load_failure_repo", repo.path());
    let scan = scan_with_corpus(
        corpus_dir.path(),
        repo.path(),
        selection_without_the_broken_rule(),
    )
    .await;

    // ── 1. THE LEDGER — the honest record of what ran ────────────────────────────────────
    let entry = scan.ledger.rule(RULE_BROKEN).unwrap_or_else(|| {
        panic!(
            "THE DEFECT: the ledger has NO entry for {RULE_BROKEN}, whose corpus file would \
             not parse. Recorded rules: {:#?}",
            scan.ledger
                .rules()
                .map(|r| r.rule_id.as_str())
                .collect::<Vec<_>>()
        )
    });
    assert!(!entry.ran, "a rule that never loaded never ran");
    assert!(
        !entry.verified_clean(),
        "it must never read as verified clean"
    );
    assert_eq!(
        entry.skip_reason.as_deref(),
        Some(CORPUS_LOAD_FAILED_REASON),
        "the reason must be the shared constant verbatim, so the gate's classifier cannot \
         decouple from the recording site"
    );
    assert!(
        !scan
            .ledger
            .healthy_rule_ids()
            .contains(&RULE_BROKEN.to_string()),
        "it must not appear in What's healthy: {:?}",
        scan.ledger.healthy_rule_ids()
    );

    // ── 2. THE GATE — the load-bearing requirement ───────────────────────────────────────
    // The SMOKE profile, deliberately: the RELAXED one a cheap local scan uses. If even that
    // tolerated an unparseable rule file, the gate would be useless for the case it is for.
    let verdict = integrity_gate::evaluate(&scan.ledger, &IntegrityGateOptions::smoke());
    assert!(
        !verdict.passed(),
        "THE LOAD-BEARING REQUIREMENT: the integrity gate must FAIL a scan in which a rule \
         file would not parse. It PASSED: {verdict:#?}"
    );
    assert_eq!(verdict.exit_code(), 1);
    let gate_hits: Vec<&str> = verdict
        .failures
        .iter()
        .filter(|f| f.code == "corpus_load_failure")
        .map(|f| f.detail.as_str())
        .collect();
    assert_eq!(
        gate_hits.len(),
        1,
        "exactly one `corpus_load_failure`, in its OWN family rather than borrowed from \
         `unjustified_skip` (which means something else and sends an operator to the wrong \
         place): {:#?}",
        verdict.failures
    );
    assert!(gate_hits[0].contains(RULE_BROKEN), "{}", gate_hits[0]);
    assert_eq!(
        SkipClass::classify(entry.skip_reason.as_deref()),
        SkipClass::CorpusLoadFailed
    );
    assert!(
        !SkipClass::classify(entry.skip_reason.as_deref()).is_justified(),
        "an unparseable rule file is never a justified skip"
    );

    // ── 3. THE CLIENT, CONCISELY (G3) ────────────────────────────────────────────────────
    assert!(
        scan.coverage_note_tools
            .contains(camerata_server::RULE_CORPUS_COVERAGE_TOOL),
        "the coverage-note channel that renders the client line must carry it: {:?}",
        scan.coverage_note_tools
    );
    let client_lines: Vec<&String> = scan
        .report_json
        .executive_summary
        .failed_passes
        .iter()
        .filter(|n| n.contains("rule library"))
        .collect();
    assert_eq!(
        client_lines.len(),
        1,
        "G3: ONE concise line, never one per broken file: {:?}",
        scan.report_json.executive_summary.failed_passes
    );
    assert_eq!(
        client_lines[0],
        "1 rule in the rule library could not be loaded this scan and did not run; anything \
         they would have reported is not reflected in this report.",
        "the exact client-facing wording is pinned: it ships in a client PDF"
    );
    // And the rule is named in the "rules not run this scan" surface both artifacts render.
    assert!(
        scan.report_json
            .rules_not_run
            .iter()
            .any(|r| r.rule_id == RULE_BROKEN),
        "the report JSON's `rules_not_run` must name it: {:#?}",
        scan.report_json.rules_not_run
    );
    let cells = workbook_strings(&scan.workbook);
    assert!(
        cells.iter().any(|c| c == RULE_BROKEN),
        "the workbook's coverage sheet must name it too — a never-run rule that reaches no \
         artifact reads to the client as checked-and-clean"
    );

    // ── 4. THE OPERATOR, IN FULL ─────────────────────────────────────────────────────────
    let operator_detail = scan
        .coverage_note_messages
        .iter()
        .find(|m| m.contains(RULE_BROKEN))
        .expect("an operator-facing note must carry the detail");
    assert!(
        operator_detail.contains("supabase-rls-enabled-1.toml"),
        "the operator must learn WHICH FILE: {operator_detail}"
    );
    assert!(
        operator_detail.contains("architectual") || operator_detail.to_lowercase().contains("toml"),
        "the operator must learn WHY, from the parser's own message — this is the W6 lesson \
         (stderr discarded, leaving an operator staring at a parse error about column 0 of an \
         empty document) held the right way round: {operator_detail}"
    );
}

// ═══ THE DEFECT, SHAPE B — the ledger claimed a JUSTIFIED skip for a rule that did not exist ═

/// req_w13: the second, worse shape. When the project's PERSISTED selection still names a rule
/// whose file has since broken, `audit_repos` records it — with an incidental reason.
///
/// # FAILS BEFORE THE FIX, and this is the shape the first cut of the fix ALSO failed
///
/// At `HEAD` the ledger held `skip_reason: "AI/semantic review not requested this run"` for a
/// rule that did not exist. That classifies as `SkipClass::ScopedOut`, i.e. JUSTIFIED, so the
/// gate reported it under "justified skips (correct behaviour)" and PASSED — an affirmative
/// false claim, strictly worse than the absence in shape A.
///
/// The first cut of this fix used `ScanLedger::record_rule`, whose accumulate semantics keep
/// the FIRST not-run reason, so it left that claim exactly where it was and this test caught
/// it (`left: Some("AI/semantic review not requested this run")`). The root cause must win.
#[tokio::test]
async fn req_w13_a_stale_selection_cannot_report_an_unparseable_rule_as_a_justified_skip() {
    let Some(corpus_dir) = synthetic_corpus() else {
        eprintln!("corpus unavailable; skipping req_w13 e2e");
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    stage_fixture_as_git_repo("corpus_load_failure_repo", repo.path());
    let scan = scan_with_corpus(
        corpus_dir.path(),
        repo.path(),
        selection_including_the_broken_rule(),
    )
    .await;

    let entry = scan
        .ledger
        .rule(RULE_BROKEN)
        .expect("a selected rule is recorded either way; the question is WITH WHAT REASON");
    assert_eq!(
        entry.skip_reason.as_deref(),
        Some(CORPUS_LOAD_FAILED_REASON),
        "the ROOT cause must override the incidental one. Anything else means the ledger \
         affirmatively claims a justified scope choice for a rule that does not exist"
    );

    let verdict = integrity_gate::evaluate(&scan.ledger, &IntegrityGateOptions::smoke());
    assert!(
        !verdict
            .justified_skips
            .values()
            .any(|ids| ids.contains(&RULE_BROKEN.to_string())),
        "it must NEVER be filed under justified skips: {:#?}",
        verdict.justified_skips
    );
    assert!(
        verdict
            .failures
            .iter()
            .any(|f| f.code == "corpus_load_failure" && f.detail.contains(RULE_BROKEN)),
        "{:#?}",
        verdict.failures
    );
    assert_eq!(verdict.exit_code(), 1);
}

// ═══ THE CLIENT ARTIFACTS CARRY NO OPERATOR DETAIL ══════════════════════════════════════════

/// req_w13: the operator detail must NOT reach the client's artifacts. The full path names the
/// reviewer's own filesystem and the `RulesError` text names TOML internals; G3's ruling is
/// that coverage is client-facing but "never an internal diagnostic dump".
///
/// Asserted over the WHOLE serialised report JSON and the WHOLE workbook string pool, not a
/// named field, because the property is "it is nowhere in the deliverable" rather than "it is
/// absent from today's field".
#[tokio::test]
async fn req_w13_the_client_artifacts_carry_no_file_path_and_no_parser_text() {
    let Some(corpus_dir) = synthetic_corpus() else {
        eprintln!("corpus unavailable; skipping req_w13 e2e");
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    stage_fixture_as_git_repo("corpus_load_failure_repo", repo.path());
    let scan = scan_with_corpus(
        corpus_dir.path(),
        repo.path(),
        selection_including_the_broken_rule(),
    )
    .await;

    let doc = serde_json::to_string(&scan.json_value).unwrap();
    let cells = workbook_strings(&scan.workbook).join("\n");
    let broken_path = scan.load_failure_paths[0].display().to_string();
    let broken_file_name = scan.load_failure_paths[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    for (surface, body) in [("report JSON", &doc), ("workbook cells", &cells)] {
        for leak in [
            broken_path.as_str(),
            broken_file_name.as_str(),
            "architectual",
            "unknown variant",
            "TOML parse error",
            camerata_server::RULE_CORPUS_COVERAGE_TOOL,
        ] {
            assert!(
                !body.contains(leak),
                "the {surface} must not carry the operator-only detail {leak:?} — the full \
                 path and parser message belong in the log and the internal coverage note, \
                 never in a client deliverable"
            );
        }
    }
}

// ═══ THE CONTROL — the gate must stay switched on ═══════════════════════════════════════════

/// req_w13 + H1's other half: the identical scan over an INTACT corpus must PASS, and must
/// acquire no `corpus_load_failure` and no client line.
///
/// A gate that cries wolf gets disabled, which costs more than the defect it guarded. Without
/// this control, every assertion above could be satisfied by a gate that fails every scan.
#[tokio::test]
async fn req_w13_the_same_scan_over_an_intact_corpus_still_passes_the_gate() {
    let real = camerata_rules::corpus_path();
    if !real.exists() {
        eprintln!("corpus unavailable; skipping req_w13 control");
        return;
    }
    // The same corpus with the SAME three rules, none broken — so the ONLY difference from the
    // tests above is the one typo'd enum value.
    let corpus_dir = tempfile::TempDir::new().unwrap();
    for id in [RULE_SQL_CONCAT, RULE_SECRETS, RULE_BROKEN] {
        let (name, body) = find_real_rule_file(&real, id).unwrap();
        std::fs::write(corpus_dir.path().join(name), body).unwrap();
    }
    let repo = tempfile::tempdir().unwrap();
    stage_fixture_as_git_repo("corpus_load_failure_repo", repo.path());
    let scan = scan_with_corpus(
        corpus_dir.path(),
        repo.path(),
        selection_including_the_broken_rule(),
    )
    .await;

    assert!(
        scan.load_failure_paths.is_empty(),
        "control: an intact corpus reports no load failures: {:?}",
        scan.load_failure_paths
    );
    assert!(
        !scan
            .coverage_note_tools
            .contains(camerata_server::RULE_CORPUS_COVERAGE_TOOL),
        "control: no client line when nothing failed to load"
    );
    assert!(
        !scan
            .report_json
            .executive_summary
            .failed_passes
            .iter()
            .any(|n| n.contains("rule library")),
        "control: the client artifact must not mention the rule library at all: {:?}",
        scan.report_json.executive_summary.failed_passes
    );

    let verdict = integrity_gate::evaluate(&scan.ledger, &IntegrityGateOptions::smoke());
    assert!(
        !verdict
            .failures
            .iter()
            .any(|f| f.code == "corpus_load_failure"),
        "control: a correct scan must acquire NO corpus_load_failure. A gate that cries wolf \
         on a correct scan gets switched off: {:#?}",
        verdict.failures
    );
    assert!(
        verdict.passed(),
        "control: a clean synthetic scan must pass the smoke profile: {:#?}",
        verdict.failures
    );
}
