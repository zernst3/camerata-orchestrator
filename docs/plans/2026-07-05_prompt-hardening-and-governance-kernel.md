# Prompt hardening + the shared governance kernel — 2026-07-05

Provenance: read-only Fable 5 sweep of every agent-driving prompt in Camerata. Analysis + ready-to-apply
prompt text; NO source was modified. Motivation: Camerata is model-agnostic (Claude today; open-weight
GLM 5.2 / DeepSeek V4 tomorrow). Safety-tuned models (Claude) SPONTANEOUSLY write tests, self-correct,
and program defensively; literal open-weight models will NOT unless the prompt explicitly mandates it.
Goal: identical governed behavior at every entry point, across every model. Review before applying.

---

## 1. Inventory: every prompt Camerata sends to a governed agent

### A. Agentic (code-writing / repo-walking) task prompts
| Entry point | file : symbol | Drives | Typical tier |
|---|---|---|---|
| Brownfield implement run | dev_implement_run.rs : `implement_prompt` (L572) | Governed code-writing agent implementing a story from approved decisions | strongest |
| Investigation run | investigation_run.rs : `investigation_prompt` (L119), `investigation_resume_prompt` (L163) | Read-only analysis agent producing decisions/tradeoffs | strongest |
| PR feedback resolve | pr_resolve_run.rs : `resolve_prompt` (L37) | Agent fixing review comments + failing CI | balanced/strongest |
| Merge-conflict resolve | update_branch_run.rs : `conflict_prompt` (L86) | Agent reconciling conflict markers | balanced/strongest |
| Fleet stage task | fleet/src/lib.rs : `stage_task_for` (L195) | Each greenfield fleet agent (one gated_write) | mixed per TaskKind |
| Lead orchestrator suffix | fleet/orchestrator.rs : `orchestrator_prompt_suffix` (L217) | Teaches the lead to delegate across tiers | strongest |
| Delegate child framing | gateway/delegate.rs : `run_delegate` (L437) | Each delegated child subtask (has the INCOMPLETE: escape hatch) | fast/balanced |
| API-driver system prompt | api_agent_driver.rs : `build_system_prompt` (L1540) | System prompt for EVERY OpenRouter/API in-process loop (the open-weight path) | any |

### B. Single-shot completion prompts
| Entry point | file : symbol | Drives | Tier |
|---|---|---|---|
| Conformance audit | ai_audit.rs : `audit_system_prompt` (L194) | Rule-by-file audit JSON | audit step |
| Calibration / deep-security / soc2 / threat lenses | ai_audit.rs : L426 / L2275 / L2231 / L2306 | Findings JSON | audit |
| L3 review | review_agent.rs : `L3_SYSTEM_PROMPT` (L68) | PASS/BOUNCE verdict + completeness | l3 model |
| Integration gate | review_agent.rs : `INTEGRATION_SYSTEM_PROMPT` (L149) | PASS/MISMATCH cross-repo verdict | gate model |
| Story / design / diagram / mockup authoring | lib.rs : `STORY_AUTHOR_SYSTEM` (L8505), `DESIGN_AUTHOR_SYSTEM` (L9235), `DIAGRAM_AUTHOR_SYSTEM` (L8966), `MOCKUP_AUTHOR_SYSTEM` (L9072) | Draft JSON / Mermaid / HTML | authoring |
| Decomposition | decompose.rs : `propose_ai` base_system (L102) | Child-stories JSON | decomposition |
| Clarifying questions | lib.rs : `suggest_clarifications` (L6694) | Questions JSON | clarification |
| Routine prompt author | lib.rs : `draft_routine_prompt` (L6831) | Authors the prompt a future governed routine runs (a prompt that writes prompts) | user |
| Escalation translate / chat | app-core/escalation.rs : L192 / L337 | Resume payload / advisory chat | escalation |
| Intake lead engineer | intake/engine.rs : `build_prompt` (L628); intake/review.rs (L415) | Checklist/verdict/plan JSON | strongest |

### C. Corpus directives (prompts by another name)
- 353 principle TOMLs; each `[[option]].directive` is injected into grounding (grounding.rs : `render_rule_context` L98) + baked into AGENTS.md/CONVENTIONS.md (onboard.rs).
- Pattern gap: directives are DECLARATIVE ("A service never instantiates its own repository") — they say WHAT holds, never what the agent must DO on a violation / when unsure. Safety-tuned models treat these as constraints; literal models treat them as descriptions.
- grounding.rs : `assemble` (L254) is the closest thing to a shared preamble today, but covers ONLY grounding, not TDD/verify/defensive/output.

---

## 2. Per-prompt gap analysis
Behaviors: [TDD] test-first, [VER] self-verify, [DEF] defensive, [OUT] output contract, [STEP] stepwise, [GND] grounding, [UNSURE] if-unsure policy, [COMP] completeness.

- **implement_prompt (highest impact):** [TDD] missing (says "add tests" but no order/loop, and the gated role has NO Bash so the agent literally cannot run the build the prompt tells it to). [VER] missing (unverifiable done-condition -> literal model just declares done; replace with a mandatory self-review re-read pass). [DEF] absent. [OUT] no final-report contract. [UNSURE] partial (only for escalation-spec rules). [COMP] missing at implementer level.
- **investigation_prompt:** good [OUT]/[UNSURE]; [GND] partial (grounding only when Some; no "cite files you read" mandate -> plausible ungrounded note passes review). [VER] missing.
- **pr_resolve / conflict_prompt:** rely on unrunnable "keep it building"; no per-item report mapping each comment/conflict to its fix (literal model silently skips one). No "reviewer is wrong / sides incompatible" policy.
- **L3_SYSTEM_PROMPT (strongest today):** gaps: no [STEP] (check rule-by-rule then criteria-by-criteria BEFORE the verdict), no [UNSURE] ("cannot verify" -> BOUNCE, not silent PASS), no "quote the violated rule id in each bounce reason".
- **INTEGRATION_SYSTEM_PROMPT:** very thin; no clause-by-clause enumeration; empty-diff repo not checked against the contract; "insufficient evidence" unaddressed.
- **audit_system_prompt (gold standard):** already near target; add severity-ordering (graceful truncation), self-validate-JSON, enumerate-in-order.
- **authoring constants:** missing "Acceptance Criteria must be objectively verifiable" (where test-first culture starts) + "draft only against capabilities the repo actually has".
- **draft_routine_prompt (high leverage, currently weakest vs blast radius):** authors an operational prompt for an arbitrary future model but gives no checklist of what a good prompt must contain. Must be required to EMBED the kernel.
- **api_agent_driver build_system_prompt:** THE system prompt for every open-weight agent, contains ONLY tool constraints. Single highest-impact insertion point for the kernel.

---

## 3. Hardened rewrites (highest impact)
`{KERNEL}` = the shared kernel in section 4. Rewrites keep existing interpolation variables.

### 3.1 implement_prompt (dev_implement_run.rs) — hardened body
```
You are the BROWNFIELD IMPLEMENTER for story `{story_id}` (branch `{target_branch}`).
{KERNEL}
{grounding_block}
## Story
Title: {story_title}
Description: {story_desc}
## Architect-approved decisions (the spec)
{decisions_text}
The approved decisions are binding. If the actual code contradicts a decision, do NOT
silently pick one: implement the decision if possible and state the contradiction in your
final report. Never substitute your own preference for an approved decision.
{escalation_block}
## Required procedure (IN ORDER)
1. READ FIRST. Read every file you intend to change and its callers. If a file is not
   where you expect, Grep/Glob for it; never assume its contents.
2. PLAN the minimal change satisfying the story AND every decision. If the story names a
   pattern/class of defect, Grep and enumerate EVERY occurrence; cover all of them.
3. TESTS WITH THE CHANGE. Each new/changed behavior gets a test in the project's style
   that fails if the behavior is removed. A change with no test must be justified.
4. IMPLEMENT via gated_write only, full file contents. Handle error/empty cases on every
   path; validate input at boundaries; no new unwrap/panic on fallible paths unless the
   file already does; match existing conventions exactly.
5. SELF-REVIEW BEFORE DONE. Re-read every changed file end to end: each criterion+decision
   maps to a change; no syntax errors / missing imports / dangling refs; no unrelated file
   touched; every grounding rule still holds. Fix, then re-read again.
## Hard prohibitions: no git commit/push; no unrelated files; never weaken/skip tests.
## Final report (exact): CHANGES / TESTS / DECISIONS-TRACE / CONCERNS (NONE if empty).
```

### 3.5 build_system_prompt (api_agent_driver.rs) — the open-weight chokepoint
```
You are a governed software engineering agent in the `{role}` role under Camerata.
CONSTRAINTS: write files ONLY via gated_write (denied writes are information, not an
obstacle to route around); Read/Glob/Grep/LS to read (read before you write; never guess
contents/locations); NO Bash/Task/Edit/Write/MultiEdit or unlisted tools.
WORKING DISCIPLINE (in order): (1) read relevant code; (2) plan the minimal complete
change; (3) write tests with any behavior change; (4) implement defensively (explicit
error/empty handling, boundary validation, follow file conventions); (5) before finishing,
re-read every file you wrote and fix any incompleteness/syntax/import/rule issue. Not done
until this self-review finds nothing.
IF UNSURE: do not guess/invent. Prefer: read more; take the most conservative compliant
action; or state precisely what is unknown. Never fabricate file contents, APIs, or facts.
COMPLETION: final text message with CHANGES / TESTS / CONCERNS.
Role: `{role}`   Allowed paths: {paths}
```

(Full rewrites for investigation_prompt [3.2], L3 [3.3], integration [3.4], and short directives for
audit lenses [3.6], decompose [3.7], and the lower-impact prompts [3.8] are in the sweep transcript;
the pattern is identical: embed {KERNEL}, add explicit stepwise + unsure + per-item-report mandates.)

Short directives (3.8): resolve/conflict prompts embed kernel + per-item report + "reviewer-wrong /
sides-incompatible" policy + Grep-for-conflict-markers=0; stage_task_for adds read-back-after-write +
"tests call the public API and fail on regression"; orchestrator/delegate add "verify delegate output by
Reading the files it claims to have written"; authoring adds testable-AC + draft-only-real-capabilities;
draft_routine_prompt must inject the kernel into the prompt it authors; emitted AGENTS.md/CONVENTIONS.md
get one compliance-protocol paragraph at the emitter (converts all 353 declarative directives into
checked behavior without touching the TOMLs).

---

## 4. THE KEY DELIVERABLE: the shared governance prompt kernel
Home: `pub const GOVERNANCE_KERNEL: &str` in camerata-app-core (next to DEFAULT_MODEL), embedded by every
task-prompt builder AND api_agent_driver::build_system_prompt. Two variants; ~450 tokens each (cache-friendly static prefix).

### Full kernel (writing agents)
```
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
=== END OPERATING PROTOCOL ===
```

### Read-only kernel (reviewers, auditors, analysts, single-shot JSON)
```
=== CAMERATA OPERATING PROTOCOL (analysis) ===
1. GROUND EVERY CLAIM. Base every statement only on provided material or files you read.
   Cite the file/line/clause. Label anything unverifiable "cannot verify"; never present
   an assumption as fact.
2. ENUMERATE, THEN JUDGE. Work through the inputs systematically (each rule, criterion,
   clause, file, in order) and finish the enumeration before concluding. Do not stop at
   the first hit.
3. IF UNSURE, SAY SO. An unverifiable point is reported as such (finding / bounce reason /
   unknown), never silently passed and never guessed.
4. EXACT OUTPUT ONLY. Emit exactly the specified format: nothing before/after, no fences
   unless asked. Re-check output against the schema before finishing.
=== END OPERATING PROTOCOL ===
```

Notes: every clause is imperative + inspectable (assertable by prompt unit tests like `implement_prompt_contains_...`), vendor-neutral, tool refs conditional ("if available"), under ~450 tokens for cache stability (llm.rs `cache_prefix_len`).

---

## 5. Prioritized rollout (blast radius x gap x likelihood-of-non-Claude-model)
1. **api_agent_driver.rs build_system_prompt** — one function; the system prompt for the exact path open-weight models arrive through. Biggest gain per line.
2. **dev_implement_run.rs implement_prompt** — the only prompt that produces committed app code. Full kernel + 3.1. Update its prompt unit tests.
3. **Land `GOVERNANCE_KERNEL` (+ read-only variant) in app-core**, add presence tests, thread into pr_resolve / update_branch / stage_task_for / delegate.
4. **review_agent.rs both prompts** (L3 + integration) — the safety net that keeps quality flat when the implementer runs on a weaker model.
5. **investigation_prompt** — ungrounded decisions poison the whole pipeline.
6. **draft_routine_prompt** — inject the kernel into authored routine prompts (propagates the standard).
7. **authoring + decompose** — testable ACs upstream make TDD mandates downstream meaningful.
8. **emitted AGENTS.md/CONVENTIONS.md preamble** (onboard.rs) — one paragraph converts 353 directives into checked behavior.
9. **audit lens prompts** — lowest; already near target.

### Per-tier addenda (keep ONE kernel; add a small tier-keyed addendum via `kernel_for(model_or_tier)`)
- **fast / low (Haiku, DeepSeek-Flash):** "Do exactly what the task says and nothing else. If anything is ambiguous or exceeds what you can verify, return INCOMPLETE: <reason> instead of attempting it." Low tiers fail loudly, not creatively.
- **balanced / mid (Sonnet, DeepSeek-Pro — surgically precise but literal):** full TDD loop ("write the failing test FIRST, then implement, then confirm the test would fail without the change") + run rule-5 self-review TWICE.
- **strongest / orchestration (Opus, GLM):** kernel as-is + delegation-verification (Read the files a delegate claims to have written; restate acceptance criteria in every subtask).

Seam: every runner resolves its model from the project tier_map before building the prompt, so `kernel_for(tier)` needs no new plumbing.

## Open design question (GLM 5.2 1M context)
For the high tier, prefer a CHUNKED / retrieval-grounded planning strategy over blind whole-repo
ingestion: (a) it keeps the cache prefix stable (grounding.rs relies on a stable prefix), (b) it avoids
attention dilution across a large monolith, (c) it keeps the kernel + relevant subsystem + cross-repo
contracts in the high-signal part of the window. Use the 1M window to hold the full RELEVANT subsystem
and its contracts during a task, not the entire codebase indiscriminately.
