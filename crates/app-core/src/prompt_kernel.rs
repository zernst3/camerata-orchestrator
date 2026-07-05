//! The shared governance prompt kernel.
//!
//! Camerata is model-agnostic (Claude today; open-weight GLM / DeepSeek tomorrow). Safety-tuned
//! models spontaneously write tests, self-correct, and program defensively; literal open-weight
//! models will NOT unless the prompt explicitly mandates it. This module owns the ONE shared
//! protocol every agent prompt embeds, so that behavior is identical at every entry point across
//! every model.
//!
//! Two variants:
//! - [`GOVERNANCE_KERNEL`] — full protocol for writing agents (implementers, resolvers, fleet).
//! - [`GOVERNANCE_KERNEL_READONLY`] — the analysis protocol for reviewers, auditors, analysts,
//!   and single-shot JSON producers.
//!
//! [`kernel_for`] returns the full kernel plus the correct per-tier addendum (see the plan's
//! "Per-tier addenda"). Every runner resolves its model from the project tier map before
//! building the prompt, so keying the addendum off the model or tier string needs no new plumbing.
//!
//! Source: `docs/plans/2026-07-05_prompt-hardening-and-governance-kernel.md` (sections 4 and 5).
//! Every clause is imperative + inspectable (assertable by prompt unit tests), vendor-neutral,
//! with tool references kept conditional ("if available"), and each variant kept under ~450 tokens
//! for cache-prefix stability.

/// The full governance protocol for WRITING agents (code-writing / repo-walking task prompts).
///
/// Embed verbatim (via `{KERNEL}`) in every task-prompt builder and in
/// `api_agent_driver::build_system_prompt`. See section 4 of the plan.
pub const GOVERNANCE_KERNEL: &str = "\
=== CAMERATA OPERATING PROTOCOL (mandatory for every agent, every model) ===
These rules are not suggestions. Follow them exactly, in order, on every task.
1. GROUND EVERY FACT. Read the actual code before acting on it. Never state, assume, or
   build on a repo fact you have not verified by reading a file this session. Inventing
   files, APIs, symbols, or capabilities is the worst failure you can commit.
2. PLAN, THEN ACT. Before your first write, enumerate the files you will change, the
   behavior that must hold, and the tests that will prove it. If the task names a pattern
   or class of problem, search and enumerate EVERY occurrence.
3. TESTS ARE PART OF THE CHANGE. Every new/changed behavior gets a test in the project's
   style that fails if the behavior is removed. Never weaken/delete/skip an existing test
   to fit your change. A change you cannot test must be called out in your final report.
4. PROGRAM DEFENSIVELY. Handle error and empty/None cases on every path you touch.
   Validate external input at the boundary. No panics/unwraps/unhandled exceptions on
   fallible paths unless the file's pattern does. Match surrounding conventions; add no
   new dependency/pattern/style the task does not require.
5. VERIFY BEFORE DONE. You are done only after re-reading, end to end, every file you
   changed and confirming: every requirement maps to a concrete change; no syntax errors,
   missing imports, or dangling references; no unrelated file touched; every project rule
   still holds. Fix what you find, then check again.
6. IF UNSURE, DO NOT GUESS. In order: (a) read more of the repo; (b) if a clarification or
   escalation tool is available and the blocker qualifies, use it and stop; (c) else take
   the most conservative compliant action and record the uncertainty in your report.
7. REPORT IN CONTRACT FORM. End with exactly the task's specified output format. If none,
   end with CHANGES / TESTS / CONCERNS. No other prose after the report.
=== END OPERATING PROTOCOL ===";

/// The read-only governance protocol for reviewers, auditors, analysts, and single-shot JSON
/// producers. See section 4 of the plan.
pub const GOVERNANCE_KERNEL_READONLY: &str = "\
=== CAMERATA OPERATING PROTOCOL (analysis) ===
1. GROUND EVERY CLAIM. Base every statement only on provided material or files you read.
   Cite the file/line/clause. Label anything unverifiable \"cannot verify\"; never present
   an assumption as fact.
2. ENUMERATE, THEN JUDGE. Work through the inputs systematically (each rule, criterion,
   clause, file, in order) and finish the enumeration before concluding. Do not stop at
   the first hit.
3. IF UNSURE, SAY SO. An unverifiable point is reported as such (finding / bounce reason /
   unknown), never silently passed and never guessed.
4. EXACT OUTPUT ONLY. Emit exactly the specified format: nothing before/after, no fences
   unless asked. Re-check output against the schema before finishing.
=== END OPERATING PROTOCOL ===";

/// The per-tier addendum for the FAST / LOW tier (Haiku, DeepSeek-Flash). Low tiers must fail
/// loudly, not creatively. See section 5 "Per-tier addenda".
pub const KERNEL_ADDENDUM_FAST: &str = "\
TIER DISCIPLINE (fast): Do exactly what the task says and nothing else. If anything is \
ambiguous or exceeds what you can verify, return INCOMPLETE: <reason> instead of attempting it.";

/// The per-tier addendum for the BALANCED / MID tier (Sonnet, DeepSeek-Pro — surgically precise
/// but literal). See section 5 "Per-tier addenda".
pub const KERNEL_ADDENDUM_BALANCED: &str = "\
TIER DISCIPLINE (balanced): Use the full TDD loop. Write the failing test FIRST, then \
implement, then confirm the test would fail without the change. Run the rule-5 self-review TWICE.";

/// The per-tier addendum for the STRONGEST / ORCHESTRATION tier (Opus, GLM). See section 5.
pub const KERNEL_ADDENDUM_STRONGEST: &str = "\
TIER DISCIPLINE (strongest): Verify delegation. Read the files a delegate claims to have \
written; restate the acceptance criteria in every subtask you hand off.";

/// The tier a model string resolves to for the purpose of selecting a kernel addendum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelTier {
    /// Fast / low tier: fail loudly (Haiku, DeepSeek-Flash).
    Fast,
    /// Balanced / mid tier: full TDD loop + double self-review (Sonnet, DeepSeek-Pro).
    Balanced,
    /// Strongest / orchestration tier: delegation verification (Opus, GLM).
    Strongest,
}

impl KernelTier {
    /// The per-tier addendum text for this tier.
    pub fn addendum(self) -> &'static str {
        match self {
            KernelTier::Fast => KERNEL_ADDENDUM_FAST,
            KernelTier::Balanced => KERNEL_ADDENDUM_BALANCED,
            KernelTier::Strongest => KERNEL_ADDENDUM_STRONGEST,
        }
    }
}

/// Resolve a model id or tier name to a [`KernelTier`].
///
/// Accepts both explicit tier names (`fast`, `low`, `balanced`, `mid`, `strongest`,
/// `orchestration`) and model ids/substrings (`haiku`, `deepseek-flash`, `sonnet`,
/// `deepseek-pro`, `opus`, `glm`, ...). Matching is case-insensitive and substring-based so a
/// fully-qualified model id (e.g. `claude-opus-4-8` or `us.anthropic.claude-3-5-sonnet`) maps
/// correctly. Anything unrecognized defaults to [`KernelTier::Balanced`] — the middle,
/// literal-model discipline is the safest default for an unknown model.
pub fn tier_of(model_or_tier: &str) -> KernelTier {
    let s = model_or_tier.to_ascii_lowercase();

    // Explicit tier names first (exact-ish), then model-id substrings.
    if s == "fast" || s == "low" {
        return KernelTier::Fast;
    }
    if s == "balanced" || s == "mid" {
        return KernelTier::Balanced;
    }
    if s == "strongest" || s == "orchestration" || s == "high" {
        return KernelTier::Strongest;
    }

    // Strongest / orchestration models.
    if s.contains("opus") || s.contains("glm") {
        return KernelTier::Strongest;
    }
    // Fast / low models.
    if s.contains("haiku") || s.contains("flash") {
        return KernelTier::Fast;
    }
    // Balanced / mid models.
    if s.contains("sonnet") || s.contains("deepseek-pro") || s.contains("deepseek-v") {
        return KernelTier::Balanced;
    }

    // Unknown model: default to the literal-model (balanced) discipline.
    KernelTier::Balanced
}

/// Return the full writing kernel plus the correct per-tier addendum for `model_or_tier`.
///
/// This is the single seam every writing runner uses: it resolves its model from the project
/// tier map, then calls `kernel_for(model)` to get the kernel already specialized for that model's
/// tier. See section 5 of the plan.
pub fn kernel_for(model_or_tier: &str) -> String {
    format!("{}\n\n{}", GOVERNANCE_KERNEL, tier_of(model_or_tier).addendum())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernels_are_non_empty() {
        assert!(!GOVERNANCE_KERNEL.trim().is_empty());
        assert!(!GOVERNANCE_KERNEL_READONLY.trim().is_empty());
    }

    #[test]
    fn full_kernel_contains_section_markers_and_all_seven_clauses() {
        assert!(GOVERNANCE_KERNEL.contains("=== CAMERATA OPERATING PROTOCOL"));
        assert!(GOVERNANCE_KERNEL.contains("=== END OPERATING PROTOCOL ==="));
        // The seven imperative clauses.
        assert!(GOVERNANCE_KERNEL.contains("GROUND EVERY FACT"));
        assert!(GOVERNANCE_KERNEL.contains("PLAN, THEN ACT"));
        assert!(GOVERNANCE_KERNEL.contains("TESTS ARE PART OF THE CHANGE"));
        assert!(GOVERNANCE_KERNEL.contains("PROGRAM DEFENSIVELY"));
        assert!(GOVERNANCE_KERNEL.contains("VERIFY BEFORE DONE"));
        assert!(GOVERNANCE_KERNEL.contains("IF UNSURE, DO NOT GUESS"));
        assert!(GOVERNANCE_KERNEL.contains("REPORT IN CONTRACT FORM"));
    }

    #[test]
    fn readonly_kernel_contains_analysis_markers_and_clauses() {
        assert!(GOVERNANCE_KERNEL_READONLY.contains("=== CAMERATA OPERATING PROTOCOL (analysis) ==="));
        assert!(GOVERNANCE_KERNEL_READONLY.contains("=== END OPERATING PROTOCOL ==="));
        assert!(GOVERNANCE_KERNEL_READONLY.contains("GROUND EVERY CLAIM"));
        assert!(GOVERNANCE_KERNEL_READONLY.contains("ENUMERATE, THEN JUDGE"));
        assert!(GOVERNANCE_KERNEL_READONLY.contains("IF UNSURE, SAY SO"));
        assert!(GOVERNANCE_KERNEL_READONLY.contains("EXACT OUTPUT ONLY"));
    }

    #[test]
    fn no_dashes_in_kernel_text() {
        for text in [
            GOVERNANCE_KERNEL,
            GOVERNANCE_KERNEL_READONLY,
            KERNEL_ADDENDUM_FAST,
            KERNEL_ADDENDUM_BALANCED,
            KERNEL_ADDENDUM_STRONGEST,
        ] {
            assert!(!text.contains('\u{2014}'), "em-dash present in kernel text");
            assert!(!text.contains('\u{2013}'), "en-dash present in kernel text");
        }
    }

    #[test]
    fn tier_of_resolves_explicit_tier_names() {
        assert_eq!(tier_of("fast"), KernelTier::Fast);
        assert_eq!(tier_of("low"), KernelTier::Fast);
        assert_eq!(tier_of("balanced"), KernelTier::Balanced);
        assert_eq!(tier_of("mid"), KernelTier::Balanced);
        assert_eq!(tier_of("strongest"), KernelTier::Strongest);
        assert_eq!(tier_of("orchestration"), KernelTier::Strongest);
    }

    #[test]
    fn tier_of_resolves_model_ids_case_insensitively() {
        assert_eq!(tier_of("claude-opus-4-8"), KernelTier::Strongest);
        assert_eq!(tier_of("GLM-5.2"), KernelTier::Strongest);
        assert_eq!(tier_of("claude-3-5-haiku"), KernelTier::Fast);
        assert_eq!(tier_of("deepseek-flash"), KernelTier::Fast);
        assert_eq!(tier_of("us.anthropic.claude-3-5-sonnet"), KernelTier::Balanced);
        assert_eq!(tier_of("deepseek-pro"), KernelTier::Balanced);
    }

    #[test]
    fn tier_of_unknown_defaults_to_balanced() {
        assert_eq!(tier_of("some-unknown-model"), KernelTier::Balanced);
        assert_eq!(tier_of(""), KernelTier::Balanced);
    }

    #[test]
    fn kernel_for_appends_the_right_addendum_per_tier() {
        let fast = kernel_for("haiku");
        assert!(fast.contains(GOVERNANCE_KERNEL));
        assert!(fast.contains(KERNEL_ADDENDUM_FAST));
        assert!(!fast.contains(KERNEL_ADDENDUM_BALANCED));
        assert!(!fast.contains(KERNEL_ADDENDUM_STRONGEST));

        let balanced = kernel_for("sonnet");
        assert!(balanced.contains(GOVERNANCE_KERNEL));
        assert!(balanced.contains(KERNEL_ADDENDUM_BALANCED));
        assert!(!balanced.contains(KERNEL_ADDENDUM_FAST));

        let strongest = kernel_for("claude-opus-4-8");
        assert!(strongest.contains(GOVERNANCE_KERNEL));
        assert!(strongest.contains(KERNEL_ADDENDUM_STRONGEST));
        assert!(!strongest.contains(KERNEL_ADDENDUM_FAST));
    }
}
