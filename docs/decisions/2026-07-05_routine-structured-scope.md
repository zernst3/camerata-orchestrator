# Routine scope is a structured, enforced boundary (GAP-8)

Date: 2026-07-05
Status: Accepted; BUILT on `fix/gap8-routine-scope`.
Deciders: Zach (architect), Claude (architect)

Companion docs: ADR [`routine_dashboard`](2026-06-15_routine_dashboard.md),
plan [`escalation-decisions`](../plans/2026-07-05_escalation-decisions.md) (GAP-8).

## Context

`Routine.scope` was a decorative `String`. Its own doc comment admitted it was only
interpolated into the scaffolded prompt, never checked at the gate. A prompt-string scope
is an ADVISORY guardrail: it tells the agent what it may do and trusts it to comply. That
is precisely the anti-pattern Camerata exists to replace with a real, deny-before-execute
boundary. The 2026-07-04 audit flagged it as GAP-8: when live routine execution lands, a
prompt-string scope would be an unenforced boundary on an unattended agent, exactly where
governance most needs to be real.

Governing principle (from the escalation plan): never defer hardening/correctness of
Camerata. So GAP-8 is build-now, as a prerequisite for live routine execution, latent
until then.

## Decision

Replace `scope: String` with a structured `RoutineScope`
(`crates/app-core/src/routine.rs`) carrying the three inputs the gateway session
registration already uses for a DEV run:

- `rule_subset: RuleSubsetRef` — `All` (every enforced gate rule, the default) or `Ids`
  (the enforced floor PLUS explicit domain rule ids). A routine's scope can only ADD
  rules on top of the gate floor; it can never lower it.
- `write: WritePolicy` — `ReadOnly` | `WriteGated` | `WriteOpenPr`. Drives the tool
  allowlist and whether a write jail is registered at all.
- `write_jail: Option<PathScope>` — `Worktree` when the policy writes; `None` for
  read-only, so `gated_write` has no target and the agent has no write path.
- `note: String` — preserves the human-authored scope text so nothing is lost and the
  dashboard keeps a legible label.

### Mapping onto the SAME primitives dev runs use

`resolve_scope_registration` (`crates/server/src/scope_registration.rs`) turns a
`RoutineScope` into a `RoutineSessionRegistration { role, tool_allowlist, write_jail }`:

- rule subset -> `camerata_fleet::governed_role` (which guarantees the enforced gate-rule
  floor) unioned with the scope's explicit domain rules; the role's `rule_subset`
  serializes to `rules.json` for the gateway;
- tool allowlist -> `camerata_agent::allowed_tools_for_role` (the identical derivation a
  dev run's driver uses; a routine is never an orchestrator, so no `delegate`/`fan_out`);
- write jail -> the worktree passed to `camerata_agent::prepare_session`, which sets
  `CAMERATA_WORKTREE_ROOT` (a read-only scope registers no jail).

These are the SAME three inputs `run_one_investigation_pass` feeds `prepare_session` for a
live DEV run, so a routine run enforces governance identically. The seam is wired at
`RoutineStore::resolve_run_registration` (`crates/server/src/routine.rs`).

### Backward compatibility (no data loss)

Serde accepts BOTH shapes via `deserialize_scope`: a legacy JSON string maps through
`RoutineScope::from_legacy_string` (the string becomes `note`; enforcement fields take the
safe read-only defaults unless the string names a write level), and the structured object
deserializes directly. This covers persisted `routines.json`, project exports/imports
(`ImportedRoutine.scope`), and templates. The UI reduces either shape to a display string
via a tolerant deserializer, so the create-form scope `<select>` still round-trips.

## Honest limit

Live routine execution is separate and still latent: the auto-fire scheduler runs the
token-free scripted gate (`run_now` -> `run_event_script`) today, and no production caller
spawns a routine agent yet. GAP-8 makes the scope a real, structured, TESTED boundary and
wires the resolution seam so it WILL be enforced the moment the live execution path lands.
It does not, by itself, make routines execute governed agent runs.

## Alternatives considered

- **Keep `scope: String`, enforce by parsing the prompt at runtime.** Rejected: parsing
  free text back into a boundary is fragile and re-introduces the advisory pattern.
- **Put the resolution fn in `app-core`.** Rejected: building the role reads the rule
  corpus (I/O); that belongs in the server adapter, above the pure domain crate. `app-core`
  owns the data shape + the pure policy decision only.
