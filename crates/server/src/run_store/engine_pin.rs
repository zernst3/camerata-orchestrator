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
    /// The folder the binary was built from (the workspace root `cargo` compiled it in), so a
    /// refusal can say where the build that started an inspection is kept. Operator state only:
    /// it is a local path, never part of the build's identity ([`check_pin`] compares commits)
    /// and never exported ([`ScanBuild`] leaves it out). `None` for a pin saved before it was
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_from: Option<String>,
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
            built_from: compiled_in_folder(),
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
            built_from: None,
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
    /// Replace the build for the tests that hold `serial(process_env)` (the one shared key); `None` restores it.
    pub fn set(build: Option<BuildId>) {
        *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = build;
    }
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}

/// The workspace root this crate was compiled in (`crates/server` is two folders below it).
#[cfg(not(test))]
fn compiled_in_folder() -> Option<String> {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(|root| root.to_string_lossy().into_owned())
}

/// The build an inspection was scanned with, as the package states it (README, `manifest.json`,
/// `json/inspection.json` and the PDF's last page). Saved on the scan's
/// [`crate::onboard::ScanProvenance::scanned_with`] from the run's [`EnginePin`] at scan time, so
/// an export made later, by a newer build, reads it and never recomputes it. Holds no local path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanBuild {
    /// The crate version of the scanning build.
    pub version: String,
    /// The full git commit of the scanning build; `None` outside a git checkout.
    pub commit: Option<String>,
    /// Whether the scanning build had uncommitted changes.
    pub dirty: bool,
    /// [`camerata_rules::corpus_version`] of the rule library the scan loaded; `None` when none
    /// loaded.
    pub corpus_version: Option<String>,
}

impl ScanBuild {
    /// The scanning build, from the pin the run was started on.
    pub fn from_pin(pin: &EnginePin) -> Self {
        Self {
            version: pin.build.version.clone(),
            commit: pin.build.commit.clone(),
            dirty: pin.build.dirty,
            corpus_version: Some(pin.corpus_version.clone()).filter(|c| !c.is_empty()),
        }
    }

    /// The build running now, the one that builds a package: its version, commit and dirty flag
    /// (the rule library is the export's own, see [`crate::ManifestProvenance::corpus_version`]).
    pub fn package_build() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: crate::ENGINE_COMMIT.map(str::to_string),
            dirty: crate::ENGINE_DIRTY,
            corpus_version: None,
        }
    }

    /// The first 12 characters of the commit, the build's reference in client text.
    pub fn short_commit(&self) -> Option<String> {
        self.commit.as_deref().map(short)
    }

    /// Whether `other` is the same build: version, commit and dirty flag.
    pub fn same_build(&self, other: &Self) -> bool {
        self.version == other.version && self.commit == other.commit && self.dirty == other.dirty
    }

    /// The README's words for this build: `Camerata 0.4.0, build 1a2b3c4d5e6f`, with
    /// `(built with uncommitted changes)` after a dirty one; the version alone when no commit
    /// was recorded.
    pub fn readme_label(&self) -> String {
        let mut out = format!("Camerata {}", self.version);
        if let Some(short) = self.short_commit() {
            out.push_str(&format!(", build {short}"));
        }
        if self.dirty {
            out.push_str(&format!(" {UNCOMMITTED_NOTE}"));
        }
        out
    }

    /// The one sentence the client PDF's last page carries about the build (owner ruling Q2).
    /// `None` when the scanning build recorded no commit: there is no reference to give.
    pub fn pdf_sentence(&self) -> Option<String> {
        let short = self.short_commit()?;
        Some(format!(
            "Inspected with Camerata {} (reference {short}). The full technical record is in the \
             manifest file included with this report.",
            self.version
        ))
    }

    /// The manifest's object for this build. `with_corpus` adds `corpus_version` (the scanning
    /// build's; the package build's rule library is the manifest's own `corpus_version`).
    pub fn manifest_json(&self, with_corpus: bool) -> serde_json::Value {
        let mut out = serde_json::json!({
            "version": self.version,
            "commit": self.commit,
            "dirty": self.dirty,
        });
        if with_corpus {
            out["corpus_version"] = serde_json::json!(self.corpus_version);
        }
        out
    }
}

/// The README's note after a build with uncommitted changes.
pub const UNCOMMITTED_NOTE: &str = "(built with uncommitted changes)";

/// Whether a saved run was started on another commit than the running build (both recorded).
/// The case the app answers with "Start again on this build".
pub fn is_other_build(saved: Option<&EnginePin>, now: &BuildId) -> bool {
    match (saved.and_then(|p| p.build.commit.as_ref()), &now.commit) {
        (Some(saved), Some(now)) => saved != now,
        _ => false,
    }
}

/// The refusal for a saved run started on another commit than the running build, naming the
/// folder that build was made in when the pin recorded it; `None` when it is the same commit (or
/// either commit is unknown, which [`check_pin`] refuses in its own words).
pub fn other_build_refusal(saved: Option<&EnginePin>, now: &BuildId) -> Option<String> {
    if !is_other_build(saved, now) {
        return None;
    }
    let saved = saved?;
    let saved_short = saved.build.short_commit().unwrap_or_default();
    let now_short = now.short_commit().unwrap_or_default();
    let open = match saved
        .build
        .built_from
        .as_deref()
        .filter(|f| !f.trim().is_empty())
    {
        Some(folder) => format!("Open build {saved_short} from the folder {folder}"),
        None => format!("Open build {saved_short}"),
    };
    Some(format!(
        "This inspection was started on build {saved_short} and cannot be continued on this build \
         ({now_short}). The build that starts an inspection must finish it. {open} to run what \
         remains, or start the inspection again on this build."
    ))
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

    /// A resume keeps the clone judge model the inspection started on, whatever the project or the
    /// command line names now (the stored clone verdicts came from it, and a mid-inspection
    /// settings change must not strand the run). Returns a note when the pick had to be put back.
    pub fn hold_clone_judge_model(
        &self,
        controls: &mut crate::cost_cap::RunControls,
    ) -> Option<String> {
        let saved = self.clone_judge_model.trim();
        if saved.is_empty() {
            return None;
        }
        let asked = controls
            .clone_judge_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty());
        if asked == Some(saved) {
            return None;
        }
        let was = asked.map(str::to_string);
        controls.clone_judge_model = Some(saved.to_string());
        Some(match was {
            Some(pick) => format!(
                "The clone check stays on {saved}, the model this inspection started with \
                 (not {pick})."
            ),
            None => {
                format!("The clone check stays on {saved}, the model this inspection started with.")
            }
        })
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
    if now.commit.is_none() {
        return Err(format!(
            "This copy of Camerata does not know which build it is, so it cannot continue a saved \
             inspection. Open build {} to run what remains, or start the inspection again.",
            saved_short.as_deref().unwrap_or("(unknown)")
        ));
    }
    if saved.build.commit.is_none() {
        return Err(
            "This inspection was started on a build that did not record its commit, so it \
             cannot be continued. Start it again."
                .to_string(),
        );
    }
    if let Some(refusal) = other_build_refusal(Some(saved), now) {
        return Err(refusal);
    }
    if !saved.build.dirty && now.dirty {
        return Err(format!(
            "This inspection was started on a clean build and this copy has changes that are \
             not committed, so it cannot be continued here. Open build {} to run what remains, \
             or start the inspection again.",
            saved_short.as_deref().unwrap_or("(unknown)")
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

/// The note for a resume on the same commit when the saved build carried uncommitted changes (a
/// clean saved build is never continued on a dirty one, see [`check_pin`]).
pub fn dirty_note(saved: Option<&EnginePin>, now: &BuildId) -> Option<String> {
    let saved = saved?;
    (saved.build.commit == now.commit && saved.build.dirty && now.dirty)
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
