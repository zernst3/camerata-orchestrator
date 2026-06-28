//! Hermetic end-to-end integration tests for the VCS-action gate.
//!
//! These exercise the PUBLIC API of `camerata_checks::vcs_action` exactly as a
//! caller would: start from a serde-shaped [`ProcessRuleConfig`], build the live
//! rule set with [`build_rules`], and assert the resulting gate decision
//! (`gate` / `gate_or_bypass`) against concrete commit / PR / branch actions.
//!
//! They are deliberately black-box (no access to the in-module helpers) and pure
//! (no I/O, no git, no network) — the same determinism the gate itself holds.
//! The goal is to surface integration bugs in the CONFIG → RULES → DECISION
//! pipeline (enable/disable interplay, option propagation, PR coverage,
//! per-target application) before QA.

use camerata_checks::vcs_action::{
    build_rules, gate, gate_or_bypass, AdoLinkConfig, BranchNamingConfig, BypassRequest,
    CommitDocConfig, ConventionalCommitConfig, GateOrBypassResult, IdLocation, PrCoverage,
    ProcessRuleConfig, StoryIdFormat, VcsAction, VcsTarget,
};

// ── small constructors ──────────────────────────────────────────────────────

fn commit(message: &str) -> VcsAction {
    VcsAction::Commit {
        message: message.to_string(),
    }
}

fn pr(title: &str, body: &str) -> VcsAction {
    VcsAction::PullRequest {
        title: title.to_string(),
        body: body.to_string(),
    }
}

fn branch(name: &str) -> VcsAction {
    VcsAction::Branch {
        name: name.to_string(),
    }
}

/// A config that disables every rule, so each test can enable exactly the rule
/// it is exercising and assert on it in isolation (no cross-rule interference).
fn all_disabled() -> ProcessRuleConfig {
    ProcessRuleConfig {
        commit_doc: CommitDocConfig {
            enabled: false,
            ..Default::default()
        },
        conventional_commit: ConventionalCommitConfig {
            enabled: false,
            ..Default::default()
        },
        branch_naming: BranchNamingConfig {
            enabled: false,
            ..Default::default()
        },
        ado_link: AdoLinkConfig {
            enabled: false,
            ..Default::default()
        },
        pr: PrCoverage::default(),
    }
}

fn rule_ids(config: &ProcessRuleConfig) -> Vec<String> {
    build_rules(config).into_iter().map(|r| r.id).collect()
}

// ════════════════════════════════════════════════════════════════════════════
// build_rules: enabled-set selection + option propagation
// ════════════════════════════════════════════════════════════════════════════

/// The shipped default config: conventional-commit + commit-doc enabled,
/// branch-naming + ado-link disabled. This is the contract every project that
/// does not set an explicit config inherits.
#[test]
fn default_config_builds_exactly_the_two_default_on_rules() {
    let ids = rule_ids(&ProcessRuleConfig::default());
    assert!(
        ids.contains(&"PROCESS-CONVENTIONAL-COMMIT-1".to_string()),
        "conventional-commit on by default: {ids:?}"
    );
    assert!(
        ids.contains(&"PROCESS-COMMIT-DOC-1".to_string()),
        "commit-doc on by default: {ids:?}"
    );
    assert!(
        !ids.contains(&"PROCESS-BRANCH-NAMING-1".to_string()),
        "branch-naming off by default: {ids:?}"
    );
    assert!(
        !ids.contains(&"PROCESS-ADO-LINK-1".to_string()),
        "ado-link off by default: {ids:?}"
    );
}

/// An all-disabled config builds an EMPTY rule set: nothing is enforced, so
/// every action passes the gate. A disabled rule is genuinely absent, not just
/// inert.
#[test]
fn all_disabled_config_builds_no_rules_and_gates_everything_open() {
    let config = all_disabled();
    let rules = build_rules(&config);
    assert!(rules.is_empty(), "no rules when all disabled: {rules:?}");

    // With no rules, even a deliberately non-conforming action passes.
    assert!(gate(&rules, &commit("garbage subject, no body")).is_ok());
    assert!(gate(&rules, &pr("garbage", "")).is_ok());
    assert!(gate(&rules, &branch("nonsense-branch")).is_ok());
}

/// Every rule turned on: build_rules must include all four PROCESS-* families.
#[test]
fn fully_enabled_config_builds_all_four_rule_families() {
    let config = ProcessRuleConfig {
        commit_doc: CommitDocConfig {
            enabled: true,
            ..Default::default()
        },
        conventional_commit: ConventionalCommitConfig {
            enabled: true,
            ..Default::default()
        },
        branch_naming: BranchNamingConfig {
            enabled: true,
            ..Default::default()
        },
        ado_link: AdoLinkConfig {
            enabled: true,
            ..Default::default()
        },
        pr: PrCoverage::default(),
    };
    let ids = rule_ids(&config);
    for expected in [
        "PROCESS-COMMIT-DOC-1",
        "PROCESS-CONVENTIONAL-COMMIT-1",
        "PROCESS-BRANCH-NAMING-1",
        "PROCESS-ADO-LINK-1",
    ] {
        assert!(
            ids.contains(&expected.to_string()),
            "{expected} must be present when enabled: {ids:?}"
        );
    }
}

/// The conventional-commit `types` option propagates into the built rule: a
/// custom type set is honoured, and the standard default types are honoured by
/// the default config.
#[test]
fn build_rules_propagates_conventional_commit_types() {
    // Custom set: only `feat` + `spike`.
    let mut config = all_disabled();
    config.conventional_commit = ConventionalCommitConfig {
        enabled: true,
        types: vec!["feat".to_string(), "spike".to_string()],
    };
    let rules = build_rules(&config);

    assert!(gate(&rules, &commit("spike: explore caching")).is_ok());
    assert!(
        gate(&rules, &commit("chore: tidy")).is_err(),
        "chore not in custom type set -> denied"
    );

    // Default config's conventional types include `chore`.
    let default_rules = build_rules(&ProcessRuleConfig::default());
    // Need a substantive body too, since commit-doc is on by default.
    assert!(gate(
        &default_rules,
        &commit("chore: tidy up\n\nRemoves dead code paths across modules. Refs #5.")
    )
    .is_ok());
}

/// The branch-naming `prefixes` option propagates into the built rule.
#[test]
fn build_rules_propagates_branch_prefixes() {
    let mut config = all_disabled();
    config.branch_naming = BranchNamingConfig {
        enabled: true,
        prefixes: vec!["spike/".to_string(), "wip/".to_string()],
    };
    let rules = build_rules(&config);

    assert!(gate(&rules, &branch("spike/cache-poc")).is_ok());
    assert!(gate(&rules, &branch("wip/draft")).is_ok());
    assert!(
        gate(&rules, &branch("feature/login")).is_err(),
        "feature/ not in the configured prefix set -> denied"
    );
}

/// The ado-link `prefix` option propagates: a project on a non-default prefix
/// (e.g. `JIRA`) gates on `JIRA#<n>` and not on `AB#<n>`.
#[test]
fn build_rules_propagates_ado_prefix() {
    let mut config = all_disabled();
    config.ado_link = AdoLinkConfig {
        enabled: true,
        prefix: "JIRA".to_string(),
    };
    let rules = build_rules(&config);

    assert!(gate(&rules, &commit("JIRA#77 add export")).is_ok());
    assert!(
        gate(&rules, &commit("AB#77 add export")).is_err(),
        "AB# does not satisfy a JIRA# prefix rule"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// PROCESS-CONVENTIONAL-COMMIT-1
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn conventional_commit_conforming_subject_passes() {
    let mut config = all_disabled();
    config.conventional_commit.enabled = true;
    let rules = build_rules(&config);

    for good in [
        "feat: add export",
        "fix(api): handle null",
        "feat!: breaking",
        "chore(deps)!: bump serde",
    ] {
        assert!(gate(&rules, &commit(good)).is_ok(), "should pass: {good:?}");
    }
}

#[test]
fn conventional_commit_malformed_subject_denied() {
    let mut config = all_disabled();
    config.conventional_commit.enabled = true;
    let rules = build_rules(&config);

    for bad in [
        "just a random message", // no type/colon
        "feat add export",       // missing colon
        "feat:",                 // empty subject
        "wip: not a known type", // unknown type
        "feat(scope: unbalanced paren",
    ] {
        let err = gate(&rules, &commit(bad)).expect_err("should deny");
        assert_eq!(err[0].rule_id, "PROCESS-CONVENTIONAL-COMMIT-1");
        assert_eq!(err[0].target, VcsTarget::CommitSubject);
    }
}

/// A disabled conventional-commit rule is a no-op: a non-conforming subject
/// passes because the rule is absent from the built set.
#[test]
fn conventional_commit_disabled_is_a_no_op() {
    let config = all_disabled(); // conventional_commit.enabled = false
    let rules = build_rules(&config);
    assert!(gate(&rules, &commit("definitely not conventional")).is_ok());
}

// ════════════════════════════════════════════════════════════════════════════
// PROCESS-COMMIT-DOC-1
// ════════════════════════════════════════════════════════════════════════════

/// Helper: commit-doc on, conventional-commit off, default story-id format
/// (bare `#<num>`), body location.
fn commit_doc_only(min_body: usize, require_story_id: bool) -> ProcessRuleConfig {
    let mut config = all_disabled();
    config.commit_doc = CommitDocConfig {
        enabled: true,
        min_body_chars: min_body,
        require_story_id,
        id_location: IdLocation::Body,
        story_id_format: StoryIdFormat::default(),
    };
    config
}

#[test]
fn commit_doc_substantive_body_with_story_id_passes() {
    let rules = build_rules(&commit_doc_only(20, true));
    let action = commit("feat: add export\n\nImplements the CSV export flow end to end. Refs #42.");
    assert!(gate(&rules, &action).is_ok());
}

#[test]
fn commit_doc_empty_or_trivial_body_denied() {
    let rules = build_rules(&commit_doc_only(20, true));

    // Subject only: empty body.
    let subject_only = commit("feat: add export");
    let err = gate(&rules, &subject_only).expect_err("subject-only must be denied");
    assert!(err
        .iter()
        .any(|v| v.rule_id == "PROCESS-COMMIT-DOC-1" && v.target == VcsTarget::CommitBody));

    // Trivial body below the char minimum (even with a story id present).
    let trivial = commit("feat: x\n\n#42");
    assert!(
        gate(&rules, &trivial).is_err(),
        "body under min_body_chars must be denied"
    );
}

#[test]
fn commit_doc_missing_required_story_id_denied() {
    let rules = build_rules(&commit_doc_only(20, true));
    // Body is long enough but has no #<num> story reference.
    let action = commit("feat: add export\n\nAdds the new export flow for CSV downloads now.");
    let err = gate(&rules, &action).expect_err("missing story id must be denied");
    assert!(err.iter().any(|v| v.rule_id == "PROCESS-COMMIT-DOC-1"));
}

#[test]
fn commit_doc_require_story_id_false_accepts_body_without_ref() {
    let rules = build_rules(&commit_doc_only(20, false));
    // Substantive body, no story ref — allowed because require_story_id=false.
    let action = commit("feat: add export\n\nAdds the new export flow for CSV downloads now.");
    assert!(gate(&rules, &action).is_ok());
    // But a too-short body is still denied (the length check still fires).
    assert!(gate(&rules, &commit("feat: x\n\ntiny")).is_err());
}

/// Story-id FORMAT option (prefix + separator) is honoured end to end.
#[test]
fn commit_doc_story_id_format_honored_jira_style() {
    let mut config = all_disabled();
    config.commit_doc = CommitDocConfig {
        enabled: true,
        min_body_chars: 10,
        require_story_id: true,
        id_location: IdLocation::Body,
        story_id_format: StoryIdFormat {
            prefix: "PROJ".to_string(),
            separator: '-',
            custom_regex: None,
        },
    };
    let rules = build_rules(&config);

    assert!(gate(
        &rules,
        &commit("feat: widget\n\nAdds the widget component. PROJ-42 tracked.")
    )
    .is_ok());
    // A bare #42 must NOT satisfy a PROJ-<n> rule.
    assert!(gate(
        &rules,
        &commit("feat: widget\n\nAdds the widget component. #42 tracked.")
    )
    .is_err());
}

/// Story-id LOCATION option: `Subject` requires the id in the subject, not the
/// body. This drives the multi-rule expansion inside build_rules.
#[test]
fn commit_doc_id_location_subject_checks_subject_not_body() {
    let mut config = all_disabled();
    config.commit_doc = CommitDocConfig {
        enabled: true,
        min_body_chars: 10,
        require_story_id: true,
        id_location: IdLocation::Subject,
        story_id_format: StoryIdFormat::default(),
    };
    let rules = build_rules(&config);

    // Id in subject, substantive body without id: passes.
    let ok = commit("fix: #42 handle null\n\nFixes the null pointer in the handler path.");
    assert!(gate(&rules, &ok).is_ok());

    // Id only in body: subject lacks it -> denied.
    let bad = commit("fix: handle null\n\nFixes the null pointer path. Refs #42. Long enough.");
    assert!(gate(&rules, &bad).is_err());
}

/// Story-id LOCATION `Either`: id in subject OR body is sufficient; absent from
/// both is denied.
#[test]
fn commit_doc_id_location_either_accepts_subject_or_body() {
    let mut config = all_disabled();
    config.commit_doc = CommitDocConfig {
        enabled: true,
        min_body_chars: 10,
        require_story_id: true,
        id_location: IdLocation::Either,
        story_id_format: StoryIdFormat::default(),
    };
    let rules = build_rules(&config);

    let in_subject = commit("fix: #42 handle null\n\nFixes the null pointer handler path here.");
    let in_body = commit("fix: handle null\n\nFixes the null pointer handler path. Refs #42.");
    let nowhere = commit("fix: handle null\n\nFixes the null pointer handler path here now.");

    assert!(gate(&rules, &in_subject).is_ok(), "id in subject");
    assert!(gate(&rules, &in_body).is_ok(), "id in body");
    assert!(gate(&rules, &nowhere).is_err(), "id in neither -> denied");
}

#[test]
fn commit_doc_disabled_is_a_no_op() {
    let config = all_disabled(); // commit_doc.enabled = false
    let rules = build_rules(&config);
    // A bare subject-only commit passes when the doc rule is off.
    assert!(gate(&rules, &commit("feat: no body at all")).is_ok());
}

/// A branch action is out of scope for commit-doc (no CommitBody/PrBody slice),
/// so it is never gated by it.
#[test]
fn commit_doc_does_not_gate_branch_actions() {
    let rules = build_rules(&commit_doc_only(20, true));
    assert!(gate(&rules, &branch("feature/export")).is_ok());
}

// ════════════════════════════════════════════════════════════════════════════
// PROCESS-BRANCH-NAMING-1 (opt-in / default-off)
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn branch_naming_allowed_prefix_passes_disallowed_denied() {
    let mut config = all_disabled();
    config.branch_naming = BranchNamingConfig {
        enabled: true,
        prefixes: vec!["feature/".to_string(), "release/".to_string(), "hotfix/".to_string()],
    };
    let rules = build_rules(&config);

    assert!(gate(&rules, &branch("feature/login")).is_ok());
    assert!(gate(&rules, &branch("release/v1.2.0")).is_ok());

    let err = gate(&rules, &branch("my-random-branch")).expect_err("disallowed prefix denied");
    assert_eq!(err[0].rule_id, "PROCESS-BRANCH-NAMING-1");
    assert_eq!(err[0].target, VcsTarget::BranchName);
}

/// Branch naming is default-off: it only fires when explicitly enabled. With the
/// shipped default config, ANY branch name passes (the rule is absent).
#[test]
fn branch_naming_default_off_does_not_fire() {
    let rules = build_rules(&ProcessRuleConfig::default());
    assert!(
        gate(&rules, &branch("totally-nonstandard-branch")).is_ok(),
        "branch naming must not be enforced under the default config"
    );
}

/// Branch naming applies ONLY to branch actions: it never fires on a commit or a
/// PR (those have no BranchName slice).
#[test]
fn branch_naming_not_over_applied_to_commit_or_pr() {
    let mut config = all_disabled();
    config.branch_naming.enabled = true;
    let rules = build_rules(&config);

    // A commit / PR with a name that would never match a branch prefix still
    // passes — the branch rule simply does not apply to them.
    assert!(gate(&rules, &commit("anything at all here")).is_ok());
    assert!(gate(&rules, &pr("anything at all", "body")).is_ok());
}

// ════════════════════════════════════════════════════════════════════════════
// PROCESS-ADO-LINK-1 (subject + PR title; opt-in)
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn ado_link_reference_present_passes_absent_denied() {
    let mut config = all_disabled();
    config.ado_link = AdoLinkConfig {
        enabled: true,
        prefix: "AB".to_string(),
    };
    let rules = build_rules(&config);

    // Present in the commit subject.
    assert!(gate(&rules, &commit("AB#123 add export")).is_ok());
    // Absent from the commit subject.
    let err = gate(&rules, &commit("add export endpoint")).expect_err("missing AB# denied");
    assert_eq!(err[0].rule_id, "PROCESS-ADO-LINK-1");
    assert_eq!(err[0].target, VcsTarget::CommitSubject);
}

/// ADO link applies to the PR TITLE too (apply_id_rule default = true): present
/// in the title passes; absent from the title is denied even if the body has it.
#[test]
fn ado_link_checks_pr_title_not_body() {
    let mut config = all_disabled();
    config.ado_link = AdoLinkConfig {
        enabled: true,
        prefix: "AB".to_string(),
    };
    // default pr coverage: apply_id_rule = true
    let rules = build_rules(&config);

    // Title has the ref -> pass.
    assert!(gate(&rules, &pr("AB#123 Add export endpoint", "")).is_ok());

    // Title lacks the ref, body has it -> denied (the rule targets the title).
    let err = gate(&rules, &pr("Add export endpoint", "refs AB#123"))
        .expect_err("ref in body does not satisfy a title rule");
    assert!(err.iter().any(|v| v.target == VcsTarget::PrTitle));
}

/// When PR id-coverage is OFF (`apply_id_rule = false`), the ADO rule is built
/// to target only the commit subject, NOT the PR title. A PR with no ref then
/// passes (the rule does not over-apply to PRs).
#[test]
fn ado_link_pr_coverage_off_does_not_gate_pr_title() {
    let mut config = all_disabled();
    config.ado_link = AdoLinkConfig {
        enabled: true,
        prefix: "AB".to_string(),
    };
    config.pr = PrCoverage {
        apply_body_rule: false,
        apply_id_rule: false,
    };
    let rules = build_rules(&config);

    // Commit subject is still gated.
    assert!(gate(&rules, &commit("no ref here")).is_err());
    // But a PR without a ref passes, because apply_id_rule = false.
    assert!(
        gate(&rules, &pr("Add export endpoint", "")).is_ok(),
        "with apply_id_rule=false the ADO rule must not gate the PR title"
    );
}

#[test]
fn ado_link_disabled_is_a_no_op() {
    let config = all_disabled(); // ado_link.enabled = false
    let rules = build_rules(&config);
    assert!(gate(&rules, &commit("no AB ref at all")).is_ok());
    assert!(gate(&rules, &pr("no AB ref", "")).is_ok());
}

// ════════════════════════════════════════════════════════════════════════════
// PR vs commit application (PrCoverage)
// ════════════════════════════════════════════════════════════════════════════

/// commit-doc with apply_body_rule = true (default) gates the PR body too: an
/// empty PR body is denied.
#[test]
fn commit_doc_pr_coverage_on_gates_pr_body() {
    let mut config = commit_doc_only(20, true);
    config.pr = PrCoverage {
        apply_body_rule: true,
        apply_id_rule: true,
    };
    let rules = build_rules(&config);

    // Empty PR body -> denied.
    let err = gate(&rules, &pr("Add export endpoint", "")).expect_err("empty PR body denied");
    assert!(err.iter().any(|v| v.target == VcsTarget::PrBody));

    // Substantive PR body with story id -> passes.
    assert!(gate(
        &rules,
        &pr("Add export endpoint", "Implements the CSV export feature. Closes #99.")
    )
    .is_ok());
}

/// commit-doc with apply_body_rule = false must NOT gate the PR body: an empty
/// PR body passes (the rule covers only the commit body).
#[test]
fn commit_doc_pr_coverage_off_does_not_gate_pr_body() {
    let mut config = commit_doc_only(20, true);
    config.pr = PrCoverage {
        apply_body_rule: false,
        apply_id_rule: false,
    };
    let rules = build_rules(&config);

    // Empty PR body passes because PR body coverage is off.
    assert!(
        gate(&rules, &pr("Add export endpoint", "")).is_ok(),
        "with apply_body_rule=false the commit-doc rule must not gate the PR body"
    );

    // The commit body is still gated.
    assert!(gate(&rules, &commit("feat: x\n\ntiny")).is_err());
}

// ════════════════════════════════════════════════════════════════════════════
// Auditable bypass (gate_or_bypass)
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn bypass_with_reason_is_allowed_and_records_suppressed_rules() {
    let config = ProcessRuleConfig::default(); // conventional + commit-doc on
    let rules = build_rules(&config);

    // A commit that violates conventional-commit AND commit-doc.
    let action = commit("garbage subject with no story id and a bad shape");
    let req = BypassRequest {
        reason: "machine-generated merge commit from the rebase pipeline".to_string(),
    };

    let result =
        gate_or_bypass(&rules, &action, Some(&req)).expect("bypass WITH reason must be allowed");
    match result {
        GateOrBypassResult::Bypassed(record) => {
            assert_eq!(record.reason, req.reason, "the override reason is recorded");
            assert!(
                record
                    .suppressed_rule_ids
                    .contains(&"PROCESS-CONVENTIONAL-COMMIT-1".to_string()),
                "suppressed rules recorded: {:?}",
                record.suppressed_rule_ids
            );
            assert!(
                record
                    .suppressed_rule_ids
                    .contains(&"PROCESS-COMMIT-DOC-1".to_string()),
                "suppressed rules recorded: {:?}",
                record.suppressed_rule_ids
            );
        }
        other => panic!("expected Bypassed, got {other:?}"),
    }
}

#[test]
fn bypass_without_reason_is_itself_a_gate_violation() {
    let rules = build_rules(&ProcessRuleConfig::default());
    let action = commit("garbage subject, no body, no id");

    // Empty reason.
    let empty = BypassRequest {
        reason: String::new(),
    };
    assert!(
        gate_or_bypass(&rules, &action, Some(&empty)).is_err(),
        "a reason-less bypass must be rejected (suppression-waiver invariant)"
    );

    // Whitespace-only reason is treated identically.
    let ws = BypassRequest {
        reason: "   \t  ".to_string(),
    };
    assert!(
        gate_or_bypass(&rules, &action, Some(&ws)).is_err(),
        "a whitespace-only reason must also be rejected"
    );
}

#[test]
fn bypass_none_behaves_like_plain_gate() {
    let rules = build_rules(&ProcessRuleConfig::default());

    // Failing action with no bypass -> Failed(violations).
    let bad = commit("garbage, no body, no id");
    match gate_or_bypass(&rules, &bad, None).expect("no bypass request, no reason error") {
        GateOrBypassResult::Failed(v) => assert!(!v.is_empty()),
        other => panic!("expected Failed, got {other:?}"),
    }

    // Passing action with a (harmless) bypass reason -> Passed, never Bypassed.
    let good = commit("feat: add export\n\nImplements the export flow fully. Refs #7.");
    let req = BypassRequest {
        reason: "not actually needed".to_string(),
    };
    assert_eq!(
        gate_or_bypass(&rules, &good, Some(&req)).expect("valid reason"),
        GateOrBypassResult::Passed,
        "a passing action returns Passed even with a bypass request"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Full-stack: multiple rules accumulate, clean action passes all
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn fully_enabled_config_clean_commit_passes_dirty_commit_accumulates() {
    let config = ProcessRuleConfig {
        commit_doc: CommitDocConfig {
            enabled: true,
            min_body_chars: 20,
            require_story_id: true,
            id_location: IdLocation::Body,
            story_id_format: StoryIdFormat::default(),
        },
        conventional_commit: ConventionalCommitConfig {
            enabled: true,
            ..Default::default()
        },
        branch_naming: BranchNamingConfig {
            enabled: false,
            ..Default::default()
        },
        ado_link: AdoLinkConfig {
            enabled: true,
            prefix: "AB".to_string(),
        },
        pr: PrCoverage::default(),
    };
    let rules = build_rules(&config);

    // Clean commit: conventional shape + AB# ref in subject + substantive body
    // with bare #<num> story id.
    let clean = commit(
        "feat: AB#1234 add export\n\nImplements the CSV export pipeline end to end. Refs #1234.",
    );
    assert!(
        gate(&rules, &clean).is_ok(),
        "a fully-conforming commit must pass every enabled rule"
    );

    // Dirty commit: wrong shape, no AB# ref, thin body, no story id.
    let dirty = commit("did some stuff");
    let err = gate(&rules, &dirty).expect_err("must accumulate violations");
    let ids: Vec<&str> = err.iter().map(|v| v.rule_id.as_str()).collect();
    assert!(ids.contains(&"PROCESS-CONVENTIONAL-COMMIT-1"), "{ids:?}");
    assert!(ids.contains(&"PROCESS-ADO-LINK-1"), "{ids:?}");
    assert!(ids.contains(&"PROCESS-COMMIT-DOC-1"), "{ids:?}");
}
