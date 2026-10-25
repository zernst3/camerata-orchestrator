# Firebase / Firestore rules: the build-ready design (2026-10-25)

Written against branch `feat/audit-report-export`, after reading the work order
(`_workspace/brownfield-audits/WORK_ORDER_FIREBASE_RULES.md` and its addendum), MASTER-SEQUENCE item
24, the W29 detector design (the ONE-IMPORT-PATH, DEAD-CODE and SHARED-CONSTANT sections and their
review rounds), `docs/RULE_AUTHORING.md`, the Supabase rulepack and its checkers
(`crates/checks/src/supabase/`), `onboard/{files,propose,architectural,audit,count}.rs`,
`arch_checker.rs`, `crates/secrets/`, and Firebase's own documentation (every URL in section 13 was
fetched on 2026-10-25 and says what this document claims). Build-ready: every "decide" below is
decided, and the owner questions in section 12 are the only ones left.

The governing law is the resolver's: **false-negative, never false-positive.** A client reads each
finding as a fact they can check by opening the named file at the named line. Where a rule cannot
state such a fact deterministically, the finding is review-graded (held, not asserted) or left to the
advisory tier, and this document says which.

## 0. What the investigation found (summary)

- **Nothing Firebase reaches a check today.** `files.rs` CODE_EXTS has no `rules` extension and no
  `dart`, so `firestore.rules`, `storage.rules` and every Flutter source file are dropped before any
  consumer runs. `firebase.json`, `.firebaserc` (a dotfile) and `database.rules.json` are already
  admitted. Section 2 is the blocking fix (24a).
- **Every Firebase app would get a false critical.** The secrets floor runs
  `SEC-NO-VENDOR-TOKEN-1` and `SEC-NO-HARDCODED-SECRETS-1` over every admitted file through
  `camerata_secrets::match_lines_for_rule(content, rule, twins_count = true)`, and the `GoogleApiKey`
  shape (`\bAIza[0-9A-Za-z_-]{35}\b`) has no twin. The Firebase web config `apiKey` is an `AIza` key.
  Firebase's own page says keys restricted to Firebase services "do not need to be treated as
  secrets, and it's safe to include them in your code or configuration files." Section 3 adds the
  twin at the one place every consumer shares (`twins::classify`), so the gate, the floor, the chat
  scrubber and the release gate all agree.
- **No Firebase stack detection, no `firebase` domain.** `detect_frameworks` has no Firebase marker,
  `FRAMEWORK_DOMAINS` no row, `domain_to_glob` no arm. A rule in a new `firebase` folder would fail
  `req_h1_every_corpus_domain_is_emittable_by_some_stack` until section 4 lands.
- **The rules language has no grammar in the repo.** Firestore and Storage rules are a CEL superset
  (Firebase: Storage "leverages a superset of the Common Expression Language (CEL)"); Realtime
  Database rules are JSON with JavaScript-like string expressions. Section 5 specs a small
  block/statement reader, in the `supabase/splitter.rs` hardening tradition (a `Vec<char>` scanner,
  never a byte-index panic, best effort to EOF), with one difference the law requires: an input the
  reader cannot account for degrades to **not assessed**, never to clean.
- **Thirteen rules: eight carry deterministic facts, five are review-graded or advisory.** The exact
  split is the table in section 7.0. The one new seam the deterministic tier needs is a per-violation
  grade on `ArchViolation` (defect, review, not-assessed), so a public-read grant can be stated as a
  fact and held for review instead of asserted as a defect (section 9.1).
- **Two deterministic facts this work order did not ask for but the docs make precise:** an
  `update` rule that checks ownership only on the incoming document (`request.resource.data`) lets a
  caller reassign ownership (OWNERSHIP's deterministic edge, section 7.4), and a role read from a
  document the same user may write is a self-grant (ADMIN-CLAIMS's deterministic edge, section 7.9).

## 1. What already EXISTS (reuse, do not rebuild)

- **The checker seam.** `ArchChecker { rule_ids, interest_globs, check(&RepoView), advisory_coexisting,
  runs_at_write_time, config_unsatisfied_for }` in `crates/checks/src/arch_checker.rs`, registered in
  `all_checkers()`, adapted to `Finding` by `onboard/architectural.rs::arch_violation_to_finding`
  (always `kind = Defect`, `preview = true`, redacted). `checker_applies` skips a checker with zero
  interest files; `config_unsatisfied_for` keeps a rule's ids in the advisory prompt and out of the
  ledger credit for a repo the checker cannot answer. `RULES_NEEDING_COMPLETE_SCAN` withholds
  absence rules on a truncated scan. All of this is reused as is; section 9.1 adds one field.
- **The Supabase DSL precedent.** `supabase/splitter.rs` (char scanner, comments, strings, never
  panics), `supabase/sql_parse.rs` (shallow classifier over split statements), `supabase/config.rs`
  (`parse_exposed_schemas` falls back to a default, never errors), `rls_checker.rs` (the honesty
  caveat "this reflects the migration history in this repository only"). The Firebase reader follows
  the same shape with the degradation contract in section 5.5.
- **The ECMA extractors.** `extract::{imports, functions, method_calls, literals}` over TS/TSX/JS via
  tree-sitter (`extract/ecma.rs`), `extract::paths::is_test_path`, the import `resolver`. Cloud
  Functions are TS/JS, so rules 10 and 11 need no new grammar.
- **The shared secret detector.** `crates/secrets`: `SecretShape::GoogleApiKey` (S9), `Twin` with
  `counts_as_line`, `twins::classify`, `detect::push`, runtime fixtures (`fixtures::planted`, `body`),
  the twin test idiom (`tests/twins.rs::assert_twin`). The gate's `deny_on_secret_shape` and the
  floor's `content_match_lines` both read it, so one twin fixes both.
- **Stack detection and proposal.** `propose.rs::detect_frameworks` (manifest basename plus path
  markers), `LANGUAGE_DOMAINS` / `FRAMEWORK_DOMAINS` / `domains_for_stack`, the H1 gate and the A6
  witness table (`FRAMEWORK_WITNESSES`, the per-domain witness repos), `domain_to_glob` in
  `crates/rules/src/lib.rs`.
- **`camerata count`** (`onboard/count.rs`) reads the same filter and `lang_for_ext`; Dart appears in
  it the moment `lang_for_ext` knows the extension.
- **Generated-Dart exclusions** already exist: `.g.dart`, `.freezed.dart`, `.gen.dart` in
  `NOISE_SUFFIXES`; `.dart_tool` in `NOISE_DIRS`; `build/` and `Pods/` are already noise.

## 2. File admission (24a, blocking, first)

### 2.1 Exactly what changes in `crates/server/src/onboard/files.rs`

1. **`CODE_EXTS` gains `"rules"` and `"dart"`.** Both are audited code (full treatment: the secrets
   floor, stack detection, the architectural tier, the AI digest). `rules` goes in CODE_EXTS and not
   CONFIG_TEXT_EXTS because the architectural checkers must see it as an audited file; the two lists
   admit identically into `ExtractedRepo.files`, so this is a statement of intent, not a behaviour
   difference. Comment to add beside `rules`: "Firebase Security Rules (`firestore.rules`,
   `storage.rules`, and any `.rules` file `firebase.json` names). Other tools also use `.rules`
   (udev, Prometheus alerting, Snort); those are admitted as config text and ignored by the Firebase
   checker, which recognises a Firebase rules file by its `service` header, never by its name."
2. **Why the extension and not the two basenames.** Firebase lets `firebase.json` point at any
   file (`"rules": "rules/firestore.prod.rules"`, one per named database or bucket). Basename
   admission would drop exactly the renamed files, and the finding would read "clean". The
   extension admits them all; the checker (section 5.3) decides which are Firebase files.
3. **Generated Dart.** Keep the existing three suffixes and add the build_runner outputs that carry a
   fixed suffix and no hand-written content: `.mocks.dart` (mockito), `.gr.dart` (auto_route),
   `.pb.dart`, `.pbenum.dart`, `.pbjson.dart`, `.pbserver.dart` (protobuf). Do NOT add `.config.dart`
   (injectable generates it, but a hand-written `env.config.dart` is common; excluding it is a
   false negative for the secrets floor). `lib/firebase_options.dart` (FlutterFire CLI output,
   header `// File generated by FlutterFire CLI.`) has no suffix match and MUST stay admitted: it is
   where a Flutter app's `apiKey` lives and where the twin in section 3 must fire.
4. **`lang_for_ext` (propose.rs) gains `"dart" => "Dart"`.** Without it Dart lines land in the
   "other (config)" bucket of `camerata count` and in no language row of the inspection's volume
   figures, which misprices a Flutter repo. With it, `detect_stack` reports `Dart` as a language;
   `LANGUAGE_DOMAINS` has no Dart row, so `domains_for_stack` gives it `DEFAULT_LANGUAGE_DOMAINS`
   (`api-layer`, which implies `permissions` and `integration`), exactly as Kotlin and Swift get
   today. Recorded, not changed (section 12 notes it; it is not a blocking question).
5. **Nothing changes in the reference tier.** `.rules` is not reference text and Dart is code; the
   dead-code rule does not read Dart (no resolver dialect), so no `REFERENCE_TEXT_EXTS` change.
6. **Noise.** Nothing to add: Flutter's `build/`, `.dart_tool/`, `ios/Pods/`, `android/.gradle/`
   are already in `NOISE_DIRS`. `.firebase/` (hosting cache written by `firebase deploy`) is NOT
   pruned today and is a dotdir admitted as dotfiles. Add `".firebase"` to `NOISE_DIRS`: it holds
   `hosting.<hash>.cache`, a generated manifest, never source. Test it.
7. **`is_admissible_text`** needs no change: `.firebaserc` is a dotfile (admitted), `firebase.json`,
   `database.rules.json`, `firestore.indexes.json`, `google-services.json` are `.json`,
   `GoogleService-Info.plist` is `.plist` and is NOT admitted today. Add `"plist"` to
   `CONFIG_TEXT_EXTS`: it is human-readable XML config, it carries the iOS Firebase `API_KEY`
   (which the twin must see to keep readable) and real committed credentials do land in plists.

### 2.2 The CLI `count` and the inspection figures

`count_repo` changes behaviour only through the two tables: a Flutter repo now shows a `Dart` row
(files and non-blank lines), and `.rules` files count under `other (config)` (no `lang_for_ext`
mapping, deliberately: a rules file is configuration, and pricing by lines of rules would be odd).
The `to_text` layout is unchanged. The pre-quote number a client sees therefore goes UP for Flutter
repos by exactly their Dart line count; the cockpit's volume figures move the same way, since both
read `count_source_lines`.

### 2.3 Tests (files.rs, propose.rs, count.rs), by name

- `firebase_rules_files_are_admitted_as_audited_code_in_a_non_git_tree` and
  `..._in_a_git_tree` (the existing pair pattern): fixture `firestore.rules`, `storage.rules`,
  `rules/firestore.prod.rules`, `database.rules.json`, `firebase.json`, `.firebaserc`,
  `src/main.ts`. Expect all seven in `paths(&extracted)`; none in `reference_paths`.
- `dart_sources_are_admitted_and_generated_dart_stays_noise` (both trees): `lib/main.dart`,
  `lib/firebase_options.dart`, `lib/models/user.g.dart`, `lib/models/user.freezed.dart`,
  `lib/gen/assets.gen.dart`, `test/mocks.mocks.dart`, `lib/router.gr.dart`, `lib/proto/a.pb.dart`,
  `lib/env.config.dart`. Expect `main.dart`, `firebase_options.dart`, `env.config.dart` admitted;
  the rest counted in `excluded_noise_paths` (not reference text: GENERATED_NOISE_SUFFIXES is
  unchanged, so they are dropped, not handed to the architectural tier).
- `the_firebase_hosting_cache_folder_is_noise`: `.firebase/hosting.abc.cache` excluded;
  `.firebaserc` beside it admitted.
- `plist_config_is_admitted_as_config_text`: `ios/Runner/GoogleService-Info.plist` admitted.
- `a_rules_file_from_another_tool_is_admitted_as_config_text`: `etc/udev/70-usb.rules` and
  `monitoring/alerts.rules` admitted (and, in the checker tests of section 5, ignored).
- `has_code_ext_covers_rules_and_dart` in `onboard.rs`'s existing `has_code_ext` test block.
- `lang_for_ext_maps_dart` (propose.rs) and `count_reports_dart_as_a_language_and_rules_files_as_other`
  (count.rs, extending `fixture()` with `lib/app.dart` (3 lines) and `firestore.rules` (4 lines):
  expect a `Dart` row with files 1 / lines 3 and `other_lines` up by 4).
- `a_secret_planted_in_a_dart_file_is_now_caught_by_the_secrets_floor` (server e2e, sibling of the
  existing `bin` script test): a runtime-built Stripe live key in `lib/config.dart` produces a
  `SEC-NO-VENDOR-TOKEN-1` row. This is the test that proves 24a changed something for Flutter.

## 3. The Firebase web `apiKey` safe twin

### 3.1 The decision

**A Google API key that is the `apiKey` of a Firebase app configuration is a safe twin: readable,
not a finding row, not a gate denial. A Google API key anywhere else keeps today's behaviour.**

Grounding: Firebase, "Learn about using and managing API keys for Firebase": API keys for Firebase
services "only identify your Firebase project and app to those services", are "not used to control
access to backend resources", and "API keys restricted to Firebase services do not need to be
treated as secrets, and it's safe to include them in your code or configuration files." The same
page says that for other Google APIs ("the Places API for Maps or the Gemini Developer API") you
should "use a separate API key", that all keys need "appropriate API restrictions and limits", and
that the Gemini Developer API key "should never be included in your code or configuration files."

Why the second half stays flagged: a bare `AIza` key with no Firebase context is, as far as the
repository shows, a Google Cloud key. It may be a referrer-restricted Maps browser key (public by
design) or an unrestricted server key with billing attached (a real exposure). The repository
cannot show the restriction, so the honest statement is "a Google API key is committed here; its
restrictions are not visible in the repository", which is what today's row says in effect. Whether
that row should stay critical or move to a review grade is a product change to an existing floor
rule, and it is the owner's call (section 12, Q1). V1 changes nothing for it.

### 3.2 Recognition (content only, no path)

`twins::classify` receives `before_value` (empty for a whole-span vendor shape) and no wider text,
so the Firebase shape cannot be recognised there today. Change `detect::push` to also pass a
**context window**: `text[win_start..win_end]` where `win_start = floor_boundary(text,
span.start.saturating_sub(WINDOW))`, `win_end = floor_boundary(text, span.end + WINDOW)`,
`WINDOW = 800` bytes (a Firebase config object is 6 to 8 short lines; a `google-services.json`
`client` block fits). The window and the match's own line prefix (text from the previous `\n` to
`span.start`, at most 200 bytes) are the only inputs. `classify(shape, value, before_value, role,
context: TwinContext { line_prefix, window })`; every existing caller passes the computed context.

`Twin::FirebaseWebApiKey` is returned for `SecretShape::GoogleApiKey` only, when EITHER:

- **(a) the key is the value of a Firebase-config key name AND the window carries Firebase sibling
  keys.** `line_prefix` matches, case-sensitively,
  `(apiKey|api_key|current_key|API_KEY)\s*["']?\s*[:=]\s*["']?$` (JS object `apiKey: "`, JSON
  `"apiKey": "`, Dart named argument `apiKey: '`, `google-services.json` `"current_key": "`, a
  `.env`-style `API_KEY=`), or the plist form `<key>API_KEY</key>\s*<string>$`; AND the window
  contains at least TWO distinct members of the sibling set `authDomain projectId storageBucket
  messagingSenderId appId measurementId databaseURL project_id storage_bucket mobilesdk_app_id
  gcm_defaultSenderId firebase_url GOOGLE_APP_ID GCM_SENDER_ID PROJECT_ID STORAGE_BUCKET
  BUNDLE_ID`, matched as whole words.
- **(b) the key is the value of an identifier whose name contains `FIREBASE`.** `line_prefix`
  matches `[A-Za-z0-9_]*FIREBASE[A-Za-z0-9_]*\s*["']?\s*[:=]\s*["']?$` (`VITE_FIREBASE_API_KEY=`,
  `NEXT_PUBLIC_FIREBASE_API_KEY=`, `REACT_APP_FIREBASE_API_KEY`, `EXPO_PUBLIC_FIREBASE_API_KEY`,
  `"FIREBASE_API_KEY": "` in an Expo `app.json`, `firebaseApiKey:`). No sibling requirement: the
  name is the context.
- **(c) the window carries a Firebase initialisation call** (`initializeApp(`, `Firebase.initializeApp(`,
  `FirebaseOptions(`, `firebaseConfig`) AND the line prefix is a key-value position
  (`[A-Za-z_][A-Za-z0-9_]*\s*["']?\s*[:=]\s*["']?$`) AND at least ONE sibling key is in the window.
  This covers a config spread across lines with the init call beside it.

`Twin::counts_as_line` returns `false` for the new variant (no floor row, no gate denial, no
release-gate failure); `detect_secrets` excludes it, so the redactor leaves it readable (a client
report may show the real `apiKey`, which Firebase says is fine). The twin is NOT applied to any
other shape: a Stripe key inside a Firebase config object is still a secret.

### 3.3 Traps and guards

- **A Maps key named `apiKey` in a Google Maps loader** (`new Loader({ apiKey: "AIza..." })`,
  `googleMapsApiKey: "AIza..."`): (a) needs two Firebase sibling keys; a Maps loader has `libraries`,
  `version`, not `authDomain`/`projectId`. Stays a finding.
- **A Firebase config and a Maps key in one file** (common: `config.ts` with both): the Maps key's
  window may contain the Firebase siblings. Guard: in (a) and (c) the sibling keys must appear
  AFTER the nearest preceding `{` or `(` that precedes the match and BEFORE the matching `}`/`)`,
  approximated as: siblings counted only within the smallest `{...}` or `(...)` span in the window
  that contains the match (brace-balanced scan over the window, strings skipped). A Maps key sitting
  in its own object does not inherit the Firebase object's siblings. Test
  `not_a_twin_maps_key_in_its_own_object_beside_a_firebase_config_is_a_secret`.
- **An env file with both `VITE_FIREBASE_API_KEY=AIza...` and `VITE_GOOGLE_MAPS_KEY=AIza...`**: (b)
  keys on the identifier of the match's own line only. The Maps line stays a finding.
- **Server-side Firebase Admin config** never carries `apiKey` (it uses a service account), so no
  admin credential is hidden by this twin; the PEM shape still catches the private key.
- **A placeholder `AIzaSyXXXXXXXX...`**: already T1 (`xxxx`); unchanged.
- **Minified bundles** (`dist/*.js`): noise-pruned before the floor; unchanged.
- **The key appears again as a bare literal elsewhere** (a test asserting the config): no context,
  so it is a finding. Correct: the twin claims only what it can see.

### 3.4 Tests, by name (crates/secrets/src/tests/twins.rs unless stated)

Every `AIza` value is built at runtime: `format!("AIza{}", body(35, URL_SAFE, seed))`.

- `twin_firebase_web_config_api_key_in_a_js_object_stays_readable`: `const firebaseConfig = {
  apiKey: "<k>", authDomain: "x.firebaseapp.com", projectId: "x", appId: "1:2:web:3" };` expect
  one match, `Twin::FirebaseWebApiKey`, `detect_secrets` empty, `redact(text).text == text`.
- `twin_firebase_api_key_inside_initialize_app_call_stays_readable`: `initializeApp({ apiKey: "<k>",
  projectId: "x", messagingSenderId: "1" })`.
- `twin_firebase_api_key_in_firebase_options_dart_stays_readable`: `static const FirebaseOptions web
  = FirebaseOptions(apiKey: '<k>', appId: '...', messagingSenderId: '...', projectId: 'x');`.
- `twin_firebase_api_key_in_google_services_json_stays_readable`: the `api_key: [{ "current_key":
  "<k>" }]` block with `project_id` and `mobilesdk_app_id` in the window.
- `twin_firebase_api_key_in_google_service_info_plist_stays_readable`: `<key>API_KEY</key>
  <string><k></string>` with `GOOGLE_APP_ID` and `PROJECT_ID` keys.
- `twin_firebase_named_env_var_stays_readable`: `VITE_FIREBASE_API_KEY=<k>` and
  `"EXPO_PUBLIC_FIREBASE_API_KEY": "<k>"`.
- `twin_firebase_api_key_is_not_a_floor_row`: `match_lines_for_rule(text, "SEC-NO-VENDOR-TOKEN-1",
  true)` and `..."SEC-NO-HARDCODED-SECRETS-1"` both empty on the JS object fixture.
- `not_a_twin_google_key_without_firebase_context_is_a_secret`: `const key = "<k>";`.
- `not_a_twin_maps_key_in_its_own_object_beside_a_firebase_config_is_a_secret`: the two-object
  file; exactly the Maps match is a secret.
- `not_a_twin_env_maps_key_beside_a_firebase_env_key_is_a_secret`.
- `not_a_twin_firebase_siblings_outside_the_window_do_not_count`: siblings 2,000 bytes away.
- `not_a_twin_stripe_key_inside_a_firebase_config_object_is_still_a_secret`.
- Gateway (`crates/gateway/src/lib.rs` tests): `sec_no_vendor_token_1_does_not_deny_a_firebase_web_config`
  and `sec_no_hardcoded_secrets_1_does_not_deny_a_firebase_web_config`.
- Server (`onboard/audit.rs` tests): `a_firebase_web_config_in_client_code_produces_no_floor_finding`
  over `src/firebase.ts`, `lib/firebase_options.dart`, `android/app/google-services.json`,
  `ios/Runner/GoogleService-Info.plist`, `.env` through `audit_files`: zero `SEC-*` rows.
- Secrets `tests/source_hygiene.rs` keeps passing: no `AIza` literal 39 chars long appears in source
  (the new regexes mention `AIza` only as a four-character prefix inside fixtures built at runtime).

## 4. Stack detection and the `firebase` domain

### 4.1 `detect_frameworks` (propose.rs): the `Firebase` label

Add, in the `package.json` arm: `"firebase"` (the client SDK key, matched as `"\"firebase\":"` so
`"firebase-admin"` and `"firebase-functions"` are matched by their own clauses), `"firebase-admin"`,
`"firebase-functions"`, `"@react-native-firebase/`, `"@angular/fire"`, `"reactfire"`, `"firebaseui"`,
`"@firebase/`. Add a new manifest arm `"pubspec.yaml"`: `firebase_core`, `cloud_firestore`,
`firebase_auth`, `firebase_storage`, `firebase_database`, `cloud_functions`. Add path/basename
markers (any depth, like the Supabase markers): basename `firebase.json`, `.firebaserc`,
`firestore.rules`, `storage.rules`, `database.rules.json`, `firestore.indexes.json`,
`google-services.json`, `GoogleService-Info.plist`, `firebase_options.dart`; and any `.rules` file
whose content contains `service cloud.firestore` or `service firebase.storage` (content check, so
a udev file never trips it). Every clause adds the one label `Firebase`.

### 4.2 Domain and tables

- One flat domain, **`firebase`**, folder `crates/rules/principles/firebase/`, all 13 rules in it.
  (The Supabase pack uses `supabase:<area>` leaves because it has 35 rules in seven areas; 13 rules
  do not need the leaf wiring, and `propose_corpus_rules` matches the exact domain string either
  way.)
- `FRAMEWORK_DOMAINS`: `("Firebase", &["firebase"])`. The Functions rules reach JS/TS repos only
  through this row; they carry no `extra_domains`, so a plain Express app never sees them.
- `FRAMEWORK_WITNESSES`: `("Firebase", "firebase.json", r#"{"firestore":{"rules":"firestore.rules"}}"#)`.
- The per-domain witness table (the `req_a6_every_corpus_domain_has_a_detectable_witness_repo`
  list): `("firebase", &[("firebase.json", "{...}")])`.
- `domain_to_glob` (rules/lib.rs): `"firebase" => "**/*.{rules,json,js,ts,jsx,tsx,mjs,cjs,dart}"`,
  with the existing `**` catch-all appended by `derive_allowed_paths`. Add the arm to
  `derive_allowed_paths_maps_known_domains`.
- `emittable_domains` derives from the tables, so the H1 gate passes once the row exists.

### 4.3 Tests, by name (propose.rs)

- `firebase_is_detected_from_each_marker`: one `detect_frameworks` call per marker above (manifest
  content and path markers), each producing `Firebase`; a `package.json` with only `"express"` and a
  `monitoring/alerts.rules` with Prometheus content produce nothing.
- `firebase_rules_are_proposed_for_a_firebase_repo_and_not_for_a_plain_node_api`: the W29
  proposed-rules test shape (`propose_corpus_rules` over `domains_for_stack(detect_stack(...))`):
  a repo with `firebase.json` + `src/index.ts` gets all 13 ids with `recommended` true; a repo with
  `package.json` `{"express":"4"}` + `src/index.ts` gets none.
- `a_flutter_firebase_repo_is_detected_as_dart_plus_firebase`: `pubspec.yaml` with `firebase_core`,
  `lib/main.dart`: languages contain `Dart`, frameworks contain `Firebase`.

## 5. The rule-file model and reader

New module `crates/checks/src/firebase/` with `mod.rs` (the checkers, section 7), `rules_parse.rs`
(the DSL reader), `rules_model.rs` (the resolved model and recognisers), `rtdb.rs` (JSON rules),
`config.rs` (`firebase.json` resolution), `functions.rs` (section 8), `tests/`.

### 5.1 The Firestore / Storage DSL reader (`rules_parse.rs`)

**Hardening contract, copied from `supabase/splitter.rs`:** the reader runs over untrusted,
hand-edited, possibly truncated files; it must never panic; every loop advances; it operates on a
`Vec<char>`; an unterminated construct runs to EOF. One addition: the reader records every place it
could not account for the input (`errors: Vec<ParseError { line, what }>`), and a file with any
error is **degraded** (section 5.5).

Lexical layer, in one left-to-right pass:
- `//` line comments and `/* */` block comments (the docs show `//`; `/* */` is accepted
  defensively so a file using it does not look unparseable). Comments are dropped; line numbers
  are kept.
- Strings: `'...'` and `"..."` with backslash escapes (`\'`, `\"`, `\\`, `\n`, `\t`, `\uXXXX`
  consumed as two or more chars without interpretation). An unterminated string runs to EOF and
  records an error. String contents are kept verbatim in conditions and paths (a `matches('...')`
  regex may contain `{`, `;` or `/`).
- Braces `{ }`, parentheses `( )`, brackets `[ ]` are tracked as depth counters outside strings and
  comments. Statements end at `;` at the current block depth; blocks open at `{` and close at `}`.
- A `}` at depth 0, or EOF with depth greater than 0, records an error.

Statement layer (over the token text between terminators):
- `rules_version = '2';` (or `"2"`) sets `version = Some(2)`; absent means `v1` (Firebase: "If no
  `rules_version` statement is supplied, your rules will be evaluated using the `v1` engine").
- `service <dotted-ident> {` opens the service block: `cloud.firestore` becomes `Service::Firestore`,
  `firebase.storage` (and the legacy `cloud.storage` the Storage docs mention)
  `Service::Storage`, anything else `Service::Other(name)` (the file is then NOT a Firebase rules
  file for the Firestore/Storage checkers; it is left alone with no error).
- `match <path> {` opens a match block. The path is split on `/` outside `{}` and `$()` into
  segments: `Literal(text)`, `Wildcard { name, recursive: bool }` (`{x}` / `{x=**}`), and
  `Interpolated(text)` for `$(...)`. A wildcard that is not a single `{...}` segment records an
  error. The effective path of a block is its ancestors' segments followed by its own (Firebase:
  "the path of the inner `match` statement is always relative to the path of the outer `match`
  statement").
- `allow <methods>[: if <expr>];` adds `Allow { methods: BTreeSet<Method>, condition:
  Option<String>, line }` to the innermost block. Methods: `read write get list create update
  delete` (anything else records an error). `condition = None` means unconditional (Firebase: a
  missing condition "means the request is always allowed").
- `function name(params) { ... }` anywhere (at service level or inside a match) records
  `FnDef { name, params, lets: Vec<(name, expr)>, ret: Option<String>, line }` by reading `let x =
  expr;` statements and the single `return expr;`. A function with no `return`, more than one, or
  statements it does not understand records an error.
- Any other statement (`deny`, a bare identifier, a stray `if`) records an error with the
  statement's first line.

Output: `RulesFile { version, service, root: Vec<MatchBlock>, functions: Vec<FnDef>, errors }` where
`MatchBlock { segments, effective_path, allows, functions, children, line }`.

### 5.2 Realtime Database rules (`rtdb.rs`)

`database.rules.json` (or whatever `firebase.json` names) is parsed with `serde_json`. The tree
under the top-level `"rules"` key is walked; at each node the keys `.read`, `.write`, `.validate`,
`.indexOn` are rule keys and every other key is a child path segment (`$var` keys are wildcards).
A `.read`/`.write` value is `Bool(true|false)` or a `String(expr)`; a string is classified by the
same recognisers as a Firestore condition after RTDB normalisation (`auth` in place of
`request.auth`, `!==` and `===` treated as `!=` and `==`). Cascade is modelled (Firebase: ".read
and .write rules cascade" and a shallower grant "takes precedence"): a node's effective read grant
is the OR of its own and every ancestor's. A file that is not a JSON object with a `rules` key, or
whose JSON does not parse, is degraded. A `.json` file that is not named by `firebase.json` and is
not `database.rules.json` is never read as RTDB rules.

### 5.3 Which files are assessed (`config.rs`)

This is the guard against the "multiple rules files" and "non-default path" traps.

1. Collect every `firebase.json` in `files` (any depth: monorepos keep one per app). Parse each with
   `serde_json`; an unparseable one is recorded (the repo's Firebase config is degraded; findings
   still ship for files resolved by convention, each carrying the sentence "firebase.json could not
   be read, so the deployed rules file could not be confirmed").
2. From each, resolve the **named rules files**, relative to the directory of that `firebase.json`:
   `firestore.rules` (string) or `firestore[].rules`; `storage.rules` or `storage[].rules`;
   `database.rules` or `database[].rules`. Each resolved path is tagged with its service, its
   `firebase.json`, and the `database` / `bucket` / `target` / `instance` label when present (the
   finding names it: "the rules for database `analytics`").
3. **Conventional files**: for a directory holding a `firebase.json` that names NO file for a
   service, the basenames `firestore.rules`, `storage.rules`, `database.rules.json` beside it are
   the defaults (`firebase init` creates exactly these). A repository with no `firebase.json` at
   all assesses conventional basenames anywhere, outside test paths.
4. **Every other `.rules` file** with a Firebase `service` header is **unreferenced**: it is parsed
   (so a parse error there still degrades honestly) but produces no rule findings. The repo-level
   report gets ONE informational row per unreferenced file: "`rules/firestore.dev.rules` is a
   Firestore rules file that `firebase.json` does not reference, so it was not assessed as the
   deployed ruleset." (Grade: not-assessed, section 9.1.) A dev or draft ruleset therefore never
   produces a defect against the deployed state.
5. **Test rules files** (`*.test.rules`, `*.spec.rules`, any path `is_test_path` or under
   `emulator/`, `emulators/`): excluded from step 3; included in step 2 if `firebase.json` names
   them (the named file is what deploys, whatever its folder).
6. A `.rules` file whose header is `service` something else, or a `.rules` file with no `service`
   at all (udev, Prometheus): not a Firebase rules file; silently ignored; no row.

### 5.4 Condition normalisation and the recognisers (`rules_model.rs`)

A condition string is normalised before any recognition: comments already gone; whitespace removed
outside strings; redundant outer parentheses stripped repeatedly; `!==`/`===` (RTDB) mapped to
`!=`/`==`. **Function inlining:** a call `f(args)` to a `FnDef` in scope (the match's own, an
ancestor's, or the service's) is replaced by the function's `ret` with `let`s substituted and
parameters substituted textually, to a depth of 5 (the language forbids recursion and caps call
depth at 20; 5 is enough for every real helper chain and keeps a malicious file bounded). A call
to an unknown function, or to a built-in (`get`, `exists`, `getAfter`, `debug`, `math.*`,
`duration.*`, `timestamp.*`, `string(...)`, `.matches(...)`, `.hasOnly(...)`), is left as is.

Recognisers (each returns a bool over the normalised, inlined string; anything not exactly matched
is "not recognised", which every rule treats as the safe direction):

- `is_true`: `None`, `true`, or a top-level disjunction with a `true` member (`x||true`).
- `is_false`: `false`, or a top-level conjunction with a `false` member.
- `is_auth_only`: the whole condition is one of `request.auth!=null`, `null!=request.auth`,
  `request.auth.uid!=null`, `request.auth.uid!=""`, `request.auth.uid!=''`,
  `request.auth.token!=null`, `request.auth!=null&&request.auth.uid!=null`, or (RTDB)
  `auth!=null`, `auth.uid!=null`, `auth.uid!=""`. ANY extra conjunct makes it not auth-only (a
  rule that also checks `request.auth.token.email_verified` is narrower than auth-only and is not
  the docs' "any authenticated user" shape). A disjunction of auth-only members is auth-only.
- `is_test_mode`: `request.time<timestamp.date(Y,M,D)` or `<=` (also `timestamp.value(ms)`),
  alone or as a disjunct; returns the parsed date.
- `references(cond, prefix)`: a whole-token occurrence of `request.resource.data`,
  `resource.data`, `request.auth.uid`, `request.auth.token`, a wildcard name, etc.
- `uid_compared_to(cond) -> Vec<Target>`: every `request.auth.uid==X` / `X==request.auth.uid` (and
  `in`-forms `request.auth.uid in X`) with `Target::Wildcard(name)`, `Target::Incoming(field)`
  (`request.resource.data.<f>`), `Target::Existing(field)` (`resource.data.<f>`), `Target::Other`.
- `validates_fields(cond)`: contains `request.resource.data.keys().hasOnly(` or `.hasAll(` or
  `.diff(resource.data).affectedKeys()` or at least one `request.resource.data.<f> is <type>`.
- `pins_field(cond, f)`: contains `request.resource.data.<f>==resource.data.<f>` or an
  `affectedKeys().hasOnly([...])` list that excludes `f`, or `!(... in request.resource.data)` for f.
- `role_lookup(cond) -> Option<(collection, key_expr, field, value)>`: a
  `get(/databases/$(database)/documents/<coll>/$(<key>)).data.<field>==<literal>` or
  `exists(/databases/$(database)/documents/<coll>/$(<key>))` where `<key>` is `request.auth.uid`.

### 5.5 The degradation contract

- A **degraded file** (any `ParseError`) produces no rule findings. It produces one not-assessed row
  (section 9.1): "`firestore.rules` could not be read as a Firebase rules file (line 41: a statement
  this reader does not understand), so the Firebase rules checks were not run on it." A client
  never reads "clean" over a file the reader did not understand.
- A repo whose Firebase file set (section 5.3, steps 2 and 3) is empty makes
  `config_unsatisfied_for` return `true`: the ids stay advisory and the ledger does not credit a
  run, so a Flutter app that keeps its rules only in the console is reported as "rules not in the
  repository" (a not-assessed row with that sentence), never as clean.
- Conditions the recognisers do not match are "not recognised", and every rule below says what
  that means for it (always: no finding).
- **Honesty caveat on every rules finding** (the RLS checker's precedent): "This reflects the rules
  file in the repository. Rules edited in the Firebase console never land in the repository, so
  confirm against the deployed rules before treating this as settled." The deploy docs state the
  inverse direction too: "the rules defined in your project directory overwrite any existing rules
  in the Firebase console", so the repository file is what the next deploy makes live.

### 5.6 Reader tests, by name (`firebase/tests/parse.rs`)

- `parses_the_firebase_init_default_firestore_template` (the firebase-tools template: comments,
  `rules_version='2'`, root recursive wildcard, `request.time < timestamp.date(2026, 11, 24)`).
- `parses_nested_and_flat_match_forms_to_the_same_effective_path` (`/cities/{city}` + nested
  `/landmarks/{landmark}` versus `/cities/{city}/landmarks/{landmark}`).
- `parses_functions_with_lets_and_inlines_them_into_conditions`.
- `string_contents_with_braces_semicolons_and_slashes_do_not_split_statements`
  (`allow read: if resource.data.tag.matches('a;{b}/c');`).
- `block_comments_and_line_comments_are_dropped_and_lines_are_kept`.
- `an_unterminated_string_or_block_degrades_the_file_and_never_panics` (truncated file; a file of
  binary noise; a file of 100,000 `{`; a file with `}` at depth 0).
- `an_unknown_statement_degrades_the_file` (`deny read;`).
- `a_service_other_than_firestore_or_storage_is_not_a_firebase_rules_file`.
- `a_udev_or_prometheus_rules_file_is_ignored_without_error`.
- `rtdb_rules_cascade_a_parent_grant_to_children` and
  `rtdb_rules_that_are_not_a_rules_object_degrade`.
- `firebase_json_names_rules_files_relative_to_its_own_directory` (monorepo: `apps/a/firebase.json`
  naming `rules/fs.rules`).
- `multi_database_and_multi_bucket_arrays_resolve_every_named_file`.
- `an_unreferenced_rules_file_is_parsed_but_produces_no_rule_findings`.
- `a_test_rules_file_is_skipped_unless_firebase_json_names_it`.
- `recognisers_match_exact_shapes_only` (table test: each auth-only spelling true; `request.auth
  != null && request.auth.token.email_verified` false; `isSignedIn()` inlined true; `|| true` is
  true; `timestamp.date(2027,1,1)` parses).

## 6. Shared false-positive guards

Numbered so the rule sections can cite them.

- **G1 Deployed-set only.** Deterministic findings come only from files `firebase.json` names or
  the conventional defaults (5.3). Unreferenced, draft, dev and test rules files produce only a
  not-assessed row.
- **G2 Test and emulator paths.** `is_test_path`, `*.test.rules`, `*.spec.rules`, `emulator(s)/`
  (5.3 step 5), unless named by `firebase.json`.
- **G3 Exact recognisers.** A condition is open, auth-only or test-mode only when the normalised,
  inlined string matches a listed form exactly. Any unrecognised conjunct means no finding.
- **G4 Helper functions are inlined** (5.4) before recognition, so `allow read: if isSignedIn();`
  is seen as auth-only and `allow read: if isOwner(userId);` whose body compares
  `request.auth.uid == userId` is seen as an ownership check, not as auth-only.
- **G5 Public vocabulary.** A read grant on a path whose effective segments include a literal in
  `public published static assets catalog faq announcements changelog legal locales` is never
  reported by the read-side rules (false negative by design; Firebase's own examples put public
  files under `/public/`).
- **G6 `if false` is a lock, not a grant.** A recursive wildcard carrying only `allow ...: if
  false` (the locked-mode default and the "closed access" pattern) is never a finding, and it does
  not "override" anything, because access is granted if ANY rule grants it and a false rule grants
  nothing.
- **G7 One owner per fact.** When RECURSIVE-WILDCARD fires on a broad allow, OPEN, TEST-MODE and
  AUTH-ONLY do not also fire on that allow (7.6). An AI-tier row landing on the same line is merged
  by the existing `merge_by_location`.
- **G8 Version semantics.** In `v1` a recursive wildcard matches one or more segments, in `v2`
  zero or more. The "covers everything" test for a root grant is: effective path is
  `/databases/{database}/documents/{x=**}` (Firestore) or `/b/{bucket}/o/{x=**}` (Storage), in
  either version (both cover every document that has a path); the v1/v2 difference only matters
  for the collection itself and is not relied on.
- **G9 Review grade for public reads.** A grant that lets anyone, or any signed-in user, READ a
  named path is a fact with a legitimate reading (public content, shared data) and ships
  review-graded (held), never as an asserted defect, except at the root (G10).
- **G10 Root is different.** A grant covering every document or every file has no "this
  collection is public" reading; Firebase labels the root open and root auth-only forms "Not
  recommended" and the open form "NEVER use this rule set in production". Root grants are asserted
  defects.
- **G11 Writes are different.** An unconditional write grant, or an auth-only write grant, on any
  path means anyone (or any account holder) can change or delete other people's data; Firebase's
  insecure-rules page offers no legitimate version. Asserted defects, with `create`-only open
  grants one step lower (an anonymous submission box is a real pattern that still needs
  validation).
- **G12 Complete scan for absence rules.** TESTS and APP-CHECK reason from absence and join
  `RULES_NEEDING_COMPLETE_SCAN`.
- **G13 Python and other Functions runtimes.** A `functions/` package whose manifest is
  `requirements.txt`/`pyproject.toml` with `firebase-functions` is reported as not assessed for
  rules 10 and 12 ("Cloud Functions written in Python are not inspected in this version"), never
  clean.

## 7. The rules

### 7.0 The split

| # | id | tier | deterministic fact | review / advisory part | grounding |
|---|----|------|--------------------|------------------------|-----------|
| 1 | FIREBASE-RULES-OPEN-1 | D | open write anywhere; open read at root | open read on a named collection (review) | grounded |
| 2 | FIREBASE-RULES-TEST-MODE-1 | D | the `request.time < timestamp.date()` scaffold, future or past | none | grounded |
| 3 | FIREBASE-RULES-AUTH-ONLY-1 | D | auth-only at root (Firestore, Storage, RTDB) | auth-only on a named path (review) | grounded |
| 4 | FIREBASE-RULES-OWNERSHIP-1 | D edge + J | update ownership checked on the incoming document only | per-user paths without an owner comparison (advisory) | grounded |
| 5 | FIREBASE-RULES-FIELD-VALIDATION-1 | D edge (review) | a write grant that never reads `request.resource.data` | which fields matter (advisory) | grounded |
| 6 | FIREBASE-RULES-RECURSIVE-WILDCARD-1 | D | a broad open / auth-only / test-mode grant that overrides narrower rules | broad non-trivial grants (advisory) | grounded |
| 7 | FIREBASE-STORAGE-RULES-OPEN-1 | D | open or auth-only write; auth-only read at root | open read on a non-public folder (review) | grounded |
| 8 | FIREBASE-RTDB-RULES-OPEN-1 | D | `.write: true` anywhere; `.read: true` at root; shadowed child denials | `.read: true` on a named path (review) | grounded |
| 9 | FIREBASE-ADMIN-CLAIMS-1 | D edge + J | role read from a document the same user may write | document-defined roles in general (advisory) | grounded |
| 10 | FIREBASE-FUNCTIONS-AUTH-1 | S (review) | an HTTP function that writes with the Admin SDK and never verifies a token; a callable that never reads `auth` and writes | none asserted | grounded |
| 11 | FIREBASE-ADMIN-SDK-CLIENT-1 | S | `firebase-admin` imported by browser-bundled code | none | grounded |
| 12 | FIREBASE-APP-CHECK-1 | A (review, low) | no App Check initialisation or enforcement anywhere | whether abuse matters | grounded |
| 13 | FIREBASE-RULES-TESTS-1 | A (low) | no rules unit-testing dependency | none | grounded |

D: a pass decides, no model. S: structural, over the ECMA extractors. J: advisory tier (the
semantic pass reads the rules files, which are in the digest once admitted). A: advisory, absence.
Every rule is `grounded`: each cites a Firebase page that states the convention (section 13). None
needs `policy`.

**Common corpus fields** for all thirteen (stated once): `version = 1`, `domain = "firebase"`,
`layer = "platform"`, `enforcement = "architectural"` for 1 to 11 (a detector ships) and
`"structured"` for 12 and 13 (absence checks whose main channel is the emitted convention),
`kind = "defect"`, `inspectable = true`, `default = true`, `verification = "grounded"`, two options
(the correct state and one `violates = true` state), `remediation` on the default option, `cwe` where
one fits (`CWE-284` improper access control for 1, 3, 6, 7, 8; `CWE-639` for 4; `CWE-915` mass
assignment for 5; `CWE-269` for 9; `CWE-306` missing authentication for 10; `CWE-200`/`CWE-798` for
11). Labels pass LABEL-1/2 (they name a state of the client's rules). No label, directive or message
contains a product name, `needs_review`, or a dash of any kind. The TOMLs land only under a recorded
owner go (the corpus is protected); detectors and tests may land first.

### 7.1 FIREBASE-RULES-OPEN-1

**Deterministic.** Over every assessed Firestore rules file (5.3), for every `allow` whose
condition `is_true` (G3, G4):

- **(a) Write side (G11).** Methods include `write`, `update` or `delete`: Defect, `high`; at the
  root recursive wildcard (G8): `critical`. Methods are `create` only: Defect, `medium`.
- **(b) Read side at root (G10).** Methods include `read`, `get` or `list` and the effective path
  is the root recursive wildcard: Defect, `high`.
- **(c) Read side on a named path (G9).** Review grade, `medium`, unless G5 applies (silent).
  Also silent when the same block (or an ancestor block covering the same path) carries the
  write-side finding (a) at the same severity or higher: the open-read fact is then a clause in
  the write finding's message, not a second row.

**The fact each finding states** (client voice): (a) "Line 7 of `firestore.rules` allows anyone on
the internet, signed in or not, to create, change and delete every document under `/orders/{id}`
(`allow write: if true`)." (b) "Line 5 of `firestore.rules` allows anyone to read every document in
the database (`match /{document=**} { allow read: if true; }`)." (c) "Line 12 of `firestore.rules`
allows anyone, signed in or not, to read every document in `/profiles/{uid}`. If profiles are meant
to be public, no change is needed; if they hold email addresses, phone numbers or anything a user
expects to be private, scope the read to the owner." Every message ends with the console caveat
(5.5).

**Traps and guards.** Deliberately public collections: G5 (silent) and G9 (held, named, asked, never
asserted). `allow read: if true` paired with owner-only writes (the docs' "mixed public and private
access" pattern) is exactly case (c): held, not a defect. Test and draft files: G1, G2. Locked
default `if false`: G6. A disjunction `request.auth != null || true`: `is_true` (a `|| true`
member makes the whole grant unconditional; this is a real bug shape). A condition the recogniser
does not match (`if resource.data.visibility == 'public'`): silent. Anything inside a
`service firebase.storage` file: not this rule (7.7).

**Tests** (`firebase/tests/open.rs`; helper `files(vec![...])` and `run(&f)` as in
`one_import_path_checker.rs`, with a fixed `firebase.json` naming `firestore.rules` unless stated):
- `open_write_on_a_collection_is_high`: `match /orders/{id} { allow read, write: if true; }`.
  Expect one `high` Defect, line of the allow, `object = Some("/orders/{id}")`, message contains
  `create, change and delete` and no second row for the read.
- `open_read_and_write_at_the_root_is_critical`: the docs' "open access" snippet. One `critical`.
- `open_create_only_is_medium`: `allow create: if true; allow read: if request.auth.uid == resource.data.uid;`.
- `open_read_at_the_root_is_high_and_asserted`: `match /{document=**} { allow read: if true; }`.
- `open_read_on_a_collection_is_review_graded_medium`: `match /profiles/{uid} { allow read: if true;
  allow write: if request.auth.uid == uid; }`. Expect grade Review.
- `or_true_makes_a_grant_unconditional`: `allow read: if request.auth != null || true;`.
- `twin_public_collection_read_is_silent`: `match /public/{doc} { allow read: if true; }` and
  `/catalog/{item}`.
- `twin_locked_mode_default_is_silent`: `allow read, write: if false;`.
- `twin_unrecognised_condition_is_silent`: `if resource.data.visibility == 'public'`.
- `twin_draft_and_test_rules_files_produce_no_defect`: `rules/firestore.dev.rules` and
  `test/firestore.test.rules` both wide open; `firebase.json` names `firestore.rules` (clean).
  Expect zero defects and one not-assessed row for the dev file.
- `twin_storage_file_is_not_this_rule`.
- `findings_are_sorted_and_stable`.

**TOML** `firebase/firebase-rules-open-1.toml`: `max_severity = "critical"`, `min_severity = "medium"`.
Options `no-unconditional-grant-on-private-data` (default; label "No Firestore rule grants read or
write access with an unconditional `true` condition except on paths whose contents are meant to be
public") and `unconditional-grant-on-user-data` (`violates = true`; label "A Firestore rule grants
access to user data with an unconditional `true` condition"; `why`: "Anyone holding the project id
can read, change or delete the documents under the path, with no sign-in. Firebase's own guide lists
`allow read, write: if true` under rules never to use in production."). `qualifies` states the
write/read split, the root rule, the public vocabulary, and that console-only rules are invisible.
Sources: "Avoid insecure rules" (open access), "Get started with Cloud Firestore Security Rules"
(the "Allow all" warning), "Security Rules structure" (overlapping matches).

### 7.2 FIREBASE-RULES-TEST-MODE-1

**Deterministic.** An `allow` whose condition `is_test_mode` (G3), in an assessed Firestore or
Storage file. Both dates are flagged, differently:

- **Future date** (date >= today): Defect; `critical` at the root recursive wildcard (that is the
  template: it allows "anyone with your database reference to view, edit, and delete all data"),
  `high` elsewhere. Message: "Line 9 of `firestore.rules` is the console's test-mode scaffold: it
  allows anyone to read and write every document until 24 November 2026 (`request.time <
  timestamp.date(2026, 11, 24)`). Firebase's template says this rule is for getting started and
  leaves the app open to attackers until it expires."
- **Past date** (date < today): Review grade, `medium`. Message: "Line 9 of `firestore.rules` is
  the console's test-mode scaffold with an expiry of 24 November 2025, which has passed. As
  written, every client request is now denied. If the app still works, the rules running in the
  console are not the rules in this repository; if the app does not, this is why. Either way, write
  real rules and deploy them." Justification for flagging both: the past-date form is still the
  scaffold shipped as the repository's rules, and it proves one of two defects (an app that is
  down, or a console-versus-repository drift). It cannot be asserted which, so it is held.

`today` is injected: `FirebaseRulesChecker { today: Option<NaiveDate> }`, `Default` reading the
system clock through `chrono` (already in the workspace lockfile via `api-types`); tests pin it.

**Traps.** A time clause with extra conjuncts (`request.time < timestamp.date(...) && request.auth
!= null`): not test mode (G3); the auth-only recogniser also fails because of the extra conjunct;
silent. A time clause used as a legitimate embargo (`allow read: if request.time >
timestamp.date(2026, 1, 1)`): the direction is `>`; not test mode. `timestamp.date` with
non-literal arguments: not recognised. The Storage template from `firebase init` is `if false`
(locked), so no Storage test-mode false positive arises from the CLI; the console's Storage test
mode uses the same `request.time` form and is caught. G1, G2, G7 apply.

**Tests** (`tests/test_mode.rs`, `today = 2026-10-25`):
- `future_test_mode_at_the_root_is_critical` (the firebase-tools template with 2026-11-24).
- `future_test_mode_on_a_collection_is_high`.
- `past_test_mode_is_review_graded_medium_and_names_both_readings` (2025-11-24).
- `test_mode_in_storage_rules_is_reported`.
- `twin_embargo_with_greater_than_is_silent`.
- `twin_time_clause_with_an_auth_conjunct_is_silent`.
- `twin_timestamp_date_with_variables_is_silent`.

**TOML**: `max_severity = "critical"`, `min_severity = "medium"`. Options
`no-test-mode-scaffold-in-deployed-rules` (default) and `test-mode-scaffold-deployed`
(`violates = true`). Sources: the `firebase init` Firestore template in the firebase-tools repository
(`templates/init/firestore/firestore.rules`: the comment block and `request.time <
timestamp.date(...)`), "Get started with Cloud Firestore" (test mode "allows anyone to read and
overwrite your data"), "Avoid insecure rules" ("grant access to all users (Test mode)").

### 7.3 FIREBASE-RULES-AUTH-ONLY-1

**Deterministic.** An `allow` whose condition `is_auth_only` (G3, G4) in an assessed Firestore,
Storage or RTDB file (Firebase's insecure-rules page lists the same "Access for any authenticated
user" pattern for all three):

- **Root** (G8, G10): Defect, `high`, read or write. Message: "Line 5 of `firestore.rules` lets any
  signed-in user read and write every document in the database (`allow read, write: if
  request.auth != null`). Anyone who can create an account, including with anonymous or email
  sign-in, has full access to all users' data."
- **Named path, write side** (G11): Defect, `high`. "Line 14 lets any signed-in user change or
  delete any document under `/posts/{postId}`, not only their own."
- **Named path, read side** (G9): Review grade, `medium`. "Line 14 lets any signed-in user read
  every document under `/users/{uid}`. If this collection is shared among all users by design, no
  change is needed; if it holds per-user data, scope reads to the owner."

**The fact.** The condition text at that line, and the effective path, both verifiable.

**Traps and guards.** Genuinely shared data among signed-in users: the read side is held, not
asserted (G9); a write-side auth-only grant on shared data (a wiki, a shared board) is still
reported, because "any account can delete anyone's contribution" is the fact and Firebase offers
no version of it as safe; the message says "not only their own" so the client sees exactly what is
claimed. Helper functions (`isSignedIn()`): G4. Extra conjuncts (`&& request.auth.token.email_verified
== true`, `&& isMember(roomId)`): not auth-only; silent. Public vocabulary: G5 for reads. G1, G2,
G7. RTDB: the same forms over `auth`; cascade means a root `".read": "auth != null"` is the root
case even when children say `false` (7.8 explains the shadowing fact).

**Tests** (`tests/auth_only.rs`):
- `auth_only_at_the_firestore_root_is_high`: the docs' snippet with `request.auth.uid != null`.
- `auth_only_write_on_a_collection_is_high`.
- `auth_only_read_on_a_collection_is_review_graded_medium`.
- `auth_only_through_a_helper_function_is_recognised`: `function isSignedIn() { return request.auth
  != null; } ... allow read: if isSignedIn();`.
- `auth_only_in_storage_root_is_high` (the old Storage default) and
  `auth_only_in_rtdb_root_is_high` (`".read": "auth.uid !== null"`).
- `twin_auth_plus_membership_check_is_silent`: `request.auth != null && exists(/databases/$(database)/documents/rooms/$(roomId)/members/$(request.auth.uid))`.
- `twin_email_verified_conjunct_is_silent`.
- `twin_owner_check_through_helper_is_not_auth_only`: `isOwner(userId)` whose body is
  `request.auth.uid == userId`.
- `twin_public_collection_auth_only_read_is_silent`.

**TOML**: `max_severity = "high"`, `min_severity = "medium"`. Options
`signed-in-is-not-the-only-condition-on-private-data` (default; label "No rule on private data
grants access on the sole condition that the caller is signed in") and
`any-signed-in-user-reaches-private-data` (`violates = true`). Sources: "Avoid insecure rules"
(access for any authenticated user, all three products), "Basic Security Rules" (development
rules, "any authenticated user", content-owner-only solution).

### 7.4 FIREBASE-RULES-OWNERSHIP-1

**Deterministic edge.** In an assessed Firestore file, an `allow` whose methods include `update` or
`write`, whose condition (inlined) has `uid_compared_to` containing at least one
`Target::Incoming(f)` and NO `Target::Existing(_)`, NO `Target::Wildcard(_)`, and does NOT
`pins_field(cond, f)`: Defect, `high`. The fact: "Line 18 of `firestore.rules` lets a signed-in
user update any document under `/posts/{postId}` by sending `authorId` set to their own id: the rule
compares the caller to the INCOMING document (`request.resource.data.authorId`) and never to the
stored one (`resource.data.authorId`). Firebase's own update example checks `resource.data`." This
is a verifiable, two-token fact (which field, which side), and the consequence (ownership
reassignment) follows from the rules language's definitions of the two variables (`request.resource`
is the incoming document; `resource` is the stored one).

**Judgment part (advisory tier).** "Per-user documents compare the owner to `request.auth.uid`" in
general needs to know which collections are per-user. The rule's `qualifies` and `directive` tell
the semantic pass to look for paths whose wildcard is a user id (`{userId}`, `{uid}`) or whose
parent literal is `users`/`profiles`/`accounts`, with reads or writes that reference neither the
wildcard nor an owner field. The checker sets `advisory_coexisting = true` so the id stays in the
prompt. An AI-tier row under this id is an ordinary advisory finding (calibrated, cited); a
deterministic edge row is `preview = true`.

**Traps.** `allow create: if request.auth.uid == request.resource.data.authorId`: create has no
stored document; checking the incoming one is correct and the methods set excludes `update`/`write`,
so silent. `allow update: if request.auth.uid == request.resource.data.authorId &&
request.resource.data.authorId == resource.data.authorId`: `pins_field` true; silent. `allow write`
with `request.auth.uid == userId` (wildcard): `Target::Wildcard` present; silent. A helper
`isOwner()` reading `resource.data.owner`: inlined (G4), `Existing` present; silent. A condition
with `||`: the comparison may be one disjunct of several; the rule requires the Incoming comparison
to be the only uid comparison in a top-level CONJUNCTION; any top-level `||` makes it not
recognised; silent. G1, G2.

**Tests** (`tests/ownership.rs`): `update_checking_only_the_incoming_owner_is_high`,
`write_checking_only_the_incoming_owner_is_high`, `twin_create_checking_the_incoming_owner_is_correct`,
`twin_update_that_pins_the_owner_field_is_silent`, `twin_wildcard_owner_comparison_is_silent`,
`twin_owner_helper_reading_resource_data_is_silent`, `twin_disjunction_is_not_recognised`.

**TOML**: `max_severity = "high"`, `min_severity = "medium"`, `advisory_coexisting` documented in
`qualifies` ("the incoming-only comparison is checked mechanically; whether a collection is per-user
is reviewed"). Options `owner-compared-to-the-stored-document-on-update` (default) and
`ownership-taken-from-the-incoming-document` (`violates = true`). Sources: "Basic Security Rules"
(content-owner-only; update and delete check `resource.data.author_uid`), "Security Rules language"
(`request` and `resource` variables).

### 7.5 FIREBASE-RULES-FIELD-VALIDATION-1

**Deterministic edge, review-graded.** In an assessed Firestore file, an `allow` whose methods
include `create`, `update` or `write`, whose condition is NOT `is_true` and NOT `is_auth_only` (those
are owned by 7.1/7.3), and whose inlined condition never `references(cond, "request.resource.data")`:
Review grade, emitted `medium`, corpus `max_severity = "low"` (clamped at export, the W29
mechanism). One row per match block (not per allow). The fact: "Writes to `/users/{uid}` (line 11)
are scoped to the owner but not validated: any field of any type can be written, including fields
the application treats as privileged such as `role`, `isAdmin`, `credits` or `price`. Firestore
documents are schemaless, so the rules are the only place field names and types can be
constrained." Which fields matter is the advisory tier's judgment (`advisory_coexisting = true`);
the deterministic row says only "nothing is validated", which is verifiable.

**Traps.** Any `request.resource.data` reference at all (a single `is string`, a `hasOnly`, a
`diff()`) makes the block silent, even when incomplete (partial validation is a judgment, not a
fact). A `create`-only rule on a write-once log where validation lives in a Cloud Function: still
reported, low, held; the message's "if writes are validated elsewhere" clause covers it. Paths with
G5 public segments: not exempt (writes to public content still need validation) but the row is low.
G1, G2.

**Tests** (`tests/field_validation.rs`): `owner_scoped_write_with_no_field_validation_is_a_low_review_row`,
`one_row_per_match_block_not_per_allow`, `twin_has_only_key_allowlist_is_silent`,
`twin_affected_keys_diff_is_silent`, `twin_single_type_check_is_silent`,
`twin_open_or_auth_only_writes_belong_to_other_rules` (no FIELD-VALIDATION row beside the OPEN row),
`twin_read_only_block_is_silent`.

**TOML**: `max_severity = "low"`. Options `write-rules-constrain-fields-and-types` (default) and
`write-rules-accept-any-fields` (`violates = true`). Sources: "Control access to specific fields"
(`hasOnly`, `hasAll`, `diff().affectedKeys()`, `is string`/`is int`), "Basic Security Rules".

### 7.6 FIREBASE-RULES-RECURSIVE-WILDCARD-1

**Deterministic.** In an assessed Firestore or Storage file: a match block B whose last segment is a
recursive wildcard, carrying an allow A whose condition `is_true`, `is_auth_only` or `is_test_mode`,
when at least one OTHER match block N exists whose effective path is covered by B's (N's segments
extend B's prefix, or B is the root wildcard), and N carries an allow for an overlapping method set
whose condition is none of those three (a stricter rule). Grade and severity: those of A under
7.1/7.2/7.3 (the broad grant decides); by G7, 7.1/7.2/7.3 then do NOT emit for A. The fact: "Line 5
of `firestore.rules` grants read and write to any signed-in user on `/{document=**}`. Because access
is granted when ANY matching rule allows it, this grant makes the narrower rules at lines 12, 19 and
27 (`/users/{uid}`, `/orders/{id}`, `/invoices/{id}`) ineffective: a request they would deny is
allowed by line 5." Lists every overridden line (no "and N more").

**Advisory part.** A broad grant with a non-trivial condition (`if isAdmin()` at the root, `if
request.auth.uid == resource.data.owner` at the root) is a design choice (an admin override is
legitimate; see 7.9 for the one deterministic admin fact). `qualifies` tells the semantic pass to
review such grants for whether they widen the narrower rules unintentionally; nothing is asserted.

**Traps.** `allow read, write: if false` on the wildcard: G6, silent, and not counted as a
broad grant. A recursive wildcard with NO narrower rules: not this rule (7.1/7.3 report the grant
alone). `v1` versus `v2` coverage: G8. A narrower rule that is itself open: not "stricter"; not
counted (both are 7.1's). A `match /{path=**}/comments/{c}` (wildcard in the middle, allowed in v2):
the covered set is "every `comments` subcollection", and a narrower `/posts/{p}/comments/{c}` is
covered; the same test applies. G1, G2.

**Tests** (`tests/recursive_wildcard.rs`):
`broad_auth_only_grant_lists_every_overridden_narrower_rule` (three narrower rules; message names
all three lines; exactly one row; no AUTH-ONLY row), `broad_open_grant_with_narrower_rules_is_critical_and_owns_the_fact`,
`broad_test_mode_grant_with_narrower_rules`, `twin_recursive_wildcard_locked_with_false_is_silent`,
`twin_recursive_wildcard_alone_is_reported_by_open_not_here`,
`twin_admin_override_with_a_claims_condition_is_silent`, `mid_path_wildcard_covers_matching_subcollections`.

**TOML**: `max_severity = "critical"`, `min_severity = "medium"`. Options
`broad-wildcard-grants-do-not-override-narrower-rules` (default) and
`a-broad-wildcard-grant-overrides-the-narrower-rules` (`violates = true`). Sources: "Security Rules
structure" (recursive wildcards; "the access is allowed if any of the conditions is true"),
"Security Rules language" ("If any matching rule grants access, the request is permitted"), Storage
"Core syntax" ("the result is the OR of the result of all rules evaluations").

### 7.7 FIREBASE-STORAGE-RULES-OPEN-1

**Deterministic.** In an assessed Storage rules file (`service firebase.storage`), with the Storage
root being `/b/{bucket}/o/{x=**}` (G8):

- Write side (`write`, `create`, `update`, `delete`) with `is_true`: Defect, `high` (`critical` at
  the root). With `is_auth_only`: Defect, `high` ("any signed-in user can upload, replace or delete
  any file"; Firebase's insecure-rules page lists this Storage form as "Not recommended").
- Read side `is_true` at the root: Defect, `high`. On a named folder: Review grade, `medium`,
  unless G5. The message names the folder and asks: "Line 8 of `storage.rules` lets anyone with the
  URL read every file under `/uploads/{userId}/{file}`. Verification photos, identity documents and
  private uploads must not be readable this way; public assets belong under an explicit public
  folder with its own rule."
- Read side `is_auth_only` at the root: Defect, `medium` (the shape of the old default; the file
  set is "every file" so G10 applies, but the exposure is to account holders, one step below open).
  On a named folder: Review, `low` (emitted `medium`, clamped by the rule's floor/ceiling pair is
  not possible per finding; the checker emits `medium` with Review grade and the message says
  "signed-in users").

The TOML carries a comment that this is the class of the July 2025 Tea app exposure (verification
selfies and ID photos in an unsecured Firebase Storage bucket, reported by The Record and others);
that is a comment for the reader, never a `[[sources]]` entry.

**Traps.** `/public/`, `/static/`, `/assets/` folders: G5. Conditions on `resource.metadata`,
`resource.size`, `request.resource.contentType`: not recognised, silent (a size-capped public read
under `/public/` is the docs' own example and is G5 anyway). `allow read;` with no condition is
`is_true` (the docs' "mixed public and private access" Storage example uses it for user files that
are meant to be public: held, not asserted, exactly case (c)). The legacy `service cloud.storage`
header parses as Storage. G1, G2, G7.

**Tests** (`tests/storage.rs`): `open_write_on_a_storage_folder_is_high`,
`open_read_and_write_at_the_storage_root_is_critical`, `auth_only_write_anywhere_is_high`,
`auth_only_read_at_the_root_is_medium` (the old default), `open_read_on_uploads_is_review_graded`,
`twin_public_folder_open_read_is_silent`, `twin_size_capped_public_read_is_silent`,
`twin_owner_scoped_folder_is_silent` (`allow read, write: if request.auth.uid == userId`),
`twin_firestore_file_is_not_this_rule`.

**TOML**: `max_severity = "critical"`, `min_severity = "medium"`. Options
`private-files-are-readable-only-by-authorized-users` (default; label "Storage rules grant reads
and writes on private files only to the owner or an authorized role, and public files sit under an
explicit public path") and `private-files-are-publicly-readable-or-writable` (`violates = true`).
Sources: "Avoid insecure rules" (Cloud Storage open and any-authenticated forms, content-owner
solution), Storage "Use conditions in Firebase Cloud Storage Security Rules" (any rule that does not
consider `request.auth` "can be considered a public rule"; `/public/{imageId}` example).

### 7.8 FIREBASE-RTDB-RULES-OPEN-1

**Deterministic.** Over the assessed RTDB rules JSON (5.2):

- `".write"` effectively `true` (own value, or inherited by cascade) at the root: Defect,
  `critical`; at a named node: `high`. `".read": true` at the root: Defect, `high`. `".read": true`
  at a named node: Review grade, `medium` (G9), unless G5.
- **Shadowed denial** (Firebase's "Improperly inherited rules"): a node with `".read": false` or
  `".write": false` whose ancestor grants `true` (or auth-only) for the same key. Defect at the
  ancestor's severity, one row per shadowed node, message: "`/users/$uid/private` sets `.read` to
  `false` (line 14), but `/users` grants `.read: true` (line 9). Rules at a child path can only grant
  more access, never less, so the denial has no effect." This is a verifiable fact from two lines.
- Auth-only forms are 7.3's (same cascade logic).

**Traps.** `.validate` never cascades and never grants; ignored. `.indexOn`: ignored. A `.read` of
`"true"` (the string): the docs show both forms; the string `"true"` normalises to `is_true`. A
`.write: true` on a leaf used as a public counter or presence node (`/status/$uid`): still "anyone
can write", reported `high` on a named node; the message names the node. G1 (the file
`firebase.json` names, or `database.rules.json`), G2.

**Tests** (`tests/rtdb.rs`): `root_read_and_write_true_is_critical`, `named_node_write_true_is_high`,
`named_node_read_true_is_review_graded`, `a_child_denial_under_a_parent_grant_is_reported_as_ineffective`,
`twin_public_node_read_true_is_silent`, `twin_owner_scoped_node_is_silent` (`"$uid": { ".write":
"$uid === auth.uid" }`), `twin_validate_rules_are_ignored`, `malformed_rules_json_degrades_to_not_assessed`.

**TOML**: `max_severity = "critical"`, `min_severity = "medium"`. Options
`no-realtime-database-node-is-unconditionally-readable-or-writable` (default) and
`a-realtime-database-node-is-open` (`violates = true`). Sources: "Avoid insecure rules" (Realtime
Database open access, improperly inherited rules), "Understand Firebase Realtime Database Security
Rules" (".read and .write rules cascade"; shallower rules take precedence).

### 7.9 FIREBASE-ADMIN-CLAIMS-1

**Deterministic edge.** In an assessed Firestore file: an allow A whose inlined condition has a
`role_lookup` (5.4) reading `<coll>/$(request.auth.uid)` field `<field>`, AND a match block M whose
effective path is `/<coll>/{w}` (same collection, one wildcard) carrying an allow W with methods
including `create`, `update` or `write`, whose condition is satisfied by the owner (its
`uid_compared_to` contains `Target::Wildcard(w)` or `Target::Existing(_)`/`Incoming(_)` naming an
owner-like field, or it `is_auth_only` or `is_true`), AND `!pins_field(W.condition, <field>)`:
Defect, `high`. The fact: "Line 6 of `firestore.rules` grants admin access when
`users/{uid}.role == 'admin'`, and line 15 lets each user write their own `users/{uid}` document
without pinning `role`. A user can set `role: 'admin'` on their own document and gain admin access."

**Advisory part.** Firebase documents data-defined roles (`get(...).data.admin == true`) as a
legitimate pattern with a read cost, and custom claims (`request.auth.token.admin == true`) as the
pattern where "Claims are set up in Firebase Authentication" and the custom-claims page says to
"Use custom claims to store data for controlling user access only" and to set them "only ... from a
privileged server environment". The rule's directive therefore reads: "Admin and role checks come
from custom claims (`request.auth.token.<claim>`) or from documents that no client can write; a role
stored in a document the user can edit is a self-grant." The general preference for claims is
advisory; only the self-grant is asserted.

**Traps.** The role document is in a collection with `allow write: if false` or write only via
`request.auth.token.admin == true`: W does not exist or is not owner-satisfiable; silent. The owner
write pins the field (`request.resource.data.role == resource.data.role` or
`affectedKeys().hasOnly(['displayName', 'photoURL'])`): `pins_field`; silent. The role lives in a
separate collection (`/roles/{uid}`) with no client write: silent. The lookup keys on something other
than `request.auth.uid` (`$(request.resource.data.ownerId)`): not a self-lookup; silent. Helpers
(`isAdmin()`): G4. G1, G2.

**Tests** (`tests/admin_claims.rs`): `role_read_from_a_self_writable_user_document_is_high`,
`role_lookup_through_a_helper_is_recognised`, `twin_role_document_with_no_client_write_is_silent`,
`twin_owner_write_that_pins_the_role_field_is_silent`,
`twin_owner_write_with_has_only_excluding_role_is_silent`, `twin_custom_claims_check_is_silent`,
`twin_lookup_keyed_on_another_document_is_silent`.

**TOML**: `max_severity = "high"`, `min_severity = "high"`. Options
`roles-come-from-claims-or-documents-no-client-can-write` (default) and
`a-role-is-stored-where-its-holder-can-write-it` (`violates = true`). Sources: "Basic Security
Rules" (custom-claim attributes and roles; data-defined roles), "Control Access with Custom Claims
and Security Rules" (claims set only from a privileged environment; "controlling user access only").

### 7.10 FIREBASE-FUNCTIONS-AUTH-1

Structural, over the ECMA extractors; discovery in section 8. Two review-graded facts, `medium`,
emitted per function, `max_severity = "high"` with `min_severity = "medium"`:

- **HTTP function.** An exported `onRequest` handler H (8.2) in package P such that: no file in P
  contains a call to `verifyIdToken(`, `verifySessionCookie(`, `getAppCheck().verifyToken(` /
  `verifyToken(` (App Check), and H's body (or, when H is an Express app, any file in P) performs
  an Admin SDK write (`.set(`, `.update(`, `.delete(`, `.add(`, `.create(`, `.push(`, `.remove(`,
  `batch.commit(`, `runTransaction(` on a receiver chain containing `firestore`, `getFirestore`,
  `database`, `getDatabase`, `storage`, `getStorage`, `bucket`). Review grade. Fact: "The HTTP
  function `createOrder` (`functions/src/orders.ts`, line 12) is reachable by anyone who knows its
  URL. Nothing in the `functions` package verifies a Firebase ID token (`verifyIdToken`) or an App
  Check token, and the handler writes to Firestore with the Admin SDK, which bypasses security
  rules." Firebase's own sample (`authorized-https-endpoint`) shows the middleware: read
  `Authorization: Bearer <ID token>`, `verifyIdToken`, 403 otherwise.
- **Callable function.** An exported `onCall` handler whose body (and the bodies of helper
  functions it calls in the same file) never references `request.auth` / `context.auth` / a
  destructured `auth` of the first (v2) or second (v1) parameter, and never calls a helper whose name
  matches `(?i)(require|assert|ensure|check).*auth|auth.*(require|guard|check)` with that
  parameter, and performs an Admin SDK write or reads with a caller-supplied id (`request.data.<id>`
  / `data.<id>` used in a `.doc(` / `.collection(` / `.child(` argument). Review grade. Fact: "The
  callable function `deleteAccount` (`functions/src/index.ts`, line 40) never checks `request.auth`,
  so an unauthenticated client can call it, and it deletes `users/{request.data.uid}` with the
  Admin SDK." Firebase's callable guide shows the check (`if (!request.auth) { throw new
  HttpsError(...) }`) and notes callables "automatically ... validate auth tokens" (a present token
  is validated; an absent one is not rejected unless the handler checks).

**Why review-graded.** Public HTTP endpoints and public callables are legitimate (webhooks, health
checks, public counters). The guards below remove the known legitimate shapes; what remains is a
true fact ("no verification, and a privileged write") whose defect status still depends on intent.

**Traps and guards.**
- **Webhooks**: a handler whose module references `constructEvent(`, `webhooks.`, `verifySignature`,
  `timingSafeEqual(`, `x-hub-signature`, `x-signature`, `svix`, `hmac` is a webhook; silent.
- **Function names and paths** containing `webhook`, `hook`, `callback`, `health`, `ping`,
  `sitemap`, `robots`, `og`, `image`, `thumbnail`, `cron`, `scheduled`: silent (false negative).
- **IAM-protected functions**: v2 `onRequest({ invoker: "private" }, ...)` or any `invoker` option
  that is not `"public"`: silent.
- **Express apps** (`onRequest(app)`): verification may live in middleware in another file; the
  whole package is searched, so a package with one `verifyIdToken` anywhere is silent for every
  `onRequest` (false negative by design; per-route tracing is V2).
- **Scheduler, Pub/Sub, Firestore triggers** (`onSchedule`, `onMessagePublished`,
  `onDocumentWritten`, `functions.pubsub`, `functions.firestore`): not HTTP; never candidates.
- **Hosting rewrites**: a function behind `firebase.json` `hosting.rewrites[].function` is still an
  HTTP function; no exemption.
- **Callable auth in a wrapper**: `export const f = onCall(withAuth(async (req) => ...))`: any call
  wrapping the handler whose callee name matches the auth-name regex counts as checked; silent.
- **Reads only**: an HTTP function or callable that only reads public data: no write, no
  caller-supplied id: silent.
- **Python functions**: G13, not assessed.
- **Emulator-only code** under `is_test_path`: skipped.

**Tests** (`tests/functions_auth.rs`): `http_function_that_writes_with_admin_sdk_and_no_verify_is_review_graded`,
`http_function_with_verify_id_token_middleware_anywhere_in_the_package_is_silent`,
`http_function_verifying_app_check_is_silent`, `twin_stripe_webhook_is_silent`,
`twin_private_invoker_is_silent`, `twin_health_endpoint_is_silent`, `twin_read_only_http_function_is_silent`,
`callable_without_an_auth_check_that_deletes_by_caller_id_is_review_graded`,
`callable_checking_request_auth_is_silent` (v2) and `callable_checking_context_auth_is_silent` (v1),
`callable_using_a_require_auth_helper_is_silent`, `callable_wrapped_by_with_auth_is_silent`,
`twin_scheduled_and_firestore_triggers_are_never_candidates`,
`a_python_functions_package_is_not_assessed_rather_than_clean`.

**TOML**: options `functions-verify-the-caller-before-privileged-work` (default; label "HTTP
functions verify a Firebase ID token or App Check token before acting, and callable functions
check `request.auth` before privileged work") and `functions-act-for-unverified-callers`
(`violates = true`). Sources: "Verify ID Tokens" (send the ID token over HTTPS; the server verifies
it with `verifyIdToken` and reads `uid`), the firebase/functions-samples `authorized-https-endpoint`
(`Authorization: Bearer <Firebase ID Token>`, 403 on failure), "Call functions from your app"
(`request.auth`; "Checking that the user is authenticated" with `HttpsError`).

### 7.11 FIREBASE-ADMIN-SDK-CLIENT-1

**Structural, deterministic.** An `Import` (via `extract::imports`) whose specifier is
`firebase-admin` or starts with `firebase-admin/` in a file F that is **browser-bundled**:

- F is in a package P (nearest `package.json`) whose dependencies include a client bundler or SPA
  framework (`vite`, `react-scripts`, `@angular/core`, `vue`, `svelte` without `@sveltejs/kit`,
  `parcel`, `webpack` with `react`) AND do NOT include a server runtime that legitimately uses the
  Admin SDK (`next`, `nuxt`, `@sveltejs/kit`, `@remix-run/*`, `react-router` with `app/root.*`,
  `astro`, `express`, `fastify`, `koa`, `hono`, `firebase-functions`, `@nestjs/*`); OR
- F's first statement is the directive `"use client"` / `'use client'` (a Next.js client component,
  bundled for the browser whatever P has); OR
- F is under `public/`, `static/`, or `www/`.

Defect, `high`. Fact: "`src/lib/db.ts` (line 2) imports `firebase-admin`, and this package is a
browser application (`vite` in `package.json`). The Admin SDK is for privileged server environments;
in a browser bundle it either fails to build or ships the service-account credentials it is
initialised with." Firebase: the Admin SDK lets you work with Firebase "from privileged
environments"; service account keys demand "Extremely high security awareness".

**Traps.** Next.js, Nuxt, SvelteKit, Remix, Astro: server code lives beside client code in one
package; the manifest test exempts the whole package and only the `"use client"` directive flags
inside it. A `functions/` package or an Express API: exempt. A monorepo where the SPA's manifest is a
parent `package.json` listing both `vite` and `express` (a single-package fullstack repo): both
present, exempt (false negative). Type-only imports (`import type { App } from 'firebase-admin/app'`,
`Import.type_only`): silent (erased at build). A `firebase-admin` import in a test under
`is_test_path`: silent. `.d.ts` files: silent. The service-account key file itself: the secrets
floor's PEM shape already reports it; this rule never duplicates that.

**Tests** (`tests/admin_sdk_client.rs`): `admin_sdk_imported_in_a_vite_app_is_high`,
`admin_sdk_imported_in_a_use_client_component_is_high`, `twin_admin_sdk_in_a_next_server_file_is_silent`,
`twin_admin_sdk_in_the_functions_package_is_silent`, `twin_admin_sdk_in_an_express_api_is_silent`,
`twin_type_only_import_is_silent`, `twin_fullstack_single_package_is_silent`.

**TOML**: `max_severity = "high"`, `min_severity = "high"`. Options
`admin-sdk-stays-in-server-code` (default) and `admin-sdk-imported-by-browser-code`
(`violates = true`). Sources: "Add the Firebase Admin SDK to your server" (privileged environments;
service account key handling), "Learn about using and managing API keys for Firebase" (separate
restricted keys for non-Firebase APIs; the Gemini key never in code).

### 7.12 FIREBASE-APP-CHECK-1

**Advisory, low, absence.** One review-graded row per repository when: a Firebase client
initialisation exists (`initializeApp(` from `firebase/app`, `Firebase.initializeApp(` in Dart,
`FirebaseApp.configure()` in Swift is out of scope) AND none of these appears anywhere outside test
paths: `initializeAppCheck(`, `FirebaseAppCheck.instance.activate(`, `enforceAppCheck`,
`request.app` / `context.app` in a callable, `X-Firebase-AppCheck`, `getAppCheck()`. Emitted
`medium`, corpus `max_severity = "low"`. Fact: "This Firebase app initialises the client SDK
(`src/firebase.ts`, line 4) but App Check is neither initialised in the client nor enforced on any
of its 6 callable functions. Firebase's callable guide says to enable App Check before launch so
that only your apps can reach your function endpoints." Whether abuse matters for this app is the
judgment the client makes from the held row.

**Traps.** A backend-only repo (Functions with no client init): silent. Enforcement configured in
the console only (the Firestore/RTDB/Storage App Check toggles live in the console, not the repo):
the message says "in the repository" and the row is held, never asserted; the client SDK
initialisation, which must be in code, is the half the repository can show. G12 (complete scan).

**Tests** (`tests/app_check.rs`): `client_init_without_app_check_is_one_low_review_row`,
`twin_initialize_app_check_anywhere_is_silent`, `twin_flutter_activate_is_silent`,
`twin_enforce_app_check_on_callables_is_silent`, `twin_backend_only_repo_is_silent`,
`a_truncated_scan_withholds_the_rule`.

**TOML**: `enforcement = "structured"`, `max_severity = "low"`, `runs_at_write_time = false`.
Options `app-check-protects-the-backends-that-matter` (default) and `no-app-check-anywhere`
(`violates = true`). Sources: "Firebase App Check" overview (protects backends from abuse; per
service enablement; callable functions), "Enable App Check enforcement for Cloud Functions"
(`enforceAppCheck: true`; `request.app`), "Call functions from your app" ("Before you launch your
app, you should enable App Check").

### 7.13 FIREBASE-RULES-TESTS-1

**Advisory, low, absence.** One row per repository when at least one assessed rules file exists
(5.3) and no `package.json` in the repository lists `@firebase/rules-unit-testing` (or the
deprecated `@firebase/testing`) in any dependency block. Defect grade (the absence is a plain fact),
emitted `medium`, `max_severity = "low"`. Fact: "This repository deploys security rules
(`firestore.rules`, `storage.rules`) but has no rules tests: no package depends on
`@firebase/rules-unit-testing`, the library Firebase provides to run rules against the emulator."

**Traps.** Rules tests written against the emulator REST API in another language, or kept in a
separate repository: invisible; the row is low and the remediation says how to close it. G12.
Repos with no rules files: silent (nothing to test). G1 and G2 do not apply (the test files are the
point).

**Tests** (`tests/rules_tests.rs`): `rules_files_without_the_testing_library_is_one_low_row`,
`twin_rules_unit_testing_in_dev_dependencies_is_silent`, `twin_deprecated_firebase_testing_is_silent`,
`twin_no_rules_files_is_silent`, `a_truncated_scan_withholds_the_rule`.

**TOML**: `enforcement = "structured"`, `max_severity = "low"`, `runs_at_write_time = false`.
Options `security-rules-have-unit-tests` (default) and `security-rules-are-untested`
(`violates = true`). Sources: "Build unit tests" for Security Rules (`@firebase/rules-unit-testing`,
`initializeTestEnvironment`, `assertSucceeds`/`assertFails`, "validate your Security Rules"), "Avoid
insecure rules" (the "Test your Cloud Firestore Security Rules" section).

## 8. Cloud Functions: discovery and structural recognition (rules 10, 11, 12)

### 8.1 Finding the functions packages

A **functions package** is the directory of any `package.json` whose `dependencies` or
`devDependencies` contain `firebase-functions`. `firebase.json` `functions.source` (string or
array of `{ source, codebase }`) names them too and is read when present; the manifest test is the
one that decides (a `functions/` folder with no manifest is not a package). The default is
`functions/`. Files of a package: every `.ts .tsx .js .mjs .cjs` under its directory, excluding
`node_modules` (noise), `lib/` and `dist/` (compiled output, noise) and `is_test_path`.

### 8.2 Recognising handlers

Over each file: `extract::imports` to learn what `onRequest`, `onCall`, `https`, `functions` are
bound to (`firebase-functions/v2/https`, `firebase-functions/https`, `firebase-functions/v1`,
`firebase-functions/v1/https`, `firebase-functions` default/namespace imports), then a tree-sitter
query over `call_expression` nodes (the same mechanics `extract::method_calls` uses) for:

- v2 / unversioned: `onRequest(<handler>)`, `onRequest(<options>, <handler>)`, `onCall(<handler>)`,
  `onCall(<options>, <handler>)`, where the callee is the imported binding or `https.onRequest` /
  `https.onCall` on an imported `https` namespace.
- v1: `functions.https.onRequest(...)`, `functions.https.onCall(...)`,
  `functions.region(...).https.onRequest(...)`, `functions.runWith({...}).https.onRequest(...)`
  (the chain is walked to its `https` member).

The **exported name** is the enclosing `export const X = ...` / `exports.X = ...` /
`module.exports = { X }` binding (or `<anonymous>` with the line). `<handler>` is an arrow or
function expression (body analysed directly), an identifier (resolved to a function or const in the
same file; otherwise "unknown handler": not a candidate), or, for `onRequest`, an identifier bound to
an Express/Hono app (`express()`, `new Hono()`): the package-wide search applies.

**Body analysis** is textual over the handler's span (and the spans of same-file helper functions
it calls, one level): the token checks listed in 7.10. Parameters: v2 callable's first parameter
name (and destructured `{ auth, data }`), v1 callable's second. A `request.auth` reference is any
member access `<param>.auth` or a destructured `auth` from `<param>`.

**Options** are read from the first argument when it is an object literal: `invoker`,
`enforceAppCheck`, `cors`. Non-literal options (`onRequest(opts, h)` with `opts` a variable):
`invoker` unknown, treated as public (the default), but the review grade keeps the law.

### 8.3 Tests (`firebase/tests/functions_discovery.rs`)

`finds_v2_on_request_and_on_call_with_and_without_options`, `finds_v1_chained_https_handlers`,
`finds_express_app_handlers_and_searches_the_package`, `exported_names_are_resolved_for_const_exports_and_module_exports`,
`compiled_lib_and_dist_folders_are_not_read`, `a_functions_folder_without_a_manifest_is_not_a_package`,
`multiple_codebases_from_firebase_json_are_all_found`, `an_unknown_handler_binding_is_not_a_candidate`.

## 9. Wiring

### 9.1 The one seam change: a grade on `ArchViolation`

Add `pub grade: ViolationGrade` to `ArchViolation` with
`enum ViolationGrade { Defect, Review, NotAssessed }` (34 struct-literal construction sites in
`crates/checks/src` set `grade: ViolationGrade::Defect`; the compiler lists them). The adapter
`arch_violation_to_finding`:

- `Defect`: as today.
- `Review`: `needs_review = true`, `confidence = Some("needs-review")` (the biconditional
  `ai_audit.rs` maintains), `kind = Defect`; `report_export::is_held_for_review` then routes it to
  the held bucket exactly as a calibration-doubt AI row is routed today.
- `NotAssessed`: `kind = FindingKind::NotAssessed(NotAssessedKind { reason:
  NotAssessedReason::PassFailed, reason_detail: message })`, `severity = "info"`, `located =
  line > 0`, `preview = true`; `is_informational` keeps it out of the action tiers, and the
  exporters already render the kind ("armed but not evaluated").
- `NativeArchCheckRunner` (the Layer-2 write-time gate) drops `Review` and `NotAssessed` violations
  before deciding a bounce: a held fact must never bounce an agent. Test:
  `the_write_time_runner_never_bounces_on_a_review_graded_violation`.
- The ledger loop counts `findings_emitted` over `Defect` and `Review` only. `camerata-check`
  prints not-assessed rows under a "not assessed" heading.

### 9.2 Checkers and registry

Four `ArchChecker`s in `crates/checks/src/firebase/`, each registered in `all_checkers()`:

| checker | rule ids | interest globs | `config_unsatisfied_for` true when | write time |
|---|---|---|---|---|
| `FirebaseRulesChecker` | 1, 2, 3, 4, 5, 6, 7, 8, 9 | `**/*.rules`, `**/firebase.json`, `**/.firebaserc`, `**/database.rules.json`, `**/*.rules.json`, `**/*.json` (context, for a renamed RTDB file) | the assessed file set (5.3) is empty | yes (Review/NotAssessed dropped by 9.1) |
| `FirebaseFunctionsChecker` | 10, 12 | `**/package.json`, `**/firebase.json`, `**/*.ts`, `**/*.tsx`, `**/*.js`, `**/*.mjs`, `**/*.cjs`, `**/*.dart` (App Check activate) | no functions package and no client init | no (review-graded) |
| `FirebaseAdminSdkClientChecker` | 11 | `**/package.json`, the JS/TS globs | no `firebase-admin` import anywhere | yes |
| `FirebaseRulesTestsChecker` | 13 | `**/package.json`, `**/*.rules`, `**/firebase.json`, `**/database.rules.json` | no assessed rules file | no |

`advisory_coexisting() = true` on `FirebaseRulesChecker` (rules 4, 5, 6, 9 have judgment halves the
semantic pass must still see) and on `FirebaseFunctionsChecker` (12's "where abuse matters").
`RULES_NEEDING_COMPLETE_SCAN` gains 12 and 13 (G12). `capture_token_for` gains the Firebase ids
with token `path` (the match path or node, `ArchViolation.object`), so remediation text can say
"scope reads on `<path>` to the owner".

### 9.3 Proposal, ledger, Layer 2 and CI

- Proposal: section 4; the proposed-rules test proves all 13 arm for a Firebase repo and none for a
  Node API, a Vue app or a Python repo.
- Ledger: the generic loop credits `arch_checker` channel ids; `config_unsatisfied_for` keeps
  unanswered repos out of "verified clean".
- Layer 2 (`NativeArchCheckRunner`) and the `camerata-check` CI binary load files by the checkers'
  interest globs through `collect_interest_files`, which prunes `multilang::PRUNED_DIRS`; `.rules`,
  `firebase.json` and `.firebaserc` match the globs above (the glob matcher handles a literal dot
  segment). A CLI test exercises `firestore.rules` through the real walk, as the hooks-lint rule did
  for `.eslintrc.json`.
- The AI pass: `.rules` files are admitted files, so they are chunked into the digest like any
  other; no prompt change. The rules' `directive` text carries the judgment halves.

## 10. What V1 explicitly does NOT cover

- **Dart client logic.** Dart is read as text by the secrets floor and the advisory pass; there is
  no tree-sitter-dart grammar, no `SourceLang::Dart`, no imports/functions extraction, and no rule
  about what a Flutter client does (client-side authorization checks, Firestore queries that assume
  rules, `FirebaseAuth` misuse). V2, with the Firebase fixture.
- **Flutter-specific rules** (FlutterFlow export patterns, `firebase_options.dart` hygiene beyond
  the apiKey twin, platform `google-services.json` placement). V2.
- **The keyed calibration fixture** for the Firebase pack. V2 (Zach: moves to the front if a
  Firebase or Flutter client signs first).
- **Python and other Cloud Functions runtimes** (G13): reported not assessed.
- **Per-route verification inside Express apps** behind `onRequest(app)`: package-wide presence of
  `verifyIdToken` silences the rule (false negative).
- **Console state**: rules, App Check enforcement toggles and function IAM edited in the console
  are not in the repository; every rules finding says so.
- **Condition semantics beyond the exact recognisers**: anything with `get()`/`exists()` other than
  the self-role lookup, `resource.data.*` predicates, `matches()`, `in` lists, or `||` at the top
  level is left to the advisory tier.
- **Static-analysis of flat `firebase.json` Hosting headers** or rewrites: out of scope.

**The honest scope sentence for the report's coverage section** (client voice): "Firebase coverage
in this inspection is the security rules files in the repository (Cloud Firestore, Cloud Storage
and Realtime Database), the Firebase project configuration, and Cloud Functions written in
JavaScript or TypeScript. Dart and Flutter client code was read for committed secrets and by the
advisory review, but it was not parsed structurally and no Flutter-specific rule was applied. Rules
and settings that exist only in the Firebase console are not visible to a repository inspection."

## 11. Build order (small green commits; 24a first)

1. **24a admission.** `files.rs`: `rules`, `dart`, `plist`, the generated-Dart suffixes,
   `.firebase` noise; `propose.rs::lang_for_ext` Dart; the section 2.3 tests; `count.rs` test.
   Green: the server suite.
2. **The apiKey twin.** `crates/secrets`: `TwinContext`, `Twin::FirebaseWebApiKey`, `counts_as_line`,
   the classify rules, section 3.4 tests (secrets, gateway, server e2e). Green: secrets, gateway,
   server. This closes the acceptance item "no false critical on the apiKey".
3. **Stack detection and domain.** `detect_frameworks` Firebase markers (manifest, pubspec, paths,
   content), `FRAMEWORK_DOMAINS` row, witnesses, `domain_to_glob` arm, section 4.3 tests. Green:
   server + rules. (The H1 gate stays green because no `firebase/` folder exists yet.)
4. **The seam grade.** `ViolationGrade` on `ArchViolation`, the adapter, the write-time runner drop,
   `camerata-check` rendering, tests. Mechanical; one commit.
5. **The reader.** `firebase/rules_parse.rs`, `rtdb.rs`, `config.rs`, `rules_model.rs`, the 5.6
   tests. No checker yet.
6. **Rules 1, 2, 3, 6** (one checker, shared recognisers, the G7 ownership logic) with their tests
   and the not-assessed rows for degraded and unreferenced files.
7. **Rules 7 and 8** (Storage, RTDB) in the same checker.
8. **Rules 4, 5, 9** (the deterministic edges) in the same checker; `advisory_coexisting`.
9. **Functions discovery (8) and rules 10, 11, 12.**
10. **Rule 13**, `RULES_NEEDING_COMPLETE_SCAN` additions, proposed-rules test for all 13.
11. **Corpus TOMLs** (13 files under `crates/rules/principles/firebase/`), `camerata corpus lock`,
    under the recorded owner go in `docs/OVERNIGHT_DECISIONS.md`.
12. **Acceptance.** A small fixture repo under `crates/server/tests/fixtures/firebase-app/` (a
    Vite + Firebase web app with `firestore.rules`, `storage.rules`, `firebase.json`, `functions/`,
    `src/firebase.ts` with a runtime-assembled config) scanned end to end through
    `audit_repos_with_llm` with the stub model: zero `SEC-*` rows, the expected rules rows, the
    coverage sentence present. An `#[ignore]` sibling scans a named open-source Firebase app from
    disk when `CAMERATA_FIREBASE_SAMPLE` points at a checkout.

Each step is one PR-sized commit with its tests; Sonnet builds, Fable reviews, as MASTER-SEQUENCE
item 24 says.

## 12. Owner questions (only the ones that are genuinely his)

1. **A Google API key OUTSIDE a Firebase configuration: keep it a critical floor finding (today's
   behaviour), or make it a review-graded high?** Terms: a *review-graded* finding is shown in the
   report's held section with the fact stated ("a Google API key is committed at `src/maps.ts:4`;
   its API restrictions are not visible in the repository") and asks the client to confirm, instead
   of being asserted as a defect in the action list. Why it is yours: Google says Firebase keys are
   safe to commit and other Google keys must be restricted; a Maps browser key is designed to be in
   client code and protected by referrer restrictions the repository cannot show, so "critical" is
   sometimes a false positive, while an unrestricted server key with billing is a real exposure.
   My recommendation: review-graded high, with the message above; but it changes an existing floor
   rule's behaviour for every client, so I did not decide it. V1 default: unchanged.
2. **Dart proposes the API-layer rule family.** With `Dart` recognised as a language, a Flutter
   repo gets `api-layer` (and with it `permissions` and `integration`) proposed, exactly as Kotlin
   and Swift repos do today, because those domains are the default for a language with no row.
   Terms: *proposed* means pre-checked in onboarding, not forced. I kept the existing default for
   consistency. Say so if you want Dart to propose nothing beyond `firebase` until the Flutter
   rules exist; it is a one-line table row.

Everything else in this document is decided and justified in place (test-mode past dates, the
read-side review grade, write-side assertion, the apiKey twin's shape, G7 ownership, Python
functions not assessed).

## 13. Sources (fetched 2026-10-25; every claim above attributes to one of these)

- Avoid insecure rules: https://firebase.google.com/docs/rules/insecure-rules
- Basic Security Rules: https://firebase.google.com/docs/rules/basics
- Security Rules language: https://firebase.google.com/docs/rules/rules-language
- Structuring Cloud Firestore Security Rules: https://firebase.google.com/docs/firestore/security/rules-structure
- Get started with Cloud Firestore Security Rules: https://firebase.google.com/docs/firestore/security/get-started
- Get started with Cloud Firestore (test mode / production mode): https://firebase.google.com/docs/firestore/quickstart
- Control access to specific fields: https://firebase.google.com/docs/firestore/security/rules-fields
- Understand Firebase Realtime Database Security Rules: https://firebase.google.com/docs/database/security
- Realtime Database: Use conditions in rules: https://firebase.google.com/docs/database/security/rules-conditions
- Cloud Storage: Use conditions in Security Rules: https://firebase.google.com/docs/storage/security/rules-conditions
- Cloud Storage: Core syntax: https://firebase.google.com/docs/storage/security/core-syntax
- Manage and deploy Firebase Security Rules: https://firebase.google.com/docs/rules/manage-deploy
- Build unit tests (rules-unit-testing): https://firebase.google.com/docs/rules/unit-tests
- Learn about using and managing API keys for Firebase: https://firebase.google.com/docs/projects/api-keys
- Verify ID Tokens: https://firebase.google.com/docs/auth/admin/verify-id-tokens
- Control Access with Custom Claims and Security Rules: https://firebase.google.com/docs/auth/admin/custom-claims
- Call functions from your app (callable): https://firebase.google.com/docs/functions/callable
- Call functions via HTTP requests: https://firebase.google.com/docs/functions/http-events
- Firebase App Check: https://firebase.google.com/docs/app-check
- Enable App Check enforcement for Cloud Functions: https://firebase.google.com/docs/app-check/cloud-functions
- Protect non-Firebase resources with App Check: https://firebase.google.com/docs/app-check/custom-resource-backend
- Add the Firebase Admin SDK to your server: https://firebase.google.com/docs/admin/setup
- The `firebase init` Firestore template (test-mode scaffold): https://github.com/firebase/firebase-tools/blob/master/templates/init/firestore/firestore.rules
- The `firebase init` Storage template (locked): https://github.com/firebase/firebase-tools/blob/master/templates/init/storage/storage.rules
- Official sample, ID-token-protected HTTPS endpoint: https://github.com/firebase/functions-samples/blob/main/Node-1st-gen/authorized-https-endpoint/functions/index.js
- Tea app exposure (context for 7.7's comment, not a rule source): https://therecord.media/tea-app-data-breach-stolen-ids-leaked
