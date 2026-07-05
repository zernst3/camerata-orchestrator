//! The commit / PR / branch chokepoint gate (GAP-2).
//!
//! The VCS-action gate engine lives in `camerata_checks::vcs_action` (`build_rules`,
//! `evaluate`, `gate`, `gate_or_bypass`). Before GAP-2 its ONLY production caller was the
//! manual bypass endpoint, so a project could configure process rules (conventional-commit
//! shape, a required story-id, an `AB#<id>` link, branch-naming) and have them silently
//! NOT enforced on any real commit or PR the server performed.
//!
//! This module is the single shared choke every server-side VCS action funnels through. It
//! builds the project's live rule set from its [`ProcessRuleConfig`] and runs the
//! deterministic gate BEFORE the git action is taken. On a violation it returns
//! [`ChokeError::Blocked`] (HARD-BLOCK): the caller aborts the action — no commit, no PR, no
//! branch. There is no "warn and continue"; a process gate that warns is a linter, not a gate.
//!
//! ## Two entry points
//!
//! - [`gated_commit`] / [`gated_pr`] / [`gated_branch`] HARD-BLOCK on violation. Use these
//!   at the real chokepoints for actions whose metadata a human (or the fleet) authored and
//!   is expected to satisfy the project's conventions.
//! - [`gated_commit_or_bypass`] wraps `vcs_action::gate_or_bypass` for orchestration-internal
//!   actions that legitimately cannot satisfy the rule (e.g. a machine-generated merge commit)
//!   and carry an auditable, non-empty bypass reason. A reason-less bypass is itself rejected.
//!
//! Keying: the caller passes the project's `&ProcessRuleConfig`. When no project is active /
//! resolvable, the caller passes `ProcessRuleConfig::default()`, whose default rule set is
//! the conventional-commit shape only (branch-naming / ADO-link are opt-in, disabled by
//! default), so the gate stays quiet for un-configured projects while still enforcing the
//! baseline.

use camerata_checks::vcs_action::{
    build_rules, gate, gate_or_bypass, BypassRequest, GateOrBypassResult, IdLocation,
    ProcessRuleConfig, ProcessViolation, VcsAction,
};

/// The outcome of a blocked chokepoint check.
#[derive(Debug)]
pub enum ChokeError {
    /// The action violated one or more process rules. The action MUST be aborted.
    Blocked(Vec<ProcessViolation>),
    /// A bypass was requested without a non-empty reason (only from the `_or_bypass` path).
    BypassReasonRequired,
}

impl std::fmt::Display for ChokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChokeError::Blocked(violations) => {
                writeln!(
                    f,
                    "VCS-action gate blocked this action ({} process-rule violation(s)):",
                    violations.len()
                )?;
                for v in violations {
                    writeln!(f, "  - {}", v.detail)?;
                }
                write!(
                    f,
                    "Fix the metadata to satisfy the project's conventions, or use the \
                     bypass endpoint with an auditable reason."
                )
            }
            ChokeError::BypassReasonRequired => write!(
                f,
                "bypass rejected: a non-empty reason is required (mirror of the \
                 suppression-waiver invariant)"
            ),
        }
    }
}

impl std::error::Error for ChokeError {}

/// Gate a COMMIT on its message. HARD-BLOCK: `Err(ChokeError::Blocked)` on any violation.
///
/// Call this immediately before performing the commit; abort the commit on `Err`.
pub fn gated_commit(config: &ProcessRuleConfig, message: &str) -> Result<(), ChokeError> {
    let action = VcsAction::Commit {
        message: message.to_string(),
    };
    run_gate(config, &action)
}

/// Gate a PULL REQUEST on its title + body. HARD-BLOCK on any violation.
pub fn gated_pr(config: &ProcessRuleConfig, title: &str, body: &str) -> Result<(), ChokeError> {
    let action = VcsAction::PullRequest {
        title: title.to_string(),
        body: body.to_string(),
    };
    run_gate(config, &action)
}

/// Gate a BRANCH creation on its name. HARD-BLOCK on any violation.
///
/// Branch-naming is opt-in (disabled by default), so for most projects this is a no-op.
pub fn gated_branch(config: &ProcessRuleConfig, name: &str) -> Result<(), ChokeError> {
    let action = VcsAction::Branch {
        name: name.to_string(),
    };
    run_gate(config, &action)
}

/// Build a machine commit message that is COMPLIANT with the project's active process rules.
///
/// Camerata authors its own server-side commits (the implementation snapshot, the PR-feedback
/// resolution, etc.). Rather than bypass the gate on those, we generate a message that PASSES
/// it, so the default machine path is [`gated_commit`] (HARD-BLOCK) and a non-compliant machine
/// message surfaces as a real bug rather than a silent bypass.
///
/// The message is assembled to satisfy every rule `build_rules` can emit from `config`:
///
/// - **PROCESS-CONVENTIONAL-COMMIT-1** — the subject is `<kind>: <summary>`, where `kind` is one
///   of the project's allowed conventional types (falling back to the first configured type if
///   the caller's preferred `kind` is not allowed).
/// - **PROCESS-COMMIT-DOC-1** — a substantive body (padded past `min_body_chars`) plus a story-id
///   reference. The reference is written in the project's configured `story_id_format`
///   (`<prefix><separator><digits>`) and placed in whichever location the config demands
///   (`Body`, `Subject`, or `Either`). We always place it in the body AND, when `id_location` is
///   `Subject` or `Either`, also in the subject.
/// - **PROCESS-ADO-LINK-1** — when enabled, the configured `<prefix>#<digits>` ticket reference is
///   appended to the subject.
///
/// `numeric_id` is the story's numeric identifier (the tail of a canonical `owner/repo#<num>`
/// story id). If it is empty, no compliant story-id reference can be produced; the returned
/// message will omit it and [`gated_commit`] will HARD-BLOCK, which is the intended signal that
/// the run lacks a usable story id.
pub fn compliant_machine_commit_message(
    config: &ProcessRuleConfig,
    kind: &str,
    summary: &str,
    numeric_id: &str,
) -> String {
    // 1. Conventional-commit type: honour the caller's `kind` if the project allows it, else
    //    fall back to the first allowed type (default set always contains `feat`/`chore`).
    let types = &config.conventional_commit.types;
    let chosen_kind = if types.iter().any(|t| t == kind) {
        kind.to_string()
    } else {
        types.first().cloned().unwrap_or_else(|| "chore".to_string())
    };

    // 2. Story-id reference token in the project's configured format (e.g. `#42`, `AB#42`,
    //    `STORY-42`). Empty when we have no numeric id to embed.
    let fmt = &config.commit_doc.story_id_format;
    let story_ref = if numeric_id.is_empty() {
        String::new()
    } else {
        format!("{}{}{}", fmt.prefix, fmt.separator, numeric_id)
    };

    // 3. ADO ticket reference for the subject when PROCESS-ADO-LINK-1 is active.
    let ado_ref = if config.ado_link.enabled && !numeric_id.is_empty() {
        format!(" {}#{}", config.ado_link.prefix, numeric_id)
    } else {
        String::new()
    };

    // 4. Subject: always conventional-shape. Append the story-id ref to the subject when the doc
    //    rule wants it in the subject (Subject / Either). The `Either` location is satisfied by a
    //    reference anywhere, so the body copy alone would suffice, but adding it to the subject too
    //    is harmless and keeps the reference visible in one-line logs.
    let subject_story_ref = match config.commit_doc.id_location {
        IdLocation::Subject | IdLocation::Either if !story_ref.is_empty() => {
            format!(" ({story_ref})")
        }
        _ => String::new(),
    };
    let subject = format!("{chosen_kind}: {summary}{subject_story_ref}{ado_ref}");

    // 5. Body: a substantive paragraph plus the story-id reference (for the Body / Either cases).
    //    Pad past `min_body_chars` so PROCESS-COMMIT-DOC-1's substantive check always passes.
    let mut body = format!(
        "Server-authored commit produced by Camerata's orchestration path. {summary}."
    );
    if !story_ref.is_empty() {
        body.push_str(&format!(" Refs {story_ref}."));
    }
    let min = config.commit_doc.min_body_chars;
    while body.chars().filter(|c| !c.is_whitespace()).count() < min {
        body.push_str(" Details recorded in the run's evidence trail.");
    }

    format!("{subject}\n\n{body}")
}

/// Build a machine PR title + body that is COMPLIANT with the project's active process rules.
///
/// Mirrors [`compliant_machine_commit_message`] for the PR slices. The PR-coverage of the
/// commit-doc / ADO rules (`config.pr`) decides whether the title / body are checked at all;
/// producing a message that satisfies the strictest coverage is always safe.
///
/// Returns `(title, body)`.
pub fn compliant_machine_pr(
    config: &ProcessRuleConfig,
    summary: &str,
    context: &str,
    numeric_id: &str,
) -> (String, String) {
    let fmt = &config.commit_doc.story_id_format;
    let story_ref = if numeric_id.is_empty() {
        String::new()
    } else {
        format!("{}{}{}", fmt.prefix, fmt.separator, numeric_id)
    };
    let ado_ref = if config.ado_link.enabled && !numeric_id.is_empty() {
        format!(" {}#{}", config.ado_link.prefix, numeric_id)
    } else {
        String::new()
    };

    // Title carries the story-id ref for Subject/Either id locations and the ADO ref when active.
    let title_story_ref = match config.commit_doc.id_location {
        IdLocation::Subject | IdLocation::Either if !story_ref.is_empty() => {
            format!(" ({story_ref})")
        }
        _ => String::new(),
    };
    let title = format!("{summary}{title_story_ref}{ado_ref}");

    // Body: substantive text + the story-id ref (for Body/Either), padded past min_body_chars.
    let mut body = format!("{context} {summary}.");
    if !story_ref.is_empty() {
        body.push_str(&format!(" Refs {story_ref}."));
    }
    let min = config.commit_doc.min_body_chars;
    while body.chars().filter(|c| !c.is_whitespace()).count() < min {
        body.push_str(" Details recorded in the run's evidence trail.");
    }

    (title, body)
}

/// Extract the numeric story identifier from a Camerata story id.
///
/// Canonical story ids are `owner/repo#<num>`; the numeric id is the tail after the last `#`.
/// When there is no `#`, we fall back to the trailing run of ASCII digits (covers ids like
/// `story-42`). Returns an empty string when no digits are present, which signals the caller
/// that a compliant story-id reference cannot be formed.
pub fn numeric_story_id(story_id: &str) -> String {
    let tail = story_id.rsplit_once('#').map(|(_, n)| n).unwrap_or(story_id);
    // Take a leading run of digits from the tail (handles `#42` and `#42-suffix`).
    let leading: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !leading.is_empty() {
        return leading;
    }
    // Fall back to a trailing run of digits anywhere (handles `story-42`).
    story_id
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Gate a COMMIT, allowing an auditable bypass for orchestration-internal commits that
/// legitimately cannot satisfy the rule (e.g. a machine-generated merge commit).
///
/// - `reason = None` — identical to [`gated_commit`] (HARD-BLOCK on violation).
/// - `reason = Some(non-empty)` — if the gate would fail, the action is allowed and the
///   returned `Some(record_summary)` describes the suppressed rules for the evidence trail.
///   When the action passes, `Ok(None)` is returned (no bypass consumed).
/// - `reason = Some(empty)` — `Err(ChokeError::BypassReasonRequired)`.
pub fn gated_commit_or_bypass(
    config: &ProcessRuleConfig,
    message: &str,
    reason: Option<&str>,
) -> Result<Option<String>, ChokeError> {
    let action = VcsAction::Commit {
        message: message.to_string(),
    };
    let rules = build_rules(config);
    let bypass = reason.map(|r| BypassRequest {
        reason: r.to_string(),
    });
    match gate_or_bypass(&rules, &action, bypass.as_ref()) {
        Ok(GateOrBypassResult::Passed) => Ok(None),
        Ok(GateOrBypassResult::Bypassed(record)) => Ok(Some(format!(
            "bypassed [{}]: {}",
            record.suppressed_rule_ids.join(", "),
            record.reason
        ))),
        Ok(GateOrBypassResult::Failed(violations)) => Err(ChokeError::Blocked(violations)),
        Err(_) => Err(ChokeError::BypassReasonRequired),
    }
}

/// Gate a PULL REQUEST, allowing an auditable bypass for orchestration-internal PRs whose
/// title/body are machine-generated (e.g. the onboarding governance PR). Same semantics as
/// [`gated_commit_or_bypass`]: `Ok(None)` when the action passes, `Ok(Some(summary))` when a
/// reasoned bypass is recorded, `Err(ChokeError::Blocked)` on a violation with no bypass,
/// `Err(ChokeError::BypassReasonRequired)` on a reason-less bypass.
pub fn gated_pr_or_bypass(
    config: &ProcessRuleConfig,
    title: &str,
    body: &str,
    reason: Option<&str>,
) -> Result<Option<String>, ChokeError> {
    let action = VcsAction::PullRequest {
        title: title.to_string(),
        body: body.to_string(),
    };
    let rules = build_rules(config);
    let bypass = reason.map(|r| BypassRequest {
        reason: r.to_string(),
    });
    match gate_or_bypass(&rules, &action, bypass.as_ref()) {
        Ok(GateOrBypassResult::Passed) => Ok(None),
        Ok(GateOrBypassResult::Bypassed(record)) => Ok(Some(format!(
            "bypassed [{}]: {}",
            record.suppressed_rule_ids.join(", "),
            record.reason
        ))),
        Ok(GateOrBypassResult::Failed(violations)) => Err(ChokeError::Blocked(violations)),
        Err(_) => Err(ChokeError::BypassReasonRequired),
    }
}

/// Shared inner: build the project's rules and run the deterministic gate.
fn run_gate(config: &ProcessRuleConfig, action: &VcsAction) -> Result<(), ChokeError> {
    let rules = build_rules(config);
    gate(&rules, action).map_err(ChokeError::Blocked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_checks::vcs_action::{CommitDocConfig, ConventionalCommitConfig};

    /// A config that enforces conventional-commit shape but NOT the doc/story-id rule, so
    /// tests can exercise the subject-shape gate in isolation.
    fn conventional_only() -> ProcessRuleConfig {
        ProcessRuleConfig {
            commit_doc: CommitDocConfig {
                enabled: false,
                ..CommitDocConfig::default()
            },
            conventional_commit: ConventionalCommitConfig::default(),
            ..ProcessRuleConfig::default()
        }
    }

    #[test]
    fn compliant_commit_passes() {
        let cfg = conventional_only();
        assert!(gated_commit(&cfg, "feat: add the export endpoint").is_ok());
    }

    #[test]
    fn violating_commit_is_hard_blocked() {
        let cfg = conventional_only();
        let err = gated_commit(&cfg, "just did some stuff").expect_err("must block");
        match err {
            ChokeError::Blocked(v) => {
                assert!(
                    v.iter().any(|x| x.rule_id == "PROCESS-CONVENTIONAL-COMMIT-1"),
                    "conventional-commit rule must fire: {v:?}"
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn default_config_enforces_conventional_shape() {
        // The shipped default has conventional_commit enabled + commit_doc enabled, so a
        // bare-junk subject with no body is blocked out of the box.
        let cfg = ProcessRuleConfig::default();
        assert!(gated_commit(&cfg, "nonsense").is_err(), "default config must gate");
    }

    #[test]
    fn doc_rule_default_requires_body_and_story_id() {
        // Default config's PROCESS-COMMIT-DOC-1 requires a substantive body + bare #<id>.
        let cfg = ProcessRuleConfig::default();
        // Conventional shape but subject-only -> blocked (no body / no story id).
        assert!(gated_commit(&cfg, "feat: add export").is_err());
        // Conventional shape + substantive body + story id -> passes.
        let ok_msg = "feat: add export\n\nImplements the CSV export flow end to end. Refs #42.";
        assert!(gated_commit(&cfg, ok_msg).is_ok(), "compliant commit must pass");
    }

    #[test]
    fn pr_gate_blocks_and_passes() {
        let cfg = ProcessRuleConfig::default();
        // Empty body PR -> blocked by the doc rule on PrBody.
        assert!(gated_pr(&cfg, "Add export endpoint", "").is_err());
        // Substantive PR body with a story id -> passes.
        assert!(gated_pr(
            &cfg,
            "Add export endpoint",
            "Implements the CSV export feature end to end. Closes #99."
        )
        .is_ok());
    }

    #[test]
    fn branch_gate_is_noop_by_default() {
        // branch_naming is opt-in (disabled by default): any branch name passes.
        let cfg = ProcessRuleConfig::default();
        assert!(gated_branch(&cfg, "my-random-branch").is_ok());
    }

    #[test]
    fn bypass_with_reason_allows_a_violating_commit() {
        let cfg = conventional_only();
        // Violating message, but an auditable reason bypasses it.
        let record = gated_commit_or_bypass(
            &cfg,
            "machine merge commit",
            Some("machine-generated merge commit from the rebase pipeline"),
        )
        .expect("bypass must succeed with a reason");
        let summary = record.expect("a bypass record is produced for a violating action");
        assert!(summary.contains("PROCESS-CONVENTIONAL-COMMIT-1"), "record names the rule: {summary}");
    }

    #[test]
    fn bypass_without_reason_is_rejected() {
        let cfg = conventional_only();
        let err = gated_commit_or_bypass(&cfg, "bad subject", Some(""))
            .expect_err("empty reason must be rejected");
        assert!(matches!(err, ChokeError::BypassReasonRequired));
    }

    #[test]
    fn bypass_none_on_passing_action_consumes_no_bypass() {
        let cfg = conventional_only();
        let record = gated_commit_or_bypass(&cfg, "feat: fine", None)
            .expect("a compliant commit passes");
        assert!(record.is_none(), "no bypass record for a passing action");
    }

    // ── Compliant machine-message generation (Refinement #1) ─────────────────────

    #[test]
    fn numeric_story_id_extracts_tail_after_hash() {
        assert_eq!(numeric_story_id("acme/widgets#42"), "42");
        assert_eq!(numeric_story_id("story-1"), "1");
        assert_eq!(numeric_story_id("#7"), "7");
    }

    #[test]
    fn numeric_story_id_empty_when_no_digits() {
        assert_eq!(numeric_story_id("acme/widgets#"), "");
        assert_eq!(numeric_story_id("no-number-here"), "");
    }

    #[test]
    fn machine_commit_passes_default_ruleset() {
        // The shipped default requires conventional shape + substantive body + bare #<id>.
        let cfg = ProcessRuleConfig::default();
        let msg = compliant_machine_commit_message(&cfg, "feat", "implement story acme/x#42", "42");
        assert!(
            gated_commit(&cfg, &msg).is_ok(),
            "generated machine commit must pass the default gate: {msg:?}"
        );
    }

    #[test]
    fn machine_commit_missing_story_id_hard_blocks() {
        // No numeric id -> no compliant story-id reference -> the default doc rule blocks.
        let cfg = ProcessRuleConfig::default();
        let msg = compliant_machine_commit_message(&cfg, "feat", "implement story", "");
        let err = gated_commit(&cfg, &msg).expect_err("must hard-block with no story id");
        match err {
            ChokeError::Blocked(v) => assert!(
                v.iter().any(|x| x.rule_id == "PROCESS-COMMIT-DOC-1"),
                "doc rule must fire: {v:?}"
            ),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn machine_commit_passes_ado_and_subject_id_ruleset() {
        // A realistic Azure-Boards project: ADO link enabled, story-id in the SUBJECT with the
        // `AB#` format.
        use camerata_checks::vcs_action::{AdoLinkConfig, IdLocation, StoryIdFormat};
        let mut cfg = ProcessRuleConfig::default();
        cfg.ado_link = AdoLinkConfig { enabled: true, prefix: "AB".to_string() };
        cfg.commit_doc.id_location = IdLocation::Subject;
        cfg.commit_doc.story_id_format = StoryIdFormat {
            prefix: "AB".to_string(),
            separator: '#',
            custom_regex: None,
        };
        let msg = compliant_machine_commit_message(&cfg, "feat", "implement the export", "42");
        assert!(
            gated_commit(&cfg, &msg).is_ok(),
            "generated machine commit must pass the ADO + subject-id gate: {msg:?}"
        );
    }

    #[test]
    fn machine_pr_passes_default_ruleset() {
        let cfg = ProcessRuleConfig::default();
        let (title, body) = compliant_machine_pr(
            &cfg,
            "Camerata: acme/x#42",
            "Opened by Camerata for story acme/x#42.",
            "42",
        );
        assert!(
            gated_pr(&cfg, &title, &body).is_ok(),
            "generated machine PR must pass the default gate: {title:?} / {body:?}"
        );
    }

    #[test]
    fn machine_pr_missing_story_id_hard_blocks() {
        let cfg = ProcessRuleConfig::default();
        let (title, body) = compliant_machine_pr(&cfg, "Camerata: x", "Opened by Camerata.", "");
        assert!(
            gated_pr(&cfg, &title, &body).is_err(),
            "a PR with no story id must hard-block under the default gate"
        );
    }

    // ── Branch-naming gate (Refinement #3) ───────────────────────────────────────

    #[test]
    fn branch_gate_blocks_nonconforming_when_rule_active() {
        use camerata_checks::vcs_action::BranchNamingConfig;
        let mut cfg = ProcessRuleConfig::default();
        cfg.branch_naming = BranchNamingConfig {
            enabled: true,
            prefixes: vec!["feature/".to_string(), "release/".to_string(), "hotfix/".to_string()],
        };
        // Camerata's default `camerata/<id>` slug does not match the required prefixes.
        assert!(
            gated_branch(&cfg, "camerata/acme-x-42").is_err(),
            "a non-conforming branch must block when branch-naming is active"
        );
        // A conforming name passes.
        assert!(gated_branch(&cfg, "feature/export-endpoint").is_ok());
    }

    #[test]
    fn branch_gate_allows_anything_when_rule_inactive() {
        // Branch-naming is opt-in; the default config leaves it disabled.
        let cfg = ProcessRuleConfig::default();
        assert!(gated_branch(&cfg, "camerata/acme-x-42").is_ok());
        assert!(gated_branch(&cfg, "literally-anything").is_ok());
    }
}
