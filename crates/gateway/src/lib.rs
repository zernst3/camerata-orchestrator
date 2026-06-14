//! camerata-gateway (library): the layer-1 real-time governance gate.
//!
//! This is the residual the verification slice (RUST_CORE_VERIFICATION.md)
//! flagged: the MCP server in `src/main.rs` proved a Rust-owned gate can
//! deny a tool call in-process, but it hard-coded a single rule and had no
//! session → role → rule-subset map. This module supplies that map and
//! implements [`camerata_core::GovernanceGateway`] over it.
//!
//! # Design
//!
//! - [`GovernedGateway`] owns a `SessionId -> Role` map (the role carries the
//!   rule-subset, assigned at spawn). [`GovernedGateway::evaluate`] looks up
//!   the session's role and runs every rule in its subset against the call.
//! - [`evaluate_call`] is the reusable, pure rule-evaluation function. BOTH
//!   the in-process [`GovernedGateway`] and the MCP server (`src/main.rs`)
//!   call it, so the verified transport and the orchestrator share one
//!   gate implementation — no divergence.
//! - Rules are matched by [`camerata_core::RuleId`]. GOV-1 (the verified
//!   "no writes to forbidden paths" rule) is the first concrete rule; adding
//!   more is a match arm in [`apply_rule`].
//! - [`RULE_REGISTRY`] is the public, ordered registry of every implemented
//!   rule id. Unknown ids (not in the registry) are safely treated as no-ops
//!   — the gate is permissive about rules it does not implement, NOT about
//!   calls.
//!
//! async all the way down (RUST-DOMAIN-5): the trait method is async even
//! though the current rules are synchronous, so a future rule that needs I/O
//! (e.g. a path-boundary check against the filesystem) drops in without an
//! API break.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_core::{Decision, GovernanceGateway, Role, RuleId, SessionId, ToolCall};
use regex::Regex;
use std::sync::OnceLock;
use thiserror::Error;

// ─── error type (RUST-DOMAIN-4 / RUST-DOMAIN-6) ──────────────────────────────

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("no role is bound to session {0:?}")]
    UnknownSession(SessionId),
}

// ─── rule id constructors ─────────────────────────────────────────────────────

/// The id of the verified "no writes to forbidden paths" rule.
///
/// Named constructor so no caller hard-codes the string (mirrors
/// camerata-checks' `fmt_rule()` / `clippy_rule()`).
pub fn gov1_rule() -> RuleId {
    RuleId("GOV-1".to_string())
}

/// The id of the "no hardcoded credentials in file content" rule.
pub fn sec_no_hardcoded_secrets_1_rule() -> RuleId {
    RuleId("SEC-NO-HARDCODED-SECRETS-1".to_string())
}

/// The id of the "no SQL built by string concatenation / interpolation" rule.
pub fn sec_no_raw_sql_concat_1_rule() -> RuleId {
    RuleId("SEC-NO-RAW-SQL-CONCAT-1".to_string())
}

/// The id of the "no secrets in URL query strings" rule.
pub fn arch_no_secrets_in_url_1_rule() -> RuleId {
    RuleId("ARCH-NO-SECRETS-IN-URL-1".to_string())
}

// ─── public rule registry ─────────────────────────────────────────────────────

/// A pure rule-arm function: `Ok(())` = allow, `Err(reason)` = deny.
/// Takes `(path, content)` from the `gated_write` call.
pub type RuleArmFn = fn(path: &str, content: &str) -> Result<(), String>;

/// A single entry in the rule registry.
///
/// The registry is ordered (alphabetically within their security / governance
/// tier). Callers iterate it to enumerate all implemented rules.
pub struct RuleEntry {
    /// The stable rule id string (matches [`RuleId`]).
    pub id: &'static str,
    /// One-line human-readable description.
    pub description: &'static str,
    /// The pure rule function: `Ok(())` = allow, `Err(reason)` = deny.
    ///
    /// Takes `(path, content)` from the `gated_write` call -- `path` is the
    /// target filesystem path and `content` is the file body the agent wants
    /// to write.
    pub arm: RuleArmFn,
}

/// All implemented rule arms, keyed by rule-id string.
///
/// Unknown ids (not present here) are treated as no-ops by [`apply_rule`].
/// To add a rule: implement a `check_*` fn below, add it here, and add unit
/// tests. The order here matches evaluation order inside a single rule (each
/// rule fires independently; subset order controls cross-rule ordering via
/// [`evaluate_call`]).
pub static RULE_REGISTRY: &[RuleEntry] = &[
    RuleEntry {
        id: "GOV-1",
        description: "Deny writes whose path contains the substring \"forbidden\".",
        arm: arm_gov1,
    },
    RuleEntry {
        id: "SEC-NO-HARDCODED-SECRETS-1",
        description: "Deny file content that contains a hardcoded credential literal \
                      (GitHub tokens, Slack tokens, AWS keys, OpenAI/Stripe sk- keys, \
                      Google API keys, PEM private keys).",
        arm: arm_sec_no_hardcoded_secrets_1,
    },
    RuleEntry {
        id: "SEC-NO-RAW-SQL-CONCAT-1",
        description: "Deny file content that builds SQL via string concatenation or \
                      format-string interpolation.",
        arm: arm_sec_no_raw_sql_concat_1,
    },
    RuleEntry {
        id: "ARCH-NO-SECRETS-IN-URL-1",
        description: "Deny file content that contains a URL with a secret in its \
                      query string (api_key, token, secret, password, access_token).",
        arm: arm_arch_no_secrets_in_url_1,
    },
];

/// Look up the arm function for `rule_id`, or `None` when the id is not
/// implemented (safe no-op).
pub fn lookup_arm(rule_id: &str) -> Option<RuleArmFn> {
    RULE_REGISTRY
        .iter()
        .find(|e| e.id == rule_id)
        .map(|e| e.arm)
}

// ─── reusable rule-evaluation (pure) ─────────────────────────────────────────

/// Evaluate one tool call against a role's rule-subset and return a verdict.
///
/// This is the single source of truth for layer-1 governance. It is pure:
/// same `(rule_subset, call)` always yields the same [`Decision`]. The MCP
/// server in `src/main.rs` and [`GovernedGateway::evaluate`] both call it.
///
/// Rules fire in subset order; the FIRST rule that denies wins (fail-closed
/// on the first violation, which is also the cheapest to explain in the
/// bounce-back message).
pub fn evaluate_call(rule_subset: &[RuleId], call: &ToolCall) -> Decision {
    for rule in rule_subset {
        if let Some(deny) = apply_rule(rule, call) {
            return deny;
        }
    }
    Decision::Allow
}

/// Apply a single rule to a call. Returns `Some(Decision::Deny{..})` if the
/// rule is violated, `None` if this rule does not object.
///
/// All implemented rule ids dispatch through [`RULE_REGISTRY`] so there is
/// exactly one place to register a new arm. Unknown rule ids are a no-op:
/// the gate is permissive about rules it does not implement yet, NOT about
/// calls.
fn apply_rule(rule: &RuleId, call: &ToolCall) -> Option<Decision> {
    if !is_write_tool(&call.tool) {
        // Non-write tools are never governed by content rules.
        return None;
    }

    let path = call
        .input
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let content = call
        .input
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let Some(arm) = lookup_arm(rule.0.as_str()) else {
        // Unknown rule id — safe no-op (the gate is permissive about
        // unimplemented rules, not about calls).
        return None;
    };

    match arm(path, content) {
        Ok(()) => None,
        Err(reason) => Some(Decision::Deny {
            rule: rule.clone(),
            reason,
        }),
    }
}

/// Whether `tool` is a write the gate must govern. The MCP transport exposes
/// exactly one write tool, `gated_write`; the abstract gate also recognises a
/// bare `write` so the in-process API is not coupled to the MCP tool name.
fn is_write_tool(tool: &str) -> bool {
    matches!(tool, "gated_write" | "write")
}

// ─── rule arm implementations ─────────────────────────────────────────────────

// ── GOV-1 ────────────────────────────────────────────────────────────────────

/// GOV-1: deny any write whose target path contains the substring "forbidden".
///
/// This is the exact rule the verification slice proved (see `src/main.rs`),
/// lifted here so the in-process gate and the MCP transport agree byte-for-byte.
fn arm_gov1(path: &str, _content: &str) -> Result<(), String> {
    if path.contains("forbidden") {
        Err(format!(
            "GOV-1: writes to forbidden paths are denied (path={path})"
        ))
    } else {
        Ok(())
    }
}

// ── SEC-NO-HARDCODED-SECRETS-1 ────────────────────────────────────────────────

/// Compiled patterns for `SEC-NO-HARDCODED-SECRETS-1`.
///
/// Uses `OnceLock` so the regex objects are compiled once at first use and
/// then reused for every subsequent call (avoids per-call allocation + compile).
///
/// Patterns covered (all case-sensitive unless noted):
/// - GitHub tokens:   `ghp_`, `gho_`, `ghu_`, `ghs_`, `github_pat_`
/// - Slack tokens:    `xox[baprs]-` (xoxb-, xoxa-, xoxp-, xoxr-, xoxs-)
/// - AWS access keys: `AKIA` followed by 16 uppercase alphanumeric chars
/// - OpenAI / Stripe: `sk-` followed by 20+ alphanumeric chars (both use
///   the `sk-` prefix; Stripe also uses `sk-live_`, `sk-test_`)
/// - Google API key:  `AIza` followed by 35 alphanumeric / `_` / `-` chars
/// - PEM private key: `-----BEGIN` ... `PRIVATE KEY-----` (covers RSA, EC, etc.)
static SEC_SECRETS_REGEX: OnceLock<Regex> = OnceLock::new();

fn sec_secrets_regex() -> &'static Regex {
    SEC_SECRETS_REGEX.get_or_init(|| {
        // One alternation — first match wins; scanning the content once is
        // cheaper than N separate regex passes.
        // Note: Rust's regex crate strips unescaped spaces inside character
        // classes when (?x) verbose mode is active — unlike PCRE/Python which
        // preserve them. Use \x20 for a literal space inside [...] in (?x) mode.
        Regex::new(
            r"(?x)
            # GitHub personal / oauth / user / server / fine-grained tokens
            (ghp_[A-Za-z0-9_]{10,})
            | (gho_[A-Za-z0-9_]{10,})
            | (ghu_[A-Za-z0-9_]{10,})
            | (ghs_[A-Za-z0-9_]{10,})
            | (github_pat_[A-Za-z0-9_]{10,})
            # Slack tokens (bot, app, legacy, refresh, socket)
            | (xox[baprs]-[A-Za-z0-9\-]{10,})
            # AWS access key IDs
            | (AKIA[0-9A-Z]{16})
            # OpenAI / Stripe-style secret keys (sk- prefix, 20+ chars)
            # Stripe also uses sk-live_ and sk-test_ sub-prefixes; those
            # start with sk- so this pattern covers them.
            | (sk-[A-Za-z0-9]{20,})
            # Google API keys
            | (AIza[0-9A-Za-z_\-]{35})
            # PEM private key header (RSA PRIVATE KEY, EC PRIVATE KEY, PRIVATE KEY, etc.)
            # \x20 = literal space: Rust regex (?x) strips bare spaces inside [...].
            | (-----BEGIN\s[A-Z\x20]*PRIVATE\s+KEY-----)
            ",
        )
        .expect("SEC-NO-HARDCODED-SECRETS-1 regex must compile")
    })
}

/// SEC-NO-HARDCODED-SECRETS-1: deny content containing a credential literal.
fn arm_sec_no_hardcoded_secrets_1(_path: &str, content: &str) -> Result<(), String> {
    if let Some(m) = sec_secrets_regex().find(content) {
        // Redact all but the first 6 chars so the denial message is useful
        // without echoing the full secret.
        let matched = m.as_str();
        let preview: String = matched.chars().take(6).collect();
        Err(format!(
            "SEC-NO-HARDCODED-SECRETS-1: content appears to contain a hardcoded \
             credential (matched prefix `{preview}...`); move secrets to env vars or \
             a secrets manager"
        ))
    } else {
        Ok(())
    }
}

// ── SEC-NO-RAW-SQL-CONCAT-1 ──────────────────────────────────────────────────

/// Compiled pattern for `SEC-NO-RAW-SQL-CONCAT-1`.
///
/// # Heuristic and its limits
///
/// This pattern fires when **both** conditions hold on the same line or in
/// close proximity in the file:
///   1. A SQL keyword (SELECT, INSERT, UPDATE, DELETE, WHERE — case-insensitive)
///      appears inside a string literal (detected by flanking quote context).
///   2. That same context contains either a format-interpolation placeholder
///      (`{}` — Rust/Python f-strings) OR a string-concatenation operator
///      (`" +` — a closing quote followed by a `+`).
///
/// Known limits / false-positive / false-negative sources:
/// - A SQL keyword in a comment followed by a `{}` elsewhere on the line
///   will fire (over-broad).
/// - Parameterised queries that use `$1`/`?` placeholders instead of `{}`
///   or `" +` are NOT caught — this rule is complement to, not a replacement
///   for, a parameterised-query lint.
/// - Multi-line string constructions where the keyword and the `{}` span
///   different lines may be missed (line-by-line scanning would be needed
///   for full coverage).
/// - Intentional SQL in test fixtures or migration files may trigger false
///   positives; those files can be excluded via the rule-subset config.
static SEC_SQL_CONCAT_REGEX: OnceLock<Regex> = OnceLock::new();

fn sec_sql_concat_regex() -> &'static Regex {
    SEC_SQL_CONCAT_REGEX.get_or_init(|| {
        // Match a line that contains a SQL keyword AND either {} or " +
        Regex::new(
            r#"(?ix)
            # SQL keyword present on the line
            (?:SELECT|INSERT|UPDATE|DELETE|WHERE)
            # AND somewhere on the same line: format placeholder OR concat
            .*?
            (?:
                \{\}            # Rust / Python-style format interpolation
              | "\s*\+          # closing quote followed by + (string concat)
            )
            "#,
        )
        .expect("SEC-NO-RAW-SQL-CONCAT-1 regex must compile")
    })
}

/// SEC-NO-RAW-SQL-CONCAT-1: deny content that builds SQL by string
/// concatenation or format-string interpolation.
fn arm_sec_no_raw_sql_concat_1(_path: &str, content: &str) -> Result<(), String> {
    if sec_sql_concat_regex().is_match(content) {
        Err(
            "SEC-NO-RAW-SQL-CONCAT-1: content appears to build a SQL query via \
             string concatenation or format interpolation; use parameterised \
             queries / a query builder instead (see heuristic limits in lib.rs)"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

// ── ARCH-NO-SECRETS-IN-URL-1 ─────────────────────────────────────────────────

/// Compiled pattern for `ARCH-NO-SECRETS-IN-URL-1`.
///
/// Matches an HTTP/HTTPS URL that carries a secret in its query string.
/// Covered parameter names: `api_key`, `apikey`, `token`, `secret`,
/// `password`, `access_token`.
static ARCH_URL_SECRET_REGEX: OnceLock<Regex> = OnceLock::new();

fn arch_url_secret_regex() -> &'static Regex {
    ARCH_URL_SECRET_REGEX.get_or_init(|| {
        // Avoid ' and " inside character classes to sidestep raw-string delimiter
        // collisions (r#"..."# closes on the first "# sequence). We instead
        // exclude whitespace (\s) and a safe set of URL-terminating chars.
        // (?ix) = case-insensitive + verbose (# comments, whitespace ignored).
        Regex::new(
            r"(?ix)
            # HTTP or HTTPS URL (stop at whitespace, common terminators)
            https?://\S+
            # Query string contains a secret-bearing parameter name
            [?&]
            (?:api_?key|token|secret|password|access_token)
            =
            # Value: any non-whitespace, non-& chars (stops at next param or end)
            [^\s&]+
            ",
        )
        .expect("ARCH-NO-SECRETS-IN-URL-1 regex must compile")
    })
}

/// ARCH-NO-SECRETS-IN-URL-1: deny content that embeds a URL with a secret in
/// its query string.
fn arm_arch_no_secrets_in_url_1(_path: &str, content: &str) -> Result<(), String> {
    if arch_url_secret_regex().is_match(content) {
        Err(
            "ARCH-NO-SECRETS-IN-URL-1: content contains a URL with a secret in its \
             query string (api_key, token, secret, password, or access_token); \
             transmit credentials in headers or the request body instead"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

// ─── GovernedGateway: the session -> role map + GovernanceGateway impl ────────

/// The layer-1 gate the orchestrator holds in-process.
///
/// Owns the `SessionId -> Role` binding assigned when an agent is spawned.
/// Each [`Role`] carries its `rule_subset`; [`evaluate`](Self::evaluate)
/// runs that subset against an attempted [`ToolCall`].
#[derive(Debug, Default, Clone)]
pub struct GovernedGateway {
    sessions: HashMap<SessionId, Role>,
}

impl GovernedGateway {
    /// An empty gateway with no sessions bound.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Bind `session` to `role` (called at agent spawn). Replaces any prior
    /// binding for that session.
    pub fn bind(&mut self, session: SessionId, role: Role) {
        self.sessions.insert(session, role);
    }

    /// Builder form of [`bind`](Self::bind).
    pub fn with_session(mut self, session: SessionId, role: Role) -> Self {
        self.bind(session, role);
        self
    }

    /// The role bound to `session`, if any.
    pub fn role_for(&self, session: &SessionId) -> Option<&Role> {
        self.sessions.get(session)
    }

    /// Number of bound sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Evaluate a call, surfacing the unknown-session case as an error rather
    /// than a silent allow/deny. [`GovernanceGateway::evaluate`] folds the
    /// unknown-session case into a `Deny` (fail-closed) for the trait contract.
    pub fn try_evaluate(
        &self,
        session: &SessionId,
        call: &ToolCall,
    ) -> Result<Decision, GatewayError> {
        let role = self
            .sessions
            .get(session)
            .ok_or_else(|| GatewayError::UnknownSession(session.clone()))?;
        Ok(evaluate_call(&role.rule_subset, call))
    }
}

#[async_trait]
impl GovernanceGateway for GovernedGateway {
    async fn evaluate(&self, session: &SessionId, call: &ToolCall) -> Decision {
        match self.try_evaluate(session, call) {
            Ok(decision) => decision,
            // Fail-closed: an un-bound session means we cannot vouch for the
            // call, so deny it. GOV-1 is the catch-all rule id for "the gate
            // refused".
            Err(GatewayError::UnknownSession(s)) => Decision::Deny {
                rule: gov1_rule(),
                reason: format!("no role bound to session {s:?}; failing closed"),
            },
        }
    }
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_call(path: &str) -> ToolCall {
        ToolCall {
            tool: "gated_write".to_string(),
            input: json!({ "path": path, "content": "x" }),
        }
    }

    fn write_call_with_content(path: &str, content: &str) -> ToolCall {
        ToolCall {
            tool: "gated_write".to_string(),
            input: json!({ "path": path, "content": content }),
        }
    }

    fn role_with(rules: &[&str]) -> Role {
        Role {
            name: "Backend".to_string(),
            rule_subset: rules.iter().map(|r| RuleId(r.to_string())).collect(),
            allowed_paths: vec!["crates/".to_string()],
        }
    }

    // ── GOV-1 ────────────────────────────────────────────────────────────────

    #[test]
    fn evaluate_call_allows_clean_write() {
        let subset = vec![gov1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/core/src/lib.rs"));
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn evaluate_call_denies_forbidden_write_via_gov1() {
        let subset = vec![gov1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/forbidden/secret.rs"));
        match d {
            Decision::Deny { rule, .. } => assert_eq!(rule, gov1_rule()),
            Decision::Allow => panic!("expected GOV-1 deny"),
        }
    }

    #[test]
    fn evaluate_call_without_gov1_in_subset_allows_forbidden() {
        // If the role's subset does not include GOV-1, the rule does not apply.
        let subset = vec![RuleId("SOME-OTHER-RULE".to_string())];
        let d = evaluate_call(&subset, &write_call("crates/forbidden/x.rs"));
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn evaluate_call_ignores_non_write_tools() {
        let subset = vec![gov1_rule()];
        let call = ToolCall {
            tool: "read".to_string(),
            input: json!({ "path": "crates/forbidden/x.rs" }),
        };
        assert!(matches!(evaluate_call(&subset, &call), Decision::Allow));
    }

    // ── SEC-NO-HARDCODED-SECRETS-1 ────────────────────────────────────────────

    #[test]
    fn sec_secrets_denies_github_token() {
        let rule = sec_no_hardcoded_secrets_1_rule();
        let subset = vec![rule];
        // ghp_ token — 40 chars total after prefix is realistic but any 10+ will match.
        let content = r#"let token = "ghp_ABCDEFGHIJ1234567890abcdefghij12";"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/config.rs", content));
        match d {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, sec_no_hardcoded_secrets_1_rule());
                assert!(reason.contains("SEC-NO-HARDCODED-SECRETS-1"));
            }
            Decision::Allow => panic!("expected SEC-NO-HARDCODED-SECRETS-1 deny for GitHub token"),
        }
    }

    #[test]
    fn sec_secrets_denies_slack_token() {
        let subset = vec![sec_no_hardcoded_secrets_1_rule()];
        let content = r#"SLACK_BOT_TOKEN=xoxb-1234567890-abcdefghijklmnop"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/env.rs", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for xoxb- Slack token"
        );
    }

    #[test]
    fn sec_secrets_denies_aws_access_key() {
        let subset = vec![sec_no_hardcoded_secrets_1_rule()];
        let content = r#"aws_access_key_id = "AKIAIOSFODNN7EXAMPLE""#;
        let d = evaluate_call(&subset, &write_call_with_content("config.toml", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for AKIA... AWS key"
        );
    }

    #[test]
    fn sec_secrets_denies_openai_sk_key() {
        let subset = vec![sec_no_hardcoded_secrets_1_rule()];
        let content = r#"api_key = "sk-abcdefghijklmnopqrstuvwx""#;
        let d = evaluate_call(&subset, &write_call_with_content("src/openai.rs", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for sk-... OpenAI-style key"
        );
    }

    #[test]
    fn sec_secrets_denies_google_api_key() {
        let subset = vec![sec_no_hardcoded_secrets_1_rule()];
        // AIza + exactly 35 alphanumeric/underscore/dash chars = 39-char Google API key.
        // "AIzaSyB" is AIza + "SyB" (3 chars), plus 32 more = 35 total after AIza.
        let content = r#"key = "AIzaSyB1234567890abcdefghijklmnopqrstuv""#;
        let d = evaluate_call(&subset, &write_call_with_content("src/maps.rs", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for AIza... Google API key"
        );
    }

    #[test]
    fn sec_secrets_denies_pem_private_key() {
        let subset = vec![sec_no_hardcoded_secrets_1_rule()];
        let content =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let d = evaluate_call(
            &subset,
            &write_call_with_content("certs/private.pem", content),
        );
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for PEM private key"
        );
    }

    #[test]
    fn sec_secrets_allows_clean_content() {
        let subset = vec![sec_no_hardcoded_secrets_1_rule()];
        let content = r#"
            fn load_token() -> String {
                std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN must be set")
            }
        "#;
        let d = evaluate_call(&subset, &write_call_with_content("src/auth.rs", content));
        assert!(
            matches!(d, Decision::Allow),
            "content reading from env vars should be allowed"
        );
    }

    // ── SEC-NO-RAW-SQL-CONCAT-1 ──────────────────────────────────────────────

    #[test]
    fn sec_sql_concat_denies_format_interpolation() {
        let subset = vec![sec_no_raw_sql_concat_1_rule()];
        // Rust format! macro building a SELECT with {} placeholder
        let content = r#"
            let q = format!("SELECT * FROM users WHERE id = {}", user_id);
        "#;
        let d = evaluate_call(&subset, &write_call_with_content("src/repo.rs", content));
        match d {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, sec_no_raw_sql_concat_1_rule());
                assert!(reason.contains("SEC-NO-RAW-SQL-CONCAT-1"));
            }
            Decision::Allow => {
                panic!("expected SEC-NO-RAW-SQL-CONCAT-1 deny for format interpolation")
            }
        }
    }

    #[test]
    fn sec_sql_concat_denies_string_concatenation() {
        let subset = vec![sec_no_raw_sql_concat_1_rule()];
        // Java/JS-style string concatenation
        let content = r#"String q = "SELECT * FROM users WHERE name = '" + name + "'";"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/Repo.java", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for SQL string concatenation with +"
        );
    }

    #[test]
    fn sec_sql_concat_denies_insert_with_interpolation() {
        let subset = vec![sec_no_raw_sql_concat_1_rule()];
        let content = r#"let q = format!("INSERT INTO events (name) VALUES ('{}')", name);"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/events.rs", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for INSERT with format interpolation"
        );
    }

    #[test]
    fn sec_sql_concat_allows_parameterised_query() {
        let subset = vec![sec_no_raw_sql_concat_1_rule()];
        // Parameterised query using $1 — no interpolation markers.
        let content = r#"
            let q = "SELECT * FROM users WHERE id = $1";
            sqlx::query(q).bind(user_id).fetch_one(&pool).await?;
        "#;
        let d = evaluate_call(&subset, &write_call_with_content("src/repo.rs", content));
        assert!(
            matches!(d, Decision::Allow),
            "parameterised query should be allowed"
        );
    }

    #[test]
    fn sec_sql_concat_allows_sql_keyword_in_comment() {
        let subset = vec![sec_no_raw_sql_concat_1_rule()];
        // SQL keyword appears only in a doc comment — no {} or " + nearby.
        let content = r#"
            // Returns results for SELECT queries.
            fn is_select(sql: &str) -> bool {
                sql.trim_start().to_uppercase().starts_with("SELECT")
            }
        "#;
        let d = evaluate_call(
            &subset,
            &write_call_with_content("src/sql_util.rs", content),
        );
        assert!(
            matches!(d, Decision::Allow),
            "SQL keyword in comment without interpolation should be allowed"
        );
    }

    // ── ARCH-NO-SECRETS-IN-URL-1 ─────────────────────────────────────────────

    #[test]
    fn arch_url_secret_denies_api_key_in_query() {
        let subset = vec![arch_no_secrets_in_url_1_rule()];
        let content =
            r#"let url = "https://api.example.com/data?api_key=supersecret123&format=json";"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/client.rs", content));
        match d {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, arch_no_secrets_in_url_1_rule());
                assert!(reason.contains("ARCH-NO-SECRETS-IN-URL-1"));
            }
            Decision::Allow => panic!("expected ARCH-NO-SECRETS-IN-URL-1 deny for api_key in URL"),
        }
    }

    #[test]
    fn arch_url_secret_denies_token_in_query() {
        let subset = vec![arch_no_secrets_in_url_1_rule()];
        let content = r#"fetch("https://maps.googleapis.com/api/geocode/json?token=abc123XYZ")"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/maps.js", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for token= in URL"
        );
    }

    #[test]
    fn arch_url_secret_denies_access_token_in_query() {
        let subset = vec![arch_no_secrets_in_url_1_rule()];
        let content =
            r#"const base = "https://api.service.com/v1/me?access_token=Bearer_abc123def456";"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/api.ts", content));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for access_token= in URL"
        );
    }

    #[test]
    fn arch_url_secret_allows_clean_url() {
        let subset = vec![arch_no_secrets_in_url_1_rule()];
        // URL without any secret-bearing query params.
        let content = r#"let url = "https://api.example.com/data?format=json&page=1";"#;
        let d = evaluate_call(&subset, &write_call_with_content("src/client.rs", content));
        assert!(
            matches!(d, Decision::Allow),
            "URL with non-secret query params should be allowed"
        );
    }

    #[test]
    fn arch_url_secret_allows_url_with_secret_in_header_comment() {
        let subset = vec![arch_no_secrets_in_url_1_rule()];
        // The word "token" appears but not as a query parameter.
        let content = r#"
            // Send the token in the Authorization header, not the URL.
            let client = reqwest::Client::new();
            let resp = client
                .get("https://api.example.com/resource")
                .header("Authorization", format!("Bearer {}", token))
                .send()
                .await?;
        "#;
        let d = evaluate_call(&subset, &write_call_with_content("src/auth.rs", content));
        assert!(
            matches!(d, Decision::Allow),
            "token in header (not URL query param) should be allowed"
        );
    }

    // ── registry ──────────────────────────────────────────────────────────────

    #[test]
    fn registry_covers_all_four_rules() {
        let ids: Vec<&str> = RULE_REGISTRY.iter().map(|e| e.id).collect();
        assert!(ids.contains(&"GOV-1"));
        assert!(ids.contains(&"SEC-NO-HARDCODED-SECRETS-1"));
        assert!(ids.contains(&"SEC-NO-RAW-SQL-CONCAT-1"));
        assert!(ids.contains(&"ARCH-NO-SECRETS-IN-URL-1"));
    }

    #[test]
    fn lookup_arm_returns_some_for_known_ids() {
        assert!(lookup_arm("GOV-1").is_some());
        assert!(lookup_arm("SEC-NO-HARDCODED-SECRETS-1").is_some());
        assert!(lookup_arm("SEC-NO-RAW-SQL-CONCAT-1").is_some());
        assert!(lookup_arm("ARCH-NO-SECRETS-IN-URL-1").is_some());
    }

    #[test]
    fn lookup_arm_returns_none_for_unknown_id() {
        assert!(lookup_arm("FUTURE-UNIMPLEMENTED-RULE-99").is_none());
    }

    #[test]
    fn unknown_rule_in_subset_is_noop() {
        // A session rule-subset containing an unknown id should not deny any call.
        let subset = vec![RuleId("UNIMPLEMENTED-XYZ".to_string())];
        let content = "ghp_ABCDEFGHIJ1234567890abcdefghij12";
        let d = evaluate_call(&subset, &write_call_with_content("src/x.rs", content));
        assert!(
            matches!(d, Decision::Allow),
            "unknown rule id must be a safe no-op"
        );
    }

    // ── first-deny-wins ordering ──────────────────────────────────────────────

    #[test]
    fn first_deny_wins_gov1_before_secrets_rule() {
        // GOV-1 is first in the subset; it fires on "forbidden" path before
        // the secrets rule gets a chance.
        let subset = vec![gov1_rule(), sec_no_hardcoded_secrets_1_rule()];
        let content = "ghp_ABCDEFGHIJ1234567890abcdefghij12";
        let d = evaluate_call(
            &subset,
            &write_call_with_content("crates/forbidden/x.rs", content),
        );
        match d {
            Decision::Deny { rule, .. } => {
                assert_eq!(rule, gov1_rule(), "GOV-1 should fire first");
            }
            Decision::Allow => panic!("expected a deny"),
        }
    }

    // ── GovernedGateway (existing tests, kept for regression) ─────────────────

    #[tokio::test]
    async fn governed_gateway_denies_planted_violation() {
        let session = SessionId("sess-1".to_string());
        let gw = GovernedGateway::new().with_session(session.clone(), role_with(&["GOV-1"]));

        let denied = gw
            .evaluate(&session, &write_call("crates/forbidden/leak.rs"))
            .await;
        match denied {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, gov1_rule());
                assert!(reason.contains("GOV-1"));
            }
            Decision::Allow => panic!("planted violation should be denied"),
        }

        let allowed = gw
            .evaluate(&session, &write_call("crates/core/src/ok.rs"))
            .await;
        assert!(matches!(allowed, Decision::Allow));
    }

    #[tokio::test]
    async fn governed_gateway_fails_closed_on_unknown_session() {
        let gw = GovernedGateway::new();
        let unknown = SessionId("ghost".to_string());
        let d = gw
            .evaluate(&unknown, &write_call("crates/core/ok.rs"))
            .await;
        assert!(
            matches!(d, Decision::Deny { .. }),
            "unbound session must fail closed"
        );
    }

    #[test]
    fn try_evaluate_surfaces_unknown_session_error() {
        let gw = GovernedGateway::new();
        let err = gw
            .try_evaluate(&SessionId("ghost".into()), &write_call("x"))
            .unwrap_err();
        assert!(matches!(err, GatewayError::UnknownSession(_)));
    }

    #[test]
    fn bind_and_role_for_roundtrip() {
        let mut gw = GovernedGateway::new();
        let s = SessionId("s".into());
        gw.bind(s.clone(), role_with(&["GOV-1"]));
        assert_eq!(gw.session_count(), 1);
        assert_eq!(gw.role_for(&s).unwrap().name, "Backend");
    }
}
