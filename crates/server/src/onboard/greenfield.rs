//! Greenfield scaffold: create a new local git repo with governance baked in
//! from commit zero.

/// The outcome of a greenfield scaffold operation: the local directory created,
/// the governance files written into it, and the git commit sha of the initial
/// commit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GreenfieldResult {
    /// Absolute path to the newly-created repo directory on disk.
    pub path: String,
    /// Governance files written (path -> content), in the order they were written.
    pub files_written: Vec<String>,
    /// The sha of the initial commit (shortened), or empty on commit failure.
    pub commit_sha: String,
    /// Human-readable summary for the UI.
    pub message: String,
}

/// Scaffold a NEW local git repo with governance baked in from commit zero.
///
/// Given a target directory (`dest`) that MUST NOT already exist, a list of arm
/// rules (already resolved by the caller — same shape `arm.rs` emits), and the
/// project's custom rules, this function:
///
/// 1. Creates `dest` and `git init`s it.
/// 2. Calls [`crate::arm::arm_files_for_repo`] to emit AGENTS.md, CONVENTIONS.md,
///    `.camerata/rules.json`, and (when mechanical rules are present) the CI
///    governance workflow — reusing the EXACT same emit path as the brownfield apply
///    flow so there is no duplicate logic.
/// 3. Writes every emitted file into the new working tree, creating parent dirs.
/// 4. Stages all files (`git add -A`) and makes the initial commit.
/// 5. Returns a [`GreenfieldResult`] describing what was created.
///
/// The function is intentionally synchronous-via-blocking (call via
/// `tokio::task::spawn_blocking`) so the git operations don't block the async runtime.
pub fn scaffold_greenfield_blocking(
    dest: &std::path::Path,
    rules: &[&crate::arm::ArmRule],
    custom: &[&crate::project::CustomRule],
    repo_label: &str,
) -> anyhow::Result<GreenfieldResult> {
    // Safety: refuse to clobber an existing directory.
    if dest.exists() {
        anyhow::bail!(
            "{} already exists — greenfield scaffold requires a new (non-existent) directory",
            dest.display()
        );
    }

    // 1. Create the root directory (and any parents the caller chose to nest under).
    std::fs::create_dir_all(dest).map_err(|e| {
        anyhow::anyhow!("could not create {}: {e}", dest.display())
    })?;

    // 2. `git init` the new directory.
    let git_init = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("git init failed: {e}"))?;
    if !git_init.status.success() {
        // Older git versions don't support `-b main`; fall back to plain `init`.
        let git_init2 = std::process::Command::new("git")
            .arg("init")
            .current_dir(dest)
            .output()
            .map_err(|e| anyhow::anyhow!("git init failed: {e}"))?;
        if !git_init2.status.success() {
            let err = String::from_utf8_lossy(&git_init2.stderr);
            anyhow::bail!("git init: {err}");
        }
    }

    // 3. Emit governance files using the SAME arm_files_for_repo primitive as the
    //    brownfield apply path — zero code duplication, guaranteed identical output.
    let emitted = crate::arm::arm_files_for_repo(rules, custom);

    // 4. Write every emitted file into the working tree.
    let mut files_written = Vec::with_capacity(emitted.len());
    for (rel, content) in &emitted {
        let full = dest.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("create dir {}: {e}", parent.display()))?;
        }
        std::fs::write(&full, content)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", full.display()))?;
        files_written.push(rel.clone());
    }

    // 5. Stage all files.
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("git add: {e}"))?;
    if !add.status.success() {
        let err = String::from_utf8_lossy(&add.stderr);
        anyhow::bail!("git add: {err}");
    }

    // 6. Initial commit. We need a user identity; use a fallback when the environment
    //    has no global git config (common in CI/test environments).
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "camerata@example.com"])
        .current_dir(dest)
        .output();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "Camerata"])
        .current_dir(dest)
        .output();

    let commit_msg = format!(
        "chore(governance): greenfield scaffold for {repo_label}\n\n\
         Governance baked in from commit zero via Camerata.\n\
         Rules: AGENTS.md, CONVENTIONS.md, .camerata/rules.json"
    );
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", &commit_msg])
        .current_dir(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("git commit: {e}"))?;
    if !commit.status.success() {
        let err = String::from_utf8_lossy(&commit.stderr);
        anyhow::bail!("git commit: {err}");
    }

    // 7. Read the short sha of the initial commit.
    let commit_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dest)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let n_rules = rules.len();
    let n_custom = custom.len();
    let message = format!(
        "Scaffolded {repo_label} at {} with {n_rules} base rule(s) and {n_custom} custom rule(s). \
         {n_files} governance file(s) committed as the initial commit ({commit_sha}).",
        dest.display(),
        n_files = files_written.len(),
    );

    Ok(GreenfieldResult {
        path: dest.to_string_lossy().into_owned(),
        files_written,
        commit_sha,
        message,
    })
}
