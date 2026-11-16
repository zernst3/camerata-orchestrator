//! A style linter's rule is one decision per repository, never one decision per place.
//!
//! An outside style linter (ESLint, Clippy, Ruff) can flag one rule at thousands of places
//! (`no-var`, `eqeqeq`). Whether that rule applies to the client's code is one call: "fix it
//! everywhere" or "rule it out everywhere". So every place a style linter flags under one rule in
//! one repository is ONE rule group ([`lint_sub_id`]), and that group is one Fix of its own (it
//! never folds into another Fix and nothing folds into it). A decision on it is written to every
//! place it holds, as any group decision is.
//!
//! Nothing hides. Every place stays a row, in All problems and in `findings.json`, counted in the
//! totals. A place of a lint group never takes an urgent (escalation) slot: the rule is decided
//! as a batch. A problem of another rule at the same place is a row of its own group and keeps its
//! own decision.
//!
//! A linter's security checks keep the per-place handling every other problem has
//! ([`is_security_family_lint`]): an `eslint-plugin-security` rule (`security/...`), a Ruff `S`
//! rule (flake8-bandit), and any lint the tool tables or the corpus map to a security rule.

use crate::onboard::Finding;

use super::mapping::{is_external_tool_row, ToolMap};

/// The tools whose findings are a strictness preference about style, not a judgement about harm
/// (W24). Semgrep is not one: it is a security scanner.
pub const STYLE_LINTERS: &[&str] = &["eslint", "clippy", "ruff"];

/// The last path segment of a lint rule group's id starts with this (`sub:<rule>/lint-<repo>`).
pub const LINT_SEGMENT_PREFIX: &str = "lint-";

/// How many hex digits of the repository's digest name its lint group.
const REPO_DIGITS: usize = 12;

/// Whether `tool` (a row's `preview_tool`) is a style linter.
pub fn is_style_linter(tool: &str) -> bool {
    STYLE_LINTERS.iter().any(|s| tool.eq_ignore_ascii_case(s))
}

/// Whether a native rule id names a security rule (the same class test every row gets).
fn is_security_rule_id(rule_id: &str) -> bool {
    let probe = Finding {
        rule_id: rule_id.to_string(),
        category: crate::ai_audit::categorize_rule_id(rule_id),
        ..Finding::default()
    };
    crate::ai_audit::finding_class(&probe) == crate::ai_audit::FindingClass::Security
}

/// Whether the id alone says `rule_id`, a rule of the style linter `tool`, is a security check: an
/// `eslint-plugin-security` id (`security/...`, or any plugin whose name says security) or a Ruff
/// `S` rule (flake8-bandit, `S` and three digits).
pub fn is_security_lint_id(tool: &str, rule_id: &str) -> bool {
    let id = rule_id.to_ascii_lowercase();
    let last = id.rsplit(['/', ':']).next().unwrap_or(&id);
    let is_ruff_s = tool.eq_ignore_ascii_case("ruff")
        && last.len() == 4
        && last.starts_with('s')
        && last[1..].bytes().all(|b| b.is_ascii_digit());
    let is_security_plugin = id
        .split('/')
        .rev()
        .skip(1)
        .any(|segment| segment.contains("security"));
    is_ruff_s || is_security_plugin
}

/// Whether `rule_id`, a rule of the style linter `tool`, is one of the linter's security checks:
/// one its id names ([`is_security_lint_id`]), a lint the tool tables give a security category,
/// or a lint the tool tables or the corpus map to a security rule.
pub fn is_security_family_lint(tool: &str, rule_id: &str, tools: &ToolMap) -> bool {
    is_security_lint_id(tool, rule_id)
        || crate::finding_security_category(rule_id).is_some()
        || tools
            .native_for(rule_id)
            .is_some_and(|native| is_security_rule_id(&native))
}

/// Whether `f` is a style linter's problem that is grouped one rule per repository: a row of an
/// outside style linter whose rule is not a security check.
pub fn is_style_lint_row(f: &Finding, tools: &ToolMap) -> bool {
    let Some(tool) = f.preview_tool.as_deref() else {
        return false;
    };
    is_external_tool_row(f)
        && is_style_linter(tool)
        && !is_security_family_lint(tool, &f.rule_id, tools)
}

/// Whether a stored rule node a row of `f` sits under is a native security rule the lint was
/// mapped to when the scan ran (with the corpus loaded): such a row keeps per-place handling even
/// where the tables at hand no longer know the mapping.
pub(super) fn rule_node_is_security(f: &Finding, rule_node: &str) -> bool {
    let bare = rule_node.strip_prefix("rule:").unwrap_or(rule_node);
    let own = format!(
        "{}/{}",
        f.preview_tool.as_deref().unwrap_or_default(),
        f.rule_id
    );
    bare != own && is_security_rule_id(bare)
}

/// The id of the lint rule group of `repo` under `rule_node_id`: `sub:<rule>/lint-<digest>`. The
/// same rule and repository give the same id on every run.
pub fn lint_sub_id(rule_node_id: &str, repo: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(repo.as_bytes())
        .iter()
        .take(REPO_DIGITS / 2)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "sub:{}/{LINT_SEGMENT_PREFIX}{digest}",
        rule_node_id.strip_prefix("rule:").unwrap_or(rule_node_id)
    )
}

/// Whether `segment`, the last path segment of a sub-group id, names a lint rule group.
pub(super) fn is_lint_segment(segment: &str) -> bool {
    segment
        .strip_prefix(LINT_SEGMENT_PREFIX)
        .is_some_and(|d| d.len() == REPO_DIGITS && d.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether `id` is a lint rule group's id.
pub fn is_lint_sub_id(id: &str) -> bool {
    id.starts_with("sub:") && id.rsplit('/').next().is_some_and(is_lint_segment)
}

/// Whether `f` sits in a lint rule group (its place, as the tree wrote it).
pub fn in_lint_group(f: &Finding) -> bool {
    f.group_path.get(2).is_some_and(|id| is_lint_sub_id(id))
}

/// The label of a lint rule group: the rule and the repository.
pub(super) fn lint_label(rule_id: &str, repo: &str) -> String {
    format!("Every place of {rule_id} in {repo}")
}

/// The cause of every lint rule group.
pub(super) const LINT_CAUSE: &str = "one style rule, decided once for every place";

/// The summary of a lint rule group: a fixed sentence naming how many places it holds, so it
/// always agrees with the rows.
pub(super) fn lint_summary(places: usize) -> String {
    if places == 1 {
        "1 place of this style rule. A decision on this group applies to it.".to_string()
    } else {
        format!(
            "{places} places of this style rule. A decision on this group applies to every place."
        )
    }
}
