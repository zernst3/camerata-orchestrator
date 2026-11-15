//! Stage 1 calibration sees the file: the whole file a site is in when it fits the budget (the
//! 61-line window above it), and a second pass with a file its reply names when the answer
//! depends on code there. Synthetic data only.

use super::calibration_tests::{numbered_file, response, run, security_site, site, Scripted, GOOD};
use super::*;

/// A file of `n` lines, each `width` characters of padding before its number.
fn wide_file(path: &str, n: usize, width: usize) -> (String, String) {
    let pad = "x".repeat(width);
    (
        path.to_string(),
        (1..=n).map(|i| format!("{pad} line {i}\n")).collect(),
    )
}

/// A file of exactly `chars` characters (ASCII), ending in a newline.
fn file_of_exactly(path: &str, chars: usize) -> (String, String) {
    let line = format!("{}\n", "y".repeat(99));
    let mut body = line.repeat(chars / line.len());
    let rest = chars - body.len();
    if rest > 0 {
        body.push_str(&"z".repeat(rest - 1));
        body.push('\n');
    }
    assert_eq!(body.chars().count(), chars);
    (path.to_string(), body)
}

// ── The whole file under the budget, the window over it ────────────────────────────────────

#[tokio::test]
async fn the_prompt_shows_the_whole_file_under_the_budget_and_the_window_over_it() {
    // A 200-line file well under the budget is shown whole, every line numbered.
    let llm = Scripted::text(GOOD);
    let files = vec![numbered_file("a.rs", 200)];
    let _ = run(&llm, vec![site("a.rs", 100)], &files).await;
    let reqs = llm.calibration_requests();
    assert_eq!(reqs.len(), 1);
    let prompt = &reqs[0].prompt;
    assert!(
        prompt.contains("    1| line 1\n"),
        "the first line of the file"
    );
    assert!(
        prompt.contains("  200| line 200\n"),
        "the last line of the file"
    );
    assert!(
        prompt.contains("The whole file a.rs (200 lines; the finding is at line 100)"),
        "{prompt}"
    );

    // A file over the budget is shown as the window: 30 lines each side and no more.
    let llm = Scripted::text(GOOD);
    let files = vec![wide_file("big.rs", 400, 100)];
    assert!(files[0].1.chars().count() > WHOLE_FILE_BUDGET_CHARS);
    let _ = run(&llm, vec![site("big.rs", 100)], &files).await;
    let prompt = &llm.calibration_requests()[0].prompt;
    assert!(prompt.contains(" line 70\n") && prompt.contains(" line 130\n"));
    assert!(!prompt.contains(" line 69\n") && !prompt.contains(" line 131\n"));
    assert!(!prompt.contains("The whole file"), "{prompt}");
    assert!(prompt.contains("lines 70 to 130 of big.rs"), "{prompt}");

    // The budget is inclusive: a file of exactly the budget is whole, one character more is not.
    for (chars, whole) in [
        (WHOLE_FILE_BUDGET_CHARS, true),
        (WHOLE_FILE_BUDGET_CHARS + 1, false),
    ] {
        let llm = Scripted::text(GOOD);
        let files = vec![file_of_exactly("edge.rs", chars)];
        let _ = run(&llm, vec![site("edge.rs", 120)], &files).await;
        let prompt = &llm.calibration_requests()[0].prompt;
        assert_eq!(
            prompt.contains("The whole file edge.rs"),
            whole,
            "{chars} characters"
        );
    }
    // The engine reads the estimator's budget, so the quote prices what is sent.
    assert_eq!(
        WHOLE_FILE_BUDGET_CHARS,
        camerata_ui_core::scan::CALIBRATION_FILE_BUDGET_CHARS
    );
}

// ── The needs-more-code second pass ───────────────────────────────────────────────────────

const NEEDS_AUTH: &str = r#"{"severity":"high","real":"unclear","effort":"low","category":"authorization","reason":"the admin check is imported from another file","more_code_file":"src/auth.ts"}"#;
const REAL_WITH_AUTH: &str = r#"{"severity":"high","real":"yes","effort":"low","category":"authorization","reason":"requireAdmin only checks that a session exists"}"#;

fn route_and_auth() -> Vec<(String, String)> {
    vec![
        (
            "src/route.ts".to_string(),
            "import { requireAdmin } from './auth';\nexport async function POST() {\n  await requireAdmin();\n  return deleteAll();\n}\n"
                .replace("requireAdmin", "guard"),
        ),
        (
            "src/auth.ts".to_string(),
            "export async function requireAdmin() {\n  const s = await session();\n  if (!s) throw new Error('no');\n}\n"
                .to_string(),
        ),
    ]
}

#[tokio::test]
async fn a_reply_naming_another_file_gets_a_second_pass_with_that_file() {
    // The first pass sees only the route and says the answer is in src/auth.ts; the second pass
    // is shown that file and settles it.
    let llm = Scripted::new(|req| {
        Ok(response(
            if req.prompt.contains("export async function requireAdmin") {
                REAL_WITH_AUTH
            } else {
                NEEDS_AUTH
            },
        ))
    });
    let files = route_and_auth();
    let (out, failed) = run(&llm, vec![security_site("src/route.ts", 4)], &files).await;
    assert!(failed.is_empty(), "{failed:?}");
    let reqs = llm.calibration_requests();
    assert_eq!(reqs.len(), 2, "the first pass and one second pass");
    assert!(!reqs[0]
        .prompt
        .contains("export async function requireAdmin"));
    let second = &reqs[1].prompt;
    assert!(
        second.contains("export async function requireAdmin"),
        "the named file is in the second pass:\n{second}"
    );
    assert!(second.contains("call_4();"), "with the same site");
    assert!(second.contains("src/auth.ts"), "{second}");
    assert!(
        second.contains("return deleteAll();"),
        "and the site's own file"
    );
    // The second verdict is the row's, and the row records which file settled it.
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].confidence.as_deref(), Some("high"));
    assert!(!out[0].needs_review);
    let stage1 = stage1_of(&out[0]).expect("a stage 1 record");
    assert_eq!(stage1.real, Real::Yes);
    assert_eq!(stage1.more_code_file.as_deref(), Some("src/auth.ts"));
}

#[tokio::test]
async fn the_second_pass_is_asked_once_and_only_for_a_file_it_can_show() {
    let files = route_and_auth();

    // A second reply that names a file again gets no third pass: its verdict stands.
    let llm = Scripted::text(NEEDS_AUTH);
    let (out, _) = run(&llm, vec![security_site("src/route.ts", 4)], &files).await;
    assert_eq!(
        llm.calibration_requests().len(),
        2,
        "one second pass at most"
    );
    assert_eq!(out[0].confidence.as_deref(), Some("needs-review"));

    // A path written without its folder still names the file when only one file ends in it.
    let llm = Scripted::new(|req| {
        Ok(response(
            if req.prompt.contains("export async function requireAdmin") {
                REAL_WITH_AUTH
            } else {
                NEEDS_AUTH_BARE.as_str()
            },
        ))
    });
    let (out, _) = run(&llm, vec![security_site("src/route.ts", 4)], &files).await;
    assert_eq!(llm.calibration_requests().len(), 2);
    assert_eq!(out[0].confidence.as_deref(), Some("high"));

    // No second pass for a file the run does not have, for the site's own file, for a file
    // over the budget, or for a verdict that is not `unclear`.
    let big = wide_file("src/huge.ts", 400, 100);
    let mut with_big = files.clone();
    with_big.push(big);
    for named in ["src/missing.ts", "src/route.ts", "src/huge.ts"] {
        let reply = NEEDS_AUTH.replace("src/auth.ts", named);
        let llm = Scripted::text(&reply);
        let (out, failed) = run(&llm, vec![security_site("src/route.ts", 4)], &with_big).await;
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            llm.calibration_requests().len(),
            1,
            "no second pass for {named}"
        );
        assert_eq!(out[0].confidence.as_deref(), Some("needs-review"));
        assert_eq!(stage1_of(&out[0]).expect("record").more_code_file, None);
    }
    let llm = Scripted::text(&NEEDS_AUTH.replace(r#""real":"unclear""#, r#""real":"yes""#));
    let _ = run(&llm, vec![security_site("src/route.ts", 4)], &files).await;
    assert_eq!(
        llm.calibration_requests().len(),
        1,
        "a settled verdict is not asked again"
    );
}

static NEEDS_AUTH_BARE: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| NEEDS_AUTH.replace("src/auth.ts", "auth.ts"));

// ── The rubric: unauthenticated access is high by default (owner ruling R-8) ──────────────

#[test]
fn the_rubric_rates_unauthenticated_access_high_by_default_and_critical_on_two_conditions() {
    let system = verify_system_prompt();
    let flat: String = system.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(
            "Unauthenticated access to a protected resource or operation is \"high\" by default."
        ),
        "{flat}"
    );
    // The two conditions that make it critical, and nothing else.
    assert!(
        flat.contains("It is \"critical\" only when it reaches sensitive data or allows a write:"),
        "{flat}"
    );
    assert!(flat.contains("(a) it reaches sensitive data"), "{flat}");
    assert!(flat.contains("(b) it allows a write"), "{flat}");
    assert!(
        flat.contains("An unauthenticated read of data that is not sensitive stays \"high\"."),
        "{flat}"
    );
    // The old rule of thumb is gone, and no other line still says it.
    assert!(!flat.contains("ALWAYS \"critical\""), "{flat}");
    assert!(!flat.contains("Unauthenticated access to ANY protected resource"));
    assert!(
        !flat.contains("\"critical\" is reserved for an unauthenticated reach or"),
        "the client-side gate line follows the ruling: {flat}"
    );
}
