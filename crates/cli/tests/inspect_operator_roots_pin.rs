//! `camerata inspect` tells the release gate the folders of the repos it read, so the gate's
//! clone-root check runs on the CLI export too (#557). A resumed run restores the arguments it
//! started with (the repo folders among them) and goes through the same pipeline and the same
//! export, so one call site covers a new run and a resume. Source pins: the export is the one
//! place that builds the package, and a later edit that drops the folders fails here.

const SOURCE: &str = include_str!("../src/inspect_cmd.rs");

fn body_of<'a>(start: &str) -> &'a str {
    let at = SOURCE.find(start).unwrap_or_else(|| panic!("{start}"));
    let rest = &SOURCE[at..];
    &rest[..rest.find("\n}\n").expect("the end of the function")]
}

#[test]
fn the_export_hands_the_gate_the_repo_folders_of_the_run() {
    let export = body_of("async fn release_product_export(");
    assert!(
        export.contains("options.operator_roots = repo_dirs"),
        "the CLI export must set operator_roots from the run's repo folders"
    );
    assert!(
        SOURCE.contains(
            "&args.repo_dirs(),\n    )\n    .await\n    {\n        Ok(released) => released,"
        ),
        "the pipeline's export must pass the run's repo folders to release_product_export"
    );
}

#[test]
fn a_resume_runs_the_pipeline_on_the_arguments_the_run_saved() {
    // `resume_inspect` is `resume_inspect_controlled` on a fresh run control (#583).
    assert!(
        body_of("pub async fn resume_inspect(").contains("resume_inspect_controlled("),
        "resume_inspect must go through resume_inspect_controlled"
    );
    let resume = body_of("pub async fn resume_inspect_controlled(");
    assert!(
        resume.contains("serde_json::from_value(record.args.clone())"),
        "a resume must restore the saved arguments, repo folders included"
    );
    assert!(
        resume.contains("run_inspect_pipeline_controlled("),
        "a resume must go through the pipeline whose export passes the repo folders"
    );
    // The pipeline a resume runs is the one whose export passes the repo folders.
    assert!(
        body_of("pub async fn run_inspect_pipeline_controlled(").contains(
            "&args.repo_dirs(),\n    )\n    .await\n    {\n        Ok(released) => released,"
        ),
        "the controlled pipeline must pass the run's repo folders to release_product_export"
    );
}
