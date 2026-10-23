# Secret redaction and the export release gate: the build-ready design (2026-10-23)

Written 2026-10-23 against branch `feat/audit-report-export` tip `35b4855`, after reading the
work order (`_workspace/brownfield-audits/WORK_ORDER_SECRET_REDACTION.md`), the owner's binding
decisions (`docs/OVERNIGHT_DECISIONS.md`, DECISIONS MADE, 2026-10-22), `crates/gateway/src/lib.rs`
(the SEC arms), `crates/orchestrator/src/secrets.rs` (the chat scrubber), `crates/llm/src/
credentials.rs`, every production `Finding` constructor and text-mutation site in `crates/server`,
the three exporters, `templates/audit_report.typ`, the scan cache, the prompt builders, gates 1
to 7, and the latest sweep exports under `~/Documents/Repos/Camerata tests/_runs/sweep5b-2026-10-16/`
(faktura, hearth, supabase-portal-fixture; the 2026-10-18 hearth rerun agrees with sweep5b).
Build-ready: every "decide" below is decided. The governing law, from the work order: **a value
that matches a secret shape never appears in full in anything the client receives, and never
reaches a model when we can avoid it.** Redaction happens once, where a finding is built; the
release gate at export time proves it, over the exact bytes being shipped, and refuses the export
otherwise. Failure messages never print the secret.

Secret values in this document are written in their redacted form only.

## 0. What the investigation found (summary)

Three independent secret detectors exist today and disagree with each other:

| Where | What it knows | Gaps |
|---|---|---|
| `camerata_gateway` SEC arms (`crates/gateway/src/lib.rs:938-993` hardcoded-secrets regex, `1140-1182` URL query, `1213-1219` PEM header, `1248-1268` vendor tokens incl. `sb_secret_`, `1296-1323` secret file names) | GitHub, Slack, AWS, OpenAI/Stripe `sk-`, Google, PEM header, generic key/secret/token literal, URL query params, `sb_secret_` | No Supabase service_role JWT (the corpus rule `sec-no-vendor-token-1.toml:49-57` PROMISES a decode-at-match-time arm; it is not built), no Postgres connection string with inline password (promised at `:58-62`), no `whsec_`, no `rk_live_`, no Bearer, no Azure. The `sb_secret_` body class allows `-`, so the prose "sb_secret_-prefixed" matches. |
| `camerata_orchestrator_core::secrets` (`crates/orchestrator/src/secrets.rs`) | Stripe `sk_live_/sk_test_/rk_live_`, PEM block with body, AWS, GitHub, Azure storage, OpenAI, Bearer | Only wired into `submit_feedback` (`crates/server/src/lib.rs:3031-3034`). Replaces with `{{SECRET:kind_n}}` placeholders, not the owner's prefix/tail/length form. |
| `camerata_llm::credentials::mask_value` (`crates/llm/src/credentials.rs:70-73`) | Camerata's OWN API keys: first 4 chars + `••••` | Not a detector; a display mask for the settings UI. Unrelated to client secrets, stays as is. |

The scan's deterministic floor reuses the gateway regexes through
`camerata_gateway::content_match_lines` (`crates/server/src/onboard/audit.rs:229`) and then copies
the **raw matched line** into `Finding.snippet` (`audit.rs:278-282`). That snippet then flows
unchanged into the AI calibration prompt, the fix-specific prompt (which also sends eight raw lines
of surrounding code), the model-written `fix_specific`, `findings.json`, the xlsx, the csv/json
datasets, the PDF data, and the scan cache on disk. `crates/checks` detects no secret by value (its
`secret`/`token` vocabulary is the authorization-predicate heuristic in
`supabase/privileged_no_authz_checker.rs:134`); nothing to migrate there.

The one existing export-side text choke point, `report_export::sanitize_finding_text`
(`crates/server/src/report_export.rs:207-220`, zero-width stripping), covers `path`, `snippet`,
`detail` and `captures` only. It misses `fix_specific`, `also_locations[].snippet` and
`calibration_rationale`: exactly where the hearth Stripe keys and every `fix_specific` leak sit.

The existing client-text crawler in the `export_invariants_gate` test module
(`report_export.rs`, `all_client_text_fields`) reads a NAMED list of fields and also misses
`also_locations`. The release gate must walk every string, never a field list.

## 1. What already EXISTS (reuse, do not rebuild)

- **One zip assembler.** `camerata_server::build_product_zip` (`crates/server/src/lib.rs:16865`)
  is already the single function all three real exporters call: the HTTP handler `export_product`
  (`lib.rs:16379`), the CLI `build_product_export_bytes` (`crates/cli/src/inspect_cmd.rs:387`),
  and the sample generator (`crates/server/tests/generate_sample_report.rs:536`). The manifest is
  built last over final bytes (`build_manifest`, `lib.rs:16809`). The gate hooks here.
- **The PDF's data is one JSON file.** `compile_pdf` (`report_export.rs:4940-4956`) writes
  `serde_json::to_string(&AuditReportJson)` to `data.json`; the template's only data source is
  `#let d = json("data.json")` (`crates/server/templates/audit_report.typ:1`; no other `json(`,
  `read(` or `sys.inputs`). Every client-visible dynamic string in the PDF is therefore in that
  serialized struct. The template's own static prose carries no client data.
- **PDF text extraction is already a dependency.** `pdf-extract = "0.12"` is a dev-dependency of
  `camerata-server` (`Cargo.toml`), used as `pdf_extract::extract_text_from_mem(&pdf)` in
  `report_export.rs:8155, 10130, 10165, 10191`. It resolves `lopdf 0.42.0`. Typst 0.15.0 is on
  this machine at `/opt/homebrew/bin/typst`.
- **Workbook string readback.** `workbook_shared_strings_xml` / `workbook_shared_string_texts`
  (test helpers in `report_export.rs`'s `export_invariants_gate` module) read every string of a
  built `.xlsx` out of `xl/sharedStrings.xml`; `rust_xlsxwriter` writes every string cell as one
  plain `<t>` run, so this is exhaustive for our workbook. Promote to production (section 7).
- **Gates 1 to 7** (`docs/design/2026-10-20_machine-readable-exports.md` §Testing). Their
  comparison logic lives in test files: `manifest_failures` (`crates/server/tests/
  product_export_manifest_gates.rs:173-196`, gate 6), `parse_readme_provenance` + the gate 7 tests
  (`:268-430`), `assert_csv_matches_json` and `assert_every_dataset_agrees` (`crates/server/tests/
  machine_export_dataset_parity_e2e.rs:525-571, 1260-1303`, gates 2 and 4), and
  `req_d1_pdf_findings_json_and_xlsx_contain_exactly_the_same_rows` (`report_export.rs`
  `export_invariants_gate`). Section 7 moves each comparison into a production function the test
  then calls.
- **Twin precedents.** `is_test_or_fixture_path` (`gateway/src/lib.rs:566`), the `.env` template
  suffix list in `arm_sec_no_secret_file_1` (`:1300-1306`: example, sample, template, dist,
  defaults, tpl), and the de-literalized Stripe fixture builder in `orchestrator/src/secrets.rs`
  tests (`fn stripe(prefix)`), which exists because GitHub push protection matches `sk_`/`rk_`
  shapes in source. The new crate's tests and its published-key allowlist follow that rule: no
  secret-shaped literal in source (section 2.4).
- **Fingerprint + keys.** `suppression::fingerprint(rule_id, snippet)` (`crates/server/src/
  suppression.rs:156-159`), `report_export::finding_key` (`report_export.rs:239-244`), and the
  UI's `ui-core/src/triage.rs:1016` all hash or concatenate `snippet`. Section 6.

## 2. Secret shape catalogue

Every shape the shared detector knows, with its exact pattern (Rust `regex` syntax; `(?x)`
verbose mode where shown), what the redactor keeps, and the safe twins that stay readable.
Shapes are matched left to right over the whole text; when two matches overlap, the longer wins,
then the earlier. A match never spans a newline unless the shape says so (PEM only), so line
counts are preserved and line numbers stay valid after redaction.

### 2.1 The redaction form

For a match of value `v` (length `N` in chars) with a recognisable prefix `P` (the vendor prefix
the shape names) and opaque body `B = v[P.len()..]`:

- **Long body** (`B.chars().count() > 12`): `{P}…{last4} ({N} chars)`, where `last4` is the last
  four characters of `v`. Example from the work order: `sb_secret_…XyQ7 (52 chars)`.
- **Short body** (`B.chars().count() <= 12`): `{P}… ({N} chars)`. No tail (the owner's short-secret
  rule: show only the length). A tail on a 10-char body would reveal 40% of it.
- **Prefix-less shapes** (generic named literal, connection-string password, URL query value,
  Bearer token): `P` is empty; `…{last4} ({N} chars)` when `N > 16`, else `… ({N} chars)`. Only
  the VALUE is replaced; surrounding syntax (quotes, `Bearer `, `user:` and `@host`, `param=`)
  stays so the reader can see what kind of thing was there.
- **JWT**: `eyJ…{last4} ({N} chars, role {role})` where `role` is the decoded `role` claim or
  `unknown`. The header segment is not shown (it is public, but showing it invites "that is not
  secret" arguments; the role is what identifies the key to rotate).
- **PEM block**: the header and footer lines stay readable; every body line between them is
  replaced by ONE line `[{kind} private key material: {n} lines redacted]`. A header alone (a
  one-line snippet) carries no key material and is left as is.

The ellipsis is U+2026 (one char), as in the work order. `strip_zero_width` does not touch it.
The redaction output is itself a safe twin of every shape (the `…` breaks every body class and the
tail is 4 chars), so `redact(redact(x)) == redact(x)`; a test enforces this for every shape.

### 2.2 The shapes

| # | Shape (`SecretShape`) | Pattern | Prefix kept | Notes |
|---|---|---|---|---|
| S1 | `SupabaseSecretKey` | `sb_secret_[A-Za-z0-9_]{10,}` | `sb_secret_` | Body class excludes `-` (today's gateway class includes it and matches the prose `sb_secret_-prefixed`, seen in the portal export's `findings[5].detail`). Observed real bodies: 41 and 32 chars. `sb_publishable_` is never matched (public by design; `sec-no-vendor-token-1.toml:55-57`). |
| S2 | `SupabaseServiceRoleJwt` | `eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}` then DECODE the middle segment (base64url, no padding, `base64::engine::general_purpose::URL_SAFE_NO_PAD`) as JSON and read `role` | `eyJ` | `role == "service_role"`: secret. `role` in {`anon`, `authenticated`}: twin T4 (public). Any other role or no `role` claim, or an undecodable payload: shape S2b `OpaqueJwt` (redacted everywhere, NOT a finding; see 3.2 on rule mapping). This is the arm the corpus rule describes and the gateway never built. |
| S3 | `StripeLiveSecretKey` | `(?:sk_live_|rk_live_)[A-Za-z0-9]{16,}` | `sk_live_` / `rk_live_` | Observed: 77 chars (`sk_live_51…`). |
| S4 | `StripeTestSecretKey` | `(?:sk_test_|rk_test_)[A-Za-z0-9]{16,}` | `sk_test_` / `rk_test_` | Redacted unless twin T3 (published) or T1. Owner question Q1. `pk_live_`/`pk_test_` (publishable) are never matched. |
| S5 | `StripeWebhookSecret` | `whsec_[A-Za-z0-9]{16,}` | `whsec_` | Observed in hearth (38 chars). New; no detector has it today. |
| S6 | `AwsAccessKeyId` | `\b(?:AKIA|ASIA)[0-9A-Z]{16}\b` | `AKIA`/`ASIA` | AWS's documented example ids are twins T3. The paired 40-char secret access key is caught by S13 (it always sits next to a `secret`-named identifier). |
| S7 | `GitHubToken` | `\b(?:gh[pousr]_[A-Za-z0-9]{20,}\|github_pat_[A-Za-z0-9_]{22,})` | `ghp_` etc. / `github_pat_` | Unifies the gateway's two thresholds (`{10,}` in the hardcoded arm, `{36}`/`{82}` exact in the vendor arm) to one. |
| S8 | `SlackToken` | `\bxox[abceprs]-[A-Za-z0-9-]{10,}` | `xox?-` | As the gateway's hardcoded arm (the vendor arm's narrower `[baprs]` class is dropped). |
| S9 | `GoogleApiKey` | `\bAIza[0-9A-Za-z_-]{35}\b` | `AIza` | |
| S10 | `AnthropicKey` | `\bsk-ant-[A-Za-z0-9_-]{20,}` | `sk-ant-` | Tried before S11 so `sk-ant-` is not reported twice. |
| S11 | `OpenAiKey` | `\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}` | `sk-` / `sk-proj-` | Covers the gateway's `sk-[A-Za-z0-9]{20,}` and the newer project keys. |
| S12 | `PemPrivateKey` | `-----BEGIN (?:RSA \|EC \|DSA \|OPENSSH \|PGP \|ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----` optionally followed within 8000 chars by the matching `-----END … PRIVATE KEY(?: BLOCK)?-----` (the scrubber's pattern, `orchestrator/src/secrets.rs`) | header/footer kept | Multi-line by design; the only shape allowed to span newlines. Redaction keeps the line COUNT equal (body lines collapse to one line, padded with empty lines) so line numbers downstream stay valid. |
| S13 | `NamedSecretLiteral` | `(?i:[A-Za-z0-9_]*(?:key\|secret\|token\|password\|passwd\|pwd\|credential)[A-Za-z0-9_]*)[^\n]{0,40}?(?:"([A-Za-z0-9+/_]{24,}=*)"\|'([A-Za-z0-9+/_]{24,}=*)')` | none | The gateway's heuristic (`lib.rs:969-990`) with `/` admitted into the literal class (base64 secrets contain `/`; the gateway excluded it to avoid paths, but a 24+ char contiguous run with no `.` or `-` is not a path). Only the quoted literal (capture group) is redacted. Twins T1, T2, T5. |
| S14 | `BearerToken` | `(?i)\bbearer\s+([A-Za-z0-9_.=-]{20,})` | `Bearer ` kept | Only the token is redacted. If the token itself matches S2/S7/etc., that shape wins (overlap rule). |
| S15 | `ConnectionStringPassword` | `\b[a-z][a-z0-9+.-]{1,30}://[^\s/:@"']+:([^\s@/"']{4,})@[^\s"']+` | scheme, user and host kept | Only the password group is redacted: `postgres://postgres:…9Qz (24 chars)@db.ref.supabase.co:5432/postgres`. Covers the corpus rule's promised `postgres(ql)://…@db.<ref>.supabase.co` arm and every other DSN. Twins T5 (`[YOUR-PASSWORD]`, `${…}`, `{{…}}`, `<…>`, `%s`, the literal word `password`). |
| S16 | `AzureStorageConnectionString` | `DefaultEndpointsProtocol=https?;[^\n]{0,300}?AccountKey=([A-Za-z0-9+/=]{20,})` | `AccountKey=` kept | From the scrubber. Only the key group is redacted. |
| S17 | `SecretInUrlQuery` | `[?&](?i:access_token\|refresh_token\|auth_token\|client_secret\|private_key\|api_?key\|passwd\|password\|pwd\|secret\|signature\|sig\|token\|key)=([^\s&"'<>{}$%]{8,})` | `param=` kept | The gateway's `ARCH-NO-SECRETS-IN-URL-1` param list (`lib.rs:1158-1161`). Only the value group is redacted. A value that starts with `{`, `$`, `<` or `%` is a template (twin T5) and the negated class already excludes it. |

Shapes S1 to S12 are "vendor shaped" (a fixed prefix; near-zero false positives). S13 to S17 are
"context shaped" (a name or syntax says this is a credential). The distinction matters for
findings (3.2) but not for redaction: every shape is redacted.

### 2.3 Safe twins (left readable, never counted as a leak)

| Twin | Rule | Why |
|---|---|---|
| T1 `PlaceholderWord` | The opaque body (for S1 to S11: the text after the vendor prefix; for S13/S15/S16/S17: the captured value) contains, case-insensitively, one of: `your`, `example`, `placeholder`, `replace`, `changeme`, `change_me`, `change-me`, `dummy`, `fake`, `sample`, `redacted`, `xxxx`, `todo`, `insert`, `_here`, `-here`, `do_not`, `donot`, `mock`. | `sb_secret_your_key_here` and the portal fixture's values (an uppercase body containing `DO_NOT` and `FAKE`; a `.env.example` value ending in `here`). The word list is a `const` slice; adding to it is a one-line change with a twin test. |
| T2 `EnvNameLiteral` | S13 only: the quoted literal is an identifier, `^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$` (uppercase, at least one underscore, no lowercase). | The faktura PDF page 40 hit: `process.env['SUPABASE_SERVICE_ROLE_KEY']`-style code quotes the NAME of the key, 25 chars, and today's heuristic calls it a secret. A real secret body is never an all-caps snake-case identifier. |
| T3 `PublishedExampleKey` | The matched value's SHA-256 (hex) is in `PUBLISHED_EXAMPLES`, a const list of digests with a doc comment per entry naming the vendor page it comes from. Initial entries: Stripe's documented test secret key (`sk_test_4e…p7dc`, 32 chars; it is in hearth's `tests/fixtures/session.json:2`), Stripe's documented test publishable key (never matched anyway), AWS's two documented example access key ids (`AKIAIOSFODNN7EXAMPLE`-style; these contain `EXAMPLE` and are already T1), GitHub's documented `ghp_16C7…8B4a` token-format example. | "Published test keys stay readable" (work order §4). Stored as digests, not literals, so the repo never contains a secret-shaped literal (push protection; the same reason `secrets.rs` builds its Stripe fixture from fragments). The builder verifies each digest against the vendor page before adding it. |
| T4 `PublicKeyRole` | S2 whose decoded `role` is `anon` or `authenticated`. | The Supabase anon/publishable key is designed to be public (`sec-no-vendor-token-1.toml:55-57`, `supabase-key-service-role-client-1.toml`). Its presence is an RLS question, not a leak. |
| T5 `TemplateToken` | S13/S15/S16/S17 value is wrapped in or starts with `${`, `{{`, `<`, `[`, `%`, or equals (case-insensitive) `password`, `secret`, `token`, `changeme`. | `[YOUR-PASSWORD]` from Supabase's own docs; `{{base}}?token={{token}}`; `<redacted>`. |
| T6 `RedactionMarker` | The text already has the `…` form (idempotence): a body or value containing U+2026 never matches any shape because `…` is outside every body class. No explicit rule needed; a test proves it. | A redacted export re-read by the gate must pass. |
| (not a twin) `.env.example`-style FILE NAMES | A template file name does NOT make its contents readable. Redaction is content-only. | hearth's `.env.local.example:5` carries a 77-char `sk_live_51…` key (real-looking, not a placeholder). The gateway's `SEC-NO-SECRET-FILE-1` template suffix list keeps governing which FILE is a finding; it never governs redaction. |
| (not a twin) `sk_test_` by prefix alone | A real-looking test-mode secret key is redacted (S4). Only T1/T3 make it readable. | Default pending owner question Q1. |

Twins are still DETECTED (`detect_all` returns them with `twin: Some(..)`); they are excluded by
`detect_secrets` (the redactor and the gate) and included by the floor's finding detection where
the rule wants them (3.2). This keeps "is it a finding" and "is it readable" as two separate
policies over one detector.

### 2.4 Fixtures in tests: no secret-shaped literal in source

Every test that needs a secret value builds it at runtime from fragments or by repetition
(`format!("sb_secret_{}", "A".repeat(41))`, `["sk_live", "51XYZ…"].join("_")`), as
`orchestrator/src/secrets.rs`'s `fn stripe(prefix)` already does. T3 digests are the only
"real published value" data in the repo and they are hashes. A corpus-style gate test in the new
crate (`no_secret_shaped_literal_in_this_crates_source`) runs the detector over the crate's own
`src/` and fails on any raw match that is not a twin.

## 3. The shared detector and redactor

### 3.1 Where it lives: a new leaf crate `crates/secrets`, package `camerata-secrets`

Decision: a new workspace member, `crates/secrets` (`camerata-secrets`), with NO camerata
dependencies and exactly three external ones: `regex`, `base64 = "0.22"` (the JWT payload
decode; the workspace already locks 0.22.1), `serde_json` (the claims object; workspace dep).
Optional `serde` derive on the public types.

Why a leaf crate and not an existing one:

- Callers: `camerata-gateway` (the SEC arms), `camerata-server` (floor, finding redaction, prompt
  redaction, release gate), `camerata-orchestrator-core` (`secrets::scrub`), `camerata-checks`
  (clone judge excerpts, 5.4), `camerata` CLI (acceptance subcommand), `camerata-llm` (nothing
  today; stays free to depend later). `camerata-checks` must never depend on `camerata-server`
  (`Cargo.toml` workspace comment on `camerata-check`), and `camerata-llm` depends only on
  `api-types`. Only a leaf satisfies all of them with no cycle; this is the `api-types` /
  `liveness` / `test-env` precedent.
- `camerata-core` was considered: `checks` and `gateway` already depend on it, but `llm` and
  `checks` would gain `regex` + `base64` through a crate whose remit is shared primitives, and
  `core`'s dev-deps already pull `gateway`/`checks`/`agent`, so growing it grows every test build.
  Rejected.
- `camerata-gateway` was considered (it owns the regexes today): `checks` and `llm` cannot depend
  on it (gateway depends on `rmcp`, `agent`, `rules`), and `camerata-check` (the client CI binary)
  must stay server- and gateway-free. Rejected.

Add the member to `Cargo.toml` under `crates/api-types` with a one-line comment ("leaf crate:
the one secret-shape detector + redactor shared by the gateway, the scan floor, the chat scrubber
and the export release gate; no camerata deps"). Update `docs/OVERVIEW.md`'s crate map
(CONTRACTS / CORE row) and `docs/TECHNICAL.md` in the build (section 9).

### 3.2 The API (exact)

```rust
// crates/secrets/src/lib.rs
#![forbid(unsafe_code)]

/// Every secret shape the detector knows. Order is the match-priority order used to break ties
/// between equal-length overlapping matches (lower variant first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SecretShape {
    SupabaseSecretKey,            // S1
    SupabaseServiceRoleJwt,       // S2 (role == service_role)
    OpaqueJwt,                    // S2b (any other / no role)
    StripeLiveSecretKey,          // S3
    StripeTestSecretKey,          // S4
    StripeWebhookSecret,          // S5
    AwsAccessKeyId,               // S6
    GitHubToken,                  // S7
    SlackToken,                   // S8
    GoogleApiKey,                 // S9
    AnthropicKey,                 // S10
    OpenAiKey,                    // S11
    PemPrivateKey,                // S12
    NamedSecretLiteral,           // S13
    BearerToken,                  // S14
    ConnectionStringPassword,     // S15
    AzureStorageConnectionString, // S16
    SecretInUrlQuery,             // S17
}

impl SecretShape {
    pub const ALL: &'static [SecretShape];
    /// Stable snake_case label for messages and JSON ("supabase_secret_key", …).
    pub fn label(self) -> &'static str;
    /// Client-facing noun for the gate's message ("Supabase secret key", "Stripe live key", …).
    pub fn display_name(self) -> &'static str;
    /// The fixed prefix kept in the redacted form, if the shape has one (`Some("sb_secret_")`).
    pub fn vendor_prefix(self) -> Option<&'static str>;
    /// True for S1..S12 (a fixed vendor prefix; near-zero FP). False for S13..S17.
    pub fn is_vendor_shaped(self) -> bool;
}

/// Why a match is NOT a secret (left readable, never a gate failure).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Twin {
    PlaceholderWord(&'static str), // which word matched (T1)
    EnvNameLiteral,                // T2
    PublishedExampleKey,           // T3
    PublicKeyRole(String),         // T4: the decoded role ("anon" / "authenticated")
    TemplateToken,                 // T5
}

/// One match. `start..end` is the WHOLE matched span in `text`; `value` is the sub-span that the
/// redactor replaces (the whole span for vendor shapes; the capture group for S13..S17 and the
/// PEM body for S12). Byte offsets, always on char boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    pub start: usize,
    pub end: usize,
    pub value: std::ops::Range<usize>,
    pub shape: SecretShape,
    pub twin: Option<Twin>,
    /// For S2/S2b: the decoded `role` claim, if any.
    pub jwt_role: Option<String>,
}

impl SecretMatch {
    /// The redacted replacement for this match's `value` (section 2.1). Never contains the value.
    pub fn redacted_form(&self, text: &str) -> String;
    pub fn is_secret(&self) -> bool { self.twin.is_none() }
}

/// Every match, twins INCLUDED, left to right, non-overlapping (longest wins, then earliest, then
/// `SecretShape` order). Deterministic. Linear in `text.len()` per shape.
pub fn detect_all(text: &str) -> Vec<SecretMatch>;

/// `detect_all` minus twins. This is what the redactor and the release gate use.
pub fn detect_secrets(text: &str) -> Vec<SecretMatch>;

/// Fast path for the gate: the first non-twin match, if any.
pub fn first_secret(text: &str) -> Option<SecretMatch>;

/// `detect_all` restricted to `shapes`, twins included. The scan floor and the gateway arms use
/// this with `shapes_for_rule`.
pub fn detect_shapes(text: &str, shapes: &[SecretShape]) -> Vec<SecretMatch>;

/// 1-based line numbers (sorted, deduped) where any of `shapes` matches; replaces
/// `camerata_gateway::content_match_lines` for the secret rules. `twins_count` says whether a
/// twin match still counts as a line (the floor passes `true` for the vendor rule: a committed
/// placeholder key in `.env.production` is still a row; see 3.3).
pub fn match_lines(text: &str, shapes: &[SecretShape], twins_count: bool) -> Vec<usize>;

/// The shapes a corpus rule id governs. Unknown id → empty slice.
///   "SEC-NO-VENDOR-TOKEN-1"      → S1 S2 S3 S4 S5 S6 S7 S8 S9 S10 S11 S15(supabase hosts only: see
///                                   `is_supabase_dsn`) S16
///   "SEC-NO-HARDCODED-SECRETS-1" → S6 S7 S8 S9 S11 S12 S13 (the gateway's current set) + S2
///   "SEC-NO-PRIVATE-KEY-1"       → S12
///   "ARCH-NO-SECRETS-IN-URL-1"   → S17
///   (S2b OpaqueJwt and S14 BearerToken belong to NO rule: redaction-only shapes.)
pub fn shapes_for_rule(rule_id: &str) -> &'static [SecretShape];

/// Result of `redact`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// One entry per replaced match, in order: shape, redacted form, original byte span in the
    /// INPUT. Never the value. Lets a caller log "redacted 2 values" or build a gate message.
    pub redactions: Vec<Redaction>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction { pub shape: SecretShape, pub form: String, pub span: std::ops::Range<usize> }

/// Replace every non-twin match with its redacted form. Preserves the number of `\n` in `text`
/// (S12 pads). Idempotent. Returns `Cow::Borrowed` semantics via `redactions.is_empty()`.
pub fn redact(text: &str) -> Redacted;

/// `redact(text).text`, allocation-free when nothing matched (returns `Cow<str>`).
pub fn redact_str(text: &str) -> std::borrow::Cow<'_, str>;

/// Redact every string value in a `serde_json::Value` tree in place; returns the redaction count.
/// (The seal-time sweep over a `ScanReport` and the gate's JSON walk both use this.)
pub fn redact_json_values(value: &mut serde_json::Value) -> usize;

/// Walk every string in a JSON tree and report non-twin matches with a JSON-pointer-style path
/// (`findings[0].also_locations[1].snippet`). The release gate's JSON check.
pub fn find_secrets_in_json(value: &serde_json::Value) -> Vec<(String, SecretMatch)>;

pub mod published { /* PUBLISHED_EXAMPLES: &[(&str /*sha256 hex*/, &str /*vendor doc url*/)] */ }
```

Implementation notes the builder must follow:

- One `OnceLock<Regex>` per shape (the gateway's style), compiled from `const` pattern strings
  that the catalogue table above quotes; a test asserts each const compiles and matches its
  documented example built from fragments.
- `detect_all` runs every shape's `find_iter`, collects candidate spans, classifies twins, then
  resolves overlaps: sort by `(start, Reverse(len), shape order)`, sweep, drop any candidate that
  starts before the previous kept candidate's `end`. S14's token overlapping an S2/S7 match is
  therefore reported once, as the specific shape (the scrubber's existing behaviour).
- JWT decode: split on `.`, take segment 1, decode URL_SAFE_NO_PAD (fall back to STANDARD_NO_PAD),
  `serde_json::from_slice::<Value>`, read `["role"].as_str()`. Any failure → `OpaqueJwt`,
  `jwt_role: None`. Cap decode work at 4 KiB payload.
- Twin classification order: T3 (digest) first, then T4 (role), then T2, then T1, then T5. A match
  with any twin reason is a twin.
- `redact` builds the output with one pass over `redactions` (no nested replacement), so a
  redacted form can never be re-matched within the same call.

### 3.3 Migrating the existing callers

**`camerata-gateway`** (`crates/gateway/src/lib.rs`): delete `sec_secrets_regex`,
`arch_url_secret_regex`, `sec_private_key_regex`, `sec_vendor_token_regex` and their `OnceLock`s.
The four content arms become:

```rust
fn arm_sec_no_vendor_token_1(path, content) -> Result<(), String> {
    let Some(m) = camerata_secrets::detect_shapes(content, camerata_secrets::shapes_for_rule("SEC-NO-VENDOR-TOKEN-1")).into_iter().next() else { return Ok(()) };
    // test-scope policy exactly as today (is_test_or_fixture_path / is_in_test_scope / test_scope_policy)
    Err(format!("SEC-NO-VENDOR-TOKEN-1: content contains a vendor credential token ({} {}); tokens must live in a secrets manager …", m.shape.display_name(), m.redacted_form(content)))
}
```

The denial message's `preview` (first 6 or 8 raw chars today, `lib.rs:1010-1016`, `1281-1287`)
is replaced by the redacted form; the vendor prefix is what those previews were showing anyway.
`content_match_lines` (`lib.rs:418`) keeps its signature; its four secret arms call
`camerata_secrets::match_lines(content, shapes_for_rule(id), /*twins_count*/ true)`. The gateway
arms themselves (`gated_write` denials) also pass `true`: writing a placeholder `sb_secret_…` into
`.env.production` is still something the gate should bounce, and the test-scope policy is where
test fixtures get their waiver, as today. Existing gateway tests keep passing except where the
unified thresholds (S7 `{20,}`, S1 `{10,}`, no `-` in the S1 body) change a fixture; the builder
adjusts those fixtures and lists each in the commit message. New arm behaviour gained for free and
tested in the gateway: S2 (service_role JWT denied; anon JWT allowed), S5, S15, S16.

**The scan floor** (`crates/server/src/onboard/audit.rs:229`): unchanged call shape
(`content_match_lines`), so no edit beyond the snippet redaction in section 4.1. `AUDIT_RULES`
(`onboard.rs:74-85`) is unchanged.

**`camerata_orchestrator_core::secrets`**: keep the public API (`detect_secrets`, `scrub`,
`SecretKind`, `{{SECRET:kind_n}}` placeholders, extracted values for the vault flow). Replace the
seven private regexes with `camerata_secrets::detect_secrets` and map `SecretShape → SecretKind`
(add variants for the shapes it lacked: Supabase, Slack, Google, Anthropic, webhook, connection
string, URL query, named literal). Its unit tests stay. Its module doc's "two independent modules"
paragraph is rewritten to "thin adapter over camerata-secrets".

**`camerata_llm::credentials::mask_value`**: unchanged (it masks OUR keys for the settings UI).

**The orchestrator crate's `Cargo.toml`** gains `camerata-secrets`; its direct `regex` dependency
stays only if other modules use it.

## 4. Redaction points

### 4.1 The construction-time choke point

A new module `crates/server/src/onboard/redact.rs` (`pub(crate)`):

```rust
/// Redact every client-visible free-text field of a finding IN PLACE. Called by every producer
/// (4.2) the moment the finding is complete, and again by `ScanReport::seal` and
/// `report_export::sanitize_finding_text` as belt and braces. Idempotent.
pub(crate) fn redact_finding(f: &mut Finding) -> usize /* redactions */ {
    // snippet, detail, fix_specific, calibration_rationale, captures values,
    // also_locations[*].snippet, kind payload strings (ConventionObserved.convention_label,
    // NotAssessed.reason_detail, NoConvention.observed_options[*].label, Deviation.convention_label)
}
pub(crate) fn redact_findings(fs: &mut [Finding]) -> usize;

/// The ONE way a code line becomes a snippet: trim, redact, then cap at `max` chars (redact
/// BEFORE capping, so a cap can never cut a secret in half and leave a prefix+partial body that
/// the gate cannot recognise). Replaces the four ad hoc `.trim().chars().take(160)` sites.
pub(crate) fn snippet_from_line(line: &str, max: usize) -> String;

/// Redact ScanReport-level text that bypasses Finding: recommendations[*].evidence and
/// recommendation_reasoning and observed_options[*].label, coverage_notes[*].message,
/// failed_passes[*].reason, ai_error, ai_blocked_reason, message, proposed_rules text.
pub(crate) fn redact_report_text(r: &mut ScanReport) -> usize;
```

`ScanReport::seal()` (the one gateway into every exporter; `SealedReport`) calls
`redact_findings` + `redact_report_text` before sealing, and logs (`tracing::warn`) the count when
it is non-zero with the message "N secret values reached seal unredacted; a producer is missing
redact_finding (see docs/design/2026-10-23)" so the belt-and-braces catch is visible, never
silent. `report_export::sanitize_finding_text` (`report_export.rs:207-220`) is extended to call
`redact_finding` and to cover `fix_specific`, `also_locations`, `calibration_rationale` for the
zero-width strip too (closing that unrelated gap at the same time).

`Finding` keeps its public fields (making construction private would touch ~30 test fixtures
and every `..Finding::default()` producer; the gate, not the type system, is the enforcement of
record in V1). The V2 hardening (a `ClientText` newtype whose only constructor redacts) is noted,
not built.

### 4.2 Every path a raw secret reaches a client artifact or a model, and how each is covered

Legend: **LEAK (seen)** = the sweep exports show a real value there; **LEAK (possible)** = raw
client text flows there; **safe** = no client text, listed for completeness.

**A. Finding constructors (production; file:line of the `Finding {` literal)**

| # | Site | What it writes | Status | Coverage |
|---|---|---|---|---|
| A1 | `onboard/audit.rs:306` floor content row | `snippet` = raw trimmed line, 160 chars (`:278-282`) | LEAK (seen: faktura `findings[0].snippet`, hearth `findings[1].snippet`) | `snippet_from_line`; `redact_finding` at `:329` (after the literal) |
| A2 | `onboard/audit.rs:242` floor path-based row | `snippet` = path; static detail | safe | `redact_finding` anyway |
| A3 | `onboard/audit.rs:534` reason-less waiver row | static text | safe | same |
| A4 | `onboard/architectural.rs:176` `arch_violation_to_finding` | `snippet` = checker `object`, `detail` = checker `message`, `captures` | LEAK (possible: a checker message that quotes a literal) | `redact_finding` before return |
| A5 | `scan_tools.rs:984` `preview_finding` | `snippet` = tool message (160 chars), `detail` = message | LEAK (possible: semgrep/eslint messages interpolate matched text) | `redact_finding` before return |
| A6 | `scan_tools.rs:402` `note_finding` | tool failure text, `NotAssessed.reason_detail` | LEAK (possible: tool stderr) | same, incl. kind payload |
| A7 | `dep_audit.rs:245` | package coordinate + advisory | safe | same |
| A8 | `ai_audit.rs:653` `parse_ai_findings` | `snippet` = model's `code` VERBATIM, `detail` = model prose, `captures` from model | LEAK (seen: portal `findings[5]`, an AI-tier `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` row whose `snippet` is the key line) | `redact_finding` at push; prompts also redacted (5) |
| A9 | `ai_audit.rs:6449` violating-option row | `snippet` = `evidence` (200 chars, model text) | LEAK (possible) | `redact_finding` |
| A10 | `ai_audit.rs:6645` `convention_observed` rows | `snippet` = `rec.evidence`, `detail` += `recommendation_reasoning` | LEAK (possible) | `redact_finding` |
| A11 | `ai_audit.rs:6702` `no_convention_findings` | option labels + reasoning | LEAK (possible) | `redact_finding` |
| A12 | `ai_audit.rs:6742` `not_assessed_findings` | skip note | safe | same |
| A13 | `ai_audit.rs:5862` `build_structural_group_finding` | detail from members; `also_locations` snippets (`:5843-5850`) | inherits | members already redacted; `redact_finding` anyway |
| A14 | `ai_audit.rs:4614` `merge_location_group`, `:5597` `merge_semantic_group` | carry `snippet`, `fix_specific`, `also_locations` (`:4650`, `:5650`, `:5690`) | inherits | inputs redacted; call `redact_finding` on the primary before return |
| A15 | `onboard/clone_advisory/findings.rs:344` `confirmed_finding` | `snippet` = `first_line(site, files)` raw code; `also_locations[*].snippet` raw; `detail` via `logic_detail` may quote `differing_strings` from code | LEAK (possible) | `first_line` → `snippet_from_line`; `redact_finding` before return |
| A16 | `onboard/clone_advisory/findings.rs:378` `not_assessed_row` | reason text | safe | same |

**B. Text mutation sites after construction (production)**

| # | Site | What | Coverage |
|---|---|---|---|
| B1 | `ai_audit.rs:3378` `fix_specific = Some(text)` (model) | LEAK (seen: faktura `findings[0]` and `[43]`, hearth `findings[1]`: the fix text repeats the full key and instructs with it) | `redact_str` on `text` before the self-check (`fix_grounded`/`fix_contradicts_detail` then compare redacted to redacted); and the fix-specific system prompt (`:2958`) gains the rule "never repeat a credential value; name the variable and line" (work order §3). Residual: `redact_finding` at seal. |
| B2 | `ai_audit.rs:3515` `fix_specific = resolve_fix(...)` fallback | authored remediation + captures | captures redacted (A); `redact_finding` at seal |
| B3 | `ai_audit.rs:1113` `detail` += `[needs review: reason]` (model reason) | LEAK (possible) | `redact_str(reason)` |
| B4 | `ai_audit.rs:1050, 1506, 1784-1897` `calibration_rationale` | templated; may embed detail text | `redact_finding` at seal (field now covered) |
| B5 | `ai_audit.rs:5143` `captures.insert("object", …)` from `structural_objects(f)` | identifiers from text | at seal |
| B6 | `ai_audit.rs:6107` `detail` rewrite (`strip_cross_reference_sentences`) | rewrites already-redacted text | none needed |
| B7 | `clone_advisory/findings.rs:316, 323` captures `concept`/`family` | judge concept is `scrub_concept`-ed (`prompt.rs`), family is an id | none needed |
| B8 | `report_export.rs:213-214` `sanitize_finding_text` | zero-width strip | extended per 4.1 |

**C. Report-level text that bypasses `Finding`**

| # | Site | Reaches | Coverage |
|---|---|---|---|
| C1 | `ScanReport.recommendations[*].evidence` (`ai_audit.rs:122`, model text) | xlsx "Rules applied" rows (`xlsx_export.rs:257-278`, `evidence: rec.evidence`) → Coverage sheet, `csv/rules-applied.csv`, `json/rules-applied.json` | LEAK (possible, a surface no finding redaction touches) | `redact_report_text` at seal; gate walks these files |
| C2 | `recommendation_reasoning`, `observed_options[*].label` | A10/A11 detail/snippet; evidence gate (`report_export.rs:3747-3800`) | same |
| C3 | `coverage_notes[*].message` (`onboard.rs:574-576`, tool text) → `MethodologyJson.not_done` | PDF methodology | same |
| C4 | `failed_passes[*].reason` → `MethodologyJson.failed_passes`, `ExecutiveSummaryJson.failed_passes` | PDF | same |
| C5 | `ReportOptions.executive_summary_override` (`report_export.rs:74`, operator-typed) → `ExecutiveSummaryJson.narrative` | PDF | gate catches (operator paste) |
| C6 | `ai_error`, `ai_blocked_reason`, `message`, `proposed_rules` | operator UI only, not exported | `redact_report_text` anyway |

**D. Export builders (all read the sealed, redacted set; listed so the gate's coverage is explicit)**

| # | Builder | Output |
|---|---|---|
| D1 | `report_export::build_report_json` (`report_export.rs:3837`) → `CuratedSiteJson{snippet, detail, headline, fix, fix_for_this_finding, also_locations}` (`:1150-1228`), `ExecutiveSummaryJson`, `MethodologyJson` | `AuditReportJson` → `data.json` → PDF (`compile_pdf`, `:4940`) |
| D2 | `xlsx_export::partition_rows` (`xlsx_export.rs:307`) → `FindingRow` (`:105`, incl. `snippet`, `detail`, `fix_specific`, `fix`, `also_locations`) | `build_workbook` (`:2104`), `build_findings_export` (`:2369`, `findings.json`), `build_json_exports` (`:2393`), `build_csv_exports` (`:2510`) |
| D3 | `xlsx_export::build_rules_applied_rows` (`:253`) | Coverage sheet + `rules-applied` csv/json (C1) |
| D4 | `product_export_readme` (`lib.rs:16580`), `build_manifest` (`lib.rs:16809`) | provenance only (repo names, shas, dates); walked by the gate anyway |
| D5 | `export_audit_report` (`lib.rs:16254`, PDF-only route `/api/projects/:id/audit-report`) | a PDF outside any zip; the gate's secrets check runs on its `data.json` too (7.5) |

**E. What the scan stores on disk**

| # | Store | Content | Coverage |
|---|---|---|---|
| E1 | Server incremental-scan cache `~/Library/Application Support/camerata/scan-cache.json` (`lib.rs:923`, `scan_cache.rs:341-349`) | `ScanManifest.ai_findings`: full `Finding`s, today raw | findings redacted at construction → cache inherits. `MANIFEST_VERSION` 1 → 2 (`scan_cache.rs:29`); `ScanCacheStore::load_or_new` drops every manifest whose version differs AND rewrites the file at once, so the raw v1 content is gone on first start, not on next scan. |
| E2 | CLI cache `~/Library/Caches/camerata/<repo-key>.json` (`inspect_cmd.rs:229-237`, `save_manifest` `:323`) | same `ScanManifest` | same version bump; `load_prior_manifest` already rejects a mismatch; add the overwrite |
| E3 | Legacy in-repo `.camerata-scan-cache.json` / `.camerata-scan-ledger.json` | written by an old CLI into the scanned repo (`scan_ledger.rs:368`); hearth's `findings[1].also_locations[5..7]` cite `.camerata-scan-cache.json:1965/1990/2015`, i.e. the engine re-ingested its own cache | already excluded (`is_noise_path`, `onboard/files.rs:326` `ENGINE_ARTIFACT_PREFIX`; integrity gate `integrity_gate.rs:401`). Acceptance prep: delete the two stale files from the hearth fixture checkout. |
| E4 | `last_scan` / `recent_scan` (`lib.rs:717-745`) | in memory only | inherits |
| E5 | `.camerata-scan-ledger.json` (CLI, `inspect_cmd.rs:366`) and the server ledger | counts only (`scan_ledger.rs:130-285`), no text | safe |
| E6 | Transcripts (`transcript.rs` `TranscriptStore`, in memory; registered at `ai_audit.rs:3690`, `:7855-7863` with `prompt: prompt.clone()`, clone judge `clone_advisory.rs:309`) | prompts + model output, served to the cockpit | prompts redacted (5); model output is whatever came back |
| E7 | Client-repo `.camerata/baseline.json` | `rule_id, path, fingerprint, reason`; no snippet | safe; fingerprints change (6) |
| E8 | Feedback store (`lib.rs:3031-3034`) | already scrubbed | adapter now shared (3.3) |

**F. Operator-only surfaces (not client artifacts; listed because the rule is class-level)**

| # | Surface | Coverage |
|---|---|---|
| F1 | `OnboardFinding` DTO (`lib.rs:7820`) and every `/api/onboard/*` findings response → cockpit | inherits construction-time redaction |
| F2 | `GET /api/onboard/finding-context` (`onboard/finding_context.rs`, route `lib.rs:1083`) reads the file on demand and returns the enclosing code block raw | apply `redact_str` to the returned block lines (cheap; keeps the operator's own screen share safe) |

### 4.3 Model prompts (every builder that sends code or finding text)

| # | Builder | What it sends | Coverage (section 5) |
|---|---|---|---|
| P1 | `ai_audit::build_digest(files)` (`ai_audit.rs:375`) → `build_pass_prompt` (`:3642`) → `audit_pass` (`:3671`); the deep lenses (`:7850-7878`, `{repo_map}{digest}`); the alternatives pass (`:8010-8040`, same digest) | numbered raw file contents | `files` redacted once before any digest (5.1) |
| P2 | `verify_findings` (`:2016`; per finding `severity, path, line, snippet, detail` at `:2045-2047`) | finding text | findings already redacted (4.1) |
| P3 | `build_fix_specific_block` (`:3036`) | `snippet`, `detail`, `captures`, and `fix_context_window(files, path, line, 8)`: eight raw lines either side | `files` redacted (5.1) + findings redacted |
| P4 | Clone judge: `camerata_checks::clone_rules::judge::judge_input` (`crates/checks/src/clone_rules/judge.rs`) → `excerpt(source, …)` code + `differing_strings`; sent by `onboard/clone_advisory.rs:297-309` | raw code excerpts and string literals | `req.files` is the redacted vector (5.1); `judge.rs` additionally calls `camerata_secrets::redact_str` on `code` and each differing string (5.4), so the checks crate is safe when called from `camerata-check` or any other host |
| P5 | Governed-dev side: `grounding::render_repo_digest` (`grounding.rs:130`) → `chat_escalation` (`lib.rs:9868`), conflict/PR resolvers (`lib.rs:15033, 15367`), `decompose.rs:131`, `review_agent.rs:106` (diff) | repo digest / diff | `render_repo_digest` redacts its output; `build_l3_prompt` redacts the diff (5.3) |
| P6 | "The convention synthesiser" | There is no separate model pass by that name: convention / no-convention / not-assessed rows are synthesised DETERMINISTICALLY (`ai_audit.rs:6600-6760`) from the alternatives pass's `RuleRecommendation` (P1 digest in, `evidence`/`reasoning` out). Covered by P1 (input) and A10/A11/C1/C2 (output). |

## 5. Model-prompt redaction

Decision (owner): secrets are redacted BEFORE any code is sent to a model, by default.

### 5.1 One point: the file vector

Every inspection prompt that carries code reads from the scan's `files: Vec<(String, String)>`
(path, content). Add to `onboard.rs` (next to the file loader that builds that vector for the AI
tiers):

```rust
/// The file set the MODEL tiers read. Same paths, same line count, every secret value replaced
/// by its redacted form. The deterministic floor and the architectural checkers keep reading the
/// RAW set (they must still FIND the secret); only what leaves the machine is redacted.
pub(crate) fn files_for_model(files: &[(String, String)]) -> Vec<(String, String)>
```

It is applied exactly once per scan, where `audit_repo` (the AI audit entry) receives its files,
and the resulting vector is what `build_digest`, `fix_context_window`, the deep lenses, the
alternatives pass and `judge_clone_candidates`'s `req.files` receive. Line counts are preserved
by construction (2.1), so every `path:line` the model cites still resolves. The system prompts
(`audit_system_prompt`, `verify_system_prompt`, `fix_specific_system_prompt`, `alternatives_system_prompt`,
the deep lens prompts, the clone judge `system_prompt`) gain one sentence: "Credential values in
the code appear as `prefix…tail (N chars)`; this is deliberate redaction, not a bug in the code.
Never reproduce a credential value; refer to it by its variable name and line." The fix-specific
self-check (`fix_grounded`) treats a redacted token as part of the evidence it was given.

### 5.2 The switch

`feature_flags.rs` gains `model_prompt_redaction` (default ON). When OFF, `files_for_model`
returns a clone of the raw set and `tracing::warn!`s once per scan that raw code is leaving the
machine. The flag is operator-only and exists because the owner said "by default"; it has no UI
affordance in V1 (an env override through the existing flags mechanism is enough).

### 5.3 Governed-dev prompts

`grounding::render_repo_digest` returns `redact_str` of its digest; `review_agent::build_l3_prompt`
redacts the diff it embeds; `decompose.rs`'s repo digest goes through `render_repo_digest`
already. These are not the inspection product but the same class rule applies and the cost is one
call each.

### 5.4 The clone judge inside `camerata-checks`

`camerata-checks` gains `camerata-secrets` as a dependency and `judge.rs::member_of` redacts
`code` and `differing_strings` before they are placed in `JudgeInput`. This makes the judge input
safe regardless of which host assembled `files` (the server passes a redacted set anyway; the
double application is idempotent).

### 5.5 What this does not cover (stated plainly)

- The CLI backend's `claude -p` subprocess keeps its own conversation transcripts under the
  operator's home directory; those contain the (now redacted) prompts. Nothing raw leaves.
- Model OUTPUT can still contain a secret the model knows from elsewhere or reconstructs; the
  construction-time redaction (A8, B1) and the gate cover the output side.

## 6. The fingerprint change

`suppression::fingerprint(rule_id, snippet)` (`suppression.rs:156-159`), `report_export::finding_key`
(`report_export.rs:239`) and `ui_core::triage::finding_key` (`triage.rs:1016`) all consume
`snippet`. With redaction at construction they now hash the REDACTED snippet. Consequences,
accepted by the owner (no clients yet):

- Every fingerprint in a client repo's `.camerata/baseline.json` that was computed from a raw
  snippet stops matching; those findings return to `active`. The fixtures' baselines (if any) are
  regenerated during the acceptance run.
- Dispositions stored by the cockpit under `finding_key` for a secret-bearing row are orphaned
  once; the UI keeps showing the row as undispositioned.
- The scan cache is invalidated by the version bump (E1/E2), so no carried finding mixes a raw
  snippet with a redacted twin.
- No code change is needed in the three functions. One new test each proves the fingerprint and
  key are computed from redacted text and are stable across two scans of the same file.

## 7. The export release gate

### 7.1 Shape and placement

New module `crates/server/src/release_gate.rs` (`pub mod release_gate;` in `lib.rs` next to
`integrity_gate`). The integrity gate (`integrity_gate.rs:367 evaluate`) stays as the pre-export
INPUT gate over the ledger; the release gate is the post-build OUTPUT gate over the bytes.

```rust
/// Everything one product export consists of, built by `build_product_package` and nothing else.
pub struct ProductPackage {
    pub stem: String,
    pub report_json: report_export::AuditReportJson,   // the PDF's input data
    pub data_json: String,                              // the EXACT bytes compile_pdf wrote
    pub pdf: Vec<u8>,
    pub xlsx: Vec<u8>,
    pub findings_export: xlsx_export::FindingsExport,
    pub findings_json: Vec<u8>,
    pub datasets: Vec<(String, Vec<u8>)>,               // csv/*.csv then json/*.json
    pub readme: String,
    pub provenance: ManifestProvenance,
}

/// THE assembly: build_report_json → render_data_json → compile_pdf → build_workbook →
/// build_findings_export → build_csv_exports → build_json_exports → export_provenance →
/// product_export_readme. Replaces the three hand-rolled copies (lib.rs export_product,
/// cli inspect_cmd build_product_export_bytes, tests/generate_sample_report.rs).
pub async fn build_product_package(
    report: &onboard::SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    options: &ReportOptions,
) -> anyhow::Result<ProductPackage>;

/// Run every check (7.2) over the package, zip it, re-read the zip and run the manifest and
/// provenance checks over the real archive. Ok only when every check passes. This is the ONLY
/// function the three real exporters call to obtain zip bytes.
pub fn release(package: ProductPackage) -> Result<ReleasedZip, GateFailure>;

pub struct ReleasedZip { pub stem: String, pub bytes: Vec<u8>, pub report: GateReport }

/// Which checks ran, in order, and what each found (all empty on success). Written to the
/// operator log and returned in the HTTP body on success as `gate: {checks: [...]}`.
pub struct GateReport { pub checks: Vec<CheckOutcome> }
pub struct CheckOutcome { pub check: CheckId, pub problems: Vec<Problem> }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckId { Secrets, DatasetSet, DatasetParity, RowParity, Manifest, Provenance }

/// One fail-loud line. Never carries a secret: `evidence` is the redacted form only.
pub struct Problem { pub file: String, pub location: String, pub message: String, pub evidence: Option<String> }

pub struct GateFailure { pub stem: String, pub report: GateReport }
impl std::fmt::Display for GateFailure { /* 7.3 */ }

/// The checks over an EXISTING zip (the acceptance run and `camerata release-gate <zip>`):
/// secrets over every text member, xlsx strings, and `pdf_text` when the caller extracted it;
/// manifest; provenance. Cannot run DatasetParity against in-memory rows (there are none) but
/// does run csv↔json parity and the dataset set.
pub fn audit_zip(zip_bytes: &[u8], pdf_text: Option<&str>) -> GateReport;

/// The PDF-only route's gate: the secrets check over the data.json alone.
pub fn check_pdf_data(data_json: &str) -> Result<(), GateFailure>;
```

`compile_pdf(json)` is split into `render_data_json(&AuditReportJson) -> String` (one
`serde_json::to_string`) and `compile_pdf_from_data(data_json: &str) -> Result<Vec<u8>>`, so the
gate inspects the byte-identical input Typst read. `build_product_zip` becomes `pub(crate)` and
is called only by `release` (and by the manifest tests through a `#[doc(hidden)] pub` re-export
`assemble_unreleased_zip` so the corruption tests can still build a tampered archive).

### 7.2 The checks, in order (secrets first)

1. **`Secrets`** (`camerata_secrets::find_secrets_in_json` / `first_secret`), over, in order:
   `data_json` (parsed; locations like `curated_findings[3].sites[0].snippet`), `findings_json`
   (parsed), each `json/*.json` (parsed), each `csv/*.csv` (parsed with the `csv` crate; location
   `row 12, column "Snippet"`), the xlsx shared strings (`workbook_shared_string_texts` promoted
   to `release_gate::xlsx_strings(bytes) -> Vec<String>`; location `shared string #n`, plus the
   sheet/cell when the gate's small `<c r=…>` reader resolves it; the test module's existing
   cell reader in `machine_export_dataset_parity_e2e.rs:405-478` moves into this module as
   `xlsx_cells(bytes) -> Vec<(sheet, cell, text)>` and both the gate and gate 2 use it), the
   README, the stem. Twins are not problems. Any hit is a problem with
   `evidence = Some(m.redacted_form(text))` and `message = "{shape.display_name()} present in a
   client file"`. Runs even when a later check would also fail; the gate reports everything.
2. **`DatasetSet`** (gate 4 runtime): exactly `DATASETS` (the eight names) under `csv/` and
   `json/`, no extras, plus `{stem}.pdf` (starts with `%PDF-`), `{stem}-findings.xlsx` (a zip
   containing `xl/sharedStrings.xml`), `findings.json`, `README.txt` present and non-empty.
   `DATASETS` moves from the test file to `xlsx_export` (one definition).
3. **`DatasetParity`** (gates 1/2 runtime): `compare_csv_to_json(dataset, csv, json) ->
   Vec<Problem>` (the body of `assert_csv_matches_json`, returning instead of asserting: row count,
   header set, every cell), for every dataset; and `findings.json.findings` equals
   `json/all-findings.json` rows (gate 1). The xlsx cell-by-cell projection stays test-only but
   calls the same `compare_*` functions with rows it read via `xlsx_cells`.
4. **`RowParity`** (`req_d1` runtime): the set of `(repo, rule_id, path, line)` over
   `report_json.curated_findings[*].sites` + `held_for_review_findings[*].sites` equals the
   non-FP rows of `findings_export.findings`; `cover`/scorecard totals equal the row counts
   (`req_d2`). Moved from the two `export_invariants_gate` tests into `row_parity(&AuditReportJson,
   &FindingsExport) -> Vec<Problem>`; the tests assert it returns empty.
5. **`Manifest`** (gate 6 runtime, AFTER zipping): read the archive back, `verify_manifest(entries)
   -> Vec<Problem>` (the body of `manifest_failures`: every `files[]` sha256 equals the member's
   hash; every member except `manifest.json` is listed; a listed path exists).
6. **`Provenance`** (gate 7 runtime): `verify_readme_provenance(readme, manifest_json) ->
   Vec<Problem>` (the body of the gate 7 tests: each `inspection` value appears in the README;
   each README provenance line equals its manifest field; absent values are null and omitted).

DRY rule: each test named in section 8.5 calls the production function and asserts on its
returned `Vec<Problem>`; no comparison logic remains in a test file.

### 7.3 The fail-loud message

`GateFailure`'s `Display`, written to stderr by the CLI, returned as the HTTP body's `message`,
and shown by the cockpit:

```
EXPORT BLOCKED by the release gate for camerata-inspection-faktura-5225b49 (3 problems)
  [secrets]    findings.json › findings[0].fix_specific
               Supabase secret key present in a client file: sb_secret_…WvXy (51 chars)
  [secrets]    camerata-inspection-faktura-5225b49-findings.xlsx › sheet "All Findings" cell V2
               Supabase secret key present in a client file: sb_secret_…WvXy (51 chars)
  [secrets]    PDF data › curated_findings[0].sites[0].snippet (renders on the PDF)
               Supabase secret key present in a client file: sb_secret_…WvXy (51 chars)
Nothing was written. The value above is shown redacted; the finding's own file:line names where
it lives in the client's code. Fix the producer, rescan or re-export, and the gate runs again.
```

One line per problem: `[check]`, `file › location`, then the message with the redacted evidence.
The gate never prints the value, never prints a raw cell, never prints a raw prompt. A test
plants a secret in every surface and asserts the whole `Display` string contains no raw value
(`gate_failure_message_never_contains_the_secret`).

### 7.4 Hook points

| Caller | Today | After |
|---|---|---|
| HTTP `export_product` (`lib.rs:16379-16562`) | inline assembly, `build_product_zip` | `let pkg = release_gate::build_product_package(&report, &req.dispositions, corpus.as_ref(), &req.options).await?; match release_gate::release(pkg) { Ok(z) => 200 zip, Err(f) => 422 Json{ok:false, gate:"release", message: f.to_string(), problems: f.report} }` |
| HTTP `export_audit_report` (`lib.rs:16254`, PDF only) | `build_report_json` + `compile_pdf` | `render_data_json` → `release_gate::check_pdf_data(&data)?` (422 on failure) → `compile_pdf_from_data` |
| CLI `inspect --export` (`inspect_cmd.rs:387 build_product_export_bytes`, written at `:653`) | its own copy | calls `build_product_package` + `release`; on `Err(f)` prints `f` and exits 3 (new exit code documented next to the integrity gate's 2) ; the zip is never written |
| Sample generator (`tests/generate_sample_report.rs:536`) | its own copy | `build_product_package` + `release`; `expect` panics with the `Display` block |
| `camerata release-gate <zip>` (new CLI subcommand, `crates/cli/src/release_gate_cmd.rs`) | n/a | `audit_zip(bytes, pdf_text)` where `pdf_text` comes from `pdf_extract` behind a cargo feature `pdf-text` on the CLI (pdf-extract becomes a normal dep of the CLI only under that feature; the server keeps it as a dev-dep). Exit 0 clean, 3 blocked. This is the acceptance tool. |

A source-grep test (precedent: `lib.rs:26220` finds `async fn export_product(` in source) asserts
that `build_product_zip(` is called from exactly one production site (`release_gate.rs`) and that
`compile_pdf_from_data(` appears only in `release_gate.rs` and `export_audit_report`.

### 7.5 What the operator sees

- **Cockpit:** the export dialog shows a red block titled "Export blocked by the release gate"
  with the problem lines verbatim and a "Copy details" button. No file is downloaded. The 422
  body is `{ ok: false, gate: "release", stem, message, problems: [{check, file, location, message,
  evidence}] }`; `ui-core` maps it to the dialog state (one new variant on the existing export
  error state; no new screen).
- **CLI:** the block on stderr, exit 3, no zip on disk, and the usual `inspect.log` line
  `release gate: BLOCKED (3 problems)`. On success: `release gate: passed (6 checks)`.
- **Success path:** the 200 response and the CLI summary both carry `gate.checks` (six names,
  zero problems) so an operator can see the gate ran; the README gains one line under provenance:
  `Release checks: secrets, dataset set, dataset parity, row parity, manifest, provenance (all
  passed)`. Gate 7's README/manifest parity covers that line too (`inspection.release_checks`).

## 8. Tests, by name

Helpers: `secret(shape)` builds a fixture value at runtime from fragments (2.4), never a literal;
`plant(f: &mut Finding, field, value)`; `package_with(findings) -> ProductPackage` builds through
`build_product_package` with a tiny corpus-less report (typst-present-only where the PDF is needed,
following `which_typst()`'s skip-with-note convention).

### 8.1 `camerata-secrets` (unit; no model, no I/O)

Positive, one per shape S1..S17 (`detects_<label>` and `redacts_<label>_to_prefix_tail_length`):
each asserts the shape, the span, `twin == None`, and the exact redacted form including the
`(N chars)` count. Plus:
- `supabase_jwt_with_service_role_claim_is_a_secret_and_the_form_names_the_role`
- `supabase_jwt_with_anon_role_is_the_public_key_twin`
- `supabase_jwt_with_authenticated_role_is_a_twin`
- `jwt_with_no_role_claim_is_opaque_jwt_redacted_but_belongs_to_no_rule`
- `undecodable_jwt_payload_is_opaque_jwt`
- `short_body_shows_length_only_no_tail` (S1 with a 10-char body; S13 with a 25-char literal → `… (25 chars)` rule for prefix-less when `N <= 16` is exercised with a 16-char URL value)
- `connection_string_redacts_only_the_password_and_keeps_user_and_host`
- `url_query_redacts_only_the_value_and_keeps_the_param_name`
- `named_literal_redacts_only_the_quoted_literal_and_keeps_the_identifier`
- `bearer_redacts_only_the_token`
- `bearer_wrapping_a_github_token_is_reported_once_as_github`
- `pem_block_collapses_body_lines_and_keeps_line_count`
- `pem_header_alone_is_left_readable`
- `redaction_preserves_newline_count_for_every_shape`
- `redaction_is_idempotent_for_every_shape`
- `overlapping_matches_keep_the_longest_then_earliest`
- `match_lines_attributes_each_match_to_its_starting_line_and_dedupes`
- `shapes_for_rule_maps_every_sec_rule_and_unknown_is_empty`
- `every_shape_has_a_label_display_name_and_prefix_decision`

Safe twins (each expects `twin == Some(..)`, `detect_secrets` empty, `redact` unchanged):
- `twin_placeholder_word_in_vendor_body_stays_readable` (one case per word in the T1 list, via a table)
- `twin_uppercase_snake_case_body_with_do_not_and_fake_stays_readable` (the portal shape)
- `twin_env_name_literal_is_a_name_not_a_value` (`'SUPABASE_SERVICE_ROLE_KEY'` next to the word key; the faktura page-40 case)
- `twin_published_stripe_docs_test_key_by_digest` (value built from fragments, hashed, compared)
- `twin_published_aws_example_ids`
- `twin_supabase_publishable_prefix_is_never_matched`
- `twin_stripe_publishable_live_and_test_are_never_matched`
- `twin_template_tokens_in_dsn_and_query_stay_readable` (`[YOUR-PASSWORD]`, `${PASSWORD}`, `{{token}}`, `<secret>`, `%s`)
- `twin_prose_sb_secret_hyphen_prefixed_does_not_match`
- `twin_redaction_marker_never_rematches`
- `twin_ordinary_prose_about_keys_tokens_and_bearer_bonds_is_untouched` (the scrubber's prose test, ported)
- `not_a_twin_env_example_filename_does_not_change_detection` (the detector has no path input; this test exists to document it)
- `not_a_twin_real_looking_sk_test_is_redacted` (pending Q1)
- `no_secret_shaped_literal_in_this_crates_source` (runs `detect_secrets` over `src/**/*.rs`)

### 8.2 Gateway migration (`crates/gateway/src/lib.rs` tests)

- `vendor_token_arm_denies_a_service_role_jwt_and_allows_the_anon_jwt`
- `vendor_token_arm_denies_whsec_and_rk_live`
- `vendor_token_arm_denies_a_supabase_dsn_with_inline_password`
- `hardcoded_secrets_arm_denial_message_shows_only_the_redacted_form`
- `content_match_lines_agrees_with_camerata_secrets_match_lines_for_every_sec_rule`
- every existing SEC test keeps its name; fixtures adjusted for the unified thresholds are listed in the commit.

### 8.3 Construction-time redaction (server unit tests, one per producer and surface)

For each shape S in {S1, S2, S3, S5, S12, S13, S15} (the shapes the sweep exports and the work
order name), a table-driven test per producer asserts the produced `Finding` carries the redacted
form and not the value in `snippet`, `detail`, `fix_specific`, `also_locations[*].snippet`,
`captures`:
- `floor_audit_content_snippet_is_redacted_for_<S>` (`audit.rs`)
- `floor_snippet_is_redacted_before_the_160_char_cap` (a secret straddling the cap)
- `arch_violation_to_finding_redacts_message_and_object`
- `preview_finding_redacts_a_tool_message_that_echoes_a_literal`
- `note_finding_redacts_tool_stderr_in_detail_and_reason_detail`
- `parse_ai_findings_redacts_model_code_and_detail`
- `violating_option_and_convention_rows_redact_evidence_and_reasoning`
- `clone_confirmed_finding_redacts_first_line_and_also_locations_and_differing_strings`
- `merge_location_group_output_is_redacted_when_a_member_was_not` (belt and braces)
- `fix_specific_assignment_redacts_model_text_before_the_self_check`
- `fix_specific_prompt_forbids_repeating_the_value` (asserts the system prompt sentence)
- `needs_review_reason_appended_to_detail_is_redacted`
- `seal_redacts_any_finding_a_producer_missed_and_warns_once`
- `seal_redacts_recommendation_evidence_coverage_notes_and_failed_passes`
- `sanitize_finding_text_covers_fix_specific_also_locations_and_rationale`
- `redact_finding_is_idempotent`
- `fingerprint_and_finding_key_are_computed_from_the_redacted_snippet_and_stable`
- `scan_cache_manifest_v1_is_dropped_and_the_file_rewritten_on_load` (`scan_cache.rs`) and the CLI twin `load_prior_manifest_rejects_v1_and_save_overwrites_it`
- `finding_context_lookup_redacts_the_returned_block`

### 8.4 Model-prompt redaction

- `files_for_model_redacts_every_shape_and_keeps_line_counts`
- `build_digest_over_files_for_model_contains_no_secret_value`
- `fix_context_window_never_contains_a_secret_value`
- `deep_lens_and_alternatives_prompts_contain_no_secret_value` (stub `LlmPort` captures the request; existing `digest_segment` helper at `ai_audit.rs:12615`)
- `verify_findings_prompt_contains_only_redacted_snippets`
- `clone_judge_input_code_and_differing_strings_are_redacted` (`checks` crate; no model)
- `model_prompt_redaction_flag_off_sends_raw_and_warns`
- `render_repo_digest_is_redacted` (`grounding.rs`), `l3_prompt_diff_is_redacted` (`review_agent.rs`)
- `audit_system_prompt_explains_redaction_markers` (one assertion per system prompt)

### 8.5 The release gate (`release_gate` tests + the existing gate files now calling production code)

- `release_passes_a_clean_package_and_reports_six_checks`
- `release_blocks_a_secret_planted_in_<surface>` for each surface: `data_json` (PDF data),
  `findings_json`, each dataset json, each dataset csv, xlsx shared string, README, stem; the
  problem's `file`, `location` and `evidence` are asserted exactly
- `release_reports_every_problem_not_just_the_first`
- `gate_failure_message_never_contains_the_secret` (every shape, every surface, `Display` string)
- `secrets_check_ignores_twins_in_every_surface`
- `secrets_check_locates_an_xlsx_hit_by_sheet_and_cell`
- `secrets_check_walks_also_locations_not_a_named_field_list` (the hearth regression)
- `dataset_set_check_flags_a_missing_and_an_extra_dataset`
- `dataset_parity_check_flags_a_cell_disagreement` (reuses gate 2's `gate2_detects_a_disagreement_in_each_format` fixture via the shared function)
- `row_parity_check_flags_a_pdf_row_absent_from_findings_json`
- `manifest_check_flags_one_corrupted_byte_in_any_member` (gate 6's three tests now call `verify_manifest`)
- `provenance_check_flags_a_readme_manifest_mismatch` (gate 7's three tests now call `verify_readme_provenance`)
- `release_never_writes_or_returns_bytes_on_failure`
- `build_product_zip_is_called_from_release_only` and `compile_pdf_from_data_is_called_from_release_and_the_pdf_route_only` (source-grep tests)
- `export_product_returns_422_with_the_problem_list_when_blocked` (router test, stub typst via the existing `typst_on_path` skip)
- `export_audit_report_pdf_route_is_gated_on_data_json`
- `cli_inspect_export_exits_3_and_writes_nothing_when_blocked`
- `audit_zip_over_a_released_zip_is_clean` and `audit_zip_flags_a_tampered_member`
- Existing: `gate2_*`, `gate6_*`, `gate7_*`, `req_d1_*`, `req_d2_*` keep their names and now assert `Vec::is_empty()` on the production functions.

### 8.6 The PDF smoke test (typst-present-only; `pdf-extract` dev-dep)

- `pdf_text_contains_the_redacted_form_and_never_the_value`: build a sealed report with one
  finding per shape (values from fragments), `build_product_package`, `release`, then
  `pdf_extract::extract_text_from_mem(&pkg.pdf)`; assert each redacted form appears and no value
  appears, also over the text with newlines removed (Typst wraps long tokens; the sweep scan
  needed the joined variant to see a wrapped key).
- `pdf_text_extraction_detects_a_raw_secret_when_the_gate_is_bypassed`: compile a hand-built
  `AuditReportJson` with a raw value in a snippet through `compile_pdf_from_data` directly, extract,
  assert `camerata_secrets::first_secret(&text).is_some()`. This proves the extraction approach
  can FAIL (the gate 2 "must be able to fail" principle) and that the data.json check is the
  load-bearing one.
- Justification of the approach: the PDF check of record reads `data.json` (owner decision),
  because Typst stores page text in compressed content streams with font subsetting, and a byte
  scan of the PDF is meaningless; `pdf-extract` is already a dependency, resolves `lopdf`, and is
  used by four existing tests; it stays a dev-dependency of the server (the gate itself never
  parses a PDF) and an optional feature dependency of the CLI for `camerata release-gate <zip>`.

### 8.7 Acceptance over the three real exports (`#[ignore]`, run by hand)

`crates/server/tests/release_gate_acceptance.rs`, reading the directory in
`CAMERATA_SWEEP_EXPORTS` (default `~/Documents/Repos/Camerata tests/_runs/sweep5b-2026-10-16`):

- `acceptance_known_leaks_are_flagged_in_the_old_exports`: for `faktura.zip` and `hearth.zip`,
  `audit_zip(bytes, Some(pdf_text))` reports at least the problems in section 10 (by file,
  location and shape; redacted forms compared by prefix+length). For
  `supabase-portal-fixture.zip` the expected outcome depends on owner question Q2 (default: the
  placeholder values are twins, the package is clean, and the test asserts exactly that).
- Then regenerate: `camerata inspect --repo <fixture> --export <out>/<name>.zip` for the three
  fixtures (after deleting hearth's stale `.camerata-scan-cache.json` / `.camerata-scan-ledger.json`
  and purging the two caches), and `acceptance_regenerated_exports_are_clean` runs `audit_zip`
  over the new zips and expects zero problems, with the redacted forms present in `findings.json`
  (`sb_secret_…WvXy (51 chars)` in faktura `findings[*].snippet`), proving the finding survived and
  only its value changed.

## 9. Build order (small green commits; Sonnet builds, Fable reviews)

1. `feat(secrets): camerata-secrets leaf crate with the shape catalogue, twins and redactor`
   (crate, workspace member, 8.1 tests, OVERVIEW crate-map line). No caller yet.
2. `refactor(gateway): SEC content arms read camerata-secrets` (3.3, 8.2; denial messages
   redacted; fixtures adjusted for unified thresholds, listed in the message).
3. `refactor(orchestrator-core): secrets::scrub is an adapter over camerata-secrets` (API kept).
4. `feat(server): redact at finding construction` (`onboard/redact.rs`, every A/B site, seal sweep,
   `sanitize_finding_text` extension, `snippet_from_line`, finding-context block; 8.3 minus cache).
5. `feat(server,cli): scan-cache manifest v2 invalidates and overwrites raw caches` (E1/E2 tests).
6. `feat(server,checks): redact code before any model prompt` (`files_for_model`, flag, system
   prompt sentence, clone judge, grounding, L3; 8.4).
7. `refactor(server): one product-package assembly` (`ProductPackage`, `build_product_package`,
   `render_data_json`/`compile_pdf_from_data`; the three callers switch; behaviour unchanged;
   existing export tests green).
8. `feat(server): export release gate` (`release_gate.rs`, six checks, promoted test helpers,
   `release()`, hooks in both routes, CLI exit 3, sample generator, source-grep tests; 8.5).
9. `test(server): PDF text smoke tests` (8.6).
10. `feat(cli): camerata release-gate <zip>` behind the `pdf-text` feature, plus the acceptance test
    file (8.7). Run the acceptance by hand; record the before/after in `docs/OVERNIGHT_DECISIONS.md`.
11. `docs: OVERVIEW crate map, TECHNICAL (new section "Secret redaction and the release gate"),
    the `rules/principles/universal/sec-no-vendor-token-1.toml` note that items (b) and (c) are now
    built` (bump the rule's `version` and regenerate `CORPUS_LOCK.toml` only if the rule text
    changes).

Each commit leaves `cargo clippy -D warnings`, fmt and the full suite green; `unwrap_used` is
denied workspace-wide, so the new crate uses `expect` only inside `OnceLock` initialisers with a
"must compile" message, as the gateway does.

## 10. The known leaks, confirmed (redacted; fields, files and locations only)

Scanned with a throwaway script over the sweep5b exports (and the 2026-10-18 hearth rerun, which
agrees with sweep5b). Column letters: `T` = Snippet, `U` = Detail, `V` = Fix (specific to this
finding) on the "All Findings" sheet.

**faktura** (`camerata-inspection-faktura-5225b49`): one real Supabase secret key,
`sb_secret_…WvXy (51 chars)`, in
- `findings.json`: `findings[0].snippet`, `findings[0].fix_specific`,
  `findings[0].also_locations[1].snippet` (row: `SEC-NO-VENDOR-TOKEN-1`, `.env:3`, critical) and
  `findings[43].fix_specific` (row: `SEC-NO-HARDCODED-SECRETS-1`, `.env:2`, low; its own `snippet`
  carries the `sb_publishable_…GdEa (46 chars)` public key, which stays readable by design);
- xlsx: `All Findings!T2`, `V2`, `V45`; `Universal!T2`, `V2`, `V8`;
- PDF: pages 11 and 57 (text extraction, including the newline-joined variant).
  Page 40 carries a `SUPABASE_SERVICE_ROLE_KEY` NAME literal that today's heuristic calls a
  secret; twin T2.
- README clean.

**hearth** (`camerata-inspection-hearth-a2b3859`), all on the one merged row `findings[1]`
(`SEC-NO-VENDOR-TOKEN-1`, `.env.production:4`, critical):
- `snippet` and `fix_specific`: `sb_secret_…gJ2k (42 chars)`;
- `also_locations[*].snippet`: two distinct Stripe live keys `sk_live_51…3vTq (77 chars)` and
  `sk_live_51…TkMv (77 chars)` (at `.env.production:6` and `.env.local.example:5`), a webhook
  secret `whsec_7Tn2…Ue5A (38 chars)` (`.env.production:7`), the live publishable key
  `pk_live_51…zLu9 (70 chars)` (`.env.production:5`; public, readable), and Stripe's documented
  test key `sk_test_4e…p7dc (32 chars)` (`tests/fixtures/session.json:2`; twin T3). Three further
  entries repeat the Supabase and Stripe keys from `.camerata-scan-cache.json:1965/1990/2015`,
  the engine's own old cache that was then scanned (E3);
- xlsx: `All Findings!T3`, `V3`; `Universal!T3`, `V3` (the "Also found at" column did not yet
  exist in this export);
- PDF: page 17 (the Supabase key; `also_locations` do not render in the PDF).
  So hearth carries FOUR real secret values (one Supabase, two Stripe live, one webhook), not three.

**supabase-portal-fixture** (`camerata-inspection-supabase-portal-fixture-4fa19cc`): two
secret-SHAPED values, both fixture placeholders:
- `findings[0]` (`SEC-NO-VENDOR-TOKEN-1`, `.env.production:6`) and `findings[5]`
  (`SUPABASE-KEY-SERVICE-ROLE-CLIENT-1`, same line, AI tier): `snippet` and `fix_specific` carry
  `sb_secret_` + a 15-char all-caps body containing `DO_NOT` and `FAKE` (`sb_secret_…_USE
  (25 chars)`); `findings[0].also_locations[1].snippet` (`.env.example:6`) carries
  `sb_secret_` + a `your…here` body (36 chars); `findings[5].detail` contains the prose
  `sb_secret_-prefixed` (19 chars; the hyphen-class artefact, not a value);
- xlsx: `All Findings!T2`, `V2`, `T7`, `U7`; `Universal!T2`, `V2`; `Supabase Secrets!T2`, `U2`;
  PDF pages 12 and 20.
  Under the twin rules (T1) these stay readable and the gate passes portal clean; see Q2.

In all three packages the fix text instructs with the literal value ("replace `sb_secret_…`"),
which is work order §3, closed by B1.

## 11. Owner questions (only the ones that are genuinely his)

**Q1. Stripe test-mode secret keys (`sk_test_…`, `rk_test_…`).** A test-mode key cannot move
real money, but it can read a merchant's test customers and create test charges, and the work
order's class rule says "any value matching a secret shape". The work order also says
"published test keys stay readable", which the design satisfies by recognising Stripe's own
documented example key (twin T3). Default built: a real-looking `sk_test_` value is redacted like
a live key. Do you want test-mode keys left readable instead?

**Q2. The portal fixture's placeholders.** The two "secrets" in the portal export are fixture
placeholders (an all-caps body containing `DO_NOT` and `FAKE`; a `.env.example` value ending in
`here`). The twin rule the work order asks for leaves placeholders readable, so the acceptance
result for portal is "clean, placeholders readable", not "two leaks flagged". Confirm that is the
intended reading, or say that any `sb_secret_`-prefixed value should be redacted regardless of
its body (then T1 would not apply to vendor-prefixed shapes, only to the generic ones).

No other question. The JWT-with-no-role decision (redact everywhere, never a finding), the
`.env.example`-filename decision (content-only redaction) and the leaf-crate placement are
decided above with their reasons.
