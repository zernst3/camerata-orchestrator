//! The agent finder's rows in Agent activity: every model call of a finder run (each piece, the
//! repair turn, send again, the missing-rules turn and the joints call) shows as its own row when
//! it starts, with the prompt it was handed, and ends done or blocked with its output. The same
//! transcript store and the same row shape as the clone judge ([`crate::onboard::clone_advisory`]).
//! Nothing here changes what a call sends or what the run does with the answer.

use super::super::{AgentRun, AgentStop};
use crate::transcript::{AgentTranscript, TranscriptStore};

/// The most output text a row keeps for one call.
const MAX_OUTPUT_CHARS: usize = 20_000;

/// Where the finder's rows go: the run's transcript store and the key the Agent activity panel
/// reads. Carried with the run's future ([`super::with_finder_activity`]).
#[derive(Clone)]
pub struct FinderActivity {
    store: TranscriptStore,
    key: String,
}

impl FinderActivity {
    pub fn new(store: TranscriptStore, key: &str) -> Self {
        Self {
            store,
            key: key.to_string(),
        }
    }

    /// Open a running row for one call. Secrets are redacted from the prompt.
    pub(super) fn begin(&self, session: &str, role: &str, prompt: &str) {
        let mut prompt = prompt.to_string();
        crate::onboard::redact::redact_string(&mut prompt);
        self.store.register(
            &self.key,
            AgentTranscript {
                session_id: session.to_string(),
                role: role.to_string(),
                prompt,
                output: String::new(),
                status: "running".to_string(),
            },
        );
    }

    /// Close a row after the call came back: done when the call ended normally, blocked with the
    /// reason when it did not.
    pub(super) fn end(&self, session: &str, result: &anyhow::Result<AgentRun>) {
        match result {
            Ok(run) => {
                let mut text = String::new();
                for (path, body) in &run.outputs {
                    text.push_str(&format!("--- {path} ---\n{body}\n"));
                }
                let mut text = clip(&text);
                crate::onboard::redact::redact_string(&mut text);
                if !text.is_empty() {
                    self.store.append_output(&self.key, session, &text);
                }
                let turns = run.usage.len();
                let calls = run.trace.len();
                let (line, status) = match &run.stop {
                    AgentStop::Finished => (
                        format!("Done: {turns} model turn(s), {calls} file or search call(s)"),
                        "done",
                    ),
                    AgentStop::TurnLimit(why)
                    | AgentStop::SpendLimit(why)
                    | AgentStop::Refused(why)
                    | AgentStop::Failed(why) => (format!("Did not finish: {why}"), "blocked"),
                };
                self.store.append_output(&self.key, session, &line);
                self.store.set_status(&self.key, session, status);
            }
            Err(e) => {
                let mut why = e.to_string();
                crate::onboard::redact::redact_string(&mut why);
                self.store
                    .append_output(&self.key, session, &format!("Did not run: {why}"));
                self.store.set_status(&self.key, session, "blocked");
            }
        }
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n[output cut at {MAX_OUTPUT_CHARS} characters]")
}
