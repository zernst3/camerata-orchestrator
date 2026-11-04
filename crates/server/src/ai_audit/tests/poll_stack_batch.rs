//! Tests for poll stack batch.

use super::*;

// ── poll_batch_until_ended: consecutive-failure tolerance (compliance-audit GAP 1) ──
//
// Before this, a single failed `poll_batch_status` call propagated straight out of the
// batch loop via `?`, killing an hours-long (and already-paid-for) batch run over one
// transient blip. These tests drive the extracted helper with a fake `poll_once` — no
// live batch, no network — to prove the tolerate-N-then-give-up / reset-on-success
// contract without depending on the real Anthropic API.

fn stub_status(ended: bool) -> crate::llm::BatchStatus {
    crate::llm::BatchStatus {
        processing_status: if ended { "ended" } else { "in_progress" }.to_string(),
        request_counts: crate::llm::BatchRequestCounts::default(),
    }
}

#[tokio::test]
async fn poll_loop_succeeds_immediately_when_first_poll_reports_ended() {
    let calls = std::cell::RefCell::new(0u32);
    let statuses_seen = std::cell::RefCell::new(Vec::new());
    let res = poll_batch_until_ended(
        || {
            *calls.borrow_mut() += 1;
            std::future::ready(Ok(stub_status(true)))
        },
        std::time::Duration::from_millis(1),
        5,
        |s| statuses_seen.borrow_mut().push(s.processing_status.clone()),
    )
    .await;
    assert!(res.is_ok());
    assert_eq!(*calls.borrow(), 1);
    assert_eq!(*statuses_seen.borrow(), vec!["ended".to_string()]);
}

#[tokio::test]
async fn poll_loop_tolerates_failures_below_the_cap_then_succeeds() {
    // Fails 4 times (below the cap of 5), then a successful in_progress poll, then ended.
    // The counter must not carry over once a success resets it — this proves the whole
    // budget of 5 is available again after any single success mid-run.
    let call = std::cell::RefCell::new(0u32);
    let res = poll_batch_until_ended(
        || {
            let mut c = call.borrow_mut();
            *c += 1;
            let n = *c;
            async move {
                if n <= 4 {
                    anyhow::bail!("simulated transient poll failure #{n}");
                } else if n == 5 {
                    Ok(stub_status(false)) // success resets the failure counter
                } else if n <= 9 {
                    anyhow::bail!("simulated transient poll failure #{n}")
                } else {
                    Ok(stub_status(true))
                }
            }
        },
        std::time::Duration::from_millis(1),
        5,
        |_| {},
    )
    .await;
    assert!(
        res.is_ok(),
        "4 failures, then a success, then 4 more failures (all below the cap of 5 \
             consecutive) must not exhaust the budget: {res:?}"
    );
    assert_eq!(*call.borrow(), 10);
}

#[tokio::test]
async fn poll_loop_gives_up_after_n_consecutive_failures() {
    let call = std::cell::RefCell::new(0u32);
    let res = poll_batch_until_ended(
        || {
            *call.borrow_mut() += 1;
            std::future::ready(Err(anyhow::anyhow!("simulated persistent poll failure")))
        },
        std::time::Duration::from_millis(1),
        3,
        |_| {},
    )
    .await;
    let err = res.expect_err("5 (here: 3-cap) consecutive failures must give up, not hang forever");
    assert!(err.to_string().contains("3 times consecutively"));
    assert_eq!(*call.borrow(), 3, "must stop AT the cap, not overshoot it");
}

#[tokio::test]
async fn poll_loop_never_calls_on_status_for_a_failed_poll() {
    // on_status must only fire on SUCCESSFUL polls (it reads fields off BatchStatus,
    // which doesn't exist for a failed attempt).
    let seen = std::cell::RefCell::new(0u32);
    let call = std::cell::RefCell::new(0u32);
    let _ = poll_batch_until_ended(
        || {
            let mut c = call.borrow_mut();
            *c += 1;
            let n = *c;
            async move {
                if n == 1 {
                    anyhow::bail!("simulated failure")
                } else {
                    Ok(stub_status(true))
                }
            }
        },
        std::time::Duration::from_millis(1),
        5,
        |_| {
            *seen.borrow_mut() += 1;
        },
    )
    .await;
    assert_eq!(
        *seen.borrow(),
        1,
        "on_status fires exactly once, for the ended poll only"
    );
}

#[test]
fn parse_needs_files_reads_array_and_tolerates_absence() {
    let with = r#"{"findings":[],"proposed_rules":[],"needs_files":["a/repo.rs"," ","b/svc.rs"]}"#;
    let n = parse_needs_files(with);
    assert_eq!(n, vec!["a/repo.rs".to_string(), "b/svc.rs".to_string()]);
    // Absent / garbage -> empty, never errors.
    assert!(parse_needs_files(r#"{"findings":[]}"#).is_empty());
    assert!(parse_needs_files("not json").is_empty());
}

pub(super) fn site_finding(
    rule_id: &str,
    path: &str,
    line: usize,
    sev: &str,
    title: &str,
) -> Finding {
    Finding {
        could_be_intentional: false,
        site_key: String::new(),
        group_path: Vec::new(),
        group_role: crate::review_tree::GroupRole::Primary,
        review: Default::default(),
        flagged_by: Vec::new(),
        possible_duplicate_of: None,
        repo: "o/r".to_string(),
        kind: None,
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: sev.to_string(),
        snippet: title.to_string(),
        detail: format!("detail for {rule_id}"),
        status: "active".to_string(),
        also_matches: Vec::new(),
        preview: false,
        preview_tool: None,
        in_test: false,
        needs_review: false,
        confidence: None,
        effort: None,
        category: None,
        located: true,
        captures: Default::default(),
        evaluated_option_id: None,
        also_locations: Vec::new(),
        fix_specific: None,
        calibration_rationale: None,
    }
}

// ── P7: stack exceptions ────────────────────────────────────────────────────────────

/// THE WIRED INSTANCE, against the REAL corpus: `ARCH-MONOLITH-FIRST-1` must not flag a
/// `supabase/functions/` finding on a repo whose detected stack includes Supabase — Edge
/// Functions are the idiomatic second deployable for webhooks on that platform, not a
/// monolith-topology violation.
#[tokio::test]
async fn stack_exception_spares_monolith_first_on_supabase_edge_functions() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    assert!(
        corpus.get_by_id("ARCH-MONOLITH-FIRST-1").is_some(),
        "the wired rule must exist in the real corpus"
    );

    let excepted = site_finding(
        "ARCH-MONOLITH-FIRST-1",
        "supabase/functions/send-invite/index.ts",
        1,
        "medium",
        "a second deployable exists",
    );
    let unrelated = site_finding("ARCH-MONOLITH-FIRST-1", "src/server.rs", 5, "medium", "");
    let frameworks = vec!["Supabase".to_string()];
    let out = apply_stack_exceptions(vec![excepted, unrelated], &frameworks, &corpus);

    // A10: the excepted finding is kept and marked thrown out, not deleted.
    assert_eq!(out.len(), 2, "no row is deleted: {out:?}");
    let (thrown, shipped) = crate::engine_discard::split_discarded(out);
    assert_eq!(thrown.len(), 1, "the Edge Functions finding is thrown out");
    assert_eq!(
        crate::engine_discard::reason_of(&thrown[0]),
        Some(crate::engine_discard::STACK_EXCEPTION)
    );
    assert_eq!(
        shipped[0].path, "src/server.rs",
        "a monolith-first violation OUTSIDE supabase/functions/ must still fire"
    );
}

/// The SAME rule, on a repo that does NOT have Supabase in its detected stack: the
/// exception must never apply just because the path happens to match — both conditions
/// (framework present AND path match) are required.
#[tokio::test]
async fn stack_exception_does_not_apply_without_the_framework_present() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty());
    let f = site_finding(
        "ARCH-MONOLITH-FIRST-1",
        "supabase/functions/send-invite/index.ts",
        1,
        "medium",
        "",
    );
    let frameworks = vec!["Next.js".to_string()]; // Supabase NOT detected
    let out = apply_stack_exceptions(vec![f], &frameworks, &corpus);
    assert_eq!(
        out.len(),
        1,
        "no Supabase in the stack ⇒ the exception must not apply"
    );
}

/// GENERALITY: an entirely different rule + framework pair, via a SYNTHETIC ruleset (never
/// touching the real corpus), proves the mechanism is not special-cased to monolith-first
/// or Supabase — any rule that declares a `stack_exceptions` entry gets the same treatment.
#[test]
fn stack_exception_mechanism_generalizes_beyond_the_one_wired_rule() {
    let mut rule = camerata_rules::bare_rule("SOME-OTHER-RULE-1", "javascript");
    rule.stack_exceptions.push(camerata_rules::StackException {
        framework: "Next.js".to_string(),
        path_glob: "app/api/**/route.ts".to_string(),
        note: "Route Handlers are idiomatic in the Next.js App Router.".to_string(),
    });
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);

    let excepted = site_finding("SOME-OTHER-RULE-1", "app/api/users/route.ts", 1, "low", "");
    let unrelated = site_finding("SOME-OTHER-RULE-1", "pages/api/users.ts", 1, "low", "");
    let frameworks = vec!["Next.js".to_string()];
    let out = apply_stack_exceptions(vec![excepted, unrelated], &frameworks, &set);

    // A10: kept and marked thrown out, not deleted.
    assert_eq!(out.len(), 2, "no row is deleted: {out:?}");
    let (thrown, shipped) = crate::engine_discard::split_discarded(out);
    assert_eq!(
        thrown.len(),
        1,
        "the app-router-idiomatic path must be thrown out: {thrown:?}"
    );
    assert_eq!(shipped[0].path, "pages/api/users.ts");
}

/// A rule with NO `stack_exceptions` declared (the overwhelming majority of the corpus)
/// behaves exactly as before this feature existed — nothing is ever suppressed for it.
#[test]
fn apply_stack_exceptions_is_a_no_op_for_a_rule_with_no_declared_exceptions() {
    let mut set = camerata_rules::RuleSet::default();
    set.push(camerata_rules::bare_rule("PLAIN-RULE-1", "rust"));
    let f = site_finding("PLAIN-RULE-1", "supabase/functions/x.ts", 1, "medium", "");
    let out = apply_stack_exceptions(vec![f], &["Supabase".to_string()], &set);
    assert_eq!(out.len(), 1);
}

/// An AI-invented rule id (no corresponding corpus entry, `AI-` prefixed) can never be
/// excepted — only a real corpus rule's own declared exceptions apply.
#[test]
fn apply_stack_exceptions_never_excepts_an_uncorpused_ai_invented_rule_id() {
    let set = camerata_rules::RuleSet::default();
    let f = site_finding(
        "AI-SOME-NOVEL-DEFECT",
        "supabase/functions/x.ts",
        1,
        "medium",
        "",
    );
    let out = apply_stack_exceptions(vec![f], &["Supabase".to_string()], &set);
    assert_eq!(out.len(), 1, "an uncorpused rule id is never excepted");
}

#[test]
fn strip_dedup_pointers_removes_cross_references() {
    assert_eq!(strip_dedup_pointers("Same as [6]"), "");
    assert_eq!(strip_dedup_pointers("duplicate of [10]"), "");
    assert_eq!(
        strip_dedup_pointers("Real panic risk; same as [3]"),
        "Real panic risk"
    );
    assert_eq!(
        strip_dedup_pointers("over-flagged for a mini app, duplicate of 7"),
        "over-flagged for a mini app"
    );
    // The newer "index N" / "as index N" / "row N" pointer phrasing.
    assert_eq!(
        strip_dedup_pointers("directly observable failure as index 0"),
        "directly observable failure"
    );
    assert_eq!(strip_dedup_pointers("index 6"), "");
    assert_eq!(
        strip_dedup_pointers("maintainability concern; see index 9"),
        "maintainability concern"
    );
    assert_eq!(
        strip_dedup_pointers("same root cause, row 3"),
        "same root cause"
    );
    // Legit prose that merely contains the word "index" (no pointer number) survives.
    assert_eq!(
        strip_dedup_pointers("add a composite index on (user_id, created_at)"),
        "add a composite index on (user_id, created_at)"
    );
    // A clean reason is untouched.
    assert_eq!(
        strip_dedup_pointers("maintainability, not correctness"),
        "maintainability, not correctness"
    );
}

#[test]
fn labels_at_one_location_stay_three_rows_and_the_lower_ranked_two_are_linked() {
    // One smell at h.rs:12 reported under two invented names PLUS the adopted id, each with
    // a DIFFERENT title. They are three rules, so they are three rows: nothing is folded
    // away, each keeps its own severity, and the two the engine ranks lower carry a link
    // to the one it ranks highest.
    let code = "self.db.query(sql)";
    let files = vec![("h.rs".to_string(), format!("fn handler() {{ {code} }}"))];
    let findings = vec![
        site_finding("AI-CONTROLLER-DIRECT-DB", "h.rs", 12, "medium", code),
        site_finding("ARCH-STRICT-LAYERING-1", "h.rs", 12, "high", code),
        site_finding("AI-HANDLER-BYPASSES-REPO", "h.rs", 12, "low", code),
    ];
    assert_eq!(
        collapse_located(findings.clone(), &files).len(),
        3,
        "no collapse"
    );
    let rows = crate::review_tree::collapse_and_group(findings, &files);
    assert_eq!(
        rows.len(),
        3,
        "three labels at one location stay three rows"
    );
    let severities: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.rule_id.as_str(), r.severity.as_str()))
        .collect();
    assert_eq!(
        severities,
        vec![
            ("AI-CONTROLLER-DIRECT-DB", "medium"),
            ("ARCH-STRICT-LAYERING-1", "high"),
            ("AI-HANDLER-BYPASSES-REPO", "low"),
        ],
        "every row keeps its own severity"
    );
    assert!(rows
        .iter()
        .all(|r| r.also_matches.is_empty() && r.also_locations.is_empty()));
    let links = crate::review_tree::test_support::link_pairs(&rows);
    assert_eq!(
        links.len(),
        2,
        "two of the three point at the highest-ranked: {links:?}"
    );
    assert!(links.iter().all(|(_, _, hint)| hint == "same_line"));
    assert_eq!(
        links
            .iter()
            .map(|(_, t, _)| t.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1,
        "all point at one row"
    );
}

#[test]
fn overlapping_corpus_rules_at_one_location_are_separate_linked_rows() {
    // "Handler opens its own pool" legitimately trips layering + DI + entities-chain. They
    // are three rules, so three rows; the sub-group is "same line" and two rows are linked.
    let code = "Pool::connect(url).await";
    let files = vec![("h.rs".to_string(), format!("let pool = {code};"))];
    let findings = vec![
        site_finding("ARCH-STRICT-LAYERING-1", "h.rs", 41, "high", code),
        site_finding("ARCH-SERVICE-DI-1", "h.rs", 41, "medium", code),
        site_finding("RUST-ENTITIES-13", "h.rs", 41, "low", code),
    ];
    let rows = crate::review_tree::collapse_and_group(findings, &files);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        crate::review_tree::test_support::link_pairs(&rows).len(),
        2,
        "two non-head rows are linked to the head"
    );
}

#[test]
fn merge_does_not_collapse_distinct_line_zero_findings() {
    // Line 0 (file-level / uncited) must NOT location-merge — unrelated file-level
    // issues legitimately share line 0.
    let findings = vec![
        site_finding(
            "AI-NO-MAPPERS-CRATE",
            "lib.rs",
            0,
            "low",
            "no mappers crate",
        ),
        site_finding("AI-NO-TESTS", "lib.rs", 0, "low", "no tests"),
    ];
    let merged = collapse_located(findings, &[]);
    assert_eq!(merged.len(), 2, "distinct line-0 findings stay separate");
}

#[test]
fn merge_keeps_absence_findings_at_a_shared_line_separate() {
    // The real bug from the agora-mini verification: two ABSENCE findings ("no error
    // handler", "no API versioning") whose snippets describe a gap (NOT code in the file)
    // got anchored to the same representative line and wrongly merged — the error-handler
    // row picked up a spurious `ARCH-API-VERSIONING-1` in also_matches. They must stay
    // separate, since neither snippet is real code present at that line.
    let files = vec![(
        "app.ts".to_string(),
        "const app = express();\napp.use(express.json());\napp.listen(3000);".to_string(),
    )];
    let findings = vec![
        site_finding(
            "ARCH-CENTRAL-ERROR-HANDLER-1",
            "app.ts",
            2,
            "high",
            "no central error handler is registered",
        ),
        site_finding(
            "ARCH-API-VERSIONING-1",
            "app.ts",
            2,
            "medium",
            "routes are not version-prefixed",
        ),
    ];
    let merged = collapse_located(findings, &files);
    assert_eq!(
        merged.len(),
        2,
        "unrelated absence findings at one line stay separate"
    );
    assert!(
        merged.iter().all(|f| f.also_matches.is_empty()),
        "no spurious also_matches"
    );
}

#[test]
fn colocated_real_code_under_two_rule_families_is_two_linked_rows() {
    // Two findings that BOTH cite real code present at the same line are two rule
    // families, so two rows; the lower-ranked one links to the other.
    let files = vec![(
        "u.ts".to_string(),
        "const q = `SELECT * FROM t WHERE name ILIKE '%${name}%'`;".to_string(),
    )];
    let findings = vec![
        site_finding(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "u.ts",
            1,
            "critical",
            "ILIKE '%${name}%'",
        ),
        site_finding(
            "AI-SQL-INJECTION",
            "u.ts",
            1,
            "high",
            "SELECT * FROM t WHERE name ILIKE",
        ),
    ];
    let rows = crate::review_tree::collapse_and_group(findings, &files);
    assert_eq!(rows.len(), 2, "different rule families are not collapsed");
    let links = crate::review_tree::test_support::link_pairs(&rows);
    assert_eq!(
        links,
        vec![(
            "AI-SQL-INJECTION".to_string(),
            "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
            "same_line".to_string()
        )]
    );
}

#[test]
fn canonicalize_maps_invented_names_only_when_adopted() {
    let adopted: std::collections::HashSet<String> = [
        "ARCH-STRUCTURED-ERRORS-1".to_string(),
        "ARCH-STRICT-LAYERING-1".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        canonical_adopted_rule("HANDLER-PANICS-ON-DB-ERROR", &adopted).as_deref(),
        Some("ARCH-STRUCTURED-ERRORS-1")
    );
    assert_eq!(
        canonical_adopted_rule("HANDLER-CREATES-OWN-POOL", &adopted).as_deref(),
        Some("ARCH-STRICT-LAYERING-1")
    );
    // Secret-in-URL canonical isn't adopted here -> no phantom id.
    assert_eq!(canonical_adopted_rule("SECRET-IN-URL", &adopted), None);
    // A genuinely-novel name maps to nothing.
    assert_eq!(canonical_adopted_rule("MISSING-RATE-LIMIT", &adopted), None);
}

#[test]
fn extract_public_symbols_finds_rust_and_ts_exports() {
    let rust = "use x;\npub struct AdminStats { a: i32 }\nfn private() {}\npub trait Repo {}\n";
    let s = extract_public_symbols(rust);
    assert!(s.contains(&"AdminStats".to_string()));
    assert!(s.contains(&"Repo".to_string()));
    assert!(!s.iter().any(|x| x == "private"));
    let ts = "export class UserService {}\nexport interface Dto {}\n";
    let s2 = extract_public_symbols(ts);
    assert!(s2.contains(&"UserService".to_string()));
    assert!(s2.contains(&"Dto".to_string()));
}

#[test]
fn repo_map_lists_every_file_with_symbols() {
    let files = vec![
        (
            "crates/api/src/repositories/user_repo.rs".to_string(),
            "pub struct UserRepo {}".to_string(),
        ),
        (
            "crates/ui/src/services/admin_stats.rs".to_string(),
            "pub struct AdminStats {}".to_string(),
        ),
    ];
    let map = build_repo_map(&files, &crate::prompt_frame::FrameNonce::generate());
    // Every file is in the map even though a chunk may only hold one of them.
    assert!(map.contains("crates/api/src/repositories/user_repo.rs"));
    assert!(map.contains("crates/ui/src/services/admin_stats.rs"));
    assert!(map.contains("UserRepo"));
    assert!(map.contains("AdminStats"));
}

#[test]
fn chunk_files_covers_every_file_and_respects_budget() {
    // 10 files of ~100 bytes each; a 250-byte budget forces several chunks.
    let files: Vec<(String, String)> = (0..10)
        .map(|i| (format!("f{i}.rs"), "x".repeat(90)))
        .collect();
    let chunks = chunk_files(&files, 250);
    // Every file appears exactly once across all chunks (nothing dropped).
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    assert_eq!(total, 10, "all files covered, none dropped");
    assert!(chunks.len() > 1, "small budget forces multiple chunks");
    // Reassembled order matches the input order.
    let flat: Vec<&str> = chunks
        .iter()
        .flat_map(|c| c.iter().map(|(p, _)| p.as_str()))
        .collect();
    let want: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
    assert_eq!(flat, want.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn chunk_files_oversized_file_gets_its_own_chunk() {
    let files = vec![
        ("small.rs".to_string(), "x".repeat(10)),
        ("huge.rs".to_string(), "x".repeat(1000)),
        ("small2.rs".to_string(), "x".repeat(10)),
    ];
    let chunks = chunk_files(&files, 100);
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    assert_eq!(total, 3, "oversized file still included, nothing dropped");
}

#[test]
fn digest_concatenates_and_caps() {
    let files = vec![
        ("a.rs".to_string(), "fn a() {}".to_string()),
        ("b.rs".to_string(), "fn b() {}".to_string()),
    ];
    let nonce = crate::prompt_frame::FrameNonce::generate();
    let d = build_digest(&files, &nonce);
    assert!(d.contains(&format!("<<<UNTRUSTED {nonce} \"a.rs\">>>")));
    assert!(d.contains(&format!("<<<UNTRUSTED {nonce} \"b.rs\">>>")));
    assert!(d.contains("fn a()"));

    // A file larger than the cap truncates and notes it.
    let big = vec![("big.rs".to_string(), "x".repeat(MAX_DIGEST_CHARS + 1000))];
    let d2 = build_digest(&big, &nonce);
    assert!(d2.len() <= MAX_DIGEST_CHARS + 200);
    assert!(d2.contains("truncated"));
}

// ── GAP-3: batch-mode prompt-cache cost fix ─────────────────────────────────────
//
// Two problems, one fix. (1) The OLD prefix embedded the chunk index BEFORE the repo
// map (`"Repository: {repo} ({label} {ci}/{n_c})\n\n{repo_map}{digest}"`), so the
// "static" prefix actually differed at ~byte 30 on every chunk — the repo map could
// NEVER cache-hit across chunks, in any mode. (2) Batch mode defaults to ONE rule-batch
// per chunk (`BATCH_RULE_BATCH_SIZE = usize::MAX`), so the single cache breakpoint was
// written once and never read back — pure ~1.25x cost, zero benefit.
//
// The fix: lead with `Repository: {repo}\n\n{repo_map}` (byte-identical across every
// chunk), then the chunk's `(label n/n_c)\n\n{digest}\n\n` segment, then the varying
// task line + rules. Two cache breakpoints — one after the repo map (re-reads across
// chunks), one after the chunk segment (re-reads across that chunk's rule-batches,
// dropped entirely when there's only one rule-batch).

#[test]
fn cache_breakpoints_for_pass_keeps_repo_map_bp_always_and_digest_bp_only_when_n_b_gt_1() {
    // n_b > 1 (parallel mode's normal case, or a batch-mode run with a smaller
    // BATCH_RULE_BATCH_SIZE): both breakpoints present, repo-map first.
    let bps = cache_breakpoints_for_pass(100, 250, 3);
    assert_eq!(bps, vec![100, 250], "both breakpoints present when n_b > 1");

    // n_b == 1 (batch mode's default: BATCH_RULE_BATCH_SIZE = usize::MAX -> one
    // rule-batch per chunk): the digest breakpoint is dropped — it would be written
    // once and never re-read, paying a pure cache-write premium for zero benefit.
    let bps_one = cache_breakpoints_for_pass(100, 250, 1);
    assert_eq!(
        bps_one,
        vec![100],
        "digest breakpoint dropped when n_b == 1"
    );

    // n_b == 0 is a degenerate/defensive case (should never happen — there's always at
    // least one rule-batch) but must not panic or add a spurious second breakpoint.
    let bps_zero = cache_breakpoints_for_pass(100, 250, 0);
    assert_eq!(bps_zero, vec![100]);
}

#[test]
fn build_pass_prompt_leads_with_the_shared_repo_map_not_the_chunk_label() {
    let repo = "acme/widgets";
    let repo_map = "REPO MAP:\nfoo.rs: fn foo()\n";
    let (prompt, _bps) = build_pass_prompt(
        repo, repo_map, "parallel", 0, 3, "DIGEST-0", 2, "TASK", "RULES",
    );
    // The prompt must START with the repo-map segment, not the chunk label — this is
    // the exact ordering bug being fixed (old: "Repository: {repo} ({label} n/n_c)...").
    assert!(
        prompt.starts_with(&format!("Repository: {repo}\n\n{repo_map}")),
        "prompt must lead with the shared repo-map segment: {prompt:?}"
    );
    // The digest follows the repo map; the chunk label no longer sits in the cached prefix
    // at all (it leads the uncached suffix, after the digest).
    let repo_map_prefix = format!("Repository: {repo}\n\n{repo_map}");
    let rest = &prompt[repo_map_prefix.len()..];
    assert!(
        rest.starts_with("DIGEST-0"),
        "the digest follows the repo map: {rest:?}"
    );
    assert!(
        rest.contains("DIGEST-0\n\n(parallel 1/3)\n\nTASK"),
        "the label leads the varying suffix: {rest:?}"
    );
}

#[test]
fn build_pass_prompt_repo_map_segment_is_byte_identical_across_chunk_indices() {
    let repo = "acme/widgets";
    let repo_map = "REPO MAP:\nfoo.rs: fn foo()\nbar.rs: fn bar()\n";
    let (prompt_c0, bps_c0) = build_pass_prompt(
        repo,
        repo_map,
        "parallel",
        0,
        5,
        "digest for chunk 0",
        2,
        "T",
        "R",
    );
    let (prompt_c1, bps_c1) = build_pass_prompt(
        repo,
        repo_map,
        "parallel",
        1,
        5,
        "digest for chunk 1",
        2,
        "T",
        "R",
    );

    // Both prompts carry the SAME first breakpoint offset...
    assert_eq!(
        bps_c0[0], bps_c1[0],
        "the repo-map breakpoint offset must match across chunks"
    );
    // ...and the bytes up to that offset are IDENTICAL — this is what actually lets the
    // provider cache-hit the repo map on chunk 2, 3, ... instead of re-billing it.
    assert_eq!(
        &prompt_c0[..bps_c0[0]],
        &prompt_c1[..bps_c1[0]],
        "the repo-map segment must be byte-identical regardless of chunk index"
    );
    // Sanity: the chunk-specific segments genuinely differ (each chunk's digest+label).
    assert_ne!(&prompt_c0[bps_c0[0]..], &prompt_c1[bps_c1[0]..]);
}

#[test]
fn build_pass_prompt_two_breakpoints_when_n_b_gt_1_one_when_n_b_eq_1() {
    let repo = "acme/widgets";
    let repo_map = "REPO MAP:\nfoo.rs: fn foo()\n";

    // n_b > 1 (parallel mode's normal case): two breakpoints, repo-map then digest.
    let (prompt, bps) = build_pass_prompt(
        repo, repo_map, "parallel", 2, 5, "DIGEST-2", 3, "TASK", "RULES",
    );
    assert_eq!(bps.len(), 2, "n_b > 1 keeps both breakpoints");
    assert!(bps[0] < bps[1], "breakpoints are ascending");
    // The segment between the two breakpoints is the digest alone; the label is in the
    // uncached suffix so routing groups over the same files share it.
    let digest_segment = &prompt[bps[0]..bps[1]];
    assert!(!digest_segment.contains("(parallel 3/5)"));
    assert!(digest_segment.contains("DIGEST-2"));
    assert!(prompt[bps[1]..].contains("(parallel 3/5)"));
    // The suffix (after the last breakpoint) is the varying task line + rules — the
    // rules must NEVER leak into a cached segment.
    let suffix = &prompt[bps[1]..];
    assert!(suffix.contains("TASK") && suffix.contains("RULES"));
    assert!(
        !digest_segment.contains("RULES"),
        "rules must not leak into the cached prefix"
    );

    // n_b == 1 (batch mode's default rule-batch size): only the repo-map breakpoint
    // survives; the chunk label + digest fold into the uncached suffix instead.
    let (prompt_one, bps_one) = build_pass_prompt(
        repo, repo_map, "batch", 2, 5, "DIGEST-2", 1, "TASK", "RULES",
    );
    assert_eq!(bps_one.len(), 1, "n_b == 1 drops the digest breakpoint");
    let suffix_one = &prompt_one[bps_one[0]..];
    assert!(
        suffix_one.contains("DIGEST-2")
            && suffix_one.contains("TASK")
            && suffix_one.contains("RULES"),
        "digest + task + rules all fold into the single uncached suffix: {suffix_one:?}"
    );
}

#[test]
fn parse_valid_json_into_findings_and_rules() {
    let raw = r#"Here is the audit:
        {
          "findings": [
            {"path": "src/orders.rs", "line": 42, "severity": "high", "rule": "auth-on-write-paths", "title": "create_order writes with no auth check", "detail": "Anyone can POST."},
            {"path": "src/svc.rs", "line": 10, "severity": "medium", "rule": "no-db-in-services", "title": "OrderService queries db directly", "detail": "Bypasses repo."}
          ],
          "proposed_rules": [
            {"name": "auth-on-write-paths", "title": "Every write path checks authorization", "rationale": "x", "severity": "high", "enforcement": "review"}
          ]
        }
        Thanks!"#;
    let none = std::collections::HashSet::new();
    let (findings, rules) = parse_ai_findings("me/api", raw, &none);
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].rule_id, "AI-AUTH-ON-WRITE-PATHS");
    assert_eq!(findings[0].repo, "me/api");
    assert_eq!(findings[0].severity, "high");
    assert_eq!(findings[0].line, 42);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, "AI-AUTH-ON-WRITE-PATHS");
    assert_eq!(rules[0].kind, "review");
    assert_eq!(rules[0].enforcement_point, "integration");
    // The rule's finding_count picks up its matching finding.
    assert_eq!(rules[0].finding_count, 1);
}

/// A well-formed, allowlisted `captures` object on a model finding maps straight through
/// to `Finding.captures` — the general path that lets an AI-emitted finding (e.g. a
/// public-bucket or exposed-schema finding, which has no deterministic checker) still
/// name the real object in the report instead of falling back to the generic wording.
#[test]
fn parse_ai_findings_maps_a_well_formed_captures_object_through() {
    let raw = r#"{"findings":[
            {"path":"supabase/migrations/1.sql","line":5,"severity":"high",
             "rule":"public-bucket-not-reviewed",
             "title":"the invoices bucket is public",
             "detail":"anyone with the link can download every file in it",
             "captures":{"bucket":"invoices"}}
        ],"proposed_rules":[]}"#;
    let none = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("me/api", raw, &none);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].captures.get("bucket").map(String::as_str),
        Some("invoices")
    );
}

/// `captures` is optional enrichment: absence must never affect parsing of the rest of the
/// finding, and the resulting map must simply be empty (the report's generic fallback
/// covers it) rather than the finding being dropped or erroring.
#[test]
fn parse_ai_findings_defaults_to_empty_captures_when_the_field_is_absent() {
    let raw = r#"{"findings":[
            {"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d"}
        ],"proposed_rules":[]}"#;
    let none = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("me/api", raw, &none);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].captures.is_empty());
}

/// A malformed `captures` blob — wrong JSON type, an unknown/hallucinated token key, a
/// non-string value, and an oversized value all mixed together — must degrade to "skip the
/// bad entries" rather than panicking or dropping the finding. This is the adversarial
/// half of Step 2's defensiveness requirement: the model's output is untrusted input.
#[test]
fn parse_ai_findings_degrades_a_malformed_captures_blob_without_panicking() {
    let huge = "x".repeat(500);
    let raw = format!(
        r#"{{"findings":[
                {{"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d",
                  "captures":{{
                    "bucket":"avatars",
                    "not-a-real-token":"whatever",
                    "table":42,
                    "schema":"   ",
                    "view":"{huge}"
                  }}}}
            ],"proposed_rules":[]}}"#
    );
    let none = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("me/api", &raw, &none);
    assert_eq!(
        findings.len(),
        1,
        "the finding itself must survive a malformed captures blob"
    );
    assert_eq!(
        findings[0].captures.get("bucket").map(String::as_str),
        Some("avatars"),
        "the one well-formed entry must still come through"
    );
    assert!(
        !findings[0].captures.contains_key("not-a-real-token"),
        "a key outside the known token allowlist must be dropped"
    );
    assert!(
        !findings[0].captures.contains_key("table"),
        "a non-string value must be dropped, not coerced or panicking"
    );
    assert!(
        !findings[0].captures.contains_key("schema"),
        "a whitespace-only value must be dropped"
    );
    assert!(
        !findings[0].captures.contains_key("view"),
        "a value over the length cap must be dropped"
    );
    assert_eq!(findings[0].captures.len(), 1);
}

/// `captures` being an entirely wrong JSON shape (a string, not an object) must also
/// degrade to empty rather than erroring or panicking.
#[test]
fn parse_ai_findings_degrades_a_non_object_captures_value_to_empty() {
    let raw = r#"{"findings":[
            {"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d",
             "captures":"not an object"}
        ],"proposed_rules":[]}"#;
    let none = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("me/api", raw, &none);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].captures.is_empty());
}

/// The raw-audit parser must ACCEPT and PRESERVE an explicit `"critical"` severity from
/// the model — this is the emit-path half of the critical-severity fix (the schema now
/// offers it; this pins that the parser doesn't silently demote it to "medium" the way an
/// unrecognized string would).
#[test]
fn parse_ai_findings_accepts_and_preserves_critical_severity() {
    let raw = r#"{"findings":[
            {"path":"supabase/functions/charge/index.ts","line":30,"severity":"critical",
             "rule":"unauthenticated-charge-endpoint",
             "title":"charge handler has no auth check and holds a service_role key",
             "code":"export default async function handler(req) {",
             "detail":"any caller can trigger a real charge with the service_role key"},
            {"path":"a.rs","line":1,"severity":"medium","rule":"y","title":"t","detail":"d"}
        ],"proposed_rules":[]}"#;
    let none = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("me/api", raw, &none);
    assert_eq!(findings.len(), 2);
    assert_eq!(
        findings[0].severity, "critical",
        "an explicit critical verdict from the model must survive parsing unchanged"
    );
    // Anti-over-rotation guard: the sibling medium finding must NOT be swept up into
    // critical just because another finding in the same batch was critical.
    assert_eq!(findings[1].severity, "medium");
}

/// D1, A10: the row a scan once returned for a rule named `JAVASCRIPT-TYPESRIPT-PLACEHOLDER`
/// (title `placeholder`, code and detail `x`) is not a finding; the real row beside it is.
/// The placeholder row is KEPT, marked thrown out with the reason; the real row is unmarked.
#[test]
fn parse_ai_findings_marks_a_placeholder_row_and_keeps_the_real_one() {
    let raw = r#"{"findings":[
            {"path":"lib/supabase/admin.ts","line":8,"severity":"low",
             "rule":"JAVASCRIPT-TYPESRIPT-PLACEHOLDER","title":"placeholder","code":"x","detail":"x"},
            {"path":"lib/supabase/admin.ts","line":12,"severity":"high",
             "rule":"service-role-key-in-client-bundle","title":"Service role key reaches the browser",
             "code":"export const admin = createClient(url, serviceKey)",
             "detail":"The admin client is built with the service role key in a module the browser imports."}
        ],"proposed_rules":[]}"#;
    let none = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("me/api", raw, &none);
    assert_eq!(findings.len(), 2, "no row is deleted: {findings:#?}");
    let (thrown, kept) = crate::engine_discard::split_discarded(findings);
    assert_eq!(kept.len(), 1, "{kept:#?}");
    assert_eq!(kept[0].line, 12);
    assert_eq!(thrown.len(), 1, "{thrown:#?}");
    assert_eq!(thrown[0].line, 8);
    assert!(crate::engine_discard::reason_of(&thrown[0])
        .unwrap()
        .starts_with(crate::engine_discard::PLACEHOLDER_ROW));
}

/// R1B-5: a bare table name and its schema-qualified form are the same object, so they do
/// not veto a same-rule merge; different tables and different schemas still do.
#[test]
fn object_names_match_bare_against_qualified_but_not_different_tables_or_schemas() {
    assert!(object_names_match(
        "member_contacts",
        "public.member_contacts"
    ));
    assert!(object_names_match(
        "public.member_contacts",
        "member_contacts"
    ));
    assert!(object_names_match("public.orders", "public.orders"));
    assert!(!object_names_match("profiles", "public.member_contacts"));
    assert!(!object_names_match("public.orders", "private.orders"));
    assert!(!object_names_match("orders", "public.order_items"));
    // A bare name means the public schema (the floor strips exactly `public.`), so it never
    // matches a table of the same name in another schema.
    assert!(!object_names_match("users", "auth.users"));
    assert!(!object_names_match("orders", "private.orders"));
    assert!(!object_names_match("orders", "app.db.orders"));
    assert!(!object_names_match("auth.users", "users"));
}

/// R1B-3: a rule id the model mistyped by one or two characters is keyed to the one adopted
/// rule it is closest to (`TYPESRIPT` for `TYPESCRIPT`), not left as an `AI-` invented id.
#[test]
fn parse_ai_findings_canonicalises_a_typo_to_the_one_adopted_rule_within_two_edits() {
    let adopted: std::collections::HashSet<String> = [
        "JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1".to_string(),
        "JAVASCRIPT-TYPESCRIPT-NO-ANY-1".to_string(),
    ]
    .into_iter()
    .collect();
    let raw = r#"{"findings":[{"path":"a.ts","line":3,"rule":"JAVASCRIPT-TYPESRIPT-NO-NON-NULL-ASSERTION-1",
            "title":"Non-null assertion","code":"user!.id","detail":"The assertion hides a null."}],
            "proposed_rules":[]}"#;
    let (findings, _) = parse_ai_findings("me/api", raw, &adopted);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(
        findings[0].rule_id,
        "JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1"
    );
}

/// R1B-3: an id within two edits of TWO adopted ids is ambiguous, so it keeps today's
/// behaviour (the `AI-` invented id); so does an id with no adopted id that close.
#[test]
fn parse_ai_findings_leaves_an_ambiguous_or_distant_typo_as_an_invented_id() {
    let adopted: std::collections::HashSet<String> = [
        "SUPABASE-STORAGE-BUCKET-POLICY-A-1".to_string(),
        "SUPABASE-STORAGE-BUCKET-POLICY-B-1".to_string(),
        "SUPABASE-AUTH-SESSION-REFRESH-ROTATION-1".to_string(),
    ]
    .into_iter()
    .collect();
    // One edit from both policy ids: ambiguous, so not canonicalised.
    let ambiguous = "SUPABASE-STORAGE-BUCKET-POLICY-C-1";
    assert_eq!(adopted_typo_match(ambiguous, &adopted), None);
    assert_eq!(
        adopted_typo_match("SUPABASE-AUTH-SESSION-REFRESH-ROTATON-1", &adopted),
        Some("SUPABASE-AUTH-SESSION-REFRESH-ROTATION-1".to_string()),
        "one adopted id within two edits"
    );
    // The numeric suffix names a different rule per number, so it is never a typo.
    let numbered: std::collections::HashSet<String> =
        ["RUST-DIOXUS-3".to_string(), "SQL-DB-INDEX-1".to_string()]
            .into_iter()
            .collect();
    assert_eq!(adopted_typo_match("RUST-DIOXUS-7", &numbered), None);
    assert_eq!(adopted_typo_match("SQL-DB-INDEX-2", &numbered), None);
    for rule in [ambiguous, "SUPABASE-AUTH-SESSION-TIMEOUT-HANDLING-1"] {
        let raw = format!(
            r#"{{"findings":[{{"path":"a.ts","line":3,"rule":"{rule}",
                "title":"A real title","code":"x.y()","detail":"A real explanation."}}],
                "proposed_rules":[]}}"#
        );
        let (findings, _) = parse_ai_findings("me/api", &raw, &adopted);
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].rule_id, format!("AI-{rule}"));
    }
}

/// R1B-3: the typo match never rescues a placeholder id: the junk filter still throws it out
/// (A10: kept and marked, not deleted).
#[test]
fn parse_ai_findings_does_not_canonicalise_a_placeholder_id_onto_an_adopted_rule() {
    let adopted: std::collections::HashSet<String> =
        ["JAVASCRIPT-TYPESCRIPT-PLACEHOLDER-1".to_string()]
            .into_iter()
            .collect();
    let raw = r#"{"findings":[{"path":"a.ts","line":3,"rule":"JAVASCRIPT-TYPESRIPT-PLACEHOLDER-1",
            "title":"A real title","code":"user!.id","detail":"A real explanation."}],
            "proposed_rules":[]}"#;
    let (findings, _) = parse_ai_findings("me/api", raw, &adopted);
    let (thrown, kept) = crate::engine_discard::split_discarded(findings);
    assert!(kept.is_empty(), "{kept:#?}");
    assert_eq!(thrown.len(), 1, "{thrown:#?}");
}

/// D1, A10: each junk shape is thrown out on its own, kept and marked (an empty reply for code and detail, a
/// placeholder title over real text, a placeholder rule id over real text).
#[test]
fn parse_ai_findings_refuses_each_junk_shape_for_an_invented_rule() {
    let none = std::collections::HashSet::new();
    for row in [
        r#"{"path":"a.ts","line":1,"rule":"made-up-rule","title":"A real title","code":"","detail":""}"#,
        r#"{"path":"a.ts","line":1,"rule":"made-up-rule","title":"A real title","code":"x","detail":"n/a"}"#,
        r#"{"path":"a.ts","line":1,"rule":"made-up-rule","title":"placeholder","code":"db.query(sql)","detail":"Builds SQL from input."}"#,
        r#"{"path":"a.ts","line":1,"rule":"typescript-placeholder","title":"A real title","code":"db.query(sql)","detail":"Builds SQL from input."}"#,
    ] {
        let raw = format!(r#"{{"findings":[{row}],"proposed_rules":[]}}"#);
        let (findings, _) = parse_ai_findings("me/api", &raw, &none);
        // A10: the row is kept, marked thrown out; none of it goes on as a finding.
        let (thrown, kept) = crate::engine_discard::split_discarded(findings);
        assert!(kept.is_empty(), "{row} produced {kept:#?}");
        assert_eq!(thrown.len(), 1, "{row} was deleted");
    }
}

/// D1: a row under an ADOPTED rule is kept when it has a real title even with no code and
/// no detail, since the rule backs it.
#[test]
fn parse_ai_findings_keeps_an_adopted_rule_row_that_has_only_a_title() {
    let adopted: std::collections::HashSet<String> = ["SEC-NO-RAW-SQL-CONCAT-1".to_string()]
        .into_iter()
        .collect();
    let raw = r#"{"findings":[{"path":"a.ts","line":1,"rule":"SEC-NO-RAW-SQL-CONCAT-1",
            "title":"SQL built from request input","code":"","detail":""}],"proposed_rules":[]}"#;
    let (findings, _) = parse_ai_findings("me/api", raw, &adopted);
    assert_eq!(findings.len(), 1);
}

/// D1, A10: after the detail and calibration steps, a model-invented row whose explanation
/// says to discard it is KEPT and marked thrown out (with its reason); a row under an adopted
/// rule is never marked by its prose. No row is removed.
#[test]
fn mark_discarded_invented_rows_acts_on_the_discard_verdict() {
    let mut discarded = finding("AI-NOTE-ONLY", "low");
    discarded.detail = "This finding contains only a placeholder statement with the code 'x'. Discard this finding.".into();
    let mut kept_ai = finding("AI-REAL-ONE", "high");
    kept_ai.detail = "Real title — The handler trusts the caller.".into();
    let mut adopted = finding("SEC-NO-RAW-SQL-CONCAT-1", "high");
    adopted.detail = "Raw SQL — a note that says discard this finding".into();
    let mut rows = vec![discarded, kept_ai, adopted];
    let marked = mark_discarded_invented_rows(
        "me/api",
        &mut rows,
        &crate::engine_discard::RestoredKeys::default(),
    );
    assert_eq!(marked, 1);
    let ids: Vec<&str> = rows.iter().map(|f| f.rule_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["AI-NOTE-ONLY", "AI-REAL-ONE", "SEC-NO-RAW-SQL-CONCAT-1"],
        "no row is removed"
    );
    assert_eq!(
        crate::engine_discard::reason_of(&rows[0]),
        Some(crate::engine_discard::DISCARD_VERDICT)
    );
    assert!(!crate::engine_discard::is_discarded(&rows[1]));
    assert!(!crate::engine_discard::is_discarded(&rows[2]));
}

#[test]
fn parse_uses_verbatim_code_as_snippet_and_keeps_title_in_detail() {
    let raw = r#"{"findings":[{"path":"a.rs","line":5,"severity":"high","rule":"x",
          "title":"raw SQL built by format!","code":"let q = format!(\"SELECT ...\");",
          "detail":"use a query builder"}],"proposed_rules":[]}"#;
    let none = std::collections::HashSet::new();
    let (f, _) = parse_ai_findings("r/r", raw, &none);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].snippet, "let q = format!(\"SELECT ...\");",
        "snippet is the verbatim code"
    );
    assert!(
        f[0].detail.starts_with("raw SQL built by format!"),
        "title leads the detail"
    );
    assert!(f[0].detail.contains("use a query builder"));
}

#[test]
fn resolve_finding_lines_corrects_from_verbatim_snippet() {
    let content = "fn a() {}\nlet x = 1;\nthe offending CALL here\nlet y = 2;\n";
    let files = vec![("src/lib.rs".to_string(), content.to_string())];
    // Model guessed line 1, snippet is on line 3.
    let mut findings = vec![site_finding(
        "AI-X",
        "src/lib.rs",
        1,
        "high",
        "the offending CALL here",
    )];
    resolve_finding_lines(&mut findings, &files);
    assert_eq!(
        findings[0].line, 3,
        "line resolved from the verbatim snippet"
    );

    // Duplicate snippet → nearest occurrence to the model's estimate wins.
    let dup = "data_tr(a)\nx\ndata_tr(a)\n";
    let files2 = vec![("d.rs".to_string(), dup.to_string())];
    let mut f2 = vec![site_finding("AI-Y", "d.rs", 3, "high", "data_tr(a)")];
    resolve_finding_lines(&mut f2, &files2);
    assert_eq!(f2[0].line, 3, "duplicate resolves to the nearest match");

    // Paraphrase not present → keep the model's line.
    let mut f3 = vec![site_finding(
        "AI-Z",
        "src/lib.rs",
        2,
        "high",
        "paraphrase not in the file",
    )];
    resolve_finding_lines(&mut f3, &files);
    assert_eq!(f3[0].line, 2, "no match keeps the model's line");
}
