# Supabase Stack Rules — Taxonomy + Grounding Spec

**Date:** 2026-07-26 · **Branch:** `feat/audit-report-export` · **Status:** design (rules to be authored by a follow-up Sonnet pass)

**Audience:** the rule-authoring agent. This doc specifies WHAT rules to write, at which tier, with which grounding. It writes no TOML itself.

**Commercial context:** the brownfield-audit prospect pool is heavily vibe-coded Supabase monoliths (Lovable / Bolt / Cursor output). The buyer wants their RLS hardened AS-IS, not replatformed. Every finding must therefore name the concrete exposed object ("your `profiles` table"), never the abstract principle.

---

## 1. Corpus placement, naming, schema conventions

- **Folder:** `crates/rules/principles/supabase/` with subfolders per group. Domain is folder-derived (`load_one` joins path components with `:`), so `supabase/rls/…` → domain `supabase:rls`. Mirrors the existing `javascript/react` pattern.
  - Subfolders: `rls/`, `auth/`, `secrets/`, `storage/`, `database-functions/`, `exposure/`.
- **Rule ids:** `SUPABASE-<AREA>-<NAME>-1` (uppercase, matches `JAVASCRIPT-NO-VAR-1` convention).
- **Header fields** (match `javascript-no-var-1.toml`): `tag = "stack"`, `domain = "supabase:<area>"` (must agree with folder or a warning logs; derived wins), `layer = "platform"`, `enforcement = prose|structured|mechanical|architectural`, `default = true`, `qualifies = "…"` (one-paragraph conformance statement), `verification`, `[[sources]]`, `[decision]` + `[[option]]` blocks in the SEC-NO-UNSAFE-DESERIALIZATION-1 voice (default option + at least one rejected alternative with the rejection rationale).
- **Verification honesty (hard rule):** `grounded` ONLY with a real external URL in `[[sources]]`. Camerata stances with no external authority are `policy`. Never fabricate a citation. Where a rule mirrors a real Supabase Splinter lint, put it in the source's `linter` field as `"splinter: <lint_name>"` — Splinter is Supabase's own database linter (`supabase db lint` / dashboard Security Advisor), which makes these rules grounded in exactly the way `eslint: no-var` grounds `JAVASCRIPT-NO-VAR-1`. Splinter lint URLs follow `https://supabase.com/docs/guides/database/database-advisors?lint=<NNNN>_<name>`; the author must open each URL and confirm the lint name/number before citing (do not trust the numbers below blindly).
- **`domain_to_glob`** (`crates/rules/src/lib.rs`) has no `supabase` mapping → falls back to `**` with a warning. Acceptable for now (Supabase rules span `.sql`, `.ts`, `.toml`, `.env`); note it, don't fix it in the authoring pass.
- **Tiering vocabulary** (from `onboard.rs`): the **deterministic floor** = `AUDIT_RULES` content rules — near-zero-FP, always-block, run on every repo. Everything else is advisory: `structured` (thresholdable, may carry `needs_review`), `architectural` (deterministic but needs a parse pass, not a regex), `prose` (routes to Claude with evidence attached). Per the floor-first layering rule: only near-zero-FP + always-block-worthy checks go in the floor.
- **Severities** use the existing finding vocabulary: `critical` / `high` / `medium` (`low` reserved for test-scope downgrades).

---

## 2. Rule inventory

### 2.1 RLS (`supabase/rls/` → domain `supabase:rls`) — the load-bearing group

| id | Detects | Buyer framing | Tier | Grounding | Sev |
|---|---|---|---|---|---|
| `SUPABASE-RLS-ENABLED-1` | Table created in an API-exposed schema (`public` by default) with no `ENABLE ROW LEVEL SECURITY` anywhere in the migration history | "Your `profiles` table has no Row Level Security. Anyone holding your public API key — which ships in your frontend — can read and write every row." | **architectural** (migration-timeline replay, §4) | Supabase RLS guide: <https://supabase.com/docs/guides/database/postgres/row-level-security>; splinter `rls_disabled_in_public` (lint 0013); OWASP A01:2021 Broken Access Control → `grounded` | critical |
| `SUPABASE-RLS-POLICY-DISABLED-1` | `CREATE POLICY` exists for a table whose RLS is never enabled — policies written but silently inactive | "You wrote access rules for `orders`, but they are switched off. The table looks protected in your code and is fully open in production." | **architectural** (same replay pass) | splinter `policy_exists_rls_disabled` (lint 0007); same RLS guide → `grounded` | critical |
| `SUPABASE-RLS-NO-POLICY-1` | RLS enabled but zero policies → deny-all for anon/authenticated (breakage or a sign the app bypasses RLS via service_role) | "`invoices` is locked to everyone. Either a feature is broken, or your app is reading it with the master key — which skips security entirely." | **architectural** (same replay pass) | splinter `rls_enabled_no_policy` (lint 0008) → `grounded` | medium |
| `SUPABASE-RLS-PERMISSIVE-TRUE-1` | `CREATE POLICY … USING (true)` / `WITH CHECK (true)` targeting `anon`/`public` (an absent `TO` clause defaults to `public`) — flag writes at high, reads at medium with `needs_review` (public read of reference data can be intentional) | "Your `messages` table has a rule that literally says 'everyone is allowed'. That is not a policy; it is a hole shaped like one." | **structured** (statement-granular regex once statements are split; write vs read severity split) | RLS guide (policy semantics); OWASP A01:2021 → `grounded` | high (write) / medium (read) |
| `SUPABASE-RLS-USER-METADATA-1` | RLS policy predicate referencing `user_metadata` / `raw_user_meta_data` (via `auth.jwt()` or joins) — user-editable data used as an authorization input = self-service privilege escalation | "Your admin check reads a field any user can edit about themselves. Anyone can promote themselves to admin with one API call." | **mechanical** — `user_metadata`/`raw_user_meta_data` inside a `CREATE POLICY` statement is near-zero-FP → **floor-port candidate** | Supabase RLS guide, `auth.jwt()` section, which explicitly warns user_metadata is user-updatable and must not be used for authorization: <https://supabase.com/docs/guides/database/postgres/row-level-security#authjwt>; CWE-269 Improper Privilege Management → `grounded` | critical |
| `SUPABASE-RLS-VIEW-INVOKER-1` | View in an exposed schema without `security_invoker = true` — the view executes with its owner's privileges and silently bypasses base-table RLS | "Your `customer_summary` view punches through the security on the tables underneath it. The tables are locked; the window next to them is open." | **structured** (`CREATE VIEW` in `public` lacking `security_invoker`; Postgres ≥15 option) | splinter `security_definer_view` (lint 0010); Postgres `CREATE VIEW` docs (`security_invoker`): <https://www.postgresql.org/docs/current/sql-createview.html> → `grounded` | high |
| `SUPABASE-RLS-INITPLAN-1` (opt-in) | `auth.uid()` / `auth.jwt()` called un-wrapped in a policy (re-evaluated per row instead of once via `(select auth.uid())`) | "Your security rules re-check identity on every single row. Works today; melts the first time a table gets large." | **structured**; performance not security — mark `opt_in_only = true` so it is never pre-ticked | splinter `auth_rls_initplan` (lint 0003) → `grounded` | medium |

**Report precision requirement (all RLS rules):** the finding's `detail` must carry the table/view name and the migration file + line where the state was last established, plus the honest confidence caveat from §4 ("no evidence in repo" ≠ "disabled in production").

### 2.2 Auth (`supabase/auth/` → `supabase:auth`)

| id | Detects | Buyer framing | Tier | Grounding | Sev |
|---|---|---|---|---|---|
| `SUPABASE-AUTH-EDGE-JWT-1` | Edge function with JWT verification off — `verify_jwt = false` in `supabase/config.toml` (`[functions.<name>]`) or `--no-verify-jwt` in deploy scripts/CI — with no compensating auth check inside the function | "Your `charge-card` function accepts requests from anyone on the internet, logged in or not." | **two-stage**: the flag is **mechanical** (regex over config.toml + scripts, near-zero FP on the flag itself); "does the function body do its own auth" is **prose** (Claude reads the function with the flag-finding as evidence). Emit the mechanical finding always; escalate to prose for the compensating-check judgment. | Supabase Edge Function auth docs: <https://supabase.com/docs/guides/functions/auth>; CLI config reference (`functions.verify_jwt`): <https://supabase.com/docs/guides/cli/config>; CWE-306 Missing Authentication for Critical Function → `grounded` | high |
| `SUPABASE-AUTH-GETSESSION-SERVER-1` | Server-side code (Next.js middleware / server components / API routes, SvelteKit hooks, etc.) trusting `getSession()` instead of `getUser()` — `getSession` returns the client's unverified cookie JWT; `getUser` validates against the auth server | "Your server trusts a badge the visitor printed themselves instead of checking it at the desk. A forged cookie can impersonate any user." | **structured** — `getSession(` in server-path files (path heuristics: `middleware.*`, `app/**/route.*`, `+page.server.*`, `api/`); FP: legit client-side usage → path-scoped + `needs_review` on ambiguous paths | Supabase SSR guide, which warns explicitly to use `getUser()` and never trust `getSession()` in server code: <https://supabase.com/docs/guides/auth/server-side/nextjs>; CWE-345 Insufficient Verification of Data Authenticity → `grounded` | high |
| `SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1` | A `service_role`-backed client used inside user-reachable request handlers without an explicit authorization check — service_role bypasses ALL RLS, so every such path must re-implement authorization | "One of your API routes uses the master key on behalf of whoever calls it. Your database security does not apply on that path." | **prose** — whether an authorization check exists is semantic; feed Claude the handler + the client-construction site | Supabase API-keys docs (service_role "bypasses Row Level Security; never expose it"): <https://supabase.com/docs/guides/api/api-keys>; CWE-862 Missing Authorization → `grounded` | high |
| `SUPABASE-AUTH-USERS-EXPOSED-1` | `auth.users` exposed to `anon`/`authenticated` via a view/materialized view/grant in an exposed schema — leaks emails, phone numbers, metadata for every account | "Your entire user list — emails included — is readable through your public API." | **structured** (SQL referencing `auth.users` in `CREATE VIEW`/`GRANT` within exposed schemas) | splinter `auth_users_exposed` (lint 0002) → `grounded` | critical |

### 2.3 Secrets & keys (`supabase/secrets/` → `supabase:secrets`)

| id | Detects | Buyer framing | Tier | Grounding | Sev |
|---|---|---|---|---|---|
| *(extend, don't duplicate)* `SEC-NO-VENDOR-TOKEN-1` | Add Supabase secret shapes to the existing universal vendor-token match-set: (a) `sb_secret_…` new-format secret keys (fixed prefix, near-zero FP); (b) legacy JWT keys whose base64 payload decodes to `"role":"service_role"` (decode step at match time, like the SafeLoader carve-out precedent); (c) Supabase Postgres connection strings with an inline password (`postgres(ql)?://…:…@db.<ref>.supabase.co`) | (inherits vendor-token framing) | **mechanical — already floor** (`AUDIT_RULES` includes `SEC-NO-VENDOR-TOKEN-1`) | Supabase API-keys docs: <https://supabase.com/docs/guides/api/api-keys>; CWE-798 → existing rule stays `grounded` | critical |
| `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` | The *exposure axis* the vendor-token rule can't express: service_role key wired into client-delivered code — env names carrying a client-bundle prefix (`NEXT_PUBLIC_*`, `VITE_*`, `REACT_APP_*`, `EXPO_PUBLIC_*`) combined with `SERVICE_ROLE`/`sb_secret`, or a service_role/`sb_secret_` value referenced in frontend source dirs | "Your master database key is shipped inside the website itself. Anyone who opens dev-tools owns your database — every table, security off." | **mechanical** — the env-prefix + SERVICE_ROLE combination is near-zero-FP → **floor-port candidate** | Same API-keys doc (service_role: "never expose it in the browser"); CWE-798 / CWE-200 → `grounded` | critical |
| `SUPABASE-FUNC-SECRETS-1` | *(prune as a standalone rule)* Hardcoded secrets inside `supabase/functions/**` instead of `Deno.env.get(...)` — already covered by `SEC-NO-HARDCODED-SECRETS-1` / `SEC-NO-VENDOR-TOKEN-1` on the floor. Authoring note only: ensure the file-walker does not exclude `supabase/functions/` as a vendored dir. | — | covered by floor | Edge-function secrets docs, for the report's remediation text: <https://supabase.com/docs/guides/functions/secrets> | — |

**Anon-key note:** the anon/publishable key is *designed* to be public — do NOT flag its presence in client code (a classic naive-scanner FP that would embarrass the report). The anon-key risk is entirely downstream RLS quality, which §2.1 covers. Say this in `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1`'s `qualifies` so the boundary is explicit.

### 2.4 Storage (`supabase/storage/` → `supabase:storage`)

| id | Detects | Buyer framing | Tier | Grounding | Sev |
|---|---|---|---|---|---|
| `SUPABASE-STORAGE-PUBLIC-BUCKET-1` | Bucket created public — `insert into storage.buckets … public` / `update storage.buckets set public = true` in migrations, or `createBucket(name, { public: true })` in code — where a public bucket serves objects to anyone with the URL, no auth, no RLS | "Your `documents` bucket is public: every file in it is downloadable by anyone who has or guesses the link." | **structured** + `needs_review` (public buckets are legitimate for avatars/assets; the finding names the bucket and asks "should `documents` really be public?") | Storage access-control docs: <https://supabase.com/docs/guides/storage/security/access-control>; CWE-732 Incorrect Permission Assignment → `grounded` | high |
| `SUPABASE-STORAGE-OBJECT-POLICY-1` | Over-permissive policies on `storage.objects` — `USING (true)` / no owner- or folder-scoping, `anon` write/delete grants on private buckets | "Anyone can upload into — or delete from — your `uploads` bucket, not just the file's owner." | **structured** — same policy machinery as `SUPABASE-RLS-PERMISSIVE-TRUE-1`, specialized to `storage.objects` so the finding names the bucket (from the policy's `bucket_id` predicate) instead of a table | Same storage access-control docs (storage authorization IS RLS on `storage.objects`); OWASP A01:2021 → `grounded` | high (write/delete) / medium (read) |

### 2.5 Database functions (`supabase/database-functions/` → `supabase:database-functions`)

| id | Detects | Buyer framing | Tier | Grounding | Sev |
|---|---|---|---|---|---|
| `SUPABASE-FUNC-SEARCH-PATH-1` | `CREATE FUNCTION … SECURITY DEFINER` without `SET search_path` — attacker-created objects in an earlier schema hijack name resolution inside a privileged function | "One of your database functions runs with elevated power but resolves names loosely — a known Postgres attack lets someone swap in their own code underneath it." | **architectural** (multi-line statement scan: DEFINER present, `SET search_path` absent — deterministic once statements are split; near-zero FP on the DEFINER subset → **floor-port candidate** after the statement-splitter exists) | PostgreSQL docs, "Writing SECURITY DEFINER Functions Safely": <https://www.postgresql.org/docs/current/sql-createfunction.html#SQL-CREATEFUNCTION-SECURITY>; splinter `function_search_path_mutable` (lint 0011) → `grounded` | high |
| `SUPABASE-FUNC-DEFINER-MINIMAL-1` | *(prune)* "Prefer SECURITY INVOKER unless DEFINER is required" — pure judgment, low report value next to the search_path rule. Fold its remediation advice into `SUPABASE-FUNC-SEARCH-PATH-1`'s option text. | — | — | — | — |

### 2.6 Exposure (`supabase/exposure/` → `supabase:exposure`)

| id | Detects | Buyer framing | Tier | Grounding | Sev |
|---|---|---|---|---|---|
| `SUPABASE-EXPOSURE-MATVIEW-1` | Materialized view in an API-exposed schema — materialized views do not support RLS, so exposure is all-or-nothing | "Your `revenue_rollup` snapshot table can't be row-secured at all, and it's reachable from the public API." | **structured** (`CREATE MATERIALIZED VIEW` in `public`) | splinter `materialized_view_in_api` (lint 0016) → `grounded` | high |
| `SUPABASE-EXPOSURE-SCHEMAS-1` | Extra schemas exposed through PostgREST — `[api] schemas = […]` in `supabase/config.toml` beyond `public`/`graphql_public`, widening the API surface to tables nobody RLS-audited | "Your API quietly serves a second set of tables that were never reviewed for access rules." | **structured** (config.toml regex; also feeds §4's exposed-schema set so the RLS rules scan the right schemas instead of hardcoding `public`) | Custom-schemas docs: <https://supabase.com/docs/guides/api/using-custom-schemas>; CLI config reference → `grounded` | medium |
| Public Postgres port / direct DB exposure | *(out of scope for the static scan)* Not detectable from a repo; it is a live-infrastructure property. Mention in the audit report's "what a repo scan cannot see" section (§4), do not author a rule that pretends to detect it. | — | — | — | — |

---

## 3. Floor vs advisory summary

**Floor (near-zero-FP, always-block) — `AUDIT_RULES` additions / extensions:**
1. `SEC-NO-VENDOR-TOKEN-1` match-set extension (`sb_secret_`, service_role JWT decode, Supabase conn-string) — extends an existing floor rule, no new id.
2. `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` (client-bundle env prefix + SERVICE_ROLE).
3. `SUPABASE-RLS-USER-METADATA-1` (user_metadata inside CREATE POLICY).
4. `SUPABASE-FUNC-SEARCH-PATH-1` — floor-*candidate* only once statement-granular SQL scanning exists; ships advisory-architectural until then.

Everything else is advisory (structured/architectural/prose) with `needs_review` on the acknowledged-FP rules (`PERMISSIVE-TRUE` reads, `PUBLIC-BUCKET`, `GETSESSION-SERVER` ambiguous paths). Note for the author: `AUDIT_RULES` is documented as *content* rules (pure functions over file content); the RLS-replay rules are corpus rules + scan passes, NOT `AUDIT_RULES` entries, regardless of tier.

---

## 4. RLS misconfiguration — detection precision (the load-bearing section)

**What static analysis over the repo CAN determine, mechanically:**

- **Per-statement facts** (regex-grade once SQL is split into statements): `ENABLE/DISABLE ROW LEVEL SECURITY`, `CREATE POLICY` (+ `TO` roles, `USING`/`WITH CHECK` text), `CREATE VIEW` options, `SECURITY DEFINER`, storage-bucket DDL/DML, `user_metadata` in predicates.
- **Timeline-replay facts** (the architectural pass this design requires): the *final* RLS state per table. Migrations are ordered (`supabase/migrations/<timestamp>_*.sql`); a mini schema-state interpreter folds `CREATE TABLE` / `ALTER … ENABLE|DISABLE ROW LEVEL SECURITY` / `DROP TABLE` / `ALTER … RENAME` / `CREATE|DROP POLICY` across the timeline, per schema. This is what makes "your `profiles` table has no RLS" a *statement about the repo's end state* rather than a per-file grep guess. Per-file regex alone produces both FPs (RLS enabled in a later migration) and FNs (enabled then disabled) — do not ship the RLS-state rules at regex tier.
- **Exposure scoping:** which schemas are API-exposed comes from `SUPABASE-EXPOSURE-SCHEMAS-1`'s config parse (default `public`). A table in a non-exposed schema without RLS is a demoted finding (defense-in-depth note), not a critical.
- **Declarative-schema shortcut:** if the repo contains `supabase db dump`-style schema files or declarative `supabase/schemas/*.sql`, treat them as an authoritative end-state snapshot — cheaper and more accurate than replay for what they cover.

**What static analysis can flag but NOT judge (→ structured-with-`needs_review` or prose):**

- Policy predicate *correctness* beyond `USING (true)`: whether `auth.uid() = user_id` scopes the right column, tenant-isolation completeness, etc. Parse and extract the predicates mechanically; route the judgment to the prose tier with the full policy set per table as evidence.
- **Permissive-OR semantics:** multiple `PERMISSIVE` policies on the same table/action combine with OR — one over-broad policy defeats every careful one. Detecting "multiple permissive policies exist" is mechanical (cf. splinter `multiple_permissive_policies`, lint 0006 — a perf lint, but the OR-semantics security note rides on the same detection); judging whether the union is over-broad is prose.
- Intentional-publicness: read-only reference tables, public avatar buckets. Never auto-critical; name the object and ask.

**What ONLY the live database can answer (be honest in the report):**

- Changes made in the Supabase dashboard that never landed in a migration — **the dominant FP/FN source in vibe-coded repos** (Lovable/Bolt users click the dashboard constantly). Repo says "no RLS" while production has it (FP), or a migration enables RLS that someone later disabled in the dashboard (FN — the dangerous direction).
- Squashed/partial migration history (baseline dumps), actual role grants, actual bucket contents, network exposure of the Postgres port.

**Consequences for the product:**
1. **Phrase repo-only findings honestly:** "no evidence of RLS in the repository for `profiles`" with an explicit "confirm against production" step — never assert live state from the repo alone.
2. **The paid-audit closer:** offer a live verification step — run Splinter (`supabase db lint`, or its published SQL from <https://github.com/supabase/splinter>) against the client's database and reconcile with the repo scan. Repo-vs-live *drift* ("your code says X, your database says Y") is itself a premium finding class no repo-only scanner sells. This is a product note for the audit-report flow, not a corpus rule.

---

## 5. Authoring checklist for the Sonnet pass

1. Create `crates/rules/principles/supabase/{rls,auth,secrets,storage,database-functions,exposure}/` and author the 14 rules marked with ids above (skip the two explicit prunes; the vendor-token item is an edit to `universal/sec-no-vendor-token-1.toml`'s match-set documentation, not a new file).
2. Verify every cited URL resolves and says what this doc claims before setting `verification = "grounded"`; confirm each Splinter lint's exact name/number at the database-advisors page. Anything that fails verification ships `policy` (if it is a defensible Camerata stance) or `draft` — never a fabricated source.
3. Every rule: `[decision]` with question/default/why + ≥2 `[[option]]` blocks (default + rejected alternative with rejection rationale), in the existing SEC-* voice. The `why` must carry the concrete match-set / detection description like SEC-NO-UNSAFE-DESERIALIZATION-1 does, including the documented FP carve-outs (anon-key-is-public, intentional public buckets, dashboard-drift caveat).
4. `opt_in_only = true` on `SUPABASE-RLS-INITPLAN-1` only.
5. Run the corpus loader tests (`cargo test -p camerata-rules`) — the corpus must still load; folder-derived domains must match any in-file `domain` fields.
6. Do NOT wire scan passes / `AUDIT_RULES` in the same PR — floor wiring for the §3 candidates is a separate, deliberately reviewed change (floor rules are always-block; per the floor-first layering rule they graduate only after the near-zero-FP bar is demonstrated).
