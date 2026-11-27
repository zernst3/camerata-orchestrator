//! No path on the operator's machine reaches a client file (#557).
//!
//! Two defenses, tested apart. The note a stored scan wrote for a dependency check that did not
//! run can carry osv-scanner's own stderr, and that holds paths of the machine that ran it; the
//! export renders fixed wording for such a note, so the stored text never reaches a client file.
//! And the release gate refuses a package in which any file names the home directory, a clone or
//! scan folder, or a scratch folder of the operator, whatever put it there. A client's own code
//! quotes paths like `/home/node/app`, so the gate matches the operator's own folders, never
//! `/home/` or the macOS home root as such.

use std::collections::{BTreeMap, HashMap};

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::CoverageNote;
use crate::report_export::{build_report_json, render_data_json, ReviewedReport};

const STEM: &str = "acme-inspection";
const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the gate tests";

/// The label the gate prints for the check (see `CheckId::label`).
const CHECK_LABEL: &str = "operator paths kept out";

/// The sentence a client reads in place of the tool's own words.
const FIXED_WORDING: &str = "dependency audit (osv-scanner) did not run on this repository";

fn osv_note(message: &str) -> CoverageNote {
    CoverageNote {
        tool: "osv-scanner".to_string(),
        message: message.to_string(),
        repo: None,
        tool_failed: false,
    }
}

/// A package from a report whose dependency audit ran (so the only dependency text is the notes).
fn package_of(
    notes: Vec<CoverageNote>,
    options: ReportOptions,
    edit: impl FnOnce(&mut crate::onboard::ScanReport),
) -> ProductPackage {
    let mut report = scan_report();
    report.provenance.osv_scanner_version = Some("1.9.2".to_string());
    report.coverage_notes = notes;
    edit(&mut report);
    let sealed = report.seal();
    let json = build_report_json(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

/// Every text a client of the package can read, by file: the PDF's data, every archive member's
/// text, and every workbook string.
fn client_texts(data_json: String, zip: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("PDF data".to_string(), data_json);
    let entries = read_zip_entries(zip).expect("the package is a zip");
    for (name, bytes) in entries {
        if name.ends_with(".xlsx") {
            out.insert(
                format!("{name} (strings)"),
                xlsx_strings(&bytes).expect("the workbook reads").join("\n"),
            );
        } else {
            out.insert(name, String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    out
}

/// The PDF's data and the released zip of `package`.
fn released(package: ProductPackage) -> (String, Vec<u8>) {
    let data_json = package.data_json.clone();
    let zip = release(package)
        .unwrap_or_else(|e| panic!("a package with fixed wording clears the gate: {e}"))
        .bytes;
    (data_json, zip)
}

// ---- export time: a stored note that carries the tool's own words -----------------------------

/// The stored forms a scan build wrote, each with a path of the operator's machine that is NOT the
/// scan folder: the process-exit form, a provisioning failure, a spawn failure, an unreadable
/// result, and the unread-files sentence that quotes stderr lines.
const STORED_FORMS: [(&str, &str); 5] = [
    (
        "process exit",
        "dependency audit (osv-scanner) did not run: process exited with code 2: failed to open \
         /Users/auditor/Library/Application Support/osv-scanner/db.json: permission denied",
    ),
    (
        "provisioning",
        "dependency audit (osv-scanner) did not run: `go install` failed: cannot write \
         /Users/auditor/go/pkg/mod/cache/x: no space left",
    ),
    (
        "spawn",
        "dependency audit (osv-scanner) did not run: could not spawn process: \
         /Users/auditor/.camerata/tooling/osv-scanner: bad CPU type",
    ),
    (
        "unparsed output",
        "dependency audit (osv-scanner) could not parse output: unexpected token while reading \
         /Users/auditor/Library/Caches/osv/out.json",
    ),
    (
        "unread files",
        "Dependency files not read: osv-scanner reported problems reading some files, so they may \
         not have been checked (failed to parse /Users/auditor/projects/acme/package-lock.json: \
         bad json).",
    ),
];

/// A stored note holding a path of the operator's machine reaches no client file, and the client
/// still reads that the check did not run: through the PDF's data, `json/inspection.json`, the
/// workbook, and every CSV and JSON dataset.
#[test]
fn a_stored_osv_note_with_an_operator_path_exports_as_fixed_wording_on_every_surface() {
    for (form, message) in STORED_FORMS {
        let package = package_of(vec![osv_note(message)], client_options(), |_| {});
        let (data_json, zip) = released(package);
        let texts = client_texts(data_json, &zip);
        let leaking: Vec<&String> = texts
            .iter()
            .filter(|(_, text)| text.contains("/Users/auditor"))
            .map(|(file, _)| file)
            .collect();
        assert!(
            leaking.is_empty(),
            "{form}: the operator's path is in {leaking:?}"
        );
        // The unread-files form says some files were not read; the others say the check did not run.
        let wording = if form == "unread files" {
            "problems reading some files"
        } else {
            FIXED_WORDING
        };
        for file in [
            "PDF data",
            "json/inspection.json",
            "acme-inspection-findings.xlsx (strings)",
        ] {
            assert!(
                texts[file].to_lowercase().contains(&wording.to_lowercase()),
                "{form}: {file} does not say the check did not run (wanted {wording:?})"
            );
        }
    }
}

/// A reason the engine wrote itself, from the fixed set it owns, stays in client text on every
/// surface: it holds no word of the tool's. A stderr reason still becomes the fixed wording (the
/// test above).
#[test]
fn a_stored_engine_reason_stays_in_client_text_on_every_surface() {
    for reason in [
        "no network",
        "subprocess timed out after 120 s",
        "it exited normally but produced no output",
    ] {
        let note = format!("dependency audit (osv-scanner) did not run: {reason}");
        let package = package_of(vec![osv_note(&note)], client_options(), |_| {});
        let (data_json, zip) = released(package);
        let texts = client_texts(data_json, &zip);
        for file in [
            "PDF data",
            "json/inspection.json",
            "acme-inspection-findings.xlsx (strings)",
        ] {
            assert!(
                texts[file].contains(reason),
                "{reason:?} is not in {file}, so the client lost why the audit did not run"
            );
        }
    }
}

/// The folders handed to the gate: the workspace root, a repo's own folder when the operator set
/// one, and `<workspace root>/<owner>/<repo>` for the others, whether or not they exist.
#[test]
fn the_export_roots_are_the_workspace_root_each_override_and_each_resolved_repo_folder() {
    let state = crate::AppState::new(std::sync::Arc::new(
        camerata_worktracker::InMemoryStoryStore::new(),
    ));
    state
        .settings
        .set_workspace_root(Some("/srv/ws-acme/work".to_string()));
    state
        .settings
        .set_repo_path("acme/portal", Some("/srv/other/portal-clone".to_string()));
    let project = state
        .projects
        .create(
            "Acme",
            vec!["acme/portal".to_string(), "acme/api".to_string()],
        )
        .expect("project created");
    let roots = state.export_operator_roots(&project);
    let expected: Vec<std::path::PathBuf> = [
        "/srv/ws-acme/work",
        "/srv/other/portal-clone",
        "/srv/ws-acme/work/acme/api",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    assert_eq!(roots, expected);
}

/// Without a workspace root or an override, a repo has no folder to name.
#[test]
fn the_export_roots_are_empty_when_no_folder_is_known() {
    let state = crate::AppState::new(std::sync::Arc::new(
        camerata_worktracker::InMemoryStoryStore::new(),
    ));
    let project = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project created");
    assert!(state.export_operator_roots(&project).is_empty());
}

/// The route: a report that names the workspace folder is refused by the product export, with the
/// folder in the problem the operator reads.
#[tokio::test]
async fn the_product_export_route_refuses_a_report_that_names_the_workspace_folder() {
    let state = crate::AppState::new(std::sync::Arc::new(
        camerata_worktracker::InMemoryStoryStore::new(),
    ));
    state
        .settings
        .set_workspace_root(Some("/srv/ws-acme/work".to_string()));
    let project = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project created");
    let project = state
        .projects
        .update(&project.id, |p| {
            p.client_name = "Example Client Ltd".to_string()
        })
        .expect("project exists");
    let mut report = scan_report();
    report.findings[0].snippet = "open(\"/srv/ws-acme/work/acme/portal/src/db.rs\")".to_string();
    report.findings[0].detail =
        "could not read /srv/ws-acme/work/acme/portal/src/db.rs".to_string();
    state.set_last_scan(project.id.clone(), report);

    let response = crate::export_product(
        axum::extract::State(state),
        axum::extract::Path(project.id.clone()),
        axum::Json(crate::AuditReportReq {
            dispositions: HashMap::new(),
            options: ReportOptions::default(),
        }),
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains(CHECK_LABEL), "{body}");
    assert!(body.contains("/srv/ws-acme/work"), "{body}");
}

/// Where the dependency notes reach: the PDF's data, `json/inspection.json` and the workbook carry
/// them; no CSV or JSON dataset does, so the rewrite has nothing to do there. A note that needs no
/// rewrite (it holds none of the tool's words) is the probe.
#[test]
fn the_dependency_notes_reach_the_pdf_the_inspection_json_and_the_workbook_only() {
    let probe = "Dependency manifests not audited (no lockfile): web/package.json.";
    let package = package_of(vec![osv_note(probe)], client_options(), |_| {});
    let (data_json, zip) = released(package);
    let texts = client_texts(data_json, &zip);
    let carrying: Vec<&str> = texts
        .iter()
        .filter(|(_, text)| text.contains("Dependency manifests not audited"))
        .map(|(file, _)| file.as_str())
        .collect();
    assert_eq!(
        carrying,
        [
            "PDF data",
            "acme-inspection-findings.xlsx (strings)",
            "json/inspection.json"
        ],
        "the files that carry a dependency note"
    );
}

// ---- the gate: the operator's own folders -----------------------------------------------------

fn operator_path_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check.label() == CHECK_LABEL)
        .flat_map(|o| o.problems.iter())
        .collect()
}

fn refused(result: Result<ReleasedZip, ReleaseError>, what: &str) -> GateFailure {
    match result {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("{what}: expected the gate to block, got {other}"),
        Ok(_) => panic!("{what}: a package naming an operator folder was released"),
    }
}

fn file_of(surface: super::tests::Surface) -> &'static str {
    use super::tests::Surface;
    match surface {
        Surface::DataJson => "PDF data (data.json)",
        Surface::FindingsJson => "json/inspection.json",
        Surface::DatasetJson => "json/deviations.json",
        Surface::DatasetCsv => "csv/conventions.csv",
        Surface::XlsxSharedString => "acme-inspection-findings.xlsx",
        Surface::Readme => "README.txt",
        Surface::Stem => "(archive member name)",
    }
}

/// The gate refuses a package that names `path`, on every surface it has, naming the file and the
/// path.
fn assert_refused_everywhere(options: impl Fn() -> ReportOptions, path: &str, what: &str) {
    for surface in super::tests::ALL_SURFACES {
        let mut package = package_of(Vec::new(), options(), |_| {});
        super::tests::plant(
            &mut package,
            surface,
            &format!("could not read {path}/src/lib.rs"),
        );
        let failure = refused(release(package), &format!("{what} on {surface:?}"));
        let problems = operator_path_problems(&failure);
        let file = file_of(surface);
        assert!(
            problems
                .iter()
                .any(|p| p.file == file && p.evidence.as_deref() == Some(path)),
            "{what} on {surface:?}: no problem naming {file} and {path}: {failure}"
        );
    }
}

/// The home directory at export time.
#[test]
fn the_gate_refuses_the_operators_home_directory_on_every_surface() {
    let Some(home) = dirs::home_dir() else { return };
    let home = home.to_string_lossy().into_owned();
    assert_refused_everywhere(client_options, &home, "the home directory");
}

/// A clone or scan folder the export was told about.
#[test]
fn the_gate_refuses_a_clone_or_scan_root_on_every_surface() {
    let root = "/srv/clones/acme/portal";
    assert_refused_everywhere(
        || ReportOptions {
            operator_roots: vec![std::path::PathBuf::from(root)],
            ..client_options()
        },
        root,
        "a scan root",
    );
}

/// The folder the engine writes its scan copies to, and the temp folder.
#[test]
fn the_gate_refuses_the_engines_scratch_and_temp_folders_on_every_surface() {
    let scratch = crate::scan_mirror::mirrors_root();
    assert_refused_everywhere(
        client_options,
        &scratch.to_string_lossy(),
        "the scratch folder",
    );
    let temp = std::env::temp_dir();
    // `/tmp` alone is a folder a client's code quotes; only a temp folder with its own name is
    // the operator's (macOS keeps one under /var/folders).
    if temp.components().count() > 3 {
        let temp = temp.to_string_lossy().trim_end_matches('/').to_string();
        assert_refused_everywhere(client_options, &temp, "the temp folder");
    }
}

/// A client's own code quotes paths like `/home/node/app`; that is no reason to refuse.
#[test]
fn a_client_excerpt_with_a_home_style_path_does_not_trip_the_gate() {
    let package = package_of(Vec::new(), client_options(), |report| {
        report.findings[0].snippet = "WORKDIR /home/node/app".to_string();
        report.findings[0].detail =
            "the image runs as /home/app and writes /home/deploy/cache".to_string();
    });
    let released = release(package).unwrap_or_else(|e| panic!("a client excerpt tripped it: {e}"));
    assert!(released.report.check_names().contains(&CHECK_LABEL));
}

/// No known home folder: the check could not look for it, so the gate refuses, to the operator.
#[test]
fn the_gate_refuses_a_package_when_the_home_folder_is_not_known() {
    let package = package_of(Vec::new(), client_options(), |_| {});
    let (_, zip) = released(package);
    let report = audit_zip_with_home(&zip, None, false, true, &[], None);
    let problems: Vec<&Problem> = report
        .checks
        .iter()
        .filter(|o| o.check.label() == CHECK_LABEL)
        .flat_map(|o| o.problems.iter())
        .collect();
    assert!(
        problems
            .iter()
            .any(|p| p.message.contains("home folder could not be read")),
        "{problems:?}"
    );
}
