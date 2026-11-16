//! A style linter's rule is one decision per repository, never one decision per place.
//!
//! An outside style linter (ESLint, Clippy, Ruff) can flag one rule at thousands of places
//! (`no-var`, `eqeqeq`). Whether that rule applies to the client's code is one call: "fix it
//! everywhere" or "rule it out everywhere". So every place a style linter flags under one rule in
//! one repository is ONE rule group ([`lint_sub_id`]), and that group is one Fix of its own (it
//! never folds into another Fix and nothing folds into it). A decision on it is written to every
//! place it holds, as any group decision is. Two lint rules under one native rule node are two groups: the group's id digests the repository and the rule.
//!
//! Nothing hides. Every place stays a row, in All problems and in `findings.json`, counted in the
//! totals. A place of a lint group never takes an urgent (escalation) slot: the rule is decided
//! as a batch. A problem of another rule at the same place is a row of its own group and keeps its
//! own decision.
//!
//! A linter's security checks keep the per-place handling every other problem has
//! ([`is_security_family_lint`]): an `eslint-plugin-security` rule (`security/...`), a Ruff `S`
//! rule (flake8-bandit), and any lint the tool tables or the corpus map to a security rule.
//!
//! The test fails closed: a lint is grouped only when the tool tables or the corpus map it to a
//! rule that is not a security rule, or when it is on the short list of known style lints
//! ([`is_known_style_lint`]), and the row's own class and category are not security. Any other
//! lint (`no-eval`, `react/no-danger`, a Clippy lint nobody mapped) stays per place.

use crate::onboard::Finding;

use super::mapping::{is_external_tool_row, ToolMap};

/// The tools whose findings are a strictness preference about style, not a judgement about harm
/// (W24). Semgrep is not one: it is a security scanner.
pub const STYLE_LINTERS: &[&str] = &["eslint", "clippy", "ruff"];

/// The last path segment of a lint rule group's id starts with this
/// (`sub:<rule node>/lint-<digest>`).
pub const LINT_SEGMENT_PREFIX: &str = "lint-";

/// How many hex digits of the (repository, rule) digest name a lint group.
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

/// ESLint core and TypeScript rules about style or code shape, with no security reading. A rule
/// the ESLint table maps to a corpus rule needs no entry here.
const KNOWN_ESLINT_STYLE: &[&str] = &[
    "camelcase",
    "comma-dangle",
    "curly",
    "eol-last",
    "indent",
    "no-console",
    "no-else-return",
    "no-multiple-empty-lines",
    "no-trailing-spaces",
    "no-unused-vars",
    "object-shorthand",
    "prefer-arrow-callback",
    "prefer-template",
    "quotes",
    "semi",
    "@typescript-eslint/no-unused-vars",
];

/// Clippy lints of its `style` and `complexity` groups (without the `clippy::` prefix).
const KNOWN_CLIPPY_STYLE: &[&str] = &[
    "bool_comparison",
    "collapsible_else_if",
    "collapsible_if",
    "len_zero",
    "let_and_return",
    "manual_range_contains",
    "needless_borrow",
    "needless_return",
    "new_without_default",
    "redundant_closure",
    "redundant_field_names",
    "redundant_static_lifetimes",
    "single_match",
    "unused_unit",
];

/// Ruff rule families about layout, naming, imports, docstrings and quoting: pycodestyle `E` (not
/// `E9`, which is a syntax or runtime error) and `W`, isort `I`, pep8-naming `N`, pydocstyle `D`,
/// flake8-quotes `Q`, pyupgrade `UP` and flake8-commas `COM`.
const KNOWN_RUFF_STYLE_FAMILIES: &[&str] = &["E", "W", "I", "N", "D", "Q", "UP", "COM"];

/// Whether `rule_id` of `tool` is on the short list of known style lints: grouped even when no
/// table maps it.
pub fn is_known_style_lint(tool: &str, rule_id: &str) -> bool {
    let id = rule_id.trim();
    if tool.eq_ignore_ascii_case("eslint") {
        return KNOWN_ESLINT_STYLE.contains(&id);
    }
    if tool.eq_ignore_ascii_case("clippy") {
        let bare = id.strip_prefix("clippy::").unwrap_or(id);
        return KNOWN_CLIPPY_STYLE.contains(&bare);
    }
    if tool.eq_ignore_ascii_case("ruff") {
        let split = id.find(|c: char| c.is_ascii_digit()).unwrap_or(id.len());
        let (family, digits) = id.split_at(split);
        return !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && KNOWN_RUFF_STYLE_FAMILIES.contains(&family)
            && !(family == "E" && digits.starts_with('9'));
    }
    false
}

/// Whether the row itself reads as a security problem: its own class (rule id and category), or
/// the category its rule id alone gives it.
fn row_is_security(f: &Finding) -> bool {
    let by_id = Finding {
        rule_id: f.rule_id.clone(),
        category: crate::ai_audit::categorize_rule_id(&f.rule_id),
        ..Finding::default()
    };
    crate::ai_audit::finding_class(f) == crate::ai_audit::FindingClass::Security
        || crate::ai_audit::finding_class(&by_id) == crate::ai_audit::FindingClass::Security
}

/// The native rule a stored rule node names for a row of `f`, when it is not the row's own tool
/// node (`rule:<tool>/<id>`): the mapping the scan made, with the corpus loaded.
fn stored_native<'a>(f: &Finding, rule_node: &'a str) -> Option<&'a str> {
    let bare = rule_node.strip_prefix("rule:").unwrap_or(rule_node);
    let own = format!(
        "{}/{}",
        f.preview_tool.as_deref().unwrap_or_default(),
        f.rule_id
    );
    (bare != own).then_some(bare)
}

/// Whether `f` is a style linter's problem that is grouped one rule per repository (see the
/// module comment), placed fresh: no stored rule node is read.
pub fn is_style_lint_row(f: &Finding, tools: &ToolMap) -> bool {
    is_style_lint_row_at(f, tools, None)
}

/// [`is_style_lint_row`] for a row whose stored rule node is `stored_rule`: a mapping the scan
/// recorded there counts as the tables' own, so a tree assembled without the corpus groups the
/// same rows the scan grouped. Fails closed: a lint nothing maps and no list knows stays per
/// place.
pub(super) fn is_style_lint_row_at(
    f: &Finding,
    tools: &ToolMap,
    stored_rule: Option<&str>,
) -> bool {
    let Some(tool) = f.preview_tool.as_deref() else {
        return false;
    };
    if !is_external_tool_row(f)
        || !is_style_linter(tool)
        || is_security_family_lint(tool, &f.rule_id, tools)
        || row_is_security(f)
        || stored_rule.is_some_and(|r| rule_node_is_security(f, r))
    {
        return false;
    }
    // Every security mapping is ruled out above, so any mapping left names a rule that is not one.
    let mapped = stored_rule.and_then(|r| stored_native(f, r)).is_some()
        || tools.native_for(&f.rule_id).is_some();
    mapped || is_known_style_lint(tool, &f.rule_id)
}

/// Whether a stored rule node a row of `f` sits under is a native security rule the lint was
/// mapped to when the scan ran (with the corpus loaded): such a row keeps per-place handling even
/// where the tables at hand no longer know the mapping.
pub(super) fn rule_node_is_security(f: &Finding, rule_node: &str) -> bool {
    stored_native(f, rule_node).is_some_and(is_security_rule_id)
}

/// The id of the lint rule group of `rule_id` in `repo` under `rule_node_id`:
/// `sub:<rule node>/lint-<digest of the repository and the rule id>`. Two lint rules under one
/// native rule node are two groups. The same rule and repository give the same id on every run.
pub fn lint_sub_id(rule_node_id: &str, repo: &str, rule_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(format!("{repo}\n{rule_id}").as_bytes())
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
