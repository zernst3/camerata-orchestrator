//! Hostile-input bounds: no recursion past a fixed depth and no unbounded work, so one rules file
//! cannot abort the process that scans it. The reviewer's stress shapes, run on the 2 MiB stack a
//! tokio worker gets (the process aborts on overflow, so a regression here kills the test binary).

use super::{findings, firestore, rows, run};
use crate::arch_checker::{ArchViolation, ViolationGrade};
use crate::firebase::grants::covers;
use crate::firebase::rules_parse::Segment;
use crate::firebase::{RULE_FIELD_VALIDATION, RULE_OPEN};

/// Run the rules checker over `files` on a thread with a 2 MiB stack, the tokio worker default.
fn run_small_stack(files: Vec<(String, String)>) -> Vec<ArchViolation> {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || run(&files))
        .expect("spawn")
        .join()
        .expect("the checker must finish without panicking or overflowing")
}

#[test]
fn an_8000_deep_or_condition_is_not_recognised_and_does_not_abort() {
    let n = 8000;
    let body = format!(
        "    match /a/{{x}} {{ allow read: if {}true{}; }}",
        "(a||".repeat(n),
        ")".repeat(n)
    );
    let all = run_small_stack(firestore(&body));
    assert!(
        rows(&all, RULE_OPEN, ViolationGrade::Defect).is_empty(),
        "an over-deep condition is not recognised as open"
    );
}

#[test]
fn a_4000_deep_and_condition_is_not_recognised_and_does_not_abort() {
    let n = 4000;
    let body = format!(
        "    match /a/{{x}} {{ allow update: if {}request.auth.uid == request.resource.data.o{}; }}",
        "(a&&".repeat(n),
        ")".repeat(n)
    );
    let all = run_small_stack(firestore(&body));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn a_deep_condition_inside_the_cap_is_still_recognised() {
    // 40 nested disjuncts: real conditions are a handful deep, and 40 is inside the cap.
    let n = 40;
    let body = format!(
        "    match /a/{{x}} {{ allow read, write: if {}true{}; }}",
        "(a||".repeat(n),
        ")".repeat(n)
    );
    let all = run_small_stack(firestore(&body));
    assert_eq!(
        rows(&all, RULE_OPEN, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}

fn lit(s: &str) -> Segment {
    Segment::Literal(s.to_string())
}

fn recursive(name: &str) -> Segment {
    Segment::Wildcard {
        name: name.to_string(),
        recursive: true,
    }
}

#[test]
fn covers_on_a_60000_segment_shared_prefix_does_not_abort() {
    let n = 60_000;
    let mut broad: Vec<Segment> = (0..n).map(|_| lit("a")).collect();
    broad.push(recursive("x"));
    let mut narrow: Vec<Segment> = (0..n).map(|_| lit("a")).collect();
    narrow.push(lit("b"));
    let covered = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || covers(&broad, &narrow, true))
        .expect("spawn")
        .join()
        .expect("covers must not overflow the stack");
    assert!(!covered, "a path past the cap is not recognised as covered");
}

#[test]
fn a_60000_segment_match_path_degrades_the_file_to_not_assessed() {
    let body = format!(
        "    match /{{x=**}} {{ allow read: if true; }}\n    match {} {{ allow read: if false; }}",
        "/a".repeat(60_000)
    );
    let all = run_small_stack(firestore(&body));
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
    assert!(all[0].message.contains("64"), "{}", all[0].message);
}

#[test]
fn covers_with_many_recursive_wildcards_is_not_exponential() {
    // Ten recursive wildcards over a 48-segment path: the naive search tries C(48, 10) splits.
    let broad: Vec<Segment> = (0..10).map(|i| recursive(&format!("w{i}"))).collect();
    let narrow: Vec<Segment> = (0..48).map(|_| lit("a")).collect();
    assert!(covers(&broad, &narrow, true));
    assert!(!covers(&broad, &narrow[..0], false), "v1 needs one segment");
    let mut tail = broad.clone();
    tail.push(lit("z"));
    assert!(
        !covers(&tail, &narrow, true),
        "the literal tail never matches"
    );
}

#[test]
fn covers_keeps_its_v1_and_v2_meaning_for_a_recursive_wildcard() {
    let broad = vec![lit("a"), recursive("r")];
    assert!(covers(&broad, &[lit("a")], true), "v2: zero or more");
    assert!(!covers(&broad, &[lit("a")], false), "v1: one or more");
    assert!(covers(&broad, &[lit("a"), lit("b"), lit("c")], false));
    assert!(!covers(&broad, &[lit("b")], true));
}

// ── size caps ───────────────────────────────────────────────────────────────────────────────

use crate::firebase::RULE_AUTH_ONLY;

/// `levels` helper layers, each calling the next `fan` times; the last one is "signed in". The
/// fully inlined condition is `fan ^ levels` copies of the sign-in check.
fn helper_pyramid(levels: usize, fan: usize) -> String {
    let mut out = String::from("    function leaf() { return request.auth != null; }\n");
    for level in (0..levels).rev() {
        let next = if level + 1 == levels {
            "leaf".to_string()
        } else {
            format!("f{}", level + 1)
        };
        let calls: Vec<String> = (0..fan).map(|_| format!("{next}()")).collect();
        out.push_str(&format!(
            "    function f{level}() {{ return {}; }}\n",
            calls.join(" && ")
        ));
    }
    out
}

#[test]
fn a_small_helper_pyramid_is_inlined_and_recognised() {
    // 4 x 4 = 16 copies of the check: small, so it is read as signed-in-only.
    let body = format!(
        "{}    match /a/{{x}} {{ allow read: if f0(); }}",
        helper_pyramid(2, 4)
    );
    let all = run(&firestore(&body));
    assert_eq!(
        rows(&all, RULE_AUTH_ONLY, ViolationGrade::Review).len(),
        1,
        "{all:#?}"
    );
}

#[test]
fn a_helper_expansion_past_64_kib_is_not_recognised() {
    // 10^4 copies of a 19-byte check, joined by `&&`: over 200 KiB once inlined.
    let body = format!(
        "{}    match /a/{{x}} {{ allow read, write: if f0(); }}",
        helper_pyramid(4, 10)
    );
    let all = run_small_stack(firestore(&body));
    assert!(
        rows(&all, RULE_AUTH_ONLY, ViolationGrade::Defect).is_empty()
            && rows(&all, RULE_AUTH_ONLY, ViolationGrade::Review).is_empty(),
        "an expansion past the cap is not recognised: {all:#?}"
    );
}

#[test]
fn a_condition_that_was_not_recognised_is_never_claimed_to_skip_the_incoming_document() {
    let body = format!(
        "{}    match /a/{{x}} {{ allow update: if f0(); }}",
        helper_pyramid(4, 10)
    );
    let all = run_small_stack(firestore(&body));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

fn many_allows(n: usize) -> Vec<(String, String)> {
    let mut body = String::from(
        "    function isOwner(u) { return request.auth.uid == u; }\n    match /a/{x} {\n",
    );
    for _ in 0..n {
        body.push_str("      allow read: if isOwner(x) || request.auth.token.admin == true;\n");
    }
    body.push_str("    }");
    firestore(&body)
}

#[test]
fn a_file_with_more_than_5000_allows_is_not_assessed() {
    let all = run_small_stack(many_allows(20_000));
    assert_eq!(all.len(), 1, "{:#?}", all.len());
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
    assert!(all[0].message.contains("5000"), "{}", all[0].message);
}

#[test]
fn a_file_just_inside_the_allow_cap_is_still_assessed() {
    // One block, one field-validation row however many allows it holds.
    let mut body = String::from("    match /a/{x} {\n");
    for _ in 0..3000 {
        body.push_str("      allow write: if request.auth.token.admin == true;\n");
    }
    body.push_str("    }");
    let all = run_small_stack(firestore(&body));
    assert_eq!(
        rows(&all, RULE_FIELD_VALIDATION, ViolationGrade::Review).len(),
        1,
        "{} rows",
        all.len()
    );
}
