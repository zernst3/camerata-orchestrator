# Scan UX: test-path down-rank for floor findings + dead "Ask AI" button removal

Date: 2026-06-22
Status: Accepted and implemented.
Deciders: Zach (architect), Claude (implementor)

## Fix 1: Down-rank deterministic-floor findings in test/fixture/example paths

### Problem

The deterministic floor (`audit_files` / `audit_content` in
`crates/server/src/onboard.rs`) walks every file in a scanned repo with no
exclusion for test, fixture, or example directories. The floor's `severity_for`
helper unconditionally returns `"critical"` for every match.

In practice this means:

- A fake GitHub PAT in `tests/auth_test.rs` used to verify the detection logic
  shows up as **Critical** alongside a real credential leak in `src/config.rs`.
- Camerata's own detection-test fixtures self-flag as Critical during a scan.
- Architects see dozens of Critical findings that are not exploitable, eroding
  trust in the findings list.

### Decision

Add a pure path classifier `is_test_or_fixture_path(path: &str) -> bool` that
recognises the conventional test/fixture/example directory and filename patterns
shared across major language ecosystems. When a floor finding's path matches,
down-rank it to `"low"` (the lowest named severity rung in the findings table)
and append `" (in test/fixture code — likely a non-exploitable test value;
verify)"` to the `detail` field.

The finding is **kept visible** — a real credential checked into a test file still
merits a look. Only the severity is adjusted so that synthetic test values cannot
crowd out genuine production-path leaks at the top of the findings table.

Production-path findings are unchanged: they keep their `"critical"` rating.

### Classifier rules (`is_test_or_fixture_path`)

All comparisons are case-insensitive.

**Directory-segment match** — any path component (not the filename) equal to one
of: `tests`, `test`, `testdata`, `fixtures`, `__tests__`, `examples`, `benches`.

**Filename match** — the last path component matches:
- `*_test.<ext>` — Go / Rust convention (`auth_test.go`, `auth_test.rs`)
- `*.test.<ext>` — JS/TS convention (`auth.test.ts`)
- `*.spec.<ext>` — JS/TS convention (`auth.spec.ts`)
- `test_*.py` — Python unittest convention
- `conftest.py` — pytest fixture file

### Implementation location

`crates/server/src/onboard.rs`:
- `pub fn is_test_or_fixture_path(path: &str) -> bool` — pure, publicly testable.
- `const TEST_PATH_NOTE: &str` — the appended note text.
- `const TEST_PATH_SEVERITY: &str = "low"` — the down-ranked severity.
- `audit_content` calls `is_test_or_fixture_path(path)` once per file and
  branches on the result when building each `Finding`.

The floor's `severity_for` function is unchanged; the branch in `audit_content`
is where the down-rank happens so the path context is available.

### Tests added (all in `crates/server/src/onboard.rs`)

- `test_path_classifier_false_cases` — 7 production paths that must return false.
- `test_path_classifier_true_cases` — 15 test/fixture paths (directory segments,
  filename patterns, case-insensitive variants) that must return true.
- `floor_finding_on_test_path_is_low_with_note` — a GitHub PAT in
  `tests/auth_test.rs` comes back at severity `"low"` with the note in `detail`.
- `floor_finding_on_production_path_stays_critical` — the same PAT in
  `src/config.rs` stays `"critical"` with no note.
- `floor_finding_on_fixture_subdir_is_low` — a PAT in
  `crates/x/src/fixtures/keys.py` (deeply nested fixture dir) is down-ranked.

---

## Fix 2: Remove the dead "Ask AI about this finding" button

### Problem

`crates/ui/src/cockpit.rs` contained a button (class `ask-finding-btn`, text
"Ask AI about this finding") that wrote a `crate::chat::FindingContext` into an
app-level signal. The consumer side of that signal was never connected to anything
that opens the chat panel with the finding loaded: the `_ask_finding_present`
binding in `CockpitApp` was suppressed with a leading underscore and used only to
pull the context into scope, with no downstream action. The button did nothing
visible to the user.

### Decision

Remove the dead wiring in `cockpit.rs`. Do not remove the `FindingContext` type
or any plumbing in `chat.rs` or `main.rs` — those remain in place for issue #68,
which will re-wire the feature properly.

### Removed from `cockpit.rs`

1. The `button` element with class `ask-finding-btn` and all its `onclick` closure
   body (lines ~7916-7943 in the original).
2. `let mut ask_finding = use_context::<Signal<Option<crate::chat::FindingContext>>>()`
   and its comment block — unused after the button is gone.
3. `let id_map_ask = id_map.clone()` and its comment — only cloned for the button.
4. The `_ask_finding_present` binding and its 4-line comment block in `CockpitApp`
   — the context is still provided by `main.rs`; this binding was just an unused
   re-read.

### Intentionally kept (for #68)

- `crate::chat::FindingContext` struct and all its fields in `chat.rs`.
- The `use_signal / use_context_provider` in `main.rs` that provides the signal
  into the context tree.
- The `ChatBubble { finding: ask_finding() }` call in `main.rs` that passes the
  signal value to the chat panel.

When #68 lands it will add the button back (or a replacement affordance) and wire
it to the already-provided signal, without needing to reconstruct the type or the
provider.
