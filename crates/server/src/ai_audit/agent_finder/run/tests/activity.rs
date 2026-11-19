//! Every model call of a finder run shows in Agent activity: a running row when it starts, done
//! or blocked (with the reason) when it ends. No model is called; a stand-in agent plays the
//! calls and looks at the transcript store while its call is still open.

use std::sync::Mutex;

use super::super::with_finder_activity;
use super::super::FinderActivity;
use super::*;
use crate::transcript::{AgentTranscript, TranscriptStore};

const KEY: &str = "scan-key";

/// Plays the wrapped agent and records what the transcript store showed while each call was open.
struct Watching {
    inner: ScriptedAgent,
    store: TranscriptStore,
    open_views: Mutex<Vec<Vec<AgentTranscript>>>,
}

impl Watching {
    fn new(inner: ScriptedAgent, store: &TranscriptStore) -> Self {
        Self {
            inner,
            store: store.clone(),
            open_views: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl PieceAgent for Watching {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.open_views.lock().unwrap().push(self.store.get(KEY));
        self.inner.run(room, brief, limits).await
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.open_views.lock().unwrap().push(self.store.get(KEY));
        self.inner.follow_up(room, prior, turn, limits).await
    }

    fn release(&self, handle: &super::super::super::ConversationHandle) {
        self.inner.release(handle);
    }
}

/// An agent whose every call fails before anything is sent.
struct Refusing;

#[async_trait::async_trait]
impl PieceAgent for Refusing {
    async fn run(&self, _: &Room, _: &str, _: &PieceLimits) -> anyhow::Result<AgentRun> {
        anyhow::bail!("the command line tool could not start")
    }

    async fn follow_up(
        &self,
        _: &Room,
        _: &AgentRun,
        _: &FollowUp,
        _: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        anyhow::bail!("not reached")
    }
}

async fn shown<A: PieceAgent>(
    agent: &A,
    store: &TranscriptStore,
    opts: &FinderOptions,
) -> FinderResult {
    with_finder_activity(
        Some(FinderActivity::new(store.clone(), KEY)),
        run_with(agent, &repo_files(), &rules(), &corpus(), opts),
    )
    .await
}

fn row<'a>(rows: &'a [AgentTranscript], role: &str) -> &'a AgentTranscript {
    rows.iter()
        .find(|r| r.role == role)
        .unwrap_or_else(|| panic!("no row {role:?} in {rows:#?}"))
}

#[tokio::test]
async fn a_piece_call_shows_a_running_row_before_it_returns_and_a_done_row_after() {
    let store = TranscriptStore::new();
    let agent = Watching::new(ScriptedAgent::new(good_script(vec![], vec![])), &store);
    let (_rows, _, _, failed, _, _) = shown(&agent, &store, &options()).await;
    assert!(failed.is_empty(), "{failed:?}");

    // While the call was open, its row was already there and running, with the brief as prompt.
    let open = agent.open_views.lock().unwrap().clone();
    assert_eq!(open.len(), 1, "one call, no follow-up");
    let role = "Finder, piece 1 of 1 (acme/app)";
    let during = row(&open[0], role);
    assert_eq!(during.status, "running");
    assert!(
        during.prompt.contains("security and architecture review"),
        "the generated brief is the prompt: {}",
        during.prompt
    );

    // After it, the same row is done and carries what the agent wrote.
    let after = store.get(KEY);
    assert_eq!(after.len(), 1, "{after:#?}");
    let done = row(&after, role);
    assert_eq!(done.status, "done");
    assert!(done.output.contains("_camerata/out/problems-1.json"));
    assert!(done.output.starts_with("---") && done.output.contains("Done:"));
}

#[tokio::test]
async fn a_piece_that_fails_shows_blocked_with_the_reason() {
    let store = TranscriptStore::new();
    let agent = ScriptedAgent::new(vec![read_all("src/a.rs", 6)])
        .with_stop(AgentStop::Failed("the tool loop broke".to_string()));
    let _ = shown(&agent, &store, &options()).await;
    let rows = store.get(KEY);
    let failed = row(&rows, "Finder, piece 1 of 1 (acme/app)");
    assert_eq!(failed.status, "blocked", "{failed:#?}");
    assert!(
        failed.output.contains("the tool loop broke"),
        "the reason is shown: {}",
        failed.output
    );
}

#[tokio::test]
async fn a_call_that_never_started_shows_blocked_with_the_reason() {
    let store = TranscriptStore::new();
    let _ = with_finder_activity(
        Some(FinderActivity::new(store.clone(), KEY)),
        find_repo(
            &Refusing,
            REPO,
            &repo_files(),
            &rules(),
            &corpus(),
            &RunControls::default(),
            &options(),
            None,
        ),
    )
    .await;
    let rows = store.get(KEY);
    let failed = row(&rows, "Finder, piece 1 of 1 (acme/app)");
    assert_eq!(failed.status, "blocked");
    assert!(failed.output.contains("could not start"), "{failed:#?}");
}

#[tokio::test]
async fn a_repair_turn_is_its_own_row_and_a_secret_in_a_prompt_is_redacted() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;
    let store = TranscriptStore::new();
    // A problems file the reader refuses draws the repair turn.
    let mut script = good_script(vec![], vec![]);
    script.insert(
        0,
        ScriptStep::Write {
            path: "_camerata/out/problems-2.json".to_string(),
            text: "not json".to_string(),
        },
    );
    let agent = ScriptedAgent::new(script).with_again(vec![]);
    let _ = shown(&agent, &store, &options()).await;
    let rows = store.get(KEY);
    assert!(
        rows.iter()
            .any(|r| r.role == "Finder, piece 1 of 1, repair turn (acme/app)"),
        "{rows:#?}"
    );

    // A secret that reaches a prompt is redacted in the row.
    let activity = FinderActivity::new(store.clone(), "other");
    let p = planted(SecretShape::StripeLiveSecretKey);
    activity.begin("s", "role", &format!("the key is {}", p.text));
    let shown_prompt = store.get("other")[0].prompt.clone();
    assert!(!shown_prompt.contains(&p.text), "{shown_prompt}");
}

#[tokio::test]
async fn every_call_of_a_two_piece_run_shows_including_the_joints_call() {
    let store = TranscriptStore::new();
    let (a, b) = (
        ScriptedAgent::new(full_script(
            &[("src/a.rs", 6)],
            vec![],
            vec![],
            vec![
                answer_clean("ARCH-ONE-1", "src/a.rs", 1),
                answer_clean("SEC-TWO-1", "src/a.rs", 1),
            ],
        )),
        ScriptedAgent::new(full_script(
            &[("src/b.rs", 4)],
            vec![],
            vec![],
            vec![
                answer_clean("ARCH-ONE-1", "src/b.rs", 1),
                answer_clean("SEC-TWO-1", "src/b.rs", 1),
            ],
        )),
    );
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([])),
    ]);
    let agent = ByPiece::new(vec![("src/a.rs", a), ("src/b.rs", b), ("joints", joints)]);
    let mut opts = options();
    opts.force_pieces = Some(2);
    let _ = shown(&agent, &store, &opts).await;
    let rows = store.get(KEY);
    for role in [
        "Finder, piece 1 of 2 (acme/app)",
        "Finder, piece 2 of 2 (acme/app)",
        "Finder, joints between the pieces (acme/app)",
    ] {
        assert_eq!(row(&rows, role).status, "done", "{role}: {rows:#?}");
    }
}

#[tokio::test]
async fn with_no_activity_the_run_is_unchanged_and_shows_nothing() {
    let store = TranscriptStore::new();
    let agent = ScriptedAgent::new(good_script(vec![], vec![]));
    let _ = run(&agent).await;
    assert!(store.get(KEY).is_empty());
}

#[test]
fn a_secret_across_the_output_cut_is_redacted_before_the_cut() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;
    let p = planted(SecretShape::StripeLiveSecretKey);
    // The secret starts 32 characters before the 20,000 character cut (after the file header).
    let mut body = "x".repeat(19_930);
    body.push_str(&p.text);
    body.push_str(&"y".repeat(100));
    let mut run = AgentRun::empty(AgentStop::Finished);
    run.outputs
        .insert("_camerata/out/problems-1.json".to_string(), body);
    let store = TranscriptStore::new();
    let activity = FinderActivity::new(store.clone(), KEY);
    activity.begin("s", "role", "prompt");
    activity.end("s", &Ok(run));
    let output = store.get(KEY)[0].output.clone();
    let prefix = &p.value[8..14];
    assert!(
        !output.contains(prefix),
        "no part of the secret survives the cut: {}",
        &output[output.len().saturating_sub(300)..]
    );
}

#[test]
fn a_secret_in_a_failure_reason_is_redacted_in_the_done_or_blocked_line() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;
    let p = planted(SecretShape::StripeLiveSecretKey);
    for stop in [
        AgentStop::TurnLimit(format!("limit {}", p.text)),
        AgentStop::SpendLimit(format!("spend {}", p.text)),
        AgentStop::Refused(format!("refused {}", p.text)),
        AgentStop::Failed(format!("failed {}", p.text)),
    ] {
        let store = TranscriptStore::new();
        let activity = FinderActivity::new(store.clone(), KEY);
        activity.begin("s", "role", "prompt");
        activity.end("s", &Ok(AgentRun::empty(stop.clone())));
        let row = &store.get(KEY)[0];
        assert_eq!(row.status, "blocked");
        assert!(row.output.contains("Did not finish:"), "{stop:?}");
        assert!(!row.output.contains(&p.text), "{stop:?}: {}", row.output);
    }
}
