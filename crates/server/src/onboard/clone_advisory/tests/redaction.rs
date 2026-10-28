//! A clone finding quotes the first line of each copy and the strings the copies differ in; both
//! are client code, so both are redacted at construction (docs/design/2026-10-23, path A15).

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::*;

/// `files` with the given 1-based line of `path` replaced by `replacement`.
fn with_line_replaced(
    files: &[(String, String)],
    path: &str,
    line: usize,
    replacement: &str,
) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(p, content)| {
            if p != path {
                return (p.clone(), content.clone());
            }
            let lines: Vec<String> = content
                .lines()
                .enumerate()
                .map(|(i, l)| {
                    if i + 1 == line {
                        replacement.to_string()
                    } else {
                        l.to_string()
                    }
                })
                .collect();
            (p.clone(), lines.join("\n") + "\n")
        })
        .collect()
}

#[tokio::test]
async fn clone_confirmed_finding_redacts_first_line_and_also_locations() {
    let clean = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the discount");
    let baseline = judge(
        Some(&stub),
        &clean,
        set_for(&clean, &[CloneRule::SingleSourceLogic]),
    )
    .await;
    let f = &baseline.findings[0];
    let (primary_path, primary_line) = (f.path.clone(), f.line);
    let (other_path, other_line) = (f.also_locations[0].path.clone(), f.also_locations[0].line);

    // The judge is handed the same candidates, but the files the finding quotes now carry a
    // secret on the line each site starts at.
    let p = planted(SecretShape::StripeLiveSecretKey);
    let tainted = with_line_replaced(&clean, &primary_path, primary_line, &p.text);
    let tainted = with_line_replaced(&tainted, &other_path, other_line, &p.text);
    let set = set_for(&clean, &[CloneRule::SingleSourceLogic]);
    let stub = StubJudge::answering("same_decision", "the discount");
    let mut cache = VerdictCache::default();
    let outcome =
        judge_clone_candidates(Some(&stub), request(&tainted, set), &mut cache, None, None).await;
    assert!(!outcome.findings.is_empty());
    for f in &outcome.findings {
        let json = serde_json::to_string(f).expect("serializes");
        assert!(!json.contains(&p.value), "clone finding leaked: {json}");
        assert!(
            f.snippet.contains("sk_live_"),
            "the redacted form stands in: {}",
            f.snippet
        );
    }
}

/// H2: the code the judge SENDS to the model is redacted too, not only the finding it returns.
/// The secret sits on the first line of each copy, which the judge quotes in its prompt.
#[tokio::test]
async fn the_judge_prompt_carries_no_secret_value() {
    let clean = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the discount");
    let baseline = judge(
        Some(&stub),
        &clean,
        set_for(&clean, &[CloneRule::SingleSourceLogic]),
    )
    .await;
    let f = &baseline.findings[0];
    let (primary_path, primary_line) = (f.path.clone(), f.line);
    let (other_path, other_line) = (f.also_locations[0].path.clone(), f.also_locations[0].line);

    let p = planted(SecretShape::StripeLiveSecretKey);
    let tainted = with_line_replaced(&clean, &primary_path, primary_line, &p.text);
    let tainted = with_line_replaced(&tainted, &other_path, other_line, &p.text);
    let set = set_for(&clean, &[CloneRule::SingleSourceLogic]);
    let stub = StubJudge::answering("same_decision", "the discount");
    let mut cache = VerdictCache::default();
    let _ =
        judge_clone_candidates(Some(&stub), request(&tainted, set), &mut cache, None, None).await;
    assert!(stub.call_count() > 0, "the judge was asked");
    for n in 0..stub.call_count() {
        let req = stub.request(n);
        assert!(
            !req.prompt.contains(&p.value),
            "the judge prompt leaked a secret value"
        );
    }
    assert!(
        stub.request(0).prompt.contains("sk_live_"),
        "the redacted form stands in for the value"
    );
}

#[test]
fn not_assessed_row_redacts_its_reason() {
    let p = planted(SecretShape::GitHubToken);
    let row = findings::not_assessed_row(
        "acme/shop",
        CloneRule::SingleSourceLogic,
        NotAssessedReason::ModelDeclined,
        format!("the judge quoted {}", p.value),
        format!("could not decide: {}", p.value),
    );
    let json = serde_json::to_string(&row).expect("serializes");
    assert!(!json.contains(&p.value), "{json}");
}
