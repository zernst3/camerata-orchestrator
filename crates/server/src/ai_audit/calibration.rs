//! Stage 1 calibration, the per-site first reviewer (review tree design 4.1, WO4).
//!
//! Every site gets its own structured call. The model sees the site (the rule it was raised
//! under with the corpus text, the quoted code and the explanation) and about 30 lines of code
//! each side of it, and answers `{severity, real: yes|no|unclear, effort, category, reason}`.
//! It never removes a site. `real` is the only answer that bears on doubt: the row's
//! `confidence` is derived from it and `needs_review` is set from `unclear` or from test code.
//! Neither escalates a site by itself; the group reviewer (stage 2) decides what reaches the
//! architect's queue.
//!
//! The model comes only from the `--calibration-model` flag or the project setting, resolved by
//! [`resolve_calibration_model`]; this stage reads no environment variable.
//!
//! The stage stays on the real-time path in this release, one call per site at bounded
//! concurrency, in groups of [`CALIBRATION_BATCH_SIZE`] for the transcript and for the failed-part
//! rows. [`Stage1Items`] is the same work in the shape a batch runner takes: one request per
//! site, an absorb for each reply, one retry for a reply that was cut off.

use super::stage_batch::{
    mode_and_api, run_stage, ItemReject as RunnerReject, StageCtx, StageItems, StageSettings,
};
use super::*;
use crate::llm::LlmResponse;

/// Lines of code shown on each side of a site.
pub const CODE_WINDOW_RADIUS: usize = 30;

/// The radii a site's window shrinks through when its request would not fit the model's window.
/// The first is the full window; the last shows the site's own line only.
const WINDOW_SHRINK_RADII: [usize; 5] = [CODE_WINDOW_RADIUS, 15, 7, 3, 0];

/// The lines of one file shown around a site, numbered with their real line numbers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodeWindow {
    /// The first line shown (1-based). 0 when nothing is shown.
    pub first_line: usize,
    /// The last line shown (1-based). 0 when nothing is shown.
    pub last_line: usize,
    /// The lines, one per line of the file, each as `{number:>5}| {text}`.
    pub text: String,
}

/// The window of `content` around `line` (1-based): `radius` lines each side, clamped at the
/// first and last line of the file. A line past the end of the file is clamped to the last line.
/// An empty file, or line 0, shows nothing.
pub fn code_window(content: &str, line: usize, radius: usize) -> CodeWindow {
    if line == 0 {
        return CodeWindow::default();
    }
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return CodeWindow::default();
    }
    let site = line.min(lines.len());
    let first = site.saturating_sub(radius).max(1);
    let last = (site + radius).min(lines.len());
    let mut text = String::new();
    for (i, text_line) in lines.iter().enumerate().take(last).skip(first - 1) {
        text.push_str(&format!("{:>5}| {text_line}\n", i + 1));
    }
    CodeWindow {
        first_line: first,
        last_line: last,
        text,
    }
}

/// What stage 1 needs beyond the findings.
#[derive(Clone)]
pub struct Stage1Inputs<'a> {
    /// The model-facing (redacted) files, for the code window of each site. A site whose file is
    /// not among them is rated from its quoted code alone.
    pub files: &'a [(String, String)],
    /// `(rule id, directive)` for the corpus rule text of each site.
    pub rules: &'a [(String, String)],
    /// One repo-shape sentence, e.g. "This is a Next.js repo with 42 code files." The model reads
    /// a codebase's size and maturity from it instead of a hardcoded size threshold. Empty is
    /// fine.
    pub repo_shape: &'a str,
    /// How many sites are asked at once.
    pub concurrency: usize,
    /// Ask the sites as Message Batches (half price, answers in minutes to hours) when the model
    /// seam carries a batch API; without one, or with this off, they are asked in real time.
    pub batch: bool,
    /// The job the stage reports to: its activity is touched and its progress set as sites end,
    /// and a poll tick of a batch does the same.
    pub job: Option<(&'a crate::jobs::JobStore, &'a str)>,
}

impl<'a> Stage1Inputs<'a> {
    /// Inputs with no files, no rule text and one site at a time.
    pub fn new(repo_shape: &'a str) -> Self {
        Self {
            files: &[],
            rules: &[],
            repo_shape,
            concurrency: 1,
            batch: false,
            job: None,
        }
    }

    pub fn with_files(mut self, files: &'a [(String, String)]) -> Self {
        self.files = files;
        self
    }

    pub fn with_rules(mut self, rules: &'a [(String, String)]) -> Self {
        self.rules = rules;
        self
    }

    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Ask the sites as batches when the seam has a batch API (see [`Self::batch`]).
    pub fn with_batch(mut self, batch: bool) -> Self {
        self.batch = batch;
        self
    }

    /// Report to `job`.
    pub fn with_job(mut self, job: Option<(&'a crate::jobs::JobStore, &'a str)>) -> Self {
        self.job = job;
        self
    }
}

#[cfg(test)]
thread_local! {
    /// The size of every range of requests stage 1 built, in order (a test thread's own record).
    pub(super) static BUILT_RANGES: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The notice the removed `--thorough` flag prints for one release. `None` when the flag is off.
pub fn thorough_notice(thorough: bool) -> Option<&'static str> {
    thorough.then_some(
        "--thorough is removed: calibration runs one pass per site. The group reviewer is the \
         second opinion now. The flag is ignored and will be dropped in the next release.",
    )
}

// ── What stage 1 leaves on a row ───────────────────────────────────────────────────────────

/// Stage 1's answer to "is this finding real, from the code shown?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Real {
    Yes,
    No,
    /// The code shown does not settle it.
    Unclear,
}

impl Real {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "yes" => Some(Self::Yes),
            "no" => Some(Self::No),
            "unclear" => Some(Self::Unclear),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unclear => "unclear",
        }
    }

    /// The row's `confidence`, derived from `real` and nothing else.
    fn confidence(self) -> &'static str {
        match self {
            Self::Yes => "high",
            Self::No => "disputed",
            Self::Unclear => "needs-review",
        }
    }
}

/// The `captures` key holding the stage 1 verdict the group reviewer reads: `{"real","reason"}`.
/// A bookkeeping key, so it names no object and is held aside across the merge stages.
pub const STAGE1_CAPTURE: &str = "stage1_verdict";

/// The `captures` key holding the outlier reasons stage 1 recorded, comma separated
/// (`severity_raise_held`). The tree builder reads it; a bookkeeping key like the others.
pub const OUTLIER_REASONS_CAPTURE: &str = "outlier_reasons";

/// The stage 1 verdict kept on a row for the group reviewer. The severity and effort are the
/// row's own fields; what has no field of its own is kept here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1Record {
    pub real: Real,
    /// Stage 1's one-line reason, redacted and with cross-finding pointers removed.
    pub reason: String,
}

/// The stage 1 verdict recorded on `f`, when there is one.
pub fn stage1_of(f: &Finding) -> Option<Stage1Record> {
    let v: serde_json::Value = serde_json::from_str(f.captures.get(STAGE1_CAPTURE)?).ok()?;
    Some(Stage1Record {
        real: Real::parse(v["real"].as_str()?)?,
        reason: v["reason"].as_str().unwrap_or("").to_string(),
    })
}

/// The outlier reasons stage 1 recorded on `f` (today only `severity_raise_held`).
pub fn outlier_reasons_of(f: &Finding) -> Vec<crate::review_tree::OutlierReason> {
    f.captures
        .get(OUTLIER_REASONS_CAPTURE)
        .map(|list| {
            list.split(',')
                .filter_map(|name| {
                    serde_json::from_value(serde_json::Value::String(name.trim().to_string())).ok()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn set_outlier_reasons(f: &mut Finding, reasons: &[crate::review_tree::OutlierReason]) {
    let names: Vec<String> = reasons
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if names.is_empty() {
        f.captures.remove(OUTLIER_REASONS_CAPTURE);
    } else {
        f.captures
            .insert(OUTLIER_REASONS_CAPTURE.to_string(), names.join(","));
    }
}

/// One site's verdict as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stage1Verdict {
    pub severity: Option<String>,
    pub real: Real,
    pub effort: Option<String>,
    pub category: Option<String>,
    pub reason: String,
}

impl Stage1Verdict {
    /// A verdict object, or `None` when it does not answer `real`: a reply that does not say
    /// whether the finding is real is not a verdict.
    pub(super) fn from_value(v: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| v[key].as_str().map(str::to_string);
        Some(Self {
            severity: text("severity"),
            real: Real::parse(v["real"].as_str()?)?,
            effort: text("effort"),
            category: text("category"),
            reason: text("reason").unwrap_or_default(),
        })
    }
}

/// The system prompt for the stage 1 first reviewer. It frames the model as the first reader of
/// each finding, never a filter: every site is kept, and the answers (`severity`, `real`,
/// `effort`, `category`, `reason`) only tell the architect where to look. It names no model, so
/// it works for any capable one, and it carries no confidence instruction: doubt is the one word
/// `real: unclear`, meaning "the code shown does not settle it".
pub fn verify_system_prompt() -> String {
    r#"You are the first reviewer of an automated audit. A senior architect will see every finding. You never remove one. Your job is to rate this finding so the architect spends their time where it matters: give its real-world severity by the rules of thumb below, say whether it is real from the code shown and estimate the fix effort.

You are shown ONE finding: the rule it was raised under, the code and explanation it quotes, and the lines of code around it. Judge it on its own merits. Whoever scanned the code may have been assertive on debatable points; that does NOT carry over, and your job is to put the nuance back. Rate only this finding. Refer to no other finding by index, row or any other pointer.

Answer four things, and give a one-line reason:
- Assign a CALIBRATED SEVERITY (critical/high/medium/low) for this app's real-world context,
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
  Rate the severity as if the finding is real. Whether it is real is the next answer, and it does not lower the severity.
- Say whether the finding is REAL, from the code shown:
  * "yes": the code shown has the defect the finding describes.
  * "no": the code shown contradicts the finding, for example the guard it says is missing is in the lines shown.
  * "unclear": the code shown does not settle it. Use this when the answer depends on code you were not shown.
  A finding you rate "no" or "unclear" stays in the report. The architect decides what happens to it.
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

Your `reason` is one line about THIS finding's severity and whether it is real, with no reference to any other finding.

A finding whose rule id you don't recognize as a named convention is NOT automatically less
credible — it is the audit's BACKSTOP catching a genuine issue that no named rule happened to
cover. Judge it purely on the evidence in its own `detail`/`snippet`, exactly like any other
finding: do not discount its severity for being unnamed, and do not inflate it
either. The absence of a rule id is not evidence of anything.

Return ONLY JSON, no prose:
{"severity":"critical|high|medium|low","real":"yes|no|unclear","effort":"low|medium|high","category":"authorization|authentication|secret-exposure|injection|transport-security|rls-policy|resource-exposure|input-validation|error-handling|arch-conformance|testing-style|performance","reason":"one line"}"#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// Remove cross-finding dedup pointers like "same as [6]" / "duplicate of [10]" (and a
/// trailing bare "[12]") from a calibration reason, case-insensitively, then tidy leftover
/// separators. These indices are batch-local and unreliable; the merge relationship is
/// already structural (rule_id + path + line + also_matches), so the prose is pure noise.
pub(super) fn strip_dedup_pointers(reason: &str) -> String {
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
/// CRITICAL GUARDRAIL (calibration review, the cycle-2 queue-hardening notes,
/// extended cycle-5): this predicate is used ONLY to (a) block an upward calibration move in
/// [`apply_verdicts`], (b) cap the deterministic D5 floor's critical escalation in
/// [`severity_calibration_floor`], and (c) break ties in the "do now" ranking in
/// `report_export`. It must NEVER be used to LOWER a severity — a genuine critical whose
/// justification merely mentions a mitigation must never be buried by this logic. See the call
/// sites' own doc comments for the never-demote invariant.
pub(super) const EXPLOIT_PRECONDITION_PHRASES: &[&str] = &[
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
pub(super) const REQUIRES_NEGATION_SUFFIX: &str = "no ";

/// Whether `lower` (already-lowercased) contains the "requires " precondition phrase in a
/// NON-negated form — i.e. an occurrence of "requires " not immediately followed by "no "
/// ([`REQUIRES_NEGATION_SUFFIX`]). Scans every occurrence (not just the first) so text with both
/// a negated occurrence ("...requires no login...") and a genuine one elsewhere ("...requires
/// forging a session cookie...") still registers the genuine one.
pub(super) fn contains_non_negated_requires(lower: &str) -> bool {
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

/// Apply one site's verdict to its row. The row is always kept.
///
/// * Severity: the verdict's, through the same rules as ever. A raise that the row's own text or
///   the verdict's reason says rests on an exploit precondition is held at the row's severity: the
///   hold is written to `calibration_rationale` and recorded on the row as the outlier reason
///   [`OutlierReason::SeverityRaiseHeld`], for the tree to show. A precondition can only block a
///   raise, never justify one, and never lowers a severity.
/// * `real` sets `confidence` (`yes` is `high`, `no` is `disputed`, `unclear` is `needs-review`)
///   and nothing else; severity is not lowered because a site is not real.
/// * `needs_review` is derived: set when stage 1 said `unclear` or the site is test code, cleared
///   otherwise (a stale `true` from an earlier pass does not stick). It escalates nothing by
///   itself.
/// * The one-line reason is kept on the row (for the group reviewer) in its `stage1_verdict`
///   capture and only there: the client's `detail` is left exactly as it was, with no
///   `[calibrated: ...]` and no `[needs review: ...]` suffix. A `real: no` verdict writes nothing
///   discard-shaped anywhere: it never removes a site, which only the architect does.
fn apply_verdict(mut f: Finding, verdict: &Stage1Verdict) -> Finding {
    let mut outlier_reasons = Vec::new();
    let raw_reason = verdict.reason.as_str();
    if let Some(sev) = verdict.severity.as_deref() {
        // An unrecognised label keeps the finding's current severity (M3): it must not be read as
        // a verdict of "medium".
        let candidate = parse_model_severity(sev, &f.severity);
        let candidate = candidate.as_str();
        // Raising = moving to a MORE severe label: the candidate's rank EXCEEDS the base
        // severity's, the severity the row carries as it enters this call.
        let would_raise = severity_rank(candidate) > severity_rank(&f.severity);
        let precondition_text = format!("{} {} {raw_reason}", f.detail, f.snippet);
        if would_raise && mentions_exploit_precondition(&precondition_text) {
            // HELD: keep the authored/base severity. A verdict that proposes no raise (equal or a
            // downgrade) is untouched by this branch and falls through exactly as before.
            f.calibration_rationale = Some(format!(
                "Calibration held at {} (the model proposed {candidate}): the finding's own text \
                 or the model's own reasoning records an exploit precondition, which can only \
                 block an upward calibration, never justify one.",
                f.severity
            ));
            outlier_reasons.push(crate::review_tree::OutlierReason::SeverityRaiseHeld);
        } else if candidate != f.severity {
            let before = std::mem::replace(&mut f.severity, candidate.to_string());
            // A fixed template: the two severities and the basis they were checked against, the
            // severity basis of the finding's rule. No model text: this sentence is shown to the
            // client, and the stage 1 reason stays in the row's `stage1_verdict` record only.
            let basis = format!("checked against the severity basis of rule {}", f.rule_id);
            record_severity_adjustment(&mut f, &before, candidate, &basis);
        }
    }
    set_outlier_reasons(&mut f, &outlier_reasons);
    // Derived from `real` alone. `needs_review` is a flag for the reader, not a verdict: it is set
    // from `unclear` or test code and never changes a severity or moves a row.
    f.confidence = Some(verdict.real.confidence().to_string());
    f.needs_review = verdict.real == Real::Unclear || f.in_test;
    // Only the three known effort values are accepted; anything else leaves `effort` as it was.
    if let Some(eff) = verdict.effort.as_deref() {
        if matches!(eff, "low" | "medium" | "high") {
            f.effort = Some(eff.to_string());
        }
    }
    // Only a KNOWN category is accepted; anything else leaves `category` for the rule-id heuristic
    // in `group_semantic` to fill.
    if let Some(cat) = verdict.category.as_deref() {
        if is_known_category(cat) {
            f.category = Some(cat.to_string());
        }
    }
    // Cross-finding pointers ("same as [6]") the model still volunteers are batch-local and wrong;
    // the reason is redacted before it goes anywhere.
    let reason = strip_dedup_pointers(raw_reason.trim());
    let reason = camerata_secrets::redact_str(&reason).into_owned();
    let reason = reason.trim().to_string();
    f.captures.insert(
        STAGE1_CAPTURE.to_string(),
        serde_json::json!({ "real": verdict.real.as_str(), "reason": reason }).to_string(),
    );
    // The reason stays in the row's stage 1 record and goes nowhere else. The client's explanation
    // is left exactly as it was before calibration: no `[calibrated: ...]` and no
    // `[needs review: ...]`, which would put engine bookkeeping in the client's text. In particular
    // a `real: no` verdict never writes anything the junk detection could read as "discard this
    // finding": it never removes a site, which only the architect does.
    f
}

/// Whether `raw` is a stage 1 reply: a verdict object that answers `real` (a site's own call), or
/// a `verdicts` array ([`apply_verdicts`]). The run store decides a stored reply is usable with
/// this; a transcript that merely quotes some object is not.
pub(crate) fn is_verdict_reply(raw: &str) -> bool {
    extract_json_object(raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .is_some_and(|v| v["verdicts"].is_array() || Stage1Verdict::from_value(&v).is_some())
}

/// Apply stage 1 verdicts, addressed by `index`, to `findings`: `{"verdicts":[{"index":0,
/// "severity":..,"real":..,"effort":..,"category":..,"reason":..}]}`. Each verdict goes through
/// [`apply_verdict`]. NEVER drops a finding: one with no verdict, or whose verdict does not say
/// `real`, keeps what it had, and unparseable input keeps every finding as it was.
///
/// A reply cut off at the output limit mid-verdict still holds every verdict that was written out
/// in full: those are read one by one ([`answered_verdicts`]).
pub fn apply_verdicts(raw: &str, findings: Vec<Finding>) -> Vec<Finding> {
    let whole = extract_json_object(raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .filter(|v| v["verdicts"].is_array());
    let verdicts: Vec<serde_json::Value> = match whole {
        Some(v) => v["verdicts"].as_array().cloned().unwrap_or_default(),
        None => answered_verdicts(raw, findings.len()),
    };
    if verdicts.is_empty() {
        return findings;
    }
    findings
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            match verdicts
                .iter()
                .find(|x| x["index"].as_u64() == Some(i as u64))
                .and_then(Stage1Verdict::from_value)
            {
                Some(verdict) => apply_verdict(f, &verdict),
                None => f, // never dropped
            }
        })
        .collect()
}

/// Per-site token estimate for stage 1's reply: one compact verdict object with a one-line reason.
pub(super) const VERIFY_TOKENS_PER_FINDING: u32 = 150;

/// How many sites make one group. A group is the unit of the transcript entry, of the failed-part
/// row (which names the sites it left unchecked) and of the order the sites are asked in; it is
/// not a batch (every site is its own call).
pub(super) const CALIBRATION_BATCH_SIZE: usize = 40;

/// About how much input one site's request carries, in bytes, for the FALLBACK projection only.
/// The batch spend gate projects a window from the bytes of the requests the runner is about to
/// send ([`super::stage_batch::StageSettings::per_item_usd`] is read only where a request is not
/// built or its model is not priced), so a window of large sites cannot outrun the limit on this
/// figure.
const SITE_INPUT_BYTES: u64 = 10_000;

/// The fallback projected input dollars of one site's request on `model`, at the batch price and
/// three characters a token. `None` for a model the price table does not know.
fn site_input_usd(model: &str) -> Option<f64> {
    crate::llm::input_cost_usd(
        model,
        SITE_INPUT_BYTES.div_ceil(3),
        crate::llm::PriceBasis::batch(crate::llm::BATCH_CACHE_TTL),
    )
}

/// `max_tokens` for the reply to `n_findings` sites: the per-verdict text budget plus the
/// calibration stage's thinking allowance. A site's call asks for one.
pub(crate) fn calibration_max_tokens(n_findings: usize) -> u32 {
    staged_max_tokens(
        n_findings,
        VERIFY_TOKENS_PER_FINDING,
        camerata_ui_core::scan::CALIBRATION_EFFORT,
    )
}

/// The verdict objects a calibration reply holds IN FULL, one per finding index below `n` (the
/// first of a repeated index wins), read one by one with [`complete_array_objects`]. The last,
/// unfinished object of a reply cut off at the output limit is not guessed at. Empty for a reply
/// that holds no `verdicts` array.
pub(super) fn answered_verdicts(raw: &str, n: usize) -> Vec<serde_json::Value> {
    let mut seen = std::collections::BTreeSet::new();
    complete_array_objects(raw, "verdicts")
        .into_iter()
        .filter_map(|object| serde_json::from_str::<serde_json::Value>(object).ok())
        .filter(|v| {
            v["index"]
                .as_u64()
                .is_some_and(|i| (i as usize) < n && seen.insert(i))
        })
        .collect()
}

// ── One structured call per site ───────────────────────────────────────────────────────────

/// Why a reply was turned down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemReject {
    pub reason: String,
    /// The reply was cut off at the output limit, so asking again with a larger cap can help.
    pub truncated: bool,
}

/// The sites of one stage 1 run, as one request each.
///
/// This is the shape a batch runner takes (`items`, `absorb`, `retry`), and the real-time driver
/// ([`verify_findings`]) runs the same sites through the same methods: a site's request is built
/// by [`Self::fitted_request`], its reply read by [`Self::parse_reply`] and kept by
/// [`Self::absorb`], and a reply that was cut off gets one retry from [`Self::retry`].
///
/// Every request passes `ensure_request_fits` before it leaves. A site whose request is over the
/// model's window is split first, by shrinking the code shown around it (30 lines each side, then
/// 15, 7, 3 and the site's own line); one that is over the window even then is refused, listed in
/// [`Self::refused`] and never sent. Nothing is truncated and no site is dropped.
pub struct Stage1Items<'a> {
    repo: String,
    model: Option<String>,
    system: String,
    nonce: crate::prompt_frame::FrameNonce,
    repo_shape: String,
    files: std::collections::HashMap<&'a str, &'a str>,
    rules: std::collections::HashMap<String, &'a str>,
    findings: Vec<Finding>,
    verdicts: Vec<Option<Stage1Verdict>>,
    refused: Vec<(usize, request_budget::RequestTooLarge)>,
}

impl<'a> Stage1Items<'a> {
    /// The sites of `findings`, in order, for `repo`. The model is the caller's calibration pick
    /// (`None` asks the run's default model); see [`resolve_calibration_model`].
    // The batch runner's entry points (the batch-stages work order); tested here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn build(
        repo: &str,
        findings: &[Finding],
        calibration_model: Option<&str>,
        inputs: &Stage1Inputs<'a>,
    ) -> Self {
        Self::from_findings(repo, findings.to_vec(), calibration_model, inputs)
    }

    fn from_findings(
        repo: &str,
        findings: Vec<Finding>,
        calibration_model: Option<&str>,
        inputs: &Stage1Inputs<'a>,
    ) -> Self {
        // One delimiter token for every call of this run (see `crate::prompt_frame`), drawn so that
        // it occurs in none of the text it will frame: the evidence of each site and the files the
        // windows are cut from.
        let evidence = finding_evidence_texts(&findings);
        let mut avoid: Vec<&str> = evidence.iter().map(String::as_str).collect();
        for (path, body) in inputs.files {
            avoid.push(path);
            avoid.push(body);
        }
        let nonce = crate::prompt_frame::FrameNonce::generate_avoiding(&avoid);
        let system = crate::prompt_frame::with_untrusted_clause(verify_system_prompt(), &nonce);
        let verdicts = vec![None; findings.len()];
        Self {
            repo: repo.to_string(),
            model: calibration_model.map(str::to_string),
            system,
            nonce,
            repo_shape: inputs.repo_shape.to_string(),
            files: inputs
                .files
                .iter()
                .map(|(p, b)| (p.as_str(), b.as_str()))
                .collect(),
            rules: inputs
                .rules
                .iter()
                .map(|(id, text)| (id.to_ascii_uppercase(), text.as_str()))
                .collect(),
            findings,
            verdicts,
            refused: Vec::new(),
        }
    }

    /// Bytes of the system prompt every request of the run carries.
    #[cfg(test)]
    pub(super) fn system_len(&self) -> usize {
        self.system.len()
    }

    pub(super) fn len(&self) -> usize {
        self.findings.len()
    }

    /// The window of the file around site `i`, `radius` lines each side; empty when the file is
    /// not among the run's files.
    fn window(&self, i: usize, radius: usize) -> CodeWindow {
        let f = &self.findings[i];
        let path = f.path.trim_start_matches("./");
        let content = self
            .files
            .get(f.path.as_str())
            .or_else(|| self.files.get(path));
        match content {
            Some(content) => {
                let mut w = code_window(content, f.line, radius);
                w.text = camerata_secrets::redact_str(&w.text).into_owned();
                w
            }
            None => CodeWindow::default(),
        }
    }

    /// The part of every site's prompt that is the same for all of them: the repository line and
    /// the proportionality paragraph. The system prompt and this text are the cacheable prefix.
    pub(super) fn shared_prefix(&self) -> String {
        let mut prompt = format!("Repository: {}\n", self.repo);
        // Proportionality signal: a small or young codebase must not be held to the architecture
        // of a large one.
        prompt.push_str(&format!(
            "{} Judge this finding PROPORTIONALLY to the codebase's size and maturity: an \
             'over-engineering'/'missing abstraction'/YAGNI note on a small codebase is a \
             debatable preference with a capped severity, not a violation.\n",
            self.repo_shape
        ));
        prompt
    }

    /// The user prompt for site `i` with `radius` lines of code each side, after the shared prefix.
    fn site_prompt(&self, i: usize, radius: usize) -> String {
        let f = &self.findings[i];
        let mut prompt = self.shared_prefix();
        match self.rules.get(&f.rule_id.to_ascii_uppercase()) {
            Some(text) => prompt.push_str(&format!("\nRule {}: {text}\n", f.rule_id)),
            None => prompt.push_str(&format!("\nRule {}\n", f.rule_id)),
        }
        prompt.push_str(&format!(
            "\nThe finding (scanned severity {}):\n",
            f.severity
        ));
        // The location, the quoted code and the detail all derive from the client's files.
        prompt.push_str(&crate::prompt_frame::frame_untrusted(
            &self.nonce,
            &format!("finding {i}"),
            &format!(
                "{}:{} — {} :: {}",
                f.path,
                f.line,
                camerata_secrets::redact_str(&f.snippet),
                camerata_secrets::redact_str(&f.detail)
            ),
        ));
        let window = self.window(i, radius);
        if window.text.is_empty() {
            prompt.push_str(
                "\nThe rest of the file is not available, so the quoted code above is all you can \
                 see. Say `unclear` if it does not settle whether the finding is real.\n",
            );
        } else {
            prompt.push_str(&format!(
                "\nThe code around the site (lines {} to {} of {}; the finding is at line {}):\n",
                window.first_line, window.last_line, f.path, f.line
            ));
            prompt.push_str(&crate::prompt_frame::frame_untrusted(
                &self.nonce,
                &format!("code around finding {i}"),
                &window.text,
            ));
        }
        prompt
    }

    fn request_at(&self, i: usize, radius: usize) -> LlmRequest {
        // The system prompt and the shared prefix are the same for every site of the run: one
        // cache breakpoint at the end of the prefix lets the API serve them from its prompt cache
        // instead of charging them again for every site.
        let prefix_end = self.shared_prefix().len();
        with_stage_effort(
            LlmRequest::new(self.site_prompt(i, radius))
                .with_cache_breakpoints(vec![prefix_end])
                .with_system(self.system.clone())
                .with_max_tokens(calibration_max_tokens(1))
                .with_temperature(AUDIT_TEMPERATURE)
                .with_response_schema(reply_schemas::stage1_reply_schema())
                .with_model(stage_model(self.model.as_deref())),
            Stage::Calibration,
        )
    }

    /// The request for site `i`, at the widest window that fits the model's window. An error when
    /// not even the site's own line fits: such a request is never sent.
    pub(super) fn fitted_request(
        &self,
        i: usize,
    ) -> Result<LlmRequest, request_budget::RequestTooLarge> {
        let radii: &[usize] = if self.window(i, CODE_WINDOW_RADIUS).text.is_empty() {
            &[0]
        } else {
            &WINDOW_SHRINK_RADII
        };
        let mut refusal = None;
        for &radius in radii {
            let req = self.request_at(i, radius);
            match request_budget::ensure_request_fits(&req) {
                Ok(()) => return Ok(req),
                Err(e) => refusal = Some(e),
            }
        }
        Err(refusal.expect("at least one radius was tried"))
    }

    /// One request per site that fits, with the site's position as its id. A site that does not
    /// fit even with the least code shown is left out and listed by [`Self::refused`].
    // The batch runner's entry points (the batch-stages work order); tested here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn items(&mut self) -> Vec<(usize, LlmRequest)> {
        self.refused.clear();
        let mut out = Vec::with_capacity(self.len());
        for i in 0..self.len() {
            match self.fitted_request(i) {
                Ok(req) => out.push((i, req)),
                Err(e) => self.refused.push((i, e)),
            }
        }
        out
    }

    /// The sites whose request did not fit the model's window, with the refusal. Never sent.
    // The batch runner's entry points (the batch-stages work order); tested here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn refused(&self) -> &[(usize, request_budget::RequestTooLarge)] {
        &self.refused
    }

    /// Read a reply as a verdict: a JSON object that answers `real`.
    fn parse_reply(&self, resp: &LlmResponse) -> Result<Stage1Verdict, ItemReject> {
        extract_json_object(&resp.text)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|v| Stage1Verdict::from_value(&v))
            .ok_or_else(|| ItemReject {
                reason: if resp.truncated {
                    "the reply was cut off at the output limit before the verdict was complete"
                        .to_string()
                } else {
                    "the reply is not a verdict that says whether the finding is real".to_string()
                },
                truncated: resp.truncated,
            })
    }

    /// Keep the verdict in `resp` for site `id`. A reply that is not a verdict is turned down with
    /// the reason, and the site keeps what it had.
    // The batch runner's entry points (the batch-stages work order); tested here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn absorb(&mut self, id: usize, resp: &LlmResponse) -> Result<(), ItemReject> {
        let verdict = self.parse_reply(resp)?;
        self.verdicts[id] = Some(verdict);
        Ok(())
    }

    /// The one retry a turned-down reply gets: the same request with twice the output cap, for a
    /// reply that was cut off. A reply that is simply not a verdict gets none.
    pub fn retry(&self, id: usize, reject: &ItemReject) -> Option<LlmRequest> {
        if !reject.truncated {
            return None;
        }
        let base = self.fitted_request(id).ok()?;
        let raised = base
            .max_tokens
            .saturating_mul(2)
            .min(AGGREGATE_MAX_TOKENS_CEILING);
        if raised <= base.max_tokens {
            return None;
        }
        let req = base.with_max_tokens(raised);
        request_budget::ensure_request_fits(&req).ok()?;
        Some(req)
    }

    /// The sites with their verdicts applied, in order. Every site is returned.
    pub fn into_findings(self) -> Vec<Finding> {
        self.findings
            .into_iter()
            .zip(self.verdicts)
            .map(|(f, verdict)| match verdict {
                Some(v) => apply_verdict(f, &v),
                None => f,
            })
            .collect()
    }
}

/// The sites asked per batch submission on the batch path. A window of requests is built, sent,
/// waited for and absorbed before the next is built, so memory is bounded by a window however many
/// sites the run has (about 50 MB at 10 KB a request).
pub(super) const STAGE1_BATCH_WINDOW: usize = 5_000;

/// Stage 1's sites in the shape the shared batch runner takes ([`StageItems`]): one item per
/// site, named by its site key, its request built only when the runner is about to send it, in
/// ranges of [`CALIBRATION_BATCH_SIZE`]. Wraps [`Stage1Items`] (the requests, the reading of a
/// reply, the verdicts) and adds what only a run needs: the keys, the sites refused for size, the
/// transcript of each group of sites.
struct Stage1Run<'a, 't> {
    items: Stage1Items<'a>,
    /// The site key of each site, by position.
    keys: Vec<String>,
    index_of: std::collections::HashMap<String, usize>,
    /// Sites whose request did not fit the window; recorded while the runner builds requests.
    refused: std::sync::Mutex<Vec<(usize, request_budget::RequestTooLarge)>>,
    /// Sites whose last turned-down reply was cut off at the output limit (the retry needs it).
    truncated: std::collections::BTreeSet<usize>,
    feedback: Option<(&'t crate::transcript::TranscriptStore, &'t str)>,
    repo: String,
    model_label: String,
    n_groups: usize,
    /// Groups with a transcript entry so far, with the sites answered in each.
    registered: std::collections::BTreeMap<usize, usize>,
}

impl Stage1Run<'_, '_> {
    fn group_of(site: usize) -> usize {
        site / CALIBRATION_BATCH_SIZE
    }

    /// The session id of group `gi`: single-group runs keep the id this stage always used.
    fn session(&self, gi: usize) -> String {
        if self.n_groups > 1 {
            format!("audit-{}-calibrate-b{gi}", self.repo)
        } else {
            format!("audit-{}-calibrate", self.repo)
        }
    }

    fn group_len(&self, gi: usize) -> usize {
        CALIBRATION_BATCH_SIZE.min(self.items.len() - gi * CALIBRATION_BATCH_SIZE)
    }

    /// Open group `gi`'s transcript entry the first time one of its sites is heard from.
    fn open_group(&mut self, gi: usize) {
        let Some((store, key)) = self.feedback else {
            return;
        };
        if self.registered.contains_key(&gi) {
            return;
        }
        let group_len = self.group_len(gi);
        let first_prompt = self
            .items
            .fitted_request(gi * CALIBRATION_BATCH_SIZE)
            .map(|r| r.prompt)
            .unwrap_or_default();
        let prompt = if group_len > 1 {
            format!(
                "{first_prompt}\n(this is the first of {group_len} sites in this group; each \
                 site is its own call with the same instructions)"
            )
        } else {
            first_prompt
        };
        let repo = &self.repo;
        store.register(
            key,
            crate::transcript::AgentTranscript {
                session_id: self.session(gi),
                role: format!(
                    "calibrating {group_len} findings on {} \u{2014} {repo}{}",
                    self.model_label,
                    if self.n_groups > 1 {
                        format!(" (batch {}/{})", gi + 1, self.n_groups)
                    } else {
                        String::new()
                    }
                ),
                prompt,
                output: String::new(),
                status: "running".to_string(),
            },
        );
        self.registered.insert(gi, 0);
    }

    fn note_output(&mut self, site: usize, text: &str, answered: bool) {
        let gi = Self::group_of(site);
        self.open_group(gi);
        let Some((store, key)) = self.feedback else {
            return;
        };
        let session = self.session(gi);
        if self.group_len(gi) == 1 && answered {
            store.append_output(key, &session, text);
        } else {
            store.append_output(
                key,
                &session,
                &format!("\u{2500}\u{2500} site {site} \u{2500}\u{2500}\n{text}"),
            );
        }
        if answered {
            *self.registered.entry(gi).or_insert(0) += 1;
        }
    }

    /// Close every transcript entry: blocked where nothing in the group was answered but some
    /// site failed, done otherwise.
    fn close_groups(&self, failed_in: &std::collections::BTreeMap<usize, usize>) {
        let Some((store, key)) = self.feedback else {
            return;
        };
        for (&gi, &answered) in &self.registered {
            let session = self.session(gi);
            let failed = failed_in.get(&gi).copied().unwrap_or(0);
            if answered == 0 && failed > 0 {
                store.append_output(
                    key,
                    &session,
                    "every calibration call for this group failed or timed out \u{2014} findings \
                     pass through unchanged.",
                );
                store.set_status(key, &session, "blocked");
            } else {
                store.set_status(key, &session, "done");
            }
        }
    }
}

impl StageItems for Stage1Run<'_, '_> {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        Vec::new()
    }

    fn build_range(&self) -> usize {
        CALIBRATION_BATCH_SIZE
    }

    fn batch_window(&self) -> usize {
        STAGE1_BATCH_WINDOW
    }

    fn ids(&self) -> Vec<String> {
        self.keys.clone()
    }

    fn items_for(&self, ids: &[String]) -> Vec<(String, LlmRequest)> {
        #[cfg(test)]
        BUILT_RANGES.with(|r| r.borrow_mut().push(ids.len()));
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(&i) = self.index_of.get(id) else {
                continue;
            };
            match self.items.fitted_request(i) {
                Ok(req) => out.push((id.clone(), req)),
                Err(e) => self
                    .refused
                    .lock()
                    .expect("the refused list is not poisoned")
                    .push((i, e)),
            }
        }
        out
    }

    fn absorb(&mut self, id: &String, resp: LlmResponse) -> Result<(), RunnerReject> {
        let Some(&i) = self.index_of.get(id) else {
            return Err(RunnerReject::new(
                "the reply names a site this run does not have",
            ));
        };
        match self.items.absorb(i, &resp) {
            Ok(()) => {
                self.truncated.remove(&i);
                self.note_output(i, &resp.text, true);
                Ok(())
            }
            Err(reject) => {
                if reject.truncated {
                    self.truncated.insert(i);
                }
                self.note_output(i, &resp.text, false);
                Err(RunnerReject::new(reject.reason))
            }
        }
    }

    fn retry(&self, id: &String, reject: &RunnerReject) -> Option<LlmRequest> {
        let &i = self.index_of.get(id)?;
        self.items.retry(
            i,
            &ItemReject {
                reason: reject.reason.clone(),
                truncated: self.truncated.contains(&i),
            },
        )
    }
}

/// Run the stage 1 first reviewer over a repo's findings, one structured call per site, on the
/// shared batch runner ([`stage_batch::run_stage`]): as Message Batches when the run asked for
/// them and the model seam has a batch API, in real time otherwise. Graceful: a site that cannot
/// be asked passes through unchanged and is disclosed as a failed part; none is ever dropped.
///
/// The sites are grouped in [`CALIBRATION_BATCH_SIZE`] for the transcript and for the failed-part
/// rows, and their requests are built in ranges of that size, never all at once. With a run's
/// checkpoint folder in scope ([`crate::checkpoint::with_run_checkpoints`]) every absorbed site
/// is written down as it ends, so a restarted run asks only the sites with no result, polls a
/// batch still running instead of submitting it again, and reports the usage of the sites it
/// skipped.
///
/// `feedback`, when present, records each group into the transcript: the group's entry opens with
/// its REAL generated prompt when its first site is heard from, each reply is appended as it
/// lands, and a terminal status is set at the end. A transcript-recording failure can never
/// affect calibration.
///
/// C3-1b: this pass is also the ONLY source of the `effort` (hour-estimation) field, so it
/// returns a second `Vec<FailedPass>` alongside the (never-dropped) findings. A single
/// `FailedPass{pass: "hour estimation", ..}` entry says the pass produced NO usable effort
/// estimate for ANY finding. Otherwise each group with sites that could not be checked (every call
/// failed, or the reply was not a verdict) is a `calibration batch` part naming those sites, and
/// the sites whose request did not fit the model's window are one part naming them. Every site
/// either has a verdict or is named in a failed part.
pub async fn verify_findings(
    llm: &dyn LlmPort,
    repo: &str,
    findings: Vec<Finding>,
    calibration_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
    inputs: &Stage1Inputs<'_>,
) -> (Vec<Finding>, Vec<FailedPass>) {
    if findings.is_empty() {
        return (findings, Vec::new());
    }
    let keys = crate::inspections::site_keys_of(&findings);
    let items = Stage1Items::from_findings(repo, findings, calibration_model, inputs);
    let n = items.len();
    let n_groups = n.div_ceil(CALIBRATION_BATCH_SIZE);
    let index_of: std::collections::HashMap<String, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i))
        .collect();
    let mut run = Stage1Run {
        items,
        keys,
        index_of,
        refused: std::sync::Mutex::new(Vec::new()),
        truncated: std::collections::BTreeSet::new(),
        feedback,
        repo: repo.to_string(),
        model_label: calibration_model.unwrap_or("default").to_string(),
        n_groups,
        registered: std::collections::BTreeMap::new(),
    };

    let model = stage_model(calibration_model);
    let (mode, batch_api) = mode_and_api(llm, inputs.batch, inputs.concurrency);
    let checkpoint = crate::checkpoint::stage_checkpoint(&format!("calibration_{repo}"));
    let ctx = StageCtx {
        repo,
        model: &model,
        realtime: llm,
        batch: batch_api,
        meter,
        job: inputs.job,
        checkpoint: checkpoint.as_ref(),
        settings: StageSettings {
            per_item_usd: site_input_usd(&model),
            ..StageSettings::default()
        },
    };
    let outcome = run_stage(Stage::Calibration, &mut run, mode, &ctx).await;

    // What the runner could not get a result for, by site. A refusal for size is its own part
    // (below); the spend limit and the outage stop record themselves on the meter.
    let refused_sites: std::collections::BTreeSet<usize> = run
        .refused
        .lock()
        .expect("the refused list is not poisoned")
        .iter()
        .map(|(i, _)| *i)
        .collect();
    let mut trail = StageTrail::default();
    let mut group_trails: Vec<StageTrail> = vec![StageTrail::default(); n_groups];
    let mut unchecked: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut failed_in: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for f in &outcome.failed {
        let Some(&i) = run.index_of.get(&f.id) else {
            continue;
        };
        if refused_sites.contains(&i) {
            continue;
        }
        let gi = Stage1Run::group_of(i);
        *failed_in.entry(gi).or_insert(0) += 1;
        if matches!(f.class, FailureClass::SpendLimit | FailureClass::OutageStop) {
            continue;
        }
        if f.class == FailureClass::Content {
            trail.note_unusable(&f.reason, false, vec![f.reason.clone()]);
            group_trails[gi].note_unusable(&f.reason, false, vec![f.reason.clone()]);
        } else {
            trail.note_failed_item(f.class, &f.reason, &f.attempts);
            group_trails[gi].note_failed_item(f.class, &f.reason, &f.attempts);
        }
        unchecked.insert(i);
    }
    if let Some(cancelled) = &outcome.cancelled {
        for id in &cancelled.not_sent {
            if let Some(&i) = run.index_of.get(id) {
                if !refused_sites.contains(&i) {
                    unchecked.insert(i);
                }
            }
        }
    }
    run.close_groups(&failed_in);

    // Sites still unchecked, grouped, and the sites that were never sent; each is named below.
    let failed_groups: Vec<(usize, Vec<Finding>)> = (0..n_groups)
        .filter_map(|gi| {
            let sites: Vec<Finding> = unchecked
                .iter()
                .filter(|&&i| Stage1Run::group_of(i) == gi)
                .map(|&i| run.items.findings[i].clone())
                .collect();
            (!sites.is_empty()).then_some((gi, sites))
        })
        .collect();
    let refused_list = std::mem::take(
        &mut *run
            .refused
            .lock()
            .expect("the refused list is not poisoned"),
    );
    let mut refused: Vec<(Finding, request_budget::RequestTooLarge)> = refused_list
        .into_iter()
        .map(|(i, e)| (run.items.findings[i].clone(), e))
        .collect();
    refused.sort_by(|a, b| (a.0.path.as_str(), a.0.line).cmp(&(b.0.path.as_str(), b.0.line)));
    let calibrated = run.items.into_findings();

    // C3-1b: a run-wide (not per-site) disclosure: every finding calibration was handed came back
    // with NO usable effort estimate at all. Neither D5 (severity floor) nor D6 (severity ceiling)
    // below ever sets `effort`, so checking it here, before those text-derived passes run, is
    // equivalent to checking the final output and reads clearer.
    let mut estimation_failed_passes = Vec::new();
    if !calibrated.is_empty() && calibrated.iter().all(|f| f.effort.is_none()) {
        if trail.attempts.is_empty() {
            trail.note_unusable(
                "the calibration reply held no usable remediation-effort estimate",
                false,
                Vec::new(),
            );
        }
        estimation_failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: "hour estimation".to_string(),
            reason: "the calibration pass returned no usable remediation-effort estimate for \
                     any finding this run"
                .to_string(),
            files_not_reviewed: 0,
            class: trail.class(),
            files: Vec::new(),
            attempts: trail.attempts.clone(),
            scope_note: false,
        });
    } else {
        // Some findings were estimated, so the run-wide miss above does not apply, but a group
        // with sites that never got their verdict is still a part that failed.
        for (gi, sites) in failed_groups {
            estimation_failed_passes.push(failed_calibration_chunk(
                repo,
                gi,
                n_groups,
                &sites,
                &group_trails[gi],
            ));
        }
    }
    if !refused.is_empty() {
        estimation_failed_passes.push(failed_unsent_sites(repo, &refused));
    }
    // D5: the deterministic severity floor runs regardless of whether the LLM calibration pass
    // succeeded: it re-derives its verdict from the finding's own text, not the model's, so it is
    // exactly as available when every call failed as when one succeeded.
    // D6: the severity ceiling (R1/R2) runs immediately after, over the SAME set, for the same
    // reason: both are text-derived and safe regardless of whether calibration ran.
    let calibrated = apply_severity_ceiling_rules(apply_severity_calibration_rules(calibrated));
    (calibrated, estimation_failed_passes)
}

/// The failed part for the sites whose request did not fit the model's window: names them (as
/// `path:line`, at most five spelled out) and the files they sit in. They were not sent and pass
/// through without a severity check or an effort estimate.
fn failed_unsent_sites(
    repo: &str,
    refused: &[(Finding, request_budget::RequestTooLarge)],
) -> FailedPass {
    let mut files: Vec<String> = refused.iter().map(|(f, _)| f.path.clone()).collect();
    files.sort();
    files.dedup();
    let named: Vec<String> = refused
        .iter()
        .take(5)
        .map(|(f, _)| format!("{}:{}", f.path, f.line))
        .collect();
    let more = refused.len().saturating_sub(named.len());
    let mut trail = StageTrail::default();
    trail.note_unusable(&refused[0].1.to_string(), false, Vec::new());
    FailedPass {
        repo: repo.to_string(),
        pass: "calibration (request too large)".to_string(),
        reason: format!(
            "the request for {} finding{} does not fit the model's input window even with the \
             least code shown, so {} not sent and {} left without a severity check or an effort \
             estimate: {}{}. {}",
            refused.len(),
            if refused.len() == 1 { "" } else { "s" },
            if refused.len() == 1 {
                "it was"
            } else {
                "they were"
            },
            if refused.len() == 1 {
                "it is"
            } else {
                "they are"
            },
            named.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            },
            refused[0].1
        ),
        files_not_reviewed: 0,
        class: FailureClass::Content,
        files,
        attempts: trail.attempts,
        scope_note: false,
    }
}

/// The failed part for one calibration chunk whose calls all failed: names the findings it left
/// uncalibrated (as `path:line`, at most five spelled out) and the files they sit in, so the
/// operator can see what was not checked and the resume can run the chunk again.
pub(super) fn failed_calibration_chunk(
    repo: &str,
    chunk_idx: usize,
    n_chunks: usize,
    chunk: &[Finding],
    trail: &StageTrail,
) -> FailedPass {
    let mut files: Vec<String> = chunk.iter().map(|f| f.path.clone()).collect();
    files.sort();
    files.dedup();
    let named: Vec<String> = chunk
        .iter()
        .take(5)
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect();
    let more = chunk.len().saturating_sub(named.len());
    let pass = if n_chunks > 1 {
        format!("calibration batch {} of {n_chunks}", chunk_idx + 1)
    } else {
        "calibration".to_string()
    };
    FailedPass {
        repo: repo.to_string(),
        pass,
        reason: format!(
            "the calibration step failed for {} finding{} left without a severity check or an \
             effort estimate: {}{}",
            chunk.len(),
            if chunk.len() == 1 { "" } else { "s" },
            named.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ),
        files_not_reviewed: 0,
        class: trail.class(),
        files,
        attempts: trail.attempts.clone(),
        scope_note: false,
    }
}
