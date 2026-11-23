//! The strings a client can read use the client words: no retired word, no dash, no serial comma.
//! (The failed-part reasons reach the Coverage disclosure; the rows reach the problem lists.)

use super::*;
use crate::ai_audit::UsageMeter;

fn plain(text: &str, what: &str) {
    assert_eq!(
        crate::xlsx_export::vocabulary::banned_synonym_in(text),
        None,
        "{what}: {text}"
    );
    assert!(
        !text.contains(['\u{2014}', '\u{2013}'])
            && !text.contains(", and ")
            && !text.contains(", or "),
        "{what}: {text}"
    );
}

async fn failures() -> Vec<crate::ai_audit::FailedPass> {
    let mut all = Vec::new();
    // Files not read, even after sending again.
    let a = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    ))
    .with_follow_up(vec![], AgentStop::Finished);
    let _ = a;
    // A turn limit on the main review, a refusal, and a failure.
    for stop in [
        AgentStop::TurnLimit("60 turns".into()),
        AgentStop::Refused("declined".into()),
        AgentStop::SpendLimit("a piece ceiling".into()),
        AgentStop::Failed("HTTP 529 overloaded".into()),
    ] {
        let agent = ScriptedAgent::new(good_script(vec![], vec![])).with_stop(stop);
        all.extend(run(&agent).await.3);
    }
    // A rule with no answer, a clean answer with no valid place, and output that cannot be used.
    let missing = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/b.rs", 99),
        ],
    ));
    all.extend(run(&missing).await.3);
    let mut bad = good_script(vec![], vec![]);
    bad[2] = ScriptStep::Write {
        path: "_camerata/out/problems-1.json".into(),
        text: "not json".into(),
    };
    all.extend(run(&ScriptedAgent::new(bad)).await.3);
    // A second step that leaves a file unread, and a file the room cannot serve.
    let skip = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    ))
    .with_follow_up(vec![], AgentStop::TurnLimit("60 turns".into()));
    all.extend(run(&skip).await.3);
    let mut files = repo_files();
    files.push(("_camerata/notes.rs".to_string(), rust_file(2)));
    let ok = ScriptedAgent::new(good_script(vec![], vec![]));
    all.extend(
        run_with(&ok, &files, &rules(), &corpus(), &options())
            .await
            .3,
    );
    // Not run: the spend limit.
    let meter = UsageMeter::with_controls(RunControls {
        scope: Default::default(),
        max_cost_usd: Some(0.5),
        ..RunControls::default()
    });
    let mut two = options();
    two.force_pieces = Some(2);
    let costly = |p: &str, n: usize| {
        ScriptedAgent::new(full_script(
            &[(p, n)],
            vec![],
            vec![],
            vec![
                answer_clean("ARCH-ONE-1", p, 1),
                answer_clean("SEC-TWO-1", p, 2),
            ],
        ))
        .with_usage(usage(3.0))
    };
    let agent = ByPiece::new(vec![
        ("src/a.rs", costly("src/a.rs", 6)),
        ("src/b.rs", costly("src/b.rs", 4)),
        (
            "joints",
            ScriptedAgent::new(vec![out("problems-1.json", &json!([]))]),
        ),
    ]);
    let r = find_repo(
        &agent,
        REPO,
        &repo_files(),
        &rules(),
        &corpus(),
        &RunControls::default(),
        &two,
        Some(&meter),
    )
    .await
    .unwrap();
    all.extend(r.3);
    all
}

#[tokio::test]
async fn every_failed_part_reason_and_pass_name_uses_the_client_words() {
    let all = failures().await;
    assert!(all.len() >= 8, "{} failed parts: {all:?}", all.len());
    for f in &all {
        plain(&f.pass, "pass name");
        plain(&f.reason, "reason");
    }
}

#[tokio::test]
async fn the_rows_the_runner_writes_itself_use_the_client_words() {
    // A lead nothing settled, a departure and the agent's own wording.
    let (a, b) = {
        let a = ScriptedAgent::new(full_script(
            &[("src/a.rs", 6)],
            vec![],
            vec![],
            vec![
                answer_clean("ARCH-ONE-1", "src/a.rs", 1),
                answer_clean("SEC-TWO-1", "src/a.rs", 2),
            ],
        ));
        let b = ScriptedAgent::new(full_script(
            &[("src/b.rs", 4)],
            vec![problem(
                1,
                "src/a.rs",
                3,
                "medium",
                "fn item_3() { run(3); }",
            )],
            vec![],
            vec![
                answer_clean("ARCH-ONE-1", "src/b.rs", 1),
                answer_clean("SEC-TWO-1", "src/b.rs", 2),
            ],
        ));
        (a, b)
    };
    let agent = ByPiece::new(vec![
        ("src/a.rs", a),
        ("src/b.rs", b),
        (
            "joints",
            ScriptedAgent::new(vec![out("problems-1.json", &json!([]))]),
        ),
    ]);
    let mut two = options();
    two.force_pieces = Some(2);
    let (rows, ..) = run_with(&agent, &repo_files(), &rules(), &corpus(), &two).await;
    let lead = rows
        .iter()
        .find(|f| f.rule_id == "AI-OPEN-QUESTION")
        .expect("the lead is a row");
    let detail = lead
        .detail
        .replace(crate::ai_audit::digest_parse::HEADLINE_SEP, " ");
    plain(&detail, "lead row");
    assert!(lead.needs_review);
}
