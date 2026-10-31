//! Retry fixes F3, item 4: the detail reply splits the explanation (`why`) from the direction of
//! the fix (`fix_direction`). The exploit-precondition check reads `why` alone; what a client
//! reads is the two joined, word for word what one `detail` field of the same words would have
//! said. No network: every model is a stub.

use std::sync::Mutex;

use super::detail_regen::{
    accept_entry, parse_detail_reply, regenerate_full_details, Entry, Reject,
};
use super::reply_schemas::check::prompt_ids;
use super::structured_output_tests::{files, finding, response};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const WHY: &str = "The handler passes raw input to doWork with no check, so any caller reaches it.";
const DIRECTION: &str = "Validate the input in the handler before it reaches doWork.";

/// Answers each detail prompt with one entry per id, shaped by `legacy`: `{id, detail}` of the
/// two sentences joined, or `{id, why, fix_direction}`.
struct Shaped {
    legacy: bool,
    log: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for Shaped {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let entries: Vec<serde_json::Value> = prompt_ids(&req.prompt)
            .iter()
            .map(|id| {
                if self.legacy {
                    serde_json::json!({"id": id, "detail": format!("{WHY} {DIRECTION}")})
                } else {
                    serde_json::json!({"id": id, "why": WHY, "fix_direction": DIRECTION})
                }
            })
            .collect();
        self.log.lock().unwrap().push(req);
        Ok(response(
            &serde_json::json!({ "details": entries }).to_string(),
            "api",
            "claude-sonnet-5-5",
        ))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::generate_avoiding(&[])
}

fn statement() -> Finding {
    finding(
        "src/work.ts",
        2,
        &format!("Raw input reaches doWork{HEADLINE_SEP}Passes raw input to doWork unchecked."),
        "doWork(raw)",
    )
}

async fn written(legacy: bool) -> Finding {
    let llm = Shaped {
        legacy,
        log: Mutex::new(Vec::new()),
    };
    let (mut out, failed) = regenerate_full_details(
        &llm,
        "o/r",
        vec![statement()],
        &files(),
        &[],
        &nonce(),
        None,
        None,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    out.remove(0)
}

/// What a client reads: the two fields joined with a space, the same text one `detail` field of
/// those words gives. Pinned so a client-facing explanation does not change with the reply shape.
#[tokio::test]
async fn the_split_reply_reads_exactly_as_a_single_detail_of_the_same_words() {
    let split = written(false).await;
    let single = written(true).await;
    assert_eq!(split.detail, single.detail);
    assert_eq!(
        split.detail,
        format!("Raw input reaches doWork{HEADLINE_SEP}{WHY} {DIRECTION}")
    );
}

/// The precondition check reads the explanation alone: a fix direction that says what the fix
/// needs, in any wording, is not an exploit precondition.
#[test]
fn the_precondition_check_reads_why_and_not_the_fix_direction() {
    let f = statement();
    let entry = |why: &str, direction: &str| Entry {
        id: "x".to_string(),
        detail: format!("{why} {direction}"),
        why: Some(why.to_string()),
    };
    // A fix direction that does not open with a fix verb and says "requires" is not read.
    let ok = entry(
        WHY,
        "Make sure the endpoint requires a valid session cookie before doWork runs.",
    );
    assert!(accept_entry(&f, &ok, "", &files()).is_ok());
    // The same words in `why` are an exploit precondition nobody made.
    let bad = entry(
        "Exploiting this requires a valid session cookie, so doWork is reachable.",
        DIRECTION,
    );
    assert_eq!(
        accept_entry(&f, &bad, "", &files()),
        Err(Reject::InventedPrecondition)
    );
    // A single `detail` (a backend that never saw the schema) is read by sentence, as before.
    let legacy = Entry {
        id: "x".to_string(),
        detail: format!("{WHY} Exploiting this requires a valid session cookie."),
        why: None,
    };
    assert_eq!(
        accept_entry(&f, &legacy, "", &files()),
        Err(Reject::InventedPrecondition)
    );
}

/// Retry fixes F3, item 7: the check names exploit preconditions (who must be logged in, what must
/// be enabled for an attacker), not every "depends on". The two shapes that turned down good
/// Sonnet explanations in the efficiency run pass; the real ones are still turned down.
#[test]
fn only_an_exploit_precondition_in_why_is_turned_down() {
    let f = statement();
    let with_why = |why: &str| Entry {
        id: "x".to_string(),
        detail: format!("{why} {DIRECTION}"),
        why: Some(why.to_string()),
    };
    for harmless in [
        "How the sort order looks depends on the runtime's locale, and doWork formats it with no check.",
        "The retention of these rows depends on your audit obligations, and doWork keeps them forever.",
        "The helper requires a trailing slash, and doWork builds the path without one.",
        "doWork reads the value, which depends on the order the files are loaded.",
    ] {
        assert!(
            accept_entry(&f, &with_why(harmless), "", &files()).is_ok(),
            "{harmless}"
        );
    }
    for exploit in [
        "Exploitation requires a valid session cookie, and doWork runs with no check.",
        "An attacker must be logged in first, and doWork then runs with no check.",
        "This is reachable only if the admin feature is enabled for the caller, and doWork skips the check.",
        "It depends on whether the attacker holds a valid token, and doWork never looks.",
        "Assuming the user is signed in, doWork runs with no check.",
        // An exploit precondition told from the other side: the victim, the attack, a forged or
        // stolen credential, a compromised account.
        "The attack requires the victim to open a crafted link while doWork runs.",
        "This depends on a forged cookie reaching doWork.",
        "Reading the rows requires a stolen refresh token, and doWork never checks it.",
        "It is a problem only if an account is already compromised, since doWork trusts it.",
    ] {
        assert_eq!(
            accept_entry(&f, &with_why(exploit), "", &files()),
            Err(Reject::InventedPrecondition),
            "{exploit}"
        );
    }
}

/// The calibration guard reads the shared phrase list, which is unchanged: a written explanation
/// with a bare phrase still blocks an upward calibration there.
#[test]
fn the_calibration_guard_still_reads_the_whole_phrase_list() {
    for text in [
        "It depends on the runtime's locale.",
        "The helper requires a trailing slash.",
    ] {
        assert!(mentions_exploit_precondition(text), "{text}");
    }
    assert!(!mentions_exploit_precondition(
        "The endpoint requires no authentication."
    ));
}

/// The reader takes both shapes: split fields, one `detail`, and a reply with a `why` only.
#[test]
fn the_reader_takes_the_split_the_single_and_the_partial_shape() {
    let raw = serde_json::json!({"details": [
        {"id": "a", "why": "Why.", "fix_direction": "Fix."},
        {"id": "b", "detail": "Whole text."},
        {"id": "c", "why": "Only why."},
        {"id": "d", "fix_direction": "Only direction."},
        {"id": "e"},
        {"id": "f", "why": "  ", "fix_direction": " "}
    ]})
    .to_string();
    let parsed = parse_detail_reply(&raw, false);
    let by_id = |id: &str| parsed.entries.iter().find(|e| e.id == id).cloned();
    assert_eq!(by_id("a").unwrap().detail, "Why. Fix.");
    assert_eq!(by_id("a").unwrap().why.as_deref(), Some("Why."));
    assert_eq!(by_id("b").unwrap().detail, "Whole text.");
    assert_eq!(by_id("b").unwrap().why, None);
    assert_eq!(by_id("c").unwrap().detail, "Only why.");
    assert_eq!(by_id("d").unwrap().detail, "Only direction.");
    assert!(by_id("e").is_none() && by_id("f").is_none());
    assert_eq!(parsed.unusable, 2);
}

/// The prompt asks for the two fields and says which one the condition rule is about.
#[test]
fn the_prompt_asks_for_why_and_fix_direction() {
    let system = detail_regen::detail_regen_system_prompt();
    assert!(
        system.contains(r#""why":"...","fix_direction":"...""#),
        "{system}"
    );
    assert!(system.contains("In `why`, do not write that exploiting it requires"));
}
