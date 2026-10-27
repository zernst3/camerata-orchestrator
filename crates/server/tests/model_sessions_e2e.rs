#![allow(clippy::unwrap_used)]
//! END-TO-END: the Claude Code sessions a scan's model calls create are recorded on the report.
//!
//! Source deletion (docs/design/2026-10-24_delete-and-refetch-client-source.md, 5.1) removes the
//! `~/.claude/projects/*/<session id>.jsonl` files that hold the client's prompts, and it can
//! only remove what the scan recorded. This drives the real `onboard::audit_repos` against a
//! fake `claude` on PATH (no network, no spend) and checks the recorded ids are exactly the ids
//! the child processes were given, and that the Api backend records none.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::llm::BackendResolution;
use camerata_server::onboard::{self, ScanReport, SelectedRule};

const SINGLE: &str = "ARCH-SINGLE-SOURCE-LOGIC-1";
const NAMING: &str = "ARCH-NAMING-ONE-CONCEPT-1";

/// A fake `claude` that appends every argument of every call to `args.log` and answers with an
/// empty (but well-formed) audit result.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
printf '%s\n' "$@" >> "$(dirname "$0")/args.log"
printf '%s\n' '{"type":"result","is_error":false,"result":"{\"findings\":[],\"proposed_rules\":[],\"needs_files\":[],\"verdicts\":[],\"fixes\":[],\"recommendations\":[]}","total_cost_usd":0.001,"usage":{"input_tokens":10,"output_tokens":5}}'
"#;

fn discount(func: &str, rate: &str) -> String {
    format!(
        "import {{ money }} from './money';
export function {func}(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = base * {rate};
    return base - discount;
  }}
  return base + order.shipping;
}}
"
    )
}

fn stage_repo(dest: &Path) {
    camerata_server::test_git::init_repo(
        dest,
        &[
            ("src/cart/total.ts", &discount("applyDiscount", "0.2")),
            ("src/checkout/summary.ts", &discount("quoteTotal", "0.15")),
        ],
    );
}

fn selected() -> Vec<SelectedRule> {
    [SINGLE, NAMING]
        .into_iter()
        .map(|id| SelectedRule {
            id: id.to_string(),
            directive: format!("model sessions e2e: {id}"),
            repos: Vec::new(),
        })
        .collect()
}

async fn scan(dir: &Path, backend: BackendResolution) -> ScanReport {
    let corpus = camerata_rules::load_corpus(&camerata_rules::corpus_path())
        .await
        .unwrap();
    let sources = vec![("acme/shop".to_string(), dir.to_path_buf())];
    let (report, _manifest) = onboard::audit_repos(
        &camerata_server::credentials::MemoryCredentialStore::new(),
        &sources,
        &selected(),
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
        true, // run_ai_review: the model is called
        true,
        None,
        backend,
        Some(&corpus),
        &std::collections::HashMap::new(),
    )
    .await;
    report
}

fn install_fake_claude(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    std::fs::write(&bin, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn model_sessions_recorded_on_cli_backend_and_empty_on_api() {
    // ── Cli backend: a fake `claude` on PATH records what it was given. ──────────────────
    let tools = tempfile::tempdir().unwrap();
    install_fake_claude(tools.path());
    let path = match std::env::var("PATH") {
        Ok(p) => format!("{}:{p}", tools.path().display()),
        Err(_) => tools.path().display().to_string(),
    };
    let _path = camerata_test_env::EnvGuard::set("PATH", path);
    let _no_dep_audit = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");

    let repo = tempfile::tempdir().unwrap();
    stage_repo(repo.path());
    let cli = scan(repo.path(), BackendResolution::Cli).await;

    let given: Vec<String> = std::fs::read_to_string(tools.path().join("args.log"))
        .expect("the fake claude was called at least once")
        .lines()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|w| w[0] == "--session-id")
        .map(|w| w[1].to_string())
        .collect();
    assert!(!given.is_empty(), "the scan made model calls");
    let mut recorded: Vec<String> = cli
        .provenance
        .model_sessions
        .iter()
        .map(|s| s.session_id.clone())
        .collect();
    let mut given_sorted = given.clone();
    recorded.sort();
    given_sorted.sort();
    assert_eq!(
        recorded, given_sorted,
        "the report records exactly the session ids the child processes were given"
    );
    assert!(
        recorded.iter().all(|id| id.len() == 36),
        "ids are UUIDs: {recorded:?}"
    );

    // ── Api backend: no `claude` is spawned, so nothing is recorded. ─────────────────────
    let _no_key = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let before = std::fs::read_to_string(tools.path().join("args.log")).unwrap();
    let api = scan(repo.path(), BackendResolution::Api).await;
    assert!(
        api.provenance.model_sessions.is_empty(),
        "{:?}",
        api.provenance.model_sessions
    );
    assert_eq!(
        std::fs::read_to_string(tools.path().join("args.log")).unwrap(),
        before,
        "the Api backend never starts a Claude Code process"
    );
}
