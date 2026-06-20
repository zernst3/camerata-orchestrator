//! The VCS-action gate: process rules over commit / PR / branch METADATA.
//!
//! This is the fourth enforcement point (see
//! `docs/decisions/2026-06-15_process_rules_and_vcs_action_gate.md`). Layers 1/2
//! and the integration tier all enforce on CODE (file content, diffs, the
//! assembled tree). A process rule like "the PR title and the first line of the
//! commit must contain `AB#{ticketId}`" (a real ADO-linking convention) is about
//! VCS METADATA, which no code gate ever sees.
//!
//! Camerata's own orchestration code is the SOLE committer and PR-opener: the
//! agent has no `git` (Bash is denied at the cage). So there is exactly one
//! chokepoint for every commit and PR, and this gate runs there — validating the
//! action's metadata before Camerata performs it, and refusing on a miss. That
//! is why the gate is complete by construction: there is no second path.
//!
//! Everything here is deterministic and pure (matchers over strings, no LLM
//! judgement, no network), so the verdict is binary and reproducible — the same
//! hard line the other gates hold. Matchers are hand-rolled (no regex dependency)
//! and cover the templates the ADR calls out: an `AB#{id}` ticket reference,
//! conventional-commit shape, and branch naming.
//!
//! ## Configurable rules (`ProcessRuleConfig`)
//!
//! [`ProcessRuleConfig`] is the per-project, serde-serializable knob panel. It
//! enables or disables each rule and exposes tunables (minimum body length, story-id
//! format, allowed commit types, branch prefixes). The shipped defaults match the
//! previous hardcoded behaviour, so existing projects that do not set an explicit
//! config see no change.
//!
//! Build the live [`ProcessRule`] set from a config with [`build_rules`].
//!
//! ## Auditable bypass
//!
//! Sometimes a legitimately unusual action (e.g. a machine-generated merge commit,
//! a one-time onboarding branch) cannot satisfy the rule without distorting the
//! commit history. [`BypassRequest`] lets a caller supply a non-empty reason that
//! is recorded in the returned [`BypassRecord`], giving an auditable trail. A
//! bypass without a reason is itself a gate violation (mirrors the suppression-waiver
//! invariant). Use [`gate_or_bypass`] instead of [`gate`] when bypass is needed.

use serde::{Deserialize, Serialize};

// ── The action being gated ─────────────────────────────────────────────────────

/// A version-control action whose metadata Camerata is about to perform. The
/// gate validates the relevant metadata BEFORE the action is taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VcsAction {
    /// A commit, gated on its message.
    Commit {
        /// The full commit message (subject + body).
        message: String,
    },
    /// A pull request, gated on its title and body.
    PullRequest {
        /// The PR title.
        title: String,
        /// The PR body / description.
        body: String,
    },
    /// A branch creation, gated on its name.
    Branch {
        /// The branch name (e.g. `feature/login`).
        name: String,
    },
}

/// Which slice of which action a process rule applies to. A rule that targets a
/// slice absent from the action being gated simply does not apply (no violation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsTarget {
    /// The full commit message.
    CommitMessage,
    /// Only the first line (subject) of the commit message.
    CommitSubject,
    /// Everything after the first line of the commit message (the body).
    ///
    /// For a message with only a subject and no blank line + body, this target
    /// yields an empty string (not `None`) so that body-presence rules fire
    /// rather than silently skip.
    CommitBody,
    /// The pull-request title.
    PrTitle,
    /// The pull-request body.
    PrBody,
    /// The branch name.
    BranchName,
}

impl VcsTarget {
    /// Extract this target's text from `action`, or `None` when the action does
    /// not have this slice (e.g. `PrTitle` against a `Commit`).
    fn extract<'a>(&self, action: &'a VcsAction) -> Option<&'a str> {
        match (self, action) {
            (VcsTarget::CommitMessage, VcsAction::Commit { message }) => Some(message),
            (VcsTarget::CommitSubject, VcsAction::Commit { message }) => {
                Some(message.lines().next().unwrap_or(""))
            }
            (VcsTarget::CommitBody, VcsAction::Commit { message }) => {
                // Body = everything after the first newline.  When there is no
                // newline the body is empty (""), not absent (None), so
                // body-presence rules fire on subject-only commits.
                let body = match message.find('\n') {
                    Some(pos) => &message[pos + 1..],
                    None => "",
                };
                Some(body)
            }
            (VcsTarget::PrTitle, VcsAction::PullRequest { title, .. }) => Some(title),
            (VcsTarget::PrBody, VcsAction::PullRequest { body, .. }) => Some(body),
            (VcsTarget::BranchName, VcsAction::Branch { name }) => Some(name),
            _ => None,
        }
    }

    /// A short human label for violation messages.
    fn label(self) -> &'static str {
        match self {
            VcsTarget::CommitMessage => "commit message",
            VcsTarget::CommitSubject => "commit subject (first line)",
            VcsTarget::CommitBody => "commit body (lines after subject)",
            VcsTarget::PrTitle => "PR title",
            VcsTarget::PrBody => "PR body",
            VcsTarget::BranchName => "branch name",
        }
    }
}

// ── Matchers (deterministic, no regex) ─────────────────────────────────────────

/// A deterministic predicate over a metadata string. Hand-rolled to avoid a
/// regex dependency while covering the ADR's templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// The text must contain `prefix` immediately followed by `#` and at least
    /// one ASCII digit (e.g. prefix `"AB"` matches `AB#1234`). This is the
    /// ticket-reference template.
    TicketRef {
        /// The link prefix, e.g. `"AB"` for Azure Boards.
        prefix: String,
    },
    /// The text must contain `needle` as a literal substring.
    Contains {
        /// The required literal substring.
        needle: String,
    },
    /// The text (trimmed) must start with one of these prefixes (e.g. branch
    /// names starting `feature/` or `release/`).
    StartsWithAny {
        /// The allowed leading prefixes.
        prefixes: Vec<String>,
    },
    /// The text's first token before `:` must be one of these conventional-commit
    /// types, optionally followed by a `(scope)` and/or `!`, then `: `.
    ConventionalCommit {
        /// Allowed commit types (e.g. `feat`, `fix`, `chore`).
        types: Vec<String>,
    },
    /// The text must be substantive (at least `min_non_blank_chars` non-whitespace
    /// characters across at least one non-blank line) AND must contain a story-id
    /// reference matching the given prefix pattern.
    ///
    /// The story-id pattern follows the same `PREFIX#<digits>` convention as
    /// [`Matcher::TicketRef`] when `story_id_prefix` ends without `#` (e.g.
    /// `"#"` for a bare `#42`, `"AB"` for `AB#42`), or matches `PREFIX-<digits>`
    /// when `story_id_separator` is set to `'-'`.  The defaults (prefix `"#"`,
    /// separator `'#'`, no further constraints) accept a bare `#42` reference in
    /// the body.
    ///
    /// This is the compound check for PROCESS-COMMIT-DOC-1.
    SubstantiveWithStoryId {
        /// Minimum number of non-whitespace characters the body must contain.
        min_non_blank_chars: usize,
        /// The prefix before the separator + digits (e.g. `"#"` for `#42`,
        /// `"AB"` for `AB#42`, or `"STORY"` for `STORY-42`).
        story_id_prefix: String,
        /// The character separating the prefix from the digits (`'#'` or `'-'`).
        story_id_separator: char,
    },
    /// The text must have at least `min_non_blank_chars` non-whitespace characters
    /// but no story-id is required. Used when [`CommitDocConfig::require_story_id`]
    /// is `false` so the body-length check still fires.
    Substantive {
        /// Minimum number of non-whitespace characters the text must contain.
        min_non_blank_chars: usize,
    },
}

impl Matcher {
    /// Does `text` satisfy this matcher?
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Matcher::TicketRef { prefix } => contains_ticket_ref(text, prefix),
            Matcher::Contains { needle } => text.contains(needle.as_str()),
            Matcher::StartsWithAny { prefixes } => {
                let t = text.trim_start();
                prefixes.iter().any(|p| t.starts_with(p.as_str()))
            }
            Matcher::ConventionalCommit { types } => is_conventional_commit(text, types),
            Matcher::SubstantiveWithStoryId {
                min_non_blank_chars,
                story_id_prefix,
                story_id_separator,
            } => {
                is_substantive(text, *min_non_blank_chars)
                    && contains_story_id(text, story_id_prefix, *story_id_separator)
            }
            Matcher::Substantive { min_non_blank_chars } => {
                is_substantive(text, *min_non_blank_chars)
            }
        }
    }

    /// A short description of what this matcher requires, for violation messages.
    fn requirement(&self) -> String {
        match self {
            Matcher::TicketRef { prefix } => format!("a `{prefix}#<number>` reference"),
            Matcher::Contains { needle } => format!("the text `{needle}`"),
            Matcher::StartsWithAny { prefixes } => {
                format!("a prefix of [{}]", prefixes.join(", "))
            }
            Matcher::ConventionalCommit { types } => {
                format!("a conventional-commit type of [{}]", types.join(", "))
            }
            Matcher::SubstantiveWithStoryId {
                min_non_blank_chars,
                story_id_prefix,
                story_id_separator,
            } => format!(
                "at least {min_non_blank_chars} non-blank characters and a `{story_id_prefix}{story_id_separator}<number>` story-id reference"
            ),
            Matcher::Substantive { min_non_blank_chars } => {
                format!("at least {min_non_blank_chars} non-blank characters")
            }
        }
    }
}

/// True when `text` contains `prefix` immediately followed by `#` and one or more
/// ASCII digits. Scans every occurrence of `prefix#` so trailing non-digits do
/// not mask a valid reference elsewhere.
fn contains_ticket_ref(text: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    let token = format!("{prefix}#");
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(&token) {
        let after = search_from + rel + token.len();
        if text[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        {
            return true;
        }
        search_from = after; // keep scanning past this `prefix#`
    }
    false
}

/// True when the first line of `text` is a conventional commit: `type` (one of
/// `types`), an optional `(scope)`, an optional `!`, then `:` and a space and a
/// non-empty subject.
fn is_conventional_commit(text: &str, types: &[String]) -> bool {
    let first = text.lines().next().unwrap_or("");
    let Some((head, rest)) = first.split_once(':') else {
        return false;
    };
    // Subject after the colon must be non-empty (allowing the leading space).
    if rest.trim().is_empty() {
        return false;
    }
    // Strip an optional trailing '!' (breaking-change marker).
    let head = head.strip_suffix('!').unwrap_or(head);
    // Strip an optional `(scope)` suffix.
    let type_part = match head.split_once('(') {
        Some((t, scope)) if scope.ends_with(')') => t,
        Some(_) => return false, // unbalanced paren
        None => head,
    };
    types.iter().any(|t| t == type_part)
}

/// True when `text` has at least `min_non_blank_chars` non-whitespace characters
/// across one or more non-blank lines. An empty string or whitespace-only string
/// never satisfies a non-zero minimum.
fn is_substantive(text: &str, min_non_blank_chars: usize) -> bool {
    if min_non_blank_chars == 0 {
        // A zero-minimum is vacuously true.
        return true;
    }
    let count: usize = text.lines().map(|l| l.chars().filter(|c| !c.is_whitespace()).count()).sum();
    count >= min_non_blank_chars
}

/// True when `text` contains `prefix` immediately followed by `separator` and one
/// or more ASCII digits (e.g. prefix `"#"`, separator `'#'` matches `#42`; prefix
/// `"AB"`, separator `'#'` matches `AB#42`; prefix `"STORY"`, separator `'-'`
/// matches `STORY-42`).
///
/// Scans every occurrence of `prefix + separator` so a non-digit occurrence does
/// not mask a valid reference elsewhere.
fn contains_story_id(text: &str, prefix: &str, separator: char) -> bool {
    // Build the token we search for: e.g. "#" + '#' = "##" (for bare #42), or
    // "AB" + '#' = "AB#", or "STORY" + '-' = "STORY-".
    let mut token = prefix.to_owned();
    token.push(separator);

    // Special-case: prefix "#" with separator '#' means we want bare `#42`.
    // The token would be "##" which won't match "#42".  Handle this by treating
    // an empty prefix specially — if prefix starts with the separator char we
    // use just the separator as the token.
    // Actually, a cleaner design: if prefix is empty, token is just the separator.
    // The caller's contract: prefix="" + separator='#' means "a bare #<num>".
    let token = if prefix.is_empty() {
        separator.to_string()
    } else {
        token
    };

    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(&token) {
        let after = search_from + rel + token.len();
        if text[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        {
            return true;
        }
        search_from = after;
    }
    false
}

// ── Process rule + evaluation ──────────────────────────────────────────────────

/// One process rule: a named, deterministic predicate applied to one or more
/// slices of a VCS action. Per-account custom (e.g. a team's `AB#{id}`
/// convention), authored once and enforced firmly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRule {
    /// Stable rule id (the `PROCESS-*` family).
    pub id: String,
    /// Human description of the convention.
    pub description: String,
    /// Which action slices this rule applies to.
    pub applies_to: Vec<VcsTarget>,
    /// The predicate each applicable slice must satisfy.
    pub matcher: Matcher,
}

impl ProcessRule {
    /// The Azure-Boards ticket-link convention: the commit SUBJECT and the PR
    /// TITLE must each contain an `AB#<number>` reference (the real workplace
    /// convention that auto-links commits/PRs to ADO work items).
    pub fn ado_ticket_link() -> Self {
        Self {
            id: "PROCESS-ADO-LINK-1".to_string(),
            description:
                "Commit subject and PR title must contain an `AB#<id>` Azure Boards reference."
                    .to_string(),
            applies_to: vec![VcsTarget::CommitSubject, VcsTarget::PrTitle],
            matcher: Matcher::TicketRef {
                prefix: "AB".to_string(),
            },
        }
    }

    /// Conventional-commit shape on the commit subject, with the common type set.
    pub fn conventional_commits() -> Self {
        Self::conventional_commits_with_types(&[
            "feat", "fix", "chore", "docs", "refactor", "test", "perf", "build", "ci",
            "style", "revert",
        ])
    }

    /// Conventional-commit shape on the commit subject, with a caller-supplied type set.
    ///
    /// Used by [`build_rules`] to honour [`ConventionalCommitConfig::types`].
    pub fn conventional_commits_with_types(types: &[impl AsRef<str>]) -> Self {
        Self {
            id: "PROCESS-CONVENTIONAL-COMMIT-1".to_string(),
            description: "Commit subject must follow conventional-commits (type: subject)."
                .to_string(),
            applies_to: vec![VcsTarget::CommitSubject],
            matcher: Matcher::ConventionalCommit {
                types: types.iter().map(|s| s.as_ref().to_string()).collect(),
            },
        }
    }

    /// Branch-naming convention: the branch must start with one of `prefixes`
    /// (e.g. `["feature/", "release/", "hotfix/"]`).
    pub fn branch_naming(prefixes: &[&str]) -> Self {
        Self {
            id: "PROCESS-BRANCH-NAMING-1".to_string(),
            description: format!(
                "Branch name must start with one of: {}",
                prefixes.join(", ")
            ),
            applies_to: vec![VcsTarget::BranchName],
            matcher: Matcher::StartsWithAny {
                prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    /// Documentation gate (PROCESS-COMMIT-DOC-1): the commit body AND the PR body
    /// must each be substantive (at least `min_non_blank_chars` non-whitespace
    /// characters) AND must contain a story-id reference of the form
    /// `<story_id_prefix><story_id_separator><digits>`.
    ///
    /// # Rationale
    ///
    /// A commit/PR record is the durable in-repo history for every governed
    /// change.  A subject-only commit is too thin: it tells reviewers *what*
    /// was done but not *why*, *which story* motivated it, or *what was
    /// decided*.  This gate ensures a minimum prose body is always present and
    /// is keyed to the governing story so readers can navigate to the full
    /// context.
    ///
    /// # Defaults
    ///
    /// - `min_non_blank_chars = 20` — long enough to rule out a one-word
    ///   placeholder, short enough not to block valid one-liners.
    /// - `story_id_prefix = ""`, `story_id_separator = '#'` — accepts a bare
    ///   `#<num>` GitHub-style story reference.
    ///
    /// Callers can pass custom values to adapt the rule to their tracker
    /// (e.g. `"AB"` + `'#'` for Azure Boards `AB#42`, or `"STORY"` + `'-'`
    /// for a Jira-style `STORY-42`).
    ///
    /// # Applies to
    ///
    /// [`VcsTarget::CommitBody`] for commits and [`VcsTarget::PrBody`] for PRs.
    /// A branch action is not gated by this rule (the target is absent).
    pub fn commit_documentation(
        min_non_blank_chars: usize,
        story_id_prefix: &str,
        story_id_separator: char,
    ) -> Self {
        Self {
            id: "PROCESS-COMMIT-DOC-1".to_string(),
            description: format!(
                "Commit body and PR body must contain at least {min_non_blank_chars} non-blank \
                 characters and a story-id reference \
                 ({story_id_prefix}{story_id_separator}<number>)."
            ),
            applies_to: vec![VcsTarget::CommitBody, VcsTarget::PrBody],
            matcher: Matcher::SubstantiveWithStoryId {
                min_non_blank_chars,
                story_id_prefix: story_id_prefix.to_string(),
                story_id_separator,
            },
        }
    }
}

/// A single rule violation against one action slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessViolation {
    /// The rule that was violated.
    pub rule_id: String,
    /// The slice that failed.
    pub target: VcsTarget,
    /// A human-readable explanation (what was required, on which slice).
    pub detail: String,
}

/// Evaluate every rule against `action`, returning all violations. A rule whose
/// target slice is absent from the action does not apply (no violation).
pub fn evaluate(rules: &[ProcessRule], action: &VcsAction) -> Vec<ProcessViolation> {
    let mut violations = Vec::new();
    for rule in rules {
        for &target in &rule.applies_to {
            let Some(text) = target.extract(action) else {
                continue; // this rule's slice is not part of this action
            };
            if !rule.matcher.matches(text) {
                violations.push(ProcessViolation {
                    rule_id: rule.id.clone(),
                    target,
                    detail: format!(
                        "[{}] {} must contain {} ({})",
                        rule.id,
                        target.label(),
                        rule.matcher.requirement(),
                        rule.description,
                    ),
                });
            }
        }
    }
    violations
}

/// The gate: `Ok(())` when `action` satisfies every rule, else `Err(violations)`.
///
/// Camerata's commit/PR path calls this before performing the action and refuses
/// (does not commit / does not open the PR) on `Err`. Gated firmly — there is no
/// "warn and continue"; a process gate that warns is a linter, not a gate.
pub fn gate(rules: &[ProcessRule], action: &VcsAction) -> Result<(), Vec<ProcessViolation>> {
    let violations = evaluate(rules, action);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

// ── Configurable rule set ─────────────────────────────────────────────────────

/// Where the story-id may appear in the commit/PR body. Controls which sub-field
/// of [`CommitDocConfig`] the [`Matcher::SubstantiveWithStoryId`] inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdLocation {
    /// The story-id must appear in the commit subject (first line) / PR title.
    Subject,
    /// The story-id must appear in the commit body / PR description.
    #[default]
    Body,
    /// The story-id may appear in either the subject/title or the body.
    Either,
}

/// Format of the story-id reference: `<prefix><separator><digits>`.
///
/// # Examples
///
/// | Tracker | prefix | separator | example match |
/// |---------|--------|-----------|---------------|
/// | GitHub  | `""`   | `'#'`     | `#42`         |
/// | Azure Boards | `"AB"` | `'#'` | `AB#123`     |
/// | Jira    | `"PROJ"` | `'-'`  | `PROJ-42`     |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryIdFormat {
    /// The prefix before the separator (empty string = bare reference, e.g. `#42`).
    #[serde(default)]
    pub prefix: String,
    /// The separator character between the prefix and the numeric id (`'#'` or `'-'`).
    #[serde(default = "default_story_id_separator")]
    pub separator: char,
    /// An optional custom regex that overrides `prefix` + `separator` matching when
    /// set. Currently reserved for future use; the gate ignores it (falls through to
    /// the prefix+separator logic). Documented here so the API surface is stable.
    #[serde(default)]
    pub custom_regex: Option<String>,
}

fn default_story_id_separator() -> char {
    '#'
}

impl Default for StoryIdFormat {
    /// Default: bare `#<num>` (GitHub issue reference, `prefix=""`, `separator='#'`).
    fn default() -> Self {
        Self {
            prefix: String::new(),
            separator: '#',
            custom_regex: None,
        }
    }
}

/// Per-rule tunables for `PROCESS-COMMIT-DOC-1`.
///
/// Controls whether a substantive body + story-id reference is required on every
/// commit and PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitDocConfig {
    /// Whether the rule is enforced. `true` by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum number of non-whitespace characters the body must contain. Default: `20`.
    #[serde(default = "default_min_body_chars")]
    pub min_body_chars: usize,
    /// Whether a story-id reference is required in addition to the body length. Default: `true`.
    #[serde(default = "default_true")]
    pub require_story_id: bool,
    /// Where the story-id must appear (body, subject, or either). Default: `body`.
    #[serde(default)]
    pub id_location: IdLocation,
    /// Format of the story-id reference. Default: bare `#<num>`.
    #[serde(default)]
    pub story_id_format: StoryIdFormat,
}

fn default_min_body_chars() -> usize {
    20
}

fn default_true() -> bool {
    true
}

impl Default for CommitDocConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_body_chars: default_min_body_chars(),
            require_story_id: true,
            id_location: IdLocation::default(),
            story_id_format: StoryIdFormat::default(),
        }
    }
}

/// Per-rule tunables for `PROCESS-CONVENTIONAL-COMMIT-1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConventionalCommitConfig {
    /// Whether the rule is enforced. `true` by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Allowed commit types. Defaults to the standard set (`feat`, `fix`, `chore`,
    /// `docs`, `refactor`, `test`, `perf`, `build`, `ci`, `style`, `revert`).
    #[serde(default = "default_conventional_types")]
    pub types: Vec<String>,
}

fn default_conventional_types() -> Vec<String> {
    ["feat", "fix", "chore", "docs", "refactor", "test", "perf", "build", "ci", "style", "revert"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for ConventionalCommitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            types: default_conventional_types(),
        }
    }
}

/// Per-rule tunables for `PROCESS-BRANCH-NAMING-1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchNamingConfig {
    /// Whether the rule is enforced. `false` by default (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// Allowed branch name prefixes. Default: `["feature/", "release/", "hotfix/"]`.
    #[serde(default = "default_branch_prefixes")]
    pub prefixes: Vec<String>,
}

fn default_branch_prefixes() -> Vec<String> {
    ["feature/", "release/", "hotfix/"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for BranchNamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prefixes: default_branch_prefixes(),
        }
    }
}

/// Per-rule tunables for `PROCESS-ADO-LINK-1`.
///
/// This rule requires a ticket reference of the form `<prefix>#<digits>` in the
/// commit subject and/or PR title. Disabled by default because it is an
/// ADO-specific convention; teams that use Azure Boards enable it and configure
/// their `prefix` (typically `"AB"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoLinkConfig {
    /// Whether the rule is enforced. `false` by default (opt-in for ADO users).
    #[serde(default)]
    pub enabled: bool,
    /// The link prefix expected before `#<digits>`. Default: `"AB"` (Azure Boards).
    #[serde(default = "default_ado_prefix")]
    pub prefix: String,
}

fn default_ado_prefix() -> String {
    "AB".to_string()
}

impl Default for AdoLinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prefix: default_ado_prefix(),
        }
    }
}

/// Whether PR actions are covered by a rule that normally applies to commits.
///
/// Each commit-level rule can independently opt in or out of also checking PRs.
/// By default, `PROCESS-COMMIT-DOC-1` covers both commits AND PRs; the others
/// cover only commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrCoverage {
    /// Apply commit-doc body/id rules to the PR body as well. Default: `true`.
    #[serde(default = "default_true")]
    pub apply_body_rule: bool,
    /// Apply commit-subject-level id rules to the PR title as well. Default: `true`.
    #[serde(default = "default_true")]
    pub apply_id_rule: bool,
}

impl Default for PrCoverage {
    fn default() -> Self {
        Self {
            apply_body_rule: true,
            apply_id_rule: true,
        }
    }
}

/// The per-project, serde-serializable configuration for the VCS-action gate.
///
/// Each project persists one of these as `process_rule_config`; the gate builds
/// its live [`ProcessRule`] set from it via [`build_rules`]. The shipped defaults
/// reproduce the previous hardcoded behaviour exactly, so a project with no
/// explicit config is unchanged.
///
/// # Serde back-compatibility
///
/// All fields carry `#[serde(default)]`, so a config document that predates any
/// given field (or omits it) deserialises with the correct default. Adding new
/// fields to this struct is always backwards-compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcessRuleConfig {
    /// Tunables for `PROCESS-COMMIT-DOC-1`: substantive body + story-id reference.
    #[serde(default)]
    pub commit_doc: CommitDocConfig,
    /// Tunables for `PROCESS-CONVENTIONAL-COMMIT-1`: commit subject shape.
    #[serde(default)]
    pub conventional_commit: ConventionalCommitConfig,
    /// Tunables for `PROCESS-BRANCH-NAMING-1`: allowed branch prefixes.
    #[serde(default)]
    pub branch_naming: BranchNamingConfig,
    /// Tunables for `PROCESS-ADO-LINK-1`: ADO ticket reference in subject/title.
    #[serde(default)]
    pub ado_link: AdoLinkConfig,
    /// Controls whether ADO / commit-doc id rules also cover PR fields.
    #[serde(default)]
    pub pr: PrCoverage,
}

/// Build the live set of [`ProcessRule`]s from a [`ProcessRuleConfig`].
///
/// Disabled rules are excluded from the returned set, so the gate only
/// evaluates the rules that are actually active. The rules in the returned
/// slice are equivalent to (and replace) the previous hardcoded constructors.
///
/// # Example
///
/// ```rust
/// use camerata_checks::vcs_action::{ProcessRuleConfig, build_rules, VcsAction, gate};
///
/// let config = ProcessRuleConfig::default();
/// let rules = build_rules(&config);
/// // With default config, conventional_commit is enabled; branch_naming and
/// // ado_link are disabled.
/// assert!(rules.iter().any(|r| r.id == "PROCESS-CONVENTIONAL-COMMIT-1"));
/// assert!(!rules.iter().any(|r| r.id == "PROCESS-ADO-LINK-1"));
/// ```
pub fn build_rules(config: &ProcessRuleConfig) -> Vec<ProcessRule> {
    let mut rules = Vec::new();

    // PROCESS-COMMIT-DOC-1: substantive body + optional story-id.
    if config.commit_doc.enabled {
        let cfg = &config.commit_doc;
        let fmt = &cfg.story_id_format;

        // The commit target: CommitBody.
        // The PR target: PrBody (when apply_body_rule is true).
        let mut applies_to = vec![VcsTarget::CommitBody];
        if config.pr.apply_body_rule {
            applies_to.push(VcsTarget::PrBody);
        }

        if cfg.require_story_id {
            rules.push(ProcessRule {
                id: "PROCESS-COMMIT-DOC-1".to_string(),
                description: format!(
                    "Commit body and PR body must contain at least {} non-blank \
                     characters and a story-id reference ({}{}digits).",
                    cfg.min_body_chars,
                    fmt.prefix,
                    fmt.separator,
                ),
                applies_to,
                matcher: Matcher::SubstantiveWithStoryId {
                    min_non_blank_chars: cfg.min_body_chars,
                    story_id_prefix: fmt.prefix.clone(),
                    story_id_separator: fmt.separator,
                },
            });
        } else {
            // Story-id not required: only the body-length check applies.
            // Matcher::Substantive covers this exactly (no story-id component).
            rules.push(ProcessRule {
                id: "PROCESS-COMMIT-DOC-1".to_string(),
                description: format!(
                    "Commit body and PR body must contain at least {} non-blank characters.",
                    cfg.min_body_chars,
                ),
                applies_to,
                matcher: Matcher::Substantive {
                    min_non_blank_chars: cfg.min_body_chars,
                },
            });
        }
    }

    // PROCESS-CONVENTIONAL-COMMIT-1: subject must follow conventional-commit shape.
    if config.conventional_commit.enabled {
        rules.push(ProcessRule::conventional_commits_with_types(
            &config.conventional_commit.types,
        ));
    }

    // PROCESS-BRANCH-NAMING-1: branch name must start with one of the configured prefixes.
    if config.branch_naming.enabled {
        rules.push(ProcessRule::branch_naming(
            &config
                .branch_naming
                .prefixes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        ));
    }

    // PROCESS-ADO-LINK-1: commit subject + PR title must contain the ADO ticket ref.
    if config.ado_link.enabled {
        let mut applies_to = vec![VcsTarget::CommitSubject];
        if config.pr.apply_id_rule {
            applies_to.push(VcsTarget::PrTitle);
        }
        rules.push(ProcessRule {
            id: "PROCESS-ADO-LINK-1".to_string(),
            description: format!(
                "Commit subject and PR title must contain an `{}#<id>` reference.",
                config.ado_link.prefix,
            ),
            applies_to,
            matcher: Matcher::TicketRef {
                prefix: config.ado_link.prefix.clone(),
            },
        });
    }

    rules
}

// ── Auditable bypass ──────────────────────────────────────────────────────────

/// A request to bypass the VCS-action gate for one action with an explicit reason.
///
/// The bypass is only honoured when `reason` is non-empty. A reason-less bypass
/// is itself a gate violation, mirroring the suppression-waiver invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassRequest {
    /// Non-empty reason why this action is allowed to bypass the gate. Examples:
    /// "machine-generated merge commit from the rebase pipeline",
    /// "one-time onboarding branch pre-dating this project's conventions".
    pub reason: String,
}

/// An auditable record of a successful bypass, returned by [`gate_or_bypass`].
///
/// The caller is responsible for surfacing this record in the evidence trail
/// (e.g. appending it to the UoW history or emitting it as a notification) so
/// bypasses are visible and reviewable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassRecord {
    /// The reason supplied with the bypass request.
    pub reason: String,
    /// The rule ids that would have fired without the bypass.
    pub suppressed_rule_ids: Vec<String>,
}

/// Error returned when a bypass is attempted without a reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bypass rejected: a non-empty reason is required (mirror of the suppression-waiver invariant)")]
pub struct BypassReasonRequired;

/// The gate result when bypass is in play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOrBypassResult {
    /// The action passed the gate — no violations, no bypass needed.
    Passed,
    /// The action failed the gate. The violations are returned.
    Failed(Vec<ProcessViolation>),
    /// The gate would have fired but was bypassed with an auditable reason.
    Bypassed(BypassRecord),
}

/// Like [`gate`], but the caller may supply an optional [`BypassRequest`].
///
/// - `bypass = None` — identical to calling [`gate`] directly.
/// - `bypass = Some(req)` with `req.reason` non-empty — if the gate would fail,
///   return [`GateOrBypassResult::Bypassed`] with a [`BypassRecord`] for the
///   evidence trail. If the gate passes, `Bypassed` is never returned (a bypass
///   on a passing action is a no-op; no record is produced).
/// - `bypass = Some(req)` with `req.reason` empty — return
///   `Err(BypassReasonRequired)`. A reason-less bypass is itself a violation.
///
/// # Errors
///
/// Returns `Err(BypassReasonRequired)` when a bypass is requested without a
/// non-empty reason.
pub fn gate_or_bypass(
    rules: &[ProcessRule],
    action: &VcsAction,
    bypass: Option<&BypassRequest>,
) -> Result<GateOrBypassResult, BypassReasonRequired> {
    // Validate the bypass request first: a reason-less bypass is a hard error.
    if let Some(req) = bypass {
        if req.reason.trim().is_empty() {
            return Err(BypassReasonRequired);
        }
    }

    let violations = evaluate(rules, action);

    if violations.is_empty() {
        // Action passed the gate outright; bypass is irrelevant.
        return Ok(GateOrBypassResult::Passed);
    }

    if let Some(req) = bypass {
        // Bypass is active and the reason is non-empty (checked above).
        let suppressed_rule_ids = violations
            .iter()
            .map(|v| v.rule_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        return Ok(GateOrBypassResult::Bypassed(BypassRecord {
            reason: req.reason.clone(),
            suppressed_rule_ids,
        }));
    }

    // No bypass: propagate the violations.
    Ok(GateOrBypassResult::Failed(violations))
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(msg: &str) -> VcsAction {
        VcsAction::Commit {
            message: msg.to_string(),
        }
    }
    fn pr(title: &str) -> VcsAction {
        VcsAction::PullRequest {
            title: title.to_string(),
            body: String::new(),
        }
    }

    // ── ticket-ref matcher ──────────────────────────────────────────────────

    #[test]
    fn ticket_ref_requires_prefix_hash_and_digits() {
        assert!(contains_ticket_ref("AB#1234 fix the thing", "AB"));
        assert!(contains_ticket_ref("fix the thing (AB#7)", "AB"));
        assert!(!contains_ticket_ref("AB# no number", "AB"));
        assert!(!contains_ticket_ref("AB#xyz not digits", "AB"));
        assert!(!contains_ticket_ref("no reference here", "AB"));
        // A non-digit AB# must not mask a later valid one.
        assert!(contains_ticket_ref("AB#x and also AB#42", "AB"));
    }

    // ── the AB#{id} workplace rule ──────────────────────────────────────────

    #[test]
    fn ado_link_passes_commit_and_pr_with_reference() {
        let rules = [ProcessRule::ado_ticket_link()];
        assert!(gate(&rules, &commit("AB#1234 add export")).is_ok());
        assert!(gate(&rules, &pr("AB#1234 Add export endpoint")).is_ok());
    }

    #[test]
    fn ado_link_refuses_commit_subject_without_reference() {
        let rules = [ProcessRule::ado_ticket_link()];
        let err = gate(&rules, &commit("add export endpoint")).expect_err("must refuse");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].rule_id, "PROCESS-ADO-LINK-1");
        assert_eq!(err[0].target, VcsTarget::CommitSubject);
        assert!(err[0].detail.contains("AB#"));
    }

    #[test]
    fn ado_link_refuses_pr_title_without_reference() {
        let rules = [ProcessRule::ado_ticket_link()];
        let err = gate(&rules, &pr("Add export endpoint")).expect_err("must refuse");
        assert_eq!(err[0].target, VcsTarget::PrTitle);
    }

    #[test]
    fn ado_link_checks_only_the_subject_not_the_body() {
        // A reference in the body does NOT satisfy a subject rule.
        let rules = [ProcessRule::ado_ticket_link()];
        let action = VcsAction::Commit {
            message: "add export\n\nrefs AB#1234".to_string(),
        };
        assert!(
            gate(&rules, &action).is_err(),
            "subject lacks the reference"
        );
    }

    #[test]
    fn rule_targeting_pr_does_not_fire_on_a_commit_action() {
        // ado_ticket_link applies to CommitSubject AND PrTitle. Against a Commit
        // action the PrTitle target is absent, so only the subject is checked.
        let rules = [ProcessRule::ado_ticket_link()];
        let violations = evaluate(&rules, &commit("AB#9 ok"));
        assert!(violations.is_empty());
        // And a branch-only rule never fires on a commit.
        let branch_rules = [ProcessRule::branch_naming(&["feature/"])];
        assert!(evaluate(&branch_rules, &commit("anything")).is_empty());
    }

    // ── conventional commits ────────────────────────────────────────────────

    #[test]
    fn conventional_commit_shapes() {
        let rules = [ProcessRule::conventional_commits()];
        assert!(gate(&rules, &commit("feat: add export")).is_ok());
        assert!(gate(&rules, &commit("fix(api): handle null")).is_ok());
        assert!(gate(&rules, &commit("feat!: breaking change")).is_ok());
        assert!(gate(&rules, &commit("chore(deps)!: bump")).is_ok());
        assert!(gate(&rules, &commit("just a random message")).is_err());
        assert!(gate(&rules, &commit("feat:")).is_err(), "empty subject");
        assert!(gate(&rules, &commit("nope(scope: unbalanced")).is_err());
    }

    // ── branch naming ───────────────────────────────────────────────────────

    #[test]
    fn branch_naming_enforced() {
        let rules = [ProcessRule::branch_naming(&[
            "feature/", "release/", "hotfix/",
        ])];
        assert!(gate(
            &rules,
            &VcsAction::Branch {
                name: "feature/login".into()
            }
        )
        .is_ok());
        assert!(gate(
            &rules,
            &VcsAction::Branch {
                name: "release/v1.2.0".into()
            }
        )
        .is_ok());
        let err = gate(
            &rules,
            &VcsAction::Branch {
                name: "my-random-branch".into(),
            },
        )
        .expect_err("must refuse");
        assert_eq!(err[0].target, VcsTarget::BranchName);
    }

    // ── multiple rules accumulate violations ────────────────────────────────

    #[test]
    fn multiple_rules_report_all_violations() {
        let rules = [
            ProcessRule::ado_ticket_link(),
            ProcessRule::conventional_commits(),
        ];
        // Missing both the AB# ref and the conventional shape.
        let err = gate(&rules, &commit("did some stuff")).expect_err("must refuse");
        assert_eq!(err.len(), 2, "both rules should fire: {err:?}");
        let ids: Vec<&str> = err.iter().map(|v| v.rule_id.as_str()).collect();
        assert!(ids.contains(&"PROCESS-ADO-LINK-1"));
        assert!(ids.contains(&"PROCESS-CONVENTIONAL-COMMIT-1"));
    }

    #[test]
    fn clean_action_passes_all_rules() {
        let rules = [
            ProcessRule::ado_ticket_link(),
            ProcessRule::conventional_commits(),
        ];
        assert!(gate(&rules, &commit("feat: AB#1234 add export")).is_ok());
    }

    // ── Matcher::requirement descriptions ────────────────────────────────────

    #[test]
    fn matcher_requirement_descriptions_are_human_readable() {
        let ticket = Matcher::TicketRef {
            prefix: "AB".to_string(),
        };
        assert!(
            ticket.requirement().contains("AB#"),
            "ticket requirement should mention the prefix#number pattern: {}",
            ticket.requirement()
        );

        let contains = Matcher::Contains {
            needle: "APPROVED".to_string(),
        };
        assert!(contains.requirement().contains("APPROVED"));

        let starts = Matcher::StartsWithAny {
            prefixes: vec!["feature/".to_string(), "hotfix/".to_string()],
        };
        assert!(starts.requirement().contains("feature/"));
        assert!(starts.requirement().contains("hotfix/"));

        let cc = Matcher::ConventionalCommit {
            types: vec!["feat".to_string(), "fix".to_string()],
        };
        assert!(cc.requirement().contains("feat"));
        assert!(cc.requirement().contains("fix"));

        let substantive = Matcher::SubstantiveWithStoryId {
            min_non_blank_chars: 20,
            story_id_prefix: "AB".to_string(),
            story_id_separator: '#',
        };
        let req = substantive.requirement();
        assert!(req.contains("20"), "should mention the min char count");
        assert!(req.contains("AB"), "should mention the prefix");
        assert!(req.contains('#'.to_string().as_str()), "should mention the separator");
    }

    // ── PROCESS-COMMIT-DOC-1 ────────────────────────────────────────────────────

    /// Build a standard PROCESS-COMMIT-DOC-1 rule: 20 non-blank chars, bare `#<num>`.
    fn doc_rule() -> ProcessRule {
        ProcessRule::commit_documentation(20, "", '#')
    }

    #[test]
    fn doc_rule_passes_commit_with_substantive_body_and_story_id() {
        let rules = [doc_rule()];
        // Subject + blank line + body with story id.
        let action = VcsAction::Commit {
            message: "feat: add export endpoint\n\nImplements the export flow. Refs #42."
                .to_string(),
        };
        assert!(gate(&rules, &action).is_ok(), "body is substantive and has story id");
    }

    #[test]
    fn doc_rule_fails_commit_with_subject_only() {
        let rules = [doc_rule()];
        let action = VcsAction::Commit {
            message: "feat: add export endpoint".to_string(),
        };
        let err = gate(&rules, &action).expect_err("subject-only commit must be refused");
        assert!(
            err.iter().any(|v| v.rule_id == "PROCESS-COMMIT-DOC-1"),
            "PROCESS-COMMIT-DOC-1 must fire: {err:?}"
        );
        assert!(
            err.iter().any(|v| v.target == VcsTarget::CommitBody),
            "CommitBody target must be in violation: {err:?}"
        );
    }

    #[test]
    fn doc_rule_fails_commit_body_without_story_id() {
        let rules = [doc_rule()];
        // Body is long enough but lacks the story reference.
        let action = VcsAction::Commit {
            message: "feat: add export endpoint\n\nAdds the new export flow for CSV downloads."
                .to_string(),
        };
        let err = gate(&rules, &action).expect_err("missing story id must be refused");
        assert!(
            err.iter().any(|v| v.rule_id == "PROCESS-COMMIT-DOC-1"),
            "PROCESS-COMMIT-DOC-1 must fire: {err:?}"
        );
    }

    #[test]
    fn doc_rule_fails_commit_body_too_short_even_with_story_id() {
        let rules = [doc_rule()];
        // Body has the story ref but is below the 20-char minimum.
        let action = VcsAction::Commit {
            message: "feat: export\n\n#42".to_string(),
        };
        let err = gate(&rules, &action).expect_err("short body must be refused");
        assert!(
            err.iter().any(|v| v.rule_id == "PROCESS-COMMIT-DOC-1"),
            "PROCESS-COMMIT-DOC-1 must fire: {err:?}"
        );
    }

    #[test]
    fn doc_rule_passes_pr_with_substantive_body_and_story_id() {
        let rules = [doc_rule()];
        let action = VcsAction::PullRequest {
            title: "Add export endpoint".to_string(),
            body: "Implements the CSV export feature. Closes #99.".to_string(),
        };
        assert!(gate(&rules, &action).is_ok(), "PR with body + story id should pass");
    }

    #[test]
    fn doc_rule_fails_pr_with_empty_body() {
        let rules = [doc_rule()];
        let action = VcsAction::PullRequest {
            title: "Add export endpoint".to_string(),
            body: String::new(),
        };
        let err = gate(&rules, &action).expect_err("empty PR body must be refused");
        assert!(
            err.iter().any(|v| v.target == VcsTarget::PrBody),
            "PrBody target must fire: {err:?}"
        );
    }

    #[test]
    fn doc_rule_branch_action_not_gated() {
        // The documentation rule does not apply to branch actions.
        let rules = [doc_rule()];
        let action = VcsAction::Branch {
            name: "feature/export".to_string(),
        };
        assert!(
            gate(&rules, &action).is_ok(),
            "branch action is not in scope for PROCESS-COMMIT-DOC-1"
        );
    }

    #[test]
    fn doc_rule_custom_prefix_and_separator_ado_style() {
        // Custom: AB#42 Azure-Boards-style story reference.
        let rule = ProcessRule::commit_documentation(10, "AB", '#');
        let rules = [rule];

        let passing = VcsAction::Commit {
            message: "fix: null check\n\nFixes null ptr. AB#1234 tracked.".to_string(),
        };
        assert!(gate(&rules, &passing).is_ok());

        // A bare #42 must NOT satisfy an AB# rule.
        let failing = VcsAction::Commit {
            message: "fix: null check\n\nFixes null pointer in handler. #42 tracked.".to_string(),
        };
        assert!(
            gate(&rules, &failing).is_err(),
            "bare #42 does not satisfy AB# prefix rule"
        );
    }

    #[test]
    fn doc_rule_custom_prefix_and_separator_jira_style() {
        // Custom: PROJ-42 Jira-style story reference.
        let rule = ProcessRule::commit_documentation(10, "PROJ", '-');
        let rules = [rule];

        let passing = VcsAction::Commit {
            message: "feat: new widget\n\nAdds the widget. PROJ-42 tracked.".to_string(),
        };
        assert!(gate(&rules, &passing).is_ok());
    }

    // ── is_substantive helper ────────────────────────────────────────────────

    #[test]
    fn is_substantive_counts_non_whitespace_chars() {
        assert!(is_substantive("hello world", 10), "11 non-blank chars");
        assert!(!is_substantive("hello", 10), "only 5 non-blank chars");
        assert!(is_substantive("   lots   of   spaces   here   ", 4));
        assert!(!is_substantive("   ", 1), "whitespace only fails any positive min");
        assert!(is_substantive("", 0), "zero-min is always satisfied");
        assert!(!is_substantive("", 1), "empty string has zero non-blank chars");
    }

    // ── contains_story_id helper ─────────────────────────────────────────────

    #[test]
    fn contains_story_id_bare_hash_reference() {
        assert!(contains_story_id("Closes #42.", "", '#'));
        assert!(contains_story_id("#1 is the first issue", "", '#'));
        assert!(!contains_story_id("no reference here", "", '#'));
    }

    #[test]
    fn contains_story_id_ado_style() {
        assert!(contains_story_id("AB#1234 fix done", "AB", '#'));
        assert!(!contains_story_id("#1234 bare ref not AB", "AB", '#'));
    }

    #[test]
    fn contains_story_id_jira_style() {
        assert!(contains_story_id("PROJ-42 done", "PROJ", '-'));
        assert!(!contains_story_id("PROJ- no number", "PROJ", '-'));
        assert!(!contains_story_id("OTHER-42 wrong prefix", "PROJ", '-'));
    }

    // ── VcsTarget::CommitBody extract ─────────────────────────────────────────

    #[test]
    fn extract_commit_body_returns_text_after_first_newline() {
        let action = VcsAction::Commit {
            message: "feat: subject\n\nBody paragraph here.".to_string(),
        };
        let body = VcsTarget::CommitBody.extract(&action).unwrap();
        assert_eq!(body, "\nBody paragraph here.");
    }

    #[test]
    fn extract_commit_body_returns_empty_string_for_subject_only() {
        let action = VcsAction::Commit {
            message: "feat: subject only".to_string(),
        };
        let body = VcsTarget::CommitBody.extract(&action).unwrap();
        assert_eq!(body, "", "subject-only commit has an empty body, not None");
    }

    // ── VcsTarget::extract edge cases ────────────────────────────────────────

    #[test]
    fn extract_commit_subject_returns_only_first_line() {
        let action = VcsAction::Commit {
            message: "feat: summary line\n\nBody paragraph here.".to_string(),
        };
        let subject = VcsTarget::CommitSubject.extract(&action);
        assert_eq!(subject, Some("feat: summary line"));
    }

    #[test]
    fn extract_returns_none_when_target_mismatches_action_type() {
        // PR title target against a Commit action must return None.
        let action = VcsAction::Commit {
            message: "any message".to_string(),
        };
        assert!(VcsTarget::PrTitle.extract(&action).is_none());
        assert!(VcsTarget::PrBody.extract(&action).is_none());
        assert!(VcsTarget::BranchName.extract(&action).is_none());

        // CommitSubject / CommitBody targets against a PullRequest action must return None.
        let pr_action = VcsAction::PullRequest {
            title: "PR title".to_string(),
            body: "body".to_string(),
        };
        assert!(VcsTarget::CommitSubject.extract(&pr_action).is_none());
        assert!(VcsTarget::CommitMessage.extract(&pr_action).is_none());
        assert!(VcsTarget::CommitBody.extract(&pr_action).is_none());
    }

    #[test]
    fn extract_pr_body_returns_body_text() {
        let action = VcsAction::PullRequest {
            title: "Title".to_string(),
            body: "Detailed PR body here.".to_string(),
        };
        assert_eq!(
            VcsTarget::PrBody.extract(&action),
            Some("Detailed PR body here.")
        );
    }

    // ── contains_ticket_ref edge cases ───────────────────────────────────────

    #[test]
    fn ticket_ref_empty_prefix_always_returns_false() {
        // An empty prefix is pathological; the function must not panic.
        assert!(!contains_ticket_ref("AB#1234 fix", ""));
    }

    #[test]
    fn ticket_ref_scans_past_invalid_occurrences() {
        // First occurrence has no digits, but the second one does.
        assert!(contains_ticket_ref("AB#abc and AB#99 done", "AB"));
    }

    // ── is_conventional_commit edge cases ────────────────────────────────────

    #[test]
    fn conventional_commit_rejects_unbalanced_scope_parens() {
        let types = vec!["feat".to_string()];
        // "(scope" without closing ')' is invalid.
        assert!(!is_conventional_commit("feat(scope: missing close", &types));
    }

    #[test]
    fn conventional_commit_rejects_unknown_type() {
        let types = vec!["feat".to_string(), "fix".to_string()];
        assert!(!is_conventional_commit("wip: work in progress", &types));
    }

    #[test]
    fn conventional_commit_requires_non_empty_subject() {
        let types = vec!["feat".to_string()];
        // "feat:" with nothing after the colon is invalid.
        assert!(!is_conventional_commit("feat:", &types));
        // Whitespace-only subject is also invalid.
        assert!(!is_conventional_commit("feat:   ", &types));
    }

    // ── ProcessRuleConfig + build_rules ─────────────────────────────────────────

    #[test]
    fn default_config_enables_conventional_commit_and_commit_doc_only() {
        // Default: conventional_commit and commit_doc are enabled; ado_link and
        // branch_naming are disabled (opt-in).
        let config = ProcessRuleConfig::default();
        let rules = build_rules(&config);
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"PROCESS-CONVENTIONAL-COMMIT-1"),
            "conventional commit must be enabled by default: {ids:?}"
        );
        assert!(
            ids.contains(&"PROCESS-COMMIT-DOC-1"),
            "commit doc must be enabled by default: {ids:?}"
        );
        assert!(
            !ids.contains(&"PROCESS-ADO-LINK-1"),
            "ado link must be disabled by default: {ids:?}"
        );
        assert!(
            !ids.contains(&"PROCESS-BRANCH-NAMING-1"),
            "branch naming must be disabled by default: {ids:?}"
        );
    }

    #[test]
    fn config_driven_ado_prefix_ab_hash_passes_ab_style_and_rejects_plain() {
        // Enable ado_link with the AB# prefix (Azure Boards style).
        let config = ProcessRuleConfig {
            ado_link: AdoLinkConfig {
                enabled: true,
                prefix: "AB".to_string(),
            },
            conventional_commit: ConventionalCommitConfig { enabled: false, ..Default::default() },
            commit_doc: CommitDocConfig { enabled: false, ..Default::default() },
            ..Default::default()
        };
        let rules = build_rules(&config);
        assert!(rules.iter().any(|r| r.id == "PROCESS-ADO-LINK-1"));

        // AB#123 satisfies the rule.
        let passing = VcsAction::Commit {
            message: "AB#123 fix the thing".to_string(),
        };
        assert!(gate(&rules, &passing).is_ok(), "AB#123 must pass the ADO rule");

        // A plain #123 (GitHub style) does NOT satisfy the AB# rule.
        let failing = VcsAction::Commit {
            message: "#123 fix the thing".to_string(),
        };
        assert!(
            gate(&rules, &failing).is_err(),
            "plain #123 must NOT satisfy the AB# rule"
        );
    }

    #[test]
    fn config_driven_min_body_chars_enforced() {
        // Set a custom min_body_chars of 5 and disable story-id.
        let config = ProcessRuleConfig {
            commit_doc: CommitDocConfig {
                enabled: true,
                min_body_chars: 5,
                require_story_id: false,
                ..Default::default()
            },
            conventional_commit: ConventionalCommitConfig { enabled: false, ..Default::default() },
            ..Default::default()
        };
        let rules = build_rules(&config);

        // Body with 5+ non-blank chars should pass.
        let passing = VcsAction::Commit {
            message: "feat: thing\n\nhello world".to_string(),
        };
        assert!(gate(&rules, &passing).is_ok(), "5+ char body must pass");

        // Body with < 5 non-blank chars should fail.
        let failing = VcsAction::Commit {
            message: "feat: thing\n\nhi".to_string(), // "hi" = 2 chars
        };
        assert!(gate(&rules, &failing).is_err(), "short body must fail");
    }

    #[test]
    fn config_driven_require_story_id_false_ignores_story_ref() {
        // With require_story_id=false, a substantive body without any story ref passes.
        let config = ProcessRuleConfig {
            commit_doc: CommitDocConfig {
                enabled: true,
                min_body_chars: 20,
                require_story_id: false,
                ..Default::default()
            },
            conventional_commit: ConventionalCommitConfig { enabled: false, ..Default::default() },
            ..Default::default()
        };
        let rules = build_rules(&config);

        let action = VcsAction::Commit {
            message: "feat: export\n\nAdds the CSV export flow without a story ref.".to_string(),
        };
        assert!(
            gate(&rules, &action).is_ok(),
            "substantive body without story ref must pass when require_story_id=false"
        );
    }

    #[test]
    fn config_driven_custom_commit_types_accepted() {
        let config = ProcessRuleConfig {
            conventional_commit: ConventionalCommitConfig {
                enabled: true,
                types: vec!["feat".to_string(), "wip".to_string()],
            },
            commit_doc: CommitDocConfig { enabled: false, ..Default::default() },
            ..Default::default()
        };
        let rules = build_rules(&config);

        // "wip" is a custom type that the default set does not include.
        let action = VcsAction::Commit {
            message: "wip: in progress".to_string(),
        };
        assert!(gate(&rules, &action).is_ok(), "custom type 'wip' must be accepted");

        // "chore" is in the default set but not this custom set — it must fail.
        let unknown = VcsAction::Commit {
            message: "chore: cleanup".to_string(),
        };
        assert!(gate(&rules, &unknown).is_err(), "'chore' not in custom type set must fail");
    }

    #[test]
    fn config_driven_branch_naming_opt_in() {
        let config = ProcessRuleConfig {
            branch_naming: BranchNamingConfig {
                enabled: true,
                prefixes: vec!["feature/".to_string(), "release/".to_string()],
            },
            conventional_commit: ConventionalCommitConfig { enabled: false, ..Default::default() },
            commit_doc: CommitDocConfig { enabled: false, ..Default::default() },
            ..Default::default()
        };
        let rules = build_rules(&config);
        assert!(rules.iter().any(|r| r.id == "PROCESS-BRANCH-NAMING-1"));

        let ok_branch = VcsAction::Branch { name: "feature/my-thing".to_string() };
        assert!(gate(&rules, &ok_branch).is_ok());

        let bad_branch = VcsAction::Branch { name: "my-thing".to_string() };
        assert!(gate(&rules, &bad_branch).is_err());
    }

    #[test]
    fn config_serde_round_trip() {
        // The config must survive JSON round-trip (the persistence contract).
        let config = ProcessRuleConfig {
            commit_doc: CommitDocConfig {
                enabled: true,
                min_body_chars: 30,
                require_story_id: true,
                id_location: IdLocation::Either,
                story_id_format: StoryIdFormat {
                    prefix: "AB".to_string(),
                    separator: '#',
                    custom_regex: None,
                },
            },
            conventional_commit: ConventionalCommitConfig {
                enabled: true,
                types: vec!["feat".to_string(), "fix".to_string()],
            },
            branch_naming: BranchNamingConfig {
                enabled: true,
                prefixes: vec!["feature/".to_string()],
            },
            ado_link: AdoLinkConfig {
                enabled: true,
                prefix: "AB".to_string(),
            },
            pr: PrCoverage {
                apply_body_rule: true,
                apply_id_rule: false,
            },
        };
        let json = serde_json::to_string(&config).expect("must serialize");
        let back: ProcessRuleConfig = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(config, back, "config must round-trip through JSON");
    }

    #[test]
    fn config_defaults_fill_missing_fields_from_old_json() {
        // A legacy/partial JSON document must deserialize with correct defaults.
        let json = r#"{"commit_doc": {"enabled": true}}"#;
        let config: ProcessRuleConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(config.commit_doc.min_body_chars, 20, "default min_body_chars");
        assert!(config.commit_doc.require_story_id, "default require_story_id");
        assert!(!config.ado_link.enabled, "default ado_link disabled");
        assert!(!config.branch_naming.enabled, "default branch_naming disabled");
    }

    // ── Bypass mechanism ──────────────────────────────────────────────────────

    #[test]
    fn bypass_without_reason_is_rejected() {
        let rules = [ProcessRule::conventional_commits()];
        let action = commit("just a random message");
        let req = BypassRequest { reason: String::new() };
        assert!(
            gate_or_bypass(&rules, &action, Some(&req)).is_err(),
            "reason-less bypass must return Err(BypassReasonRequired)"
        );

        // Whitespace-only is also rejected.
        let req_ws = BypassRequest { reason: "   ".to_string() };
        assert!(
            gate_or_bypass(&rules, &action, Some(&req_ws)).is_err(),
            "whitespace-only reason must also be rejected"
        );
    }

    #[test]
    fn bypass_with_reason_records_suppressed_rules() {
        let rules = [
            ProcessRule::conventional_commits(),
            ProcessRule::ado_ticket_link(),
        ];
        // This commit violates both rules.
        let action = commit("not a conventional commit, no ADO ref");
        let req = BypassRequest {
            reason: "machine-generated merge commit, pre-dates conventions".to_string(),
        };

        let result = gate_or_bypass(&rules, &action, Some(&req))
            .expect("bypass with reason must not return Err");

        match result {
            GateOrBypassResult::Bypassed(record) => {
                assert_eq!(record.reason, req.reason, "reason must be preserved");
                assert!(
                    record.suppressed_rule_ids.contains(&"PROCESS-CONVENTIONAL-COMMIT-1".to_string()),
                    "conventional commit violation must be recorded: {:?}",
                    record.suppressed_rule_ids
                );
                assert!(
                    record.suppressed_rule_ids.contains(&"PROCESS-ADO-LINK-1".to_string()),
                    "ADO link violation must be recorded: {:?}",
                    record.suppressed_rule_ids
                );
            }
            other => panic!("expected Bypassed, got {other:?}"),
        }
    }

    #[test]
    fn bypass_on_passing_action_returns_passed_not_bypassed() {
        // When the action already passes, Bypassed is never returned — even if a
        // bypass request is supplied.
        let rules = [ProcessRule::conventional_commits()];
        let action = commit("feat: add export");
        let req = BypassRequest { reason: "unnecessary but harmless".to_string() };

        let result = gate_or_bypass(&rules, &action, Some(&req))
            .expect("must not error on a valid reason");
        assert_eq!(
            result,
            GateOrBypassResult::Passed,
            "a passing action must return Passed, never Bypassed"
        );
    }

    #[test]
    fn bypass_none_propagates_violations_like_gate() {
        // With bypass=None, gate_or_bypass is equivalent to gate().
        let rules = [ProcessRule::conventional_commits()];
        let action = commit("just a message");
        let result = gate_or_bypass(&rules, &action, None)
            .expect("no bypass request means no reason-check error");
        match result {
            GateOrBypassResult::Failed(violations) => {
                assert!(!violations.is_empty(), "violations must propagate");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
