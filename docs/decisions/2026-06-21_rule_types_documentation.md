# 2026-06-21: Rule types documentation

## What was added

### `docs/USER_GUIDE.md` — §13 "Understanding rule types"

Added as the final numbered section (before "The whole loop" closing paragraph). Contents:

- The five buckets (security gate / mechanical / architectural / structured / prose), each with one
  line of "what it means for you" and "where it's enforced."
- An explicit one-liner that the security gate is a small hardwired built-in set, not a category you
  author into.
- The objectivity spectrum table (prose → structured → mechanical → architectural, human → machine).
- A plain-language prose vs. structured distinction: prose = human must judge; structured = human can
  verify against a binary contract. Both outside CI; difference is judgment, not format.
- Where rules are written (arm.rs routing: prose → AGENTS.md, rest → CONVENTIONS.md).
- The verification badge table (verified / grounded / draft) with the honest current state
  (0 verified, grounded is the shippable baseline).

### `docs/TECHNICAL.md` — §5a "Rule type model: two axes"

Inserted after §5 (Rule corpus) and before §6 (Onboarding scan pipeline). Contents:

- **Axis A** — the four `EnforcementKind` variants (Prose / Structured / Mechanical / Architectural)
  with their conformance test, plain-English meaning, and render target. The full spectrum table.
  Current corpus counts (described as approximate, per spec). Render-target routing source of truth
  (arm.rs + onboard.rs lines cited). The exact prose-vs-structured line, clearly stated for the
  chatbot probe.
- **Axis B** — the five enforcement points: MCP gate (layer-1), layer-2 check runner, layer-3 CI,
  agent directive, human review. Each described precisely with source file citations.
- **Gate-is-a-point proof** — 5 of 6 gate rule-ids are not in the corpus at all; only
  ARCH-NO-SECRETS-IN-URL-1 is also a corpus rule (tagged structured). Cited: crates/gateway/src/lib.rs
  RULE_REGISTRY.
- **Layer-2 polyglot state** — described as done: JsCheckRunner / PythonCheckRunner / GoCheckRunner /
  RustCheckRunner + detect_languages + PolyglotCheckRunner in crates/checks/src/multilang.rs.
  Fail-closed semantics and repo-pinned toolchain both documented. Defense-in-depth note (layer-2
  fast/in-loop; layer-3 authoritative backstop; intentional redundancy, not duplication).
- **Mechanical linter gap** — stated as closed: every mechanical rule maps to a real linter rule;
  rules with no off-the-shelf match were reclassified.
- **Verification ladder** — draft / grounded / verified with the no-agent-may-set-verified trust
  boundary. Honest current state: 0 verified, mechanical rules are grounded.
- **Chatbot grounding confirmation** — quotes the two include_str! lines from crates/ui/src/chat.rs
  (TECHNICAL_DOC and USER_GUIDE). Confirms both are baked into the unified system prompt as the
  static layer-1, cache-eligible. States that the canonical probe is answerable from §5a.

## Chatbot grounding status

`crates/ui/src/chat.rs` already includes both docs at compile time:

```rust
const TECHNICAL_DOC: &str = include_str!("../../../docs/TECHNICAL.md");
const USER_GUIDE: &str    = include_str!("../../../docs/USER_GUIDE.md");
```

No wiring change was needed. A doc change recompiles camerata-ui; the chatbot sees the new content
automatically. The canonical probe "what is the difference between a prose and a structured rule?" is
now answerable from TECHNICAL.md §5a without guessing: prose = judge (degree); structured = verify
(binary contract). Both same TOML shape; difference is judgment.

## Decisions reflected

- Current-state corrections from the task brief applied: mechanical-without-linter gap is ZERO
  (stated as closed); layer-2 is polyglot and fail-closed (stated as done). No stale caveats from
  the original spec §4 were written.
- Spec §3 (verification ladder: draft/grounded/verified, agent-may-never-set-verified) documented in
  both guides.
- Gate-is-a-point nuance (§2.1 of spec): stated precisely in TECHNICAL.md §5a with 5-of-6 proof.
  USER_GUIDE §13 gives the accessible version.
- Chatbot grounding: confirmed by reading chat.rs — no additional wiring required.
