// Integration tests unwrap freely on setup I/O (test fixtures); the workspace
// convention is a file-level allow for `crates/*/tests/` (see root Cargo.toml).
#![allow(clippy::unwrap_used)]

//! Hermetic end-to-end integration tests for the Layer-2 Governed Development write-time
//! gate's Pass 4c call-site AST checkers — the PRODUCTION `ARCH-HANDLER-NO-DB-1`
//! (`HandlerNoDbChecker`) and the `ARCH-STRICT-LAYERING-1` CALL facet
//! (`StrictLayeringCallChecker`). Mirrors `import_boundary_checker_gov_dev_loop_e2e.rs`'s
//! convention for Pass 4b-2's `ImportBoundaryChecker`.
//!
//! These drive the PUBLIC entry point a real gov-dev loop uses —
//! [`camerata_checks::runner_for_worktree`] — over a real on-disk worktree. Black-box and
//! hermetic: no git, no network, no model calls — pure filesystem + the public `CheckRunner`
//! trait.

use std::path::Path;

use camerata_checks::runner_for_worktree;
use camerata_core::{Role, RuleId};

const RULE_HANDLER_NO_DB: &str = "ARCH-HANDLER-NO-DB-1";
const RULE_STRICT_LAYERING: &str = "ARCH-STRICT-LAYERING-1";

const ARCHITECTURE_TOML: &str = r#"
version = 1

[layers]
handlers     = ["src/routes/**"]
services     = ["src/services/**"]
repositories = ["src/repositories/**"]

[imports]
handlers     = ["services"]
services     = ["repositories"]
repositories = []

[db]
handles = ["db"]
allowed_in = ["repositories"]
tx_flow_control_in = ["services"]
"#;

fn role_armed_with_handler_no_db_and_strict_layering() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: [RULE_HANDLER_NO_DB, RULE_STRICT_LAYERING]
            .into_iter()
            .map(|id| RuleId(id.to_string()))
            .collect(),
        allowed_paths: vec!["src/".to_string()],
    }
}

fn write_file(worktree: &Path, rel: &str, content: &str) {
    let path = worktree.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_architecture_toml(worktree: &Path) {
    write_file(worktree, ".camerata/architecture.toml", ARCHITECTURE_TOML);
}

/// A worktree whose handler calls a `db` handle directly (forbidden — only the repositories
/// layer may hold a DB handle per `[db].allowed_in`) must be BOUNCED by the combined Layer-2
/// runner under BOTH rule ids, each with the exact file:line surfaced in the diagnostics.
#[tokio::test]
async fn worktree_with_direct_db_call_in_a_handler_is_bounced_under_both_rule_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_architecture_toml(dir.path());
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "export function listOrders(db: Db) {\n  return db.query('select 1');\n}\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_handler_no_db_and_strict_layering();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a well-formed worktree");

    for rule in [RULE_HANDLER_NO_DB, RULE_STRICT_LAYERING] {
        assert!(
            outcome.violated.contains(&RuleId(rule.to_string())),
            "a handler calling a db handle directly must bounce under {rule}, got: {:?}",
            outcome.violated
        );
    }
    assert!(
        outcome.diagnostics.contains("src/routes/orders.ts"),
        "the bounce diagnostics must name the establishing file: {:?}",
        outcome.diagnostics
    );
    assert!(
        outcome.diagnostics.contains(":2"),
        "the bounce diagnostics must name the establishing line (the db.query call): {:?}",
        outcome.diagnostics
    );
}

/// The mirror-image control: a worktree whose only DB access lives in the repository layer
/// (and a service legitimately wrapping a call in `.transaction(...)`) must pass clean.
#[tokio::test]
async fn worktree_with_compliant_layering_passes_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_architecture_toml(dir.path());
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "import { OrderService } from '../services/order_service';\nexport function listOrders(svc: OrderService) {\n  return svc.list();\n}\n",
    );
    write_file(
        dir.path(),
        "src/services/order_service.ts",
        "export class OrderService {\n  constructor(private db: Db) {}\n  list() { return this.db.transaction(() => this.repo()); }\n  repo() { return []; }\n}\n",
    );
    write_file(
        dir.path(),
        "src/repositories/orders_repo.ts",
        "export class OrdersRepo {\n  constructor(private db: Db) {}\n  fetchAll() { return this.db.query('select 1'); }\n}\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_handler_no_db_and_strict_layering();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a well-formed worktree");

    assert!(
        outcome.violated.is_empty(),
        "compliant layering (repository DB access + tx-wrapped service) must not bounce: {:?}",
        outcome.violated
    );
}

/// An architectural rule that is NOT armed for this role/work item must never bounce the loop,
/// even though its checker's interest files are present and would otherwise match.
#[tokio::test]
async fn unarmed_rules_do_not_bounce_the_combined_runner() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_architecture_toml(dir.path());
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "export function listOrders(db: Db) {\n  return db.query('select 1');\n}\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = Role { name: "Backend".to_string(), rule_subset: vec![], allowed_paths: vec![] };
    let outcome = runner.check(&role, dir.path()).await.expect("gov-dev loop check must not error");

    assert!(
        outcome.violated.is_empty(),
        "no rule armed -> the combined runner must not bounce on it: {:?}",
        outcome.violated
    );
}

/// D3 at Layer-2: a worktree with the IDENTICAL violation shape but NO
/// `.camerata/architecture.toml` at all, AND no handler-ish function name either, must not
/// bounce for either rule — both checkers abstain (config-gated / no structural signal at all).
#[tokio::test]
async fn unconfigured_worktree_with_the_same_shape_does_not_bounce() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Deliberately NO .camerata/architecture.toml.
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "export function listOrders(db: Db) {\n  return db.query('select 1');\n}\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_handler_no_db_and_strict_layering();
    let outcome = runner.check(&role, dir.path()).await.expect("gov-dev loop check must not error");

    assert!(
        outcome.violated.is_empty(),
        "no .camerata/architecture.toml -> both checkers must abstain, never guess: {:?}",
        outcome.violated
    );
}
