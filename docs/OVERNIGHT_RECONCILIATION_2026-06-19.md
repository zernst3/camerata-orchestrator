# Overnight reconciliation guide — 2026-06-19

20 unmerged branches were produced overnight (4 fan-out "waves" + 2 interactive feature branches).
**`main` is untouched at `774ab1f`. Nothing was pushed.** All branches are local + most are also on
`origin` only for waves 1's six branches (pushed before the no-push rule; left in place). This doc is
the map for merging them back: what each branch is, test status, files touched, the conflict
hotspots, and a suggested merge order.

> Verification legend: **[V]** = I personally ran `cargo check`/tests on the branch; **[A]** =
> agent-reported green (I did not re-run). The fan-out agents run their own `cargo test` before
> committing, so [A] is reliable, but [V] is certified by me.

## The branches

### Interactive feature branches (the two you asked for tonight)
| Branch | What | Tests | Files |
|---|---|---|---|
| `feat/scan-cost-controls` | **Incremental scan** (skip AI on unchanged files, carry findings forward, per-project cache, full-scan checkbox, rule-set auto-invalidation) + **rule-routing core** (language-scoped, conservative; wiring left ROUTE-1) + paced-scan design | **[V]** 147 server + 10 scan_cache + 7 scan_routing | ai_audit.rs, lib.rs, onboard.rs, scan_cache.rs (new), scan_routing.rs (new), suppression.rs, cockpit.rs, decision doc |
| `feat/apply-overwrite-warning` | Pre-apply detection of existing AGENTS.md/CONVENTIONS.md/CI/baseline + confirm dialog before overwrite | **[V]** compiles; [A] 133 server | arm.rs, lib.rs, cockpit.rs, style.rs |

### Wave 1 (phase2/*) — also on origin
| Branch | What | Tests | Note |
|---|---|---|---|
| `phase2/issue-11-precision-recall-eval` | precision/recall eval harness (CLI + server + UI) | [A] | touches cockpit + lib + docs |
| `phase2/issue-16-gate-adversarial-hardening` | gate hardening | [A] | **scope mismatch:** GH #16 is "Layer-2 bounce loop"; agent did gate-hardening per prompt. Re-title or re-scope |
| `phase2/issue-20-github-issue-intake` | story intake from GitHub issues | [A] | new github_issues.rs |
| `phase2/issue-21-writeback-provenance-signoff` | PR creation + provenance + sign-off | [A] | run.rs, uow.rs |
| `phase2/issue-29-max-iteration-loop-guard` | configurable max-iteration loop guard | [A] | core/fleet.rs, project.rs |
| `phase2/issue-48-python-corpus` | 11 Python corpus rules + detection | [A] | mostly new toml (low conflict) + onboard.rs |

### Wave 2
| Branch | What | Tests | Note |
|---|---|---|---|
| `wave2/issue-47-ast-rule-tier` | new `EnforcementKind::Architectural` + AST proof checker + 2 rules + doc | [A] | **structural** — routes the `ArchitecturalCheck` trait + `syn` dep to you (doc) |
| `wave2/issue-44-model-selector` | thread model through fleet/intake/run + run picker | [A] 267 | intake engine/review (conflicts w/ wave3 debt-remediation) |
| `wave2/issue-18-investigation-phase` | investigation/decision types + gate predicate + doc | [A] | new worktracker file; routes ArtifactKind/server/UI wiring (ROUTE-A/B/C) |
| `wave2/lint-test-hardening` | clippy `-D warnings` fix (incl. fleet gate_probe unwraps) + `cargo fmt` (37 files) + 40 tests | [A] | **MERGE LAST or re-run fmt** — the fmt pass touches 37 files and conflicts with everything |
| `wave2/debt-audit` | DEBT_INVENTORY doc (604 sites) | n/a | doc only — merge anytime |

### Wave 3
| Branch | What | Tests | Note |
|---|---|---|---|
| `wave3/issue-43-routine-escalation` | routine status enum + AI-translated escalation resume | [A] 138 | routines.rs, style.rs |
| `wave3/debt-remediation-integrations` | harden intake LLM-parse error paths | [A] | intake engine/review (conflicts w/ #44) |
| `wave3/issue-41-techdebt-boundaries` | repo↔issue boundary test + per-repo CSV; board-linkage routed (doc) | [A] | onboard.rs |
| `wave3/rustdoc-coverage` | rustdoc on core/rules/gateway | [A] | gateway/lib.rs only |

### Wave 4
| Branch | What | Tests | Note |
|---|---|---|---|
| `wave4/issue-22-clarify-bridge` | stable question-markers + idempotent posting + status + tests | [A] 191 wt / 132 srv | clarify.rs, clarify_marker.rs (new), cockpit.rs |
| `wave4/issue-19-github-auth` | typed GitHub auth errors + GET /user probe + 19 tests | [A] 149 | connections.rs, toast.rs (isolated) |
| `wave4/issue-31-guide-knowledge` | strengthen Guide-mode not-covered guardrail + 12 tests | [A] | chat.rs only (isolated) |
| `wave4/corpus-toml-audit` | **fixed 4 real corpus bugs** (rules with `default=true` but no `[decision].default`) + audit doc | [A] 27 | 4 toml + doc |

## Conflict hotspots (resolve carefully when these collide)

- **`crates/ui/src/cockpit.rs`** — ~11 branches touch it (phase2 #11/#16/#20/#21/#29/#48, wave2 #44,
  wave4 #22, scan-cost-controls, apply-overwrite-warning). The biggest hotspot.
- **`crates/server/src/lib.rs`** — ~10 branches (phase2 #11/#20/#21/#29, wave2 #44/#47, wave3 #43,
  scan-cost-controls, apply-overwrite-warning). Mostly additive handlers/routes — conflicts are
  usually "two new handlers near each other", resolvable by keeping both.
- **`crates/server/src/onboard.rs`** — phase2 #48, wave2 #47, wave3 #41, scan-cost-controls.
- **`crates/server/src/ai_audit.rs`** — wave2 #47, scan-cost-controls.
- **`crates/intake/{engine,review}.rs`** — wave2 #44 vs wave3 debt-remediation (both edit parse paths).
- **`crates/ui/src/style.rs`** — phase2 #29, wave3 #43, apply-overwrite-warning (each appends CSS
  INSIDE GLOBAL_CSS — append-conflicts, easy to resolve by keeping all blocks).
- **`crates/ui/src/routines.rs`** — wave2 lint, wave3 #43.
- **`docs/USER_GUIDE.md` / `docs/TECHNICAL.md`** — the wave-1 (phase2) branches edited these, but
  `main` already has the 774ab1f doc refresh. Prefer main's version and re-apply each branch's net-new
  doc additions (or drop the branch's doc edits — the features can be documented in one pass after merge).
- **`Cargo.lock`** — several; just `cargo update`/regenerate after merges, don't hand-merge.

## Suggested merge order

**Phase A — isolated, low-conflict (merge freely, almost no conflicts):**
1. `wave4/corpus-toml-audit` (4 toml bugfixes — genuinely fixes broken rules; merge early)
2. `wave2/debt-audit`, `wave3/rustdoc-coverage`, `wave2/issue-18` (docs / new files / gateway-doc)
3. `wave4/issue-31-guide-knowledge` (chat.rs only), `wave4/issue-19-github-auth` (connections/toast)
4. `phase2/issue-48-python-corpus` (mostly new toml)

**Phase B — feature branches (merge one at a time, expect cockpit.rs / lib.rs conflicts):**
5. `feat/scan-cost-controls` and `feat/apply-overwrite-warning` (tonight's asks — [V] verified)
6. `wave3/issue-43-routine-escalation`, `wave4/issue-22-clarify-bridge`, `wave2/issue-44-model-selector`
7. `wave3/debt-remediation-integrations` (after #44, since both touch intake)
8. `wave3/issue-41-techdebt-boundaries`, `phase2/#11/#20/#21/#29`
9. `wave2/issue-47-ast-rule-tier` (review the routed `syn`-dep/trait decision first)

**Phase C — last:**
10. `wave2/lint-test-hardening` — its `cargo fmt` pass (37 files) will conflict with everything
    merged above. **Recommended: skip merging this branch and instead, after all features are in,
    re-run `cargo fmt --all` + `cargo clippy --workspace --fix` on main yourself** (cheaper than
    resolving 37 files of fmt conflicts). The one piece worth cherry-picking explicitly: the
    `crates/fleet/src/gate_probe.rs` unwrap fix (3 lines) — it currently blocks `cargo clippy` on
    server/fleet across ALL branches.

## Known follow-ups surfaced by the agents
- **fleet gate_probe.rs unwraps** block `cargo clippy` on the whole workspace until fixed (the fix
  lives in wave2/lint-test-hardening; cherry-pick it or re-apply the 3-line change).
- **#16 branch ≠ #16 issue** — reconcile the title/scope.
- **scan rule-routing wiring** — ROUTE-1, see `docs/decisions/2026-06-19_scan_cost_controls.md`.
- **#47 ArchitecturalCheck trait + syn dep** — ROUTE-1, see its decision doc.
- **#18 ArtifactKind + server gate + cockpit tab** — ROUTE-A/B/C in its decision doc.
