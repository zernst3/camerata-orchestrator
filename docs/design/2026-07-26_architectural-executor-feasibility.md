# Feasibility: a deterministic architectural-rule executor

Date: 2026-07-26
Status: Investigation memo — for a DEFER vs BUILD-NOW decision by Zach
Companion: [`2026-06-19_ast_architectural_rule_tier.md`](../decisions/2026-06-19_ast_architectural_rule_tier.md),
[`2026-07-26_supabase-stack-rules.md`](2026-07-26_supabase-stack-rules.md) §4

**The question:** the `Architectural` enforcement tier exists in the schema but has no
deterministic executor. Supabase RLS (migration-timeline replay) is the first rule family that
*needs* one. Do we build the general executor subsystem now, or keep deferring?

**Zach's framing is correct, with one amendment.** It is genuinely a class decision, not an
RLS-scan decision — a bespoke RLS interpreter with no seam would fix where all 16 non-process
architectural rules live. The amendment: the seam is *less greenfield than the framing implies*.
Roughly 80% of the executor's plumbing already exists as first-class, tested subsystems
(the `CheckRunner` trait, the manifest runner as the Layer-2 template, the `Finding` pipeline,
the scan's file walk, the routed `ArchitecturalCheck` trait proposal). What's missing is one
trait generalization and two wiring points, then checkers behind it.

---

## 1. Current reality: how findings are produced, per tier

| Path | What runs | Where | Deterministic? | Architectural rules? |
|---|---|---|---|---|
| **Floor** | `AUDIT_RULES` (8 SEC-* content rules) via `audit_files` — pure fns shared with the Layer-1 gate arms | `onboard.rs:64`, called in `audit_repos` (~line 726) | Yes | No — floor is content-regex grade |
| **Scan-tool preview** | `group_by_tool` routes **Mechanical** rules to clippy/ruff/eslint/semgrep | `scan_tools.rs:277–310` | Yes (tool output) | **Explicitly skipped** — `scan_tools.rs:291–296` (`enforcement != Mechanical` ⇒ `continue`), locked by tests `group_by_tool_skips_architectural_rules` (:1357) and the preview-ids test (:1442) |
| **AI review** | Semantic rules → LLM audit passes (`run_ai_review`) | `audit_repos`, `ai_audit.rs` | No — advisory | Yes, this is their only scan coverage today |
| **CI story** | `ci_story_body_architectural` (`server/src/lib.rs:6510`) files a GitHub issue telling the **client team** to design/build a bespoke checker per rule | arm/CI-wiring flow | N/A — it's a work ticket | Yes — a "go build this yourself" story |
| **Layer-1 write gate** | `evaluate_call` over `RULE_REGISTRY`, per `gated_write` call | `gateway/src/lib.rs` | Yes | No — and the 06-19 ADR *argues* they can't run here (needs whole module; mid-edit trees don't parse) |
| **Layer-2 gov-dev loop** | `runner_for_worktree` → `CombinedCheckRunner` = built-in language runners (fmt/clippy/test/polyglot) + **`ManifestCheckRunner`** over `.camerata/checks.toml` (`in_loop = true`, operator-authored shell, agent-tamper-proof via SEC-NO-CAMERATA-CONFIG-1) | `checks/src/multilang.rs:1520`, `manifest_runner.rs` | Yes | Only if the operator hand-writes an external script per rule — no native checkers |
| **VCS-action gate** | Deterministic matchers over commit/PR/branch **metadata** | `checks/src/vcs_action.rs` | Yes | Covers the 4 `process-*` architectural rules already |

**Confirmed: no Camerata-native architectural executor exists.** The one shipped checker —
`handler_no_direct_db` in `crates/checks/src/architectural.rs` (404 lines, lexical
brace-tracking, 9 unit tests) — is a *proof of concept called from nowhere* (no call sites
outside its own file). Note `escalation.rs:314` is a red herring: that string is a **test
fixture** for the escalation store, not a gate path.

**The 06-19 ADR both designed and punted, explicitly.** It proposed the production shape
(an `ArchitecturalCheck` trait over a `ParsedModule`; `syn` for Rust, tree-sitter for the long
tail), then **routed** the trait + `syn` dependency to Zach per ROUTE-1 and listed as
deliberately-not-built: the trait, the real CI runner, the cross-boundary-imports checker, and
the language-agnostic layer. So this memo is not re-deciding that ADR — it is the routing
artifact the ADR asked for, updated with what RLS teaches us about the trait's shape.

**Corpus inventory: 20 rules carry `enforcement = "architectural"` today.**

| Family | Rules | Executor status |
|---|---|---|
| process/* (4) | branch-naming, ado-link, conventional-commit, commit-doc | **Already executed** — VCS-action gate |
| supabase/* (4) | rls-enabled, rls-no-policy, rls-policy-disabled, func-search-path | Nothing — the motivating case |
| api-layer/* (6) | handler-no-db, strict-layering, no-cross-boundary-imports, api-dtos, structured-errors, exact-decimals | Proof checker (unwired) for one; nothing else |
| ui/* (2), universal (1), python-testing (1), permissions (1), agentic (1) | image-component, utc-dates, resource-lifecycle, testing-file-naming, server-authz, no-test-tamper | Nothing (test-tamper has separate UoW-escalation handling) |

So the executor's real backlog is **16 rules**, of which the 4 Supabase ones share one model.

---

## 2. The executor seam

### 2.1 The checker abstraction

The 06-19 trait (`check(&self, module: &ParsedModule)`) is **too narrow** — RLS proves it. The
RLS answer is not a property of one parsed module; it's a fold over an *ordered set of files*
(the migration timeline). Same for import-boundary rules (the import graph spans modules).
Generalize the unit of analysis from "parsed module" to "repo view"; model-building becomes each
checker's private concern:

```rust
/// crates/checks — a deterministic architectural checker.
/// Builds a per-repo model (AST / migration timeline / import graph),
/// runs deterministic queries, emits violations. No LLM, no network.
pub trait ArchChecker: Send + Sync {
    /// Corpus rule ids this checker can answer (e.g. SUPABASE-RLS-ENABLED-1).
    fn rule_ids(&self) -> &'static [&'static str];
    /// Path globs the checker needs (e.g. "supabase/migrations/*.sql",
    /// "supabase/config.toml"). Lets callers feed it a narrow file slice —
    /// the Layer-2 loop stays cheap, and `applies` falls out for free
    /// (zero matching files ⇒ skip, no false "✓ clean").
    fn interest_globs(&self) -> &'static [&'static str];
    /// Build the model from the supplied files and answer every rule.
    fn check(&self, repo: &RepoView) -> Vec<ArchViolation>;
}

/// The shared input currency: repo spec + (path, content) pairs.
/// Callers construct it from what they already have — the scan from its
/// in-memory walk, the Layer-2 runner from the worktree.
pub struct RepoView<'a> { pub spec: &'a str, pub files: &'a [(String, String)] }
```

Plus a registry, `pub fn all_checkers() -> Vec<Box<dyn ArchChecker>>`, and one shared adapter
`arch_violation_to_finding(...) -> Finding` so architectural findings ride the **existing**
pipeline unchanged: suppression classification (`camerata:allow` / baseline via
`classify_repo_findings`), test-scope down-ranking, report/CSV/UI. A checker runs iff its
`rule_ids` intersect the selected (scan) or armed (gov-dev) ruleset — same per-repo binding
logic `audit_repos` already applies.

Key simplification vs. the 06-19 proposal: **no shared `ParsedModule` abstraction, no `syn`
needed yet.** Each checker owns its model. The `syn`/tree-sitter decision only becomes due when
the first *code-AST* checker (api-layer family) is promoted — RLS never touches it.

### 2.2 Plug point A — the brownfield scan (`audit_repos`)

Inside the per-repo loop in `onboard.rs` (`audit_repos`, ~line 673), immediately after the floor
call (`let floor = audit_files(spec, &files)`, ~line 726), add the third deterministic engine:

```
route by engine (existing comment at onboard.rs:654 already says this):
  gate-arm rules        → audit_files (floor)          [exists]
  mechanical rules      → scan-tools preview           [exists]
  ARCHITECTURAL rules   → ArchChecker registry         [NEW — ~30 lines]
  semantic rules        → LLM audit                    [exists]
```

Gated by the same `run_deterministic` flag as the floor; findings marked with a provenance tag
(e.g. `preview_tool: Some("camerata-arch")` or a new field) so the UI can badge them
"deterministic — replayed end-state". The corresponding change on the AI side: rules a native
checker covers are *excluded* from the LLM prompt (exactly how `lookup_arm` rules are excluded
today), so the model never fuzzes what code answers exactly. The `scan_tools.rs:291` skip stays
correct as-is — architectural rules still don't route to *off-the-shelf* tools; they route to
the new registry.

### 2.3 Plug point B — the Governed Development write-time gate

Honor the 06-19 ADR's argument: architectural checks **cannot** run per-`gated_write` in Layer 1
(mid-edit trees don't parse; the answer needs the assembled tree). The write-time enforcement
surface for architectural rules is **Layer 2, at the checkpoint** — which is still "gating the
agent's write" in the sense Zach means: a violation bounces the work back for revision before it
ever becomes a commit, exactly like a clippy failure.

Mechanically this is a `NativeArchCheckRunner` implementing the existing
`camerata_core::CheckRunner` trait (`core/src/lib.rs:153`), composed into
`CombinedCheckRunner` right beside `ManifestCheckRunner` (`runner_for_worktree`,
`multilang.rs:1520`). It reads only the checkers' `interest_globs` from the worktree, runs the
registry against the armed rule ids, maps `ArchViolation` → `CheckOutcome.violated`. It is the
native sibling of the manifest runner: same position in the loop, but zero operator-authored
shell — the checker ships inside Camerata. For RLS this is a *strong* fit: an agent that writes
a migration adding a table without RLS gets bounced in-loop, deterministically, with the exact
file/line.

### 2.4 The one honest wrinkle: Layer-3 CI parity

The generated `camerata-gates.yml` runs in the **client's** GitHub Actions, where the Camerata
binary does not exist. Manifest checks survive there because they're shell. Native checkers
don't, until there's a distributable (a small published `camerata-check` binary, or
per-checker emitted scripts). **Recommendation: accept the asymmetry for now.** Native checkers
cover the two Camerata-side surfaces (scan + Layer-2 loop); the CI story
(`ci_story_body_architectural`) remains the client-side path, and can now say "or run
`camerata-check`" once a distributable exists. Do not block the seam on this.

---

## 3. First checker: Supabase RLS migration replay (as a plain instance)

```
SupabaseRlsChecker : ArchChecker
  rule_ids        = [SUPABASE-RLS-ENABLED-1, SUPABASE-RLS-NO-POLICY-1,
                     SUPABASE-RLS-POLICY-DISABLED-1]
  interest_globs  = ["supabase/migrations/*.sql", "supabase/schemas/*.sql",
                     "supabase/config.toml"]
  check(repo):
    1. exposed_schemas ← parse config.toml [api].schemas (default {"public"})
       — the same parse SUPABASE-EXPOSURE-SCHEMAS-1 specifies (spec §2.6)
    2. order migrations by <timestamp>_ filename prefix
    3. split each file into SQL statements (comment/dollar-quote aware splitter)
    4. fold per (schema, table): CREATE/DROP/RENAME TABLE,
       ALTER … ENABLE|DISABLE ROW LEVEL SECURITY, CREATE|DROP POLICY
       — recording file+line of each last-establishing statement
    5. declarative shortcut: supabase/schemas/*.sql present ⇒ treat as
       authoritative end-state for covered objects (spec §4)
    6. queries over final state:
       exposed ∧ ¬rls            → RLS-ENABLED-1   (critical; non-exposed ⇒ demoted note)
       rls ∧ policies = ∅        → RLS-NO-POLICY-1
       policies ≠ ∅ ∧ ¬rls       → RLS-POLICY-DISABLED-1
    7. every finding carries table, establishing file:line, and the §4 honesty
       caveat ("no evidence in repo" ≠ "disabled in production")
```

Nothing about this is a special case: the trait sees only `rule_ids` / `interest_globs` /
`check`. Note the parsing depth is *shallow by design* — spec §4 already establishes that the
per-statement facts are regex-grade once statements are split; the architectural content is the
**ordering and the fold**, not deep SQL parsing. No `sqlparser` dependency needed; a
dollar-quote-aware splitter plus per-statement classifiers is a few hundred lines with
fixture-driven tests.

### Other checkers the same seam hosts (proving the generalization)

| Checker | Model built | Rules answered | Notes |
|---|---|---|---|
| **SupabaseFnSearchPathChecker** | the *same* migration-timeline fold, over `CREATE FUNCTION … SECURITY DEFINER` + `SET search_path` | SUPABASE-FUNC-SEARCH-PATH-1 | Near-free once RLS ships — shares the splitter + timeline; strongest evidence the model is per-stack, per-checker reusable |
| **ImportBoundaryChecker** | import graph from parsed `use`/import declarations + a declared boundary map | ARCH-NO-CROSS-BOUNDARY-IMPORTS-1, ARCH-STRICT-LAYERING-1 | The 06-19 ADR's own second example; needs the `syn`/tree-sitter decision (routed) — a per-language import *extractor* is much shallower than full AST |
| **HandlerNoDbChecker** | function-boundary scan per module | ARCH-HANDLER-NO-DB-1 | Already built as the proof fn; promotion = wrapping `handler_no_direct_db` in the trait (~an hour), production quality later via `syn` |
| **TestFileNamingChecker** | file tree + naming convention | PYTHON-TESTING-FILE-NAMING-1 | Trivially path-shaped; shows the seam scales *down* too |

(The 4 `process-*` architectural rules stay where they are — the VCS-action gate is their
correct, already-complete executor. Worth a follow-up corpus note so nobody expects them from
this seam.)

---

## 4. Effort and risk (honest, non-inflated)

Calibrated against the documented ~5x AI-orchestration overestimation pattern; these are
orchestrated-build estimates, not hand-typing estimates.

| Piece | Estimate | Why it's cheap / where the risk is |
|---|---|---|
| (a) Seam: trait + `RepoView` + registry + violation→`Finding` adapter + scan wiring in `audit_repos` + LLM-exclusion filter + tests | **~1 day** | Every pattern has a template in-repo (floor engine routing, preview provenance flags, `classify_repo_findings`). Risk: low. ROUTE-1 formality: the trait is public cross-crate surface — this memo is the routing artifact; Zach's sign-off clears it. |
| (b) `SupabaseRlsChecker`: splitter + timeline fold + 3 queries + config.toml exposure parse + declarative shortcut + fixture corpus (incl. enable-then-disable, rename, drop-recreate cases) | **~1–1.5 days** | Routine: the fold, the queries, the fixtures. Genuinely careful parts: the statement splitter (dollar-quoted function bodies, nested comments) and getting the §4 honesty phrasing into every finding. No new dependencies. |
| (c) Layer-2 `NativeArchCheckRunner` + composition into `CombinedCheckRunner` + parity note | **~0.5–1 day** | `ManifestCheckRunner` is a near-line-for-line template. Risk: low. |
| Deferred: `syn`/tree-sitter code-AST layer + api-layer checkers | 2–4 days when due | The genuinely hard tail: multi-language parsing, symbol resolution, boundary-map config format. **Not needed for RLS.** |
| Deferred: CI distributable (`camerata-check` binary) | ~1 day when due | Blocked on wanting Layer-3 parity for native checks at all (§2.4). |

**Total for seam + RLS checker + gov-dev wiring: roughly 2.5–3.5 orchestrated days.** Reused
wholesale: the scan's file walk and in-memory file set, the `Finding`
suppression/baseline/report pipeline, `CheckRunner`/`CombinedCheckRunner`, the manifest-runner
subprocess-and-composition pattern, the existing proof checker, the config.toml parse specified
for SUPABASE-EXPOSURE-SCHEMAS-1, and the corpus TOMLs (already authored, `grounded`, with the
replay semantics written into their `qualifies` fields).

---

## 6. Pass 1 landed (2026-07-26)

Scope actually shipped: **(a) the seam + (b) the two Supabase checkers, wired into the
brownfield SCAN only** — exactly §2.1 + §2.2 + §3, narrowed per the build order (Layer-2
wiring from §2.3/(c), the `syn`/tree-sitter code-AST layer, and the api-layer checkers are
explicitly deferred to Pass 2, below).

**The seam** — `crates/checks/src/arch_checker.rs`: `ArchChecker` trait
(`rule_ids`/`interest_globs`/`check`), `RepoView<'a>`, `ArchViolation` (rule_id, file, line,
object, message, severity), a minimal segment-anchored glob matcher (`glob_match`,
`checker_applies`, `any_file_matches_globs` — no crate dependency, three fixed literal
patterns didn't warrant one), and the registry (`all_checkers`, `all_checker_rule_ids`).

**`SupabaseRlsChecker` + `SupabaseFnSearchPathChecker`** — `crates/checks/src/supabase/`:
- `splitter.rs`: the dollar-quote/comment-aware statement splitter. Operates on `Vec<char>`
  (never raw bytes, so no UTF-8 boundary panics), handles `'...'`/`"..."`/`$$...$$`/`$tag$...$tag$`,
  `--` and `/* */` comments, CRLF normalization, and degrades to a best-effort flush on
  EOF for every unterminated construct (string, comment, dollar-quote) — never panics,
  by construction and by a dedicated adversarial test battery (unterminated everything,
  garbage/binary-ish input, positional `$1` vs a dollar-quote open, multi-statement lines).
- `sql_parse.rs`: a shallow, panic-free DDL classifier over already-split statement text
  (`CREATE/DROP TABLE`, `ALTER TABLE ... RENAME/ENABLE|DISABLE ROW LEVEL SECURITY`,
  `CREATE/DROP POLICY`, `CREATE [OR REPLACE] FUNCTION` with a `SECURITY DEFINER`/
  `SET search_path` scan scoped to the statement's SIGNATURE only — before the first
  dollar-quoted body — so a string the function builds can never spoof the clause).
- `timeline.rs`: the fold shared by both checkers — `CREATE`/`DROP`/`RENAME TABLE` and
  `ALTER ... ROW LEVEL SECURITY` and `CREATE`/`DROP POLICY` replayed in migration-filename
  order (then `supabase/schemas/*.sql`, folded last — the declarative shortcut falls out
  for free from file ordering, no special-casing needed), keyed `(schema, table)` /
  `(schema, function)` in a `BTreeMap` for deterministic finding order.
- `config.rs`: `supabase/config.toml` `[api].schemas` parse, default `{"public"}` per this
  build's spec (documented as a deliberate narrowing vs. Supabase's own CLI default of
  `["public","storage","graphql_public"]`, which `SUPABASE-EXPOSURE-SCHEMAS-1` documents).
- `rls_checker.rs` / `search_path_checker.rs`: the three RLS queries + the search-path
  query over the folded `Timeline`, each finding carrying the table/function name, the
  establishing `file:line`, and the honesty caveat verbatim.

**Scan wiring** — `crates/server/src/onboard.rs` (`audit_repos`, inside the
`if run_deterministic` block, right after the floor call) + the new adapter module
`crates/server/src/onboard/architectural.rs` (`audit_architectural`,
`arch_violation_to_finding`) — `camerata-checks` cannot depend on `camerata-server`'s
`Finding` type, so the adapter lives on the consuming side, exactly as this memo's own
module doc anticipated. Findings are tagged `preview: true, preview_tool:
Some("camerata-arch")`, reusing the EXISTING preview/authority-column mechanism rather than
adding a new field (an architectural finding is, today, precisely "deterministic but not yet
wired into a write-time gate" — Pass 2 is what would change that). **LLM-exclusion**: a
`HashSet` built once from `arch_checker::all_checker_rule_ids()` is subtracted from each
repo's semantic (LLM) rule set the same way `camerata_gateway::lookup_arm`-covered rules
already are, immediately above it in the same filter chain.

**Tests**: 322 passing tests in `camerata-checks` (up from ~290) covering the splitter,
the classifier, the timeline fold (enable-then-disable, rename-crossing-enable/disable,
drop-then-recreate, RLS-in-a-later-migration, declarative-schema-shortcut,
`IF NOT EXISTS` idempotency, `CREATE OR REPLACE` supersession), and both checkers
(exposed/non-exposed severity split, config.toml multi-schema, policy-without-RLS,
RLS-without-policy, policy-on-a-non-exposed-schema-table, empty/junk migration dirs,
zero-file skip). `camerata-server`'s onboard suite gained the adapter's own unit tests
plus a new end-to-end pair in `crates/server/tests/architectural_executor_e2e.rs` against a
committed fixture (`tests/fixtures/supabase_rls_repo/`) proving the finding fires with the
exact table/file/line through `audit_repos`, AND that it survives `report_export::
build_report_json` → `compile_pdf` as a real `%PDF` — the full report spine, not just the
scan. `cargo check --workspace` and both crates' full test suites are green.

**What's left for Pass 2** (deliberately not built here, per this task's scope):
- The Layer-2 `NativeArchCheckRunner` (§2.3) — composing the same registry into
  `CombinedCheckRunner` beside `ManifestCheckRunner` so a governed-dev write gets bounced
  in-loop on an architectural violation, not just flagged at scan time.
- The `syn`/tree-sitter code-AST layer and the api-layer checkers (`handler-no-db`
  promotion, `strict-layering`, `no-cross-boundary-imports`) — still routed, per §2.1's
  own note that RLS never needed this layer.
- Layer-3 CI parity (§2.4) — unchanged, still accepted as an asymmetry.
- `SUPABASE-RLS-USER-METADATA-1` floor-porting and any other rule this memo flagged as a
  floor-port *candidate* — out of scope; this pass shipped `architectural` tier only, per
  the explicit instruction not to touch floor wiring.

---

## 5. Recommendation

**BUILD-NOW, narrowly: phases (a)+(b) — the seam plus the RLS checker, scan surface first —
with (c) immediately behind them; DEFER the code-AST layer and the CI distributable on crisp
triggers.** The defer-everything option is legitimate, but it has a concrete cost right now:
the four Supabase RLS-family rules were just authored at `architectural` tier with replay
semantics written into their grounding, spec §4 explicitly bans shipping them at regex tier,
and without an executor they are AI-advisory only — which means the flagship finding of the
paid Supabase audit ("your `profiles` table has no RLS, established at
`20240301_init.sql:14`") cannot be demonstrated deterministically during exactly the outreach
push it's meant to sell. At ~2.5–3.5 orchestrated days with ~80% of the plumbing already
standing, the seam is cheap, it is the class-level answer Zach asked for (16 rules eventually
live behind it, 4 nearly free once the migration-timeline model exists), and it converts the
tier from a schema promise into a product capability. Defer triggers for the tail: build the
`syn`/tree-sitter layer when the first api-layer checker is actually demanded by an engagement;
build the CI distributable when a client wants native checks enforced in *their* CI rather
than via the existing CI-story path.
