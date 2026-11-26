//! The rule snapshot's own checks (#560): what reads back, and every named refusal.

use super::*;

/// Two synthetic rules, one with an option whose fix text a test can change.
fn rules() -> RuleSet {
    let mut set = camerata_rules::ruleset_with_unauthored_rule("ACME-SNAP-1");
    let mut second = camerata_rules::bare_rule("ACME-SNAP-2", "acme");
    second.max_severity = Some("high".to_string());
    second.cwe = vec!["CWE-89".to_string()];
    set.push(second);
    set
}

#[test]
fn a_snapshot_reads_back_as_the_same_rules_with_the_pinned_digest() {
    let set = rules();
    let (digest, bytes) = encode(&set);
    assert_eq!(digest, camerata_rules::corpus_version(&set));
    let back = decode(&bytes, &digest).expect("it reads back");
    assert_eq!(camerata_rules::corpus_version(&back), digest);
    assert_eq!(back.len(), 2);
    assert_eq!(
        back.get_by_id("ACME-SNAP-2")
            .and_then(|r| r.max_severity.as_deref()),
        Some("high")
    );
}

#[test]
fn a_snapshot_with_one_byte_changed_is_refused() {
    let set = rules();
    let (digest, bytes) = encode(&set);
    // Change one letter of the rule text: the file still parses, the digest does not match.
    let text = String::from_utf8(bytes).expect("utf8");
    let at = text.find("Test-fixture detection").expect("the directive") + 5;
    let mut damaged = text.into_bytes();
    damaged[at] = b'X';
    let err = decode(&damaged, &digest).expect_err("a damaged snapshot is refused");
    assert!(
        matches!(err, RefusalKind::DigestMismatch { .. }),
        "refused as a digest mismatch: {err:?}"
    );
}

#[test]
fn a_snapshot_that_does_not_parse_is_refused() {
    let (digest, mut bytes) = encode(&rules());
    bytes.truncate(bytes.len() - 3);
    let err = decode(&bytes, &digest).expect_err("refused");
    assert!(matches!(err, RefusalKind::Unreadable(_)), "{err:?}");
}

#[test]
fn a_snapshot_pinned_to_another_digest_is_refused() {
    let (_, bytes) = encode(&rules());
    let other = camerata_rules::corpus_version(&camerata_rules::ruleset_with_unauthored_rule(
        "ACME-OTHER-1",
    ));
    let err = decode(&bytes, &other).expect_err("refused");
    assert!(
        matches!(&err, RefusalKind::DigestMismatch { pinned, .. } if *pinned == other),
        "{err:?}"
    );
}

#[test]
fn a_snapshot_whose_own_digest_line_is_changed_is_refused() {
    let set = rules();
    let (digest, bytes) = encode(&set);
    let mut value: Value = serde_json::from_slice(&bytes).expect("json");
    value["digest"] = Value::String(format!("sha256:{}", "0".repeat(64)));
    let err = decode(&serde_json::to_vec(&value).expect("json"), &digest).expect_err("refused");
    assert!(matches!(err, RefusalKind::DigestMismatch { .. }), "{err:?}");
}

#[test]
fn a_field_this_build_cannot_read_is_refused_never_dropped() {
    let set = rules();
    let (digest, bytes) = encode(&set);
    let mut value: Value = serde_json::from_slice(&bytes).expect("json");
    value["rules"][1]["sources"] = serde_json::json!([{
        "url": "https://example.com/acme",
        "title": "Acme guide",
        "linter": null,
        "relevance": null,
        "a_field_from_the_future": "kept text"
    }]);
    let err = decode(&serde_json::to_vec(&value).expect("json"), &digest).expect_err("refused");
    assert_eq!(
        err,
        RefusalKind::Dropped {
            rule: "ACME-SNAP-2".to_string(),
            field: "`sources[0].a_field_from_the_future`".to_string()
        }
    );
}

#[test]
fn a_rule_level_field_this_build_cannot_read_is_refused() {
    let (digest, bytes) = encode(&rules());
    let mut value: Value = serde_json::from_slice(&bytes).expect("json");
    value["rules"][0]["owner_note"] = Value::String("acme".to_string());
    let err = decode(&serde_json::to_vec(&value).expect("json"), &digest).expect_err("refused");
    assert!(
        matches!(&err, RefusalKind::Dropped { field, .. } if field == "`owner_note`"),
        "{err:?}"
    );
}

#[test]
fn an_unknown_record_kind_is_refused() {
    let (digest, bytes) = encode(&rules());
    let mut value: Value = serde_json::from_slice(&bytes).expect("json");
    value["rules"][0]["kind"] = Value::String("a_kind_from_the_future".to_string());
    let err = decode(&serde_json::to_vec(&value).expect("json"), &digest).expect_err("refused");
    assert!(
        matches!(&err, RefusalKind::Unreadable(why) if why.contains("a_kind_from_the_future")),
        "{err:?}"
    );
}

#[test]
fn another_file_kind_or_a_newer_format_is_refused() {
    let (digest, bytes) = encode(&rules());
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    let mut kind = value.clone();
    kind["kind"] = Value::String("camerata-something-else".to_string());
    let err = decode(&serde_json::to_vec(&kind).expect("json"), &digest).expect_err("refused");
    assert!(
        matches!(&err, RefusalKind::Unreadable(why) if why.contains("kind")),
        "{err:?}"
    );
    let mut format = value.clone();
    format["format"] = Value::from(SNAPSHOT_FORMAT + 1);
    let err = decode(&serde_json::to_vec(&format).expect("json"), &digest).expect_err("refused");
    assert!(
        matches!(&err, RefusalKind::Unreadable(why) if why.contains("format")),
        "{err:?}"
    );
    let mut extra = value;
    extra["notes"] = Value::String("acme".to_string());
    let err = decode(&serde_json::to_vec(&extra).expect("json"), &digest).expect_err("refused");
    assert!(matches!(err, RefusalKind::Unreadable(_)), "{err:?}");
}

#[test]
fn a_field_added_by_a_newer_build_reads_from_a_snapshot_without_it() {
    // A snapshot written before a defaulted field existed holds no key for it. Reading it is
    // fine, because nothing the file holds is dropped. Shown with a source's defaulted fields.
    let mut set = rules();
    let mut second = set.get_by_id("ACME-SNAP-2").cloned().expect("rule");
    second.sources = vec![camerata_rules::RuleSource {
        url: "https://example.com/acme".to_string(),
        title: "Acme guide".to_string(),
        linter: None,
        relevance: None,
    }];
    let first = set.iter().next().cloned().expect("rule");
    set = RuleSet::default();
    set.push(first);
    set.push(second);
    let (digest, bytes) = encode(&set);
    let mut value: Value = serde_json::from_slice(&bytes).expect("json");
    let source = value["rules"][1]["sources"][0]
        .as_object_mut()
        .expect("a source");
    source.remove("linter");
    source.remove("relevance");
    let back = decode(&serde_json::to_vec(&value).expect("json"), &digest).expect("it reads");
    assert_eq!(camerata_rules::corpus_version(&back), digest);
}

#[test]
fn a_file_name_is_made_only_from_a_sha256_digest() {
    let digest = format!("sha256:{}", "ab".repeat(32));
    assert_eq!(
        file_name(&digest).as_deref(),
        Some(format!("sha256-{}.json", "ab".repeat(32)).as_str())
    );
    assert_eq!(file_name(""), None);
    assert_eq!(file_name("sha256:../../etc"), None);
    assert_eq!(file_name(&format!("sha256:{}", "AB".repeat(32))), None);
    assert_eq!(file_name(&format!("md5:{}", "ab".repeat(32))), None);
}

#[test]
fn the_missing_snapshot_refusal_names_the_backfill_command() {
    let refusal = SnapshotRefusal::new(Some("20261126T000000Z-abc123"), RefusalKind::Missing);
    let text = refusal.to_string();
    assert!(
        text.contains(
            "camerata run snapshot-rules --run 20261126T000000Z-abc123 --from <rule library folder>"
        ),
        "{text}"
    );
    assert_eq!(refusal.code(), "missing");
}

#[test]
fn every_refusal_reads_as_plain_client_style_text() {
    let kinds = [
        RefusalKind::NoRun,
        RefusalKind::RunUnreadable("acme".to_string()),
        RefusalKind::NoPin,
        RefusalKind::NoRules,
        RefusalKind::Missing,
        RefusalKind::Unreadable("acme".to_string()),
        RefusalKind::Dropped {
            rule: "ACME-1".to_string(),
            field: "`x`".to_string(),
        },
        RefusalKind::DigestMismatch {
            pinned: "a".to_string(),
            found: "b".to_string(),
        },
    ];
    for kind in kinds {
        let text = SnapshotRefusal::new(Some("run-1"), kind).to_string();
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{text}"
        );
        assert!(
            !text.contains(", and ") && !text.contains(", or "),
            "{text}"
        );
    }
}

// ── The shared write in `RunStore::create`, read back from the run's folder ─────────────────

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().expect("tempdir");
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

fn new_run(
    repo: &std::path::Path,
    pin_rules: Option<&RuleSet>,
    kept: Option<&RuleSet>,
) -> Result<std::sync::Arc<crate::run_store::RunStore>, crate::run_store::RunStoreError> {
    use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};
    RunStore::create(NewRun {
        pin: crate::run_store::engine_pin::EnginePin::for_run(pin_rules, &Default::default()),
        rules: kept,
        project_key: "acme-snapshot".to_string(),
        repos: vec![("acme/api".to_string(), repo.to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp1".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cli,
        project_id: None,
    })
}

#[test]
#[serial_test::serial(process_env)]
fn a_new_run_keeps_the_rules_it_loaded_and_reads_them_back_against_its_pin() {
    let (_state, _guard) = state_dir();
    let repo = tempfile::tempdir().expect("repo");
    let set = rules();
    let store = new_run(repo.path(), Some(&set), Some(&set)).expect("the run is created");
    let pinned = store.record().pin.expect("pinned").corpus_version;
    let path = path_in(store.dir(), &pinned).expect("a digest file name");
    assert!(path.is_file(), "the snapshot is in the run's own folder");
    let snapshot = read_run(&store.run_id()).expect("it reads back");
    assert_eq!(snapshot.digest, pinned);
    assert_eq!(camerata_rules::corpus_version(&snapshot.rules), pinned);
    assert_eq!(snapshot.rules.len(), set.len());
}

#[test]
#[serial_test::serial(process_env)]
fn a_run_whose_kept_rules_are_not_its_pinned_rules_is_not_created() {
    let (_state, _guard) = state_dir();
    let repo = tempfile::tempdir().expect("repo");
    let other = camerata_rules::ruleset_with_unauthored_rule("ACME-OTHER-1");
    let err = new_run(repo.path(), Some(&rules()), Some(&other))
        .err()
        .expect("refused");
    assert!(err.to_string().contains("not the rules"), "{err}");
}

#[test]
#[serial_test::serial(process_env)]
fn a_run_without_a_snapshot_or_without_rules_refuses_by_name() {
    let (_state, _guard) = state_dir();
    let repo = tempfile::tempdir().expect("repo");
    // No rule library loaded: the pin's digest is empty.
    let store = new_run(repo.path(), None, None).expect("created");
    let err = read_run(&store.run_id()).expect_err("refused");
    assert_eq!(err.kind, RefusalKind::NoRules);
    // A run that names rules it was not handed is not started.
    let set = rules();
    let err = new_run(repo.path(), Some(&set), None)
        .err()
        .expect("refused");
    assert!(err.to_string().contains("not handed the rules"), "{err}");
    // A run made before snapshots were kept: pinned, nothing in `rules/`.
    let store = new_run(repo.path(), Some(&set), Some(&set)).expect("created");
    std::fs::remove_dir_all(store.dir().join(SNAPSHOT_DIR)).expect("remove the snapshot");
    let err = read_run(&store.run_id()).expect_err("refused");
    assert_eq!(err.kind, RefusalKind::Missing);
    assert_eq!(err.run_id.as_deref(), Some(store.run_id().as_str()));
    // A run id that is not one: never a path out of the run store, even to a real run.
    let around = format!("../runs/{}", store.run_id());
    assert!(matches!(
        read_run(&around).expect_err("refused").kind,
        RefusalKind::RunUnreadable(_)
    ));
    assert!(matches!(
        read_run("20261126T000000Z-000000")
            .expect_err("refused")
            .kind,
        RefusalKind::RunUnreadable(_)
    ));
}

#[test]
fn a_value_that_reads_back_changed_is_refused() {
    // A scope's language is normalized on reading, so "Rust" in the file would read as "rust":
    // the rule read is not the rule kept, even though its digest would match.
    let mut set = RuleSet::default();
    let mut rule = camerata_rules::bare_rule("ACME-SNAP-3", "acme");
    rule.applies_to = Some(
        camerata_rules::AppliesTo::new(Vec::new(), vec!["rust".to_string()], Vec::new())
            .expect("a scope"),
    );
    set.push(rule);
    let (digest, bytes) = encode(&set);
    let mut value: Value = serde_json::from_slice(&bytes).expect("json");
    value["rules"][0]["applies_to"]["languages"][0] = Value::String("Rust".to_string());
    let err = decode(&serde_json::to_vec(&value).expect("json"), &digest).expect_err("refused");
    assert_eq!(
        err,
        RefusalKind::Dropped {
            rule: "ACME-SNAP-3".to_string(),
            field: "`applies_to.languages[0]`".to_string()
        }
    );
}
