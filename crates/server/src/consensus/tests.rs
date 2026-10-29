use super::*;

fn f(rule: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        ..Finding::default()
    }
}

#[test]
fn a_finding_two_of_three_passes_report_is_kept_once_and_a_one_off_is_dropped() {
    let shared = f("ARCH-A-1", "src/a.rs", 3, "a()");
    let mut shared_moved = shared.clone();
    shared_moved.line = 4; // the same finding quoted at a neighbouring line
    let one_off = f("ARCH-B-1", "src/b.rs", 9, "b()");
    let passes = vec![
        vec![shared.clone(), one_off],
        vec![shared_moved],
        vec![f("ARCH-C-1", "src/c.rs", 1, "c()")],
    ];
    let (kept, sample) = consensus_findings(&passes, CONSENSUS_QUORUM);
    assert_eq!(kept, vec![shared], "the earliest pass's copy is kept, once");

    assert_eq!(sample.stage, CONSENSUS_STAGE);
    assert_eq!((sample.rows_in, sample.rows_out), (4, 1));
    assert_eq!(
        sample.accounting.deduped, 1,
        "the agreeing copy from pass 2"
    );
    assert_eq!(
        sample.accounting.other,
        vec![("consensus_minority_dropped".to_string(), 2)]
    );
    assert_eq!(
        sample.accounting.accounted(),
        3,
        "every removed row is accounted for"
    );
}

#[test]
fn repeats_are_kept_as_often_as_the_quorum_agrees() {
    let x = f("ARCH-A-1", "src/a.rs", 3, "a()");
    let mut x2 = x.clone();
    x2.line = 30;
    let passes = vec![
        vec![x.clone(), x2.clone()],
        vec![x.clone(), x2.clone()],
        vec![x.clone()],
    ];
    let (kept, sample) = consensus_findings(&passes, CONSENSUS_QUORUM);
    assert_eq!(kept, vec![x, x2], "two passes saw it twice");
    assert_eq!((sample.rows_in, sample.rows_out), (5, 2));
    assert_eq!(sample.accounting.accounted(), 3);
}

#[test]
fn identical_passes_keep_everything_and_order_follows_the_first_pass() {
    let run = vec![
        f("ARCH-B-1", "src/b.rs", 1, "b()"),
        f("ARCH-A-1", "src/a.rs", 1, "a()"),
    ];
    let (kept, sample) = consensus_findings(&[run.clone(), run.clone(), run.clone()], 2);
    assert_eq!(kept, run);
    assert_eq!(sample.accounting.deduped, 4);
    assert!(sample.accounting.other.iter().all(|(_, n)| *n == 0));
}

#[test]
fn a_quorum_of_one_keeps_the_union() {
    let a = f("ARCH-A-1", "src/a.rs", 1, "a()");
    let b = f("ARCH-B-1", "src/b.rs", 1, "b()");
    let (kept, _) = consensus_findings(&[vec![a.clone()], vec![b.clone()]], 1);
    assert_eq!(kept, vec![a, b]);
}

// ── run_audit_passes ───────────────────────────────────────────────────────────────────────

fn output(findings: Vec<Finding>, failed: &str) -> AuditRepoOutput {
    let failed = if failed.is_empty() {
        Vec::new()
    } else {
        vec![crate::ai_audit::FailedPass {
            repo: "acme/app".to_string(),
            pass: failed.to_string(),
            reason: "r".to_string(),
            files_not_reviewed: 0,
            class: crate::ai_audit::FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
        }]
    };
    (findings, Vec::new(), Vec::new(), failed, Vec::new())
}

/// Runs `run_audit_passes` over scripted per-pass results; returns the result and the number
/// of audits it ran.
async fn passes(
    consensus: bool,
    script: Vec<anyhow::Result<AuditRepoOutput>>,
) -> (anyhow::Result<AuditRepoOutput>, usize) {
    let script = std::sync::Mutex::new(script.into_iter());
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let out = run_audit_passes("acme/app", consensus, || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let next = script.lock().unwrap().next().expect("scripted");
        async move { next }
    })
    .await;
    (out, calls.into_inner())
}

#[tokio::test]
async fn off_runs_once_and_returns_the_audit_untouched() {
    let a = f("ARCH-A-1", "src/a.rs", 1, "a()");
    let (out, calls) = passes(false, vec![Ok(output(vec![a.clone()], "hour estimation"))]).await;
    let out = out.unwrap();
    assert_eq!(calls, 1);
    assert_eq!(out.0, vec![a]);
    assert_eq!(out.3.len(), 1);
    assert!(out.4.is_empty(), "no vote stage when consensus is off");
}

#[tokio::test]
async fn on_votes_over_three_audits_and_dedupes_their_disclosures() {
    let a = f("ARCH-A-1", "src/a.rs", 1, "a()");
    let b = f("ARCH-B-1", "src/b.rs", 1, "b()");
    let (out, calls) = passes(
        true,
        vec![
            Ok(output(vec![a.clone(), b], "hour estimation")),
            Ok(output(vec![a.clone()], "hour estimation")),
            Ok(output(vec![a.clone()], "")),
        ],
    )
    .await;
    let out = out.unwrap();
    assert_eq!(calls, CONSENSUS_PASSES);
    assert_eq!(out.0, vec![a]);
    assert_eq!(
        out.3.len(),
        1,
        "the same disclosure from two audits is listed once"
    );
    assert_eq!(
        out.4.last().map(|s| s.stage.as_str()),
        Some(CONSENSUS_STAGE)
    );
}

#[tokio::test]
async fn an_audit_that_errors_is_disclosed_and_the_other_two_must_still_agree() {
    let a = f("ARCH-A-1", "src/a.rs", 1, "a()");
    let b = f("ARCH-B-1", "src/b.rs", 1, "b()");
    let (out, _) = passes(
        true,
        vec![
            Ok(output(vec![a.clone(), b], "")),
            Err(anyhow::anyhow!("the model is unavailable")),
            Ok(output(vec![a.clone()], "")),
        ],
    )
    .await;
    let out = out.unwrap();
    assert_eq!(
        out.0,
        vec![a],
        "two answers, quorum two: only the agreed finding"
    );
    let disclosed: Vec<&str> = out.3.iter().map(|p| p.pass.as_str()).collect();
    assert_eq!(disclosed, vec!["consensus audit 2 of 3"]);
    assert!(out.3[0].reason.contains("the model is unavailable"));
    assert!(out.3[0].reason.contains("2 of 3 consensus audits answered"));
}

#[tokio::test]
async fn when_no_audit_answers_the_first_error_is_returned() {
    let (out, calls) = passes(
        true,
        vec![
            Err(anyhow::anyhow!("first")),
            Err(anyhow::anyhow!("second")),
            Err(anyhow::anyhow!("third")),
        ],
    )
    .await;
    assert_eq!(calls, 3);
    assert_eq!(out.unwrap_err().to_string(), "first");
}
