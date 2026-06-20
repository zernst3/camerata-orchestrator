# UI Consolidation Wave (2026-06-20)

**Status:** Implemented — `dev5/ui-consolidation` branch

## Context

Waves 2 and 3 built out several backend capabilities (deep compliance audit tier, per-project model-tier map, ask-a-finding chat context, VCS gate settings) that were left as explicit UI follow-ups. This wave surfaces all four in the cockpit.

## Decisions

### 1. Deep compliance & security tier (#55)

**Decision:** Opt-in checkbox in the audit panel. Runs three extra whole-repo passes (SOC-2 gap analysis, deep security audit, threat model) after the standard audit and attaches the results as `report.deep`. ADVISORY only — never a SOC-2 report or a penetration test.

**Rationale:**
- The backend already produces `DeepReport`; the UI was the only missing piece.
- Opt-in by default: the tier is the most expensive (~3 extra whole-repo passes) and the output requires expert interpretation. Forcing it on would both inflate costs and mislead users about the output's legal standing.
- Advisory disclaimers are surfaced at three levels: the checkbox hint, the tier-level disclaimer at the top of the output panel, and per-lens before the findings.
- The cost readout is extended to note that the deep tier will materially increase the billed cost.

**Output structure rendered:**
- Per-lens sections (`soc2-gap`, `deep-security`, `threat-model`) with heading, description, disclaimer, summary, and either a SOC-2 gap table (for `soc2-gap`) or free-text detail (for the others).
- SOC-2 gap table uses traffic-light row highlighting: `gap` rows red, `partial` amber, `met` green.

**UI-side types added:** `Soc2GapView`, `DeepLensResultView`, `DeepReportView` (all in `cockpit.rs`).

**Wire-up changes:**
- `ScanReportView`: added `deep: Option<DeepReportView>`.
- `audit_against` / `audit_job_start`: added `deep: bool` parameter, forwarded as `"deep": deep` in the JSON body.
- `ScanResults`: `audit_deep` signal (defaults `false`), captured as `deep` before `spawn` so the closure owns it.

---

### 2. Model-tier map editor (#63)

**Decision:** A dev-console settings section in the Rules window ("SETTINGS: Model tier map") backed by a new `TierMapEditor` component. Saves via `POST /api/projects/:id/tier-map` (endpoint added in wave 3).

**Rationale:**
- Placed in the Rules window because that is already the "project configuration" surface the architect uses. Labeled SETTINGS (not rules) so it is visually distinct — it does not emit to AGENTS.md/CONVENTIONS.md, it controls runtime fleet behavior.
- All three bands (fast / balanced / strongest) are always sent in a single round-trip (patch semantics). Partial sends would allow the server default to silently "win" for the unsent band after a partial save, which is a hard-to-debug footgun.
- Validation: all three bands must be non-empty before saving. A toast surfaces the error if any are blank.

**Server side:** `POST /api/projects/:id/tier-map` added in `crates/server/src/lib.rs` (handler: `set_tier_map`). Patch semantics: only non-empty values in the request override the stored value.

**UI-side types added:** `TierMapView` (with serde defaults matching fleet defaults), `default_fast_model_str`, `default_balanced_model_str`, `default_strongest_model_str`.

**`ProjectView`:** added `tier_map: TierMapView` with `#[serde(default)]`.

---

### 3. Ask-a-finding (#54)

**Decision:** An "Ask AI about this finding" button in the findings toolbar (class `ask-finding-btn`) that builds a `FindingContext` from the first selected finding and writes it to an app-level `Signal<Option<FindingContext>>`. `ChatBubble` in `main.rs` receives the signal's value as its `finding` prop and auto-opens in Project mode focused on that finding.

**Rationale for lifting the signal to `App` (not `CockpitApp`):**
- `ChatBubble` is a sibling of `CockpitShell` in `App`'s rsx, not a descendant. Context only flows down the component tree.
- Lifting to `App` is the only topology-respecting approach. `CockpitApp` detects the signal via `use_context` (it was already provided by `App`) rather than `use_context_provider`.
- Only the first selected row is used when the button fires. Asking about multiple findings at once is deferred — one coherent conversation per finding produces better AI output.

**Wire-up:**
- `main.rs` `App`: `use_signal(|| Option::<chat::FindingContext>::None)` + `use_context_provider`.
- `CockpitApp`: `use_context::<Signal<Option<chat::FindingContext>>>()` (consume, not provide).
- `FindingsTable`: `use_context::<Signal<Option<chat::FindingContext>>>()` + the "Ask" button handler.
- `main.rs` `ChatBubble { finding: ask_finding() }` — passes the reactive value.

---

### 4. Commit / PR gate settings panel (#65)

**Decision:** `VcsGateSettings` (already in `vcs_settings.rs`) mounted in the Rules window as "SETTINGS: Commit / PR gate" — a distinct section after the custom rules and before export/import. Uses the existing `/api/projects/:id/process-rule-config` endpoints (GET + POST) with no new server code.

**Rationale:**
- The component existed and was complete; only the mount point was missing.
- Labeled SETTINGS (not rules) for the same reason as the tier-map editor: it configures runtime gate behavior, it does not contribute to the emitted ruleset.
- No prop-drilling changes needed: `VcsGateSettings` takes only a `project_id: String`.

---

## CSS additions (`crates/ui/src/style.rs`)

All new styles appended to `GLOBAL_CSS`:

- `.deep-tier-warning`, `.deep-tier-panel`, `.deep-tier-heading`, `.deep-tier-disclaimer`, `.deep-lens`, `.deep-lens-{heading,desc,disclaimer,summary,detail}` — deep tier output panel.
- `.soc2-gap-table`, `.soc2-gap-row`, `.soc2-gap-row.header`, `.soc2-gap-row.soc2-status-{gap,partial,met}`, `.soc2-badge-{gap,partial,met,unknown}`, `.soc2-col-{ctrl,gap}` — SOC-2 gap table.
- `.tier-map-editor`, `.tier-map-heading`, `.tier-map-hint`, `.tier-map-rows`, `.tier-map-row`, `.tier-map-band-label`, `.tier-map-{fast,balanced,strongest}`, `.tier-map-band-desc`, `.tier-map-input` — tier-map editor.
- `.settings-label` — amber left-border label for SETTINGS sections in the Rules window.

The `.ask-finding-btn` class already existed (wave 2).

---

## Files touched

- `crates/ui/src/cockpit.rs` — all four features
- `crates/ui/src/main.rs` — ask-finding signal lift + ChatBubble prop
- `crates/ui/src/style.rs` — CSS additions
- `crates/server/src/lib.rs` — `set_tier_map` endpoint (additive only)

## Test results

`cargo test -p camerata-ui`: 32 tests, 0 failures.
`cargo check -p camerata-server`: clean.
