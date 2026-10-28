# Rule-kind classification (Pass-1 W2)

Generated 2026-10-09 as part of Pass-1 W2 (`docs/plans/2026-10-09_audit-findings-tracker.md`, A5.1).

**Method.** Every rule in `crates/rules/principles/` (417 files) was classified from its own
`title`/`qualifies`/`[decision].why`/`[[option]]` text — never from what would score well against any
benchmark repo (per the hard rule: the benchmark repos in the private fixtures folder were never read,
listed, or consulted for this classification). Classification combined a scripted heuristic
(keyword/phrase signals for an option describing the absence/degenerate form of the rule's
requirement, plus decision-framing phrases) with a full manual read of every rule the heuristic
flagged `defect`/`default_with_exceptions`, every rule it flagged `decision` with high confidence,
and a targeted re-check of every `violates` flag whose own text suggested it might describe DETECTOR
precision rather than codebase behavior. Per the owner's explicit instruction, anything left
genuinely uncertain after that review was forced to `decision` (the conservative kind — it can only
ever report inconsistency, never manufacture a false defect) and marked `ambiguous` below, never
guessed into `defect`/`default_with_exceptions`.

**Tallies.**

- `defect` (clear): 132
- `default_with_exceptions` (clear): 117
- `decision` (clear): 110
- `decision` (ambiguous — forced conservative, **NEEDS OWNER DECISION**): 58
- **Total: 417**

> **Drift note, 2026-10-11 (docs pass). TREAT THE TALLY ABOVE AS A DATED SNAPSHOT, NOT A CURRENT
> FACT.** It is the 2026-10-09 classification and the corpus has moved since. Reclassification is
> ACTIVE as this note is written, so any number recorded here goes stale within hours; the
> authoritative count is always the TOMLs themselves:
>
> ```
> git grep -h '^kind = ' HEAD -- crates/rules/principles | sort | uniq -c
> ```
>
> The total is still 417 rules. The commits that have moved a `kind` since the snapshot, newest
> first: `bc6f013` (19 security rules to `defect` so they can report a fault), `3a9e3e6`
> (`ARCH-HOT-READ-CACHE-1`), `509e17e` (five more to `decision`, plus the `violates` flags that
> contradicted their own prose), `84088c9` (two to `decision`). A separate, parallel effort has been
> correcting `violates` flags corpus-wide (`a0118da`, `ffee7b2`, `79ee0f2`, `330734b`, `aa003f3`,
> `ece0d7d`, `bc9e78e`, `0ca0632`) without touching `kind`.
>
> What has NOT changed: the 58-rule hold list below, and the fact that the W8/W9/W10
> severity-ceiling work touched zero `kind` fields by construction, enforced by
> `req_a5_7_no_held_rule_carries_an_inferred_deviation_ceiling`.

> **Second drift note, 2026-10-14. THE TAXONOMY ITSELF GREW: there are now FOUR kinds, not
> three.** W17 added `decision_with_floor` (`crates/rules/src/lib.rs:263`) for the shape this
> classification pass had no slot for - a rule with SEVERAL equally legitimate options PLUS
> options that are genuine faults under all of them. The method described above could
> therefore only ever force such a rule into `decision` (dropping real fault flags) or
> `default_with_exceptions` (fabricating a default). A candidate sweep over the corpus found
> exactly ONE member, `RUST-DOMAIN-6`, which moved out of `default_with_exceptions`; the
> membership is asserted as an EQUALITY by
> `req_w17_decision_with_floor_has_exactly_one_member`
> (`crates/rules/tests/w17_decision_with_floor_kind.rs:212`), and the 17 near-miss `decision`
> rules examined and rejected by that sweep are pinned to stay `decision` at `:239`.
>
> Measured distribution on 2026-10-14: `decision` 154, `defect` 152,
> `default_with_exceptions` 110, `decision_with_floor` 1. Total still 417.
>
> Two further corpus-wide changes landed since the first drift note and are relevant to
> anyone working this hold list:
> - **A `decision`-kind rule may no longer carry a `violates` flag at all** - it is a build
>   failure (`decision_kind_rules_with_violates_flag`, `crates/rules/src/lib.rs:2007`). A
>   `decision`-kind rule whose option prose opens with `Rejected:` is reported by an
>   ADVISORY gate instead (`decision_kind_rules_with_rejected_prose`, `:1963`), pinned to an
>   equality roster precisely because the A5.7 hold below is still open. Read that roster as
>   a shortlist of rules whose kind is probably wrong.
> - **Every rule now also declares `inspectable`** (W16), which is orthogonal to `kind`: it
>   says whether a scan can assess the rule at all. 26 rules are emit-only. Classification
>   decisions below do not change it and are not changed by it.
>
> Semantics for all four kinds, with consumers cited: `docs/RULES_REFERENCE.md` section 3.6.

---

## NEEDS OWNER DECISION (58 rules)

Every rule below is currently shipped as `kind = "decision"` (the safe default — no option is
marked `violates`, so none of these can emit a false defect). Each line states the specific tension
that made automatic classification unsafe. Zach's call changes `kind` (and, where noted, adds
`violates` flags) directly in the rule's TOML.

| Rule id | Tension |
|---|---|
| `ARCH-OBSERVABILITY-CORRELATION-ID-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `ARCH-OBSERVABILITY-SILENT-FAILURE-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `ARCH-PARALLEL-INDEPENDENT-1` | the flagged option just states no project-wide default exists (a process/consistency note), not a degenerate implementation — genuine architecture-taste tension between sequential-by-default and concurrent-by-default. |
| `ARCH-SERVER-CLIENT-PROJECTION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `ARCH-TRUNK-SYNC-1` | trunk-only (no release branch) is a legitimate branching strategy for a team that doesn't cut release branches at all, not a degenerate case of THIS rule (which only governs sync once release branches exist). |
| `CICD-BUILD-PROVENANCE-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `CICD-DEPENDENCY-AUDIT-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `CSHARP-TESTING-MOCK-AT-BOUNDARY-1` | genuine, unsettled testing-philosophy debate (mock-at-boundary vs. integration-style with real infra) — both are legitimate, widely-practiced schools, not a defect/non-defect pair. |
| `CSHARP-TESTING-SEPARATE-UNIT-PROJECT-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `GO-ACCEPT-INTERFACES-RETURN-STRUCTS-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `GO-HANDLER-SERVICE-REPOSITORY-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `GO-TESTING-COLOCATED-TEST-FILES-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `INTEGRATION-EVENT-WIRING-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `JAVA-TESTING-MOCK-BOUNDARIES-1` | same testing-philosophy tension as CSHARP-TESTING-MOCK-AT-BOUNDARY-1. |
| `JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1` | the flagged option (shared logic duplicated across controllers) is a DRY/maintenance concern, not an absence of the cross-cutting behavior itself — both options still implement the same auth/logging logic, just in different places. |
| `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` | trimming dependency arrays is a well-known React footgun in general, but has legitimate, deliberate uses (intentionally-stale closures) that the rule's own text doesn't clearly rule in or out. |
| `JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `PROC-MIGRATION-ROLE-GRANTS-1` | deferred-grant timing is a genuine process tension (could be a real access-control gap if grants are forgotten, or a legitimate phased rollout) that needs the owner's read on the team's migration discipline. |
| `PROCESS-ADO-LINK-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `PROCESS-BRANCH-NAMING-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `PROCESS-COMMIT-DOC-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `PROCESS-CONVENTIONAL-COMMIT-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |
| `PYTHON-DJANGO-FAT-MODEL-SERVICE-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `PYTHON-DJANGO-FORMS-SERIALIZERS-VALIDATE-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `PYTHON-FLASK-SERVICE-LAYER-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `PYTHON-SERVICE-LAYER-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `PYTHON-TESTING-MOCK-BOUNDARIES-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `RUBY-CONCERNS-JUDICIOUSLY-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `RUBY-FROZEN-STRING-LITERAL-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `RUBY-RAILS-SKINNY-CONTROLLERS-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `RUBY-TESTING-INTEGRATION-LOCATION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `RUBY-TESTING-UNIT-FILE-LOCATION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `RUST-SEAORM-RAW-SQL-ESCAPE-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SEC-NO-COMMAND-INJECTION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SEC-NO-PRIVATE-KEY-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SEC-NO-VENDOR-TOKEN-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SPIRIT-ROBUSTNESS-1` | directly touches the owner's own stated robustness-over-terseness preference; whether 'always maximally defensive' is itself a violation or just an extreme-but-valid reading is the owner's call, not a safe inference. |
| `SQL-PRIVILEGED-WRITE-AUDIT-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-AUTH-EMAIL-CONFIRMATION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-AUTH-GETSESSION-SERVER-1` | this rule's alternatives describe DETECTOR SCOPE (flag everywhere vs. only server paths), not the scanned repo's own behavior — the kind taxonomy was designed for repo-behavior alternatives and doesn't map cleanly onto detector-precision options. |
| `SUPABASE-AUTH-MFA-PRIVILEGED-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-AUTH-REDIRECT-ALLOWLIST-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1` | detector-precision meta-rule (see SUPABASE-AUTH-GETSESSION-SERVER-1's note) — options describe how broadly the CHECK should fire, not a codebase choice. |
| `SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1` | detector-precision meta-rule — options describe how broadly the CHECK should fire, not a codebase choice. |
| `SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1` | detector-precision meta-rule — options describe how broadly the CHECK should fire, not a codebase choice. |
| `SUPABASE-FUNC-SEARCH-PATH-1` | detector-precision meta-rule — options describe how broadly the CHECK should fire, not a codebase choice. |
| `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` | the alt option is a CAMERATA rule-coverage meta-decision (whether to author a separate detector axis at all), not the scanned repo's own behavior — same tension as the Supabase detector-precision family. |
| `SUPABASE-RLS-DISABLED-NOT-RESTORED-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-RLS-INITPLAN-1` | the alternatives describe how CAMERATA'S OWN REPORT should classify an initplan finding (opt-in perf note vs. mandatory security finding), not the scanned repo's own behavior. |
| `SUPABASE-RLS-NO-POLICY-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-RLS-PERMISSIVE-TRUE-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-RLS-USER-METADATA-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-STORAGE-KEY-SANITIZATION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-STORAGE-OBJECT-POLICY-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-STORAGE-SIGNED-URL-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `SUPABASE-STORAGE-UPLOAD-CONSTRAINTS-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `TESTING-AS-DOCUMENTATION-1` | no option reads as a clear degenerate/absence form, but the rule's own question is phrased as a yes/no compliance check, not an open style choice — could be an under-authored default_with_exceptions rather than a genuine decision. |
| `UI-QUERY-LIBRARY-1` | every option reads as degenerate and none is declared the corpus default, so which (if any) is actually intended as correct cannot be inferred safely. |

---

## Full classification (clear)

Grouped by kind. `violates` lists the option id(s) marked as the degenerate/absent form of the
rule's requirement (empty for every `decision`-kind rule, by design — A5.1).

### `defect` (132)

| Rule id | violates option id(s) |
|---|---|
| `ARCH-BOUNDARY-VALIDATION-1` | `every-layer-controller-service-repository-re-val`, `no-validation-step-runs-at-the-controller-bounda` |
| `ARCH-CURRENCY-CONSISTENCY-1` | `assume-uniform-minor-unit-and-single-currency-throughout` |
| `ARCH-HANDLER-NO-DB-1` | `handlers-may-query-the-database-directly` |
| `ARCH-IAC-1` | `infrastructure-is-provisioned-manually-through-a`, `secrets-committed-directly-into-iac-source-files` |
| `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` | `modules-may-import-any-other-module` |
| `ARCH-NO-LEAKY-ERROR-DETAILS-1` | `forward-raw-exception-message-gated-by-environment-check`, `forward-raw-exception-message-unconditionally` |
| `ARCH-NO-PAN-STORAGE-1` | `accept-raw-card-data-server-side-and-pass-through` |
| `ARCH-NO-STORE-SENSITIVE-RESPONSES-1` | `no-explicit-caching-header-rely-on-framework-default` |
| `ARCH-OBSERVABILITY-LOG-CONSISTENCY-1` | `allow-mixed-logging-styles` |
| `ARCH-PAYMENT-RECONCILIATION-1` | `trust-the-synchronous-response-as-final` |
| `ARCH-REFUND-AUTHORIZATION-1` | `role-permission-check-only-no-amount-bound` |
| `ARCH-RESOURCE-LIFECYCLE-1` | `subprocess-and-temp-lifecycle-are-left-implicit` |
| `ARCH-RESPONSE-FIELD-PROJECTION-1` | `default-serialize-whole-model-rely-on-client-to-not-use-extra-fields` |
| `ARCH-SERVER-AUTHZ-1` | `the-ui-inspects-raw-permission-codes-or-role-mem` |
| `ARCH-SERVER-COMPUTED-AMOUNT-1` | `trust-client-supplied-amount-with-display-validation-only` |
| `ARCH-TOKEN-REFRESH-ROTATION-1` | `non-rotating-refresh-token-reused-indefinitely`, `single-long-lived-access-token-as-the-session` |
| `ARCH-WEBHOOK-SIGNATURE-VERIFICATION-1` | `trust-any-request-shaped-like-a-provider-event` |
| `CICD-LEAST-PRIVILEGE-TOKEN-1` | `no-explicit-permissions-block-inherit-platform-default` |
| `CICD-NO-SECRETS-ON-FORK-PR-1` | `pull-request-target-checks-out-and-executes-fork-head-with-secrets` |
| `CICD-PIN-THIRD-PARTY-ACTIONS-1` | `reference-actions-by-mutable-tag-or-branch` |
| `CICD-PRIVILEGED-DEPLOY-GATE-1` | `privileged-job-runs-unconditionally-on-its-trigger` |
| `CICD-SECRETS-VIA-SECRETS-STORE-1` | `secret-literal-in-workflow-yaml`, `secrets-store-reference-but-unscoped-workflow-wide` |
| `CSHARP-ASPNETCORE-MIDDLEWARE-ORDERING-1` | `ad-hoc-middleware-order` |
| `CSHARP-EFCORE-NO-LAZY-LOADING-1` | `lazy-loading-enabled-interfaces`, `lazy-loading-enabled-proxies` |
| `CSHARP-NO-HARDCODED-SECRETS-1` | `hardcoded-in-comments`, `hardcoded-in-source` |
| `CSHARP-NO-SWALLOWED-EXCEPTIONS-1` | `catch-all-ignore`, `swallow-broad-exceptions` |
| `CSHARP-SQL-PARAMETERIZED-1` | `string-concatenation-with-escaping` |
| `GO-CONTEXT-PROPAGATION-1` | `background-context-in-services`, `context-optional` |
| `GO-GORM-CONTEXT-QUERIES-1` | `no-context-in-queries` |
| `GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1` | `hybrid-interceptor-handler`, `per-handler-inline` |
| `GO-GRPC-STATUS-CODES-CORRECT-1` | `status-all-internal`, `status-inconsistent` |
| `GO-GRPC-STREAMING-CLEANUP-1` | `streaming-close-unchecked`, `streaming-ignore-context` |
| `GO-LOGGING-STRUCTURED-1` | `unstructured-printf-logging` |
| `GO-NO-GOROUTINE-GLOBALS-1` | `sync-globals-minimally` |
| `GO-PACKAGE-BOUNDARIES-CLEAR-1` | `minimal-boundary-enforcement` |
| `GO-SQL-PARAMETERIZED-1` | `interpolate-with-escaping` |
| `GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1` | `sleep-based-timing` |
| `GO-TESTING-HELPER-T-HELPER-1` | `no-t-helper` |
| `GO-WEB-GRACEFUL-SHUTDOWN-1` | `immediate-termination`, `shutdown-no-timeout` |
| `JAVA-NO-HARDCODED-SECRETS-1` | `hardcoded-in-config-files`, `hardcoded-in-source` |
| `JAVA-RESOURCE-MANAGEMENT-1` | `explicit-close-method-calls`, `garbage-collection-finalize` |
| `JAVA-SQL-PARAMETERIZED-1` | `string-concatenation-with-escaping` |
| `JAVA-TESTING-DETERMINISTIC-1` | `sleep-based-timing`, `static-shared-state` |
| `JAVA-TESTING-UNIT-TEST-LOCATION-1` | `separate-test-package` |
| `JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1` | `services-are-injected-via-constructor-or-inject`, `services-are-manually-instantiated-with-new-where` |
| `JAVASCRIPT-ANGULAR-ROUTE-GUARDS-1` | `authorization-is-checked-in-component-oninitonaf` |
| `JAVASCRIPT-ANGULAR-SUBSCRIPTION-CLEANUP-1` | `manual-subscribe-calls-are-tracked-and-unsubscrib` |
| `JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1` | `protective-response-headers-are-set-ad-hoc-or-le` |
| `JAVASCRIPT-EXPRESS-VALIDATE-INPUT-1` | `request-fields-are-read-directly-and-passed-down` |
| `JAVASCRIPT-NEXT-DUAL-API-1` | `a-single-isomorphic-api-helper-covers-both-brows`, `api-calls-are-made-via-raw-fetch-at-each-call-si` |
| `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` | `secret-correctly-un-prefixed-but-read-from-client-side-code-anyway`, `secret-shaped-value-assigned-a-public-prefixed-variable` |
| `JAVASCRIPT-REACT-KEYED-LISTS-1` | `rendered-lists-use-the-array-index-as-the-key-re` |
| `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` | `hooks-may-be-called-conditionally-or-inside-loop` |
| `JAVASCRIPT-REDUX-NO-STATE-MUTATION-1` | `reducers-mutate-the-existing-state-object-in-pla` |
| `JAVASCRIPT-REDUX-TOOLKIT-DEFAULT-1` | `state-is-built-with-hand-written-action-type-con` |
| `JAVASCRIPT-STRICT-EQUALITY-1` | `loose-equality-is-used-relying-on-javascripts-ty` |
| `JAVASCRIPT-TESTING-AAA-STRUCTURE-1` | `test-body-is-free-form-no-required-structure` |
| `JAVASCRIPT-TESTING-DETERMINISTIC-1` | `tests-use-real-timers-and-may-include-live-network-calls` |
| `JAVASCRIPT-TESTING-NAMING-1` | `test-names-are-free-form-no-convention-enforced` |
| `JAVASCRIPT-TYPESCRIPT-NO-EXPLICIT-ANY-1` | `any-is-permitted-as-an-escape-hatch-wherever-a-t` |
| `JAVASCRIPT-TYPESCRIPT-NO-FLOATING-PROMISES-1` | `promise-returning-calls-may-be-left-floating-whe` |
| `JAVASCRIPT-VUE-COMPUTED-OVER-METHODS-1` | `methods-for-derived-state` |
| `JAVASCRIPT-VUE-PROPS-DOWN-EVENTS-UP-1` | `direct-method-calls-and-shared-state` |
| `JAVASCRIPT-VUE-SCOPED-STYLES-1` | `use-global-css` |
| `ORCH-BUDGET-MONITOR-1` | `no-budget-caps`, `per-session-cap-only` |
| `ORCH-PRECISION-RECALL-1` | `default-to-precision-opt-specific-signals-into-b`, `no-tuning-accept-default-signal-sensitivity` |
| `PROC-PERMISSION-CONFIG-1` | `every-access-gap-is-treated-as-a-code-change-acc`, `every-access-gap-is-treated-as-a-configuration-c` |
| `PROC-PR-CONCURRENCY-1` | `automated-race-detection-in-ci`, `prs-touching-shared-mutable-state-are-reviewed-w` |
| `PYTHON-DJANGO-MIGRATIONS-CHECKED-IN-1` | `manual-ddl`, `migrations-gitignored` |
| `PYTHON-DJANGO-ORM-PARAMETERIZED-1` | `extra-method`, `raw-with-string-formatting` |
| `PYTHON-EXPLICIT-IMPORTS-1` | `permissive-imports` |
| `PYTHON-FASTAPI-AUTH-DEPENDENCY-1` | `in-handler-auth-check` |
| `PYTHON-FASTAPI-PYDANTIC-MODELS-1` | `raw-dict-bodies` |
| `PYTHON-FLASK-AUTH-ON-PROTECTED-ROUTES-1` | `manual-check-in-route-body`, `no-auth-assumed-from-frontend` |
| `PYTHON-FLASK-PARAMETERIZED-SQL-1` | `f-string-in-sql`, `stored-procedures-only` |
| `PYTHON-FLASK-REQUEST-VALIDATION-1` | `manual-dict-access`, `no-validation-trust-client` |
| `PYTHON-PARAMETERIZED-SQL-1` | `interpolate-with-escaping` |
| `PYTHON-TESTING-FIXTURES-CONFTEST-1` | `module-globals`, `setup-teardown-methods` |
| `PYTHON-TESTING-NO-FLAKY-1` | `allow-nondeterminism` |
| `PYTHON-TYPE-HINTS-1` | `annotate-without-checker`, `no-type-hints` |
| `RUBY-AVOID-EVAL-SEND-1` | `eval-with-user-input`, `permitted-symbols` |
| `RUBY-RAILS-CSRF-1` | `csrf-disabled-globally`, `csrf-double-submit-cookie` |
| `RUBY-RAILS-NO-STRING-SQL-1` | `sanitize-sql-for-conditions`, `string-interpolation` |
| `RUBY-TESTING-DESCRIBE-NAMING-1` | `freeform-strings` |
| `RUBY-TESTING-DETERMINISTIC-NO-FLAKY-1` | `fixed-order-no-cleanup` |
| `RUBY-TESTING-MOCK-AT-BOUNDARY-1` | `mock-freely-with-plain-doubles` |
| `RUST-AXUM-MIDDLEWARE-TOWER-1` | `cross-cutting-concerns-inline-in-handlers` |
| `RUST-AXUM-TIMEOUT-LIMITS-1` | `no-timeout-or-limit-configuration` |
| `RUST-SQLX-COMPILE-CHECKED-QUERIES-1` | `use-format-or-string-interpolation` |
| `RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1` | `no-explicit-transactions` |
| `RUST-TESTING-6` | `per-test-cleanup-with-mutex`, `sequential-tests-with-global-state` |
| `RUST-TESTING-7` | `examples-dir-only-no-doc-tests`, `no-examples-prose-doc-only` |
| `RUST-TOKIO-BOUNDED-CHANNELS-1` | `use-unbounded-channels` |
| `RUST-TOKIO-CANCELLATION-SAFE-SELECTS-1` | `select-branches-not-validated` |
| `RUST-TOKIO-NO-STD-MUTEX-ACROSS-AWAIT-1` | `use-std-mutex-in-async-code` |
| `SEC-NO-DISABLED-TLS-1` | `disable-for-dev-convenience` |
| `SEC-NO-HARDCODED-SECRETS-1` | `allow-hardcoded-for-internal-tools` |
| `SEC-NO-OPEN-REDIRECT-1` | — |
| `SEC-NO-PERMISSIVE-CORS-CREDENTIALS-1` | — |
| `SEC-NO-QUERY-GRAMMAR-INJECTION-1` | — |
| `SEC-NO-SECRET-FILE-1` | `allow-secret-files-with-gitignore` |
| `SEC-NO-SENSITIVE-DATA-IN-LOGS-1` | `log-values-as-is-rely-on-log-store-access-controls` |
| `SEC-NO-UNSAFE-DESERIALIZATION-1` | `allow-on-trusted-input` |
| `SEC-NO-UNSAFE-HTML-SINK-1` | — |
| `SEC-NO-WEAK-TOKEN-RANDOMNESS-1` | — |
| `SPIRIT-DOC-DECISIONS-1` | `decisions-accumulate-in-conversation-as-they-are` |
| `SPIRIT-OPTIMIZE-1` | `performant-patterns-are-added-reactively-after-p` |
| `SQL-DB-NPLUSONE-1` | `code-fetches-a-list-of-rows-and-then-loops-over` |
| `SQL-FK-CASCADE-FINANCIAL-1` | `cascade-deletes-uniformly-regardless-of-referenced-table-semantics` |
| `SQL-INVARIANTS-CONSTRAINTS-1` | `application-code-is-the-sole-enforcement-layer-for-invariants` |
| `SQL-MONEY-FLOAT-1` | `use-floating-point-for-money-and-round-at-display-time` |
| `SQL-SOFT-DELETE-PARTIAL-INDEX-1` | `require-every-caller-to-filter-soft-deleted-rows-by-hand` |
| `SQL-TIMESTAMPTZ-1` | `store-event-timestamps-as-zone-less-and-track-the-write-zone-separately` |
| `SUPABASE-AUTH-ANON-SIGNIN-1` | `enable-anonymous-signins-without-reviewing-existing-policies` |
| `SUPABASE-AUTH-EDGE-JWT-1` | `flag-verify-jwt-false-as-always-critical` |
| `SUPABASE-AUTH-PASSWORD-POLICY-1` | `accept-the-permissive-default-password-policy` |
| `SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1` | `trust-service-role-usage-in-any-server-side-code` |
| `SUPABASE-AUTH-SESSION-LIFETIME-1` | `disable-rotation-or-extend-expiry-for-fewer-refreshes` |
| `SUPABASE-AUTH-USERS-EXPOSED-1` | `allow-column-restricted-views-over-auth-users` |
| `SUPABASE-EXPOSURE-MATVIEW-1` | `ignore-materialized-views-treat-as-equivalent-to-regular-views` |
| `SUPABASE-EXPOSURE-SCHEMAS-1` | `hardcode-public-as-the-only-exposed-schema-everywhere` |
| `SUPABASE-REALTIME-AUTHORIZATION-1` | `treat-application-table-rls-as-sufficient-for-realtime-channels` |
| `SUPABASE-REALTIME-CHANNEL-SCOPE-1` | `treat-an-unguessable-topic-name-as-sufficient-access-control` |
| `SUPABASE-REALTIME-PAYLOAD-EXPOSURE-1` | `broadcast-the-full-row-and-filter-sensitive-fields-client-side` |
| `SUPABASE-REALTIME-PRESENCE-IDENTITY-1` | `pass-the-full-user-object-to-track-for-convenience` |
| `SUPABASE-RLS-ENABLED-1` | `regex-grep-each-migration-independently` |
| `SUPABASE-RLS-POLICY-DISABLED-1` | `fold-into-bare-missing-rls-finding` |
| `SUPABASE-RLS-VIEW-INVOKER-1` | `allow-owner-privileged-views-for-convenience` |
| `SUPABASE-STORAGE-DOWNLOAD-OWNERSHIP-1` | `trust-the-storage-objects-rls-policy-alone` |
| `SUPABASE-STORAGE-PUBLIC-BUCKET-1` | `flag-only-buckets-with-suspicious-names-as-critical` |
| `TESTING-ARRANGE-ACT-ASSERT-1` | `freeform-no-required-structure` |
| `UI-AUTH-TOKEN-STORAGE-1` | `cookie-set-via-client-side-javascript-no-httponly`, `store-token-in-local-or-session-storage` |

### `default_with_exceptions` (117)

| Rule id | violates option id(s) |
|---|---|
| `AGENTIC-NO-TEST-TAMPER-1` | `no-restriction-on-test-edits` |
| `ARCH-API-DTOS-1` | `endpoints-return-domain-types-directly-in-the-re` |
| `ARCH-API-VERSIONING-1` | `the-api-has-no-version-segment-at-all-and-evolve` |
| `ARCH-CURSOR-PAGINATION-1` | `list-endpoints-paginate-by-cursor-not-by-offset` |
| `ARCH-EXACT-DECIMALS-1` | `monetary-and-quantity-values-are-represented-in` |
| `ARCH-FETCH-THEN-AUTHORIZE-1` | `checks-in-business-logic` |
| `ARCH-HOT-READ-CACHE-1` | `hot-reads-run-directly-against-the-database-with`, `hot-reads-sit-behind-a-ttl-cache-with-no-write-s` |
| `ARCH-IDEMPOTENCY-KEYS-1` | `the-api-accepts-retries-without-any-idempotency`, `the-api-does-no-server-side-deduplication-at-all` |
| `ARCH-MIDDLEWARE-FIRST-1` | `cross-cutting-concerns-are-inlined-in-each-handl` |
| `ARCH-NO-SECRETS-IN-URL-1` | `rely-on-client-js-to-intercept-the-submit` |
| `ARCH-REPO-PER-AGGREGATE-1` | `the-domain-layer-declares-no-repository-abstract`, `the-domain-layer-declares-one-repository-abstrac` |
| `ARCH-REPO-RETURNS-DOMAIN-1` | `repository-methods-return-the-persistence-librar` |
| `ARCH-SERVICE-DI-1` | `global-service-locator` |
| `ARCH-STRICT-LAYERING-1` | `controllers-services-and-repositories-are-not-se` |
| `ARCH-STRUCTURED-ERRORS-1` | `error-responses-are-plain-strings-or-contain-onl` |
| `ARCH-WEBHOOK-EVENT-DEDUP-1` | `apply-the-effect-unconditionally-on-every-delivery` |
| `CICD-NO-SECRET-LOGGING-1` | `rely-on-automatic-platform-redaction-only` |
| `CSHARP-ASPNETCORE-ASYNC-ACTIONS-1` | `async-void-handlers`, `sync-actions-blocking` |
| `CSHARP-ASPNETCORE-AUTHORIZE-ATTRIBUTE-1` | `authorize-per-action` |
| `CSHARP-ASPNETCORE-CORS-EXPLICIT-1` | `no-cors-configured` |
| `CSHARP-ASPNETCORE-MODEL-VALIDATION-1` | `no-model-validation` |
| `CSHARP-ASPNETCORE-OPTIONS-PATTERN-1` | `hardcoded-fallbacks` |
| `CSHARP-ASPNETCORE-THIN-CONTROLLERS-1` | `no-repository-over-ef-core` |
| `CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1` | `service-locator` |
| `CSHARP-EFCORE-ASNOTRACKING-1` | `always-track` |
| `CSHARP-EFCORE-DBCONTEXT-SCOPED-1` | `transient-dbcontext` |
| `CSHARP-EFCORE-EXPLICIT-TRANSACTIONS-1` | `multiple-saveschanges-no-transaction` |
| `CSHARP-EFCORE-MIGRATIONS-CHECKED-IN-1` | `ensure-created-no-migrations` |
| `CSHARP-EFCORE-NO-NPLUS1-1` | `lazy-loading-via-proxies` |
| `CSHARP-EFCORE-PARAMETERIZED-LINQ-1` | `string-concatenation-raw-sql` |
| `CSHARP-IDISPOSABLE-USING-1` | `manual-dispose` |
| `CSHARP-NULLABLE-REFERENCE-TYPES-1` | `nullable-ref-types-disabled` |
| `CSHARP-TESTING-AAA-STRUCTURE-1` | `logic-in-test` |
| `CSHARP-TESTING-DETERMINISTIC-NO-FLAKY-1` | `shared-static-state` |
| `CSHARP-TESTING-INTEGRATION-WEBAPPLICATIONFACTORY-1` | `no-integration-tests` |
| `CSHARP-TESTING-NAMING-METHOD-SCENARIO-RESULT-1` | `no-convention` |
| `CSHARP-TESTING-XUNIT-CONSTRUCTOR-ISOLATION-1` | `shared-constructor-setup` |
| `GO-ERRORS-MUST-BE-CHECKED-1` | `ignore-low-consequence-errors` |
| `GO-GORM-MIGRATIONS-MANAGED-1` | `autoversion-only`, `manual-sql-alters` |
| `GO-GORM-NO-N-PLUS-ONE-1` | `lazy-loading-in-loop` |
| `GO-GORM-PARAMETERIZED-QUERIES-1` | `string-interpolation` |
| `GO-GORM-TRANSACTIONS-MULTI-WRITE-1` | `no-transactions` |
| `GO-GRPC-CONTEXT-DEADLINES-1` | `ignore-deadlines` |
| `GO-TESTING-INTEGRATION-BUILD-TAGS-1` | `no-separation` |
| `GO-WEB-HANDLER-TIMEOUTS-1` | `no-timeouts` |
| `GO-WEB-MIDDLEWARE-CROSS-CUTTING-1` | `per-handler-inline` |
| `GO-WEB-REQUEST-BINDING-VALIDATION-1` | `raw-map-access` |
| `GO-WEB-STRUCTURED-ERROR-RESPONSES-1` | `ad-hoc-error-format` |
| `INTEGRATION-API-CONTRACT-1` | `no-cross-agent-contract-check` |
| `INTEGRATION-AUTH-SEAM-1` | `no-auth-seam-check` |
| `JAVA-EXCEPTION-HANDLING-1` | `catch-and-return-sentinel` |
| `JAVA-IMMUTABILITY-FINAL-1` | `mutable-by-default` |
| `JAVA-LOGGING-STRUCTURED-1` | `system-print-statements` |
| `JAVA-OPTIONAL-OVER-NULL-1` | `return-null` |
| `JAVA-PACKAGE-BY-FEATURE-1` | `no-hierarchy` |
| `JAVA-SMALL-INTERFACES-1` | `no-interfaces` |
| `JAVA-SPRING-CONSTRUCTOR-INJECTION-1` | `setter-injection` |
| `JAVA-SPRING-DTO-BOUNDARY-1` | `entities-as-api-objects` |
| `JAVA-SPRING-LAYERED-ARCHITECTURE-1` | `no-formal-layering` |
| `JAVA-SPRING-METHOD-SECURITY-1` | `manual-role-check-in-controller`, `preauthorize-on-controller-methods` |
| `JAVA-SPRING-NO-NPLUS1-FETCH-JOIN-1` | `lazy-with-open-session-in-view` |
| `JAVASCRIPT-NO-VAR-1` | `var-remains-permitted-alongside-const-and-let-le` |
| `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1` | `the-non-null-assertion-operator-is-permitted-to` |
| `JAVASCRIPT-TYPESCRIPT-STRICT-MODE-1` | `the-compiler-runs-with-strict-mode-off-or-with-i` |
| `ORCH-CONFLICTING-ROBUSTNESS-1` | `when-options-are-each-robust-against-different-f` |
| `ORCH-CONFORMANCE-1` | `architectural-commitments-are-held-as-advisory-d`, `architectural-commitments-live-only-in-the-agent` |
| `ORCH-CONTEXT-OVERRIDE-1` | `documented-rules-are-absolute-the-agent-never-ov` |
| `ORCH-NEW-PATH-TESTS-1` | `the-change-pipeline-requires-the-project-to-stay` |
| `ORCH-NOVELTY-1` | `the-agent-searches-for-analogies-in-adjacent-cod` |
| `ORCH-ONE-WAY-DOOR-1` | `the-agent-auto-decides-whenever-the-winner-feels` |
| `ORCH-TIERED-ESCALATION-1` | `never-pause` |
| `PROC-HIDE-DEAD-END-1` | `the-ui-renders-every-action-regardless-of-the-cu` |
| `PROC-REGRESSION-TEST-1` | `the-bug-fix-lands-with-a-regression-test-only-wh`, `the-bug-fix-lands-without-a-regression-test-but`, `the-bug-fix-lands-without-any-accompanying-test` |
| `PYTHON-DJANGO-CSRF-AUTH-1` | `permission-classes-drf`, `remove-csrf-middleware` |
| `PYTHON-DJANGO-SELECT-RELATED-1` | `lazy-loading-accepted` |
| `PYTHON-DJANGO-SETTINGS-FROM-ENV-1` | `hardcoded-in-settings` |
| `PYTHON-FASTAPI-DI-SESSION-1` | `session-in-handler` |
| `PYTHON-FLASK-CONFIG-FROM-ENV-1` | `hardcoded-in-source` |
| `PYTHON-FLASK-ERROR-HANDLERS-1` | `flask-default-error-pages` |
| `PYTHON-NO-BARE-EXCEPT-1` | `allow-broad-catch` |
| `PYTHON-NO-BLOCKING-IO-ASYNC-1` | `blocking-in-async` |
| `PYTHON-SETTINGS-FROM-ENV-1` | `hardcoded-config` |
| `PYTHON-TESTING-UNIT-LAYOUT-1` | `scattered` |
| `RUBY-RAILS-BACKGROUND-JOBS-1` | `thread-spawn` |
| `RUBY-RAILS-EAGER-LOAD-1` | `select-n-plus-1-accepted` |
| `RUBY-RAILS-MODEL-VALIDATIONS-1` | `client-side-validation-only`, `controller-validation-only` |
| `RUBY-RAILS-SCOPES-1` | `default-scope` |
| `RUBY-RAILS-STRONG-PARAMS-1` | `no-mass-assignment-protection` |
| `RUBY-TESTING-FACTORIES-OVER-FIXTURES-1` | `bare-active-record-create` |
| `RUST-AXUM-STATE-SHARED-DEPS-1` | `use-global-statics-for-dependencies` |
| `RUST-DIOXUS-12` | `icons-are-loaded-from-an-external-svg-sprite-she` |
| `RUST-DIOXUS-13` | `form-fields-are-validated-only-on-the-server-the` |
| `RUST-DIOXUS-14` | `the-ui-has-no-primitives-layer-views-inline-util` |
| `RUST-DIOXUS-3` | `local-component-state-uses-the-legacy-hook-based` |
| `RUST-DIOXUS-4` | `cross-tree-state-is-held-in-global-statics-wrapp`, `cross-tree-state-is-threaded-through-props-at-ev` |
| `RUST-DIOXUS-6` | `ui-data-fetching-runs-directly-in-the-component` |
| `RUST-DIOXUS-8` | `lists-rendered-in-templates-omit-the-key-attribu`, `templates-are-rendered-as-concatenated-strings-t` |
| `RUST-DOMAIN-3` | `newtype-validated-strings-email-url-slug`, `the-codebase-makes-no-distinction-between-valida` |
| `RUST-DOMAIN-6` | `each-function-or-aggregate-defines-its-own-error`, `errors-are-category-scoped-one-shared-enum-per-f` |
| `RUST-ENTITIES-1` | `every-entity-is-generated-by-the-seaorm-cli-gene` |
| `RUST-ENTITIES-10` | `the-entity-stores-its-audit-relationships-create` |
| `RUST-ENTITIES-4` | `the-entities-crate-declares-no-foreign-key-relat` |
| `RUST-ENTITIES-6` | `entity-files-carry-no-citation-of-the-source-of`, `the-source-of-truth-schema-citation-lives-in-a-s` |
| `RUST-ENTITIES-7` | `the-composite-or-partial-uniqueness-constraint-i`, `the-entity-carries-no-uniqueness-annotation-and` |
| `RUST-NO-UNWRAP-1` | `discourage-unwrap-by-convention-and-code-review` |
| `RUST-SEAORM-INTRA-AGGREGATE-TX-1` | `no-repository-transaction-boundaries` |
| `RUST-TESTING-4` | `free-form-test-body` |
| `RUST-TOKIO-NO-BLOCKING-IN-ASYNC-1` | `blocking-calls-inline-in-async` |
| `RUST-TOKIO-STRUCTURED-CONCURRENCY-JOINSET-1` | `use-unstructured-spawn` |
| `SPIRIT-FILE-SIZE-1` | `hand-maintained-files-are-allowed-to-grow-withou` |
| `SQL-AUDIT-COLUMNS-1` | `no-table-in-the-schema-carries-any-audit-columns`, `the-audit-columns-are-written-by-orm-level-lifec` |
| `SQL-DB-INDEX-1` | `the-project-relies-on-the-database-engine-to-aut` |
| `SQL-ENUM-TEXT-CONSTRAINT-1` | `leave-closed-set-columns-as-unconstrained-text` |
| `TESTING-DETERMINISTIC-1` | `retry-on-failure` |
| `TESTING-ONE-ASSERTION-PER-TEST-1` | `no-assertion-count-constraint` |
| `TESTING-PYRAMID-1` | `no-explicit-distribution-policy` |
| `UI-CONSENT-GATED-1` | `consent-is-checked-once-at-the-page-or-route-lev` |

### `decision` (110)

| Rule id | violates option id(s) |
|---|---|
| `ARCH-EXPAND-CONTRACT-1` | — |
| `ARCH-EXPLICIT-TX-1` | — |
| `ARCH-MONOLITH-FIRST-1` | — |
| `ARCH-TRIGGER-ENV-1` | — |
| `ARCH-TYPED-PATH-PARAMS-1` | — |
| `ARCH-UTC-TIMESTAMPS-1` | — |
| `CICD-CODEQL-SECURITY-SCAN-1` | — |
| `CICD-SEMGREP-SECURITY-SCAN-1` | — |
| `CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1` | — |
| `CSHARP-IENUMERABLE-COLLECTIONS-1` | — |
| `CSHARP-RECORDS-IMMUTABILITY-1` | — |
| `CSHARP-SMALL-FOCUSED-INTERFACES-1` | — |
| `GO-ERROR-WRAPPING-WITH-FMT-W-1` | — |
| `GO-GOROUTINE-LEAKS-DEFER-CLEANUP-1` | — |
| `GO-GRPC-PROTO-GENERATED-IMMUTABLE-1` | — |
| `GO-SMALL-INTERFACES-1` | — |
| `GO-TESTING-BLACK-BOX-PKG-TEST-1` | — |
| `GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1` | — |
| `GO-TESTING-TABLE-DRIVEN-T-RUN-1` | — |
| `JAVA-SPRING-CONFIGURATION-PROPERTIES-1` | — |
| `JAVA-SPRING-THIN-CONTROLLERS-1` | — |
| `JAVA-SPRING-TRANSACTIONAL-SERVICE-LAYER-1` | — |
| `JAVA-SPRING-VALID-REQUEST-VALIDATION-1` | — |
| `JAVA-TESTING-AAA-STRUCTURE-1` | — |
| `JAVA-TESTING-ASSERTIONS-ASSERTJ-1` | — |
| `JAVA-TESTING-INTEGRATION-TEST-LOCATION-1` | — |
| `JAVA-TESTING-NAMING-1` | — |
| `JAVASCRIPT-ANGULAR-AVOID-LOGIC-IN-TEMPLATES-1` | — |
| `JAVASCRIPT-ANGULAR-LAZY-LOADING-ROUTES-1` | — |
| `JAVASCRIPT-ANGULAR-NO-DIRECT-DOM-MANIPULATION-1` | — |
| `JAVASCRIPT-ANGULAR-ONPUSH-CHANGE-DETECTION-1` | — |
| `JAVASCRIPT-ANGULAR-REACTIVE-FORMS-1` | — |
| `JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1` | — |
| `JAVASCRIPT-ANGULAR-STANDALONE-COMPONENTS-1` | — |
| `JAVASCRIPT-ANGULAR-TYPED-HTTPCLIENT-1` | — |
| `JAVASCRIPT-CONST-DEFAULT-1` | — |
| `JAVASCRIPT-EXPRESS-CENTRAL-ERROR-HANDLER-1` | — |
| `JAVASCRIPT-EXPRESS-THIN-CONTROLLERS-1` | — |
| `JAVASCRIPT-NEST-DTOS-VALIDATION-PIPE-1` | — |
| `JAVASCRIPT-NEST-GUARDS-FOR-AUTH-1` | — |
| `JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1` | — |
| `JAVASCRIPT-NEST-THIN-CONTROLLERS-DELEGATE-1` | — |
| `JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1` | — |
| `JAVASCRIPT-REACT-FUNCTION-COMPONENTS-1` | — |
| `JAVASCRIPT-REDUX-NO-EFFECTS-IN-REDUCERS-1` | — |
| `JAVASCRIPT-REDUX-SERIALIZABLE-STATE-1` | — |
| `JAVASCRIPT-TESTING-INTEGRATION-LOCATION-1` | — |
| `JAVASCRIPT-TESTING-MOCK-AT-BOUNDARIES-1` | — |
| `JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1` | — |
| `JAVASCRIPT-TESTING-UNIT-COLOCATION-1` | — |
| `JAVASCRIPT-VUE-COMPOSITION-SCRIPT-SETUP-1` | — |
| `JAVASCRIPT-VUE-NO-DIRECT-DOM-1` | — |
| `ORCH-AUTOCALLS-LEDGER-1` | — |
| `ORCH-CLEAR-WINNER-1` | — |
| `ORCH-ENV-GATED-QUALITY-1` | — |
| `ORCH-MODEL-TIERING-1` | — |
| `ORCH-NO-NATURAL-BREAK-1` | — |
| `ORCH-OUTPUT-DIGEST-1` | — |
| `ORCH-PREREVIEW-1` | — |
| `ORCH-REVIEWER-SPLIT-1` | — |
| `ORCH-TRAINING-CUTOFF-1` | — |
| `PROC-AUTO-MERGE-1` | — |
| `PROC-CI-MIGRATION-HYGIENE-1` | — |
| `PROC-CITE-CONVENTION-ID-1` | — |
| `PROC-FEATURE-FLAGS-1` | — |
| `PROC-STORY-DOCS-1` | — |
| `PYTHON-FLASK-APP-FACTORY-1` | — |
| `PYTHON-ORM-EAGER-LOAD-1` | — |
| `PYTHON-TESTING-AAA-STRUCTURE-1` | — |
| `PYTHON-TESTING-FILE-NAMING-1` | — |
| `PYTHON-TESTING-PARAMETRIZE-1` | — |
| `RUBY-RAILS-NO-SECRETS-IN-CODE-1` | — |
| `RUBY-SMALL-METHODS-1` | — |
| `RUBY-TESTING-AAA-STRUCTURE-1` | — |
| `RUST-AXUM-EXTRACTORS-VALIDATE-1` | — |
| `RUST-AXUM-HANDLERS-THIN-DELEGATE-1` | — |
| `RUST-AXUM-TYPED-ERROR-RESPONSE-1` | — |
| `RUST-DIOXUS-1` | — |
| `RUST-DIOXUS-11` | — |
| `RUST-DIOXUS-2` | — |
| `RUST-DIOXUS-5` | — |
| `RUST-DIOXUS-7` | — |
| `RUST-DIOXUS-9` | — |
| `RUST-DOMAIN-1` | — |
| `RUST-DOMAIN-2` | — |
| `RUST-DOMAIN-4` | — |
| `RUST-DOMAIN-5` | — |
| `RUST-DOMAIN-7` | — |
| `RUST-ENTITIES-11` | — |
| `RUST-ENTITIES-12` | — |
| `RUST-ENTITIES-13` | — |
| `RUST-ENTITIES-2` | — |
| `RUST-ENTITIES-3` | — |
| `RUST-ENTITIES-5` | — |
| `RUST-ENTITIES-9` | — |
| `RUST-HEADLESS-CORE-1` | — |
| `RUST-MAPPER-1` | — |
| `RUST-PURE-STATE-TRANSITIONS-1` | — |
| `RUST-SQLX-CONNECTION-POOL-SIZED-1` | — |
| `RUST-SQLX-MIGRATIONS-CHECKED-IN-1` | — |
| `RUST-TESTING-1` | — |
| `RUST-TESTING-2` | — |
| `RUST-TESTING-3` | — |
| `RUST-TESTING-5` | — |
| `SEC-NO-RAW-SQL-CONCAT-1` | — |
| `SQL-DB-INDEX-2` | — |
| `TESTING-BEHAVIOR-NOT-IMPLEMENTATION-1` | — |
| `TESTING-FAST-UNIT-TESTS-1` | — |
| `UI-IMAGE-COMPONENT-1` | — |
| `UI-UTC-DATES-1` | — |
