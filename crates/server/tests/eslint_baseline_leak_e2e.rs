#![allow(clippy::unwrap_used)]
//! END-TO-END: the eslint scan tier reports only the rules the project selected.
//!
//! Camerata's bundled eslint config enables a baseline of core rules (`no-console`, `eqeqeq`,
//! `no-var`, `prefer-const`, ...) so eslint always has a coherent base. Before this fix those
//! baseline rules were reported too: every row eslint printed became a preview finding in
//! `report.findings` and so in every client artifact, and `eqeqeq` / `no-var` / `prefer-const`
//! even grounded to `JAVASCRIPT-STRICT-EQUALITY-1`, `JAVASCRIPT-NO-VAR-1` and
//! `JAVASCRIPT-CONST-DEFAULT-1` with their citations although nobody had selected them.
//!
//! The scan here selects ONLY the two React Hooks rules over a file that violates a hooks rule
//! AND the baseline rules. Skips (never fails, never provisions) when this machine has no
//! cached Camerata eslint workspace with the hooks plugin.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};

/// READ-ONLY probe of the shared workspace: the full health probe (binary answers, every pinned
/// package installed, config current), so a stale or half-installed workspace makes this test
/// skip instead of letting `audit_repos` drive a re-provision of the tree every other scan and
/// test is using. (A test process is also refused provisioning into the real tooling dir.)
async fn workspace_with_hooks_plugin_is_cached() -> bool {
    let Some(tooling) = camerata_server::tool_provisioning::tooling_dir() else {
        return false;
    };
    let workspace = camerata_server::tool_provisioning::eslint_workspace_dir(&tooling);
    camerata_server::tool_provisioning::eslint_is_provisioned(&workspace).await
        && workspace
            .join("node_modules")
            .join("eslint-plugin-react-hooks")
            .join("package.json")
            .exists()
}

fn selected(ids: &[&str]) -> Vec<SelectedRule> {
    ids.iter()
        .map(|id| SelectedRule {
            id: (*id).to_string(),
            directive: "baseline leak e2e".to_string(),
            repos: Vec::new(),
        })
        .collect()
}

fn write(root: &Path, rel: &str, content: &str) {
    std::fs::write(root.join(rel), content).unwrap();
}

#[tokio::test]
async fn a_scan_that_selects_only_the_hooks_rules_reports_no_baseline_eslint_rows() {
    if !workspace_with_hooks_plugin_is_cached().await {
        eprintln!(
            "skipping: no cached Camerata eslint workspace with eslint-plugin-react-hooks on this \
             machine (this test never provisions over the network)"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // One hooks violation, plus every shape the baseline would also have reported.
    write(
        root,
        "Thing.jsx",
        "import { useEffect } from \"react\";\n\
         var legacy = 1;\n\
         let neverReassigned = 2;\n\
         export function Thing({ id }) {\n\
         \x20 if (legacy == 1) { console.log(neverReassigned); }\n\
         \x20 useEffect(() => { console.log(id); }, []);\n\
         \x20 return null;\n\
         }\n",
    );

    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let rules = selected(&[
        "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
        "JAVASCRIPT-REACT-RULES-OF-HOOKS-1",
    ]);
    let sources = vec![("e2e/baseline".to_string(), root.to_path_buf())];
    let (mut report, _manifest) = onboard::audit_repos(
        &camerata_server::credentials::MemoryCredentialStore::new(),
        &sources,
        &rules,
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
        false,
        true,
        None,
        camerata_server::llm::BackendResolution::Api,
        Some(&corpus),
        &std::collections::HashMap::new(),
    )
    .await;
    camerata_server::merge_scan_preview(
        &mut report,
        &sources,
        &rules,
        Some(&corpus),
        None,
        camerata_server::client_lock::ClientTooling::INTERNAL,
    )
    .await;

    let eslint_rows: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.preview_tool.as_deref() == Some("eslint"))
        .collect();
    // The positive control: eslint ran and the selected rule fired.
    assert!(
        eslint_rows
            .iter()
            .any(|f| f.rule_id == "react-hooks/exhaustive-deps"),
        "the selected hooks rule must report: {:?}",
        report.findings
    );
    // Nothing else did.
    let leaked: Vec<&str> = eslint_rows
        .iter()
        .map(|f| f.rule_id.as_str())
        .filter(|id| !id.starts_with("react-hooks/"))
        .collect();
    assert!(
        leaked.is_empty(),
        "unselected eslint rules leaked: {leaked:?}"
    );
    // And they are absent from the serialized findings every artifact is built from.
    let json = serde_json::to_string(&report.findings).unwrap();
    for baseline in [
        "no-console",
        "eqeqeq",
        "no-var",
        "prefer-const",
        "no-unused-vars",
    ] {
        assert!(
            !json.contains(&format!("\"rule_id\":\"{baseline}\"")),
            "{baseline} must not reach the findings: {json}"
        );
    }
}
