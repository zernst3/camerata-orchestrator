# Provider Neutrality

> Audience: technical evaluators and collaborators who have read POSITIONING.md
> and want to verify the moat claim with code pointers, not adjectives.

## The claim

POSITIONING.md states that Camerata's governance gate is "provider-neutral by
construction." This document turns that assertion into a demonstrated, tested
artifact.

## Why it holds structurally

Provider neutrality is not a promise the code keeps by convention. It is a
consequence of two structural facts that cannot be undone without rewriting the
seam contracts.

### Fact 1: the AgentDriver seam

The coordinator and fleet coordinator depend on the `AgentDriver` trait
(`crates/core/src/lib.rs`):

```
pub trait AgentDriver: Send + Sync {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome>;
}
```

The `Coordinator` and `FleetCoordinator` hold `&dyn AgentDriver`. They call
`driver.run(role, task)` and receive an `AgentOutcome`. There is no model field,
no provider enum, no Claude-specific type anywhere in `crates/core`. The
coordinator is structurally incapable of discriminating by provider.

### Fact 2: the gateway decides on the ToolCall, not the model

`camerata_gateway::evaluate_call` (`crates/gateway/src/lib.rs`) has this
signature:

```
pub fn evaluate_call(rule_subset: &[RuleId], call: &ToolCall) -> Decision
```

The inputs are a rule-subset (derived from the `Role`, assigned at spawn time,
before any model runs) and a `ToolCall` (tool name, target path, content). There
is no model parameter. The function has no place to receive provider information
even if a caller tried to pass it. The same `(rule_subset, call)` always yields
the same `Decision`, regardless of which agent produced the call.

## The evidence: two concrete artifacts

### 1. GenericCliDriver (`crates/agent/src/generic.rs`)

`GenericCliDriver` implements `AgentDriver` with zero code shared with
`ClaudeCliDriver` beyond the trait. Its `build_args` produces an argv for any
command-line agent (`program` is caller-supplied, e.g. `"llm"` or `"aider"`).
It carries none of the Claude-specific flags: no `--strict-mcp-config`, no
`--dangerously-skip-permissions`, no `--allowedTools`. The coordinator and
gateway cannot tell the difference at runtime, because neither receives the
driver's concrete type.

### 2. The proof test (`crates/core/tests/provider_neutrality.rs`)

Three assertions, one file:

**Proof 1** (`fleet_coordinator_governs_fake_non_claude_driver_identically`):
A `FakeProviderXDriver` (implementing `AgentDriver` with zero Claude code) is
passed to a `FleetCoordinator`. The coordinator runs a two-stage pipeline, fires
the bounce-and-revise pass on a scripted violation, and produces a `FleetReport`
with the same shape and semantics as any Claude-driver run. The coordinator never
knew which provider ran.

**Proof 2** (`generic_cli_driver_build_args_contains_no_claude_flags`):
`GenericCliDriver::build_args` with `program = "llm"` and
`task_flag = "--prompt"` produces an argv with no Claude-specific flags. The
runtime is not hard-wired to the Claude CLI binary.

**Proof 3** (`evaluate_call_has_no_provider_input_and_decides_on_tool_call_alone`):
`evaluate_call` is called twice with identical rule-subsets, once labeled as
"Claude-backed" and once as "generic-provider-backed." The decisions are
identical: the forbidden write is denied both times; the clean write is allowed
both times. The gate has no provider input to discriminate on.

## Honest limits

The gate is neutral. The output quality of the agent running behind it is not.
A given model may generate better or worse code than another, and layer-2
(the `CheckRunner` that runs `cargo fmt --check`, `cargo clippy`, and similar
structural gates after each agent task) catches quality failures regardless of
which model produced them. But "passes clippy" is a lower bound, not a ceiling.
Layer-2 + bounce-and-revise is the mechanism that makes the floor provider-
neutral; it does not make every provider equally skilled above that floor.

That distinction matters for the moat argument. Provider neutrality is the
property that prevents the governance layer from becoming a single-vendor
dependency. It is not a claim that all models are equivalent.

## Tie-back to POSITIONING.md

POSITIONING.md identifies provider neutrality as the second tier-1 moat item:
"A model vendor's guardrail governs THAT vendor's agents. Camerata's gate is
provider-neutral by design (an MCP tool-gateway plus an agent-runtime seam, so
a non-Claude model swaps in without touching the gate)."

The `AgentDriver` trait is the agent-runtime seam. `GenericCliDriver` is the
first non-Claude implementation. The proof test in
`crates/core/tests/provider_neutrality.rs` is the executable demonstration.
