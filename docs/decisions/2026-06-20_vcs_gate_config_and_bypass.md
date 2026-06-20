# VCS-gate configurable process rules + auditable bypass

Date: 2026-06-20
Status: Accepted + Implemented
Deciders: Zach (architect)
Issue: #65

Companion: `2026-06-15_process_rules_and_vcs_action_gate.md` (the underlying gate),
`2026-06-16_baseline_ratchet_and_suppressions.md` (bypass mirrors suppression-waiver
invariant).

## What was built

Three discrete additions on top of the existing VCS-action gate:

1. **`ProcessRuleConfig`** (`crates/checks/src/vcs_action.rs`) — a serde-serializable
   per-project knob panel that enables/disables each of the four process rules and
   exposes their tunables. Shipped with serde `#[serde(default)]` on every field so
   existing persisted projects get sensible defaults (no migration, no breaking change).

2. **`Project::process_rule_config`** (`crates/server/src/project.rs`) — the
   `ProcessRuleConfig` value is persisted on each project. The BFF exposes two new
   endpoints: `GET /api/projects/:id/process-rule-config` (read) and
   `POST /api/projects/:id/process-rule-config` (replace the full document).

3. **Auditable bypass** (`crates/checks/src/vcs_action.rs`) — `gate_or_bypass` wraps
   `gate` and accepts an optional `BypassRequest { reason }`. A non-empty reason
   converts a gate failure into a `BypassRecord { reason, suppressed_rule_ids }` for
   the evidence trail. A reason-less bypass is `Err(BypassReasonRequired)`. The BFF
   endpoint is `POST /api/projects/:id/vcs-gate/bypass`.

4. **Settings UI** (`crates/ui/src/vcs_settings.rs`) — a new, standalone
   `VcsGateSettings` component, kept entirely out of the cockpit story/audit views.

## Why this design

### Config shape: full-document replace, not field-level PATCH

The config is persisted as a single JSON blob on `Project`. The `POST` endpoint
replaces the whole document (caller sends the full config). Partial-update PATCH was
considered and rejected: the full-document replace is explicit (no ambiguity about
which fields are defaults vs intentionally absent), and the UI always reads the
current config before saving, so the round-trip is safe. Serde defaults handle
any new fields added in future versions.

### Per-project vs global config

Config lives on `Project`, not in the global `Settings`. Process rules are
team/project-specific: one project may require `AB#<id>` (Azure Boards), another
may use `#<id>` (GitHub issues), a third may have no story-id requirement. A single
global config would force every project onto the same convention.

### Bypass mirrors the suppression-waiver invariant

The existing suppression mechanism (`crates/server/src/suppression.rs`, ADR
`2026-06-16_baseline_ratchet_and_suppressions.md`) makes a reason-less waiver a
governance violation. The bypass follows the same invariant: an empty reason is
`Err(BypassReasonRequired)` at the Rust level, not a `200 ok: false` — it is a hard
error, not a soft rejection. This ensures the bypass is always traceable.

### `Matcher::Substantive` added (additive, no variant removal)

When `CommitDocConfig::require_story_id = false` the rule still enforces the body
length but not the story-id. The existing `Matcher::SubstantiveWithStoryId` always
requires both, so a new `Matcher::Substantive { min_non_blank_chars }` was added. No
existing variant was removed or changed. All 74 existing tests still pass.

### `VcsAction` + `VcsTarget` + `ProcessViolation` now derive `Serialize/Deserialize`

The bypass endpoint needs to parse a `VcsAction` from the request body and return
`ProcessViolation` details. Adding serde derives is backward-compatible (these types
were already fully in the public API of `camerata-checks`).

## Config tunables per rule

| Rule | Tunable | Default |
|------|---------|---------|
| `PROCESS-COMMIT-DOC-1` | `enabled` | `true` |
| | `min_body_chars` | `20` |
| | `require_story_id` | `true` |
| | `story_id_format.prefix` | `""` (bare `#42`) |
| | `story_id_format.separator` | `'#'` |
| `PROCESS-CONVENTIONAL-COMMIT-1` | `enabled` | `true` |
| | `types` | standard 11-type set |
| `PROCESS-BRANCH-NAMING-1` | `enabled` | `false` (opt-in) |
| | `prefixes` | `["feature/", "release/", "hotfix/"]` |
| `PROCESS-ADO-LINK-1` | `enabled` | `false` (opt-in) |
| | `prefix` | `"AB"` |

## Usage

### Configuring an Azure Boards project (AB#123 everywhere)

```json
{
  "commit_doc": {
    "enabled": true,
    "min_body_chars": 20,
    "require_story_id": true,
    "story_id_format": { "prefix": "AB", "separator": "#" }
  },
  "conventional_commit": { "enabled": true },
  "branch_naming": { "enabled": false },
  "ado_link": { "enabled": true, "prefix": "AB" }
}
```

### Bypassing for a machine-generated commit

```
POST /api/projects/proj-1/vcs-gate/bypass
{
  "action": { "kind": "commit", "message": "Merge pull request #42 from bot/fix" },
  "reason": "machine-generated merge commit from the automated rebase pipeline"
}
```

Response when the gate would have fired:
```json
{
  "ok": true,
  "bypassed": true,
  "record": {
    "reason": "machine-generated merge commit from the automated rebase pipeline",
    "suppressed_rule_ids": ["PROCESS-CONVENTIONAL-COMMIT-1"]
  }
}
```

## Files touched

- `crates/checks/src/vcs_action.rs` — `ProcessRuleConfig` + subtypes, `build_rules`,
  `gate_or_bypass`, `BypassRequest`, `BypassRecord`, `Matcher::Substantive`, serde
  derives on `VcsAction`/`VcsTarget`/`ProcessViolation`
- `crates/checks/Cargo.toml` — added `serde`, `serde_json` workspace deps
- `crates/server/src/project.rs` — `process_rule_config` field on `Project`,
  `set_process_rule_config` method, updated `create`/`import_or_overwrite`
- `crates/server/src/model_tier.rs` — updated test fixture (`process_rule_config`)
- `crates/server/src/lib.rs` — two new routes + three new handlers
  (`get_process_rule_config`, `set_process_rule_config_handler`, `vcs_gate_bypass`)
- `crates/server/Cargo.toml` — added `camerata-checks` dependency
- `crates/ui/src/vcs_settings.rs` — NEW settings component + BFF helpers
- `crates/ui/src/style.rs` — new CSS in `GLOBAL_CSS`
- `crates/ui/src/main.rs` — `mod vcs_settings`
- `docs/decisions/2026-06-20_vcs_gate_config_and_bypass.md` — this file
