//! The draft store never loses what it was given: a previous copy survives a rewrite, and a
//! file it cannot read is set aside, not overwritten.

use super::draft::DraftStore;

#[test]
fn a_rewrite_leaves_the_previous_content_as_a_bak() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft.json");
    let store = DraftStore::at(path.clone());
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));

    let bak = dir.path().join("onboarding-draft.json.bak");
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

#[test]
fn a_file_that_is_not_a_draft_map_is_kept_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft.json");
    let torn = r#"{"p1": {"scan": {"repos": ["a/b"]}, "audit": {"findings": ["#;
    std::fs::write(&path, torn).unwrap();

    let store = DraftStore::at(path.clone());
    store.save("p1", serde_json::json!({"scan": 1}));

    let kept = dir.path().join("onboarding-draft.json.unreadable");
    assert_eq!(
        std::fs::read_to_string(&kept).expect("the unreadable file was set aside"),
        torn
    );
}

#[test]
fn only_one_previous_copy_is_kept_and_a_clear_also_leaves_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("onboarding-draft.json");
    let store = DraftStore::at(path.clone());
    store.save("p1", serde_json::json!({"scan": 1}));
    store.save("p1", serde_json::json!({"scan": 2}));
    store.save("p1", serde_json::json!({"scan": 3}));
    let bak = dir.path().join("onboarding-draft.json.bak");
    let prev: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bak).unwrap()).unwrap();
    assert_eq!(
        prev,
        serde_json::json!({"p1": {"scan": 2}}),
        "one copy: the write before"
    );

    store.clear_checked("p1").unwrap();
    let prev: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bak).unwrap()).unwrap();
    assert_eq!(
        prev,
        serde_json::json!({"p1": {"scan": 3}}),
        "a clear keeps what it removed"
    );
}
