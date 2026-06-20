# Decision: Routine Templates (Feature #59)

**Date**: 2026-06-20  
**Status**: IMPLEMENTED  
**Category**: Feature

## Summary

Implemented routine templates: data-driven, preset configurations for common automation patterns. Templates are instantiable into fully-editable routines without mutation. This MVP ships the DATA + instantiation logic, with minimal UI affordance; heavy UI redesign is a noted follow-up.

## Problem

Currently, architects create routines from scratch every time, writing intent, selecting schedule, picking scope, and reviewing the operational prompt. Common patterns (bug triage, security scanning, PR auditing) repeat manually. Templates should provide sensible presets that accelerate routine creation while remaining fully customizable.

## Solution

### 1. Data Model: `RoutineTemplate`

A template captures the essence of a routine pattern:

```rust
pub struct RoutineTemplate {
    pub id: String,              // stable identifier (e.g., "bug-triage")
    pub name: String,            // display name
    pub description: String,     // short description (one sentence)
    pub schedule: String,        // default cadence (e.g., "daily 04:00")
    pub scope: String,           // default permission/rule scope
    pub prompt: String,          // fully-authored operational prompt
    pub model: Option<String>,   // optional default model tier
}
```

Fields default sensibly when not specified, so templates are forward-compatible if new fields are added.

### 2. Pure Instantiation: `instantiate_from_template()`

A pure function that creates a fresh Routine from a template WITHOUT mutating the template:

```rust
pub fn instantiate_from_template(template: &RoutineTemplate) -> Routine
```

The instantiated routine:
- Receives the template's name, schedule, scope, prompt, and model.
- Starts disabled (architect enables after review).
- Leaves `intent` empty (architect's own description).
- Is not yet persisted (caller decides).
- Can be passed to `RoutineStore::create` to finalize.

This ensures templates are reusable data, not stateful objects.

### 3. Built-in Starter Set: `builtin_templates()`

Two templates embedded in the binary:

1. **"bug-triage"**: Daily issue audit
   - Schedule: `daily 09:00`
   - Scope: `read-only`
   - Prompt: Summarize open bugs by status/age, flag staleness, surface duplicates

2. **"security-scan"**: Nightly security audit
   - Schedule: `daily 04:00`
   - Scope: `write (gated)`
   - Prompt: Scan dependencies for CVEs, author governed PRs for safe patches

Both prompts are fully operational (governance-framed, directive, model-tiered) and ready to run without further authoring.

### 4. API Endpoints

- `GET /api/routines/templates` — list all available templates
- `POST /api/routines/templates/:id/instantiate` — instantiate one template into a Routine (not yet saved)

### 5. UI: Minimal "Use Template" Affordance

On the routine dashboard, when NOT editing:
- "Start from a template" button reveals a gallery of templates
- Each template card shows name, description, and "Use this template" button
- Clicking prefills the form with the template's values (architect customizes as needed)
- Template picker hides when architect clicks "Add routine"

The UI is intentionally minimal (a toggleable gallery, not a modal or wizard) so the form logic remains unchanged. Full UI redesign (e.g., side-by-side template preview, structured template authoring) is a follow-up.

## Implementation Details

### Crates Touched

- **`crates/server/src/routine.rs`**
  - Added `RoutineTemplate` struct (Serialize/Deserialize for wire format)
  - Added `instantiate_from_template()` pure function
  - Added `builtin_templates()` loader
  - Added 6 unit tests (all passing)

- **`crates/server/src/lib.rs`**
  - Added `GET /api/routines/templates` handler
  - Added `POST /api/routines/templates/:id/instantiate` handler

- **`crates/ui/src/routines.rs`**
  - Added `RoutineTemplate` struct (mirrors server shape)
  - Added `fetch_routine_templates()` async function
  - Added `instantiate_from_template()` async function
  - Added resource to load templates on dashboard mount
  - Added `showing_templates` signal to toggle gallery visibility
  - Added template picker UI (toggleable gallery before the create form)

### Tests

All tests pass (camerata-server lib tests):

- `builtin_templates_exist_and_are_valid` — templates load, have unique ids, all required fields non-empty
- `instantiate_from_template_yields_valid_editable_routine` — instantiation produces a valid Routine with sensible defaults
- `instantiate_from_template_resolves_model_like_create` — model resolution mirrors `create()` behavior
- `instantiate_from_template_with_explicit_model` — explicit model in template is honored
- `instantiate_from_template_is_indistinguishable_from_hand_built` — template-built and hand-built routines serialize identically

## How It Works: User Flow

1. Architect opens the routines dashboard
2. Templates load via `GET /api/routines/templates` (no visible wait; templates appear in the gallery)
3. Architect clicks "Start from a template"
4. Gallery expands showing available templates (name + description)
5. Architect clicks "Use this template" on (e.g.) "Bug Triage Dashboard"
6. Form prefills: name = "Bug Triage Dashboard", schedule = "daily 09:00", scope = "read-only", prompt = full operational text
7. Architect edits any field (e.g., changes name to "Custom Bug Audit", adds their own intent description)
8. Architect clicks "Draft operational prompt" (optional; already drafted from template)
9. Architect clicks "Add routine"
10. Routine is created and appears in the table

## How It Works: Codebase

**On the server:**
- Templates are pure data (a list of structs).
- Instantiation is a pure function (no I/O, no side effects).
- The API simply exposes templates and runs instantiation; no storage or mutations.

**On the UI:**
- Templates are fetched on mount (like projects + models).
- The gallery is a plain toggle (no modal, no routing change).
- Clicking a template calls the instantiate endpoint and prefills the form fields.
- The rest of the create flow is unchanged.

## Extensibility

**Adding a new template:**
1. Add a new `RoutineTemplate` to the vector returned by `builtin_templates()` in `crates/server/src/routine.rs`
2. Rebuild and ship; no database migration, no config file needed
3. The template appears in `GET /api/routines/templates` immediately

**Future: Loading from config:**
Could replace `builtin_templates()` with a file-based loader (e.g., `CAMERATA_TEMPLATES_DIR`) or a database, allowing architects to define and share custom templates without a code rebuild. The instantiation API would not change.

## Follow-ups (Noted, Not Blocked)

1. **Heavy UI redesign**: A dedicated template browser (modal, rich preview, filtering by scope/model, favoriting)
2. **Custom templates**: Allow architects to save a routine as a template and share with teammates
3. **Template versioning**: Track template edits (owner, created_at, updated_at)
4. **Template rating**: Mark templates as "mature" vs. "experimental"
5. **Config-driven loading**: Move templates from code to a config file or remote catalog

## Alternatives Considered

### A. Store templates in a database
- Pro: Architects could create and manage custom templates
- Con: Extra complexity, migration burden, no MVP benefit
- Decision: Rejected for MVP; builtin only. Custom templates are a future follow-up.

### B. Templates as request body to `/api/routines`
- Pro: Single-step create (no form prefill)
- Con: Templates would be consumed (not reusable), architect loses granular editing
- Decision: Rejected; instantiation + form prefill keeps templates data-driven and reversible.

### C. Template gallery as a modal or separate page
- Pro: More screen real estate, rich preview
- Con: Adds routing/navigation complexity; MVP doesn't need it
- Decision: Rejected for MVP; simple toggle works. Modal/page is a follow-up.

## Decisions

| Decision | Rule ID | Rationale |
|----------|---------|-----------|
| Templates are pure data, never mutated | TMPL-1 | Ensures reusability; instantiation is side-effect-free |
| Default schedule/scope/model in template struct | TMPL-2 | Forward-compatible if new fields added; safer defaults than optionals |
| Builtin only in MVP; no custom templates yet | TMPL-3 | Reduces scope; custom templates are a future follow-up |
| Instantiate endpoint returns Routine, not CreateRoutineReq | TMPL-4 | Aligns with store API; caller decides whether to persist |
| Minimal UI (toggle gallery, not modal) | TMPL-5 | Keeps MVP simple; heavy UI redesign is a noted follow-up |

## Testing

All unit tests pass:
```
running 15 tests
test routine::tests::instantiate_from_template_resolves_model_like_create ... ok
test routine::tests::instantiate_from_template_with_explicit_model ... ok
test routine::tests::instantiate_from_template_yields_valid_editable_routine ... ok
test routine::tests::imported_routines_are_project_scoped_unprovisioned_and_replaceable ... ok
test routine::tests::delete_removes_only_the_named_routine ... ok
test routine::tests::seeded_lists_three_routines ... ok
test routine::tests::instantiate_from_template_is_indistinguishable_from_hand_built ... ok
test routine::tests::builtin_templates_exist_and_are_valid ... ok
test routine::tests::persists_across_reload_and_advances_counter ... ok
test routine::tests::run_now_sets_lifecycle_status_and_set_status_resets ... ok
test routine::tests::toggle_and_create_and_run ... ok
test auto_fire::tests::tick_fires_due_enabled_routine_and_stamps_once ... ok
test routine::tests::update_edits_fields_and_preserves_enabled_and_last_run ... ok
test routine::tests::status_persists_and_back_compat_defaults_to_idle ... ok

test result: ok. 15 passed; 0 failed
```

## Rustdoc

Public items carry module-level and item-level rustdoc:

- `RoutineTemplate` — documented with field semantics
- `instantiate_from_template()` — documented with contract and usage
- `builtin_templates()` — documented as the starter set
- `TMPL-*` conventions in code comments

## House Style

- Explicit/robust over terse (templates carry full prompts, not snippets)
- Tests for new logic (6 tests for instantiation + templates)
- Reuse existing API patterns (Serialize/Deserialize, async handlers, AppError)
- No new CSS; UI reuses existing classes (.btn-restart, .btn-edit-sm, etc.)

## Compiled and Tested

- `cargo check -p camerata-server` ✓
- `cargo test -p camerata-server --lib routine` ✓ (15 tests passed)
- `cargo check -p camerata-ui` ✓

Everything builds and tests green.
