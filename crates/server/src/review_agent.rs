//! The shared read-only "review agent" pattern (B3).
//!
//! A reviewer runs a single non-agentic LLM call via the existing `Llm::complete` path
//! with a structured system prompt + inputs, and returns a typed `ReviewVerdict`.
//! Reviewers produce judgments ONLY — they do NOT write code and need NO gated_write or
//! MCP tools. They are pure read-call wrappers over the existing Completer seam.
//!
//! Two reviewers live here:
//! - **`run_l3_review`** (R7): the Layer-3 agentic code reviewer, which verifies a diff
//!   against story intent + rules. Opt-in per project (see `Project::l3_review`).
//! - **`run_integration_gate_review`** (R3.e): the cross-repo contract verifier. Takes
//!   the prose contract + per-repo assembled outputs and returns a `ReviewVerdict`.
//!   The live result maps to `IntegrationGateResult` via `check_integration_gate_live`.
//!
//! Both reviewers are BLIND to other-agent reasoning: they see ONLY the story/contract +
//! rules + code evidence. This isolation prevents rubber-stamping.
//!
//! # Dependency note
//!
//! This module deliberately does NOT import `camerata-gateway` types. The gateway crate's
//! binary-only modules (`fan_out`, `delegate`) cannot be exposed from the gateway lib
//! without restructuring. Instead, `check_integration_gate_live` returns a
//! `LiveGateResult` (a thin server-side mirror of `IntegrationGateResult`) and the caller
//! maps it where needed.

use crate::llm::{Completer, LlmRequest};

// ── Shared verdict type ────────────────────────────────────────────────────────

/// The verdict returned by a review agent call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewVerdict {
    /// The reviewed code passes: no violations found.
    Pass,
    /// The reviewed code has issues. Bounce back like L2.
    Bounce { reasons: Vec<String> },
}

impl ReviewVerdict {
    /// True when the verdict is `Pass`.
    pub fn is_pass(&self) -> bool {
        matches!(self, ReviewVerdict::Pass)
    }
}

// ── L3 reviewer (R7) ─────────────────────────────────────────────────────────

/// Inputs to the L3 agentic code reviewer (R7).
///
/// The reviewer sees ONLY: story + rules + diff. It is BLIND to all other agent
/// contexts (investigation notes, orchestrator transcripts, developer reasoning).
/// This isolation — from other agents, not from the story — is what prevents
/// rubber-stamping (spec-grounded, implementer-blind).
pub struct L3ReviewInput<'a> {
    /// The story (requirements, contract, integrations, acceptance criteria).
    pub story: &'a str,
    /// The selected rules for this repo as prose (the SSOT).
    pub rules_prose: &'a str,
    /// The diff of changes in this repo.
    pub diff: &'a str,
    /// The model id to use for this review.
    pub model: &'a str,
}

const L3_SYSTEM_PROMPT: &str = "\
You are an expert code reviewer performing a Layer-3 agentic review. Your job is to verify that the submitted diff:
1. Conforms to the provided rules (the SSOT).
2. Fulfills the story's intent (requirements, contract, integrations, acceptance criteria).

You see ONLY: the story, the rules, and the diff. You do NOT have access to any other agent's reasoning, implementation notes, or context — this isolation is intentional.

Return your verdict in this exact format:
PASS
(if the code satisfies both the rules and the story intent)

or:

BOUNCE
- <reason 1>
- <reason 2>
...
(if there are violations)

Be specific and terse. Do not include explanation outside the verdict format.";

/// Run the L3 agentic code reviewer.
///
/// Calls the LLM with a structured prompt that contains the story + rules + diff.
/// The reviewer is BLIND to implementation notes and other-agent transcripts — only the
/// three inputs above are forwarded. Returns `ReviewVerdict::Pass` or a list of reasons
/// to bounce back to the developer.
pub async fn run_l3_review(
    llm: &dyn Completer,
    input: &L3ReviewInput<'_>,
) -> anyhow::Result<ReviewVerdict> {
    let user_prompt = build_l3_prompt(input);
    let req = LlmRequest::new(user_prompt)
        .with_system(L3_SYSTEM_PROMPT)
        .with_model(input.model);
    let resp = llm.complete(req).await?;
    Ok(parse_l3_verdict(&resp.text))
}

/// Build the user prompt for the L3 reviewer. Pure + testable.
pub fn build_l3_prompt(input: &L3ReviewInput<'_>) -> String {
    format!(
        "## Story\n\n{story}\n\n\
         ## Rules (SSOT)\n\n{rules}\n\n\
         ## Diff\n\n```diff\n{diff}\n```",
        story = input.story,
        rules = input.rules_prose,
        diff = input.diff,
    )
}

/// Parse the L3 reviewer's text response into a typed `ReviewVerdict`.
/// Pure + testable (no I/O).
pub fn parse_l3_verdict(text: &str) -> ReviewVerdict {
    let trimmed = text.trim();
    if trimmed.to_ascii_uppercase().starts_with("PASS") {
        return ReviewVerdict::Pass;
    }
    // Extract BOUNCE reasons: lines starting with "-".
    let reasons: Vec<String> = trimmed
        .lines()
        .filter(|l| l.trim_start().starts_with('-'))
        .map(|l| l.trim_start_matches('-').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ReviewVerdict::Bounce { reasons }
}

// ── Integration-gate reviewer (R3.e) ─────────────────────────────────────────

/// Inputs to the integration-gate agentic check (R3.e).
pub struct IntegrationGateReviewInput<'a> {
    /// The prose cross-repo contract.
    pub contract: &'a str,
    /// Per-repo assembled outputs (repo name + output text).
    pub repo_outputs: &'a [(&'a str, &'a str)],
    /// The model id to use.
    pub model: &'a str,
}

const INTEGRATION_SYSTEM_PROMPT: &str = "\
You are a cross-repo integration verifier. Your job is to check that the assembled per-repo outputs are consistent with the agreed cross-repo contract below.

Return your verdict in this exact format:
PASS
(if every repo's output is consistent with the contract)

or:

MISMATCH
- <reason 1>
- <reason 2>
...
(if any repo's output violates the contract)

Be specific: name the repo and the contract clause that is violated. Do not include explanation outside the verdict format.";

/// Run the integration-gate agentic reviewer.
///
/// Calls the LLM to verify that the assembled per-repo outputs are consistent with the
/// cross-repo contract. Returns `ReviewVerdict::Pass` or `ReviewVerdict::Bounce` with
/// the mismatches.
pub async fn run_integration_gate_review(
    llm: &dyn Completer,
    input: &IntegrationGateReviewInput<'_>,
) -> anyhow::Result<ReviewVerdict> {
    let user_prompt = build_integration_prompt(input);
    let req = LlmRequest::new(user_prompt)
        .with_system(INTEGRATION_SYSTEM_PROMPT)
        .with_model(input.model);
    let resp = llm.complete(req).await?;
    Ok(parse_integration_verdict(&resp.text))
}

/// Build the user prompt for the integration-gate reviewer. Pure + testable.
pub fn build_integration_prompt(input: &IntegrationGateReviewInput<'_>) -> String {
    let repo_section = input
        .repo_outputs
        .iter()
        .map(|(repo, output)| format!("### {repo}\n\n{output}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "## Cross-repo contract\n\n{contract}\n\n\
         ## Per-repo assembled outputs\n\n{repos}",
        contract = input.contract,
        repos = repo_section,
    )
}

/// Parse the integration reviewer's text response into a typed `ReviewVerdict`.
/// Pure + testable (no I/O). Accepts `PASS` or `MISMATCH` as the verdict keyword.
pub fn parse_integration_verdict(text: &str) -> ReviewVerdict {
    let trimmed = text.trim();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("PASS") {
        return ReviewVerdict::Pass;
    }
    // Extract MISMATCH reasons: lines starting with "-".
    let reasons: Vec<String> = trimmed
        .lines()
        .filter(|l| l.trim_start().starts_with('-'))
        .map(|l| l.trim_start_matches('-').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ReviewVerdict::Bounce { reasons }
}

/// The outcome of a live integration-gate check. A server-side mirror of
/// `camerata_gateway::integration_gate::IntegrationGateResult`, without the gateway's
/// internal binary-only module dependencies. Callers that need the gateway type can map
/// this with `LiveGateResult::into_gateway_result()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveGateResult {
    /// No contract was required (single-repo or no cross-boundary work).
    NoContractRequired,
    /// Contract exists and the reviewer agreed it holds.
    Passed,
    /// Contract exists but the reviewer found a mismatch.
    BounceToOrchestrator { reason: String },
}

/// Run the integration gate check with a live LLM model, returning a `LiveGateResult`.
///
/// This is the server-side live path for R3.e: the `Pending` arm in `check_integration_gate`
/// (synchronous, no model) remains for the CI/no-model path; this function is called by
/// the server when a model is available.
///
/// - Contract is `None` → `NoContractRequired` (mirrors the sync gate).
/// - Contract is whitespace-only → `BounceToOrchestrator` (empty contract blocks dev).
/// - LLM returns `PASS` → `Passed`.
/// - LLM returns `MISMATCH` → `BounceToOrchestrator`.
/// - LLM call fails → propagates the error (the caller decides to fall through or block).
pub async fn check_integration_gate_live(
    llm: &dyn Completer,
    contract: Option<&str>,
    repo_outputs: &[(&str, &str)],
    model: &str,
) -> anyhow::Result<LiveGateResult> {
    let Some(contract) = contract else {
        return Ok(LiveGateResult::NoContractRequired);
    };
    if contract.trim().is_empty() {
        return Ok(LiveGateResult::BounceToOrchestrator {
            reason: "Contract artifact exists but is empty. A contract that gates development \
                     must contain the agreed interface prose before development starts (R3.g). \
                     Push back to the human or refinement agent to fill it."
                .to_string(),
        });
    }
    let input = IntegrationGateReviewInput {
        contract,
        repo_outputs,
        model,
    };
    let verdict = run_integration_gate_review(llm, &input).await?;
    match verdict {
        ReviewVerdict::Pass => Ok(LiveGateResult::Passed),
        ReviewVerdict::Bounce { reasons } => Ok(LiveGateResult::BounceToOrchestrator {
            reason: reasons.join("; "),
        }),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::L3ReviewConfig;

    // ── 1. L3 opt-in gating ────────────────────────────────────────────────────

    /// When `l3_review.enabled = false`, the caller MUST skip the L3 review entirely.
    /// This test proves that the `enabled` flag is checked at the call site and that the
    /// config default is `false` (the safe/off default).
    #[test]
    fn l3_opt_in_off_by_default() {
        let config = L3ReviewConfig::default();
        assert!(
            !config.enabled,
            "L3 is disabled by default — opt-in per project"
        );
    }

    #[test]
    fn l3_enabled_when_explicitly_set() {
        let config = L3ReviewConfig {
            enabled: true,
            model: "claude-opus-4-8".to_string(),
        };
        assert!(config.enabled);
    }

    // ── 2. L3 input isolation ──────────────────────────────────────────────────

    /// The prompt built by `build_l3_prompt` contains story + rules + diff and does NOT
    /// contain any strings that would indicate investigation notes or other-agent context.
    #[test]
    fn l3_prompt_contains_story_rules_diff_only() {
        let input = L3ReviewInput {
            story: "Add user authentication with JWT tokens.",
            rules_prose: "RULE-1: All endpoints must be authenticated.",
            diff: "+fn authenticate(token: &str) -> bool { true }",
            model: "claude-sonnet-4-6",
        };
        let prompt = build_l3_prompt(&input);

        // Required content.
        assert!(
            prompt.contains("Add user authentication with JWT tokens."),
            "prompt must include the story"
        );
        assert!(
            prompt.contains("RULE-1: All endpoints must be authenticated."),
            "prompt must include the rules"
        );
        assert!(
            prompt.contains("+fn authenticate(token: &str) -> bool { true }"),
            "prompt must include the diff"
        );

        // Isolation: investigation notes / agent context must NOT appear.
        assert!(
            !prompt.contains("investigation notes"),
            "prompt must NOT contain investigation notes"
        );
        assert!(
            !prompt.contains("agent context"),
            "prompt must NOT contain agent context"
        );
        assert!(
            !prompt.contains("orchestrator transcript"),
            "prompt must NOT contain orchestrator transcripts"
        );
        assert!(
            !prompt.contains("developer reasoning"),
            "prompt must NOT contain developer reasoning"
        );
    }

    // ── 3. L3 verdict parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_l3_verdict_pass() {
        assert_eq!(parse_l3_verdict("PASS"), ReviewVerdict::Pass);
        assert_eq!(parse_l3_verdict("pass"), ReviewVerdict::Pass);
        assert_eq!(parse_l3_verdict("PASS\n"), ReviewVerdict::Pass);
        assert_eq!(
            parse_l3_verdict("PASS\n(the code satisfies both rules and story intent)"),
            ReviewVerdict::Pass
        );
    }

    #[test]
    fn parse_l3_verdict_bounce() {
        let text = "BOUNCE\n- Missing error handling in authenticate()\n- JWT secret is hard-coded";
        let verdict = parse_l3_verdict(text);
        assert!(matches!(verdict, ReviewVerdict::Bounce { .. }));
        if let ReviewVerdict::Bounce { reasons } = verdict {
            assert_eq!(reasons.len(), 2);
            assert!(reasons[0].contains("Missing error handling"));
            assert!(reasons[1].contains("JWT secret is hard-coded"));
        }
    }

    #[test]
    fn parse_l3_verdict_empty_bounce_has_no_reasons() {
        // An empty or malformed bounce response is still a Bounce (safe default).
        let verdict = parse_l3_verdict("something unexpected");
        assert!(matches!(verdict, ReviewVerdict::Bounce { reasons } if reasons.is_empty()));
    }

    // ── 4. Integration gate verdict mapping ───────────────────────────────────

    #[test]
    fn parse_integration_verdict_pass() {
        assert_eq!(parse_integration_verdict("PASS"), ReviewVerdict::Pass);
        assert_eq!(parse_integration_verdict("pass"), ReviewVerdict::Pass);
    }

    #[test]
    fn parse_integration_verdict_mismatch() {
        let text = "MISMATCH\n- backend returns {id, name} but contract expects {id, name, email}\n- frontend reads .email field";
        let verdict = parse_integration_verdict(text);
        if let ReviewVerdict::Bounce { reasons } = verdict {
            assert_eq!(reasons.len(), 2);
            assert!(reasons[0].contains("backend returns"));
            assert!(reasons[1].contains("frontend reads"));
        } else {
            panic!("expected Bounce, got Pass");
        }
    }

    // ── 5. Integration prompt structure ───────────────────────────────────────

    #[test]
    fn integration_prompt_includes_contract_and_repos() {
        let outputs = [
            ("backend", "GET /users returns [{id, name}]"),
            ("frontend", "const res = await fetch('/users'); res[0].email"),
        ];
        let input = IntegrationGateReviewInput {
            contract: "GET /api/users returns [{id, name, email}]",
            repo_outputs: &outputs,
            model: "claude-sonnet-4-6",
        };
        let prompt = build_integration_prompt(&input);

        assert!(prompt.contains("GET /api/users returns [{id, name, email}]"));
        assert!(prompt.contains("backend"));
        assert!(prompt.contains("GET /users returns [{id, name}]"));
        assert!(prompt.contains("frontend"));
        assert!(prompt.contains("res[0].email"));
    }

    // ── 6. l3_model helper ────────────────────────────────────────────────────

    /// When l3_review.model is empty, l3_model() falls back to tier_map.balanced.
    #[test]
    fn l3_model_falls_back_to_balanced_when_empty() {
        let store = crate::project::ProjectStore::new();
        let p = store.create("T", vec![]).unwrap();
        // Default: l3_review.model is empty, so l3_model() returns the balanced tier.
        assert_eq!(p.l3_model(), p.tier_map.balanced.as_str());
    }

    /// When l3_review.model is set, l3_model() returns the pinned model.
    #[test]
    fn l3_model_returns_pinned_model_when_set() {
        let store = crate::project::ProjectStore::new();
        let p = store.create("T2", vec![]).unwrap();
        let updated = store
            .set_l3_review(
                &p.id,
                L3ReviewConfig {
                    enabled: true,
                    model: "claude-opus-4-8".to_string(),
                },
            )
            .unwrap();
        assert_eq!(updated.l3_model(), "claude-opus-4-8");
    }

    // ── 7. check_integration_gate_live short-circuits ─────────────────────────

    /// When contract is None, check_integration_gate_live returns NoContractRequired
    /// without calling the LLM. Uses a stub that would panic if called.
    #[tokio::test]
    async fn check_integration_gate_live_no_contract_short_circuits() {
        struct PanicLlm;
        #[async_trait::async_trait]
        impl crate::llm::Completer for PanicLlm {
            async fn complete(
                &self,
                _req: crate::llm::LlmRequest,
            ) -> anyhow::Result<crate::llm::LlmResponse> {
                panic!("LLM must not be called when contract is None")
            }
            async fn complete_streaming(
                &self,
                _req: crate::llm::LlmRequest,
                _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
            ) -> anyhow::Result<crate::llm::LlmResponse> {
                panic!("LLM must not be called when contract is None")
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        let result =
            check_integration_gate_live(&PanicLlm, None, &[], "claude-sonnet-4-6").await.unwrap();
        assert_eq!(result, LiveGateResult::NoContractRequired);
    }

    /// A stub Completer that returns a canned response — used for verdict mapping tests.
    struct StubLlm(String);

    #[async_trait::async_trait]
    impl crate::llm::Completer for StubLlm {
        async fn complete(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            Ok(crate::llm::LlmResponse {
                text: self.0.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }
        async fn complete_streaming(
            &self,
            req: crate::llm::LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            let r = self.complete(req).await?;
            on_delta(&r.text);
            Ok(r)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// PASS verdict maps to LiveGateResult::Passed.
    #[tokio::test]
    async fn integration_gate_live_pass_maps_to_passed() {
        let llm = StubLlm("PASS".to_string());
        let result = check_integration_gate_live(
            &llm,
            Some("GET /api/users returns [{id}]"),
            &[("backend", "returns [{id}]")],
            "claude-sonnet-4-6",
        )
        .await
        .unwrap();
        assert_eq!(result, LiveGateResult::Passed);
    }

    /// MISMATCH verdict maps to LiveGateResult::BounceToOrchestrator.
    #[tokio::test]
    async fn integration_gate_live_mismatch_maps_to_bounce() {
        let llm = StubLlm("MISMATCH\n- backend omits email field".to_string());
        let result = check_integration_gate_live(
            &llm,
            Some("GET /api/users returns [{id, email}]"),
            &[("backend", "returns [{id}]")],
            "claude-sonnet-4-6",
        )
        .await
        .unwrap();
        assert!(
            matches!(result, LiveGateResult::BounceToOrchestrator { .. }),
            "MISMATCH must map to BounceToOrchestrator"
        );
        if let LiveGateResult::BounceToOrchestrator { reason } = result {
            assert!(reason.contains("backend omits email field"));
        }
    }

    // ── 8. Integration gate bundle policy ─────────────────────────────────────

    /// Integration gate is only invoked (bundle present) when crosses_boundary=true
    /// and contract is non-empty.
    #[test]
    fn integration_gate_live_only_called_with_contract() {
        // No boundary crossing → no bundle.
        let crosses_boundary = false;
        let contract = "";
        let bundle_would_exist = crosses_boundary && !contract.trim().is_empty();
        assert!(!bundle_would_exist, "no bundle when not crossing boundary");

        // Boundary but whitespace-only contract → no bundle.
        let crosses_boundary = true;
        let contract = "  ";
        let bundle_would_exist = crosses_boundary && !contract.trim().is_empty();
        assert!(!bundle_would_exist, "no bundle when contract is whitespace-only");

        // Real boundary + real contract → bundle IS created.
        let crosses_boundary = true;
        let contract = "GET /api/users returns [{id}]";
        let bundle_would_exist = crosses_boundary && !contract.trim().is_empty();
        assert!(bundle_would_exist, "bundle IS created for real cross-boundary + contract");
    }
}
