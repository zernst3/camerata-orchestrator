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

use crate::llm::{Llm, LlmRequest};
use crate::onboard::{Finding, ProposedRule};

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
    /// when the API backend is in use with `cache_prefix_len` set on the request.
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
const MAX_DIGEST_CHARS: usize = 600_000;

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
pub fn build_digest(files: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut truncated = false;
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
            }
            truncated = true;
            break;
        }
        out.push_str(&header);
        out.push_str(&numbered);
        out.push('\n');
    }
    if truncated {
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

CRITICAL — do NOT invent rule names that duplicate adopted rules. Before you set `rule`, check whether the violation is already covered by one of the adopted `[RULE-ID]`s above. If it is, you MUST use that exact adopted RULE-ID — even if you would have phrased the issue differently. A controller reaching into the database directly is `ARCH-STRICT-LAYERING-1`, not "controller-direct-db" or "handler-bypasses-repo"; a handler panicking on a DB error is `ARCH-STRUCTURED-ERRORS-1`, not "panic-on-db-error". Inventing a new name for a violation an adopted rule already covers is the single worst failure mode of this audit — it produces triplicate findings that all mean the same thing.

Flagging novel issues (issues no adopted rule covers) is GATED by this pass's instruction line. ONLY when that line says to "ALSO flag any other genuine issues" may you report something outside the adopted rules — and then set `rule` to a short kebab name (e.g. "auth-on-write-paths"), reserved strictly for genuinely-novel issues (if any adopted rule fits, use the adopted id instead). When the instruction line says to check ONLY the listed rules, report nothing outside them.

DO NOT report: hardcoded secrets, secrets embedded in URLs, raw SQL string concatenation, or path-escape writes — a separate deterministic scanner already covers those precisely. Do not report pure style/formatting nits.

Each line in the digest is prefixed with its line number as `NNNN| `. Cite that exact number in `line` — do not estimate.

WATCH FOR INDIRECTION before flagging something MISSING. When a loop renders items via a HELPER CALL (e.g. `for row in rows { data_tr(row, …) }`), an attribute the rule wants — a `key`, a CSS class, an error handler — is very often set INSIDE that helper, not at the call site. Do NOT flag "missing key"/"missing X" on the call site without checking the called function's body. If that body is in the digest, read it; if it's elsewhere, request it via `needs_files` and defer — never assume it's missing just because it's not inline at the loop. (This is a real false-positive class: row renderers extracted into helpers that DO set the key.)

Cross-file context: you have the REPO MAP (every file + its public symbols) but only SOME file bodies in this pass. If judging a rule needs the actual BODY of a file that is in the map but NOT included below (e.g. you must read a repository's implementation, or a type defined elsewhere, to decide), do NOT guess and do NOT stay silent — list EVERY file path involved in that deferred judgment (the file under suspicion AND the files it depends on) in `needs_files`. A follow-up pass will include those bodies together so you can decide then.

For `code`, copy the offending source text VERBATIM from the digest — the exact characters of the line you're flagging, not a paraphrase. A deterministic post-step locates the true line by finding this text in the file, so an exact copy gives an exact line; a paraphrase makes the line approximate. Keep it to the single most relevant line (or short span). Still set `line` to your best estimate as a fallback.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {
      "path": "relative/file/path",
      "line": 0,
      "severity": "high|medium|low",
      "rule": "EXACT adopted RULE-ID, or a short-kebab-name for an unlisted issue",
      "title": "one-line statement of the specific violation here",
      "code": "the EXACT offending source line, copied verbatim from the digest",
      "detail": "why it's a problem and what the fix direction is"
    }
  ],
  "proposed_rules": [
    {
      "name": "short-kebab-name (only for issues NOT covered by an adopted rule)",
      "title": "the rule to enforce going forward",
      "rationale": "why this rule, grounded in the findings",
      "severity": "high|medium|low",
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
    } else if has("PANIC") {
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
- Assign a CALIBRATED severity (high/medium/low) for this app's real-world context, using this
  rubric:
  * A concrete, demonstrable SECURITY or CORRECTNESS break (injection, missing auth on a write
    path, data loss/corruption, a real exploit) can be "high".
  * A DEBATABLE ARCHITECTURAL PREFERENCE — a "valid pattern but not the one this rule prefers"
    call, a layering/structure/abstraction opinion, an over-engineering/YAGNI note on a small
    codebase, a stylistic or convention preference — is NOT "high". Cap it at "medium", usually
    "low". These are preferences a reasonable team could disagree on, not violations.
  * A real but low-impact issue is "low", not removed.
- Set confidence: "low" when the finding is a debatable preference (per above), is theoretical,
  is likely over-flagged, or you cannot tell it is real without seeing more code; "high" only
  for clear, concrete violations. Confidence "low" flags the finding for the architect's review —
  it is ADVICE, not a deletion. When in doubt between a violation and a preference, treat it as a
  preference: low confidence, capped severity.

Do NOT deduplicate, and do NOT cross-reference other findings — no "same as [N]", "duplicate
of [N]", "as index N", "index N", "row N", or ANY pointer to another finding by index/row.
Deduplication already happened upstream; your `reason` is one line about THIS finding's
severity/confidence only, with no reference to any other finding.

Return ONLY JSON, no prose:
{"verdicts":[{"index":0,"severity":"high|medium|low","confidence":"high|low","reason":"one line"}]}
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

/// Apply the calibration verdicts: recalibrate severity and annotate confidence/reason.
/// NEVER drops a finding — recall-first discovery hands every finding to the architect.
/// Robust: unparseable verdicts keep all findings as-is.
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
                f.severity = match sev {
                    "high" => "high",
                    "low" => "low",
                    _ => "medium",
                }
                .to_string();
            }
            let low_conf = verdict["confidence"].as_str() == Some("low");
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

/// Run the skeptic pass over a repo's AI findings (a fresh, reasoning-based perspective —
/// deliberately NOT re-sent the whole digest, so it judges exploitability/context, not
/// code minutiae). Graceful: on any model failure the findings pass through unchanged.
pub async fn verify_findings(
    llm: &Llm,
    repo: &str,
    findings: Vec<Finding>,
    calibration_model: Option<&str>,
    meter: Option<&UsageMeter>,
    thorough: bool,
    files_count: usize,
) -> Vec<Finding> {
    if findings.is_empty() {
        return findings;
    }
    let mut prompt = format!("Repository: {repo}\n");
    if thorough {
        // Proportionality signal (#51): a small/young codebase should not be held to the
        // architecture of a large one — over-engineering / YAGNI notes auto-hedge.
        prompt.push_str(&format!(
            "This repository has {files_count} code files. Judge each finding PROPORTIONALLY to the \
             codebase's size and maturity: an 'over-engineering'/'missing abstraction'/YAGNI note on \
             a small codebase is a debatable preference (low confidence, capped severity), not a \
             violation.\n"
        ));
    }
    prompt.push_str("\nScrutinize these findings:\n");
    for (i, f) in findings.iter().enumerate() {
        prompt.push_str(&format!(
            "[{i}] (severity {}) {}:{} — {} :: {}\n",
            f.severity, f.path, f.line, f.snippet, f.detail
        ));
    }
    // Calibration runs on its OWN selected model (the UI exposes it). Build a fresh request per
    // pass (LlmRequest is consumed by complete).
    let system = verify_system_prompt();
    let build_req = || {
        let mut req = LlmRequest::new(prompt.clone())
            .with_system(system.clone())
            // Aggregated findings across all chunks can be many; one verdict each.
            .with_max_tokens(4096);
        if let Some(m) = calibration_model {
            req = req.with_model(m.to_string());
        }
        req
    };

    // THOROUGH mode (#51): run the calibration verdict MULTIPLE times and take the conservative
    // consensus, so a single over-confident pass can't push a debatable finding to HIGH. Costs
    // ~3x the calibration tokens (opt-in). Default mode is a single pass (unchanged behavior).
    let passes = if thorough { 3 } else { 1 };
    let mut votes: Vec<String> = Vec::new();
    for _ in 0..passes {
        // Non-streaming, so use the coarse total backstop; a failed pass is simply skipped
        // (calibration is best-effort, never load-bearing).
        if let Ok(Ok(resp)) =
            tokio::time::timeout(total_backstop(), llm.complete(build_req())).await
        {
            if let Some(m) = meter {
                m.record(&resp);
            }
            votes.push(resp.text);
        }
    }
    match votes.len() {
        0 => findings, // every pass failed — pass findings through unchanged
        1 => apply_verdicts(&votes[0], findings),
        _ => apply_verdicts(&consensus_verdicts(&votes, findings.len()), findings),
    }
}

/// Merge several calibration passes into one CONSERVATIVE consensus verdict set (#51 thorough
/// mode). For each finding index: severity = the majority vote (ties break to the LOWER severity);
/// confidence = "high" only when the passes AGREE (all "high" and a single agreed severity) —
/// any disagreement means uncertainty, which is exactly what the architect should review, so it
/// becomes "low" (needs review). Returns a `{"verdicts":[…]}` JSON string for `apply_verdicts`.
fn consensus_verdicts(votes: &[String], n: usize) -> String {
    use serde_json::Value;
    // Per index: collected (severity, confidence, reason) across passes.
    let mut per: Vec<Vec<(String, String, String)>> = vec![Vec::new(); n];
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
            per[idx].push((sev, conf, reason));
        }
    }
    let rank = |s: &str| match s {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    };
    let mut verdicts = Vec::new();
    for (idx, votes_for) in per.iter().enumerate() {
        if votes_for.is_empty() {
            continue;
        }
        // Majority severity; tie breaks to the lower rank (humble).
        let mut counts = [0u32; 3]; // [low, medium, high]
        for (s, _, _) in votes_for {
            counts[rank(s)] += 1;
        }
        let max = counts.iter().copied().max().unwrap_or(0);
        let sev = if counts[2] == max {
            "high"
        } else if counts[1] == max {
            "medium"
        } else {
            "low"
        };
        // Disagreement on severity, or any low-confidence vote → low confidence (needs review).
        let distinct_sevs = counts.iter().filter(|&&c| c > 0).count();
        let any_low_conf = votes_for.iter().any(|(_, c, _)| c == "low");
        let agreed_high = sev == "high" && distinct_sevs == 1 && !any_low_conf;
        let confidence = if agreed_high {
            "high"
        } else if distinct_sevs > 1 || any_low_conf {
            "low"
        } else {
            "high"
        };
        // First non-empty reason, preferring a low-confidence pass's reason.
        let reason = votes_for
            .iter()
            .find(|(_, c, r)| c == "low" && !r.is_empty())
            .or_else(|| votes_for.iter().find(|(_, _, r)| !r.is_empty()))
            .map(|(_, _, r)| r.clone())
            .unwrap_or_default();
        verdicts.push(serde_json::json!({
            "index": idx, "severity": sev, "confidence": confidence, "reason": reason
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

/// One audit pass: build the request, run it (streaming into the transcript when feedback
/// is present), and parse out findings + proposed rules + any `needs_files` request. Shared
/// by the primary chunk loop and the resolution round so neither duplicates the call logic.
///
/// `cache_prefix_len` — when `Some(n)`, the first `n` bytes of the prompt (the static
/// codebase context: repo map + chunk digest) are marked as the cacheable prefix via
/// [`LlmRequest::with_cache_prefix_len`]. On the API backend this tells the provider to
/// cache that prefix and re-read it cheaply for every subsequent rule-batch over the same
/// chunk. The CLI backend ignores this (no-op). Pass `None` to disable caching (default).
#[allow(clippy::too_many_arguments)]
async fn audit_pass(
    llm: &Llm,
    audit_model: Option<&str>,
    prompt: String,
    cache_prefix_len: Option<usize>,
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
    if let Some(prefix_len) = cache_prefix_len {
        req = req.with_cache_prefix_len(prefix_len);
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
    /// `(max concurrent calls, rules per batch)` for the REAL-TIME modes. Not used by the
    /// Batch path (it submits everything at once and lets the API schedule). Sequential =
    /// one call, all rules together; Parallel = batched + concurrent.
    fn tuning(self) -> (usize, usize) {
        match self {
            ScanMode::Sequential => (1, usize::MAX),
            ScanMode::Parallel | ScanMode::Batch => (PARALLEL_CONCURRENCY, RULE_BATCH_SIZE),
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
#[allow(clippy::too_many_arguments)]
async fn run_passes(
    llm: &Llm,
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
                let advisory = bi == 0;
                let task_line = if advisory {
                    format!("── Check the code above against the ADOPTED rules below (batch {}/{n_b}); ALSO flag any other genuine issues NOT covered by an adopted rule. Use the REPO MAP for cross-file context. ──", bi + 1)
                } else {
                    format!("── Check the code above against ONLY the ADOPTED rules below (batch {}/{n_b}). Do NOT report issues outside these rules — a separate pass already covers novel findings. Use the REPO MAP for cross-file context. ──", bi + 1)
                };
                // PROMPT ORDER IS CACHE-AWARE: the STABLE content (the per-chunk repo map +
                // digest) leads, so it forms a reusable cached prefix across this chunk's
                // rule-batches (and the system prompt before it). The VARYING content (the
                // batch number + the rules) trails, so it never breaks the prefix. The
                // opening line is deliberately free of the batch number for the same reason.
                // Bonus: rules landing last = most recent context = strongest rule-following.
                //
                // CACHING: the static prefix ends at the double-newline after `digest` and
                // before `task_line`. We compute its byte length here so `audit_pass` can
                // mark it for the API backend's cache_control breakpoint. The CLI backend
                // ignores this field entirely.
                let static_prefix = format!(
                    "Repository: {repo} ({label} {}/{n_c})\n\n{repo_map}{digest}\n\n",
                    ci + 1,
                );
                let cache_prefix_len = static_prefix.len();
                let prompt = format!("{static_prefix}{task_line}\n\n{rb}");
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
                let r = audit_pass(llm, audit_model, prompt, Some(cache_prefix_len), repo, adopted, feedback, &session, meter).await;
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
            "batch mode requires the `api` backend with ANTHROPIC_API_KEY set; \
             set CAMERATA_LLM_BACKEND=api and ANTHROPIC_API_KEY, or use parallel mode"
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
    // Retain the (ci, bi, prompt, cache_prefix_len) tuples so we can parse results.
    let mut work_meta: Vec<(usize, usize, String, usize)> = Vec::with_capacity(n_c * n_b);

    for ci in 0..n_c {
        let digest = &digests[ci];
        for bi in 0..n_b {
            let batch = batches[bi];
            let rb = build_rules_block(batch);
            let advisory = bi == 0;
            let task_line = if advisory {
                format!("── Check the code above against the ADOPTED rules below (batch {}/{n_b}); ALSO flag any other genuine issues NOT covered by an adopted rule. Use the REPO MAP for cross-file context. ──", bi + 1)
            } else {
                format!("── Check the code above against ONLY the ADOPTED rules below (batch {}/{n_b}). Do NOT report issues outside these rules — a separate pass already covers novel findings. Use the REPO MAP for cross-file context. ──", bi + 1)
            };
            let static_prefix = format!(
                "Repository: {repo} ({label} {}/{n_c})\n\n{repo_map}{digest}\n\n",
                ci + 1,
            );
            let cache_prefix_len = static_prefix.len();
            let prompt = format!("{static_prefix}{task_line}\n\n{rb}");

            let custom_id = format!("c{ci}-b{bi}");
            let req = {
                let mut r = LlmRequest::new(prompt.clone())
                    .with_system(audit_system_prompt())
                    .with_max_tokens(8192)
                    .with_model(model.clone())
                    .with_cache_prefix_len(cache_prefix_len);
                // audit_model overrides the default; already folded into `model` above.
                let _ = &mut r; // avoid unused_mut lint
                r
            };
            items.push(build_batch_item(&custom_id, &req, &model));
            work_meta.push((ci, bi, prompt, cache_prefix_len));
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
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
            let status = llm.poll_batch_status(&batch_id).await?;
            eprintln!(
                "[camerata-server] batch {batch_id}: status={} (processing={}, succeeded={}, errored={})",
                status.processing_status,
                status.request_counts.processing,
                status.request_counts.succeeded,
                status.request_counts.errored,
            );
            if status.processing_status == "ended" {
                break;
            }
        }

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

    for (ci, bi, _prompt, _cache_prefix_len) in &work_meta {
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

/// Collapse one `(path, line)` group of findings into a SINGLE finding. The model routinely
/// reports one smell under several rule names — an invented `AI-` name PLUS the adopted
/// corpus rule it maps to PLUS sibling invented names — each with a different title, so a
/// `.expect()` panic at handlers.rs:41 arrives as five rows. This keeps ONE primary
/// (preferring an adopted corpus rule id over an invented `AI-` one, then the most severe,
/// then earliest), demotes every OTHER distinct rule id to `also_matches`, and keeps the
/// max severity — so the row honestly reads "violates layering + DI + entities-chain" rather
/// than emitting five near-duplicates.
fn merge_location_group(group: Vec<Finding>) -> Finding {
    // Index of the primary: adopted (non-AI-) beats invented; then higher severity; then
    // earliest appearance (so the order is deterministic, not HashMap-dependent).
    let primary_idx = group
        .iter()
        .enumerate()
        .max_by_key(|(i, f)| {
            let adopted = u8::from(!f.rule_id.starts_with("AI-"));
            (adopted, severity_rank(&f.severity), group.len() - i)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let max_sev = group
        .iter()
        .max_by_key(|f| severity_rank(&f.severity))
        .map(|f| f.severity.clone())
        .unwrap_or_else(|| "low".to_string());

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
    primary.severity = max_sev;
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
fn merge_by_location(findings: Vec<Finding>, files: &[(String, String)]) -> Vec<Finding> {
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
    for f in findings {
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

/// Run the AI architectural audit for one repo. Returns the findings + proposed rules.
///
/// The repo is audited in CONTEXT-SIZED CHUNKS (see `chunk_files`): a real repo is far too
/// large for one model context (a 2.8M-char repo is ~700k tokens vs a 200k window), and the
/// old single-digest path silently fed the model only the first ~10% of files — so a
/// blatant violation in a later file produced zero findings purely because the file was
/// never in the input. Every chunk is audited against the full ruleset and the findings are
/// aggregated. A model/transport failure on a chunk is noted and the audit continues, so a
/// single bad pass never discards the others.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub async fn audit_repo(
    llm: &Llm,
    repo: &str,
    files: &[(String, String)],
    selected: &[(String, String)],
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
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>)> {
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
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
    let calib_model = calibration_model
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CAMERATA_CALIBRATION_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| audit_model.clone());

    // Mode is the speed/scale knob: Sequential = 1 call per chunk with all rules; Parallel =
    // rule-batches × file-chunks run concurrently; Batch = one Anthropic Message Batch at
    // 50% discount, reassembled by custom_id.
    let (concurrency, batch_size) = mode.tuning();
    let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
    let batches: Vec<&[(String, String)]> = if selected.is_empty() {
        vec![selected] // one empty batch -> a single free-form pass per chunk
    } else {
        selected.chunks(batch_size.max(1)).collect()
    };

    // Dispatch to the appropriate execution engine.
    let (mut all_findings, mut all_proposed, requested, ok_passes, last_err) = if mode
        == ScanMode::Batch
    {
        // Batch path: submit all (chunk × rule-batch) pairs as one Message Batch, poll to
        // completion, reassemble by custom_id. The job's add_total is called inside
        // run_passes_batch (it knows the full item count before any network I/O).
        run_passes_batch(
            llm,
            repo,
            &repo_map,
            &adopted,
            audit_model.as_deref(),
            job,
            &chunks,
            &batches,
            "pass",
            meter,
        )
        .await?
    } else {
        // Real-time path (parallel or sequential): tell the job the total pass count so the
        // progress bar can be pre-seeded, then run the streaming passes.
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, chunks.len() * batches.len());
        }
        run_passes(
            llm,
            repo,
            &repo_map,
            &adopted,
            audit_model.as_deref(),
            feedback,
            job,
            &chunks,
            &batches,
            concurrency,
            "pass",
            &format!("audit-{repo}"),
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
    // Batch mode: the resolution round uses the PARALLEL engine (it is typically just a
    // handful of files, not worth a separate batch submission with its polling overhead).
    let resolution: Vec<(String, String)> = files
        .iter()
        .filter(|(p, _)| requested.contains(p))
        .cloned()
        .collect();
    if !resolution.is_empty() {
        let res_chunks = chunk_files(&resolution, CHUNK_DIGEST_CHARS);
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, res_chunks.len() * batches.len());
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
            &batches,
            res_concurrency,
            "resolution",
            &format!("audit-{repo}-res"),
            meter,
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
        let mut seen = std::collections::HashSet::new();
        all_findings.retain(|f| seen.insert((f.path.clone(), f.line, f.rule_id.clone())));
        all_findings = merge_by_location(all_findings, files);
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
    // showed every pass "done" while the spinner kept turning for another minute. Register
    // it as its own transcript agent so the cockpit shows "calibrating N findings" instead
    // of a mystery hang. (Dedup/merge also shrinks N, so this round is now faster too.)
    let verified = if all_findings.is_empty() {
        all_findings
    } else {
        let session = format!("audit-{repo}-calibrate");
        if let Some((store, key)) = feedback {
            store.register(
                key,
                crate::transcript::AgentTranscript {
                    session_id: session.clone(),
                    role: format!(
                        "calibrating {} findings on {} — {repo}",
                        all_findings.len(),
                        calib_model.as_deref().unwrap_or("default")
                    ),
                    prompt: String::new(),
                    output: String::new(),
                    status: "running".to_string(),
                },
            );
        }
        let out = verify_findings(
            llm,
            repo,
            all_findings,
            calib_model.as_deref(),
            meter,
            thorough,
            files.len(),
        )
        .await;
        if let Some((store, key)) = feedback {
            store.set_status(key, &session, "done");
        }
        out
    };
    Ok((verified, all_proposed))
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

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {"path":"…","line":0,"severity":"high|medium|low","rule":"short-kebab-security-name","title":"…","code":"the exact offending line","detail":"why it's exploitable and the fix direction"}
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
    llm: &Llm,
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
    llm: &Llm,
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
    llm: &Llm,
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
#[allow(clippy::too_many_arguments)]
pub async fn run_deep_tier(
    llm: &Llm,
    repo: &str,
    files: &[(String, String)],
    audit_model: Option<&str>,
    mode: ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
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

    // Run the three lenses concurrently — they are independent reads of the same repo.
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
    let (soc2, security, threat) = tokio::join!(soc2, security, threat);

    DeepReport {
        // Stable order: gap analysis, security, threat model.
        lenses: vec![soc2, security, threat],
        advisory: true,
        disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
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
}
