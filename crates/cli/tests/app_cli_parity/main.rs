#![allow(clippy::unwrap_used, clippy::expect_used)]
//! App and command line parity (#280): the same inputs through the app's scan job and through
//! `camerata inspect` give the same sealed result.
//!
//! One neutral two-repo project (`acme/api` and `acme/web`) is inspected twice over the SAME
//! folders with the SAME scripted model replies: once through `run_inspect_pipeline_with` (the
//! command line's pipeline, its `Seams` carrying the scripted model) and once through the app's
//! own HTTP routes (`POST /api/onboard/audit/start`, then `POST /api/projects/:id/product-export`)
//! with the scripted model installed through `camerata_server::scan_job_llm` (test builds only).
//! No network, no keychain and no live model: the API backend is chosen with a placeholder key
//! that nothing sends, because every model call goes to the scripted model.
//!
//! The fixture holds one file of each kind the engine treats specially:
//! - `acme/api` `assets/bundle.js` is over the read limit and not excluded: it is left out of the
//!   inspection (and still searched for secrets);
//! - `acme/web` `generated/` is excluded by agreement (`web/generated/**` on both paths);
//! - `acme/api` `src/config.ts` holds a vendor token the deterministic floor always flags.
//!
//! The two export packages are compared on: the findings (rule id, repo, file, line, severity,
//! kind), the Coverage counts and the not-read list, the excluded-by-agreement list, the package's
//! file list, the body of every JSON file in it, the README and CSV files, every entry of the
//! workbook and (when both were built in the same minute) the PDF byte for byte. The number of
//! model calls each path made is compared too. Only values that legitimately differ between two
//! runs are normalized, each one named in `package.rs` with the reason. Every difference that
//! remains is listed in the failure message, so one run names them all.

mod compare;
mod fixture;
mod package;
mod paths;

use compare::differences;
use fixture::stage_project;
use package::sealed;
use paths::{cli_rule_selection, run_app, run_cli};

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn the_app_and_the_command_line_seal_the_same_result_for_the_same_inputs() {
    if !camerata_test_env::typst_on_path_for(
        "the_app_and_the_command_line_seal_the_same_result_for_the_same_inputs",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dirs = stage_project(&root);

    let (cli_zip, cli_calls) = run_cli(&root, &dirs).await;
    let rules = cli_rule_selection(&dirs).await;
    let (app_zip, app_calls) = run_app(&root, rules).await;

    let cli = sealed(&cli_zip, &root);
    let app = sealed(&app_zip, &root);

    // The fixture did what it is for, on the command line's side (the reference): a planted
    // token found, the scripted model's finding kept, a file left out over the limit and a file
    // excluded by agreement.
    assert!(
        cli.findings
            .iter()
            .any(|f| f.0 == "SEC-NO-VENDOR-TOKEN-1" && f.2 == "src/config.ts"),
        "the planted token is found: {:?}",
        cli.findings
    );
    assert!(
        cli.findings
            .iter()
            .any(|f| f.0 == "AI-ACME-GREETING-ESCAPE-1" && f.1 == "acme/web"),
        "the scripted model's finding is kept: {:?}",
        cli.findings
    );
    assert!(
        cli.not_read.iter().any(
            |(_, path, reason)| path == "assets/bundle.js" && reason != "excluded by agreement"
        ),
        "the bundle over the limit is left out: {:?}",
        cli.not_read
    );
    assert_eq!(
        cli.excluded
            .iter()
            .map(|(_, p)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["generated/client.ts"],
        "the generated folder is excluded by agreement"
    );

    let mut diffs = differences(&app, &cli);
    if app_calls != cli_calls {
        diffs.push(format!(
            "model calls:\n  app: {app_calls}\n  cli: {cli_calls}"
        ));
    }
    assert!(
        diffs.is_empty(),
        "the app and the command line sealed different results for the same inputs \
         ({} differences):\n\n{}",
        diffs.len(),
        diffs.join("\n\n")
    );
}
