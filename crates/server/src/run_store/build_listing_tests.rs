//! Issue #341 (V1 item 39, steps B and C): what the listing says about the build a saved run was
//! started on, the refusal that names where that build is kept, and the scanning build as the
//! package states it. Synthetic data only (acme names, /Users/auditor paths).

use super::*;

const OTHER_COMMIT: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b";
const KEPT_FOLDER: &str = "/Users/auditor/camerata-scan-acme";

/// Rewrite the saved record of `run_id` the way another build would have left it.
fn patch_record(run_id: &str, patch: impl FnOnce(&mut serde_json::Value)) {
    let path = runs_dir().join(run_id).join("run.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    patch(&mut value);
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn interrupted_cockpit_run(repo: &tempfile::TempDir) -> String {
    let store = cockpit_run(repo);
    let id = store.run_id();
    drop(store);
    id
}

/// An unfinished cockpit run started on another commit is listed with the build it started on,
/// the folder that build was made in, `other_build` and the refusal in plain words, and is not
/// offered for "Run what remains". One on this build says which build it started on and stays
/// resumable.
#[test]
#[serial_test::serial(process_env)]
fn a_run_started_on_another_build_is_listed_as_such_and_not_resumable_here() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let here = interrupted_cockpit_run(&repo);
    let elsewhere = interrupted_cockpit_run(&repo);
    patch_record(&elsewhere, |v| {
        v["pin"]["build"]["commit"] = serde_json::json!(OTHER_COMMIT);
        v["pin"]["build"]["built_from"] = serde_json::json!(KEPT_FOLDER);
    });
    let listed = list_inspections_in(&runs_dir(), |_| true);
    let row = |id: &str| listed.iter().find(|r| r.id == id).unwrap().clone();

    let other = row(&elsewhere);
    assert!(other.other_build, "{other:?}");
    assert!(!other.resumable_here, "{other:?}");
    assert_eq!(other.started_on_build.as_deref(), Some("1a2b3c4d5e6f"));
    assert_eq!(other.built_from.as_deref(), Some(KEPT_FOLDER));
    let refusal = other.build_refusal.clone().expect("the refusal is listed");
    assert!(
        refusal.contains("started on build 1a2b3c4d5e6f"),
        "{refusal}"
    );
    assert!(refusal.contains("must finish it"), "{refusal}");
    assert!(refusal.contains(KEPT_FOLDER), "{refusal}");

    let same = row(&here);
    assert!(!same.other_build, "{same:?}");
    assert!(same.resumable_here, "{same:?}");
    assert_eq!(same.started_on_build.as_deref(), Some("aaaaaaaaaaaa"));
    assert_eq!(same.build_refusal, None);
}

/// The listing serializes the three new fields under their names (the app reads them).
#[test]
#[serial_test::serial(process_env)]
fn the_listing_carries_the_build_fields_on_the_wire() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let id = interrupted_cockpit_run(&repo);
    patch_record(&id, |v| {
        v["pin"]["build"]["commit"] = serde_json::json!(OTHER_COMMIT);
    });
    let listed = list_inspections_in(&runs_dir(), |_| true);
    let wire = serde_json::to_value(&listed[0]).unwrap();
    assert_eq!(wire["started_on_build"], "1a2b3c4d5e6f", "{wire}");
    assert_eq!(wire["built_from"], serde_json::Value::Null, "{wire}");
    assert_eq!(wire["other_build"], true, "{wire}");
    assert_eq!(wire["resumable_here"], false, "{wire}");
}

/// Refusal 3 names the folder the saved build was made in when the pin recorded it, and reads as
/// before when it did not.
#[test]
#[serial_test::serial(process_env)]
fn the_other_build_refusal_names_the_kept_folder_when_it_is_known() {
    let (_s, _g, _repo, mut record) = pinned_record();
    let pin = record.pin.as_mut().unwrap();
    pin.build.commit = Some(OTHER_COMMIT.to_string());
    let now = engine_pin::BuildId::current();
    let Err(RunStoreError::Refused(plain)) = resume_on(&record, &now, None) else {
        panic!("another commit refuses");
    };
    assert!(
        plain.contains("Open build 1a2b3c4d5e6f to run what remains"),
        "{plain}"
    );
    assert!(!plain.contains("folder"), "{plain}");

    record.pin.as_mut().unwrap().build.built_from = Some(KEPT_FOLDER.to_string());
    let Err(RunStoreError::Refused(named)) = resume_on(&record, &now, None) else {
        panic!("another commit refuses");
    };
    assert!(
        named.contains(&format!(
            "Open build 1a2b3c4d5e6f from the folder {KEPT_FOLDER} to run what remains"
        )),
        "{named}"
    );
    assert_eq!(
        engine_pin::other_build_refusal(record.pin.as_ref(), &now).as_deref(),
        Some(named.as_str()),
        "the listing and the resume give the same sentence"
    );
}

/// A pin saved before `built_from` existed still loads, with no folder.
#[test]
fn a_pin_saved_before_the_folder_was_recorded_still_loads() {
    let json = serde_json::json!({
        "build": {"version": "0.4.0", "commit": OTHER_COMMIT, "dirty": false,
                  "prompt_schema": 1, "clone_prompt_schema": 1},
        "corpus_version": "sha256:abc",
        "clone_judge_model": "m"
    });
    let pin: engine_pin::EnginePin = serde_json::from_value(json).unwrap();
    assert_eq!(pin.build.built_from, None);
}

fn scan_build(commit: Option<&str>, dirty: bool) -> engine_pin::ScanBuild {
    engine_pin::ScanBuild {
        version: "0.4.0".to_string(),
        commit: commit.map(str::to_string),
        dirty,
        corpus_version: Some("sha256:abc".to_string()),
    }
}

/// The scanning build is taken from the pin as it was saved, never from the running binary, and
/// carries no local path.
#[test]
fn the_scanning_build_is_read_from_the_pin() {
    let pin = engine_pin::EnginePin {
        build: engine_pin::BuildId {
            version: "0.3.9".to_string(),
            commit: Some(OTHER_COMMIT.to_string()),
            dirty: true,
            built_from: Some(KEPT_FOLDER.to_string()),
            ..engine_pin::BuildId::clean_test_build()
        },
        corpus_version: "sha256:abc".to_string(),
        clone_judge_model: "m".to_string(),
    };
    let built = engine_pin::ScanBuild::from_pin(&pin);
    assert_eq!(built.version, "0.3.9");
    assert_eq!(built.commit.as_deref(), Some(OTHER_COMMIT));
    assert!(built.dirty);
    assert_eq!(built.corpus_version.as_deref(), Some("sha256:abc"));
    assert!(!serde_json::to_string(&built).unwrap().contains("auditor"));
    // An empty rule library digest is no rule library, never an empty string.
    let bare = engine_pin::EnginePin {
        corpus_version: String::new(),
        ..pin
    };
    assert_eq!(engine_pin::ScanBuild::from_pin(&bare).corpus_version, None);
}

#[test]
fn the_build_label_and_the_pdf_sentence_use_twelve_characters() {
    let clean = scan_build(Some(OTHER_COMMIT), false);
    assert_eq!(clean.readme_label(), "Camerata 0.4.0, build 1a2b3c4d5e6f");
    assert_eq!(
        scan_build(Some(OTHER_COMMIT), true).readme_label(),
        "Camerata 0.4.0, build 1a2b3c4d5e6f (built with uncommitted changes)"
    );
    assert_eq!(scan_build(None, false).readme_label(), "Camerata 0.4.0");
    assert_eq!(
        clean.pdf_sentence().as_deref(),
        Some(
            "Inspected with Camerata 0.4.0 (reference 1a2b3c4d5e6f). The full technical record \
             is in the manifest file included with this report."
        )
    );
    assert_eq!(scan_build(None, false).pdf_sentence(), None);
    assert!(clean.same_build(&scan_build(Some(OTHER_COMMIT), false)));
    assert!(!clean.same_build(&scan_build(Some(OTHER_COMMIT), true)));
    assert!(!clean.same_build(&scan_build(Some(&"b".repeat(40)), false)));
}
