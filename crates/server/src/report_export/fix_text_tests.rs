//! Fix text in the report data and the PDF: the rule TOML's hard line wraps never reach a card,
//! blank lines still split paragraphs, and backtick spans reach Typst as inline raw (mono).

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with, which_typst};
use super::*;

const SERVICE_ROLE: &str = "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1";

async fn corpus() -> camerata_rules::RuleSet {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    corpus
}

/// The ONE normalization the PDF text tests allow. Code runs are set in a mono font, and
/// `pdf_extract` (the text reader these tests use) puts a space where the font changes, so a code
/// run followed by punctuation reads "VITE_KEY ." in its output. Evidence that the PDF itself is
/// right: the rendered page (PNG) shows the period flush against the code, and `pypdf`'s copy of
/// the same page reads "VITE_KEY." with no space. So only `pdf_extract` adds it. This removes
/// exactly that space (before . , ; : ) right after a given code run) and nothing else: two prose
/// words that lost their space still fail a strict comparison.
fn undo_extraction_space(flat: &str, codes: &[&str]) -> String {
    let mut out = flat.to_string();
    for code in codes {
        for p in ['.', ',', ';', ':', ')'] {
            out = out.replace(&format!("{code} {p}"), &format!("{code}{p}"));
        }
    }
    out
}

#[test]
fn the_extraction_normalization_hides_only_the_space_after_a_code_run() {
    // The artifact: removed.
    assert_eq!(
        undo_extraction_space("rename VITE_KEY . Redeploy", &["VITE_KEY"]),
        "rename VITE_KEY. Redeploy"
    );
    // Guard: a space lost between two prose words is NOT restored, so a strict comparison fails.
    let lost = undo_extraction_space("rename VITE_KEY . Redeployafterwards.", &["VITE_KEY"]);
    assert!(!lost.contains("Redeploy afterwards."), "{lost}");
    // Guard: a space before punctuation that does not follow a code run is left alone.
    assert_eq!(
        undo_extraction_space("two words . later", &["VITE_KEY"]),
        "two words . later"
    );
}

/// The shipped rule's remediation is hard-wrapped in its TOML, with backtick spans.
#[tokio::test]
async fn resolve_fix_returns_the_authored_remediation_as_one_paragraph() {
    let corpus = corpus().await;
    let f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    let fix = resolve_fix(SERVICE_ROLE, Some(&corpus), &f, None).expect("authored remediation");
    assert!(!fix.contains('\n'), "hard wraps must be joined: {fix:?}");
    // Backtick spans survive for the PDF to set as mono; the placeholder is filled first.
    assert!(fix.contains("`apps/web/.env.production`"), "{fix:?}");
    assert!(fix.contains("`VITE_`, `REACT_APP_`"), "{fix:?}");
}

#[tokio::test]
async fn the_pdf_data_joins_a_hard_wrapped_specific_fix_and_keeps_paragraph_breaks() {
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(
        "Move the key out of `apps/web/.env.production`\nand treat it as\ncompromised.\n\nRedeploy."
            .to_string(),
    );
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(
        site.fix_for_this_finding.as_deref(),
        Some("Move the key out of `apps/web/.env.production` and treat it as compromised.\n\nRedeploy.")
    );
    let generic = site.fix.as_deref().expect("generic guidance");
    assert!(!generic.contains('\n'), "{generic:?}");
}

/// A specific fix that is just the rule's own text (the give-up path) is still suppressed from
/// "General guidance" when the two are the same sentence, even if the model text was wrapped.
#[tokio::test]
async fn an_identical_wrapped_specific_fix_is_not_repeated_as_general_guidance() {
    let corpus = corpus().await;
    let f0 = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    let generic = resolve_fix(SERVICE_ROLE, Some(&corpus), &f0, None).unwrap();
    let mut f = f0.clone();
    f.fix_specific = Some(generic.replace(". ", ".\n"));
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(site.fix, None, "{:?}", site.fix);
}

/// The PDF prints a fix only for an escalated place, in two sentences, as plain text (the rows the
/// PDF is built from carry `fix_text::plain`, the form the workbook prints): the backticks of a
/// code span never reach the document.
#[tokio::test]
async fn a_code_span_in_fix_text_prints_without_its_backticks() {
    if which_typst().is_none() {
        eprintln!("skipping: typst is not on PATH");
        return;
    }
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(
        "Move the key out of `apps/web/.env.production` and rename `VITE_KEY`.\n\nRedeploy afterwards."
            .to_string(),
    );
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(
        json.escalations.items[0].what_to_do,
        "Move the key out of apps/web/.env.production and rename VITE_KEY. Redeploy afterwards."
    );
    let data = render_data_json(&json).unwrap();
    let pdf = crate::report_export::compile_pdf_from_data(&data)
        .await
        .expect("the PDF compiles");
    // Test-only hook, the same one `addition_tests` documents: keep the PDF for a look.
    if let Ok(out) = std::env::var("CAMERATA_RENDER_OUT") {
        std::fs::write(out, &pdf).expect("write the PDF for a look");
    }
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !flat.contains('`'),
        "a literal backtick reached the document"
    );
    let flat = undo_extraction_space(&flat, &["apps/web/.env.production", "VITE_KEY"]);
    assert!(
        flat.contains("rename VITE_KEY. Redeploy afterwards."),
        "the fix is printed in two sentences: {flat}"
    );
}

const REAL_SQL: &str = "Replace the sql.unsafe call with a tagged template so the search term is sent as bound parameters, for example: const rows = await sql`select id, title, price_cents, category from public.listings where status = 'active' and (title ilike ${'%' + q + '%'} or category ilike ${'%' + q + '%'}) order by created_at desc limit 30`; also escape percent and underscore characters in q as in the other search route.";

/// The cases every surface must agree on: the raw (prepared) text, the plain text the workbook,
/// CSV and JSON print, and the code runs the PDF sets in mono. The first row is the sample
/// report's SQL finding (see `REAL_SQL`); the next two are stored single-backtick prose spans;
/// the rest are the shapes #391 names.
fn backtick_cases() -> Vec<(String, String, Vec<String>)> {
    let sql_code =
        REAL_SQL[REAL_SQL.find("sql`").unwrap()..REAL_SQL.rfind('`').unwrap() + 1].to_string();
    let c = |raw: &str, plain: &str, codes: &[&str]| {
        (
            raw.to_string(),
            plain.to_string(),
            codes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };
    vec![
        // The tagged template: single backticks glued to an identifier. Nothing is dropped.
        (REAL_SQL.to_string(), REAL_SQL.to_string(), vec![sql_code]),
        c(
            "set it to private (`update storage.buckets set public = false where id = 'member-documents'`) and add policies",
            "set it to private (update storage.buckets set public = false where id = 'member-documents') and add policies",
            &["update storage.buckets set public = false where id = 'member-documents'"],
        ),
        c(
            "for example `using (auth.uid() = user_id)` instead of `using (true)` and `NEXT_PUBLIC_`, `VITE_`.",
            "for example using (auth.uid() = user_id) instead of using (true) and NEXT_PUBLIC_, VITE_.",
            &["using (auth.uid() = user_id)", "using (true)", "NEXT_PUBLIC_", "VITE_"],
        ),
        c("set `x` to 1", "set x to 1", &["x"]),
        // A shell command substitution, wrapped in a longer run: the inner backticks are code.
        c("Run `` echo `date` `` to stamp it", "Run echo `date` to stamp it", &["echo `date`"]),
        // A Markdown example, wrapped in a longer run.
        c("Open with `` ```rust `` as the fence", "Open with ```rust as the fence", &["```rust"]),
        // Unmatched runs print literally and later spans still work.
        c("a ``` b `c` d", "a ``` b c d", &["c"]),
        c("a ` b", "a ` b", &[]),
        // Bare tagged templates stay mono with both backticks.
        c(
            "Set css`color:red` here",
            "Set css`color:red` here",
            &["css`color:red`"],
        ),
        c("Use a.b`x-y` now", "Use a.b`x-y` now", &["a.b`x-y`"]),
        c(
            "Use sql`a ${b`c`} d` now",
            "Use sql`a ${b`c`} d` now",
            &["sql`a ${b`c`} d`"],
        ),
        // A multi-line template: `prepare` joins the lines, the backticks stay.
        c(
            "Use sql`\n select 1\n`; now",
            "Use sql` select 1 `; now",
            &["sql` select 1 `"],
        ),
        // Ordinary prose spans parse exactly as plain CommonMark: separate spans, prose between.
        c(
            "the anon key (`A_KEY`) and the URL (`A_URL`) are public",
            "the anon key (A_KEY) and the URL (A_URL) are public",
            &["A_KEY", "A_URL"],
        ),
        c("set `NODE_ENV`=`production`", "set NODE_ENV=production", &["NODE_ENV", "production"]),
        c("`GET`/`POST`", "GET/POST", &["GET", "POST"]),
        c("`db`.`table`", "db.table", &["db", "table"]),
        c(
            "Move `KEY`, the secret, to (`api/keys.ts`)",
            "Move KEY, the secret, to (api/keys.ts)",
            &["KEY", "api/keys.ts"],
        ),
        c(
            "the `VITE_`-prefixed variables (`.env`) are public",
            "the VITE_-prefixed variables (.env) are public",
            &["VITE_", ".env"],
        ),
        c("call `fetch`(`/api/users`)", "call fetch(/api/users)", &["fetch", "/api/users"]),
        c("**`foo`** and **`.env`**", "**foo** and **.env**", &["foo", ".env"]),
        c("the `user`\u{2019}s file (`.env`)", "the user\u{2019}s file (.env)", &["user", ".env"]),
        c("`env`/`printenv`", "env/printenv", &["env", "printenv"]),
        c(
            "Set `timeout` to 30. Replace the empty string (``) with a default.",
            "Set timeout to 30. Replace the empty string (``) with a default.",
            &["timeout"],
        ),
        // AMBIGUOUS texts (a backtick glued to a word on its outside) are ONE prose run with every
        // backtick printed: nothing dropped, still copy-paste ready, none of it in mono.
        c(
            "`const rows = await sql`select id limit 30`;`",
            "`const rows = await sql`select id limit 30`;`",
            &[],
        ),
        c(
            "Run `const q = sql`select 1`` now.",
            "Run `const q = sql`select 1`` now.",
            &[],
        ),
        c("Run `echo `date`` now", "Run `echo `date`` now", &[]),
        c(
            "Run `echo `date`` and `echo `pwd``",
            "Run `echo `date`` and `echo `pwd``",
            &[],
        ),
        c("the`foo` call", "the`foo` call", &[]),
        c("`a`or`b`", "`a`or`b`", &[]),
        c("don`t use `foo`", "don`t use `foo`", &[]),
        c("use t`Hello` now", "use t`Hello` now", &[]),
        // Brackets inside quotes or a character class do not count, and a bracket-only body is
        // code: these stay ordinary spans.
        c(
            "call `split(')')` here",
            "call split(')') here",
            &["split(')')"],
        ),
        c("regex `[^)]*` here", "regex [^)]* here", &["[^)]*"]),
        c(
            "use `{` and `(` and `)` here",
            "use { and ( and ) here",
            &["{", "(", ")"],
        ),
        // A wrapped template whose inner backtick touches punctuation: literal, nothing dropped.
        c(
            "use `fetch(`/api/users/${id}`)` here",
            "use `fetch(`/api/users/${id}`)` here",
            &[],
        ),
        c(
            "use `db.query(sql`${cols} from t`)` here",
            "use `db.query(sql`${cols} from t`)` here",
            &[],
        ),
        c(
            "Use `html`<div>hi</div>`` now",
            "Use `html`<div>hi</div>`` now",
            &[],
        ),
        // "call `foo`s": a closer glued to a word is ambiguous, so it goes literal.
        c("call `foo`s", "call `foo`s", &[]),
    ]
}

#[test]
fn plain_and_segments_agree_on_every_case() {
    for (raw, plain, codes) in backtick_cases() {
        assert_eq!(fix_text::plain(&raw), plain, "raw: {raw}");
        let segs = fix_text::segments(&fix_text::prepare(&raw));
        let got: Vec<String> = segs
            .iter()
            .filter_map(|s| match s {
                fix_text::Seg::Code(t) => Some(t.clone()),
                fix_text::Seg::Text(_) => None,
            })
            .collect();
        assert_eq!(got, codes, "raw: {raw}");
        assert_eq!(fix_text::join_plain(&segs), plain, "raw: {raw}");
    }
}

#[test]
fn plain_never_loses_a_backtick_inside_code() {
    // Backticks out = backticks in, minus the span delimiters only.
    let raw = "x `` a`b `` y `c` z sql`q r`";
    assert_eq!(fix_text::plain(raw), "x a`b y c z sql`q r`");
}

#[test]
fn plain_keeps_both_backticks_of_the_real_sql_template() {
    let out = fix_text::plain(REAL_SQL);
    assert_eq!(out.matches('`').count(), 2, "{out}");
    assert!(out.contains("await sql`select id, title"), "{out}");
    assert!(out.contains("limit 30`; also escape"), "{out}");
}

/// M2: a run of three or more backticks (a fence) keeps the old behavior: it prints literally,
/// and the language tag is not glued into code.
#[test]
fn a_fence_prints_literally() {
    assert_eq!(
        fix_text::plain("```ts const x = 1 ```"),
        "```ts const x = 1 ```"
    );
    assert!(fix_text::segments("```ts const x = 1 ```")
        .iter()
        .all(|s| matches!(s, fix_text::Seg::Text(_))));
    // A longer run inside a double-backtick span is body text.
    assert_eq!(fix_text::plain("a `` x ``` y `` b"), "a x ``` y b");
}

/// The escalation cut reads the RAW text: the "." in sql.unsafe and the ";" are not sentence
/// ends, and the cut lands after the template for the real string.
#[test]
fn the_escalation_cut_keeps_the_real_sql_template_whole() {
    let segs = crate::report_export::first_sentences_raw(&fix_text::prepare(REAL_SQL), 2, 400);
    let plain = fix_text::join_plain(&segs);
    assert!(plain.contains("await sql`select id, title"), "{plain}");
    assert!(plain.contains("limit 30`;"), "{plain}");
    assert_eq!(plain.matches('`').count(), 2, "{plain}");
    assert!(segs
        .iter()
        .any(|s| matches!(s, fix_text::Seg::Code(t) if t.starts_with("sql`") && t.ends_with('`'))));
}

/// LOW: the character limit never cuts inside a code span or a tagged template. A template about
/// 100 characters longer than the real one puts the limit inside it: the cut moves before it.
#[test]
fn the_character_limit_never_cuts_inside_a_template() {
    let body = "select id, title, price_cents, category from public.listings where status = 'active' and (title ilike ${'%' + q + '%'} or category ilike ${'%' + q + '%'}) order by created_at desc, id desc, price_cents asc, category asc, title asc, updated_at desc, owner_id asc, status asc, slug asc, region asc limit 30";
    let raw = format!(
        "Replace the sql.unsafe call with a tagged template so the search term is sent as bound parameters, for example: const rows = await sql`{body}`; also escape percent and underscore characters in q as in the other search route."
    );
    let segs = crate::report_export::first_sentences_raw(&raw, 2, 400);
    let plain = fix_text::join_plain(&segs);
    // The template starts before the limit and ends after it.
    let start = raw.find("sql`").unwrap();
    let end = raw.rfind('`').unwrap() + 1;
    assert!(start < 400 && end > 400, "fixture: {start} {end}");
    assert_eq!(
        plain.matches('`').count(),
        0,
        "cut before the template: {plain}"
    );
    assert!(plain.ends_with("await..."), "{plain}");
    assert!(plain.chars().count() <= 403, "{plain}");
    // A span that fits is kept whole.
    let fits = crate::report_export::first_sentences_raw(&raw, 2, 900);
    assert!(fix_text::join_plain(&fits).contains(&format!("sql`{body}`;")));
}

#[test]
fn a_dot_inside_code_is_not_a_sentence_end() {
    let segs = crate::report_export::first_sentences_raw(
        "Use `a.b. Next` here. Then more. Third one.",
        2,
        400,
    );
    assert_eq!(
        fix_text::join_plain(&segs),
        "Use a.b. Next here. Then more."
    );
}

/// ONE parser (H1): the production path (`build_report_json`, `first_sentences` and all) hands
/// the template text and code runs, and `render_data_json` carries them. The PDF text agrees with
/// the workbook's `plain()` text, and the code runs are the ones in the case table, so mono
/// against prose is checked on the data the template renders. A missing typst fails the test.
#[tokio::test]
async fn the_production_path_sends_the_same_text_and_code_runs_to_every_surface() {
    assert!(
        which_typst().is_some(),
        "typst must be on PATH: this test compiles the PDF"
    );
    let corpus = corpus().await;
    for (raw, plain, codes) in backtick_cases() {
        let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
        f.fix_specific = Some(raw.clone());
        f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
        let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
        let json = build_report_json(
            &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
            Some(&corpus),
            &empty_opts(),
        );
        let item = &json.escalations.items[0];
        // The surfaces: plain text and runs are the same text; the runs hold the code.
        assert_eq!(
            item.what_to_do,
            fix_text::join_plain(&item.what_to_do_segments),
            "{raw}"
        );
        let got_codes: Vec<String> = item
            .what_to_do_segments
            .iter()
            .filter_map(|s| match s {
                fix_text::Seg::Code(t) => Some(t.clone()),
                fix_text::Seg::Text(_) => None,
            })
            .collect();
        assert_eq!(got_codes, codes, "{raw}");
        if plain.chars().count() <= 400 {
            assert_eq!(item.what_to_do, plain, "{raw}");
        } else {
            assert!(
                plain.starts_with(item.what_to_do.trim_end_matches("...")),
                "{raw}"
            );
        }
        // The data the template reads carries the same runs.
        let data = render_data_json(&json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&data).unwrap();
        let sent: Vec<String> = value["escalations"]["items"][0]["what_to_do_segments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["code"].as_bool() == Some(true))
            .map(|s| s["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(sent, codes, "{raw}");
        // And the PDF prints that text.
        let pdf = crate::report_export::compile_pdf_from_data(&data)
            .await
            .expect("the PDF compiles");
        // Test-only hook (like CAMERATA_RENDER_OUT in `addition_tests`): keep each case's PDF
        // in this directory for a look.
        if let Ok(dir) = std::env::var("CAMERATA_RENDER_OUT_DIR") {
            let n = codes.len();
            std::fs::write(format!("{dir}/case-{}-{n}.pdf", raw.len()), &pdf)
                .expect("write the PDF for a look");
        }
        let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
        // Prose is compared strictly (a lost space between words fails), after the one allowed
        // extraction normalization. A long code run wraps in the PDF (`breakable` chunks it), so
        // the text layer may break it at a line end: code is compared without whitespace.
        let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat = norm(&text);
        let flat_nospace: String = flat.split_whitespace().collect();
        for seg in &item.what_to_do_segments {
            match seg {
                // A prose run is searched without its edge spaces, so the extraction's one
                // artifact (a space before punctuation that follows a code run) cannot matter,
                // while a space lost INSIDE the prose fails.
                fix_text::Seg::Text(t) => assert!(
                    flat.contains(&norm(t)),
                    "PDF text lacks the prose {t:?} (raw {raw:?}): {flat}"
                ),
                fix_text::Seg::Code(t) => assert!(
                    flat_nospace.contains(&t.split_whitespace().collect::<String>()),
                    "PDF text lacks the code {t:?} (raw {raw:?})"
                ),
            }
        }
    }
}

/// The export wire contract is unchanged: the raw fix fields the PDF reads are `serde(skip)`, so
/// json/inspection.json (the serialized rows) gains no key, and the fix keys carry plain text.
#[test]
fn the_exported_rows_gain_no_key_and_carry_plain_fix_text() {
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(REAL_SQL.to_string());
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new());
    let json = build_report_json(&reviewed, None, &empty_opts());
    let fe = crate::xlsx_export::build_findings_export(&reviewed, None, &json, &HashMap::new());
    let row = serde_json::to_value(&fe.findings[0]).expect("row serializes");
    let keys: Vec<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        !keys.iter().any(|k| k.contains("raw")),
        "a raw field leaked into the export: {keys:?}"
    );
    // The keys the export had before this change are all still there.
    for before in [
        "severity",
        "headline",
        "repo",
        "path",
        "line",
        "rule_id",
        "category",
        "bucket",
        "disposition_kind",
        "disposition_label",
        "effort",
        "est_hours",
        "confidence",
        "needs_review",
        "in_test",
        "provenance",
        "citation_label",
        "citation_urls",
        "also_matches",
        "status",
        "snippet",
        "detail",
        "fix_specific",
        "fix",
        "fix_type",
        "is_fp",
        "fp_reason",
        "also_locations",
        "flagged_by",
        "site_count",
    ] {
        assert!(keys.contains(&before), "{before} is missing: {keys:?}");
    }
    // The plain text keeps the template's backticks (the datasets are copy-paste ready).
    assert_eq!(row["fix_specific"].as_str(), Some(REAL_SQL));
}

/// Escalated fix text through the production path, using only API the code had before the
/// one-parser change (so the same test runs red against it).
async fn escalation_json_for(raw: &str) -> crate::report_export::AuditReportJson {
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(raw.to_string());
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    )
}

/// H1: the PDF used to parse the text twice, so a command substitution inside a longer run lost
/// its backticks ("echo date"). The PDF text must carry `date` with its backticks.
#[tokio::test]
async fn the_pdf_keeps_the_backticks_of_a_command_substitution() {
    assert!(which_typst().is_some(), "typst must be on PATH");
    let json = escalation_json_for("Run `` echo `date` `` to stamp it").await;
    let data = render_data_json(&json).unwrap();
    let pdf = crate::report_export::compile_pdf_from_data(&data)
        .await
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("echo"), "{flat}");
    assert!(
        flat.contains("`date`"),
        "the PDF lost the code's backticks: {flat}"
    );
}

/// The escalation cut never lands inside a template: a fix whose template runs past the 400
/// character limit is cut before it, never in the middle of its select.
#[tokio::test]
async fn the_escalation_cut_does_not_land_inside_a_long_template() {
    let body = "select id, title, price_cents, category from public.listings where status = 'active' and (title ilike ${'%' + q + '%'} or category ilike ${'%' + q + '%'}) order by created_at desc, id desc, price_cents asc, category asc, title asc, updated_at desc, owner_id asc, status asc, slug asc, region asc limit 30";
    let raw = format!(
        "Replace the sql.unsafe call with a tagged template so the search term is sent as bound parameters, for example: const rows = await sql`{body}`; also escape percent and underscore characters in q as in the other search route."
    );
    let json = escalation_json_for(&raw).await;
    let what = &json.escalations.items[0].what_to_do;
    assert!(
        !what.contains("select id") || what.contains("limit 30"),
        "the cut landed inside the template: {what}"
    );
}

/// The runs the PDF sets come from the FINAL client text: the sealed finding (secrets redacted)
/// after the reviewer's edits, the same `fix_specific` the data.json row and the workbook read.
/// A planted secret never appears in the runs, the plain text or the PDF, and a reviewer's edit
/// replaces the model's text everywhere.
#[tokio::test]
async fn the_segments_come_from_the_redacted_and_reviewed_fix_text() {
    use camerata_api_types::edits::{Edit, EditField};
    assert!(which_typst().is_some(), "typst must be on PATH");
    let corpus = corpus().await;
    let secret =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey)
            .value;
    let model_text = format!("Zorble `{secret}` now. Then redeploy the service.");
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(model_text.clone());
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let sealed = report_with(vec![f], vec![SERVICE_ROLE]).seal();

    let render = |reviewed: &crate::report_export::ReviewedReport| {
        let json = build_report_json(reviewed, Some(&corpus), &empty_opts());
        let data = render_data_json(&json).unwrap();
        (json, data)
    };
    let pdf_text = |data: String| async move {
        let pdf = crate::report_export::compile_pdf_from_data(&data)
            .await
            .expect("the PDF compiles");
        pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts")
    };

    // 1. Redaction: no raw secret in the runs, the plain text, the data, or the PDF.
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let (json, data) = render(&reviewed);
    let item = &json.escalations.items[0];
    let runs = fix_text::join_plain(&item.what_to_do_segments);
    assert!(!runs.contains(&secret), "the runs carry the secret: {runs}");
    assert!(!item.what_to_do.contains(&secret), "{}", item.what_to_do);
    assert!(
        !data.contains(&secret),
        "the data sent to Typst carries the secret"
    );
    let text = pdf_text(data).await;
    assert!(!text.contains(&secret), "the PDF carries the secret");
    assert!(
        runs.contains("Zorble"),
        "anti-vacuity: the fix is rendered: {runs}"
    );

    // 2. The reviewer's edit replaces the model's text in the runs and in the PDF.
    let site_key = sealed.findings[0].site_key.clone();
    let edit = Edit {
        id: "edit-000001".to_string(),
        key: site_key,
        field: EditField::Fix,
        old: String::new(),
        new: "Use the `safe_read` helper in src/lib.rs. Then redeploy.".to_string(),
        reason: "the engine was wrong".to_string(),
        by: "reviewer".to_string(),
        at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(
            &sealed.findings[0].rule_id,
            &sealed.findings[0].snippet,
        ),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    };
    let reviewed = crate::report_export::apply_overlay(&sealed, &[], &[edit], &[]);
    let (json, data) = render(&reviewed);
    let item = &json.escalations.items[0];
    assert_eq!(
        item.what_to_do,
        "Use the safe_read helper in src/lib.rs. Then redeploy."
    );
    assert!(item
        .what_to_do_segments
        .contains(&fix_text::Seg::Code("safe_read".to_string())));
    assert!(
        !data.contains("Zorble"),
        "the model's text is still in the data"
    );
    let text = pdf_text(data).await;
    assert!(!text.contains("Zorble"), "the model's text reached the PDF");
    assert!(text.contains("safe_read"), "the edit did not reach the PDF");
}

/// B: when the place's own row has no fix text, the Fix's own text is used (the fallback in
/// `xlsx_export/subgroups.rs`). It goes through the same runs and span-safe cut: a "." inside a
/// template is not a sentence end, the cut never lands inside it, and the code stays code.
#[tokio::test]
async fn the_fallback_fix_text_is_cut_like_the_main_path() {
    let sentence_in_code =
        "Call await sql`select a from t where b = 1. Order by id limit 30` and keep it.";
    let long_body = "select id, title, price_cents, category from public.listings where status = 'active' and (title ilike ${'%' + q + '%'} or category ilike ${'%' + q + '%'}) order by created_at desc, id desc, price_cents asc, category asc, title asc, updated_at desc, owner_id asc, status asc, slug asc, region asc, extra_one asc, extra_two asc, extra_three asc limit 30";
    let long = format!("Call await sql`{long_body}` and keep it.");
    for (fix, tag) in [(sentence_in_code.to_string(), "short"), (long, "long")] {
        // Place A has no fix text of its own (no corpus, no specific fix); place B, in the same
        // Fix, carries the example. A is the escalated place, so its text is the fallback.
        let mut a = finding("AI-CUSTOM-RULE-1", "src/a.ts", 3, "high");
        a.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
        let mut b = finding("AI-CUSTOM-RULE-1", "src/a.ts", 9, "critical");
        b.fix_specific = Some(fix.clone());
        let report = report_with(vec![a, b], vec!["AI-CUSTOM-RULE-1"]).seal();
        let json = build_report_json(
            &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
            None,
            &empty_opts(),
        );
        let item = json
            .escalations
            .items
            .iter()
            .find(|i| i.path == "src/a.ts" && i.line == 3)
            .unwrap_or_else(|| panic!("{tag}: the escalated place is listed"));
        assert!(
            !item.what_to_do.is_empty(),
            "{tag}: the fallback text is used"
        );
        assert_eq!(
            item.what_to_do.matches('`').count() % 2,
            0,
            "{tag}: the cut left an unbalanced backtick: {}",
            item.what_to_do
        );
        assert!(
            item.what_to_do_segments.iter().any(
                |s| matches!(s, fix_text::Seg::Code(t) if t.starts_with("sql`") && t.ends_with('`'))
            ),
            "{tag}: the template is one code run: {:?}",
            item.what_to_do_segments
        );
        assert_eq!(
            item.what_to_do,
            fix_text::join_plain(&item.what_to_do_segments)
        );
    }
}

/// D1: a secret placed in a REVIEWER'S EDIT (applied after the seal, so the seal's redaction never
/// saw it) must not reach the runs, the plain text, the data sent to Typst or the PDF.
#[tokio::test]
async fn a_secret_in_a_reviewers_edit_never_reaches_the_runs() {
    use camerata_api_types::edits::{Edit, EditField};
    assert!(which_typst().is_some(), "typst must be on PATH");
    let corpus = corpus().await;
    let secret =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey)
            .value;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some("Move it out.".to_string());
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let sealed = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let edit = Edit {
        id: "edit-000001".to_string(),
        key: sealed.findings[0].site_key.clone(),
        field: EditField::Fix,
        old: String::new(),
        new: format!("Replace `{secret}` with the `safe_read` helper. Then redeploy."),
        reason: "the engine was wrong".to_string(),
        by: "reviewer".to_string(),
        at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(
            &sealed.findings[0].rule_id,
            &sealed.findings[0].snippet,
        ),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    };
    let reviewed = crate::report_export::apply_overlay(&sealed, &[], &[edit], &[]);
    let json = build_report_json(&reviewed, Some(&corpus), &empty_opts());
    let item = &json.escalations.items[0];
    assert!(
        !fix_text::join_plain(&item.what_to_do_segments).contains(&secret),
        "the runs carry the reviewer's secret"
    );
    assert!(!item.what_to_do.contains(&secret), "{}", item.what_to_do);
    let data = render_data_json(&json).unwrap();
    assert!(
        !data.contains(&secret),
        "the data sent to Typst carries the secret"
    );
    let pdf = crate::report_export::compile_pdf_from_data(&data)
        .await
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    assert!(!text.contains(&secret), "the PDF carries the secret");
    // The rest of the edit is rendered (anti-vacuity).
    assert!(item.what_to_do.contains("safe_read"), "{}", item.what_to_do);
    // The workbook and CSV text read the same redacted text.
    let fe =
        crate::xlsx_export::build_findings_export(&reviewed, Some(&corpus), &json, &HashMap::new());
    let row = serde_json::to_string(&fe.findings[0]).unwrap();
    assert!(
        !row.contains(&secret),
        "the exported row carries the secret"
    );
}

/// D2: a code span in a headline reaches the template as a code run.
#[tokio::test]
async fn a_code_span_in_a_headline_is_a_code_run_in_the_data() {
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.detail = format!(
        "Calls `getSession()` for the identity check.{}It trusts the cookie.",
        crate::ai_audit::HEADLINE_SEP
    );
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let data: serde_json::Value = serde_json::from_str(&render_data_json(&json).unwrap()).unwrap();
    let runs = data["escalations"]["items"][0]["headline_segments"]
        .as_array()
        .expect("the headline runs are sent");
    assert!(
        runs.iter()
            .any(|s| s["code"] == true && s["text"] == "getSession()"),
        "the span is a code run: {runs:?}"
    );
    assert!(
        runs.iter()
            .all(|s| !s["text"].as_str().unwrap().contains('`')),
        "no delimiter backtick reaches the template: {runs:?}"
    );
}

/// D3: the two fallbacks in `render_data_json`. Runs missing for a non-empty `what_to_do` become
/// ONE prose run (the text is never parsed again); an empty `what_to_do` has no runs.
#[tokio::test]
async fn render_data_json_falls_back_to_one_prose_run_and_to_none() {
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let mut json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let item = &mut json.escalations.items[0];
    item.what_to_do = "Move `x` now.".to_string();
    item.what_to_do_segments.clear();
    let data: serde_json::Value = serde_json::from_str(&render_data_json(&json).unwrap()).unwrap();
    assert_eq!(
        data["escalations"]["items"][0]["what_to_do_segments"],
        serde_json::json!([{"code": false, "text": "Move `x` now."}])
    );
    let item = &mut json.escalations.items[0];
    item.what_to_do = String::new();
    let data: serde_json::Value = serde_json::from_str(&render_data_json(&json).unwrap()).unwrap();
    assert_eq!(
        data["escalations"]["items"][0]["what_to_do_segments"],
        serde_json::json!([])
    );
}

/// The round 0 parser (875aa6b7), reproduced as a small reference: code spans of any run length,
/// a single backtick glued to an identifier is a tagged template. Returns the runs as
/// `(is_code, text)`.
fn round0_runs(text: &str) -> Vec<(bool, String)> {
    let prepared = fix_text::prepare(text);
    let bytes = prepared.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let st = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            runs.push((st, i - st));
        } else {
            i += 1;
        }
    }
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut pos = 0usize;
    let mut k = 0usize;
    while k < runs.len() {
        let (start, n) = runs[k];
        let Some(j) = (k + 1..runs.len()).find(|&j| runs[j].1 == n) else {
            k += 1;
            continue;
        };
        let (cstart, _) = runs[j];
        let cend = cstart + n;
        let prefix = &prepared[pos..start];
        if n == 1 && prefix.as_bytes().last().is_some_and(|b| is_ident(*b)) {
            let pb = prefix.as_bytes();
            let mut s = pb.len();
            while s > 0 && (is_ident(pb[s - 1]) || pb[s - 1] == b'.') {
                s -= 1;
            }
            while s < pb.len() && !is_ident(pb[s]) {
                s += 1;
            }
            if s > 0 {
                out.push((false, prefix[..s].to_string()));
            }
            out.push((true, format!("{}{}", &prefix[s..], &prepared[start..cend])));
        } else {
            if !prefix.is_empty() {
                out.push((false, prefix.to_string()));
            }
            let body = &prepared[start + n..cstart];
            let stripped = if body.len() >= 2
                && body.starts_with(' ')
                && body.ends_with(' ')
                && !body.bytes().all(|b| b == b' ')
            {
                &body[1..body.len() - 1]
            } else {
                body
            };
            out.push((true, stripped.to_string()));
        }
        pos = cend;
        k = j + 1;
    }
    if pos < prepared.len() {
        out.push((false, prepared[pos..].to_string()));
    }
    out
}

fn current_runs(text: &str) -> Vec<(bool, String)> {
    fix_text::segments(&fix_text::prepare(text))
        .into_iter()
        .map(|s| match s {
            fix_text::Seg::Text(t) => (false, t),
            fix_text::Seg::Code(t) => (true, t),
        })
        .collect()
}

/// The model-written fix texts the repo's own test fixtures and the sample generator hold: the
/// string literal given to `fix_specific = Some(...)` or `fix_specific: Some(...)` (also inside
/// `format!`), read from the Rust sources. Synthetic fixtures only, never client data.
fn fixture_fix_texts() -> Vec<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let re = regex::Regex::new(
        r#"fix_specific[^;{}]{0,12}?(?:=|:)\s*Some\(\s*(?:format!\(\s*)?"((?:[^"\\]|\\.)*)""#,
    )
    .unwrap();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if !matches!(name, "target" | ".git" | "node_modules") {
                    stack.push(path);
                }
            } else if name.ends_with(".rs") && !name.ends_with("fix_text_tests.rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Ok(src) = std::fs::read_to_string(&f) else {
            continue;
        };
        for cap in re.captures_iter(&src) {
            let mut text = String::new();
            let mut chars = cap[1].chars().peekable();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    text.push(c);
                    continue;
                }
                match chars.next() {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('\n') => {
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next();
                        }
                    }
                    Some(other) => text.push(other),
                    None => {}
                }
            }
            out.push(text);
        }
    }
    out
}

/// Texts whose output differs between round 0 and the current parser, each with the reason. Every
/// entry is a case where the current parser prints a backtick that belongs to code, or prints a
/// text with a backtick glued to a word literally (one prose run) instead of guessing. The key is
/// a substring unique to the text.
const EXPECTED_CHANGES: &[(&str, &str)] = &[
    ("`const rows = await sql`select id limit 30`;`", "ambiguous: a backtick glued to a word on its outside, so literal; round 0 dropped the template's opening backtick"),
    ("Run `const q = sql`select 1`` now.", "ambiguous: literal; round 0 dropped the template's opening backtick"),
    ("Run `echo `date`` now", "ambiguous: literal; round 0 dropped the inner backticks"),
    ("Run `echo `date`` and `echo `pwd``", "ambiguous: literal; round 0 dropped the inner backticks"),
    ("the`foo` call", "ambiguous (glued word): literal; round 0 kept it as a tagged template"),
    ("`a`or`b`", "ambiguous (closer glued to a word): literal; round 0 mixed spans and a template"),
    ("don`t use `foo`", "ambiguous (opener glued to a word): literal; round 0 dropped delimiters"),
    ("use t`Hello` now", "ambiguous (one-word glued body): literal; round 0 kept it as a template"),
    (
        "use `fetch(`/api/users/${id}`)` here",
        "ambiguous (unbalanced brackets in a span): literal; round 0 dropped the template's backticks",
    ),
    (
        "use `db.query(sql`${cols} from t`)` here",
        "ambiguous (unbalanced brackets, closer before '$'): literal; round 0 dropped backticks",
    ),
    (
        "Use `html`<div>hi</div>`` now",
        "ambiguous (closer before '<'): literal; round 0 dropped backticks",
    ),
    ("call `foo`s", "ambiguous (closer glued to a word): literal; round 0 set `foo` in mono and ran the s on"),
    ("Use sql`a ${b`c`} d` now", "adjacent template runs are one code run; the text is the same"),
];

/// The number of option remediation texts in the shipped rule library. Update with the corpus.
const EXPECTED_CORPUS_TEXTS: usize = 537;
/// The number of fixture fix texts found in the repo's Rust sources. Update when fixtures change.
const EXPECTED_FIXTURE_TEXTS: usize = 28;

/// Safety net: every fix text the repo ships (the rule library's remediation texts, the fixtures'
/// model-written fix texts, plus every row of `backtick_cases`) goes through round 0 and the
/// current parser, in plain text AND in the runs the PDF sets. Any text whose output differs
/// must be listed in `EXPECTED_CHANGES` with a reason.
#[tokio::test]
async fn the_parser_changes_no_shipped_fix_text_beyond_the_listed_cases() {
    let corpus = corpus().await;
    let mut texts: Vec<String> = Vec::new();
    for rule in corpus.iter() {
        for option in &rule.options {
            if let Some(r) = &option.remediation {
                texts.push(r.clone());
            }
        }
    }
    let from_rules = texts.len();
    let fixtures = fixture_fix_texts();
    let from_fixtures = fixtures.len();
    texts.extend(fixtures);
    let from_cases = backtick_cases().len();
    texts.extend(backtick_cases().into_iter().map(|(raw, _, _)| raw));
    assert_eq!(
        from_rules, EXPECTED_CORPUS_TEXTS,
        "the corpus count changed"
    );
    assert_eq!(
        from_fixtures, EXPECTED_FIXTURE_TEXTS,
        "the fixture count changed"
    );
    let mut differing: Vec<(String, String, String)> = Vec::new();
    for t in &texts {
        let (old_runs, new_runs) = (round0_runs(t), current_runs(t));
        let old_plain: String = old_runs.iter().map(|(_, x)| x.as_str()).collect();
        let new_plain = fix_text::plain(t);
        if old_plain != new_plain || old_runs != new_runs {
            differing.push((t.clone(), format!("{old_runs:?}"), format!("{new_runs:?}")));
        }
    }
    eprintln!(
        "differential: {} texts checked ({from_rules} rule library, {from_fixtures} fixtures, {from_cases} cases), {} differ",
        texts.len(),
        differing.len()
    );
    // The rule library's own texts do not change at all.
    assert!(
        differing
            .iter()
            .all(|(t, _, _)| !texts[..from_rules].contains(t)),
        "a rule library text changed"
    );
    let unexpected: Vec<String> = differing
        .iter()
        .filter(|(t, _, _)| !EXPECTED_CHANGES.iter().any(|(key, _)| t.contains(key)))
        .map(|(t, old, new)| format!("TEXT {t:?}\n  round0 {old}\n  now    {new}"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "unlisted parser changes:\n{}",
        unexpected.join("\n")
    );
    for (key, reason) in EXPECTED_CHANGES {
        assert!(
            differing.iter().any(|(t, _, _)| t.contains(key)),
            "stale expected change {key:?} ({reason})"
        );
    }
}

/// The fallback path reads the Fix's raw text (`worked_example_raw`): a planted secret in the
/// other place's fix text never reaches the escalated place's runs, plain text or data.
#[tokio::test]
async fn the_fallback_runs_carry_no_secret() {
    let secret =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey)
            .value;
    let mut a = finding("AI-CUSTOM-RULE-1", "src/a.ts", 3, "high");
    a.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let mut b = finding("AI-CUSTOM-RULE-1", "src/a.ts", 9, "critical");
    b.fix_specific = Some(format!("Replace `{secret}` with the `safe_read` helper."));
    let report = report_with(vec![a, b], vec!["AI-CUSTOM-RULE-1"]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let item = json
        .escalations
        .items
        .iter()
        .find(|i| i.line == 3)
        .expect("the escalated place is listed");
    assert!(
        item.what_to_do.contains("safe_read"),
        "anti-vacuity: the fallback text is used: {}",
        item.what_to_do
    );
    assert!(!fix_text::join_plain(&item.what_to_do_segments).contains(&secret));
    assert!(!render_data_json(&json).unwrap().contains(&secret));
}

/// Finding 3: `prepare` joins the lines of a multi-line template, which must keep its backticks.
#[test]
fn a_multi_line_template_keeps_its_backticks() {
    assert_eq!(
        fix_text::plain("Run sql`\n select 1\n`; now"),
        "Run sql` select 1 `; now"
    );
}

/// The ambiguity rule: a backtick glued to a word on its outside makes the text ONE prose run
/// with every backtick printed, and ordinary prose spans around it are not touched.
#[test]
fn an_ambiguous_text_is_one_literal_prose_run() {
    for raw in [
        "`const rows = await sql`select id limit 30`;`",
        "Run `echo `date`` and `echo `pwd``",
        "call `foo`s",
    ] {
        assert_eq!(
            fix_text::segments(&fix_text::prepare(raw)),
            vec![fix_text::Seg::Text(raw.to_string())],
            "{raw}"
        );
    }
    // An ordinary prose text is split into spans as plain CommonMark would.
    assert!(fix_text::segments("the anon key (`A_KEY`) and (`A_URL`)")
        .contains(&fix_text::Seg::Code("A_URL".to_string())));
}

/// In a literal (ambiguous) text the cut never lands between the first and the last backtick: a
/// "? AND" inside SQL is not a sentence end, and a limit inside the code cuts before it.
#[test]
fn a_cut_never_lands_inside_a_literal_texts_code() {
    // "? AND" inside the code is not a sentence end.
    let raw =
        "Use `const r = await sql`select 1 where a = ? AND b = 2`;` first. Then more. Third one.";
    let plain = fix_text::join_plain(&crate::report_export::first_sentences_raw(raw, 1, 400));
    assert!(plain.contains("AND b = 2"), "{plain}");
    assert!(plain.ends_with("first."), "{plain}");
    // A wrapped template over the limit: cut before it, so no partial code shows.
    let body = "select id, title, price_cents, category from public.listings where status = 'active' and (title ilike ${'%' + q + '%'}) order by created_at desc, id desc, price_cents asc, category asc, title asc, updated_at desc, owner_id asc, status asc, slug asc, region asc, extra_one asc, extra_two asc, extra_three asc, extra_four asc, extra_five asc, extra_six asc, extra_seven asc, extra_eight asc limit 30";
    let raw = format!("Use `const r = await sql`{body}`;` and keep it.");
    // The code ends within twice the cap: the sentence that holds the last backtick is shown whole.
    let plain = fix_text::join_plain(&crate::report_export::first_sentences_raw(&raw, 2, 400));
    assert_eq!(plain.matches('`').count(), 4, "{plain}");
    assert!(plain.ends_with("and keep it."), "{plain}");
}

/// The advisory path (`first_sentences`, plain text) never takes the literal cut: it behaves as it
/// always did, even when the advisory holds backticks.
#[test]
fn an_advisory_with_backticks_is_cut_like_any_plain_text() {
    let advisory = "Versions of `lodash` before 4.17 are vulnerable. Use `_.merge` with a trusted source only, and upgrade as soon as a fixed release is available to you, because the merge helper can be abused to change object prototypes in your application.";
    assert!(advisory.chars().count() > 160);
    assert_eq!(
        crate::report_export::first_sentences(advisory, 1, 160),
        "Versions of `lodash` before 4.17 are vulnerable."
    );
    let starts = format!("`lodash` before 4.17 is vulnerable. {}", &advisory[49..]);
    assert_eq!(
        crate::report_export::first_sentences(&starts, 1, 160),
        "`lodash` before 4.17 is vulnerable."
    );
}

/// A literal text is never emitted beyond twice the cap (the PDF card; the workbook keeps the
/// full text). Code ending beyond that is cut before the first backtick.
#[test]
fn a_literal_cut_is_bounded() {
    let pad = "col, ".repeat(600);
    // First backtick at index 0, 3000 characters: nothing to show but "...".
    let raw = format!("`const r = await sql`select {pad} limit 30`;` done.");
    assert!(raw.chars().count() > 2500);
    let plain = fix_text::join_plain(&crate::report_export::first_sentences_raw(&raw, 2, 400));
    assert_eq!(plain, "...");
    // First backtick at character 2: only the prose before it.
    let raw = format!("A:`const r = await sql`select {pad} limit 30`;` done.");
    let plain = fix_text::join_plain(&crate::report_export::first_sentences_raw(&raw, 2, 400));
    assert_eq!(plain, "A...");
    // Code that ends within twice the cap but a sentence that runs past it: cut at a word after
    // the code, no further than twice the cap.
    let raw = format!(
        "Use `const r = await sql`select 1`; {}",
        "word ".repeat(400)
    );
    let plain = fix_text::join_plain(&crate::report_export::first_sentences_raw(&raw, 2, 400));
    assert!(plain.chars().count() <= 803, "{}", plain.chars().count());
    assert!(plain.ends_with("..."), "{plain}");
    assert!(plain.contains("select 1`;"), "{plain}");
}

/// Each wrapped template whose inner backtick touches punctuation is literal: every backtick is
/// printed and nothing is set in mono. All three are checked, and all failures listed.
#[test]
fn wrapped_templates_with_punctuation_go_literal() {
    let bad: Vec<String> = [
        "use `fetch(`/api/users/${id}`)` here",
        "use `db.query(sql`${cols} from t`)` here",
        "Use `html`<div>hi</div>`` now",
    ]
    .iter()
    .filter(|raw| {
        fix_text::segments(&fix_text::prepare(raw)) != vec![fix_text::Seg::Text(raw.to_string())]
    })
    .map(|raw| format!("{raw} -> {:?}", fix_text::plain(raw)))
    .collect();
    assert!(bad.is_empty(), "not literal: {bad:?}");
}
