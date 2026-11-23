//! The build a run was started on, saved with the run, and the rule that the build that starts an
//! inspection finishes it (V1 item 39; design in camerata-internal docs/plans/2026-11-23).
//!
//! Everything here is read from the RUNNING binary and the rule library it actually loaded: the
//! commit, dirty flag and version compiled in by `build.rs`, the two prompt schema numbers, and
//! the digest of the loaded rules. Nothing is read from the folder the binary runs in, so a kept
//! worktree today and a sealed executable with the rules built in later record exactly the same
//! identity.

use serde::{Deserialize, Serialize};

/// What is known about a build at compile time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildId {
    /// The crate version.
    pub version: String,
    /// The full git commit the binary was built from; `None` outside a git checkout.
    pub commit: Option<String>,
    /// Whether the tree had uncommitted changes when the build script last ran.
    pub dirty: bool,
    /// [`crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION`].
    pub prompt_schema: u32,
    /// The clone advisory's prompt schema number.
    pub clone_prompt_schema: u32,
}

impl BuildId {
    /// The running binary's own identity. The one place the build is read.
    #[cfg(not(test))]
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: crate::ENGINE_COMMIT.map(str::to_string),
            dirty: crate::ENGINE_DIRTY,
            prompt_schema: crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
            clone_prompt_schema: crate::onboard::clone_advisory::cache::PROMPT_SCHEMA_VERSION,
        }
    }

    /// In this crate's own tests the build is a fixed clean commit, so a test result does not
    /// depend on the state of the checkout; a test can replace it with [`test_build::set`].
    #[cfg(test)]
    pub fn current() -> Self {
        test_build::get().unwrap_or_else(Self::clean_test_build)
    }

    #[cfg(test)]
    pub fn clean_test_build() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: Some("a".repeat(40)),
            dirty: false,
            prompt_schema: crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
            clone_prompt_schema: crate::onboard::clone_advisory::cache::PROMPT_SCHEMA_VERSION,
        }
    }

    /// The first 12 characters of the commit, for messages.
    pub fn short_commit(&self) -> Option<String> {
        self.commit.as_deref().map(short)
    }
}

#[cfg(test)]
pub mod test_build {
    use super::BuildId;
    use std::sync::Mutex;
    static OVERRIDE: Mutex<Option<BuildId>> = Mutex::new(None);
    pub fn get() -> Option<BuildId> {
        OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Replace the build for the tests that hold `serial(engine_build)`; `None` restores it.
    pub fn set(build: Option<BuildId>) {
        *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = build;
    }
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}

/// The identity saved with a run: the build, the rule library and the clone judge model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnginePin {
    pub build: BuildId,
    /// [`camerata_rules::corpus_version`] of the rule library this run loaded; empty when none
    /// loaded.
    pub corpus_version: String,
    /// The clone judge's resolved model id, kept for the life of the run.
    pub clone_judge_model: String,
}

impl EnginePin {
    /// Computed once per start, from the running binary, the rules it loaded and the run controls.
    pub fn for_run(
        corpus: Option<&camerata_rules::RuleSet>,
        controls: &crate::cost_cap::RunControls,
    ) -> Self {
        Self {
            build: BuildId::current(),
            corpus_version: corpus
                .map(camerata_rules::corpus_version)
                .unwrap_or_default(),
            clone_judge_model: controls.clone_judge_model_for(),
        }
    }

    /// A resume names its clone judge model only when the pick is its own; the run keeps the model
    /// it started on. An explicit pick that differs is refused. Returns a note when the saved
    /// model is put back.
    pub fn hold_clone_judge_model(
        &self,
        controls: &mut crate::cost_cap::RunControls,
    ) -> Result<Option<String>, String> {
        let saved = self.clone_judge_model.trim();
        if saved.is_empty() {
            return Ok(None);
        }
        let explicit = controls
            .clone_judge_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty());
        match explicit {
            Some(pick) if pick != saved => Err(format!(
                "this inspection's clone check runs on {saved}, and this resume asks for {pick}. \
                 The stored clone verdicts came from {saved}, so resume it with the same model, or \
                 start the inspection again"
            )),
            Some(_) => Ok(None),
            None => {
                controls.clone_judge_model = Some(saved.to_string());
                Ok(Some(format!(
                    "The clone check stays on {saved}, the model this inspection started with."
                )))
            }
        }
    }
}

/// The refusal when a saved run cannot continue on this build, in plain words; `Ok` when it can.
/// `corpus_version` is the digest of the rule library now loaded, or `None` when it is not known
/// yet (the app's preview), in which case the rule library is checked when the resume starts.
pub fn check_pin(
    saved: Option<&EnginePin>,
    now: &BuildId,
    corpus_version: Option<&str>,
) -> Result<(), String> {
    let Some(saved) = saved else {
        return Err(
            "This inspection was started before Camerata recorded which build ran it, so \
                    it cannot be continued. Start it again."
                .to_string(),
        );
    };
    let saved_short = saved.build.short_commit();
    let Some(now_commit) = &now.commit else {
        return Err(format!(
            "This copy of Camerata does not know which build it is, so it cannot continue a saved \
             inspection. Open build {} to run what remains, or start the inspection again.",
            saved_short.as_deref().unwrap_or("(unknown)")
        ));
    };
    if saved.build.commit.as_ref() != Some(now_commit) {
        return Err(format!(
            "This inspection was started on build {} and cannot be continued on this build ({}). \
             The build that starts an inspection must finish it. Open build {} to run what remains, \
             or start the inspection again on this build.",
            saved_short.as_deref().unwrap_or("(unknown)"),
            short(now_commit),
            saved_short.as_deref().unwrap_or("(unknown)"),
        ));
    }
    if saved.build.prompt_schema != now.prompt_schema
        || saved.build.clone_prompt_schema != now.clone_prompt_schema
    {
        return Err(
            "The questions Camerata asks the model changed since this inspection started, \
                    so it cannot be continued. Start it again."
                .to_string(),
        );
    }
    if let Some(now_corpus) = corpus_version {
        if saved.corpus_version != now_corpus {
            return Err(format!(
                "The rule library changed since this inspection started, so it cannot be \
                 continued. Put the rule library back as it was on build {}, or start the \
                 inspection again.",
                saved_short.as_deref().unwrap_or("(unknown)")
            ));
        }
    }
    Ok(())
}

/// The note for a resume on the same commit when either side carries uncommitted changes.
pub fn dirty_note(saved: Option<&EnginePin>, now: &BuildId) -> Option<String> {
    let saved = saved?;
    (saved.build.commit == now.commit && (saved.build.dirty || now.dirty))
        .then(|| "Continued on a build with uncommitted changes.".to_string())
}

/// The refusal to START an inspection for a client on a build that cannot be named exactly: one
/// with uncommitted changes or no commit. Internal runs are not refused.
pub fn client_start_refusal(client_engagement: bool, build: &BuildId) -> Option<String> {
    if !client_engagement {
        return None;
    }
    if build.commit.is_none() {
        return Some(
            "This copy of Camerata does not know which build it is, so it cannot start an \
             inspection for a client. Build it from a git checkout, or run it as an internal \
             inspection."
                .to_string(),
        );
    }
    if build.dirty {
        return Some(
            "This copy of Camerata was built with changes that are not committed, so it cannot \
             start an inspection for a client. Commit the changes and build again, or run it as \
             an internal inspection."
                .to_string(),
        );
    }
    None
}
