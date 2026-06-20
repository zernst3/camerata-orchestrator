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

/// The id of the "no path escape / no writes to protected dirs" rule.
pub fn sec_no_path_escape_1_rule() -> RuleId {
    RuleId("SEC-NO-PATH-ESCAPE-1".to_string())
}

/// The id for the secret-files rule.
pub fn sec_no_secret_files_1_rule() -> RuleId {
    RuleId("SEC-NO-SECRET-FILES-1".to_string())
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
    RuleEntry {
        id: "SEC-NO-PATH-ESCAPE-1",
        description: "Deny writes whose path escapes the workspace via a `..` \
                      traversal segment, or targets version-control / SSH internals \
                      (a `.git` or `.ssh` directory component).",
        arm: arm_sec_no_path_escape_1,
    },
    RuleEntry {
        id: "SEC-NO-SECRET-FILES-1",
        description: "Deny writing a secret-bearing file by name: a real `.env` (not a \
                      template), a private-key file (.pem/.key/.p12/.pfx/id_rsa/…), or a \
                      keystore. Secrets belong in a secret manager, never the repo.",
        arm: arm_sec_no_secret_files_1,
    },
];

/// For a CONTENT rule, return the 1-based line numbers where it matches in `content`,
/// scanning the WHOLE content at once (so multi-line constructs — e.g. a `format!`
/// SQL whose keyword and interpolation are on different lines — are caught, which a
/// line-by-line scan misses). Path-based rules return empty. The brownfield audit uses
/// this for per-line findings; the write-time gate still uses the boolean arm.
pub fn content_match_lines(rule_id: &str, content: &str) -> Vec<usize> {
    let re = match rule_id {
        "SEC-NO-HARDCODED-SECRETS-1" => sec_secrets_regex(),
        "SEC-NO-RAW-SQL-CONCAT-1" => sec_sql_concat_regex(),
        "ARCH-NO-SECRETS-IN-URL-1" => arch_url_secret_regex(),
        _ => return Vec::new(),
    };
    let mut lines: Vec<usize> = re
        .find_iter(content)
        .map(|m| content[..m.start()].bytes().filter(|&b| b == b'\n').count() + 1)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Look up the arm function for `rule_id`, or `None` when the id is not
/// implemented (safe no-op).
pub fn lookup_arm(rule_id: &str) -> Option<RuleArmFn> {
    RULE_REGISTRY
        .iter()
        .find(|e| e.id == rule_id)
        .map(|e| e.arm)
}

/// Every rule that has a real enforcement arm today, as [`RuleId`]s.
///
/// This is the single source of truth for "which gate rules genuinely fire", so
/// callers that need the whole enforced set (the fleet, the live demo) ride along
/// with ALL of them instead of hand-listing a subset that silently drifts out of
/// date as arms are added. Derived from [`RULE_REGISTRY`] so adding a rule there
/// automatically propagates here.
pub fn enforced_gate_rules() -> Vec<RuleId> {
    RULE_REGISTRY
        .iter()
        .map(|e| RuleId(e.id.to_string()))
        .collect()
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

// ── SEC-NO-PATH-ESCAPE-1 ──────────────────────────────────────────────────────

/// SEC-NO-PATH-ESCAPE-1: deny writes that escape the workspace via a `..`
/// traversal segment, or that target version-control / SSH internals (a `.git`
/// or `.ssh` directory component).
///
/// Unlike GOV-1's substring guard, this matches on path *segments* (splitting on
/// both `/` and `\`), so a directory legitimately named `foo.git` or a file like
/// `notes..md` is not a false positive, while a write into an actual `.git`
/// directory or a `../` climb out of the sandbox is denied. A file-writing agent
/// has no business rewriting VCS internals, planting SSH keys/config, or climbing
/// out of its workspace; all three are unambiguous and deterministic to catch.
fn arm_sec_no_path_escape_1(path: &str, _content: &str) -> Result<(), String> {
    for segment in path.split(['/', '\\']) {
        match segment {
            ".." => {
                return Err(format!(
                    "SEC-NO-PATH-ESCAPE-1: write path contains a `..` traversal \
                     segment, which can escape the workspace (path={path})"
                ));
            }
            ".git" | ".ssh" => {
                return Err(format!(
                    "SEC-NO-PATH-ESCAPE-1: write targets a protected `{segment}` \
                     directory (version-control or SSH internals are off-limits) \
                     (path={path})"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

// ── SEC-NO-SECRET-FILES-1 ─────────────────────────────────────────────────────

/// SEC-NO-SECRET-FILES-1: deny writing a file whose NAME marks it as secret-bearing —
/// a real `.env` (not a template), a private-key file, or a keystore. Secrets belong in
/// a secret manager, never committed to the repo. Path-based and high-precision: these
/// names are secrets by convention, so the false-positive rate is near zero.
fn arm_sec_no_secret_files_1(path: &str, _content: &str) -> Result<(), String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();

    // `.env` and `.env.<env>` ARE denied, but templates (no real secrets) are allowed.
    const ENV_TEMPLATE_SUFFIXES: &[&str] =
        &["example", "sample", "template", "dist", "defaults", "tpl"];
    let is_env = lower == ".env"
        || (lower.starts_with(".env.")
            && !ENV_TEMPLATE_SUFFIXES.iter().any(|suf| lower.ends_with(suf)));

    // Private-key / keystore file extensions.
    const KEY_EXTS: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".keystore", ".jks", ".asc"];
    let is_key_ext = KEY_EXTS.iter().any(|ext| lower.ends_with(ext));

    // Conventional SSH / signing private-key filenames.
    let is_private_key_file = matches!(
        lower.as_str(),
        "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519" | ".npmrc" | ".pgpass"
    );

    if is_env || is_key_ext || is_private_key_file {
        Err(format!(
            "SEC-NO-SECRET-FILES-1: refusing to write a secret-bearing file (path={path}); \
             keep secrets out of the repo — use a secret manager and commit a `.env.example` \
             template instead"
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
            # Heuristic: a long opaque literal assigned to a SECRET-named identifier —
            # catches provider-agnostic keys (e.g. a Finnhub key) that match no known
            # prefix. The identifier carries key/secret/token/password/credential, then
            # within a short window a quoted 24+ char CONTIGUOUS-alphanumeric literal.
            # Precision guards: the literal class excludes / and - (so a file PATH or a
            # hyphenated secret-NAME like plaid-access-token-item-1 no longer matches), and
            # 24+ chars drops short env-var names. This is a heuristic, not entropy/AST; the
            # name-vs-value precision limit is the gitleaks/semgrep path (see BACKLOG).
            | ((?i:[A-Za-z0-9_]*(?:key|secret|token|password|passwd|credential)[A-Za-z0-9_]*)
               [^\n]{0,40}?
               \x22[A-Za-z0-9+_]{24,}\x22)
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
/// Fires only when **all three** co-occur within a bounded window (across lines):
///   1. A DML keyword (SELECT/INSERT/UPDATE/DELETE), **word-bounded** so substrings like
///      `Selection`/`selected` and labels like "Select page" do not match.
///   2. A confirming SQL clause (FROM/INTO/SET/VALUES/JOIN/WHERE) — the statement-shape gate
///      that distinguishes real SQL from ordinary text that happens to contain a keyword.
///   3. A format interpolation (`{}`/`{named}`) or string-concat (`" +`) — i.e. the query is
///      being BUILT by concatenation, which is the actual risk.
///
/// Requiring (2) is the precision fix: before it, the bare keyword + a nearby `{}` was enough,
/// so `"Selection: {n} row(s)"` and other rsx text critical-flagged frontend code.
///
/// Known limits / false-negative sources:
/// - Parameterised queries using `$1`/`?` placeholders instead of `{}`/`" +` are NOT caught —
///   this rule complements, not replaces, a parameterised-query lint.
/// - A query whose keyword and clause/interpolation span more than the window may be missed.
/// - Intentional raw SQL in test fixtures / migrations triggers it; exclude those via the
///   rule-subset config.
static SEC_SQL_CONCAT_REGEX: OnceLock<Regex> = OnceLock::new();

fn sec_sql_concat_regex() -> &'static Regex {
    SEC_SQL_CONCAT_REGEX.get_or_init(|| {
        // Require actual SQL-STATEMENT SHAPE, not a bare keyword. Three parts must co-occur
        // within a bounded window (across lines via the `s` dotall flag):
        //   1. a DML keyword, WORD-BOUNDED — `\b…\b` so `Selection`, `selected`, a button
        //      labelled "Select page", etc. no longer match the keyword at all;
        //   2. a CONFIRMING clause (FROM / INTO / SET / VALUES / JOIN / WHERE) — this is the
        //      "is it really SQL" gate. A UI string like "Selection: {n} row(s)" has a
        //      keyword-ish prefix but no clause, so it's rejected;
        //   3. an interpolation `{}`/`{named}` or string-concat `" +` — the rule is about
        //      BUILDING the query via concat/interpolation, not a static query string.
        // This is the precision fix for the deterministic floor: it was matching the
        // substring "Select" in ordinary rsx text and critical-flagging frontend code.
        Regex::new(
            r#"(?isx)
            \b(?:SELECT|INSERT|UPDATE|DELETE)\b   # 1. DML keyword, word-bounded
            .{0,200}?
            \b(?:FROM|INTO|SET|VALUES|JOIN|WHERE)\b   # 2. confirming SQL clause
            .{0,200}?
            (?:
                \{\w*\}         # 3a. {} or {named} format interpolation
              | "\s*\+          # 3b. closing quote followed by + (string concat)
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
            (?:
                # Case A: a literal HTTP/HTTPS URL carrying a secret param.
                https?://\S+ [?&] (?:api_?key|token|secret|password|access_token) =
                  [^\s&]+
              |
                # Case B: a query-string SHAPE even without a literal scheme — a `?`
                # query start, then a secret param, e.g. a templated URL like
                # `{base}?symbol={symbol}&token={token}`. Requires the `?` so it stays
                # URL-shaped (not any stray `&token=`).
                \? [^\s\x22]* [?&] (?:api_?key|token|secret|password|access_token) =
            )
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

    // ── deterministic content rules: the testbed plants (multi-line / format gaps) ──

    #[test]
    fn sql_concat_catches_multiline_named_format_args() {
        // The exact shape from budget-tracker-testrepo: `format!(` on one line, the
        // SELECT + `'{user_id}'` on the next — and NAMED args, not `{}`.
        let content = "        let sql = format!(\n\
            \x20            \"SELECT category_id, SUM(amount) AS spent \\\n\
            \x20             FROM transactions \\\n\
            \x20             WHERE user_id = '{user_id}' \\\n\
            \x20               AND EXTRACT(YEAR FROM date) = {year}\",\n\
            \x20            user_id = user_id.value(),\n        );";
        let lines = content_match_lines("SEC-NO-RAW-SQL-CONCAT-1", content);
        assert!(
            !lines.is_empty(),
            "multi-line named-arg SQL format! must be caught"
        );
    }

    #[test]
    fn secrets_catches_bare_key_assigned_to_secret_named_const() {
        // A provider-agnostic key (no ghp_/sk-/AKIA prefix) assigned to a *_KEY const.
        let content = "const FALLBACK_FINNHUB_KEY: &str = \"c8r9v2aad3i9q1m4f7g0bv8s5p2qk1n7\";";
        let d = arm_sec_no_hardcoded_secrets_1("", content);
        assert!(
            d.is_err(),
            "a long opaque literal on a *_KEY const must be flagged"
        );
    }

    #[test]
    fn sql_concat_precision_guards() {
        // A Dioxus `select {}` (no opening string quote) must NOT match.
        assert!(content_match_lines("SEC-NO-RAW-SQL-CONCAT-1", "rsx! { select {} }").is_empty());
        // A SQL keyword as an identifier/method (no quote) must NOT match.
        assert!(content_match_lines(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "let selected = items.select(|x| x);"
        )
        .is_empty());
        // The real plant (full statement shape + interpolation) STILL matches.
        assert!(!content_match_lines(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "format!(\"SELECT x WHERE id = {id}\")"
        )
        .is_empty());

        // Regression: bare "Select"/"Selection" in rsx text must NOT match (the dogfooding
        // false positives on rust-chorale, a frontend lib with zero SQL).
        for s in [
            r#"rsx! { "Selection: {count} row(s)" }"#, // "Selection" + interpolation, no SQL clause
            r#"button { "Select page" }"#,             // a button label
            r#"h1 { "Selection example" }"#,           // a heading
            r#"let selected = view.get(); rsx!{ "{selected} chosen" }"#, // keyword-ish + interp, no clause
        ] {
            assert!(
                content_match_lines("SEC-NO-RAW-SQL-CONCAT-1", s).is_empty(),
                "UI text must not match the SQL-concat floor: {s}"
            );
        }
        // A genuine concat-built UPDATE still matches (DML + SET + interpolation).
        assert!(!content_match_lines(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "format!(\"UPDATE users SET name = '{name}' WHERE id = {id}\")"
        )
        .is_empty());
    }

    #[test]
    fn secrets_precision_guards_paths_and_hyphenated_names() {
        // A file PATH literal on a token-named var (has `/`) must NOT match.
        assert!(arm_sec_no_hardcoded_secrets_1(
            "",
            "let token_path = \"src/some/very/long/path.rs\";"
        )
        .is_ok());
        // A hyphenated secret NAME (a reference, not a value) must NOT match.
        assert!(
            arm_sec_no_hardcoded_secrets_1("", "let k = \"plaid-access-token-item-1\";").is_ok()
        );
        // The real bare key (24+ contiguous alphanumeric) STILL matches.
        assert!(arm_sec_no_hardcoded_secrets_1(
            "",
            "const FINNHUB_KEY: &str = \"c8r9v2aad3i9q1m4f7g0bv8s5p2qk1n7\";"
        )
        .is_err());
    }

    #[test]
    fn secrets_does_not_flag_short_or_namelike_constants() {
        // A header NAME / env-var NAME on a secret-ish const is not a secret VALUE.
        assert!(arm_sec_no_hardcoded_secrets_1(
            "",
            "const TOKEN_HEADER: &str = \"X-Finnhub-Token\";"
        )
        .is_ok());
        assert!(
            arm_sec_no_hardcoded_secrets_1("", "const API_KEY_ENV: &str = \"FINNHUB_KEY\";")
                .is_ok()
        );
    }

    #[test]
    fn secret_in_url_catches_templated_query_without_scheme() {
        // The plant: a format string with no literal http(s):// but a `?…&token={…}`.
        let content = "format!(\"{base}?symbol={symbol}&token={token}\")";
        let d = arm_arch_no_secrets_in_url_1("", content);
        assert!(
            d.is_err(),
            "a templated URL query with a token param must be flagged"
        );
        // And the literal-scheme case still works.
        assert!(arm_arch_no_secrets_in_url_1("", "https://api.x.com/q?api_key=abc123").is_err());
        // A bare `&token=` with no query start is NOT flagged (avoids form-body FPs).
        assert!(arm_arch_no_secrets_in_url_1("", "let body = \"&token=\".to_string();").is_ok());
    }

    // ── SEC-NO-SECRET-FILES-1 ────────────────────────────────────────────────

    #[test]
    fn secret_files_denies_env_keys_and_keystores() {
        let subset = vec![sec_no_secret_files_1_rule()];
        for p in [
            ".env",
            ".env.production",
            ".env.local",
            "config/.env",
            "certs/server.pem",
            "deploy/tls.key",
            "secrets/app.p12",
            "keystore.jks",
            ".ssh/id_rsa",
            "id_ed25519",
            ".npmrc",
        ] {
            assert!(
                matches!(
                    evaluate_call(&subset, &write_call(p)),
                    Decision::Deny { .. }
                ),
                "expected DENY for {p}"
            );
        }
    }

    #[test]
    fn secret_files_allows_templates_and_normal_code() {
        let subset = vec![sec_no_secret_files_1_rule()];
        for p in [
            ".env.example",
            ".env.sample",
            ".env.template",
            "crates/api/src/config.rs",
            "src/keys.rs", // a source file named keys — not a key file
            "docs/env.md",
        ] {
            assert!(
                matches!(evaluate_call(&subset, &write_call(p)), Decision::Allow),
                "expected ALLOW for {p}"
            );
        }
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

    // ── SEC-NO-PATH-ESCAPE-1 ──────────────────────────────────────────────────

    #[test]
    fn path_escape_denies_dotdot_traversal() {
        let subset = vec![sec_no_path_escape_1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/../../etc/passwd"));
        match d {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, sec_no_path_escape_1_rule());
                assert!(reason.contains("traversal"), "reason was: {reason}");
            }
            Decision::Allow => panic!("expected SEC-NO-PATH-ESCAPE-1 deny for `..` traversal"),
        }
    }

    #[test]
    fn path_escape_denies_git_internals() {
        let subset = vec![sec_no_path_escape_1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/core/.git/config"));
        match d {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, sec_no_path_escape_1_rule());
                assert!(reason.contains(".git"), "reason was: {reason}");
            }
            Decision::Allow => panic!("expected deny for a write into .git/"),
        }
    }

    #[test]
    fn path_escape_denies_ssh_internals() {
        let subset = vec![sec_no_path_escape_1_rule()];
        let d = evaluate_call(&subset, &write_call(".ssh/authorized_keys"));
        assert!(
            matches!(d, Decision::Deny { .. }),
            "expected deny for a write into .ssh/"
        );
    }

    #[test]
    fn path_escape_allows_clean_path() {
        let subset = vec![sec_no_path_escape_1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/core/src/lib.rs"));
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn path_escape_does_not_false_positive_on_segment_substring() {
        // A directory literally named `foo.git` (or a `..`-containing filename)
        // is NOT a `.git` directory component / traversal segment, so it must be
        // allowed. This is the case GOV-1's substring matching would get wrong.
        let subset = vec![sec_no_path_escape_1_rule()];
        assert!(matches!(
            evaluate_call(&subset, &write_call("mirrors/foo.git/readme.md")),
            Decision::Allow
        ));
        assert!(matches!(
            evaluate_call(&subset, &write_call("notes/release..md")),
            Decision::Allow
        ));
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
    fn registry_covers_all_enforced_rules() {
        let ids: Vec<&str> = RULE_REGISTRY.iter().map(|e| e.id).collect();
        assert!(ids.contains(&"GOV-1"));
        assert!(ids.contains(&"SEC-NO-HARDCODED-SECRETS-1"));
        assert!(ids.contains(&"SEC-NO-RAW-SQL-CONCAT-1"));
        assert!(ids.contains(&"ARCH-NO-SECRETS-IN-URL-1"));
        assert!(ids.contains(&"SEC-NO-PATH-ESCAPE-1"));
    }

    #[test]
    fn lookup_arm_returns_some_for_known_ids() {
        assert!(lookup_arm("GOV-1").is_some());
        assert!(lookup_arm("SEC-NO-HARDCODED-SECRETS-1").is_some());
        assert!(lookup_arm("SEC-NO-RAW-SQL-CONCAT-1").is_some());
        assert!(lookup_arm("ARCH-NO-SECRETS-IN-URL-1").is_some());
        assert!(lookup_arm("SEC-NO-PATH-ESCAPE-1").is_some());
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
