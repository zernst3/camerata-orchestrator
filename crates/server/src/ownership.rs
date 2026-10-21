//! W33 — the OWNERSHIP axis: who can fix a finding (design:
//! `docs/design/2026-10-21_unfixable-findings-class.md`).
//!
//! A finding can be real and correctly detected yet not fixable in the client's OWN first-party
//! code. Ranking such a finding beside an actionable one spends the client's attention on
//! something they cannot act on; dropping it hides a real observation. The resolution is a
//! structured axis, derived here and consumed by the exporters: a non-`ClientOwned` finding
//! keeps its true severity but is routed to a "Findings outside your direct control" section and
//! excluded from the remediation budget.
//!
//! Derivation is PURE over the finding's `rule_id` and `captures` — the DETECTORS (which know
//! where the finding lives) populate structured capture keys; this module only reads them, so
//! the logic is one source of truth shared by the PDF path (`report_export`) and the FindingRow
//! path (`xlsx_export`), and is testable without constructing a full `Finding`.
//!
//! Capture-key contract:
//! - `platform_schema` — set by the RLS/storage checker when the finding's resource is in a
//!   platform-owned schema (Supabase `auth`/`storage`/`realtime`/`vault`). Presence ⇒
//!   `PlatformManaged`.
//! - `osv_fixed_version` — set by `dep_audit::parse_osv_json` when OSV reports a patched version.
//!   Presence ⇒ the dependency CVE is fixable by upgrade (`ClientOwned`); absence ⇒
//!   `ExternalDependency` (no patch yet).

use std::collections::BTreeMap;

pub use camerata_api_types::audit::Ownership;

use crate::dep_audit::DEP_AUDIT_RULE_ID;

/// Derive a finding's [`Ownership`] from its `rule_id` and `captures`. Pure; see the capture-key
/// contract in the module doc. Defaults to `ClientOwned` — the normal, actionable case.
pub fn ownership_of(rule_id: &str, captures: &BTreeMap<String, String>) -> Ownership {
    // A detector tagged the resource as living in a platform-owned schema.
    if captures.get("platform_schema").is_some_and(|s| !s.trim().is_empty()) {
        return Ownership::PlatformManaged;
    }
    // A known-vulnerable dependency: fixable (ClientOwned) IFF a patched version exists — an
    // upgrade is the fix; otherwise it is not-yet-fixable and outside the client's control.
    if rule_id == DEP_AUDIT_RULE_ID {
        return match captures.get("osv_fixed_version") {
            Some(v) if !v.trim().is_empty() => Ownership::ClientOwned,
            _ => Ownership::ExternalDependency,
        };
    }
    Ownership::ClientOwned
}

/// The realistic client ACTION for a finding the client cannot fix in their own code — what to
/// do instead of a code fix. `None` for `ClientOwned`: the normal fix flow (`resolve_fix`)
/// applies, so there is nothing special to say. Every non-`ClientOwned` finding gets a concrete
/// instruction here, so the "outside your control" section never ships a bare "you can't fix
/// this" (the W33 gate).
pub fn ownership_action(ownership: Ownership) -> Option<&'static str> {
    match ownership {
        Ownership::ClientOwned => None,
        Ownership::PlatformManaged => Some(
            "This is a platform-managed resource you do not control. Accept the risk with a \
             documented justification, or escalate to the platform provider; it cannot be fixed \
             in your own code.",
        ),
        Ownership::ExternalDependency => Some(
            "No patched version is available yet. Monitor the advisory and upgrade when a fix \
             ships, accept the risk in the meantime, or replace the dependency.",
        ),
        Ownership::Generated => Some(
            "Fix this in the generator or its source, not in the generated artifact — the \
             change would be overwritten on the next generation.",
        ),
        Ownership::Vendored => Some(
            "Fix this upstream in the vendored dependency's own source, then re-vendor; editing \
             the vendored copy in place diverges it from upstream.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn default_is_client_owned() {
        assert_eq!(ownership_of("ARCH-STRICT-LAYERING-1", &caps(&[])), Ownership::ClientOwned);
        assert!(ownership_action(Ownership::ClientOwned).is_none());
    }

    #[test]
    fn platform_schema_capture_makes_it_platform_managed() {
        let o = ownership_of("SUPABASE-RLS-ENABLED-1", &caps(&[("platform_schema", "auth")]));
        assert_eq!(o, Ownership::PlatformManaged);
        assert!(!o.is_client_fixable());
        assert!(ownership_action(o).unwrap().contains("platform-managed"));
    }

    #[test]
    fn empty_platform_schema_is_ignored() {
        // A blank capture must not trip the branch.
        assert_eq!(
            ownership_of("SUPABASE-RLS-ENABLED-1", &caps(&[("platform_schema", "")])),
            Ownership::ClientOwned
        );
    }

    #[test]
    fn dependency_with_a_fix_is_client_owned_dependency_without_is_external() {
        // A CVE WITH a patched version is fixable by upgrade → client-owned.
        assert_eq!(
            ownership_of(DEP_AUDIT_RULE_ID, &caps(&[("osv_fixed_version", "1.2.3")])),
            Ownership::ClientOwned
        );
        // A CVE with NO patch is not-yet-fixable → external-dependency (this class).
        let o = ownership_of(DEP_AUDIT_RULE_ID, &caps(&[]));
        assert_eq!(o, Ownership::ExternalDependency);
        assert!(ownership_action(o).unwrap().contains("No patched version"));
    }

    #[test]
    fn platform_schema_wins_over_dependency() {
        // If somehow both signals are present, platform-managed (fully unfixable) takes priority.
        let o = ownership_of(
            DEP_AUDIT_RULE_ID,
            &caps(&[("platform_schema", "storage"), ("osv_fixed_version", "9.9.9")]),
        );
        assert_eq!(o, Ownership::PlatformManaged);
    }

    #[test]
    fn every_non_client_owned_ownership_has_an_action() {
        for o in [
            Ownership::PlatformManaged,
            Ownership::ExternalDependency,
            Ownership::Generated,
            Ownership::Vendored,
        ] {
            assert!(ownership_action(o).is_some(), "{o:?} must carry a client action");
        }
    }
}
