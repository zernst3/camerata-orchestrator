# 2026-06-21 Server Product Wave — Endpoint and DTO Shapes

Implemented in PR branch `pw/server-features` (off `dev/integration`).
Six areas, each with its concrete wire contract documented here so the
UI-agent can build against the shapes without guessing.

---

## 1. Rule auto-recommendation gate

### Rule change (`camerata-rules`)

```rust
impl Rule {
    /// `true` iff the rule's `verification` is `Grounded` or `Verified`.
    /// Draft / NeedsRecheck rules are listed on onboarding but NOT pre-checked.
    pub fn is_auto_recommended(&self) -> bool {
        matches!(self.verification, Verification::Grounded | Verification::Verified)
    }
}
```

### `ProposedRule` DTO (onboard.rs + ai_audit.rs)

Added field:

```json
{
  "rule_id": "SECURITY-1",
  "rule_text": "...",
  "chosen_option": null,
  "is_auto_recommended": true
}
```

`is_auto_recommended` is `true` only when the corpus rule has `Grounded` or
`Verified` verification. All inline proposals (greenfield scaffold, AI parse)
default to `false`. The UI should pre-check rules where `is_auto_recommended
= true` in the onboarding proposal list; Draft/NeedsRecheck rules appear in
the list but unchecked.

---

## 2. Development context endpoint

### `GET /api/development/context`

No path/query params. Returns ALL `UnitOfWork` entries from the in-memory
`UowStore`, shaped for chat-sidebar display.

**Response:**

```json
{
  "ok": true,
  "units": [
    {
      "story_id": "story-abc123",
      "stage": "development",
      "stage_label": "Development",
      "dev_status": "in_progress",
      "gate_status": "open",
      "bounce_count": 0,
      "sign_off_state": "unsigned",
      "last_activity": "2026-06-21T10:00:00Z"
    }
  ],
  "count": 1
}
```

Field notes:
- `stage`: wire string from `UowStage::wire_str()` (`"intake"`, `"investigating"`, `"decisions_approved"`, `"development"`, `"awaiting_qa"`, `"signed_off"`)
- `stage_label`: human display label from `UowStage::label()`
- `dev_status`: `"new"`, `"in_progress"`, or `"done"`
- `gate_status`: `"open"` / `"bounced"` / `"blocked"` derived from `bounce_count`
- `bounce_count`: count of gate bounces on this UoW
- `sign_off_state`: `"unsigned"` / `"signed"` — `"signed"` when `uow.signed_off_at` is `Some`
- `last_activity`: ISO-8601 timestamp from `uow.updated_at`

---

## 3. Update detection

### `GET /api/updates/check`

No params. Makes a live GitHub API call to the `zernst3/camerata-orchestrator`
releases endpoint and compares against the running binary version (or `"dev"`
when not set). Also computes per-rule content drift for rules in all
onboarded projects.

**Response:**

```json
{
  "ok": true,
  "current_version": "v0.3.0",
  "latest_version": "v0.4.0",
  "update_available": true,
  "release_url": "https://github.com/zernst3/camerata-orchestrator/releases/tag/v0.4.0",
  "rule_drift": [
    {
      "rule_id": "SECURITY-1",
      "project_id": "proj-1",
      "applied_hash": "a1b2c3d4",
      "current_hash": "e5f6g7h8",
      "changed": true
    }
  ]
}
```

Field notes:
- `current_version`: value of `CAMERATA_VERSION` env var or `"dev"` if unset
- `latest_version`: tag name of the latest GitHub release; `null` on network error
- `update_available`: `true` when `latest_version != current_version && latest_version != null`
- `release_url`: `html_url` from the GitHub release object
- `rule_drift`: one entry per applied rule per project that has changed (i.e. `changed = true`)
- `applied_hash`: xxHash-style hex string of the rule text at the time the project applied it
- `current_hash`: xxHash-style hex string of the CURRENT corpus rule text
- When the GitHub call errors, `latest_version` and `release_url` are `null`

**Content hash implementation:** SHA-256 over `rule.id + rule.rule` + all option ids sorted.

---

## 4. Single-rule edit endpoints

### Project-level override

**`GET /api/projects/:id/rules/:rule_id`**

Returns the current project-level `RuleSelection` for the rule (or a default
stub when no override exists).

```json
{
  "ok": true,
  "rule_id": "SECURITY-1",
  "project_id": "proj-1",
  "chosen_option": "opt-a",
  "repos": []
}
```

**`POST /api/projects/:id/rules/:rule_id`**

Body:
```json
{
  "chosen_option": "opt-a",
  "directive_override": "Require X in every handler."
}
```

Both fields are optional. `chosen_option` replaces the prior selection.
`directive_override` is accepted but not yet persisted (will land when
`RuleSelection` gains the field in a later migration).

Response:
```json
{
  "ok": true,
  "rule_id": "SECURITY-1",
  "project_id": "proj-1",
  "chosen_option": "opt-a"
}
```

### Repo-level override

**`GET /api/projects/:id/repos/:repo/rules/:rule_id`**

Returns the `RuleSelection` entry scoped to the specific `repo` (or a default
stub). `repo` is URL-encoded (e.g. `owner%2Frepo`).

```json
{
  "ok": true,
  "rule_id": "SECURITY-1",
  "project_id": "proj-1",
  "repo": "me/api",
  "chosen_option": null
}
```

**`POST /api/projects/:id/repos/:repo/rules/:rule_id`**

Body:
```json
{
  "chosen_option": "opt-b",
  "directive_override": "Also require Y."
}
```

Response:
```json
{
  "ok": true,
  "rule_id": "SECURITY-1",
  "project_id": "proj-1",
  "repo": "me/api",
  "chosen_option": "opt-b"
}
```

**Scoping semantics:** Repo overrides supplement project overrides. A rule
lookup at audit time should apply: repo override > project override > corpus
default.

---

## 5. Deep-report export

### `GET /api/projects/:id/deep-report`

Returns the most recent deep-tier report for the project as a Markdown
document. Uses the latest completed job that has a non-null `deep` field in
its `ScanReport`.

**Content-Type:** `text/markdown; charset=utf-8`

**Response headers:**
```
Content-Disposition: attachment; filename="deep-report.md"
```

**Structure of the Markdown body:**

```markdown
# Camerata Deep Audit Report

> **ADVISORY NOTICE** — This report is produced by an AI system
> reviewing source code. It is NOT externally validated and NOT a
> penetration test. All findings require human expert review before
> acting on them. Do not treat this report as a compliance artifact.

---

*Exported: 2026-06-21T10:00:00Z*

*Advisory: true*

---

## SOC-2 Gap Analysis

[table of control / title / status / observed / gap — one row per Soc2Gap]

---

## Deep Security Review

[bullet list of security findings]

---

## Threat Model

[table of component / threat / kind / severity / category]

---
*Lens error (if any):*
```

**Flag awareness:** When `soc2 = false` in feature flags, the SOC-2 lens
does not run so it will be absent from the `DeepReport.lenses` list. The
export renders only the lenses that are present; a missing SOC-2 section is
expected and correct.

**404 response** (no completed deep audit found):
```json
{ "ok": false, "error": "no deep report available yet" }
```

---

## 6. Feature flags

### `GET /api/feature-flags`

No params. Returns the current `FeatureFlags` struct as JSON.

```json
{
  "soc2": false
}
```

All flags default to `true` when absent from config. The `soc2` flag is
shipped `false` in `.camerata/features.toml`.

### Config file: `.camerata/features.toml`

```toml
# soc2 = false disables the SOC-2 gap-analysis lens in run_deep_tier.
# Default (absent) = true. Enable by setting soc2 = true or removing this line.
soc2 = false
```

### Env override

`CAMERATA_FEATURE_SOC2=false` — disables the SOC-2 lens regardless of config.
Only the exact string `"false"` (case-insensitive) disables; any other value
or an absent var leaves the flag at its configured value.

### Gate in `run_deep_tier`

`run_deep_tier` now takes a `soc2_enabled: bool` parameter. When `false`,
only the deep-security and threat-model lenses run concurrently; the SOC-2
lens code path is skipped at runtime. The returned `DeepReport` has 2 lenses
(not 3); the `advisory` flag and `disclaimer` are still set.

---

## Files touched

| File | Change |
|---|---|
| `crates/rules/src/lib.rs` | `Rule::is_auto_recommended()` method + tests |
| `crates/server/src/feature_flags.rs` | new module — `FeatureFlags` load/env/test |
| `crates/server/src/lib.rs` | 6 new routes + 7 handlers + `AppState.feature_flags` |
| `crates/server/src/onboard.rs` | `ProposedRule.is_auto_recommended`, `soc2_enabled` param |
| `crates/server/src/ai_audit.rs` | `run_deep_tier` soc2 gate + lens tests |
| `crates/server/src/jobs.rs` | `JobStore::latest_deep_report()` |
| `crates/server/Cargo.toml` | added `toml = "0.8"` dep |
| `.camerata/features.toml` | new config file — ships `soc2 = false` |
