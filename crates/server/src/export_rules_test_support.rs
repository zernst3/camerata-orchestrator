//! Test support for fixtures that put a report on a project without running an inspection: they
//! have no saved run, so their exports would refuse (#560). This attaches the bundled rule
//! library as the inspection's snapshot, as if its run had kept it.

/// Keep the bundled rule library as the rules the project's current inspection ran with. Adds
/// an inspection entry for the current report when the fixture set none (a report stored in
/// memory only, or one with no audited repo).
pub(crate) fn attach_bundled_rules(state: &crate::AppState, project_id: &str) {
    attach_rules(state, project_id, bundled_rules());
}

/// The bundled rule library, loaded once (on a thread of its own, so a synchronous fixture can
/// call it from inside a test's runtime).
pub(crate) fn bundled_rules() -> &'static camerata_rules::RuleSet {
    static RULES: std::sync::OnceLock<camerata_rules::RuleSet> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            let (rules, errors) = runtime.block_on(camerata_rules::load_corpus_lenient(
                &camerata_rules::corpus_path(),
            ));
            assert!(errors.is_empty(), "the bundled corpus loads: {errors:?}");
            rules
        })
        .join()
        .expect("the corpus loads")
    })
}

/// [`attach_bundled_rules`] with the rules given.
pub(crate) fn attach_rules(
    state: &crate::AppState,
    project_id: &str,
    rules: &camerata_rules::RuleSet,
) {
    let report = state
        .get_last_scan(project_id)
        .expect("the fixture stored a report");
    let has_entry = state
        .inspections
        .latest(project_id)
        .is_some_and(|e| e.report.run_id == report.run_id);
    if !has_entry {
        state
            .inspections
            .append(crate::inspections::InspectionEntry::from_report(
                project_id,
                report,
                crate::inspections::InspectionKind::Full,
            ))
            .expect("append the entry");
    }
    assert!(
        state
            .inspections
            .attach_rule_snapshot(project_id, rules)
            .expect("keep the rules"),
        "the project has an inspection to attach the rules to"
    );
}
