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

Cross-file context: you have the REPO MAP (every file + its public symbols) but only SOME file bodies in this pass. If judging a rule needs the actual BODY of a file that is in the map but NOT included below (e.g. you must read a repository's implementation, or a type defined elsewhere, to decide), do NOT guess and do NOT stay silent — list EVERY file path involved in that deferred judgment (the file under suspicion AND the files it depends on) in `needs_files`. A follow-up pass will include those bodies together so you can decide then.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {
      "path": "relative/file/path",
      "line": 0,
      "severity": "high|medium|low",
      "rule": "EXACT adopted RULE-ID, or a short-kebab-name for an unlisted issue",
      "title": "one-line statement of the specific violation here",
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
            let detail = f["detail"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() && detail.is_empty() {
                continue;
            }
            findings.push(Finding {
                repo: repo.to_string(),
                path: f["path"].as_str().unwrap_or("(repo)").to_string(),
                line: f["line"].as_u64().unwrap_or(0) as usize,
                rule_id,
                severity: severity.to_string(),
                snippet: title,
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
            let mechanical = r["enforcement"].as_str() == Some("mechanical");
            let title = r["title"].as_str().unwrap_or(name).trim().to_string();
            // How many AI findings this rule's name accounts for.
            let finding_count = findings
                .iter()
                .filter(|f| f.rule_id == id)
                .count();
            proposed.push(ProposedRule {
                id,
                title,
                // AI-discovered architectural rules are human-judged, not auto-mechanical.
                kind: if mechanical { "mechanical".to_string() } else { "review".to_string() },
                // Architectural guidance partitions to CONVENTIONS.md (structured).
                enforcement: "structured".to_string(),
                options: Vec::new(),
                default_option: None,
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

For EACH finding, do two things:
- Assign a CALIBRATED severity (high/medium/low) for this app's real-world context. A real
  but low-impact issue is severity "low", not removed.
- If the finding looks weak (likely over-flagged, theoretical, or you cannot tell it is
  real without seeing more code), set confidence "low"; otherwise "high". This is advice
  for the human, not a deletion.

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
        .trim_matches(|c: char| {
            matches!(c, ';' | ',' | '.' | '-' | ':') || c.is_whitespace()
        })
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
                let tag = if low_conf { "needs review" } else { "calibrated" };
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
) -> Vec<Finding> {
    if findings.is_empty() {
        return findings;
    }
    let mut prompt = format!("Repository: {repo}\n\nScrutinize these findings:\n");
    for (i, f) in findings.iter().enumerate() {
        prompt.push_str(&format!(
            "[{i}] (severity {}) {}:{} — {} :: {}\n",
            f.severity, f.path, f.line, f.snippet, f.detail
        ));
    }
    let mut req = LlmRequest::new(prompt)
        .with_system(verify_system_prompt())
        // Aggregated findings across all chunks can be many; one verdict each.
        .with_max_tokens(4096);
    // Calibration runs on its OWN selected model (the UI exposes it). Previously it silently
    // used the backend default, so a "Haiku" scan was really Haiku-scan + default-calibrate;
    // now the model the user picked for calibration is the one that runs.
    if let Some(m) = calibration_model {
        req = req.with_model(m.to_string());
    }
    // Non-streaming, so use the coarse total backstop; on timeout or error the findings
    // pass through unchanged (calibration is best-effort, never load-bearing).
    match tokio::time::timeout(total_backstop(), llm.complete(req)).await {
        Ok(Ok(resp)) => {
            if let Some(m) = meter {
                m.record(&resp);
            }
            apply_verdicts(&resp.text, findings)
        }
        _ => findings,
    }
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
#[allow(clippy::too_many_arguments)]
async fn audit_pass(
    llm: &Llm,
    audit_model: Option<&str>,
    prompt: String,
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
}

impl ScanMode {
    /// `(max concurrent calls, rules per batch)`. Sequential = one call, all rules together;
    /// Parallel = batched + concurrent.
    fn tuning(self) -> (usize, usize) {
        match self {
            ScanMode::Sequential => (1, usize::MAX),
            ScanMode::Parallel => (PARALLEL_CONCURRENCY, RULE_BATCH_SIZE),
        }
    }
    /// Parse the wire value; unknown / empty → Parallel (the efficient default floor).
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("sequential") => ScanMode::Sequential,
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
                let prompt = format!(
                    "Repository: {repo} ({label} {}/{n_c})\n\n{repo_map}{digest}\n\n{task_line}\n\n{rb}",
                    ci + 1,
                );
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
                let r = audit_pass(llm, audit_model, prompt, repo, adopted, feedback, &session, meter).await;
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

/// Merge findings that sit at the SAME code location into one row, keyed on `(path, line)`
/// — NOT on the title (the model writes a different title for each invented rule name, so a
/// title key never collapses them). This is the deterministic reduce that turns the audit's
/// duplication explosion (e.g. one panic reported under five rule ids) into one honest row
/// per location via [`merge_location_group`]. Line 0 (file-level / uncited) findings are
/// NOT location-merged — unrelated file-level issues legitimately share line 0 — so each is
/// passed through untouched (the exact `(path, line, rule_id)` dedup upstream already
/// removed byte-identical line-0 repeats).
fn merge_by_location(findings: Vec<Finding>) -> Vec<Finding> {
    // `disambiguator` is 0 for real lines (so all hits at one line group together) and a
    // unique counter for line 0 (so each line-0 finding stays its own group).
    let mut order: Vec<(String, usize, usize)> = Vec::new();
    let mut groups: std::collections::HashMap<(String, usize, usize), Vec<Finding>> =
        std::collections::HashMap::new();
    let mut solo: usize = 0;
    for f in findings {
        let key = if f.line == 0 {
            solo += 1;
            (f.path.clone(), 0, solo)
        } else {
            (f.path.clone(), f.line, 0)
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
pub async fn audit_repo(
    llm: &Llm,
    repo: &str,
    files: &[(String, String)],
    selected: &[(String, String)],
    model: Option<&str>,
    calibration_model: Option<&str>,
    mode: ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>)> {
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // Cross-file context for every chunk (which dirs are which layer, where types live).
    let repo_map = build_repo_map(files);
    // Key findings to the adopted rule ids (so a violation shows under e.g.
    // ARCH-STRICT-LAYERING-1, not an invented AI- name).
    let adopted: std::collections::HashSet<String> =
        selected.iter().map(|(id, _)| id.to_ascii_uppercase()).collect();
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
    // rule-batches × file-chunks run concurrently (the default efficient floor).
    let (concurrency, batch_size) = mode.tuning();
    let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
    let batches: Vec<&[(String, String)]> = if selected.is_empty() {
        vec![selected] // one empty batch -> a single free-form pass per chunk
    } else {
        selected.chunks(batch_size.max(1)).collect()
    };
    // Tell the job how many passes this repo adds (the denominator climbs per repo).
    if let Some((jstore, jid)) = job {
        jstore.add_total(jid, chunks.len() * batches.len());
    }

    let (mut all_findings, mut all_proposed, requested, ok_passes, last_err) = run_passes(
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
    .await;

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
    // Resolution round: the same parallel engine, over just the files earlier passes asked
    // for (needs_files). Its own needs_files are ignored — single round, bounded.
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
            concurrency,
            "resolution",
            &format!("audit-{repo}-res"),
            meter,
        )
        .await;
        all_findings.extend(rf);
        all_proposed.extend(rp);
    }

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
        all_findings = merge_by_location(all_findings);
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
        let out = verify_findings(llm, repo, all_findings, calib_model.as_deref(), meter).await;
        if let Some((store, key)) = feedback {
            store.set_status(key, &session, "done");
        }
        out
    };
    Ok((verified, all_proposed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_needs_files_reads_array_and_tolerates_absence() {
        let with = r#"{"findings":[],"proposed_rules":[],"needs_files":["a/repo.rs"," ","b/svc.rs"]}"#;
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
        assert_eq!(strip_dedup_pointers("same root cause, row 3"), "same root cause");
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
        let findings = vec![
            site_finding("AI-CONTROLLER-DIRECT-DB", "h.rs", 12, "medium", "Controller accesses DB directly"),
            site_finding("ARCH-STRICT-LAYERING-1", "h.rs", 12, "high", "Layering violation in handler"),
            site_finding("AI-HANDLER-BYPASSES-REPO", "h.rs", 12, "low", "Handler bypasses repository"),
        ];
        let merged = merge_by_location(findings);
        assert_eq!(merged.len(), 1, "three labels at one location collapse to one row");
        // Adopted id wins as primary; highest severity kept; others demoted to also_matches.
        assert_eq!(merged[0].rule_id, "ARCH-STRICT-LAYERING-1");
        assert_eq!(merged[0].severity, "high");
        assert!(merged[0].also_matches.contains(&"AI-CONTROLLER-DIRECT-DB".to_string()));
        assert!(merged[0].also_matches.contains(&"AI-HANDLER-BYPASSES-REPO".to_string()));
        assert!(!merged[0].also_matches.contains(&"ARCH-STRICT-LAYERING-1".to_string()));
    }

    #[test]
    fn merge_folds_overlapping_corpus_rules_at_one_location() {
        // "Handler opens its own pool" legitimately trips layering + DI + entities-chain.
        // That's one finding that names all three, not three rows.
        let findings = vec![
            site_finding("ARCH-STRICT-LAYERING-1", "h.rs", 41, "high", "own pool"),
            site_finding("ARCH-SERVICE-DI-1", "h.rs", 41, "medium", "own pool"),
            site_finding("RUST-ENTITIES-13", "h.rs", 41, "low", "own pool"),
        ];
        let merged = merge_by_location(findings);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].also_matches.len(), 2, "two non-primary rules demoted");
    }

    #[test]
    fn merge_does_not_collapse_distinct_line_zero_findings() {
        // Line 0 (file-level / uncited) must NOT location-merge — unrelated file-level
        // issues legitimately share line 0.
        let findings = vec![
            site_finding("AI-NO-MAPPERS-CRATE", "lib.rs", 0, "low", "no mappers crate"),
            site_finding("AI-NO-TESTS", "lib.rs", 0, "low", "no tests"),
        ];
        let merged = merge_by_location(findings);
        assert_eq!(merged.len(), 2, "distinct line-0 findings stay separate");
    }

    #[test]
    fn canonicalize_maps_invented_names_only_when_adopted() {
        let adopted: std::collections::HashSet<String> =
            ["ARCH-STRUCTURED-ERRORS-1".to_string(), "ARCH-STRICT-LAYERING-1".to_string()]
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
        assert!(timing.detail.contains("[needs review"), "low-confidence flagged");
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
        let (f, r) = parse_ai_findings("me/api", r#"{"findings": [], "proposed_rules": []}"#, &none);
        assert!(f.is_empty());
        assert!(r.is_empty());
    }
}
