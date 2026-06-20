# UoW SOC-2 Evidence Record and Scoped Security Scan

**Date:** 2026-06-20
**Issue:** #53
**Branch:** dev1/soc2-evidence
**Author:** Claude (Opus 4.8, 1M context) / Zach Ernst (architect)
**Status:** Additive new module, lifecycle wiring routed (see below)

---

## What was built

`crates/server/src/evidence.rs` — a new, additive module inside `camerata-server`. It contains
three complete systems:

### 1. `UowEvidenceRecord` — the per-Unit-of-Work SOC-2 evidence record

A serde-serializable struct that captures, for a single governed run on a story:

- **Actor / operation history** (`Vec<EvidenceEntry>`) — ordered events (run, gate_allow,
  gate_deny, sign_off, note, security_finding, critical_finding), each automatically mapped
  to the SOC-2 Trust-Services Criteria / Common Criteria controls they satisfy.
- **Gate decisions** (`Vec<GateDecision>`) — every allow/deny verdict, with the rule id that
  fired on a deny and the controls the decision satisfies.
- **Rules enforced** (`Vec<EnforcedRule>`) — which rules were active during the run, with
  their directives and enforcement tiers.
- **Sign-off** (`Option<EvidenceSignOff>`) — the architect's explicit approval, built from the
  existing `crate::uow::SignOff` shape. Never auto-populated.
- **PR / commit links** (`Vec<ChangeLink>`) — where the changes landed in VCS.
- **Scoped scan summary** (`Option<ScopedScanSummary>`) — the deterministic security findings
  from the changed-file scan (see below).
- **Content hash** (`String`) — FNV-1a (32-bit) over the canonical JSON (with the hash field
  zeroed) for tamper-evidence. `compute_hash()` stamps the record; `verify_hash()` checks it.

**SOC-2 control mapping** — `controls_for_event_kind(kind)` maps each event kind to one or more
TSC/CC controls (CC2.2, CC4.2, CC6.1, CC6.8, CC7.1, CC7.2, CC7.3, CC8.1, A1.1). The mapping
is documented in the module-level rustdoc table and in `CONTROL_DESCRIPTIONS`.

**Blocking sign-off flag** — `is_sign_off_blocked()` returns `true` when the scoped scan found
a critical finding. The PR renderer marks the record as "sign-off BLOCKED" in that case.

### 2. `render_pr_markdown` — the PR injection renderer

A pure function that renders a `UowEvidenceRecord` as structured markdown for a PR description
or comment. Sections:

- Advisory notice (SOC-2 GAP ANALYSIS label, per issue #62 guardrail)
- Record metadata (story, run, created-at, content hash)
- Sign-off status (blocked / pending / signed-off with who, when, controls)
- Scoped security scan table (if present)
- Governance event history (collapsible details block)
- Gate decisions (allow/deny counts, collapsible deny table)
- Rules enforced (collapsible table)
- PR / commit links
- SOC-2 control index (deduplicated BTreeMap across all events)
- Footer repeating the advisory label

**Pure function — no GitHub I/O.** Posting the markdown to a PR (description or comment) is
lifecycle wiring that belongs to a sibling stream. This function only produces the text.

### 3. `scoped_audit` — the scoped security pass

A function that runs the existing deterministic security floor (`crate::onboard::audit_files`)
over a SUBSET of files — only the UoW's changed files / diff — rather than the whole repo.

```rust
pub fn scoped_audit(
    repo: &str,
    all_files: &[(String, String)],
    changed_paths: &[String],
) -> ScopedAuditResult
```

It filters `all_files` to only the paths in `changed_paths`, then calls `audit_files` on that
subset. The full AI pass (`audit_repo`) is deliberately NOT run here: the scoped scan is
deterministic, token-free, and always cheap. The AI advisory pass lives in the full brownfield
audit (a separate, longer-running flow).

`ScopedAuditResult::has_critical` is `true` when any finding has severity `"critical"` — this
is the blocking sign-off signal. The `ScopedScanSummary` (embedded in the evidence record)
carries the same flag plus the full finding list for PR injection.

---

## Why these choices

### FNV-1a for tamper-evidence

FNV-1a is zero-dependency (no new crate needed), deterministic, and fast. It is not a
cryptographic hash — this is an integrity signal for auditing, not an adversarial security
boundary. The comment in the module is honest about this. A future upgrade to SHA-256 or BLAKE3
is straightforward (replace `fnv1a_hex`; no other code changes).

### Canonical JSON with zeroed hash field

`serde_json::to_string` on a struct produces deterministic field-order output (declaration
order). Zeroing `content_hash` before hashing avoids the chicken-and-egg problem. Re-computing
the hash on the same struct always produces the same result.

### SOC-2 GAP ANALYSIS labelling (advisory guardrail)

Issue #62 mandates that all SOC-2 language be labelled advisory. The rendered PR artifact has
a prominent block-quote at the top and a repeated advisory label in the footer. The module-level
rustdoc also states this. No claim of compliance is made anywhere.

### Deterministic floor only in `scoped_audit`, not AI

The AI pass (`audit_repo`) is asynchronous, token-consuming, and depends on a live LLM
connection. Requiring it for every UoW scoped scan would block governance events and make the
scoped scan unavailable offline or in test environments. The deterministic floor catches the
hardest, most exploitable violations (hardcoded secrets, raw SQL concat, secret-in-URL) and is
the right tool for a per-change blocking check. AI findings belong in the full brownfield audit
where the architect has deliberately opted in.

### No new crates, no cross-crate public trait changes (ROUTE-1)

The entire implementation is additive within `camerata-server`. The module imports from
`crate::uow::SignOff` and `crate::onboard::{Finding, audit_files}`, both of which are already
public within the crate. No new workspace crate was created; no public trait surface was changed.

---

## Routed items (not built here — require sibling streams or explicit Zach decision)

### ROUTE-A: Lifecycle wiring — attach record to UoW

The `UowEvidenceRecord` is built and manipulated by pure functions. Wiring it to the `UowStore`
(persisting the record to `<data_dir>/camerata/uow_evidence/<story_id>.json`, appending events
from the fleet's governed run log, and surfacing it via an API endpoint) is a lifecycle concern
that touches `UoW` core and the server's governed-run path. That wiring is owned by the UoW dev
loop stream (`dev1/uow-dev-loop`) and should be coordinated there.

**Proposed shape:** `UowStore` grows a companion `UowEvidenceStore` (same Arc<Mutex<HashMap>>
+ JSON-file pattern) keyed by `(story_id, run_id)`. The governed fleet appends to it as it runs.
Needs Zach's explicit go-ahead before any cross-module mutation.

### ROUTE-B: Scoped scan in the dev loop

`scoped_audit` is ready to be called. Wiring it into the governed dev loop — calling it at the
end of each run over the run's changed files, then embedding the `ScopedScanSummary` into the
evidence record — requires touching the fleet execution path. That is live-fleet territory
(`dev1/uow-dev-loop`).

### ROUTE-C: PR injection (posting markdown to GitHub)

`render_pr_markdown` produces the text. Posting it to a PR description or comment requires:
1. The GitHub token with PR write scope.
2. The PR number (from the UoW's branch + GitHub API lookup).
3. A decision on whether to post as a PR description update or a new comment.

The GitHub write primitives already exist in `crates/server/src/arm.rs` (the governance PR
path). Routing the evidence markdown through a similar `create_or_update_pr_comment` call is
straightforward once the PR number is available in the evidence record.

---

## How to use this module

### Building a record

```rust
use camerata_server::evidence::{UowEvidenceRecord, GateDecision, scoped_audit};

let mut record = UowEvidenceRecord::new("STORY-42", "run-7", chrono::Utc::now().to_rfc3339());

// 1. Record the governed run starting.
record.add_event(chrono::Utc::now().to_rfc3339(), "governed-fleet", "run", "Governed run started on feature/auth-fix");

// 2. Record gate decisions as the fleet writes files.
record.record_gate_decision(GateDecision::allow(chrono::Utc::now().to_rfc3339(), "src/auth.rs"));
record.record_gate_decision(GateDecision::deny(chrono::Utc::now().to_rfc3339(), "src/secrets.rs", "SEC-NO-HARDCODED-SECRETS-1"));

// 3. Record rules that were enforced.
record.record_rule("SEC-NO-HARDCODED-SECRETS-1", "Never hardcode secrets in source.", "mechanical");

// 4. Run the scoped audit over the changed files.
let changed = vec!["src/auth.rs".to_string()];
let audit_result = scoped_audit("owner/repo", &all_repo_files, &changed);
record.set_scoped_scan(audit_result.summary);

// 5. Add a change link once the PR is open.
record.add_change_link("pr", "https://github.com/owner/repo/pull/123", "#123");

// 6. Compute the content hash before persisting.
record.compute_hash();
assert!(record.verify_hash());
```

### Rendering for a PR

```rust
use camerata_server::evidence::render_pr_markdown;

let markdown = render_pr_markdown(&record);
// POST markdown to a PR description or comment (lifecycle wiring — see ROUTE-C above).
```

### Verifying record integrity

```rust
let ok = record.verify_hash();
if !ok {
    // Record was modified after hashing — flag for review.
}
```

### Checking for blocking findings

```rust
if record.is_sign_off_blocked() {
    // Do not permit sign-off until the critical finding is resolved.
}
```

---

## Test coverage

35 unit tests in `evidence::tests` cover:

- `controls_for_event_kind`: all known kinds + unknown fallback.
- `EvidenceEntry::new`: control population from kind.
- `GateDecision::allow` / `deny`: verdict + control wiring.
- `UowEvidenceRecord`: construction, `add_event`, `record_gate_decision`, `set_sign_off`,
  `is_sign_off_blocked`.
- Content hashing: round-trip, tamper detection, stable re-compute, empty-hash failure.
- `render_pr_markdown`: advisory notice, story/run ids, pending/blocked/signed-off states,
  event history, gate decisions, SOC-2 control index, footer.
- `scoped_audit`: empty changed paths, only-changed-files filtering, structural invariants.
- `escape_md_table`, `fnv1a_hex`: determinism, collision, length.

1 doctest covers the `scoped_audit` public API.

---

## Files touched

- `crates/server/src/evidence.rs` — new module (additive).
- `crates/server/src/lib.rs` — added `pub mod evidence;`.
- `docs/decisions/2026-06-20_uow_soc2_evidence_and_scoped_scan.md` — this file.
