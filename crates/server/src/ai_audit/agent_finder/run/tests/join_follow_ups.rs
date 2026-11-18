//! Follow-ups from the third review round: the joined problems text, the clash repair message,
//! which files count as problems outputs, and paths that name the same place.

use super::*;

fn big_files() -> Vec<(String, String)> {
    vec![("src/a.rs".to_string(), rust_file(60))]
}

fn item(n: u32, line: usize) -> Value {
    problem(
        n,
        "src/a.rs",
        line,
        "low",
        &format!("fn item_{line}() {{ run({line}); }}"),
    )
}

/// A review of `src/a.rs` whose problems file holds `first`, with SEC-TWO-1 left unanswered so
/// the runner asks one follow-up.
fn first_step(first: Vec<Value>) -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 60)],
        first,
        vec![],
        vec![answer_clean("ARCH-ONE-1", "src/a.rs", 50)],
    )
}

fn sec_answer() -> ScriptStep {
    out(
        "checklist.json",
        &json!({"answers": [answer_clean("SEC-TWO-1", "src/a.rs", 51)]}),
    )
}

async fn go(agent: &ScriptedAgent) -> FinderResult {
    run_with(agent, &big_files(), &rules(), &corpus(), &options()).await
}

#[tokio::test]
async fn the_joined_problems_text_is_not_held_to_the_forty_per_file_cap() {
    let base: Vec<Value> = (1..=35).map(|n| item(n, n as usize)).collect();
    let later: Vec<Value> = (36..=45).map(|n| item(n, n as usize)).collect();
    let agent = ScriptedAgent::new(first_step(base)).with_follow_up(
        vec![out("problems-1.json", &Value::Array(later)), sec_answer()],
        AgentStop::Finished,
    );
    let (rows, _, _, failed, _, _) = go(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(agent.messages().len(), 1, "{:?}", agent.messages());
    assert_eq!(rows.len(), 45, "the engine's own joined text is usable");
}

#[tokio::test]
async fn the_agents_own_valid_file_of_forty_one_problems_is_read() {
    let many: Vec<Value> = (1..=41).map(|n| item(n, n as usize)).collect();
    let agent = ScriptedAgent::new(first_step(many))
        .with_follow_up(vec![sec_answer()], AgentStop::Finished);
    let (rows, _, _, failed, _, _) = go(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(rows.len(), 41);
}

#[tokio::test]
async fn the_repair_request_restates_the_format_and_every_refusal() {
    let bare = json!([{"n": 1, "path": "src/a.rs", "line": 2, "description": "x"}]);
    let mut script = first_step(vec![]);
    script.push(out("problems-1.json", &bare));
    script.push(out("problems-2.json", &bare));
    let agent = ScriptedAgent::new(script);
    let _ = go(&agent).await;
    let sent = agent.messages();
    let repair = sent
        .iter()
        .find(|m| m.contains("problems-1.json"))
        .unwrap_or_else(|| panic!("a repair turn names the file: {sent:?}"));
    assert!(repair.contains("problems-2.json"), "{repair}");
    for field in crate::ai_audit::agent_finder::brief_text::PROBLEM_FIELDS.split(", ") {
        assert!(repair.contains(field), "{field}: {repair}");
    }
    assert!(repair.contains("may not be dropped"), "{repair}");
    assert!(
        repair.contains("stopping before the `=` or `:`"),
        "{repair}"
    );
    assert!(!repair.contains("value omitted"), "{repair}");
}

#[tokio::test]
async fn the_clash_repair_message_names_the_earlier_place_and_asks_for_a_renumber() {
    let agent = ScriptedAgent::new(first_step(vec![item(1, 2)])).with_follow_up(
        vec![out("problems-1.json", &json!([item(1, 4)])), sec_answer()],
        AgentStop::Finished,
    );
    let _ = go(&agent).await;
    let sent = agent.messages();
    let repair = sent
        .iter()
        .find(|m| m.contains("problems-1.json"))
        .unwrap_or_else(|| panic!("a repair turn names the file: {sent:?}"));
    assert!(repair.contains("earlier step"), "{repair}");
    assert!(repair.contains("number 1"), "{repair}");
    assert!(repair.contains("src/a.rs:2"), "{repair}");
    assert!(repair.contains("keep that problem"), "{repair}");
    assert!(repair.contains("renumber"), "{repair}");
    assert!(!repair.contains("used twice"), "{repair}");
}

#[tokio::test]
async fn only_problems_json_files_are_problems_outputs() {
    for name in ["problems.md", "problems-notes.txt"] {
        let mut script = good_script(vec![], vec![]);
        script.push(out(name, &json!({"note": "nothing here"})));
        let agent = ScriptedAgent::new(script);
        let (_, _, _, failed, _, extras) = run(&agent).await;
        assert!(
            agent.messages().is_empty(),
            "{name}: no repair turn for a file that is not a problems output: {:?}",
            agent.messages()
        );
        assert!(failed.is_empty(), "{name}: {failed:?}");
        assert!(
            extras.notes.iter().any(|n| n.contains(name)),
            "{name}: still visible: {:?}",
            extras.notes
        );
    }
}

#[tokio::test]
async fn a_leading_dot_slash_names_the_same_place() {
    let agent = ScriptedAgent::new(first_step(vec![item(1, 2)])).with_follow_up(
        vec![
            out(
                "problems-1.json",
                &json!([problem(
                    1,
                    "./src/a.rs",
                    2,
                    "high",
                    "fn item_2() { run(2); }"
                )]),
            ),
            sec_answer(),
        ],
        AgentStop::Finished,
    );
    let (rows, _, _, failed, _, _) = go(&agent).await;
    assert_eq!(agent.messages().len(), 1, "{:?}", agent.messages());
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(rows.len(), 1, "a correction, not a clash: {rows:?}");
}
