# Cockpit UI — Product Wave (pw/cockpit-ui)

**Date**: 2026-06-21
**Branch**: pw/cockpit-ui
**Status**: Implemented
**Files**: `crates/ui/src/cockpit.rs`, `crates/ui/src/style.rs`

---

## Context

The pw/cockpit-ui product wave adds five features to the cockpit UI that
were blocked on server-side endpoints shipping in a prior wave. All five
features touch only the UI crate (cockpit.rs + style.rs); no server crate
was modified. Server endpoints are called optimistically — every async fn
returns `Option<T>` or a default so the UI degrades gracefully if a
server is older than the current UI build.

---

## Feature 1 — AUTO-RECOMMEND display

**Decision**: Onboarding pre-checks only rules where `effective_auto_recommended()` returns true.

`effective_auto_recommended()` is the single truth-gate. It:
1. Returns `true` immediately if the server sent `is_auto_recommended: true`.
2. Falls back to `recommended && matches!(verification, "grounded" | "verified")` for old
   server payloads that omit the field (backward-compat; `#[serde(default)]` ensures the
   field is `false` when absent).

`draft` and `needs_recheck` rules appear LISTED but unchecked so the
architect must explicitly opt them in. The "Recommendation" column in
the proposed-rules table uses a checkmark badge ("Recommended") vs.
"Available" to make the distinction visible at a glance.

**Rationale**: The verification ladder (draft < grounded < verified) is the
corpus quality signal. Pre-checking unreviewed draft rules would silently
inject low-confidence rules into repo governance. The explicit server flag
gives the corpus team control without requiring a UI release.

---

## Feature 2 — Update-detection UI

### App-update banner

`AppUpdateBanner` polls `GET /api/release` on mount via `use_resource(fetch_app_release)`.
When `update_available == true` the banner is shown at the top of every view (Onboard,
Rules, Routines, Workspace, Docs, Stories). A dismiss button hides it for the session
(local signal; no server call). The banner links to release notes when provided.

### Rule-drift notice

`RuleDriftNotice` polls `GET /api/projects/:id/rule-drift`. Each `RuleDriftEntry`
carries `applied_directive` (what is currently in the repo) vs. `corpus_directive`
(what the corpus now says). The notice:

- Renders in `RulesView` after `RepoHealthPanel`.
- Returns `rsx! {}` when the drift list is empty (no visual noise on clean projects).
- Expands an inline side-by-side diff on click (old on left, new on right).
- Provides an "Update this rule" button that calls
  `POST /api/projects/:id/rule-drift/:rule_id/accept` and refreshes via a
  `use_signal(refresh)` counter idiom.

**Rationale**: Applied rules age out of sync with the corpus whenever the corpus
team revises a rule. The drift notice surfaces this without requiring the user to
manually re-run onboarding.

---

## Feature 3 — Single-rule editing UI

`SingleRuleEditor` is a modal overlay that allows scoped edits to an individual rule.

**Scope model**:
- `Project` scope: the chosen option applies to all repos in the project (no repo
  override). Server call: `POST /api/projects/:id/rules/:rule_id` with `{"scope":"project","chosen_option":"<opt>"}`.
- `Repo` scope: an override for one named repo. Server call adds `"repo":"<name>"` to the body.

The editor is triggered from a dropdown selector above the rules table in `RulesView`.
It renders as an overlay (ghost-click-eater via `onclick: e.stop_propagation()` on the
modal body) so it does not interfere with the Chorale table below.

**Rationale**: Per-repo overrides reflect real-world scenarios where org default
differs from a specific service (e.g., a stricter enforcement level for a core
security service). The two-level model (project vs. repo) matches the server's
provenance-ladder: project option is the default; repo option is an override.

---

## Feature 4 — Deep-report export button

`DeepReportExportPanel` is placed at the bottom of the deep-tier panel in `ScanResults`.
It calls `GET /api/projects/:id/deep-report?include_soc2=<bool>` and opens a modal
displaying the returned Markdown. A "Save to file" button delegates to the browser's
`rfd::AsyncFileDialog` (already used elsewhere in the codebase for file saves).

The `soc2_enabled` prop threads the Feature 5 flag into the export query param so
the exported Markdown matches what was rendered on screen.

**Rationale**: Deep-tier findings are advisory and often need to be shared with
security reviewers or compliance teams. A direct Markdown export avoids screenshot
workflows. The export is a separate deliberate action (not automatic) to prevent
accidental exfiltration of sensitive findings.

---

## Feature 5 — Feature-flag aware rendering

`FeatureFlagMap` is fetched once on mount in `ScanResults` via
`use_resource(fetch_feature_flags)` calling `GET /api/feature-flags`.

```rust
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct FeatureFlagMap {
    #[serde(default)] soc2: bool,
    #[serde(flatten)] extra: std::collections::HashMap<String, serde_json::Value>,
}
```

When `soc2 == false`:
- The `soc2-gap` lens is skipped entirely in the deep-tier rendering loop.
  `deep-security` and `threat-model` lenses still render unconditionally.
- A `.deep-soc2-disabled-notice` informs the user the affordance is off.
- The export call passes `include_soc2=false` so the Markdown excludes the SOC-2 section.
- The SOC-2 gap table (soc2_gaps rows) has a belt-and-suspenders guard inside the
  loop as well.

**Rationale**: SOC-2 readiness analysis is a distinct commercial affordance. The
feature flag lets the server control its visibility without a UI release. Default is
`false` (flags absent from old servers), so the feature is opt-in, not opt-out.

**Future flags**: `#[serde(flatten)]` on `extra: HashMap<String, Value>` absorbs
any future flags the server sends without requiring a UI update to avoid a parse error.
New flags become first-class fields when the UI needs to gate on them.

---

## CSS

All new CSS classes are in `crates/ui/src/style.rs` under the `GLOBAL_CSS` constant,
appended after the existing `.settings-label` block. The section is titled
`/* ── pw/cockpit-ui product wave ── */` with per-feature comments.

---

## Tests

14 new unit tests in `cockpit::tests`:
- 6 tests for `effective_auto_recommended` covering all combinations of
  server flag / recommended flag / verification level.
- 5 tests for `FeatureFlagMap` deserialization covering `soc2=true`,
  `soc2=false`, absent key, extra keys, and Default impl.

All 60 tests pass; no warnings.

---

## Alternatives considered

- **Feature 1**: Could have kept `recommended` as the single gate and added
  a separate `verification_gate` check at call site. Rejected: pushes the
  logic into callers (three call sites) rather than encapsulating in the type.

- **Feature 5**: Could have fetched flags at the app root and provided via
  context. Rejected: only `ScanResults` needs the flags today; premature
  context plumbing. If a second component needs flags, promote to context then.

- **Feature 3 scope model**: Considered a per-rule-file override (AGENTS.md
  vs CONVENTIONS.md). Deferred: the server does not expose that today.
