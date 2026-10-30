//! Rank 5, result-cache hardening. A carried-forward AI finding is only as good as the question it
//! answered: the model that answered, the prompt it was asked under, and the rule text it was
//! asked to enforce. The cache used to key on the file fingerprint and the rule IDS alone, so
//! changing the model, the prompt or a rule's directive reused stale results.

use super::*;

const MODEL: &str = "claude-sonnet-5";

fn rule(id: &str, directive: &str) -> crate::onboard::SelectedRule {
    crate::onboard::SelectedRule {
        id: id.to_string(),
        directive: directive.to_string(),
        repos: Vec::new(),
    }
}

fn key(
    model: &str,
    calibration: Option<&str>,
    rules: &[crate::onboard::SelectedRule],
) -> AiScanKey {
    AiScanKey::new(model, calibration, rules)
}

fn manifest_under(rules_fp: &str, key: &AiScanKey) -> ScanManifest {
    ManifestBuilder::new()
        .with_rules_fingerprint(rules_fp.to_string())
        .with_ai_key(key.clone())
        .with_ai_covered(true)
        .finish()
}

#[test]
fn the_same_inputs_hit_the_cache() {
    let rules = [rule("ARCH-A-1", "do A"), rule("SEC-B-1", "do B")];
    let stored = key(MODEL, None, &rules);
    let m = manifest_under("fp", &stored);
    assert!(m.is_reusable_baseline("fp", &key(MODEL, None, &rules)));
    // Rule order is not part of the question.
    let reordered = [rule("SEC-B-1", "do B"), rule("ARCH-A-1", "do A")];
    assert!(m.is_reusable_baseline("fp", &key(MODEL, None, &reordered)));
}

#[test]
fn a_changed_model_misses() {
    let rules = [rule("ARCH-A-1", "do A")];
    let m = manifest_under("fp", &key(MODEL, None, &rules));
    assert!(!m.is_reusable_baseline("fp", &key("claude-opus-5", None, &rules)));
}

#[test]
fn a_changed_calibration_model_misses() {
    // Carried findings keep the severity and confidence the calibration pass gave them.
    let rules = [rule("ARCH-A-1", "do A")];
    let m = manifest_under("fp", &key(MODEL, None, &rules));
    assert!(!m.is_reusable_baseline("fp", &key(MODEL, Some("claude-haiku-4-5"), &rules)));
}

#[test]
fn a_changed_prompt_schema_version_misses() {
    let rules = [rule("ARCH-A-1", "do A")];
    let stored = key(MODEL, None, &rules);
    let m = manifest_under("fp", &stored);
    let bumped = AiScanKey {
        prompt_schema_version: stored.prompt_schema_version + 1,
        ..stored
    };
    assert!(!m.is_reusable_baseline("fp", &bumped));
}

#[test]
fn a_changed_rule_directive_misses_even_though_the_rule_id_is_the_same() {
    // The architect picked another option for the same rule: same id, different question.
    let m = manifest_under("fp", &key(MODEL, None, &[rule("ARCH-A-1", "use layering")]));
    assert!(!m.is_reusable_baseline(
        "fp",
        &key(MODEL, None, &[rule("ARCH-A-1", "use hexagonal")])
    ));
}

#[test]
fn the_existing_gates_still_apply() {
    let rules = [rule("ARCH-A-1", "do A")];
    let k = key(MODEL, None, &rules);
    // A different rule selection (the rules fingerprint) still misses.
    assert!(!manifest_under("fp", &k).is_reusable_baseline("other-fp", &k));
    // A run where the AI did not complete is never an AI baseline.
    let not_covered = ManifestBuilder::new()
        .with_rules_fingerprint("fp".to_string())
        .with_ai_key(k.clone())
        .with_ai_covered(false)
        .finish();
    assert!(!not_covered.is_reusable_baseline("fp", &k));
}

#[test]
fn a_manifest_that_predates_the_key_never_matches_a_real_one() {
    let rules = [rule("ARCH-A-1", "do A")];
    let legacy: ScanManifest = serde_json::from_str(
        r#"{"version":2,"rules_fingerprint":"fp","files":{},"findings":[],"ai_covered":true}"#,
    )
    .unwrap();
    assert!(!legacy.is_reusable_baseline("fp", &key(MODEL, None, &rules)));
}

#[test]
fn the_manifest_version_is_bumped_so_every_old_cache_is_dropped() {
    assert_eq!(MANIFEST_VERSION, 3);
    let old = ScanManifest {
        version: 2,
        ..ScanManifest::default()
    };
    assert!(!old.is_current());
    // And `partition` treats a non-current prior as no cache at all.
    let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let p = partition(Some(&old), "me/api", &files);
    assert_eq!(p.changed.len(), 1);
    assert!(p.carried.is_empty());
}

#[test]
fn the_key_round_trips_through_the_persisted_manifest() {
    let rules = [rule("ARCH-A-1", "do A")];
    let m = manifest_under("fp", &key(MODEL, Some("claude-haiku-4-5"), &rules));
    let back: ScanManifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert!(back.is_reusable_baseline("fp", &key(MODEL, Some("claude-haiku-4-5"), &rules)));
}

/// The audit system prompt is half of "the prompt" a carried finding answered. Editing it without
/// bumping [`crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION`] would reuse answers to the OLD
/// prompt, so the pair is pinned: change the prompt, bump the version, update both numbers here.
/// The pin covers the untrusted-content clause every scan call appends
/// ([`crate::prompt_frame::UNTRUSTED_CONTENT_CLAUSE`]) as well.
#[test]
fn the_system_prompt_is_pinned_to_the_prompt_schema_version() {
    let digest = format!(
        "{:016x}",
        crate::suppression::fnv1a(&format!(
            "{}{}",
            crate::ai_audit::audit_system_prompt(),
            crate::prompt_frame::UNTRUSTED_CONTENT_CLAUSE
        ))
    );
    assert_eq!(
        (
            crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
            digest.as_str()
        ),
        (4, "a46a9cedc3a8fa97"),
        "the audit system prompt changed: bump AUDIT_PROMPT_SCHEMA_VERSION and update this pin"
    );
}

/// Rank 4: `terse_output` changes what the model is asked, so a baseline written under the
/// other setting is not reused. A key stored before the field existed reads as `false`, which
/// is what those findings were produced under.
#[test]
fn the_terse_output_setting_is_part_of_the_question() {
    let rules = [rule("ARCH-A-1", "do A")];
    let stored_off = key(MODEL, None, &rules);
    let m = manifest_under("fp", &stored_off);
    assert!(m.is_reusable_baseline("fp", &key(MODEL, None, &rules)));
    assert!(!m.is_reusable_baseline("fp", &key(MODEL, None, &rules).with_terse_output(true)));

    let m_on = manifest_under("fp", &key(MODEL, None, &rules).with_terse_output(true));
    assert!(m_on.is_reusable_baseline("fp", &key(MODEL, None, &rules).with_terse_output(true)));
    assert!(!m_on.is_reusable_baseline("fp", &key(MODEL, None, &rules)));

    // A key persisted before the field existed.
    let legacy: AiScanKey = serde_json::from_str(
        r#"{"audit_model":"claude-sonnet-5","calibration_model":"","prompt_schema_version":1,"directives_fingerprint":"x"}"#,
    )
    .unwrap();
    assert!(!legacy.terse_output);
}

/// The terse-output prompts are the other half of the question a `terse_output` finding
/// answered: the terse scan system prompt, the deep security lens's, and the prompt that writes
/// the full detail afterwards. Editing any of them without bumping
/// [`crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION`] would reuse answers to the old prompt, so
/// they are pinned the same way: change one, bump the version, update the numbers here.
#[test]
fn the_terse_prompts_are_pinned_to_the_prompt_schema_version() {
    let hash = |text: String| format!("{:016x}", crate::suppression::fnv1a(&text));
    assert_eq!(
        (
            crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
            hash(crate::ai_audit::audit_system_prompt_with(true)).as_str(),
            hash(crate::ai_audit::deep_security_system_prompt_with(true)).as_str(),
            hash(crate::ai_audit::detail_regen::detail_regen_system_prompt()).as_str(),
        ),
        (
            4,
            "5bc27c39facb8c9d",
            "fb0d0667d9ea0281",
            "dc1838569c7357fc"
        ),
        "a terse-output prompt changed: bump AUDIT_PROMPT_SCHEMA_VERSION and update this pin"
    );
}

/// Rank 7: `applies_to_routing` decides which rules are sent with which files, so findings
/// written under one setting answered a different question than the other setting asks. Neither
/// setting's cache answers the other, in both directions. A key stored before the field existed
/// reads as `false`: those findings were produced without `applies_to` routing.
#[test]
fn the_applies_to_routing_setting_is_part_of_the_question() {
    let rules = [rule("ARCH-A-1", "do A")];
    let off = key(MODEL, None, &rules);
    let on = key(MODEL, None, &rules).with_applies_to_routing(true);
    assert_ne!(off, on);

    let m_off = manifest_under("fp", &off);
    assert!(m_off.is_reusable_baseline("fp", &key(MODEL, None, &rules)));
    assert!(
        !m_off.is_reusable_baseline("fp", &on),
        "off's cache must not answer on"
    );

    let m_on = manifest_under("fp", &on);
    assert!(m_on.is_reusable_baseline(
        "fp",
        &key(MODEL, None, &rules).with_applies_to_routing(true)
    ));
    assert!(
        !m_on.is_reusable_baseline("fp", &off),
        "on's cache must not answer off"
    );

    let legacy: AiScanKey = serde_json::from_str(
        r#"{"audit_model":"claude-sonnet-5","calibration_model":"","prompt_schema_version":1,"directives_fingerprint":"x","terse_output":true}"#,
    )
    .unwrap();
    assert!(!legacy.applies_to_routing);
    assert!(legacy.terse_output);
}
