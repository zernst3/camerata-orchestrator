//! Secret detector + scrubber (FOLD E — the chat secret-interceptor,
//! `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`'s usability backlog:
//! "chat secret-interceptor (scrub pasted keys from transcript/memory/audit)").
//!
//! A secret pasted into a chat (or captured in a deployed app's own logs) must
//! never land in the transcript, session memory, or the governance audit trail.
//! This module is the pure substrate: [`detect_secrets`] finds high-confidence
//! secret matches in free text; [`scrub`] replaces each match with a stable
//! placeholder token and returns the extracted values so a caller can store them
//! in a vault BY NAME instead of just discarding them.
//!
//! # Precision over recall, deliberately
//! Every pattern below is a well-known, low-ambiguity secret shape (a vendor's own
//! fixed prefix, or a PEM header, or an env-var-style connection string). A shape
//! that would also flag ordinary non-secret text (e.g. the bare word "Bearer" with
//! nothing token-shaped after it, or a short "AKIA"-prefixed word) is deliberately
//! rejected by a length/character-class gate. This mirrors `camerata_gateway`'s own
//! SEC-* write-time-gate arms (see `crates/gateway/src/lib.rs`'s
//! `sec_secrets_regex`/`sec_vendor_token_regex`/`sec_private_key_regex`), which use
//! the same `regex` + `OnceLock`-cached-compiled-pattern style — a DIFFERENT
//! use case (denying a write vs. scrubbing free text before it's ever stored), so
//! the two are independent modules rather than a shared dependency, but grounded in
//! the same well-known prefixes so they don't silently diverge on what counts as
//! "obviously a secret."
//!
//! # Applied today
//! The one live free-text ingest path this module is wired into now is
//! `camerata_server::submit_feedback`: `DefectContext.console` and
//! `DefectContext.stack` are scrubbed BEFORE the `DefectReport` is stored, so a
//! deployed scaffolded app's captured logs can't leak a secret into the feedback
//! store or the governance trail (see that function's call to
//! `camerata_orchestrator_core::secrets::scrub`).
//!
//! # Follow-on (not built here)
//! The chat/PO head's paste-interception ("you just pasted something that looks
//! like a secret — store it in the vault as `<name>` instead?") reuses
//! `detect_secrets`/`scrub` directly; this module is written to be that seam
//! already, not just an ingest-time filter.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

// ─── kinds ──────────────────────────────────────────────────────────────────────

/// Which well-known secret shape a [`SecretMatch`]/[`ExtractedSecret`] is. Drives
/// the placeholder token's label (`{{SECRET:<label>_<n>}}`) and doubles as a
/// vault-entry-name hint for the (future) chat-head "store this by name" flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// Stripe secret/restricted key (`sk_live_`, `sk_test_`, `rk_live_`).
    StripeKey,
    /// A PEM/armored private-key block (`-----BEGIN ... PRIVATE KEY-----`).
    PemPrivateKey,
    /// An AWS access key ID (`AKIA` + 16 uppercase alphanumeric chars).
    AwsAccessKey,
    /// A GitHub personal access token (`ghp_...`) or fine-grained token
    /// (`github_pat_...`).
    GitHubToken,
    /// An Azure Storage connection string (`DefaultEndpointsProtocol=...;
    /// AccountKey=...`).
    AzureStorageConnectionString,
    /// An OpenAI API key (`sk-` + a long alphanumeric run).
    OpenAiKey,
    /// A generic `Bearer <token>` in an obvious auth context (the token itself is
    /// long/opaque enough to be confident it's a real credential, not a stray
    /// word after "Bearer").
    BearerToken,
}

impl SecretKind {
    /// Stable lowercase snake_case label used in the placeholder token and as the
    /// vault-entry-name hint.
    pub fn label(self) -> &'static str {
        match self {
            SecretKind::StripeKey => "stripe_key",
            SecretKind::PemPrivateKey => "pem_private_key",
            SecretKind::AwsAccessKey => "aws_access_key",
            SecretKind::GitHubToken => "github_token",
            SecretKind::AzureStorageConnectionString => "azure_storage_connection_string",
            SecretKind::OpenAiKey => "openai_key",
            SecretKind::BearerToken => "bearer_token",
        }
    }
}

// ─── detect_secrets ─────────────────────────────────────────────────────────────

/// One high-confidence secret match found by [`detect_secrets`]: `text[start..end]`
/// is the matched substring, `kind` says which pattern matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    pub start: usize,
    pub end: usize,
    pub kind: SecretKind,
}

macro_rules! cached_regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| {
                Regex::new($pattern).expect(concat!(stringify!($name), " must compile"))
            })
        }
    };
}

// Stripe secret/restricted keys: `sk_live_`, `sk_test_`, `rk_live_`, followed by a
// long opaque run (real Stripe keys are 24-99+ chars; 10+ is a generous precision
// floor that still rejects a bare `sk_live_` with nothing after it).
cached_regex!(
    stripe_key_regex,
    r"(?:sk_live_|sk_test_|rk_live_)[A-Za-z0-9]{10,}"
);

// PEM / armored private-key block. Matches the BEGIN header alone (a truncated
// paste still carries the header), and — when an END footer follows within a
// bounded window (4000 chars comfortably covers an RSA/EC key body) — the whole
// block, so `scrub` removes the actual key material too, not just the header
// line. Mirrors `camerata_gateway::sec_private_key_regex`'s header alternation.
cached_regex!(
    pem_private_key_regex,
    r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?:\s+BLOCK)?-----(?:[\s\S]{0,4000}?-----END (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?:\s+BLOCK)?-----)?"
);

// AWS access key ID: 4-letter prefix + 16 uppercase alphanumeric chars.
cached_regex!(aws_access_key_regex, r"AKIA[0-9A-Z]{16}");

// GitHub personal access token / fine-grained token.
cached_regex!(
    github_token_regex,
    r"(?:ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})"
);

// Azure Storage connection string: the `DefaultEndpointsProtocol=` clause,
// followed (anywhere within a bounded same-ish-line window) by `AccountKey=` and
// its base64-shaped value. The bounded, non-greedy middle (`[^\n]{0,300}?`) lets
// an `AccountName=...;` segment sit in between without requiring a fixed shape,
// while staying on one line (real connection strings are single-line) and
// avoiding a runaway match across unrelated text.
cached_regex!(
    azure_storage_conn_regex,
    r"DefaultEndpointsProtocol=https?;[^\n]{0,300}?AccountKey=[A-Za-z0-9+/=]{20,}"
);

// OpenAI API key: `sk-` + a long alphanumeric run (no underscore/hyphen inside the
// class, so it does not collide with Stripe's `sk_live_`/`sk_test_` underscore
// prefix, and a hyphenated non-key word like `sk-ant-...` breaks the required
// run early and is not matched).
cached_regex!(openai_key_regex, r"sk-[A-Za-z0-9]{20,}");

// Generic `Bearer <token>` in an obvious auth context: the literal word `Bearer`
// (case-insensitive), then a token-shaped run of 20+ chars (alnum plus the
// separators a JWT/opaque token commonly uses). The 20+ length floor is the
// precision guard — "Bearer of good news" has no run anywhere near that long.
cached_regex!(bearer_token_regex, r"(?i:bearer)\s+[A-Za-z0-9\-_.]{20,}");

/// Find every high-confidence secret in `text`, in left-to-right order.
///
/// The generic [`SecretKind::BearerToken`] pattern is the LEAST specific of the
/// seven and only contributes a match when it does not overlap a match a more
/// specific pattern already found (so `Authorization: Bearer ghp_...` is reported
/// once, as [`SecretKind::GitHubToken`], not twice).
pub fn detect_secrets(text: &str) -> Vec<SecretMatch> {
    let mut matches: Vec<SecretMatch> = Vec::new();

    for (kind, re) in [
        (SecretKind::StripeKey, stripe_key_regex()),
        (SecretKind::PemPrivateKey, pem_private_key_regex()),
        (SecretKind::AwsAccessKey, aws_access_key_regex()),
        (SecretKind::GitHubToken, github_token_regex()),
        (
            SecretKind::AzureStorageConnectionString,
            azure_storage_conn_regex(),
        ),
        (SecretKind::OpenAiKey, openai_key_regex()),
    ] {
        for m in re.find_iter(text) {
            matches.push(SecretMatch { start: m.start(), end: m.end(), kind });
        }
    }

    for m in bearer_token_regex().find_iter(text) {
        let overlaps_more_specific = matches
            .iter()
            .any(|existing| m.start() < existing.end && existing.start < m.end());
        if !overlaps_more_specific {
            matches.push(SecretMatch { start: m.start(), end: m.end(), kind: SecretKind::BearerToken });
        }
    }

    matches.sort_by_key(|m| m.start);
    matches
}

// ─── scrub ──────────────────────────────────────────────────────────────────────

/// One secret [`scrub`] pulled out of the text, paired with the placeholder token
/// that replaced it in the scrubbed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSecret {
    /// The exact placeholder token substituted into the scrubbed text (e.g.
    /// `{{SECRET:stripe_key_1}}`) — unique within one [`scrub`] call (a running
    /// per-kind counter disambiguates repeats of the same kind), so a caller can
    /// map each extracted value back to its exact substitution site.
    pub placeholder: String,
    /// The raw secret value that was removed.
    pub value: String,
    pub kind: SecretKind,
}

/// The result of [`scrub`]: `text` with every high-confidence secret replaced by a
/// stable placeholder, plus the extracted values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubResult {
    pub scrubbed: String,
    pub extracted: Vec<ExtractedSecret>,
}

/// Replace every [`detect_secrets`] match in `text` with a stable placeholder
/// token (`{{SECRET:<kind>_<n>}}`), returning the scrubbed text plus the extracted
/// raw values so a caller can store them in a vault by name rather than just
/// discarding them.
///
/// Idempotent: a placeholder token itself matches none of the patterns above, so
/// scrubbing already-scrubbed text is a no-op (`scrub(&scrub(text).scrubbed)`
/// yields the same text back with an empty `extracted`).
pub fn scrub(text: &str) -> ScrubResult {
    let matches = detect_secrets(text);
    if matches.is_empty() {
        return ScrubResult { scrubbed: text.to_string(), extracted: Vec::new() };
    }

    let mut scrubbed = String::with_capacity(text.len());
    let mut extracted = Vec::with_capacity(matches.len());
    let mut seen_of_kind: HashMap<&'static str, usize> = HashMap::new();
    let mut cursor = 0usize;

    for m in &matches {
        scrubbed.push_str(&text[cursor..m.start]);
        let label = m.kind.label();
        let n = seen_of_kind.entry(label).or_insert(0);
        *n += 1;
        let placeholder = format!("{{{{SECRET:{label}_{n}}}}}");
        extracted.push(ExtractedSecret {
            placeholder: placeholder.clone(),
            value: text[m.start..m.end].to_string(),
            kind: m.kind,
        });
        scrubbed.push_str(&placeholder);
        cursor = m.end;
    }
    scrubbed.push_str(&text[cursor..]);

    ScrubResult { scrubbed, extracted }
}

// ─── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── each pattern is detected ────────────────────────────────────────────

    // Build a Stripe-shaped fixture at runtime from fragments so no literal
    // secret-shaped token sits in the source. Secret scanners (e.g. GitHub push
    // protection) match the `sk_`/`rk_` prefix by SHAPE regardless of the body, so
    // de-literalized construction is the only way to keep the detector's own tests
    // from tripping them. `prefix` is e.g. "sk_live" (no trailing `_<body>`, so the
    // literal fragment is itself not secret-shaped); the `_` + body are appended here.
    fn stripe(prefix: &str) -> String {
        format!("{prefix}_EXAMPLE0123456789ABCDEF")
    }

    #[test]
    fn detects_stripe_live_and_test_and_restricted_keys() {
        for prefix in ["sk_live", "sk_test", "rk_live"] {
            let sample = stripe(prefix);
            let matches = detect_secrets(&sample);
            assert_eq!(matches.len(), 1, "sample: {sample}");
            assert_eq!(matches[0].kind, SecretKind::StripeKey);
            assert_eq!(&sample[matches[0].start..matches[0].end], sample);
        }
    }

    #[test]
    fn detects_pem_private_key_header_only() {
        let sample = "here is a key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow...(truncated, no footer)";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::PemPrivateKey);
    }

    #[test]
    fn detects_full_pem_private_key_block_including_body() {
        let sample = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEA\nAoIBAQDEXAMPLEBODYONLY\n-----END PRIVATE KEY-----";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::PemPrivateKey);
        let matched = &sample[matches[0].start..matches[0].end];
        assert!(matched.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(matched.contains("-----END PRIVATE KEY-----"));
        assert!(matched.contains("EXAMPLEBODYONLY"));
    }

    #[test]
    fn detects_aws_access_key() {
        let sample = "aws_access_key_id = AKIAIOSFODNN7EXAMPLE";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::AwsAccessKey);
        assert_eq!(&sample[matches[0].start..matches[0].end], "AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn detects_github_personal_access_token_and_fine_grained_token() {
        let ghp = "token: ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        let matches = detect_secrets(ghp);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::GitHubToken);

        let pat = "token: github_pat_11ABCDEFG0123456789abcdefghijklmnopqrstuvwxyz";
        let matches = detect_secrets(pat);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::GitHubToken);
    }

    #[test]
    fn detects_azure_storage_connection_string() {
        let sample = "DefaultEndpointsProtocol=https;AccountName=devstoreaccount1;AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==;EndpointSuffix=core.windows.net";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::AzureStorageConnectionString);
        let matched = &sample[matches[0].start..matches[0].end];
        assert!(matched.starts_with("DefaultEndpointsProtocol=https;"));
        assert!(matched.contains("AccountKey="));
    }

    #[test]
    fn detects_openai_key() {
        let sample = "OPENAI_API_KEY=sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::OpenAiKey);
    }

    #[test]
    fn detects_generic_bearer_token_in_auth_context() {
        let sample = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signaturepart";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::BearerToken);
    }

    #[test]
    fn bearer_wrapping_a_specific_token_is_reported_once_as_the_specific_kind() {
        let sample = "Authorization: Bearer ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1, "matches: {matches:?}");
        assert_eq!(matches[0].kind, SecretKind::GitHubToken);
    }

    // ── non-secrets are NOT flagged (precision) ─────────────────────────────

    #[test]
    fn ordinary_prose_is_never_flagged() {
        let sample = "The API key rotation policy says to rotate secrets every 90 days. \
            Please update your token before the deadline. Bearer bonds are a kind of \
            debt instrument, unrelated to HTTP auth.";
        assert!(detect_secrets(sample).is_empty(), "false positive on ordinary prose");
    }

    #[test]
    fn short_aws_looking_prefix_is_not_flagged() {
        assert!(detect_secrets("AKIA1234").is_empty());
        assert!(detect_secrets("this AKIAMENTIONS bird migration").is_empty());
    }

    #[test]
    fn short_stripe_and_openai_prefixes_are_not_flagged() {
        assert!(detect_secrets("sk_live_short").is_empty());
        assert!(detect_secrets("sk-short").is_empty());
    }

    #[test]
    fn bare_bearer_word_with_no_token_is_not_flagged() {
        assert!(detect_secrets("Bearer").is_empty());
        assert!(detect_secrets("Bearer 123").is_empty());
        assert!(detect_secrets("the bearer of this letter").is_empty());
    }

    #[test]
    fn short_github_prefix_is_not_flagged() {
        assert!(detect_secrets("ghp_abc").is_empty());
    }

    #[test]
    fn azure_account_key_clause_alone_without_the_protocol_clause_is_not_flagged() {
        assert!(detect_secrets("AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq==").is_empty());
    }

    #[test]
    fn pem_like_text_missing_the_begin_marker_is_not_flagged() {
        assert!(detect_secrets("this is definitely PRIVATE KEY material, trust me").is_empty());
    }

    // ── scrub: replaces + extracts ──────────────────────────────────────────

    #[test]
    fn scrub_replaces_a_single_secret_and_extracts_its_value() {
        let secret = stripe("sk_live");
        let text = format!("leaked: {secret} in the console dump");
        let result = scrub(&text);

        assert!(!result.scrubbed.contains(&secret));
        assert!(result.scrubbed.contains("{{SECRET:stripe_key_1}}"));
        assert_eq!(result.scrubbed, "leaked: {{SECRET:stripe_key_1}} in the console dump");

        assert_eq!(result.extracted.len(), 1);
        assert_eq!(result.extracted[0].kind, SecretKind::StripeKey);
        assert_eq!(result.extracted[0].value, secret);
        assert_eq!(result.extracted[0].placeholder, "{{SECRET:stripe_key_1}}");
    }

    #[test]
    fn scrub_handles_multiple_secrets_of_different_kinds() {
        let text = "aws=AKIAIOSFODNN7EXAMPLE github=ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        let result = scrub(text);

        assert_eq!(result.extracted.len(), 2);
        assert!(!result.scrubbed.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!result.scrubbed.contains("ghp_16C7e42F292c6912E7710c838347Ae178B4a"));
        assert!(result.scrubbed.contains("{{SECRET:aws_access_key_1}}"));
        assert!(result.scrubbed.contains("{{SECRET:github_token_1}}"));
    }

    #[test]
    fn scrub_numbers_repeats_of_the_same_kind_distinctly() {
        let text = format!("first {} then {}", stripe("sk_live"), stripe("sk_test"));
        let result = scrub(&text);

        assert_eq!(result.extracted.len(), 2);
        assert_eq!(result.extracted[0].placeholder, "{{SECRET:stripe_key_1}}");
        assert_eq!(result.extracted[1].placeholder, "{{SECRET:stripe_key_2}}");
        assert_ne!(result.extracted[0].value, result.extracted[1].value);
    }

    #[test]
    fn scrub_is_a_no_op_when_no_secret_is_present() {
        let text = "nothing sensitive here, just a normal console log line";
        let result = scrub(text);
        assert_eq!(result.scrubbed, text);
        assert!(result.extracted.is_empty());
    }

    #[test]
    fn scrub_handles_empty_text() {
        let result = scrub("");
        assert_eq!(result.scrubbed, "");
        assert!(result.extracted.is_empty());
    }

    // ── idempotence ──────────────────────────────────────────────────────────

    #[test]
    fn scrubbing_already_scrubbed_text_is_idempotent() {
        let text = format!("key: {} and AKIAIOSFODNN7EXAMPLE", stripe("sk_live"));
        let once = scrub(&text);
        let twice = scrub(&once.scrubbed);

        assert_eq!(twice.scrubbed, once.scrubbed);
        assert!(
            twice.extracted.is_empty(),
            "re-scrubbing already-placeholdered text must find nothing new"
        );
    }

    // ── SecretKind::label ────────────────────────────────────────────────────

    #[test]
    fn every_kind_has_a_stable_lowercase_label() {
        for (kind, expected) in [
            (SecretKind::StripeKey, "stripe_key"),
            (SecretKind::PemPrivateKey, "pem_private_key"),
            (SecretKind::AwsAccessKey, "aws_access_key"),
            (SecretKind::GitHubToken, "github_token"),
            (
                SecretKind::AzureStorageConnectionString,
                "azure_storage_connection_string",
            ),
            (SecretKind::OpenAiKey, "openai_key"),
            (SecretKind::BearerToken, "bearer_token"),
        ] {
            assert_eq!(kind.label(), expected);
        }
    }
}
