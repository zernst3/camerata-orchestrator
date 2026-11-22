//! The draft store never loses what it was given: a previous copy survives a rewrite, and a
//! file it cannot read is set aside, not overwritten. Each project's draft has its own file,
//! `onboarding-draft-<project>.json`, beside the legacy shared `onboarding-draft.json` name the
//! store is opened at.

use super::draft::DraftStore;

#[test]
fn a_rewrite_leaves_the_previous_content_as_a_bak() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("onboarding-draft.json");
    let path = dir.path().join("onboarding-draft-p1.json");
    let store = DraftStore::at(shared);
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));

    let bak = dir.path().join("onboarding-draft-p1.json.bak");
    let prev: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bak).expect("a .bak exists")).unwrap();
    assert_eq!(
        prev,
        serde_json::json!({"p1": {"scan": 1}}),
        "the previous write"
    );
    let now: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(now, serde_json::json!({"p1": {"scan": 2}}));
}

fn aside_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .starts_with("onboarding-draft-p1.json.unreadable-")
            })
        })
        .collect()
}

#[test]
fn a_file_that_is_not_a_draft_map_is_kept_not_overwritten_and_the_last_good_bak_survives() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft-p1.json");
    // A good copy from an earlier session, then a file that was torn.
    let good_bak = r#"{"p1":{"scan":{"repos":["a/b"]},"audit":{"findings":[]}}}"#;
    let bak = dir.path().join("onboarding-draft-p1.json.bak");
    std::fs::write(&bak, good_bak).unwrap();
    let torn = r#"{"p1": {"scan": {"repos": ["a/b"]}, "audit": {"findings": ["#;
    std::fs::write(&path, torn).unwrap();

    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));

    let kept = aside_files(dir.path());
    assert_eq!(
        kept.len(),
        1,
        "the unreadable file was set aside, with a timestamp"
    );
    assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), torn);
    // The first write after the unreadable start did not copy the torn file over the good .bak:
    // the .bak is the good copy until a readable file is rotated into it.
    let after_first_save: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bak).unwrap()).unwrap();
    assert_eq!(
        after_first_save,
        serde_json::json!({"p1": {"scan": 1}}),
        "the .bak is a readable draft, never the torn file"
    );
}

#[test]
fn the_good_bak_is_untouched_by_the_first_save_over_an_unreadable_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft-p1.json");
    let good_bak = r#"{"p1":{"scan":{"repos":["a/b"]},"audit":{"findings":[]}}}"#;
    let bak = dir.path().join("onboarding-draft-p1.json.bak");
    std::fs::write(&bak, good_bak).unwrap();
    std::fs::write(&path, "{ torn").unwrap();

    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    assert_eq!(
        std::fs::read_to_string(&bak).unwrap(),
        good_bak,
        "the first write did not rotate the unreadable file over the last good copy"
    );
}

#[test]
fn a_file_that_is_not_utf8_is_set_aside_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft-p1.json");
    let bytes: Vec<u8> = vec![0xff, 0xfe, 0x7b, 0x00, 0xc3, 0x28];
    std::fs::write(&path, &bytes).unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    let kept = aside_files(dir.path());
    assert_eq!(kept.len(), 1, "set aside, not treated as no file");
    assert_eq!(std::fs::read(&kept[0]).unwrap(), bytes);
    assert!(store.take_unreadable_notice().is_some());
}

#[test]
fn the_notice_is_told_once_and_names_the_recovery_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft-p1.json");
    std::fs::write(&path, "{ torn").unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    let notice = store.take_unreadable_notice().expect("told");
    assert!(notice.contains("onboarding-draft-p1.json.bak"), "{notice}");
    assert!(
        notice.contains("onboarding-draft-p1.json.unreadable-"),
        "{notice}"
    );
    assert!(store.take_unreadable_notice().is_none(), "told once");
}

#[test]
fn a_write_that_failed_does_not_clear_the_notice_and_a_good_one_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft-p1.json");
    std::fs::write(&path, "{ torn").unwrap();
    // The temp file's name is taken by a directory, so the write cannot happen.
    let tmp = dir.path().join("onboarding-draft-p1.json.tmp");
    std::fs::create_dir(&tmp).unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    assert!(
        store.take_unreadable_notice().is_some(),
        "the notice stays while no write has succeeded"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ torn",
        "the file was not touched"
    );

    std::fs::remove_dir(&tmp).unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    assert!(
        store.take_unreadable_notice().is_none(),
        "a good write clears it"
    );
    assert!(!tmp.exists(), "the temp file was renamed into place");
    let now: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(now["p1"], serde_json::json!({"scan": 1}));
}

#[test]
fn only_one_previous_copy_is_kept_and_a_projects_deletion_leaves_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));
    store.save("p1", serde_json::json!({"scan": 3}));
    let bak = dir.path().join("onboarding-draft-p1.json.bak");
    let prev: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bak).unwrap()).unwrap();
    assert_eq!(
        prev,
        serde_json::json!({"p1": {"scan": 2}}),
        "one copy: the write before"
    );

    // A project's deletion leaves no copy of its draft behind, the .bak included.
    store.clear_checked("p1").unwrap();
    assert!(!bak.exists(), "the .bak is gone");
    assert!(!dir.path().join("onboarding-draft-p1.json").exists());
}

// ── adopt_audit, directly ─────────────────────────────────────────────────────────────────

fn repos(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn draft_of(names: &[&str]) -> serde_json::Value {
    serde_json::json!({ "scan": { "repos": names }, "audit": null })
}

fn run(at: &str) -> serde_json::Value {
    serde_json::json!({ "provenance": { "finished_at": at }, "findings": ["from the run"] })
}

const T1: &str = "2026-11-19T10:00:00+00:00";
const T2: &str = "2026-11-19T11:00:00+00:00";

#[test]
fn an_audit_the_draft_already_holds_is_never_replaced() {
    let store = DraftStore::new();
    store.save(
        "p1",
        serde_json::json!({ "scan": { "repos": ["a/b"] }, "audit": { "mine": true } }),
    );
    assert!(!store.adopt_audit("p1", &repos(&["a/b"]), T2, || run(T2)));
    assert_eq!(
        store.load("p1").unwrap()["audit"],
        serde_json::json!({"mine": true})
    );
}

#[test]
fn a_draft_with_no_audit_adopts_the_run_when_the_repos_match_exactly() {
    let store = DraftStore::new();
    store.save("p1", draft_of(&["a/b", "c/d"]));
    assert!(store.adopt_audit("p1", &repos(&["a/b", "c/d"]), T1, || run(T1)));
    assert_eq!(store.load("p1").unwrap()["audit"], run(T1));
}

#[test]
fn reordered_repos_are_not_the_same_scan() {
    let store = DraftStore::new();
    store.save("p1", draft_of(&["a/b", "c/d"]));
    assert!(!store.adopt_audit("p1", &repos(&["c/d", "a/b"]), T1, || run(T1)));
    assert!(store.load("p1").unwrap()["audit"].is_null());
}

#[test]
fn a_repo_added_or_removed_is_not_the_same_scan() {
    let store = DraftStore::new();
    store.save("p1", draft_of(&["a/b"]));
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b", "c/d"]), T1, || run(T1)),
        "repo added to the run"
    );
    store.save("p1", draft_of(&["a/b", "c/d"]));
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)),
        "repo removed from the run"
    );
    assert!(store.load("p1").unwrap()["audit"].is_null());
}

#[test]
fn no_draft_and_a_draft_without_a_scan_adopt_nothing() {
    let store = DraftStore::new();
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)),
        "no draft"
    );
    assert!(store.load("p1").is_none(), "no draft was invented");
    store.save("p1", serde_json::json!({ "audit": null }));
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)),
        "no scan"
    );
    assert!(store.load("p1").unwrap()["audit"].is_null());
}

#[test]
fn an_adopted_audit_is_replaced_by_a_newer_run_and_not_by_an_older_one() {
    let store = DraftStore::new();
    store.save("p1", draft_of(&["a/b"]));
    assert!(store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)));
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)),
        "the same run again"
    );
    assert!(
        store.adopt_audit("p1", &repos(&["a/b"]), T2, || run(T2)),
        "a newer run replaces it"
    );
    assert_eq!(store.load("p1").unwrap()["audit"], run(T2));
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), "2026-11-19T09:00:00Z", || run(
            "2026-11-19T09:00:00Z"
        )),
        "an older run does not"
    );
}

#[test]
fn a_run_that_finished_before_the_draft_was_discarded_is_not_adopted_and_a_later_one_is() {
    let store = DraftStore::new();
    store.clear_at("p1", "2026-11-19T10:30:00Z");
    store.save("p1", draft_of(&["a/b"]));
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)),
        "finished before the discard"
    );
    assert!(
        !store.adopt_audit("p1", &repos(&["a/b"]), "unparseable", || run(T1)),
        "no readable time, after a discard"
    );
    assert!(
        store.adopt_audit("p1", &repos(&["a/b"]), T2, || run(T2)),
        "finished after it"
    );
}

#[test]
fn a_draft_with_no_discard_stamp_still_adopts() {
    let store = DraftStore::new();
    store.save("p1", draft_of(&["a/b"]));
    assert!(store.adopt_audit("p1", &repos(&["a/b"]), T1, || run(T1)));
}

fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_deletion_that_fails_part_way_leaves_memory_and_disk_agreeing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));
    store.save("p2", serde_json::json!({"scan": 3}));
    // The folder will not let a file be removed from it.
    set_mode(dir.path(), 0o555);
    assert!(
        std::fs::write(dir.path().join("probe"), "x").is_err(),
        "run these tests as an ordinary user"
    );

    assert!(
        store.clear_checked("p1").is_err(),
        "the failure is reported"
    );
    assert!(store.load("p1").is_some(), "memory still holds the draft");
    let main = dir.path().join("onboarding-draft-p1.json");
    assert!(
        std::fs::read_to_string(&main).unwrap().contains("p1"),
        "the file still holds it too"
    );

    set_mode(dir.path(), 0o755);
    store.clear_checked("p1").expect("the retry succeeds");
    assert!(store.load("p1").is_none());
    let names = names_in(dir.path());
    assert!(names.iter().all(|n| !n.contains("p1")), "{names:?}");
    assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 3})));
}

// ── one draft file per project ────────────────────────────────────────────────────────────

fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn deleting_one_project_removes_exactly_its_draft_files_and_leaves_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let store = DraftStore::at(dir.path().join("onboarding-draft.json"));
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));
    store.save("p2", serde_json::json!({"scan": 3}));
    store.save("p2", serde_json::json!({"scan": 4}));
    // A set-aside copy of each project's own file.
    std::fs::write(
        dir.path()
            .join("onboarding-draft-p1.json.unreadable-1700000000"),
        "{ torn p1",
    )
    .unwrap();
    std::fs::write(
        dir.path()
            .join("onboarding-draft-p2.json.unreadable-1700000000"),
        "{ torn p2",
    )
    .unwrap();

    store.clear_checked("p1").unwrap();

    let names = names_in(dir.path());
    assert!(
        names.iter().all(|n| !n.contains("p1")),
        "nothing of p1 is left: {names:?}"
    );
    for f in [
        "onboarding-draft-p2.json",
        "onboarding-draft-p2.json.bak",
        "onboarding-draft-p2.json.unreadable-1700000000",
    ] {
        assert!(names.iter().any(|n| n == f), "{f} untouched: {names:?}");
    }
    assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 4})));
    assert!(store.load("p1").is_none());
}

#[test]
fn a_shared_draft_file_is_split_into_one_file_per_project_and_kept_as_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("onboarding-draft.json");
    let text = r#"{"p1":{"scan":1},"p2":{"scan":2},"_cleared_at":{"p1":"2026-11-19T10:00:00Z","p3":"2026-11-19T11:00:00Z"}}"#;
    std::fs::write(&shared, text).unwrap();

    let store = DraftStore::at(shared.clone());
    assert_eq!(store.load("p1"), Some(serde_json::json!({"scan": 1})));
    assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 2})));
    let read = |f: &str| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.path().join(f)).unwrap()).unwrap()
    };
    assert_eq!(
        read("onboarding-draft-p1.json"),
        serde_json::json!({"p1": {"scan": 1}, "_cleared_at": {"p1": "2026-11-19T10:00:00Z"}})
    );
    assert_eq!(
        read("onboarding-draft-p2.json"),
        serde_json::json!({"p2": {"scan": 2}})
    );
    assert_eq!(
        read("onboarding-draft-p3.json"),
        serde_json::json!({"_cleared_at": {"p3": "2026-11-19T11:00:00Z"}}),
        "a discard stamp with no draft keeps its own file"
    );
    assert!(!shared.exists(), "the shared name is free");
    let backup = dir.path().join("onboarding-draft.json.shared-backup");
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        text,
        "the shared file is kept whole as the backup"
    );

    let before = names_in(dir.path());
    let again = DraftStore::at(shared);
    assert_eq!(
        names_in(dir.path()),
        before,
        "a second start changes nothing"
    );
    assert_eq!(again.load("p2"), Some(serde_json::json!({"scan": 2})));
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), text);
}
