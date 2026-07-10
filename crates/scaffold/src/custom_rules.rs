//! The skeleton's two INVENTED rules — DB-on-demand and the mandatory auto-capture
//! reporter — are CUSTOM rules, not corpus rules (Zach's call: "these are more like
//! custom rules than corpus rules"). Camerata already has a first-class custom-rules
//! feature (`CustomRule` in `camerata_app_core::project`, rendered as `### CUSTOM-<name>`
//! markdown blocks by `render_custom` in `crates/server/src/arm.rs`) — these two
//! project-specific decisions belong there, not framed with invented corpus-style IDs
//! (`DB-ON-DEMAND-1`, `PWA-AUTO-CAPTURE-1`) that imply they came from the shared rule
//! corpus. They didn't; they're specific to this scaffolder template.
//!
//! This module is the SINGLE source of truth for both rules' body text: the
//! `templates/skeleton/CONVENTIONS.md` template's two `### CUSTOM-*` blocks are
//! authored to read identically to [`default_custom_rules`]'s bodies (checked by
//! `skeleton::tests::conventions_md_reformats_invented_rules_as_custom_blocks`), so a
//! human reading the freshly scaffolded repo sees the exact same rule text a
//! Camerata project's ruleset will show them.
//!
//! Deliberately returns plain `(name, body)` string pairs, not `camerata_app_core`'s
//! `CustomRule` type — this crate is leaf-ish (no camerata-crate deps, see
//! `Cargo.toml`'s comment) so the future orchestrator/server can depend on it without
//! pulling anything else in. The caller (the Part 2 server endpoint) wraps these into
//! real `CustomRule`s when it seeds the new project's ruleset.

/// The two custom rules this skeleton ships with, as `(name, body)` pairs. `name` is
/// the bare identifier (e.g. `"db-on-demand"`) — a renderer prefixes it with
/// `CUSTOM-` (e.g. `### CUSTOM-db-on-demand`); `body` is the free-text directive.
pub fn default_custom_rules() -> Vec<(&'static str, &'static str)> {
    vec![
        ("db-on-demand", DB_ON_DEMAND_BODY),
        ("pwa-auto-capture", PWA_AUTO_CAPTURE_BODY),
    ]
}

/// Body text for the `CUSTOM-db-on-demand` rule. Kept in sync (by hand, checked by a
/// test) with the equivalent paragraph in `templates/skeleton/CONVENTIONS.md`.
pub const DB_ON_DEMAND_BODY: &str = "No database until a requirement needs one. This skeleton ships with no Postgres, no `sqlx`, no migrations, and no ORM. Persistence is added by a later phase only when the app's actual requirements need it (see `AppRequirements::needs_persistence` in the scaffolder) — never speculatively.";

/// Body text for the `CUSTOM-pwa-auto-capture` rule. Kept in sync (by hand, checked
/// by a test) with the equivalent paragraph in `templates/skeleton/CONVENTIONS.md`.
pub const PWA_AUTO_CAPTURE_BODY: &str = "The auto-capture reporter is never removed or bypassed. `assets/error-reporter.js` (window.onerror / unhandledrejection / failed-fetch) and `src/wasm_bridge.rs` (the Rust panic hook) together catch runtime defects before a user has to report them, and POST a `DefectReport`-shaped payload to the capture endpoint. New code must not swallow errors in a way that keeps them from reaching these listeners (e.g. a blanket `try/catch` around the whole render tree with no re-throw), and must not remove the reporter to \"clean up\" the entrypoint.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_exactly_the_two_invented_rules() {
        let rules = default_custom_rules();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].0, "db-on-demand");
        assert_eq!(rules[1].0, "pwa-auto-capture");
        assert!(!rules[0].1.trim().is_empty());
        assert!(!rules[1].1.trim().is_empty());
    }

    #[test]
    fn bodies_carry_no_invented_corpus_id() {
        // These are CUSTOM rules, not corpus rules — their bodies must not smuggle in
        // an invented corpus-style ID (that framing is exactly what Zach's decision
        // removed).
        for (_, body) in default_custom_rules() {
            assert!(!body.contains("DB-ON-DEMAND-1"));
            assert!(!body.contains("PWA-AUTO-CAPTURE-1"));
        }
    }
}
