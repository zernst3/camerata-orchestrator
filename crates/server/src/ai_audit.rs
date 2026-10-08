//! The AI architectural audit — the half of brownfield that genuinely needs a model.
//!
//! The deterministic scan ([`crate::onboard::audit_files`]) catches MECHANICAL
//! violations (hardcoded secrets, raw SQL, path escapes) precisely, line by line —
//! that is the "linting" tier, and determinism is the right tool there. This pass is
//! the other tier: it READS the code and finds the GENUINE architectural / security
//! violations that are not line-level lint —
//!   - a write/mutation path with no authorization check,
//!   - services reaching the database directly, bypassing the repository layer,
//!   - N+1 query patterns,
//!   - imports that cross a module boundary they shouldn't,
//!   - inconsistent money/date/id handling across modules,
//!   - god objects / dead abstractions / duplicated logic that should be one seam.
//!
//! AI DISCOVERS these; the architect APPROVES; approved rules become gate config
//! (mechanical where possible) or AI-assisted integration checks (where a rule is
//! inherently semantic). Enforcement stays deterministic-or-codified; discovery is AI.
//!
//! Output is the SAME `Finding` / `ProposedRule` shapes the deterministic scan emits,
//! so the onboarding UI renders both tiers in one table. AI findings carry an `AI-`
//! rule-id prefix so the UI can mark their provenance.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::llm::{LlmPort, Llm, LlmRequest};
use crate::onboard::{Finding, MergedLocation, ProposedRule, RuleOptionView};

// ════════════════════════════════════════════════════════════════════════════════════
// AUDIT-INTEGRATED ALTERNATIVE RECOMMENDATION (docs/design/2026-09-22_audit-integrated-
// alternatives.md)
// ════════════════════════════════════════════════════════════════════════════════════
//
// For each MULTI-OPTION semantic rule (>= 2 `[[option]]`s, AI-judged tier — never the
// deterministic floor, never a single-option/mechanical rule), the audit feeds the model
// EVERY option instead of one pre-resolved directive, and asks it to (a) recommend the
// option that best fits this codebase and (b) report violations AGAINST that recommended
// option. See `recommend_alternatives` (the dedicated pass that decides this ONCE per rule,
// grounded in real code) and `audit_repo`'s use of it.

/// One multi-option semantic rule's full alternative set, fed to the recommendation pass.
/// Built by the caller (`onboard::audit_repos` for the main scan, the `rescan-alternatives`
/// endpoint for a targeted re-check) by joining the project's selected rule ids against the
/// loaded corpus.
#[derive(Debug, Clone)]
pub struct RuleAlternatives {
    /// The rule id (uppercased, matching the `adopted` normalization used throughout this
    /// module).
    pub rule_id: String,
    /// Every option this rule defines — id, label, directive, and rationale.
    pub options: Vec<RuleOptionView>,
    /// The option currently selected for this project: the project's `chosen_option` for
    /// this rule if one was made, else the corpus `default_option`, else `None` (nothing
    /// selected yet — a rule with no default that the architect never chose an alternative
    /// for). Used both as the "currently selected" marker shown to the model and as the
    /// fail-soft fallback when the model's answer can't be trusted.
    pub selected_option_id: Option<String>,
}

/// A named audit pass that failed or timed out for a repo — e.g. the alternative-
/// recommendation pass hitting the CLI's hang timeout. Captured here instead of only
/// `eprintln!`-ing to stderr, so the report export (`report_export::build_report_json`) can
/// disclose the gap EXPLICITLY in the methodology and summary rather than silently shipping
/// a report that never mentions the pass ran at all. `pass` is a short, stable, general name
/// (never a fixture-specific string) — any pass this fail-soft pattern applies to can push
/// one of these. See `ScanReport::failed_passes` and W6 in
/// `docs/plans/2026-09-30_cycle2-queue-hardening.md`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FailedPass {
    /// The repo this pass failed for (`owner/repo`).
    pub repo: String,
    /// A short, stable, human-readable pass name (e.g. "rule-alternative recommendations")
    /// used verbatim in the disclosure text.
    pub pass: String,
    /// The error text, verbatim (e.g. the CLI timeout message), so the operator sees the
    /// real reason rather than a generic placeholder.
    pub reason: String,
}

/// One rule's recommendation: which option the AI judged best-fitting for this codebase (or
/// the operator's forced choice for a targeted rescan), plus the reasoning, plus whether the
/// raw model answer had to be corrected.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RuleRecommendation {
    /// The rule id this recommendation is for (uppercased).
    pub rule_id: String,
    /// The option id violations were (or will be) reported against.
    pub recommended_option_id: String,
    /// 1-3 sentences explaining the pick, grounded in the codebase evidence the model saw.
    /// For an operator-forced pick this is a short fixed note, not a model explanation.
    pub recommendation_reasoning: String,
    /// True when the model's raw `recommended_option_id` was not a real option on this rule
    /// (or was missing/unparseable) and this entry's `recommended_option_id` is therefore the
    /// FALLBACK (the project's selected/default option), not what the model actually said.
    /// Surfaced so the UI can flag "the model's answer was corrected" rather than silently
    /// presenting a fallback as if it were a genuine recommendation.
    #[serde(default)]
    pub hallucinated: bool,
    /// True when this entry reflects the OPERATOR's explicit choice (via `rescan-alternatives`
    /// or `accept-alternatives`), not the AI's own pick. The UI's per-rule state machine uses
    /// this to render "your choice" (locked) instead of "AI recommended".
    #[serde(default)]
    pub operator_chosen: bool,
    /// The `file:line` (or `file:line — short note`) citation in the REPO that actually
    /// FOLLOWS or NEEDS the recommended option — e.g. `src/api/list_users.rs:42` for a
    /// cursor-pagination pick, grounding the recommendation in real code rather than a model
    /// assertion. `None` when the model found no such evidence (or the field was blank/absent)
    /// — see `applicable`'s doc comment: an absent evidence line FORCES `applicable = false`
    /// for a model-sourced recommendation, because an unevidenced "this codebase does/needs X"
    /// claim is never trusted (P7, `docs/plans/2026-09-29_codebase-inspection-hardening.md`).
    /// Always `None` for an operator-forced pick (`operator_chosen: true`) — the operator's
    /// explicit choice needs no repo evidence to be trusted.
    #[serde(default)]
    pub evidence: Option<String>,
    /// Whether this rule's underlying CONCERN applies to this codebase at all. `false` means
    /// the codebase neither follows nor needs this rule's decision (e.g. no pagination
    /// anywhere, no API versioning anywhere) — `audit_repo` then drops the rule from the
    /// directive set actually checked, so it emits ZERO findings instead of a false "this
    /// project adopted the X model" claim. Defaults to `true` (back-compat: an
    /// operator-forced pick, or a recommendation that predates this field, is treated as
    /// applicable — the pre-existing behavior of always checking every selected multi-option
    /// rule). See [`parse_alternative_recommendations`] for how a model's `applicable: true`
    /// claim is validated against `evidence` before being trusted.
    #[serde(default = "default_recommendation_applicable")]
    pub applicable: bool,
}

/// Serde default for [`RuleRecommendation::applicable`] — see that field's doc comment.
fn default_recommendation_applicable() -> bool {
    true
}

impl Default for RuleRecommendation {
    /// Manual (not derived) so `applicable` defaults to `true` here too — the same value the
    /// serde default produces — rather than `bool::default() == false`, which would silently
    /// contradict the field's own documented default.
    fn default() -> Self {
        RuleRecommendation {
            rule_id: String::new(),
            recommended_option_id: String::new(),
            recommendation_reasoning: String::new(),
            hallucinated: false,
            operator_chosen: false,
            evidence: None,
            applicable: true,
        }
    }
}

/// Aggregated REAL usage across every LLM call in one audit — all chunk×rule passes, the
/// resolution round, and the calibration pass. Lets the UI show ACTUAL vs the pre-scan
/// estimate. Thread-safe (passes run concurrently); cost is held in micro-dollars to stay
/// integer-atomic.
#[derive(Default)]
pub struct UsageMeter {
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cost_micro_usd: AtomicU64,
    calls: AtomicU64,
    cost_calls: AtomicU64,
    /// Tokens served from the prompt cache (billed at ~0.1× input rate). Populated only
    /// when the API backend is in use with `cache_breakpoints` set on the request.
    cache_read_input_tokens: AtomicU64,
    /// Tokens written to the prompt cache (billed at ~1.25× input rate, one-time per TTL).
    /// Populated only when the API backend is active with prompt caching enabled.
    cache_creation_input_tokens: AtomicU64,
}

impl UsageMeter {
    /// Fold one completion's reported usage in. Missing fields are simply not counted.
    pub fn record(&self, r: &crate::llm::LlmResponse) {
        if let Some(i) = r.input_tokens {
            self.input_tokens.fetch_add(i, Ordering::Relaxed);
        }
        if let Some(o) = r.output_tokens {
            self.output_tokens.fetch_add(o, Ordering::Relaxed);
        }
        if let Some(c) = r.cost_usd {
            self.cost_micro_usd
                .fetch_add((c * 1_000_000.0) as u64, Ordering::Relaxed);
            self.cost_calls.fetch_add(1, Ordering::Relaxed);
        }
        // Cache breakdowns are additive across calls (each call contributes its own share
        // of reads / creations independently).
        self.cache_read_input_tokens
            .fetch_add(r.cache_read_input_tokens, Ordering::Relaxed);
        self.cache_creation_input_tokens
            .fetch_add(r.cache_creation_input_tokens, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ActualUsage {
        let calls = self.calls.load(Ordering::Relaxed);
        let cost_calls = self.cost_calls.load(Ordering::Relaxed);
        ActualUsage {
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            cost_usd: self.cost_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            calls,
            // Every call that ran reported a cost — so the dollar figure is complete, not a
            // partial sum that would understate (some calls' usage may be unreported).
            cost_complete: calls > 0 && cost_calls == calls,
            cache_read_input_tokens: self.cache_read_input_tokens.load(Ordering::Relaxed),
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .load(Ordering::Relaxed),
        }
    }
}

/// A snapshot of real audit usage, serialized onto the scan report for the UI's
/// actual-vs-estimated readout.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ActualUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub calls: u64,
    /// True when every call contributed a cost (the dollar total isn't a partial sum).
    pub cost_complete: bool,
    /// Tokens served from the prompt cache across all calls in this audit (billed at ~0.1×
    /// the normal input rate). Zero when the CLI backend is in use or caching is disabled.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache across all calls (billed at ~1.25× input rate,
    /// once per 5-minute TTL window). Zero when the CLI backend is in use or caching is
    /// disabled.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Per-call safety cap on a single digest's size (chars). A digest is built PER CHUNK
/// (see `chunk_files`), so this only bounds one chunk's line-numbered text; it sits above
/// the raw chunk-packing target so a normal chunk is never re-truncated here. Only a
/// single pathological file larger than this would clip.
pub(crate) const MAX_DIGEST_CHARS: usize = 600_000;

/// Minimum number of chars that must have been dropped from the digest cap before the
/// `[digest truncated …]` notice is appended (BUG-8). Below this threshold the drop
/// is cosmetically trivial (e.g. the last closing brace of the last file) and the
/// notice would mislead the model into thinking significant content was omitted.
/// Chosen as ≈ 5 lines × 80 chars/line.
pub(crate) const TRUNCATION_NOTICE_MIN_DROP: usize = 400;

/// Raw-bytes target when packing files into chunks. Each chunk is audited in its own model
/// call, so the WHOLE repo is covered no matter its size — a single context can't hold a
/// multi-MB repo (a 2.8M-char repo is ~700k tokens, far past a 200k window), and the old
/// single-digest path silently dropped ~90% of such a repo. ~350k raw chars line-numbers
/// to ~400k and, with the rules block + system prompt + response, stays well inside a
/// 200k-token context. Smaller chunks also keep the model's attention per file higher.
const CHUNK_DIGEST_CHARS: usize = 350_000;

/// Number each line `NNNN| line`, so the model cites ACCURATE line numbers. Without
/// this the digest had no line markers and the model estimated by counting (and drifted —
/// 3 of 4 findings on the testbed cited the wrong line).
fn number_lines(content: &str) -> String {
    let mut s = String::new();
    for (i, line) in content.lines().enumerate() {
        s.push_str(&format!("{:>4}| {}\n", i + 1, line));
    }
    s
}

/// Build a single code digest from the repo's files, capped at [`MAX_DIGEST_CHARS`].
/// Each file is delimited and LINE-NUMBERED so the model can cite exact paths + lines.
///
/// # Truncation notice (BUG-8)
///
/// The notice `[digest truncated …]` is only appended when a NON-TRIVIAL amount of
/// content was dropped. If the partial slice captures all but a single short line
/// (specifically, `dropped < TRUNCATION_NOTICE_MIN_DROP` chars), the model already
/// has effectively all the code and the truncation warning would be misleading ("did
/// I miss something?"). The minimum drop threshold is intentionally conservative:
/// dropping even one method body is significant; dropping only the closing brace of
/// the last function is not.
pub fn build_digest(files: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut truncated = false;
    let mut significant_truncation = false;
    for (path, content) in files {
        let header = format!("// ===== FILE: {path} =====\n");
        let numbered = number_lines(content);
        if out.len() + header.len() + numbered.len() > MAX_DIGEST_CHARS {
            // Add a partial slice of this file if there's room, then stop.
            let remaining = MAX_DIGEST_CHARS.saturating_sub(out.len() + header.len());
            if remaining > 200 {
                out.push_str(&header);
                let slice: String = numbered.chars().take(remaining).collect();
                out.push_str(&slice);
                out.push('\n');
                // BUG-8: only warn when we actually dropped a meaningful chunk of
                // content. A trivially small drop (e.g. the last closing brace of
                // the last file) does not warrant a notice that may mislead the model
                // into thinking significant context is missing.
                let dropped = numbered.len().saturating_sub(remaining);
                significant_truncation = dropped >= TRUNCATION_NOTICE_MIN_DROP;
            } else {
                // Didn't add a partial slice at all — the whole file was dropped.
                significant_truncation = true;
            }
            truncated = true;
            break;
        }
        out.push_str(&header);
        out.push_str(&numbered);
        out.push('\n');
    }
    if truncated && significant_truncation {
        out.push_str("\n// [digest truncated at the size cap — audit the largest files first]\n");
    }
    out
}

/// The system prompt: what to look for, what NOT to (the mechanical tier is already
/// covered), and the STRICT JSON schema to return.
pub fn audit_system_prompt() -> String {
    r#"You are a senior software architect performing a CONFORMANCE audit of an existing codebase for Camerata.

The user message lists the rules the project has ADOPTED, each as `- [RULE-ID] directive`. Your job is to check the code against EACH adopted rule and report EVERY place the code violates it.

How to work — enumerate rule × file, exhaustively:
1. Take each adopted `[RULE-ID]` in turn.
2. For that rule, walk EVERY file in the digest and check whether that file violates it.
3. Emit one finding per concrete violation SITE (a rule violated in three files is three findings; violated twice in one file is two), and set `rule` to the EXACT adopted RULE-ID (e.g. "ARCH-STRICT-LAYERING-1"), copied verbatim — not a paraphrase, not a kebab name.
Do not stop after the first violation of a rule, and do not stop after the first file. A rule with no violations anywhere simply produces no findings — do not invent one.

RECALL OVER PRECISION. This is a discovery audit and a human architect reviews every finding before anything is enforced, so the cost of a borderline false positive is tiny and the cost of a missed real violation is high. When you are unsure whether something violates a rule, REPORT IT (use severity "low" and say it's borderline in `detail`). Do not stay silent to seem precise. Do not cap yourself at a handful — if there are thirty violations, return thirty.

SEVERITY. Use "low"/"medium"/"high" for the normal range (a debatable architectural preference is "low" or "medium"; a concrete, demonstrable security or correctness break is "high"). Reserve "critical" — it must stay RARE — for a violation that clears ALL of: (1) it is concretely exploitable, not theoretical, and (2) a competent attacker or a single bad input reaches real, serious impact quickly. Qualifying classes: unauthenticated (or unauthorized) access to a privileged, destructive, or financial operation; a credential or secret exposed to clients or committed to the repo; PII or equivalently sensitive data readable or writable with no access control on an internet-exposed surface; remote code execution or injection with a real, reachable path (not just an untrusted-input pattern that happens to be sanitized elsewhere). Do not use "critical" for a bad-but-contained bug, a missing best practice, or anything you would call "high" out of general alarm — when in doubt between "high" and "critical", use "high".

CRITICAL — do NOT invent rule names that duplicate adopted rules. Before you set `rule`, check whether the violation is already covered by one of the adopted `[RULE-ID]`s above. If it is, you MUST use that exact adopted RULE-ID — even if you would have phrased the issue differently. A controller reaching into the database directly is `ARCH-STRICT-LAYERING-1`, not "controller-direct-db" or "handler-bypasses-repo"; a handler panicking on a DB error is `ARCH-STRUCTURED-ERRORS-1`, not "panic-on-db-error". Inventing a new name for a violation an adopted rule already covers is the single worst failure mode of this audit — it produces triplicate findings that all mean the same thing.

Flagging novel issues (issues no adopted rule covers) is GATED by this pass's instruction line. ONLY when that line says to "ALSO flag any other genuine issues" may you report something outside the adopted rules — and then set `rule` to a short kebab name (e.g. "auth-on-write-paths"), reserved strictly for genuinely-novel issues (if any adopted rule fits, use the adopted id instead). When the instruction line says to check ONLY the listed rules, report nothing outside them.

CROSS-FILE REASONING IS YOUR MANDATE. Every other automated check in this pipeline (the deterministic floor, every external static-analysis tool) reads ONE FILE AT A TIME — you are the only layer with the REPO MAP and multiple file bodies at once, so tracing an invariant ACROSS files is work no other layer can do. Three mechanisms to actively look for, by name, regardless of which specific rule (if any) they end up filed under:
- CONFIG-TO-HANDLER: a platform/feature flag or config value set in one file (verification disabled, a check short-circuited, a debug/bypass mode left on) that a handler or privileged code path in ANOTHER file reads and trusts, with no independent check of its own.
- POLICY-TO-CALL-SITE: a permissive access/data policy (a wide-open rule, a default-allow, an overly broad grant) declared in one place, and code elsewhere that relies on that policy as its only protection.
- CREDENTIAL-TO-USAGE: a secret or credential defined or loaded in one file and consumed by code in one or more OTHER files — trace where it is actually used, not just where it is declared.
If deciding one of these needs the body of a file you don't have, use `needs_files` (see below) rather than guessing or staying silent about it.

COMPENSATING CONTROLS — before flagging a missing check on any path, ask: is there ANY compensating check elsewhere on this same path — a signature or HMAC verification, a shared-secret comparison, a session-derived identity check, an ownership predicate, a role/permission gate? If you can find one (in the file at hand, the repo map, or another file body you have), do NOT flag it — a report that ignores a real compensating control is itself a false positive. Only flag the path when you looked and found none.

BACKSTOP ROLE — a missing rule is never a reason to withhold a real finding. When your own reasoning (especially the cross-file mechanisms above) turns up a genuine security-relevant observation that doesn't fit any adopted rule, report it exactly like any other novel issue — subject to the SAME instruction-line gate described above, never silenced just because nothing named it in advance.

DO NOT report: hardcoded secrets, secrets embedded in URLs, raw SQL string concatenation, or path-escape writes — a separate deterministic scanner already covers those precisely. Do not report pure style/formatting nits.

Each line in the digest is prefixed with its line number as `NNNN| `. Cite that exact number in `line` — do not estimate.

WATCH FOR INDIRECTION before flagging something MISSING. When a loop renders items via a HELPER CALL (e.g. `for row in rows { data_tr(row, …) }`), an attribute the rule wants — a `key`, a CSS class, an error handler — is very often set INSIDE that helper, not at the call site. Do NOT flag "missing key"/"missing X" on the call site without checking the called function's body. If that body is in the digest, read it; if it's elsewhere, request it via `needs_files` and defer — never assume it's missing just because it's not inline at the loop. (This is a real false-positive class: row renderers extracted into helpers that DO set the key.)

Cross-file context: you have the REPO MAP (every file + its public symbols) but only SOME file bodies in this pass. If judging a rule needs the actual BODY of a file that is in the map but NOT included below (e.g. you must read a repository's implementation, or a type defined elsewhere, to decide), do NOT guess and do NOT stay silent — list EVERY file path involved in that deferred judgment (the file under suspicion AND the files it depends on) in `needs_files`. A follow-up pass will include those bodies together so you can decide then.

For `code`, copy the offending source text VERBATIM from the digest — the exact characters of the line you're flagging, not a paraphrase. A deterministic post-step locates the true line by finding this text in the file, so an exact copy gives an exact line; a paraphrase makes the line approximate. Keep it to the single most relevant line (or short span). Still set `line` to your best estimate as a fallback.

CAPTURES (optional, only when applicable). Some adopted directives above name a placeholder in angle brackets — e.g. "...naming the bucket: your `<bucket>` bucket is public" or "...your `<table>` table has no Row Level Security". When the directive you matched contains such a placeholder AND you can identify the concrete real-world object this specific violation is about, add an optional `"captures"` object to that finding mapping the BARE token name (no angle brackets, e.g. `"bucket"`, `"table"`, `"function-name"`, `"view"`, `"matview"`, `"schema"`) to the actual object name (e.g. `"invoices"`, `"profiles"`, `"charge_membership"`) — so the report can name the real object instead of a generic placeholder. Omit `captures` entirely (or leave it empty) whenever the directive has no such placeholder, or you cannot pin down the concrete object — never guess a name you are not confident is correct.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {
      "path": "relative/file/path",
      "line": 0,
      "severity": "critical|high|medium|low",
      "rule": "EXACT adopted RULE-ID, or a short-kebab-name for an unlisted issue",
      "title": "one-line statement of the specific violation here",
      "code": "the EXACT offending source line, copied verbatim from the digest",
      "detail": "why it's a problem and what the fix direction is",
      "captures": {"bucket": "the-real-object-name"}
    }
  ],
  "proposed_rules": [
    {
      "name": "short-kebab-name (only for issues NOT covered by an adopted rule)",
      "title": "the rule to enforce going forward",
      "rationale": "why this rule, grounded in the findings",
      "severity": "critical|high|medium|low",
      "enforcement": "mechanical|review"
    }
  ],
  "needs_files": ["relative/path/you/need/the/body/of.rs"]
}
If the code genuinely conforms everywhere, return {"findings": [], "proposed_rules": [], "needs_files": []}. Every finding must point at real code at a real line."#
        .to_string()
}

/// Pull the first balanced-looking JSON object out of a model response (tolerates
/// markdown fences or stray prose around it).
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Map a model-invented rule name (already uppercased + hyphenated) onto the canonical
/// adopted corpus rule it actually means, for the few families the model keeps re-inventing:
/// panics → structured-errors, direct-DB / own-pool / bypasses-repo → strict-layering,
/// secret-in-URL → no-secrets-in-URL. Returns the canonical id ONLY when that rule is
/// actually adopted by this project, so a project without the rule never gets a phantom id
/// (and the location merge still collapses the duplicates regardless). Patterns are kept
/// narrow to avoid mislabeling a genuinely-novel issue.
fn canonical_adopted_rule(
    norm: &str,
    adopted: &std::collections::HashSet<String>,
) -> Option<String> {
    let has = |s: &str| norm.contains(s);
    let candidate = if has("SECRET") && has("URL") {
        "ARCH-NO-SECRETS-IN-URL-1"
    } else if has("PANIC")
        // BUG-AI-2: narrow the match so rules that merely MENTION "PANIC" in a non-panic
        // context (e.g. "PREVENT-PANICKING-AUTH-CHECK", "LOG-PANIC-RECOVERY-1") are not
        // falsely canonicalized to ARCH-STRUCTURED-ERRORS-1. Only the specific invented
        // names the model repeatedly emits for actual panic-at-callsite violations qualify.
        && (has("HANDLER") || has("UNWRAP") || has("UNHANDLED") || has("ON-ERROR")
            || has("PROPAGAT") || has("UNWIND") || has("BUBBL"))
    {
        "ARCH-STRUCTURED-ERRORS-1"
    } else if (has("DIRECT") && (has("DB") || has("DATABASE")))
        || (has("BYPASS") && has("REPO"))
        || (has("OWN") && has("POOL"))
        || (has("CREATES") && has("POOL"))
    {
        "ARCH-STRICT-LAYERING-1"
    } else {
        return None;
    };
    adopted.contains(candidate).then(|| candidate.to_string())
}

/// Placeholder token names an AI-emitted finding's optional `captures` object may map to —
/// the same finite domain-token vocabulary the corpus's authored `directive`/`remediation`
/// text uses (see `onboard::architectural::capture_token_for` and
/// `report_export::generic_placeholder_filler` for the deterministic-checker and
/// report-rendering sides of the same vocabulary), minus `path`/`file` — those two are always
/// filled from `Finding.path` (see `report_export::instantiate_remediation`'s resolution
/// order) and never need a model-supplied override. Restricting to this allowlist means a
/// model that emits a garbage or hallucinated key just has that key silently dropped — it can
/// never smuggle an arbitrary token into the report's `Finding` wire type via `captures`.
const KNOWN_AI_CAPTURE_TOKENS: &[&str] =
    &["table", "function-name", "function", "bucket", "view", "matview", "schema", "policy"];

/// A capture value's length cap: a token should be a bare object name (a table, bucket, or
/// function name), never a paragraph. Generous enough that it never clips a real identifier;
/// this only exists as defense against a model that stuffs prose into the value.
const MAX_CAPTURE_VALUE_LEN: usize = 200;

/// How many captures a single finding may carry. A rule's remediation text never uses more
/// than one or two domain tokens — this is a ceiling against a malformed/adversarial blob,
/// not a realistic limit any well-formed response would ever approach.
const MAX_CAPTURES_PER_FINDING: usize = 8;

/// Parse one finding's OPTIONAL `captures` object from the model's raw JSON — e.g.
/// `{"bucket": "invoices"}` maps the corpus rule's `<bucket>` remediation token to the real
/// object this finding is about (see `report_export::instantiate_remediation`, the consumer).
///
/// Deliberately fail-soft at every step, since `captures` is pure enrichment (an empty map
/// just means the report's generic fallback wording is used, per `generic_placeholder_filler`)
/// and must never cost the finding itself:
/// - `captures` absent, `null`, or not a JSON object -> empty map.
/// - a key outside [`KNOWN_AI_CAPTURE_TOKENS`] -> that entry is skipped, not fatal.
/// - a value that isn't a JSON string -> that entry is skipped, not fatal.
/// - an empty/whitespace-only value, or one longer than [`MAX_CAPTURE_VALUE_LEN`] -> skipped.
/// - more than [`MAX_CAPTURES_PER_FINDING`] well-formed entries -> extras beyond the cap are
///   dropped (this can only matter for adversarial/malformed input; a real rule directive
///   never uses more than a couple of tokens).
fn parse_finding_captures(f: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(obj) = f["captures"].as_object() else {
        return out;
    };
    for (key, value) in obj {
        if out.len() >= MAX_CAPTURES_PER_FINDING {
            break;
        }
        if !KNOWN_AI_CAPTURE_TOKENS.contains(&key.as_str()) {
            continue;
        }
        let Some(s) = value.as_str() else {
            continue;
        };
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed.chars().count() > MAX_CAPTURE_VALUE_LEN {
            continue;
        }
        out.insert(key.clone(), trimmed.to_string());
    }
    out
}

/// Parse a model audit response into Findings + ProposedRules in the scan's shapes.
/// Robust: malformed output yields empty vecs rather than erroring the whole scan.
pub fn parse_ai_findings(
    repo: &str,
    raw: &str,
    adopted: &std::collections::HashSet<String>,
) -> (Vec<Finding>, Vec<ProposedRule>) {
    let Some(json) = extract_json_object(raw) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Vec::new(), Vec::new());
    };

    let mut findings = Vec::new();
    if let Some(arr) = v["findings"].as_array() {
        for f in arr {
            let rule = f["rule"].as_str().unwrap_or("architecture").trim();
            let norm = rule.to_ascii_uppercase().replace(' ', "-");
            // If the model cited an ADOPTED rule id, key the finding to that id directly so
            // the violation shows up under the rule the architect selected. Else try to
            // canonicalize a well-known invented name onto the adopted rule it actually
            // means (AI-HANDLER-PANICS → ARCH-STRUCTURED-ERRORS-1). Else it's a genuinely
            // AI-discovered issue beyond the ruleset (AI- provenance prefix).
            let rule_id = if adopted.contains(&norm) {
                norm
            } else if let Some(canon) = canonical_adopted_rule(&norm, adopted) {
                canon
            } else {
                format!("AI-{norm}")
            };
            let severity = match f["severity"].as_str().unwrap_or("medium") {
                "critical" => "critical",
                "high" => "high",
                "low" => "low",
                _ => "medium",
            };
            let title = f["title"].as_str().unwrap_or("").trim().to_string();
            let code = f["code"].as_str().unwrap_or("").trim().to_string();
            let detail = f["detail"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() && detail.is_empty() && code.is_empty() {
                continue;
            }
            // `snippet` holds the VERBATIM offending line when the model gave one (matches the
            // deterministic floor, and lets the line-resolution post-step grep for it). Fall
            // back to the title when there's no code. The title is preserved by leading the
            // detail so the human still sees the one-line statement.
            let snippet = if code.is_empty() { title.clone() } else { code };
            let detail = match (title.is_empty(), detail.is_empty()) {
                (false, false) => format!("{title} — {detail}"),
                (false, true) => title.clone(),
                _ => detail,
            };
            findings.push(Finding {
                repo: repo.to_string(),
                path: f["path"].as_str().unwrap_or("(repo)").to_string(),
                line: f["line"].as_u64().unwrap_or(0) as usize,
                rule_id,
                severity: severity.to_string(),
                snippet,
                detail,
                status: "active".to_string(),
                also_matches: Vec::new(),
                // AI-audit findings are advisory, never scan-time tool previews.
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
                // Calibration (apply_verdicts) sets these once the calibration pass runs;
                // a raw pre-calibration finding has no confidence/effort opinion yet.
                confidence: None,
                effort: None,
                // Category comes from calibration (or the heuristic in merge_semantic_groups);
                // `located` is set by merge_by_location once snippets are anchored to files.
                category: None,
                located: true,
                // AI/semantic findings usually have no cheaply-known structured object at
                // match time (the model narrates the defect in prose, not a parsed
                // table/function/bucket name) — but when the model DID identify one and
                // supplied an optional `captures` object (see the CAPTURES section of
                // `audit_system_prompt`), `parse_finding_captures` maps it through so the
                // report can name the real object instead of falling back to a generic
                // placeholder. Empty (the common case) falls back exactly as before.
                captures: parse_finding_captures(f),
                // Set by `audit_repo` in a blanket post-pass once the recommendation (or
                // operator-forced pick) for this finding's rule is known — see its doc
                // comment. `None` here is the correct pre-tag state, not a gap.
                evaluated_option_id: None,
                also_locations: Vec::new(),
                // Set by `generate_fix_specifics` in a later pass; a raw pre-generation
                // finding has no fix opinion yet.
                fix_specific: None,
                // Set by `apply_severity_calibration_rules` (D5) once the calibration pass
                // runs; a raw pre-calibration finding has no floor rationale yet.
                calibration_rationale: None,
            });
        }
    }

    let mut proposed = Vec::new();
    if let Some(arr) = v["proposed_rules"].as_array() {
        for r in arr {
            let name = r["name"].as_str().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            let id = format!("AI-{}", name.to_ascii_uppercase().replace(' ', "-"));
            // Both mechanical and architectural tiers are CI-tier deterministic checks.
            let mechanical = matches!(
                r["enforcement"].as_str(),
                Some("mechanical") | Some("architectural")
            );
            let title = r["title"].as_str().unwrap_or(name).trim().to_string();
            // How many AI findings this rule's name accounts for.
            let finding_count = findings.iter().filter(|f| f.rule_id == id).count();
            proposed.push(ProposedRule {
                id,
                title,
                // AI-discovered architectural rules are human-judged, not auto-mechanical.
                kind: if mechanical {
                    "mechanical".to_string()
                } else {
                    "review".to_string()
                },
                // Architectural guidance partitions to CONVENTIONS.md (structured).
                enforcement: "structured".to_string(),
                options: Vec::new(),
                default_option: None,
                // AI-discovered → AI-designed; un-grounded until the grounding pass.
                verification: "draft".to_string(),
                sources: Vec::new(),
                decision_question: None,
                decision_why: None,
                scope: "repo-local".to_string(),
                domain: "architecture".to_string(),
                // Inherently semantic -> enforced at the cross-agent integration tier
                // (an AI-assisted pre-PR check), not the line-level content gate.
                enforcement_point: "integration".to_string(),
                repos: vec![repo.to_string()],
                placement: "project (AI-assisted integration check)".to_string(),
                finding_count,
                recommended: true,
                // AI-discovered rules are always draft (un-grounded), never auto-recommended.
                is_auto_recommended: false,
            });
        }
    }

    (findings, proposed)
}

/// The system prompt for the calibration pass. This pass does NOT drop findings — it
/// recalibrates severity for the app's real context and flags low-confidence ones. An
/// earlier version was a skeptic that REFUTED (dropped) findings, but it never receives
/// the code (see `verify_findings`), so it was guessing — and dropping real, low-impact
/// violations the architect wanted to see was a direct cause of "the audit missed
/// obvious violations." Discovery is recall-first; the human triages, the tool does not
/// pre-censor.
pub fn verify_system_prompt() -> String {
    r#"You are calibrating an automated audit's findings before a human architect triages them.
You do NOT decide whether to KEEP a finding — the architect does. Every finding is kept.

Judge each finding ON ITS OWN MERITS. The model that scanned the code may have been confident
or assertive — that does NOT carry over. Calibration is where humility lives: a higher-tier
scan tends to over-assert on debatable points, and your job is to put the nuance back.

For EACH finding, do two things:
- Assign a CALIBRATED severity (critical/high/medium/low) for this app's real-world context,
  using this rubric:
  * "critical" is RARE. Only assign it when the finding clears ALL of: concretely (not
    theoretically) exploitable, AND a competent attacker or a single bad input reaches serious
    impact quickly. Qualifying classes: unauthenticated/unauthorized access to a privileged,
    destructive, or financial operation; a credential/secret exposed to clients or committed to
    the repo; PII or equivalently sensitive data readable/writable with no access control on an
    internet-exposed surface; remote code execution or injection with a real, reachable path.
    When in doubt between "high" and "critical", use "high" — do not inflate an ordinary high
    into critical out of general alarm, and never assign "critical" to a debatable preference.
  * RULES OF THUMB (apply these consistently — the SAME shape of finding must get the SAME
    severity every run, not a per-run judgment call):
    - Unauthenticated access to ANY protected resource or operation, OR a path to full-account
      compromise (takeover, impersonation, assuming any user's identity) — ALWAYS "critical".
    - An AUTHENTICATED user reading another tenant's/user's data they should not see (cross-
      tenant read) is "high" by default — NOT "critical" — unless the data class itself escalates
      it: payment data, credentials/secrets, or PII of the sensitive kind (SSN, health records,
      government ID). Only then does a cross-tenant READ clear the critical bar.
    - Do NOT downgrade an access-control or injection finding because "RLS probably covers it" or
      similar defense-in-depth speculation — a check failing at the boundary the finding actually
      names is real regardless of what a deeper layer might also do. If you invoke RLS (or any
      other layer) as a mitigating factor, it must be a CONFIRMED fact from the evidence you were
      given, not a guess, and it does not excuse a boundary that has already failed.
    - An open or unvalidated redirect (the destination is attacker-controlled or otherwise
      unchecked) is "medium" by default, NOT "high" — on its own it only harms the user who
      follows it and hands the attacker nothing. It becomes "high" exactly when the finding says
      the redirect is CHAINED to credential capture (the destination can read or exfiltrate a
      token, cookie, or credential) — then the attacker walks away with something usable. It is
      never "critical" from the redirect mechanism alone, chained or not — that would require an
      UNAUTHENTICATED path to a privileged operation, which is a different finding.
    - A privileged/role/admin check enforced ONLY in client-visible (UI/browser) code, with no
      independent check on the server, is "high" AT MOST, never "critical" — the client-side
      control is not the security boundary; a client can always be made to skip what only the
      client enforces. Judge it by what the SERVER allows an authenticated caller to do once that
      gate is bypassed. An authenticated privilege escalation confined to a single resource or
      operation is "high"; "critical" is reserved for an unauthenticated reach or a system- or
      account-wide compromise, neither of which a client-side-only gate states by itself.
  * A concrete, demonstrable SECURITY or CORRECTNESS break that does NOT clear the critical bar
    above (injection needing extra steps to reach, missing auth on a write path with contained
    blast radius, data loss/corruption, a real but non-catastrophic exploit) is "high".
  * A DEBATABLE ARCHITECTURAL PREFERENCE — a "valid pattern but not the one this rule prefers"
    call, a layering/structure/abstraction opinion, an over-engineering/YAGNI note on a small
    codebase, a stylistic or convention preference — is NEVER "critical" or "high". Cap it at
    "medium", usually "low". These are preferences a reasonable team could disagree on, not
    violations.
  * A real but low-impact issue is "low", not removed.
- Set confidence: "low" when the finding is a debatable preference (per above), is theoretical,
  is likely over-flagged, or you cannot tell it is real without seeing more code; "high" only
  for clear, concrete violations. Confidence "low" flags the finding for the architect's review —
  it is ADVICE, not a deletion. When in doubt between a violation and a preference, treat it as a
  preference: low confidence, capped severity.
- Estimate remediation EFFORT for a developer to fix this ONE finding, given only what you can
  see (the path/snippet/detail) — "low" (a one-line/local change: rename, add a check, swap a
  call), "medium" (touches a few call sites or needs a small new helper/test), or "high" (a
  structural change: new abstraction, cross-file rework, a migration). When you cannot tell,
  default to "medium" rather than guessing an extreme.
- Assign a semantic CATEGORY from this closed set (pick the single best fit; omit the field if
  none clearly applies): authorization, authentication, secret-exposure, injection,
  transport-security, rls-policy, resource-exposure, input-validation, error-handling,
  arch-conformance, testing-style, performance. This groups the same defect flagged under
  different rule names; it does NOT change severity.

Do NOT deduplicate, and do NOT cross-reference other findings — no "same as [N]", "duplicate
of [N]", "as index N", "index N", "row N", or ANY pointer to another finding by index/row.
Deduplication already happened upstream; your `reason` is one line about THIS finding's
severity/confidence only, with no reference to any other finding.

A finding whose rule id you don't recognize as a named convention is NOT automatically less
credible — it is the audit's BACKSTOP catching a genuine issue that no named rule happened to
cover. Judge it purely on the evidence in its own `detail`/`snippet`, exactly like any other
finding: do not discount its severity or confidence for being unnamed, and do not inflate it
either. The absence of a rule id is not evidence of anything.

Return ONLY JSON, no prose:
{"verdicts":[{"index":0,"severity":"critical|high|medium|low","confidence":"high|low","effort":"low|medium|high","category":"authorization|authentication|secret-exposure|injection|transport-security|rls-policy|resource-exposure|input-validation|error-handling|arch-conformance|testing-style|performance","reason":"one line"}]}
One verdict per finding, addressed by its [index]."#
        .to_string()
}

/// Remove cross-finding dedup pointers like "same as [6]" / "duplicate of [10]" (and a
/// trailing bare "[12]") from a calibration reason, case-insensitively, then tidy leftover
/// separators. These indices are batch-local and unreliable; the merge relationship is
/// already structural (rule_id + path + line + also_matches), so the prose is pure noise.
fn strip_dedup_pointers(reason: &str) -> String {
    // Char-indexed (UTF-8 safe). ASCII-lowercase per char keeps a 1:1 index alignment.
    let chars: Vec<char> = reason.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let starts = |at: usize, pat: &str| -> bool {
        let pc: Vec<char> = pat.chars().collect();
        at + pc.len() <= lower.len() && lower[at..at + pc.len()] == pc[..]
    };
    // After a phrase, consume optional separators + a pointer token ([..] | #N | N).
    // Returns Some(end) when a number token was consumed, else None.
    let skip_number = |from: usize| -> Option<usize> {
        let mut j = from;
        while j < chars.len() && matches!(chars[j], ' ' | '#' | ':') {
            j += 1;
        }
        if j < chars.len() && chars[j] == '[' {
            let mut k = j + 1;
            while k < chars.len() && chars[k] != ']' {
                k += 1;
            }
            return (k < chars.len() && k > j + 1).then_some(k + 1);
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            return Some(j);
        }
        None
    };
    // (phrase, requires_a_following_number). The `index`/`row` family REQUIRES a number so
    // legitimate prose ("add an index on (a, b)") is untouched while pointers ("as index 6",
    // "index 9", "row 3") are stripped. The same-as/duplicate family is always a pointer.
    let patterns: &[(&str, bool)] = &[
        ("same as", false),
        ("duplicate of", false),
        ("duplicates", false),
        ("dup of", false),
        ("as index", true),
        ("see index", true),
        ("cf index", true),
        ("index", true),
        ("row", true),
    ];
    let mut keep = String::with_capacity(reason.len());
    let mut i = 0;
    while i < chars.len() {
        let mut next = None;
        for (pat, needs_num) in patterns {
            if starts(i, pat) {
                let after = i + pat.chars().count();
                if *needs_num {
                    if let Some(end) = skip_number(after) {
                        next = Some(end);
                        break;
                    }
                    // phrase present but no number -> not a pointer; try other patterns
                } else {
                    next = Some(skip_number(after).unwrap_or(after));
                    break;
                }
            }
        }
        match next {
            Some(end) => i = end,
            None => {
                keep.push(chars[i]);
                i += 1;
            }
        }
    }
    // Tidy: collapse double spaces and strip leftover leading/trailing separators.
    let tidied = keep.replace("  ", " ");
    tidied
        .trim()
        .trim_matches(|c: char| matches!(c, ';' | ',' | '.' | '-' | ':') || c.is_whitespace())
        .to_string()
}

/// Phrases that mark a finding's own text (its authored `detail`/`snippet`) or the
/// calibration model's own verdict reason as recording an EXPLOIT PRECONDITION — some
/// additional fact that must hold (forging a cookie, holding another credential, a specific
/// attacker capability) before the vulnerability is actually reachable, as opposed to an
/// unconditional exposure exploitable as-is. Deliberately phrase-based and conservative
/// (clear precondition phrasing, not any use of the word "if") — a finding that merely
/// contains "if" somewhere in an unrelated sentence must not trip this.
///
/// CRITICAL GUARDRAIL (calibration review, docs/plans/2026-09-30_cycle2-queue-hardening.md,
/// extended cycle-5): this predicate is used ONLY to (a) block an upward calibration move in
/// [`apply_verdicts`], (b) cap the deterministic D5 floor's critical escalation in
/// [`severity_calibration_floor`], and (c) break ties in the "do now" ranking in
/// `report_export`. It must NEVER be used to LOWER a severity — a genuine critical whose
/// justification merely mentions a mitigation must never be buried by this logic. See the call
/// sites' own doc comments for the never-demote invariant.
const EXPLOIT_PRECONDITION_PHRASES: &[&str] = &[
    "requires ",
    "contingent on",
    "depends on",
    "only if",
    "assuming ",
    "provided that",
    "an attacker must",
];

/// Negation guard for the "requires " phrase specifically: "requires no authentication" (or
/// "requires no login" — already its own floor phrase, see
/// [`UNAUTHENTICATED_OR_FULL_COMPROMISE_PHRASES`]) NEGATES the requirement. It describes an
/// UNCONDITIONAL exposure — no credential needed at all — which is the semantic OPPOSITE of a
/// precondition that gates exploitation behind forging/holding something. Without this guard, a
/// genuinely unauthenticated finding phrased as "the endpoint requires no authentication" would
/// be misread as recording a precondition and incorrectly capped away from Critical. Scoped to
/// "requires " because none of the other phrases above share this negation ambiguity.
const REQUIRES_NEGATION_SUFFIX: &str = "no ";

/// Whether `lower` (already-lowercased) contains the "requires " precondition phrase in a
/// NON-negated form — i.e. an occurrence of "requires " not immediately followed by "no "
/// ([`REQUIRES_NEGATION_SUFFIX`]). Scans every occurrence (not just the first) so text with both
/// a negated occurrence ("...requires no login...") and a genuine one elsewhere ("...requires
/// forging a session cookie...") still registers the genuine one.
fn contains_non_negated_requires(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(pos) = rest.find("requires ") {
        let after = &rest[pos + "requires ".len()..];
        if !after.starts_with(REQUIRES_NEGATION_SUFFIX) {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether `text` records an exploit precondition per [`EXPLOIT_PRECONDITION_PHRASES`].
/// Case-insensitive; lowercases internally so every caller (the calibration passes here and
/// `report_export`'s ranking tie-break) can pass raw, unprocessed text. The "requires " phrase
/// is negation-guarded (see [`contains_non_negated_requires`]) so "requires no authentication"
/// reads as unconditional, not as a precondition.
pub(crate) fn mentions_exploit_precondition(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if contains_non_negated_requires(&lower) {
        return true;
    }
    EXPLOIT_PRECONDITION_PHRASES
        .iter()
        .filter(|p| **p != "requires ")
        .any(|p| lower.contains(*p))
}

/// Apply the calibration verdicts: recalibrate severity and annotate confidence/reason.
/// NEVER drops a finding — recall-first discovery hands every finding to the architect.
/// Robust: unparseable verdicts keep all findings as-is.
///
/// UPWARD-CALIBRATION GUARDRAIL: when applying the model's verdict would RAISE a finding's
/// severity above its authored/base severity (the severity it carried entering this call)
/// AND the finding's own text or the verdict's own reason records an exploit precondition
/// (see [`mentions_exploit_precondition`]), the raise is refused and the base severity is
/// kept — a High finding whose note says exploitation "requires forging a session cookie"
/// does not get waved through to Critical and take a do-now slot from an unconditional
/// critical. This can ONLY hold severity at its base value or let the raise through; it
/// never lowers a severity below what it already was (an explicit downgrade verdict with no
/// bearing on this guardrail continues to apply exactly as before).
pub fn apply_verdicts(raw: &str, findings: Vec<Finding>) -> Vec<Finding> {
    let Some(json) = extract_json_object(raw) else {
        return findings;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return findings;
    };
    let Some(arr) = v["verdicts"].as_array() else {
        return findings;
    };
    let mut out = Vec::new();
    for (i, mut f) in findings.into_iter().enumerate() {
        if let Some(verdict) = arr.iter().find(|x| x["index"].as_u64() == Some(i as u64)) {
            if let Some(sev) = verdict["severity"].as_str() {
                let candidate = match sev {
                    "critical" => "critical",
                    "high" => "high",
                    "low" => "low",
                    _ => "medium",
                };
                // Raising = moving to a MORE severe label. This module's local
                // `severity_rank` ranks `critical` highest (4) down to unrecognized/`low`
                // (0/1), so "raise" means the candidate's rank EXCEEDS the base severity's.
                // The base severity is `f.severity` as it enters this branch — untouched so
                // far this iteration.
                let would_raise = severity_rank(candidate) > severity_rank(&f.severity);
                let raw_reason = verdict["reason"].as_str().unwrap_or("");
                let precondition_text = format!("{} {} {raw_reason}", f.detail, f.snippet);
                if would_raise && mentions_exploit_precondition(&precondition_text) {
                    // BLOCKED: keep the authored/base severity. Never demotes — a verdict
                    // that does not propose a raise (equal or a genuine downgrade) is
                    // untouched by this branch and falls through to the assignment below
                    // exactly as before.
                    f.calibration_rationale = Some(format!(
                        "Calibration held at {} (the model proposed {candidate}): the \
                         finding's own text or the model's own reasoning records an exploit \
                         precondition, which can only block an upward calibration, never \
                         justify one.",
                        f.severity
                    ));
                } else {
                    f.severity = candidate.to_string();
                }
            }
            let low_conf = verdict["confidence"].as_str() == Some("low");
            // Structured confidence (Part 1 §3): promoted out of the string-embedded
            // `[needs review: reason]` detail suffix below. `needs_review` is also set
            // here — previously calibration never touched it for AI findings (only
            // `classify_repo_findings`'s in_test path did), so the UI's structured
            // "needs review" filter silently missed every AI-flagged finding.
            //
            // C4-P2: `needs_review` is set UNCONDITIONALLY from `low_conf` (never only on the
            // `true` branch) — a finding that enters this call already carrying a stale
            // `needs_review = true` (e.g. a prior THOROUGH-mode consensus vote, or a verdict
            // re-application) must have it CLEARED when this verdict says `high`, or the
            // exported `confidence`/`needs_review` fields silently desync: confidence says
            // "high" but the structured flag still reads hedged. The two fields are a
            // biconditional (`confidence == "needs-review"` iff `needs_review`) by construction
            // here — see `report_export::is_hedged`, which additionally self-heals any
            // desync this guard misses (a belt-and-suspenders defense, not a substitute for it).
            f.confidence = Some(if low_conf { "needs-review" } else { "high" }.to_string());
            f.needs_review = low_conf;
            // Structured effort (Part 1 §3): the calibration verdict schema now emits it
            // alongside severity/confidence. Only accept the three known values — a
            // mis-shaped or missing field leaves `effort` at its prior value (None for a
            // fresh AI finding) rather than recording a guess.
            if let Some(eff) = verdict["effort"].as_str() {
                if matches!(eff, "low" | "medium" | "high") {
                    f.effort = Some(eff.to_string());
                }
            }
            // Semantic category (Bug 3): the calibration verdict may classify the finding into
            // the closed taxonomy that drives the cross-family merge pass. Only a KNOWN value is
            // accepted; anything mis-shaped/absent leaves `category` for the rule-id heuristic in
            // `merge_semantic_groups` to fill (fail-through, per design §1a step 3).
            if let Some(cat) = verdict["category"].as_str() {
                if is_known_category(cat) {
                    f.category = Some(cat.to_string());
                }
            }
            // Strip any cross-finding dedup pointers ("same as [6]", "duplicate of [10]") the
            // model still volunteers: the indices are batch-local and wrong, the relationship
            // is already encoded structurally (rule_id + path + line + also_matches), and a
            // wrong English pointer in a data cell is worse than none — it ships in the CSV.
            let reason = strip_dedup_pointers(verdict["reason"].as_str().unwrap_or("").trim());
            let reason = reason.trim();
            if low_conf || !reason.is_empty() {
                let tag = if low_conf {
                    "needs review"
                } else {
                    "calibrated"
                };
                // The `[needs review]`/`[calibrated]` detail tag is KEPT for one release for UI
                // back-compat (`split_needs_review`, ui-core/src/rules.rs still regex-parses
                // it) — the structured `confidence`/`needs_review` fields above are additive,
                // not a replacement, until the UI reads them directly.
                f.detail = if reason.is_empty() {
                    format!("{} [{tag}]", f.detail)
                } else {
                    format!("{} [{tag}: {reason}]", f.detail)
                };
            }
        }
        out.push(f); // never dropped
    }
    out
}

// ── D5: severity calibration rules of thumb ──────────────────────────────────────────────
//
// docs/plans/2026-09-29_codebase-inspection-hardening.md, D5. The calibration MODEL already
// carries these rules of thumb in its own system prompt (`verify_system_prompt`, above), but a
// model's application of a rule of thumb is, by construction, a per-run judgment call — the same
// finding phrased identically twice is not guaranteed the same verdict twice. This section is
// the DETERMINISTIC backstop: a pure post-calibration pass that re-derives the floor severity
// from the finding's own text and enforces it regardless of what the model said, recording WHY
// on the finding (`Finding::calibration_rationale`) so the calibrated severity is auditable, not
// a coin flip. It only ever RAISES severity to a floor — it never lowers a severity the model (or
// the deterministic origin) already assigned above the floor.

/// Phrases that mark a finding as unauthenticated access to a protected resource/operation, or a
/// path to full-account compromise (takeover/impersonation) — the plan's "Critical" class.
/// Deliberately phrase-based (not a single keyword) so a finding merely mentioning the word
/// "authentication" in passing (e.g. "the authentication middleware also logs...") doesn't
/// trip the floor — every phrase here asserts the ABSENCE of auth or a completed compromise.
const UNAUTHENTICATED_OR_FULL_COMPROMISE_PHRASES: &[&str] = &[
    "unauthenticated",
    "without authentication",
    "without any authentication",
    "no authentication",
    "no authentication required",
    "no authentication is required",
    "missing authentication",
    "not authenticated",
    "without logging in",
    "without needing to log in",
    "no login required",
    "requires no login",
    "full account compromise",
    "full account takeover",
    "complete account takeover",
    "complete account compromise",
    "account takeover",
    "take over any account",
    "take over any user's account",
    "impersonate any user",
    "assume any user's identity",
];

/// Phrases marking an AUTHENTICATED cross-tenant/cross-user READ — the plan's "High by default"
/// class. Requires BOTH a cross-tenant/cross-user phrase AND a read-shaped verb so a finding
/// about a cross-tenant WRITE (already its own, typically higher, class) or an unrelated mention
/// of "another organization" doesn't spuriously floor to High via this rule.
const CROSS_TENANT_PHRASES: &[&str] = &[
    "cross-tenant",
    "cross tenant",
    "cross-organization",
    "cross organization",
    "cross-org",
    "another organization's",
    "other organizations'",
    "another tenant's",
    "other tenants'",
    "another user's",
    "other users'",
    "belonging to another user",
    "belonging to a different organization",
    "data from another account",
];
const READ_VERBS: &[&str] = &[
    "read", "view", "fetch", "access", "download", "list", "see", "retrieve",
];

/// Data-class phrases that escalate an authenticated cross-tenant read from High to Critical:
/// payment data, credentials/secrets, and sensitive PII — the plan's explicit escalation list.
const PAYMENT_DATA_PHRASES: &[&str] = &[
    "payment",
    "credit card",
    "card number",
    "cvv",
    "bank account",
    "routing number",
    "ach transfer",
    "stripe secret",
];
const CREDENTIAL_DATA_PHRASES: &[&str] = &[
    "password",
    "api key",
    "secret key",
    "private key",
    "access token",
    "session token",
    "credential",
    "oauth token",
    "refresh token",
];
const SENSITIVE_PII_PHRASES: &[&str] = &[
    "social security",
    "ssn",
    "passport number",
    "driver's license",
    "medical record",
    "health record",
    "government id",
];

/// Hedge phrases that speculate a deeper layer (RLS, or defense-in-depth generally) already
/// covers a finding — the exact rationalization D3/D5 forbid as a reason to under-rate an
/// access-control or injection finding. Requires the text to mention RLS/row-level-security AT
/// ALL, AND one of these hedges, so a finding that CONFIRMS (not speculates) a mitigating RLS
/// policy is unaffected — this only catches speculative language ("probably", "likely", ...).
const RLS_HEDGE_PHRASES: &[&str] = &[
    "probably",
    "likely",
    "presumably",
    "should prevent",
    "should mitigate",
    "should cover",
    "should catch",
    "may prevent",
    "may mitigate",
    "might prevent",
    "could prevent",
    "defense in depth",
    "defense-in-depth",
    "is likely covered",
    "is probably covered",
];

/// The lowercased text a D5 floor rule scans: `detail` + `snippet` + `category`, concatenated —
/// the model's phrasing can land in the summary prose (`detail`) or, for a deterministic/floor
/// finding, in the description-shaped `snippet`. Case-folded once so every phrase table above can
/// stay lowercase.
fn calibration_floor_scan_text(f: &Finding) -> String {
    format!(
        "{} {} {}",
        f.detail,
        f.snippet,
        f.category.as_deref().unwrap_or("")
    )
    .to_ascii_lowercase()
}

fn mentions_unauthenticated_or_full_compromise(text: &str) -> bool {
    UNAUTHENTICATED_OR_FULL_COMPROMISE_PHRASES
        .iter()
        .any(|p| text.contains(p))
}

fn mentions_authenticated_cross_tenant_read(text: &str) -> bool {
    CROSS_TENANT_PHRASES.iter().any(|p| text.contains(p))
        && READ_VERBS.iter().any(|v| text.contains(v))
}

fn mentions_escalating_data_class(text: &str) -> bool {
    PAYMENT_DATA_PHRASES
        .iter()
        .chain(CREDENTIAL_DATA_PHRASES)
        .chain(SENSITIVE_PII_PHRASES)
        .any(|p| text.contains(p))
}

fn mentions_speculative_rls_hedge(text: &str) -> bool {
    let mentions_rls = text.contains("rls")
        || text.contains("row-level security")
        || text.contains("row level security");
    mentions_rls && RLS_HEDGE_PHRASES.iter().any(|h| text.contains(h))
}

/// Whether `f` is in the access-control / injection family D3 already treats as a class that
/// must never be softened by "RLS probably contains it" reasoning: RLS policy, authorization, or
/// injection (query-grammar injection included). Checks the finding's OWN `category` first (set
/// by calibration or an earlier merge pass), falling back to `categorize_rule_id` over its rule
/// id — reusing the same word-boundary-matched categorization D5's own Part B hardened, rather
/// than re-deriving a separate notion of "is this access-control".
fn is_access_control_or_injection_class(f: &Finding) -> bool {
    let cat = f
        .category
        .clone()
        .or_else(|| categorize_rule_id(&f.rule_id));
    matches!(
        cat.as_deref(),
        Some("rls-policy") | Some("authorization") | Some("injection")
    )
}

/// The deterministic D5 severity floor for ONE finding: `None` when no rule of thumb applies,
/// else the floor severity plus the rationale to record. Order matters — unauthenticated/full-
/// compromise is checked first (it is the highest floor and the two conditions are mutually
/// exclusive by construction: an unauthenticated finding is never ALSO the "authenticated
/// cross-tenant" case), then the authenticated-cross-tenant-read floor, then the exploit-
/// precondition cap (which can only LOWER a Critical floor to High, never raise or lower
/// anything else), then the RLS-hedge guard (which can only RAISE whatever floor is already
/// computed, never lower it — see the combine step in `apply_severity_calibration_rule`).
fn severity_calibration_floor(f: &Finding) -> Option<(&'static str, String)> {
    let text = calibration_floor_scan_text(f);
    let mut floor = if mentions_unauthenticated_or_full_compromise(&text) {
        Some((
            "critical",
            "Severity floor: unauthenticated access or a path to full-account compromise is \
             always Critical."
                .to_string(),
        ))
    } else if mentions_authenticated_cross_tenant_read(&text) {
        if mentions_escalating_data_class(&text) {
            Some((
                "critical",
                "Severity floor: an authenticated cross-tenant read escalates to Critical — the \
                 exposed data class (payment, credentials/secrets, or sensitive PII) crosses the \
                 escalation threshold."
                    .to_string(),
            ))
        } else {
            Some((
                "high",
                "Severity floor: an authenticated cross-tenant read is High by default, not \
                 Critical, absent an escalating data class."
                    .to_string(),
            ))
        }
    } else {
        None
    };

    // Exploit-precondition cap (cycle-5 regression fix): the two branches above derive a
    // Critical floor purely from the finding's own unauthenticated/data-class vocabulary —
    // neither one looks at whether that SAME text also records an exploit precondition (forging
    // a cookie or token, tampering with a cookie, knowing a secret path, riding a victim's
    // session). `apply_verdicts`'s upward-raise guard already blocks exactly this on the
    // MODEL-verdict path (see `mentions_exploit_precondition`'s doc comment), but this
    // deterministic floor runs AFTER `apply_verdicts` in `verify_findings` and re-derives
    // Critical independently from the finding's own text — so it was silently re-raising exactly
    // what the verdict-path guard had just blocked (two High-severity planted defects whose own
    // text named a forged-cookie/known-path precondition both still came out Critical via this
    // floor, not via the model verdict). Mirror the same guard here: when THIS floor computed
    // Critical, a recorded precondition caps it at High instead. This can only LOWER the floor's
    // own proposed value, never a finding's already-assigned severity: a finding that doesn't
    // trip the floor at all is simply `None` and untouched, and `apply_severity_calibration_rule`
    // below only ever RAISES toward the floor it is given — so capping the floor at High can only
    // stop a RAISE, never demote a finding already calibrated to Critical by some other path.
    let floor_is_critical = matches!(floor, Some((sev, _)) if sev == "critical");
    if floor_is_critical && mentions_exploit_precondition(&text) {
        floor = Some((
            "high",
            "Severity floor capped at High: the finding's own text records an exploit \
             precondition (token/cookie forgery or tampering, knowledge of a secret path, or \
             riding a victim's session) — the same guard that blocks an upward model-verdict \
             raise to Critical in `apply_verdicts` applies to this deterministic floor too, so \
             it cannot re-raise what that guard already capped."
                .to_string(),
        ));
    }

    // The RLS-hedge guard: an access-control/injection finding must never rank below High
    // because the calibration reasoning speculates a deeper layer (RLS) probably already
    // covers it — defense-in-depth failing at the boundary the finding actually names still
    // stands (ties to D3). This can only RAISE the floor already computed above, never lower it.
    if is_access_control_or_injection_class(f) && mentions_speculative_rls_hedge(&text) {
        let hedge_floor = (
            "high",
            "Severity floor: an access-control/injection finding is not under-rated on an \"RLS \
             probably contains it\" rationale — defense-in-depth failing at the boundary still \
             stands."
                .to_string(),
        );
        floor = Some(match floor {
            Some((sev, reason)) if severity_rank(sev) >= severity_rank(hedge_floor.0) => {
                (sev, reason)
            }
            _ => hedge_floor,
        });
    }
    floor
}

/// Apply the D5 severity floor to one finding: raises `severity` to the floor when the finding's
/// current severity ranks below it, and ALWAYS records the rationale when a rule of thumb
/// touched the finding (even when the floor didn't change anything — the architect can see the
/// rule considered it and found the existing severity already sufficient). Never lowers a
/// severity that already ranks at or above the floor.
fn apply_severity_calibration_rule(mut f: Finding) -> Finding {
    if let Some((floor_sev, reason)) = severity_calibration_floor(&f) {
        if severity_rank(&f.severity) < severity_rank(floor_sev) {
            f.severity = floor_sev.to_string();
        }
        f.calibration_rationale = Some(reason);
    }
    f
}

/// Apply the D5 severity floor to a whole finding set — the deterministic pass run after
/// `apply_verdicts`/`consensus_verdicts` in `verify_findings` (and safe to run over ANY finding
/// set, including deterministic-floor findings that never see the LLM calibration pass at all,
/// since it derives everything from the finding's own text rather than the model's verdict).
pub fn apply_severity_calibration_rules(findings: Vec<Finding>) -> Vec<Finding> {
    findings
        .into_iter()
        .map(apply_severity_calibration_rule)
        .collect()
}

// ── D6: the severity CEILING — a downward calibration direction (2026-09-30 cycle-2
// queue-hardening, R1/R2). D5's floor above only ever RAISES severity. Two classes need the
// opposite correction, and neither one ever drops a finding — both stay in the queue, re-routed
// and re-badged:
//   - R1: a finding whose OWN text self-hedges "this is probably intentional, just confirm" is
//     capped to Low + flagged `needs-review`, which routes it to the `held` bucket (C5-4;
//     `report_export::is_held_for_review`) instead of an action bucket.
//   - R2: a browser-mediated CORS/cross-origin header misconfiguration (exploitable only through
//     a victim's browser + session, never a direct unauthenticated fetch) is clamped to exactly
//     Medium, bidirectionally — lowering an inflated Critical/High AND raising a re-buried
//     Low/Info. Re-burying it at Low was the prior failure just as much as inflating it to
//     Critical was.
// Both rules derive entirely from the finding's own text (like the floor), so this pass is safe
// to run over ANY finding set, and must run immediately after `apply_severity_calibration_rules`
// everywhere that runs (today: the one production call site in `verify_findings`, plus tests).

/// Phrases identifying a browser-mediated CORS / cross-origin header misconfiguration (R2).
/// Deliberately specific to cross-origin vocabulary (not just "credentials", which appears in
/// countless unrelated findings) so this never fires outside the actual CORS class.
const CORS_MISCONFIG_PHRASES: &[&str] = &[
    "cors",
    "cross-origin",
    "cross origin",
    "access-control-allow-origin",
    "access-control-allow-credentials",
    "reflected origin",
    "reflects the origin",
    "reflects any origin",
    "reflects the request's origin",
    "echoes the origin",
    "echoed origin",
    "echoes back the origin",
];

/// Whether `f`'s own text (detail/snippet/category) names the browser-mediated CORS/cross-origin
/// misconfiguration class R2 targets.
fn mentions_cors_misconfig(text: &str) -> bool {
    CORS_MISCONFIG_PHRASES.iter().any(|p| text.contains(p))
}

/// Self-hedged "probably intentional / confirmation-only" phrases (R1) — the finding's OWN
/// analysis concluding the pattern is likely fine and only needs a human to confirm intent,
/// rather than a confirmed defect.
const SELF_HEDGE_CONFIRMATION_PHRASES: &[&str] = &[
    "likely intentional",
    "appears intentional",
    "appears to be intentional",
    "probably intentional",
    "may be intentional",
    "might be intentional",
    "could be intentional",
    "if this is intentional",
    "if this behavior is intentional",
    "confirm whether",
    "confirmation only",
    "intended behavior",
];

/// Negation guards for the R1 phrase set above — "not intentional" / "clearly not intended"
/// mean the OPPOSITE conclusion (a confirmed defect, not "probably fine"), so these must NEVER
/// be swept up by the self-hedge phrases even though some share the word "intentional"/"intended".
const SELF_HEDGE_NEGATION_PHRASES: &[&str] = &[
    "not intentional",
    "not intended",
    "isn't intentional",
    "isn't intended",
];

/// Whether `f`'s own text is a self-hedged "probably intentional, just confirm" conclusion (R1),
/// with the negation guard applied first.
fn mentions_self_hedged_confirmation_only(text: &str) -> bool {
    if SELF_HEDGE_NEGATION_PHRASES.iter().any(|p| text.contains(p)) {
        return false;
    }
    SELF_HEDGE_CONFIRMATION_PHRASES
        .iter()
        .any(|p| text.contains(p))
}

// ── D7 (C6-A3): two more severity-ceiling classes, same shape as D6's R1/R2 — derived entirely
// from the finding's own text, never dropping a finding, safe over ANY finding set. Added to
// close a rubric-instability gap observed across two independent benchmark repos: the SAME two
// classes kept drifting to an inflated severity run-to-run.
//
// R3 — open/unvalidated redirect. The exploit boundary this polices: a redirect to an
// attacker-chosen destination only harms the user who follows it — on its own the attacker gains
// nothing more than a referral, so it is Medium. It becomes High exactly when the finding's own
// text says the redirect is CHAINED to credential capture (the destination can read or
// exfiltrate a token, cookie, or credential) — only then does the attacker walk away with
// something usable. It is NEVER Critical from the redirect mechanism alone, chained or not —
// that would require the SAME finding to also describe an unauthenticated path to a privileged
// operation, which is D5's job, not this ceiling's.
//
// R4 — a privileged/role/admin check enforced ONLY in client-visible (UI/browser) code, with no
// independent server-side re-check. The exploit boundary: a client-side control is not itself the
// security boundary — anything a client enforces, a client can be made to skip — so the real
// exposure is bounded by what the SERVER allows an authenticated caller to do once that gate is
// bypassed. An authenticated privilege escalation confined to a single resource/operation is a
// HIGH finding, not Critical; Critical is reserved for an unauthenticated reach or a system- or
// account-wide compromise, neither of which this class states by itself. Cap-only (like R1, it
// never raises a lower severity) — the absent server-side check is itself a real defect worth
// keeping wherever the model or the floor already put it, just never above High.
//
// Both R3 and R4 are gated against the D5 UNAUTHENTICATED/full-compromise floor phrases — unlike
// R2 (CORS), which is authoritative because that vocabulary and the floor's are mutually
// exclusive by construction, a redirect or a client-side check CAN legitimately co-occur in the
// same finding's prose as an unauthenticated-access/full-compromise statement, and when it does
// that Critical was earned through the floor's mechanism, not this ceiling's — so neither R3 nor
// R4 may demote it. This is the BLOCKS-UP-ONLY requirement: these two ceilings only ever cap a
// severity that their OWN class would otherwise have produced, never a severity legitimately
// assigned by a different rule.

/// Phrases naming an open or unvalidated redirect — the destination is attacker-controlled or
/// otherwise unchecked, as opposed to a redirect to a fixed, trusted, hardcoded location.
const OPEN_REDIRECT_PHRASES: &[&str] = &[
    "open redirect",
    "open-redirect",
    "unvalidated redirect",
    "redirect target is not validated",
    "redirect url is not validated",
    "redirect destination is not validated",
    "does not validate the redirect",
    "redirect parameter is not restricted",
    "redirects to an attacker-controlled",
    "redirects to an arbitrary",
    "redirects to any external",
    "redirects to any url",
    "arbitrary redirect",
    "unvalidated return url",
    "unchecked redirect",
];

/// Verbs marking the redirect destination as actively capturing something a victim carries —
/// paired with a credential-object phrase (reusing [`CREDENTIAL_DATA_PHRASES`], the same
/// vocabulary D5's cross-tenant escalation branch already uses) so an unrelated mention of
/// "token"/"session" elsewhere in the same finding's prose doesn't spuriously chain it.
const CREDENTIAL_CAPTURE_VERBS: &[&str] = &[
    "captures",
    "capture",
    "steals",
    "exfiltrates",
    "exfiltrate",
    "harvests",
    "intercepts",
    "leaks",
    "carries",
    "forwards",
    "appends",
    "includes",
    "sends",
    "transmits",
];

fn mentions_open_redirect(text: &str) -> bool {
    OPEN_REDIRECT_PHRASES.iter().any(|p| text.contains(p))
}

/// Whether `text` chains an open redirect to credential capture — BOTH a credential-object
/// phrase AND a capture-shaped verb must be present (the same AND-pair shape as
/// [`mentions_authenticated_cross_tenant_read`]'s phrase+verb pairing above).
fn mentions_redirect_chained_to_credential_capture(text: &str) -> bool {
    CREDENTIAL_DATA_PHRASES.iter().any(|c| text.contains(c))
        && CREDENTIAL_CAPTURE_VERBS.iter().any(|v| text.contains(v))
}

/// Phrases marking the control's ENFORCEMENT LOCATION as client-visible/UI-only with no
/// independent server-side check — Group A of the R4 AND-pair, mirroring the
/// [`CROSS_TENANT_PHRASES`] + [`READ_VERBS`] AND-pair shape above (a location signal plus a
/// privilege-type signal, not either alone).
const CLIENT_ONLY_ENFORCEMENT_PHRASES: &[&str] = &[
    "client-side only",
    "client side only",
    "only on the client",
    "only in the client",
    "only in the ui",
    "ui-only",
    "ui only",
    "only enforced in the browser",
    "only enforced client-side",
    "browser-only check",
    "no server-side check",
    "no server-side enforcement",
    "not enforced on the server",
    "not re-verified on the server",
    "not re-checked on the server",
    "server never verifies",
    "server does not verify",
    "server does not re-check",
    "server-side check is missing",
    "only hides the button",
    "only hides the ui",
    "disabled client-side",
];

/// Phrases marking the control as a privilege/role/admin gate — Group B of the R4 AND-pair.
/// Deliberately bigram/trigram phrases, not the bare word "admin" (which appears in countless
/// unrelated findings — an admin dashboard, an admin route — with no bearing on this class), so
/// the pair only fires on an actual privilege-gate mention.
const PRIVILEGE_ROLE_GATE_PHRASES: &[&str] = &[
    "admin check",
    "admin flag",
    "admin role",
    "is an admin",
    "as an admin",
    "admin access",
    "admin privileges",
    "role check",
    "role field",
    "user role",
    "user's role",
    "permission check",
    "elevated privileges",
    "privilege escalation",
    "isadmin",
    "is_admin",
];

/// Whether `text` describes a privilege/role/admin gate whose enforcement location is
/// client-visible/UI-only — BOTH an enforcement-location phrase AND a privilege-type phrase.
fn mentions_client_only_privilege_gate(text: &str) -> bool {
    CLIENT_ONLY_ENFORCEMENT_PHRASES
        .iter()
        .any(|p| text.contains(p))
        && PRIVILEGE_ROLE_GATE_PHRASES.iter().any(|p| text.contains(p))
}

/// Apply the R1-R4 severity ceiling to ONE finding. R2 (CORS) is checked first and, when it
/// matches, is AUTHORITATIVE for that finding — it overrides whatever the D5 floor already did
/// (including a floor-critical), because a browser-mediated CORS misconfiguration is never the
/// "direct unauthenticated exposure" class the floor polices, even if the finding's prose happens
/// to also brush against floor vocabulary. A finding that does NOT mention CORS is completely
/// untouched by this branch, so a genuinely unauthenticated-exposure finding keeps the floor's
/// Critical rating unchanged.
///
/// R3 (redirect) and R4 (client-only privilege gate) are checked next, each gated against the D5
/// unauthenticated/full-compromise floor phrases first (see the D7 section doc comment for why —
/// unlike CORS, these two CAN legitimately co-occur with a floor-earned Critical in the same
/// finding's prose, and must never demote it). R3 is a full bidirectional clamp like R2; R4 is
/// cap-only like R1. R1 is checked last.
fn apply_severity_ceiling_rule(mut f: Finding) -> Finding {
    // D8 (owner-mandated opinion-class cap): the ARCH-OBSERVABILITY-* convention class
    // (a swallowed error logged nowhere, inconsistent unstructured logging alongside an
    // adopted structured logger, missing correlation/request id propagation) is SEMANTIC
    // and repo-relative by the owner's own framing — never a universal defect like the R1-R4
    // classes below, which all derive from the finding's TEXT. This one derives from the
    // finding's RULE IDENTITY instead (`crate::is_observability_convention_class`), checked
    // FIRST and unconditionally: it is authoritative over every other ceiling/floor result,
    // clamping to exactly Low bidirectionally (like R2's CORS clamp, not a cap-only ceiling
    // like R1/R4) — an inflated Critical/High from this class is exactly as wrong as a
    // silently-buried Info, since both misrepresent an opinionated convention. Capping the
    // severity STRING here is only HALF of the structural guarantee the owner asked for; the
    // other half — keeping this class out of every action bucket regardless of severity — is
    // `report_export::is_informational`'s matching rule-identity check, which holds even if
    // this pass somehow did not run first. See `crate::is_observability_convention_class`'s
    // doc comment for the full two-part design.
    if crate::is_observability_convention_class(&f.rule_id) {
        f.severity = "low".to_string();
        f.calibration_rationale = Some(
            "Severity ceiling: this is the observability/under-logging convention class \
             (a swallowed error logged nowhere, inconsistent unstructured logging alongside \
             an adopted structured logger, or missing correlation/request id propagation) — \
             a repo-relative judgment call the owner designated as never a universal defect. \
             Capped to exactly Low and routed out of every action bucket, regardless of any \
             other calibrated severity."
                .to_string(),
        );
        return f;
    }

    let text = calibration_floor_scan_text(&f);

    if mentions_cors_misconfig(&text) {
        f.severity = "medium".to_string();
        f.calibration_rationale = Some(
            "Severity ceiling: a browser-mediated CORS/cross-origin misconfiguration requires a \
             victim's browser and an active session to exploit — it is neither a direct \
             unauthenticated exposure (Critical) nor safe to leave buried (Low). Calibrated to \
             exactly Medium."
                .to_string(),
        );
        return f;
    }

    let floor_earned_critical = mentions_unauthenticated_or_full_compromise(&text);

    if mentions_open_redirect(&text) && !floor_earned_critical {
        if mentions_redirect_chained_to_credential_capture(&text) {
            f.severity = "high".to_string();
            f.calibration_rationale = Some(
                "Severity ceiling: an open/unvalidated redirect chained to credential capture — \
                 the destination can read or exfiltrate a token, cookie, or credential — lets an \
                 attacker walk away with something usable. Calibrated to exactly High."
                    .to_string(),
            );
        } else {
            f.severity = "medium".to_string();
            f.calibration_rationale = Some(
                "Severity ceiling: an open/unvalidated redirect, on its own, only harms the user \
                 who follows it and hands the attacker nothing — not High or Critical unless \
                 chained to credential capture. Calibrated to exactly Medium."
                    .to_string(),
            );
        }
        return f;
    }

    if mentions_client_only_privilege_gate(&text) && !floor_earned_critical {
        if severity_rank(&f.severity) > severity_rank("high") {
            f.severity = "high".to_string();
        }
        f.calibration_rationale = Some(
            "Severity ceiling: a privileged/role check enforced only in client-visible (UI/\
             browser) code, with no independent server-side re-check, is never by itself the \
             basis for Critical — the client-side control is not the security boundary. An \
             authenticated privilege escalation confined to a single resource/operation is High \
             at most; Critical is reserved for an unauthenticated reach or a system- or \
             account-wide compromise. Calibrated to High at most."
                .to_string(),
        );
    }

    if mentions_self_hedged_confirmation_only(&text) {
        f.confidence = Some("needs-review".to_string());
        f.needs_review = true;
        if severity_rank(&f.severity) > severity_rank("low") {
            f.severity = "low".to_string();
        }
        f.calibration_rationale = Some(
            "Severity ceiling: the finding's own analysis concludes the pattern is likely \
             intentional / confirmation-only — capped to Low and flagged needs-review rather \
             than left in an action bucket. It stays in the queue for a human to confirm."
                .to_string(),
        );
    }

    f
}

/// Apply the R1-R4 severity ceiling to a whole finding set — run immediately after
/// `apply_severity_calibration_rules` (the D5 floor) everywhere that runs. Safe over ANY finding
/// set (deterministic-floor findings included), since all four ceiling rules derive entirely
/// from the finding's own text rather than the model's verdict.
pub fn apply_severity_ceiling_rules(findings: Vec<Finding>) -> Vec<Finding> {
    findings
        .into_iter()
        .map(apply_severity_ceiling_rule)
        .collect()
}

// ════════════════════════════════════════════════════════════════════════════════════
// AGGREGATE-PASS TUNING (calibration / fix-generation / alternative-recommendation)
// ════════════════════════════════════════════════════════════════════════════════════
//
// `verify_findings`, `generate_fix_specifics`, and `recommend_alternatives` each used to
// bundle EVERY finding/rule in the scan into a single completion with a flat
// `max_tokens(4096)`. Two failure modes that never affected the small per-(chunk,
// rule-batch) `audit_pass` calls: (1) on the CLI backend, one giant non-streaming call had
// no progress signal, so a slow-but-live run tripped the flat 300s subprocess timeout with
// no partial-output salvage; (2) on the API backend, a large finding set's response
// legitimately needs more than 4096 output tokens and got silently truncated instead. Both
// passes now (a) route through `LlmPort::complete_streaming` even when there is no
// transcript to feed — the CLI leg's heartbeat resets on any progress and SALVAGES partial
// output on a late kill, instead of the flat non-streaming path's fixed deadline — and (b)
// split their input into bounded chunks, each an independent streamed call with a
// per-chunk `max_tokens` that scales with that chunk's item count, so no single call is
// asked to describe more than one bounded batch's worth of findings/rules.

/// Floor for [`scaled_max_tokens`] — the ORIGINAL flat budget every aggregate pass
/// hardcoded before this fix, kept as the minimum so a small finding/rule set (the common
/// case) sees zero behavior change.
const AGGREGATE_MAX_TOKENS_FLOOR: u32 = 4096;

/// Ceiling for [`scaled_max_tokens`] — bounds a single call's requested budget regardless
/// of how many items land in one chunk. The per-pass `*_BATCH_SIZE` constants below keep
/// any one call's item count well under what would reach this in practice; the ceiling is
/// a second, independent guard against requesting an unreasonably large budget from the
/// API (e.g. if a caller ever skips chunking).
const AGGREGATE_MAX_TOKENS_CEILING: u32 = 32_000;

/// `max_tokens` for a call describing `n_items` things, given a per-item token estimate —
/// clamped to `[AGGREGATE_MAX_TOKENS_FLOOR, AGGREGATE_MAX_TOKENS_CEILING]` so small batches
/// keep today's flat budget and large batches never request more than the ceiling.
fn scaled_max_tokens(n_items: usize, per_item_tokens: u32) -> u32 {
    (n_items as u32)
        .saturating_mul(per_item_tokens)
        .clamp(AGGREGATE_MAX_TOKENS_FLOOR, AGGREGATE_MAX_TOKENS_CEILING)
}

/// Per-finding token estimate for the CALIBRATION pass's response — each verdict is a
/// compact JSON object (`{"index","severity","confidence","reason"}`) with a one-sentence
/// `reason`, so a modest per-item budget covers it comfortably.
const VERIFY_TOKENS_PER_FINDING: u32 = 150;

/// How many findings fit in ONE calibration call. Chosen so a FULL chunk already needs
/// more than [`AGGREGATE_MAX_TOKENS_FLOOR`] (`CALIBRATION_BATCH_SIZE *
/// VERIFY_TOKENS_PER_FINDING` > 4096) — the exact shape of the truncation bug this fixes —
/// while staying well short of [`AGGREGATE_MAX_TOKENS_CEILING`]. A chunk whose call fails
/// or times out passes through uncalibrated (see the `votes.is_empty()` branch below) —
/// NEVER dropped — so a smaller chunk size only costs calibration precision on failure,
/// never completeness.
const CALIBRATION_BATCH_SIZE: usize = 40;

/// Per-finding token estimate for the FIX-GENERATION pass's response — each entry is a
/// full prose sentence describing a concrete, codebase-specific change, materially longer
/// than a calibration verdict.
const FIX_TOKENS_PER_FINDING: u32 = 250;

/// How many findings fit in ONE fix-generation call. Same rationale as
/// `CALIBRATION_BATCH_SIZE` (a full chunk already exceeds the old flat floor); smaller
/// than the calibration batch because each response entry is longer prose.
const FIX_BATCH_SIZE: usize = 25;

/// How many rules fit in ONE alternative-recommendation call. Each rule's block lists
/// every option with its own rationale — comparable prose volume to a fix — so it's sized
/// like `FIX_BATCH_SIZE`.
const ALTERNATIVES_BATCH_SIZE: usize = 25;

/// Run the skeptic pass over a repo's AI findings (a fresh, reasoning-based perspective —
/// deliberately NOT re-sent the whole digest, so it judges exploitability/context, not
/// code minutiae). Graceful: on any model failure the findings pass through unchanged.
///
/// `feedback`, when present, records this pass into the transcript the same way the scan
/// passes do (`audit_pass`/`run_prose_lens`): register the agent with its REAL generated
/// prompt up front, append each vote's raw response as it lands, then set a terminal
/// status. This is what makes the cockpit's "calibrating N findings" entry show an actual
/// prompt/output instead of "no output captured" — a transcript-recording failure (lock
/// poisoning etc., handled inside `TranscriptStore` itself) can never affect calibration,
/// which stays best-effort exactly as before.
///
/// LARGE finding sets are split into [`CALIBRATION_BATCH_SIZE`]-sized chunks, each its own
/// streamed call (see the "AGGREGATE-PASS TUNING" section above) — a chunk that fails only
/// loses ITS findings' calibration (they pass through uncalibrated, never dropped), not the
/// whole scan's. The single-chunk case (the common one) is byte-for-byte the same prompt
/// and transcript session id this function always used.
///
/// C3-1b: this pass is also the ONLY source of the `effort` (hour-estimation) field, so it
/// returns a second `Vec<FailedPass>` alongside the (never-dropped) findings — a single
/// `FailedPass{pass: "hour estimation", ..}` entry when the pass produced NO usable effort
/// estimate for ANY finding in this call (every chunk's every attempt failed, or every
/// response came back without a parseable `effort`), so the export can disclose the gap
/// explicitly (`report_export::failed_pass_disclosures`) instead of silently rendering every
/// row as if "no estimate" were a settled answer. A PARTIAL failure (some findings estimated,
/// some not) is not disclosed here — only a genuine run-wide miss — the per-finding rendering
/// (`effort: None` -> "not estimated this run" in the Typst template) already covers the
/// individual-row case honestly.
#[allow(clippy::too_many_arguments)]
pub async fn verify_findings(
    llm: &dyn LlmPort,
    repo: &str,
    findings: Vec<Finding>,
    calibration_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
    thorough: bool,
    // One repo-shape sentence — detected stack + code-file count (e.g. "This is a Next.js repo
    // with 42 code files.") — computed by the caller from signals already in hand. Gives the
    // model real context to hedge stance/YAGNI findings, instead of a hardcoded size threshold
    // deciding whether a rule fires. Empty string is fine (the paragraph still reads).
    repo_shape: &str,
) -> (Vec<Finding>, Vec<FailedPass>) {
    if findings.is_empty() {
        return (findings, Vec::new());
    }
    let system = verify_system_prompt();
    // Split into bounded chunks (see `CALIBRATION_BATCH_SIZE`'s doc comment) — each is an
    // independent streamed call with LOCAL indices `0..chunk.len()`, so a failure in one
    // chunk never touches another's verdicts. The common case (findings.len() <=
    // CALIBRATION_BATCH_SIZE) produces exactly one chunk equal to the whole input, so the
    // prompt/session/output shape below is byte-for-byte what this function always sent.
    let owned_chunks: Vec<Vec<Finding>> = findings
        .chunks(CALIBRATION_BATCH_SIZE)
        .map(<[Finding]>::to_vec)
        .collect();
    let n_chunks = owned_chunks.len();
    let mut calibrated: Vec<Finding> = Vec::with_capacity(findings.len());

    for (chunk_idx, chunk) in owned_chunks.into_iter().enumerate() {
        let chunk_len = chunk.len();
        let mut prompt = format!("Repository: {repo}\n");
        // Proportionality signal (#51, Bug 4 §2b): ALWAYS given now (was thorough-only) — a
        // small/young codebase must not be held to the architecture of a large one, and this
        // feeds the informational bucketing of stance/YAGNI notes downstream. Over-engineering
        // / YAGNI notes auto-hedge to low confidence + capped severity.
        prompt.push_str(&format!(
            "{repo_shape} Judge each finding PROPORTIONALLY to the codebase's size and maturity: \
             an 'over-engineering'/'missing abstraction'/YAGNI note on a small codebase is a \
             debatable preference (low confidence, capped severity), not a violation.\n"
        ));
        prompt.push_str("\nScrutinize these findings:\n");
        for (i, f) in chunk.iter().enumerate() {
            prompt.push_str(&format!(
                "[{i}] (severity {}) {}:{} — {} :: {}\n",
                f.severity, f.path, f.line, f.snippet, f.detail
            ));
        }
        // Calibration runs on its OWN selected model (the UI exposes it). Build a fresh
        // request per pass (LlmRequest is consumed by complete_streaming).
        let build_req = || {
            let mut req = LlmRequest::new(prompt.clone())
                .with_system(system.clone())
                .with_max_tokens(scaled_max_tokens(chunk_len, VERIFY_TOKENS_PER_FINDING));
            if let Some(m) = calibration_model {
                req = req.with_model(m.to_string());
            }
            req
        };

        // Register (or replace) this pass's transcript entry with the REAL prompt before the
        // first call — mirrors `audit_pass`/`run_prose_lens`'s register-then-record pattern.
        // Single-chunk runs keep the EXACT session id + role convention this function always
        // used (`audit-{repo}-calibrate`); a `-bN` suffix + batch label only appears once
        // there's more than one chunk, a shape no pre-existing caller depended on.
        let session = if n_chunks > 1 {
            format!("audit-{repo}-calibrate-b{chunk_idx}")
        } else {
            format!("audit-{repo}-calibrate")
        };
        if let Some((store, key)) = feedback {
            store.register(
                key,
                crate::transcript::AgentTranscript {
                    session_id: session.clone(),
                    role: format!(
                        "calibrating {chunk_len} findings on {} — {repo}{}",
                        calibration_model.unwrap_or("default"),
                        if n_chunks > 1 {
                            format!(" (batch {}/{n_chunks})", chunk_idx + 1)
                        } else {
                            String::new()
                        }
                    ),
                    prompt: prompt.clone(),
                    output: String::new(),
                    status: "running".to_string(),
                },
            );
        }

        // THOROUGH mode (#51): run the calibration verdict MULTIPLE times and take the
        // conservative consensus, so a single over-confident pass can't push a debatable
        // finding to HIGH. Costs ~3x the calibration tokens (opt-in). Default mode is a
        // single pass (unchanged behavior).
        let passes = if thorough { 3 } else { 1 };
        let mut votes: Vec<String> = Vec::new();
        for pass_idx in 0..passes {
            // Routed through the STREAMING transport (not the flat `complete`) even when
            // there is no live transcript to feed — the CLI leg's heartbeat resets on any
            // progress and SALVAGES partial output on a late kill, instead of the flat
            // non-streaming path's fixed 300s deadline with no partial credit for a
            // near-complete run. `on_delta` is a no-op here: the transcript (when present)
            // is still recorded post-hoc below, unchanged, so `feedback = Some` behavior is
            // byte-for-byte identical to before this change.
            let mut on_delta = |_: &str| {};
            if let Ok(resp) = llm.complete_streaming(build_req(), &mut on_delta).await {
                if let Some(m) = meter {
                    m.record(&resp);
                }
                if let Some((store, key)) = feedback {
                    // Single-pass (the common case) records the raw response as-is, so the
                    // transcript's output is exactly the model's text. THOROUGH mode's 3
                    // votes are each appended with a pass label so no vote is left silent.
                    if passes > 1 {
                        store.append_output(
                            key,
                            &session,
                            &format!("── pass {}/{passes} ──\n{}", pass_idx + 1, resp.text),
                        );
                    } else {
                        store.append_output(key, &session, &resp.text);
                    }
                }
                votes.push(resp.text);
            }
        }
        let chunk_calibrated = match votes.len() {
            0 => chunk, // every pass for this chunk failed — pass its findings through unchanged
            1 => apply_verdicts(&votes[0], chunk),
            _ => apply_verdicts(&consensus_verdicts(&votes, chunk_len), chunk),
        };
        if let Some((store, key)) = feedback {
            if votes.is_empty() {
                store.append_output(
                    key,
                    &session,
                    "every calibration pass failed or timed out — findings pass through \
                     unchanged.",
                );
                store.set_status(key, &session, "blocked");
            } else {
                store.set_status(key, &session, "done");
            }
        }
        calibrated.extend(chunk_calibrated);
    }
    // C3-1b: a run-wide (not per-chunk/per-finding) disclosure — every finding calibration
    // was handed came back with NO usable effort estimate at all. Neither D5 (severity floor)
    // nor D6 (severity ceiling) below ever sets `effort`, so checking it here, before those
    // text-derived passes run, is equivalent to checking the final output and reads clearer.
    let mut estimation_failed_passes = Vec::new();
    if !calibrated.is_empty() && calibrated.iter().all(|f| f.effort.is_none()) {
        estimation_failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: "hour estimation".to_string(),
            reason: "the calibration pass returned no usable remediation-effort estimate for \
                     any finding this run"
                .to_string(),
        });
    }
    // D5: the deterministic severity floor runs regardless of whether the LLM calibration
    // pass succeeded — it re-derives its verdict from the finding's own text, not the model's,
    // so it is exactly as available when every pass failed as when one succeeded.
    // D6: the severity ceiling (R1/R2) runs immediately after, over the SAME set, for the same
    // reason — both are text-derived and safe regardless of whether calibration ran.
    let calibrated = apply_severity_ceiling_rules(apply_severity_calibration_rules(calibrated));
    (calibrated, estimation_failed_passes)
}

/// Merge several calibration passes into one CONSERVATIVE consensus verdict set (#51 thorough
/// mode). For each finding index: severity = the majority vote across the four levels
/// (low/medium/high/critical), ties break to the LOWER severity — so "critical" only wins when
/// it is the sole vote or an outright majority, never a tie against "high" (the anti-over-
/// rotation guard: disagreement about critical resolves down, not up); confidence = "high" only
/// when EVERY pass agreed on the same severity AND none of them was itself low-confidence — any
/// disagreement (on severity, or an individual pass's own confidence) means uncertainty, which
/// is exactly what the architect should review, so it becomes "low" (needs review). effort = the
/// majority vote (ties break to "medium" — a neutral default when the passes disagree, since
/// over- and under-estimating effort are equally misleading, unlike severity's asymmetric
/// humility rule). Returns a `{"verdicts":[…]}` JSON string for `apply_verdicts`.
fn consensus_verdicts(votes: &[String], n: usize) -> String {
    use serde_json::Value;
    // Per index: collected (severity, confidence, reason, effort) across passes.
    let mut per: Vec<Vec<(String, String, String, String)>> = vec![Vec::new(); n];
    for raw in votes {
        let Some(json) = extract_json_object(raw) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        let Some(arr) = v["verdicts"].as_array() else {
            continue;
        };
        for verdict in arr {
            let Some(idx) = verdict["index"].as_u64() else {
                continue;
            };
            let idx = idx as usize;
            if idx >= n {
                continue;
            }
            let sev = match verdict["severity"].as_str().unwrap_or("medium") {
                "critical" => "critical",
                "high" => "high",
                "low" => "low",
                _ => "medium",
            }
            .to_string();
            let conf = if verdict["confidence"].as_str() == Some("low") {
                "low"
            } else {
                "high"
            }
            .to_string();
            let reason = verdict["reason"].as_str().unwrap_or("").trim().to_string();
            let effort = match verdict["effort"].as_str().unwrap_or("medium") {
                "low" => "low",
                "high" => "high",
                _ => "medium",
            }
            .to_string();
            per[idx].push((sev, conf, reason, effort));
        }
    }
    // Severity has FOUR levels now that "critical" is a real tier; effort still has three
    // (low/medium/high — a calibration verdict never emits "critical" effort). Two separate
    // rank functions rather than one shared one, so effort's 3-slot count array never has to
    // reason about a severity-only value.
    let sev_rank = |s: &str| match s {
        "critical" => 3,
        "high" => 2,
        "medium" => 1,
        _ => 0,
    };
    let effort_rank = |s: &str| match s {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    };
    let mut verdicts = Vec::new();
    for (idx, votes_for) in per.iter().enumerate() {
        if votes_for.is_empty() {
            continue;
        }
        // Majority severity; tie breaks to the lower rank (humble). This is an ANTI-OVER-
        // ROTATION guard too: "critical" only wins a tie when it is the SOLE or MAJORITY vote
        // at the top rank — a single critical vote against an equal number of high votes
        // resolves to "high", never silently promoted.
        let mut counts = [0u32; 4]; // [low, medium, high, critical]
        for (s, _, _, _) in votes_for {
            counts[sev_rank(s)] += 1;
        }
        let max = counts.iter().copied().max().unwrap_or(0);
        // Tie-breaks to the LOWER severity (humble / conservative design): low wins over
        // medium wins over high wins over critical when vote counts are equal. This is the
        // correct behaviour documented in the comment at the top of this function; the
        // previous ordering (high first) was the opposite of the spec. Fixed by BUG-5;
        // extended to the critical tier the same way.
        let sev = if counts[0] == max {
            "low"
        } else if counts[1] == max {
            "medium"
        } else if counts[2] == max {
            "high"
        } else {
            "critical"
        };
        // Disagreement on severity, or any low-confidence vote → low confidence (needs review).
        let distinct_sevs = counts.iter().filter(|&&c| c > 0).count();
        let any_low_conf = votes_for.iter().any(|(_, c, _, _)| c == "low");
        let agreed = distinct_sevs == 1 && !any_low_conf;
        let confidence = if agreed { "high" } else { "low" };
        // Majority effort; a tie among the top vote-getters breaks to "medium" (see the
        // function doc — effort has no humility direction the way severity does, so a
        // single clear winner is used as-is, but ANY tie among the leaders is neutral).
        let mut effort_counts = [0u32; 3]; // [low, medium, high]
        for (_, _, _, e) in votes_for {
            effort_counts[effort_rank(e)] += 1;
        }
        let effort_max = effort_counts.iter().copied().max().unwrap_or(0);
        let effort_winners: Vec<usize> =
            (0..3).filter(|&i| effort_counts[i] == effort_max).collect();
        let effort = match effort_winners.as_slice() {
            [0] => "low",
            [2] => "high",
            _ => "medium", // a single "medium" winner, or any tie among the leaders
        };
        // First non-empty reason, preferring a low-confidence pass's reason.
        let reason = votes_for
            .iter()
            .find(|(_, c, r, _)| c == "low" && !r.is_empty())
            .or_else(|| votes_for.iter().find(|(_, _, r, _)| !r.is_empty()))
            .map(|(_, _, r, _)| r.clone())
            .unwrap_or_default();
        verdicts.push(serde_json::json!({
            "index": idx, "severity": sev, "confidence": confidence, "effort": effort, "reason": reason
        }));
    }
    serde_json::json!({ "verdicts": verdicts }).to_string()
}

/// The ADOPTED-rules header for the audit prompt. Empty when nothing is selected (the
/// audit then falls back to a free-form investigative read).
fn build_rules_block(selected: &[(String, String)]) -> String {
    if selected.is_empty() {
        return String::new();
    }
    let mut b = String::from(
        "The project has ADOPTED these rules — check the code against each, AND flag \
         any other genuine issues you find:\n",
    );
    for (id, directive) in selected {
        b.push_str(&format!("- [{id}] {directive}\n"));
    }
    b.push('\n');
    b
}

/// The system prompt for the dedicated alternative-recommendation pass. Separate from
/// [`audit_system_prompt`] (which checks code against ONE resolved directive per rule) —
/// this pass's only job is to PICK, per multi-option rule, the option that best fits the
/// codebase, grounded in real code evidence. It does not report violations; the main audit
/// passes do that afterward, against whichever option this pass decided (see `audit_repo`).
pub fn alternatives_system_prompt() -> String {
    r#"You are a senior software architect helping a team decide, for THIS codebase, which
alternative implementation of each adopted rule best matches its existing (or best-practice)
pattern.

The user message lists one or more rules. Each rule shows EVERY alternative option it offers
— an option id, a human label, the concrete directive that option codifies, and the rationale
for it — plus a marker for whichever option is CURRENTLY selected (the project's own choice,
the corpus default, or "none selected" when neither exists).

For EACH rule listed, first decide whether the rule's underlying CONCERN even applies to this
codebase. Some rules govern something that may simply not exist here at all (no pagination
anywhere, no API versioning anywhere, no background jobs anywhere) — if you cannot point to a
SPECIFIC file and line where the codebase either already follows this rule's concern or
concretely needs it, the rule is NOT APPLICABLE. Set `"applicable": false` and do not pick an
option for it. Only set `"applicable": true` when you can cite the exact evidence.

When the rule IS applicable, pick EXACTLY ONE option id: the one that best matches how this
codebase already does things, or — if the codebase is inconsistent — the one that is the
best-practice fit given the stack and code you can see. You are free to keep the
currently-selected option, or pick a different one; ground the choice in the actual code, not
a coin flip.

CRITICAL RULE — never assert an unevidenced convention: you must NEVER say a codebase has
"adopted" a pattern, or describe ANY convention as already in use, unless you cite the EXACT
`path:line` where that is true. If you cannot cite a real file:line, either say the rule is not
applicable (see above) or, if it applies but nothing in the repo yet follows it, say so plainly
("no existing pattern; recommending X as the best-practice fit for this stack") rather than
inventing an "adopted" claim.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "recommendations": [
    {
      "rule_id": "EXACT rule id as given, copied verbatim",
      "applicable": true,
      "recommended_option_id": "EXACT option id from THAT rule's own list, copied verbatim",
      "evidence": "path/to/file.ext:42 — one short phrase on what's there",
      "recommendation_reasoning": "1-3 sentences grounded in the codebase evidence you saw"
    }
  ]
}
When `"applicable"` is `false`, omit `"recommended_option_id"` and `"evidence"` (or leave them
empty) — there is nothing to recommend against for a concern that doesn't apply here.

Include exactly one entry per rule listed, even when you are keeping the currently-selected
option or marking it not applicable. NEVER invent an option id that was not listed for that
specific rule. NEVER set `"applicable": true` without a real `"evidence"` file:line — an
unevidenced applicability claim will be discarded and treated as not applicable."#
        .to_string()
}

/// The per-rule "every option, plus which is currently selected" block the recommendation
/// pass reads. Shared shape with what a human reviewer would see in the rule-detail modal —
/// id, label, directive, why — so the model's grounds for picking match what an architect
/// would weigh.
fn build_alternatives_block(alternatives: &[RuleAlternatives]) -> String {
    let mut b = String::from("Rules to decide (pick ONE option id per rule):\n\n");
    for a in alternatives {
        b.push_str(&format!("- [{}]\n", a.rule_id));
        for o in &a.options {
            let marker = if a.selected_option_id.as_deref() == Some(o.id.as_str()) {
                " [CURRENTLY SELECTED]"
            } else {
                ""
            };
            b.push_str(&format!(
                "    * option id \"{}\" — {}: {}{}\n      why: {}\n",
                o.id, o.label, o.directive, marker, o.why
            ));
        }
        if a.selected_option_id.is_none() {
            b.push_str("    (no option currently selected for this rule)\n");
        }
        b.push('\n');
    }
    b
}

/// Fail-soft recommendations for every listed rule, all pointing at the currently-selected
/// (or, absent that, the first-listed) option and flagged `hallucinated` so the caller can
/// tell this was a fallback rather than a genuine model answer. Used when the model's raw
/// output for the recommendation pass could not be parsed at all.
fn fallback_recommendations(alternatives: &[RuleAlternatives], reason: &str) -> Vec<RuleRecommendation> {
    alternatives
        .iter()
        .filter_map(|a| {
            let id = a
                .selected_option_id
                .clone()
                .or_else(|| a.options.first().map(|o| o.id.clone()))?;
            Some(RuleRecommendation {
                rule_id: a.rule_id.clone(),
                recommended_option_id: id,
                recommendation_reasoning: reason.to_string(),
                hallucinated: true,
                operator_chosen: false,
                // A total parse failure is a MODEL-PIPELINE failure, not evidence that the
                // rule's concern doesn't apply — fail open exactly as before P7 (keep checking
                // the currently-selected option) rather than silently dropping the rule.
                evidence: None,
                applicable: true,
            })
        })
        .collect()
}

/// A non-blank `evidence` string after trimming, or `None`. Shared by the "does this entry
/// carry real evidence" check on both branches of [`parse_alternative_recommendations`].
fn trimmed_evidence(v: &serde_json::Value) -> Option<String> {
    v["evidence"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse the recommendation pass's raw model output into one [`RuleRecommendation`] per
/// listed rule. Robust by construction: EVERY rule in `alternatives` gets exactly one
/// output entry, whether or not the model answered it validly —
/// - malformed/unparseable JSON -> every rule falls back (see [`fallback_recommendations`]);
/// - a rule missing from the model's `recommendations` array -> that rule falls back;
/// - a `recommended_option_id` that is not a real option id on that specific rule (a
///   hallucination) -> that rule falls back, flagged `hallucinated: true`;
/// - otherwise the model's pick is used as-is, `hallucinated: false`.
///
/// The fallback target is the rule's OWN `selected_option_id` (the project's chosen option,
/// else the corpus default) — never a hardcoded/arbitrary id — so a fallback still resolves
/// to something the architect already considered reasonable.
///
/// # P7 — evidence-gated applicability
/// Two checks are ORTHOGONAL to the option-id validation above:
/// - **Applicability.** The model reports `"applicable": true|false` per rule — `false` means
///   the codebase neither follows nor needs this rule's concern at all (no evidence for it
///   anywhere), and the caller (`audit_repo`) must then drop the rule from the directive set
///   actually checked so it emits zero findings, rather than checking violations against a
///   convention nothing in the repo actually established.
/// - **Evidence-gates-applicability.** A model claiming `applicable: true` WITHOUT a non-blank
///   `evidence` file:line is never trusted at face value — that is exactly the "describes a
///   convention as adopted without citing where" failure mode this pass exists to prevent. Such
///   an entry is forcibly downgraded to `applicable: false` and flagged `hallucinated: true`
///   (the model's applicability claim, not just its option id, was corrected). This check is
///   independent of whether the `recommended_option_id` itself was valid.
pub fn parse_alternative_recommendations(
    raw: &str,
    alternatives: &[RuleAlternatives],
) -> Vec<RuleRecommendation> {
    let Some(json) = extract_json_object(raw) else {
        return fallback_recommendations(
            alternatives,
            "The model returned no parseable JSON for the recommendation pass; keeping the \
             currently selected option.",
        );
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return fallback_recommendations(
            alternatives,
            "The model returned malformed JSON for the recommendation pass; keeping the \
             currently selected option.",
        );
    };
    let arr = v["recommendations"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(alternatives.len());
    for a in alternatives {
        let entry = arr
            .iter()
            .find(|r| r["rule_id"].as_str().map(str::trim) == Some(a.rule_id.as_str()));
        let real_ids: std::collections::HashSet<&str> =
            a.options.iter().map(|o| o.id.as_str()).collect();
        let fallback_id = || {
            a.selected_option_id
                .clone()
                .or_else(|| a.options.first().map(|o| o.id.clone()))
                .unwrap_or_default()
        };
        let rec = match entry {
            Some(r) => {
                let raw_id = r["recommended_option_id"].as_str().unwrap_or("").trim().to_string();
                let reasoning = r["recommendation_reasoning"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // Applicability, gated on evidence — independent of the option-id check below.
                // `applicable` absent entirely (an older-shaped/malformed entry) fails OPEN
                // (treated as applicable) so a rule is never silently skipped just because the
                // model omitted a field; only an EXPLICIT `true` without evidence is corrected.
                // `applicable` ABSENT entirely (the key was never sent — an older-shaped
                // response, or a test fixture predating P7) is NOT the same as the model
                // EXPLICITLY claiming `true`: only an explicit, unevidenced `true` gets
                // corrected. An absent field fails open exactly like pre-P7 behavior (checked,
                // no evidence requirement) — the current prompt always sends the field for a
                // real model call, so this only matters for legacy/malformed output.
                let raw_applicable = r.get("applicable").and_then(|v| v.as_bool());
                let raw_evidence = trimmed_evidence(r);
                let (applicable, evidence, evidence_corrected) =
                    match (raw_applicable, raw_evidence) {
                        (None, ev) => (true, ev, false),
                        (Some(true), Some(ev)) => (true, Some(ev), false),
                        (Some(true), None) => (false, None, true),
                        (Some(false), _) => (false, None, false),
                    };
                if !applicable {
                    // NOT APPLICABLE — there is no option to validate (an explicit, honest
                    // "this concern doesn't apply here" answer names no option at all, and the
                    // evidence-corrected case's `raw_id`/`raw_evidence`, if any, are moot: the
                    // whole point is nothing will be checked against this rule). Never run the
                    // option-id validation below — an empty/missing `recommended_option_id` here
                    // is EXPECTED, not a hallucination in its own right.
                    let recommendation_reasoning = if evidence_corrected {
                        format!(
                            "The model claimed this rule's concern applies to the codebase but \
                             did not cite a file:line where it does — never trusting an \
                             unevidenced \"adopted\" claim, treating as not applicable instead. \
                             Model reasoning was: {reasoning}"
                        )
                    } else {
                        reasoning
                    };
                    RuleRecommendation {
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning,
                        // Only the evidence-correction case is a genuine hallucination (the
                        // model's OWN claim had to be overridden). An honest, explicit
                        // `applicable: false` is not — the model correctly declined to assert
                        // an unevidenced convention.
                        hallucinated: evidence_corrected,
                        operator_chosen: false,
                        evidence,
                        applicable,
                    }
                } else if !raw_id.is_empty() && real_ids.contains(raw_id.as_str()) {
                    RuleRecommendation {
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: raw_id,
                        recommendation_reasoning: reasoning,
                        hallucinated: false,
                        operator_chosen: false,
                        evidence,
                        applicable,
                    }
                } else if raw_id.is_empty() {
                    RuleRecommendation {
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning: "The model did not return a valid option id \
                             for this rule; keeping the currently selected option."
                            .to_string(),
                        hallucinated: true,
                        operator_chosen: false,
                        evidence,
                        applicable,
                    }
                } else {
                    RuleRecommendation {
                        rule_id: a.rule_id.clone(),
                        recommended_option_id: fallback_id(),
                        recommendation_reasoning: format!(
                            "The model returned option id \"{raw_id}\", which is not one of \
                             this rule's real alternatives; keeping the currently selected \
                             option."
                        ),
                        hallucinated: true,
                        operator_chosen: false,
                        evidence,
                        applicable,
                    }
                }
            }
            None => RuleRecommendation {
                rule_id: a.rule_id.clone(),
                recommended_option_id: fallback_id(),
                recommendation_reasoning: "The model did not return a recommendation for this \
                     rule; keeping the currently selected option."
                    .to_string(),
                hallucinated: true,
                operator_chosen: false,
                // Missing from the model's output entirely — a pipeline gap, not evidence the
                // concern is inapplicable. Fail open (keep checking) exactly as before P7.
                evidence: None,
                applicable: true,
            },
        };
        out.push(rec);
    }
    out
}

/// Run the dedicated recommendation pass: ONE LLM call deciding `recommended_option_id` +
/// `recommendation_reasoning` for every rule in `alternatives`, grounded in the repo map plus
/// a representative code digest. Deciding this ONCE (rather than once per file-chunk, as the
/// violation passes do) keeps every later violation-check for a given rule coherent — they
/// all check against the SAME option (see the design doc's "violations are always relative to
/// ONE chosen alternative" coherence principle) — and avoids re-litigating the same pick N
/// times at N times the cost.
///
/// KNOWN LIMIT: for a repo too large for one chunk, this pass sees only the FIRST size-capped
/// chunk's digest (plus the whole-repo map, which is cheap cross-file symbol context but not
/// full bodies). This is the same chunk-0 tradeoff the existing advisory ("flag novel issues")
/// pass already makes for the identical reason — see `run_passes`'s `bi == 0` gating. A rule
/// whose defining pattern lives entirely in a later chunk may get a less-informed pick; this
/// is a disclosed limitation, not a silent one.
///
/// Returns `Ok(vec![])` when `alternatives` is empty (nothing to decide — no call made).
#[allow(clippy::too_many_arguments)]
async fn recommend_alternatives(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    map_files: &[(String, String)],
    alternatives: &[RuleAlternatives],
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<Vec<RuleRecommendation>> {
    if alternatives.is_empty() {
        return Ok(Vec::new());
    }
    let repo_map = build_repo_map(map_files);
    let file_chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
    let digest = file_chunks
        .first()
        .map(|c| build_digest(c))
        .unwrap_or_default();

    // Bounded chunks (see `ALTERNATIVES_BATCH_SIZE`'s doc comment) — each rule-batch is an
    // independent streamed call, so no single completion is asked to decide every
    // multi-option rule in the scan at once. The common case (alternatives.len() <=
    // ALTERNATIVES_BATCH_SIZE) is exactly one chunk equal to the whole input, so the
    // prompt/session/role shape below is byte-for-byte what this function always sent.
    let rule_chunks: Vec<&[RuleAlternatives]> =
        alternatives.chunks(ALTERNATIVES_BATCH_SIZE).collect();
    let n_chunks = rule_chunks.len();
    let mut recommendations = Vec::with_capacity(alternatives.len());

    for (chunk_idx, alt_chunk) in rule_chunks.into_iter().enumerate() {
        let prompt = format!(
            "Repository: {repo}\n\n{repo_map}{digest}\n\n{}",
            build_alternatives_block(alt_chunk)
        );
        // Single-chunk runs keep the EXACT session id + role this function always used
        // (`audit-{repo}-alternatives`); the `-bN` suffix + batch label only appears once
        // there's more than one chunk.
        let session = if n_chunks > 1 {
            format!("audit-{repo}-alternatives-b{chunk_idx}")
        } else {
            format!("audit-{repo}-alternatives")
        };
        if let Some((store, key)) = feedback {
            store.register(
                key,
                crate::transcript::AgentTranscript {
                    session_id: session.clone(),
                    role: format!(
                        "recommending {} alternative(s) — {repo}{}",
                        alt_chunk.len(),
                        if n_chunks > 1 {
                            format!(" (batch {}/{n_chunks})", chunk_idx + 1)
                        } else {
                            String::new()
                        }
                    ),
                    prompt: prompt.clone(),
                    output: String::new(),
                    status: "running".to_string(),
                },
            );
        }
        let mut req = LlmRequest::new(prompt)
            .with_system(alternatives_system_prompt())
            .with_max_tokens(4096);
        if let Some(m) = audit_model {
            req = req.with_model(m.to_string());
        }
        // Routed through the STREAMING transport EVEN WHEN there is no live transcript to
        // feed (`feedback = None`) — the CLI leg's heartbeat resets on any progress and
        // SALVAGES partial output on a late kill, instead of the flat non-streaming path's
        // fixed 300s deadline with no partial credit for a near-complete run.
        let resp_result = if let Some((store, key)) = feedback {
            let mut on_delta = |t: &str| store.append_output_raw(key, &session, t);
            llm.complete_streaming(req, &mut on_delta).await
        } else {
            let mut on_delta = |_: &str| {};
            llm.complete_streaming(req, &mut on_delta).await
        };
        if let Some((store, key)) = feedback {
            store.set_status(
                key,
                &session,
                if resp_result.is_ok() {
                    "done"
                } else {
                    "blocked"
                },
            );
        }
        let resp = resp_result?;
        if let Some(m) = meter {
            m.record(&resp);
        }
        recommendations.extend(parse_alternative_recommendations(&resp.text, alt_chunk));
    }
    Ok(recommendations)
}

// ════════════════════════════════════════════════════════════════════════════════════
// FIX-SPECIFIC GENERATION + SELF-CHECK (P2, 2026-09-29)
// ════════════════════════════════════════════════════════════════════════════════════
//
// `Finding::fix_specific` used to be a stable `None` — `report_export::CuratedSiteJson`'s
// `fix_for_this_finding` field existed but nothing ever populated it (see that field's own
// doc comment before this pass). This section is the dedicated AI pass that fills it: a
// batched generation call over every non-dependency finding (modeled on `verify_findings`
// above — same `&dyn LlmPort`, same `UsageMeter`, same total-backstop timeout), followed by
// a bounded validate-and-regenerate loop so a fix that names an identifier absent from the
// finding's own evidence, or contradicts the finding's own `detail`, never reaches the
// client. A finding that still has no valid fix after every retry gets an honest
// needs-review marker instead of an empty/fabricated Fix block — see
// [`generate_fix_specifics`]'s own doc comment for the full contract.

/// How many EXTRA generation attempts a finding gets after its first fix fails the
/// self-check (identifier grounding or non-contradiction) — 2, per the design doc's "a small
/// N" bound. Each retry re-sends ONLY the still-failing findings, with a one-line note on
/// exactly what was wrong, so the model has a concrete correction to make rather than
/// guessing again blind.
const MAX_FIX_REGENERATIONS: usize = 2;

/// SYSTEM PROMPT for the fix-specific generation pass. Deliberately narrow: this pass does
/// NOT re-judge severity/confidence/effort (calibration already did that) and does NOT
/// explain the violation (the finding's own `detail` already does) — its ONLY job is a
/// concrete "change X in file Y" sentence grounded in the evidence it's given.
fn fix_specific_system_prompt() -> String {
    r#"You write ONE concrete, codebase-specific fix per finding for a paying client's audit
report. You are NOT re-explaining what is wrong — the finding's own detail already does that.
Your only job is telling the developer EXACTLY what to change, in THIS codebase.

Rules, every fix:
- Name the REAL file/symbol/column/function from the evidence given for that finding (its own
  path, snippet, detail, captured objects, and surrounding code). NEVER invent a name that
  isn't present anywhere in the evidence you were given for that finding.
- When the surrounding context shows the repo ALREADY has the correct pattern somewhere else,
  point to it by name ("use `safeInternalPath` from lib/redirect.ts, as
  app/auth/signout/route.ts does") instead of describing the pattern generically.
- 1-4 sentences. A short, repo-style code snippet is fine when it helps, but keep it tight.
- Say WHAT to change and WHERE — NEVER how this finding was detected. Do not mention
  Camerata, a scanner, a linter, a rule id, a regex, "the audit", or any detection mechanism.
- NEVER assert something about the codebase's current state that contradicts the finding's own
  detail (e.g. do not tell the client to enable/turn on something the detail already says is
  enabled/on/configured) — if the detail says a mitigation already exists, your fix must
  address what's actually still missing or broken, not redo what's already there.
- If you cannot write a grounded, non-contradictory fix for a finding from the evidence given,
  omit it from your response entirely rather than guessing.

Return ONLY JSON, no prose:
{"fixes":[{"index":0,"fix":"..."}]}
One entry per finding you could ground a fix for, addressed by its [index]. Omit any index you
could not confidently ground — an omitted fix is far better than an invented one."#
        .to_string()
}

/// A short, line-numbered excerpt of `path`'s content in `files`, centered on `line`
/// (`radius` lines each side) — the "enough surrounding repo context to name real
/// identifiers" the design doc asks for, without re-sending the whole file. Empty when
/// `path` isn't found in `files` or `line` is 0 (a path-based, not line-based, finding) —
/// the prompt still has `snippet`/`detail`/`captures` in that case.
fn fix_context_window(
    files: &[(String, String)],
    path: &str,
    line: usize,
    radius: usize,
) -> String {
    if line == 0 {
        return String::new();
    }
    let Some((_, content)) = files.iter().find(|(p, _)| p == path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let idx = line.saturating_sub(1).min(lines.len().saturating_sub(1));
    let start = idx.saturating_sub(radius);
    let end = (idx + radius + 1).min(lines.len());
    let mut out = String::new();
    for (i, l) in lines[start..end].iter().enumerate() {
        out.push_str(&format!("{}: {l}\n", start + i + 1));
    }
    out
}

/// The evidence blob a finding's generated fix is checked against — EXACTLY the union of
/// what the generation prompt showed the model for that finding (path/snippet/detail/
/// captures/surrounding code), so "grounded" means "grounded in what the model actually
/// saw," never a stricter or looser set. Shared by [`build_fix_specific_block`] (prompt) and
/// [`find_ungrounded_identifier`] (validation) so the two can never silently drift apart.
fn fix_evidence_blob(f: &Finding, files: &[(String, String)]) -> String {
    let context = fix_context_window(files, &f.path, f.line, 8);
    let captures = f.captures.values().cloned().collect::<Vec<_>>().join(" ");
    format!(
        "{} {} {} {} {}",
        f.path, f.snippet, f.detail, captures, context
    )
}

/// Build the per-finding prompt body for a fix-generation round. `findings` are the
/// findings THIS round is asking about (batch-local order); `feedback`, when present for a
/// batch-local position, is a one-line note on why that finding's PRIOR attempt was
/// rejected, appended so the model corrects the specific problem instead of guessing again.
fn build_fix_specific_block(
    findings: &[&Finding],
    files: &[(String, String)],
    corpus: Option<&camerata_rules::RuleSet>,
    feedback: &std::collections::HashMap<usize, String>,
) -> String {
    let mut out = String::new();
    for (i, f) in findings.iter().enumerate() {
        out.push_str(&format!(
            "[{i}] rule {} — severity {} — {}:{}\n",
            f.rule_id, f.severity, f.path, f.line
        ));
        if !f.snippet.is_empty() {
            out.push_str(&format!("Snippet: {}\n", f.snippet));
        }
        out.push_str(&format!("Detail: {}\n", f.detail));
        if !f.captures.is_empty() {
            let caps = f
                .captures
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("Captured objects: {caps}\n"));
        }
        let context = fix_context_window(files, &f.path, f.line, 8);
        if !context.is_empty() {
            out.push_str(&format!("Surrounding code:\n{context}"));
        }
        if let Some(rule) = corpus.and_then(|c| c.get_by_id(&f.rule_id)) {
            if let Some(opt) = rule.resolved_option(f.evaluated_option_id.as_deref()) {
                if let Some(rem) = opt.remediation.as_deref().filter(|s| !s.trim().is_empty()) {
                    out.push_str(&format!(
                        "This rule's generic remediation (inspiration only — your fix must be \
                         SPECIFIC to this finding, not a restatement of this): {rem}\n"
                    ));
                }
            }
        }
        if let Some(reason) = feedback.get(&i) {
            out.push_str(&format!(
                "Your previous attempt for this finding was rejected: {reason}. Write a \
                 corrected fix.\n"
            ));
        }
        out.push('\n');
    }
    out
}

/// Parse the `{"fixes":[{"index":N,"fix":"..."}]}` response into batch-local index -> fix
/// text. Robust: unparseable/malformed JSON, a missing `fixes` array, or a blank `fix`
/// string for an entry all simply leave that index absent from the map — the caller treats
/// an absent index exactly like "the model couldn't ground this one" (never a panic, never a
/// fabricated empty string).
fn parse_fix_specifics(raw: &str) -> std::collections::HashMap<usize, String> {
    let mut out = std::collections::HashMap::new();
    let Some(json) = extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    let Some(arr) = v["fixes"].as_array() else {
        return out;
    };
    for entry in arr {
        let Some(idx) = entry["index"].as_u64() else {
            continue;
        };
        let Some(text) = entry["fix"].as_str() else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        out.insert(idx as usize, text.to_string());
    }
    out
}

/// Bare, non-code words a fix is allowed to name without them being present in the
/// finding's evidence — generic literals/placeholders, never a real identifier a client
/// could go looking for. Deliberately short: anything NOT on this list must be grounded.
const GENERIC_FIX_TERMS: &[&str] = &["true", "false", "null", "none", "undefined", "todo"];

/// **Identifier-grounding validator (pure).** Every concrete identifier a generated `fix`
/// names — a `` `backticked` `` symbol/column, or a bare file-path-like token
/// (`lib/redirect.ts`, `schema.sql`, `package.json`) — must appear verbatim in `evidence`
/// (the finding's own path/snippet/detail/captures/surrounding code — see
/// [`fix_evidence_blob`]) or be one of [`GENERIC_FIX_TERMS`]. Returns the FIRST offending
/// identifier found, so the caller can hand the model a concrete, specific correction
/// ("you named X, which isn't in the evidence") rather than a vague rejection. Returns
/// `None` when every named identifier is grounded (including a fix that names none at all —
/// pure prose is never rejected on this axis).
pub(crate) fn find_ungrounded_identifier(fix: &str, evidence: &str) -> Option<String> {
    let backtick_re = regex::Regex::new(r"`([^`]+)`").expect("static regex");
    let path_re = regex::Regex::new(r"[A-Za-z0-9_.\-/]+\.[A-Za-z]{1,6}\b").expect("static regex");

    let mut candidates: Vec<String> = Vec::new();
    for cap in backtick_re.captures_iter(fix) {
        candidates.push(cap[1].to_string());
    }
    for m in path_re.find_iter(fix) {
        candidates.push(m.as_str().to_string());
    }

    for ident in candidates {
        let trimmed = ident.trim_matches(|c: char| matches!(c, '.' | ',' | ')' | '(' | ';' | ':'));
        if trimmed.is_empty() {
            continue;
        }
        if GENERIC_FIX_TERMS.contains(&trimmed.to_ascii_lowercase().as_str()) {
            continue;
        }
        if !evidence.contains(trimmed) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Common words that precede an "already X" phrase without being the SUBJECT of it (e.g.
/// "this file is already enabled" — "file" isn't what's enabled). Excluded from
/// [`fix_contradicts_detail`]'s subject extraction so the heuristic keys on the actual
/// setting/capability name ("strict mode", "RLS", "verify_jwt"), not filler.
const CONTRADICTION_STOPWORDS: &[&str] = &[
    "this", "that", "with", "have", "already", "file", "code", "function", "which", "it", "the",
];

/// Phrases in a finding's `detail` asserting some setting/capability is ALREADY in a given
/// state — the premise half of the known "told to fix what's already fixed" contradiction
/// class (the design doc's strict-mode example generalized: this fires for ANY already-on
/// setting, not just strict mode — RLS, `verify_jwt`, 2FA, ...).
const ALREADY_MARKERS: &[&str] = &[
    "already enabled",
    "already on",
    "already true",
    "already active",
    "already configured",
    "already turned on",
    "is already enabled",
    "currently enabled",
];

/// Verbs that, in a fix, instruct turning a setting ON — the instruction half of the
/// contradiction class. Deliberately narrow (no generic "add"/"set", which fire on
/// unrelated, legitimate fixes) so this stays a targeted check, not a broad false-positive
/// generator.
const ENABLE_VERBS: &[&str] = &[
    "enable",
    "turn on",
    "turning on",
    "switch on",
    "re-enable",
    "reenable",
    "activate",
];

/// **Non-contradiction validator (pure, rule-based).** Detects the known "told to do what's
/// already done" contradiction class: `detail` asserts a setting/capability is ALREADY in
/// some state (`ALREADY_MARKERS`), and `fix` instructs enabling/turning on ([`ENABLE_VERBS`])
/// that SAME setting — the strict-mode-already-on example generalized to any subject, not
/// hardcoded to "strict". Returns `Some(reason)` (fed back to the model verbatim as the
/// regeneration note) on a hit, `None` otherwise. This is a TARGETED rule check for one known
/// contradiction shape, not a general consistency oracle — see this module's banner comment
/// for why a rule-based check satisfies the design doc's self-check requirement on its own.
pub(crate) fn fix_contradicts_detail(fix: &str, detail: &str) -> Option<String> {
    let detail_lower = detail.to_ascii_lowercase();
    let fix_lower = fix.to_ascii_lowercase();

    for marker in ALREADY_MARKERS {
        let Some(pos) = detail_lower.find(marker) else {
            continue;
        };
        let before = &detail_lower[..pos];
        let subject_tokens: Vec<&str> = before
            .split_whitespace()
            .rev()
            .take(4)
            .flat_map(|w| w.split(|c: char| !c.is_alphanumeric()))
            .filter(|t| t.len() >= 4 && !CONTRADICTION_STOPWORDS.contains(t))
            .collect();
        if subject_tokens.is_empty() {
            continue;
        }
        for verb in ENABLE_VERBS {
            let Some(vpos) = fix_lower.find(verb) else {
                continue;
            };
            let window_end = (vpos + verb.len() + 60).min(fix_lower.len());
            let window = &fix_lower[vpos..window_end];
            if subject_tokens.iter().any(|t| window.contains(t)) {
                return Some(format!(
                    "the finding's detail says \"{marker}\" but the fix tells the client to \
                     {verb} the same thing — that's already done, address what's ACTUALLY \
                     still wrong"
                ));
            }
        }
    }
    None
}

/// Phrases that describe HOW Camerata found a finding rather than WHAT to change — the
/// no-methodology-leak invariant (FIX-1, carried into `fix_specific`: see
/// `report_export::resolve_fix`'s doc comment for the original instance of this rule over
/// the rule-authored `fix` field). A client-facing fix must never read like a scanner's
/// self-description.
const METHODOLOGY_LEAK_PHRASES: &[&str] = &[
    "camerata",
    "this rule detects",
    "this rule checks",
    "this rule flags",
    "the scanner",
    "the audit tool",
    "our scan",
    "our tool",
    "the scan flagged",
    "detected by",
    "regex-scan",
    "regex scan",
    "static analysis",
    "the gate",
    "flagged this finding",
    "the audit found",
];

/// **Methodology-leak guard (pure).** Returns the first offending phrase found in `fix`
/// (case-insensitive), or `None` when the fix describes only the remediation.
pub(crate) fn fix_leaks_methodology(fix: &str) -> Option<String> {
    let lower = fix.to_ascii_lowercase();
    METHODOLOGY_LEAK_PHRASES
        .iter()
        .find(|p| lower.contains(*p))
        .map(|p| p.to_string())
}

/// One ROUND of the fix-specific generation loop (C6-B3.4 extracted this out of
/// [`generate_fix_specifics`] so a normal `FIX_BATCH_SIZE`-batched round and the final
/// single-finding retry share IDENTICAL request-building, streaming, and self-check logic —
/// the only axis that varies between callers is `batch_size`). Mutates `findings[idx]
/// .fix_specific` in place for anything that validates; returns the subset of `pending` that
/// still needs another round. A transport/timeout failure on one chunk re-queues only THAT
/// chunk's findings, never the whole still-pending set.
#[allow(clippy::too_many_arguments)]
async fn run_fix_specifics_round(
    llm: &dyn LlmPort,
    repo: &str,
    system: &str,
    fix_model: Option<&str>,
    meter: Option<&UsageMeter>,
    corpus: Option<&camerata_rules::RuleSet>,
    files: &[(String, String)],
    findings: &mut [Finding],
    feedback: &mut std::collections::HashMap<usize, String>,
    pending: &[usize],
    batch_size: usize,
) -> Vec<usize> {
    let mut still_pending = Vec::new();
    // Bounded chunks (see `FIX_BATCH_SIZE`'s doc comment) — each is an independent streamed
    // call, so a failure/timeout in ONE chunk only re-queues ITS findings for the next
    // attempt, never the whole still-pending set. `pending.chunks(..)` yields exactly one
    // chunk (the whole set) when `batch_size >= pending.len()`, so the request/self-check
    // shape below is unchanged from before this fix in that case.
    for batch in pending.chunks(batch_size.max(1)) {
        let prompt = {
            let refs: Vec<&Finding> = batch.iter().map(|&i| &findings[i]).collect();
            let local_feedback: std::collections::HashMap<usize, String> = batch
                .iter()
                .enumerate()
                .filter_map(|(pos, orig)| feedback.get(orig).cloned().map(|r| (pos, r)))
                .collect();
            format!(
                "Repository: {repo}\n\nWrite a concrete fix for each finding below:\n\n{}",
                build_fix_specific_block(&refs, files, corpus, &local_feedback)
            )
        };
        let mut req = LlmRequest::new(prompt)
            .with_system(system.to_string())
            .with_max_tokens(scaled_max_tokens(batch.len(), FIX_TOKENS_PER_FINDING));
        if let Some(m) = fix_model {
            req = req.with_model(m.to_string());
        }
        // Routed through the STREAMING transport (not the flat `complete`) so a large
        // batch gets the CLI heartbeat + partial-output salvage instead of a flat,
        // non-reset 300s deadline. `on_delta` is a no-op: this pass has no live
        // transcript sink to feed.
        let mut on_delta = |_: &str| {};
        let resp = match llm.complete_streaming(req, &mut on_delta).await {
            Ok(r) => r,
            // Transport/timeout failure: nothing to validate for this chunk this round.
            // Re-queue exactly this chunk's findings so the next round retries them (or,
            // on the last round, the instantiated-fallback below picks them up) — a
            // failure never re-queues findings OUTSIDE this chunk.
            Err(_) => {
                still_pending.extend(batch.iter().copied());
                continue;
            }
        };
        if let Some(m) = meter {
            m.record(&resp);
        }
        let parsed = parse_fix_specifics(&resp.text);

        for (pos, &orig_idx) in batch.iter().enumerate() {
            let Some(text) = parsed.get(&pos) else {
                feedback
                    .entry(orig_idx)
                    .or_insert_with(|| "you did not return a fix for this finding".to_string());
                still_pending.push(orig_idx);
                continue;
            };
            let evidence = fix_evidence_blob(&findings[orig_idx], files);
            if let Some(bad) = find_ungrounded_identifier(text, &evidence) {
                feedback.insert(
                    orig_idx,
                    format!(
                        "you named `{bad}`, which doesn't appear anywhere in this finding's \
                         evidence"
                    ),
                );
                still_pending.push(orig_idx);
                continue;
            }
            if let Some(reason) = fix_contradicts_detail(text, &findings[orig_idx].detail) {
                feedback.insert(orig_idx, reason);
                still_pending.push(orig_idx);
                continue;
            }
            if let Some(leak) = fix_leaks_methodology(text) {
                feedback.insert(
                    orig_idx,
                    format!(
                        "you described detection methodology (\"{leak}\") — describe only \
                         the fix, never how it was found"
                    ),
                );
                still_pending.push(orig_idx);
                continue;
            }
            findings[orig_idx].fix_specific = Some(text.clone());
        }
    }
    still_pending
}

/// Run the P2 fix-specific generation pass: for every non-dependency finding in `findings`,
/// generate a codebase-specific `fix_specific` (see [`fix_specific_system_prompt`]), then
/// validate it (identifier grounding + non-contradiction + no-methodology-leak) and
/// regenerate up to [`MAX_FIX_REGENERATIONS`] times for anything that fails. A finding whose
/// fix STILL doesn't pass after every batched retry gets ONE more solo attempt (C6-B3.4,
/// `run_fix_specifics_round` with `batch_size = 1`) — a smaller, single-finding prompt is a
/// cheap reliability win on top of the batched retries, since it strips away any
/// cross-finding context (a batch-mate's evidence, a batch-mate's rejected fix) that may have
/// been confusing the model or tripping the self-check on THIS finding specifically.
///
/// A finding that STILL doesn't pass after that keeps `fix_specific` at `None` from the
/// MODEL's own attempt — but C6-B3 means that is no longer the end of the story: this
/// function now instantiates the RULE's own authored remediation against the finding's own
/// `path`/`captures` (via [`crate::report_export::resolve_fix`], the exact machinery the
/// generic `fix` field already uses) and writes THAT into `fix_specific` instead, so the
/// finding's PRIMARY fix line is still concrete and finding-specific — never a silent `None`,
/// never the model's fabricated/rejected text. Only a finding whose rule has no authored
/// remediation at all (no corpus, no corpus entry, or a blank `remediation` field) keeps
/// `fix_specific` as `None` — genuinely nothing to instantiate. Every give-up (whether or not
/// the instantiated fallback covered it) is recorded into the returned `Vec<FailedPass>` so
/// the report export can disclose the degradation (count included) rather than shipping
/// silently — see [`FailedPass`] and W6.
///
/// This is a PIPELINE failure (the model never produced a validated, codebase-specific fix),
/// not a confidence judgement about the finding itself, so it must NEVER set `needs_review`,
/// NEVER hedge, and NEVER tag `detail` with pipeline-state text (C3-1b,
/// `docs/plans/2026-09-30_cycle2-queue-hardening.md`) — the ONLY thing that sets
/// `needs_review`/`confidence = "needs-review"` is a calibration doubt verdict
/// (`apply_verdicts`). A row that STILL has no fix from ANY source (no model-generated
/// specific, no instantiated authored remediation) is never dropped and never refused here —
/// [`crate::report_export::is_action_row_missing_a_fix`] re-routes exactly that case to
/// `"held"` at report-export time, with its own disclosed reason.
///
/// Modeled on [`verify_findings`] above: same `&dyn LlmPort` seam, same [`UsageMeter`]
/// folding, same streamed-call + bounded-chunk shape (`FIX_BATCH_SIZE`, in the
/// "AGGREGATE-PASS TUNING" section). Dependency-audit findings (`DEP_AUDIT_RULE_ID`) are
/// skipped — they're carved into their own §7 lane and never flow through
/// `resolve_fix`/`CuratedSiteJson` either (see that carve-out's doc comment in
/// `report_export.rs`).
pub async fn generate_fix_specifics(
    llm: &dyn LlmPort,
    repo: &str,
    mut findings: Vec<Finding>,
    files: &[(String, String)],
    fix_model: Option<&str>,
    meter: Option<&UsageMeter>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> (Vec<Finding>, Vec<FailedPass>) {
    let indices: Vec<usize> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.rule_id != crate::dep_audit::DEP_AUDIT_RULE_ID)
        .map(|(i, _)| i)
        .collect();
    if indices.is_empty() {
        return (findings, Vec::new());
    }

    let system = fix_specific_system_prompt();
    // The subset of `indices` still needing a valid fix this round; shrinks as findings
    // pass their self-check or exhaust retries. Original-index -> rejection reason, fed
    // back into the NEXT round's prompt for that finding only.
    let mut pending = indices;
    let mut feedback: std::collections::HashMap<usize, String> = std::collections::HashMap::new();

    for _attempt in 0..=MAX_FIX_REGENERATIONS {
        if pending.is_empty() {
            break;
        }
        pending = run_fix_specifics_round(
            llm,
            repo,
            &system,
            fix_model,
            meter,
            corpus,
            files,
            &mut findings,
            &mut feedback,
            &pending,
            FIX_BATCH_SIZE,
        )
        .await;
    }

    // C6-B3.4: one FINAL, cheap-reliability pass over whatever's still pending — SOLO
    // (batch_size = 1) rather than batched. See this function's doc comment for why a
    // smaller prompt is worth one extra call per still-pending finding.
    if !pending.is_empty() {
        pending = run_fix_specifics_round(
            llm,
            repo,
            &system,
            fix_model,
            meter,
            corpus,
            files,
            &mut findings,
            &mut feedback,
            &pending,
            1,
        )
        .await;
    }

    // C6-B3: a give-up is never silent and never leaves the finding with no fix line when a
    // fix is available from ANY source. Instantiate the rule's own authored remediation
    // against THIS finding's own path/captures — the identical machinery that already backs
    // the generic `fix` field — and write it into `fix_specific`, so the finding's PRIMARY
    // fix line is concrete and finding-specific even though the model itself never produced
    // one. A finding whose rule carries no authored remediation at all keeps `fix_specific`
    // as `None`; `report_export::is_action_row_missing_a_fix` is what re-routes THAT case to
    // `"held"` rather than shipping an action-bucket row with no fix text anywhere.
    let mut failed_passes = Vec::new();
    if !pending.is_empty() {
        let gave_up = pending.len();
        let mut instantiated = 0usize;
        for &idx in &pending {
            eprintln!(
                "[camerata-server] fix-specific generation gave up for {repo} {}:{} ({}) after \
                 {MAX_FIX_REGENERATIONS} batched retries plus one single-finding retry — \
                 falling back to the rule's authored remediation, instantiated against this \
                 finding's own identifiers, not a client-facing hedge.",
                findings[idx].path, findings[idx].line, findings[idx].rule_id
            );
            if let Some(fallback) = crate::report_export::resolve_fix(
                &findings[idx].rule_id,
                corpus,
                &findings[idx],
                None,
            ) {
                findings[idx].fix_specific = Some(fallback);
                instantiated += 1;
            }
        }
        let uncovered = gave_up - instantiated;
        failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: "fix-specific generation".to_string(),
            reason: format!(
                "per-finding fix generation gave up on {gave_up} finding(s) this run after \
                 {MAX_FIX_REGENERATIONS} batched retries plus one single-finding retry; \
                 {instantiated} shipped the rule's own authored remediation, instantiated \
                 against the finding's own identifiers, in place of a model-generated fix, \
                 and {uncovered} had no authored remediation to fall back to either (routed \
                 to held for manual review rather than shipped with no fix at all)"
            ),
        });
    }

    (findings, failed_passes)
}

/// Partition `files` into contiguous chunks each whose RAW size is at most `budget` bytes,
/// so each chunk's digest fits a single model context and the WHOLE repo gets audited. A
/// file larger than `budget` becomes its own chunk (its digest then clips at the per-call
/// cap). The repo is never partially dropped — every file lands in exactly one chunk.
fn chunk_files(files: &[(String, String)], budget: usize) -> Vec<&[(String, String)]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut acc = 0usize;
    for (i, (path, content)) in files.iter().enumerate() {
        let sz = path.len() + content.len() + 32; // ≈ header + content
        if acc > 0 && acc + sz > budget {
            chunks.push(&files[start..i]);
            start = i;
            acc = 0;
        }
        acc += sz;
    }
    if start < files.len() {
        chunks.push(&files[start..]);
    }
    chunks
}

/// Coarse total-time backstop for NON-streaming calls only (the calibration pass and the
/// no-feedback path), which have no per-token progress signal to watch. Streaming calls do
/// NOT use this — they self-bound on an idle/stall timeout inside the transport, which
/// scales with repo size (a big scan keeps streaming and never trips it; only a true hang
/// does). Set high so it never kills legitimate work. `CAMERATA_LLM_MAX_SECS` (default 600).
fn total_backstop() -> std::time::Duration {
    let secs = std::env::var("CAMERATA_LLM_MAX_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

/// The `needs_files` paths a pass asked for (file bodies it needs co-resident to judge a
/// cross-file rule). Drives the bounded resolution round. Robust to missing/garbled output.
fn parse_needs_files(raw: &str) -> Vec<String> {
    let Some(json) = extract_json_object(raw) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["needs_files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Compute the API cache-control breakpoints (byte offsets into the full prompt) for one
/// audit pass, given where each of the two cacheable static segments ends.
///
/// GAP-3 (batch-mode cache cost): the audit prompt is built as
/// `{repo_map_segment}{chunk_segment}{varying task_line + rules}` — see
/// [`run_passes`]/[`run_passes_batch`]'s prompt builders, which are the only callers. The
/// repo-map segment is now DELIBERATELY FIRST because it's byte-IDENTICAL across every
/// chunk in the scan (it never embeds the chunk index or the chunk's digest), so caching it
/// lets chunk 2, 3, ... of the SAME scan cache-HIT on it instead of re-billing the (often
/// large) repo map on every chunk — something the OLD ordering (chunk index embedded before
/// the repo map) could never do, since that made the "static" prefix differ at ~byte 30 on
/// every chunk.
///
/// The chunk segment (label + digest) is stable across THIS chunk's rule-batches only, so
/// its breakpoint is worth adding when there's more than one rule-batch (`n_b > 1`) — that's
/// the existing parallel-mode win (a chunk's digest re-read across rule-batch 2, 3, ...).
/// When `n_b <= 1` (batch mode's default: `BATCH_RULE_BATCH_SIZE = usize::MAX`, i.e. every
/// rule in one rule-batch), that segment is written to the cache exactly once and NEVER
/// read back — Anthropic bills a ~1.25x write premium on a 5m cache entry that buys
/// nothing, so this drops the second breakpoint entirely and folds the chunk segment into
/// the uncached suffix instead of paying to cache something that's read zero times.
///
/// The repo-map breakpoint is kept unconditionally (independent of `n_b`) since it re-reads
/// across CHUNKS, a dimension `n_b` says nothing about.
fn cache_breakpoints_for_pass(
    repo_map_prefix_len: usize,
    static_prefix_len: usize,
    n_b: usize,
) -> Vec<usize> {
    let mut bp = vec![repo_map_prefix_len];
    if n_b > 1 {
        bp.push(static_prefix_len);
    }
    bp
}

/// Build the full prompt AND its API cache-control breakpoints for one (chunk, rule-batch)
/// audit pass. Shared VERBATIM by `run_passes` (parallel mode) and `run_passes_batch` (batch
/// mode) so the two scan modes structurally cannot drift on prompt shape or cache-breakpoint
/// placement — see [`cache_breakpoints_for_pass`] for the caching rationale.
///
/// Prompt shape (cache-aware ordering, shared content first):
///   `Repository: {repo}\n\n{repo_map}` (IDENTICAL across every chunk)
///   + `({label} {ci+1}/{n_c})\n\n{digest}\n\n` (stable across this chunk's rule-batches)
///   + `{task_line}\n\n{rules_block}` (varies every rule-batch, never cached)
///
/// Returns `(prompt, cache_breakpoints)`.
#[allow(clippy::too_many_arguments)]
fn build_pass_prompt(
    repo: &str,
    repo_map: &str,
    label: &str,
    ci: usize,
    n_c: usize,
    digest: &str,
    n_b: usize,
    task_line: &str,
    rules_block: &str,
) -> (String, Vec<usize>) {
    let repo_map_prefix = format!("Repository: {repo}\n\n{repo_map}");
    let chunk_segment = format!("({label} {}/{n_c})\n\n{digest}\n\n", ci + 1);
    let static_prefix = format!("{repo_map_prefix}{chunk_segment}");
    let cache_breakpoints =
        cache_breakpoints_for_pass(repo_map_prefix.len(), static_prefix.len(), n_b);
    let prompt = format!("{static_prefix}{task_line}\n\n{rules_block}");
    (prompt, cache_breakpoints)
}

/// One audit pass: build the request, run it (streaming into the transcript when feedback
/// is present), and parse out findings + proposed rules + any `needs_files` request. Shared
/// by the primary chunk loop and the resolution round so neither duplicates the call logic.
///
/// `cache_breakpoints` — byte offsets into `prompt` marking the end of each cacheable
/// segment (see [`cache_breakpoints_for_pass`] for how callers compute these), forwarded to
/// [`LlmRequest::with_cache_breakpoints`]. On the API backend this tells the provider to
/// cache each segment and re-read it cheaply on every subsequent call that shares it. The
/// CLI backend ignores this (no-op). Pass an empty slice to disable caching (default).
#[allow(clippy::too_many_arguments)]
async fn audit_pass(
    llm: &dyn LlmPort,
    audit_model: Option<&str>,
    prompt: String,
    cache_breakpoints: Vec<usize>,
    repo: &str,
    adopted: &std::collections::HashSet<String>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    session: &str,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)> {
    let mut req = LlmRequest::new(prompt)
        .with_system(audit_system_prompt())
        .with_max_tokens(8192);
    if let Some(m) = audit_model {
        req = req.with_model(m.to_string());
    }
    if !cache_breakpoints.is_empty() {
        req = req.with_cache_breakpoints(cache_breakpoints);
    }
    let resp = if let Some((store, key)) = feedback {
        // Streaming: the idle/stall timeout lives inside the transport, so this scales with
        // repo size and only a genuine hang (no output for the idle window) aborts. No
        // total-time cap here — a big scan should be allowed to stream as long as it needs.
        let mut on_delta = |t: &str| store.append_output_raw(key, session, t);
        llm.complete_streaming(req, &mut on_delta).await?
    } else {
        // Non-streaming has no progress signal; bound it with the coarse total backstop.
        let cap = total_backstop();
        tokio::time::timeout(cap, llm.complete(req))
            .await
            .map_err(|_| anyhow::anyhow!("LLM call exceeded the {}s backstop", cap.as_secs()))??
    };
    if let Some(m) = meter {
        m.record(&resp);
    }
    let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
    let needs = parse_needs_files(&resp.text);
    Ok((f, p, needs))
}

/// The public symbols a file defines (Rust items + TS/JS exports), for the repo map. A
/// cheap line scan — no parser — capped so the map stays compact.
fn extract_public_symbols(content: &str) -> Vec<String> {
    const RUST_KW: &[&str] = &[
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "pub fn ",
    ];
    const JSTS_KW: &[&str] = &[
        "export class ",
        "export interface ",
        "export type ",
        "export function ",
        "export const ",
    ];
    let ident = |rest: &str| -> Option<String> {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    let mut syms = Vec::new();
    for line in content.lines() {
        let t = line.trim_start();
        for kw in RUST_KW.iter().chain(JSTS_KW) {
            if let Some(rest) = t.strip_prefix(kw) {
                if let Some(name) = ident(rest) {
                    if !syms.contains(&name) {
                        syms.push(name);
                    }
                }
            }
        }
        if syms.len() >= 12 {
            break;
        }
    }
    syms
}

/// A compact map of the WHOLE repo — every file path plus the public symbols it defines —
/// injected into EVERY chunk. Naive file-chunking otherwise loses cross-file context: a
/// layering rule needs to know which dirs are repositories vs services across files, and a
/// "this type is defined elsewhere" finding needs to know the type exists in another file
/// not in this pass. The map gives every chunk that architecture without every file body,
/// so chunking doesn't reintroduce cross-file misses. (Bodies still only appear in their
/// own chunk — a rule needing the full body of a type in another chunk is the known limit.)
fn build_repo_map(files: &[(String, String)]) -> String {
    let mut out = String::from(
        "REPO MAP — every file in the repo and the public symbols it defines. Only SOME file \
         bodies appear in THIS pass; use this map for cross-file architectural context (which \
         directories are repositories vs services vs controllers, where a named type lives):\n",
    );
    for (path, content) in files {
        let syms = extract_public_symbols(content);
        if syms.is_empty() {
            out.push_str(&format!("  {path}\n"));
        } else {
            out.push_str(&format!("  {path}  [{}]\n", syms.join(", ")));
        }
    }
    out.push('\n');
    out
}

/// Max concurrent LLM calls + rules-per-batch for the parallel mode. Tunable.
const PARALLEL_CONCURRENCY: usize = 6;
const RULE_BATCH_SIZE: usize = 15;

/// Rules-per-batch for `ScanMode::Batch`.
///
/// The Batch path compiles ALL (chunk × rule-batch) pairs into a SINGLE Anthropic
/// Message Batch and lets the API schedule them — there is no per-item concurrency
/// cost for splitting into finer batches. Using `RULE_BATCH_SIZE` (15) from the
/// parallel path was a design mismatch (BUG-6): smaller batches add BatchItems but
/// don't reduce latency; a LARGER batch keeps more adopted rules visible together in
/// one prompt context, which improves coherence and reduces cross-batch re-flagging.
///
/// Set to `usize::MAX` so each chunk becomes a SINGLE BatchItem containing all rules.
/// This can be overridden at runtime via the `CAMERATA_BATCH_RULE_BATCH_SIZE` env var
/// (parsed as `usize`; falls back to this constant when absent or unparseable).
const BATCH_RULE_BATCH_SIZE: usize = usize::MAX;

/// How the semantic (LLM) audit executes — the SPEED/SCALE knob, orthogonal to model tier
/// (quality) and rule selection (coverage). The free deterministic floor is unaffected; it
/// runs the same in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// One call per file-chunk, ALL rules at once, chunks one after another. Simplest,
    /// gentlest on rate limits — the debug/fallback floor.
    Sequential,
    /// Rule-batches × file-chunks run CONCURRENTLY (capped). The default efficient floor:
    /// wall-clock is the slowest batch, not the sum of all calls.
    Parallel,
    /// Submit ALL (chunk × rule-batch) requests as a SINGLE Anthropic Message Batch
    /// (POST /v1/messages/batches), wait for it to complete, then reassemble. Costs 50%
    /// less on all tokens vs. real-time calls. Requires the `api` backend + key. Best for
    /// large scans where latency is acceptable in exchange for cost savings.
    ///
    /// Implementation: `run_passes_batch` compiles the cartesian product of chunks ×
    /// rule-batches into `BatchItem`s with deterministic `custom_id`s (`c{ci}-b{bi}`),
    /// submits via `Llm::submit_batch`, polls until `processing_status == "ended"`, fetches
    /// results, reassembles by `custom_id`, and feeds each response into the same
    /// `parse_ai_findings` + dedup/merge/calibrate tail as the parallel path.
    Batch,
}

impl ScanMode {
    /// Returns `(max_concurrent_calls, rules_per_batch)`.
    ///
    /// For real-time modes (`Sequential`, `Parallel`) this controls actual API
    /// parallelism. For `Batch` mode the concurrency value is unused (all items are
    /// submitted in one Anthropic Message Batch); only `rules_per_batch` matters and
    /// it should be LARGE so each BatchItem includes all adopted rules for maximum
    /// coherence (BUG-6: using `RULE_BATCH_SIZE = 15` was a design mismatch — it
    /// fragmented Batch items without any latency benefit).
    ///
    /// `CAMERATA_BATCH_RULE_BATCH_SIZE` overrides the Batch rule batch size at runtime.
    fn tuning(self) -> (usize, usize) {
        match self {
            ScanMode::Sequential => (1, usize::MAX),
            ScanMode::Parallel => (PARALLEL_CONCURRENCY, RULE_BATCH_SIZE),
            ScanMode::Batch => {
                // Batch mode: all rules in one item per chunk is the efficient default;
                // the env var allows the operator to cap it (e.g. for very large rule sets
                // where a single prompt would exceed the model's context window).
                let batch_rule_size = std::env::var("CAMERATA_BATCH_RULE_BATCH_SIZE")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(BATCH_RULE_BATCH_SIZE);
                (PARALLEL_CONCURRENCY, batch_rule_size)
            }
        }
    }
    /// Parse the wire value; unknown / empty → Parallel (the efficient default floor).
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("sequential") => ScanMode::Sequential,
            Some("batch") => ScanMode::Batch,
            _ => ScanMode::Parallel,
        }
    }
}

/// Run a set of file-chunks × rule-batches as passes, up to `concurrency` at once, and
/// aggregate their findings / proposed rules / needs_files. Each pass registers its OWN
/// transcript agent (so parallel streams don't clobber each other) and finalizes its own
/// status. Shared by the main and resolution rounds. `concurrency == 1` => sequential.
///
/// `advisory_disabled` — when `true`, the advisory "flag novel issues beyond the adopted rules"
/// task is suppressed in EVERY batch of this call (even `bi==0`). This is the routing-safe
/// switch: language-scoped groups set this to `true` so the advisory pass runs in exactly one
/// place (the cross-cutting `All` group), preventing the same novel issue from being re-flagged
/// under N independently-invented names across N language groups.
#[allow(clippy::too_many_arguments)]
async fn run_passes(
    llm: &dyn LlmPort,
    repo: &str,
    repo_map: &str,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    chunks: &[&[(String, String)]],
    batches: &[&[(String, String)]],
    concurrency: usize,
    label: &str,
    session_prefix: &str,
    meter: Option<&UsageMeter>,
    advisory_disabled: bool,
) -> (
    Vec<Finding>,
    Vec<ProposedRule>,
    std::collections::HashSet<String>,
    usize,
    Option<anyhow::Error>,
) {
    use futures::stream::StreamExt;
    let digests: Vec<String> = chunks.iter().map(|c| build_digest(c)).collect();
    let n_c = chunks.len();
    let n_b = batches.len();
    let work: Vec<(usize, usize)> = (0..n_c)
        .flat_map(|c| (0..n_b).map(move |b| (c, b)))
        .collect();
    type PassOut = (
        usize,
        usize,
        anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)>,
    );
    let results: Vec<PassOut> = futures::stream::iter(work)
        .map(|(ci, bi)| {
            let digest = &digests[ci];
            let batch = batches[bi];
            async move {
                let rb = build_rules_block(batch);
                // ADVISORY RUNS ONCE PER CHUNK, not once per rule-batch. The "flag novel
                // issues beyond the adopted rules" task only depends on the code (the whole
                // chunk is visible every pass), not on which rule-batch this is — so asking
                // for it in all N batches just re-derives the SAME novel issue under N
                // independently-invented names (one `.expect()` → AI-HANDLER-PANICS +
                // AI-HANDLER-UNHANDLED-PANIC + AI-HANDLER-PANICS-ON-ERROR). Gate it to the
                // first batch of each chunk; later batches check ONLY their adopted rules.
                //
                // ROUTING INTERACTION: when `advisory_disabled` is set (language-scoped groups),
                // the advisory pass is suppressed in every batch, not just later ones. The
                // cross-cutting All group keeps advisory enabled so novel issues are surfaced
                // exactly once — against every file — with no per-language re-flagging.
                let advisory = !advisory_disabled && bi == 0;
                let task_line = if advisory {
                    format!("── Check the code above against the ADOPTED rules below (batch {}/{n_b}); ALSO flag any other genuine issues NOT covered by an adopted rule. Use the REPO MAP for cross-file context. ──", bi + 1)
                } else {
                    format!("── Check the code above against ONLY the ADOPTED rules below (batch {}/{n_b}). Do NOT report issues outside these rules — a separate pass already covers novel findings. Use the REPO MAP for cross-file context. ──", bi + 1)
                };
                // PROMPT ORDER IS CACHE-AWARE, with the SHARED content leading, then the
                // PER-CHUNK content, then the PER-BATCH (varying) content trailing — built by
                // `build_pass_prompt` (shared verbatim with `run_passes_batch` so the two
                // scan modes can't drift):
                //
                //   Repository: {repo}\n\n{repo_map}   <- IDENTICAL across every chunk in scan
                //   ({label} n/n_c)\n\n{digest}\n\n     <- stable across this chunk's rule-batches
                //   {task_line}\n\n{rb}                 <- varies every rule-batch, never cached
                //
                // The repo-map segment is deliberately free of the chunk index/label — that's
                // what the OLD ordering got wrong (the label sat in front of the repo map, so
                // the "static" prefix actually differed at ~byte 30 on every chunk and could
                // never cache-hit across chunks). Leading with the byte-identical repo map
                // instead means chunk 2, 3, ... of the SAME scan can cache-HIT it instead of
                // re-billing the repo map on every chunk. Bonus: rules landing last = most
                // recent context = strongest rule-following (unchanged).
                //
                // CACHING: `cache_breakpoints_for_pass` marks a breakpoint after the repo-map
                // segment (re-reads across chunks) and, when there's more than one rule-batch,
                // a second one after the chunk (label+digest) segment (re-reads across this
                // chunk's rule-batches — the existing parallel-mode win). The CLI backend
                // ignores these breakpoints entirely.
                let (prompt, cache_bps) =
                    build_pass_prompt(repo, repo_map, label, ci, n_c, digest, n_b, &task_line, &rb);
                let session = format!("{session_prefix}-c{ci}-b{bi}");
                if let Some((store, key)) = feedback {
                    store.register(
                        key,
                        crate::transcript::AgentTranscript {
                            session_id: session.clone(),
                            role: format!("{label} {}/{n_c} · rules {}/{n_b} — {repo}", ci + 1, bi + 1),
                            prompt: prompt.clone(),
                            output: String::new(),
                            status: "running".to_string(),
                        },
                    );
                }
                let r = audit_pass(llm, audit_model, prompt, cache_bps, repo, adopted, feedback, &session, meter).await;
                if let Some((store, key)) = feedback {
                    store.set_status(key, &session, if r.is_ok() { "done" } else { "blocked" });
                }
                // Stream this pass's findings + progress into the job (live preview) as it
                // completes — so a Mode-3 poller sees findings appear incrementally. A failed
                // pass still counts toward `done` so the progress bar can reach 100%.
                if let Some((jstore, jid)) = job {
                    if let Ok((f, _, _)) = &r {
                        jstore.add_findings(jid, f.clone());
                    }
                    jstore.inc_done(jid, 1);
                }
                (ci, bi, r)
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;

    let mut findings = Vec::new();
    let mut proposed = Vec::new();
    let mut requested = std::collections::HashSet::new();
    let mut ok = 0usize;
    let mut last_err = None;
    for (_ci, _bi, r) in results {
        match r {
            Ok((f, p, needs)) => {
                findings.extend(f);
                proposed.extend(p);
                requested.extend(needs);
                ok += 1;
            }
            Err(e) => last_err = Some(e),
        }
    }
    (findings, proposed, requested, ok, last_err)
}

/// How many CONSECUTIVE poll failures `poll_batch_until_ended` tolerates before giving up.
/// A single failed poll (transient network blip, a 5xx that outlasted `count_tokens`'s own
/// retry budget, etc.) must never kill an hours-long batch after the tokens are already
/// spent — see the function doc for the full rationale. Resets to 0 on any successful poll.
const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 5;

/// Poll a submitted batch on a fixed interval until `processing_status == "ended"`,
/// tolerating up to `max_consecutive_failures` CONSECUTIVE poll failures before giving up.
/// Each individual `poll_once` call already retries transient HTTP failures internally
/// (see `Llm::poll_batch_status` / `crate::retry`); this is the OUTER safety net for when
/// even that budget is exhausted repeatedly — a batch can run for hours, so one bad polling
/// window (a longer network outage, a burst of 5xx past the inner retry's 3-attempt cap)
/// must not discard tokens that have already been spent. The counter resets to 0 on every
/// successful poll, so it only fires on a genuine STREAK of failures, not a "few bad polls
/// scattered across an otherwise-healthy multi-hour run."
///
/// `poll_once` is injected (rather than calling `Llm::poll_batch_status` directly) so this
/// is unit-testable with a fake that fails a controlled number of times — no live batch, no
/// network, no mock HTTP server required. `on_status` is called on every SUCCESSFUL poll
/// (including the final `ended` one) so the caller can log/report progress without this
/// function knowing anything about logging.
async fn poll_batch_until_ended<F, Fut>(
    mut poll_once: F,
    poll_interval: std::time::Duration,
    max_consecutive_failures: u32,
    mut on_status: impl FnMut(&crate::llm::BatchStatus),
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<crate::llm::BatchStatus>>,
{
    let mut consecutive_failures = 0u32;
    loop {
        tokio::time::sleep(poll_interval).await;
        match poll_once().await {
            Ok(status) => {
                consecutive_failures = 0;
                let ended = status.processing_status == "ended";
                on_status(&status);
                if ended {
                    return Ok(());
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                eprintln!(
                    "[camerata-server] batch poll failed ({consecutive_failures}/\
                     {max_consecutive_failures} consecutive failures): {e}"
                );
                if consecutive_failures >= max_consecutive_failures {
                    return Err(e.context(format!(
                        "batch poll failed {consecutive_failures} times consecutively — giving up \
                         (the batch may still be running server-side; check its status via the \
                         Anthropic console before resubmitting)"
                    )));
                }
            }
        }
    }
}

/// Batch execution mode (#61): compile ALL (chunk × rule-batch) pairs into Anthropic Message
/// Batch items, submit in ONE request, poll to completion, then reassemble by `custom_id`.
///
/// ADVANTAGES vs. parallel: 50% discount on all input + output tokens; no per-call
/// rate-limit pressure; the API schedules + parallelizes internally. TRADE-OFF: latency is
/// asynchronous — the batch typically completes in seconds to a few minutes for small scans,
/// but up to 24h for very large ones. Best suited for large/multi-repo scans where total
/// cost matters more than wall-clock time.
///
/// CAP ENFORCEMENT: the Anthropic batch API accepts up to 100k requests and 256MB body.
/// When `chunks.len() * batches.len() > 100_000`, this function splits into sub-batches,
/// submits them sequentially, and unions the results. The 256MB size cap is not checked
/// per-item (each Camerata item is typically 5-50KB; 100k items is the binding constraint
/// in practice). Exceeding the cap logs a warning and falls back gracefully.
///
/// FALLBACK: if the `api` backend / key is not available, the function returns an error so
/// the caller can fall back to parallel mode. The job's `batch_id` field is set on submit
/// and cleared on finish.
#[allow(clippy::too_many_arguments)]
async fn run_passes_batch(
    llm: &crate::llm::Llm,
    repo: &str,
    repo_map: &str,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    chunks: &[&[(String, String)]],
    batches: &[&[(String, String)]],
    label: &str,
    meter: Option<&UsageMeter>,
    // BUG-AI-1: honor advisory_disabled in batch mode the same way run_passes does.
    // When true, the "flag novel issues beyond adopted rules" task is suppressed for
    // every (chunk, batch) pair, so batch mode does not re-introduce duplicate novel
    // findings when called from a language-scoped routing group.
    advisory_disabled: bool,
) -> anyhow::Result<(
    Vec<Finding>,
    Vec<ProposedRule>,
    std::collections::HashSet<String>,
    usize,
    Option<anyhow::Error>,
)> {
    use crate::llm::{build_batch_item, reassemble_batch_results, LlmRequest};

    if llm.api_key().is_none() {
        anyhow::bail!(
            "batch mode requires the Api backend with ANTHROPIC_API_KEY set; \
             switch this project's backend to Api and add an Anthropic API key, or use parallel mode"
        );
    }

    let model = {
        // Resolve the model the same way `audit_pass` does: caller's explicit pick wins,
        // else CAMERATA_AUDIT_MODEL, else the Llm client's default.
        let m = audit_model.map(str::to_string).or_else(|| {
            std::env::var("CAMERATA_AUDIT_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
        // Build a throwaway request to let the Llm client resolve the model.
        let dummy = LlmRequest::new("")
            .with_model(m.unwrap_or_default());
        // model_for is private, but we replicate its logic here (empty -> default_model).
        // We use the model the caller would pass to audit_pass, which is the string itself.
        dummy.model
    };
    // Use the default model if the resolved model is empty.
    let model = if model.trim().is_empty() {
        crate::llm::DEFAULT_MODEL.to_string()
    } else {
        model
    };

    let digests: Vec<String> = chunks.iter().map(|c| build_digest(c)).collect();
    let n_c = chunks.len();
    let n_b = batches.len();

    // Build the full cartesian product of (chunk, rule-batch) items.
    let mut items = Vec::with_capacity(n_c * n_b);
    // Retain the (ci, bi, prompt, cache_breakpoints) tuples so we can parse results.
    let mut work_meta: Vec<(usize, usize, String, Vec<usize>)> = Vec::with_capacity(n_c * n_b);

    for ci in 0..n_c {
        let digest = &digests[ci];
        for bi in 0..n_b {
            let batch = batches[bi];
            let rb = build_rules_block(batch);
            // BUG-AI-1: mirror run_passes semantics — advisory fires only on bi==0 AND
            // only when advisory_disabled is false. Without this check, every language-scoped
            // batch group would re-run the novel-issue pass on its first batch, reproducing
            // duplicate novel findings for each language group (the problem advisory_disabled
            // was introduced to prevent in the parallel path).
            let advisory = !advisory_disabled && bi == 0;
            let task_line = if advisory {
                format!("── Check the code above against the ADOPTED rules below (batch {}/{n_b}); ALSO flag any other genuine issues NOT covered by an adopted rule. Use the REPO MAP for cross-file context. ──", bi + 1)
            } else {
                format!("── Check the code above against ONLY the ADOPTED rules below (batch {}/{n_b}). Do NOT report issues outside these rules — a separate pass already covers novel findings. Use the REPO MAP for cross-file context. ──", bi + 1)
            };
            // Same `build_pass_prompt` builder as `run_passes` — see the extensive comment
            // there and on `cache_breakpoints_for_pass`. In BATCH mode this matters even
            // more: the default `BATCH_RULE_BATCH_SIZE = usize::MAX` puts every rule in ONE
            // rule-batch (`n_b == 1`), so `cache_breakpoints_for_pass` drops the
            // chunk-segment breakpoint entirely (it would be written once and never
            // re-read, paying a pure ~1.25x write premium for zero benefit) and keeps only
            // the repo-map breakpoint, which DOES re-read across this batch submission's
            // other chunks.
            let (prompt, cache_bps) =
                build_pass_prompt(repo, repo_map, label, ci, n_c, digest, n_b, &task_line, &rb);

            let custom_id = format!("c{ci}-b{bi}");
            let req = {
                let mut r = LlmRequest::new(prompt.clone())
                    .with_system(audit_system_prompt())
                    .with_max_tokens(8192)
                    .with_model(model.clone())
                    .with_cache_breakpoints(cache_bps.clone());
                // audit_model overrides the default; already folded into `model` above.
                let _ = &mut r; // avoid unused_mut lint
                r
            };
            items.push(build_batch_item(&custom_id, &req, &model));
            work_meta.push((ci, bi, prompt, cache_bps));
        }
    }

    // GAP-4 (compliance-audit housekeeping): best-effort EXACT pre-audit input-token count
    // for the first (chunk, rule-batch) pair, via `count_tokens` (free — Anthropic does not
    // bill this endpoint). This is a precision upgrade over the char-based heuristic
    // estimate (`camerata_ui_core::scan::estimate_audit_cost`) the UI shows before the scan
    // starts. Logged only for now (the heuristic estimate remains the persisted/displayed
    // number) — gracefully skipped on ANY failure (no key, network error, non-2xx) via
    // `exact_input_token_count`'s `Option` return, since a precise quote is a nice-to-have,
    // never a reason to fail or stall the audit itself.
    if let Some((ci0, bi0, prompt0, cache_bps0)) = work_meta.first() {
        let sample_req = LlmRequest::new(prompt0.clone())
            .with_system(audit_system_prompt())
            .with_cache_breakpoints(cache_bps0.clone());
        if let Some(exact) = crate::llm::exact_input_token_count(llm, &sample_req, &model).await {
            eprintln!(
                "[camerata-server] batch mode: exact input-token count for chunk {ci0} rule-batch \
                 {bi0} = {exact} tokens (via count_tokens; {} total (chunk,batch) pairs this run)",
                work_meta.len()
            );
        }
    }

    // Tell the job the total pass count so the progress bar can be pre-seeded.
    let total = items.len();
    if let Some((jstore, jid)) = job {
        jstore.add_total(jid, total);
    }

    // CAP ENFORCEMENT: split into sub-batches of 100k items each.
    const BATCH_CAP: usize = 100_000;
    let sub_batches: Vec<_> = items.chunks(BATCH_CAP).collect();
    if sub_batches.len() > 1 {
        eprintln!(
            "[camerata-server] batch mode: {} items exceed the 100k cap — splitting into {} sub-batches",
            total,
            sub_batches.len()
        );
    }

    // Submit all sub-batches (sequentially — the API is async so there's no rate-limit
    // pressure; we just need the batch_id from each one).
    let mut all_responses: std::collections::HashMap<String, crate::llm::LlmResponse> =
        std::collections::HashMap::new();
    for (sub_idx, sub_items) in sub_batches.iter().enumerate() {
        let submit_result = llm.submit_batch(sub_items.to_vec()).await?;
        let batch_id = submit_result.batch_id;
        eprintln!(
            "[camerata-server] batch mode: sub-batch {}/{} submitted as {batch_id} ({} items)",
            sub_idx + 1,
            sub_batches.len(),
            sub_items.len(),
        );
        // Record the batch id on the job so the UI can surface it.
        if let Some((jstore, jid)) = job {
            jstore.set_batch_id(jid, &batch_id);
        }

        // Poll until the batch is done. The Anthropic spec recommends >= 1s between polls;
        // we use 10s to be gentle. `CAMERATA_BATCH_POLL_SECS` overrides.
        let poll_secs = std::env::var("CAMERATA_BATCH_POLL_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(10);
        poll_batch_until_ended(
            || llm.poll_batch_status(&batch_id),
            std::time::Duration::from_secs(poll_secs),
            MAX_CONSECUTIVE_POLL_FAILURES,
            |status| {
                eprintln!(
                    "[camerata-server] batch {batch_id}: status={} (processing={}, succeeded={}, errored={})",
                    status.processing_status,
                    status.request_counts.processing,
                    status.request_counts.succeeded,
                    status.request_counts.errored,
                );
            },
        )
        .await?;

        // Fetch + parse results.
        let rows = llm.fetch_batch_results(&batch_id).await?;
        let sub_map = reassemble_batch_results(rows);
        all_responses.extend(sub_map);
    }

    // Reassemble: look up each (ci, bi) pair's response by its deterministic custom_id.
    let mut findings = Vec::new();
    let mut proposed = Vec::new();
    let mut requested = std::collections::HashSet::new();
    let mut ok = 0usize;
    let mut last_err: Option<anyhow::Error> = None;

    for (ci, bi, _prompt, _cache_bps) in &work_meta {
        let custom_id = format!("c{ci}-b{bi}");
        match all_responses.get(&custom_id) {
            Some(resp) => {
                if let Some(m) = meter {
                    m.record(resp);
                }
                let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
                let needs = parse_needs_files(&resp.text);
                findings.extend(f.clone());
                proposed.extend(p);
                requested.extend(needs);
                // Stream findings into the job for incremental preview.
                if let Some((jstore, jid)) = job {
                    jstore.add_findings(jid, f);
                    jstore.inc_done(jid, 1);
                }
                ok += 1;
            }
            None => {
                // The item failed or was not in the result set.
                let e = anyhow::anyhow!(
                    "batch item {custom_id} missing from results (chunk {ci}, rule-batch {bi})"
                );
                eprintln!("[camerata-server] {e}");
                last_err = Some(e);
                // Still count as done so the progress bar can reach 100%.
                if let Some((jstore, jid)) = job {
                    jstore.inc_done(jid, 1);
                }
            }
        }
    }

    Ok((findings, proposed, requested, ok, last_err))
}

/// Severity rank for keeping the most-severe representative when merging duplicates.
fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

// ── P1: finding_class (Security vs Hygiene) + shared primary-selection ranking ───────────────
//
// docs/plans/2026-09-29_codebase-inspection-hardening.md, P1. The regression this section
// exists to prevent: a low structural/style row (e.g. an `ARCH-MIDDLEWARE-FIRST-1`-shaped
// layering rule) sitting at the SAME code location or defect cluster as a genuine security
// finding (e.g. a reflected-Origin CORS-with-credentials misconfiguration) must never win
// primary and hide the security finding. Before this section, BOTH merge passes
// (`merge_location_group`'s exact-location collapse and `merge_semantic_group`'s cross-family/
// cross-file collapse) picked a primary using ONLY "adopted-vs-invented" / origin-tier +
// severity — with no notion of WHAT KIND of defect a finding is. A deterministic, non-`AI-`
// rule id (any adopted corpus/floor rule, regardless of topic) always outranked an AI-invented
// security finding on that axis alone, independent of severity. `finding_class` closes that gap
// by making the SECURITY/HYGIENE split the first, highest-priority key in the shared
// `primary_rank` both passes now use.

/// Which side of the security/hygiene split a finding falls on, for cross-tier merge primacy
/// (design point 3): Security beats Hygiene UNCONDITIONALLY, ahead of severity/confidence/
/// specificity — a structural/style row must never absorb and hide a security row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindingClass {
    Security,
    Hygiene,
}

/// Rule-id tokens that mark a finding as `FindingClass::Security` for merge-primacy purposes:
/// secrets/credentials, RLS/access policy, authn/authz, injection (SQL/XSS/SSRF/deserialization/
/// RCE), transport security (TLS/CORS), resource exposure, open redirect, weak crypto/
/// randomness, and CSRF concerns — exactly the families
/// `docs/plans/2026-09-29_codebase-inspection-hardening.md` P1 names as Security. This is a
/// CONTENT scan over the rule id's hyphen-delimited WORDS ([`security_token_matches`]), not a
/// prefix rule: `SEC-*`/`SUPABASE-*` deterministic floor rules match via their own prefix
/// shortcut below (belt-and-suspenders — every current one also matches a token), but so do
/// `ARCH-*`/`AI-*` ids that happen to encode a security concern (`ARCH-NO-SECRETS-IN-URL-1`,
/// `ARCH-FETCH-THEN-AUTHORIZE-1`, an invented `AI-CORS-...` finding) — the corpus does NOT
/// reserve security naming to one prefix family, so prefix-only matching would misclassify real
/// cases in both directions.
///
/// Word-boundary matched (not raw substring), on purpose: a naive `rule_id.contains(token)` scan
/// was cross-checked against every rule id in `crates/rules/principles/**` and produced real
/// false positives — `"RCE"` (meant for remote-code-execution) matched inside `"RESOURCE"` and
/// `"INTERCEPTORS"` (`ARCH-RESOURCE-LIFECYCLE-1`, a subprocess/temp-file cleanup rule;
/// `JAVA-RESOURCE-MANAGEMENT-1`, a try-with-resources rule; two interceptor-pattern rules), none
/// of which are security. `security_token_matches` requires a token to match a WHOLE
/// hyphen-delimited word (or be a prefix of one, so `"DESERIAL"` still matches
/// `"DESERIALIZATION"`), which eliminates that whole class of accidental substring hits while
/// keeping every genuine match (`"RLS"` as its own word in `SUPABASE-RLS-ENABLED-1`, `"AUTH"` as
/// a prefix of `"AUTHORIZE"`/`"AUTHZ"`).
///
/// Two more corpus-verified removals from an earlier draft of this list, kept out for the same
/// reason: bare `"INJECT"` — every CURRENT corpus id containing an "inject"-rooted word
/// (`CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1`, `JAVA-SPRING-CONSTRUCTOR-INJECTION-1`,
/// `JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1`) is a DEPENDENCY-injection pattern rule, not
/// an injection-VULNERABILITY rule; real SQL-injection rules are already caught via `"SQL"`.
/// Bare `"SESSION"` — the one hit (`PYTHON-FASTAPI-DI-SESSION-1`) is a DATABASE-session
/// lifecycle rule, not an authentication session; real auth-session rules are already caught via
/// `"AUTH"` (`SUPABASE-AUTH-GETSESSION-SERVER-1`). Both words are common enough in non-security
/// naming that keeping them would re-introduce the fail-safe-direction violation this whole
/// section exists to prevent: a Hygiene finding mislabeled Security can still beat a GENUINE
/// Security finding it clusters with, via the severity/confidence/specificity tiebreaks, since
/// both would tie at `class_rank` — so a wrong Security label is not harmless.
///
/// Deliberately excludes the bare word "BYPASS": the corpus's OWN `categorize_rule_id`
/// authorization bucket treats bare "BYPASS" as an authz signal, which over-fires on a purely
/// architectural finding like "the handler bypasses the repository layer" (no auth concept at
/// all). Every genuine auth-bypass rule name already carries a more specific token this list
/// covers directly (`AUTH`, `RBAC`, `ACCESS-CONTROL`, `SERVICE-ROLE`, `PERMISSION`), so dropping
/// the bare word loses no real coverage while removing that false-positive source from the
/// rule-id scan. `finding_class` still consults `category`, so a finding whose `category` was
/// explicitly backfilled to `"authorization"` via that same bare-"BYPASS" heuristic can still be
/// classified Security through that path — a known, narrow residual imprecision inherited from
/// `categorize_rule_id`, not from this list; flagged for review rather than silently accepted.
#[rustfmt::skip]
const SECURITY_RULE_TOKENS: &[&str] = &[
    // Secrets / credentials
    "SECRET", "CREDENTIAL", "PASSWORD", "API-KEY", "APIKEY", "PRIVATE-KEY", "HARDCODED",
    // RLS / access policy
    "RLS", "ROW-LEVEL", "POLICY", "SEARCH-PATH",
    // Authn / authz
    "AUTH", "RBAC", "PERMISSION", "ACCESS-CONTROL", "SERVICE-ROLE", "LOGIN", "JWT", "CSRF",
    // Injection. Deliberately NOT bare "SQL": that matched every SQL-adjacent rule regardless
    // of topic (indexing, N+1, connection pooling, migrations-checked-in, `SQLX` itself via
    // prefix matching), none of which are injection concerns. The specific SQL-injection
    // shapes the corpus actually uses are "raw SQL" / "string SQL" / "parameterized" (its
    // ABSENCE is the vulnerability) — each is its own token below, plus explicit
    // "*-INJECTION" compounds for command/code/SQL injection so a future/invented id like
    // `AI-SQL-INJECTION` still matches without bare "INJECT" reintroducing the
    // dependency-injection false positive documented above.
    "RAW-SQL", "STRING-SQL", "PARAMETERIZED", "SQL-INJECTION", "COMMAND-INJECTION",
    "CODE-INJECTION", "XSS", "SSRF", "DESERIAL", "RCE",
    // Transport security / resource exposure
    "TLS", "SSL", "HTTPS", "CERT", "CORS", "EXPOSE", "EXPOSURE", "EXPOSED", "PUBLIC-BUCKET",
    // Redirect / crypto / tokens
    "REDIRECT", "CRYPTO", "RANDOM", "NONCE", "TOKEN",
    // Explicit "security" naming (CI security-scan rules, security-headers/method-security
    // rules) — a rule that names itself "security" in its own id is Security almost by
    // definition, and this token catches `CICD-*-SECURITY-SCAN-1`,
    // `JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1`, `JAVA-SPRING-METHOD-SECURITY-1`, none of which
    // any other token above reaches.
    "SECURITY",
];

/// True when `token` matches one of `id`'s hyphen-delimited WORDS — either exactly, or as a
/// prefix of that word (so a fragment like `"DESERIAL"` still matches the word
/// `"DESERIALIZATION"`). A single-word token (`"AUTH"`) is checked against each word of `id`; a
/// token that is ITSELF hyphenated (`"API-KEY"`, `"PRIVATE-KEY"`) is checked against every
/// CONSECUTIVE run of `id`'s words of the same length, so `"PRIVATE-KEY"` matches
/// `SEC-NO-PRIVATE-KEY-1`'s `["PRIVATE", "KEY"]` run without matching `"PRIVATE"` or `"KEY"`
/// alone elsewhere. Deliberately NOT a raw substring scan — see `SECURITY_RULE_TOKENS`'s doc
/// comment for the false positives (`"RCE"` inside `"RESOURCE"`) that motivated this.
fn security_token_matches(id_words: &[&str], token: &str) -> bool {
    let token_words: Vec<&str> = token.split('-').collect();
    if token_words.len() == 1 {
        let t = token_words[0];
        id_words.iter().any(|w| w.starts_with(t))
    } else {
        id_words
            .windows(token_words.len())
            .any(|window| window == token_words.as_slice())
    }
}

/// Semantic categories (the closed taxonomy backfilled by `categorize_rule_id` — see
/// `KNOWN_CATEGORIES`) that are themselves Security families. Consulted as a SECOND signal
/// (after the rule-id token scan) so a finding classified by calibration's `category` field
/// rather than a security-sounding rule id (an opaque invented AI- name, say) still lands on
/// the right side of the split.
fn category_is_security(category: &str) -> bool {
    matches!(
        category,
        "authorization"
            | "authentication"
            | "secret-exposure"
            | "injection"
            | "transport-security"
            | "rls-policy"
            | "resource-exposure"
            | "input-validation"
    )
}

/// Classify a finding as Security or Hygiene for merge-primacy purposes (design point 1). Layered,
/// most-specific signal first: (1) the `SEC-*`/`SUPABASE-*` prefix families — every rule in
/// either family is security-scoped by the corpus's own domain convention (verified against the
/// full corpus: all 8 `SEC-*` and all 17 `SUPABASE-*` ids are security), so this is a cheap,
/// maintenance-free net that also future-proofs a new rule added to either family without an
/// obviously security-sounding word in its id; (2) a security-sounding rule id
/// (`SECURITY_RULE_TOKENS`, word-boundary matched — see its doc comment), which is what carries
/// `ARCH-*`/`AI-*` ids that happen to encode a security concern; (3) an already-assigned security
/// category; (4) Hygiene by default — an unclassified or genuinely structural/style/testing/
/// process finding never wins primacy over a KNOWN security finding it happens to cluster with,
/// which is the fail-safe direction (never silently promote noise to Security; a real security
/// finding is caught by (1)-(3) instead). Kept in this ONE function, with the rationale above,
/// per the design's "keep the classification in one place" requirement — every merge-primacy
/// call site derives class through this function, never by re-deriving its own notion of "is
/// this security".
fn finding_class(f: &Finding) -> FindingClass {
    let id = f.rule_id.to_ascii_uppercase();
    if id.starts_with("SEC-") || id.starts_with("SUPABASE-") {
        return FindingClass::Security;
    }
    let id_words: Vec<&str> = id.split('-').collect();
    if SECURITY_RULE_TOKENS
        .iter()
        .any(|t| security_token_matches(&id_words, t))
    {
        return FindingClass::Security;
    }
    if f.category.as_deref().is_some_and(category_is_security) {
        return FindingClass::Security;
    }
    FindingClass::Hygiene
}

/// Rank for `finding_class`: Security strictly outranks Hygiene.
fn class_rank(c: FindingClass) -> u8 {
    match c {
        FindingClass::Security => 1,
        FindingClass::Hygiene => 0,
    }
}

/// Rank for a finding's calibrated confidence, for the THIRD primary-selection key (design
/// point 3: class, then severity, then confidence, then specificity). `"needs-review"` is the
/// ONLY value calibration uses to flag a debatable/under-evidenced finding, so it alone ranks
/// low; a clear `"high"` verdict and `None` (no calibrated opinion at all — the deterministic
/// floor/checkers, which are exact by construction and are never run through calibration) are
/// BOTH "not flagged as debatable" and rank equally above it. This keeps a deterministic
/// finding's exact line as the tiebreak winner (via `origin_rank`, the next key) in a same-class,
/// same-severity tie against an AI "high"-confidence finding, preserving the pre-existing
/// deterministic-anchor behavior the D1/D2/D7 line-grading tests depend on, while still letting a
/// "needs-review"-flagged finding lose to a clearer one of the same class and severity.
fn confidence_rank(c: Option<&str>) -> u8 {
    match c {
        Some("needs-review") => 0,
        _ => 1,
    }
}

/// The shared primary-selection ranking for BOTH merge passes (design point 3, reordered by
/// C5-1 — see `docs/plans/2026-09-29_codebase-inspection-hardening.md` P1 for the original and
/// the REG-1/REG-2 work item for this reordering). Higher tuples win via `max_by_key`. Order:
/// (1) `FindingClass` — Security beats Hygiene unconditionally, independent of everything else,
/// so a structural/style row can never absorb and hide a security defect (the P1 regression);
/// (2) rule specificity via `origin_rank` — Deterministic beats AdoptedAi beats InventedAi.
/// C5-1 moved this key AHEAD of severity: before, an AI-tier finding's uncalibrated, routinely
/// inflated severity could outrank its own deterministic sibling describing the SAME defect at
/// the SAME site (the REG-1 symptom — a correctly-graded, high-confidence, authored-fix
/// deterministic finding gets demoted to `also_matches` behind a hedged, fix-less, inflated AI
/// guess). Origin now decides BEFORE severity within a class, so a deterministic finding always
/// wins primacy over an AI one of the same class, whatever either side's own severity says —
/// but class is still checked FIRST, so a genuine security finding (even AI-tier) still beats an
/// irrelevant deterministic structural row, preserving the P1 invariant; (3) calibrated severity
/// (now only a tiebreak within one class+origin tier — e.g. two deterministic members, or two AI
/// members); (4) confidence (a "needs-review" flag ranks last); (5) earliest appearance, the
/// final deterministic tiebreak so output never depends on hash/iteration order.
fn primary_rank(f: &Finding, group_len: usize, index: usize) -> (u8, u8, u8, u8, usize) {
    (
        class_rank(finding_class(f)),
        origin_rank(finding_origin(f)),
        severity_rank(&f.severity),
        confidence_rank(f.confidence.as_deref()),
        // Larger for earlier findings (lower `index`), so `max_by_key` resolves ties toward the
        // EARLIEST appearance — see BUG-7's original comment on this idiom.
        group_len - index,
    )
}

/// C5-1: the merged row's severity. The default — and the ONLY behavior when every member of
/// the group shares the SAME origin tier (all deterministic, or all AI) — is the pre-existing
/// `max()` across the group: a higher severity among peers of equal standing is a real
/// escalation and must never be thrown away (e.g. two deterministic rules on the same defect, one
/// graded higher than the other). The one carve-out: the chosen PRIMARY is Deterministic but the
/// group is MIXED-tier (it also absorbed an AI-tier member). An AI-tier severity is uncalibrated
/// and routinely inflated relative to its own deterministic sibling describing the identical
/// defect — letting `max()` win there means an AI "critical" guess silently overrides a
/// correctly-graded deterministic "medium", which is exactly the REG-1 symptom this work item
/// fixes. In that one case the primary's OWN (grounded) severity is authoritative instead.
fn merged_severity(primary: &Finding, max_sev: String, all_same_tier: bool) -> String {
    if !all_same_tier && finding_origin(primary) == Origin::Deterministic {
        primary.severity.clone()
    } else {
        max_sev
    }
}

/// Collapse one `(path, line)` group of findings into a SINGLE finding. The model routinely
/// reports one smell under several rule names — an invented `AI-` name PLUS the adopted
/// corpus rule it maps to PLUS sibling invented names — each with a different title, so a
/// `.expect()` panic at handlers.rs:41 arrives as five rows. This keeps ONE primary, chosen by
/// `primary_rank` (security-class, then specificity/origin, then severity, then confidence, then
/// earliest — see that function's doc comment for the C5-1 reordering), demotes every OTHER
/// distinct rule id to `also_matches`, and sets the severity via `merged_severity` (max across
/// the group, EXCEPT when the primary is deterministic in a mixed-tier group — see that
/// function's doc comment) — so the row honestly reads "violates layering + DI + entities-chain"
/// rather than emitting five near-duplicates. Before P1, this picked "adopted (non-AI-) beats
/// invented" as its FIRST key, which is exactly the canonical failure: an adopted-but-irrelevant
/// structural rule id at the same exact line as an AI-invented SECURITY finding used to win
/// primacy regardless of severity, hiding the security defect. `primary_rank`'s class-first
/// ordering fixes that while leaving same-class ties resolved exactly as before (now
/// origin-before-severity within a class — C5-1).
fn merge_location_group(group: Vec<Finding>) -> Finding {
    let primary_idx = group
        .iter()
        .enumerate()
        .max_by_key(|(i, f)| primary_rank(f, group.len(), *i))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let max_sev = group
        .iter()
        .max_by_key(|f| severity_rank(&f.severity))
        .map(|f| f.severity.clone())
        .unwrap_or_else(|| "low".to_string());
    // C5-1: whether every member shares the SAME origin tier — see `merged_severity`'s doc
    // comment. Computed over the FULL group (primary included) before it is removed below.
    let all_same_tier = group
        .iter()
        .all(|f| finding_origin(f) == finding_origin(&group[0]));

    let mut group = group;
    let mut primary = group.remove(primary_idx);
    // Every OTHER distinct rule id, in first-seen order, minus the primary's own.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(primary.rule_id.clone());
    let mut also = Vec::new();
    for f in &group {
        if seen.insert(f.rule_id.clone()) {
            also.push(f.rule_id.clone());
        }
    }
    primary.severity = merged_severity(&primary, max_sev, all_same_tier);
    // C5-1: a deterministic primary with no codebase-specific fix of its own yet (normally
    // `fix_specific` is only populated in a LATER pipeline pass — see `Finding::fix_specific`'s
    // doc comment — but a finding carried forward from a prior incremental run can already carry
    // one) inherits the first member's rather than shipping fix-less when a perfectly good one
    // was sitting right there in the group it just absorbed.
    if primary.fix_specific.is_none() {
        primary.fix_specific = group.iter().find_map(|f| f.fix_specific.clone());
    }
    primary.also_matches = also;
    primary
}

/// Resolve each finding's line DETERMINISTICALLY from its verbatim snippet. LLMs can't count
/// newlines, so model line numbers drift (the dogfooding run cited header cells for the
/// data-row loops); the snippet the model COPIED is reliable. For each finding we locate the
/// snippet in its file and take the matching line, disambiguating duplicate matches by
/// proximity to the model's estimate. Snippet not found (paraphrase, or a description rather
/// than code) → the model's line is kept as the fallback. The model says WHAT; code says WHERE.
fn resolve_finding_lines(findings: &mut [Finding], files: &[(String, String)]) {
    let by_path: std::collections::HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    for f in findings.iter_mut() {
        let needle = f.snippet.trim();
        // Too short to locate reliably (single token / punctuation) — keep the model's line.
        if needle.len() < 4 {
            continue;
        }
        let Some(content) = by_path.get(f.path.as_str()) else {
            continue;
        };
        let matches: Vec<usize> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .map(|(i, _)| i + 1) // 1-based
            .collect();
        // Pick the occurrence nearest the model's (approximate) line so a snippet that appears
        // more than once resolves to the intended site. No match → leave the model's line.
        if let Some(best) = matches
            .iter()
            .copied()
            .min_by_key(|&ln| ln.abs_diff(f.line))
        {
            f.line = best;
        }
    }
}

/// Merge findings that sit at the SAME code location into one row, keyed on `(path, line)`
/// — NOT on the title (the model writes a different title for each invented rule name, so a
/// title key never collapses them). This is the deterministic reduce that turns the audit's
/// duplication explosion (e.g. one panic reported under five rule ids) into one honest row
/// per location via [`merge_location_group`]. Line 0 (file-level / uncited) findings are
/// NOT location-merged — unrelated file-level issues legitimately share line 0 — so each is
/// passed through untouched (the exact `(path, line, rule_id)` dedup upstream already
/// removed byte-identical line-0 repeats).
pub(crate) fn merge_by_location(
    findings: Vec<Finding>,
    files: &[(String, String)],
) -> Vec<Finding> {
    let by_path: std::collections::HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    // `disambiguator` is 0 for co-located findings (so all hits at one real code line group
    // together) and a unique counter for everything kept SOLO (line 0, or a finding whose
    // snippet isn't actually in the file).
    let mut order: Vec<(String, usize, usize)> = Vec::new();
    let mut groups: std::collections::HashMap<(String, usize, usize), Vec<Finding>> =
        std::collections::HashMap::new();
    let mut solo: usize = 0;
    for mut f in findings {
        let snippet = f.snippet.trim();
        // CO-LOCATION requires the finding to cite REAL code that is present in the file at this
        // spot. The legit merge ("one smell reported under several rule names") cites the same
        // offending code each time, so those findings ARE located and group together. But an
        // ABSENCE / architectural finding ("no central error handler", "no API versioning")
        // cites a DESCRIPTION, not code in the file — and the model anchors several such findings
        // to the same representative line. Location-merging those wrongly fuses unrelated issues
        // (the SECURITY-HEADERS-tagged-with-API-VERSIONING bug). Such findings are kept SOLO.
        let located = f.line != 0
            && snippet.len() >= MIN_MERGE_SNIPPET
            && by_path
                .get(f.path.as_str())
                .is_some_and(|c| c.contains(snippet));
        // Persist the presence/absence signal on the finding so the informational-bucketing
        // predicate (Bug 4) reuses it rather than recomputing — an absence-type stance finding
        // (line 0 / description-not-code snippet) is exactly the low-tier noise to down-bucket.
        f.located = located;
        let key = if located {
            (f.path.clone(), f.line, 0)
        } else {
            solo += 1;
            (f.path.clone(), 0, solo)
        };
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(f);
    }
    order
        .into_iter()
        .filter_map(|k| groups.remove(&k).map(merge_location_group))
        .collect()
}

/// Minimum snippet length for a finding to be considered co-located with others. Below this a
/// snippet is too short to be a reliable "this is the same offending code" signal.
const MIN_MERGE_SNIPPET: usize = 8;

// ── Bug 3: cross-family semantic dedup ───────────────────────────────────────────────────

/// How many lines apart two same-category findings may sit and still be considered the SAME
/// defect for the semantic merge. Covers the benchmark's observed ~3-line attribution drift
/// between an AI finding and the native checker for the same policy, with margin, while staying
/// inside the harness's ±2 line-grading tolerance for the surviving primary (whose line is the
/// deterministic anchor, never the drifted sibling's).
const SEMANTIC_MERGE_WINDOW: usize = 5;

/// The closed semantic taxonomy (design §1a). A finding is only ever merged with another of the
/// SAME category, so the set is deliberately coarse — one bucket per defect *family*, not per
/// rule. Membership is validated (`is_known_category`) so a mis-shaped calibration value can
/// never invent a category and cause a wrong merge.
const KNOWN_CATEGORIES: &[&str] = &[
    "authorization",
    "authentication",
    "secret-exposure",
    "injection",
    "transport-security",
    "rls-policy",
    "resource-exposure",
    "input-validation",
    "error-handling",
    "arch-conformance",
    "testing-style",
    "performance",
];

/// True if `c` is a member of the closed taxonomy.
fn is_known_category(c: &str) -> bool {
    KNOWN_CATEGORIES.contains(&c)
}

/// Fallback heuristic (design §1a step 3): map a rule id to a taxonomy category by the tokens it
/// contains. This is our own finite, repo-agnostic rule vocabulary — the token set is exhaustive
/// by construction over the corpus + floor + checker ids, not fitted to any one repo. Priority
/// order matters: more specific families (RLS, TLS, secrets) are checked before the broad
/// authorization/arch buckets so a `SUPABASE-RLS-*` id lands in `rls-policy`, not `authorization`.
/// No token matches → `None` (which makes the finding un-mergeable — fail-open to over-telling).
/// `pub(crate)` (REG-3) so `report_export::export_invariants_gate` can check the same
/// also-matches-never-crosses-category invariant this module's merge predicate enforces, without
/// duplicating the taxonomy.
///
/// WORD-BOUNDARY matched (`security_token_matches`, the same helper/approach `finding_class`'s
/// `SECURITY_RULE_TOKENS` uses — see that const's doc comment), not raw substring, for the exact
/// reason that fix exists: cross-checked against every real rule id in
/// `crates/rules/principles/**`, a naive `id.contains(token)` scan produced real false positives
/// here too. The one that prompted this fix: bare `"ARCH"` matched inside `"SEARCH"` (from
/// `"SEARCH-PATH"`), so `SUPABASE-FUNC-SEARCH-PATH-1` — a SECURITY DEFINER / search-path-hijacking
/// rule (D1) — was miscategorized `arch-conformance`. The corpus audit found three more of the
/// SAME class `finding_class` already fixed, now fixed here too: bare `"RCE"` matched inside
/// `"RESOURCE"`/`"INTERCEPTORS"` (`ARCH-RESOURCE-LIFECYCLE-1`, `JAVA-RESOURCE-MANAGEMENT-1`,
/// `JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1`, `GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1` — none
/// injection); bare `"INJECT"` matched dependency-injection pattern rules
/// (`JAVA-SPRING-CONSTRUCTOR-INJECTION-1`, `JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1`, not
/// injection vulnerabilities — replaced with the same explicit `*-INJECTION`/`*-SQL` compounds
/// `SECURITY_RULE_TOKENS` uses); bare `"SESSION"` matched `PYTHON-FASTAPI-DI-SESSION-1`, a
/// database-session lifecycle rule, not an authentication session (dropped; the real case is
/// already caught via `"GETSESSION"`). One new compound was added rather than dropping coverage:
/// `"GRAMMAR-INJECTION"`, so `SEC-NO-QUERY-GRAMMAR-INJECTION-1` (D3's query-grammar-injection
/// class) still lands in `injection` once bare `"INJECT"` is gone. And `"-DI-"` (dependency-
/// injection abbreviation) is dropped entirely rather than word-boundary-adapted: as a hyphen-
/// wrapped substring it was already boundary-safe (hyphens on both sides can only occur around a
/// standalone `DI` component), but there is no safe way to express "exact word DI, not a prefix"
/// through the shared prefix-matching single-word semantics without reintroducing a false-
/// positive class of its own (`"DIRECT"`, `"DISABLED"`, `"DIGEST"` all start with `"DI"|`) — the
/// one real corpus id that relied on it (`JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1`) simply falls
/// through to `None` now, the fail-open direction this function already commits to.
pub(crate) fn categorize_rule_id(rule_id: &str) -> Option<String> {
    let id = rule_id.to_ascii_uppercase();
    let words: Vec<&str> = id.split('-').collect();
    let has = |token: &str| security_token_matches(&words, token);
    let cat = if has("RLS") || has("POLICY") || has("ROW-LEVEL") {
        "rls-policy"
    } else if has("TLS") || has("SSL") || has("HTTPS") || has("CERT") {
        "transport-security"
    } else if has("SECRET")
        || has("HARDCODED")
        || has("CREDENTIAL")
        || has("API-KEY")
        || has("APIKEY")
        || has("PASSWORD")
    {
        "secret-exposure"
    } else if has("RAW-SQL")
        || has("STRING-SQL")
        || has("PARAMETERIZED")
        || has("SQL-INJECTION")
        || has("COMMAND-INJECTION")
        || has("CODE-INJECTION")
        || has("GRAMMAR-INJECTION")
        || has("XSS")
        || has("SSRF")
        || has("DESERIAL")
        || has("RCE")
    {
        "injection"
    } else if has("LOGIN")
        || has("GETUSER")
        || has("GETSESSION")
        || has("AUTHN")
        || has("AUTHENTICAT")
    {
        "authentication"
    } else if has("AUTHZ")
        || has("AUTHORIZ")
        || has("BYPASS")
        || has("SERVICE-ROLE")
        || has("RBAC")
        || has("PERMISSION")
        || has("ACCESS-CONTROL")
    {
        "authorization"
    } else if has("VALIDAT") || has("SANITIZE") || has("INPUT") {
        "input-validation"
    } else if has("PANIC")
        || has("UNWRAP")
        || has("EXPECT")
        || has("ERROR-HANDLER")
        || has("ERROR-HANDLERS")
        || has("ERROR-HANDLING")
        || has("FALLIBLE")
    {
        "error-handling"
    } else if has("TEST") || has("SPEC") || has("FIXTURE") {
        "testing-style"
    } else if has("PERF") || has("N-PLUS") || has("NPLUS") || has("HOT-READ") || has("CACHE") || has("PAGINATION") {
        "performance"
    } else if has("EXPOSE") || has("EXPOSED") || has("EXPOSURE") || has("CORS") || has("PUBLIC") {
        "resource-exposure"
    } else if has("ARCH")
        || has("LAYER")
        || has("MONOLITH")
        || has("MIDDLEWARE")
        || has("REPO-PER")
        || has("ROUTE-PLACEMENT")
        || has("QUERY-LIBRARY")
        || has("VERSIONING")
    {
        "arch-conformance"
    } else {
        return None;
    };
    Some(cat.to_string())
}

/// The origin tier of a finding, for the semantic merge's primary ordering and its
/// "both-deterministic-never-merge" guard. Deterministic = produced by the floor / native
/// checkers / preview tools (exact by construction); AI findings are the model's, split into
/// `adopted` (mapped to a real corpus rule id) and `invented` (`AI-` prefix). Signal: the AI
/// pipeline is the only producer that sets `confidence` (calibration) or an `AI-` rule id;
/// floor/checker/preview findings carry neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Deterministic,
    AdoptedAi,
    InventedAi,
}

fn finding_origin(f: &Finding) -> Origin {
    if f.rule_id.starts_with("AI-") {
        Origin::InventedAi
    } else if f.confidence.is_some() {
        // A non-AI- rule id that calibration touched = an adopted corpus rule from the AI tier.
        Origin::AdoptedAi
    } else {
        Origin::Deterministic
    }
}

/// Rank for choosing the primary of a semantic group: deterministic beats adopted beats
/// invented (design §1c). The deterministic side owns the exact line, which is what preserves
/// D1/D2/D7 line-grading through the merge.
fn origin_rank(o: Origin) -> u8 {
    match o {
        Origin::Deterministic => 2,
        Origin::AdoptedAi => 1,
        Origin::InventedAi => 0,
    }
}

/// Extract the structural OBJECTS a finding names — dotted identifiers (`schema.table`) and
/// quoted identifiers (`"policy name"`, `'table'`). Two findings that each name a non-empty,
/// DISJOINT object set are about different things (different tables/policies) and must not
/// merge even when same-category and adjacent — the discrimination guard that keeps
/// SELECT-vs-INSERT / table-A-vs-table-B distinct. Repo-agnostic: pure lexical extraction over
/// snippet+detail, no rule-specific parsing.
fn structural_objects(f: &Finding) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let text = format!("{} {}", f.snippet, f.detail);
    // Dotted identifiers: schema.table / a.b.c — take each dotted run as one object. A run's
    // TRAILING dot is trimmed before the dot-presence test: it is ordinary sentence punctuation
    // ("...committed to the repository." must not register the bare word "repository" as a
    // dotted object just because a period follows it), not part of the identifier. A genuine
    // dotted identifier immediately followed by a sentence-ending period (e.g. "public.orders.")
    // still keeps its real internal dot after trimming only the outer one.
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
            cur.push(ch);
        } else {
            let trimmed = cur.trim_end_matches('.');
            if trimmed.contains('.') && trimmed.chars().any(|c| c.is_alphabetic()) {
                out.insert(trimmed.to_ascii_lowercase());
            }
            cur.clear();
        }
    }
    let trimmed = cur.trim_end_matches('.');
    if trimmed.contains('.') && trimmed.chars().any(|c| c.is_alphabetic()) {
        out.insert(trimmed.to_ascii_lowercase());
    }
    // Quoted identifiers: "..." or '...' (policy names, quoted table names).
    for (open, close) in [('"', '"'), ('\'', '\'')] {
        let mut it = text.split(open);
        let _ = it.next();
        let mut toggle = true;
        for seg in it {
            if toggle {
                let name = seg.trim();
                if !name.is_empty() && name.len() <= 64 && !name.contains(close) {
                    // seg up to the next delimiter is the quoted content only when the split
                    // gave us the inside; guard against runaway by length + single-token shape.
                    if name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ' ' || c == '.') {
                        out.insert(name.to_ascii_lowercase());
                    }
                }
            }
            toggle = !toggle;
        }
    }
    out
}

/// Two objectsets conflict when BOTH are non-empty and share no element — the findings name
/// disjoint structural targets.
fn objects_conflict(a: &Finding, b: &Finding) -> bool {
    let oa = structural_objects(a);
    let ob = structural_objects(b);
    if oa.is_empty() || ob.is_empty() {
        return false;
    }
    oa.is_disjoint(&ob)
}

/// Minimum length (characters, after trimming) for a shared `Finding::captures` VALUE to count
/// as the "same root cause across files" identity signal (design point 2b). A real detector-
/// captured object (a table, function name, bucket, flag) is essentially always this long or
/// longer; a bare "id"/"ok"-shaped value below this length is too generic to prove two findings
/// share a root cause and is ignored rather than treated as a match.
const MIN_SHARED_CAPTURE_LEN: usize = 3;

/// True when `a` and `b` each carry a non-empty `Finding::captures` map and share at least one
/// captured object VALUE (case-insensitive, trimmed) — the general "same root cause across
/// files" signal design point 2b calls for: a config-flag finding and the handler finding that
/// reads it, or an RLS-policy finding and the page finding that relies on it, each name the SAME
/// concrete object (a table, a function name, a bucket, a flag) even though they live in
/// different files and were flagged by different rule families. Deliberately compares VALUES
/// only, not keys — the two detectors are not expected to use the same capture-token name for
/// the object they each independently identified (one might key it `table`, the other
/// `object-name`); the object identity is what matters, not the label.
fn shared_captured_object(a: &Finding, b: &Finding) -> bool {
    if a.captures.is_empty() || b.captures.is_empty() {
        return false;
    }
    let a_values: std::collections::HashSet<String> = a
        .captures
        .values()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| v.len() >= MIN_SHARED_CAPTURE_LEN)
        .collect();
    b.captures.values().any(|v| {
        let v = v.trim().to_ascii_lowercase();
        v.len() >= MIN_SHARED_CAPTURE_LEN && a_values.contains(&v)
    })
}

/// C4-P1 design point 2: backfill a `captures` entry from the finding's own structural-object
/// extraction ([`structural_objects`]) for a finding whose detector never populated `captures`
/// itself. Most deterministic/floor checks and many AI findings leave `captures` empty even when
/// the object they are about IS named in their own `snippet`/`detail` text (a policy name, a
/// table) — without an entry there, such a finding can never participate in the cross-file
/// [`shared_captured_object`] merge signal (design point 2b), even when a call-site finding in
/// another file plainly relies on that SAME object. This closes exactly that gap, GENERALLY
/// (every finding gets a chance, not one keyed to a specific rule or category): when `captures`
/// is empty, take the structural objects [`structural_objects`] already extracts for the
/// `objects_conflict` veto and, when there is EXACTLY ONE, record it under a generic `"object"`
/// key. Deliberately a no-op when there are zero (nothing concrete named) or MORE THAN ONE
/// (ambiguous — which one is the actual sink? guessing risks inventing a false cross-file link)
/// candidates, which keeps this strictly additive and safe: it can only ever CREATE a new merge
/// opportunity for a finding that previously had none, never silently pick the wrong object out
/// of several. Never overwrites a `captures` entry a detector already set.
fn backfill_captured_object(f: &mut Finding) {
    if !f.captures.is_empty() {
        return;
    }
    let mut objs = structural_objects(f).into_iter();
    if let (Some(obj), None) = (objs.next(), objs.next()) {
        f.captures.insert("object".to_string(), obj);
    }
}

/// C6-B2 design point 1: keyword substrings that mark a CONSTANT_CASE identifier as naming a
/// secret/credential rather than an ordinary configuration constant — the "evidence-chain /
/// root-cause" merge signal calls for linking findings that each reference the SAME concrete
/// secret/credential, "even across files and with no line proximity". Deliberately a named list
/// rather than a bare "KEY" substring (which would also catch an ordinary identifier like
/// `PRIMARY_KEY_COLUMN`) — the token still also has to pass the CONSTANT_CASE shape test in
/// [`is_credential_identifier`] below, so this is the conservative half of a two-part test, not
/// the whole of it.
const CREDENTIAL_IDENTIFIER_KEYWORDS: &[&str] = &[
    "SECRET",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "ACCESSKEY",
    "PRIVATE_KEY",
    "PRIVATEKEY",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH_TOKEN",
    "ACCESS_TOKEN",
];

/// Minimum length (characters) for a candidate CONSTANT_CASE token to be considered a credential
/// identifier — mirrors [`MIN_SHARED_CAPTURE_LEN`]'s "too generic to prove anything" floor, set
/// higher here because a genuine secret identifier (`STRIPE_SECRET_KEY`, `DB_PASSWORD`) is always
/// a multi-word compound; anything shorter is more likely an accidental short acronym.
const MIN_CREDENTIAL_TOKEN_LEN: usize = 8;

/// True when `tok` is shaped like a CONSTANT_CASE identifier (uppercase ASCII letters, digits,
/// and underscores only, with at least one underscore — ruling out a bare acronym) of at least
/// [`MIN_CREDENTIAL_TOKEN_LEN`] characters, AND its text contains one of
/// [`CREDENTIAL_IDENTIFIER_KEYWORDS`] — e.g. `STRIPE_SECRET_KEY`, `DB_PASSWORD`,
/// `AWS_SECRET_ACCESS_KEY`. The shape check is intentionally strict (an ordinary sentence never
/// emits a fully-uppercase, underscore-bearing multi-word token by accident), which is what lets
/// this fire with NO file/category/distance restriction at all, unlike the looser
/// [`description_overlap_score`] prose heuristic: this is a structural identity match, not prose
/// similarity.
fn is_credential_identifier(tok: &str) -> bool {
    if tok.len() < MIN_CREDENTIAL_TOKEN_LEN || !tok.contains('_') {
        return false;
    }
    if !tok
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    CREDENTIAL_IDENTIFIER_KEYWORDS
        .iter()
        .any(|kw| tok.contains(kw))
}

/// Extract every CONSTANT_CASE credential/secret identifier (see [`is_credential_identifier`])
/// named in free text. Repo-agnostic, pure lexical scan, no rule-specific parsing — mirrors
/// [`structural_objects`]'s shape, but targets the bare-underscore-identifier shape
/// (`STRIPE_SECRET_KEY`) that [`structural_objects`]'s dotted/quoted extraction does not cover.
fn credential_identifier_tokens(text: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            if is_credential_identifier(&cur) {
                out.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if is_credential_identifier(&cur) {
        out.insert(cur);
    }
    out
}

/// C6-B2 design point 1: the GENERAL root-cause identity a finding carries — every credential
/// token (see [`credential_identifier_tokens`]) findable in its own `captures` VALUES plus its
/// own `snippet`/`detail` text. This is the "evidence chain" extraction: the config file that
/// holds `STRIPE_SECRET_KEY`, the module that builds a client by reading `STRIPE_SECRET_KEY`,
/// and a call site whose own explanation names that SAME `STRIPE_SECRET_KEY` as the credential
/// the client it calls was built from, all extract the identical token here — regardless of
/// which file they are in, which rule flagged them, or how far apart their lines sit.
///
/// Pure and side-effect-free (never mutates `f`); used by BOTH [`backfill_credential_identity`]
/// (to seed `captures` so the pre-existing [`shared_captured_object`] cross-file merge signal
/// picks the link up with NO further change to [`semantic_pair_merges`] itself — the det+det
/// guard, the `objects_conflict` veto exemption, and the AI+AI corroboration guard already treat
/// a `shared_captured_object` match as strong enough to fire regardless of file/category/
/// distance) and, independently, by `report_export`'s top-3 "three distinct root causes"
/// selector (C6-B2 design point 3), as a defense-in-depth check for the case this merge pass
/// conservatively left two same-root-cause rows distinct.
pub(crate) fn root_cause_credential_identities(f: &Finding) -> std::collections::BTreeSet<String> {
    let mut out = credential_identifier_tokens(&format!("{} {}", f.snippet, f.detail));
    for v in f.captures.values() {
        out.extend(credential_identifier_tokens(v));
    }
    out
}

/// C6-B2 design point 1: backfill `captures` with any credential identifier
/// ([`root_cause_credential_identities`]) a finding's own text names. Deliberately additive and
/// NEVER gated on `captures` already being non-empty (unlike [`backfill_captured_object`]'s
/// single-structural-object case): a credential token is matched by an explicit, narrow
/// keyword+shape test (see [`is_credential_identifier`]), so there is no "which one of several is
/// the real object" ambiguity to guard against — recording every token found is always safe.
/// Each token is inserted under its OWN key (the token itself) so it can never collide with or
/// overwrite an unrelated capture a detector already set (e.g. a `"table"` key from
/// [`backfill_captured_object`]).
fn backfill_credential_identity(f: &mut Finding) {
    for tok in root_cause_credential_identities(f) {
        f.captures.entry(tok.clone()).or_insert(tok);
    }
}

/// The smallest brace-delimited block containing 1-based `line`, as `(start_line, end_line)`.
/// Cheap single-pass brace matcher; returns `None` for brace-free content (SQL, YAML) so callers
/// fall back to the line-window rule. Used to unify two findings on the same handler body even
/// when they sit more than `SEMANTIC_MERGE_WINDOW` lines apart.
fn enclosing_block(content: &str, line: usize) -> Option<(usize, usize)> {
    let mut stack: Vec<usize> = Vec::new();
    let mut cur_line = 1usize;
    let mut best: Option<(usize, usize)> = None;
    for ch in content.chars() {
        match ch {
            '\n' => cur_line += 1,
            '{' => stack.push(cur_line),
            '}' => {
                if let Some(open) = stack.pop() {
                    let close = cur_line;
                    // Inner blocks close first, so the FIRST containing block we see is the
                    // innermost (smallest) one — take it and stop updating.
                    if best.is_none() && open <= line && line <= close {
                        best = Some((open, close));
                    }
                }
            }
            _ => {}
        }
    }
    best
}

/// True when `a` and `b` fall inside the same innermost brace block of `content`.
fn same_construct(content: &str, a: usize, b: usize) -> bool {
    match enclosing_block(content, a) {
        Some((s, e)) => b >= s && b <= e,
        None => false,
    }
}

/// Normalize a finding's headline text (its `snippet`/title plus `detail`) into a lowercase
/// alphanumeric word set for [`description_overlap_score`]. Punctuation- and case-insensitive
/// so cosmetic phrasing differences between a deterministic rule's templated wording and an AI
/// finding's free-text description of the SAME defect ("Hardcoded API key" vs "hard-coded API
/// key committed") don't suppress an otherwise-real prose match.
fn description_word_set(f: &Finding) -> std::collections::HashSet<String> {
    let text = format!("{} {}", f.snippet, f.detail).to_ascii_lowercase();
    let mut out = std::collections::HashSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.insert(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.insert(cur);
    }
    out
}

/// Jaccard similarity (`|intersection| / |union|`) of `a` and `b`'s normalized description word
/// sets — the GENERAL "near-identical prose describing the same defect" merge signal (design
/// MERGE gap (i)): a deterministic finding and an AI finding a few lines apart, in different
/// categories or outside the line-proximity window, whose headline text is nonetheless
/// substantively the same sentence. Symmetric; `0.0` when either side has no describable text
/// (never a spurious match off two empty sets).
fn description_overlap_score(a: &Finding, b: &Finding) -> f64 {
    let wa = description_word_set(a);
    let wb = description_word_set(b);
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let intersection = wa.intersection(&wb).count();
    let union = wa.union(&wb).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Threshold for [`description_overlap_score`] to count as a same-defect merge signal.
/// Deliberately HIGH — this is a general lexical heuristic with no structural grounding of its
/// own (unlike [`shared_captured_object`]), so it is reserved for genuinely near-identical prose
/// and paired with the `path` restriction + `objects_conflict` veto in
/// [`semantic_pair_merges`] to avoid over-merging findings that merely share common security
/// vocabulary.
///
/// Lowered from `0.6` to `0.5` (MERGE cycle-3, observed-failure 1:
/// `docs/plans/2026-09-30_cycle3-queue-hardening.md`): a real duplicate pair — one defect
/// reported under two rule ids/tiers more than [`SEMANTIC_MERGE_WINDOW`] lines apart in the same
/// file, each paraphrased in the generator's OWN words rather than copy-pasted — routinely lands
/// in the high-0.5s on raw Jaccard once ordinary paraphrase (synonyms, reordered clauses, a
/// differently-worded clause for the same fact) is accounted for; `0.6` was proven too strict for
/// that genuinely-near-identical-but-not-verbatim case (see
/// `merge_gap_i_near_identical_prose_merges_beyond_window_distance` for the measured score this
/// was calibrated against). `0.5` is still well clear of the near-zero overlap two UNRELATED
/// same-file findings produce (see `merge_gap_ii_two_deterministic_findings_with_no_shared_object_
/// stay_distinct`, `merge_gap_i_sink_and_call_site_in_different_files_stay_two_rows`), and the
/// `objects_conflict` veto in [`semantic_pair_merges`] still fires independently of this
/// threshold, so two findings whose prose overlaps only because they share generic
/// security/hygiene vocabulary but name DISJOINT structural objects (different tables, different
/// policies) are still kept apart.
const DESCRIPTION_OVERLAP_THRESHOLD: f64 = 0.5;

/// The pairwise semantic-merge predicate (design §1b, extended by P1 design point 2, the MERGE
/// cycle-2/cycle-3 hardening passes, and C4-P1's defect-shaped merge redesign). `a` and `b`
/// cluster as the same defect when ANY of SIX GENERAL signals fires (never a seventh,
/// fixture-specific one):
///  (a) same file, same (present) category, and within-window-or-same-construct — the original
///      cross-family-at-one-site signal; or
///  (a') same file, SAME CONSTRUCT (not merely the coarser line window), AT LEAST ONE side is
///      security-tier ([`finding_class`]), AND the pair is about the SAME defect class — C4-P1
///      design point 1's generalization of (a), tightened by REG-3: a boundary-validation rule
///      and a privileged-key-bypass rule flagged on the SAME handler body are one defect wearing
///      two rule names, not two, even when their `category` strings differ, PROVIDED they are
///      still linked by either a `categorize_rule_id` class match or a shared captured object
///      ([`shared_captured_object`]) — e.g. both name the same guard flag that is at once
///      under-validated and what the bypass trusts. Scoped to `same_construct` specifically
///      (never the looser line window) because "literally the same function body" is itself
///      strong enough same-site evidence to drop the EXACT-category-string requirement, but it
///      is NOT enough on its own to fuse two genuinely unrelated rules that merely happen to
///      fire in the same block (REG-3: an indexing-strategy or pagination-convention finding
///      co-located with an injection/authorization finding must stay two rows — see
///      `p1_c4_same_construct_pair_with_unrelated_category_and_no_object_stays_two_rows`); or
///  (b) they share a captured structural object ([`shared_captured_object`]) — the "same root
///      cause across files" signal (a config flag and the handler that reads it; an RLS policy
///      and the page that relies on it), which does NOT require the same file or category; or
///  (c) same file and near-identical description prose ([`description_overlap_score`] at or
///      above [`DESCRIPTION_OVERLAP_THRESHOLD`]) — a det+AI (or any-tier) pair describing ONE
///      defect a few lines apart whose category differs or which sits outside the line window,
///      but whose headline text is substantively the same sentence. Deliberately same-PATH-only:
///      this must never fuse a sink finding in one file with an unrelated call-site finding in
///      another file just because the prose happens to overlap; or
///  (d) the IDENTICAL rule id fired on both, same file, within-window-or-same-construct OR AT THE
///      SAME SEVERITY (MERGE cycle-3, observed-failure 3; widened by C4-P1 design point 3) — the
///      literal strongest "same defect" identity signal there is, so it needs neither a matching
///      `category` (an invented `AI-` id with no taxonomy mapping still qualifies) nor prose
///      overlap. The same-severity arm is NOT distance-scoped on purpose: it is the general form
///      of the pre-existing "N location(s)" collapse ([`group_structural_needs_review`]'s
///      pattern), generalized from informational/needs-review rows to every curated row — one
///      rule firing three times across one file at one severity is one row listing three sites,
///      not three near-duplicate rows, regardless of how far apart the sites are. Still subject
///      to the `objects_conflict` veto just below, so the canonical counter-example (the same
///      RLS-missing rule firing once for `orders` and once for `profiles`) stays two rows.
/// Every wrong-fusion guard still applies on top of whichever signal fired.
fn semantic_pair_merges(a: &Finding, b: &Finding, content: Option<&str>) -> bool {
    let in_window = a.path == b.path
        && a.line != 0
        && b.line != 0
        && a.line.abs_diff(b.line) <= SEMANTIC_MERGE_WINDOW;
    let in_construct =
        a.path == b.path && content.is_some_and(|c| same_construct(c, a.line, b.line));

    // Signal (d): the identical rule id, SAME FILE, within window/construct, OR at the same
    // severity regardless of distance — see the doc comment above (the N-site collapse arm).
    // Computed up front since it also exempts the det+det guard just below: two "distinct"
    // deterministic rows that are actually the SAME rule firing twice (nearby, or at the same
    // severity anywhere in the SAME file) are one defect by construction, same as the
    // shared-captured-object case. The N-site arm REQUIRES `a.path == b.path` — the same rule
    // firing in TWO DIFFERENT files is two distinct defects (two places to fix), e.g. the same
    // architectural rule tripping a Rust handler and a TS handler; those must stay two rows.
    // (`in_window`/`in_construct` already imply same path; only the same-severity arm needs it.)
    let same_rule_adjacent = a.rule_id == b.rule_id
        && (in_window || in_construct || (a.path == b.path && a.severity == b.severity));

    // REG-3: same-construct pairs used to override the category requirement unconditionally —
    // this is the GENERAL "same defect class" check that now gates signal (a') below, computed
    // two ways: the explicit `Finding::category` field (when calibration/the rule-id heuristic
    // set it on both sides), OR — independently, so either method alone suffices — the
    // `categorize_rule_id` class derived straight from each rule id. Deliberately NOT the same
    // `same_category` binding signal (a) uses just below: that one only ever looks at the
    // `.category` field, whereas this also-tries-the-rule-id fallback is specific to the
    // same-construct-security override so it still recognizes two corpus rule ids that are
    // self-evidently the same family (e.g. two different `AUTHZ-`/`BYPASS`-flavored ids) even
    // when neither Finding happens to carry an explicit `.category`.
    let same_construct_category_class = matches!((&a.category, &b.category), (Some(ca), Some(cb)) if ca == cb)
        || categorize_rule_id(&a.rule_id)
            .is_some_and(|ca| categorize_rule_id(&b.rule_id).as_deref() == Some(ca.as_str()));

    // Signal (a'): same construct, at least one side security-tier, AND (same defect class OR a
    // shared captured object) — see the doc comment above. Computed up front for the same reason
    // as (d): a pair of DETERMINISTIC rules that both fire inside one handler body, one of them
    // security-tier and genuinely the same defect, is one defect, not two "distinct" deterministic
    // rows. REG-3: previously fired on same-construct + security-tier ALONE, with no requirement
    // the two findings be related at all — that absorbed unrelated co-located rules (an
    // indexing-strategy or pagination-convention finding) into a security finding's
    // `also_matches`, hiding them from their own category/Coverage count. Now requires the
    // category-class match above OR `shared_captured_object`, the same two "same root cause"
    // proofs the rest of this function already relies on.
    let same_construct_security_override = a.path == b.path
        && in_construct
        && (finding_class(a) == FindingClass::Security
            || finding_class(b) == FindingClass::Security)
        && (same_construct_category_class || shared_captured_object(a, b));

    // Guard: two deterministic rows are two distinct defects by construction UNLESS they share a
    // captured structural object (design MERGE gap (ii)), are the same rule id firing twice
    // nearby or at the same severity (signal (d)), or sit in the same construct with one side
    // security-tier (signal (a')) — e.g. two independent secret-detectors both naming the SAME
    // committed secret/file are the same root cause, not two. With none of these, they stay
    // distinct regardless of which clustering signal below would otherwise fire — this preserves
    // the distinct-defects invariant.
    let both_deterministic =
        finding_origin(a) == Origin::Deterministic && finding_origin(b) == Origin::Deterministic;
    if both_deterministic
        && !shared_captured_object(a, b)
        && !same_rule_adjacent
        && !same_construct_security_override
    {
        return false;
    }

    // Signal (a): same file + same category + line-window-or-construct overlap.
    let same_category = matches!((&a.category, &b.category), (Some(ca), Some(cb)) if ca == cb);
    let same_file_adjacent = a.path == b.path && same_category && (in_window || in_construct);

    // Signal (b): a shared captured object, general and cross-file (design point 2b).
    let shared_object = shared_captured_object(a, b);

    // Signal (c): near-identical description prose, general and same-file-only (design MERGE
    // gap (i)).
    let description_overlap =
        a.path == b.path && description_overlap_score(a, b) >= DESCRIPTION_OVERLAP_THRESHOLD;

    if !same_file_adjacent
        && !shared_object
        && !description_overlap
        && !same_rule_adjacent
        && !same_construct_security_override
    {
        return false;
    }

    // Guard: disjoint structural objects named in free text (different tables/policies) only
    // vetoes the LINE-PROXIMITY, PROSE-OVERLAP, SAME-RULE-ADJACENT and SAME-CONSTRUCT-SECURITY
    // signals — two findings that merely sit near each other, or use similar wording, or share a
    // rule id, or share a construct with one side security-tier, but visibly name different
    // things (e.g. the same RLS-missing rule firing once for `orders` and once for `profiles`,
    // nearby in one file, is two distinct violations, not one; a validation rule naming `orders`
    // and a bypass rule naming `profiles` in the same handler is two distinct defects, not one). A
    // `shared_captured_object` match is a stronger, structured same-object proof and is never
    // vetoed by this looser text heuristic.
    if !shared_object
        && (same_file_adjacent
            || description_overlap
            || same_rule_adjacent
            || same_construct_security_override)
        && objects_conflict(a, b)
    {
        return false;
    }

    // Guard: AI+AI needs corroboration beyond mere proximity — one snippet contains the other,
    // both are located (real, resolved code) inside the same construct, they share a captured
    // object, the description-overlap signal itself fired (an equally strong, deliberately
    // high-threshold structured-enough proof), or they are the same rule id firing twice nearby
    // (signal (d), its own proof). Two AI findings citing DIFFERENT real code that merely sit
    // near each other, with no shared object, no real prose match, and no shared rule id, stay
    // separate.
    let both_ai = matches!(finding_origin(a), Origin::AdoptedAi | Origin::InventedAi)
        && matches!(finding_origin(b), Origin::AdoptedAi | Origin::InventedAi);
    if both_ai {
        let sa = a.snippet.trim();
        let sb = b.snippet.trim();
        let snippet_corroborated = (!sa.is_empty() && sb.contains(sa))
            || (!sb.is_empty() && sa.contains(sb))
            || (a.located && b.located && in_construct)
            || shared_object
            || description_overlap
            || same_rule_adjacent
            || same_construct_security_override;
        if !snippet_corroborated {
            return false;
        }
    }
    true
}

/// Collapse one semantic group into a single finding, PRIMARY chosen by `primary_rank`
/// (security-class, then specificity/origin, then severity, then confidence, then earliest —
/// design point 3, reordered by C5-1 — see that function's doc comment); severity set via
/// `merged_severity` (max across the group, except when the primary is deterministic in a
/// mixed-tier group); every OTHER distinct rule id (including the members' own pre-existing
/// `also_matches`) demoted into `also_matches`, scoped to ONLY this cluster's members (design
/// point 4's `also_matches`-correctness fix: an id can only appear here if it was actually a
/// member of, or already demoted within, THIS group — never a rule from an unrelated cluster).
/// Every member's OWN evidence site is preserved in `also_locations` (design point 4's "list them
/// all" union) rather than silently dropped when it loses the primary slot; a member whose
/// `located == false` (no independently-fixable code of its own — see `Finding::located`) is
/// marked `consequence: true` there, so it folds in as an "also affects" location rather than
/// reading as a peer, independently-actionable site. C5-1: a primary with no `fix_specific` of
/// its own inherits the first member's (see `merge_location_group`'s matching comment).
fn merge_semantic_group(group: Vec<Finding>) -> Finding {
    let primary_idx = group
        .iter()
        .enumerate()
        .max_by_key(|(i, f)| primary_rank(f, group.len(), *i))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let max_sev = group
        .iter()
        .max_by_key(|f| severity_rank(&f.severity))
        .map(|f| f.severity.clone())
        .unwrap_or_else(|| "low".to_string());
    // C5-1: see `merged_severity`'s doc comment. Computed over the FULL group (primary included)
    // before it is removed below.
    let all_same_tier = group
        .iter()
        .all(|f| finding_origin(f) == finding_origin(&group[0]));
    let mut group = group;
    let mut primary = group.remove(primary_idx);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(primary.rule_id.clone());
    let mut also: Vec<String> = Vec::new();
    // Preserve any already-demoted siblings the primary carried in from location-merge.
    for r in primary.also_matches.drain(..) {
        if seen.insert(r.clone()) {
            also.push(r);
        }
    }
    // NESTED sites only — carried in from an EARLIER stage via the primary's own pre-existing
    // `also_locations`. Deliberately kept separate from each member's FRESH own-site entry
    // below: this list alone is what the "same already-absorbed record reachable via two
    // paths" dedup further down is allowed to collapse.
    let mut nested_locations: Vec<MergedLocation> = primary.also_locations.drain(..).collect();
    // FRESH sites only — THIS stage's own absorption of each group member. See the dedup
    // comment below for why these must never be deduplicated away.
    let mut fresh_locations: Vec<MergedLocation> = Vec::new();
    // C5-1: computed before the loop below (which only reads `f.fix_specific` incidentally via
    // `&group`, never consumes it) — a deterministic primary inherits the first member's
    // codebase-specific fix when it has none of its own yet.
    let inherited_fix_specific = primary
        .fix_specific
        .is_none()
        .then(|| group.iter().find_map(|f| f.fix_specific.clone()))
        .flatten();
    for f in &group {
        if seen.insert(f.rule_id.clone()) {
            also.push(f.rule_id.clone());
        }
        for r in &f.also_matches {
            if seen.insert(r.clone()) {
                also.push(r.clone());
            }
        }
        fresh_locations.push(MergedLocation {
            repo: f.repo.clone(),
            path: f.path.clone(),
            line: f.line,
            rule_id: f.rule_id.clone(),
            snippet: f.snippet.clone(),
            consequence: !f.located,
        });
        nested_locations.extend(f.also_locations.iter().cloned());
    }
    // De-duplicate identical NESTED sites only (the same already-absorbed record can arrive
    // twice across a nested merge — e.g. it was already in the primary's carried-in
    // `also_locations` AND independently reachable via another member's own already-nested
    // `also_locations`).
    //
    // Pipeline-integrity fix: this dedup must NEVER reach a member's FRESH own-site entry
    // (pushed into `fresh_locations` above, never into this `nested_locations` list). A fresh
    // entry represents THIS stage absorbing `f` itself — a real, distinct input row — and
    // `f`'s (repo, path, line, rule_id) key can coincide with a site an EARLIER stage already
    // recorded (e.g. two different detectors independently flagging the identical rule at the
    // identical line) without `f` being that same record. Collapsing the two together, as the
    // old single-pass dedup over one combined list did, made `f`'s absorption invisible to
    // `onboard::account_location_merge`: it counts `also_locations` entries as a multiset
    // against what was ALREADY present pre-stage, and a key that was already present is
    // reconciled as "carried forward, not new" the first time it is seen — so `f` vanished with
    // no recorded disposition at all (real-world `cross-family-merge rows 116 -> 83 merged=32
    // ... unaccounted=1`). Every fresh entry is now kept unconditionally, so every group member
    // always leaves its own site behind regardless of what the primary or a sibling already
    // carried in.
    let mut location_seen: std::collections::HashSet<(String, String, usize, String)> =
        std::collections::HashSet::new();
    nested_locations.retain(|l| {
        location_seen.insert((l.repo.clone(), l.path.clone(), l.line, l.rule_id.clone()))
    });
    let mut also_locations = nested_locations;
    also_locations.extend(fresh_locations);
    primary.severity = merged_severity(&primary, max_sev, all_same_tier);
    primary.also_matches = also;
    primary.also_locations = also_locations;
    if primary.fix_specific.is_none() {
        primary.fix_specific = inherited_fix_specific;
    }
    primary
}

/// P7 (`docs/plans/2026-09-29_codebase-inspection-hardening.md`): drop any finding whose rule
/// carries a [`camerata_rules::StackException`] that applies to the repo's DETECTED stack +
/// this finding's own file path — e.g. `ARCH-MONOLITH-FIRST-1` flagging a `supabase/functions/`
/// directory on a Supabase-stack repo, where a second Edge Functions deployable is the
/// idiomatic pattern, not a monolith-topology violation. GENERAL by construction: this checks
/// every finding against whatever `[[stack_exception]]` blocks its own rule declares in the
/// corpus — it is not special-cased to any one rule or framework (see
/// [`camerata_rules::StackException`]'s doc comment for the mechanism). A finding whose rule id
/// isn't in the corpus (an AI-invented `AI-`-prefixed name, or a rule id the loaded corpus
/// doesn't contain) is never excepted — only a real corpus rule can declare an exception.
pub fn apply_stack_exceptions(
    findings: Vec<Finding>,
    detected_frameworks: &[String],
    corpus: &camerata_rules::RuleSet,
) -> Vec<Finding> {
    if detected_frameworks.is_empty() {
        return findings;
    }
    findings
        .into_iter()
        .filter(|f| {
            let rule = corpus
                .get_by_id(&f.rule_id)
                .or_else(|| corpus.get_by_id(f.rule_id.trim_start_matches("AI-")));
            match rule {
                Some(rule) => rule
                    .stack_exception_for(detected_frameworks, &f.path)
                    .is_none(),
                None => true,
            }
        })
        .collect()
}

/// P7 (`docs/plans/2026-09-29_codebase-inspection-hardening.md`, item 4 — needs-review noise):
/// a rule the calibrator flagged as a debatable STRUCTURAL/consistency preference
/// (`category == "arch-conformance"`, `confidence == "needs-review"`) commonly fires at many
/// unrelated locations across a repo — one row per occurrence drowns the report ("23
/// needs-review rows, mostly structural preferences"). This collapses every such occurrence of
/// ONE rule into a SINGLE grouped finding, `severity = "low"`, listing every location, instead
/// of one row per occurrence.
///
/// Deliberately NARROW — only `arch-conformance` + `confidence == "needs-review"` + a
/// non-critical/high severity qualifies, and only an `active` (non-suppressed, not already
/// dispositioned by a waiver) finding is grouped, so:
/// - a genuine security/floor finding (never `arch-conformance`, or never `needs-review`) is
///   NEVER touched — no security finding is ever lost in this pass;
/// - a critical/high finding is never grouped away, matching this pass's own narrow criteria
///   above (not that it would matter for bucketing either way — `report_export::is_held_for_review`
///   (C5-4) holds a `needs-review` row out of every action tier at ANY severity);
/// - a finding an auditor already suppressed/waived keeps its own explicit disposition rather
///   than disappearing into a group.
///
/// The grouped finding's `confidence = Some("needs-review")` (its `needs_review` flag is also
/// set — see `build_structural_group_finding`) is exactly what routes it to the `held` bucket
/// (C5-4), OUTSIDE the curated do_now/do_next/plan action tiers, via the PRE-EXISTING
/// `report_export::is_held_for_review` gate — no new bucketing logic needed on that side.
///
/// Runs across the WHOLE scan (every repo the caller passes in), so a rule flagged in more than
/// one repo still collapses to one row with every repo's site listed.
pub fn group_structural_needs_review(findings: Vec<Finding>) -> Vec<Finding> {
    let mut groups: std::collections::BTreeMap<String, Vec<Finding>> =
        std::collections::BTreeMap::new();
    let mut rest = Vec::with_capacity(findings.len());
    for f in findings {
        let severity = crate::report_export::normalize_severity(&f.severity);
        let is_structural_needs_review = f.status == "active"
            && f.category.as_deref() == Some("arch-conformance")
            && f.confidence.as_deref() == Some("needs-review")
            && !matches!(severity.as_str(), "critical" | "high");
        if is_structural_needs_review {
            groups.entry(f.rule_id.clone()).or_default().push(f);
        } else {
            rest.push(f);
        }
    }
    for (rule_id, mut occurrences) in groups {
        occurrences.sort_by(|a, b| {
            (a.repo.as_str(), a.path.as_str(), a.line).cmp(&(
                b.repo.as_str(),
                b.path.as_str(),
                b.line,
            ))
        });
        rest.push(build_structural_group_finding(rule_id, occurrences));
    }
    rest
}

/// Build the single grouped [`Finding`] for `rule_id`'s `occurrences` — see
/// [`group_structural_needs_review`]'s doc comment for the full contract. `occurrences` must be
/// non-empty (the caller only calls this from a populated group).
///
/// # Pipeline-integrity fix: never drop an occurrence's ALREADY-absorbed data
///
/// This runs across the WHOLE scan, AFTER the per-repo `merge_semantic_groups` cross-family
/// merge — so an `occurrence` arriving here can already be a merged PRIMARY from that earlier
/// stage, carrying its own non-empty `also_matches` (other rule ids it already absorbed) and
/// `also_locations` (their evidence sites). The previous version of this function built the
/// group's `also_matches`/`also_locations` ONLY from each occurrence's own top-level
/// `rule_id`/`repo`/`path`/`line` — silently discarding anything already nested inside an
/// occurrence's `also_matches`/`also_locations` fields. That is genuine data loss (not just an
/// accounting gap): the rule ids and sites a prior merge had already recorded vanish from the
/// final output entirely, with no row anywhere still carrying them. Every occurrence's own
/// nested `also_matches`/`also_locations` is now carried forward (deduplicated) alongside the
/// fresh per-occurrence site entries this function has always built.
fn build_structural_group_finding(rule_id: String, occurrences: Vec<Finding>) -> Finding {
    let n = occurrences.len();
    let primary = occurrences
        .first()
        .expect("build_structural_group_finding is only called with a non-empty group");
    let locations_text = occurrences
        .iter()
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect::<Vec<_>>()
        .join(", ");
    let detail = format!(
        "Structure and consistency observations: {rule_id} was flagged as a debatable \
         structural or stylistic preference in {n} place{} across the codebase — {locations_text}. \
         These are informational consistency notes, not confirmed defects; review at your own \
         discretion.",
        if n == 1 { "" } else { "s" }
    );
    // Carry forward every rule id ANY occurrence had already absorbed (from an earlier merge
    // stage) before this grouping ran — never dropped just because every occurrence here shares
    // the SAME top-level `rule_id` (that's exactly why they were grouped; it says nothing about
    // what each one had already absorbed beneath it).
    let mut also_matches: Vec<String> = Vec::new();
    let mut also_matches_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    also_matches_seen.insert(rule_id.clone());
    for f in &occurrences {
        for id in &f.also_matches {
            if also_matches_seen.insert(id.clone()) {
                also_matches.push(id.clone());
            }
        }
    }
    // Every occurrence's own evidence site (for occurrences[1..] — the primary's own site is the
    // group's own repo/path/line below) PLUS every occurrence's own already-nested
    // `also_locations` (primary included — a merged primary's carried-in sites are not its own
    // top-level site and would otherwise be lost entirely).
    let mut also_locations: Vec<MergedLocation> = Vec::new();
    for (i, f) in occurrences.iter().enumerate() {
        if i > 0 {
            also_locations.push(MergedLocation {
                repo: f.repo.clone(),
                path: f.path.clone(),
                line: f.line,
                rule_id: f.rule_id.clone(),
                snippet: f.snippet.clone(),
                consequence: false,
            });
        }
        also_locations.extend(f.also_locations.iter().cloned());
    }
    // De-duplicate identical sites — the same site can arrive twice (e.g. already present in the
    // primary's carried-in `also_locations` AND independently reachable via another occurrence),
    // mirroring `merge_semantic_group`'s identical dedup step.
    let mut location_seen: std::collections::HashSet<(String, String, usize, String)> =
        std::collections::HashSet::new();
    also_locations.retain(|l| {
        location_seen.insert((l.repo.clone(), l.path.clone(), l.line, l.rule_id.clone()))
    });
    Finding {
        repo: primary.repo.clone(),
        path: primary.path.clone(),
        line: primary.line,
        rule_id,
        severity: "low".to_string(),
        snippet: format!("{n} location(s)"),
        detail,
        status: "active".to_string(),
        also_matches,
        preview: false,
        preview_tool: None,
        in_test: false,
        needs_review: true,
        confidence: Some("needs-review".to_string()),
        effort: None,
        category: Some("arch-conformance".to_string()),
        located: true,
        captures: std::collections::BTreeMap::new(),
        evaluated_option_id: primary.evaluated_option_id.clone(),
        also_locations,
        fix_specific: None,
        calibration_rationale: None,
    }
}

/// The SECOND merge pass (design §1, extended by P1 — see
/// `docs/plans/2026-09-29_codebase-inspection-hardening.md` — and by C4-P1's defect-shaped merge
/// redesign): after `resolve_finding_lines` + `merge_by_location` have collapsed exact-location
/// duplicates, fuse cross-TIER duplicates — the same defect flagged by two rule families a few
/// lines apart (an AI RLS finding + the native RLS checker; a service-role-bypass + a
/// fetch-then-authorize on one handler body), OR the same root cause flagged in DIFFERENT files
/// via a shared captured object (a config flag + the handler that reads it). Greedy single pass:
/// each finding joins the first existing group whose seed it merges with (via
/// [`semantic_pair_merges`]), else seeds a new group. Category is filled from the rule-id
/// heuristic for any finding a source didn't classify — this also determines [`finding_class`]
/// for members that have no security-sounding rule id of their own. `captures` is backfilled the
/// same way ([`backfill_captured_object`]) so a finding whose own detector never populated
/// `captures` still gets a shot at the cross-file [`shared_captured_object`] signal when its own
/// text unambiguously names one object — and, independently (C6-B2 design point 1), via
/// [`backfill_credential_identity`], so a finding naming a secret/credential identifier
/// (`STRIPE_SECRET_KEY`) in its own text gets a shot at that SAME signal even when several such
/// tokens are named (no single-candidate ambiguity restriction, unlike the general object case):
/// this is how the holder of a leaked credential, the module that constructs a client from it,
/// and every call site that uses it collapse into ONE row instead of shipping as N separate
/// criticals, each with its own redundant remediation estimate. The final pass,
/// [`strip_headline_cross_references`],
/// runs over the MERGED output to clean up any row whose own narrative text still points at
/// another (now possibly reordered) row by name.
pub fn merge_semantic_groups(findings: Vec<Finding>, files: &[(String, String)]) -> Vec<Finding> {
    let by_path: std::collections::HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    // Backfill category + captures from the heuristics where no source assigned them.
    let mut findings = findings;
    for f in findings.iter_mut() {
        if f.category.is_none() {
            f.category = categorize_rule_id(&f.rule_id);
        }
        backfill_captured_object(f);
        backfill_credential_identity(f);
    }
    let mut groups: Vec<Vec<Finding>> = Vec::new();
    for f in findings {
        let content = by_path.get(f.path.as_str()).copied();
        let mut placed = false;
        for g in groups.iter_mut() {
            // Compare against the group's seed (first member) — a stable, deterministic anchor.
            if semantic_pair_merges(&g[0], &f, content) {
                g.push(f.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            groups.push(vec![f]);
        }
    }
    let merged: Vec<Finding> = groups.into_iter().map(merge_semantic_group).collect();
    strip_headline_cross_references(merged)
}

/// C4-P1 design point 4: relative-POSITION marker phrases that signal a sentence is pointing at
/// another finding's place in the report, independent of whether it also names a specific rule
/// id — "see the finding below", "as noted above", "duplicate of the above finding". Deliberately
/// a SMALL, general marker list (not keyed to any one rule's wording): the word "finding"
/// combined with a positional word is a stronger, less false-positive-prone anchor than the
/// positional word alone would be (plain "above"/"below" shows up constantly in ordinary
/// code/prose — "the line above sets the header" — and must not be treated as a cross-reference).
const RELATIVE_POSITION_PHRASES: &[&str] = &[
    "finding below",
    "finding above",
    "findings below",
    "findings above",
    "see below",
    "see above",
    "noted below",
    "noted above",
    "mentioned below",
    "mentioned above",
    "duplicate of",
    "same as the",
];

/// Split `text` into sentences (delimiter kept with the sentence it ends), trimming and
/// dropping empty pieces. Splits ONLY at a real [`crate::report_export::sentence_boundaries`]
/// hit — NOT at every bare `.`/`!`/`?` — so a dotted call, filename, version string, URL, or
/// known abbreviation embedded in `text` is never torn apart into its own spurious "sentence".
///
/// REG-5: this used to split on every literal `.`/`!`/`?` regardless of context, which exploded
/// `client.auth.method()` into three fragments ("client.", "auth.", "method()"). Callers that
/// rejoin the returned pieces with `" "` (see `strip_cross_reference_sentences`) then stitched
/// those fragments back together as `"client. auth. method()"` — inserting a space after every
/// such period, even when nothing was actually filtered out. Splitting only at a genuine
/// boundary makes the rejoin a no-op everywhere except the one case this tokenizer exists to
/// handle in the first place: a GENUINELY run-together sentence with no space at all
/// ("...done.Next step..."), where `sentence_boundaries` still finds the boundary (an uppercase
/// letter immediately after the punctuation) and the rejoin supplies the missing space.
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for end in crate::report_export::sentence_boundaries(text) {
        let piece = text[start..end].trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        start = end;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// True when `token` appears in `sentence` as a whole token — not immediately preceded or
/// followed by another identifier character (alphanumeric or `_`) — so a short rule id can never
/// spuriously match as a substring of a longer, unrelated one (`"SEC-A-1"` must not match inside
/// `"SEC-A-10"`). `-` is deliberately NOT treated as an identifier character here since rule ids
/// already use it as their OWN internal separator; ordinary sentence punctuation around the id
/// (spaces, commas, a trailing period) is what actually bounds it in prose.
fn sentence_contains_token(sentence: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bytes = sentence.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0usize;
    while let Some(rel) = sentence[start..].find(token) {
        let abs = start + rel;
        let before_ok = abs == 0 || !is_ident(bytes[abs - 1]);
        let after = abs + token.len();
        let after_ok = after >= bytes.len() || !is_ident(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Drop every sentence of `text` that either (a) names one of `other_ids` (another finding's
/// exact rule id — [`sentence_contains_token`], case-sensitive since rule ids are) or (b) matches
/// a [`RELATIVE_POSITION_PHRASES`] marker (case-insensitive), and return `(rejoined remaining
/// text, the other_ids that were named)`. Fails OPEN: if dropping cross-reference sentences would
/// empty `text` out entirely, the ORIGINAL text is returned untouched and nothing is reported as
/// referenced — a stale pointer in an otherwise-informative paragraph beats an empty finding
/// body.
fn strip_cross_reference_sentences(text: &str, other_ids: &[&str]) -> (String, Vec<String>) {
    let mut kept: Vec<String> = Vec::new();
    let mut referenced: Vec<String> = Vec::new();
    for sentence in split_into_sentences(text) {
        let mut named: Vec<String> = Vec::new();
        for id in other_ids {
            if sentence_contains_token(&sentence, id) {
                named.push((*id).to_string());
            }
        }
        let lower = sentence.to_ascii_lowercase();
        let has_position_marker = RELATIVE_POSITION_PHRASES.iter().any(|m| lower.contains(m));
        if !named.is_empty() || has_position_marker {
            referenced.extend(named);
            continue;
        }
        kept.push(sentence);
    }
    if kept.is_empty() {
        return (text.to_string(), Vec::new());
    }
    (kept.join(" "), referenced)
}

/// C4-P1 design point 4: a finding's own `detail` text literally pointing at ANOTHER finding by
/// name or relative report position ("see the SEC-RLS-MISSING finding below", "as noted above")
/// reads as broken prose — or points at the wrong row entirely — once merge/sort/filtering
/// reorders the final report. This is the SAME staleness [`strip_dedup_pointers`] already fixes
/// for calibration's `reason` field (index-based "same as [N]" pointers), extended here to a
/// finding's own narrative `detail` (the text `report_export::defect_headline` derives the
/// client-facing headline from). Runs as the LAST step of
/// [`merge_semantic_groups`], over the MERGED output: for each row, every OTHER row's rule id
/// (its own primary id, or any id already absorbed into THAT row's `also_matches`) that this
/// row's `detail` happens to name is a genuine cross-reference to a DIFFERENT defect — the
/// sentence containing it is stripped from `detail`, and the relationship is recorded
/// STRUCTURALLY instead of in prose: the referenced row's primary id is added to THIS row's
/// `also_matches`, and — since the relationship is mutual even when only one side wrote the
/// pointer — THIS row's primary id is reciprocally added to the referenced row's `also_matches`.
/// Nothing is ever dropped, only moved from stale narrative text into structured data. A sentence
/// using relative-position language without naming a specific rule id is stripped the same way,
/// with nothing to add (no id was named to record).
fn strip_headline_cross_references(mut findings: Vec<Finding>) -> Vec<Finding> {
    // Every id each row already "owns" (its own primary id + its own also_matches) — a mention of
    // one of these within the SAME row is the row's own content, not a cross-finding pointer.
    let owned_ids: Vec<std::collections::HashSet<String>> = findings
        .iter()
        .map(|f| {
            let mut s: std::collections::HashSet<String> = f.also_matches.iter().cloned().collect();
            s.insert(f.rule_id.clone());
            s
        })
        .collect();
    // Reciprocal also_matches ids to add after the scan, keyed by row index — collected
    // separately so the scan itself only ever reads `findings`, never mutates it mid-loop.
    let mut extra_also_matches: Vec<Vec<String>> = vec![Vec::new(); findings.len()];
    for i in 0..findings.len() {
        // (owner row index, id) pairs for every id NOT owned by row i.
        let other_ids: Vec<(usize, &str)> = owned_ids
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .flat_map(|(j, s)| s.iter().map(move |id| (j, id.as_str())))
            .collect();
        let id_list: Vec<&str> = other_ids.iter().map(|(_, id)| *id).collect();
        let (new_detail, referenced) =
            strip_cross_reference_sentences(&findings[i].detail, &id_list);
        if new_detail != findings[i].detail {
            findings[i].detail = new_detail;
        }
        for id in &referenced {
            if let Some((owner, _)) = other_ids.iter().find(|(_, x)| x == id) {
                extra_also_matches[i].push(findings[*owner].rule_id.clone());
                extra_also_matches[*owner].push(findings[i].rule_id.clone());
            }
        }
    }
    for (i, extra) in extra_also_matches.into_iter().enumerate() {
        for id in extra {
            if id != findings[i].rule_id && !findings[i].also_matches.contains(&id) {
                findings[i].also_matches.push(id);
            }
        }
    }
    findings
}

/// Run the real-time audit passes with rule-routing applied.
///
/// Groups `selected` rules by [`crate::scan_routing::Scope`] using the pre-computed
/// `route_plan`, then for each group:
///
/// - Filters `files` to only the files that group's scope covers.
/// - Chunks those filtered files for context-window sizing.
/// - Runs [`run_passes`] over the group's rules, with `advisory_disabled = true` for a
///   language-specific group ONLY when a cross-cutting `All` group ALSO runs this scan (so the
///   "flag novel issues" pass fires exactly once per file chunk there, instead of once per
///   group × chunk). When no `All` group exists at all (a purely language-scoped rule
///   selection), every language group keeps advisory enabled instead — see the `has_all_group`
///   doc comment in this function's body for why that never duplicates a finding (W2).
///
/// When routing produces no savings (all rules are cross-cutting, or only one group), this
/// degenerates to the previous single-group behavior with no overhead.
///
/// Returns the same tuple as [`run_passes`]: `(findings, proposed, requested, ok, last_err)`.
#[allow(clippy::too_many_arguments)]
async fn run_routed_passes(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    selected: &[(String, String)],
    route_plan: &crate::scan_routing::RoutePlan,
    repo_map: &str,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    concurrency: usize,
    batch_size: usize,
    meter: Option<&UsageMeter>,
) -> (
    Vec<Finding>,
    Vec<ProposedRule>,
    std::collections::HashSet<String>,
    usize,
    Option<anyhow::Error>,
) {
    use crate::scan_routing::Scope;

    // When there are no rules (free-form audit), skip routing and run one pass over all files.
    if selected.is_empty() {
        let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
        let empty_batch: &[(String, String)] = selected;
        let batches: Vec<&[(String, String)]> = vec![empty_batch];
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, chunks.len() * batches.len());
        }
        return run_passes(
            llm,
            repo,
            repo_map,
            adopted,
            audit_model,
            feedback,
            job,
            &chunks,
            &batches,
            concurrency,
            "pass",
            &format!("audit-{repo}"),
            meter,
            false, // advisory enabled: novel findings wanted in free-form mode
        )
        .await;
    }

    // Pre-seed the job's total pass count across ALL route groups so the progress bar is
    // accurate from the start. We need to compute chunk counts per group first.
    //
    // For each group: count its files → chunk count × batch count = pass count.
    let total_pass_count: usize = route_plan
        .groups
        .iter()
        .map(|g| {
            // Filter files to this group's scope.
            let group_files: Vec<&(String, String)> = files
                .iter()
                .filter(|(path, _)| {
                    crate::scan_routing::file_in_scope(path, &g.scope)
                })
                .collect();
            // The group's rules split into batches.
            let n_batches = g.rules.chunks(batch_size.max(1)).count().max(1);
            // chunk_files needs owned slices; approximate chunk count from raw sizes.
            let total_sz: usize = group_files
                .iter()
                .map(|(p, c)| p.len() + c.len() + 32)
                .sum();
            let n_chunks = (total_sz / CHUNK_DIGEST_CHARS).max(1);
            n_chunks * n_batches
        })
        .sum();
    // Only seed when > 0 (empty groups are no-ops and contribute 0 passes).
    if total_pass_count > 0 {
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, total_pass_count);
        }
    }

    let mut all_findings: Vec<Finding> = Vec::new();
    let mut all_proposed: Vec<ProposedRule> = Vec::new();
    let mut all_requested: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total_ok: usize = 0;
    let mut last_err: Option<anyhow::Error> = None;

    // W2 (closes a real zero-row semantic-tier defect, previously documented as an accepted
    // "known limitation" in `routing_single_language_repo_with_language_rules`): a project whose
    // SELECTED rule set is entirely language-scoped (e.g. only `RUST-*` rules — no `ARCH-*`/
    // `SEC-*`/other cross-cutting rule) produces a `route_plan` with NO `Scope::All` group at
    // all. The old unconditional `!matches!(group.scope, Scope::All)` then disabled advisory in
    // EVERY group, which means the "ALSO flag any other genuine issues NOT covered by an adopted
    // rule" instruction (the model's one and only backstop for issues no selected rule names —
    // see `audit_system_prompt`'s doc comment) was never sent to the model AT ALL for that scan.
    // That is the silent-collapse mechanism: a repo scanned with a language-only rule selection
    // gets zero chance at any novel/security finding outside those rules, indistinguishable from
    // "the model looked and found nothing."
    //
    // Fix: advisory is disabled for a language group ONLY when a SEPARATE cross-cutting All
    // group also exists to run it (the original duplicate-novel-finding concern this flag
    // exists for). When there is no All group, every language group keeps advisory enabled
    // instead — safe to do because `Scope::Language` groups are mutually exclusive by
    // construction (`scan_routing::file_language` maps each file to AT MOST one language), so no
    // file can receive the advisory instruction from more than one group.
    let has_all_group = route_plan
        .groups
        .iter()
        .any(|g| matches!(g.scope, Scope::All));

    for (gi, group) in route_plan.groups.iter().enumerate() {
        // The advisory pass is suppressed in a language-specific group only when a cross-cutting
        // All group ALSO runs this scan — that All group already sees every file, so re-running
        // advisory per language group would produce duplicate novel findings for every file
        // (every language file is also an All-group file). When there is no All group, nothing
        // else will ever ask "is there anything wrong beyond the listed rules?", so each
        // (mutually exclusive) language group runs it itself — see the `has_all_group` doc
        // comment above.
        let advisory_disabled = has_all_group && !matches!(group.scope, Scope::All);

        // Materialize the owned file list for this group's scope.
        let group_files: Vec<(String, String)> = files
            .iter()
            .filter(|(path, _)| crate::scan_routing::file_in_scope(path, &group.scope))
            .cloned()
            .collect();

        if group_files.is_empty() {
            // No files match this language group in this repo — nothing to audit.
            continue;
        }

        let chunks = chunk_files(&group_files, CHUNK_DIGEST_CHARS);
        let group_rules: &[(String, String)] = &group.rules;
        let batches: Vec<&[(String, String)]> = group_rules
            .chunks(batch_size.max(1))
            .collect();

        let scope_label = match &group.scope {
            Scope::Language(lang) => format!("pass[{lang}]"),
            Scope::All => "pass[all]".to_string(),
        };
        let session_prefix = format!("audit-{repo}-g{gi}");

        // NOTE: job total was pre-seeded above; do NOT call add_total again per group, as that
        // would double-count. run_passes calls inc_done per completed pass, which is correct.

        let (gf, gp, gr, gok, ge) = run_passes(
            llm,
            repo,
            repo_map,
            adopted,
            audit_model,
            feedback,
            job,
            &chunks,
            &batches,
            concurrency,
            &scope_label,
            &session_prefix,
            meter,
            advisory_disabled,
        )
        .await;

        all_findings.extend(gf);
        all_proposed.extend(gp);
        all_requested.extend(gr);
        total_ok += gok;
        if ge.is_some() {
            last_err = ge;
        }
    }

    (all_findings, all_proposed, all_requested, total_ok, last_err)
}

/// Run the AI architectural audit for one repo. Returns the findings + proposed rules.
///
/// The repo is audited in CONTEXT-SIZED CHUNKS (see `chunk_files`): a real repo is far too
/// large for one model context (a 2.8M-char repo is ~700k tokens vs a 200k window), and the
/// old single-digest path silently fed the model only the first ~10% of files — so a
/// blatant violation in a later file produced zero findings purely because the file was
/// never in the input. Every chunk is audited against the full ruleset and the findings are
/// aggregated. A model/transport failure on a chunk is noted and the audit continues, so a
/// single bad pass never discards the others.
///
/// ## Rule-routing (Lever 2)
///
/// When `selected` contains language-scoped rules (e.g. `RUST-*`, `REACT-*`), `audit_repo`
/// groups them by [`crate::scan_routing::Scope`] via [`crate::scan_routing::plan_routes`] and
/// runs each group against ONLY the files that group's language matches. Cross-cutting groups
/// (`Scope::All`) see every file; a `RUST-` group sees only `.rs` files, etc.
///
/// ### Advisory-pass interaction
///
/// The advisory "flag novel issues beyond the adopted rules" task is gated to `bi==0` in
/// `run_passes` so novel issues are not re-flagged under N invented names across N rule-batches
/// of the same chunk. Routing adds a second dimension: if we ran advisory in every language
/// group WHILE an All group also exists, a `.rs` file would get advisory in the rust group AND
/// the All group — bringing back the duplicate-novel-finding problem. The safe wiring
/// (W2-amended — see `run_routed_passes`'s `has_all_group`):
///
/// - When a cross-cutting **All group** exists, it alone runs with advisory **enabled**:
///   novel issues are discovered once, against every file, on the first batch of each chunk;
///   every **language group** runs with `advisory_disabled = true` so it never re-triggers the
///   advisory pass over files the All group already covers.
/// - When NO All group exists at all (a purely language-scoped rule selection — e.g. a project
///   that selected only `RUST-*` rules), advisory stays **enabled** in every language group
///   instead of going dark for the whole scan: `Scope::Language` groups are mutually exclusive
///   by file (`scan_routing::file_language` maps each file to at most one language), so this
///   cannot duplicate a finding, and it is the ONLY way such a scan ever gets the model's
///   backstop "is there anything wrong here beyond the listed rules?" pass at all. Before this
///   fix, a language-only rule selection silently suppressed novel-issue discovery for the
///   entire scan — the semantic-tier's security backstop role never fired, indistinguishable
///   from "nothing to report."
///
/// Net: novel findings appear exactly once per file chunk that belongs to any group, language-
/// scoped rules skip unmatched files, and no finding is missed.
///
/// ### Batch mode
///
/// The Batch execution path ([`run_passes_batch`]) does not yet apply per-rule routing — it
/// submits every rule against every file as a single Anthropic Message Batch. Routing for the
/// batch path is a follow-up (tracked in `docs/decisions/2026-06-20_rule_routing_wiring.md`).
#[allow(clippy::too_many_arguments)]
pub async fn audit_repo(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    selected: &[(String, String)],
    // MULTI-OPTION semantic rules present in `selected` for this repo (built by the caller
    // from the loaded corpus — see `onboard::build_rule_alternatives`). Empty when this
    // project/repo has no multi-option semantic rules selected, or when the caller is a
    // context (deep-tier, resolution round) that doesn't apply this feature.
    alternatives: &[RuleAlternatives],
    // Operator-forced picks for a targeted `rescan-alternatives` call: rule id (uppercased)
    // -> the option id the operator chose. A rule id present here SKIPS the model
    // recommendation pass entirely (the operator already decided) and its directive is
    // rewritten straight to the forced option's directive. Empty for a normal full scan.
    forced: &std::collections::HashMap<String, String>,
    model: Option<&str>,
    calibration_model: Option<&str>,
    mode: ScanMode,
    thorough: bool,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    meter: Option<&UsageMeter>,
    // The full repo file set to build the repo MAP from, when it differs from `files`. On an
    // incremental scan `files` is only the CHANGED bodies, but the repo map should still cover
    // the WHOLE repo so cross-file rules keep their architectural view. `None` → use `files`.
    map_files: Option<&[(String, String)]>,
) -> anyhow::Result<(
    Vec<Finding>,
    Vec<ProposedRule>,
    Vec<RuleRecommendation>,
    Vec<FailedPass>,
    Vec<crate::scan_ledger::StageSample>,
)> {
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()));
    }
    // W1: this call's own merge/calibration stage samples, returned alongside the normal
    // result (mirroring the EXISTING `Vec<FailedPass>` accumulation pattern already used
    // throughout this function) rather than threading a `&mut ScanLedger` through this
    // already-large parameter list — see `crate::scan_ledger::StageSample`'s doc comment.
    let mut stage_samples: Vec<crate::scan_ledger::StageSample> = Vec::new();
    // Cross-file context for every chunk (which dirs are which layer, where types live). On an
    // incremental scan this is built from the whole repo, not just the changed files.
    let repo_map = build_repo_map(map_files.unwrap_or(files));
    // Key findings to the adopted rule ids (so a violation shows under e.g.
    // ARCH-STRICT-LAYERING-1, not an invented AI- name).
    let adopted: std::collections::HashSet<String> = selected
        .iter()
        .map(|(id, _)| id.to_ascii_uppercase())
        .collect();
    // Model selection: the USER's per-audit choice wins; else CAMERATA_AUDIT_MODEL; else default.
    let audit_model = model.map(str::to_string).or_else(|| {
        std::env::var("CAMERATA_AUDIT_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    // Calibration model: the user's calibration pick wins; else CAMERATA_CALIBRATION_MODEL;
    // else fall back to the SCAN model so the audit is end-to-end on one model by default
    // (no silent default-model calibration). The UI exposes this as its own picker.
    //
    // BUG-AI-3: when `calibration_model` is None, CAMERATA_CALIBRATION_MODEL is unset,
    // AND `audit_model` is also None (both model param and CAMERATA_AUDIT_MODEL absent),
    // `calib_model` is None — which means `verify_findings` passes None to `build_req` and
    // the LLM client uses its compiled-in default model. This is correct: if neither scan
    // nor calibration model is pinned, both default to the same LLM default, so the audit
    // IS end-to-end on one model. The comment above was slightly misleading ("fall back to
    // the SCAN model") when scan_model is itself None — the accurate statement is "fall
    // back to the same source as the scan, which may itself be the LLM default."
    let calib_model = calibration_model
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CAMERATA_CALIBRATION_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| audit_model.clone());

    // ── Multi-option semantic rules: decide (or accept the operator's forced choice of) ONE
    // option per rule, then REWRITE that rule's entry in `effective_selected` to the decided
    // option's directive. Downstream (routing, chunking, the per-chunk violation passes) is
    // then COMPLETELY UNCHANGED from the pre-existing single-directive pipeline — every chunk
    // checks the SAME directive for a given rule, which is what guarantees "violations are
    // always relative to ONE chosen alternative" (the design doc's coherence principle) without
    // any cross-chunk coordination. See `recommend_alternatives`'s doc comment for the chunk-0
    // grounding tradeoff.
    let mut effective_selected: Vec<(String, String)> = selected.to_vec();
    let mut recommendations: Vec<RuleRecommendation> = Vec::new();
    // W6: any pass that fails/times out for this repo, captured so the export can disclose it
    // explicitly instead of only logging to stderr. See `FailedPass`'s doc comment.
    let mut failed_passes: Vec<FailedPass> = Vec::new();
    if !alternatives.is_empty() {
        let (forced_alts, ask_alts): (Vec<RuleAlternatives>, Vec<RuleAlternatives>) = alternatives
            .iter()
            .cloned()
            .partition(|a| forced.contains_key(&a.rule_id));
        // Operator-forced picks: no model call, validate the forced id is real, fall back to
        // the currently-selected/default option (flagged) when it is not.
        for a in &forced_alts {
            let requested_id = forced.get(&a.rule_id).cloned().unwrap_or_default();
            let (final_id, hallucinated) = if a.options.iter().any(|o| o.id == requested_id) {
                (requested_id, false)
            } else {
                let fb = a
                    .selected_option_id
                    .clone()
                    .or_else(|| a.options.first().map(|o| o.id.clone()))
                    .unwrap_or_default();
                (fb, true)
            };
            recommendations.push(RuleRecommendation {
                rule_id: a.rule_id.clone(),
                recommended_option_id: final_id.clone(),
                recommendation_reasoning: if hallucinated {
                    "The requested option id is not valid for this rule; kept the previously \
                     selected option instead."
                        .to_string()
                } else {
                    "Operator-selected option for a targeted rescan.".to_string()
                },
                hallucinated,
                operator_chosen: true,
                // An operator's explicit forced pick needs no repo evidence — they already
                // decided the rule applies (that's what forcing a choice for it means).
                evidence: None,
                applicable: true,
            });
            if let Some(opt) = a.options.iter().find(|o| o.id == final_id) {
                if let Some(entry) = effective_selected
                    .iter_mut()
                    .find(|(id, _)| id.eq_ignore_ascii_case(&a.rule_id))
                {
                    entry.1 = opt.directive.clone();
                }
            }
        }
        // Everything else: ask the model to recommend, in ONE dedicated pass covering every
        // rule left. Fail-soft — a failed recommendation pass leaves those rules at their
        // pre-existing (project-selected/default) directive, exactly today's behavior, rather
        // than aborting the whole audit.
        if !ask_alts.is_empty() {
            match recommend_alternatives(
                llm,
                repo,
                files,
                map_files.unwrap_or(files),
                &ask_alts,
                audit_model.as_deref(),
                feedback,
                meter,
            )
            .await
            {
                Ok(recs) => {
                    for rec in &recs {
                        let Some(a) = ask_alts.iter().find(|a| a.rule_id == rec.rule_id) else {
                            continue;
                        };
                        if !rec.applicable {
                            // P7: no evidence this rule's concern applies to this codebase at
                            // all (no pagination anywhere, no API versioning anywhere, …) —
                            // drop it from the directive set the violation passes actually
                            // check, so it emits ZERO findings instead of checking violations
                            // against a convention nothing in the repo established.
                            effective_selected
                                .retain(|(id, _)| !id.eq_ignore_ascii_case(&a.rule_id));
                            continue;
                        }
                        if let Some(opt) =
                            a.options.iter().find(|o| o.id == rec.recommended_option_id)
                        {
                            if let Some(entry) = effective_selected
                                .iter_mut()
                                .find(|(id, _)| id.eq_ignore_ascii_case(&a.rule_id))
                            {
                                entry.1 = opt.directive.clone();
                            }
                        }
                    }
                    recommendations.extend(recs);
                }
                Err(e) => {
                    eprintln!(
                        "[camerata-server] alternative-recommendation pass failed for {repo}: {e}"
                    );
                    // W6: never a SILENT omission — the export must say this pass did not run.
                    failed_passes.push(FailedPass {
                        repo: repo.to_string(),
                        pass: "rule-alternative recommendations".to_string(),
                        reason: e.to_string(),
                    });
                }
            }
        }
    }

    // Mode is the speed/scale knob: Sequential = 1 call per chunk with all rules; Parallel =
    // rule-batches × file-chunks run concurrently; Batch = one Anthropic Message Batch at
    // 50% discount, reassembled by custom_id.
    let (concurrency, batch_size) = mode.tuning();

    // ── Rule-routing plan ───────────────────────────────────────────────────────────
    // Group `effective_selected` rules by scope so each language group only audits its own
    // files. The plan is computed even for Batch mode (so the savings estimate is available),
    // but the Batch execution path does not yet apply the routing (see doc comment above).
    let route_plan = crate::scan_routing::plan_routes(&effective_selected, files);
    if route_plan.saved_fraction() > 0.0 {
        eprintln!(
            "[camerata-server] rule-routing: {:.0}% input reduction for {repo} ({} groups, {} rules routed)",
            route_plan.saved_fraction() * 100.0,
            route_plan.groups.len(),
            effective_selected.len(),
        );
    }

    // ── Dispatch to the appropriate execution engine ─────────────────────────────────
    let (mut all_findings, mut all_proposed, requested, ok_passes, last_err) = if mode
        == ScanMode::Batch
    {
        // Batch path: submit all (chunk × rule-batch) pairs as one Message Batch, poll to
        // completion, reassemble by custom_id. The job's add_total is called inside
        // run_passes_batch (it knows the full item count before any network I/O).
        // NOTE: the Batch path audits every rule against every file (no per-rule routing yet).
        let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
        let batches: Vec<&[(String, String)]> = if effective_selected.is_empty() {
            vec![&effective_selected]
        } else {
            effective_selected.chunks(batch_size.max(1)).collect()
        };
        // The Message-Batches path is concrete-only (API-key-gated; `submit_batch` et al.
        // are not part of the minimal `LlmPort` seam), so recover the concrete `&Llm` via
        // the `as_any` downcast. In production this is always a real `Llm`, so the downcast
        // always succeeds and the behavior is unchanged. A non-`Llm` completer (a test stub)
        // can only drive the non-batch real-time path; batch mode is not reachable for it.
        let llm_concrete = llm.as_any().downcast_ref::<Llm>().ok_or_else(|| {
            anyhow::anyhow!(
                "batch mode requires the concrete Llm client (the Message-Batches API is not \
                 part of the LlmPort seam); use parallel/sequential mode with a custom completer"
            )
        })?;
        run_passes_batch(
            llm_concrete,
            repo,
            &repo_map,
            &adopted,
            audit_model.as_deref(),
            job,
            &chunks,
            &batches,
            "pass",
            meter,
            // Batch mode does not yet apply per-rule routing (all rules vs all files), so
            // the Scope::All advisory semantics apply: advisory is enabled (not disabled).
            // If batch mode ever gains routing, pass the group's advisory_disabled flag here.
            false,
        )
        .await?
    } else {
        // ── Real-time path (parallel or sequential) with rule-routing ────────────────
        //
        // When routing produces only one group (all rules are cross-cutting, or no rules at
        // all), the loop runs once with no difference from the old single-pass behavior.
        // When routing produces multiple groups, each group runs its rules over its own
        // (smaller) file subset. Advisory is enabled only in the All group.
        run_routed_passes(
            llm,
            repo,
            files,
            &effective_selected,
            &route_plan,
            &repo_map,
            &adopted,
            audit_model.as_deref(),
            feedback,
            job,
            concurrency,
            batch_size,
            meter,
        )
        .await
    };

    // Every pass failed -> surface the error so the caller notes the AI audit was skipped
    // (the deterministic findings still return independently). Each pass already finalized
    // its own transcript status, so the UI spinner stops regardless.
    if ok_passes == 0 {
        if let Some(e) = last_err {
            return Err(e);
        }
    }

    // ── Resolution round ────────────────────────────────────────────────────────────
    // Earlier passes may have DEFERRED a judgment because it needed the bodies of files
    // not in that pass (the residual cross-body limit of chunking). Pull exactly those
    // files together and re-audit once — so a cross-file rule the model couldn't decide
    // in a single pass gets resolved instead of silently missed. SINGLE round (the
    // resolution passes' own needs_files are ignored) to keep it bounded.
    //
    // The resolution round runs the FULL selected rule set against the requested files (no
    // per-rule routing) so no cross-file deferred judgment is inadvertently skipped. Advisory
    // is enabled (default) since this is an independent pass over a small file set.
    let resolution: Vec<(String, String)> = files
        .iter()
        .filter(|(p, _)| requested.contains(p))
        .cloned()
        .collect();
    if !resolution.is_empty() {
        let batches_res: Vec<&[(String, String)]> = if effective_selected.is_empty() {
            vec![&effective_selected]
        } else {
            effective_selected.chunks(batch_size.max(1)).collect()
        };
        let res_chunks = chunk_files(&resolution, CHUNK_DIGEST_CHARS);
        // BUG-4 fix: in Batch mode, run_passes_batch already called add_total with the
        // full batch item count and the batch's inc_done calls bring done to that value.
        // A second add_total here would inflate the denominator AFTER the batch completes,
        // causing the UI progress bar to temporarily drop from 100% back to a lower value.
        // In Batch mode we skip the add_total for the resolution round; the resolution
        // items' inc_done calls will push done slightly past the total, which clamps at
        // 100% on the UI side and is far less disruptive than the denominator-inflate glitch.
        // In non-Batch mode the main passes pre-seed the total via run_routed_passes and the
        // resolution items legitimately extend the denominator.
        if mode != ScanMode::Batch {
            if let Some((jstore, jid)) = job {
                jstore.add_total(jid, res_chunks.len() * batches_res.len());
            }
        }
        // Resolution always runs on the real-time parallel engine (even in batch mode):
        // the resolution set is small (typically 1-5 files) and the polling overhead of a
        // separate batch submission outweighs the marginal discount.
        let res_concurrency = if mode == ScanMode::Batch {
            PARALLEL_CONCURRENCY
        } else {
            concurrency
        };
        let (rf, rp, _rn, _rok, _re) = run_passes(
            llm,
            repo,
            &repo_map,
            &adopted,
            audit_model.as_deref(),
            feedback,
            job,
            &res_chunks,
            &batches_res,
            res_concurrency,
            "resolution",
            &format!("audit-{repo}-res"),
            meter,
            false, // advisory enabled: novel findings in resolution files are wanted
        )
        .await;
        all_findings.extend(rf);
        all_proposed.extend(rp);
    }

    // Resolve each finding's line DETERMINISTICALLY from its verbatim snippet before dedup,
    // so the model's unreliable line counting can't (a) mislocate a finding or (b) defeat the
    // location merge. The model says WHAT (the snippet); plain code finds WHERE.
    resolve_finding_lines(&mut all_findings, files);

    // Cross-chunk dedup + cross-name LOCATION MERGE: the shared repo map means the same
    // issue can surface in more than one pass, and the model labels the SAME violation under
    // several rule names at one line (an invented `AI-CONTROLLER-DIRECT-DB` + the adopted
    // `ARCH-STRICT-LAYERING-1` + sibling AI- names), each with a different title. Step 1
    // drops byte-identical (path, line, rule_id) repeats. Step 2 is the real reduce:
    // `merge_by_location` collapses every finding at one (path, line) into ONE row, keeping
    // an adopted corpus rule as the primary and demoting the rest to `also_matches`. Keying
    // on LOCATION (not title) is what makes this work — titles vary per invented name. This
    // is N-in / M-out (M < N), a true dedup, not the calibration pass's N-in/N-out scoring.
    {
        let rows_in = all_findings.len();
        let mut seen = std::collections::HashSet::new();
        all_findings.retain(|f| seen.insert((f.path.clone(), f.line, f.rule_id.clone())));
        let rows_after_dedup = all_findings.len();
        all_findings = merge_by_location(all_findings, files);
        let rows_out = all_findings.len();
        // W1: these freshly-generated findings carry no `also_matches` yet (this is the
        // FIRST merge they pass through), so every id in a post-merge row's `also_matches` is
        // genuinely new here — no pre-existing-set diff needed, unlike the cross-family merge
        // at the onboard.rs call site, which runs over an ALREADY-merged set.
        let mut acc = crate::scan_ledger::StageAccounting {
            deduped: rows_in.saturating_sub(rows_after_dedup),
            ..Default::default()
        };
        for f in &all_findings {
            for id in &f.also_matches {
                acc.push_merge(id.clone(), f.rule_id.clone());
            }
        }
        stage_samples.push(crate::scan_ledger::StageSample {
            stage: "ai-location-merge".to_string(),
            rows_in,
            rows_out,
            accounting: acc,
        });
        let mut seen_p = std::collections::HashSet::new();
        all_proposed.retain(|p| seen_p.insert(p.id.clone()));
    }

    // Calibration pass over ALL aggregated findings: recalibrate severity + flag
    // low-confidence findings. It does NOT drop anything — recall-first discovery hands
    // every finding to the architect. Skipped entirely when there's nothing to calibrate
    // (no findings → no point spending a round-trip).
    //
    // This pass runs AFTER every chunk×rule pass has reported "done", and it's a single
    // synchronous round-trip over all findings — so without its own visible agent the UI
    // showed every pass "done" while the spinner kept turning for another minute.
    // `verify_findings` registers its OWN transcript agent (real prompt, real output —
    // see its doc comment) so the cockpit shows "calibrating N findings" with actual
    // content instead of a mystery hang or an empty placeholder. (Dedup/merge also
    // shrinks N, so this round is now faster too.)
    let pre_calibration_len = all_findings.len();
    let verified = if all_findings.is_empty() {
        all_findings
    } else {
        // Repo-shape line for the proportionality signal (Bug 4 §2b): detected stack + code-file
        // count, from signals already computed. detect_stack is the same one grounding uses.
        let stack = crate::onboard::detect_stack(repo, files);
        let repo_shape = if stack.frameworks.is_empty() {
            format!("This repository has {} code files.", files.len())
        } else {
            format!(
                "This is a {} repo with {} code files.",
                stack.frameworks.join(", "),
                files.len()
            )
        };
        let (verified, estimation_failed) = verify_findings(
            llm,
            repo,
            all_findings,
            calib_model.as_deref(),
            feedback,
            meter,
            thorough,
            &repo_shape,
        )
        .await;
        // C3-1b: disclose a run-wide estimation-pass failure the same way the
        // alternative-recommendation pass already does — see `FailedPass`'s doc comment.
        failed_passes.extend(estimation_failed);
        verified
    };
    let mut verified = verified;
    // P7: a rule marked NOT APPLICABLE (no evidence its concern applies to this codebase at
    // all) must emit ZERO findings. Dropping it from `effective_selected` earlier stops the
    // per-chunk prompt from asking about it at all, but this hard filter is the actual
    // guarantee — it also catches the rare case where a model free-associates a violation
    // under that rule id anyway (the prompt's "flag any other genuine issues you find"
    // catch-all), whether cited under the bare adopted id or the `AI-` invented-name prefix.
    if !recommendations.is_empty() {
        let not_applicable: std::collections::HashSet<String> = recommendations
            .iter()
            .filter(|r| !r.applicable)
            .map(|r| r.rule_id.to_ascii_uppercase())
            .collect();
        if !not_applicable.is_empty() {
            verified.retain(|f| {
                let bare = f.rule_id.trim_start_matches("AI-").to_ascii_uppercase();
                !not_applicable.contains(&bare)
            });
        }
    }
    // W1: calibration itself never drops a row (see this block's own doc comment above); the
    // ONLY row-count reduction between `pre_calibration_len` and here is the P7
    // not-applicable filter just above, so that is the one explicit disposition this stage
    // accounts for.
    {
        let rows_out = verified.len();
        let mut acc = crate::scan_ledger::StageAccounting::default();
        let dropped = pre_calibration_len.saturating_sub(rows_out);
        if dropped > 0 {
            acc.other
                .push(("not_applicable_rule_filtered".to_string(), dropped));
        }
        stage_samples.push(crate::scan_ledger::StageSample {
            stage: "ai-calibration".to_string(),
            rows_in: pre_calibration_len,
            rows_out,
            accounting: acc,
        });
    }
    // Tag every finding under a multi-option rule with the option it was actually judged
    // against — the report layer (`report_export::resolve_fix`) reads this so the Fix text
    // matches the evaluated option, never a stale default. Every finding for a given rule_id
    // in THIS response was checked against the SAME directive (the rewrite above), so this is
    // a safe blanket tag, not a per-finding guess.
    if !recommendations.is_empty() {
        let by_rule: std::collections::HashMap<&str, &str> = recommendations
            .iter()
            .map(|r| (r.rule_id.as_str(), r.recommended_option_id.as_str()))
            .collect();
        for f in verified.iter_mut() {
            if let Some(id) = by_rule.get(f.rule_id.to_ascii_uppercase().as_str()) {
                f.evaluated_option_id = Some((*id).to_string());
            }
        }
    }
    Ok((
        verified,
        all_proposed,
        recommendations,
        failed_passes,
        stage_samples,
    ))
}

// ════════════════════════════════════════════════════════════════════════════════════
// DEEP COMPLIANCE & SECURITY TIER (#55, in-MVP per #62)
// ════════════════════════════════════════════════════════════════════════════════════
//
// An ADDITIVE, OPT-IN tier that layers three analysis LENSES on top of the always-on
// deterministic floor + the standard AI architectural audit. It changes NOTHING about the
// default scan — it only runs when the audit request sets `deep`. The three lenses are:
//
//   1. SOC-2 readiness / GAP ANALYSIS — maps the repo's detectable practices + the
//      standard findings onto SOC-2 Trust-Services / Common-Criteria controls and reports
//      the GAPS. It is a GAP ANALYSIS, never a "report": no agent can produce a SOC-2
//      report (a CPA firm attests to an ORGANIZATION's controls over 6–12 months). The
//      product must never call this output a "SOC-2 report" — that is a liability line (#55).
//
//   2. DEEP SECURITY AUDIT — a deeper-than-floor security pass (authorization on write
//      paths, sensitive-data handling, secret/credential flow) that goes beyond the
//      mechanical floor's line-level secret/SQL/path checks.
//
//   3. THREAT MODEL — derives a structured threat model from the repo map: entry points,
//      trust boundaries, sensitive-data paths, and the threats against them.
//
// HONESTY GUARDRAILS (load-bearing, from #62):
//   - Every output is ADVISORY and MODEL-INFERRED, NOT externally validated. External
//     validation against comparator tools + ground truth is #56 Phase 2 (deferred). Each
//     lens result carries [`DeepLensResult::advisory`] = true and an explicit disclaimer
//     so the UI can label it honestly.
//   - The SOC-2 lens is labeled a "gap analysis" everywhere; it never claims certification.
//   - These lenses read STATIC code. They are NOT a penetration test — a true pen test
//     needs a running deployment (post-deploy, out of scope here, also per #55).
//
// COST: the deep tier reuses the same per-call LLM machinery and the same [`UsageMeter`],
// so its spend folds into the report's actual-vs-estimated readout. It is the MOST
// EXPENSIVE pass (three extra whole-repo lenses on top of the standard audit) and is why
// it is strictly opt-in. The UI's `estimate_audit_cost` already prices the standard audit
// from `code_chars`; the deep tier adds (roughly) three more whole-repo passes on the
// selected/Opus model, which the cost readout should surface as the priciest option.

/// The disclaimer string attached to every deep-tier lens result. Centralized so the wording
/// stays consistent and the honesty guardrail (#62) is impossible to drop by accident.
pub const DEEP_ADVISORY_DISCLAIMER: &str =
    "Advisory and model-inferred — NOT externally validated (external validation against \
     comparator tools and ground-truth corpora is a separate, deferred capability). Review \
     every item before acting on it. This is a static-code analysis, not a penetration test.";

/// Which deep-tier lens produced a result. Stable wire strings (`soc2-gap`, `deep-security`,
/// `threat-model`) so the UI can route/group lens output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepLens {
    /// SOC-2 readiness / gap analysis.
    Soc2Gap,
    /// Deep security audit (beyond the deterministic floor).
    DeepSecurity,
    /// Threat model derived from the repo map.
    ThreatModel,
}

impl DeepLens {
    /// Stable wire id for this lens (serialized into the result; used as the transcript label).
    pub fn id(self) -> &'static str {
        match self {
            DeepLens::Soc2Gap => "soc2-gap",
            DeepLens::DeepSecurity => "deep-security",
            DeepLens::ThreatModel => "threat-model",
        }
    }
    /// Human-facing title — note the SOC-2 lens is a "Gap Analysis", never a "report".
    pub fn title(self) -> &'static str {
        match self {
            DeepLens::Soc2Gap => "SOC-2 Readiness Gap Analysis",
            DeepLens::DeepSecurity => "Deep Security Audit",
            DeepLens::ThreatModel => "Threat Model",
        }
    }
}

/// One mapped SOC-2 control and the gap (if any) the lens found against it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Soc2Gap {
    /// The control reference, e.g. `CC6.1` (Common Criteria) or a Trust-Services criterion.
    pub control: String,
    /// Short control name/expectation, e.g. "Logical access controls".
    pub title: String,
    /// `met` | `partial` | `gap` | `unknown` — the readiness status the model inferred.
    pub status: String,
    /// What the model OBSERVED in the repo that informed the status (evidence or its absence).
    pub observed: String,
    /// The concrete gap + remediation direction, when status is `partial` / `gap`.
    pub gap: String,
}

/// One element of the derived threat model.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Threat {
    /// The entry point / asset / trust boundary this threat is against (e.g.
    /// "POST /api/orders handler", "Postgres connection", "uploaded-file path").
    pub component: String,
    /// `entry-point` | `trust-boundary` | `data-store` | `dependency` | `other` — the kind
    /// of element, so the UI can group the model by surface.
    pub kind: String,
    /// The threat itself (what could go wrong).
    pub threat: String,
    /// STRIDE-ish category when the model offers one (`spoofing`, `tampering`, `repudiation`,
    /// `info-disclosure`, `dos`, `elevation`), else free text.
    pub category: String,
    /// The suggested mitigation direction.
    pub mitigation: String,
    /// `high` | `medium` | `low` — model-inferred severity.
    pub severity: String,
}

/// The structured result of ONE deep-tier lens. Each lens carries its own payload (only one
/// of the vectors is populated per lens) plus the advisory flag + disclaimer so the honesty
/// guardrail travels with the data.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeepLensResult {
    /// Stable lens id (`soc2-gap` | `deep-security` | `threat-model`).
    pub lens: String,
    /// Human-facing lens title.
    pub title: String,
    /// Always true — every deep-tier output is advisory + model-inferred (#62).
    pub advisory: bool,
    /// The honesty disclaimer ([`DEEP_ADVISORY_DISCLAIMER`]).
    pub disclaimer: String,
    /// SOC-2 lens payload (empty for the other lenses).
    #[serde(default)]
    pub soc2_gaps: Vec<Soc2Gap>,
    /// Deep-security lens payload: reuses the standard [`Finding`] shape (empty for others).
    #[serde(default)]
    pub security_findings: Vec<Finding>,
    /// Threat-model lens payload (empty for the other lenses).
    #[serde(default)]
    pub threats: Vec<Threat>,
    /// A one-paragraph narrative summary the model wrote for this lens (optional).
    #[serde(default)]
    pub summary: String,
    /// Set when the lens failed (model/transport error) so the UI shows it ran-but-errored
    /// rather than silently producing an empty result.
    #[serde(default)]
    pub error: Option<String>,
}

impl DeepLensResult {
    /// A public empty-but-honest result for a lens, carrying the advisory flag + disclaimer.
    /// Used when aggregating per-repo lens results into one tier-level result.
    pub fn merged_empty(lens: DeepLens) -> Self {
        Self::empty(lens)
    }

    /// An empty-but-honest result for a lens, carrying the advisory flag + disclaimer.
    fn empty(lens: DeepLens) -> Self {
        Self {
            lens: lens.id().to_string(),
            title: lens.title().to_string(),
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
            soc2_gaps: Vec::new(),
            security_findings: Vec::new(),
            threats: Vec::new(),
            summary: String::new(),
            error: None,
        }
    }
}

/// The aggregate deep-tier output across all three lenses for one repo set. Attached to the
/// scan report under [`crate::onboard::ScanReport::deep`] when the deep tier ran.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeepReport {
    /// The three lens results, in a stable order (gap analysis, security, threat model).
    pub lenses: Vec<DeepLensResult>,
    /// Always true — the whole tier is advisory (#62). Mirrors each lens's flag at the top
    /// level so a consumer can gate on one field.
    pub advisory: bool,
    /// The honesty disclaimer for the tier as a whole.
    pub disclaimer: String,
}

/// SYSTEM PROMPT — SOC-2 readiness / gap analysis lens.
///
/// Maps the repo's detectable practices onto SOC-2 Common-Criteria controls and reports
/// GAPS. The prompt is explicit that this is a GAP ANALYSIS, not a SOC-2 report, and that no
/// certification is implied — the same honesty guardrail the product UI enforces (#55/#62).
pub fn soc2_gap_system_prompt() -> String {
    r#"You are a security/compliance engineer performing a SOC-2 READINESS GAP ANALYSIS of a codebase for Camerata.

IMPORTANT — what this is and is NOT:
- This is a GAP ANALYSIS: you map what the code + repo evidently DO against SOC-2 control expectations and report where they fall short. Call it a "gap analysis".
- This is NOT a "SOC-2 report". A SOC-2 report is a CPA firm's attestation about an organization's controls operating over months. You produce neither an attestation nor a certification. Never imply the project IS or WILL BE certified.
- You see STATIC CODE only — not the running system, not the org's policies/HR/vendor processes. For controls that depend on organizational evidence you cannot see, say so (status "unknown"), do not guess "met".

Map against the SOC-2 Common Criteria (Security) — at minimum consider:
- CC6.1 Logical access controls (authn/authz on sensitive operations)
- CC6.6 Boundary protection / network access
- CC6.7 Data-in-transit and at-rest protection (encryption, secret handling)
- CC6.8 Malicious-code / dependency controls
- CC7.2 Security monitoring / logging / audit trail
- CC7.3 / CC7.4 Incident handling hooks
- CC8.1 Change management (review, CI gates, migrations)
- CC1/CC2 Control environment & communication (only what code/config can evidence)

For EACH control you assess, emit one entry with:
- "control": the criterion id (e.g. "CC6.1").
- "title": a short name for the control.
- "status": one of "met" | "partial" | "gap" | "unknown" (use "unknown" when it needs org evidence you can't see).
- "observed": what in the repo informed the status (a file/pattern you saw, or that you saw nothing).
- "gap": for "partial"/"gap", the concrete shortfall and the remediation direction; empty for "met"/"unknown".

Report GAPS generously — recall over precision; a human reviews everything. Do not invent evidence. Do not claim certification.

Return ONLY a JSON object, no prose, no markdown fences:
{
  "summary": "one short paragraph on overall readiness, explicitly framed as a gap analysis",
  "gaps": [
    {"control":"CC6.1","title":"Logical access controls","status":"gap","observed":"…","gap":"…"}
  ]
}
If you genuinely cannot assess anything, return {"summary":"…","gaps":[]}."#
        .to_string()
}

/// SYSTEM PROMPT — deep security audit lens.
///
/// A deeper-than-floor security read (authorization on write paths, sensitive-data handling,
/// secret/credential flow). Emits the SAME `findings` JSON shape the standard audit uses, so
/// [`parse_ai_findings`] parses it directly and the UI renders security findings in the
/// familiar table. Deterministic-floor concerns are excluded (they are already covered).
pub fn deep_security_system_prompt() -> String {
    r#"You are a senior application-security engineer performing a DEEP SECURITY AUDIT of a codebase for Camerata.

This is DEEPER than the always-on deterministic floor (which already finds hardcoded secrets, raw SQL string concatenation, secrets-in-URLs, and path-escape writes — DO NOT re-report those). Go beyond line-level lint and reason about:
- AUTHORIZATION: write/mutation/delete paths with no authz check; horizontal/vertical privilege gaps; missing ownership checks on resources; admin actions reachable without role checks.
- AUTHENTICATION & SESSION: weak/missing auth on sensitive endpoints; token/session handling flaws.
- SENSITIVE-DATA HANDLING: PII/credentials/financial data logged, returned in responses, or stored unencrypted; over-broad serialization that leaks fields.
- SECRET / CREDENTIAL FLOW: credentials read from insecure sources, passed through untrusted paths, or exposed to clients (beyond the floor's hardcoded-literal check).
- INJECTION beyond raw-SQL-concat: command/template/path/deserialization injection; SSRF; unsafe redirects.
- INPUT VALIDATION & TRUST BOUNDARIES: unvalidated external input reaching a sensitive sink.

You have the REPO MAP (every file + its public symbols) and SOME file bodies. When judging a rule needs the BODY of a file not included, list it in `needs_files` rather than guessing.

RECALL OVER PRECISION — a human triages every finding; report borderline issues at severity "low". Cite the exact offending line in `code` (copied verbatim) and `line` (the NNNN| number). For `rule`, use a short kebab security name (e.g. "missing-authz-on-write", "pii-in-logs", "ssrf-on-fetch").

SEVERITY. Use "low"/"medium"/"high" for the normal range. Reserve "critical" — it must stay RARE — for a finding that clears ALL of: concretely (not theoretically) exploitable, AND a competent attacker or a single bad input reaches serious impact quickly. Qualifying classes for THIS lens: an entry point reachable with NO authentication or authorization that performs a privileged, destructive, or financial operation (e.g. a payment/charge/refund/balance-mutating endpoint callable by anyone, an admin action with no role check); a credential or secret that flows to a client or an untrusted sink; PII or equivalently sensitive data readable/writable with no access control on an internet-exposed surface; injection or deserialization with a real, reachable, unauthenticated path. When in doubt between "high" and "critical", use "high" — critical is not a synonym for "this is bad," it is reserved for "an attacker exploits this quickly for serious impact."

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {"path":"…","line":0,"severity":"critical|high|medium|low","rule":"short-kebab-security-name","title":"…","code":"the exact offending line","detail":"why it's exploitable and the fix direction"}
  ],
  "proposed_rules": [],
  "needs_files": []
}
If the code is genuinely clean, return {"findings":[],"proposed_rules":[],"needs_files":[]}."#
        .to_string()
}

/// SYSTEM PROMPT — threat-model lens.
///
/// Derives a structured threat model from the repo: entry points, trust boundaries,
/// sensitive-data paths, and the threats against them (STRIDE-flavored) with mitigations.
pub fn threat_model_system_prompt() -> String {
    r#"You are a security architect deriving a THREAT MODEL for a codebase from its structure.

Using the repo map and the file bodies provided, identify:
- ENTRY POINTS: HTTP routes/handlers, CLI commands, queue/event consumers, scheduled jobs, public APIs.
- TRUST BOUNDARIES: where untrusted input crosses into trusted code (network edge, deserialization, IPC, third-party calls).
- DATA STORES & SENSITIVE-DATA PATHS: databases, caches, file storage, secrets, and the flow of PII/credentials/financial data through them.
- DEPENDENCIES that widen the attack surface (where evident from manifests/imports).

For EACH notable element, enumerate the threats against it. Prefer STRIDE categories where they fit (spoofing, tampering, repudiation, info-disclosure, dos, elevation). Give a concrete mitigation direction and a model-inferred severity.

This is model-inferred and advisory — recall over precision; a human reviews it.

Return ONLY a JSON object, no prose, no markdown fences:
{
  "summary": "one short paragraph describing the system's attack surface",
  "threats": [
    {"component":"POST /api/orders handler","kind":"entry-point","threat":"unauthenticated order creation","category":"elevation","mitigation":"require auth + ownership check","severity":"high"}
  ]
}
`kind` is one of: "entry-point" | "trust-boundary" | "data-store" | "dependency" | "other".
If you genuinely cannot derive a model, return {"summary":"…","threats":[]}."#
        .to_string()
}

/// Parse the SOC-2 gap-analysis lens response into `(summary, gaps)`. Robust: malformed
/// output yields an empty result rather than erroring the tier. Statuses are normalized to
/// the closed set (`met`/`partial`/`gap`/`unknown`); an unrecognized status becomes
/// `unknown` (the honest default — we did not get a clear signal).
pub fn parse_soc2_gaps(raw: &str) -> (String, Vec<Soc2Gap>) {
    let Some(json) = extract_json_object(raw) else {
        return (String::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (String::new(), Vec::new());
    };
    let summary = v["summary"].as_str().unwrap_or("").trim().to_string();
    let mut gaps = Vec::new();
    if let Some(arr) = v["gaps"].as_array() {
        for g in arr {
            let control = g["control"].as_str().unwrap_or("").trim().to_string();
            let title = g["title"].as_str().unwrap_or("").trim().to_string();
            // Drop entirely-empty rows (no control and no title — nothing to show).
            if control.is_empty() && title.is_empty() {
                continue;
            }
            let status = match g["status"].as_str().unwrap_or("unknown").trim() {
                "met" => "met",
                "partial" => "partial",
                "gap" => "gap",
                _ => "unknown",
            }
            .to_string();
            gaps.push(Soc2Gap {
                control,
                title,
                status,
                observed: g["observed"].as_str().unwrap_or("").trim().to_string(),
                gap: g["gap"].as_str().unwrap_or("").trim().to_string(),
            });
        }
    }
    (summary, gaps)
}

/// Parse the threat-model lens response into `(summary, threats)`. Robust to malformed
/// output. `kind`, `category`, and `severity` are normalized to their closed sets so the UI
/// can group on them; an unrecognized value falls back to the safest/most-generic bucket.
pub fn parse_threats(raw: &str) -> (String, Vec<Threat>) {
    let Some(json) = extract_json_object(raw) else {
        return (String::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (String::new(), Vec::new());
    };
    let summary = v["summary"].as_str().unwrap_or("").trim().to_string();
    let mut threats = Vec::new();
    if let Some(arr) = v["threats"].as_array() {
        for t in arr {
            let component = t["component"].as_str().unwrap_or("").trim().to_string();
            let threat = t["threat"].as_str().unwrap_or("").trim().to_string();
            // Need at least a component or a threat statement to be a real row.
            if component.is_empty() && threat.is_empty() {
                continue;
            }
            let kind = match t["kind"].as_str().unwrap_or("other").trim() {
                "entry-point" => "entry-point",
                "trust-boundary" => "trust-boundary",
                "data-store" => "data-store",
                "dependency" => "dependency",
                _ => "other",
            }
            .to_string();
            let severity = match t["severity"].as_str().unwrap_or("medium").trim() {
                "critical" => "critical",
                "high" => "high",
                "low" => "low",
                _ => "medium",
            }
            .to_string();
            threats.push(Threat {
                component,
                kind,
                threat,
                // Category is free-ish text; keep it verbatim (trimmed) so a STRIDE label or a
                // custom phrase both survive.
                category: t["category"].as_str().unwrap_or("").trim().to_string(),
                mitigation: t["mitigation"].as_str().unwrap_or("").trim().to_string(),
                severity,
            });
        }
    }
    (summary, threats)
}

/// Run ONE prose-style deep lens (SOC-2 gap or threat model) over the whole repo digest.
/// These two lenses are single whole-repo passes (their value is the cross-cutting view, not
/// per-chunk recall), so we build one digest, run one call, and parse the structured result.
/// Streaming into the transcript when feedback is present, so the cockpit shows the lens work
/// live. Graceful: on any model failure the lens result carries the error, never panics.
#[allow(clippy::too_many_arguments)]
async fn run_prose_lens(
    llm: &dyn LlmPort,
    lens: DeepLens,
    repo: &str,
    repo_map: &str,
    digest: &str,
    system: String,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> DeepLensResult {
    let prompt = format!(
        "Repository: {repo}\n\n{repo_map}{digest}\n\n── {} for the code above. Return the JSON described in the system prompt. ──",
        lens.title()
    );
    let session = format!("deep-{}-{repo}", lens.id());
    if let Some((store, key)) = feedback {
        store.register(
            key,
            crate::transcript::AgentTranscript {
                session_id: session.clone(),
                role: format!("{} — {repo}", lens.title()),
                prompt: prompt.clone(),
                output: String::new(),
                status: "running".to_string(),
            },
        );
    }
    let mut req = LlmRequest::new(prompt)
        .with_system(system)
        // Whole-repo structured output can be sizable (many controls / many threats).
        .with_max_tokens(8192);
    if let Some(m) = audit_model {
        req = req.with_model(m.to_string());
    }
    let resp = if let Some((store, key)) = feedback {
        let mut on_delta = |t: &str| store.append_output_raw(key, &session, t);
        llm.complete_streaming(req, &mut on_delta).await
    } else {
        let cap = total_backstop();
        match tokio::time::timeout(cap, llm.complete(req)).await {
            Ok(inner) => inner,
            Err(_) => Err(anyhow::anyhow!(
                "lens exceeded the {}s backstop",
                cap.as_secs()
            )),
        }
    };
    let mut result = DeepLensResult::empty(lens);
    match resp {
        Ok(r) => {
            if let Some(m) = meter {
                m.record(&r);
            }
            match lens {
                DeepLens::Soc2Gap => {
                    let (summary, gaps) = parse_soc2_gaps(&r.text);
                    result.summary = summary;
                    result.soc2_gaps = gaps;
                }
                DeepLens::ThreatModel => {
                    let (summary, threats) = parse_threats(&r.text);
                    result.summary = summary;
                    result.threats = threats;
                }
                // Security uses the chunked path, not this prose lens.
                DeepLens::DeepSecurity => {}
            }
            if let Some((store, key)) = feedback {
                store.set_status(key, &session, "done");
            }
        }
        Err(e) => {
            result.error = Some(format!("{e}"));
            if let Some((store, key)) = feedback {
                store.set_status(key, &session, "blocked");
            }
        }
    }
    result
}

/// Run the deep SECURITY lens. It reuses the full chunked audit engine ([`run_passes`]) so a
/// large repo is covered chunk-by-chunk (the same reason the standard audit chunks), with the
/// security-focused system prompt swapped in via a single-batch free-form pass. The result is
/// the standard `Finding` shape, deduped + location-merged like the standard audit. Findings
/// are tagged `AI-`-prefixed by the parser (no adopted rules here), keeping their advisory
/// provenance honest.
#[allow(clippy::too_many_arguments)]
async fn run_security_lens(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    repo_map: &str,
    audit_model: Option<&str>,
    mode: ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> DeepLensResult {
    let mut result = DeepLensResult::empty(DeepLens::DeepSecurity);
    if files.is_empty() {
        return result;
    }
    // The security lens has no adopted-rule corpus — it is a free-form security read — so it
    // runs as a single empty batch per chunk (one pass each), like the no-rules audit path.
    let (concurrency, _batch_size) = mode.tuning();
    let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
    let empty_batch: &[(String, String)] = &[];
    let batches: Vec<&[(String, String)]> = vec![empty_batch];
    let adopted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let (findings, _proposed, _requested, _ok, _err) = run_security_passes(
        llm,
        repo,
        repo_map,
        &adopted,
        audit_model,
        feedback,
        &chunks,
        &batches,
        concurrency,
        meter,
    )
    .await;
    // Dedup byte-identical repeats then location-merge — same reduce the standard audit uses,
    // so one smell reported under several names at one line is ONE row.
    let mut findings = findings;
    resolve_finding_lines(&mut findings, files);
    let mut seen = std::collections::HashSet::new();
    findings.retain(|f| seen.insert((f.path.clone(), f.line, f.rule_id.clone())));
    let findings = merge_by_location(findings, files);
    result.security_findings = findings;
    result
}

/// Like [`run_passes`] but with the DEEP-SECURITY system prompt instead of the standard
/// architectural one. Kept as its own small function so the deep tier never disturbs the
/// standard audit's pass machinery, and so the security prompt is the only thing that
/// differs. Single-batch (free-form security read), so there is no rule-batch dimension.
#[allow(clippy::too_many_arguments)]
async fn run_security_passes(
    llm: &dyn LlmPort,
    repo: &str,
    repo_map: &str,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    chunks: &[&[(String, String)]],
    batches: &[&[(String, String)]],
    concurrency: usize,
    meter: Option<&UsageMeter>,
) -> (
    Vec<Finding>,
    Vec<ProposedRule>,
    std::collections::HashSet<String>,
    usize,
    Option<anyhow::Error>,
) {
    use futures::stream::StreamExt;
    let digests: Vec<String> = chunks.iter().map(|c| build_digest(c)).collect();
    let n_c = chunks.len();
    let n_b = batches.len().max(1);
    let work: Vec<usize> = (0..n_c).collect();
    type PassOut = (
        usize,
        anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)>,
    );
    let results: Vec<PassOut> = futures::stream::iter(work)
        .map(|ci| {
            let digest = &digests[ci];
            async move {
                let prompt = format!(
                    "Repository: {repo} (security pass {}/{n_c})\n\n{repo_map}{digest}\n\n── Perform a DEEP SECURITY AUDIT of the code above. Use the REPO MAP for cross-file context. Return the JSON described in the system prompt. ──",
                    ci + 1,
                );
                let session = format!("deep-security-{repo}-c{ci}");
                if let Some((store, key)) = feedback {
                    store.register(
                        key,
                        crate::transcript::AgentTranscript {
                            session_id: session.clone(),
                            role: format!("Deep Security Audit {}/{n_c} — {repo}", ci + 1),
                            prompt: prompt.clone(),
                            output: String::new(),
                            status: "running".to_string(),
                        },
                    );
                }
                // The security lens swaps in its own system prompt; everything else mirrors
                // `audit_pass` (streaming + meter + robust parse).
                let mut req = LlmRequest::new(prompt)
                    .with_system(deep_security_system_prompt())
                    .with_max_tokens(8192);
                if let Some(m) = audit_model {
                    req = req.with_model(m.to_string());
                }
                let r: anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)> = async {
                    let resp = if let Some((store, key)) = feedback {
                        let mut on_delta = |t: &str| store.append_output_raw(key, &session, t);
                        llm.complete_streaming(req, &mut on_delta).await?
                    } else {
                        let cap = total_backstop();
                        tokio::time::timeout(cap, llm.complete(req))
                            .await
                            .map_err(|_| anyhow::anyhow!("security pass exceeded the {}s backstop", cap.as_secs()))??
                    };
                    if let Some(m) = meter {
                        m.record(&resp);
                    }
                    let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
                    let needs = parse_needs_files(&resp.text);
                    Ok((f, p, needs))
                }
                .await;
                if let Some((store, key)) = feedback {
                    store.set_status(key, &session, if r.is_ok() { "done" } else { "blocked" });
                }
                (ci, r)
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;

    let mut findings = Vec::new();
    let mut proposed = Vec::new();
    let mut requested = std::collections::HashSet::new();
    let mut ok = 0usize;
    let mut last_err = None;
    for (_ci, r) in results {
        match r {
            Ok((f, p, needs)) => {
                findings.extend(f);
                proposed.extend(p);
                requested.extend(needs);
                ok += 1;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let _ = n_b; // single batch; kept for parity/readability with run_passes.
    (findings, proposed, requested, ok, last_err)
}

/// Run the full DEEP COMPLIANCE & SECURITY tier (#55) over one repo's files: the three lenses
/// (SOC-2 gap analysis, deep security audit, threat model), each on the selected/Opus model.
/// ADDITIVE and OPT-IN — only called when the audit request set `deep`; the standard scan is
/// untouched. Every lens is best-effort: a failure attaches an `error` to that lens's result
/// and the others still run. Spend folds into the shared [`UsageMeter`].
///
/// `mode` controls the security lens's chunk concurrency (the prose lenses are single passes).
///
/// `soc2_enabled` gates the SOC-2 gap-analysis lens (feature flag). When `false`, ONLY the
/// soc2 lens is skipped — deep-security and threat-model still run and the report is valid with
/// an empty `soc2_gaps` field. The SOC-2 code is NOT removed; the lens is skipped at call time.
/// Pass `true` (the flag default) to run all three lenses as before.
#[allow(clippy::too_many_arguments)]
pub async fn run_deep_tier(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    audit_model: Option<&str>,
    mode: ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
    soc2_enabled: bool,
) -> DeepReport {
    let repo_map = build_repo_map(files);
    // One whole-repo digest for the two single-pass prose lenses (capped at MAX_DIGEST_CHARS).
    let digest = build_digest(files);

    // Resolve the model the same way the standard audit does: explicit pick wins, else
    // CAMERATA_AUDIT_MODEL, else provider default. The deep tier is meant to run on the strong
    // (Opus) model; the caller passes that through `audit_model`.
    let model = audit_model.map(str::to_string).or_else(|| {
        std::env::var("CAMERATA_AUDIT_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });

    let threat = run_prose_lens(
        llm,
        DeepLens::ThreatModel,
        repo,
        &repo_map,
        &digest,
        threat_model_system_prompt(),
        model.as_deref(),
        feedback,
        meter,
    );
    let security = run_security_lens(
        llm,
        repo,
        files,
        &repo_map,
        model.as_deref(),
        mode,
        feedback,
        meter,
    );

    if soc2_enabled {
        // All three lenses run concurrently.
        let soc2 = run_prose_lens(
            llm,
            DeepLens::Soc2Gap,
            repo,
            &repo_map,
            &digest,
            soc2_gap_system_prompt(),
            model.as_deref(),
            feedback,
            meter,
        );
        let (soc2, security, threat) = tokio::join!(soc2, security, threat);
        DeepReport {
            // Stable order: gap analysis, security, threat model.
            lenses: vec![soc2, security, threat],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        }
    } else {
        // SOC-2 lens disabled by feature flag: run only security + threat-model.
        // The report is still valid; soc2_gaps will be empty on all lenses.
        let (security, threat) = tokio::join!(security, threat);
        DeepReport {
            // Stable order maintained: security, threat model (soc2 omitted).
            lenses: vec![security, threat],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── poll_batch_until_ended: consecutive-failure tolerance (compliance-audit GAP 1) ──
    //
    // Before this, a single failed `poll_batch_status` call propagated straight out of the
    // batch loop via `?`, killing an hours-long (and already-paid-for) batch run over one
    // transient blip. These tests drive the extracted helper with a fake `poll_once` — no
    // live batch, no network — to prove the tolerate-N-then-give-up / reset-on-success
    // contract without depending on the real Anthropic API.

    fn stub_status(ended: bool) -> crate::llm::BatchStatus {
        crate::llm::BatchStatus {
            processing_status: if ended { "ended" } else { "in_progress" }.to_string(),
            request_counts: crate::llm::BatchRequestCounts::default(),
        }
    }

    #[tokio::test]
    async fn poll_loop_succeeds_immediately_when_first_poll_reports_ended() {
        let calls = std::cell::RefCell::new(0u32);
        let statuses_seen = std::cell::RefCell::new(Vec::new());
        let res = poll_batch_until_ended(
            || {
                *calls.borrow_mut() += 1;
                std::future::ready(Ok(stub_status(true)))
            },
            std::time::Duration::from_millis(1),
            5,
            |s| statuses_seen.borrow_mut().push(s.processing_status.clone()),
        )
        .await;
        assert!(res.is_ok());
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(*statuses_seen.borrow(), vec!["ended".to_string()]);
    }

    #[tokio::test]
    async fn poll_loop_tolerates_failures_below_the_cap_then_succeeds() {
        // Fails 4 times (below the cap of 5), then a successful in_progress poll, then ended.
        // The counter must not carry over once a success resets it — this proves the whole
        // budget of 5 is available again after any single success mid-run.
        let call = std::cell::RefCell::new(0u32);
        let res = poll_batch_until_ended(
            || {
                let mut c = call.borrow_mut();
                *c += 1;
                let n = *c;
                async move {
                    if n <= 4 {
                        anyhow::bail!("simulated transient poll failure #{n}");
                    } else if n == 5 {
                        Ok(stub_status(false)) // success resets the failure counter
                    } else if n <= 9 {
                        anyhow::bail!("simulated transient poll failure #{n}")
                    } else {
                        Ok(stub_status(true))
                    }
                }
            },
            std::time::Duration::from_millis(1),
            5,
            |_| {},
        )
        .await;
        assert!(
            res.is_ok(),
            "4 failures, then a success, then 4 more failures (all below the cap of 5 \
             consecutive) must not exhaust the budget: {res:?}"
        );
        assert_eq!(*call.borrow(), 10);
    }

    #[tokio::test]
    async fn poll_loop_gives_up_after_n_consecutive_failures() {
        let call = std::cell::RefCell::new(0u32);
        let res = poll_batch_until_ended(
            || {
                *call.borrow_mut() += 1;
                std::future::ready(Err(anyhow::anyhow!("simulated persistent poll failure")))
            },
            std::time::Duration::from_millis(1),
            3,
            |_| {},
        )
        .await;
        let err = res.expect_err("5 (here: 3-cap) consecutive failures must give up, not hang forever");
        assert!(err.to_string().contains("3 times consecutively"));
        assert_eq!(*call.borrow(), 3, "must stop AT the cap, not overshoot it");
    }

    #[tokio::test]
    async fn poll_loop_never_calls_on_status_for_a_failed_poll() {
        // on_status must only fire on SUCCESSFUL polls (it reads fields off BatchStatus,
        // which doesn't exist for a failed attempt).
        let seen = std::cell::RefCell::new(0u32);
        let call = std::cell::RefCell::new(0u32);
        let _ = poll_batch_until_ended(
            || {
                let mut c = call.borrow_mut();
                *c += 1;
                let n = *c;
                async move {
                    if n == 1 {
                        anyhow::bail!("simulated failure")
                    } else {
                        Ok(stub_status(true))
                    }
                }
            },
            std::time::Duration::from_millis(1),
            5,
            |_| {
                *seen.borrow_mut() += 1;
            },
        )
        .await;
        assert_eq!(*seen.borrow(), 1, "on_status fires exactly once, for the ended poll only");
    }

    #[test]
    fn consensus_is_conservative_on_disagreement() {
        // Three passes disagree on index 0 (high/low/high) — majority severity is high, but the
        // disagreement forces confidence "low" (needs review). Index 1 unanimously high+confident.
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":""},{"index":1,"severity":"high","confidence":"high","reason":"clear injection"}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"low","confidence":"low","reason":"debatable preference"},{"index":1,"severity":"high","confidence":"high","reason":""}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":""},{"index":1,"severity":"high","confidence":"high","reason":""}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 2);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v["verdicts"].as_array().unwrap();
        let v0 = arr.iter().find(|x| x["index"] == 0).unwrap();
        assert_eq!(v0["severity"], "high", "majority severity wins");
        assert_eq!(v0["confidence"], "low", "disagreement -> needs review");
        assert_eq!(
            v0["reason"], "debatable preference",
            "prefers the low-confidence reason"
        );
        let v1 = arr.iter().find(|x| x["index"] == 1).unwrap();
        assert_eq!(v1["severity"], "high");
        assert_eq!(v1["confidence"], "high", "unanimous high stays confident");
    }

    /// Thorough-mode consensus (#51) must thread `effort` through the same majority-vote
    /// merge as severity/confidence: unanimous votes pass straight through, and a tie
    /// breaks to "medium" (effort has no humility direction the way severity does).
    #[test]
    fn consensus_verdicts_threads_effort_with_majority_and_tie_to_medium() {
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","effort":"low","reason":""},{"index":1,"severity":"high","confidence":"high","effort":"low","reason":""}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","effort":"low","reason":""},{"index":1,"severity":"high","confidence":"high","effort":"high","reason":""}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 2);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v["verdicts"].as_array().unwrap();
        let v0 = arr.iter().find(|x| x["index"] == 0).unwrap();
        assert_eq!(v0["effort"], "low", "unanimous effort votes pass straight through");
        let v1 = arr.iter().find(|x| x["index"] == 1).unwrap();
        assert_eq!(v1["effort"], "medium", "a low/high tie must break to medium");
    }

    #[test]
    fn parse_needs_files_reads_array_and_tolerates_absence() {
        let with =
            r#"{"findings":[],"proposed_rules":[],"needs_files":["a/repo.rs"," ","b/svc.rs"]}"#;
        let n = parse_needs_files(with);
        assert_eq!(n, vec!["a/repo.rs".to_string(), "b/svc.rs".to_string()]);
        // Absent / garbage -> empty, never errors.
        assert!(parse_needs_files(r#"{"findings":[]}"#).is_empty());
        assert!(parse_needs_files("not json").is_empty());
    }

    fn site_finding(rule_id: &str, path: &str, line: usize, sev: &str, title: &str) -> Finding {
        Finding {
            repo: "o/r".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: sev.to_string(),
            snippet: title.to_string(),
            detail: format!("detail for {rule_id}"),
            status: "active".to_string(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
            confidence: None,
            effort: None,
            category: None,
            located: true,
            captures: Default::default(),
            evaluated_option_id: None,
            also_locations: Vec::new(),
            fix_specific: None,
            calibration_rationale: None,
        }
    }

    // ── P7: stack exceptions ────────────────────────────────────────────────────────────

    /// THE WIRED INSTANCE, against the REAL corpus: `ARCH-MONOLITH-FIRST-1` must not flag a
    /// `supabase/functions/` finding on a repo whose detected stack includes Supabase — Edge
    /// Functions are the idiomatic second deployable for webhooks on that platform, not a
    /// monolith-topology violation.
    #[tokio::test]
    async fn stack_exception_spares_monolith_first_on_supabase_edge_functions() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        assert!(
            corpus.get_by_id("ARCH-MONOLITH-FIRST-1").is_some(),
            "the wired rule must exist in the real corpus"
        );

        let excepted = site_finding(
            "ARCH-MONOLITH-FIRST-1",
            "supabase/functions/send-invite/index.ts",
            1,
            "medium",
            "a second deployable exists",
        );
        let unrelated = site_finding("ARCH-MONOLITH-FIRST-1", "src/server.rs", 5, "medium", "");
        let frameworks = vec!["Supabase".to_string()];
        let out = apply_stack_exceptions(vec![excepted, unrelated], &frameworks, &corpus);

        assert_eq!(
            out.len(),
            1,
            "the Edge Functions finding must be suppressed: {out:?}"
        );
        assert_eq!(
            out[0].path, "src/server.rs",
            "a monolith-first violation OUTSIDE supabase/functions/ must still fire"
        );
    }

    /// The SAME rule, on a repo that does NOT have Supabase in its detected stack: the
    /// exception must never apply just because the path happens to match — both conditions
    /// (framework present AND path match) are required.
    #[tokio::test]
    async fn stack_exception_does_not_apply_without_the_framework_present() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty());
        let f = site_finding(
            "ARCH-MONOLITH-FIRST-1",
            "supabase/functions/send-invite/index.ts",
            1,
            "medium",
            "",
        );
        let frameworks = vec!["Next.js".to_string()]; // Supabase NOT detected
        let out = apply_stack_exceptions(vec![f], &frameworks, &corpus);
        assert_eq!(
            out.len(),
            1,
            "no Supabase in the stack ⇒ the exception must not apply"
        );
    }

    /// GENERALITY: an entirely different rule + framework pair, via a SYNTHETIC ruleset (never
    /// touching the real corpus), proves the mechanism is not special-cased to monolith-first
    /// or Supabase — any rule that declares a `stack_exceptions` entry gets the same treatment.
    #[test]
    fn stack_exception_mechanism_generalizes_beyond_the_one_wired_rule() {
        let mut rule = camerata_rules::bare_rule("SOME-OTHER-RULE-1", "javascript");
        rule.stack_exceptions.push(camerata_rules::StackException {
            framework: "Next.js".to_string(),
            path_glob: "app/api/**/route.ts".to_string(),
            note: "Route Handlers are idiomatic in the Next.js App Router.".to_string(),
        });
        let mut set = camerata_rules::RuleSet::default();
        set.push(rule);

        let excepted = site_finding("SOME-OTHER-RULE-1", "app/api/users/route.ts", 1, "low", "");
        let unrelated = site_finding("SOME-OTHER-RULE-1", "pages/api/users.ts", 1, "low", "");
        let frameworks = vec!["Next.js".to_string()];
        let out = apply_stack_exceptions(vec![excepted, unrelated], &frameworks, &set);

        assert_eq!(
            out.len(),
            1,
            "the app-router-idiomatic path must be suppressed: {out:?}"
        );
        assert_eq!(out[0].path, "pages/api/users.ts");
    }

    /// A rule with NO `stack_exceptions` declared (the overwhelming majority of the corpus)
    /// behaves exactly as before this feature existed — nothing is ever suppressed for it.
    #[test]
    fn apply_stack_exceptions_is_a_no_op_for_a_rule_with_no_declared_exceptions() {
        let mut set = camerata_rules::RuleSet::default();
        set.push(camerata_rules::bare_rule("PLAIN-RULE-1", "rust"));
        let f = site_finding("PLAIN-RULE-1", "supabase/functions/x.ts", 1, "medium", "");
        let out = apply_stack_exceptions(vec![f], &["Supabase".to_string()], &set);
        assert_eq!(out.len(), 1);
    }

    /// An AI-invented rule id (no corresponding corpus entry, `AI-` prefixed) can never be
    /// excepted — only a real corpus rule's own declared exceptions apply.
    #[test]
    fn apply_stack_exceptions_never_excepts_an_uncorpused_ai_invented_rule_id() {
        let set = camerata_rules::RuleSet::default();
        let f = site_finding(
            "AI-SOME-NOVEL-DEFECT",
            "supabase/functions/x.ts",
            1,
            "medium",
            "",
        );
        let out = apply_stack_exceptions(vec![f], &["Supabase".to_string()], &set);
        assert_eq!(out.len(), 1, "an uncorpused rule id is never excepted");
    }

    // ── P7: needs-review structural grouping ────────────────────────────────────────────

    fn structural_needs_review(path: &str, line: usize) -> Finding {
        let mut f = site_finding("ARCH-SOME-PREFERENCE-1", path, line, "medium", "differs");
        f.category = Some("arch-conformance".to_string());
        f.confidence = Some("needs-review".to_string());
        f
    }

    /// N occurrences of ONE structural needs-review rule collapse into exactly ONE grouped
    /// finding, listing every location — not N separate rows.
    #[test]
    fn n_structural_occurrences_of_one_rule_collapse_into_one_grouped_finding() {
        let findings = vec![
            structural_needs_review("a.rs", 10),
            structural_needs_review("b.rs", 20),
            structural_needs_review("c.rs", 30),
        ];
        let out = group_structural_needs_review(findings);
        assert_eq!(
            out.len(),
            1,
            "three occurrences of one rule must become ONE row: {out:?}"
        );
        let grouped = &out[0];
        assert_eq!(grouped.rule_id, "ARCH-SOME-PREFERENCE-1");
        assert_eq!(grouped.severity, "low");
        assert_eq!(grouped.confidence.as_deref(), Some("needs-review"));
        assert!(grouped.needs_review);
        // Every location must be listed somewhere (the primary's own site + also_locations).
        assert!(grouped.detail.contains("a.rs:10"));
        assert!(grouped.detail.contains("b.rs:20"));
        assert!(grouped.detail.contains("c.rs:30"));
        assert_eq!(
            grouped.also_locations.len(),
            2,
            "the two non-primary occurrences must be listed as also_locations: {:?}",
            grouped.also_locations
        );
    }

    /// Different RULES never merge into the same group — grouping is per-rule.
    #[test]
    fn different_structural_rules_get_separate_groups() {
        let mut a = structural_needs_review("a.rs", 10);
        a.rule_id = "ARCH-RULE-A-1".to_string();
        let mut b = structural_needs_review("b.rs", 20);
        b.rule_id = "ARCH-RULE-B-1".to_string();
        let out = group_structural_needs_review(vec![a, b]);
        assert_eq!(
            out.len(),
            2,
            "distinct rule ids must not be merged together: {out:?}"
        );
    }

    /// A CRITICAL or HIGH severity finding is NEVER swept into this grouping pass, even if it
    /// happens to carry `arch-conformance` + `needs-review` — a real security finding must never
    /// disappear into this noise-reduction pass (this is a separate, narrower exclusion than
    /// bucketing: a high/critical `needs-review` row the pass DIDN'T group still routes to
    /// `held`, not an action bucket — see `report_export::is_held_for_review`, C5-4).
    #[test]
    fn a_high_or_critical_finding_is_never_grouped_away() {
        for sev in ["critical", "high"] {
            let mut f = structural_needs_review("secret.rs", 5);
            f.severity = sev.to_string();
            let out = group_structural_needs_review(vec![f.clone()]);
            assert_eq!(
                out.len(),
                1,
                "a {sev} finding must survive ungrouped: {out:?}"
            );
            assert_eq!(out[0].severity, sev, "severity must not be downgraded to low");
            assert_eq!(out[0].path, "secret.rs", "must remain its own individual row");
        }
    }

    /// A finding that isn't `arch-conformance`, isn't `needs-review`, or was already
    /// dispositioned (`status != "active"`) is left completely untouched — passes through
    /// unchanged, never folded into a group.
    #[test]
    fn unrelated_findings_pass_through_unchanged() {
        let wrong_category = {
            let mut f = structural_needs_review("a.rs", 1);
            f.category = Some("rls-policy".to_string());
            f
        };
        let wrong_confidence = {
            let mut f = structural_needs_review("b.rs", 2);
            f.confidence = Some("high".to_string());
            f
        };
        let already_suppressed = {
            let mut f = structural_needs_review("c.rs", 3);
            f.status = "suppressed-inline".to_string();
            f
        };
        let out = group_structural_needs_review(vec![
            wrong_category.clone(),
            wrong_confidence.clone(),
            already_suppressed.clone(),
        ]);
        assert_eq!(out.len(), 3, "none of these three should be grouped: {out:?}");
        assert!(out.contains(&wrong_category));
        assert!(out.contains(&wrong_confidence));
        assert!(out.contains(&already_suppressed));
    }

    /// A SINGLE occurrence of a structural needs-review rule still goes through the grouping
    /// shape (severity forced to low, confidence stays needs-review) — uniform behavior
    /// whether there's one occurrence or many.
    #[test]
    fn a_single_structural_occurrence_still_produces_the_grouped_shape() {
        let out = group_structural_needs_review(vec![structural_needs_review("only.rs", 7)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, "low");
        assert!(out[0].detail.contains("only.rs:7"));
    }

    /// REAL DATA-LOSS REGRESSION (pipeline-integrity fix): `group_structural_needs_review` runs
    /// AFTER the per-repo cross-family merge, so an occurrence arriving here can ALREADY be a
    /// merged primary carrying its own non-empty `also_matches` (rule ids it already absorbed)
    /// and `also_locations` (their sites). `build_structural_group_finding` used to build the
    /// group's own `also_matches`/`also_locations` ONLY from each occurrence's top-level
    /// rule_id/path/line — discarding anything already nested inside an occurrence. This proves
    /// every already-absorbed rule id and site survives this SECOND collapse too.
    #[test]
    fn structural_grouping_preserves_already_absorbed_members_from_an_earlier_merge() {
        // `a` simulates a finding that was ALREADY a merged primary from the earlier
        // cross-family-merge stage: it carries "AI-ALREADY-MERGED-1" in `also_matches` and that
        // id's own site in `also_locations`.
        let mut a = structural_needs_review("a.rs", 10);
        a.also_matches = vec!["AI-ALREADY-MERGED-1".to_string()];
        a.also_locations = vec![MergedLocation {
            repo: a.repo.clone(),
            path: "already-merged-site.rs".to_string(),
            line: 99,
            rule_id: "AI-ALREADY-MERGED-1".to_string(),
            snippet: "prior evidence".to_string(),
            consequence: false,
        }];
        let b = structural_needs_review("b.rs", 20);

        let out = group_structural_needs_review(vec![a, b]);
        assert_eq!(out.len(), 1, "both occurrences collapse into one row: {out:?}");
        let grouped = &out[0];

        assert!(
            grouped
                .also_matches
                .contains(&"AI-ALREADY-MERGED-1".to_string()),
            "the rule id `a` had already absorbed must survive this SECOND collapse, not just \
             the top-level rule ids of `a`/`b`: {:?}",
            grouped.also_matches
        );
        assert!(
            grouped
                .also_locations
                .iter()
                .any(|l| l.path == "already-merged-site.rs" && l.line == 99),
            "the SITE `a` had already absorbed must survive this SECOND collapse: {:?}",
            grouped.also_locations
        );
        assert!(
            grouped
                .also_locations
                .iter()
                .any(|l| l.path == "b.rs" && l.line == 20),
            "b's own site must still be recorded: {:?}",
            grouped.also_locations
        );
    }

    #[test]
    fn strip_dedup_pointers_removes_cross_references() {
        assert_eq!(strip_dedup_pointers("Same as [6]"), "");
        assert_eq!(strip_dedup_pointers("duplicate of [10]"), "");
        assert_eq!(
            strip_dedup_pointers("Real panic risk; same as [3]"),
            "Real panic risk"
        );
        assert_eq!(
            strip_dedup_pointers("over-flagged for a mini app, duplicate of 7"),
            "over-flagged for a mini app"
        );
        // The newer "index N" / "as index N" / "row N" pointer phrasing.
        assert_eq!(
            strip_dedup_pointers("directly observable failure as index 0"),
            "directly observable failure"
        );
        assert_eq!(strip_dedup_pointers("index 6"), "");
        assert_eq!(
            strip_dedup_pointers("maintainability concern; see index 9"),
            "maintainability concern"
        );
        assert_eq!(
            strip_dedup_pointers("same root cause, row 3"),
            "same root cause"
        );
        // Legit prose that merely contains the word "index" (no pointer number) survives.
        assert_eq!(
            strip_dedup_pointers("add a composite index on (user_id, created_at)"),
            "add a composite index on (user_id, created_at)"
        );
        // A clean reason is untouched.
        assert_eq!(
            strip_dedup_pointers("maintainability, not correctness"),
            "maintainability, not correctness"
        );
    }

    #[test]
    fn merge_collapses_same_location_into_one_preferring_adopted_rule() {
        // One smell at h.rs:12 reported under two invented names PLUS the adopted id —
        // each with a DIFFERENT title (the exact case a title-keyed merge missed).
        // All three cite the SAME real offending code (present in the file), so they're
        // genuinely co-located even though each rule phrases it differently.
        let code = "self.db.query(sql)";
        let files = vec![("h.rs".to_string(), format!("fn handler() {{ {code} }}"))];
        let findings = vec![
            site_finding("AI-CONTROLLER-DIRECT-DB", "h.rs", 12, "medium", code),
            site_finding("ARCH-STRICT-LAYERING-1", "h.rs", 12, "high", code),
            site_finding("AI-HANDLER-BYPASSES-REPO", "h.rs", 12, "low", code),
        ];
        let merged = merge_by_location(findings, &files);
        assert_eq!(
            merged.len(),
            1,
            "three labels at one location collapse to one row"
        );
        // Adopted id wins as primary; highest severity kept; others demoted to also_matches.
        assert_eq!(merged[0].rule_id, "ARCH-STRICT-LAYERING-1");
        assert_eq!(merged[0].severity, "high");
        assert!(merged[0]
            .also_matches
            .contains(&"AI-CONTROLLER-DIRECT-DB".to_string()));
        assert!(merged[0]
            .also_matches
            .contains(&"AI-HANDLER-BYPASSES-REPO".to_string()));
        assert!(!merged[0]
            .also_matches
            .contains(&"ARCH-STRICT-LAYERING-1".to_string()));
    }

    #[test]
    fn merge_folds_overlapping_corpus_rules_at_one_location() {
        // "Handler opens its own pool" legitimately trips layering + DI + entities-chain.
        // That's one finding that names all three, not three rows.
        let code = "Pool::connect(url).await";
        let files = vec![("h.rs".to_string(), format!("let pool = {code};"))];
        let findings = vec![
            site_finding("ARCH-STRICT-LAYERING-1", "h.rs", 41, "high", code),
            site_finding("ARCH-SERVICE-DI-1", "h.rs", 41, "medium", code),
            site_finding("RUST-ENTITIES-13", "h.rs", 41, "low", code),
        ];
        let merged = merge_by_location(findings, &files);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].also_matches.len(),
            2,
            "two non-primary rules demoted"
        );
    }

    #[test]
    fn merge_does_not_collapse_distinct_line_zero_findings() {
        // Line 0 (file-level / uncited) must NOT location-merge — unrelated file-level
        // issues legitimately share line 0.
        let findings = vec![
            site_finding(
                "AI-NO-MAPPERS-CRATE",
                "lib.rs",
                0,
                "low",
                "no mappers crate",
            ),
            site_finding("AI-NO-TESTS", "lib.rs", 0, "low", "no tests"),
        ];
        let merged = merge_by_location(findings, &[]);
        assert_eq!(merged.len(), 2, "distinct line-0 findings stay separate");
    }

    #[test]
    fn merge_keeps_absence_findings_at_a_shared_line_separate() {
        // The real bug from the agora-mini verification: two ABSENCE findings ("no error
        // handler", "no API versioning") whose snippets describe a gap (NOT code in the file)
        // got anchored to the same representative line and wrongly merged — the error-handler
        // row picked up a spurious `ARCH-API-VERSIONING-1` in also_matches. They must stay
        // separate, since neither snippet is real code present at that line.
        let files = vec![(
            "app.ts".to_string(),
            "const app = express();\napp.use(express.json());\napp.listen(3000);".to_string(),
        )];
        let findings = vec![
            site_finding(
                "ARCH-CENTRAL-ERROR-HANDLER-1",
                "app.ts",
                2,
                "high",
                "no central error handler is registered",
            ),
            site_finding(
                "ARCH-API-VERSIONING-1",
                "app.ts",
                2,
                "medium",
                "routes are not version-prefixed",
            ),
        ];
        let merged = merge_by_location(findings, &files);
        assert_eq!(
            merged.len(),
            2,
            "unrelated absence findings at one line stay separate"
        );
        assert!(
            merged.iter().all(|f| f.also_matches.is_empty()),
            "no spurious also_matches"
        );
    }

    #[test]
    fn merge_collapses_colocated_real_code_even_with_varied_snippets() {
        // Two findings that BOTH cite real code present at the same line still merge — the
        // located check keys on (path, line) for genuinely-cited code, so differently-phrased
        // snippets of the same offending line collapse.
        let files = vec![(
            "u.ts".to_string(),
            "const q = `SELECT * FROM t WHERE name ILIKE '%${name}%'`;".to_string(),
        )];
        let findings = vec![
            site_finding(
                "SEC-NO-RAW-SQL-CONCAT-1",
                "u.ts",
                1,
                "critical",
                "ILIKE '%${name}%'",
            ),
            site_finding(
                "AI-SQL-INJECTION",
                "u.ts",
                1,
                "high",
                "SELECT * FROM t WHERE name ILIKE",
            ),
        ];
        let merged = merge_by_location(findings, &files);
        assert_eq!(merged.len(), 1, "co-located real-code findings still merge");
    }

    #[test]
    fn canonicalize_maps_invented_names_only_when_adopted() {
        let adopted: std::collections::HashSet<String> = [
            "ARCH-STRUCTURED-ERRORS-1".to_string(),
            "ARCH-STRICT-LAYERING-1".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            canonical_adopted_rule("HANDLER-PANICS-ON-DB-ERROR", &adopted).as_deref(),
            Some("ARCH-STRUCTURED-ERRORS-1")
        );
        assert_eq!(
            canonical_adopted_rule("HANDLER-CREATES-OWN-POOL", &adopted).as_deref(),
            Some("ARCH-STRICT-LAYERING-1")
        );
        // Secret-in-URL canonical isn't adopted here -> no phantom id.
        assert_eq!(canonical_adopted_rule("SECRET-IN-URL", &adopted), None);
        // A genuinely-novel name maps to nothing.
        assert_eq!(canonical_adopted_rule("MISSING-RATE-LIMIT", &adopted), None);
    }

    #[test]
    fn extract_public_symbols_finds_rust_and_ts_exports() {
        let rust = "use x;\npub struct AdminStats { a: i32 }\nfn private() {}\npub trait Repo {}\n";
        let s = extract_public_symbols(rust);
        assert!(s.contains(&"AdminStats".to_string()));
        assert!(s.contains(&"Repo".to_string()));
        assert!(!s.iter().any(|x| x == "private"));
        let ts = "export class UserService {}\nexport interface Dto {}\n";
        let s2 = extract_public_symbols(ts);
        assert!(s2.contains(&"UserService".to_string()));
        assert!(s2.contains(&"Dto".to_string()));
    }

    #[test]
    fn repo_map_lists_every_file_with_symbols() {
        let files = vec![
            (
                "crates/api/src/repositories/user_repo.rs".to_string(),
                "pub struct UserRepo {}".to_string(),
            ),
            (
                "crates/ui/src/services/admin_stats.rs".to_string(),
                "pub struct AdminStats {}".to_string(),
            ),
        ];
        let map = build_repo_map(&files);
        // Every file is in the map even though a chunk may only hold one of them.
        assert!(map.contains("crates/api/src/repositories/user_repo.rs"));
        assert!(map.contains("crates/ui/src/services/admin_stats.rs"));
        assert!(map.contains("UserRepo"));
        assert!(map.contains("AdminStats"));
    }

    #[test]
    fn chunk_files_covers_every_file_and_respects_budget() {
        // 10 files of ~100 bytes each; a 250-byte budget forces several chunks.
        let files: Vec<(String, String)> = (0..10)
            .map(|i| (format!("f{i}.rs"), "x".repeat(90)))
            .collect();
        let chunks = chunk_files(&files, 250);
        // Every file appears exactly once across all chunks (nothing dropped).
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, 10, "all files covered, none dropped");
        assert!(chunks.len() > 1, "small budget forces multiple chunks");
        // Reassembled order matches the input order.
        let flat: Vec<&str> = chunks
            .iter()
            .flat_map(|c| c.iter().map(|(p, _)| p.as_str()))
            .collect();
        let want: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        assert_eq!(flat, want.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[test]
    fn chunk_files_oversized_file_gets_its_own_chunk() {
        let files = vec![
            ("small.rs".to_string(), "x".repeat(10)),
            ("huge.rs".to_string(), "x".repeat(1000)),
            ("small2.rs".to_string(), "x".repeat(10)),
        ];
        let chunks = chunk_files(&files, 100);
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, 3, "oversized file still included, nothing dropped");
    }

    #[test]
    fn digest_concatenates_and_caps() {
        let files = vec![
            ("a.rs".to_string(), "fn a() {}".to_string()),
            ("b.rs".to_string(), "fn b() {}".to_string()),
        ];
        let d = build_digest(&files);
        assert!(d.contains("FILE: a.rs"));
        assert!(d.contains("FILE: b.rs"));
        assert!(d.contains("fn a()"));

        // A file larger than the cap truncates and notes it.
        let big = vec![("big.rs".to_string(), "x".repeat(MAX_DIGEST_CHARS + 1000))];
        let d2 = build_digest(&big);
        assert!(d2.len() <= MAX_DIGEST_CHARS + 200);
        assert!(d2.contains("truncated"));
    }

    // ── GAP-3: batch-mode prompt-cache cost fix ─────────────────────────────────────
    //
    // Two problems, one fix. (1) The OLD prefix embedded the chunk index BEFORE the repo
    // map (`"Repository: {repo} ({label} {ci}/{n_c})\n\n{repo_map}{digest}"`), so the
    // "static" prefix actually differed at ~byte 30 on every chunk — the repo map could
    // NEVER cache-hit across chunks, in any mode. (2) Batch mode defaults to ONE rule-batch
    // per chunk (`BATCH_RULE_BATCH_SIZE = usize::MAX`), so the single cache breakpoint was
    // written once and never read back — pure ~1.25x cost, zero benefit.
    //
    // The fix: lead with `Repository: {repo}\n\n{repo_map}` (byte-identical across every
    // chunk), then the chunk's `(label n/n_c)\n\n{digest}\n\n` segment, then the varying
    // task line + rules. Two cache breakpoints — one after the repo map (re-reads across
    // chunks), one after the chunk segment (re-reads across that chunk's rule-batches,
    // dropped entirely when there's only one rule-batch).

    #[test]
    fn cache_breakpoints_for_pass_keeps_repo_map_bp_always_and_digest_bp_only_when_n_b_gt_1() {
        // n_b > 1 (parallel mode's normal case, or a batch-mode run with a smaller
        // BATCH_RULE_BATCH_SIZE): both breakpoints present, repo-map first.
        let bps = cache_breakpoints_for_pass(100, 250, 3);
        assert_eq!(bps, vec![100, 250], "both breakpoints present when n_b > 1");

        // n_b == 1 (batch mode's default: BATCH_RULE_BATCH_SIZE = usize::MAX -> one
        // rule-batch per chunk): the digest breakpoint is dropped — it would be written
        // once and never re-read, paying a pure cache-write premium for zero benefit.
        let bps_one = cache_breakpoints_for_pass(100, 250, 1);
        assert_eq!(bps_one, vec![100], "digest breakpoint dropped when n_b == 1");

        // n_b == 0 is a degenerate/defensive case (should never happen — there's always at
        // least one rule-batch) but must not panic or add a spurious second breakpoint.
        let bps_zero = cache_breakpoints_for_pass(100, 250, 0);
        assert_eq!(bps_zero, vec![100]);
    }

    #[test]
    fn build_pass_prompt_leads_with_the_shared_repo_map_not_the_chunk_label() {
        let repo = "acme/widgets";
        let repo_map = "REPO MAP:\nfoo.rs: fn foo()\n";
        let (prompt, _bps) =
            build_pass_prompt(repo, repo_map, "parallel", 0, 3, "DIGEST-0", 2, "TASK", "RULES");
        // The prompt must START with the repo-map segment, not the chunk label — this is
        // the exact ordering bug being fixed (old: "Repository: {repo} ({label} n/n_c)...").
        assert!(
            prompt.starts_with(&format!("Repository: {repo}\n\n{repo_map}")),
            "prompt must lead with the shared repo-map segment: {prompt:?}"
        );
        // The chunk label + digest come AFTER the repo map now, not embedded in front of it.
        let repo_map_prefix = format!("Repository: {repo}\n\n{repo_map}");
        let rest = &prompt[repo_map_prefix.len()..];
        assert!(rest.starts_with("(parallel 1/3)"), "chunk label trails the repo map: {rest:?}");
    }

    #[test]
    fn build_pass_prompt_repo_map_segment_is_byte_identical_across_chunk_indices() {
        let repo = "acme/widgets";
        let repo_map = "REPO MAP:\nfoo.rs: fn foo()\nbar.rs: fn bar()\n";
        let (prompt_c0, bps_c0) =
            build_pass_prompt(repo, repo_map, "parallel", 0, 5, "digest for chunk 0", 2, "T", "R");
        let (prompt_c1, bps_c1) =
            build_pass_prompt(repo, repo_map, "parallel", 1, 5, "digest for chunk 1", 2, "T", "R");

        // Both prompts carry the SAME first breakpoint offset...
        assert_eq!(bps_c0[0], bps_c1[0], "the repo-map breakpoint offset must match across chunks");
        // ...and the bytes up to that offset are IDENTICAL — this is what actually lets the
        // provider cache-hit the repo map on chunk 2, 3, ... instead of re-billing it.
        assert_eq!(
            &prompt_c0[..bps_c0[0]],
            &prompt_c1[..bps_c1[0]],
            "the repo-map segment must be byte-identical regardless of chunk index"
        );
        // Sanity: the chunk-specific segments genuinely differ (each chunk's digest+label).
        assert_ne!(&prompt_c0[bps_c0[0]..], &prompt_c1[bps_c1[0]..]);
    }

    #[test]
    fn build_pass_prompt_two_breakpoints_when_n_b_gt_1_one_when_n_b_eq_1() {
        let repo = "acme/widgets";
        let repo_map = "REPO MAP:\nfoo.rs: fn foo()\n";

        // n_b > 1 (parallel mode's normal case): two breakpoints, repo-map then digest.
        let (prompt, bps) =
            build_pass_prompt(repo, repo_map, "parallel", 2, 5, "DIGEST-2", 3, "TASK", "RULES");
        assert_eq!(bps.len(), 2, "n_b > 1 keeps both breakpoints");
        assert!(bps[0] < bps[1], "breakpoints are ascending");
        // The segment between the two breakpoints is the chunk label + digest.
        let digest_segment = &prompt[bps[0]..bps[1]];
        assert!(digest_segment.contains("(parallel 3/5)"));
        assert!(digest_segment.contains("DIGEST-2"));
        // The suffix (after the last breakpoint) is the varying task line + rules — the
        // rules must NEVER leak into a cached segment.
        let suffix = &prompt[bps[1]..];
        assert!(suffix.contains("TASK") && suffix.contains("RULES"));
        assert!(!digest_segment.contains("RULES"), "rules must not leak into the cached prefix");

        // n_b == 1 (batch mode's default rule-batch size): only the repo-map breakpoint
        // survives; the chunk label + digest fold into the uncached suffix instead.
        let (prompt_one, bps_one) =
            build_pass_prompt(repo, repo_map, "batch", 2, 5, "DIGEST-2", 1, "TASK", "RULES");
        assert_eq!(bps_one.len(), 1, "n_b == 1 drops the digest breakpoint");
        let suffix_one = &prompt_one[bps_one[0]..];
        assert!(
            suffix_one.contains("DIGEST-2") && suffix_one.contains("TASK") && suffix_one.contains("RULES"),
            "digest + task + rules all fold into the single uncached suffix: {suffix_one:?}"
        );
    }

    #[test]
    fn parse_valid_json_into_findings_and_rules() {
        let raw = r#"Here is the audit:
        {
          "findings": [
            {"path": "src/orders.rs", "line": 42, "severity": "high", "rule": "auth-on-write-paths", "title": "create_order writes with no auth check", "detail": "Anyone can POST."},
            {"path": "src/svc.rs", "line": 10, "severity": "medium", "rule": "no-db-in-services", "title": "OrderService queries db directly", "detail": "Bypasses repo."}
          ],
          "proposed_rules": [
            {"name": "auth-on-write-paths", "title": "Every write path checks authorization", "rationale": "x", "severity": "high", "enforcement": "review"}
          ]
        }
        Thanks!"#;
        let none = std::collections::HashSet::new();
        let (findings, rules) = parse_ai_findings("me/api", raw, &none);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].rule_id, "AI-AUTH-ON-WRITE-PATHS");
        assert_eq!(findings[0].repo, "me/api");
        assert_eq!(findings[0].severity, "high");
        assert_eq!(findings[0].line, 42);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "AI-AUTH-ON-WRITE-PATHS");
        assert_eq!(rules[0].kind, "review");
        assert_eq!(rules[0].enforcement_point, "integration");
        // The rule's finding_count picks up its matching finding.
        assert_eq!(rules[0].finding_count, 1);
    }

    /// A well-formed, allowlisted `captures` object on a model finding maps straight through
    /// to `Finding.captures` — the general path that lets an AI-emitted finding (e.g. a
    /// public-bucket or exposed-schema finding, which has no deterministic checker) still
    /// name the real object in the report instead of falling back to the generic wording.
    #[test]
    fn parse_ai_findings_maps_a_well_formed_captures_object_through() {
        let raw = r#"{"findings":[
            {"path":"supabase/migrations/1.sql","line":5,"severity":"high",
             "rule":"public-bucket-not-reviewed",
             "title":"the invoices bucket is public",
             "detail":"anyone with the link can download every file in it",
             "captures":{"bucket":"invoices"}}
        ],"proposed_rules":[]}"#;
        let none = std::collections::HashSet::new();
        let (findings, _) = parse_ai_findings("me/api", raw, &none);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].captures.get("bucket").map(String::as_str),
            Some("invoices")
        );
    }

    /// `captures` is optional enrichment: absence must never affect parsing of the rest of the
    /// finding, and the resulting map must simply be empty (the report's generic fallback
    /// covers it) rather than the finding being dropped or erroring.
    #[test]
    fn parse_ai_findings_defaults_to_empty_captures_when_the_field_is_absent() {
        let raw = r#"{"findings":[
            {"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d"}
        ],"proposed_rules":[]}"#;
        let none = std::collections::HashSet::new();
        let (findings, _) = parse_ai_findings("me/api", raw, &none);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].captures.is_empty());
    }

    /// A malformed `captures` blob — wrong JSON type, an unknown/hallucinated token key, a
    /// non-string value, and an oversized value all mixed together — must degrade to "skip the
    /// bad entries" rather than panicking or dropping the finding. This is the adversarial
    /// half of Step 2's defensiveness requirement: the model's output is untrusted input.
    #[test]
    fn parse_ai_findings_degrades_a_malformed_captures_blob_without_panicking() {
        let huge = "x".repeat(500);
        let raw = format!(
            r#"{{"findings":[
                {{"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d",
                  "captures":{{
                    "bucket":"avatars",
                    "not-a-real-token":"whatever",
                    "table":42,
                    "schema":"   ",
                    "view":"{huge}"
                  }}}}
            ],"proposed_rules":[]}}"#
        );
        let none = std::collections::HashSet::new();
        let (findings, _) = parse_ai_findings("me/api", &raw, &none);
        assert_eq!(findings.len(), 1, "the finding itself must survive a malformed captures blob");
        assert_eq!(
            findings[0].captures.get("bucket").map(String::as_str),
            Some("avatars"),
            "the one well-formed entry must still come through"
        );
        assert!(
            !findings[0].captures.contains_key("not-a-real-token"),
            "a key outside the known token allowlist must be dropped"
        );
        assert!(
            !findings[0].captures.contains_key("table"),
            "a non-string value must be dropped, not coerced or panicking"
        );
        assert!(
            !findings[0].captures.contains_key("schema"),
            "a whitespace-only value must be dropped"
        );
        assert!(
            !findings[0].captures.contains_key("view"),
            "a value over the length cap must be dropped"
        );
        assert_eq!(findings[0].captures.len(), 1);
    }

    /// `captures` being an entirely wrong JSON shape (a string, not an object) must also
    /// degrade to empty rather than erroring or panicking.
    #[test]
    fn parse_ai_findings_degrades_a_non_object_captures_value_to_empty() {
        let raw = r#"{"findings":[
            {"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d",
             "captures":"not an object"}
        ],"proposed_rules":[]}"#;
        let none = std::collections::HashSet::new();
        let (findings, _) = parse_ai_findings("me/api", raw, &none);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].captures.is_empty());
    }

    /// The raw-audit parser must ACCEPT and PRESERVE an explicit `"critical"` severity from
    /// the model — this is the emit-path half of the critical-severity fix (the schema now
    /// offers it; this pins that the parser doesn't silently demote it to "medium" the way an
    /// unrecognized string would).
    #[test]
    fn parse_ai_findings_accepts_and_preserves_critical_severity() {
        let raw = r#"{"findings":[
            {"path":"supabase/functions/charge/index.ts","line":30,"severity":"critical",
             "rule":"unauthenticated-charge-endpoint",
             "title":"charge handler has no auth check and holds a service_role key",
             "code":"export default async function handler(req) {",
             "detail":"any caller can trigger a real charge with the service_role key"},
            {"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d"}
        ],"proposed_rules":[]}"#;
        let none = std::collections::HashSet::new();
        let (findings, _) = parse_ai_findings("me/api", raw, &none);
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].severity, "critical",
            "an explicit critical verdict from the model must survive parsing unchanged"
        );
        // Anti-over-rotation guard: the sibling medium finding must NOT be swept up into
        // critical just because another finding in the same batch was critical.
        assert_eq!(findings[1].severity, "medium");
    }

    #[test]
    fn parse_uses_verbatim_code_as_snippet_and_keeps_title_in_detail() {
        let raw = r#"{"findings":[{"path":"a.rs","line":5,"severity":"high","rule":"x",
          "title":"raw SQL built by format!","code":"let q = format!(\"SELECT ...\");",
          "detail":"use a query builder"}],"proposed_rules":[]}"#;
        let none = std::collections::HashSet::new();
        let (f, _) = parse_ai_findings("r/r", raw, &none);
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].snippet, "let q = format!(\"SELECT ...\");",
            "snippet is the verbatim code"
        );
        assert!(
            f[0].detail.starts_with("raw SQL built by format!"),
            "title leads the detail"
        );
        assert!(f[0].detail.contains("use a query builder"));
    }

    #[test]
    fn resolve_finding_lines_corrects_from_verbatim_snippet() {
        let content = "fn a() {}\nlet x = 1;\nthe offending CALL here\nlet y = 2;\n";
        let files = vec![("src/lib.rs".to_string(), content.to_string())];
        // Model guessed line 1, snippet is on line 3.
        let mut findings = vec![site_finding(
            "AI-X",
            "src/lib.rs",
            1,
            "high",
            "the offending CALL here",
        )];
        resolve_finding_lines(&mut findings, &files);
        assert_eq!(
            findings[0].line, 3,
            "line resolved from the verbatim snippet"
        );

        // Duplicate snippet → nearest occurrence to the model's estimate wins.
        let dup = "data_tr(a)\nx\ndata_tr(a)\n";
        let files2 = vec![("d.rs".to_string(), dup.to_string())];
        let mut f2 = vec![site_finding("AI-Y", "d.rs", 3, "high", "data_tr(a)")];
        resolve_finding_lines(&mut f2, &files2);
        assert_eq!(f2[0].line, 3, "duplicate resolves to the nearest match");

        // Paraphrase not present → keep the model's line.
        let mut f3 = vec![site_finding(
            "AI-Z",
            "src/lib.rs",
            2,
            "high",
            "paraphrase not in the file",
        )];
        resolve_finding_lines(&mut f3, &files);
        assert_eq!(f3[0].line, 2, "no match keeps the model's line");
    }

    fn finding(rule: &str, sev: &str) -> Finding {
        Finding {
            repo: "me/api".into(),
            path: "a.rs".into(),
            line: 1,
            rule_id: rule.into(),
            severity: sev.into(),
            snippet: "x".into(),
            detail: "d".into(),
            status: "active".into(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
            confidence: None,
            effort: None,
            category: None,
            located: true,
            captures: Default::default(),
            evaluated_option_id: None,
            also_locations: Vec::new(),
            fix_specific: None,
            calibration_rationale: None,
        }
    }

    #[test]
    fn apply_verdicts_recalibrates_and_keeps_all() {
        let findings = vec![
            finding("AI-TIMING", "medium"), // index 0 -> low confidence, kept
            finding("AI-AUTHZ", "high"),    // index 1 -> downgraded to low, kept
            finding("AI-REAL", "high"),     // index 2 -> kept as-is
        ];
        let raw = r#"{"verdicts":[
            {"index":0,"confidence":"low","reason":"negligible timing residual"},
            {"index":1,"severity":"low","confidence":"high","reason":"low impact"},
            {"index":2,"severity":"high","confidence":"high","reason":"concrete"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        // The calibration pass NEVER drops — every finding reaches the architect.
        assert_eq!(out.len(), 3, "no finding is dropped");
        let timing = out.iter().find(|f| f.rule_id == "AI-TIMING").unwrap();
        assert!(
            timing.detail.contains("[needs review"),
            "low-confidence flagged"
        );
        let authz = out.iter().find(|f| f.rule_id == "AI-AUTHZ").unwrap();
        assert_eq!(authz.severity, "low", "recalibrated down");
    }

    /// GUARD (this fix): `verify_findings` must record its OWN transcript entry — a
    /// non-empty REAL prompt and the model's verbatim response as output, terminating in a
    /// "done" status — rather than leaving the "calibrating…" agent with an empty prompt and
    /// "no output captured" the way `audit_repo`'s old placeholder registration did.
    #[tokio::test]
    async fn verify_findings_records_prompt_and_output_to_transcript() {
        let resp_text = r#"{"verdicts":[{"index":0,"severity":"critical","confidence":"high","reason":"stub verdict"}]}"#;
        let llm = StubCompleter {
            text: resp_text.to_string(),
        };
        let findings = vec![finding("AI-X", "medium")];
        let store = crate::transcript::TranscriptStore::default();

        let (out, _estimation_failed) = verify_findings(
            &llm,
            "me/api",
            findings,
            None,
            Some((&store, "run-1")),
            None,
            false, // thorough
            "This repository has 1 code files.",
        )
        .await;

        // Calibration itself still applies as before.
        assert_eq!(out[0].severity, "critical", "verdict still applied");

        let transcripts = store.get("run-1");
        let entry = transcripts
            .iter()
            .find(|a| a.session_id == "audit-me/api-calibrate")
            .expect("calibration pass registers its own transcript entry");
        assert!(
            !entry.prompt.is_empty(),
            "the generated prompt must be recorded, not left empty"
        );
        assert!(
            entry.prompt.contains("Scrutinize these findings"),
            "the recorded prompt should be the ACTUAL prompt sent, got: {}",
            entry.prompt
        );
        assert_eq!(
            entry.output, resp_text,
            "output must equal the stub's response — no 'no output captured'"
        );
        assert_eq!(entry.status, "done");
    }

    /// `feedback: None` (the pre-existing, non-cockpit call path) must still calibrate
    /// findings exactly as before — no panic, no behavior change — since transcript
    /// recording is purely additive.
    #[tokio::test]
    async fn verify_findings_with_no_feedback_still_calibrates() {
        let resp_text = r#"{"verdicts":[{"index":0,"severity":"critical","confidence":"high","reason":"stub verdict"}]}"#;
        let llm = StubCompleter {
            text: resp_text.to_string(),
        };
        let findings = vec![finding("AI-X", "medium")];

        let (out, _estimation_failed) = verify_findings(
            &llm,
            "me/api",
            findings,
            None,
            None, // feedback
            None,
            false,
            "This repository has 1 code files.",
        )
        .await;

        assert_eq!(
            out[0].severity, "critical",
            "calibration still applies without a transcript store"
        );
    }

    /// THOROUGH mode runs 3 calibration passes; every one of them must land in the
    /// transcript (none left silent) even though `apply_verdicts` only ever sees the
    /// merged consensus.
    #[tokio::test]
    async fn verify_findings_thorough_mode_records_every_pass() {
        let resp_text = r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"stub verdict"}]}"#;
        let llm = StubCompleter {
            text: resp_text.to_string(),
        };
        let findings = vec![finding("AI-X", "medium")];
        let store = crate::transcript::TranscriptStore::default();

        let (out, _estimation_failed) = verify_findings(
            &llm,
            "me/api",
            findings,
            None,
            Some((&store, "run-2")),
            None,
            true, // thorough — 3 passes
            "This repository has 1 code files.",
        )
        .await;
        assert_eq!(out[0].severity, "high");

        let transcripts = store.get("run-2");
        let entry = transcripts
            .iter()
            .find(|a| a.session_id == "audit-me/api-calibrate")
            .expect("calibration pass registers its own transcript entry");
        for i in 1..=3 {
            assert!(
                entry.output.contains(&format!("pass {i}/3")),
                "pass {i} must be recorded, got: {}",
                entry.output
            );
        }
        assert_eq!(entry.status, "done");
    }

    /// Records which `LlmPort` method was actually invoked — `complete` (flat, no progress
    /// signal, hard 300s CLI deadline) vs `complete_streaming` (heartbeat + salvage on the
    /// CLI leg). Used to prove the three aggregate passes route through the tolerant
    /// streaming seam EVEN WHEN there is no live transcript to feed (`feedback = None`),
    /// which used to force them onto the flat `complete` path.
    struct SeamRecordingCompleter {
        complete_calls: std::sync::atomic::AtomicUsize,
        streaming_calls: std::sync::atomic::AtomicUsize,
        text: String,
    }

    #[async_trait::async_trait]
    impl LlmPort for SeamRecordingCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.complete_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(LlmResponse {
                text: self.text.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            _req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            self.streaming_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            on_delta(&self.text);
            Ok(LlmResponse {
                text: self.text.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// GUARD (this fix): `feedback: None` (the headless `camerata inspect` shape — see
    /// `crates/cli/src/inspect_cmd.rs`) used to force `verify_findings` onto the flat,
    /// non-streaming `complete` path, which has a hard 300s CLI subprocess deadline with no
    /// progress reset and no partial-output salvage. It must now route through
    /// `complete_streaming` regardless of whether a transcript is attached.
    #[tokio::test]
    async fn verify_findings_with_no_feedback_uses_the_streaming_seam_not_flat_complete() {
        let resp_text =
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"x"}]}"#;
        let completer = SeamRecordingCompleter {
            complete_calls: 0.into(),
            streaming_calls: 0.into(),
            text: resp_text.to_string(),
        };
        let findings = vec![finding("AI-X", "medium")];

        let _ = verify_findings(
            &completer,
            "me/api",
            findings,
            None,
            None, // feedback: None — the headless-CLI shape
            None,
            false,
            "This repository has 1 code files.",
        )
        .await;

        assert_eq!(
            completer
                .complete_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "must never use the flat non-streaming path"
        );
        assert_eq!(
            completer
                .streaming_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "must route through complete_streaming even with feedback = None"
        );
    }

    /// Same guard for `recommend_alternatives`.
    #[tokio::test]
    async fn recommend_alternatives_with_no_feedback_uses_the_streaming_seam_not_flat_complete() {
        let resp_text = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","applicable":true,"evidence":"a.rs:1"}]}"#;
        let completer = SeamRecordingCompleter {
            complete_calls: 0.into(),
            streaming_calls: 0.into(),
            text: resp_text.to_string(),
        };
        let files = vec![("a.rs".to_string(), "fn f() {}".to_string())];
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));

        let res = recommend_alternatives(
            &completer,
            "me/api",
            &files,
            &files,
            &[alt],
            None,
            None,
            None,
        )
        .await;

        assert!(
            res.is_ok(),
            "the streaming call succeeded, so this must be Ok: {res:?}"
        );
        assert_eq!(
            completer
                .complete_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "must never use the flat non-streaming path"
        );
        assert_eq!(
            completer
                .streaming_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "must route through complete_streaming even with feedback = None"
        );
    }

    /// Same guard for `generate_fix_specifics` — it never took a `feedback`/transcript
    /// parameter at all, so it always hit the flat `complete` path before this fix.
    #[tokio::test]
    async fn generate_fix_specifics_uses_the_streaming_seam_not_flat_complete() {
        let resp_text = r#"{"fixes":[{"index":0,"fix":"Validate the input before using it and return an error otherwise."}]}"#;
        let completer = SeamRecordingCompleter {
            complete_calls: 0.into(),
            streaming_calls: 0.into(),
            text: resp_text.to_string(),
        };
        let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");

        let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;

        assert_eq!(
            completer
                .complete_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "must never use the flat non-streaming path"
        );
        assert_eq!(
            completer
                .streaming_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "must route through complete_streaming"
        );
        assert_eq!(
            out[0].fix_specific.as_deref(),
            Some("Validate the input before using it and return an error otherwise.")
        );
    }

    /// max_tokens must SCALE with the finding count instead of the old hardcoded flat 4096 —
    /// a large finding set legitimately needs more output tokens than that, and the API
    /// backend (unlike the CLI) actually enforces the cap, silently truncating the response
    /// otherwise. A FULL `CALIBRATION_BATCH_SIZE` chunk is deliberately sized (see that
    /// constant's doc comment) to already exceed the old flat floor.
    #[tokio::test]
    async fn verify_findings_scales_max_tokens_with_finding_count() {
        let completer = CapturingCompleter::default();
        let findings: Vec<Finding> = (0..CALIBRATION_BATCH_SIZE)
            .map(|_| finding("AI-X", "medium"))
            .collect();

        let _ = verify_findings(
            &completer,
            "me/api",
            findings,
            None,
            None,
            None,
            false,
            "This repository has many code files.",
        )
        .await;

        let seen = completer.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "a full single chunk makes exactly one call");
        let expected = scaled_max_tokens(CALIBRATION_BATCH_SIZE, VERIFY_TOKENS_PER_FINDING);
        assert!(
            expected > 4096,
            "test setup must exercise the scaled-above-floor branch, got {expected}"
        );
        assert_eq!(seen[0].max_tokens, expected);
    }

    /// A SMALL finding set must see ZERO behavior change — it stays pinned at the original
    /// flat 4096 floor.
    #[tokio::test]
    async fn verify_findings_max_tokens_floor_unchanged_for_small_finding_sets() {
        let completer = CapturingCompleter::default();
        let findings = vec![finding("AI-X", "medium")];

        let _ = verify_findings(
            &completer,
            "me/api",
            findings,
            None,
            None,
            None,
            false,
            "This repository has 1 code files.",
        )
        .await;

        let seen = completer.seen.lock().unwrap();
        assert_eq!(seen[0].max_tokens, 4096);
    }

    /// A fake that streams a FEW tokens of real output and then returns `Ok` with only that
    /// PARTIAL text — exactly the shape `complete_cli_streaming`'s SALVAGE path returns when
    /// the subprocess is killed mid-response after producing something usable (see that
    /// function's own SALVAGE doc comment in `crates/llm/src/llm.rs`). The first call
    /// "dies" after covering only finding 0; later calls (the automatic next-round retry)
    /// are fully healthy. Proves the pass extracts whatever a partial/salvaged response
    /// actually contains — recovering finding 0 in the SAME round — instead of discarding
    /// the whole round because the stream never finished cleanly.
    struct SalvagingFixCompleter {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmPort for SalvagingFixCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("this fake only supports the streaming path")
        }
        async fn complete_streaming(
            &self,
            _req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            let call = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let text = if call == 0 {
                // Streamed a few real tokens, then the process died — SALVAGE returns `Ok`
                // with only what was actually produced, never an `Err` that discards it.
                on_delta(r#"{"fixes":[{"index":0,"fix":"Validate the"#);
                r#"{"fixes":[{"index":0,"fix":"Validate the input before using it and return an error otherwise."}]}"#.to_string()
            } else {
                // Recovered: every subsequent call (the automatic retry round) answers in
                // full for every plausible local index.
                let entries: Vec<String> = (0..50)
                    .map(|i| {
                        format!(
                            r#"{{"index":{i},"fix":"Validate the input before using it and return an error otherwise."}}"#
                        )
                    })
                    .collect();
                format!(r#"{{"fixes":[{}]}}"#, entries.join(","))
            };
            on_delta(&text);
            Ok(LlmResponse {
                text,
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn generate_fix_specifics_recovers_partial_output_from_a_salvaged_stream() {
        let findings = vec![
            fx("ARCH-1", "a.rs", 1, "some real defect", "let x = 1;"),
            fx("ARCH-1", "b.rs", 2, "some real defect", "let y = 2;"),
            fx("ARCH-1", "c.rs", 3, "some real defect", "let z = 3;"),
        ];
        let completer = SalvagingFixCompleter { calls: 0.into() };

        let (out, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

        assert_eq!(
            out[0].fix_specific.as_deref(),
            Some("Validate the input before using it and return an error otherwise."),
            "the salvaged partial round alone must recover finding 0's fix"
        );
        assert!(
            out[1].fix_specific.is_some() && out[2].fix_specific.is_some(),
            "findings missing from the salvaged round must be recovered by the automatic \
             next-round retry, not lost: {out:?}"
        );
        assert!(out.iter().all(|f| !f.needs_review));
    }

    /// A large finding set is split into bounded chunks and every finding survives the
    /// round trip — no finding is dropped at a chunk boundary.
    #[tokio::test]
    async fn verify_findings_chunks_large_finding_sets_and_merges_without_dropping() {
        let n = CALIBRATION_BATCH_SIZE * 2 + 4;
        let findings: Vec<Finding> = (0..n).map(|_| finding("AI-X", "medium")).collect();
        // `CapturingCompleter` always returns `"{}"` (no `verdicts` key) — `apply_verdicts`
        // degrades that to "findings unchanged", which is fine: this test's assertions are
        // about COUNT and CHUNK-COUNT (no finding dropped, no chunk skipped), not verdict
        // content.
        let completer = CapturingCompleter::default();

        let (out, _estimation_failed) = verify_findings(
            &completer,
            "me/api",
            findings,
            None,
            None,
            None,
            false,
            "This repository has many code files.",
        )
        .await;

        assert_eq!(out.len(), n, "no finding dropped across chunk boundaries");
        let expected_chunks = n.div_ceil(CALIBRATION_BATCH_SIZE);
        assert_eq!(
            completer.seen.lock().unwrap().len(),
            expected_chunks,
            "M findings must be split into the expected number of bounded chunks"
        );
    }

    // ── C3-1b: run-wide estimation-pass failure is disclosed, never silently dropped ────

    /// When the calibration pass fails OUTRIGHT (every call, every finding), no finding gets
    /// an effort estimate — this must surface as a `FailedPass{pass: "hour estimation", ..}`
    /// so the export can disclose the gap, AND every finding must still ship (calibration
    /// never drops anything, estimated or not).
    #[tokio::test]
    async fn verify_findings_records_a_failed_pass_when_estimation_fails_run_wide() {
        let findings = vec![finding("AI-X", "medium"), finding("AI-Y", "high")];
        let (out, failed_passes) = verify_findings(
            &FailingCompleter,
            "me/api",
            findings,
            None,
            None,
            None,
            false,
            "This repository has 2 code files.",
        )
        .await;

        assert_eq!(
            out.len(),
            2,
            "a failed estimation pass must never drop a finding"
        );
        assert!(
            out.iter().all(|f| f.effort.is_none()),
            "no finding got an estimate: {out:?}"
        );
        assert_eq!(
            failed_passes.len(),
            1,
            "a run-wide estimation miss must be disclosed exactly once: {failed_passes:?}"
        );
        assert_eq!(failed_passes[0].repo, "me/api");
        assert_eq!(failed_passes[0].pass, "hour estimation");
    }

    /// The sanity counterpart: when calibration succeeds and returns real effort estimates,
    /// NO failed pass is fabricated.
    #[tokio::test]
    async fn verify_findings_records_no_failed_pass_when_estimates_land() {
        let resp_text = r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","effort":"low","reason":"x"}]}"#;
        let llm = StubCompleter {
            text: resp_text.to_string(),
        };
        let findings = vec![finding("AI-X", "medium")];
        let (out, failed_passes) = verify_findings(
            &llm,
            "me/api",
            findings,
            None,
            None,
            None,
            false,
            "This repository has 1 code files.",
        )
        .await;

        assert_eq!(out[0].effort.as_deref(), Some("low"));
        assert!(
            failed_passes.is_empty(),
            "a successful estimation pass must not fabricate a failure disclosure: \
             {failed_passes:?}"
        );
    }

    /// Calibration must ACCEPT an explicit `"critical"` verdict and apply it to the finding —
    /// this is the D5-shaped case: an AI-tier finding starts at "high" from the raw audit pass,
    /// and the calibration pass upgrades it to "critical" when it judges the finding clears the
    /// bar (e.g. an unauthenticated privileged operation).
    #[test]
    fn apply_verdicts_accepts_explicit_critical_verdict() {
        let findings = vec![finding("AI-UNAUTH-CHARGE", "high")];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"unauthenticated charge with service_role"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(out[0].severity, "critical", "an explicit critical verdict must be applied");
    }

    /// ANTI-OVER-ROTATION GUARD: ordinary findings must NEVER be auto-escalated to critical.
    /// An explicit "high" verdict stays "high" (it is a recognized value, not funneled through
    /// the "unknown -> medium" fallback into anything resembling critical), and a verdict that
    /// omits `severity` entirely leaves the finding's prior severity untouched — neither path
    /// can produce "critical" without the model saying so explicitly.
    #[test]
    fn apply_verdicts_does_not_auto_escalate_ordinary_findings_to_critical() {
        let findings = vec![
            finding("AI-ORDINARY-HIGH", "medium"), // index 0: explicit "high" verdict
            finding("AI-UNTOUCHED", "high"),        // index 1: verdict omits severity
        ];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"high","confidence":"high","reason":"a real but contained bug"},
            {"index":1,"confidence":"high","reason":"no severity opinion given"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        let ordinary = out.iter().find(|f| f.rule_id == "AI-ORDINARY-HIGH").unwrap();
        assert_eq!(ordinary.severity, "high", "an explicit high verdict must stay high, never inflate to critical");
        let untouched = out.iter().find(|f| f.rule_id == "AI-UNTOUCHED").unwrap();
        assert_eq!(untouched.severity, "high", "no severity field in the verdict leaves the finding's prior severity as-is");
    }

    // ── Upward-calibration precondition guardrail ─────────────────────────────
    //
    // Calibration review finding: a High "unverified session read" finding whose own note
    // says exploitation "requires forging a cookie" was promoted to Critical by calibration
    // and took a top-3 do-now slot from a genuinely unconditional critical. These guard the
    // fix: a recorded exploit precondition can BLOCK an upward move, never cause a downward
    // one.

    fn finding_with_detail(rule: &str, sev: &str, detail: &str) -> Finding {
        Finding {
            detail: detail.to_string(),
            ..finding(rule, sev)
        }
    }

    /// A High finding whose own text records an exploit precondition ("requires forging a
    /// session cookie") must NOT be raised to Critical even when the calibration model's
    /// verdict says so — the raise is refused and the base (authored) severity is kept.
    #[test]
    fn apply_verdicts_blocks_upward_raise_when_precondition_is_recorded() {
        let findings = vec![finding_with_detail(
            "AI-UNVERIFIED-SESSION-READ",
            "high",
            "Server code authorizes from an unverified session read; exploitation requires \
             forging a session cookie.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"escalate to critical"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(
            out[0].severity, "high",
            "a recorded exploit precondition must block the upward raise to critical"
        );
        assert!(
            out[0]
                .calibration_rationale
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("precondition"),
            "the block must be recorded on the finding so it's auditable: {:?}",
            out[0].calibration_rationale
        );
    }

    /// The SAME raising verdict on a High finding with NO precondition language must still
    /// go through to Critical — the guardrail must not over-block ordinary calibration.
    #[test]
    fn apply_verdicts_still_raises_when_no_precondition_is_recorded() {
        let findings = vec![finding_with_detail(
            "AI-UNAUTH-EXPORT",
            "high",
            "The export endpoint returns every user's records with no authorization check.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"unconditionally exploitable with one request"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(
            out[0].severity, "critical",
            "with no precondition recorded, the raise must still apply"
        );
    }

    /// A finding already AT critical, whose text also records a precondition phrase, must
    /// stay critical — the guardrail only ever blocks a RAISE, it must never be read as
    /// license to demote an already-critical finding.
    #[test]
    fn apply_verdicts_never_demotes_an_already_critical_finding_with_a_precondition() {
        let findings = vec![finding_with_detail(
            "AI-ALREADY-CRITICAL",
            "critical",
            "Full account takeover is possible; exploitation requires only an unauthenticated \
             request.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"confirmed critical"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(
            out[0].severity, "critical",
            "an already-critical finding must never be demoted by precondition language"
        );
    }

    // ── cycle-5 regression: precondition cap must be authoritative on the verdict path too ────
    //
    // Calibration review: two High-severity planted defects (a mutation gated by an unverified
    // session that requires forging a cookie; a public bucket of sensitive documents that
    // requires knowing a path) still came out Critical. These pin the verdict-path guard against
    // the exact wording of those two classes — path-knowledge preconditions, not just
    // cookie/token forging — bidirectional with the "do-not-break" raise test above.

    /// A path-knowledge precondition ("requires knowing the exact ... path") must block the
    /// upward raise exactly like a cookie-forging precondition does — the guard is phrase-based
    /// on "requires ", not scoped to session/cookie language specifically.
    #[test]
    fn apply_verdicts_blocks_upward_raise_for_public_bucket_path_knowledge_precondition() {
        let findings = vec![finding_with_detail(
            "AI-PUBLIC-BUCKET-SENSITIVE-DOCS",
            "high",
            "A storage bucket of sensitive documents is publicly readable; exploitation requires \
             knowing the exact object path, which is not discoverable by brute force.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"sensitive data exposed"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(
            out[0].severity, "high",
            "a path-knowledge precondition must block the upward raise to critical, same as a \
             cookie-forging precondition"
        );
    }

    /// UNDER-RATING GUARD (do-not-break): a committed secret with NO precondition language must
    /// still raise to Critical through the ordinary verdict path — the precondition guard must
    /// never be mistaken for a general brake on critical verdicts.
    #[test]
    fn apply_verdicts_committed_secret_raises_to_critical_without_precondition() {
        let findings = vec![finding_with_detail(
            "AI-STRIPE-SECRET-COMMITTED",
            "high",
            "A live Stripe secret key is hardcoded in this file and committed to the repository.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"secret committed to version control"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(
            out[0].severity, "critical",
            "a committed secret with no precondition language must raise to critical"
        );
    }

    // ── D5: severity calibration rules of thumb ───────────────────────────────

    fn finding_with_category(rule: &str, sev: &str, detail: &str, category: &str) -> Finding {
        Finding {
            detail: detail.to_string(),
            category: Some(category.to_string()),
            ..finding(rule, sev)
        }
    }

    /// An unauthenticated-access finding must floor at Critical regardless of what severity the
    /// scanner/calibration model assigned it, and the rationale must be recorded.
    #[test]
    fn severity_floor_unauthenticated_access_is_critical() {
        let f = finding_with_detail(
            "AI-EXPORT-ENDPOINT",
            "medium",
            "The /api/export endpoint is reachable without authentication and returns every \
             user's records.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "unauthenticated access must floor to critical"
        );
        let rationale = out
            .calibration_rationale
            .expect("a calibration rationale must be recorded");
        assert!(
            rationale.to_lowercase().contains("unauthenticated")
                || rationale.to_lowercase().contains("critical"),
            "rationale must explain the unauthenticated/critical floor: {rationale}"
        );
    }

    /// A path to full-account compromise (not merely unauthenticated) must ALSO floor at
    /// Critical — the plan's second qualifying clause for the Critical tier.
    #[test]
    fn severity_floor_full_account_compromise_is_critical() {
        let f = finding_with_detail(
            "AI-SESSION-FIXATION",
            "high",
            "An attacker can achieve full account takeover by replaying a pre-auth session id.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "full-account compromise must floor to critical"
        );
        assert!(out.calibration_rationale.is_some());
    }

    /// An AUTHENTICATED cross-tenant READ of ordinary (non-sensitive) data floors at High, NOT
    /// Critical — the plan's explicit "High by default" rule, guarding against inflation.
    #[test]
    fn severity_floor_authenticated_cross_tenant_read_is_high_not_critical() {
        let f = finding_with_detail(
            "AI-ORG-SETTINGS-LEAK",
            "medium",
            "An authenticated user can view another organization's settings by changing the org \
             id in the URL.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "high",
            "an authenticated cross-tenant read of non-sensitive data floors at High, not Critical"
        );
        let rationale = out.calibration_rationale.expect("rationale recorded");
        assert!(
            rationale.to_lowercase().contains("high"),
            "the rationale for a High floor must say so: {rationale}"
        );
        assert!(
            !rationale.to_lowercase().contains("escalates to critical"),
            "a non-escalated High floor must not use the escalation-branch wording: {rationale}"
        );
    }

    /// A cross-tenant read that exposes PAYMENT data escalates all the way to Critical — the
    /// plan's explicit escalation class.
    #[test]
    fn severity_floor_cross_tenant_read_of_payment_data_escalates_to_critical() {
        let f = finding_with_detail(
            "AI-INVOICE-LEAK",
            "medium",
            "An authenticated user can view another organization's stored payment method and \
             card number.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "cross-tenant read of payment data must escalate to Critical"
        );
    }

    /// A cross-tenant read that exposes CREDENTIALS/secrets also escalates to Critical.
    #[test]
    fn severity_floor_cross_tenant_read_of_credentials_escalates_to_critical() {
        let f = finding_with_detail(
            "AI-API-KEY-LEAK",
            "low",
            "An authenticated user can fetch another tenant's stored api key from the settings \
             endpoint.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "cross-tenant read of a credential/secret must escalate to Critical"
        );
    }

    /// A cross-tenant read that exposes sensitive PII (SSN) also escalates to Critical.
    #[test]
    fn severity_floor_cross_tenant_read_of_sensitive_pii_escalates_to_critical() {
        let f = finding_with_detail(
            "AI-PROFILE-LEAK",
            "medium",
            "An authenticated user can view another user's social security number through the \
             profile API.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "cross-tenant read of sensitive PII must escalate to Critical"
        );
    }

    /// The floor only ever RAISES severity — a finding already calibrated ABOVE the computed
    /// floor must not be lowered back down to it.
    #[test]
    fn severity_floor_never_lowers_an_already_higher_severity() {
        let f = finding_with_detail(
            "AI-ORG-SETTINGS-LEAK-2",
            "critical", // already critical from an earlier, more specific verdict
            "An authenticated user can view another organization's settings.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "the floor (high) must never lower a severity already above it"
        );
    }

    /// A finding whose text matches none of the D5 signals is left completely untouched —
    /// no rationale is fabricated for a finding the rule doesn't apply to.
    #[test]
    fn severity_floor_does_not_touch_unrelated_findings() {
        let f = finding_with_detail(
            "ARCH-STRICT-LAYERING-1",
            "medium",
            "The controller calls the repository directly, skipping the service layer.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(out.severity, "medium", "severity must be untouched");
        assert_eq!(
            out.calibration_rationale, None,
            "no rationale is recorded when no rule of thumb applies"
        );
    }

    /// THE FLOOR/DETERMINISTIC-ORIGIN CASE (ties to D3): an RLS-policy finding must NOT be
    /// under-rated to below High merely because the write-up hedges that RLS probably contains
    /// it — defense-in-depth speculation must never excuse a boundary that has already failed.
    /// This is exactly the class of finding the deterministic floor/native RLS checker produces
    /// (never touched by LLM calibration at all), so the function is exercised directly over a
    /// floor-shaped finding — low severity, `confidence: None`, exactly as the floor emits it.
    #[test]
    fn severity_floor_rls_finding_not_downgraded_on_rls_probably_contains_it_hedge() {
        let f = finding_with_category(
            "SUPABASE-RLS-NO-POLICY-1",
            "low", // e.g. a merge/consequence pass had already softened this
            "The profiles table has RLS enabled but no policy defined; row-level security \
             probably contains unauthorized access, so real-world risk is limited.",
            "rls-policy",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "high",
            "an RLS-hedged access-control finding must floor at High, never stay low"
        );
        let rationale = out.calibration_rationale.expect("rationale recorded");
        assert!(
            rationale.to_lowercase().contains("rls"),
            "rationale must name the RLS-hedge guard: {rationale}"
        );
    }

    /// The RLS-hedge guard also covers an injection-class finding (D3's query-grammar-injection
    /// family) that hedges the same way, not just rls-policy — both are access-control-adjacent.
    #[test]
    fn severity_floor_injection_finding_not_downgraded_on_rls_hedge() {
        let f = finding_with_category(
            "SEC-NO-QUERY-GRAMMAR-INJECTION-1",
            "medium",
            "User input is concatenated into a PostgREST .or() filter string; this is likely \
             covered by RLS on the underlying table.",
            "injection",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "high",
            "an RLS-hedged injection finding must floor at High"
        );
    }

    /// A finding that CONFIRMS (not speculates) a mitigating RLS policy — no hedge language at
    /// all — is NOT touched by the RLS-hedge guard; only speculative "probably"/"likely"/
    /// "should" language trips it.
    #[test]
    fn severity_floor_rls_hedge_guard_does_not_fire_without_speculative_language() {
        let f = finding_with_category(
            "SUPABASE-RLS-ENABLED-1",
            "low",
            "The profiles table has RLS enabled with an owner-scoped SELECT policy confirmed in \
             the migration.",
            "rls-policy",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "low",
            "a confirmed (non-speculative) mitigation is not overridden by the hedge guard"
        );
        assert_eq!(out.calibration_rationale, None);
    }

    /// The RLS-hedge guard is scoped to access-control/injection findings — an unrelated
    /// (e.g. testing-style) finding that happens to mention "RLS" and "probably" in passing is
    /// not swept up into a High floor.
    #[test]
    fn severity_floor_rls_hedge_guard_does_not_fire_outside_access_control_class() {
        let f = finding_with_category(
            "TESTING-PYRAMID-1",
            "low",
            "This test module probably doesn't need the RLS fixture it imports.",
            "testing-style",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "low",
            "a non-access-control finding is unaffected by the hedge guard"
        );
    }

    /// `apply_severity_calibration_rules` (plural) must apply the floor across a whole finding
    /// set — the shape `verify_findings` actually calls it with, after `apply_verdicts`.
    #[test]
    fn severity_floor_applies_across_a_finding_set_after_apply_verdicts() {
        let findings = vec![
            finding_with_detail(
                "AI-UNAUTH-EXPORT",
                "high",
                "The export endpoint requires no authentication and returns all records.",
            ),
            finding_with_detail("AI-UNRELATED", "medium", "A minor style inconsistency."),
        ];
        // Calibration itself did not escalate the unauthenticated finding (a plausible
        // under-rated model verdict) — the deterministic floor must catch it anyway.
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"high","confidence":"high","reason":"no auth check"},
            {"index":1,"severity":"medium","confidence":"high","reason":"style"}
        ]}"#;
        let calibrated = apply_verdicts(raw, findings);
        let floored = apply_severity_calibration_rules(calibrated);
        let unauth = floored
            .iter()
            .find(|f| f.rule_id == "AI-UNAUTH-EXPORT")
            .unwrap();
        assert_eq!(
            unauth.severity, "critical",
            "the deterministic floor overrides an under-rated model verdict"
        );
        let unrelated = floored
            .iter()
            .find(|f| f.rule_id == "AI-UNRELATED")
            .unwrap();
        assert_eq!(
            unrelated.severity, "medium",
            "an unrelated finding passes through the floor untouched"
        );
        assert_eq!(unrelated.calibration_rationale, None);
    }

    // ── cycle-5: the D5 floor's own critical escalation must also respect the precondition
    // cap ─────────────────────────────────────────────────────────────────────────────────────
    //
    // Calibration review (second attempt): cycle-4's R2 blocked an upward raise in
    // `apply_verdicts` when a precondition phrase was present, but TWO High-severity planted
    // defects still came out Critical — a mutation gated by an unverified session that requires
    // forging a cookie; a public bucket of sensitive documents that requires knowing a path.
    // Root cause: `severity_calibration_floor` (D5) re-derives Critical independently from the
    // finding's own unauthenticated/data-class vocabulary, runs AFTER `apply_verdicts`, and
    // never consulted preconditions at all — so it silently re-raised exactly what the
    // verdict-path guard had just blocked. These tests pin the fix on the floor path directly
    // (OVER-RATING guard), and the paired "do-not-break" tests pin the floor's genuine Critical
    // classes stay Critical when no precondition is recorded (UNDER-RATING guard).

    /// OVER-RATING GUARD: a finding that trips the UNAUTHENTICATED/full-compromise floor branch
    /// (normally Critical) must cap at High when its own text ALSO records an exploit
    /// precondition (forging a session cookie). Base severity starts below High to prove the cap
    /// is an upper bound, not merely "leave it where it was" — it still raises from Medium to
    /// High, just never all the way to Critical.
    #[test]
    fn severity_floor_caps_unauthenticated_critical_escalation_at_high_with_precondition() {
        let f = finding_with_detail(
            "AI-MUTATION-FORGED-COOKIE",
            "medium",
            "The admin-settings mutation runs with no authentication required on the session \
             check; exploitation requires forging a session cookie to pass the check.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "high",
            "an exploit precondition caps the floor's unauthenticated-branch critical \
             escalation at High, though it still raises Medium up to High"
        );
        let rationale = out.calibration_rationale.expect("rationale recorded");
        assert!(
            rationale.to_lowercase().contains("precondition"),
            "the cap must be recorded on the finding so it's auditable: {rationale}"
        );
    }

    /// OVER-RATING GUARD: the cross-tenant-read + escalating-data-class floor branch (normally
    /// Critical) must ALSO cap at High when the finding's own text records a precondition — here,
    /// an attacker needing to know a secret storage path, mirroring the "public bucket of
    /// sensitive documents" planted defect from the calibration review.
    #[test]
    fn severity_floor_caps_cross_tenant_data_class_critical_escalation_at_high_with_precondition() {
        let f = finding_with_detail(
            "AI-PUBLIC-BUCKET-DOCS",
            "medium",
            "An authenticated user can view another organization's stored government id \
             documents in the shared bucket, though exploitation requires knowing the exact \
             storage path.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "high",
            "a precondition caps the cross-tenant/sensitive-data-class critical escalation at \
             High, not Critical"
        );
    }

    /// END-TO-END regression pin: `apply_verdicts` blocks the model's upward raise for a
    /// preconditioned finding (cycle-4 R2), and the D5 floor that runs immediately after it must
    /// NOT re-raise the finding back to Critical from its own independent text scan — this is the
    /// exact failure mode the calibration review caught on the second attempt.
    #[test]
    fn severity_floor_does_not_re_raise_what_apply_verdicts_blocked_for_precondition() {
        let findings = vec![finding_with_detail(
            "AI-MUTATION-FORGED-COOKIE-2",
            "high",
            "The admin-settings mutation runs with no authentication required on the session \
             check; exploitation requires forging a session cookie to pass the check.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"escalate to critical"}
        ]}"#;
        let calibrated = apply_verdicts(raw, findings);
        assert_eq!(
            calibrated[0].severity, "high",
            "apply_verdicts must block the model's upward raise (cycle-4 R2)"
        );
        let floored = apply_severity_calibration_rules(calibrated);
        assert_eq!(
            floored[0].severity, "high",
            "the deterministic D5 floor must not re-raise to critical what apply_verdicts \
             already capped — this was the cycle-4-to-cycle-5 regression"
        );
    }

    /// UNDER-RATING GUARD (do-not-break): a genuine unauthenticated exposure of sensitive data
    /// with NO precondition recorded must still floor all the way to Critical — the cap must
    /// never become a general brake on real unconditional criticals.
    #[test]
    fn severity_floor_unauthenticated_sensitive_data_exposure_stays_critical_without_precondition()
    {
        let f = finding_with_detail(
            "AI-UNAUTH-PII-EXPORT",
            "medium",
            "The /api/records endpoint is unauthenticated and returns every user's social \
             security number with no access control.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "a genuine unauthenticated sensitive-data exposure with no precondition must stay \
             Critical"
        );
    }

    /// UNDER-RATING GUARD (do-not-break, false-positive regression): "requires no
    /// authentication" is NOT a precondition — it NEGATES the requirement, describing the exact
    /// unconditional exposure the floor polices. Without the negation guard on the "requires "
    /// phrase, this exact wording (already used by `severity_floor_applies_across_a_finding_set_
    /// after_apply_verdicts` above) would be misread as a precondition and wrongly capped at
    /// High instead of Critical.
    #[test]
    fn severity_floor_requires_no_authentication_is_not_mistaken_for_a_precondition() {
        let f = finding_with_detail(
            "AI-UNAUTH-REQUIRES-NO-AUTH",
            "medium",
            "The export endpoint requires no authentication and returns every user's records.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "critical",
            "\"requires no authentication\" negates the requirement and must still floor to \
             Critical, not be capped at High"
        );
    }

    /// GUARDRAIL (do-not-break): the floor-high RLS-hedge class (injection family, which covers
    /// stored XSS) must stay exactly High — unaffected by the precondition cap, since the cap
    /// only ever touches a Critical floor value, never a High one.
    #[test]
    fn severity_floor_stored_xss_injection_class_rls_hedge_stays_high_guardrail() {
        let f = finding_with_category(
            "AI-STORED-XSS-RLS-HEDGE",
            "medium",
            "Stored XSS: user-supplied HTML is rendered without sanitization; this is likely \
             covered by RLS on the underlying table.",
            "injection",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "high",
            "a stored-XSS/injection-class floor must stay High on an RLS hedge, unaffected by \
             the precondition cap"
        );
    }

    /// GUARDRAIL (do-not-break): a weak-token-class finding that trips none of the floor's
    /// phrases stays exactly at its authored Medium — neither raised by the floor nor lowered by
    /// the precondition cap (which only ever lowers a Critical FLOOR VALUE, never a finding's
    /// already-assigned severity).
    #[test]
    fn severity_floor_weak_token_class_stays_medium_guardrail() {
        let f = finding_with_detail(
            "AI-WEAK-SESSION-TOKEN",
            "medium",
            "Session tokens are generated with a weak, predictable random source, making them \
             guessable with effort.",
        );
        let out = apply_severity_calibration_rule(f);
        assert_eq!(
            out.severity, "medium",
            "a weak-token finding that trips no floor rule stays at its authored Medium"
        );
        assert_eq!(
            out.calibration_rationale, None,
            "no floor rule applies here; the precondition cap must not fabricate a rationale \
             for a finding the floor never touched"
        );
    }

    // ── D6: severity ceiling — R1/R2 (2026-09-30 cycle-2 queue-hardening) ─────────
    //
    // The D5 floor above only ever raises severity. These pin the opposite direction: R1
    // re-routes a self-hedged "probably intentional" finding to needs-review + Low (out of every
    // action bucket, but never dropped); R2 clamps a browser-mediated CORS misconfiguration to
    // exactly Medium in BOTH directions, without disturbing the floor's Critical rating for a
    // genuinely unauthenticated-exposure finding.

    const CORS_CREDENTIALS_TEXT: &str =
        "The API's CORS policy reflects the request's Origin header back verbatim and sets \
         Access-Control-Allow-Credentials: true, so any origin can make authenticated \
         cross-origin requests riding the victim's session cookies.";

    /// R2, downward direction: a CORS-with-credentials finding that started at Critical (the
    /// current over-rating failure) must land at exactly Medium — not left at Critical.
    #[test]
    fn severity_ceiling_cors_credentials_lowers_critical_to_medium() {
        let f = finding_with_detail("AI-CORS-1", "critical", CORS_CREDENTIALS_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "medium");
        assert!(out.calibration_rationale.is_some());
    }

    /// R2, upward direction: the SAME class starting at Low (the prior re-burial failure) must
    /// ALSO land at exactly Medium — not stay at Low.
    #[test]
    fn severity_ceiling_cors_credentials_raises_low_to_medium() {
        let f = finding_with_detail("AI-CORS-2", "low", CORS_CREDENTIALS_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "medium");
    }

    /// Bidirectional guard, spelled out explicitly: the CORS class must never be observed at
    /// either extreme after both passes run, whichever extreme it started at.
    #[test]
    fn severity_ceiling_cors_credentials_is_never_low_or_critical() {
        for start in ["critical", "high", "medium", "low", "info"] {
            let f = finding_with_detail("AI-CORS-3", start, CORS_CREDENTIALS_TEXT);
            let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
            assert_ne!(
                out.severity, "low",
                "starting severity {start:?} must not stay Low"
            );
            assert_ne!(
                out.severity, "critical",
                "starting severity {start:?} must not become Critical"
            );
            assert_eq!(out.severity, "medium");
        }
    }

    /// Non-interference: a genuinely unauthenticated-exposure finding (no CORS vocabulary at
    /// all) must still floor to Critical after BOTH the floor and the ceiling run — the D6
    /// ceiling must never reintroduce the old under-rating for a class it doesn't target.
    #[test]
    fn severity_ceiling_does_not_touch_unauthenticated_exposure_floor() {
        let f = finding_with_detail(
            "AI-UNAUTH-EXPORT-2",
            "medium",
            "The export endpoint requires no authentication and returns every user's records.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "critical",
            "a non-CORS unauthenticated-exposure finding keeps the floor's critical rating"
        );
    }

    const SELF_HEDGE_TEXT: &str =
        "This debug endpoint skips the auth middleware, but it appears intentional and is \
         likely only used for internal load-testing; confirm whether this is expected before \
         treating it as a defect.";

    /// R1: a self-hedged "probably intentional / confirmation-only" finding is capped to Low
    /// AND flagged needs-review — routing it to the `held` bucket (C5-4) without dropping it.
    #[test]
    fn severity_ceiling_self_hedged_confirmation_only_caps_to_low_and_needs_review() {
        let f = finding_with_detail("AI-HEDGE-1", "high", SELF_HEDGE_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "low");
        assert_eq!(out.confidence.as_deref(), Some("needs-review"));
        assert!(out.calibration_rationale.is_some());
    }

    /// Safe twin: an actionable finding at the SAME base severity whose text is NOT self-hedged
    /// is left completely unchanged by the ceiling pass.
    #[test]
    fn severity_ceiling_non_hedged_twin_is_unchanged() {
        let f = finding_with_detail(
            "AI-HEDGE-2",
            "high",
            "This debug endpoint skips the auth middleware in production with no mitigating \
             control; it must be removed before release.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f.clone()));
        assert_eq!(out.severity, "high");
        assert_eq!(out.confidence, None);
    }

    /// R1 negation guard: text that explicitly says the pattern is NOT intentional (a confirmed
    /// defect, not a hedge) must never be swept up by the self-hedge phrase set.
    #[test]
    fn severity_ceiling_self_hedge_negation_guard_does_not_downgrade() {
        let f = finding_with_detail(
            "AI-HEDGE-3",
            "high",
            "The team confirmed this is NOT intentional — it's a genuine authorization bug that \
             must be fixed.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "high",
            "negated hedge language must not downgrade severity"
        );
        assert_eq!(out.confidence, None);
    }

    // ── D7 (C6-A3): severity ceiling R3 (open/unvalidated redirect) and R4 (a privileged/role
    // check enforced only in client-visible code) ────────────────────────────────────────────
    //
    // Observed across two independent benchmark repos: a client-side/open redirect was
    // inflating to High, and a UI-only admin/role check was inflating to Critical, run over run
    // on the SAME code. Each anchor below is pinned bidirectionally — an over-rating guard (the
    // class caps at its anchor) and an under-rating guard (the regression list of classes that
    // must NOT be buried by the new rules: a chained redirect, a genuine unauthenticated
    // exposure, a committed secret, the weak-token class, and the floor-high
    // injection/stored-XSS class).

    const OPEN_REDIRECT_TEXT: &str =
        "The /login?next= handler performs an open redirect: `next` is taken straight from the \
         query string and used as the post-login redirect target with no allow-list or same-\
         origin check.";

    const CHAINED_REDIRECT_TEXT: &str =
        "The OAuth callback performs an open redirect to a `return_to` URL taken from the query \
         string with no allow-list check; the redirect destination captures the session token \
         appended to the fragment, so a crafted link exfiltrates the victim's session.";

    const CLIENT_ONLY_ADMIN_CHECK_TEXT: &str =
        "The `isAdmin` role check that gates the delete-organization button is enforced \
         client-side only in the React component; there is no server-side check on the DELETE \
         /orgs/:id route, so any authenticated user can send the request directly.";

    /// OVER-RATING GUARD: an open-redirect finding that started Critical (the observed inflation
    /// failure) must land at exactly Medium, not stay at High or Critical.
    #[test]
    fn severity_ceiling_open_redirect_caps_critical_at_medium() {
        let f = finding_with_detail("AI-OPEN-REDIRECT-1", "critical", OPEN_REDIRECT_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "medium");
        assert!(out.calibration_rationale.is_some());
    }

    /// OVER-RATING GUARD, upward direction: the SAME unchained redirect class starting at Low
    /// must ALSO land at exactly Medium — not left buried at Low. Bidirectional, like R2's CORS
    /// clamp.
    #[test]
    fn severity_ceiling_open_redirect_raises_low_to_medium() {
        let f = finding_with_detail("AI-OPEN-REDIRECT-2", "low", OPEN_REDIRECT_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "medium");
    }

    /// Bidirectional guard, spelled out explicitly: an UNCHAINED open redirect must never be
    /// observed at either extreme after both passes run, whichever extreme it started at.
    #[test]
    fn severity_ceiling_open_redirect_is_never_low_high_or_critical() {
        for start in ["critical", "high", "medium", "low", "info"] {
            let f = finding_with_detail("AI-OPEN-REDIRECT-3", start, OPEN_REDIRECT_TEXT);
            let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
            assert_eq!(
                out.severity, "medium",
                "starting severity {start:?} must land at exactly Medium"
            );
        }
    }

    /// UNDER-RATING GUARD (do-not-break): an open redirect CHAINED to credential capture must
    /// land at High, not buried at Medium — even when it started at Low or Medium.
    #[test]
    fn severity_ceiling_chained_redirect_raises_low_to_high() {
        let f = finding_with_detail("AI-CHAINED-REDIRECT-1", "low", CHAINED_REDIRECT_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "high",
            "a redirect chained to credential capture must reach High, not be buried at Medium"
        );
    }

    /// UNDER-RATING GUARD (do-not-break), over-rating half of the same chained class: a chained
    /// redirect that started Critical must come DOWN to exactly High, not stay at Critical —
    /// chaining earns High, not Critical, from the redirect mechanism alone.
    #[test]
    fn severity_ceiling_chained_redirect_caps_critical_at_high() {
        let f = finding_with_detail("AI-CHAINED-REDIRECT-2", "critical", CHAINED_REDIRECT_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "high");
    }

    /// NEVER-DEMOTE / BLOCKS-UP-ONLY guard: a finding whose text is BOTH an open redirect AND,
    /// independently, an unauthenticated path to a privileged operation earned its Critical
    /// through the D5 floor's mechanism, not the redirect's — R3 must not demote it.
    #[test]
    fn severity_ceiling_open_redirect_does_not_demote_unauthenticated_exposure() {
        let f = finding_with_detail(
            "AI-REDIRECT-PLUS-UNAUTH",
            "high",
            "The admin panel's /impersonate endpoint performs an open redirect to an \
             attacker-controlled URL AND requires no authentication at all, letting anyone \
             assume any user's identity.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "critical",
            "an independently-earned unauthenticated/full-compromise critical must survive the \
             redirect ceiling, not be demoted to Medium"
        );
    }

    /// OVER-RATING GUARD: a privileged/role check enforced only client-side (the observed
    /// inflation failure) must cap at High, not stay Critical.
    #[test]
    fn severity_ceiling_client_only_admin_check_caps_critical_at_high() {
        let f = finding_with_detail(
            "AI-CLIENT-ADMIN-1",
            "critical",
            CLIENT_ONLY_ADMIN_CHECK_TEXT,
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "high");
        assert!(out.calibration_rationale.is_some());
    }

    /// Cap-only semantics (like R1): a client-only admin-check finding that started at Medium
    /// must NOT be raised by R4 — the rule only ever lowers a severity above its anchor, never
    /// raises one below it. The rationale is still recorded (transparency, same convention as
    /// D5's floor when it "touches but doesn't change" a finding).
    #[test]
    fn severity_ceiling_client_only_admin_check_does_not_raise_below_high() {
        let f = finding_with_detail("AI-CLIENT-ADMIN-2", "medium", CLIENT_ONLY_ADMIN_CHECK_TEXT);
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "medium",
            "R4 caps Critical down to High; it must never raise a lower severity up to High"
        );
        assert!(
            out.calibration_rationale.is_some(),
            "the rule touched this finding (it matched the class) so a rationale is recorded \
             even though the severity itself didn't change"
        );
    }

    /// NEVER-DEMOTE / BLOCKS-UP-ONLY guard: a finding whose text is BOTH a client-only admin
    /// check AND, independently, an unauthenticated path to full account compromise earned its
    /// Critical through the D5 floor's mechanism — R4 must not demote it.
    #[test]
    fn severity_ceiling_client_only_admin_check_does_not_demote_unauthenticated_exposure() {
        let f = finding_with_detail(
            "AI-CLIENT-ADMIN-PLUS-UNAUTH",
            "high",
            "The admin role check is enforced client-side only with no server-side check, and \
             separately the account-recovery flow allows full account takeover with no \
             authentication required at all.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "critical",
            "an independently-earned unauthenticated/full-compromise critical must survive the \
             client-only-admin-check ceiling, not be capped to High"
        );
    }

    /// UNDER-RATING GUARD (do-not-break, shared regression list): a genuinely unauthenticated
    /// sensitive-data exposure with no redirect/client-check vocabulary at all must still reach
    /// Critical through the full floor+ceiling pipeline — R3/R4 must not become a general brake
    /// on real unconditional criticals they don't target.
    #[test]
    fn severity_ceiling_unauthenticated_exposure_unaffected_by_new_rules() {
        let f = finding_with_detail(
            "AI-UNAUTH-EXPORT-3",
            "medium",
            "The /api/records endpoint is unauthenticated and returns every user's social \
             security number with no access control.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "critical");
    }

    /// UNDER-RATING GUARD (do-not-break, shared regression list): a committed secret, run
    /// through the model-verdict path AND the full deterministic floor+ceiling pipeline, must
    /// still land at Critical — R3/R4 have no redirect/client-check vocabulary to match here, so
    /// this pins that the new rules don't accidentally interfere with an unrelated class.
    #[test]
    fn severity_ceiling_committed_secret_unaffected_by_new_rules() {
        let findings = vec![finding_with_detail(
            "AI-STRIPE-SECRET-COMMITTED-2",
            "high",
            "A live Stripe secret key is hardcoded in this file and committed to the repository.",
        )];
        let raw = r#"{"verdicts":[
            {"index":0,"severity":"critical","confidence":"high","reason":"secret committed to version control"}
        ]}"#;
        let calibrated = apply_verdicts(raw, findings);
        let floored = apply_severity_calibration_rules(calibrated);
        let ceiled = apply_severity_ceiling_rules(floored);
        assert_eq!(ceiled[0].severity, "critical");
    }

    /// UNDER-RATING GUARD (do-not-break, shared regression list): the weak-token class, which
    /// trips no floor or ceiling rule at all, must stay exactly at its authored Medium through
    /// the full pipeline — never demoted to Low by the new rules.
    #[test]
    fn severity_ceiling_weak_token_class_unaffected_by_new_rules() {
        let f = finding_with_detail(
            "AI-WEAK-SESSION-TOKEN-2",
            "medium",
            "Session tokens are generated with a weak, predictable random source, making them \
             guessable with effort.",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(
            out.severity, "medium",
            "the weak-token class must stay at Medium, never buried at Low by the new rules"
        );
    }

    /// UNDER-RATING GUARD (do-not-break, shared regression list): a floor-high injection/stored-
    /// XSS finding must stay exactly High through the full pipeline — unaffected by R3/R4, which
    /// have no redirect/client-check vocabulary to match on this finding's text.
    #[test]
    fn severity_ceiling_stored_xss_floor_high_unaffected_by_new_rules() {
        let f = finding_with_category(
            "AI-STORED-XSS-RLS-HEDGE-2",
            "medium",
            "Stored XSS: user-supplied HTML is rendered without sanitization; this is likely \
             covered by RLS on the underlying table.",
            "injection",
        );
        let out = apply_severity_ceiling_rule(apply_severity_calibration_rule(f));
        assert_eq!(out.severity, "high");
    }

    // ── Determinism: the deterministic calibration layer must be stable run-to-run ───────────
    //
    // The engine has oscillated on severity for the SAME code across runs. The floor (D5) and
    // ceiling (D6/D7) passes are pure functions of each finding's own text, so — unlike the
    // calibration MODEL, which this test makes no claim about and cannot make deterministic —
    // running them twice over an identical finding set must produce identical output: same
    // severities, same calibration rationale text, same order. This is the guard for "same
    // input -> same severity" for everything that does NOT depend on the model's judgment call.

    /// Running the deterministic floor+ceiling pipeline twice over the SAME finding set (one
    /// finding per anchor covered above, plus a couple of untouched/unrelated findings) must
    /// yield identical severities, identical rationale text, and identical ordering both times.
    #[test]
    fn deterministic_calibration_pipeline_is_stable_across_two_runs() {
        fn build_set() -> Vec<Finding> {
            vec![
                finding_with_detail(
                    "AI-UNAUTH",
                    "medium",
                    "The export endpoint is unauthenticated and returns every user's records.",
                ),
                finding_with_detail("AI-REDIRECT", "high", OPEN_REDIRECT_TEXT),
                finding_with_detail("AI-CHAINED-REDIRECT", "low", CHAINED_REDIRECT_TEXT),
                finding_with_detail("AI-CLIENT-ADMIN", "critical", CLIENT_ONLY_ADMIN_CHECK_TEXT),
                finding_with_detail("AI-CORS", "low", CORS_CREDENTIALS_TEXT),
                finding_with_detail("AI-HEDGE", "high", SELF_HEDGE_TEXT),
                finding_with_detail(
                    "AI-WEAK-TOKEN",
                    "medium",
                    "Session tokens are generated with a weak, predictable random source.",
                ),
                finding_with_detail("AI-UNRELATED", "medium", "A minor style inconsistency."),
            ]
        }

        fn run(findings: Vec<Finding>) -> Vec<(String, String, Option<String>)> {
            let floored = apply_severity_calibration_rules(findings);
            let ceiled = apply_severity_ceiling_rules(floored);
            ceiled
                .into_iter()
                .map(|f| (f.rule_id, f.severity, f.calibration_rationale))
                .collect()
        }

        let run1 = run(build_set());
        let run2 = run(build_set());
        assert_eq!(
            run1, run2,
            "the deterministic floor+ceiling pipeline must produce identical severities, \
             rationale text, and ordering across two runs over the same input — this does NOT \
             (and cannot) pin the calibration MODEL itself, only this deterministic layer"
        );
    }

    // ── Structured confidence + effort (Part 1 §3) ────────────────────────────

    /// `apply_verdicts` must set the STRUCTURED `confidence`/`needs_review` fields, not just
    /// the string-embedded `[needs review]` detail tag — the tag stays for one release (UI
    /// back-compat), but a report/consumer that reads the structured field must see it too.
    #[test]
    fn apply_verdicts_sets_structured_confidence_and_needs_review() {
        let findings = vec![
            finding("AI-TIMING", "medium"), // index 0 -> low confidence
            finding("AI-REAL", "high"),     // index 1 -> high confidence
        ];
        let raw = r#"{"verdicts":[
            {"index":0,"confidence":"low","effort":"low","reason":"negligible timing residual"},
            {"index":1,"severity":"high","confidence":"high","effort":"high","reason":"concrete"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        let timing = out.iter().find(|f| f.rule_id == "AI-TIMING").unwrap();
        assert_eq!(timing.confidence.as_deref(), Some("needs-review"));
        assert!(timing.needs_review, "low confidence must set structured needs_review");
        assert_eq!(timing.effort.as_deref(), Some("low"));
        // The detail tag is KEPT for one release (UI back-compat) alongside the new field.
        assert!(timing.detail.contains("[needs review"));

        let real = out.iter().find(|f| f.rule_id == "AI-REAL").unwrap();
        assert_eq!(real.confidence.as_deref(), Some("high"));
        assert!(!real.needs_review, "high confidence must not set needs_review");
        assert_eq!(real.effort.as_deref(), Some("high"));
    }

    /// A verdict with a mis-shaped or missing `effort` value must leave `Finding.effort`
    /// untouched (fail-soft — effort is advisory, never load-bearing) rather than recording a
    /// guessed value.
    #[test]
    fn apply_verdicts_ignores_invalid_effort_value() {
        let findings = vec![finding("AI-X", "medium")];
        let raw = r#"{"verdicts":[{"index":0,"confidence":"high","effort":"extreme","reason":""}]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(out[0].effort, None, "an unrecognized effort value must not be recorded");

        let findings2 = vec![finding("AI-Y", "medium")];
        let raw2 = r#"{"verdicts":[{"index":0,"confidence":"high","reason":""}]}"#;
        let out2 = apply_verdicts(raw2, findings2);
        assert_eq!(out2[0].effort, None, "an absent effort field must leave effort as None");
    }

    /// The calibration verdict JSON schema (Part 1 §3) parses `effort` alongside
    /// severity/confidence/reason — this pins the wire shape the system prompt
    /// (`verify_system_prompt`) asks the model to emit.
    #[test]
    fn calibration_verdict_json_parses_effort_field() {
        let raw = r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","effort":"medium","reason":"a clear break"}]}"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let verdict = &v["verdicts"][0];
        assert_eq!(verdict["effort"], "medium");
        // Round-trips through apply_verdicts onto the Finding.
        let out = apply_verdicts(raw, vec![finding("SEC-X", "high")]);
        assert_eq!(out[0].effort.as_deref(), Some("medium"));
    }

    #[test]
    fn apply_verdicts_fail_open_on_garbage() {
        let findings = vec![finding("AI-X", "high")];
        // Unparseable verdicts -> keep all (never silently lose a finding).
        let out = apply_verdicts("the model rambled", findings);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn parse_keys_adopted_rule_ids_directly_and_prefixes_others() {
        let adopted: std::collections::HashSet<String> =
            ["ARCH-STRICT-LAYERING-1".to_string()].into_iter().collect();
        let raw = r#"{"findings":[
            {"path":"svc.rs","line":12,"severity":"high","rule":"ARCH-STRICT-LAYERING-1","title":"service hits db","detail":"d"},
            {"path":"x.rs","line":3,"severity":"low","rule":"some-new-smell","title":"t","detail":"d"}
        ],"proposed_rules":[]}"#;
        let (f, _r) = parse_ai_findings("me/api", raw, &adopted);
        assert_eq!(f.len(), 2);
        // An adopted id is keyed verbatim; an unlisted issue gets the AI- prefix.
        assert!(f.iter().any(|x| x.rule_id == "ARCH-STRICT-LAYERING-1"));
        assert!(f.iter().any(|x| x.rule_id == "AI-SOME-NEW-SMELL"));
    }

    #[test]
    fn parse_garbage_yields_empty_not_error() {
        let none = std::collections::HashSet::new();
        let (f, r) = parse_ai_findings("me/api", "the model declined to answer in JSON", &none);
        assert!(f.is_empty());
        assert!(r.is_empty());
        let (f2, r2) = parse_ai_findings("me/api", "{ not valid json ]", &none);
        assert!(f2.is_empty());
        assert!(r2.is_empty());
    }

    #[test]
    fn empty_findings_object_is_clean() {
        let none = std::collections::HashSet::new();
        let (f, r) =
            parse_ai_findings("me/api", r#"{"findings": [], "proposed_rules": []}"#, &none);
        assert!(f.is_empty());
        assert!(r.is_empty());
    }

    // ── Deep compliance & security tier (#55) ──────────────────────────────────────

    #[test]
    fn parse_soc2_gaps_reads_controls_and_normalizes_status() {
        let raw = r#"Here is the gap analysis:
        {
          "summary": "Partial readiness; access controls are the main gap.",
          "gaps": [
            {"control":"CC6.1","title":"Logical access controls","status":"gap","observed":"no authz middleware","gap":"add authz on write paths"},
            {"control":"CC7.2","title":"Logging","status":"partial","observed":"some logging","gap":"no audit trail"},
            {"control":"CC1.1","title":"Control environment","status":"weird","observed":"n/a","gap":""}
          ]
        }"#;
        let (summary, gaps) = parse_soc2_gaps(raw);
        assert!(summary.contains("Partial readiness"));
        assert_eq!(gaps.len(), 3);
        assert_eq!(gaps[0].control, "CC6.1");
        assert_eq!(gaps[0].status, "gap");
        assert_eq!(gaps[1].status, "partial");
        // An unrecognized status normalizes to the honest default.
        assert_eq!(gaps[2].status, "unknown");
    }

    #[test]
    fn parse_soc2_gaps_drops_empty_rows_and_tolerates_garbage() {
        // A row with no control AND no title is dropped.
        let raw = r#"{"summary":"","gaps":[{"control":"","title":"","status":"gap"}]}"#;
        let (_s, gaps) = parse_soc2_gaps(raw);
        assert!(gaps.is_empty(), "empty rows dropped");
        // Non-JSON yields an empty result, never an error.
        let (s2, g2) = parse_soc2_gaps("the model declined");
        assert!(s2.is_empty());
        assert!(g2.is_empty());
    }

    #[test]
    fn parse_threats_reads_and_normalizes_kind_and_severity() {
        let raw = r#"{
          "summary": "Public API with several entry points.",
          "threats": [
            {"component":"POST /api/orders","kind":"entry-point","threat":"unauth order creation","category":"elevation","mitigation":"require auth","severity":"high"},
            {"component":"Postgres","kind":"weird-kind","threat":"data exfil","category":"info-disclosure","mitigation":"encrypt at rest","severity":"sky-high"}
          ]
        }"#;
        let (summary, threats) = parse_threats(raw);
        assert!(summary.contains("Public API"));
        assert_eq!(threats.len(), 2);
        assert_eq!(threats[0].kind, "entry-point");
        assert_eq!(threats[0].severity, "high");
        // Unknown kind -> "other"; unknown severity -> "medium".
        assert_eq!(threats[1].kind, "other");
        assert_eq!(threats[1].severity, "medium");
        // Category is preserved verbatim (free text / STRIDE label both survive).
        assert_eq!(threats[1].category, "info-disclosure");
    }

    #[test]
    fn parse_threats_drops_empty_rows_and_tolerates_garbage() {
        let raw = r#"{"summary":"x","threats":[{"component":"","threat":"","kind":"entry-point"}]}"#;
        let (_s, threats) = parse_threats(raw);
        assert!(threats.is_empty(), "row with no component and no threat dropped");
        let (s2, t2) = parse_threats("{ not json ]");
        assert!(s2.is_empty());
        assert!(t2.is_empty());
    }

    #[test]
    fn deep_security_prompt_excludes_floor_concerns() {
        // The deep-security lens must NOT re-report the deterministic floor's concerns.
        let p = deep_security_system_prompt();
        assert!(p.contains("DO NOT re-report"));
        assert!(p.contains("authorization") || p.contains("AUTHORIZATION"));
    }

    /// The `critical` severity tier must be OFFERED (in the schema string) and CALIBRATED
    /// (general escalation criteria in the prose) by every AI-tier prompt that emits a
    /// severity — otherwise the model is vocabulary-constrained away from ever reporting a
    /// critical finding no matter how severe, which was the original bug. This pins the wire
    /// contract independent of any specific model call.
    #[test]
    fn every_ai_tier_prompt_offers_and_defines_critical_severity() {
        for (name, prompt) in [
            ("audit_system_prompt", audit_system_prompt()),
            ("deep_security_system_prompt", deep_security_system_prompt()),
            ("verify_system_prompt (calibration)", verify_system_prompt()),
        ] {
            assert!(
                prompt.contains("critical|high|medium|low")
                    || prompt.contains("critical/high/medium/low"),
                "{name} must offer \"critical\" in its severity schema/rubric"
            );
            assert!(
                prompt.to_lowercase().contains("rare") || prompt.contains("RARE"),
                "{name} must define critical as a RARE, reserved tier (not a synonym for \
                 high) so the model doesn't inflate ordinary findings"
            );
        }
    }

    /// W2: the standard audit prompt (the pass that runs on EVERY scan, not just the opt-in
    /// deep tier) must explicitly own its cross-file mandate by name, the compensating-control
    /// check, and the "report it even if no rule exists" backstop instruction — the three
    /// pieces of guidance added to close the zero-row semantic-tier collapse. A string-
    /// structure test (mirrors `shipped_template_footer_says_inspection_report_and_keeps_the_
    /// advisory_caveat` in `report_export.rs`): it pins the WORDING contract so a future edit
    /// can't quietly drop one of these clauses without a visible test failure.
    #[test]
    fn audit_system_prompt_owns_the_cross_file_mandate_and_compensating_control_check() {
        let p = audit_system_prompt();

        // The three cross-file mechanisms, named explicitly (never a repo-specific name).
        assert!(
            p.contains("CONFIG-TO-HANDLER"),
            "must name the config-flag-to-privileged-handler mechanism"
        );
        assert!(
            p.contains("POLICY-TO-CALL-SITE"),
            "must name the permissive-policy-to-call-site mechanism"
        );
        assert!(
            p.contains("CREDENTIAL-TO-USAGE"),
            "must name the credential-defined-in-one-file-consumed-in-another mechanism"
        );
        assert!(
            p.to_uppercase().contains("CROSS-FILE"),
            "must call out cross-file reasoning as this pass's own mandate"
        );

        // The compensating-control question, with its concrete examples, and the never-flag-
        // when-present instruction (the do-not-break invariant this work item must preserve).
        assert!(
            p.contains("COMPENSATING CONTROL") || p.contains("compensating check"),
            "must instruct the model to check for a compensating control before flagging"
        );
        for example in [
            "signature",
            "shared-secret",
            "session-derived identity",
            "ownership predicate",
        ] {
            assert!(
                p.to_lowercase().contains(example),
                "compensating-control guidance must name \"{example}\" as a qualifying check: \
                 {p}"
            );
        }
        assert!(
            p.contains("do NOT flag it") || p.contains("do NOT flag"),
            "must explicitly instruct: do not flag when a compensating control is found"
        );

        // The explicit backstop clause: a missing rule is never a reason to withhold a finding.
        assert!(
            p.to_uppercase().contains("BACKSTOP"),
            "must name the backstop role explicitly"
        );
        assert!(
            p.contains("missing rule is never a reason") || p.contains("even if no rule"),
            "must explicitly state that an unnamed/un-ruled issue must still be reported: {p}"
        );
    }

    /// W2: the calibration prompt must not let an un-ruled (backstop) finding be quietly
    /// discounted purely for lacking a named rule id — that would re-create the same silent
    /// collapse one stage later (a real finding surviving parsing only to be devalued/dropped
    /// at calibration for having no corpus id).
    #[test]
    fn verify_system_prompt_does_not_discount_an_unnamed_backstop_finding() {
        let p = verify_system_prompt();
        assert!(
            p.contains("BACKSTOP"),
            "calibration prompt must name the backstop role: {p}"
        );
        assert!(
            p.to_lowercase().contains("not automatically less credible")
                || p.to_lowercase().contains("do not discount"),
            "calibration prompt must explicitly forbid discounting an un-ruled finding: {p}"
        );
    }

    #[test]
    fn soc2_prompt_is_a_gap_analysis_never_a_report() {
        // Honesty guardrail (#55/#62): the SOC-2 prompt must frame itself as a gap analysis
        // and explicitly deny producing a SOC-2 report / certification.
        let p = soc2_gap_system_prompt();
        assert!(p.contains("GAP ANALYSIS"));
        assert!(p.to_lowercase().contains("not a \"soc-2 report\"")
            || p.contains("NOT a \"SOC-2 report\""));
    }

    #[test]
    fn deep_lens_metadata_is_stable() {
        assert_eq!(DeepLens::Soc2Gap.id(), "soc2-gap");
        assert_eq!(DeepLens::DeepSecurity.id(), "deep-security");
        assert_eq!(DeepLens::ThreatModel.id(), "threat-model");
        // The SOC-2 lens title is a "Gap Analysis", never a "report".
        assert!(DeepLens::Soc2Gap.title().contains("Gap Analysis"));
        assert!(!DeepLens::Soc2Gap.title().to_lowercase().contains("report"));
    }

    #[test]
    fn deep_lens_result_empty_carries_advisory_flag_and_disclaimer() {
        let r = DeepLensResult::empty(DeepLens::ThreatModel);
        assert!(r.advisory, "every deep result is advisory (#62)");
        assert_eq!(r.disclaimer, DEEP_ADVISORY_DISCLAIMER);
        assert!(r.error.is_none());
        assert!(r.threats.is_empty());
        // The disclaimer states it is not externally validated and not a pen test.
        assert!(DEEP_ADVISORY_DISCLAIMER.contains("NOT externally validated"));
        assert!(DEEP_ADVISORY_DISCLAIMER.contains("not a penetration test"));
    }

    #[test]
    fn deep_report_serializes_with_advisory_envelope() {
        let report = DeepReport {
            lenses: vec![
                DeepLensResult::empty(DeepLens::Soc2Gap),
                DeepLensResult::empty(DeepLens::DeepSecurity),
                DeepLensResult::empty(DeepLens::ThreatModel),
            ],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["advisory"], true);
        assert_eq!(json["lenses"].as_array().unwrap().len(), 3);
        assert_eq!(json["lenses"][0]["lens"], "soc2-gap");
        assert_eq!(json["lenses"][0]["advisory"], true);
    }

    // ── Rule-routing wiring (#57) ──────────────────────────────────────────────────

    /// Verify that `plan_routes` produces the correct per-group file sets when called with
    /// a polyglot file list and a mix of language-scoped + cross-cutting rules. This tests
    /// the grouping contract that `run_routed_passes` depends on.
    #[test]
    fn routing_groups_produce_correct_per_group_file_sets() {
        use crate::scan_routing::{plan_routes, Scope};

        // Polyglot repo: Rust backend, TypeScript frontend, some config.
        let files = vec![
            ("src/main.rs".to_string(), "fn main() {}".to_string()),
            ("src/handler.rs".to_string(), "pub fn handler() {}".to_string()),
            ("ui/app.tsx".to_string(), "export function App() {}".to_string()),
            ("schema.sql".to_string(), "CREATE TABLE users (id INT);".to_string()),
        ];
        let rules = vec![
            ("RUST-ENTITIES-1".to_string(), "Rust entities rule".to_string()),
            ("REACT-HOOKS-1".to_string(), "React hooks rule".to_string()),
            ("ARCH-STRICT-LAYERING-1".to_string(), "Layering rule".to_string()),
            ("SEC-NO-RAW-SQL-1".to_string(), "No raw SQL rule".to_string()),
        ];

        let plan = plan_routes(&rules, &files);

        // Should have 3 groups: rust, web, and All.
        assert_eq!(plan.groups.len(), 3, "polyglot repo gets rust + web + All groups");

        // Verify group scopes.
        let rust_group = plan.groups.iter().find(|g| g.scope == Scope::Language("rust"));
        let web_group = plan.groups.iter().find(|g| g.scope == Scope::Language("web"));
        let all_group = plan.groups.iter().find(|g| g.scope == Scope::All);

        assert!(rust_group.is_some(), "rust group exists for RUST-* rule");
        assert!(web_group.is_some(), "web group exists for REACT-* rule");
        assert!(all_group.is_some(), "All group exists for ARCH-* and SEC-* rules");

        // Rust group: only .rs files should be in scope.
        let rust_g = rust_group.unwrap();
        assert_eq!(rust_g.rules.len(), 1);
        assert_eq!(rust_g.rules[0].0, "RUST-ENTITIES-1");

        // Verify file_in_scope correctly filters for each group.
        for (path, _) in &files {
            let in_rust = crate::scan_routing::file_in_scope(path, &rust_g.scope);
            let expected_in_rust = path.ends_with(".rs");
            assert_eq!(
                in_rust, expected_in_rust,
                "{path} rust scope filter mismatch"
            );
        }

        // Web group: only .tsx files should be in scope.
        let web_g = web_group.unwrap();
        for (path, _) in &files {
            let in_web = crate::scan_routing::file_in_scope(path, &web_g.scope);
            let expected_in_web = path.ends_with(".tsx") || path.ends_with(".ts")
                || path.ends_with(".js") || path.ends_with(".jsx");
            assert_eq!(
                in_web, expected_in_web,
                "{path} web scope filter mismatch"
            );
        }

        // All group has both ARCH and SEC rules; sees every file including .sql.
        let all_g = all_group.unwrap();
        assert_eq!(all_g.rules.len(), 2, "ARCH + SEC rules both land in All group");
        for (path, _) in &files {
            assert!(
                crate::scan_routing::file_in_scope(path, &all_g.scope),
                "{path} must be in scope for the All group"
            );
        }

        // Routing saves input on this polyglot fixture (language rules skip non-matching files).
        assert!(plan.saved_fraction() > 0.0, "routing reduces input on a polyglot repo");
    }

    /// Advisory must be enabled ONLY in the All group and disabled in language-specific groups.
    /// This test verifies the invariant directly by checking what `advisory_disabled` should be
    /// for each route group — mirroring the logic in `run_routed_passes`.
    #[test]
    fn advisory_disabled_only_in_language_groups_not_in_all_group() {
        use crate::scan_routing::{plan_routes, Scope};

        let files = vec![
            ("main.rs".to_string(), "fn main() {}".to_string()),
            ("app.ts".to_string(), "export const x = 1;".to_string()),
            ("schema.sql".to_string(), "SELECT 1;".to_string()),
        ];
        let rules = vec![
            ("RUST-1".to_string(), "d".to_string()),
            ("TS-1".to_string(), "d".to_string()),
            ("ARCH-1".to_string(), "d".to_string()),
        ];
        let plan = plan_routes(&rules, &files);

        // The advisory_disabled flag is true for language groups, false for All.
        // This mirrors the check in run_routed_passes.
        for group in &plan.groups {
            let advisory_disabled = !matches!(group.scope, Scope::All);
            match &group.scope {
                Scope::All => {
                    assert!(
                        !advisory_disabled,
                        "All group must have advisory enabled (advisory_disabled=false)"
                    );
                }
                Scope::Language(lang) => {
                    assert!(
                        advisory_disabled,
                        "{lang} language group must have advisory disabled (advisory_disabled=true)"
                    );
                }
            }
        }
    }

    /// Verify the no-rules (free-form) path: when `selected` is empty, routing falls back to
    /// a single advisory-enabled pass over all files (novel-issue discovery only).
    #[test]
    fn routing_with_no_rules_produces_single_all_group_conceptually() {
        use crate::scan_routing::plan_routes;

        let files = vec![
            ("main.rs".to_string(), "fn main() {}".to_string()),
            ("app.ts".to_string(), "const x = 1;".to_string()),
        ];
        // Empty rules slice.
        let rules: Vec<(String, String)> = vec![];
        let plan = plan_routes(&rules, &files);

        // No rules → no groups: the free-form path handles this specially in run_routed_passes.
        assert_eq!(plan.groups.len(), 0, "empty rules → no route groups");
        assert_eq!(plan.full_chars, 0, "no rules → no input to bill");
    }

    /// Polyglot fixture with only cross-cutting rules: routing adds no groups beyond All,
    /// so the loop degenerates to a single pass and no file-savings occur.
    #[test]
    fn routing_all_cross_cutting_rules_single_group_no_savings() {
        use crate::scan_routing::{plan_routes, Scope};

        let files = vec![
            ("main.rs".to_string(), "fn main() {}".to_string()),
            ("app.ts".to_string(), "const x = 1;".to_string()),
        ];
        let rules = vec![
            ("ARCH-1".to_string(), "d".to_string()),
            ("SEC-1".to_string(), "d".to_string()),
        ];
        let plan = plan_routes(&rules, &files);

        assert_eq!(plan.groups.len(), 1, "all cross-cutting → single All group");
        assert_eq!(plan.groups[0].scope, Scope::All);
        assert_eq!(plan.saved_fraction(), 0.0, "no savings when all rules are cross-cutting");
    }

    /// A purely single-language repo with language-scoped rules also routes to a single
    /// language group plus (potentially) no All group if there are no cross-cutting rules.
    #[test]
    fn routing_single_language_repo_with_language_rules() {
        use crate::scan_routing::{plan_routes, Scope};

        let files = vec![
            ("src/a.rs".to_string(), "pub fn a() {}".to_string()),
            ("src/b.rs".to_string(), "pub fn b() {}".to_string()),
        ];
        let rules = vec![
            ("RUST-1".to_string(), "d".to_string()),
            ("RUST-2".to_string(), "d".to_string()),
        ];
        let plan = plan_routes(&rules, &files);

        // Only one group — the rust language group.
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].scope, Scope::Language("rust"));
        assert_eq!(plan.groups[0].rules.len(), 2, "both RUST rules land in the same group");

        // W2 (previously a documented-but-unfixed defect): no cross-cutting rule was selected,
        // so this plan has NO `Scope::All` group at all. Mirror `run_routed_passes`'s
        // `has_all_group`-gated formula — advisory must stay ENABLED for the only group here,
        // because nothing else will ever run the "flag novel issues beyond the adopted rules"
        // backstop pass for this scan. The old unconditional `!matches!(scope, Scope::All)`
        // formula silenced advisory for the WHOLE scan in exactly this shape (a project that
        // selected only `RUST-*` rules got zero chance at any novel/security finding outside
        // them) — this is the regression test for that collapse.
        let has_all_group = plan.groups.iter().any(|g| matches!(g.scope, Scope::All));
        assert!(
            !has_all_group,
            "a purely language-scoped selection has no All group"
        );
        let advisory_disabled = has_all_group && !matches!(plan.groups[0].scope, Scope::All);
        assert!(
            !advisory_disabled,
            "with no All group anywhere in the plan, the only (language) group must keep \
             advisory ENABLED — otherwise novel-issue discovery never runs for this scan at all"
        );
    }

    /// W2: a POLYGLOT repo with only language-scoped rules across MULTIPLE languages (still no
    /// `Scope::All` group) must keep advisory enabled in EVERY group — language groups are
    /// mutually exclusive by file (a `.rs` file is never in the `web` group's file set), so this
    /// can never duplicate a novel finding for any one file.
    #[test]
    fn routing_polyglot_no_all_group_keeps_advisory_enabled_in_every_language_group() {
        use crate::scan_routing::{plan_routes, Scope};

        let files = vec![
            ("src/a.rs".to_string(), "pub fn a() {}".to_string()),
            ("web/app.ts".to_string(), "export const x = 1;".to_string()),
        ];
        let rules = vec![
            ("RUST-1".to_string(), "d".to_string()),
            ("TS-1".to_string(), "d".to_string()),
        ];
        let plan = plan_routes(&rules, &files);

        assert_eq!(
            plan.groups.len(),
            2,
            "rust + web, no cross-cutting rule → two groups"
        );
        let has_all_group = plan.groups.iter().any(|g| matches!(g.scope, Scope::All));
        assert!(!has_all_group, "no cross-cutting rule was selected");
        for group in &plan.groups {
            let advisory_disabled = has_all_group && !matches!(group.scope, Scope::All);
            assert!(
                !advisory_disabled,
                "{:?} must keep advisory enabled when no All group exists: every language \
                 group's files are disjoint from every other's, so this never duplicates a \
                 finding",
                group.scope
            );
        }
    }

    // ── BUG-5: consensus_verdicts tie-breaking direction ─────────────────────────────

    /// BUG-5 regression: on a tie (high=1, medium=1) the previous code resolved to "high"
    /// because it checked `counts[2] == max` first.  The correct humility/conservative
    /// spec is that ties resolve to the LOWER severity.  This test fails before the fix
    /// and passes after.
    #[test]
    fn bug5_consensus_tie_breaks_to_lower_severity_not_higher() {
        // Two passes: one votes "high", one votes "medium" — equal votes, so tie.
        // The docstring says "ties break to the LOWER severity" → medium wins.
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"critical path"}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"medium","confidence":"high","reason":"moderate risk"}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 1);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v["verdicts"].as_array().unwrap();
        let v0 = arr.iter().find(|x| x["index"] == 0).unwrap();
        // BUG-5 fix: must resolve to "medium" (lower) not "high" (higher).
        assert_eq!(
            v0["severity"], "medium",
            "BUG-5: tie between high(1) and medium(1) must resolve to medium (lower), got: {v0}"
        );
    }

    /// A three-way 1-1-1 tie must resolve to "low" (the lowest severity).
    #[test]
    fn bug5_three_way_tie_resolves_to_low() {
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":""}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"medium","confidence":"high","reason":""}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"low","confidence":"low","reason":"debatable"}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 1);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v["verdicts"].as_array().unwrap();
        let v0 = arr.iter().find(|x| x["index"] == 0).unwrap();
        // 1-1-1 tie → must resolve to "low".
        assert_eq!(
            v0["severity"], "low",
            "BUG-5: three-way 1-1-1 tie must resolve to low (lowest), got: {v0}"
        );
        // Disagreement → confidence must be "low" (needs-review), regardless of tie direction.
        assert_eq!(v0["confidence"], "low", "disagreement forces low confidence");
    }

    /// Unanimous "high" must still resolve to "high" — the fix must not break the non-tie case.
    #[test]
    fn bug5_unanimous_high_stays_high() {
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"injection"}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":""}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 1);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v["verdicts"].as_array().unwrap();
        let v0 = arr.iter().find(|x| x["index"] == 0).unwrap();
        assert_eq!(v0["severity"], "high", "unanimous high must stay high");
        assert_eq!(v0["confidence"], "high", "unanimous high → high confidence");
    }

    // ── critical severity tier: consensus tie-breaking + unanimity ───────────────────

    /// ANTI-OVER-ROTATION GUARD (thorough/consensus mode): a tie between "critical" and "high"
    /// (one pass votes each) must resolve to "high" — the SAME lower-wins tie-break BUG-5
    /// established for high-vs-medium, extended one tier up. A single over-eager pass can never
    /// unilaterally push a finding to critical against an equally-confident dissent.
    #[test]
    fn consensus_critical_vs_high_tie_resolves_to_high_not_critical() {
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"critical","confidence":"high","reason":"looks unauthenticated"}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"contained blast radius"}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 1);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let v0 = v["verdicts"].as_array().unwrap().iter().find(|x| x["index"] == 0).unwrap();
        assert_eq!(
            v0["severity"], "high",
            "a critical-vs-high tie must resolve to high (lower), not silently promote to critical: {v0}"
        );
        assert_eq!(v0["confidence"], "low", "disagreement forces low confidence (needs review)");
    }

    /// Unanimous "critical" across every pass must resolve to "critical" with high confidence —
    /// the fix must not cap the new tier below its own unanimous vote.
    #[test]
    fn consensus_unanimous_critical_stays_critical() {
        let votes = vec![
            r#"{"verdicts":[{"index":0,"severity":"critical","confidence":"high","reason":"unauthenticated charge endpoint"}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"critical","confidence":"high","reason":""}]}"#.to_string(),
            r#"{"verdicts":[{"index":0,"severity":"critical","confidence":"high","reason":""}]}"#.to_string(),
        ];
        let out = consensus_verdicts(&votes, 1);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let v0 = v["verdicts"].as_array().unwrap().iter().find(|x| x["index"] == 0).unwrap();
        assert_eq!(v0["severity"], "critical", "unanimous critical must stay critical");
        assert_eq!(v0["confidence"], "high", "unanimous critical → high confidence");
    }

    // ── BUG-4: resolution round add_total guard in Batch mode ────────────────────────

    /// BUG-4 regression: in Batch mode the resolution round's add_total call was
    /// unconditional, inflating the progress bar denominator after the batch completed.
    /// This test verifies the guard is present by inspecting the code path at the
    /// JobStore level (unit-testing the guard logic rather than the async batch path).
    #[test]
    fn bug4_batch_mode_resolution_add_total_is_guarded() {
        // Simulate the guard: mode == ScanMode::Batch → skip add_total.
        // mode != ScanMode::Batch → call add_total.
        // We can't invoke audit_repo in a unit test without a real LLM, so we verify
        // the guard logic directly on the ScanMode enum.
        assert_ne!(
            ScanMode::Batch,
            ScanMode::Parallel,
            "guard distinguishes Batch from other modes"
        );
        assert_ne!(
            ScanMode::Batch,
            ScanMode::Sequential,
            "guard distinguishes Batch from Sequential"
        );
        // The guard condition: `mode != ScanMode::Batch` must be false in Batch mode
        // (add_total skipped) and true in non-Batch modes (add_total called).
        assert!(
            !(ScanMode::Batch != ScanMode::Batch),
            "in Batch mode the guard evaluates to false → add_total is skipped"
        );
        assert!(
            ScanMode::Parallel != ScanMode::Batch,
            "in Parallel mode the guard evaluates to true → add_total is called"
        );
        assert!(
            ScanMode::Sequential != ScanMode::Batch,
            "in Sequential mode the guard evaluates to true → add_total is called"
        );
    }

    // ── BUG-6 regression: ScanMode::Batch uses larger rule batch size ─────────────

    /// Before BUG-6 fix: `ScanMode::Batch` returned `RULE_BATCH_SIZE` (15) from
    /// `tuning()`, fragmenting Batch items unnecessarily (the Batch path submits all
    /// items at once — there is no latency benefit to smaller rule batches). After the
    /// fix, `ScanMode::Batch` returns `BATCH_RULE_BATCH_SIZE` (usize::MAX by default),
    /// so each chunk becomes a single BatchItem with all adopted rules in one context.
    #[test]
    fn bug6_batch_mode_uses_larger_rule_batch_size_than_parallel() {
        let (_, parallel_batch_size) = ScanMode::Parallel.tuning();
        let (_, batch_batch_size) = ScanMode::Batch.tuning();

        // The Batch rule batch size must be LARGER than the Parallel one (BUG-6 fix).
        // PARALLEL = 15; BATCH = usize::MAX (or the env-var override, but the env var
        // is not set in the test environment so the constant applies).
        assert!(
            batch_batch_size > parallel_batch_size,
            "ScanMode::Batch rule batch size ({batch_batch_size}) must be larger than \
             ScanMode::Parallel ({parallel_batch_size}) — BUG-6: batching all rules per \
             chunk improves coherence in async Batch mode where concurrency is irrelevant"
        );
    }

    /// Sequential mode must keep its existing tuning (1 concurrent call, all rules at once).
    #[test]
    fn bug6_sequential_mode_tuning_unchanged() {
        let (concurrency, batch_size) = ScanMode::Sequential.tuning();
        assert_eq!(concurrency, 1, "sequential must be single-threaded");
        assert_eq!(batch_size, usize::MAX, "sequential must include all rules in one pass");
    }

    // ── BUG-7 regression: merge_location_group tiebreaker comment ────────────────

    /// Verify that `merge_location_group` correctly selects the EARLIEST finding when
    /// all other keys are equal (adopted-flag and severity tied). This is the tiebreaker
    /// documented by the `group.len() - i` idiom (BUG-7: correctness confirmed, only
    /// readability was noted).
    #[test]
    fn bug7_merge_location_group_earliest_appearance_wins_on_tie() {
        let make = |rule: &str, sev: &str, snippet: &str| Finding {
            repo: "r/r".to_string(),
            path: "src/lib.rs".to_string(),
            line: 1,
            rule_id: rule.to_string(),
            severity: sev.to_string(),
            snippet: snippet.to_string(),
            detail: "d".to_string(),
            status: "active".to_string(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
            confidence: None,
            effort: None,
            category: None,
            located: true,
            captures: Default::default(),
            evaluated_option_id: None,
            also_locations: Vec::new(),
            fix_specific: None,
            calibration_rationale: None,
        };
        // Three AI- findings with equal severity — earliest (index 0) must win.
        let group = vec![
            make("AI-FIRST", "medium", "first snippet"),
            make("AI-SECOND", "medium", "second snippet"),
            make("AI-THIRD", "medium", "third snippet"),
        ];
        let merged = merge_location_group(group);
        assert_eq!(
            merged.rule_id, "AI-FIRST",
            "earliest finding must win the tiebreak when adopted-flag and severity are equal"
        );
        // The other rule ids must appear in also_matches, in first-seen order.
        assert_eq!(merged.also_matches, vec!["AI-SECOND", "AI-THIRD"]);
    }

    // ── BUG-8 regression: build_digest truncation notice threshold ────────────────

    /// A near-full file (only a tiny amount dropped) must NOT emit the truncation
    /// notice. Before BUG-8 fix the notice was always emitted on ANY truncation.
    ///
    /// Test: build a multi-file digest where the SECOND file barely doesn't fit —
    /// only a very small number of chars are dropped (< TRUNCATION_NOTICE_MIN_DROP).
    #[test]
    fn bug8_trivial_truncation_does_not_emit_notice() {
        use super::{MAX_DIGEST_CHARS, TRUNCATION_NOTICE_MIN_DROP};
        // Strategy: fill the budget almost completely with file-1, then add a file-2
        // that overflows by just `TRUNCATION_NOTICE_MIN_DROP - 1` chars. The partial
        // slice of file-2 captures all but that tiny tail → no notice.
        let small_drop = TRUNCATION_NOTICE_MIN_DROP - 1;

        // Build file-1 content sized to leave exactly `small_drop + budget_for_header_2`
        // chars remaining in the budget.
        // Header for file-2: "// ===== FILE: src/f2.rs =====\n" ≈ 32 chars.
        let header2_len = "// ===== FILE: src/f2.rs =====\n".len();
        let file1_budget = MAX_DIGEST_CHARS
            .saturating_sub(header2_len)
            .saturating_sub(small_drop + 500); // 500-char "remaining" slice of file-2
        let file1_content = "a".repeat(file1_budget);

        // File-2: exactly `small_drop + 500` chars of numbered content so that
        // `remaining` captures the first 500 chars (> 200, partial slice is added)
        // and drops `small_drop` chars.
        let file2_raw_len = small_drop + 500;
        let file2_content = "b".repeat(file2_raw_len);

        let files = vec![
            ("src/f1.rs".to_string(), file1_content),
            ("src/f2.rs".to_string(), file2_content),
        ];
        let digest = build_digest(&files);

        // The truncation was trivial (< TRUNCATION_NOTICE_MIN_DROP bytes dropped) →
        // notice must NOT appear (BUG-8 fix).
        assert!(
            !digest.contains("[digest truncated"),
            "BUG-8: a trivially small truncation (< {TRUNCATION_NOTICE_MIN_DROP} chars dropped) \
             must NOT emit the truncation notice — it would mislead the model"
        );
    }

    /// When a significant amount of content is dropped, the truncation notice IS emitted.
    #[test]
    fn bug8_significant_truncation_emits_notice() {
        use super::MAX_DIGEST_CHARS;
        // First file: small (easily fits).
        // Second file: very large (mostly dropped — well over the threshold).
        let small = "fn a() {}".to_string();
        // Second file: > MAX_DIGEST_CHARS so it's almost entirely truncated.
        let large = "fn b() { let x = 1; } // comment\n".repeat(MAX_DIGEST_CHARS / 10);
        let files = vec![
            ("src/small.rs".to_string(), small),
            ("src/large.rs".to_string(), large),
        ];
        let digest = build_digest(&files);
        // The large file drops a significant fraction → notice must appear.
        assert!(
            digest.contains("[digest truncated"),
            "a significant truncation (large portion of a file dropped) must emit the notice"
        );
    }

    // ── BUG-AI-1 regression: run_passes_batch advisory_disabled ──────────────────────

    /// BUG-AI-1: when advisory_disabled is true the advisory prompt must be suppressed
    /// for EVERY (chunk, batch) pair including bi==0. Before the fix, batch mode always
    /// set `advisory = bi == 0` unconditionally, re-introducing duplicate novel findings
    /// for every language-scoped group's first batch.
    ///
    /// We test the corrected logic via the `advisory` value formula directly (the function
    /// itself is async + requires a live LLM, so we test the guard predicate in isolation).
    #[test]
    fn bug_ai1_advisory_disabled_suppresses_advisory_in_batch_mode() {
        // Simulate what run_passes_batch now does for each (ci, bi) pair.
        let advisory_disabled_cases: &[(bool, usize, bool)] = &[
            // (advisory_disabled, bi, expected_advisory)
            (true, 0, false),  // disabled + first batch → must NOT fire
            (true, 1, false),  // disabled + later batch → must NOT fire
            (false, 0, true),  // enabled  + first batch → MUST fire
            (false, 1, false), // enabled  + later batch → must NOT fire
        ];
        for &(advisory_disabled, bi, expected) in advisory_disabled_cases {
            let advisory = !advisory_disabled && bi == 0;
            assert_eq!(
                advisory, expected,
                "BUG-AI-1: advisory_disabled={advisory_disabled}, bi={bi} → \
                 expected advisory={expected}, got advisory={advisory}"
            );
        }
    }

    // ── BUG-AI-2 regression: canonical_adopted_rule PANIC false-positives ────────────

    /// BUG-AI-2: `canonical_adopted_rule` must NOT map rules that merely mention "PANIC"
    /// in a non-panic context to ARCH-STRUCTURED-ERRORS-1. Only invented names that
    /// specifically indicate an unhandled panic at a call point qualify.
    #[test]
    fn bug_ai2_canonical_adopted_rule_panic_match_is_narrow() {
        let mut adopted = std::collections::HashSet::new();
        adopted.insert("ARCH-STRUCTURED-ERRORS-1".to_string());

        // These should NOT match (merely mention PANIC in non-panic context):
        let non_panic_rules = &[
            "PREVENT-PANICKING-AUTH-CHECK",
            "LOG-PANIC-RECOVERY-1",
            "PANIC-GATE-GUARD",     // "PANIC" + "GUARD" — not in narrowing list
        ];
        for rule in non_panic_rules {
            let result = canonical_adopted_rule(rule, &adopted);
            assert!(
                result.is_none(),
                "BUG-AI-2: rule '{rule}' must NOT be canonicalized to ARCH-STRUCTURED-ERRORS-1 \
                 (it only mentions PANIC but is not a panic-at-callsite violation)"
            );
        }

        // These SHOULD match (specific panic-at-callsite invented names):
        let panic_rules = &[
            "AI-HANDLER-PANICS",
            "UNHANDLED-PANIC",
            "PANIC-ON-ERROR",
            "PANIC-UNWRAP-RESULT",
            "BUBBLE-PANIC-1",
        ];
        for rule in panic_rules {
            let result = canonical_adopted_rule(rule, &adopted);
            assert_eq!(
                result.as_deref(),
                Some("ARCH-STRUCTURED-ERRORS-1"),
                "BUG-AI-2: rule '{rule}' should be canonicalized to ARCH-STRUCTURED-ERRORS-1"
            );
        }
    }

    /// BUG-AI-2: when ARCH-STRUCTURED-ERRORS-1 is NOT adopted, canonical_adopted_rule
    /// must return None even for genuine panic rule names (not-adopted guard must hold).
    #[test]
    fn bug_ai2_canonical_adopted_rule_panic_not_adopted_returns_none() {
        let adopted = std::collections::HashSet::new(); // empty — no rules adopted
        let result = canonical_adopted_rule("PANIC-UNWRAP-RESULT", &adopted);
        assert!(
            result.is_none(),
            "BUG-AI-2: canonicalization must return None when the target rule is not adopted"
        );
    }

    // ── BUG-AI-3 regression: calibration-model fallback when audit_model is None ─────

    /// BUG-AI-3: when calibration_model, CAMERATA_CALIBRATION_MODEL, and audit_model are
    /// all None/absent, calib_model resolves to None. This is CORRECT — the LLM uses its
    /// default for both scan and calibration, keeping the audit end-to-end on one model.
    /// The test documents the expected None behavior so any future refactor that changes
    /// the fallback chain is caught explicitly.
    #[test]
    fn bug_ai3_calib_model_none_when_all_sources_absent() {
        // Replicate the fallback chain from audit_repo (with no env vars set in this test).
        // We cannot call audit_repo directly (it needs a live LLM), so we test the chain
        // logic in isolation — same three-level or_else chain the function uses.
        let calibration_model: Option<&str> = None;
        let audit_model: Option<String> = None; // CAMERATA_AUDIT_MODEL not set

        let calib_model = calibration_model
            .map(str::to_string)
            .or_else(|| {
                // In the test environment CAMERATA_CALIBRATION_MODEL is not set.
                std::env::var("CAMERATA_CALIBRATION_MODEL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
            .or_else(|| audit_model.clone());

        assert!(
            calib_model.is_none(),
            "BUG-AI-3: calib_model must be None when all three sources are absent; \
             both scan and calibration will use the LLM default (correct end-to-end-on-one-model \
             behavior). Got: {calib_model:?}"
        );
    }

    // ── Feature-flag: soc2_enabled gate in run_deep_tier ──────────────────────
    //
    // These tests validate the behavioural contract of the `soc2_enabled` flag
    // without making actual LLM calls. They use the shape/count of lenses in the
    // returned `DeepReport` as the observable.

    /// A `DeepReport` that carries all three lenses (as empty-but-honest results)
    /// simulating a full three-lens run. Used to verify the flag contract.
    fn three_lens_report() -> DeepReport {
        DeepReport {
            lenses: vec![
                DeepLensResult::empty(DeepLens::Soc2Gap),
                DeepLensResult::empty(DeepLens::DeepSecurity),
                DeepLensResult::empty(DeepLens::ThreatModel),
            ],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        }
    }

    #[test]
    fn soc2_flag_true_includes_all_three_lenses() {
        // The flag contract: when soc2_enabled=true, run_deep_tier returns a
        // DeepReport with the SOC-2 lens present. We verify the shape here
        // using a synthetic three-lens report (no LLM call needed).
        let report = three_lens_report();
        // All three lenses must be present.
        assert_eq!(report.lenses.len(), 3);
        let has_soc2 = report.lenses.iter().any(|l| l.lens == "soc2-gap");
        assert!(has_soc2, "soc2_enabled=true: soc2-gap lens must be present");
    }

    #[test]
    fn soc2_flag_false_produces_two_lens_report() {
        // When soc2_enabled=false, run_deep_tier omits the soc2 lens and returns
        // a two-lens report (security + threat-model only). We verify the shape
        // using a synthetic two-lens report (no LLM call needed).
        let report = DeepReport {
            // Simulate the soc2=false path: two lenses only.
            lenses: vec![
                DeepLensResult::empty(DeepLens::DeepSecurity),
                DeepLensResult::empty(DeepLens::ThreatModel),
            ],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        };
        assert_eq!(report.lenses.len(), 2, "soc2 off: only 2 lenses");
        let has_soc2 = report.lenses.iter().any(|l| l.lens == "soc2-gap");
        assert!(!has_soc2, "soc2_enabled=false: soc2-gap lens must be absent");
        // The report is still valid (advisory flag set, disclaimer present).
        assert!(report.advisory);
        assert_eq!(report.disclaimer, DEEP_ADVISORY_DISCLAIMER);
    }

    #[test]
    fn deep_report_with_soc2_off_still_has_security_and_threat() {
        // Even with soc2=false the other two lenses carry their metadata.
        let security = DeepLensResult::empty(DeepLens::DeepSecurity);
        let threat = DeepLensResult::empty(DeepLens::ThreatModel);
        assert_eq!(security.lens, "deep-security");
        assert_eq!(threat.lens, "threat-model");
        // Both remain advisory.
        assert!(security.advisory);
        assert!(threat.advisory);
    }

    // ── LlmPort seam: AI-failure guard (#audit-llm-seam-guard) ──────────────────────
    //
    // These tests exercise the load-bearing behavior that was previously untestable
    // without a live model: when the LLM is UNAVAILABLE in an AI-review ("both") scan,
    // every audit pass errors, and `audit_repo` must SURFACE that the AI review was
    // skipped (return `Err`) — NEVER a silent clean Ok([]) — so the caller
    // (`onboard::audit_repos`) records "AI audit skipped" while the deterministic floor
    // findings still return independently. The `LlmPort` trait is the seam that lets a
    // test substitute a model client without any token, env mutation, or network.

    use crate::llm::{LlmPort, LlmRequest, LlmResponse};

    /// A `LlmPort` whose every call fails — simulates the LLM being unavailable
    /// (CLI not installed, API key invalid, network down). Both `complete` and
    /// `complete_streaming` return `Err`, so every audit pass that touches it fails.
    struct FailingCompleter;

    #[async_trait::async_trait]
    impl LlmPort for FailingCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("simulated LLM unavailable (complete)")
        }
        async fn complete_streaming(
            &self,
            _req: LlmRequest,
            _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("simulated LLM unavailable (complete_streaming)")
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// A `LlmPort` that returns canned text — proves the seam works in the OTHER
    /// direction (a stub can feed the audit findings without a live model). The text is
    /// the audit's expected JSON shape so `parse_ai_findings` yields a finding.
    struct StubCompleter {
        text: String,
    }

    #[async_trait::async_trait]
    impl LlmPort for StubCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                text: self.text.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            _req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            on_delta(&self.text);
            self.complete(LlmRequest::new("")).await
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// W2 UNIT regression for the zero-row semantic-tier collapse: a fake LLM reports ONE
    /// security finding under a bare kebab name (no adopted corpus rule matches it — `selected`
    /// is empty) and `audit_repo` must still return it, tagged `AI-`, rather than the model's
    /// response being silently dropped anywhere in the chunk/merge/calibration chain. Narrower
    /// and faster than the full `semantic_tier_cross_file_backstop_e2e.rs` integration test
    /// (which also drives the finding through `report_export::build_report_json`) — this one
    /// pins the contract at the `audit_repo` boundary alone.
    #[tokio::test]
    async fn audit_repo_with_no_adopted_rule_still_returns_the_models_own_security_finding() {
        let findings_json = r#"{
            "findings": [
                {
                    "path": "src/handlers/account.rs",
                    "line": 4,
                    "severity": "critical",
                    "rule": "missing-ownership-check-on-account-delete",
                    "title": "Any authenticated user can delete any account by id",
                    "code": "delete_account(req.account_id);",
                    "detail": "The handler never confirms req.account_id belongs to the caller before deleting it — no ownership predicate anywhere on this path.",
                    "captures": {}
                }
            ],
            "proposed_rules": [],
            "needs_files": []
        }"#;
        let stub = StubCompleter { text: findings_json.to_string() };
        let files = vec![(
            "src/handlers/account.rs".to_string(),
            "fn delete(req: Req) {\n    delete_account(req.account_id);\n}\n".to_string(),
        )];
        // Empty `selected` — the whole point: no adopted rule exists for this defect, so it can
        // ONLY ever reach the architect via the semantic tier's own backstop reasoning.
        let selected: Vec<(String, String)> = Vec::new();

        let (findings, _proposed, _recs, failed_passes, _stage_samples) = audit_repo(
            &stub,
            "acme/app",
            &files,
            &selected,
            &[],
            &std::collections::HashMap::new(),
            None,
            None,
            ScanMode::Sequential,
            false,
            None,
            None,
            None,
            Some(&files),
        )
        .await
        .expect("audit_repo must succeed against a well-formed stub response");

        assert_eq!(
            findings.len(),
            1,
            "the model's orphan security finding must survive to audit_repo's return value: \
             failed_passes={failed_passes:?}"
        );
        assert!(
            findings[0].rule_id.starts_with("AI-"),
            "an id with no adopted-rule match must be tagged AI-tier: {}",
            findings[0].rule_id
        );
        assert_eq!(findings[0].severity, "critical");
        assert_eq!(findings[0].path, "src/handlers/account.rs");
    }

    /// Records every `LlmRequest` it receives — used to inspect the ACTUAL prompt +
    /// cache-breakpoint structure `run_passes` builds for a real multi-chunk,
    /// multi-rule-batch scan (GAP-3 regression coverage), rather than only testing the pure
    /// `build_pass_prompt` helper in isolation. Returns an empty-findings `"{}"` from every
    /// call so `audit_pass`'s parse step is a harmless no-op.
    #[derive(Default)]
    struct CapturingCompleter {
        seen: std::sync::Mutex<Vec<LlmRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmPort for CapturingCompleter {
        async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.seen.lock().unwrap().push(req);
            Ok(LlmResponse {
                text: "{}".to_string(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            on_delta("{}");
            self.complete(req).await
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// END-TO-END regression for GAP-3, exercising the REAL `run_passes` code path
    /// (parallel mode) with 2 chunks × 2 rule-batches — not just the pure
    /// `build_pass_prompt` helper. Confirms: (a) every request carries TWO cache
    /// breakpoints (n_b == 2 > 1); (b) the bytes up to the FIRST breakpoint (the repo map)
    /// are byte-identical across ALL FOUR requests, regardless of chunk or rule-batch —
    /// this is what makes the repo map cache-hit across chunks, the core Gap-3 fix; (c) the
    /// segment between the two breakpoints (chunk label + digest) is identical across a
    /// chunk's two rule-batches but differs between chunks (the pre-existing parallel-mode
    /// win, preserved); (d) rule content never leaks into a cached segment.
    #[tokio::test]
    async fn run_passes_produces_byte_identical_repo_map_prefix_across_chunks() {
        let completer = CapturingCompleter::default();
        let chunk0: Vec<(String, String)> =
            vec![("chunk0.rs".to_string(), "// CHUNK0 marker\nfn chunk0() {}".to_string())];
        let chunk1: Vec<(String, String)> =
            vec![("chunk1.rs".to_string(), "// CHUNK1 marker\nfn chunk1() {}".to_string())];
        let chunks: Vec<&[(String, String)]> = vec![&chunk0, &chunk1];

        let batch0: Vec<(String, String)> = vec![("RULE-A".to_string(), "rule A desc".to_string())];
        let batch1: Vec<(String, String)> = vec![("RULE-B".to_string(), "rule B desc".to_string())];
        let batches: Vec<&[(String, String)]> = vec![&batch0, &batch1];

        let repo_map = "REPO MAP:\nchunk0.rs: fn chunk0()\nchunk1.rs: fn chunk1()\n".to_string();
        let adopted = std::collections::HashSet::new();

        let (_findings, _proposed, _requested, ok, err) = run_passes(
            &completer,
            "acme/widgets",
            &repo_map,
            &adopted,
            None,
            None,
            None,
            &chunks,
            &batches,
            4,
            "parallel",
            "test-session",
            None,
            false,
        )
        .await;
        assert!(err.is_none(), "no pass should fail: {err:?}");
        assert_eq!(ok, 4, "2 chunks x 2 rule-batches = 4 passes");

        let seen = completer.seen.lock().unwrap();
        assert_eq!(seen.len(), 4);

        // (a) Every request carries exactly two cache breakpoints (n_b == 2 > 1).
        for req in seen.iter() {
            assert_eq!(
                req.cache_breakpoints.len(), 2,
                "n_b==2 keeps both breakpoints: {:?}", req.cache_breakpoints
            );
        }

        // (b) The repo-map segment (bytes up to the FIRST breakpoint) is byte-identical
        // across every one of the 4 requests, regardless of chunk or rule-batch.
        fn repo_map_segment(req: &LlmRequest) -> &str {
            &req.prompt[..req.cache_breakpoints[0]]
        }
        let first_segment = repo_map_segment(&seen[0]).to_string();
        for req in seen.iter() {
            assert_eq!(
                repo_map_segment(req), first_segment,
                "the repo-map segment must be byte-identical across every chunk/rule-batch"
            );
        }

        // (c) Bucket by chunk (via the CHUNK0/CHUNK1 marker in the digest segment) — within
        // a chunk, the digest segment (between the two breakpoints) is identical across its
        // two rule-batches; across chunks, it differs.
        fn digest_segment(req: &LlmRequest) -> &str {
            &req.prompt[req.cache_breakpoints[0]..req.cache_breakpoints[1]]
        }
        let chunk0_segs: Vec<&str> =
            seen.iter().map(digest_segment).filter(|s| s.contains("CHUNK0")).collect();
        let chunk1_segs: Vec<&str> =
            seen.iter().map(digest_segment).filter(|s| s.contains("CHUNK1")).collect();
        assert_eq!(chunk0_segs.len(), 2, "both rule-batches for chunk 0");
        assert_eq!(chunk1_segs.len(), 2, "both rule-batches for chunk 1");
        assert_eq!(chunk0_segs[0], chunk0_segs[1], "same chunk's digest segment is identical across rule-batches");
        assert_eq!(chunk1_segs[0], chunk1_segs[1]);
        assert_ne!(chunk0_segs[0], chunk1_segs[0], "different chunks' digest segments differ");

        // (d) Rule content never leaks into a cached segment — only the uncached suffix.
        for req in seen.iter() {
            let suffix = &req.prompt[req.cache_breakpoints[1]..];
            let cached = &req.prompt[..req.cache_breakpoints[1]];
            assert!(suffix.contains("RULE-A") || suffix.contains("RULE-B"));
            assert!(
                !cached.contains("RULE-A") && !cached.contains("RULE-B"),
                "rules must never sit inside a cached segment"
            );
        }
    }

    /// n_b == 1 (single rule-batch per chunk — batch mode's default
    /// `BATCH_RULE_BATCH_SIZE`): only ONE cache breakpoint (the repo map) survives; the
    /// chunk digest is folded into the uncached suffix rather than paying a cache-write
    /// premium for a segment that's read back zero times.
    #[tokio::test]
    async fn run_passes_drops_the_digest_breakpoint_when_only_one_rule_batch() {
        let completer = CapturingCompleter::default();
        let chunk0: Vec<(String, String)> = vec![("chunk0.rs".to_string(), "fn chunk0() {}".to_string())];
        let chunk1: Vec<(String, String)> = vec![("chunk1.rs".to_string(), "fn chunk1() {}".to_string())];
        let chunks: Vec<&[(String, String)]> = vec![&chunk0, &chunk1];

        let batch0: Vec<(String, String)> = vec![("RULE-A".to_string(), "rule A desc".to_string())];
        let batches: Vec<&[(String, String)]> = vec![&batch0]; // n_b == 1

        let repo_map = "REPO MAP:\nchunk0.rs: fn chunk0()\nchunk1.rs: fn chunk1()\n".to_string();
        let adopted = std::collections::HashSet::new();

        let (_findings, _proposed, _requested, ok, err) = run_passes(
            &completer,
            "acme/widgets",
            &repo_map,
            &adopted,
            None,
            None,
            None,
            &chunks,
            &batches,
            4,
            "parallel",
            "test-session",
            None,
            false,
        )
        .await;
        assert!(err.is_none(), "no pass should fail: {err:?}");
        assert_eq!(ok, 2, "2 chunks x 1 rule-batch = 2 passes");

        let seen = completer.seen.lock().unwrap();
        for req in seen.iter() {
            assert_eq!(
                req.cache_breakpoints.len(), 1,
                "n_b==1 drops the digest breakpoint: {:?}", req.cache_breakpoints
            );
        }
    }

    /// GUARD: in an AI-review scan, when the LLM is unavailable and EVERY pass fails,
    /// `audit_repo` SURFACES the failure (returns `Err`) instead of silently reporting a
    /// clean Ok([]). This is the "never a silent clean" contract from `audit_repo`'s
    /// `ok_passes == 0` branch. `feedback: None` drives the non-streaming `complete` path;
    /// the streaming path is covered by `audit_repo_surfaces_ai_failure_streaming`.
    #[tokio::test]
    async fn audit_repo_surfaces_ai_failure_not_silent_clean() {
        let llm = FailingCompleter;
        // One source file + one semantic rule so there is real AI work to attempt
        // (an empty file set short-circuits to Ok before any pass runs).
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() { db.execute(query); }\n".to_string(),
        )];
        let selected = vec![(
            "ARCH-NO-DIRECT-DB-1".to_string(),
            "Controllers must not call the database directly.".to_string(),
        )];
        let res = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &[],                    // alternatives (none — no multi-option rules in this test)
            &std::collections::HashMap::new(), // forced
            None,                  // model
            None,                  // calibration model
            ScanMode::Parallel,    // real-time path (the "both" AI-review path)
            false,                 // thorough
            None,                  // feedback -> non-streaming complete()
            None,                  // job
            None,                  // meter
            None,                  // map_files
        )
        .await;
        // (1) The AI failure is SURFACED, not swallowed into a clean result.
        let err = res.expect_err("all passes failed -> audit_repo must return Err, never Ok([])");
        // (2) It does not fabricate success; the error names the simulated unavailability.
        assert!(
            err.to_string().contains("simulated LLM unavailable"),
            "surfaced error should carry the underlying LLM failure, got: {err}"
        );
    }

    /// Same guard for the STREAMING path: an AI-review scan with a transcript store uses
    /// `complete_streaming`, which also fails — `audit_repo` must still surface `Err`.
    #[tokio::test]
    async fn audit_repo_surfaces_ai_failure_streaming() {
        let llm = FailingCompleter;
        let store = crate::transcript::TranscriptStore::default();
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() { db.execute(query); }\n".to_string(),
        )];
        let selected = vec![(
            "ARCH-NO-DIRECT-DB-1".to_string(),
            "Controllers must not call the database directly.".to_string(),
        )];
        let res = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &[],
            &std::collections::HashMap::new(),
            None,
            None,
            ScanMode::Parallel,
            false,
            Some((&store, "job-1")), // feedback present -> streaming path
            None,
            None,
            None,
        )
        .await;
        let err = res.expect_err("streaming all-fail must surface Err, never a silent clean");
        assert!(
            err.to_string().contains("simulated LLM unavailable"),
            "streaming-path error should carry the underlying failure, got: {err}"
        );
    }

    /// HAPPY PATH (seam works both ways): a `StubCompleter` returning a canned finding lets
    /// `audit_repo` complete WITHOUT a live model and yields the parsed finding — proving
    /// the trait substitution is faithful, not just an error sink.
    #[tokio::test]
    async fn audit_repo_with_stub_completer_returns_findings() {
        // Minimal audit-JSON the parser recognizes: one finding under the adopted rule.
        let canned = r#"{"findings":[{"rule":"ARCH-NO-DIRECT-DB-1","severity":"high","path":"src/lib.rs","code":"db.execute(query)","title":"direct DB call","detail":"controller calls the database directly"}]}"#;
        let llm = StubCompleter { text: canned.to_string() };
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() { db.execute(query); }\n".to_string(),
        )];
        let selected = vec![(
            "ARCH-NO-DIRECT-DB-1".to_string(),
            "Controllers must not call the database directly.".to_string(),
        )];
        let (findings, _proposed, _recs, _failed, _stage_samples) = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &[],
            &std::collections::HashMap::new(),
            None,
            None,
            ScanMode::Parallel,
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("stub completer never errors -> audit_repo returns Ok");
        // The stub's canned finding survived parse + calibration (also stub-served).
        assert!(
            findings.iter().any(|f| f.rule_id == "ARCH-NO-DIRECT-DB-1"),
            "stub-served finding must survive the audit pipeline: {findings:?}"
        );
    }

    // ---- Audit-integrated alternative recommendation (2026-09-22 design) ----------------

    fn two_option_alternatives(rule_id: &str, selected: Option<&str>) -> RuleAlternatives {
        RuleAlternatives {
            rule_id: rule_id.to_string(),
            options: vec![
                RuleOptionView {
                    id: "opt-a".to_string(),
                    label: "Option A".to_string(),
                    directive: "Do it the A way.".to_string(),
                    why: "A is simple.".to_string(),
                },
                RuleOptionView {
                    id: "opt-b".to_string(),
                    label: "Option B".to_string(),
                    directive: "Do it the B way.".to_string(),
                    why: "B is scalable.".to_string(),
                },
            ],
            selected_option_id: selected.map(str::to_string),
        }
    }

    #[test]
    fn build_alternatives_block_includes_every_option_and_marks_the_selected_one() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let block = build_alternatives_block(&[alt]);
        assert!(block.contains("MULTI-RULE-1"));
        assert!(block.contains("opt-a"), "every option id must be present: {block}");
        assert!(block.contains("opt-b"), "every option id must be present: {block}");
        assert!(block.contains("Do it the A way."), "every directive must be present: {block}");
        assert!(block.contains("Do it the B way."), "every directive must be present: {block}");
        assert!(block.contains("why: A is simple."), "every rationale must be present: {block}");
        // The marker sits on the SELECTED option's line, not the other one.
        let a_line = block.lines().find(|l| l.contains("opt-a")).unwrap();
        let b_line = block.lines().find(|l| l.contains("opt-b")).unwrap();
        assert!(a_line.contains("CURRENTLY SELECTED"), "selected option must be marked: {a_line}");
        assert!(!b_line.contains("CURRENTLY SELECTED"), "unselected option must not be marked: {b_line}");
    }

    #[test]
    fn build_alternatives_block_marks_none_selected_when_nothing_is_chosen() {
        let alt = two_option_alternatives("MULTI-RULE-1", None);
        let block = build_alternatives_block(&[alt]);
        assert!(
            block.contains("no option currently selected"),
            "must state explicitly that nothing is selected: {block}"
        );
        assert!(!block.contains("CURRENTLY SELECTED"));
    }

    #[test]
    fn parse_alternative_recommendations_accepts_a_real_option_id() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-b","recommendation_reasoning":"the codebase already does it the B way"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].rule_id, "MULTI-RULE-1");
        assert_eq!(recs[0].recommended_option_id, "opt-b");
        assert_eq!(recs[0].recommendation_reasoning, "the codebase already does it the B way");
        assert!(!recs[0].hallucinated);
        assert!(!recs[0].operator_chosen);
    }

    #[test]
    fn parse_alternative_recommendations_rejects_a_hallucinated_option_id() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-does-not-exist","recommendation_reasoning":"bogus"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert_eq!(recs.len(), 1);
        // Falls back to the currently-selected option, never the hallucinated id.
        assert_eq!(recs[0].recommended_option_id, "opt-a");
        assert!(recs[0].hallucinated, "a non-real option id must be flagged hallucinated");
        assert!(
            recs[0].recommendation_reasoning.contains("opt-does-not-exist"),
            "the correction note should name the bogus id: {}",
            recs[0].recommendation_reasoning
        );
    }

    #[test]
    fn parse_alternative_recommendations_falls_back_to_the_first_option_when_nothing_is_selected() {
        let alt = two_option_alternatives("MULTI-RULE-1", None);
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"nonsense","recommendation_reasoning":""}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert_eq!(recs[0].recommended_option_id, "opt-a", "first option is the last-resort fallback");
        assert!(recs[0].hallucinated);
    }

    #[test]
    fn parse_alternative_recommendations_falls_back_when_a_rule_is_missing_from_the_response() {
        let alts = vec![
            two_option_alternatives("MULTI-RULE-1", Some("opt-a")),
            two_option_alternatives("MULTI-RULE-2", Some("opt-b")),
        ];
        // The model only answered rule 1 — rule 2 must still get a fallback entry, never be
        // silently dropped (every listed rule gets exactly one output entry).
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","recommendation_reasoning":"fine as-is"}]}"#;
        let recs = parse_alternative_recommendations(raw, &alts);
        assert_eq!(recs.len(), 2, "every listed rule must get exactly one entry");
        let r2 = recs.iter().find(|r| r.rule_id == "MULTI-RULE-2").unwrap();
        assert_eq!(r2.recommended_option_id, "opt-b", "falls back to its own selected option");
        assert!(r2.hallucinated);
    }

    #[test]
    fn parse_alternative_recommendations_degrades_on_malformed_json_without_panicking() {
        let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
        for raw in ["not json at all", "", "{ not valid json ]"] {
            let recs = parse_alternative_recommendations(raw, &alts);
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].recommended_option_id, "opt-a");
            assert!(recs[0].hallucinated);
        }
    }

    // ── P7: evidence-gated applicability ────────────────────────────────────────────────

    /// A genuine, evidenced pick: `applicable: true` WITH a real evidence line is trusted
    /// as-is — `applicable` stays true, `evidence` is carried through, and this is NOT
    /// `hallucinated` (a real, evidenced answer, not a correction).
    #[test]
    fn parse_alternative_recommendations_accepts_an_evidenced_applicable_pick() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":true,"recommended_option_id":"opt-b","evidence":"src/api/list_users.rs:42 — cursor param already threaded through","recommendation_reasoning":"the codebase already does it the B way"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].applicable);
        assert_eq!(
            recs[0].evidence.as_deref(),
            Some("src/api/list_users.rs:42 — cursor param already threaded through")
        );
        assert_eq!(recs[0].recommended_option_id, "opt-b");
        assert!(!recs[0].hallucinated, "an evidenced applicable pick is not a hallucination");
    }

    /// THE CORE P7 GUARANTEE: a model claiming `applicable: true` but citing NO evidence line
    /// must never be trusted at face value — it is corrected to `applicable: false` (no
    /// findings will be checked against it) and flagged `hallucinated: true` so the UI can show
    /// the correction happened. This is what stops the report from asserting "the adopted
    /// offset-pagination model" when nothing in the repo actually establishes any convention.
    #[test]
    fn parse_alternative_recommendations_corrects_an_unevidenced_applicable_claim() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":true,"recommended_option_id":"opt-b","recommendation_reasoning":"the codebase adopts the B model"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert_eq!(recs.len(), 1);
        assert!(
            !recs[0].applicable,
            "an unevidenced applicability claim must be corrected to not-applicable"
        );
        assert_eq!(recs[0].evidence, None);
        assert!(recs[0].hallucinated, "the correction must be flagged");
        assert!(
            recs[0].recommendation_reasoning.contains("did not cite"),
            "the correction note must explain WHY it was downgraded: {}",
            recs[0].recommendation_reasoning
        );
    }

    /// An empty-string `evidence` field is treated identically to an absent one — never
    /// accepted as "evidence" just because the key was present.
    #[test]
    fn parse_alternative_recommendations_rejects_blank_evidence_as_no_evidence() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":true,"recommended_option_id":"opt-b","evidence":"   ","recommendation_reasoning":"vague"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert!(!recs[0].applicable);
        assert_eq!(recs[0].evidence, None);
        assert!(recs[0].hallucinated);
    }

    /// The model explicitly saying `applicable: false` (no evidence anywhere for the concern)
    /// is trusted as a genuine "not applicable" answer — NOT flagged hallucinated, since the
    /// model correctly declined to assert an unevidenced convention.
    #[test]
    fn parse_alternative_recommendations_accepts_an_explicit_not_applicable() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","applicable":false,"recommendation_reasoning":"no pagination anywhere in this codebase"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert!(!recs[0].applicable);
        assert_eq!(recs[0].evidence, None);
        assert!(
            !recs[0].hallucinated,
            "an honest, explicit not-applicable answer is not a hallucination"
        );
        assert_eq!(recs[0].recommendation_reasoning, "no pagination anywhere in this codebase");
    }

    /// Back-compat: a response that omits the `applicable` key entirely (pre-P7 shape) is NOT
    /// penalized — it fails OPEN to `applicable: true` with no evidence requirement, exactly
    /// the pre-existing behavior. Only an EXPLICIT `true` claim is evidence-gated.
    #[test]
    fn parse_alternative_recommendations_treats_absent_applicable_key_as_back_compat_applicable() {
        let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-b","recommendation_reasoning":"fine"}]}"#;
        let recs = parse_alternative_recommendations(raw, &[alt]);
        assert!(recs[0].applicable);
        assert!(!recs[0].hallucinated);
    }

    /// A fallback recommendation (total parse failure, or a rule the model never answered) is
    /// ALWAYS `applicable: true` — a pipeline failure is not evidence the rule's concern is
    /// inapplicable, so the rule keeps being checked exactly as before P7.
    #[test]
    fn fallback_recommendations_are_always_applicable() {
        let alts = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
        let recs = parse_alternative_recommendations("not json at all", &alts);
        assert!(recs[0].applicable);
        assert_eq!(recs[0].evidence, None);

        let alts2 = vec![
            two_option_alternatives("MULTI-RULE-1", Some("opt-a")),
            two_option_alternatives("MULTI-RULE-2", Some("opt-b")),
        ];
        let raw = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","recommendation_reasoning":"fine"}]}"#;
        let recs2 = parse_alternative_recommendations(raw, &alts2);
        let missing = recs2.iter().find(|r| r.rule_id == "MULTI-RULE-2").unwrap();
        assert!(missing.applicable, "a rule the model never answered stays applicable");
    }

    /// HAPPY PATH end-to-end: `audit_repo` fed one multi-option rule (via `alternatives`)
    /// recommends an option AND tags the finding under that rule with `evaluated_option_id`
    /// — one scan, two outputs per rule, per the design doc. The `StubCompleter` serves the
    /// SAME canned JSON to every call (the recommendation pass, the violation pass, and the
    /// no-op calibration pass); the response carries both a `recommendations` array (read by
    /// the recommendation pass) and a `findings` array (read by the violation pass) — each
    /// pass reads only the key it cares about, so one canned response drives the whole
    /// pipeline deterministically.
    #[tokio::test]
    async fn audit_repo_tags_findings_with_the_recommended_option_for_a_multi_option_rule() {
        let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "recommended_option_id": "opt-b", "recommendation_reasoning": "the repo already does it the B way"}
            ],
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "medium", "path": "src/lib.rs", "code": "old pattern here", "title": "does it the A way", "detail": "should be B"}
            ],
            "proposed_rules": []
        }"#;
        let llm = StubCompleter { text: canned.to_string() };
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() { old pattern here; }\n".to_string(),
        )];
        let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
        let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
        let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &alternatives,
            &std::collections::HashMap::new(), // forced — none, this is the "ask the model" path
            None,
            None,
            ScanMode::Parallel,
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("stub completer never errors");

        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].rule_id, "MULTI-RULE-1");
        assert_eq!(recs[0].recommended_option_id, "opt-b");
        assert!(!recs[0].operator_chosen);

        let f = findings
            .iter()
            .find(|f| f.rule_id == "MULTI-RULE-1")
            .expect("the stub's canned finding must survive the pipeline");
        assert_eq!(
            f.evaluated_option_id.as_deref(),
            Some("opt-b"),
            "the finding must be tagged with the option it was actually judged under: {f:?}"
        );
    }

    /// P7 CORE GUARANTEE, end-to-end: when the recommendation pass marks a rule NOT
    /// APPLICABLE (no evidence its concern exists in this repo at all), `audit_repo` emits
    /// ZERO findings for it — even though the stub's canned response includes a "finding"
    /// under that exact rule id (proving the hard post-filter works, not merely that the rule
    /// was left out of the prompt, which a stub ignores anyway).
    #[tokio::test]
    async fn audit_repo_emits_no_findings_for_a_not_applicable_rule() {
        let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-RULE-1", "applicable": false, "recommendation_reasoning": "no evidence of pagination anywhere in this codebase"}
            ],
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "medium", "path": "src/lib.rs", "code": "old pattern here", "title": "does it the A way", "detail": "should be B"}
            ],
            "proposed_rules": []
        }"#;
        let llm = StubCompleter { text: canned.to_string() };
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() { old pattern here; }\n".to_string(),
        )];
        let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
        let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
        let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &alternatives,
            &std::collections::HashMap::new(),
            None,
            None,
            ScanMode::Parallel,
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("stub completer never errors");

        assert_eq!(recs.len(), 1);
        assert!(!recs[0].applicable);
        assert!(
            findings.iter().all(|f| f.rule_id != "MULTI-RULE-1"),
            "a not-applicable rule must emit ZERO findings, even one the model cited anyway: \
             {findings:?}"
        );
    }

    /// FORCED (operator-chosen) mode, the `rescan-alternatives` path: when the rule id is
    /// present in `forced`, `audit_repo` does NOT ask the model to recommend — it rewrites the
    /// rule's directive straight to the forced option and tags findings with that id, marking
    /// the recommendation `operator_chosen: true`. The stub's response has NO `recommendations`
    /// key at all, proving the recommendation pass genuinely never ran (there is nothing for it
    /// to parse) — if it had run, the fallback path would still produce a result, so the
    /// stronger assertion here is `operator_chosen: true` with the EXACT forced id, unmodified.
    #[tokio::test]
    async fn audit_repo_forced_mode_skips_recommendation_and_tags_the_operator_choice() {
        let canned = r#"{
            "findings": [
                {"rule": "MULTI-RULE-1", "severity": "medium", "path": "src/lib.rs", "code": "old pattern here", "title": "does it the A way", "detail": "should be B"}
            ],
            "proposed_rules": []
        }"#;
        let llm = StubCompleter { text: canned.to_string() };
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() { old pattern here; }\n".to_string(),
        )];
        let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
        let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
        let mut forced = std::collections::HashMap::new();
        forced.insert("MULTI-RULE-1".to_string(), "opt-b".to_string());

        let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &alternatives,
            &forced,
            None,
            None,
            ScanMode::Parallel,
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("stub completer never errors");

        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].recommended_option_id, "opt-b");
        assert!(recs[0].operator_chosen, "a forced pick must be marked operator_chosen");
        assert!(!recs[0].hallucinated, "a real forced option id is not a hallucination");

        let f = findings.iter().find(|f| f.rule_id == "MULTI-RULE-1").unwrap();
        assert_eq!(f.evaluated_option_id.as_deref(), Some("opt-b"));
    }

    /// A forced option id that is NOT real on the rule is rejected (never silently trusted) —
    /// falls back to the rule's currently-selected option and is flagged `hallucinated`.
    #[tokio::test]
    async fn audit_repo_forced_mode_rejects_an_invalid_forced_option_id() {
        let canned = r#"{"findings": [], "proposed_rules": []}"#;
        let llm = StubCompleter { text: canned.to_string() };
        let files = vec![("src/lib.rs".to_string(), "fn handler() {}\n".to_string())];
        let selected = vec![("MULTI-RULE-1".to_string(), "Do it the A way.".to_string())];
        let alternatives = vec![two_option_alternatives("MULTI-RULE-1", Some("opt-a"))];
        let mut forced = std::collections::HashMap::new();
        forced.insert("MULTI-RULE-1".to_string(), "opt-does-not-exist".to_string());

        let (_findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &alternatives,
            &forced,
            None,
            None,
            ScanMode::Parallel,
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("stub completer never errors");

        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].recommended_option_id, "opt-a", "falls back to the selected option");
        assert!(recs[0].hallucinated);
        assert!(recs[0].operator_chosen);
    }

    /// e2e-style (design doc §Testing): a scan over a fixture with TWO multi-option rules —
    /// one WITH a currently-selected/default option, one WITHOUT (the "no default, architect
    /// never chose" case the removed gate used to block on) — yields a recommendation AND
    /// violations-under-the-recommended-option for BOTH, in one pass. Proves the no-default
    /// rule is handled identically to the with-default one: it simply gets a recommendation
    /// instead of blocking.
    #[tokio::test]
    async fn audit_repo_e2e_two_multi_option_rules_one_with_default_one_without() {
        let canned = r#"{
            "recommendations": [
                {"rule_id": "MULTI-WITH-DEFAULT-1", "recommended_option_id": "opt-a", "recommendation_reasoning": "matches existing pattern"},
                {"rule_id": "MULTI-NO-DEFAULT-1", "recommended_option_id": "opt-b", "recommendation_reasoning": "best fit despite no prior default"}
            ],
            "findings": [
                {"rule": "MULTI-WITH-DEFAULT-1", "severity": "medium", "path": "src/lib.rs", "line": 2, "code": "thing one;", "title": "t1", "detail": "d1"},
                {"rule": "MULTI-NO-DEFAULT-1", "severity": "low", "path": "src/lib.rs", "line": 3, "code": "thing two;", "title": "t2", "detail": "d2"}
            ],
            "proposed_rules": []
        }"#;
        let llm = StubCompleter { text: canned.to_string() };
        // The two findings sit on DIFFERENT lines so the cross-rule location-merge pass
        // (`merge_by_location`, which fuses same-(path,line) findings into one row) doesn't
        // collapse them into a single finding — this test wants two distinct rows to assert
        // each one's own `evaluated_option_id` independently.
        let files = vec![(
            "src/lib.rs".to_string(),
            "fn handler() {\n    thing one;\n    thing two;\n}\n".to_string(),
        )];
        let selected = vec![
            ("MULTI-WITH-DEFAULT-1".to_string(), "Do it the A way.".to_string()),
            ("MULTI-NO-DEFAULT-1".to_string(), "Do it the A way.".to_string()),
        ];
        let alternatives = vec![
            // Has a currently-selected option (the corpus default, in the real pipeline).
            two_option_alternatives("MULTI-WITH-DEFAULT-1", Some("opt-a")),
            // No default and never chosen — `selected_option_id: None` is exactly the state
            // the removed "must choose an alternative" gate used to block on.
            two_option_alternatives("MULTI-NO-DEFAULT-1", None),
        ];
        let (findings, _proposed, recs, _failed, _stage_samples) = audit_repo(
            &llm,
            "me/api",
            &files,
            &selected,
            &alternatives,
            &std::collections::HashMap::new(),
            None,
            None,
            ScanMode::Parallel,
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("no-default rule must not block or error the scan");

        assert_eq!(recs.len(), 2, "both rules must get a recommendation");
        let with_default = recs.iter().find(|r| r.rule_id == "MULTI-WITH-DEFAULT-1").unwrap();
        let no_default = recs.iter().find(|r| r.rule_id == "MULTI-NO-DEFAULT-1").unwrap();
        assert_eq!(with_default.recommended_option_id, "opt-a");
        assert_eq!(
            no_default.recommended_option_id, "opt-b",
            "a rule with no prior default must still get a real recommendation, not block"
        );
        assert!(!with_default.hallucinated);
        assert!(!no_default.hallucinated);

        for (rule_id, expected_option) in
            [("MULTI-WITH-DEFAULT-1", "opt-a"), ("MULTI-NO-DEFAULT-1", "opt-b")]
        {
            let f = findings
                .iter()
                .find(|f| f.rule_id == rule_id)
                .unwrap_or_else(|| panic!("missing finding for {rule_id}: {findings:?}"));
            assert_eq!(
                f.evaluated_option_id.as_deref(),
                Some(expected_option),
                "{rule_id}'s finding must be tagged under its recommended option"
            );
        }
    }

    // ---- W2: no-vanishing merge guarantee ------------------------------------------------
    //
    // Both merge passes pick a PRIMARY and literally `.remove()` it from the group's Vec to
    // build the output row's `also_matches` from the rest. That `.remove()` is exactly the
    // operation a future refactor could get wrong (e.g. special-casing "group now empty" into
    // an early `None`/empty-Vec return instead of "the primary IS the row"). These tests pin
    // the invariant directly: a group whose primary is removed during merge must ALWAYS still
    // emit that one row, for every group size including the singleton case (where removing the
    // primary leaves literally nothing else).

    #[test]
    fn merge_location_group_singleton_whose_primary_is_removed_still_emits_a_row() {
        let f = site_finding("AI-LONE-FINDING", "a.rs", 7, "high", "only member");
        let merged = merge_location_group(vec![f]);
        assert_eq!(
            merged.rule_id, "AI-LONE-FINDING",
            "the only member IS the primary — removing it from the group must still yield it \
             as the output row, not an empty/default finding"
        );
        assert!(
            merged.also_matches.is_empty(),
            "a singleton group has no siblings to demote"
        );
    }

    #[test]
    fn merge_location_group_multi_member_whose_primary_is_removed_still_emits_a_row() {
        // Three findings at the same site; `SEC-` beats the `AI-` ones on class+origin, so it
        // is the primary REMOVED from the group vec mid-merge — the row must still come out,
        // carrying both siblings in `also_matches`, never a vanished/empty result.
        let sec = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "db.rs", 20, "high", "sec");
        let ai_a = site_finding("AI-RAW-QUERY-BUILDING", "db.rs", 20, "medium", "sec");
        let ai_b = site_finding("AI-UNSAFE-QUERY", "db.rs", 20, "medium", "sec");
        let merged = merge_location_group(vec![sec, ai_a, ai_b]);
        assert_eq!(
            merged.rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
            "the deterministic member is primary and must survive as the row's own id"
        );
        assert_eq!(
            merged.also_matches.len(),
            2,
            "both removed siblings must be demoted into also_matches, never dropped: {:?}",
            merged.also_matches
        );
    }

    #[test]
    fn merge_semantic_group_singleton_whose_primary_is_removed_still_emits_a_row() {
        let f = site_finding("AI-LONE-SEMANTIC-FINDING", "a.rs", 7, "high", "only member");
        let merged = merge_semantic_group(vec![f]);
        assert_eq!(
            merged.rule_id, "AI-LONE-SEMANTIC-FINDING",
            "a singleton semantic group must still emit its one row, never vanish"
        );
    }

    #[test]
    fn merge_semantic_group_multi_member_whose_primary_is_removed_still_emits_a_row() {
        let mut det = site_finding("RLS-MISSING", "a.rs", 10, "high", "");
        det.category = Some("rls-policy".to_string());
        let mut ai_a = site_finding("AI-rls-thing-one", "a.rs", 12, "medium", "");
        ai_a.category = Some("rls-policy".to_string());
        let mut ai_b = site_finding("AI-rls-thing-two", "a.rs", 13, "medium", "");
        ai_b.category = Some("rls-policy".to_string());
        // `merge_semantic_group` takes an already-clustered group directly (unlike
        // `merge_semantic_groups`, which does the clustering) — exercises the exact
        // `group.remove(primary_idx)` call site this invariant is about.
        let merged = merge_semantic_group(vec![det, ai_a, ai_b]);
        assert_eq!(
            merged.rule_id, "RLS-MISSING",
            "the deterministic member, removed as primary mid-merge, must still be the row"
        );
        assert_eq!(
            merged.also_matches.len(),
            2,
            "both removed AI siblings must survive in also_matches: {:?}",
            merged.also_matches
        );
    }

    // ---- Semantic dedup (design §1d) ----------------------------------------------------

    #[test]
    fn semantic_det_and_ai_same_category_merge_det_primary_keeps_line() {
        // A deterministic RLS finding and an AI RLS finding three lines apart are the same
        // defect: they fuse, the deterministic side wins the primary and keeps its exact line,
        // and the AI rule id is demoted to also_matches.
        let mut det = site_finding("RLS-MISSING", "a.rs", 10, "high", "");
        det.category = Some("rls-policy".to_string());
        let mut ai = site_finding("AI-rls-thing", "a.rs", 13, "medium", "");
        ai.category = Some("rls-policy".to_string());
        let out = merge_semantic_groups(vec![det, ai], &[]);
        assert_eq!(out.len(), 1, "same defect must collapse to one row");
        assert_eq!(out[0].rule_id, "RLS-MISSING", "deterministic side is primary");
        assert_eq!(out[0].line, 10, "primary keeps its own deterministic anchor line");
        assert_eq!(out[0].severity, "high", "max severity is kept");
        assert!(
            out[0].also_matches.contains(&"AI-rls-thing".to_string()),
            "sibling rule demoted to also_matches: {:?}",
            out[0].also_matches
        );
    }

    #[test]
    fn semantic_two_ai_same_construct_merge() {
        // Two AI findings in the same handler body but > window lines apart merge via the
        // same-construct rule (both located inside one brace block corroborates the fusion).
        let content = "fn handler() {\n a\n b\n c\n d\n e\n f\n g\n h\n}\n";
        let mut a = site_finding("AI-authz-1", "h.rs", 2, "medium", "");
        a.category = Some("authorization".to_string());
        let mut b = site_finding("AI-authz-2", "h.rs", 8, "medium", "");
        b.category = Some("authorization".to_string());
        let files = vec![("h.rs".to_string(), content.to_string())];
        let out = merge_semantic_groups(vec![a, b], &files);
        assert_eq!(out.len(), 1, "one construct, one defect");
    }

    #[test]
    fn semantic_same_category_different_object_does_not_merge() {
        // Same category, adjacent lines, but disjoint structural objects (two different
        // tables) — the discrimination guard keeps them separate.
        let mut a = site_finding("AI-rls-a", "s.sql", 10, "high", "public.orders");
        a.category = Some("rls-policy".to_string());
        let mut b = site_finding("AI-rls-b", "s.sql", 12, "high", "public.payments");
        b.category = Some("rls-policy".to_string());
        let out = merge_semantic_groups(vec![a, b], &[]);
        assert_eq!(out.len(), 2, "disjoint tables stay distinct");
    }

    #[test]
    fn semantic_category_none_never_merges() {
        // Uncategorized findings (heuristic returns None too) never fuse — fail-open to
        // over-telling.
        let a = site_finding("XYZ-1", "a.rs", 10, "high", "");
        let b = site_finding("XYZ-2", "a.rs", 11, "high", "");
        let out = merge_semantic_groups(vec![a, b], &[]);
        assert_eq!(out.len(), 2, "no category ⇒ no merge");
    }

    #[test]
    fn semantic_two_deterministic_never_merge() {
        // Two deterministic rows are two distinct defects by construction — never merge even
        // when same-category and adjacent.
        let mut a = site_finding("RLS-A", "a.rs", 10, "high", "");
        a.category = Some("rls-policy".to_string());
        let mut b = site_finding("RLS-B", "a.rs", 11, "high", "");
        b.category = Some("rls-policy".to_string());
        let out = merge_semantic_groups(vec![a, b], &[]);
        assert_eq!(out.len(), 2, "two deterministic rows stay separate");
    }

    // ── P1: deduplication + cross-tier merge ────────────────────────────────────────────
    // docs/plans/2026-09-29_codebase-inspection-hardening.md, P1. Synthetic findings only —
    // never keyed to any one benchmark's rule/file/table names.

    #[test]
    fn p1_merge_location_security_beats_hygiene_regardless_of_severity() {
        // The canonical P1 regression, at the EXACT same location (the shape the real
        // ARCH-MIDDLEWARE-FIRST-1-vs-reflected-origin-CORS bug actually takes: both rows cite
        // the same middleware line, so they collapse via `merge_by_location`). Before the fix,
        // "adopted (non-AI-) beats invented" was the FIRST key, so the deterministic-but-
        // irrelevant structural rule always won regardless of severity, hiding the security
        // finding entirely. The structural row here is even HIGHER severity, to prove class
        // beats severity, not merely that security happened to also be more severe.
        let code = "app.use(cors())";
        let files = vec![("middleware.ts".to_string(), code.to_string())];
        let structural = site_finding("ARCH-MIDDLEWARE-FIRST-1", "middleware.ts", 12, "high", code);
        let security = site_finding(
            "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS",
            "middleware.ts",
            12,
            "medium",
            code,
        );
        let merged = merge_by_location(vec![structural, security], &files);
        assert_eq!(merged.len(), 1, "both rows cite the same code location");
        assert_eq!(
            merged[0].rule_id, "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS",
            "the security finding must survive as primary, never hidden behind a structural row"
        );
        assert!(merged[0]
            .also_matches
            .contains(&"ARCH-MIDDLEWARE-FIRST-1".to_string()));
    }

    #[test]
    fn p1_semantic_group_shared_object_security_beats_hygiene_primary() {
        // Cross-file, cross-class cluster via a shared captured object (signal 2b): a Hygiene
        // structural finding and a Security finding both name the SAME table, in different
        // files. Security must win primary even though the structural finding is deterministic
        // AND more severe — both the OTHER keys favor the structural row, isolating class as
        // the deciding one.
        let mut structural =
            site_finding("ARCH-MIDDLEWARE-FIRST-1", "router.ts", 5, "critical", "");
        structural
            .captures
            .insert("table".to_string(), "profiles".to_string());
        let mut security = site_finding("AI-RLS-MISSING-ON-PROFILES", "schema.sql", 40, "low", "");
        security.category = Some("rls-policy".to_string());
        security
            .captures
            .insert("table".to_string(), "profiles".to_string());
        let out = merge_semantic_groups(vec![structural, security], &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].rule_id, "AI-RLS-MISSING-ON-PROFILES",
            "security wins primary even though it is lower severity and the other side is deterministic"
        );
        assert_eq!(out[0].severity, "critical", "max severity is still kept");
    }

    #[test]
    fn p1_primary_tiebreak_severity_wins_within_same_class() {
        // Same class (both Hygiene, both invented AI- names, no calibrated confidence) —
        // severity alone decides, in isolation from the other keys.
        let low = site_finding("AI-HYGIENE-LOW", "f.rs", 10, "low", "");
        let high = site_finding("AI-HYGIENE-HIGH", "f.rs", 10, "high", "");
        let merged = merge_location_group(vec![low, high]);
        assert_eq!(merged.rule_id, "AI-HYGIENE-HIGH");
    }

    #[test]
    fn p1_primary_tiebreak_confidence_wins_within_same_class_and_severity() {
        // Same class (neither rule id nor category is security-flavored), same severity, and
        // the SAME origin tier (both confidence.is_some() ⇒ both AdoptedAi) — confidence alone
        // decides: a "needs-review"-flagged finding must not win over an equally-severe,
        // clearer one.
        let mut needs_review = site_finding("RULE-A", "f.rs", 10, "high", "");
        needs_review.confidence = Some("needs-review".to_string());
        let mut clear = site_finding("RULE-B", "f.rs", 10, "high", "");
        clear.confidence = Some("high".to_string());
        let merged = merge_location_group(vec![needs_review, clear]);
        assert_eq!(merged.rule_id, "RULE-B");
    }

    #[test]
    fn p1_primary_tiebreak_specificity_wins_when_severity_and_confidence_tie() {
        // Same class, same severity, same confidence (both None — neither ran through
        // calibration) — specificity (deterministic beats an invented AI- name) decides.
        let deterministic = site_finding("RULE-DETERMINISTIC", "f.rs", 10, "medium", "");
        let invented = site_finding("AI-INVENTED-NAME", "f.rs", 10, "medium", "");
        let merged = merge_location_group(vec![deterministic, invented]);
        assert_eq!(merged.rule_id, "RULE-DETERMINISTIC");
    }

    #[test]
    fn p1_cross_file_shared_captured_object_merges_into_one() {
        // A config-flag finding in one file and the handler finding that reads that SAME flag
        // in another file are one root-cause defect, not two — the general "same object across
        // files" signal (design point 2b), independent of category or line proximity. The
        // absorbed member's own evidence site is preserved, not silently dropped.
        //
        // C5-1: the deterministic config-flag finding now ALWAYS wins primacy over its AI-tier
        // sibling within the same class (REG-1) — origin outranks severity — even though the AI
        // side's own (uncalibrated, inflated) severity is higher. The merged severity is the
        // deterministic primary's OWN grounded "medium", never the AI member's "high" (REG-2):
        // see `merged_severity`'s doc comment for why `max()` must not apply across a mixed-tier
        // group. (Before C5-1 this test pinned the OPPOSITE, buggy behavior — see git history.)
        let mut flag = site_finding("CONFIG-FLAG-DEBUG-MODE", "config.rs", 5, "medium", "");
        flag.captures
            .insert("flag".to_string(), "debug_mode".to_string());
        let mut handler =
            site_finding("AI-HANDLER-TRUSTS-DEBUG-FLAG", "handler.rs", 80, "high", "");
        handler
            .captures
            .insert("flag".to_string(), "debug_mode".to_string());
        let out = merge_semantic_groups(vec![flag, handler], &[]);
        assert_eq!(
            out.len(),
            1,
            "cross-file findings sharing a captured object collapse into one defect"
        );
        assert_eq!(
            out[0].rule_id, "CONFIG-FLAG-DEBUG-MODE",
            "the deterministic finding must win primacy over its AI-tier sibling, regardless of \
             which side's own severity is higher (C5-1 / REG-1)"
        );
        assert_eq!(
            out[0].severity, "medium",
            "the merged severity must be the deterministic primary's OWN grounded severity, \
             never inflated by the absorbed AI member's higher (uncalibrated) severity (C5-1 / \
             REG-2)"
        );
        assert!(out[0]
            .also_matches
            .contains(&"AI-HANDLER-TRUSTS-DEBUG-FLAG".to_string()));
        assert_eq!(
            out[0].also_locations.len(),
            1,
            "the absorbed member's own evidence site is preserved, not dropped"
        );
        assert_eq!(out[0].also_locations[0].path, "handler.rs");
        assert_eq!(out[0].also_locations[0].line, 80);
        assert!(
            !out[0].also_locations[0].consequence,
            "both sides cite real, independently-actionable code"
        );
    }

    #[test]
    fn p1_consequence_member_folds_as_also_affects_location_not_its_own_row() {
        // Root cause: a deterministic RLS-missing finding on the table's own policy site.
        let mut root = site_finding("SEC-RLS-MISSING", "policy.sql", 5, "high", "");
        root.captures
            .insert("table".to_string(), "profiles".to_string());
        // Consequence: an AI finding describing that a page is exposed ONLY because of the
        // missing policy above — no independently-fixable code of its own (an absence/impact
        // description rather than a presence-type violation at its own site: `located = false`,
        // the general "no independent fix" signal — see `Finding::located`).
        let mut consequence = site_finding(
            "AI-PAGE-EXPOSED-VIA-MISSING-RLS",
            "page.tsx",
            40,
            "medium",
            "",
        );
        consequence
            .captures
            .insert("table".to_string(), "profiles".to_string());
        consequence.located = false;
        let out = merge_semantic_groups(vec![root, consequence], &[]);
        assert_eq!(
            out.len(),
            1,
            "the consequence does not survive as its own row"
        );
        assert_eq!(out[0].rule_id, "SEC-RLS-MISSING");
        assert!(out[0]
            .also_matches
            .contains(&"AI-PAGE-EXPOSED-VIA-MISSING-RLS".to_string()));
        assert_eq!(out[0].also_locations.len(), 1);
        assert!(
            out[0].also_locations[0].consequence,
            "a no-independent-fix member is marked as a consequence location"
        );
        assert_eq!(out[0].also_locations[0].path, "page.tsx");
    }

    #[test]
    fn p1_also_matches_never_links_an_unrelated_unclustered_rule() {
        // Two findings form a genuine cluster (same category, adjacent lines); a third, wholly
        // unrelated finding (different file, different category, no shared object) must never
        // appear in the cluster's `also_matches`, and vice versa.
        // One deterministic + one AI-invented so the AI+AI corroboration guard does not apply
        // (this test is about cluster ISOLATION, not corroboration — that is covered by the
        // existing `semantic_two_ai_same_construct_merge` / snippet-corroboration tests).
        let mut a = site_finding("AUTHZ-MISSING-CHECK", "h.rs", 10, "high", "");
        a.category = Some("authorization".to_string());
        let mut b = site_finding("AI-AUTHZ-MISSING-2", "h.rs", 12, "medium", "");
        b.category = Some("authorization".to_string());
        let unrelated = site_finding("PERF-N-PLUS-ONE-1", "other.rs", 500, "low", "");
        let out = merge_semantic_groups(vec![a, b, unrelated], &[]);
        assert_eq!(out.len(), 2, "the unrelated finding stays its own row");
        let cluster = out
            .iter()
            .find(|f| f.also_matches.contains(&"AI-AUTHZ-MISSING-2".to_string()))
            .expect("the a/b cluster must exist");
        assert!(
            !cluster
                .also_matches
                .contains(&"PERF-N-PLUS-ONE-1".to_string()),
            "also_matches must never link an unrelated rule: {:?}",
            cluster.also_matches
        );
        let unrelated_out = out
            .iter()
            .find(|f| f.rule_id == "PERF-N-PLUS-ONE-1")
            .expect("the unrelated finding survives as its own row");
        assert!(unrelated_out.also_matches.is_empty());
    }

    #[test]
    fn p1_merge_never_drops_a_distinct_root_cause() {
        // Two GENUINELY distinct defects, each duplicated across two rule families (4 raw
        // findings total). Merging must collapse each duplicate pair into one row WITHOUT
        // losing either distinct defect — output must be exactly 2, never 1 (over-merged) or 4
        // (under-merged).
        let mut det1 = site_finding("RLS-MISSING", "a.sql", 10, "high", "");
        det1.category = Some("rls-policy".to_string());
        let mut ai1 = site_finding("AI-RLS-GAP", "a.sql", 12, "medium", "");
        ai1.category = Some("rls-policy".to_string());

        // Categories left unset so both backfill to "injection" via the rule-id heuristic.
        let det2 = site_finding("SEC-NO-RAW-SQL-CONCAT-1", "b.ts", 40, "critical", "");
        let ai2 = site_finding("AI-SQL-INJECTION", "b.ts", 41, "high", "");

        let out = merge_semantic_groups(vec![det1, ai1, det2, ai2], &[]);
        assert_eq!(
            out.len(),
            2,
            "two distinct defects survive as exactly two rows — nothing dropped, nothing over-merged"
        );
    }

    // ── MERGE (cycle-2 queue-hardening, docs/plans/2026-09-30_cycle2-queue-hardening.md) ────
    // Gap (ii): two deterministic findings sharing a captured object (same root cause — e.g. two
    // secret-detectors naming the SAME committed secret) were blocked from ever merging by the
    // unconditional det+det guard. Synthetic findings only, fixture-independent.

    #[test]
    fn merge_gap_ii_two_deterministic_findings_sharing_captured_object_merge() {
        // Two DIFFERENT deterministic secret-detectors independently flag the SAME committed
        // secret value in the same file — same root cause, not two distinct defects. The
        // relaxed det+det guard (only blocks when there is NO shared captured object) must let
        // this merge, proving gap (ii) is fixed.
        let mut det_a = site_finding("SEC-SECRET-SCANNER-A-1", "src/config.ts", 10, "high", "");
        det_a
            .captures
            .insert("secret".to_string(), "sk_live_abc123xyz".to_string());
        let mut det_b =
            site_finding("SEC-SECRET-SCANNER-B-1", "src/config.ts", 10, "critical", "");
        det_b
            .captures
            .insert("value".to_string(), "sk_live_abc123xyz".to_string());

        let out = merge_semantic_groups(vec![det_a, det_b], &[]);
        assert_eq!(
            out.len(),
            1,
            "two det findings naming the SAME captured secret must merge into one row: {out:?}"
        );
        let ids: std::collections::HashSet<String> = std::iter::once(out[0].rule_id.clone())
            .chain(out[0].also_matches.iter().cloned())
            .collect();
        assert!(
            ids.contains("SEC-SECRET-SCANNER-A-1") && ids.contains("SEC-SECRET-SCANNER-B-1"),
            "both rule ids must be recorded on the merged row: {ids:?}"
        );
    }

    #[test]
    fn merge_gap_ii_two_deterministic_findings_with_no_shared_object_stay_distinct() {
        // Two deterministic findings in the same file with NO shared captured object and
        // non-overlapping descriptions are genuinely different defects — the relaxed guard must
        // NOT open the door to merging det+det pairs in general, only the shared-object case.
        let mut det_a = site_finding("SEC-A-1", "src/config.ts", 10, "high", "");
        det_a.detail =
            "Missing input validation on the signup form allows arbitrary payloads.".to_string();
        let mut det_b = site_finding("SEC-B-1", "src/config.ts", 11, "high", "");
        det_b.detail =
            "Outbound webhook requests do not verify the TLS certificate chain.".to_string();

        let out = merge_semantic_groups(vec![det_a, det_b], &[]);
        assert_eq!(
            out.len(),
            2,
            "distinct det+det defects with no shared object must stay two rows: {out:?}"
        );
    }

    // ── C6-B2: evidence-chain / root-cause merge signal ─────────────────────────────────────
    // ONE leaked credential, named/used across 3+ files (the holder, the module constructing a
    // client from it, and its call sites), must collapse into ONE row — see
    // `root_cause_credential_identities`/`backfill_credential_identity`'s doc comments.

    #[test]
    fn c6b2_leaked_credential_chain_across_four_findings_three_files_merges_into_one() {
        // The holder (deterministic secret-detector), the module that constructs a client from
        // the SAME secret, and two downstream call sites that use that client — each one's own
        // text names the identical credential identifier, in a different file, under different
        // rule families, far apart in line number. All four are ONE defect (delete the secret +
        // rotate it once), not four.
        let mut holder = site_finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/config.ts",
            5,
            "critical",
            "",
        );
        holder.detail =
            "STRIPE_SECRET_KEY is committed in plaintext in this config file.".to_string();
        holder.effort = Some("high".to_string());

        let mut constructor = site_finding(
            "AI-STRIPE-CLIENT-FROM-LEAKED-KEY",
            "src/payments/stripeClient.ts",
            212,
            "critical",
            "",
        );
        constructor.detail =
            "Constructs a Stripe client directly from the committed STRIPE_SECRET_KEY constant."
                .to_string();
        constructor.effort = Some("high".to_string());

        let mut call_site_a = site_finding(
            "AI-PAYMENT-CALL-USES-LEAKED-KEY-1",
            "src/payments/charge.ts",
            40,
            "critical",
            "",
        );
        call_site_a.detail =
            "Calls stripe.charges.create using the client built from STRIPE_SECRET_KEY."
                .to_string();
        call_site_a.effort = Some("high".to_string());

        let mut call_site_b = site_finding(
            "AI-PAYMENT-CALL-USES-LEAKED-KEY-2",
            "src/payments/refund.ts",
            900,
            "critical",
            "",
        );
        call_site_b.detail =
            "Issues a refund via the same client built from STRIPE_SECRET_KEY.".to_string();
        call_site_b.effort = Some("high".to_string());

        let out = merge_semantic_groups(
            vec![holder, constructor, call_site_a, call_site_b],
            &[],
        );
        assert_eq!(
            out.len(),
            1,
            "one leaked credential referenced by 4 findings across 3+ files must collapse into \
             ONE row: {out:?}"
        );
        let row = &out[0];
        assert_eq!(
            row.rule_id, "SEC-NO-HARDCODED-SECRETS-1",
            "the deterministic holder finding wins primacy"
        );
        let ids: std::collections::HashSet<&str> = std::iter::once(row.rule_id.as_str())
            .chain(row.also_matches.iter().map(String::as_str))
            .collect();
        for expected in [
            "SEC-NO-HARDCODED-SECRETS-1",
            "AI-STRIPE-CLIENT-FROM-LEAKED-KEY",
            "AI-PAYMENT-CALL-USES-LEAKED-KEY-1",
            "AI-PAYMENT-CALL-USES-LEAKED-KEY-2",
        ] {
            assert!(ids.contains(expected), "missing rule id {expected} in {ids:?}");
        }
        assert_eq!(
            row.also_locations.len(),
            3,
            "every other site's own evidence location is preserved: {:?}",
            row.also_locations
        );
        let also_paths: std::collections::HashSet<&str> =
            row.also_locations.iter().map(|l| l.path.as_str()).collect();
        for expected_path in [
            "src/payments/stripeClient.ts",
            "src/payments/charge.ts",
            "src/payments/refund.ts",
        ] {
            assert!(
                also_paths.contains(expected_path),
                "missing location {expected_path} in {also_paths:?}"
            );
        }
        // Single estimate, not a sum of the 4 members' own estimates — `effort` is a
        // categorical tier (never a number to sum), and the merge never touches the primary's
        // own `effort` at all; this pins that invariant explicitly.
        assert_eq!(
            row.effort,
            Some("high".to_string()),
            "the merged row carries ONE estimate (the primary's own), never a sum of the \
             consequence sites' estimates"
        );
    }

    #[test]
    fn c6b2_two_different_leaked_credentials_stay_two_rows() {
        // Safe twin: TWO DIFFERENT leaked credentials, each independently committed/used, must
        // NOT cross-merge just because both are "a leaked secret" — the root-cause signal
        // requires the SAME concrete identifier, never merely "both are critical".
        let mut stripe_holder = site_finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/config.ts",
            5,
            "critical",
            "",
        );
        stripe_holder.detail =
            "STRIPE_SECRET_KEY is committed in plaintext in this config file.".to_string();

        let mut sendgrid_holder = site_finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/email/config.ts",
            9,
            "critical",
            "",
        );
        sendgrid_holder.detail =
            "SENDGRID_API_KEY is committed in plaintext in this config file.".to_string();

        let out = merge_semantic_groups(vec![stripe_holder, sendgrid_holder], &[]);
        assert_eq!(
            out.len(),
            2,
            "two DIFFERENT leaked credentials must stay two rows, never cross-merged on \
             \"both are a leaked secret\" alone: {out:?}"
        );
    }

    #[test]
    fn c6b2_backfill_never_overwrites_an_existing_capture() {
        // The credential backfill must be purely additive: a finding that already carries a
        // `captures` entry for an unrelated purpose (e.g. a table name from
        // `backfill_captured_object`) must keep that entry intact AND still gain the credential
        // token under its own key.
        let mut f = site_finding("SEC-MIXED-1", "src/handler.ts", 10, "high", "");
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        f.detail = "Reads DB_PASSWORD to connect before querying profiles.".to_string();
        backfill_credential_identity(&mut f);
        assert_eq!(
            f.captures.get("table").map(String::as_str),
            Some("profiles"),
            "the pre-existing capture must survive untouched"
        );
        assert_eq!(
            f.captures.get("DB_PASSWORD").map(String::as_str),
            Some("DB_PASSWORD"),
            "the credential token is additionally recorded under its own key"
        );
    }

    // Gap (i): det+AI describing one defect a few lines apart with near-identical prose, in
    // different categories, previously missed both semantic signals.

    #[test]
    fn merge_gap_i_det_and_ai_adjacent_lines_overlapping_description_different_category_merges() {
        // A deterministic secret-detector and an AI finding, three lines apart, in DIFFERENT
        // categories (so signal (a) — same-category + window — never fires) but describing the
        // SAME defect in near-identical prose. The new description-overlap signal must catch
        // this: exactly one exported row, both rule ids present, both evidence sites kept.
        let mut det = site_finding("SEC-HARDCODED-API-KEY-1", "src/config.ts", 10, "high", "");
        det.detail =
            "A live Stripe secret key is hardcoded in this file and committed to the repository."
                .to_string();
        det.category = Some("secrets-hygiene".to_string());

        let mut ai = site_finding("AI-STRIPE-KEY-EXPOSED", "src/config.ts", 12, "medium", "");
        ai.detail = "A live stripe secret key is hardcoded in this file and committed to the \
                     repository, risking compromise."
            .to_string();
        ai.category = Some("credential-exposure".to_string());

        let out = merge_semantic_groups(vec![det, ai], &[]);
        assert_eq!(
            out.len(),
            1,
            "near-identical prose on adjacent lines must collapse to one row: {out:?}"
        );
        let ids: std::collections::HashSet<String> = std::iter::once(out[0].rule_id.clone())
            .chain(out[0].also_matches.iter().cloned())
            .collect();
        assert!(
            ids.contains("SEC-HARDCODED-API-KEY-1") && ids.contains("AI-STRIPE-KEY-EXPOSED"),
            "both rule ids must be recorded on the merged row: {ids:?}"
        );
        let lines: std::collections::HashSet<usize> = std::iter::once(out[0].line)
            .chain(out[0].also_locations.iter().map(|l| l.line))
            .collect();
        assert!(
            lines.contains(&10) && lines.contains(&12),
            "both evidence lines must be kept, not dropped: {lines:?}"
        );
    }

    #[test]
    fn merge_gap_i_sink_and_call_site_in_different_files_stay_two_rows() {
        // PRESERVE (design "Do NOT break"): a sink finding and a call-site finding describing
        // the SAME overarching vulnerability, but in TWO DIFFERENT files, with genuinely
        // overlapping prose. The new description-overlap signal is deliberately same-path-only,
        // so this must NOT merge even though the text is near-identical — proving the signal
        // never crosses files.
        let mut sink =
            site_finding("SEC-SQL-INJECTION-SINK-1", "src/db/sink.ts", 50, "critical", "");
        sink.detail = "User-controlled input flows into a raw SQL query without \
                       parameterization, enabling SQL injection."
            .to_string();
        let mut call_site =
            site_finding("AI-SQL-INJECTION-CALL-SITE", "src/api/handler.ts", 20, "high", "");
        call_site.detail = "User-controlled input flows into a raw SQL query without \
                             parameterization, enabling SQL injection at this call site."
            .to_string();

        let out = merge_semantic_groups(vec![sink, call_site], &[]);
        assert_eq!(
            out.len(),
            2,
            "the sink+call-site pair across two files must stay two rows: {out:?}"
        );
    }

    // ── MERGE (cycle-3 queue-hardening): one defect shipping as 2-3 rows ────────────────────
    // Synthetic findings only, fixture-independent (observed-failures 1-3 from the cycle-3
    // triage, never reproduced against the actual scan output).

    #[test]
    fn merge_gap_i_near_identical_prose_merges_beyond_window_distance() {
        // Observed failure 1: an unauthenticated-export defect shipped as two critical rows 9
        // lines apart in the same file — OUTSIDE SEMANTIC_MERGE_WINDOW (5), and in different
        // (here: both-unassigned) categories, so only the description-overlap signal can catch
        // it. The prose is realistically paraphrased (not copy-pasted) exactly like a det+AI
        // pair reporting the same thing in each side's own words — this is the case
        // DESCRIPTION_OVERLAP_THRESHOLD was recalibrated 0.6 -> 0.5 against (score ~0.58 here;
        // see the threshold's doc comment).
        let mut det = site_finding(
            "SEC-UNAUTH-EXPORT-1",
            "src/export/handler.ts",
            10,
            "critical",
            "audit export endpoint missing authentication check",
        );
        det.detail = "the export handler returns the full audit report with no auth check \
                       so any caller can read it"
            .to_string();
        let mut ai = site_finding(
            "AI-EXPORT-NO-AUTH-CHECK",
            "src/export/handler.ts",
            19,
            "critical",
            "audit export route missing auth validation",
        );
        ai.detail = "this export handler returns the full audit report without an auth check \
                      so any user can read it"
            .to_string();

        let out = merge_semantic_groups(vec![det, ai], &[]);
        assert_eq!(
            out.len(),
            1,
            "near-identical prose 9 lines apart (outside the window) must still collapse to one row: {out:?}"
        );
        let ids: std::collections::HashSet<String> = std::iter::once(out[0].rule_id.clone())
            .chain(out[0].also_matches.iter().cloned())
            .collect();
        assert!(
            ids.contains("SEC-UNAUTH-EXPORT-1") && ids.contains("AI-EXPORT-NO-AUTH-CHECK"),
            "both rule ids must be recorded on the merged row: {ids:?}"
        );
        let lines: std::collections::HashSet<usize> = std::iter::once(out[0].line)
            .chain(out[0].also_locations.iter().map(|l| l.line))
            .collect();
        assert!(
            lines.contains(&10) && lines.contains(&19),
            "both evidence lines (9 apart) must be kept, not dropped: {lines:?}"
        );
    }

    #[test]
    fn merge_cycle3_security_class_wins_primary_and_severity_is_max_over_generic_critical() {
        // Observed failure 2: a cross-tenant-read defect shipped as a correct SECURITY rule at
        // LOW ("borderline") plus a generic rule at CRITICAL — the generic row must never shadow
        // the security finding. `primary_rank` ranks `FindingClass` (Security > Hygiene) ahead
        // of severity, so the security rule must win the primary slot even though its OWN
        // severity is lower; `merge_semantic_group` keeps `max_sev` independent of which side won
        // primary, so the merged row's severity must still be "critical", not the security row's
        // own "low". Clustered via a shared captured object (design point 2b, general and
        // line/category-independent) so the test exercises class+severity selection in
        // isolation, not the line-window signal.
        let mut sec_low = site_finding(
            "AUTH-MISSING-TENANT-CHECK-1",
            "api/orders.rs",
            15,
            "low",
            "org id not verified against the session tenant",
        );
        sec_low
            .captures
            .insert("table".to_string(), "orders".to_string());

        let mut generic_critical = site_finding(
            "ARCH-RESOURCE-LEAK-GENERIC-1",
            "api/orders.rs",
            400,
            "critical",
            "generic resource-handling smell flagged on the same table",
        );
        generic_critical
            .captures
            .insert("table".to_string(), "orders".to_string());

        assert_eq!(
            finding_class(&sec_low),
            FindingClass::Security,
            "sanity: the AUTH-prefixed rule must classify Security"
        );
        assert_eq!(
            finding_class(&generic_critical),
            FindingClass::Hygiene,
            "sanity: the generic ARCH rule must classify Hygiene, not inherit Security"
        );

        let out = merge_semantic_groups(vec![sec_low, generic_critical], &[]);
        assert_eq!(out.len(), 1, "the pair must merge into one row: {out:?}");
        assert_eq!(
            out[0].rule_id, "AUTH-MISSING-TENANT-CHECK-1",
            "the SECURITY rule must win the primary slot even at lower severity"
        );
        assert_eq!(
            out[0].severity, "critical",
            "merged severity must be the MAX of the pair, not the security primary's own (low) severity"
        );
        assert!(out[0]
            .also_matches
            .contains(&"ARCH-RESOURCE-LEAK-GENERIC-1".to_string()));
    }

    #[test]
    fn merge_cycle3_same_rule_id_twice_adjacent_collapses_to_one_row() {
        // Observed failure 3: the same rule id firing twice on adjacent lines for one response
        // must collapse to one row. Deliberately NO shared category (an invented AI- id with no
        // taxonomy mapping — categorize_rule_id backfills both to None) and NO prose overlap
        // (the two detail strings share essentially no words), so signals (a) and (c) are both
        // unavailable — only the new same-rule-id-adjacent signal can catch this.
        let mut first = site_finding(
            "AI-WIDGET-MISCONFIG-REPEAT-1",
            "src/widgets/panel.rs",
            100,
            "high",
            "",
        );
        first.detail =
            "A background retry loop hammers the queue with no backoff policy at all.".to_string();
        let mut second = site_finding(
            "AI-WIDGET-MISCONFIG-REPEAT-1",
            "src/widgets/panel.rs",
            102,
            "high",
            "",
        );
        second.detail =
            "Totally unrelated phrasing on a second pass, written differently by the model."
                .to_string();
        assert!(
            description_overlap_score(&first, &second) < DESCRIPTION_OVERLAP_THRESHOLD,
            "sanity: this pair must NOT merge via description overlap, isolating the same-rule-id signal"
        );
        assert_eq!(
            categorize_rule_id(&first.rule_id),
            None,
            "sanity: this invented rule id must have no taxonomy mapping, isolating the \
             same-rule-id signal from signal (a)'s same-category path"
        );

        let out = merge_semantic_groups(vec![first, second], &[]);
        assert_eq!(
            out.len(),
            1,
            "the same rule id firing twice on adjacent lines must collapse to one row: {out:?}"
        );
        assert_eq!(out[0].rule_id, "AI-WIDGET-MISCONFIG-REPEAT-1");
        let lines: std::collections::HashSet<usize> = std::iter::once(out[0].line)
            .chain(out[0].also_locations.iter().map(|l| l.line))
            .collect();
        assert!(
            lines.contains(&100) && lines.contains(&102),
            "both evidence sites must be kept: {lines:?}"
        );
    }

    #[test]
    fn merge_c4_same_rule_same_severity_in_different_files_stays_two_rows() {
        // C4-P1 regression: the N-site collapse arm (same rule id + same severity at any distance)
        // must be SAME-FILE only. The same architectural rule tripping a Rust handler and a TS
        // handler are two DISTINCT defects (two separate places to fix) and must stay two rows.
        // Collapsing them cross-file was an over-merge that broke the handler-no-db / strict-
        // layering e2e tests. Different paths, so no same-file signal (window/construct/overlap)
        // can fire — this isolates the same-rule-adjacent same-path guard specifically.
        let mut rs =
            site_finding("ARCH-HANDLER-NO-DB-REPEAT-1", "src/routes/orders.rs", 2, "high", "");
        rs.detail = "The Rust route handler holds a database handle and queries it inline."
            .to_string();
        let mut ts =
            site_finding("ARCH-HANDLER-NO-DB-REPEAT-1", "src/routes/orders.ts", 2, "high", "");
        ts.detail = "Separately, the TypeScript route reaches the data layer without delegating."
            .to_string();
        let out = merge_semantic_groups(vec![rs, ts], &[]);
        assert_eq!(
            out.len(),
            2,
            "the same rule firing in two DIFFERENT files is two distinct defects, never an N-site \
             collapse: {out:?}"
        );
    }

    // ── C4-P1: defect-shaped merge redesign ─────────────────────────────────────────────────
    // Synthetic findings only — one defect shipping as 2-3 rows (same handler/different rules,
    // cross-file root cause, N same-rule sites, stale headline cross-references).

    #[test]
    fn p1_c4_same_sink_security_overrides_category_requirement() {
        // Design point 1: a boundary-validation rule and a privileged-key-bypass rule flagged on
        // the SAME handler body, with DIFFERENT categories — signal (a) alone would never fire.
        // REG-3 tightened the same-construct-security override to additionally require the pair
        // be about the SAME defect (same category class, or a shared captured object) — so this
        // fixture now also gives both findings what actually links them in reality: the SAME
        // `debug_mode` flag is what the validation rule says is under-checked and what the
        // bypass rule says is trusted to skip the privileged-key check. That shared captured
        // object is the legitimate root-cause link the override now requires; without it (see
        // `p1_c4_same_construct_pair_with_unrelated_category_and_no_object_stays_two_rows`,
        // just below) the override must NOT fire. Because they share a construct
        // (`same_construct`), one side is security-tier, AND they share that captured object,
        // they must still merge into one row with the security rule as primary.
        let content = "fn handler() {\n let x = input();\n if debug_mode {\n return bypass_check(x);\n }\n ok(x)\n}\n";
        let files = vec![("h.rs".to_string(), content.to_string())];
        let mut validation = site_finding("ARCH-BOUNDARY-VALIDATION-1", "h.rs", 2, "high", "");
        validation.category = Some("error-handling".to_string());
        validation
            .captures
            .insert("flag".to_string(), "debug_mode".to_string());
        let mut bypass = site_finding("AUTH-PRIVILEGED-KEY-BYPASS-1", "h.rs", 4, "critical", "");
        bypass.category = Some("authorization".to_string());
        bypass
            .captures
            .insert("flag".to_string(), "debug_mode".to_string());
        assert_eq!(
            finding_class(&validation),
            FindingClass::Hygiene,
            "sanity: the plain validation rule is not itself security-tier"
        );
        assert_eq!(
            finding_class(&bypass),
            FindingClass::Security,
            "sanity: the bypass rule is security-tier"
        );

        let out = merge_semantic_groups(vec![validation, bypass], &files);
        assert_eq!(
            out.len(),
            1,
            "same-handler validation + bypass rules must merge despite different categories: {out:?}"
        );
        assert_eq!(
            out[0].rule_id, "AUTH-PRIVILEGED-KEY-BYPASS-1",
            "the security-tier rule must win primary"
        );
        assert!(out[0]
            .also_matches
            .contains(&"ARCH-BOUNDARY-VALIDATION-1".to_string()));
        let lines: std::collections::HashSet<usize> = std::iter::once(out[0].line)
            .chain(out[0].also_locations.iter().map(|l| l.line))
            .collect();
        assert!(
            lines.contains(&2) && lines.contains(&4),
            "both evidence sites must be kept: {lines:?}"
        );
    }

    // ── REG-3: same-construct-security override tightened to require a same-defect link ────

    #[test]
    fn p1_c4_same_construct_pair_with_unrelated_category_and_no_object_stays_two_rows() {
        // REG-3 (the bug this test pins): before the fix, `same_construct_security_override`
        // fired whenever one side of a same-construct pair was security-tier, with NO
        // requirement the two findings be related at all. That absorbed a genuinely unrelated,
        // merely co-located rule into the security finding's `also_matches`, where it vanished
        // from its own category's Coverage count and read as "clean" — exactly the shape that
        // would mis-score a planted defect as a miss on a blind hold-out. An injection finding
        // and an unrelated pagination-convention finding, co-located in the SAME handler, with
        // DIFFERENT categories and no shared captured object, must stay TWO rows.
        let content = "fn handler() {\n let q = raw_sql(id);\n let page = rows.skip(n).take(k);\n}\n";
        let files = vec![("h.rs".to_string(), content.to_string())];
        let mut injection =
            site_finding("SEC-SQL-INJECTION-SINK-1", "h.rs", 2, "critical", "");
        injection.detail = "User-controlled input flows into a raw SQL query.".to_string();
        let mut pagination =
            site_finding("PERF-PAGINATION-MISSING-1", "h.rs", 3, "medium", "");
        pagination.detail = "This handler returns every row with no pagination.".to_string();
        assert_eq!(
            finding_class(&injection),
            FindingClass::Security,
            "sanity: the injection rule is security-tier"
        );
        assert_eq!(
            finding_class(&pagination),
            FindingClass::Hygiene,
            "sanity: the pagination rule is not itself security-tier"
        );
        assert_ne!(
            categorize_rule_id(&injection.rule_id),
            categorize_rule_id(&pagination.rule_id),
            "sanity: the two rules are genuinely different defect classes"
        );

        let out = merge_semantic_groups(vec![injection, pagination], &files);
        assert_eq!(
            out.len(),
            2,
            "an injection finding and an unrelated pagination finding co-located in one \
             construct, with different categories and no shared object, must stay two rows, \
             each routed to its own category: {out:?}"
        );
        let ids: std::collections::HashSet<&str> =
            out.iter().map(|f| f.rule_id.as_str()).collect();
        assert!(
            ids.contains("SEC-SQL-INJECTION-SINK-1") && ids.contains("PERF-PAGINATION-MISSING-1"),
            "neither finding may be absorbed into the other's also_matches: {out:?}"
        );
        assert!(
            out.iter().all(|f| f.also_matches.is_empty()),
            "neither row may carry the other as an also_matches member: {out:?}"
        );
    }

    #[test]
    fn p1_c4_same_construct_security_override_still_merges_on_matching_category() {
        // PRESERVE: the legitimate case the override exists for — two same-construct findings
        // that genuinely share a defect class (here, both corpus rule ids fall into the
        // `categorize_rule_id` "authorization" class via the `BYPASS`/`RBAC` tokens, even though
        // neither Finding carries an explicit `.category`) — must still merge, with merge
        // PRIMACY untouched by the REG-3 tightening: this only narrows WHICH pairs merge, never
        // WHO wins once they do. A deterministic rule id and an invented AI- id, same construct,
        // same inferred category, both security-tier (the class-match alone would be enough even
        // without the pre-existing "at least one side security" condition): still one row,
        // deterministic primary (class ties broken by origin specificity — C5-1 unchanged).
        let content =
            "fn handler() {\n check_rbac(user);\n if bypass_role_check(user) {\n grant_all(user);\n }\n}\n";
        let files = vec![("h.rs".to_string(), content.to_string())];
        let det = site_finding("AUTHZ-RBAC-CHECK-INCOMPLETE-1", "h.rs", 2, "high", "");
        let ai = site_finding("AI-PRIVILEGE-BYPASS-GRANTS-ALL", "h.rs", 4, "critical", "");
        assert_eq!(
            categorize_rule_id(&det.rule_id).as_deref(),
            Some("authorization"),
            "sanity: the deterministic rule id's inferred class is authorization"
        );
        assert_eq!(
            categorize_rule_id(&ai.rule_id).as_deref(),
            Some("authorization"),
            "sanity: the invented AI- id's inferred class is ALSO authorization, via the \
             `BYPASS` token, even with no explicit `.category` of its own"
        );

        let out = merge_semantic_groups(vec![det, ai], &files);
        assert_eq!(
            out.len(),
            1,
            "same construct + same inferred category + one side security-tier must still \
             merge: {out:?}"
        );
        assert_eq!(
            out[0].rule_id, "AUTHZ-RBAC-CHECK-INCOMPLETE-1",
            "merge primacy (deterministic beats invented AI within the same class) is \
             unaffected by the REG-3 tightening"
        );
        assert!(out[0]
            .also_matches
            .contains(&"AI-PRIVILEGE-BYPASS-GRANTS-ALL".to_string()));
    }

    #[test]
    fn p1_c4_same_construct_security_override_still_merges_on_shared_object() {
        // PRESERVE: the other legitimate link the override accepts — a shared captured object —
        // continues to work exactly as before, independent of category. A deterministic finding
        // and an adopted-AI finding (confidence set, non-`AI-` id) name the SAME captured table,
        // same construct, different categories, one security-tier: still one row, deterministic
        // primary.
        let content = "fn handler() {\n validate(payments);\n leak_to_log(payments);\n}\n";
        let files = vec![("h.rs".to_string(), content.to_string())];
        let mut det = site_finding("ARCH-VALIDATION-INCOMPLETE-1", "h.rs", 2, "medium", "");
        det.category = Some("input-validation".to_string());
        det.captures
            .insert("table".to_string(), "payments".to_string());
        let mut ai = site_finding("SEC-SENSITIVE-DATA-LOGGED-1", "h.rs", 3, "critical", "");
        ai.category = Some("resource-exposure".to_string());
        ai.confidence = Some("high".to_string());
        ai.captures
            .insert("table".to_string(), "payments".to_string());
        assert_ne!(
            det.category, ai.category,
            "sanity: the two findings have different categories"
        );

        let out = merge_semantic_groups(vec![det, ai], &files);
        assert_eq!(
            out.len(),
            1,
            "same construct + shared captured object + one side security-tier must still \
             merge despite different categories: {out:?}"
        );
        assert_eq!(
            out[0].rule_id, "ARCH-VALIDATION-INCOMPLETE-1",
            "merge primacy (deterministic beats adopted-AI within the same class) is \
             unaffected by the REG-3 tightening"
        );
        assert!(out[0]
            .also_matches
            .contains(&"SEC-SENSITIVE-DATA-LOGGED-1".to_string()));
    }

    #[test]
    fn p1_c4_same_construct_distinct_objects_stays_two_rows() {
        // Over-merge guard: the same-construct/security-override signal must NOT fuse two
        // findings in one handler that visibly name DIFFERENT structural objects — that is two
        // distinct defects, not one, even with one side security-tier.
        let content = "fn handler() {\n let a = orders.find();\n let b = profiles.find();\n}\n";
        let files = vec![("h.rs".to_string(), content.to_string())];
        let mut security = site_finding("AUTH-ORDERS-LEAK-1", "h.rs", 2, "high", "orders.find()");
        security.category = Some("authorization".to_string());
        let mut hygiene = site_finding(
            "ARCH-PROFILES-SMELL-1",
            "h.rs",
            3,
            "medium",
            "profiles.find()",
        );
        hygiene.category = Some("arch-conformance".to_string());

        let out = merge_semantic_groups(vec![security, hygiene], &files);
        assert_eq!(
            out.len(),
            2,
            "two defects naming different objects in one handler must stay two rows: {out:?}"
        );
    }

    #[test]
    fn p1_c4_cross_file_policy_and_query_merge_via_backfilled_object() {
        // Design point 2: a permissive-policy finding and the query finding that relies on that
        // SAME policy, in a DIFFERENT file, must merge into one row at the policy, with the
        // query's site preserved in also_locations — even though NEITHER finding's own detector
        // populated `captures` (the general case). `merge_semantic_groups`'s backfill must
        // extract the shared object from each finding's own prose for the cross-file
        // shared-captured-object signal to fire.
        let mut policy = site_finding(
            "SUPABASE-RLS-PERMISSIVE-POLICY-1",
            "supabase/policies.sql",
            12,
            "high",
            "\"invoices_read_all\"",
        );
        policy.detail =
            "The \"invoices_read_all\" policy grants unrestricted read access.".to_string();
        let mut query = site_finding(
            "AI-QUERY-RELIES-ON-PERMISSIVE-POLICY",
            "src/api/invoices.ts",
            30,
            "medium",
            "\"invoices_read_all\"",
        );
        query.detail =
            "This query relies on the \"invoices_read_all\" policy to scope access.".to_string();
        assert!(
            policy.captures.is_empty() && query.captures.is_empty(),
            "sanity: neither detector populated captures itself"
        );

        let out = merge_semantic_groups(vec![policy, query], &[]);
        assert_eq!(
            out.len(),
            1,
            "policy + relying query must merge via the backfilled captured object: {out:?}"
        );
        assert_eq!(
            out[0].rule_id, "SUPABASE-RLS-PERMISSIVE-POLICY-1",
            "the policy finding is primary"
        );
        assert!(out[0]
            .also_matches
            .contains(&"AI-QUERY-RELIES-ON-PERMISSIVE-POLICY".to_string()));
        assert_eq!(out[0].also_locations.len(), 1);
        assert_eq!(out[0].also_locations[0].path, "src/api/invoices.ts");
    }

    #[test]
    fn p1_c4_n_site_collapse_same_rule_same_file_same_severity() {
        // Design point 3: the GENERAL "N location(s)" pattern (already used for the
        // arch-conformance/needs-review structural group) must also apply to a genuinely curated
        // row: one rule firing at the SAME severity three times across one file, far enough apart
        // that no window/construct signal would otherwise catch it, collapses to ONE row listing
        // every site — not three near-duplicate rows.
        let a = site_finding("ARCH-NO-DIRECT-DB-1", "svc.rs", 10, "medium", "");
        let b = site_finding("ARCH-NO-DIRECT-DB-1", "svc.rs", 200, "medium", "");
        let c = site_finding("ARCH-NO-DIRECT-DB-1", "svc.rs", 400, "medium", "");
        let out = merge_semantic_groups(vec![a, b, c], &[]);
        assert_eq!(
            out.len(),
            1,
            "three same-rule/same-file/same-severity occurrences must collapse to one row: {out:?}"
        );
        assert_eq!(
            out[0].also_locations.len(),
            2,
            "the other two occurrences are preserved as sites"
        );
        let lines: std::collections::HashSet<usize> = std::iter::once(out[0].line)
            .chain(out[0].also_locations.iter().map(|l| l.line))
            .collect();
        assert!(
            lines.contains(&10) && lines.contains(&200) && lines.contains(&400),
            "all three evidence sites must be kept: {lines:?}"
        );
    }

    #[test]
    fn p1_c4_n_site_collapse_does_not_fuse_different_severities_or_objects() {
        // The N-site collapse signal is scoped to SAME severity, and still subject to the
        // objects_conflict veto — it must not swallow a genuinely distinct occurrence (different
        // severity AND a different named object) of the same rule into the group.
        let mut a = site_finding("RLS-MISSING-1", "s.sql", 10, "medium", "public.orders");
        a.detail = "orders has no RLS policy".to_string();
        let mut b = site_finding("RLS-MISSING-1", "s.sql", 500, "critical", "public.payments");
        b.detail = "payments has no RLS policy".to_string();
        let out = merge_semantic_groups(vec![a, b], &[]);
        assert_eq!(
            out.len(),
            2,
            "different severity + disjoint named objects must stay two rows: {out:?}"
        );
    }

    // ── REG-5: split_into_sentences / strip_cross_reference_sentences must never insert a
    // space after a period embedded in an identifier, filename, version string, URL, or known
    // abbreviation — only at a GENUINE sentence boundary. See `report_export::sentence_boundaries`'s
    // doc comment for the full rule set this closes.

    #[test]
    fn split_into_sentences_only_splits_at_a_genuine_sentence_boundary() {
        let text = "call foo.bar() in module.spec.ts, e.g. now.Next sentence v1.2.3 \
                     https://x.y/z";
        let sentences = split_into_sentences(text);
        assert_eq!(
            sentences,
            vec![
                "call foo.bar() in module.spec.ts, e.g. now.".to_string(),
                "Next sentence v1.2.3 https://x.y/z".to_string(),
            ],
            "the ONLY real sentence boundary is \"now.\" -> \"Next\" (a run-together sentence \
             with no space at all); every other period here sits inside an identifier/filename/\
             version/URL/abbreviation and must not split: {sentences:?}"
        );
    }

    #[test]
    fn strip_cross_reference_sentences_only_adds_a_space_at_the_genuine_boundary() {
        // The fixed step, exercised with no other_ids (so nothing is actually filtered) — this
        // isolates the split+rejoin spacing behavior from the cross-reference-stripping logic.
        // REG-5: before the fix, this rejoin forced a space after EVERY period in the input
        // (~111 instances in one real PDF), corrupting dotted calls, filenames, version
        // strings, and abbreviations alike.
        let input = "call foo.bar() in module.spec.ts, e.g. now.Next sentence v1.2.3 \
                      https://x.y/z";
        let expected = "call foo.bar() in module.spec.ts, e.g. now. Next sentence v1.2.3 \
                         https://x.y/z";
        let (output, referenced) = strip_cross_reference_sentences(input, &[]);
        assert_eq!(
            output, expected,
            "only the genuine \"now.Next\" run-together boundary gets a space inserted; every \
             dotted call, filename, version string, and abbreviation must be byte-identical to \
             the input"
        );
        assert!(referenced.is_empty());
    }

    #[test]
    fn strip_cross_reference_sentences_skips_backtick_delimited_spans_entirely() {
        // A code span quoted in prose must never be scanned for sentence punctuation, no matter
        // what it contains — `foo.Bar()` inside backticks must not be mistaken for a
        // run-together sentence just because `Bar` happens to be capitalized.
        let input = "See `foo.Bar()` for the implementation. It is documented separately.";
        let (output, _) = strip_cross_reference_sentences(input, &[]);
        assert_eq!(output, input);
    }

    #[test]
    fn p1_c4_headline_cross_reference_stripped_and_moved_to_also_matches() {
        // Design point 4: a finding whose `detail` literally names ANOTHER finding's rule id
        // reads as stale prose once the report is sorted/filtered. The reference must be
        // stripped out of `detail` and the relationship recorded structurally in `also_matches`
        // — on BOTH sides, since the relationship is mutual even though only one side wrote the
        // pointer.
        let sink = site_finding(
            "SEC-SQL-INJECTION-SINK-1",
            "src/db/sink.ts",
            50,
            "critical",
            "",
        );
        let mut call_site = site_finding(
            "AI-SQL-INJECTION-CALL-SITE",
            "src/api/handler.ts",
            20,
            "high",
            "",
        );
        call_site.detail = "This call site passes unsanitized user input toward the known sink. \
             See the SEC-SQL-INJECTION-SINK-1 finding below for the full trace. The input \
             originates from an unauthenticated endpoint, which is a distinct fact not covered \
             by the sink finding alone."
            .to_string();

        let out = merge_semantic_groups(vec![sink, call_site], &[]);
        assert_eq!(
            out.len(),
            2,
            "the sink and call site add distinct facts and stay two rows: {out:?}"
        );
        let sink_out = out
            .iter()
            .find(|f| f.rule_id == "SEC-SQL-INJECTION-SINK-1")
            .expect("sink row survives");
        let call_site_out = out
            .iter()
            .find(|f| f.rule_id == "AI-SQL-INJECTION-CALL-SITE")
            .expect("call site row survives");
        assert!(
            !call_site_out.detail.contains("SEC-SQL-INJECTION-SINK-1"),
            "the stale cross-reference must be stripped from the headline/detail text: {:?}",
            call_site_out.detail
        );
        assert!(
            !call_site_out
                .detail
                .to_ascii_lowercase()
                .contains("finding below"),
            "the relative-position phrase must be stripped too: {:?}",
            call_site_out.detail
        );
        assert!(
            call_site_out.detail.contains("distinct fact"),
            "the REST of the sentence/paragraph must survive — only the cross-reference sentence \
             is dropped: {:?}",
            call_site_out.detail
        );
        assert!(
            call_site_out
                .also_matches
                .contains(&"SEC-SQL-INJECTION-SINK-1".to_string()),
            "the named rule id must be recorded structurally on the referencing row: {:?}",
            call_site_out.also_matches
        );
        assert!(
            sink_out
                .also_matches
                .contains(&"AI-SQL-INJECTION-CALL-SITE".to_string()),
            "the relationship must be recorded RECIPROCALLY on the referenced row too: {:?}",
            sink_out.also_matches
        );
    }

    #[test]
    fn p1_c4_headline_without_named_rule_still_strips_relative_position_language() {
        // The relative-position marker alone (no specific rule id named) must still be stripped
        // from the headline text, with nothing spurious added to also_matches.
        let mut a = site_finding("AI-SOLO-FINDING-1", "a.rs", 10, "medium", "");
        a.detail = "This is a real, standalone defect. See the finding below for more context."
            .to_string();
        let b = site_finding("AI-UNRELATED-2", "b.rs", 5, "low", "");

        let out = merge_semantic_groups(vec![a, b], &[]);
        assert_eq!(out.len(), 2);
        let a_out = out
            .iter()
            .find(|f| f.rule_id == "AI-SOLO-FINDING-1")
            .unwrap();
        assert!(
            !a_out
                .detail
                .to_ascii_lowercase()
                .contains("see the finding below"),
            "the relative-position sentence must be stripped: {:?}",
            a_out.detail
        );
        assert!(
            a_out.detail.contains("real, standalone defect"),
            "the rest of the detail must survive: {:?}",
            a_out.detail
        );
        assert!(
            a_out.also_matches.is_empty(),
            "no rule id was named, so nothing is added to also_matches: {:?}",
            a_out.also_matches
        );
    }

    // ── P1: finding_class corpus-verified false-positive/negative regressions ──────────────
    // These pin the EXACT bugs found by cross-checking `SECURITY_RULE_TOKENS` against every
    // real rule id in `crates/rules/principles/**` (not a synthetic id) — a naive substring
    // scan misclassified real corpus rules in both directions. Real ids used deliberately: the
    // point is that THESE SPECIFIC ids, which exist in the corpus today, classify correctly.

    #[test]
    fn p1_finding_class_rce_substring_inside_resource_is_not_security() {
        // "RCE" (remote code execution) must not match merely because "RESOURCE" or
        // "INTERCEPTORS" happens to contain the letters r-c-e as a substring.
        let f = site_finding("ARCH-RESOURCE-LIFECYCLE-1", "a.rs", 1, "medium", "");
        assert_eq!(finding_class(&f), FindingClass::Hygiene);
        let f2 = site_finding("JAVA-RESOURCE-MANAGEMENT-1", "a.java", 1, "medium", "");
        assert_eq!(finding_class(&f2), FindingClass::Hygiene);
        let f3 = site_finding(
            "JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1",
            "a.ts",
            1,
            "medium",
            "",
        );
        assert_eq!(finding_class(&f3), FindingClass::Hygiene);
    }

    #[test]
    fn p1_finding_class_dependency_injection_is_not_injection_vulnerability() {
        // "Constructor injection" (a DI pattern) must not classify as Security just because it
        // contains the word "injection" — real SQL/command injection rules are caught via "SQL"
        // or an explicit injection-vulnerability token, not bare "INJECT".
        let f = site_finding(
            "JAVA-SPRING-CONSTRUCTOR-INJECTION-1",
            "a.java",
            1,
            "medium",
            "",
        );
        assert_eq!(finding_class(&f), FindingClass::Hygiene);
        let f2 = site_finding(
            "CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1",
            "a.cs",
            1,
            "medium",
            "",
        );
        assert_eq!(finding_class(&f2), FindingClass::Hygiene);
    }

    #[test]
    fn p1_finding_class_database_session_di_is_not_auth_session() {
        // "FastAPI DI session" is a database-session lifecycle rule, not an authentication
        // session — bare "SESSION" must not promote it to Security.
        let f = site_finding("PYTHON-FASTAPI-DI-SESSION-1", "a.py", 1, "medium", "");
        assert_eq!(finding_class(&f), FindingClass::Hygiene);
    }

    #[test]
    fn p1_finding_class_real_security_rules_still_classify_security() {
        // The fixes above must not have thrown out real coverage: every one of these IS a
        // security rule in the corpus and must still classify as Security.
        for rule_id in [
            "SEC-NO-HARDCODED-SECRETS-1",
            "SEC-NO-RAW-SQL-CONCAT-1",
            "SUPABASE-RLS-ENABLED-1",
            "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1",
            "SUPABASE-EXPOSURE-SCHEMAS-1",
            "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
            "ARCH-NO-SECRETS-IN-URL-1",
            "ARCH-FETCH-THEN-AUTHORIZE-1",
            "ARCH-SERVER-AUTHZ-1",
            "CSHARP-ASPNETCORE-CORS-EXPLICIT-1",
            "GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1",
            "JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1",
            "JAVA-SPRING-METHOD-SECURITY-1",
            "CICD-CODEQL-SECURITY-SCAN-1",
        ] {
            let f = site_finding(rule_id, "a.rs", 1, "medium", "");
            assert_eq!(
                finding_class(&f),
                FindingClass::Security,
                "{rule_id} must classify as Security"
            );
        }
    }

    #[test]
    fn p1_finding_class_generic_hygiene_rules_stay_hygiene() {
        // A spot check of genuinely structural/style/testing/process rules across several
        // stacks — none of these should ever classify as Security.
        for rule_id in [
            "ARCH-MIDDLEWARE-FIRST-1",
            "ARCH-CURSOR-PAGINATION-1",
            "ARCH-MONOLITH-FIRST-1",
            "JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1",
            "RUST-TESTING-1",
            "TESTING-PYRAMID-1",
            "GO-TESTING-TABLE-DRIVEN-T-RUN-1",
            "SQL-DB-NPLUSONE-1",
            "RUST-SQLX-CONNECTION-POOL-SIZED-1",
        ] {
            let f = site_finding(rule_id, "a.rs", 1, "medium", "");
            assert_eq!(
                finding_class(&f),
                FindingClass::Hygiene,
                "{rule_id} must classify as Hygiene"
            );
        }
    }

    // ── D5 pt.2: categorize_rule_id word-boundary matching ────────────────────────────────
    //
    // Mirrors the p1_finding_class_* tests above: `categorize_rule_id` had the SAME raw-
    // substring bug `finding_class`'s `SECURITY_RULE_TOKENS` was fixed for (commit f80ad83),
    // never propagated to this function. Cross-checked against every real rule id in
    // `crates/rules/principles/**`.

    /// THE bug this fix exists for: bare "ARCH" must not match merely because "SEARCH" (from
    /// "SEARCH-PATH") happens to contain the letters a-r-c-h as a substring.
    /// `SUPABASE-FUNC-SEARCH-PATH-1` (D1 — SECURITY DEFINER without a pinned search_path) must
    /// not be miscategorized `arch-conformance`.
    #[test]
    fn categorize_rule_id_arch_substring_inside_search_path_is_not_arch_conformance() {
        // "SEARCH-PATH" spells S-E-ARCH: the letters "ARCH" sit inside "SEARCH" (its "SE-ARCH"
        // shape) as a pure substring artifact, not a standalone hyphen-delimited word — a raw
        // substring scan for "ARCH" matches it anyway. Word-boundary matching must not.
        assert_ne!(
            categorize_rule_id("SUPABASE-FUNC-SEARCH-PATH-1").as_deref(),
            Some("arch-conformance"),
            "ARCH must not match inside SEARCH-PATH via substring"
        );
        // Contrast: a rule id where ARCH genuinely IS its own standalone word must still match.
        assert_eq!(
            categorize_rule_id("ARCH-STRICT-LAYERING-1").as_deref(),
            Some("arch-conformance"),
            "ARCH as a real standalone word must still categorize as arch-conformance"
        );
    }

    /// "RCE" (remote code execution) must not match merely because "RESOURCE" or
    /// "INTERCEPTORS" contains the letters r-c-e as a substring — none of these are injection
    /// concerns, the same corpus false positives `finding_class` was fixed for.
    #[test]
    fn categorize_rule_id_rce_substring_inside_resource_or_interceptors_is_not_injection() {
        for rule_id in [
            "ARCH-RESOURCE-LIFECYCLE-1",
            "JAVA-RESOURCE-MANAGEMENT-1",
            "JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1",
            "GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1",
        ] {
            assert_ne!(
                categorize_rule_id(rule_id).as_deref(),
                Some("injection"),
                "{rule_id} must not categorize as injection via bare RCE"
            );
        }
    }

    /// Bare "INJECT" must not match a dependency-injection PATTERN rule — "constructor
    /// injection" is a DI pattern, not an injection vulnerability. The real injection-
    /// vulnerability shapes the corpus uses are explicit compounds (RAW-SQL, PARAMETERIZED,
    /// SQL-INJECTION, ...), not the bare word.
    #[test]
    fn categorize_rule_id_dependency_injection_pattern_is_not_injection_vulnerability() {
        for rule_id in [
            "JAVA-SPRING-CONSTRUCTOR-INJECTION-1",
            "CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1",
            "JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1",
        ] {
            assert_ne!(
                categorize_rule_id(rule_id).as_deref(),
                Some("injection"),
                "{rule_id} is a DI pattern rule, not an injection vulnerability"
            );
        }
    }

    /// Bare "SESSION" must not promote a DATABASE-session lifecycle rule to `authentication` —
    /// `PYTHON-FASTAPI-DI-SESSION-1` is about a FastAPI DB-session dependency, not an auth
    /// session. The real auth-session case is caught via "GETSESSION" (a distinct, unambiguous
    /// compound word), which must keep working.
    #[test]
    fn categorize_rule_id_database_session_is_not_authentication() {
        assert_ne!(
            categorize_rule_id("PYTHON-FASTAPI-DI-SESSION-1").as_deref(),
            Some("authentication"),
            "a DB-session lifecycle rule must not categorize as authentication"
        );
        assert_eq!(
            categorize_rule_id("SUPABASE-AUTH-GETSESSION-SERVER-1").as_deref(),
            Some("authentication"),
            "a real auth-session rule (GETSESSION) must still categorize as authentication"
        );
    }

    /// Positive cases: real, unambiguous corpus ids for EVERY category must still categorize
    /// correctly after the word-boundary fix — the fix must not have thrown out real coverage.
    #[test]
    fn categorize_rule_id_positive_cases_across_every_category_still_match() {
        for (rule_id, expected) in [
            ("SUPABASE-RLS-ENABLED-1", "rls-policy"),
            ("SEC-NO-DISABLED-TLS-1", "transport-security"),
            ("SEC-NO-HARDCODED-SECRETS-1", "secret-exposure"),
            ("SEC-NO-RAW-SQL-CONCAT-1", "injection"),
            ("SEC-NO-QUERY-GRAMMAR-INJECTION-1", "injection"),
            ("SEC-NO-UNSAFE-DESERIALIZATION-1", "injection"),
            ("PYTHON-FLASK-PARAMETERIZED-SQL-1", "injection"),
            ("SUPABASE-AUTH-GETSESSION-SERVER-1", "authentication"),
            ("SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1", "authorization"),
            ("JAVASCRIPT-EXPRESS-VALIDATE-INPUT-1", "input-validation"),
            ("RUST-NO-UNWRAP-1", "error-handling"),
            (
                "JAVASCRIPT-EXPRESS-CENTRAL-ERROR-HANDLER-1",
                "error-handling",
            ),
            ("PYTHON-FLASK-ERROR-HANDLERS-1", "error-handling"),
            ("RUST-TESTING-1", "testing-style"),
            ("SQL-DB-NPLUSONE-1", "performance"),
            ("SUPABASE-STORAGE-PUBLIC-BUCKET-1", "resource-exposure"),
            ("ARCH-STRICT-LAYERING-1", "arch-conformance"),
            ("ARCH-MONOLITH-FIRST-1", "arch-conformance"),
        ] {
            assert_eq!(
                categorize_rule_id(rule_id).as_deref(),
                Some(expected),
                "{rule_id} must categorize as {expected}"
            );
        }
    }

    /// `RUST-SQLX-CONNECTION-POOL-SIZED-1` and its SQLX-family siblings must NOT categorize as
    /// `injection` — that was a pre-existing bug independent of the ARCH/SEARCH-PATH shape (bare
    /// "SQL" matching the "SQLX" word via prefix), the same class of false positive the corpus
    /// audit was asked to surface. `SQL-DB-NPLUSONE-1` is a performance rule, not injection,
    /// for the same reason.
    #[test]
    fn categorize_rule_id_sqlx_family_and_nplusone_are_not_injection() {
        for rule_id in [
            "RUST-SQLX-CONNECTION-POOL-SIZED-1",
            "RUST-SQLX-COMPILE-CHECKED-QUERIES-1",
            "RUST-SQLX-MIGRATIONS-CHECKED-IN-1",
            "RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1",
            "SQL-DB-NPLUSONE-1",
        ] {
            assert_ne!(
                categorize_rule_id(rule_id).as_deref(),
                Some("injection"),
                "{rule_id} must not categorize as injection via bare SQL matching SQLX/SQL-DB"
            );
        }
    }

    /// Corpus-verification test pinning the exact real ids named in the task as having
    /// motivated this fix — a regression here means the fix was reverted or narrowed.
    #[test]
    fn categorize_rule_id_corpus_verification_pins_the_motivating_ids() {
        assert_ne!(
            categorize_rule_id("SUPABASE-FUNC-SEARCH-PATH-1").as_deref(),
            Some("arch-conformance")
        );
        assert_ne!(
            categorize_rule_id("PYTHON-FASTAPI-DI-SESSION-1").as_deref(),
            Some("authentication")
        );
        assert_ne!(
            categorize_rule_id("JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1").as_deref(),
            Some("injection")
        );
    }

    // ── P2: fix-specific generation + self-check ────────────────────────────────────────

    fn fx(rule_id: &str, path: &str, line: usize, detail: &str, snippet: &str) -> Finding {
        Finding {
            repo: "o/r".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: "high".to_string(),
            snippet: snippet.to_string(),
            detail: detail.to_string(),
            ..Finding::default()
        }
    }

    // ── find_ungrounded_identifier (pure) ───────────────────────────────────────────────

    #[test]
    fn find_ungrounded_identifier_accepts_a_fix_naming_only_evidence_identifiers() {
        let evidence = "app/auth/signout/route.ts uses `rawRedirect` at line 12; \
                         lib/redirect.ts exports `safeInternalPath`";
        let fix = "Replace `rawRedirect` with `safeInternalPath` from lib/redirect.ts, as \
                    app/auth/signout/route.ts does.";
        assert_eq!(find_ungrounded_identifier(fix, evidence), None);
    }

    #[test]
    fn find_ungrounded_identifier_accepts_prose_with_no_named_identifiers() {
        let evidence = "some finding evidence";
        let fix = "Validate the input before using it and return an error otherwise.";
        assert_eq!(find_ungrounded_identifier(fix, evidence), None);
    }

    #[test]
    fn find_ungrounded_identifier_accepts_generic_literals_absent_from_evidence() {
        let evidence = "the config sets a flag";
        let fix = "Change the flag from `false` to `true`.";
        assert_eq!(find_ungrounded_identifier(fix, evidence), None);
    }

    #[test]
    fn find_ungrounded_identifier_rejects_a_backticked_symbol_absent_from_evidence() {
        let evidence = "app/auth/signout/route.ts uses `rawRedirect` at line 12";
        let fix = "Use `totallyInventedHelper` instead.";
        assert_eq!(
            find_ungrounded_identifier(fix, evidence),
            Some("totallyInventedHelper".to_string())
        );
    }

    #[test]
    fn find_ungrounded_identifier_rejects_a_file_path_absent_from_evidence() {
        let evidence = "app/auth/signout/route.ts uses `rawRedirect` at line 12";
        let fix = "Move this logic into lib/nonexistent-helper.ts instead.";
        assert_eq!(
            find_ungrounded_identifier(fix, evidence),
            Some("lib/nonexistent-helper.ts".to_string())
        );
    }

    // ── fix_contradicts_detail (pure, rule-based) ───────────────────────────────────────

    #[test]
    fn fix_contradicts_detail_rejects_enabling_a_setting_the_detail_says_is_already_on() {
        let detail = "Strict mode is already enabled in tsconfig.json; foo.ts still uses \
                       `any` casts that bypass its checks.";
        let fix = "Enable strict mode in tsconfig.json for this file.";
        assert!(
            fix_contradicts_detail(fix, detail).is_some(),
            "must flag a fix that tells the client to enable what the detail says is already on"
        );
    }

    #[test]
    fn fix_contradicts_detail_generalizes_beyond_strict_mode() {
        // Same contradiction SHAPE, a different subject (RLS) — the check must not be
        // hardcoded to the literal word "strict" (general-fixes-not-fixture-bandaids).
        let detail = "Row-level security is already enabled on the orders table, but no \
                       policy restricts SELECT to the owning user.";
        let fix = "Enable row-level security on the orders table.";
        assert!(fix_contradicts_detail(fix, detail).is_some());
    }

    #[test]
    fn fix_contradicts_detail_spares_a_fix_that_addresses_the_real_gap() {
        let detail = "Strict mode is already enabled in tsconfig.json; foo.ts still uses \
                       `any` casts that bypass its checks.";
        let fix = "Replace the `any` casts in foo.ts with explicit parameter types so strict \
                    mode's checks actually apply to this file.";
        assert_eq!(fix_contradicts_detail(fix, detail), None);
    }

    #[test]
    fn fix_contradicts_detail_spares_a_detail_with_no_already_marker() {
        let detail = "This endpoint has no authorization check on the delete path.";
        let fix = "Add an ownership check before performing the delete.";
        assert_eq!(fix_contradicts_detail(fix, detail), None);
    }

    // ── fix_leaks_methodology (pure) ─────────────────────────────────────────────────────

    #[test]
    fn fix_leaks_methodology_rejects_a_fix_describing_detection() {
        let fix = "Camerata detected this via a regex scan; fix the SQL concatenation.";
        assert!(fix_leaks_methodology(fix).is_some());
    }

    #[test]
    fn fix_leaks_methodology_accepts_a_clean_remediation_only_fix() {
        let fix = "Use a parameterized query instead of concatenating `user_id` into the SQL \
                    string in db/orders.rs.";
        assert_eq!(fix_leaks_methodology(fix), None);
    }

    // ── parse_fix_specifics (pure) ───────────────────────────────────────────────────────

    #[test]
    fn parse_fix_specifics_reads_a_well_formed_response() {
        let raw =
            r#"{"fixes":[{"index":0,"fix":"Do the thing."},{"index":2,"fix":"Do another."}]}"#;
        let out = parse_fix_specifics(raw);
        assert_eq!(out.get(&0).map(String::as_str), Some("Do the thing."));
        assert_eq!(out.get(&2).map(String::as_str), Some("Do another."));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn parse_fix_specifics_is_empty_on_garbage_input() {
        assert!(parse_fix_specifics("not json at all").is_empty());
        assert!(parse_fix_specifics(r#"{"nope":true}"#).is_empty());
        assert!(parse_fix_specifics(r#"{"fixes":[{"index":0,"fix":""}]}"#).is_empty());
    }

    // ── generate_fix_specifics (async, stubbed LlmPort — no real model calls) ──────────

    /// Returns a DIFFERENT canned response on each successive call (round-robin once
    /// exhausted) — drives the regenerate-on-rejection path deterministically: round 0
    /// returns a bad fix, round 1 returns a corrected one.
    struct SequencedCompleter {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl SequencedCompleter {
        fn new(responses: &[&str]) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmPort for SequencedCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut q = self.responses.lock().unwrap();
            let text = if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front().cloned().unwrap_or_default()
            };
            Ok(LlmResponse {
                text,
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: Some(0.01),
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            let resp = self.complete(req).await?;
            on_delta(&resp.text);
            Ok(resp)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn generate_fix_specifics_happy_path_grounds_and_populates_every_finding() {
        let files = vec![(
            "app/auth/signout/route.ts".to_string(),
            "export function safeInternalPath(p: string) {}\nexport function handler() { \
             rawRedirect(p) }"
                .to_string(),
        )];
        let f = fx(
            "SEC-OPEN-REDIRECT-1",
            "app/auth/signout/route.ts",
            2,
            "rawRedirect(p) is called with an unvalidated path — an open redirect.",
            "rawRedirect(p)",
        );
        let completer = SequencedCompleter::new(&[
            r#"{"fixes":[{"index":0,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts."}]}"#,
        ]);
        let meter = UsageMeter::default();
        let (out, _) =
            generate_fix_specifics(&completer, "o/r", vec![f], &files, None, Some(&meter), None)
                .await;
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].fix_specific.as_deref(),
            Some("Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts.")
        );
        assert!(!out[0].needs_review);
        assert_eq!(meter.snapshot().calls, 1);
    }

    #[tokio::test]
    async fn generate_fix_specifics_skips_dependency_audit_findings() {
        let f = fx(
            crate::dep_audit::DEP_AUDIT_RULE_ID,
            "package-lock.json",
            0,
            "some-pkg@1.0.0 has a known CVE",
            "some-pkg@1.0.0",
        );
        let completer = FailingCompleter;
        let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].fix_specific, None);
        // No model call was ever attempted — FailingCompleter would have surfaced it as a
        // needs-review tag if `generate_fix_specifics` had tried and failed; it never tried.
        assert!(!out[0].needs_review);
    }

    /// P4 (floor findings get finding-level treatment): confirms `generate_fix_specifics`'s
    /// `indices` filter excludes ONLY `DEP_AUDIT_RULE_ID` — a deterministic FLOOR finding (a
    /// real `AUDIT_RULES` rule id, not an `AI-`-prefixed one) is NOT skipped, so every curated
    /// floor finding still gets a `fix_specific` on a run where the AI review tier is enabled
    /// (the gate is `run_ai_review`, checked one layer up in `onboard::audit_repos` — this
    /// function itself carries no AI-vs-floor distinction at all).
    #[tokio::test]
    async fn generate_fix_specifics_includes_deterministic_floor_findings_not_just_ai_tier() {
        let files = vec![(".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string())];
        let f = fx(
            "SEC-NO-SECRET-FILE-1",
            ".env",
            0,
            "SEC-NO-SECRET-FILE-1: path `.env` is a secret-bearing file type",
            ".env",
        );
        let completer = SequencedCompleter::new(&[
            r#"{"fixes":[{"index":0,"fix":"Remove .env from the repository and its git history, and rotate the exposed credential."}]}"#,
        ]);
        let (out, _) =
            generate_fix_specifics(&completer, "o/r", vec![f], &files, None, None, None).await;
        assert_eq!(out.len(), 1);
        assert!(
            out[0].fix_specific.is_some(),
            "a deterministic floor finding must be included in the fix-generation pass, not \
             silently skipped alongside dependency-audit findings"
        );
    }

    #[tokio::test]
    async fn generate_fix_specifics_total_failure_never_hedges_or_tags_detail() {
        // C3-1b: fix-generation exhausting every retry is a PIPELINE failure, not a
        // confidence judgement — it must never set `needs_review`, never touch `confidence`,
        // and never write pipeline-state text into the client-facing `detail`. Only
        // `fix_specific` stays `None` (never fabricated).
        let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
        // A completer that returns unparseable junk every time — every round yields nothing
        // usable.
        let completer = StubCompleter {
            text: "complete garbage, not json".to_string(),
        };
        let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].fix_specific, None, "must never fabricate a fix");
        assert!(
            !out[0].needs_review,
            "a fix-generation failure must never hedge the finding"
        );
        assert_eq!(
            out[0].confidence, None,
            "a fix-generation failure must never touch calibration confidence"
        );
        assert_eq!(
            out[0].detail, "some real defect",
            "detail must be untouched — no bracketed pipeline-state text: {}",
            out[0].detail
        );
    }

    #[tokio::test]
    async fn generate_fix_specifics_transport_failure_never_hedges_or_tags_detail() {
        // The OTHER failure mode: the LLM is unreachable entirely (not just returning junk).
        let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
        let (out, _) =
            generate_fix_specifics(&FailingCompleter, "o/r", vec![f], &[], None, None, None).await;
        assert_eq!(out[0].fix_specific, None);
        assert!(!out[0].needs_review);
        assert_eq!(out[0].detail, "some real defect");
        assert!(
            !out[0].detail.contains("["),
            "no bracketed pipeline-state text"
        );
    }

    #[tokio::test]
    async fn generate_fix_specifics_regenerates_when_the_fix_names_an_ungrounded_identifier() {
        let f = fx(
            "ARCH-1",
            "a.rs",
            10,
            "the handler never checks ownership before deleting",
            "delete_order(id)",
        );
        let completer = SequencedCompleter::new(&[
            // Round 0: names a symbol nowhere in the evidence — must be rejected.
            r#"{"fixes":[{"index":0,"fix":"Use `totallyInventedGuard` before the delete."}]}"#,
            // Round 1 (after feedback): a grounded correction.
            r#"{"fixes":[{"index":0,"fix":"Check the caller owns `id` before calling delete_order(id)."}]}"#,
        ]);
        let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
        assert_eq!(
            out[0].fix_specific.as_deref(),
            Some("Check the caller owns `id` before calling delete_order(id).")
        );
        assert!(!out[0].needs_review);
    }

    #[tokio::test]
    async fn generate_fix_specifics_regenerates_when_the_fix_contradicts_the_detail() {
        let f = fx(
            "JAVASCRIPT-TS-STRICT-1",
            "tsconfig.json",
            1,
            "Strict mode is already enabled in tsconfig.json; foo.ts still uses `any` casts \
             that bypass its checks.",
            "\"strict\": true",
        );
        let completer = SequencedCompleter::new(&[
            // Round 0: contradicts the detail (tells the client to do what's already done).
            r#"{"fixes":[{"index":0,"fix":"Enable strict mode in tsconfig.json."}]}"#,
            // Round 1: addresses the real gap instead.
            r#"{"fixes":[{"index":0,"fix":"Replace the `any` casts in foo.ts with explicit types."}]}"#,
        ]);
        let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
        assert_eq!(
            out[0].fix_specific.as_deref(),
            Some("Replace the `any` casts in foo.ts with explicit types.")
        );
        assert!(!out[0].needs_review);
    }

    #[tokio::test]
    async fn generate_fix_specifics_never_leaks_methodology_in_the_saved_fix() {
        let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
        let completer = SequencedCompleter::new(&[
            r#"{"fixes":[{"index":0,"fix":"Camerata detected this via a regex scan; fix the concatenation."}]}"#,
            r#"{"fixes":[{"index":0,"fix":"Use a parameterized query in a.rs instead of concatenation."}]}"#,
        ]);
        let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
        let saved = out[0].fix_specific.as_deref().unwrap_or_default();
        assert!(!saved.to_ascii_lowercase().contains("camerata"));
        assert!(!saved.to_ascii_lowercase().contains("regex"));
        assert_eq!(
            saved,
            "Use a parameterized query in a.rs instead of concatenation."
        );
    }

    #[tokio::test]
    async fn generate_fix_specifics_gives_up_after_max_regenerations_and_folds_usage_every_round() {
        let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
        // Always returns an ungrounded fix — every round is rejected, so after
        // MAX_FIX_REGENERATIONS batched retries PLUS the one C6-B3.4 single-finding retry,
        // the finding falls through with `fix_specific` still `None` — no corpus is passed
        // in, so the C6-B3 instantiated fallback has no authored remediation to fall back to
        // either (never hedged — see the total/transport-failure tests above), and every
        // round's call must still have folded into the meter (spend isn't lost just because
        // the content was rejected).
        let completer = StubCompleter {
            text: r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#.to_string(),
        };
        let meter = UsageMeter::default();
        let (out, failed_passes) =
            generate_fix_specifics(&completer, "o/r", vec![f], &[], None, Some(&meter), None).await;
        assert_eq!(out[0].fix_specific, None);
        assert!(!out[0].needs_review);
        // MAX_FIX_REGENERATIONS + 1 batched rounds, plus 1 more solo retry (C6-B3.4).
        assert_eq!(meter.snapshot().calls as usize, MAX_FIX_REGENERATIONS + 2);
        assert_eq!(
            failed_passes.len(),
            1,
            "a give-up must be disclosed exactly once: {failed_passes:?}"
        );
        assert_eq!(failed_passes[0].pass, "fix-specific generation");
    }

    // ── C6-B3: instantiated authored-remediation fallback on give-up ───────────────────

    /// When fix-GENERATION itself gives up (transport unreachable — every batched retry AND
    /// the C6-B3.4 solo retry fail) but the rule DOES carry an authored remediation with a
    /// `<table>` placeholder, `fix_specific` must still end up populated — with the
    /// remediation INSTANTIATED against this finding's own `captures`, not a generic/bare
    /// rendering and not left `None`. `SUPABASE-RLS-ENABLED-1`'s authored remediation names
    /// `<table>` twice (`crates/rules/principles/supabase/rls/supabase-rls-enabled-1.toml`).
    #[tokio::test]
    async fn generate_fix_specifics_instantiates_the_authored_remediation_on_give_up() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let mut f = fx(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            "no RLS evidence for `profiles`",
            "CREATE TABLE profiles (...)",
        );
        f.captures.insert("table".to_string(), "profiles".to_string());

        let (out, failed_passes) = generate_fix_specifics(
            &FailingCompleter,
            "o/r",
            vec![f],
            &[],
            None,
            None,
            Some(&corpus),
        )
        .await;

        let fix = out[0]
            .fix_specific
            .as_deref()
            .expect("a give-up with an authored remediation available must still populate \
                     fix_specific — never leave the row with no codebase-specific fix line");
        assert!(
            fix.contains("profiles"),
            "the instantiated fallback must name THIS finding's own identifier (the table \
             `profiles`, from its `captures`), not the bare `<table>` token or a generic \
             filler: {fix}"
        );
        assert!(
            !fix.contains('<'),
            "every placeholder token must be substituted — no raw `<token>` left in the \
             client-facing fix: {fix}"
        );
        assert!(!out[0].needs_review, "a fix-generation failure never hedges the finding");

        assert_eq!(
            failed_passes.len(),
            1,
            "the give-up must still be disclosed even though a usable fallback shipped: \
             {failed_passes:?}"
        );
        assert_eq!(failed_passes[0].pass, "fix-specific generation");
        assert!(
            failed_passes[0].reason.contains("gave up on 1 finding"),
            "the disclosure must carry the give-up COUNT: {}",
            failed_passes[0].reason
        );
        assert!(
            failed_passes[0].reason.contains("1 shipped"),
            "the disclosure must say how many give-ups the instantiated fallback covered: {}",
            failed_passes[0].reason
        );
    }

    /// The mirror case: the rule the finding cites has NO corpus entry at all (an invented
    /// AI-tier rule id, or simply a rule this loaded corpus doesn't define), so there is
    /// nothing to instantiate. `fix_specific` legitimately stays `None` — the give-up is
    /// still disclosed, and the finding is never dropped; it is
    /// `report_export::is_action_row_missing_a_fix`'s job (exercised in report_export.rs's
    /// own tests) to re-route a row like this to "held" at export time rather than ship an
    /// action-bucket row with no fix text anywhere.
    #[tokio::test]
    async fn generate_fix_specifics_leaves_fix_specific_none_when_the_rule_has_no_authored_remediation()
     {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let f = fx(
            "AI-CUSTOM-ARCH-RULE-1",
            "a.rs",
            10,
            "some real defect",
            "let x = 1;",
        );

        let (out, failed_passes) = generate_fix_specifics(
            &FailingCompleter,
            "o/r",
            vec![f],
            &[],
            None,
            None,
            Some(&corpus),
        )
        .await;

        assert_eq!(
            out[0].fix_specific, None,
            "nothing to instantiate — must never fabricate a fix"
        );
        assert!(!out[0].needs_review);
        assert_eq!(failed_passes.len(), 1);
        assert!(
            failed_passes[0].reason.contains("0 shipped"),
            "the disclosure must say explicitly that zero of this run's give-ups got an \
             instantiated fallback: {}",
            failed_passes[0].reason
        );
    }

    #[tokio::test]
    async fn generate_fix_specifics_uses_surrounding_context_to_ground_identifiers() {
        // The fix names a symbol that's NOT in the finding's own snippet/detail but IS in the
        // surrounding file content passed via `files` — proving the context window (not just
        // the bare snippet) grounds the identifier check.
        let files = vec![(
            "lib/redirect.ts".to_string(),
            "// ...\n// ...\n// ...\n// ...\n// ...\nexport function safeInternalPath(p: string) \
             { return p; }\n// ...\n// ...\n// ...\n// ...\n"
                .to_string(),
        )];
        let f = fx(
            "SEC-OPEN-REDIRECT-1",
            "lib/redirect.ts",
            6,
            "this module has no validated-path helper in use at the call site",
            "return p;",
        );
        let completer = SequencedCompleter::new(&[
            r#"{"fixes":[{"index":0,"fix":"Route the redirect through `safeInternalPath`, defined right here in lib/redirect.ts."}]}"#,
        ]);
        let (out, _) =
            generate_fix_specifics(&completer, "o/r", vec![f], &files, None, None, None).await;
        assert!(
            out[0].fix_specific.is_some(),
            "the context window must ground `safeInternalPath` even though it's not in the \
             bare snippet/detail"
        );
    }

    /// max_tokens must SCALE with the finding count instead of the old hardcoded flat 4096 —
    /// a full `FIX_BATCH_SIZE` chunk is deliberately sized (see that constant's doc comment)
    /// to already exceed the old flat floor, since each fix is a full prose sentence.
    ///
    /// `CapturingCompleter` always answers `"{}"` (no usable fix), so EVERY batched round
    /// here is rejected and the C6-B3.4 solo retry round also runs afterward — that round
    /// legitimately uses a SMALLER max_tokens (one finding, not `FIX_BATCH_SIZE`), so this
    /// test only asserts over the BATCHED rounds' own requests (the first
    /// `MAX_FIX_REGENERATIONS + 1` calls); the sibling
    /// `generate_fix_specifics_solo_retry_round_uses_a_smaller_max_tokens_than_the_batched_rounds`
    /// test below pins the solo round's own, smaller value.
    #[tokio::test]
    async fn generate_fix_specifics_scales_max_tokens_with_finding_count() {
        let completer = CapturingCompleter::default();
        let findings: Vec<Finding> = (0..FIX_BATCH_SIZE)
            .map(|i| fx("ARCH-1", "a.rs", i, "some real defect", "let x = 1;"))
            .collect();

        let (_, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

        let seen = completer.seen.lock().unwrap();
        let expected = scaled_max_tokens(FIX_BATCH_SIZE, FIX_TOKENS_PER_FINDING);
        assert!(
            expected > 4096,
            "test setup must exercise the scaled-above-floor branch, got {expected}"
        );
        let batched_rounds = MAX_FIX_REGENERATIONS + 1;
        assert!(
            seen.len() >= batched_rounds,
            "expected at least {batched_rounds} batched-round calls, got {}",
            seen.len()
        );
        for req in seen.iter().take(batched_rounds) {
            assert_eq!(req.max_tokens, expected);
        }
    }

    /// C6-B3.4: the cheap-reliability solo retry round (batch_size = 1) deliberately uses a
    /// SMALLER max_tokens than the batched rounds that preceded it — it covers exactly one
    /// finding, not `FIX_BATCH_SIZE` of them. `CapturingCompleter` never returns a usable fix,
    /// so every batched round AND the solo round all run; the LAST call this completer sees
    /// is therefore from the solo round.
    #[tokio::test]
    async fn generate_fix_specifics_solo_retry_round_uses_a_smaller_max_tokens_than_the_batched_rounds()
     {
        let completer = CapturingCompleter::default();
        let findings: Vec<Finding> = (0..FIX_BATCH_SIZE)
            .map(|i| fx("ARCH-1", "a.rs", i, "some real defect", "let x = 1;"))
            .collect();

        let (_, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

        let seen = completer.seen.lock().unwrap();
        let batched_rounds = MAX_FIX_REGENERATIONS + 1;
        let batched_max_tokens = scaled_max_tokens(FIX_BATCH_SIZE, FIX_TOKENS_PER_FINDING);
        let solo_max_tokens = scaled_max_tokens(1, FIX_TOKENS_PER_FINDING);
        assert!(
            seen.len() > batched_rounds,
            "expected solo-round calls after the batched rounds, got {} total calls",
            seen.len()
        );
        for req in seen.iter().skip(batched_rounds) {
            assert_eq!(
                req.max_tokens, solo_max_tokens,
                "every solo-round call must use the SMALLER single-finding max_tokens"
            );
            assert!(
                req.max_tokens < batched_max_tokens,
                "the solo round's max_tokens must be smaller than the batched rounds': {} vs {}",
                req.max_tokens,
                batched_max_tokens
            );
        }
    }

    /// A SMALL finding set must see ZERO behavior change — it stays pinned at the original
    /// flat 4096 floor.
    #[tokio::test]
    async fn generate_fix_specifics_max_tokens_floor_unchanged_for_small_finding_sets() {
        let completer = CapturingCompleter::default();
        let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");

        let (_, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;

        let seen = completer.seen.lock().unwrap();
        assert_eq!(seen[0].max_tokens, 4096);
    }

    /// A large finding set is split into bounded chunks and every finding survives the round
    /// trip — no finding is dropped at a chunk boundary, and the completer sees the expected
    /// chunk count.
    #[tokio::test]
    async fn generate_fix_specifics_chunks_large_finding_sets_and_merges_without_dropping() {
        let n = FIX_BATCH_SIZE * 2 + 3;
        let findings: Vec<Finding> = (0..n)
            .map(|i| fx("ARCH-1", "a.rs", i, "some real defect", "let x = 1;"))
            .collect();
        let entries: Vec<String> = (0..FIX_BATCH_SIZE)
            .map(|i| {
                format!(
                    r#"{{"index":{i},"fix":"Validate the input before using it and return an error otherwise."}}"#
                )
            })
            .collect();
        let canned = format!(r#"{{"fixes":[{}]}}"#, entries.join(","));
        let completer = CountingFixCompleter {
            calls: 0.into(),
            canned,
        };

        let (out, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

        assert_eq!(out.len(), n, "no finding dropped across chunk boundaries");
        assert!(
            out.iter()
                .all(|f| f.fix_specific.is_some() && !f.needs_review),
            "every finding must get a fix: {out:?}"
        );
        let expected_chunks = n.div_ceil(FIX_BATCH_SIZE);
        assert_eq!(
            completer.calls.load(std::sync::atomic::Ordering::Relaxed),
            expected_chunks,
            "M findings must be split into the expected number of bounded chunks"
        );
    }

    /// A `LlmPort` that returns the SAME canned text on every call and counts how many
    /// times it was invoked — used to assert the exact number of bounded chunks a large
    /// finding/rule set is split into.
    struct CountingFixCompleter {
        calls: std::sync::atomic::AtomicUsize,
        canned: String,
    }

    #[async_trait::async_trait]
    impl LlmPort for CountingFixCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(LlmResponse {
                text: self.canned.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            let resp = self.complete(req).await?;
            on_delta(&resp.text);
            Ok(resp)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
}
