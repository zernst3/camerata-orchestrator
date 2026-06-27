//! Shared project-grounding: the single source of the "what is this project" block
//! every in-project AI agent receives.
//!
//! THE INVARIANT (`docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md`):
//! every agent invoked on behalf of a project MUST be grounded in (a) the project's
//! RULE context (its committed ruleset / selected rules) and (b) on-demand READ ACCESS to
//! the ENTIRE project repo — every file — not merely the digest below. The digest (detected
//! stack, dependency highlights, high-signal docs, a shallow tree) is the cheap always-on
//! BASELINE; the authoritative window is the agent reading the actual files from its working
//! directory when it needs to. No exceptions, regardless of which feature invokes it.
//!
//! Motivation: an in-project agent used to behave like a context-less product owner —
//! reasoning about capabilities and structure the project did not have, because it could
//! only see a fixed summary. The fix is twofold: hand it the digest for orientation AND give
//! it on-demand read of the real codebase so it can confirm facts by reading, never by
//! assuming. The whole point of "use an agent" is that it understands the actual code.
//!
//! ISOLATION: the digest reads ONLY the repos passed in (the active project's repos),
//! resolved via [`crate::workspace::resolve_repo_dir`] (machine-local override path or
//! `<workspace_root>/<owner>/<repo>`). It never reads another project's clone. The file
//! reader ([`crate::onboard::read_local_repo_files`]) honours `.gitignore` + the noise
//! denylist, and this module additionally redacts obvious secret files.
//!
//! BUDGET: the whole block is bounded. The digest truncates docs, caps the file tree,
//! and reads at most a small set of manifests per repo. The result is a compact prose
//! block, not the repo's source.
//!
//! PREFIX STABILITY (OpenRouter / API driver cache behaviour):
//! The pure functions in this module (`render_rule_context`, `render_repo_digest`,
//! `assemble`) are deterministic given the same inputs, but they are NOT called here on
//! every LLM turn. The `ApiAgentDriver::run_loop` calls `build_system_prompt(role)` ONCE
//! before entering the turn loop; the resulting string is then reused verbatim for every
//! turn's `system` field. This means the static prefix (rules + repo digest) is identical
//! across all turns, which allows OpenRouter's per-block `cache_control` breakpoint in
//! `call_openrouter_with_tools` to take effect from the second turn onward. No
//! regeneration happens inside the loop, so there is no prefix-instability problem to fix.

use std::path::Path;

/// Hard ceiling on the rendered grounding block (chars). A pathological monorepo with
/// huge docs can't blow the agent's context window — past this the block is truncated
/// with a marker. Generous enough that a normal project's full digest fits.
const MAX_GROUNDING_CHARS: usize = 24_000;
/// Per-doc truncation budget (chars) for README / CLAUDE.md / AGENTS.md / CONVENTIONS.md.
const MAX_DOC_CHARS: usize = 4_000;
/// Max entries listed in the shallow file/dir tree, per repo.
const MAX_TREE_ENTRIES: usize = 60;
/// Per-manifest truncation budget (chars) for the dependency-highlights excerpt.
const MAX_MANIFEST_CHARS: usize = 2_500;

/// High-signal doc basenames to surface (case-insensitive `README*` match handled
/// separately). Order = priority.
const DOC_BASENAMES: &[&str] = &["CLAUDE.md", "AGENTS.md", "CONVENTIONS.md"];

/// Manifest basenames whose dependency highlights make framework facts visible
/// (e.g. "Dioxus + Axum + SQLx, no auth crate"). Order = priority.
const MANIFEST_BASENAMES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "requirements.txt",
    "Gemfile",
    "pom.xml",
    "build.gradle",
    "composer.json",
];

/// Basenames / suffixes that may carry secrets and must NEVER be injected into an LLM
/// prompt, even when committed + unignored. Belt-and-suspenders on top of gitignore.
fn is_secret_file(rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    base == ".env"
        || base.starts_with(".env.")
        || base.ends_with(".pem")
        || base.ends_with(".key")
        || base.ends_with(".p12")
        || base.ends_with(".pfx")
        || base == "id_rsa"
        || base == "credentials"
        || base.contains("secret")
}

/// Truncate `s` to `n` chars on a char boundary, appending a marker when cut.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n).collect();
    format!("{cut}\n…[truncated]")
}

/// The rule-context section, reusing the chat's renderers so agents and chat see the
/// SAME rule picture. `committed_summary` is the post-onboard committed ruleset summary
/// (from [`crate::build_ruleset_summary`]); `selected_section` is the pre-onboard
/// in-progress selections (from [`crate::render_selected_rules_for_chat`]). Either may
/// be empty. Returns `None` when there are no rules to show at all.
pub fn render_rule_context(
    committed_summary: Option<&str>,
    selected_section: Option<&str>,
) -> Option<String> {
    let committed = committed_summary.map(str::trim).filter(|s| !s.is_empty());
    let selected = selected_section.map(str::trim).filter(|s| !s.is_empty());
    if committed.is_none() && selected.is_none() {
        return None;
    }
    let mut s = String::from("=== PROJECT RULES (the governance the agent MUST respect) ===\n");
    if let Some(c) = committed {
        s.push_str("Committed ruleset:\n");
        s.push_str(c);
        s.push('\n');
    }
    if let Some(sel) = selected {
        if committed.is_some() {
            s.push('\n');
        }
        s.push_str(sel);
        s.push('\n');
    }
    Some(s)
}

/// Build the REPO-context digest for ONE local repo clone. `repo` is `owner/repo`;
/// `dir` is its resolved local path. Reads the working tree (gitignore + noise aware),
/// detects the stack, pulls dependency highlights from manifests, the high-signal docs,
/// and a shallow file tree. Pure of project state — caller resolves the path.
///
/// Returns `None` when the directory does not exist / is unreadable (an unresolved repo
/// is reported by the caller as a NOTE, not silently dropped).
pub fn render_repo_digest(repo: &str, dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let extracted = crate::onboard::read_local_repo_files(dir).ok()?;
    let files = &extracted.files;

    let mut s = format!("--- repo: {repo} ---\n");

    // Detected stack (languages + frameworks) — makes framework facts explicit
    // (e.g. "Dioxus + Axum + SQLx", and the ABSENCE of an auth crate).
    let stack = crate::onboard::detect_stack(repo, files);
    let langs = if stack.languages.is_empty() {
        "(none detected)".to_string()
    } else {
        stack.languages.join(", ")
    };
    let fws = if stack.frameworks.is_empty() {
        "(none detected)".to_string()
    } else {
        stack.frameworks.join(", ")
    };
    s.push_str(&format!("Languages: {langs}\n"));
    s.push_str(&format!("Frameworks/libraries: {fws}\n"));

    // Dependency highlights: the workspace + member manifests verbatim (truncated), so
    // the agent can read the ACTUAL dependency set ("axum", "sqlx", and no auth crate)
    // rather than inferring it. Cap to the first few manifests by priority.
    let mut manifests: Vec<&(String, String)> = files
        .iter()
        .filter(|(p, _)| {
            let base = p.rsplit('/').next().unwrap_or(p);
            MANIFEST_BASENAMES.contains(&base)
        })
        .collect();
    // Priority: known manifest order, then shallowest path (workspace root first).
    manifests.sort_by_key(|(p, _)| {
        let base = p.rsplit('/').next().unwrap_or(p);
        let prio = MANIFEST_BASENAMES
            .iter()
            .position(|b| *b == base)
            .unwrap_or(usize::MAX);
        (p.matches('/').count(), prio, p.clone())
    });
    for (p, content) in manifests.iter().take(4) {
        s.push_str(&format!("\nDependency manifest `{p}`:\n```\n"));
        s.push_str(&truncate(content, MAX_MANIFEST_CHARS));
        s.push_str("\n```\n");
    }

    // High-signal docs: README* + CLAUDE.md / AGENTS.md / CONVENTIONS.md at the repo root,
    // truncated. Read DIRECTLY from disk (not via `files`): the file reader only returns
    // code-extension files, so Markdown docs would otherwise be invisible. Secret files are
    // never doc basenames, so no redaction concern here.
    let mut seen_doc = false;
    let mut emit_doc = |s: &mut String, name: &str, content: &str| {
        if !seen_doc {
            s.push('\n');
            seen_doc = true;
        }
        s.push_str(&format!("Doc `{name}`:\n"));
        s.push_str(&truncate(content.trim(), MAX_DOC_CHARS));
        s.push_str("\n\n");
    };
    // README* (case-insensitive, any extension) at the root, first match wins.
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut readmes: Vec<(String, std::path::PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.to_ascii_lowercase().starts_with("readme") && e.path().is_file() {
                    Some((name, e.path()))
                } else {
                    None
                }
            })
            .collect();
        readmes.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some((name, path)) = readmes.first() {
            if let Ok(content) = std::fs::read_to_string(path) {
                emit_doc(&mut s, name, &content);
            }
        }
    }
    for doc in DOC_BASENAMES {
        let path = dir.join(doc);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                emit_doc(&mut s, doc, &content);
            }
        }
    }

    // Shallow file tree: a capped sample of paths so the agent sees the repo's shape
    // (crates/, src/, etc.) without the full listing. Secret files redacted.
    s.push_str("File tree (sample):\n");
    let mut shown = 0usize;
    for (p, _) in files.iter() {
        if is_secret_file(p) {
            continue;
        }
        if shown >= MAX_TREE_ENTRIES {
            s.push_str(&format!(
                "… (+{} more files)\n",
                files.len().saturating_sub(shown)
            ));
            break;
        }
        s.push_str(&format!("  {p}\n"));
        shown += 1;
    }
    if extracted.truncated {
        s.push_str("  …(repo file scan truncated at the hard cap)\n");
    }

    Some(s)
}

/// Assemble the full grounding block from a pre-rendered rule section and the per-repo
/// digests (already resolved + rendered by the caller, which owns project state and the
/// path resolution). `unresolved` carries `(repo, reason)` for repos with no local clone
/// so the agent is TOLD a repo couldn't be read rather than silently seeing nothing.
///
/// Returns `None` only when there is nothing at all to say (no rules and no repos).
pub fn assemble(
    rule_section: Option<String>,
    repo_digests: &[String],
    unresolved: &[(String, String)],
) -> Option<String> {
    if rule_section.is_none() && repo_digests.is_empty() && unresolved.is_empty() {
        return None;
    }
    let mut s = String::from(
        "=== PROJECT GROUNDING ===\n\
         The following is a DIGEST of the actual project you are working on — its real \
         stack, dependencies, structure, and rules. This digest is a cheap orientation, \
         NOT the whole truth: you also have READ ACCESS to the full project repo from your \
         working directory. Before assuming anything about what the project does or how it \
         is built, CONSULT THE ACTUAL CODE AND CONFIG by reading the relevant files. Ground \
         every answer in what the repo actually contains, never in assumed capabilities or \
         structure.\n\n",
    );
    if let Some(rules) = rule_section {
        s.push_str(&rules);
        s.push('\n');
    }
    if !repo_digests.is_empty() {
        s.push_str("=== REPOSITORY CONTEXT (local clone digest) ===\n");
        for d in repo_digests {
            s.push_str(d);
            s.push('\n');
        }
    }
    if !unresolved.is_empty() {
        s.push_str("=== REPOS WITHOUT A LOCAL CLONE (could not be read) ===\n");
        for (repo, reason) in unresolved {
            s.push_str(&format!("- {repo}: {reason}\n"));
        }
        s.push('\n');
    }
    s.push_str("=== END PROJECT GROUNDING ===\n");
    Some(truncate(&s, MAX_GROUNDING_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_files_are_flagged() {
        assert!(is_secret_file(".env"));
        assert!(is_secret_file(".env.production"));
        assert!(is_secret_file("config/server.pem"));
        assert!(is_secret_file("deploy/id_rsa"));
        assert!(is_secret_file("app/my_secret_config.toml"));
        assert!(!is_secret_file("src/main.rs"));
        assert!(!is_secret_file("Cargo.toml"));
    }

    #[test]
    fn rule_context_none_when_empty() {
        assert!(render_rule_context(None, None).is_none());
        assert!(render_rule_context(Some("  "), Some("")).is_none());
    }

    #[test]
    fn rule_context_includes_both_sections() {
        let out = render_rule_context(Some("RULE-1: repo-local"), Some("Selected: RULE-2"))
            .expect("some");
        assert!(out.contains("PROJECT RULES"));
        assert!(out.contains("RULE-1"));
        assert!(out.contains("RULE-2"));
    }

    #[test]
    fn assemble_none_when_nothing() {
        assert!(assemble(None, &[], &[]).is_none());
    }

    #[test]
    fn assemble_directs_the_agent_to_read_the_real_code() {
        let out = assemble(
            Some("=== PROJECT RULES ===\nRULE-1\n".to_string()),
            &["--- repo: o/r ---\nLanguages: Rust\n".to_string()],
            &[],
        )
        .expect("some");
        assert!(out.contains("PROJECT GROUNDING"));
        // Neutral framing: the agent has full repo read and must consult the actual code,
        // rather than the old symptom-specific "do not ask about authentication" guidance.
        assert!(out.contains("READ ACCESS to the full project repo"));
        assert!(out.contains("CONSULT THE ACTUAL CODE"));
        assert!(
            !out.to_lowercase().contains("authentication"),
            "the auth-specific anti-hallucination guidance must be gone"
        );
        assert!(out.contains("RULE-1"));
        assert!(out.contains("Languages: Rust"));
        assert!(out.contains("END PROJECT GROUNDING"));
    }

    #[test]
    fn assemble_reports_unresolved_repos() {
        let out = assemble(None, &[], &[("o/r".to_string(), "no local path set".to_string())])
            .expect("some");
        assert!(out.contains("WITHOUT A LOCAL CLONE"));
        assert!(out.contains("o/r: no local path set"));
    }

    #[test]
    fn digest_reads_a_real_dir_and_detects_stack() {
        let dir = std::env::temp_dir().join(format!(
            "cam-grounding-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname=\"demo\"\n[dependencies]\naxum=\"0.7\"\nsqlx=\"0.7\"\ndioxus=\"0.5\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("README.md"), "# Demo\nA Dioxus + Axum + SQLx app.\n").unwrap();
        // A secret file must NOT appear in the tree.
        std::fs::write(dir.join(".env"), "SECRET=abc123\n").unwrap();

        let out = render_repo_digest("o/demo", &dir).expect("digest");
        assert!(out.contains("repo: o/demo"));
        assert!(out.contains("Rust"));
        assert!(out.contains("Cargo.toml"));
        assert!(out.contains("axum"));
        assert!(out.contains("sqlx"));
        assert!(out.contains("A Dioxus + Axum + SQLx app."));
        assert!(out.contains("src/main.rs"));
        assert!(!out.contains(".env"), "secret file leaked into tree: {out}");
        assert!(!out.contains("SECRET=abc123"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn digest_none_for_missing_dir() {
        let dir = std::env::temp_dir().join("cam-grounding-does-not-exist-xyz-123");
        assert!(render_repo_digest("o/r", &dir).is_none());
    }
}
