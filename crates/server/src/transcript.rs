//! Per-agent transcripts: the generated prompt each agent was handed and its output,
//! captured per run so the cockpit can surface what Camerata is actually telling its
//! agents — the prompting that is otherwise abstracted away.
//!
//! The user never types these prompts: they are GENERATED (governance framing + the
//! story's task). This store makes them, and the agent's resulting output, inspectable
//! live. One transcript per agent/session; a run with multiple agents has multiple.
//!
//! Output is APPENDED as the run progresses, so a poller sees it grow. Today the
//! scripted run path populates this with the real generated prompt + the agent's tool
//! calls and their REAL gate verdicts; when the live `claude -p` fleet path captures
//! per-agent streams, it writes here through the same seam (and the same UI upgrades to
//! token streaming for free).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// One agent's transcript within a run.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AgentTranscript {
    /// Stable per-agent session id (e.g. `frontend-1`).
    pub session_id: String,
    /// Human role label (e.g. `Frontend engineer`).
    pub role: String,
    /// The GENERATED operational prompt the agent was handed (not user-typed).
    pub prompt: String,
    /// The agent's output so far, appended as the run progresses.
    pub output: String,
    /// `running` | `done` | `blocked`.
    pub status: String,
}

/// In-memory transcripts keyed by run id, shared into the executor and handlers.
#[derive(Clone, Default)]
pub struct TranscriptStore {
    inner: Arc<Mutex<HashMap<String, Vec<AgentTranscript>>>>,
}

impl TranscriptStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The transcripts for a run (empty if none / unknown run).
    pub fn get(&self, run_id: &str) -> Vec<AgentTranscript> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.get(run_id).cloned())
            .unwrap_or_default()
    }

    /// Drop all transcripts for a run id (so a fresh run/scan starts clean).
    pub fn clear(&self, run_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(run_id);
        }
    }

    /// Register an agent for a run (its generated prompt + initial status). Replaces an
    /// existing agent with the same session id.
    pub fn register(&self, run_id: &str, agent: AgentTranscript) {
        if let Ok(mut g) = self.inner.lock() {
            let list = g.entry(run_id.to_string()).or_default();
            if let Some(existing) = list.iter_mut().find(|a| a.session_id == agent.session_id) {
                *existing = agent;
            } else {
                list.push(agent);
            }
        }
    }

    /// Append a line to an agent's output (a newline is added if the buffer is non-empty).
    pub fn append_output(&self, run_id: &str, session_id: &str, line: &str) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(list) = g.get_mut(run_id) {
                if let Some(a) = list.iter_mut().find(|a| a.session_id == session_id) {
                    if !a.output.is_empty() {
                        a.output.push('\n');
                    }
                    a.output.push_str(line);
                }
            }
        }
    }

    /// Set an agent's status (`running` | `done` | `blocked`).
    pub fn set_status(&self, run_id: &str, session_id: &str, status: &str) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(list) = g.get_mut(run_id) {
                if let Some(a) = list.iter_mut().find(|a| a.session_id == session_id) {
                    a.status = status.to_string();
                }
            }
        }
    }
}

/// Build the GENERATED operational prompt an agent of `role` is handed for a story —
/// the same governance framing a real agent receives, made inspectable. Representative
/// of the live fleet's `stage_task_for` wrapping; kept here so the scripted path shows
/// a faithful prompt without spending tokens.
pub fn generated_prompt(role: &str, story_id: &str, task: &str) -> String {
    format!(
        "You are the {role} on story {story_id}, working inside Camerata's governed fleet.\n\n\
         Task:\n{task}\n\n\
         Governance (enforced, not advisory):\n\
         - Every file write passes the deny-before-execute gate; a denied write never reaches disk.\n\
         - No secrets in source, no writes outside the workspace, no raw SQL string-building.\n\
         - You cannot run git; Camerata is the sole committer.\n\n\
         Produce the change for your part of the story. The gate will rule on each write."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentTranscript {
        AgentTranscript {
            session_id: id.to_string(),
            role: "Frontend engineer".to_string(),
            prompt: "do the thing".to_string(),
            output: String::new(),
            status: "running".to_string(),
        }
    }

    #[test]
    fn register_get_append_and_status() {
        let store = TranscriptStore::new();
        assert!(store.get("run-1").is_empty());

        store.register("run-1", agent("frontend-1"));
        store.register("run-1", agent("backend-1"));
        assert_eq!(store.get("run-1").len(), 2);

        store.append_output("run-1", "frontend-1", "wrote file A");
        store.append_output("run-1", "frontend-1", "DENIED: secret literal");
        let fe = store
            .get("run-1")
            .into_iter()
            .find(|a| a.session_id == "frontend-1")
            .unwrap();
        assert_eq!(fe.output, "wrote file A\nDENIED: secret literal");

        store.set_status("run-1", "frontend-1", "blocked");
        let fe = store
            .get("run-1")
            .into_iter()
            .find(|a| a.session_id == "frontend-1")
            .unwrap();
        assert_eq!(fe.status, "blocked");
    }

    #[test]
    fn register_replaces_same_session() {
        let store = TranscriptStore::new();
        store.register("r", agent("a"));
        let mut updated = agent("a");
        updated.role = "Backend engineer".to_string();
        store.register("r", updated);
        let list = store.get("r");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].role, "Backend engineer");
    }

    #[test]
    fn generated_prompt_includes_role_story_and_governance() {
        let p = generated_prompt("Frontend engineer", "CAM-1", "Build the export button.");
        assert!(p.contains("Frontend engineer"));
        assert!(p.contains("CAM-1"));
        assert!(p.contains("Build the export button."));
        assert!(p.contains("deny-before-execute"));
    }
}
