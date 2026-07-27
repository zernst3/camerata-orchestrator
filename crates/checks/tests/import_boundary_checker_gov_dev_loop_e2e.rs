// Integration tests unwrap freely on setup I/O (test fixtures); the workspace
// convention is a file-level allow for `crates/*/tests/` (see root Cargo.toml).
#![allow(clippy::unwrap_used)]

//! Hermetic end-to-end integration tests for the Layer-2 Governed Development write-time
//! gate's `ImportBoundaryChecker` tier (Pass 4b-2 — see
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group C, "Plug point B" from
//! `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.3).
//!
//! These drive the PUBLIC entry point a real gov-dev loop uses —
//! [`camerata_checks::runner_for_worktree`] — over a real on-disk worktree carrying a
//! `.camerata/architecture.toml` boundary map, mirroring the existing
//! `arch_check_runner_gov_dev_loop_e2e.rs` convention (Supabase RLS) for this new checker.
//!
//! Black-box and hermetic: no git, no network, no model calls — pure filesystem + the public
//! `CheckRunner` trait.

use std::path::Path;

use camerata_checks::runner_for_worktree;
use camerata_core::{Role, RuleId};

const RULE_NO_CROSS_BOUNDARY_IMPORTS: &str = "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1";
const RULE_API_DTOS: &str = "ARCH-API-DTOS-1";
const RULE_STRICT_LAYERING: &str = "ARCH-STRICT-LAYERING-1";

const ARCHITECTURE_TOML: &str = r#"
version = 1

[layers]
handlers     = ["src/routes/**"]
services     = ["src/services/**"]
repositories = ["src/repositories/**"]
domain       = ["src/domain/**"]

[imports]
handlers     = ["services", "domain"]
services     = ["repositories", "domain"]
repositories = ["domain"]
domain       = []
"#;

fn role_armed_with_import_boundary_family() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: [RULE_NO_CROSS_BOUNDARY_IMPORTS, RULE_API_DTOS, RULE_STRICT_LAYERING]
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

/// A worktree whose handler imports the repositories layer directly (forbidden — handlers may
/// only import services/domain per the configured `[imports]` map) must be BOUNCED by the
/// combined Layer-2 runner: the exact rule id, with the exact file:line surfaced in the
/// diagnostics, so the agent can self-correct without a human in the loop.
#[tokio::test]
async fn worktree_with_cross_boundary_violation_is_bounced_with_exact_location() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_architecture_toml(dir.path());
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "import { OrdersRepo } from '../repositories/orders_repo';\n\
         export function listOrdersHandler(repo: OrdersRepo) { return repo.count(); }\n",
    );
    write_file(dir.path(), "src/repositories/orders_repo.ts", "export class OrdersRepo { count() { return 0; } }\n");

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_import_boundary_family();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a well-formed worktree");

    assert!(
        outcome.violated.contains(&RuleId(RULE_NO_CROSS_BOUNDARY_IMPORTS.to_string())),
        "a handler importing repositories directly must bounce under ARCH-NO-CROSS-BOUNDARY-IMPORTS-1, \
         got: {:?}",
        outcome.violated
    );
    assert!(
        outcome.diagnostics.contains("src/routes/orders.ts"),
        "the bounce diagnostics must name the establishing file: {:?}",
        outcome.diagnostics
    );
    assert!(
        outcome.diagnostics.contains(":1"),
        "the bounce diagnostics must name the establishing line: {:?}",
        outcome.diagnostics
    );
    assert!(
        outcome.diagnostics.contains(RULE_NO_CROSS_BOUNDARY_IMPORTS),
        "the bounce diagnostics must name the rule id: {:?}",
        outcome.diagnostics
    );
}

/// The mirror-image control: a worktree whose imports all stay within the configured
/// boundaries must pass clean through the same combined runner — proving the gate doesn't cry
/// wolf on compliant work.
#[tokio::test]
async fn worktree_with_compliant_imports_passes_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_architecture_toml(dir.path());
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "import { OrderService } from '../services/order_service';\n\
         export function listOrdersHandler(svc: OrderService) { return svc.listOrders(); }\n",
    );
    write_file(
        dir.path(),
        "src/services/order_service.ts",
        "import { OrdersRepo } from '../repositories/orders_repo';\n\
         export class OrderService { constructor(private repo: OrdersRepo) {} listOrders() { return this.repo.list(); } }\n",
    );
    write_file(
        dir.path(),
        "src/repositories/orders_repo.ts",
        "import { Order } from '../domain/order';\n\
         export class OrdersRepo { list(): Order[] { return []; } }\n",
    );
    write_file(dir.path(), "src/domain/order.ts", "export class Order {}\n");

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_import_boundary_family();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a well-formed worktree");

    assert!(
        outcome.violated.is_empty(),
        "every import stays within its configured boundary -> must not bounce: {:?}",
        outcome.violated
    );
}

/// An architectural rule that is NOT armed for this role/work item must never bounce the loop,
/// even though its checker's interest files are present and would otherwise match.
#[tokio::test]
async fn unarmed_import_boundary_rule_does_not_bounce_the_combined_runner() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_architecture_toml(dir.path());
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "import { OrdersRepo } from '../repositories/orders_repo';\n",
    );
    write_file(dir.path(), "src/repositories/orders_repo.ts", "export class OrdersRepo {}\n");

    let runner = runner_for_worktree(dir.path());
    let role = Role {
        name: "Backend".to_string(),
        rule_subset: vec![], // nothing armed
        allowed_paths: vec![],
    };
    let outcome = runner.check(&role, dir.path()).await.expect("gov-dev loop check must not error");

    assert!(
        outcome.violated.is_empty(),
        "no import-boundary rule armed -> the combined runner must not bounce on it: {:?}",
        outcome.violated
    );
}

/// D3 at Layer-2: a worktree with the IDENTICAL violation shape but NO
/// `.camerata/architecture.toml` at all must not bounce — the checker abstains entirely
/// (config-gated, per design doc D3), so an unconfigured repo never gets a false deterministic
/// verdict from this gate either.
#[tokio::test]
async fn unconfigured_worktree_with_the_same_import_shape_does_not_bounce() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Deliberately NO .camerata/architecture.toml.
    write_file(
        dir.path(),
        "src/routes/orders.ts",
        "import { OrdersRepo } from '../repositories/orders_repo';\n",
    );
    write_file(dir.path(), "src/repositories/orders_repo.ts", "export class OrdersRepo {}\n");

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_import_boundary_family();
    let outcome = runner.check(&role, dir.path()).await.expect("gov-dev loop check must not error");

    assert!(
        outcome.violated.is_empty(),
        "no .camerata/architecture.toml -> the checker must abstain, never guess: {:?}",
        outcome.violated
    );
}
