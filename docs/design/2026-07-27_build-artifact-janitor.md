# Build-Artifact Janitor — Design

**Problem:** 195 GB of accumulated Rust `target/` filled the dev disk to 99%. Camerata has a headroom GUARD (`check_build_disk_headroom`, `crates/checks/src/lib.rs:233`; `ensure_disk_headroom`, `crates/server/src/workspace.rs:925`) and a dedup (`.camerata-shared-target`, `workspace.rs:852`) — but NOTHING ever reclaims. The guard blocks new work on a full disk and tells the *human* to clean up. This doc designs the janitor that does it instead, for any language.

**Key incident fact shaping the design:** the 195 GB culprit was the USER'S OWN `target/` from manual dev builds in the host repo — NOT Camerata scratch. So a janitor that only cleans Camerata's own mess would not have prevented this incident; one that silently deletes user build caches is a different kind of incident. That tension is the §3 routing question.

---

## 1. Artifact registry (per-language)

A static registry in `camerata-checks` (beside `multilang::detect_language`, which already knows the language markers) mapping language → known build-artifact dirs:

| Language | Marker (existing detect) | Artifact dirs | Notes |
|---|---|---|---|
| Rust | `Cargo.toml` | `target/`, `.camerata-shared-target/` | shared-target is Camerata-owned |
| JS/TS | `package.json` | `node_modules/`, `dist/`, `build/`, `.next/`, `.turbo/`, `.vite/`, `coverage/` | `node_modules` is "expensive to rebuild" tier (see below) |
| Python | `pyproject.toml`/`setup.py` | `__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `*.egg-info/`, `.tox/`; `.venv/` (expensive tier) | |
| JVM | `build.gradle*`/`pom.xml` | `build/`, `.gradle/`, `target/` (Maven) | `build/`+`target/` only counted when marker present — avoids nuking a source dir named `build/` |
| Go | `go.mod` | none in-repo (`GOPATH`/build cache are global) | out of scope v1; note in docs |
| Ruby / C# | `Gemfile` / `*.csproj` | `vendor/bundle`, `tmp/cache` / `bin/`, `obj/` | v1.1 |

Registry shape:

```rust
struct ArtifactSpec {
    dir_name: &'static str,        // matched as a path SEGMENT, not substring
    language: WorktreeLanguage,
    requires_marker: bool,          // e.g. `build/` only if build.gradle present
    tier: ArtifactTier,             // Cheap (rebuild = one command) | Expensive (node_modules, .venv)
}
fn builtin_registry() -> &'static [ArtifactSpec]
```

**Extension:** two seams. (a) Add rows to `builtin_registry()` (compile-time, PR'd like any rule). (b) Repo-local additions via `.camerata/checks.toml` — a new optional `[janitor] extra_artifact_dirs = ["..."]` table in the existing manifest (`crates/checks/src/manifest.rs`), since that file is already the per-repo single source of truth for check config. No third config system.

**Hard safety invariants on every deletion, regardless of trigger** (§3 expands):
1. Path's final segment must match a registry `dir_name` (or `extra_artifact_dirs`).
2. Dir must be git-ignored or invisible to git (`git check-ignore` / outside worktree roots like `.camerata-shared-target`). A *tracked* `dist/` is never touched.
3. Never follow symlinks; delete the link, not the destination.
4. Every deletion logged (path, bytes, trigger, timestamp) before the `remove_dir_all`.
5. Everything deleted must be reversible-by-rebuild by construction (that's what the registry *is*).

---

## 2. Triggers — recommended set (all four)

**T1 — Worktree teardown (extend existing).** `remove_uow_worktree` (`workspace.rs:791`) already removes the whole worktree dir, which takes any worktree-local artifacts with it. Gap: when `derive_shared_target_dir` returned `None` (out-of-band worktree fallback) a stray `target/` may sit inside — already covered by `remove_dir_all`. T1 is therefore mostly **already done**; the only addition is running T2's shared-target cap check right after teardown, since that's the moment artifacts just stopped being hot.

**T2 — Shared-target cap (new, the core of the janitor).** `.camerata-shared-target` currently grows without bound — it's the Camerata-scratch analog of the 195 GB incident. Policy: after each teardown and on the T3 startup sweep, if the dir exceeds `max_gb`, prune. Cargo's target layout doesn't support clean per-crate LRU, so v1 prune = **whole-dir removal when over cap or when no worktrees remain for that clone** (rebuild cost is real but bounded; cap default generous at 30 GB). v1.1 can refine to `cargo clean --profile` / mtime-based subdir pruning. Age rule: shared-target older than `max_age_days` (default 14) since last mtime AND no live worktrees → remove.

**T3 — Startup sweep (extend existing).** The startup task in `server/src/lib.rs:~1370` already removes worktrees of signed-off UoWs + prunes admin records. Extend the same loop: for each known clone, (a) remove `.camerata-worktrees/` subdirs not registered in `git worktree list` (true orphans from crashes), (b) apply T2 to `.camerata-shared-target`.

**T4 — Headroom guard reclaims before it blocks (extend existing).** In `ensure_disk_headroom` call sites (`ensure_uow_worktree` at `workspace.rs:709`; `check_build_disk_headroom` in `checks/src/lib.rs`): on headroom failure, first invoke the janitor over **Camerata-owned scratch only** (all known clones' orphan worktrees + shared-targets, most-stale first), re-check headroom, and only bail if still short. The bail message then also reports what was reclaimed and — if host-repo artifacts were *observed* — names them as the remaining candidates ("your repo's `target/` is 47 GB; run camerata janitor --host or `cargo clean`"). Guard becomes: **reclaim → re-check → block with actionable inventory**.

Not recommended: a background timer/daemon. The four event-driven triggers cover every growth path; a timer adds a scheduler and a new failure mode for marginal benefit.

---

## 3. The safety decision — ROUTE TO ZACH

**Question: does the janitor ever auto-delete artifacts in the HOST repo (the user's own clone / dev checkout), e.g. the user's own `target/`?**

**Proposed default: NO — two-zone model.**

- **Zone A, Camerata-owned scratch** (`.camerata-worktrees/*`, `.camerata-shared-target/`): fully automatic, aggressive. Camerata created it; deleting it can never lose user state (worktree teardown already honors LIFECYCLE-9's live-run guard, and branches survive teardown by design).
- **Zone B, host repo** (user's `target/`, `node_modules/`, `.venv/`, ...): **measure + warn + offer, never silently delete.** The janitor *scans* Zone B (registry-matched, gitignore-verified dirs only), reports sizes in the headroom-failure message and a UI surface, and exposes an explicit reclaim action (opt-in per repo via config, or one-click per event). `node_modules`/`.venv` (Expensive tier) additionally require per-invocation confirmation even when the repo is opted in, because rebuild cost is minutes-and-network, not one command.

Rationale: the incident dir *was* Zone B, so it's tempting to auto-clean it — but a user's `target/` can hold an incremental-compile state they're actively relying on mid-debug, and `.venv` deletion can strand a running process. "Camerata deleted my build cache while I worked" erodes exactly the trust a governance tool runs on. The warn-and-offer path still *prevents* the incident (the disk never silently fills — the guard fires at 10 GB free with a named, sized, one-click remedy) without ever surprising the user. This also matches the existing stance in `workspace.rs`'s LOCATION comment: Camerata's scratch "never pollutes the user's repo" — the inverse courtesy is not touching what's theirs.

The §1 invariants (registry-segment match, gitignore check, no symlink follow, logged, rebuild-reversible) apply to BOTH zones; Zone B adds the consent layer on top. Dry-run mode (`--dry-run` / config flag) prints the would-delete inventory for either zone.

---

## 4. Config surface

Extend the existing `CAMERATA_MIN_DISK_HEADROOM_GB` family (env var + same parse pattern as `parse_disk_headroom_gb`), not a parallel system:

| Var | Default | Meaning |
|---|---|---|
| `CAMERATA_MIN_DISK_HEADROOM_GB` | 10 | existing — block threshold (now: reclaim-then-block) |
| `CAMERATA_SHARED_TARGET_MAX_GB` | 30 | T2 cap per clone's shared target |
| `CAMERATA_ARTIFACT_MAX_AGE_DAYS` | 14 | T2/T3 staleness cutoff for unowned scratch |
| `CAMERATA_JANITOR` | `on` | `on` / `dry-run` / `off` (kill switch; Zone A only — Zone B is never auto anyway) |

Per-repo (in `.camerata/checks.toml` `[janitor]`): `extra_artifact_dirs = [...]`, `host_reclaim_opt_in = false` (Zone B one-click without re-confirm, Cheap tier only), optional `disabled_languages = [...]`. Env = machine-wide policy; manifest = repo-scoped nuance. Precedence: env kill-switch > repo config.

---

## 5. Build order & estimate

| Step | What | Est. |
|---|---|---|
| 1 | Registry module + safety invariants (`is_reclaimable(path)`: segment match, marker check, `git check-ignore`, symlink guard) + logged `reclaim(path)` + tests | 0.5 day |
| 2 | T2 shared-target cap + T3 startup-sweep extension (orphan worktrees + cap) + tests | 0.5 day |
| 3 | T4 reclaim-before-block in both guard sites + actionable-inventory message + tests | 0.5 day |
| 4 | Zone B scan/report + opt-in reclaim endpoint/UI surface + dry-run + docs | 1 day |

**Total ≈ 2–2.5 days of orchestrated work** (raw instinct said ~2 weeks; corrected per the ~5x overestimation pattern — nothing here is algorithmically novel; it's fs walks, one syscall, and plumbing into three existing seams). Steps 1–3 ship together as the incident fix; step 4 can trail. Every step ships with docs + tests per standing rule.

### Routes to Zach before building
1. **Zone B policy (§3):** confirm warn-and-offer default; is per-repo `host_reclaim_opt_in` wanted at all, or is one-click-per-event enough? (Chief decision — everything else is buildable either way.)
2. **T2 prune granularity:** accept v1 whole-dir shared-target removal at cap (simple, occasionally re-pays a full build) vs. requiring mtime-subdir pruning in v1 (more code, gentler)?
3. **Defaults sanity:** 30 GB cap / 14-day age / Expensive-tier confirm-always — bless or adjust. (Bundled; not load-bearing.)

---

## What shipped (2026-07-27)

Zach confirmed the §3 split (Zone A auto-reclaim / Zone B warn-only) and the v1 whole-dir shared-target prune. This section is the as-built ledger against the design above — read it alongside the design, not instead of it; a few things shipped narrower than originally sketched, called out explicitly rather than silently.

### Registry — `crates/checks/src/janitor.rs`

`builtin_registry() -> &'static [ArtifactSpec]` covers Rust (`target/`, unmarked), Camerata infra (`.camerata-shared-target/` exact-match; `.camerata-worktrees/` as a **container** match — see below), JS/TS (`node_modules/` Expensive; `dist/`, `.next/`, `.turbo/`, `.vite/`, `coverage/`, `build/` all Cheap and marker-gated on `package.json`), Python (`__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `.tox/` Cheap; `.venv/` Expensive), and JVM (`build/` + `.gradle/` marker-gated on `build.gradle`/`build.gradle.kts`; `target/` marker-gated on `pom.xml`, a SEPARATE entry from Rust's unmarked `target/` — a repo can match either or both). Go/Ruby/C# are out of scope for v1 exactly as designed; a repo in one of those languages can still opt in via `extra_artifact_dirs`.

**One registry-shape change from the design sketch:** `.camerata-worktrees` can't be matched as a fixed `dir_name` the way `target/` can, because its CHILDREN are per-branch and unpredictably named (`camerata__story-7`). So `ArtifactSpec` gained a `kind: SpecKind` field — `ArtifactDir` (match the path's own final segment; every language entry) vs. `OrphanContainer` (match the path's PARENT's final segment; used only for `.camerata-worktrees`). A container match is necessary but not sufficient: the Zone-A wiring additionally requires the child NOT be a currently-registered `git worktree` before it's ever handed to `reclaim_dir`. This keeps invariant 1 ("must match the registry") literally true for orphan-worktree reclaim too, rather than carving out an exception for it.

**Extension seams, both built:** (a) add a row to `builtin_registry()`; (b) `.camerata/checks.toml`'s new `[janitor]` table (`crates/checks/src/manifest.rs`) — `extra_artifact_dirs: Vec<String>`, `host_reclaim_opt_in: bool`, `disabled_languages: Vec<String>`. `disabled_languages` and `host_reclaim_opt_in` are parsed and available on `CheckManifest` now; they are not yet READ by any call site (no Zone-B UI/endpoint exists yet — see the Zone-B section below), so they are inert config today. `extra_artifact_dirs` IS wired through to `classify_artifact`/`scan_zone_b` today.

### The five invariants — where enforced, where tested

All five are enforced in one seam, `evaluate_reclaim` (`crates/checks/src/janitor.rs`), which `reclaim_dir` calls before ever touching the filesystem:

1. **Registry match** — `classify_artifact`. Tested: `classify_rust_target_matches_no_marker_needed`, `classify_gradle_build_requires_marker` (asserts an UNMARKED `build/` is rejected, then accepted once `build.gradle` appears beside it), `classify_maven_target_requires_pom_marker`, `classify_extra_artifact_dirs_matches_repo_declared_name`, `classify_unrelated_directory_never_matches`, `classify_orphan_container_matches_any_child_name`, plus the end-to-end refusal test `evaluate_reclaim_refuses_no_registry_match_even_when_gitignored` (a gitignored-but-unregistered dir is still refused — invariant 1 is independent of invariant 2).
2. **Gitignore-verified** — `check_gitignore_status` (shells to `git check-ignore --quiet`), bypassed only for `is_camerata_infra` paths (structurally outside every per-UoW worktree's git view — see module doc). Tested: `evaluate_reclaim_allows_gitignored_target_in_real_repo`, `evaluate_reclaim_refuses_tracked_directory_even_with_matching_name` (a `target/` dir with COMMITTED source inside — the literal incident-adjacent adversarial case), `evaluate_reclaim_refuses_untracked_but_not_gitignored_directory` (the "honest not-ignored" case, distinct from tracked), `evaluate_reclaim_refuses_when_not_inside_a_git_repo_and_not_infra` (fails closed, not open, when it can't verify), `evaluate_reclaim_bypasses_gitignore_for_camerata_infra`.
3. **No symlink following** — `is_safe_to_reclaim`: refuses if the path itself is a symlink, AND independently refuses if the canonicalized path resolves outside the canonicalized root (an ancestor-symlink escape). Tested: `safe_to_reclaim_refuses_symlinked_artifact_dir`, `safe_to_reclaim_refuses_ancestor_symlink_escaping_root` (plants `root/subdir -> symlink -> outside/`, `outside/target/` is the "victim"; asserts it survives), `evaluate_reclaim_never_deletes_through_symlink_escape_end_to_end` (full `reclaim_dir` call, asserts the log callback never fires AND the victim files survive), `adversarial_symlinked_node_modules_is_never_reclaimed`.
4. **Logged before deletion** — `reclaim_dir` calls the caller's `on_log` closure and emits a `tracing::info!` BEFORE `remove_dir_all` runs, unconditionally (including dry-run). Tested with a closure that checks `path.exists()` AT LOG TIME: `reclaim_dir_logs_before_deleting_camerata_infra` asserts the path is still on disk when `on_log` fires and gone after the call returns — this proves ordering, not just that both things eventually happened.
5. **Rebuild-reversible by construction** — no separate runtime check; this IS what registry membership means (a rebuild command or `git worktree add` recreates everything the registry names). Documented in the module doc rather than gated in code, per the design.

**Adversarial layouts tested** (the task's required set, all present): a directory literally named `target/` that is TRACKED SOURCE (`evaluate_reclaim_refuses_tracked_directory_even_with_matching_name`) — committed via a real `git commit`, not just gitignore-absent; a symlinked `node_modules` (`adversarial_symlinked_node_modules_is_never_reclaimed`); a `.git` directory nested INSIDE an artifact dir (`adversarial_git_inside_artifact_dir_is_still_gitignore_verified_by_outer_repo` — asserts the OUTER `target/` still reclaims normally against the outer repo's gitignore, and that `scan_zone_b` never reports the nested `.git` as its own candidate); a symlink escaping the root entirely (`safe_to_reclaim_refuses_ancestor_symlink_escaping_root`, `evaluate_reclaim_never_deletes_through_symlink_escape_end_to_end`).

**No-panic coverage:** `dir_size_never_panics_on_missing_dir` (vanished dir), `dir_size_skips_symlinked_children_without_following` (never follows a symlink even for measurement), and every fs-touching function in the module returns `Result`/`Option`/a default rather than unwrapping — `dir_size`, `classify_artifact`, `list_orphan_worktree_dirs`, `has_live_worktrees`, `check_gitignore_status`, `registered_worktree_paths` all treat I/O failure as "can't tell, be conservative" rather than propagating a panic. `has_live_worktrees` specifically fails CLOSED (assumes live worktrees exist) on any `git` query failure, so a transient git error can never cause an eager, wrong prune.

### Zone A — triggers wired

- **T1 (worktree teardown)** — already existed (`remove_uow_worktree` removes the whole worktree dir); no change needed beyond what T2 adds at the same call site.
- **T2 (shared-target cap/age/orphan)** — `should_prune_shared_target` (pure OR of three named predicates: `shared_target_over_cap`, `shared_target_orphaned` i.e. zero live worktrees for the clone, `shared_target_stale`) is checked at TWO call sites: `remove_uow_worktree` (`crates/server/src/workspace.rs`, via the new `pub(crate) async fn maybe_prune_shared_target`) right after every teardown, and the T3 startup sweep (below), reusing the exact same function so the two triggers can never diverge in policy. Tested end-to-end with a real git repo + real worktrees: `remove_uow_worktree_prunes_now_orphaned_shared_target` (tearing down a clone's LAST worktree prunes it) and `remove_uow_worktree_does_not_prune_shared_target_while_a_sibling_worktree_is_live` (a second live worktree on the same clone protects it) in `crates/server/src/workspace.rs`.
- **T3 (startup sweep)** — extended in `crates/server/src/lib.rs`'s existing per-UoW worktree housekeeping task (Pass 2 loop, right after the pre-existing `prune_worktrees` admin-record prune): for each known clone, remove any `.camerata-worktrees/*` subdir NOT in `git worktree list` (`camerata_checks::janitor::list_orphan_worktree_dirs`, itself tested in `crates/checks/src/janitor.rs` against a real git repo with one registered + one stray worktree — `list_orphan_worktree_dirs_finds_unregistered_stray_directory` asserts the registered one is NEVER listed), then applies the same `maybe_prune_shared_target` T2 policy.
- **T4 (headroom guard reclaims before blocking)** — upgraded at BOTH existing call sites: `crates/checks/src/lib.rs::check_build_disk_headroom` (the three cargo `CheckRunner`s) and `crates/server/src/workspace.rs::ensure_disk_headroom` (`ensure_uow_worktree`). Both now call the shared pure orchestrator `camerata_checks::janitor::check_headroom_with_reclaim(min, available_disk_bytes_fn, reclaim_fn)`, which queries headroom, and ONLY on a shortfall runs `reclaim` once and re-queries — never reclaims when headroom was already fine. The reclaim step is `reclaim_zone_a_for_clone`: orphan-worktree sweep + an EMERGENCY unconditional shared-target prune (disk is critically low right now, so the T2 cap/age policy is bypassed in favor of "just reclaim it"). On a still-insufficient result, the bail message reports bytes actually reclaimed and appends a Zone-B inventory (see below).

  **Scope note (a deliberate narrowing from the design sketch):** the design's T4 language ("reclaim across ALL known clones, most-stale-first") would require the pure `checks::check_build_disk_headroom(worktree: &Path)` function to know about every project's clone list, which it structurally doesn't (it only ever sees one worktree path). What shipped instead reclaims Zone A scoped to the CURRENT clone only (derived from the worktree path, same derivation `derive_shared_target_dir` already uses) at both call sites. This is a real, intentional scope reduction versus the design doc's literal wording, not a silent gap: the periodic T3 startup sweep already reaches every known clone on its own cadence, so the acute "disk is full right now" moment (T4) fixing its OWN clone first is the highest-value, lowest-blast-radius slice; a follow-up could thread a clone-list into a server-side T4 variant if the single-clone reclaim proves insufficient in practice.

- **Pure, injectable seam for T4** (per the task's explicit ask to avoid requiring a real low-disk machine): `check_headroom_with_reclaim` takes `available_disk_bytes: impl FnMut() -> Option<u64>` and `reclaim: impl FnMut() -> u64` as injected closures. Tests in `crates/checks/src/janitor.rs` drive all three outcomes with canned closures: `headroom_sufficient_never_calls_reclaim` (reclaim must NOT run when already fine), `headroom_short_then_reclaim_succeeds` (`ReclaimedSufficient`), `headroom_short_then_reclaim_still_insufficient_blocks` (`StillInsufficient`), plus the fail-open `headroom_cannot_query_fails_open` / `headroom_cannot_query_after_reclaim_also_fails_open`. Separately, `crates/server/src/workspace.rs` has two REAL-DISK integration tests (`ensure_disk_headroom_reclaims_zone_a_scratch_before_blocking`, `ensure_disk_headroom_still_insufficient_names_zone_b_candidates_never_deletes`) that prove the real wiring — not just the pure decision function — actually reaches and executes the reclaim and the Zone-B scan against real files on disk.

### Zone B — measure + warn, one-click remedy named, NEVER auto-deleted

`scan_zone_b(root, extra_dirs) -> Vec<ZoneBCandidate>` walks a repo/worktree tree for registry-matched, gitignore-verified, symlink-safe candidates (i.e., anything that WOULD pass `evaluate_reclaim` with `is_camerata_infra = false`), sized, sorted largest-first. It is measurement-only by construction: it never calls `remove_dir_all`, and it does not recurse into a matched artifact dir (so a vendored `node_modules` nested inside `node_modules` is reported once, not twice — tested in `scan_zone_b_does_not_recurse_into_a_matched_artifact_dir`).

The T4 bail message (both call sites) calls `scan_zone_b` on the shortfall clone/worktree and, if it finds anything, replaces the generic "reclaim space" remedy with `format_zone_b_inventory` — up to 5 candidates, each with a GB size, tier label, and a literal `rm -rf <path>` command the user can run. Nothing in this module or its call sites ever acts on a `ZoneBCandidate` automatically.

**What did NOT ship (the "opt-in reclaim path" from §3):** a dedicated one-click Zone-B reclaim UI/endpoint, and the manifest's `host_reclaim_opt_in` flag being READ anywhere. The design explicitly allowed this ("Full Zone-B UI can be minimal here — the warn + an opt-in reclaim path is the incident fix"); what shipped is the "warn" half plus a manual remedy command, not yet a wired one-click button. Since `scan_zone_b`'s candidates already pass every safety invariant, wiring a one-click endpoint later is "call `reclaim_dir` on a candidate from a UI action, gated on an explicit per-invocation confirmation" — no new safety logic required, which is why this was the right piece to defer under the time-box.

### Config surface — extends the existing family, no parallel system

| Var | Default | Read by |
|---|---|---|
| `CAMERATA_MIN_DISK_HEADROOM_GB` | 10 GB | unchanged (existing) |
| `CAMERATA_SHARED_TARGET_MAX_GB` | 30 GB | `janitor::shared_target_max_bytes()` — T2 |
| `CAMERATA_ARTIFACT_MAX_AGE_DAYS` | 14 days | `janitor::artifact_max_age()` — T2 |
| `CAMERATA_JANITOR` | `on` | `janitor::janitor_mode()` — gates the RECLAIM step at every Zone-A call site (T2/T3/T4); `off` restores pre-janitor pure-block/pure-teardown behavior; `dry-run` logs every would-be reclaim without deleting. Zone B is unaffected either way (never automatic regardless of this switch) |

`.camerata/checks.toml`'s new `[janitor]` table (`JanitorConfig` in `crates/checks/src/manifest.rs`): `extra_artifact_dirs` (wired), `host_reclaim_opt_in` and `disabled_languages` (parsed, not yet consumed — see Zone-B note above). All fields default to the conservative value when the table or file is absent, so every existing repo's `.camerata/checks.toml` (or lack of one) is unaffected.

### Test counts

- `cargo test -p camerata-checks`: 582 passed (lib unit tests, including 50 new janitor tests) + 4 + 2 + 4 + 4 + 4 + 31 + 1 doc-test across the crate's integration-test binaries and doc-tests — 0 failed.
- `cargo test -p camerata-server`: 1203 passed (lib unit tests, including 6 new workspace.rs janitor-wiring tests: 4 headroom-guard T4 tests + 2 T2 teardown-prune tests) + the full set of existing integration-test binaries (onboarding, exports, UoW lifecycle, VCS gate, etc.) + 2 doc-tests — 0 failed.
- `cargo check --workspace`: green (pre-existing warnings only, none introduced by this change).

### Confirmed: Zone B is never silently deleted

Every path in this module that can reach `remove_dir_all` is `reclaim_dir`, and every call site that invokes it against a Zone-B path does so with an explicit, hardcoded `is_camerata_infra = false`. There is, as of this ledger, **no call site anywhere in the codebase that calls `reclaim_dir` on a Zone-B candidate** — `scan_zone_b`'s results are surfaced only in bail-message text (`format_zone_b_inventory`) and returned as data (`Vec<ZoneBCandidate>`) for a future UI/endpoint to act on with explicit user confirmation. `CAMERATA_JANITOR=off` additionally kills the Zone-A reclaim step at every trigger without touching Zone-B's already-manual posture — there is no combination of config or trigger that causes an automatic Zone-B deletion today.
