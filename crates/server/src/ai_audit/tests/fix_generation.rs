//! Tests for fix generation.

use super::*;

// ── P2: fix-specific generation + self-check ────────────────────────────────────────

pub(super) fn fx(rule_id: &str, path: &str, line: usize, detail: &str, snippet: &str) -> Finding {
    Finding {
        repo: "o/r".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

// ── find_ungrounded_identifier (pure) ───────────────────────────────────────────────

#[test]
fn find_ungrounded_identifier_accepts_a_fix_naming_only_evidence_identifiers() {
    let evidence = "app/auth/signout/route.ts uses `rawRedirect` at line 12; \
                         lib/redirect.ts exports `safeInternalPath`";
    let fix = "Replace `rawRedirect` with `safeInternalPath` from lib/redirect.ts, as \
                    app/auth/signout/route.ts does.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);
}

/// D3: a standard JavaScript or Web API name is not an invented one, written as a backticked
/// name, inside a snippet, or bare (`import { randomUUID }`).
#[test]
fn find_ungrounded_identifier_accepts_standard_javascript_and_web_api_names() {
    let evidence = "supabase/functions/charge/index.ts uses amount_cents";
    for fix in [
        "Validate `amount_cents` with `Number.isSafeInteger(amount_cents) && amount_cents > 0`.",
        "Check that `Number.isSafeInteger` holds for `amount_cents`.",
        "Use `crypto.randomUUID()` for the key instead of `Math.random()`.",
        "Call `crypto.randomUUID` in supabase/functions/charge/index.ts.",
        "Replace it with `const id = globalThis.crypto.randomUUID();`.",
        "Read it with `const parsed = JSON.parse(raw); Object.keys(parsed).length`.",
        "Import it: `import { randomUUID, timingSafeEqual } from \"crypto\"`.",
        "Compare with `crypto.timingSafeEqual(a, b)` and encode with `new URLSearchParams()`.",
        "Await `Promise.allSettled(jobs)` and use `Array.isArray(rows)`.",
    ] {
        assert_eq!(find_ungrounded_identifier(fix, evidence), None, "{fix}");
    }
}

/// D7: hearth's fix stage gave up on a finding because the model wrote `process.env.TZ`, in
/// prose, to pin the timezone of a test (the stored rejection). Unquoted, the word reads as
/// a file name (stem `process.env`, extension `TZ`), and `TZ` is the operating system's own
/// variable, not one the repository chose. A platform variable is accepted however it is
/// written; an application's own variable (`process.env.CHARGE_SIGNING_SECRET`) is not.
#[test]
fn find_ungrounded_identifier_accepts_platform_environment_variables() {
    let evidence = "tests/format.test.ts line 10 calls formatDate with a fixed ISO input";
    for fix in [
        "Pin the timezone for this assertion by setting the test's timezone to a fixed zone \
             before calling `formatDate` in tests/format.test.ts (for example via a test setup \
             setting process.env.TZ to UTC), so the '2025' check does not depend on the machine.",
        "Set `process.env.TZ` to UTC in the test setup.",
        "Run it with `process.env.TZ = 'UTC'` before `formatDate`.",
        "Guard on `process.env.NODE_ENV` in tests/format.test.ts.",
    ] {
        assert_eq!(find_ungrounded_identifier(fix, evidence), None, "{fix}");
    }
    assert_eq!(
        find_ungrounded_identifier("Read `process.env.CHARGE_SIGNING_SECRET` first.", evidence),
        Some("process.env.CHARGE_SIGNING_SECRET".to_string())
    );
}

/// D3: a known ESLint rule name (core, `react-hooks/`, `@typescript-eslint/`) is not invented.
#[test]
fn find_ungrounded_identifier_accepts_known_eslint_rule_names() {
    let evidence = "components/Panel.tsx line 12";
    for fix in [
        "Fix the hook call that `react-hooks/rules-of-hooks` reports in components/Panel.tsx.",
        "Resolve `react-hooks/exhaustive-deps` by listing the dependency.",
        "Remove the assertion that `@typescript-eslint/no-non-null-assertion` flags.",
        "Turn `no-console` on for the build.",
        "Enable `security/detect-object-injection` and `eqeqeq`.",
    ] {
        assert_eq!(find_ungrounded_identifier(fix, evidence), None, "{fix}");
    }
}

/// D3: an invented API or rule name is still refused, including one that borrows a standard
/// namespace or a known plugin prefix.
#[test]
fn find_ungrounded_identifier_still_rejects_invented_api_and_rule_names() {
    let evidence = "supabase/functions/charge/index.ts uses amount_cents";
    for (fix, bad) in [
        ("Use `Number.toCents(amount_cents)`.", "toCents"),
        ("Use `const id = crypto.makeToken();`.", "makeToken"),
        ("Call `JSON.parseSafely(raw)`.", "parseSafely"),
        (
            "Fix what `react-hooks/hook-order-checker` reports.",
            "react-hooks/hook-order-checker",
        ),
        ("Apply `no-magic-cents`.", "no-magic-cents"),
        (
            "Read `process.env.CHARGE_SIGNING_SECRET` first.",
            "process.env.CHARGE_SIGNING_SECRET",
        ),
    ] {
        let found = find_ungrounded_identifier(fix, evidence);
        assert!(
            found.as_deref().is_some_and(|f| f.contains(bad)),
            "{fix} -> {found:?}"
        );
    }
}

#[test]
fn find_ungrounded_identifier_accepts_prose_with_no_named_identifiers() {
    let evidence = "some finding evidence";
    let fix = "Validate the input before using it and return an error otherwise.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);
}

#[test]
fn find_ungrounded_identifier_accepts_generic_literals_absent_from_evidence() {
    let evidence = "the config sets a flag";
    let fix = "Change the flag from `false` to `true`.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);
}

#[test]
fn find_ungrounded_identifier_rejects_a_backticked_symbol_absent_from_evidence() {
    let evidence = "app/auth/signout/route.ts uses `rawRedirect` at line 12";
    let fix = "Use `totallyInventedHelper` instead.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("totallyInventedHelper".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_rejects_a_file_path_absent_from_evidence() {
    let evidence = "app/auth/signout/route.ts uses `rawRedirect` at line 12";
    let fix = "Move this logic into lib/nonexistent-helper.ts instead.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("lib/nonexistent-helper.ts".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_accepts_a_code_snippet_whose_names_are_all_ordinary_words() {
    // The fix IS the new code, so the snippet is not in the evidence; its plain words
    // (`err`, `console`, `process`, `catch`) are not identifiers to ground.
    let evidence = "scripts/seed.ts line 3: main();";
    let fix = "Wrap the entry point: `main().catch((err) => { console.error(err); \
                    process.exit(1); })`.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);
}

#[test]
fn find_ungrounded_identifier_checks_only_identifier_like_names_inside_a_snippet() {
    let evidence = "create table public.member_contacts (id uuid, owner_id uuid)";
    let fix = "Run `alter table public.member_contacts enable row level security;`.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);

    let fix = "Run `alter table public.billing_accounts enable row level security;`.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("billing_accounts".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_rejects_an_invented_camel_case_symbol_inside_a_snippet() {
    let evidence = "app/api/keys/route.ts calls `loadKey` at line 4";
    let fix = "Call `await rotateServiceKey(client)` before returning.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("rotateServiceKey".to_string())
    );
    let fix = "Use `const res = NextResponse.next({ request })` here.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("NextResponse".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_rejects_an_invented_table_or_env_var_inside_a_snippet() {
    // A schema-qualified name (`public.documents`) names a table or function somebody chose,
    // even when it is one lowercase word; an environment variable read (`process.env.SECRET`)
    // is a chosen name even when it has no underscore. Neither is in this evidence.
    let evidence =
            "supabase/migrations/0004_contacts.sql: create table public.member_contacts (id uuid, owner_id uuid)";
    let fix = "Run `alter table public.documents enable row level security;` in the migration.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("documents".to_string())
    );
    let fix = "Read it as `const key = process.env.SECRET;` instead of hard-coding it.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("SECRET".to_string())
    );
    // The real table, and a schema-qualified CALL (`auth.uid()`), are fine.
    let fix = "Run `alter table public.member_contacts enable row level security;` then add \
                   `create policy p on public.member_contacts using (owner_id = auth.uid());`.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);
}

#[test]
fn find_ungrounded_identifier_still_rejects_a_bare_invented_symbol() {
    let evidence = "app/api/keys/route.ts calls `loadKey` at line 4";
    assert_eq!(
        find_ungrounded_identifier("Replace it with `rotateServiceKey`.", evidence),
        Some("rotateServiceKey".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_skips_universal_names_absent_from_the_evidence() {
    let evidence = "app/api/keys/route.ts line 4";
    for fix in [
        "Read the `Authorization` header.",
        "Close the client in a `finally` block.",
        "Log it with `console.error` and rethrow.",
        "Move it under `devDependencies`.",
        "Wrap it in `try` and `catch`.",
        "Log with console.error(err) in a catch block.",
    ] {
        assert_eq!(find_ungrounded_identifier(fix, evidence), None, "{fix}");
    }
}

/// A framework name written with its dot (`Next.js`), an abbreviation (`e.g.`) and the `void`
/// keyword look like a file or a chosen name to the regexes but are no name the repository
/// could hold: the stored efficiency runs lost eight fixes to them.
#[test]
fn find_ungrounded_identifier_skips_framework_names_abbreviations_and_void() {
    let evidence = "app/api/keys/route.ts line 4";
    for fix in [
            "Move the charge into a Next.js route handler in app/api/keys/route.ts.",
            "Add a client query library (e.g. a cached fetch) only with an explicit decision, i.e. in the PR.",
            "Return `void` from the handler and log instead.",
            "Keep the handler in app/api/keys/route.ts and declare it `async function run(): Promise<void>`.",
        ] {
            assert_eq!(find_ungrounded_identifier(fix, evidence), None, "{fix}");
        }
    // The list is not a blanket pass for dotted names: an unknown file is still rejected.
    assert_eq!(
        find_ungrounded_identifier("Move it into lib/charge.ts.", evidence),
        Some("lib/charge.ts".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_still_rejects_a_path_named_inside_a_snippet() {
    let evidence = "app/api/keys/route.ts line 4";
    let fix = "Add `import { admin } from \"lib/supabase/missing.ts\"` at the top.";
    assert_eq!(
        find_ungrounded_identifier(fix, evidence),
        Some("lib/supabase/missing.ts".to_string())
    );
}

#[test]
fn find_ungrounded_identifier_does_not_read_a_member_call_in_a_snippet_as_a_file() {
    let evidence = "app/api/keys/route.ts line 4";
    let fix = "Parse it with `const body = await req.json()` and validate it.";
    assert_eq!(find_ungrounded_identifier(fix, evidence), None);
}

#[test]
fn the_fix_prompt_and_the_checker_agree_on_snippets() {
    let prompt = fix_specific_system_prompt();
    assert!(prompt.contains("code snippet"));
    assert!(prompt.contains("describe it in plain words"));
    // The prompt invites a snippet, and the checker accepts one made of ordinary words.
    assert_eq!(
        find_ungrounded_identifier("Add `if (!user) { return null; }` first.", "x"),
        None
    );
}

// ── fix_contradicts_detail (pure, rule-based) ───────────────────────────────────────

#[test]
fn fix_contradicts_detail_rejects_enabling_a_setting_the_detail_says_is_already_on() {
    let detail = "Strict mode is already enabled in tsconfig.json; foo.ts still uses \
                       `any` casts that bypass its checks.";
    let fix = "Enable strict mode in tsconfig.json for this file.";
    assert!(
        fix_contradicts_detail(fix, detail).is_some(),
        "must flag a fix that tells the client to enable what the detail says is already on"
    );
}

#[test]
fn fix_contradicts_detail_generalizes_beyond_strict_mode() {
    // Same contradiction SHAPE, a different subject (RLS) — the check must not be
    // hardcoded to the literal word "strict" (general-fixes-not-fixture-bandaids).
    let detail = "Row-level security is already enabled on the orders table, but no \
                       policy restricts SELECT to the owning user.";
    let fix = "Enable row-level security on the orders table.";
    assert!(fix_contradicts_detail(fix, detail).is_some());
}

#[test]
fn fix_contradicts_detail_spares_a_fix_that_addresses_the_real_gap() {
    let detail = "Strict mode is already enabled in tsconfig.json; foo.ts still uses \
                       `any` casts that bypass its checks.";
    let fix = "Replace the `any` casts in foo.ts with explicit parameter types so strict \
                    mode's checks actually apply to this file.";
    assert_eq!(fix_contradicts_detail(fix, detail), None);
}

#[test]
fn fix_contradicts_detail_spares_a_detail_with_no_already_marker() {
    let detail = "This endpoint has no authorization check on the delete path.";
    let fix = "Add an ownership check before performing the delete.";
    assert_eq!(fix_contradicts_detail(fix, detail), None);
}

// ── fix_leaks_methodology (pure) ─────────────────────────────────────────────────────

#[test]
fn fix_leaks_methodology_rejects_a_fix_describing_detection() {
    let fix = "Camerata detected this via a regex scan; fix the SQL concatenation.";
    assert!(fix_leaks_methodology(fix).is_some());
}

#[test]
fn fix_leaks_methodology_accepts_a_clean_remediation_only_fix() {
    let fix = "Use a parameterized query instead of concatenating `user_id` into the SQL \
                    string in db/orders.rs.";
    assert_eq!(fix_leaks_methodology(fix), None);
}

// ── parse_fix_specifics (pure) ───────────────────────────────────────────────────────

#[test]
fn parse_fix_specifics_reads_a_well_formed_response() {
    let raw =
        r#"{"fixes":[{"id":"a1b2c3","fix":"Do the thing."},{"id":"d4e5f6","fix":"Do another."}]}"#;
    let out = parse_fix_specifics(raw);
    assert_eq!(out.get("a1b2c3").map(String::as_str), Some("Do the thing."));
    assert_eq!(out.get("d4e5f6").map(String::as_str), Some("Do another."));
    assert_eq!(out.len(), 2);
    // A position is no address any more: an entry with only an `index` matches nothing.
    assert!(parse_fix_specifics(r#"{"fixes":[{"index":0,"fix":"Do it."}]}"#).is_empty());
}

#[test]
fn parse_fix_specifics_is_empty_on_garbage_input() {
    assert!(parse_fix_specifics("not json at all").is_empty());
    assert!(parse_fix_specifics(r#"{"nope":true}"#).is_empty());
    assert!(parse_fix_specifics(r#"{"fixes":[{"id":"a1b2c3","fix":""}]}"#).is_empty());
}

// ── generate_fix_specifics (async, stubbed LlmPort — no real model calls) ──────────

/// Returns a DIFFERENT canned response on each successive call (round-robin once
/// exhausted) — drives the regenerate-on-rejection path deterministically: round 0
/// returns a bad fix, round 1 returns a corrected one.
pub(super) struct SequencedCompleter {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    pub(super) calls: std::sync::atomic::AtomicUsize,
}

impl SequencedCompleter {
    pub(super) fn new(responses: &[&str]) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LlmPort for SequencedCompleter {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut q = self.responses.lock().unwrap();
        let text = if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().cloned().unwrap_or_default()
        };
        let text = super::reply_schemas::check::address_by_id(&req.prompt, &text);
        Ok(LlmResponse {
            text,
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn generate_fix_specifics_happy_path_grounds_and_populates_every_finding() {
    let files = vec![(
        "app/auth/signout/route.ts".to_string(),
        "export function safeInternalPath(p: string) {}\nexport function handler() { \
             rawRedirect(p) }"
            .to_string(),
    )];
    let f = fx(
        "SEC-OPEN-REDIRECT-1",
        "app/auth/signout/route.ts",
        2,
        "rawRedirect(p) is called with an unvalidated path — an open redirect.",
        "rawRedirect(p)",
    );
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts."}]}"#,
    ]);
    let meter = UsageMeter::default();
    let (out, _) =
        generate_fix_specifics(&completer, "o/r", vec![f], &files, None, Some(&meter), None).await;
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts.")
    );
    assert!(!out[0].needs_review);
    assert_eq!(meter.snapshot().calls, 1);
}

// ── Resume (design 6.2): fix generation and alternatives keep a checkpoint per run ───────

#[tokio::test]
async fn fix_generation_resumes_from_its_checkpoint_without_asking_again() {
    let dir = tempfile::tempdir().unwrap();
    let files = vec![(
        "app/auth/signout/route.ts".to_string(),
        "export function safeInternalPath(p: string) {}\nexport function handler() { \
             rawRedirect(p) }"
            .to_string(),
    )];
    let mk = || {
        fx(
            "SEC-OPEN-REDIRECT-1",
            "app/auth/signout/route.ts",
            2,
            "rawRedirect(p) is called with an unvalidated path \u{2014} an open redirect.",
            "rawRedirect(p)",
        )
    };
    let reply = r#"{"fixes":[{"index":0,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts."}]}"#;

    let first = SequencedCompleter::new(&[reply]);
    let (out1, _) = crate::checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        generate_fix_specifics(&first, "o/r", vec![mk()], &files, None, None, None),
    )
    .await;
    assert_eq!(first.calls.load(std::sync::atomic::Ordering::Relaxed), 1);

    // The same run after a restart: the finished batch is not asked again, its usage is
    // reported, and the fix it produced is on the row.
    let second = SequencedCompleter::new(&[reply]);
    let meter = UsageMeter::default();
    let (out2, failed) = crate::checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        generate_fix_specifics(&second, "o/r", vec![mk()], &files, None, Some(&meter), None),
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        second.calls.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "nothing already absorbed is asked again"
    );
    assert_eq!(out2[0].fix_specific, out1[0].fix_specific);
    assert!(out2[0].fix_specific.is_some());
    assert_eq!(meter.snapshot().input_tokens, 100, "the usage is not zero");
}

#[tokio::test]
async fn alternatives_resume_from_their_checkpoint_without_asking_again() {
    let dir = tempfile::tempdir().unwrap();
    let resp_text = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","applicable":true,"evidence":"a.rs:1"}]}"#;
    let files = vec![("a.rs".to_string(), "fn f() {}".to_string())];
    let alt = two_option_alternatives("MULTI-RULE-1", Some("opt-a"));
    let run = |completer: SequencedCompleter, meter: UsageMeter| {
        let files = files.clone();
        let alt = alt.clone();
        let path = dir.path().to_path_buf();
        async move {
            let res = crate::checkpoint::with_run_checkpoints(
                Some(path),
                recommend_alternatives(
                    &completer,
                    "me/api",
                    &files,
                    &files,
                    &crate::prompt_frame::FrameNonce::generate(),
                    &[alt],
                    None,
                    None,
                    Some(&meter),
                ),
            )
            .await;
            (res, completer, meter)
        }
    };
    let (first, c1, _) = run(SequencedCompleter::new(&[resp_text]), UsageMeter::default()).await;
    assert_eq!(c1.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    let (second, c2, meter) =
        run(SequencedCompleter::new(&[resp_text]), UsageMeter::default()).await;
    assert_eq!(
        c2.calls.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "nothing already absorbed is asked again"
    );
    assert_eq!(
        second.unwrap().0.len(),
        first.unwrap().0.len(),
        "the same recommendations"
    );
    assert_eq!(meter.snapshot().input_tokens, 100, "the usage is not zero");
}

/// A row that states a convention (or an assessment gap) has no defect to fix. D2: the fix
/// stage never sends it to the model, so the model's correct "nothing to fix" is never
/// counted as a failure and re-asked, and the stage reports no failed part for it.
fn convention_row(path: &str) -> Finding {
    use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
    let mut f = fx(
        "ARCH-MONOLITH-FIRST-1",
        path,
        0,
        "The codebase consistently keeps one deployable.",
        "",
    );
    f.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "monolith".to_string(),
        convention_label: "Monolith first".to_string(),
    }));
    f
}

#[tokio::test]
async fn generate_fix_specifics_never_sends_a_convention_observed_row_to_the_model() {
    let completer = SequencedCompleter::new(&[r#"{"fixes":[]}"#]);
    let meter = UsageMeter::default();
    let (out, failed) = generate_fix_specifics(
        &completer,
        "o/r",
        vec![convention_row("a.ts")],
        &[],
        None,
        Some(&meter),
        None,
    )
    .await;
    assert_eq!(
        meter.snapshot().calls,
        0,
        "no model call for a convention row"
    );
    assert_eq!(
        completer.calls.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(failed.is_empty(), "no failed part: {failed:?}");
    assert_eq!(out.len(), 1, "the row stays in the report");
    assert_eq!(out[0].fix_specific, None);
}

/// Mixed with a defect, only the defect's block is in the prompt, and the convention row is
/// neither asked about in the first round nor in any retry.
#[tokio::test]
async fn generate_fix_specifics_asks_only_about_the_defect_beside_a_convention_row() {
    let completer = CapturingCompleter::default();
    let defect = fx(
        "SEC-OPEN-REDIRECT-1",
        "app/defect.ts",
        2,
        "rawRedirect(p) is called with an unvalidated path.",
        "rawRedirect(p)",
    );
    let (out, _) = generate_fix_specifics(
        &completer,
        "o/r",
        vec![convention_row("app/convention.ts"), defect],
        &[],
        None,
        None,
        None,
    )
    .await;
    let seen = completer.seen.lock().unwrap();
    assert!(!seen.is_empty(), "the defect is still sent");
    for req in seen.iter() {
        assert!(req.prompt.contains("app/defect.ts"), "{}", req.prompt);
        assert!(
            !req.prompt.contains("app/convention.ts"),
            "the convention row was sent: {}",
            req.prompt
        );
    }
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].fix_specific, None);
}

/// A finding a delta retest carried forward already has its codebase-specific fix. Asking the
/// model again would re-send the unchanged file's code to reproduce it, so it is left alone
/// (and the pass makes no call at all when nothing else needs one).
#[tokio::test]
async fn generate_fix_specifics_keeps_a_fix_the_finding_already_carries() {
    let mut carried = fx(
        "SEC-OPEN-REDIRECT-1",
        "app/a.ts",
        2,
        "rawRedirect(p) is called with an unvalidated path.",
        "rawRedirect(p)",
    );
    carried.fix_specific = Some("The fix from the inspection.".to_string());
    let fresh = fx(
        "SEC-OPEN-REDIRECT-1",
        "app/b.ts",
        2,
        "rawRedirect(q) is called with an unvalidated path.",
        "rawRedirect(q)",
    );
    let files = vec![
        ("app/a.ts".to_string(), "x\nrawRedirect(p)".to_string()),
        ("app/b.ts".to_string(), "y\nrawRedirect(q)".to_string()),
    ];
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Validate the path in app/b.ts before the redirect."}]}"#,
    ]);

    let (out, _) = generate_fix_specifics(
        &completer,
        "o/r",
        vec![carried, fresh],
        &files,
        None,
        None,
        None,
    )
    .await;

    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("The fix from the inspection."),
        "the carried finding keeps its fix"
    );
    assert_eq!(
        out[1].fix_specific.as_deref(),
        Some("Validate the path in app/b.ts before the redirect.")
    );
    assert_eq!(
        completer.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "one call, for the fresh finding only"
    );
}

#[tokio::test]
async fn generate_fix_specifics_makes_no_call_when_every_finding_already_has_a_fix() {
    let mut carried = fx("SEC-OPEN-REDIRECT-1", "app/a.ts", 2, "d", "rawRedirect(p)");
    carried.fix_specific = Some("Kept.".to_string());
    let completer = FailingCompleter;
    let (out, failed) =
        generate_fix_specifics(&completer, "o/r", vec![carried], &[], None, None, None).await;
    assert_eq!(out[0].fix_specific.as_deref(), Some("Kept."));
    assert!(failed.is_empty());
}

#[tokio::test]
async fn generate_fix_specifics_skips_dependency_audit_findings() {
    let f = fx(
        crate::dep_audit::DEP_AUDIT_RULE_ID,
        "package-lock.json",
        0,
        "some-pkg@1.0.0 has a known CVE",
        "some-pkg@1.0.0",
    );
    let completer = FailingCompleter;
    let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].fix_specific, None);
    // No model call was ever attempted — FailingCompleter would have surfaced it as a
    // needs-review tag if `generate_fix_specifics` had tried and failed; it never tried.
    assert!(!out[0].needs_review);
}

/// P4 (floor findings get finding-level treatment): confirms `generate_fix_specifics`'s
/// `indices` filter excludes ONLY `DEP_AUDIT_RULE_ID` — a deterministic FLOOR finding (a
/// real `AUDIT_RULES` rule id, not an `AI-`-prefixed one) is NOT skipped, so every curated
/// floor finding still gets a `fix_specific` on a run where the AI review tier is enabled
/// (the gate is `run_ai_review`, checked one layer up in `onboard::audit_repos` — this
/// function itself carries no AI-vs-floor distinction at all).
#[tokio::test]
async fn generate_fix_specifics_includes_deterministic_floor_findings_not_just_ai_tier() {
    let files = vec![(".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string())];
    let f = fx(
        "SEC-NO-SECRET-FILE-1",
        ".env",
        0,
        "SEC-NO-SECRET-FILE-1: path `.env` is a secret-bearing file type",
        ".env",
    );
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Remove .env from the repository and its git history, and rotate the exposed credential."}]}"#,
    ]);
    let (out, _) =
        generate_fix_specifics(&completer, "o/r", vec![f], &files, None, None, None).await;
    assert_eq!(out.len(), 1);
    assert!(
        out[0].fix_specific.is_some(),
        "a deterministic floor finding must be included in the fix-generation pass, not \
             silently skipped alongside dependency-audit findings"
    );
}

#[tokio::test]
async fn generate_fix_specifics_total_failure_never_hedges_or_tags_detail() {
    // C3-1b: fix-generation exhausting every retry is a PIPELINE failure, not a
    // confidence judgement — it must never set `needs_review`, never touch `confidence`,
    // and never write pipeline-state text into the client-facing `detail`. Only
    // `fix_specific` stays `None` (never fabricated).
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
    // A completer that returns unparseable junk every time — every round yields nothing
    // usable.
    let completer = StubCompleter {
        text: "complete garbage, not json".to_string(),
    };
    let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].fix_specific, None, "must never fabricate a fix");
    assert!(
        !out[0].needs_review,
        "a fix-generation failure must never hedge the finding"
    );
    assert_eq!(
        out[0].confidence, None,
        "a fix-generation failure must never touch calibration confidence"
    );
    assert_eq!(
        out[0].detail, "some real defect",
        "detail must be untouched — no bracketed pipeline-state text: {}",
        out[0].detail
    );
}

#[tokio::test]
async fn generate_fix_specifics_transport_failure_never_hedges_or_tags_detail() {
    // The OTHER failure mode: the LLM is unreachable entirely (not just returning junk).
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
    let (out, _) =
        generate_fix_specifics(&FailingCompleter, "o/r", vec![f], &[], None, None, None).await;
    assert_eq!(out[0].fix_specific, None);
    assert!(!out[0].needs_review);
    assert_eq!(out[0].detail, "some real defect");
    assert!(
        !out[0].detail.contains("["),
        "no bracketed pipeline-state text"
    );
}

#[tokio::test]
async fn generate_fix_specifics_regenerates_when_the_fix_names_an_ungrounded_identifier() {
    let f = fx(
        "ARCH-1",
        "a.rs",
        10,
        "the handler never checks ownership before deleting",
        "delete_order(id)",
    );
    let completer = SequencedCompleter::new(&[
        // Round 0: names a symbol nowhere in the evidence — must be rejected.
        r#"{"fixes":[{"index":0,"fix":"Use `totallyInventedGuard` before the delete."}]}"#,
        // Round 1 (after feedback): a grounded correction.
        r#"{"fixes":[{"index":0,"fix":"Check the caller owns `id` before calling delete_order(id)."}]}"#,
    ]);
    let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Check the caller owns `id` before calling delete_order(id).")
    );
    assert!(!out[0].needs_review);
}

#[tokio::test]
async fn generate_fix_specifics_regenerates_when_the_fix_contradicts_the_detail() {
    let f = fx(
        "JAVASCRIPT-TS-STRICT-1",
        "tsconfig.json",
        1,
        "Strict mode is already enabled in tsconfig.json; foo.ts still uses `any` casts \
             that bypass its checks.",
        "\"strict\": true",
    );
    let completer = SequencedCompleter::new(&[
        // Round 0: contradicts the detail (tells the client to do what's already done).
        r#"{"fixes":[{"index":0,"fix":"Enable strict mode in tsconfig.json."}]}"#,
        // Round 1: addresses the real gap instead.
        r#"{"fixes":[{"index":0,"fix":"Replace the `any` casts in foo.ts with explicit types."}]}"#,
    ]);
    let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Replace the `any` casts in foo.ts with explicit types.")
    );
    assert!(!out[0].needs_review);
}

#[tokio::test]
async fn generate_fix_specifics_never_leaks_methodology_in_the_saved_fix() {
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Camerata detected this via a regex scan; fix the concatenation."}]}"#,
        r#"{"fixes":[{"index":0,"fix":"Use a parameterized query in a.rs instead of concatenation."}]}"#,
    ]);
    let (out, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
    let saved = out[0].fix_specific.as_deref().unwrap_or_default();
    assert!(!saved.to_ascii_lowercase().contains("camerata"));
    assert!(!saved.to_ascii_lowercase().contains("regex"));
    assert_eq!(
        saved,
        "Use a parameterized query in a.rs instead of concatenation."
    );
}

/// M4 (was: gave up after every batched retry plus a solo retry, left `fix_specific` empty
/// and made a failed part). A fix that fails the grounding check gets ONE re-ask; if the
/// second reply fails too, the better one ships with the `fix_ungrounded` flag, the run is
/// complete, and the two calls are both folded into the meter.
#[tokio::test]
async fn generate_fix_specifics_ships_a_twice_ungrounded_fix_flagged_after_two_calls() {
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#,
    ]);
    let meter = UsageMeter::default();
    let (out, failed_passes) =
        generate_fix_specifics(&completer, "o/r", vec![f], &[], None, Some(&meter), None).await;
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Use `neverInEvidence` here.")
    );
    assert!(!out[0].needs_review);
    assert_eq!(
        crate::review_flags::flags_of(&out[0]),
        vec!["fix_ungrounded".to_string()]
    );
    // One attempt and one re-ask: where there were MAX_FIX_REGENERATIONS + 2 calls.
    assert_eq!(meter.snapshot().calls, 2);
    assert!(
        failed_passes.is_empty(),
        "wording never makes a failed part: {failed_passes:?}"
    );
}

/// The better of two failing fix replies ships: the one that failed fewer checks, in either
/// order. Here the first fails grounding only and the second fails grounding and leaks the
/// tool's name, so the first ships and the flags name only what it failed.
#[tokio::test]
async fn generate_fix_specifics_ships_the_better_of_two_failing_fixes() {
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#,
        r#"{"fixes":[{"index":0,"fix":"Camerata says use `alsoNeverInEvidence` here."}]}"#,
    ]);
    let (out, failed_passes) =
        generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;
    assert!(failed_passes.is_empty(), "{failed_passes:?}");
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Use `neverInEvidence` here.")
    );
    assert_eq!(
        crate::review_flags::flags_of(&out[0]),
        vec!["fix_ungrounded".to_string()]
    );
    assert_eq!(
        completer.calls.load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

// ── D8: a fix rejected in every round is a standard fix, not an incomplete run ─────

/// M4 (was: a fix turned down in every round shipped the rule's authored fix as a standard fix).
/// A fix that is twice ungrounded now ships as the model wrote it with the `fix_ungrounded`
/// flag: the replay of 40 such rejections found no real error among them, and a reviewer reads
/// the flag. The part is not a failed part, so the run is complete.
#[tokio::test]
async fn d8_a_fix_turned_down_for_grounding_twice_ships_flagged_and_is_not_a_failed_part() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, _errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    let mut f = fx(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "no RLS evidence for `profiles`",
        "CREATE TABLE profiles (...)",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];
    // Answers every round with a fix naming something that exists nowhere.
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#,
    ]);
    let (out, failed_passes) = generate_fix_specifics(
        &completer,
        "o/r",
        vec![f],
        &files,
        None,
        None,
        Some(&corpus),
    )
    .await;
    assert!(
        failed_passes.is_empty(),
        "a fix turned down for wording must not be a failed part: {failed_passes:?}"
    );
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Use `neverInEvidence` here.")
    );
    assert!(!out[0]
        .captures
        .contains_key(crate::report_export::FIX_SOURCE_CAPTURE));
    assert_eq!(
        crate::review_flags::flags_of(&out[0]),
        vec!["fix_ungrounded".to_string()]
    );
    assert_eq!(
        completer.calls.load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    let mut report = crate::onboard::ScanReport::gated(&[]);
    report.failed_passes = failed_passes;
    assert!(report.incomplete_reasons().is_empty());
}

/// A fix that tells the client to redo what the detail says is done, or that describes the
/// tool, is worse than the rule's own authored fix: when the second reply is still that, the
/// authored fix ships as a standard fix, and the flag records what the model wrote.
#[tokio::test]
async fn a_contradicting_or_leaking_fix_twice_ships_the_authored_fix_flagged() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, _errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    for (detail, reply, flag) in [
        (
            "Row-level security is already enabled on the profiles table, but no policy \
                 restricts SELECT to the owning user.",
            r#"{"fixes":[{"index":0,"fix":"Enable row-level security on the profiles table."}]}"#,
            "contradiction",
        ),
        (
            "no RLS evidence for `profiles`",
            r#"{"fixes":[{"index":0,"fix":"Camerata detected this; enable RLS on profiles."}]}"#,
            "methodology_leak",
        ),
    ] {
        let mut f = fx(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            detail,
            "CREATE TABLE profiles (...)",
        );
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        let files = vec![(
            "supabase/migrations/1.sql".to_string(),
            "CREATE TABLE profiles (id int);\n".to_string(),
        )];
        let completer = SequencedCompleter::new(&[reply]);
        let (out, failed_passes) = generate_fix_specifics(
            &completer,
            "o/r",
            vec![f],
            &files,
            None,
            None,
            Some(&corpus),
        )
        .await;
        assert!(failed_passes.is_empty(), "{flag}: {failed_passes:?}");
        assert_eq!(
            completer.calls.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "{flag}"
        );
        let fix = out[0]
            .fix_specific
            .as_deref()
            .expect("the authored fix ships");
        assert!(fix.contains("profiles"), "{flag}: {fix}");
        assert!(!fix.contains("Camerata"), "{flag}: {fix}");
        assert_eq!(
            out[0]
                .captures
                .get(crate::report_export::FIX_SOURCE_CAPTURE)
                .map(String::as_str),
            Some(crate::report_export::STANDARD_FIX),
            "{flag}"
        );
        assert_eq!(
            crate::review_flags::flags_of(&out[0]),
            vec![flag.to_string()]
        );
    }
}

/// The ledger's fix flag counts reconcile with the flags on the rows.
#[tokio::test]
async fn the_fix_flag_counts_reconcile_with_the_flags_on_the_rows() {
    let files = vec![(
        "a.rs".to_string(),
        "fn a() { delete_order(id) }\nfn b() { other(id) }\n".to_string(),
    )];
    let flagged = fx(
        "ARCH-1",
        "a.rs",
        1,
        "no ownership check",
        "delete_order(id)",
    );
    let clean = fx("ARCH-2", "a.rs", 2, "no validation", "other(id)");
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."},{"index":1,"fix":"Validate `id` before other(id)."}]}"#,
        r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#,
    ]);
    let meter = UsageMeter::default();
    let (out, failed_passes) = generate_fix_specifics(
        &completer,
        "o/r",
        vec![flagged, clean],
        &files,
        None,
        Some(&meter),
        None,
    )
    .await;
    assert!(failed_passes.is_empty(), "{failed_passes:?}");
    assert_eq!(
        crate::review_flags::flags_of(&out[0]),
        vec!["fix_ungrounded".to_string()]
    );
    assert!(crate::review_flags::flags_of(&out[1]).is_empty());
    let flags = meter
        .stage_telemetry(0)
        .review_flags
        .expect("a flag was recorded");
    assert_eq!((flags.detail, flags.fix), (0, 1), "{flags:?}");
    assert!(flags.reconciles(), "{flags:?}");
    let on_rows = crate::review_flags::count_on_rows(&out);
    assert_eq!(
        on_rows.get(&crate::review_flags::FlagStage::Fix),
        Some(&flags.fix)
    );
    assert_eq!(on_rows.len(), 1);
}

/// The mirror: fix work that never ran (every call failed in transport) is still a failed
/// part, and so still makes the run incomplete and resumable.
#[tokio::test]
async fn d8_a_fix_stage_that_never_ran_is_still_a_failed_part() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, _errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    let mut f = fx(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "no RLS evidence for `profiles`",
        "CREATE TABLE profiles (...)",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];
    let (out, failed_passes) = generate_fix_specifics(
        &FailingCompleter,
        "o/r",
        vec![f],
        &files,
        None,
        None,
        Some(&corpus),
    )
    .await;
    assert_eq!(failed_passes.len(), 1, "{failed_passes:?}");
    assert!(
        !out[0]
            .captures
            .contains_key(crate::report_export::FIX_SOURCE_CAPTURE),
        "work that never ran is not a standard fix"
    );
    // D8 review 2: nothing is pre-filled either. A fix written into `fix_specific` here would
    // be carried forward by the scan cache, skipped by the next inspection's fix stage (it
    // skips a finding that has a fix) and labelled "Tailored fix" with no fix work done.
    assert_eq!(out[0].fix_specific, None);
    let mut report = crate::onboard::ScanReport::gated(&[]);
    report.failed_passes = failed_passes;
    assert!(!report.incomplete_reasons().is_empty());

    // The next inspection, from what the cache carried forward, sends it to the fix stage.
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Enable RLS on `profiles` in supabase/migrations/1.sql."}]}"#,
    ]);
    let (again, failed) =
        generate_fix_specifics(&completer, "o/r", out, &files, None, None, Some(&corpus)).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        completer.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the finding went to the model"
    );
    assert_eq!(
        again[0].fix_specific.as_deref(),
        Some("Enable RLS on `profiles` in supabase/migrations/1.sql.")
    );
    assert!(!again[0]
        .captures
        .contains_key(crate::report_export::FIX_SOURCE_CAPTURE));
}

/// Answers the first call with a fix naming something that exists nowhere, then fails every
/// later call in transport.
struct AnswersOnceThenFails {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for AnswersOnceThenFails {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        if self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            > 0
        {
            anyhow::bail!("simulated transport failure");
        }
        let text = super::reply_schemas::check::address_by_id(
            &req.prompt,
            r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#,
        );
        Ok(LlmResponse {
            text,
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// M4 (was D8 review 3: a finding answered and rejected once and then lost to transport was a
/// failed part, not a Standard fix). The stage did run on the finding and the model answered;
/// only the wording re-ask was lost. It settles from the reply it has, flagged, and is not a
/// failed part: wording never holds a run. The mirror above (every call lost) still is one.
#[tokio::test]
async fn d8_a_fix_answered_once_then_lost_to_transport_ships_the_reply_flagged() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, _errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    let mut f = fx(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "no RLS evidence for `profiles`",
        "CREATE TABLE profiles (...)",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];
    let completer = AnswersOnceThenFails {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let (out, failed_passes) = generate_fix_specifics(
        &completer,
        "o/r",
        vec![f],
        &files,
        None,
        None,
        Some(&corpus),
    )
    .await;
    assert!(failed_passes.is_empty(), "{failed_passes:?}");
    assert!(!out[0]
        .captures
        .contains_key(crate::report_export::FIX_SOURCE_CAPTURE));
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Use `neverInEvidence` here.")
    );
    assert_eq!(
        crate::review_flags::flags_of(&out[0]),
        vec!["fix_ungrounded".to_string()]
    );
}

// ── C6-B3: instantiated authored-remediation fallback on give-up ───────────────────

/// When fix-GENERATION never runs to an answer (transport unreachable — every batched retry
/// AND the C6-B3.4 solo retry fail), `fix_specific` stays `None` even though the rule
/// carries an authored remediation (D8): the part is a failed, resumable part, and a
/// pre-filled authored fix would be carried forward by the scan cache and later read as a
/// model fix. The instantiated Standard fix is pinned by the rejected-every-round D8 test.
#[tokio::test]
async fn generate_fix_specifics_leaves_fix_specific_empty_when_the_fix_stage_never_ran() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = fx(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "no RLS evidence for `profiles`",
        "CREATE TABLE profiles (...)",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    // The finding has surrounding code, so the model IS asked (and its transport is down);
    // a finding with no code around it skips the model entirely and never reaches this
    // give-up path (see `retry_tests`).
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];

    let (out, failed_passes) = generate_fix_specifics(
        &FailingCompleter,
        "o/r",
        vec![f],
        &files,
        None,
        None,
        Some(&corpus),
    )
    .await;

    // D8 review 2: a part that never ran (the transport was down) pre-fills nothing; the
    // export falls back to the rule's own authored fix (labelled a Standard fix), and the next
    // inspection sends the finding to the fix stage again.
    assert_eq!(
        out[0].fix_specific, None,
        "work that never ran must not leave a fix in fix_specific"
    );
    assert!(
        !out[0].needs_review,
        "a fix-generation failure never hedges the finding"
    );

    assert_eq!(
        failed_passes.len(),
        1,
        "the give-up must still be disclosed even though a usable fallback shipped: \
             {failed_passes:?}"
    );
    assert_eq!(failed_passes[0].pass, "fix-specific generation");
    assert!(
        failed_passes[0].reason.contains("gave up on 1 finding"),
        "the disclosure must carry the give-up COUNT: {}",
        failed_passes[0].reason
    );
    assert!(
        failed_passes[0]
            .reason
            .contains("1 have the rule's own authored fix"),
        "the disclosure must say how many unfinished findings have an authored fix: {}",
        failed_passes[0].reason
    );
}

/// The mirror case: the rule the finding cites has NO corpus entry at all (an invented
/// AI-tier rule id, or simply a rule this loaded corpus doesn't define), so there is
/// nothing to instantiate. `fix_specific` legitimately stays `None` — the give-up is
/// still disclosed, and the finding is never dropped; it is
/// `report_export::is_action_row_missing_a_fix`'s job (exercised in report_export.rs's
/// own tests) to re-route a row like this to "held" at export time rather than ship an
/// action-bucket row with no fix text anywhere.
#[tokio::test]
async fn generate_fix_specifics_leaves_fix_specific_none_when_the_rule_has_no_authored_remediation()
{
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let f = fx(
        "AI-CUSTOM-ARCH-RULE-1",
        "a.rs",
        10,
        "some real defect",
        "let x = 1;",
    );

    let (out, failed_passes) = generate_fix_specifics(
        &FailingCompleter,
        "o/r",
        vec![f],
        &[],
        None,
        None,
        Some(&corpus),
    )
    .await;

    assert_eq!(
        out[0].fix_specific, None,
        "nothing to instantiate — must never fabricate a fix"
    );
    assert!(!out[0].needs_review);
    assert_eq!(failed_passes.len(), 1);
    assert!(
        failed_passes[0]
            .reason
            .contains("0 have the rule's own authored fix"),
        "the disclosure must say explicitly that none of this run's unfinished findings \
             has an authored fix: {}",
        failed_passes[0].reason
    );
}

#[tokio::test]
async fn generate_fix_specifics_uses_surrounding_context_to_ground_identifiers() {
    // The fix names a symbol that's NOT in the finding's own snippet/detail but IS in the
    // surrounding file content passed via `files` — proving the context window (not just
    // the bare snippet) grounds the identifier check.
    let files = vec![(
        "lib/redirect.ts".to_string(),
        "// ...\n// ...\n// ...\n// ...\n// ...\nexport function safeInternalPath(p: string) \
             { return p; }\n// ...\n// ...\n// ...\n// ...\n"
            .to_string(),
    )];
    let f = fx(
        "SEC-OPEN-REDIRECT-1",
        "lib/redirect.ts",
        6,
        "this module has no validated-path helper in use at the call site",
        "return p;",
    );
    let completer = SequencedCompleter::new(&[
        r#"{"fixes":[{"index":0,"fix":"Route the redirect through `safeInternalPath`, defined right here in lib/redirect.ts."}]}"#,
    ]);
    let (out, _) =
        generate_fix_specifics(&completer, "o/r", vec![f], &files, None, None, None).await;
    assert!(
        out[0].fix_specific.is_some(),
        "the context window must ground `safeInternalPath` even though it's not in the \
             bare snippet/detail"
    );
}

/// max_tokens must SCALE with the finding count instead of the old hardcoded flat 4096 —
/// a full `FIX_BATCH_SIZE` chunk is deliberately sized (see that constant's doc comment)
/// to already exceed the old flat floor, since each fix is a full prose sentence.
///
/// `CapturingCompleter` always answers `"{}"` (no usable fix), so EVERY batched round
/// here is rejected and the C6-B3.4 solo retry round also runs afterward — that round
/// legitimately uses a SMALLER max_tokens (one finding, not `FIX_BATCH_SIZE`), so this
/// test only asserts over the BATCHED rounds' own requests (the first
/// `MAX_FIX_REGENERATIONS + 1` calls); the sibling
/// `generate_fix_specifics_solo_retry_round_uses_a_smaller_max_tokens_than_the_batched_rounds`
/// test below pins the solo round's own, smaller value.
#[tokio::test]
async fn generate_fix_specifics_scales_max_tokens_with_finding_count() {
    let completer = CapturingCompleter::default();
    let findings: Vec<Finding> = (0..FIX_BATCH_SIZE)
        .map(|i| fx("ARCH-1", "a.rs", i, "some real defect", "let x = 1;"))
        .collect();

    let (_, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

    let seen = completer.seen.lock().unwrap();
    let expected = fix_max_tokens(FIX_BATCH_SIZE);
    assert!(
        expected > 4096,
        "test setup must exercise the scaled-above-floor branch, got {expected}"
    );
    let batched_rounds = MAX_FIX_REGENERATIONS + 1;
    assert!(
        seen.len() >= batched_rounds,
        "expected at least {batched_rounds} batched-round calls, got {}",
        seen.len()
    );
    for req in seen.iter().take(batched_rounds) {
        assert_eq!(req.max_tokens, expected);
    }
}

/// C6-B3.4: the cheap-reliability solo retry round (batch_size = 1) deliberately uses a
/// SMALLER max_tokens than the batched rounds that preceded it — it covers exactly one
/// finding, not `FIX_BATCH_SIZE` of them. `CapturingCompleter` never returns a usable fix,
/// so every batched round AND the solo round all run; the LAST call this completer sees
/// is therefore from the solo round.
#[tokio::test]
async fn generate_fix_specifics_solo_retry_round_uses_a_smaller_max_tokens_than_the_batched_rounds()
{
    let completer = CapturingCompleter::default();
    let findings: Vec<Finding> = (0..FIX_BATCH_SIZE)
        .map(|i| fx("ARCH-1", "a.rs", i, "some real defect", "let x = 1;"))
        .collect();

    let (_, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

    let seen = completer.seen.lock().unwrap();
    let batched_rounds = MAX_FIX_REGENERATIONS + 1;
    let batched_max_tokens = fix_max_tokens(FIX_BATCH_SIZE);
    let solo_max_tokens = fix_max_tokens(1);
    assert!(
        seen.len() > batched_rounds,
        "expected solo-round calls after the batched rounds, got {} total calls",
        seen.len()
    );
    for req in seen.iter().skip(batched_rounds) {
        assert_eq!(
            req.max_tokens, solo_max_tokens,
            "every solo-round call must use the SMALLER single-finding max_tokens"
        );
        assert!(
            req.max_tokens < batched_max_tokens,
            "the solo round's max_tokens must be smaller than the batched rounds': {} vs {}",
            req.max_tokens,
            batched_max_tokens
        );
    }
}

/// A SMALL finding set must see ZERO behavior change — it stays pinned at the original
/// flat 4096 floor.
#[tokio::test]
async fn generate_fix_specifics_max_tokens_floor_unchanged_for_small_finding_sets() {
    let completer = CapturingCompleter::default();
    let f = fx("ARCH-1", "a.rs", 10, "some real defect", "let x = 1;");

    let (_, _) = generate_fix_specifics(&completer, "o/r", vec![f], &[], None, None, None).await;

    let seen = completer.seen.lock().unwrap();
    assert_eq!(seen[0].max_tokens, 4096);
}

/// A large finding set is split into bounded chunks and every finding survives the round
/// trip — no finding is dropped at a chunk boundary, and the completer sees the expected
/// chunk count.
#[tokio::test]
async fn generate_fix_specifics_chunks_large_finding_sets_and_merges_without_dropping() {
    let n = FIX_BATCH_SIZE * 2 + 3;
    let findings: Vec<Finding> = (0..n)
        .map(|i| fx("ARCH-1", "a.rs", i, "some real defect", "let x = 1;"))
        .collect();
    let entries: Vec<String> = (0..FIX_BATCH_SIZE)
            .map(|i| {
                format!(
                    r#"{{"index":{i},"fix":"Validate the input before using it and return an error otherwise."}}"#
                )
            })
            .collect();
    let canned = format!(r#"{{"fixes":[{}]}}"#, entries.join(","));
    let completer = CountingFixCompleter {
        calls: 0.into(),
        canned,
    };

    let (out, _) = generate_fix_specifics(&completer, "o/r", findings, &[], None, None, None).await;

    assert_eq!(out.len(), n, "no finding dropped across chunk boundaries");
    assert!(
        out.iter()
            .all(|f| f.fix_specific.is_some() && !f.needs_review),
        "every finding must get a fix: {out:?}"
    );
    let expected_chunks = n.div_ceil(FIX_BATCH_SIZE);
    assert_eq!(
        completer.calls.load(std::sync::atomic::Ordering::Relaxed),
        expected_chunks,
        "M findings must be split into the expected number of bounded chunks"
    );
}

/// A `LlmPort` that returns the SAME canned text on every call and counts how many
/// times it was invoked — used to assert the exact number of bounded chunks a large
/// finding/rule set is split into.
struct CountingFixCompleter {
    calls: std::sync::atomic::AtomicUsize,
    canned: String,
}

#[async_trait::async_trait]
impl LlmPort for CountingFixCompleter {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(LlmResponse {
            text: super::reply_schemas::check::address_by_id(&req.prompt, &self.canned),
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Floor credential rules never fuse with a NEIGHBOURING different secret ─────────────
//
// The cross-family merge's same-category signal is "same file, same category, within five
// lines or one construct". That is the right key for an authorization or injection pair,
// but two credentials are two defects (two values to rotate, two places to delete) however
// close they sit, and a secret row has no structural object for `objects_conflict` to tell
// them apart by: a redacted snippet names nothing. So the floor's private-key, vendor-token
// and Supabase service-role rows carry NO `secret-exposure` category from
// `categorize_rule_id`; the cockpit identifies them by rule id instead
// (`FindingView::is_secret_exposure`). A same-credential pair at the SAME line is already one
// row through `collapse_identical`, which needs no category.

fn floor_credential_row(rule: &str, line: usize, snippet: &str, detail: &str) -> Finding {
    Finding {
        repo: "acme/api".to_string(),
        path: "src/config.ts".to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "critical".to_string(),
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        status: "active".to_string(),
        located: true,
        effort: Some("low".to_string()),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

/// A committed private key at line 12 and a live Stripe key the model flagged at line 10 are
/// two credentials. With the floor row categorized `secret-exposure` they became ONE row
/// headlined "private key", the Stripe key demoted to a location of it, and one triage
/// decision ("false positive: fixture key") would have disposed of both.
#[test]
fn a_floor_private_key_and_a_different_ai_secret_two_lines_apart_stay_two_rows() {
    let key = floor_credential_row(
        "SEC-NO-PRIVATE-KEY-1",
        12,
        "-----BEGIN RSA PRIVATE KEY-----",
        "No private key material in source",
    );
    let mut stripe = floor_credential_row(
        "AI-SECRET-IN-CONFIG-1",
        10,
        "stripe: \"sk_live_…x7Yq (32 chars)\"",
        "A live Stripe secret key is hardcoded in the config object.",
    );
    stripe.confidence = Some("high".to_string());
    stripe.category = Some("secret-exposure".to_string());
    let out = cluster_heads(vec![stripe, key], &[]);
    assert_eq!(
        out.len(),
        2,
        "two different credentials two lines apart are two rows: {out:?}"
    );
    assert!(out.iter().all(|f| f.also_locations.is_empty()), "{out:?}");
}

/// A vendor token and a private key in ONE object literal are two credentials. With both
/// floor rows in one category the same-construct security override fused them, bypassing
/// the "two deterministic rows are two defects" guard.
#[test]
fn two_different_floor_credentials_in_one_construct_stay_two_rows() {
    let content = "const config = {\n  owner: \"acme\",\n  token: \"ghp_yyy\",\n  cert: 1,\n  \
                       key: `-----BEGIN RSA PRIVATE KEY-----`,\n};\n";
    let token = floor_credential_row(
        "SEC-NO-VENDOR-TOKEN-1",
        3,
        "token: \"ghp_…yyyy (40 chars)\"",
        "No vendor API tokens in source",
    );
    let key = floor_credential_row(
        "SEC-NO-PRIVATE-KEY-1",
        5,
        "key: `-----BEGIN RSA PRIVATE KEY-----`",
        "No private key material in source",
    );
    let files = vec![("src/config.ts".to_string(), content.to_string())];
    let out = cluster_heads(vec![token, key], &files);
    assert_eq!(
        out.len(),
        2,
        "two floor credential rules in one construct are two rows: {out:?}"
    );
    let ids: Vec<&str> = out.iter().map(|f| f.rule_id.as_str()).collect();
    assert!(ids.contains(&"SEC-NO-VENDOR-TOKEN-1") && ids.contains(&"SEC-NO-PRIVATE-KEY-1"));
}

/// The same credential flagged by the floor and by the model at the SAME line is still one
/// row: `collapse_identical` fuses exact co-location and needs no category for it.
#[test]
fn the_same_credential_at_the_same_line_is_two_linked_rows_across_rule_families() {
    let floor = floor_credential_row(
        "SEC-NO-VENDOR-TOKEN-1",
        10,
        "GITHUB_TOKEN = \"ghp_…k9Qz (40 chars)\"",
        "No vendor API tokens in source",
    );
    let mut model = floor_credential_row(
        "SEC-NO-HARDCODED-SECRETS-1",
        10,
        "GITHUB_TOKEN = \"ghp_…k9Qz (40 chars)\"",
        "A GitHub personal access token is hardcoded in GITHUB_TOKEN.",
    );
    model.confidence = Some("high".to_string());
    model.category = Some("secret-exposure".to_string());
    let content = (1..=12)
        .map(|n| {
            if n == 10 {
                "GITHUB_TOKEN = \"ghp_…k9Qz (40 chars)\"".to_string()
            } else {
                format!("const line{n} = {n};")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let files = vec![("src/config.ts".to_string(), content)];
    let rows = crate::review_tree::collapse_and_group(vec![model, floor], &files);
    assert_eq!(rows.len(), 2, "two rule families are two rows: {rows:?}");
    assert!(rows.iter().all(|r| r.severity == "critical"), "{rows:?}");
    let links = crate::review_tree::test_support::link_pairs(&rows);
    assert_eq!(
        links,
        vec![(
            "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            "SEC-NO-VENDOR-TOKEN-1".to_string(),
            "same_line".to_string()
        )],
        "the model's row links to the floor row, which heads the pair"
    );
}
