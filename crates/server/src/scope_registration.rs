//! GAP-8: resolving a routine's STRUCTURED scope into the concrete gateway
//! session registration a live governed run uses.
//!
//! A [`RoutineScope`] (from `camerata_app_core::routine`) is a structured,
//! enforceable boundary: a rule subset, a write policy (which drives the tool
//! allowlist), and a write jail. This module turns that scope into the SAME
//! three inputs a DEV run hands the gateway when it spawns a session:
//!
//!   1. a [`Role`] whose `rule_subset` is the enforced gate rules (the floor a
//!      routine's scope can never lower) unioned with any explicit domain rules
//!      the scope names — built exactly like the investigation runner's role via
//!      [`camerata_fleet::governed_role`];
//!   2. a TOOL ALLOWLIST derived from the role + the scope's write policy via
//!      [`camerata_agent::allowed_tools_for_role`] (the identical derivation a
//!      dev run's driver uses); and
//!   3. a WRITE JAIL: `Some(worktree)` when the scope grants write, `None` for a
//!      read-only scope (no write path at all).
//!
//! The resulting [`RoutineSessionRegistration`] is what a live routine run would
//! feed to [`camerata_agent::prepare_session`] (rule subset -> `rules.json`,
//! write jail -> `CAMERATA_WORKTREE_ROOT`, tool allowlist -> `--allowedTools`).
//! Live routine execution itself is still latent (see the routine ADR + GAP-8),
//! so no production caller runs a routine agent yet; the seam is real and tested
//! so a routine's scope WILL be enforced the moment execution lands, on the same
//! primitives dev runs already use.

use std::path::{Path, PathBuf};

use camerata_agent::allowed_tools_for_role;
use camerata_core::{Role, RuleId};
use camerata_fleet::governed_role;

use crate::routine::{PathScope, RoutineScope, RuleSubsetRef, WritePolicy};

/// The concrete gateway session registration a routine's scope resolves to: the
/// exact inputs [`camerata_agent::prepare_session`] needs. Mirrors what a dev run
/// registers, so a routine run enforces governance identically.
#[derive(Debug, Clone)]
pub struct RoutineSessionRegistration {
    /// The governed role (its `rule_subset` is the enforced gate-rule floor
    /// unioned with the scope's explicit domain rules). Passed to
    /// `prepare_session` as-is; serialized to `rules.json` for the gateway.
    pub role: Role,
    /// The `--allowedTools` list for the run, derived from the role. Read-only
    /// scopes get the read-only built-ins + `gated_write` (the uniform surface a
    /// dev run uses); the write path is gated regardless, and closed entirely
    /// when no write jail is registered.
    pub tool_allowlist: Vec<String>,
    /// The write jail. `Some(worktree)` when the scope grants write (passed to
    /// `prepare_session` as the worktree -> `CAMERATA_WORKTREE_ROOT`); `None` for
    /// a read-only scope, so `gated_write` has no target and the agent cannot
    /// write at all.
    pub write_jail: Option<PathBuf>,
}

impl RoutineSessionRegistration {
    /// Whether this registration grants any write path (a write jail is set).
    pub fn is_writing(&self) -> bool {
        self.write_jail.is_some()
    }
}

/// Resolve a routine's [`RoutineScope`] into the concrete gateway session
/// registration a live governed run uses.
///
/// `role_name` labels the role for provenance (e.g. the routine's name).
/// `worktree` is the run's prepared worktree, used as the write jail when the
/// scope's write policy grants write. A read-only scope ignores `worktree`
/// (returns `write_jail: None`) so no write path is opened.
///
/// Building the role is I/O (it reads the rule corpus), so this is async and
/// lives in the server adapter, not the pure `app-core` domain crate. The rule
/// subset ALWAYS includes every enforced gate rule (via `governed_role`), so a
/// routine's scope can only ADD domain rules on top of the gate floor — never
/// lower it.
pub async fn resolve_scope_registration(
    scope: &RoutineScope,
    role_name: &str,
    worktree: Option<&Path>,
) -> anyhow::Result<RoutineSessionRegistration> {
    // 1. RULE SUBSET: start from the governed role (enforced gate rules + the
    //    role's corpus rules), then union any explicit domain rules the scope
    //    names. `governed_role` guarantees the enforced gate floor is present.
    let mut role = governed_role(role_name).await?;
    if let RuleSubsetRef::Ids(extra) = &scope.rule_subset {
        for id in extra {
            if !role.rule_subset.contains(id) {
                role.rule_subset.push(id.clone());
            }
        }
    }

    // 2. TOOL ALLOWLIST: derived from the role exactly as a dev run's driver
    //    derives it (read-only built-ins + the single gated write tool). A
    //    routine is never an orchestrator, so `delegate`/`fan_out` are excluded.
    let tool_allowlist = allowed_tools_for_role(&role);

    // 3. WRITE JAIL: registered only when the scope's policy writes. A read-only
    //    scope resolves to `None` so `gated_write` has no target.
    let write_jail = match (scope.write, &scope.write_jail) {
        (WritePolicy::ReadOnly, _) => None,
        (_, Some(PathScope::Worktree)) => worktree.map(Path::to_path_buf),
        // A writing policy with no explicit jail still jails to the worktree
        // (the default write jail); a writing scope must never be un-jailed.
        (_, None) => worktree.map(Path::to_path_buf),
    };

    Ok(RoutineSessionRegistration {
        role,
        tool_allowlist,
        write_jail,
    })
}

/// The explicit-domain-rule ids a scope contributes on top of the gate floor,
/// for a quick assertion / display without resolving the whole role. `All`
/// contributes none (the floor IS the subset).
pub fn scope_extra_rule_ids(scope: &RoutineScope) -> Vec<RuleId> {
    match &scope.rule_subset {
        RuleSubsetRef::All => Vec::new(),
        RuleSubsetRef::Ids(ids) => ids.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_gateway::enforced_gate_rules;

    fn tmp_worktree() -> PathBuf {
        std::env::temp_dir().join("camerata-gap8-scope-worktree")
    }

    #[tokio::test]
    async fn read_only_scope_resolves_to_no_write_jail() {
        let scope = RoutineScope::from_legacy_string("read-only");
        let wt = tmp_worktree();
        let reg = resolve_scope_registration(&scope, "Nightly", Some(&wt))
            .await
            .expect("resolves");
        // No write path: even given a worktree, a read-only scope registers no jail.
        assert!(reg.write_jail.is_none());
        assert!(!reg.is_writing());
        // The tool allowlist still carries the uniform gated surface (the gate is
        // what closes the write path, via the absent jail).
        assert!(reg
            .tool_allowlist
            .iter()
            .any(|t| t == camerata_agent::GATED_WRITE_TOOL));
        // Read-only never grants the orchestrator-only delegate/fan-out tools.
        assert!(!reg.tool_allowlist.iter().any(|t| t.contains("delegate")));
    }

    #[tokio::test]
    async fn write_gated_scope_registers_the_worktree_jail() {
        let scope = RoutineScope::from_legacy_string("write (gated)");
        let wt = tmp_worktree();
        let reg = resolve_scope_registration(&scope, "Security", Some(&wt))
            .await
            .expect("resolves");
        assert_eq!(reg.write_jail.as_deref(), Some(wt.as_path()));
        assert!(reg.is_writing());
    }

    #[tokio::test]
    async fn resolved_role_always_carries_the_enforced_gate_floor() {
        // Whatever the scope, the resolved rule subset includes EVERY enforced
        // gate rule — a routine's scope can never lower the gate floor.
        let scope = RoutineScope::from_legacy_string("read-only");
        let reg = resolve_scope_registration(&scope, "Audit", None)
            .await
            .expect("resolves");
        for gate_rule in enforced_gate_rules() {
            assert!(
                reg.role.rule_subset.contains(&gate_rule),
                "enforced gate rule {gate_rule:?} must be in the resolved subset"
            );
        }
    }

    #[tokio::test]
    async fn explicit_domain_rules_are_unioned_on_top_of_the_floor() {
        let scope = RoutineScope {
            rule_subset: RuleSubsetRef::Ids(vec![RuleId("ZZZ-CUSTOM-1".to_string())]),
            write: WritePolicy::WriteGated,
            write_jail: Some(PathScope::Worktree),
            note: "custom".to_string(),
        };
        let wt = tmp_worktree();
        let reg = resolve_scope_registration(&scope, "Custom", Some(&wt))
            .await
            .expect("resolves");
        // The explicit domain rule is present on TOP of the enforced floor.
        assert!(reg
            .role
            .rule_subset
            .contains(&RuleId("ZZZ-CUSTOM-1".to_string())));
        for gate_rule in enforced_gate_rules() {
            assert!(reg.role.rule_subset.contains(&gate_rule));
        }
        // And it writes, jailed to the worktree.
        assert_eq!(reg.write_jail.as_deref(), Some(wt.as_path()));
    }

    #[test]
    fn scope_extra_rule_ids_reads_the_explicit_subset() {
        assert!(scope_extra_rule_ids(&RoutineScope::from_legacy_string("read-only")).is_empty());
        let scope = RoutineScope {
            rule_subset: RuleSubsetRef::Ids(vec![RuleId("SEC-1".to_string())]),
            ..RoutineScope::default()
        };
        assert_eq!(
            scope_extra_rule_ids(&scope),
            vec![RuleId("SEC-1".to_string())]
        );
    }
}
