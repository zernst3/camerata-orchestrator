# Camerata Orchestrator: TECH_DESIGN.md

Status: Phase 0 design. This document answers the six VISION section 16 investigation
questions, states the chosen architecture, gives the module layout for the
TypeScript/Node orchestrator, and ends with an explicit "Unverified assumptions and
open risks" register.

This is decision-first. Every question is answered as: Recommendation, Why,
Alternatives considered. Where a recommendation rested on a claim that adversarial
verification marked refuted or uncertain, the recommendation is downgraded in place
and the fallback is adopted. The downgrades are called out plainly, not buried.

Verification anchor date: 2026-06-13. The Claude Agent SDK auth model, the June 15
2026 billing change, and current model pricing were re-checked against primary and
near-primary sources during this design pass (see the risk register for what could
and could not be confirmed).

---

## 0. Decision summary (read this first)

| # | Question | Decision | Survived verification? |
|---|----------|----------|------------------------|
| Q1 | Headless on Max subscription, no API key? | NO single free path. Two sanctioned paths behind the `session.ts` seam: (A, primary) metered `ANTHROPIC_API_KEY`, subscription Agent SDK credit auto-applies, overflow metered; (B, solo-dogfood alt) Claude Code CLI `claude -p` under the operator's own subscription OAuth. Post June 15 2026 both draw from the SAME monthly Agent SDK credit ($100/mo on the operator's Max 5x). | OAuth-in-a-product REFUTED; API-key primary + CLI-for-solo added |
| Q2 | PreToolUse hooks as the real-time gate? | YES, behind a provider-neutral `GovernanceGateway` seam. Claude PreToolUse binding ships first (deny + `systemMessage`, not just `permissionDecisionReason`); the MCP tool-gateway binding is the model-agnostic path and closes the subagent gap. Phase 0 spawns NO subagents. | CONFIRMED with two corrections + agnostic seam added |
| Q3 | Post-task structural checks mechanism | Pluggable per-language `CheckRunner` that shells to native linters first (reuse Agora's ESLint rule), ts-morph as TS fast path. | CONFIRMED |
| Q4 | Git worktree isolation flow | `git worktree add`, set SDK `cwd` (not `workingDirectory`) to the worktree, merge in dependency order, manual cleanup. Phase 0 runs the two agents SEQUENTIALLY, not in parallel. | CONFIRMED with two corrections |
| Q5 | Rule index schema + mechanical-vs-review split | 6-field index derived mechanically from the TOML. Bucket on `enforcement` alone. Add a THIRD runtime state: mechanical-but-no-shipping-check degrades to review. | CONFIRMED core; one sub-claim REFUTED |
| Q6 | Onboarding greenfield vs brownfield | Greenfield scaffolds rules from commit zero; brownfield maps + proposes the should-be RuleSet AND INSTALLS the governance scaffolding (lint config, CI gate steps, agent rules, hooks) as a human-approved diff. Onboarding installs what the repo SHOULD have, it does not merely detect what it has. New-rule synthesis deferred. | CONFIRMED + install scope clarified |

Two corrections ripple through the whole design and are stated once here:

1. **The Max-subscription "no metered API" thesis did not survive as stated, but a
   subscription path remains for solo use.** OAuth tokens cannot be used with the Agent
   SDK in a third-party *product* (Anthropic Consumer Terms; the prohibition is about
   routing other users' work through subscription credentials). So the PRIMARY path is
   an `ANTHROPIC_API_KEY`, against which the monthly Agent SDK credit auto-applies. BUT
   the subscriber driving the **Claude Code CLI (`claude -p`) for their own work** is
   permitted "ordinary use of Claude Code," and post June 15 2026 it draws from the same
   Agent SDK credit. So Phase 0 supports BOTH behind `agents/session.ts`: API-key
   (primary, product-ready) and CLI-OAuth (solo-dogfood alternative). The operator is on
   **Max 5x = $100/mo credit** (not $200 Max 20x). Cost stays inside the credit for a
   thin-slice cadence but NOT a daily one (see Q1 cost model); the "cost-safe" spirit
   holds at thin-slice scale. The single-free-path mechanism in VISION section 3 /
   section 13 is wrong and is corrected below.

2. **The Agora ESLint rule that anchors the planted-violation gate runs locally but NOT
   in CI today.** Verified directly: `.github/workflows/build-and-deploy-api.yml` runs
   only `npx tsc --noEmit` for its "Lint and Type Check" step (line 176-177); it never
   invokes ESLint. This does not break Phase 0 (the orchestrator invokes the linter
   itself, it does not rely on Agora's CI), but it means the rule is NOT "enforced on
   every PR" as some workstream prose claimed. A one-line CI fix is recommended
   separately and is not a Phase 0 dependency.

---

## 1. Chosen architecture

Three layers, one of which (the orchestrator) makes ZERO LLM calls.

```
+--------------------------------------------------------------+
|  ORCHESTRATION + GOVERNANCE LAYER  (deterministic TS/Node)   |
|  - Intake, investigation driver, rule-selection post-proc    |
|  - Coordinator: task DAG, worktrees, integration order       |
|  - Two-layer gate: PreToolUse hook wiring + post-task checks  |
|  - Persistence (SQLite/flat files), provenance, status        |
|  *** MAKES NO MODEL CALLS ITSELF ***                          |
+----------------------------+---------------------------------+
                             | spawns + wires
                             v
+--------------------------------------------------------------+
|  AGENT LAYER  (Claude Agent SDK sessions, one per Role)      |
|  - Each = query() session, scoped cwd (worktree),           |
|    allowed_tools, permission mode, injected rule subset      |
|  - GovernanceGateway attached (layer 1): Claude PreToolUse   |
|    binding now, MCP tool-gateway binding for model-agnostic  |
|  - Auth via session.ts seam: (A) ANTHROPIC_API_KEY primary,  |
|    (B) claude -p subscription OAuth for solo dogfood          |
|  - ALL model calls happen here, inside the session           |
+----------------------------+---------------------------------+
                             | produces diff on a branch
                             v
+--------------------------------------------------------------+
|  POST-TASK GATE  (deterministic checks, layer 2)            |
|  - Pluggable CheckRunner per language                        |
|  - Reuse native linters (ESLint / clippy), ts-morph fast path|
|  - On fail: bounce rule_id + message back to the agent       |
+--------------------------------------------------------------+
```

The two-layer gate (VISION section 10):

- **Layer 1, real-time:** Agent SDK `PreToolUse` hooks deny tool calls that breach a
  hard boundary (write outside `path_boundaries`, a raw DB command from the Frontend
  role) before the call executes. Cheap, immediate, no post-hoc revision needed.
- **Layer 2, post-task:** after a Task produces a diff, run the deterministic checks
  for its active mechanical rules (ESLint, ts-morph, build, tests). On any fail, the
  diff is bounced back to the agent with the specific violated rule id and message.

Data model: VISION section 8 entities are adopted verbatim (Story, Investigation,
Rule, RuleSet, Role, Task, Gate/Check, Provenance, FeatureStatus). The only schema
refinement is on `Rule` (see Q5): the runtime needs a THIRD `enforcement_kind` state to
distinguish "mechanical and a check exists" from "mechanical by declaration but no
shipping check."

---

## 2. Q1: Can the Agent SDK run headless on a Max subscription, spawn concurrent sessions, and what are the rate-limit ceilings?

### Recommendation (DOWNGRADED from the VISION assumption; TWO sanctioned paths)

Abstract authentication behind the single `agents/session.ts` seam and support two
sanctioned paths, chosen per run, not one.

- **Path A (PRIMARY): metered `ANTHROPIC_API_KEY`** from the Anthropic console. The
  subscription's separate monthly Agent SDK credit auto-applies to API-key usage
  (effective June 15 2026); once the credit is exhausted, overflow bills at metered
  rates. This is the only path that is also compliant for the eventual multi-user
  product (per-user API keys), and it gives the orchestrator clean, programmatic
  per-role hooks via the SDK. Use it as the default.
- **Path B (SOLO-DOGFOOD ALTERNATIVE): drive the Claude Code CLI headlessly
  (`claude -p`) under the operator's own subscription OAuth.** Permitted as "ordinary
  use of Claude Code" by the subscriber for their own work. Post June 15 2026, `claude
  -p` usage draws from the SAME monthly Agent SDK credit as Path A, so the two are
  economically equivalent inside the credit pool; the difference is integration shape
  and compliance scope, not cost. Its only advantages: no API-key provisioning, and it
  sits most squarely inside permitted personal use. Its costs: the orchestrator must
  spawn a subprocess and parse `--output-format stream-json`, and the layer-1 gate is
  configured as `.claude/settings.json` hook shell-commands per worktree rather than as
  programmatic `PreToolUse` callbacks (less dynamic, harder to inject a per-role
  `rule_subset`). It does NOT extend to the product: routing other users' work through
  subscription credentials is explicitly prohibited (Consumer Terms; see Alternatives).

For Phase 0, run the two role agents **sequentially**, not concurrently, which sidesteps
the entire rate-limit / lock-contention question for the thin slice regardless of path.

**Credit correction:** the operator is on **Max 5x, so the operative monthly Agent SDK
credit is $100**, not the $200 Max 20x figure used in earlier drafts (the per-plan pool
is $20 Pro / $100 Max 5x / $200 Max 20x). The cost model below is recomputed against
$100.

### Why

The original VISION section 3 / section 13 thesis ("Agent SDK headless on Max
subscription OAuth, no metered API key") **did not survive verification** and is
withdrawn. Confirmed against current Anthropic documentation and the June 2 2026
billing announcement:

- OAuth tokens obtained through a Free, Pro, or Max account may not be used with the
  Agent SDK in any other product, tool, or service. That is a Consumer Terms
  violation, and there are documented account-lockout enforcement cases. A
  TypeScript/Node orchestrator that drives Agent SDK sessions is exactly such a
  product.
- Effective June 15 2026, Agent SDK and `claude -p` usage on subscription plans draw
  from a NEW separate monthly credit pool ($20 Pro / $100 Max 5x / $200 Max 20x),
  distinct from interactive limits. Critically, that credit is consumed by **API-key**
  Agent SDK usage tied to the subscription, not by OAuth pass-through. So the correct
  mechanism is: create an API key, set `ANTHROPIC_API_KEY`, and the Max credit absorbs
  the spend until exhausted, after which standard metered rates apply.

This preserves the practical goal (no surprise metered-API bill for the user running it) for the Phase 0
scope: the thin slice (one story, two agents, a handful of investigative calls) is well
inside the monthly credit pool. The honest correction is only to the **mechanism**, not
to the cost outcome at Phase 0 scale.

On concurrency and rate limits: the verdicts conflicted, and the one durable conclusion
is that the convenient "Max 20x unlocks Tier 3 (2,000 RPM)" claim is **not established**.
Subscription credit and API tier advancement are decoupled; tier advancement is driven
by cumulative console deposit history, which the subscription credit does not
necessarily satisfy. Rather than design Phase 0 around an unverified tier ceiling, Phase
0 runs agents sequentially (Backend to completion, then Frontend), which the acceptance
criteria (two agents in two worktrees) permit. Parallelism is a P2 concern and is
explicitly deferred until the real throughput ceiling is measured.

### Cost model (corrected)

Phase 0 runs Opus 4.8 for agent sessions where reasoning depth matters; subordinate /
mechanical passes can use a cheaper tier. **Opus 4.8 standard rates are $5/1M input and
$25/1M output** (verified). The earlier workstream figure of $3/$15 was Sonnet 4.6 and
is corrected here. A thin-slice run (two agents, investigation + execution + one bounce
cycle) at roughly 500k input + 200k output tokens costs about (500k x $5 + 200k x $25) /
1M = $2.50 + $5.00 = ~$7.50 per full run. Against the operator's **$100 Max 5x** credit
that is ~13 full runs/month inside the credit. A daily cadence (~30 runs/mo) would
exhaust the $100 credit and spill to metered overflow on Path A (or hit the credit wall
on Path B), so set a console spend cap as a safety valve and do not assume a free daily
cadence on Max 5x. The earlier "comfortably daily on $200 Max 20x" claim was for the
wrong plan and is corrected.

### Alternatives considered

- **Agent SDK driven by Max-subscription OAuth tokens (the original VISION assumption).**
  Rejected and PROHIBITED. Consumer Terms forbid third-party developers from routing
  requests through Free/Pro/Max plan credentials on behalf of users, and from offering
  Claude.ai login in their product; documented account-lockout enforcement exists. This
  is distinct from Path B above: Path B is the *subscriber* using the *Claude Code CLI*
  for *their own* work (permitted "ordinary use of Claude Code"); the prohibited thing
  is the *Agent SDK* in a *product* passing OAuth tokens on behalf of *other* users. The
  line is who the work is for and which tool authenticates, not OAuth-vs-API-key alone.
- **Claude Code CLI (`claude -p`) under subscription OAuth.** ADOPTED as Path B, the
  solo-dogfood alternative (see Recommendation). Documented here because it is the path
  the earlier review dismissed by conflating it with the prohibited SDK-OAuth-product
  case; the ToS treats subscriber CLI use as permitted and, post June 15 2026, meters it
  against the same Agent SDK credit.
- **Proxy that injects the API key via `ANTHROPIC_BASE_URL`.** Viable, adds a gateway
  to harden; unnecessary for a single-user Phase 0. Defer.
- **Workload Identity Federation via an org service account.** Highest security
  posture, requires console org setup not available at Pro/Max self-service tier.
  Defer to product stage.

---

## 3. Q2: Do Agent SDK hooks reliably reject a tool call (PreToolUse) to serve as the real-time gate?

### Recommendation

Put the layer-1 real-time gate behind a **provider-neutral `GovernanceGateway`
interface** (the gate-layer analogue of the `session.ts` auth/model seam) so
model-agnosticism lives at the gate layer too, then implement two bindings:

- **Claude `PreToolUse` binding (Phase 0, ships first).** Return
  `hookSpecificOutput.permissionDecision = "deny"` to hard-block the call, AND **also emit
  a top-level `systemMessage`** carrying the rule id and reason so the model reliably sees
  why it was blocked. For Phase 0, spawn NO subagents, which avoids the one verified
  reliability gap.
- **MCP tool-gateway binding (the model-agnostic path, built later).** Expose the agent's
  tools through a Camerata-owned MCP server; the agent can act ONLY through those gated
  tools, and the gateway validates every call in-process before executing it, regardless
  of provider (Claude / Gemini / Codex). Because MCP is becoming the cross-provider tool
  standard, this makes the real-time gate portable across models AND closes the
  subagent-deny gap (nested calls also route through the gateway). This is the same idea as
  the "custom tool wrapper" alternative below, promoted to the agnostic primary once
  non-Claude providers are in scope.

The gate LOGIC (rule -> allow/deny) is written against the `GovernanceGateway` interface
and MUST NOT assume Claude hooks. The PreToolUse hook is one binding, not the gate.
Model-agnostic enforcement therefore lives at all layers: `session.ts` is agnostic for
generation, and `GovernanceGateway` is agnostic for the real-time gate (Claude binding
strongest first; MCP-gateway binding for full cross-model fidelity).

### Why

Confirmed against the official Agent SDK hooks documentation:

- A `PreToolUse` hook fires before the tool executes and `permissionDecision: "deny"`
  blocks the call unconditionally. Deny takes priority over all other hook decisions.
  This is exactly the layer-1 behavior VISION section 10.1 needs: a Frontend agent
  attempting a raw DB command hits the deny in real time and never executes it.

Two corrections from verification, both folded into the recommendation:

1. **`permissionDecisionReason` is NOT guaranteed to reach the model's context.** It is
   primarily audit metadata. The model-visible channel is a top-level `systemMessage`
   (and/or `additionalContext`). So the deny payload must include a `systemMessage` with
   the rule id and an actionable reason, or the agent may not understand how to
   reformulate. This is a one-line addition per hook, no architectural change.
2. **Subagent tool calls are a known gap.** A documented, unfixed issue means
   `PreToolUse` deny can be ignored for tool calls made by spawned subagents. Phase 0
   does not use subagents (two flat role sessions), so this does not bite Phase 0. It IS
   a one-way-door design constraint for later phases: any phase that lets agents spawn
   subagents must NOT rely solely on `PreToolUse` for those nested calls, and should add
   a layer-2 post-task backstop for them.

There are also scattered reports of `PreToolUse` deny being ignored for some MCP tools
and under parallel-tool-call races. Phase 0 mitigates both by (a) not registering MCP
tools the agents do not need, and (b) running agents sequentially. The layer-2 post-task
gate is the backstop regardless: even if a real-time deny is ever bypassed, the
post-task structural check still catches the violation in the produced diff. The gate is
deliberately defense-in-depth, not a single point of failure.

### Concrete hook (planted-violation path)

```ts
// PreToolUse hook attached to the Frontend role session.
function frontendPreToolUse(input: PreToolUseInput): PreToolUseResult {
  const { toolName, toolInput } = input;

  // Hard boundary 1: no raw DB commands from the Frontend role.
  if (toolName === "Bash" && /\b(psql|pg_dump)\b/.test(toolInput.command ?? "")) {
    return {
      // model-visible channel (the correction):
      systemMessage:
        "Blocked: ROLE-PATH-BOUNDARY-FE-1. Frontend cannot run raw database " +
        "commands. Use the Backend API instead.",
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: "FE role raw-DB boundary (ROLE-PATH-BOUNDARY-FE-1)",
      },
    };
  }

  // Hard boundary 2: writes confined to the role's path_boundaries.
  if ((toolName === "Write" || toolName === "Edit") &&
      !withinGlobs(toolInput.file_path, role.path_boundaries)) {
    return {
      systemMessage:
        `Blocked: ROLE-PATH-BOUNDARY-FE-1. Frontend may only write under ` +
        `${role.path_boundaries.join(", ")}.`,
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: "write outside path_boundaries",
      },
    };
  }

  return {}; // allow
}
```

### Alternatives considered

- **Post-hoc `PostToolUse` rejection only.** Reliable across primary and subagent
  contexts, but doubles latency and loses the real-time property. Kept as the layer-2
  backstop, not the primary real-time mechanism.
- **Custom tool wrapper (`governance_execute_tool(name, args)`).** Agents call a
  wrapper that validates in-process, sidestepping hook reliability quirks entirely.
  Strong fallback if hook reliability proves worse than documented in practice, but it
  changes the agent tool interface and is more build for Phase 0. Hold in reserve.
- **Blanket `allowed_tools` narrowing at session setup.** Reduces capability but is not
  rule-based governance. Used as a coarse complement (the DB role gets migration tools
  only), not a replacement for the rule gate.

---

## 4. Q3: Best mechanism for post-task structural checks (the "DB access only via repository layer" class)

### Recommendation

A **pluggable per-language `CheckRunner` interface** that runs **Camerata's canonical
checks (the "should-be" state)**, not whatever the target repo happens to already have.
The canonical checks are owned by Camerata / sourced from the corpus and INSTALLED into
the repo during onboarding (Q6); the runner then shells the native linter (e.g.
`eslint --format json`) against that installed config and parses the output. **Reuse of a
repo's pre-existing check is opportunistic, not the foundation:** Camerata does not assume
the target already enforces anything. A drifted or bare repo is the normal case (Agora
itself drifted), and bringing it to the should-be state is the entire point, so anchoring
on "what Agora already ships" is explicitly NOT the path. Provide a ts-morph AST fast path
for TypeScript-specific rules the native linter cannot express.

### Why

Confirmed: Agora's `apps/api/eslint.config.mjs` defines `layeredArchRule` with the
selector
`CallExpression[callee.type='MemberExpression'][callee.object.name='db'][callee.property.name=/^(insert|update|delete|select|selectDistinct|execute)$/]`
and blocks it outside `repositories/` and `ingest/`. This is a real, mature,
production-proven check. It is a useful STARTING POINT for the canonical check (the
directive is real and the error message is actionable), but the should-be path is that
Camerata OWNS this check and installs it, so it cannot be silently dropped by repo drift.

The verification finding now MOTIVATES the should-be path rather than complicating it:
this rule **does not run in Agora CI today** (the API workflow runs only `tsc --noEmit`).
That is exactly the drift Camerata exists to fix. The orchestrator invokes the linter
itself in the worktree at task completion (so Phase 0 never depends on Agora's CI), AND
onboarding (Q6 / T13) INSTALLS the missing CI gate step (`npm run lint`) as part of
bringing the repo to the should-be state. So "Agora CI does not enforce this" is not a
caveat to route around; it is the precise condition the onboarding install corrects.

### End-to-end check (ARCH-STRICT-LAYERING-1)

1. Agent commits a diff on `task-frontend-ui`, including a planted direct `db.select(...)`
   in a service file.
2. Coordinator detects the language (package.json + eslint.config.mjs present in the
   worktree) and selects `TypeScriptCheckRunner`.
3. Runner executes `npx eslint . --format json` in the worktree.
4. Parse the JSON. The violation surfaces as ESLint rule id `no-restricted-syntax` at a
   file/line.
5. Map `no-restricted-syntax` (with the layering message signature) to the Camerata id
   `ARCH-STRICT-LAYERING-1` via a small `lint-rule-map.ts`.
6. Filter to violations whose Camerata id is in the active RuleSet.
7. Bounce back to the agent: rule id, file:line, the rule message, and a fix
   suggestion ("move the query into a repository, call it from the service").
8. Agent revises, resubmits; re-run the check; integrate on pass.

### Pluggable interface

```ts
interface LanguageCheckRunner {
  language: string;
  applies(repoPath: string): Promise<boolean>;
  run(repoPath: string, activeRuleIds: string[]): Promise<CheckViolation[]>;
}

interface CheckViolation {
  filePath: string;
  line: number;
  column: number;
  ruleId: string;        // mapped to a Camerata rule id
  message: string;
  severity: "error" | "warning";
}
```

`TypeScriptCheckRunner` shells to ESLint and maps rule ids; a `RustCheckRunner` would
shell to `cargo clippy --message-format=json`. New languages are added by registering a
runner. ts-morph is used inside `TypeScriptCheckRunner` only for rules ESLint cannot
express.

### Alternatives considered

- **Reimplement layering in ts-morph from scratch.** Rejected for Agora: duplicates a
  shipping rule, two checks that can diverge. ts-morph is the fast path for rules with
  no native linter coverage, not the default.
- **Run the full `npm run build` / test suite as the gate.** Too slow for a tight
  revision loop and conflates structural conformance with functional correctness. Build
  and tests run at integration, not as the per-rule structural check.

---

## 5. Q4: Git worktree-per-agent flow

### Recommendation

Coordinator drives `git worktree add .claude/worktrees/<task> -b <task>`, sets the Agent
SDK session **`cwd`** to the worktree path, passes upstream artifacts forward as prompt
context, merges completed-and-gated tasks in dependency order, re-gates at integration,
and removes worktrees explicitly. **Phase 0 runs the two agents sequentially** (Backend
then Frontend), not in parallel.

### Why

Confirmed: worktree creation syntax, the SDK working-directory option, and auto-cleanup
behavior are all documented and sound. Two corrections from verification, both adopted:

1. **The SDK parameter is `cwd`, not `workingDirectory`.** Both TypeScript and Python
   SDKs use `cwd`. The workstream code that wrote `workingDirectory` was wrong; use
   `cwd`.
2. **Auto-cleanup does not apply to non-interactive SDK runs.** Worktrees created for
   headless SDK sessions must be removed explicitly with `git worktree remove`. The
   coordinator owns this lifecycle.

The deeper correction is on concurrency. Parallel agents introduce git lock contention
and rate-limit pressure that the workstream prose glossed over. Phase 0 acceptance only
requires two agents in two isolated worktrees, which is fully satisfiable
**sequentially**: run Backend to completion and gate it, then run Frontend to completion
and gate it, then integrate in dependency order. This eliminates lock contention, the
unverified rate-limit ceiling, and subprocess-leak races as Phase 0 concerns. Parallel
execution with rate-limit-aware scheduling is a P2 problem.

### Coordinator flow (sequential, Phase 0)

```bash
# 1. Backend task in its own worktree.
git worktree add .claude/worktrees/task-backend -b task-backend
#    -> run Backend SDK session with cwd = .claude/worktrees/task-backend
#    -> post-task gate; bounce-and-revise until green
git -C .claude/worktrees/task-backend add -A
git -C .claude/worktrees/task-backend commit -m "feat: User API contract"

# 2. Hand the contract artifact forward (file copy / prompt context, NOT a merge yet).
cp .claude/worktrees/task-backend/api-contract.ts .claude/coordination/api-contract.ts

# 3. Frontend task in its own worktree, consuming the contract via prompt context.
git worktree add .claude/worktrees/task-frontend -b task-frontend
#    -> run Frontend SDK session with cwd = .claude/worktrees/task-frontend
#    -> PreToolUse blocks the planted raw-DB call in real time (layer 1)
#    -> post-task gate catches any structural violation in the diff (layer 2)
git -C .claude/worktrees/task-frontend add -A
git -C .claude/worktrees/task-frontend commit -m "feat: User profile UI"

# 4. Integrate in dependency order, re-gate at integration.
git merge task-backend  -m "Merge Backend"
git merge task-frontend -m "Merge Frontend"
npm run lint && npm run build && npm test   # cross-task re-gate

# 5. Explicit cleanup (SDK runs do not auto-clean).
git worktree remove .claude/worktrees/task-backend
git worktree remove .claude/worktrees/task-frontend
```

### Alternatives considered

- **Parallel worktree execution.** The eventual product shape, deferred from Phase 0
  because of lock contention, an unverified rate-limit ceiling, and subprocess lifecycle
  complexity. Needs a rate-limit-aware scheduler before it is safe.
- **Single shared checkout with branch switching.** Rejected: two agents would clobber
  each other; worktrees are the isolation primitive.

---

## 6. Q5: Rule index schema, and which rules are mechanically enforceable vs review-only

### Recommendation

Build the section 11 rule index as a 6-field compact record derived mechanically from
the existing Camerata TOML. Bucket on the `enforcement` field alone. Add a THIRD runtime
state so the gate never silently passes a rule that is mechanical-by-declaration but has
no shipping check.

### The index schema (one line per rule, all 106 fit in context)

```ts
interface RuleIndexEntry {
  id: string;        // TOML id                         (1:1, verbatim)
  domain: string;    // TOML domain                     (1:1)
  layer: string;     // TOML layer                      (1:1)
  statement: string; // DERIVED: directive of the option named by decision.default.
                     //   Fallback when default absent (route-to-human rules):
                     //   use decision.question and mark unresolved = true.
  enforcement_kind:  // DERIVED from TOML enforcement (see bucketing below)
    "deterministic-active" | "deterministic-declared" | "review-heuristic";
  check_ref:         // SYNTHESIZED. null for review-heuristic and for
                     //   deterministic-declared rules with no check yet.
    | { kind: "eslint" | "ts-morph" | "clippy" | "sql-lint" | "custom"; ref: string }
    | null;
  role_scope?: "FE" | "BE" | "DB" | "global"; // computed from domain at build time
  unresolved?: boolean;
}
```

Field provenance: `id`, `domain`, `layer` map 1:1 from the TOML. `statement` is derived
(the directive of the default option; `directive` is the consumer-facing field per
`principle.rs`, `label` is architect-facing and wrong for this purpose). `enforcement_kind`
is a derived transform of `enforcement`. `check_ref` is the only field that must be
authored. `role_scope` is computed from a small `domain -> scope` lookup at index-build
time (ui / rust:dioxus / javascript:next -> FE; api-layer / rust / rust:seaorm /
permissions -> BE; sql -> DB; everything else -> global).

### Bucketing (verified core, with one downgrade)

Verified by primary-source grep and the camerata-lint source: the corpus has **16
mechanical, 67 structured, 23 prose = 106**, and **the 16 mechanical rules are exactly
the 16 that carry `qualifies`** (1:1 overlap; no structured or prose rule carries it,
and the linter enforces `qualifies` on mechanical rules as a hard schema gate). So the
premise "mechanical + qualifies => deterministic" collapses to **"mechanical =>
deterministic"**; `qualifies` adds no discriminating information. (The charter said 17
qualifies; the corpus has 16. Flagged, not papered over.)

But "mechanical" is a **declaration**, not proof a runnable check exists. This is the
load-bearing risk: trusting `enforcement == "mechanical"` as "gateable now" would route
rules to a deterministic bucket that has no executable check and silently pass them. So
the runtime needs THREE states, not two:

```
bucket(rule):
  if rule.enforcement == "mechanical":
     return check_ref_resolves(rule) ? "deterministic-active"     // a runnable check exists
                                     : "deterministic-declared"   // degrade to review until built
  else:                                                            # structured or prose
     return "review-heuristic"
```

- `deterministic-active`: mechanical AND a runnable check exists. In Phase 0 this is
  driven by which checks the orchestrator can actually invoke.
- `deterministic-declared`: mechanical by declaration, no shipping check yet. At
  runtime, **degrade to review-heuristic** (surface to the human) rather than silently
  pass. This is the honest model VISION section 10 demands: the bucket must never claim
  enforcement that does not exist.
- `review-heuristic`: all 67 structured + 23 prose = 90 rules. Surfaced to the agent in
  its prompt and to the human at QA; not auto-gated in Phase 0. Structured rules are
  individually promotable later by authoring a `qualifies` + a `check_ref`.

### How many mechanical rules have a real shipping check (DOWNGRADED)

The workstream's headline "only ARCH-STRICT-LAYERING-1 has a real shipping check; the
other 15 need building" **was refuted**. Adversarial verification found multiple
mechanical rules with real, shipping enforcement in the Agora repo, not just one. The
verified picture:

- **ARCH-STRICT-LAYERING-1** (api-layer): shipping ESLint rule. The Phase 0 anchor.
- **Several UI / permission / error rules** carry real enforcement that the first pass
  missed because it looked only at the API ESLint config. Verification cited, among
  others, a `next/image` ban (UI-IMAGE-COMPONENT-1), the `_can`-flag permission lint
  (mapped to ARCH-SERVER-AUTHZ-1 / the UI permission rule), SSR-safe UTC date helpers
  (UI-UTC-DATES-1), the dual-API server-only helper split (JAVASCRIPT-NEXT-DUAL-API-1),
  and a structured-error middleware (ARCH-STRUCTURED-ERRORS-1, middleware present, full
  conformance test partial).
- The exact count of fully-shipping checks is between 3 and 6 depending on how strictly
  "a runnable conformance check, not just a code pattern present" is scored. The 4
  `ORCH-*` mechanical rules govern the **orchestrator's own behavior**, not target code,
  and are out of scope for a code-diff gate.

Two consequences for the design:

1. The `deterministic-active` set in Phase 0 is **larger than one rule**, which is good:
   it means the gate has breadth, not a single paper anchor.
2. **Plant TWO violations, not one**, for the acceptance demo: `ARCH-STRICT-LAYERING-1`
   (a Backend/service raw-DB call) AND a UI-layer rule such as `UI-IMAGE-COMPONENT-1`
   (a Frontend `next/image` ban). Both ride on already-shipping checks (zero new check
   to build), and the demo then shows governance across both API and UI layers, not one
   layer. This directly answers the "is only one rule real?" critique.

### The Phase 0 selection pass

One LLM call **inside the investigation agent** (the orchestrator itself makes no model
call). Inputs: the full 106-line index, the Story, the codebase findings. Output: a JSON
subset with per-rule rationale, plus flagged conflicts and gaps. The orchestrator then
does deterministic post-processing with NO LLM:

1. Validate every selected id exists in the index (reject hallucinated ids; bounce
   back).
2. Re-derive `enforcement_kind` and `role_scope` from the index, not from the agent's
   echo (trust the index).
3. Split into the gate plan: `deterministic-active` ids -> post-task check list;
   `deterministic-declared` + `review-heuristic` -> surfaced to the human at QA.
4. Slice by `role_scope` to produce each Role.rule_subset.
5. Present conflicts and gaps to the human; the human owns the final RuleSet.
6. **Stamp the approved RuleSet with a version/hash** (hash of the ordered set of selected
   rule ids + each rule's `enforcement_kind` as approved). Every downstream gate event
   (`gate_result`, `gate_deny`, `gate_bounce`, `provenance_appended`) carries this ruleset
   hash AND the rule's `enforcement_kind` as-applied. This is what lets the UI detect drift
   between the rules the human approved and the rules that actually ran (UI_DESIGN drift
   banner): a rule must never silently change enforcement class between approval and
   execution and be reported as cleanly gated. The engine MUST NOT mutate a rule's
   `enforcement_kind` mid-run; if `check_ref` resolution changes (e.g. a check becomes
   unavailable), the rule degrades to `deterministic-declared` and the event carries the
   degraded kind, never a stale `active`.

No embedding/retrieval in Phase 0: 106 lines fit in context (VISION section 11 "Later
(scale)" is explicitly deferred).

### Alternatives considered

- **Two-state bucket (mechanical vs not).** Rejected: silently passes the 15-ish
  mechanical rules with no shipping check. The three-state model is the honesty fix.
- **Use `label` for the statement.** Rejected: `label` is architect-facing; the agent
  needs the `directive` (the actual instruction).
- **Embed + retrieve top-k now.** Unnecessary at 106 rules; deferred per VISION.

---

## 7. Q6: Onboarding UX, greenfield vs brownfield

### Recommendation

Design both as first-class. Greenfield scaffolds a fresh repo with selected rules baked
in from commit zero. Brownfield onboarding does three things (T13): (1) **map + propose**
the should-be RuleSet (read CLAUDE.md, note what is already enforced, cross-reference the
corpus, flag conflicts); (2) **install** the governance scaffolding that brings the repo
to the should-be state, the canonical lint config, the CI/CD gate steps, the AI-agent rule
files, the hooks; (3) **govern the install itself**, emitting it as a human-approvable
diff/PR (proposed -> approved -> committed, never a silent rewrite, because tearing down
and rebuilding CI is destructive). The key shift: onboarding does NOT merely DETECT what a
repo has; it INSTALLS what the repo SHOULD have. Full AST-based convention extraction and
brand-new rule synthesis from observed patterns remain deferred; installing the corpus's
should-be governance is in scope.

### Why

Brownfield is the default and the product (Agora is the dogfood, every real team already
has code). The Phase 0 minimal brownfield slice needs no SDK capability that the Q1-Q4
corrections touch, so it carries no downgrade. It runs once at onboard time and produces
a human-approvable proposal:

1. **Read docs.** Parse root `CLAUDE.md`, `apps/api/CLAUDE.md`, `apps/ui/CLAUDE.md`;
   extract explicitly stated conventions (layering, auth, the `_can` permission
   pattern).
2. **Detect enforced rules.** Scan `apps/api/eslint.config.mjs` and
   `apps/ui/eslint.config.mjs`; recognize which Camerata rules are already mechanically
   enforced (ARCH-STRICT-LAYERING-1, the UI permission rule, etc.). Note honestly that
   "enforced via lint locally" is distinct from "enforced in CI" (the API CI gap above).
3. **Propose a baseline RuleSet.** Cross-reference detected rules with the corpus;
   auto-select matches; mark corpus rules that apply but are not yet enforced as
   "recommended"; exclude inapplicable rules (RUST-* for a TypeScript repo).
4. **Flag conflicts.** Where an observed pattern contradicts a corpus rule (for
   example, an audit-column strategy mismatch), surface it with three options: adopt the
   rule and plan a migration, keep the pattern with an exception, or synthesize a
   project-variant rule. The human decides.
5. **Output a proposal document** plus a machine-readable baseline RuleSet for human
   approval. On approval, commit it.

Greenfield is the simpler mode: the architect specifies the stack, the system recommends
the corpus subset by domain/layer tags, and a generator scaffolds the directory
structure, CLAUDE.md/CONVENTIONS.md seeded with the directives, and a lint config
pre-populated with the selected mechanical rules. Greenfield is the easy demo; brownfield
is the product.

### Deferred (Phase 1+)

Full AST-based convention extraction, rule synthesis from observed patterns, multi-repo
RuleSet merging, and incremental adoption (baseline vs recommended tiers with a
graduation path). VISION section 17 explicitly defers these; Phase 0 only proves "work
against an existing repo."

### Alternatives considered

- **Full convention extraction in Phase 0.** Rejected as scope balloon; VISION section
  17 defers it. The minimal slice (read docs + lint configs) is enough to seed a
  credible baseline for Agora, which is clean.
- **All-or-nothing rule adoption.** Rejected: real brownfield teams adopt incrementally.
  The proposal separates auto-selected, recommended, and conflicting rules so adoption
  can be partial.

---

## 8. Module layout (TypeScript/Node orchestrator)

```
camerata-orchestrator/
  src/
    intake/
      story.ts                # Story entity, intake (one input box / CLI prompt)
    investigation/
      runner.ts               # drives the investigation agent session
      ruleSelection.ts        # builds the 106-line index, post-processes the
                              #   selection-pass JSON (validate ids, re-derive
                              #   enforcement_kind/role_scope, split gate plan)
      panels.ts               # product-question + tech-tradeoff panel assembly
    rules/
      index.ts                # RuleIndexEntry build from camerata-ai TOML
      bucket.ts               # three-state enforcement_kind classifier
      ruleMap.ts              # lint-rule-id -> Camerata-rule-id map
      corpus.ts               # loads/parses the 106 TOML files
    roles/
      role.ts                 # Role entity: system_prompt, allowed_tools,
                              #   path_boundaries, rule_subset
      scoping.ts              # domain -> role_scope lookup; rule_subset slicing
    agents/
      session.ts              # Claude Agent SDK query() wrapper; cwd, allowed_tools,
                              #   permission mode, ANTHROPIC_API_KEY auth
      hooks.ts                # PreToolUse hook builder (deny + systemMessage); the
                              #   layer-1 real-time gate
    coordinator/
      dag.ts                  # task DAG from the plan; dependency order
      worktree.ts             # git worktree add/remove; cwd wiring; explicit cleanup
      handoff.ts              # upstream-artifact pass-forward (contract -> prompt ctx)
      integrate.ts            # merge in dependency order; re-gate at integration
      schedule.ts             # Phase 0: sequential; P2 hook for rate-limit-aware parallel
    checks/                   # layer-2 post-task gate (pluggable per language)
      CheckRunner.ts          # LanguageCheckRunner interface + registry + runner
      TypeScriptCheckRunner.ts# shells to ESLint; ts-morph fast path; parse + map
      RustCheckRunner.ts      # shells to cargo clippy --message-format=json (stub P0)
    gate/
      postTask.ts             # run active deterministic-active checks; build bounce msg
      bounce.ts               # format rule_id + file:line + fix-suggestion to the agent
    onboarding/
      brownfield.ts           # read CLAUDE.md + lint configs; propose baseline RuleSet
      greenfield.ts           # scaffold generator (later phase; stub in P0)
    persistence/
      store.ts                # SQLite/flat-file Stories, RuleSets, Provenance, Status
    provenance/
      trail.ts                # per-change: task_id, role, session_id, rules_passed[]
    cli/
      main.ts                 # Phase 0 entry: seed story -> investigate -> run -> QA
  .claude/
    worktrees/                # per-task isolated checkouts (gitignored)
    coordination/             # pass-forward artifacts (api-contract.ts, etc.)
```

Responsibility boundary (the load-bearing invariant): everything under `src/` is
**deterministic TypeScript that makes ZERO LLM calls**, except `agents/session.ts`,
which is the only module that opens an Agent SDK session. The single Phase 0 model call
that is NOT a role-agent execution (the rule-selection pass) runs *inside* an
investigation agent session opened via `agents/session.ts`, not from orchestrator code
directly. The orchestrator orchestrates; the agents generate.

The pluggable check interface lives in `checks/CheckRunner.ts`
(`LanguageCheckRunner.applies()` / `.run()`), with one concrete runner per language
registered in a map. Adding a language is adding a runner, not editing the gate.

---

## 9. Unverified assumptions and open risks

Per VISION section 16 ("flag any assumption it could not verify") and ORCH-TRAINING-CUTOFF.
Honesty over confidence. Items are grouped by how they fared in verification.

### A. Claims that DID NOT survive verification (downgraded in this design)

1. **Max-subscription OAuth headless, no metered API (VISION section 3 / 13).**
   REFUTED. OAuth tokens may not be used with the Agent SDK in a third-party product
   (Consumer Terms). Design now uses an `ANTHROPIC_API_KEY`; the Max monthly Agent SDK
   credit ($100 Max 5x on the operator's plan, from June 15 2026) auto-applies to API-key
   usage. Cost outcome at thin-slice scale is preserved; the mechanism in VISION is wrong
   and is corrected. NOTE: the specific plan, credit figure, and model tier are
   DOGFOOD-TESTING details only. The product is provider / tier / model AGNOSTIC behind the
   `agents/session.ts` auth-and-model seam and the `GovernanceGateway` gate seam; a future
   binding to another model or billing model is a swap at those seams, not a redesign.
2. **"Max 20x credit unlocks Tier 3 (2,000 RPM)."** NOT ESTABLISHED. Subscription credit
   and API tier advancement are decoupled. Phase 0 avoids the question by running agents
   sequentially. The real concurrency ceiling for parallel agents is UNMEASURED and is a
   P2 prerequisite.
3. **"Only ARCH-STRICT-LAYERING-1 has a shipping check."** REFUTED. Multiple mechanical
   rules ship real enforcement in Agora (3 to 6 depending on scoring strictness). The
   design now plants TWO violations for the demo and treats the deterministic-active set
   as multi-rule.
4. **Opus 4.8 overflow priced at $3/$15 per 1M.** REFUTED. Verified Opus 4.8 is $5/1M
   input, $25/1M output ($3/$15 is Sonnet 4.6). Cost model corrected accordingly.
5. **`permissionDecisionReason` reaches the model.** REFUTED as the model-visible
   channel. It is audit metadata; the model-visible channel is top-level `systemMessage`.
   Hooks now emit both.
6. **SDK option name `workingDirectory`.** WRONG. The parameter is `cwd` in both the
   TypeScript and Python SDKs. Corrected.
7. **Worktree auto-cleanup for SDK runs.** WRONG for non-interactive SDK sessions.
   Cleanup is explicit (`git worktree remove`); the coordinator owns it.
8. **"Agora CI already enforces ARCH-STRICT-LAYERING-1 on every PR."** REFUTED by direct
   inspection: the API workflow runs only `tsc --noEmit`, never ESLint. The orchestrator
   invokes the linter itself, so Phase 0 is unaffected, but the claim is false. A
   one-line CI fix is recommended separately.

### B. Confirmed, but with caveats that constrain later phases

9. **PreToolUse deny is reliable for primary-agent tool calls** (CONFIRMED), but is
   **ignored for subagent tool calls** (documented, unfixed). Phase 0 spawns no
   subagents, so it is unaffected; any later phase that spawns subagents must add a
   layer-2 backstop for nested calls and must not rely on PreToolUse alone there.
10. **Scattered reports of PreToolUse deny being bypassed for some MCP tools and under
    parallel-tool-call races.** Phase 0 mitigates by not registering unneeded MCP tools
    and by running sequentially. The layer-2 post-task gate is the defense-in-depth
    backstop regardless. The robustness of layer-1 under heavy parallel load is UNVERIFIED.
11. **The June 15 2026 billing change is newly effective and reported via near-primary
    sources.** The credit amounts ($20 Pro / $100 Max 5x / $200 Max 20x; the operator is on
    Max 5x = $100) and the "separate pool, API-key-drawn" mechanism are consistent across
    sources, but exact rollover, fail-closed-on-exhaustion behavior, and overflow opt-in
    details should be confirmed live in week 1 before any cadence heavier than the thin
    slice. This is a dogfood-testing concern only; the product is billing-model agnostic.

### C. Assumptions that could NOT be checked and remain open

12. **Acceptable-use clearance for an automated orchestrator under metered API keys.**
    The API-key path is ToS-clean in a way the OAuth path was not, but confirm with
    Anthropic that a deterministic orchestrator spawning Agent SDK sessions for code
    generation is in-policy before any shared/team use. Single-user Phase 0 dogfood is
    the safe interpretation; team use is a separate clearance.
13. **Exact count and conformance maturity of shipping mechanical checks in Agora.**
    Verified as "more than one, roughly 3 to 6," but the precise per-rule maturity (is
    there a runnable conformance check, or merely a code pattern present?) was not
    audited rule-by-rule. The `check_ref` resolution at index-build time is the
    mechanical source of truth and must be computed against the live repo, not assumed.
14. **Rate-limit / lock-contention behavior under parallel worktrees.** Deferred to P2
    and explicitly UNMEASURED. Do not promise parallelism until benchmarked.
15. **`qualifies` count discrepancy (charter said 17, corpus has 16).** The corpus value
    (16, exactly the mechanical set) is the verified one; the charter figure is stale.
    Flagged so a future corpus change does not silently reintroduce the off-by-one.

---

## 10. Configuration storage (config-as-code)

Decision: **local, two-tier, portable via git, NOT cloud.** Cloud storage was rejected, it
adds infra, a sync-conflict problem, undercuts the "no cloud infra in V1" principle, and
puts secrets somewhere they should not be. The portability need is met by git for the part
that should be portable.

- **Project config (in-repo, committed, git-tracked).** The selected rules, role
  definitions, gate/check definitions, the approved RuleSet + its version/hash (Q5), the
  bounce-and-revise max-revision ceiling, and the project's tracker binding live in an
  in-repo `.camerata/config.toml` (and adjacent files). Portable because it travels with
  the repo; auditable because changes are PRs; on-thesis because the governance config is
  itself governed (config-as-code, same pattern as `.claude/`, eslint config,
  CONVENTIONS.md). This is also what onboarding (Q6 / T13) installs.
- **Secrets / machine config (local user dir, never committed).** `ANTHROPIC_API_KEY`,
  Jira / GitHub / Azure DevOps tokens, and per-machine preferences live in
  `~/.config/camerata/` or the OS keychain. Per-machine by design; secrets must not be
  portable. If cross-machine sync of personal prefs is ever wanted, a user's own dotfiles
  repo handles it, Camerata owns no cloud.

The split is the same line drawn elsewhere: the governed, shareable artifact is in the
repo; the secret, machine-bound artifact is not.

---

End of TECH_DESIGN.md.
