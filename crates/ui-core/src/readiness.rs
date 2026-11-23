//! Project readiness gate: pure logic for mapping the BFF's `/readiness` response into
//! view state. No rendering-framework dependency (RUST-HEADLESS-CORE-1); unit-tested here.
//!
//! A project is modeled as three facts (see `docs/decisions/2026-07-01_project-readiness-gate.md`):
//! identity (portable), local materialization (machine-local), and **readiness** — derived from
//! whether each repo resolves to a local clone. Repo-dependent actions gate on readiness so an
//! `Unlinked` / `Partial` project loads PAUSED behind a single "link repo" affordance rather than
//! each action failing independently (the dead-end-affordance principle).

/// The derived operational state of a project, mirroring the server's `readiness` string.
/// Anything the client doesn't recognise collapses to `Unlinked` (fail-safe: gate rather than
/// let actions fire into a guaranteed failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectReadiness {
    /// Every repo resolves to a local clone. Actions are live.
    Ready,
    /// No repo resolves. The project is paused; the resolve modal shows on entry.
    Unlinked,
    /// Some repos resolve, others don't. Paused until the rest are linked.
    Partial,
}

impl ProjectReadiness {
    /// Parse the server's `readiness` string. Unknown values fail safe to `Unlinked` so the
    /// gate errs toward pausing rather than releasing actions on an unrecognised state.
    pub fn parse(s: &str) -> Self {
        match s {
            "ready" => ProjectReadiness::Ready,
            "partial" => ProjectReadiness::Partial,
            // "unlinked" and anything unrecognised.
            _ => ProjectReadiness::Unlinked,
        }
    }

    /// Whether the project is operationally ready (all repos resolved).
    pub fn is_ready(self) -> bool {
        matches!(self, ProjectReadiness::Ready)
    }

    /// Whether the project is paused — i.e. NOT ready, so repo-dependent actions must be gated
    /// and the resolve modal / paused banner shown.
    pub fn is_paused(self) -> bool {
        !self.is_ready()
    }
}

/// One repo's local-resolution status, mirroring an entry of the server's `repos` array. Only the
/// fields the gate needs are modeled; extra fields on the wire are ignored by the adapter's parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoResolution {
    /// The repo identity, `owner/repo`.
    pub repo: String,
    /// Whether this repo resolves to a valid local clone.
    pub resolved: bool,
    /// The resolved (or expected) local path, when known. Empty when unresolved.
    pub path: String,
    /// A human-readable reason for the current state (e.g. "no local match", "origin mismatch").
    pub reason: String,
    /// The source was deleted from this machine on purpose (client source deletion) and has not
    /// been fetched back. Such a repo has nothing to link, so it is not "unresolved".
    pub source_deleted: bool,
    /// RFC3339 time of the deletion, when `source_deleted`.
    pub deleted_at: Option<String>,
}

/// The repos that still need resolving (not resolved AND not deliberately deleted) — the ones the
/// modal offers to clone-or-link. A repo whose source was deleted is excluded: the project keeps
/// working without it, and the way back is "Re-fetch source", not "Link repo". Order is
/// preserved from the input.
pub fn unresolved_repos(repos: &[RepoResolution]) -> Vec<&RepoResolution> {
    repos
        .iter()
        .filter(|r| !r.resolved && !r.source_deleted)
        .collect()
}

/// The scan-and-write operations that cannot run while a repo's source is deleted, and why.
/// `None` when every repo's source is available (or never was deleted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanBlock {
    /// `(repo, deleted_at)` for each repo whose source is deleted, in input order.
    pub repos: Vec<(String, String)>,
}

/// Which repos block scanning, if any. Pure.
pub fn scan_block(repos: &[RepoResolution]) -> Option<ScanBlock> {
    let deleted: Vec<(String, String)> = repos
        .iter()
        .filter(|r| r.source_deleted)
        .map(|r| (r.repo.clone(), r.deleted_at.clone().unwrap_or_default()))
        .collect();
    (!deleted.is_empty()).then_some(ScanBlock { repos: deleted })
}

/// The part of `block` that touches `scope` (the repos an action would act on), compared the way
/// GitHub names compare (case-insensitively). `None` when the action does not involve a repo
/// whose source is deleted, so scanning an unaffected repo stays possible in a mixed project.
pub fn scan_block_for(block: &ScanBlock, scope: &[String]) -> Option<ScanBlock> {
    let repos: Vec<(String, String)> = block
        .repos
        .iter()
        .filter(|(repo, _)| scope.iter().any(|s| s.trim().eq_ignore_ascii_case(repo)))
        .cloned()
        .collect();
    (!repos.is_empty()).then_some(ScanBlock { repos })
}

/// The explanation shown beside every control a deleted source disables, for example
/// "Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to
/// rescan." Several repos are listed in one sentence.
pub fn scan_block_text(block: &ScanBlock) -> String {
    let named: Vec<String> = block
        .repos
        .iter()
        .map(|(repo, at)| {
            if at.is_empty() {
                repo.clone()
            } else {
                format!("{repo} ({})", crate::source::format_date(at))
            }
        })
        .collect();
    match block.repos.as_slice() {
        [(repo, at)] if !at.is_empty() => format!(
            "Source for {repo} was deleted on {}. Re-fetch source from the Workspace tab to rescan.",
            crate::source::format_date(at)
        ),
        [(repo, _)] => format!(
            "Source for {repo} was deleted. Re-fetch source from the Workspace tab to rescan."
        ),
        _ => format!(
            "Source was deleted for {}. Re-fetch source from the Workspace tab to rescan.",
            named.join(", ")
        ),
    }
}

/// The banner headline for a paused project, given its readiness. `Ready` has no banner (returns
/// `None`); the caller renders nothing in that case.
pub fn paused_banner_text(readiness: ProjectReadiness, unresolved_count: usize) -> Option<String> {
    match readiness {
        ProjectReadiness::Ready => None,
        ProjectReadiness::Unlinked => {
            Some("Paused — link this project's repo before you can run it.".to_string())
        }
        ProjectReadiness::Partial => {
            let plural = if unresolved_count == 1 {
                "repo"
            } else {
                "repos"
            };
            Some(format!(
                "Paused — {unresolved_count} {plural} still need a local clone before this project can run."
            ))
        }
    }
}

/// The modal's per-repo prompt line (the ADR wording), for one unresolved repo.
pub fn resolve_prompt_text(repo: &str) -> String {
    format!(
        "This project is coupled with `{repo}`, but no local match exists. Clone it now, or select a local clone you already have?"
    )
}

/// Whether a repo-dependent primary action (start run / scan / apply / design-publish) should be
/// GATED (disabled) given the project's readiness. Actions are gated whenever the project is not
/// fully ready, so no action fires into a guaranteed 404 / no-op.
pub fn action_gated(readiness: ProjectReadiness) -> bool {
    readiness.is_paused()
}

/// Whether the resolve modal should auto-open on project entry: only when the project is paused.
pub fn should_open_resolve_modal(readiness: ProjectReadiness) -> bool {
    readiness.is_paused()
}

/// A file the inspection could not read because it is over the 1 MB file size limit and Excluded
/// by agreement does not name it, as the pre-start check lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverLimitFile {
    /// The repo (`owner/repo`).
    pub repo: String,
    /// The path within the repo.
    pub path: String,
    /// The path as a pattern writes it (`repo/path`, or `owner/repo/path` when two repos share a
    /// name), as the server states it. Empty when it did not.
    pub display: String,
    /// Why it cannot be read when that is not the file size limit ("has a line longer than
    /// 100 KB"), as the server states it. Empty for the size limit.
    pub reason: String,
    pub bytes: u64,
}

/// What the pre-start check shows besides a deleted source: files over the 1 MB file size limit
/// (a warning, never a hold), and patterns of Excluded by agreement that match no file (a hold: a
/// pattern that names nothing is fixed or removed, so an intended exclusion is never silently
/// void). A second source for the same Start button as [`ScanBlock`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverLimitBlock {
    /// The limit as a person reads it ("1 MB"), as the server states it.
    pub limit: String,
    pub files: Vec<OverLimitFile>,
    /// Patterns that match no file, as written.
    pub unmatched: Vec<String>,
    /// One entry per unmatched pattern: why it never can, or empty.
    pub unmatched_notes: Vec<String>,
    /// Patterns naming a repo that was not read, each as `pattern (cannot be checked: ...)`.
    /// They block Start and are never offered for removal.
    pub unchecked: Vec<String>,
    /// Patterns that match only files Camerata never reads anyway, with the reason: a warning
    /// that never holds a start, with Remove pattern still offered.
    pub warned: Vec<(String, String)>,
    /// The largest over-limit file the secret checks still search, as a person reads it ("20 MB"),
    /// as the server states it. Empty when the server did not say.
    pub secret_limit: String,
    /// Files that declare themselves generated, over every repo, and the non-blank lines in them:
    /// not read, not billed, searched for secrets only. A statement, never a hold.
    pub generated_files: usize,
    pub generated_lines: usize,
}

impl OverLimitBlock {
    /// This block with the generated files and lines the server counted.
    pub fn with_generated(mut self, files: usize, lines: usize) -> Self {
        self.generated_files = files;
        self.generated_lines = lines;
        self
    }

    /// This block with the secret search ceiling the server stated.
    pub fn with_secret_limit(mut self, limit: &str) -> Self {
        self.secret_limit = limit.trim().to_string();
        self
    }

    /// Whether the block holds Start: patterns matching nothing and patterns that cannot be
    /// checked do. Files over the limit and never-read patterns are warnings and do not.
    pub fn blocks(&self) -> bool {
        !self.unmatched.is_empty() || !self.unchecked.is_empty()
    }
}

/// The block for `files` and `unmatched` patterns, or `None` when neither is left. Pure.
pub fn over_limit_block(
    limit: &str,
    files: Vec<OverLimitFile>,
    unmatched: Vec<String>,
    unmatched_notes: Vec<String>,
    unchecked: Vec<String>,
    warned: Vec<(String, String)>,
) -> Option<OverLimitBlock> {
    (!files.is_empty() || !unmatched.is_empty() || !unchecked.is_empty() || !warned.is_empty())
        .then(|| OverLimitBlock {
            limit: limit.trim().to_string(),
            files,
            unmatched,
            unmatched_notes,
            unchecked,
            warned,
            secret_limit: String::new(),
            generated_files: 0,
            generated_lines: 0,
        })
}

/// The sentence for the generated files of the block: how many and how many lines are not read or
/// billed, in the one phrase every surface uses ([`GENERATED_FILES_LABEL`]). With the security
/// floor off (AI review only) nothing searches them, and the sentence says so. Empty when there
/// are none.
pub fn generated_text(block: &OverLimitBlock, floor_on: bool) -> String {
    let n = block.generated_files;
    if n == 0 {
        return String::new();
    }
    let what = format!(
        "{n} {} ({} {}) will not be read for the inspection",
        if n == 1 { "file" } else { "files" },
        group_thousands(block.generated_lines),
        if block.generated_lines == 1 {
            "line"
        } else {
            "lines"
        },
    );
    if floor_on {
        format!(
            "{what}, {}.",
            camerata_api_types::audit::GENERATED_FILES_LABEL
        )
    } else {
        format!("{what} or billed, and are not searched for secrets in this run: the security floor is off.")
    }
}

/// `n` with a comma between thousands (1,234).
fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A size as the pre-start check prints it, in the base the limit is written in (1 MB is
/// 1,048,576 bytes): whole kilobytes under a megabyte, then megabytes to one decimal rounded UP,
/// so a file over the limit never prints as 1.0 MB.
pub fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{} KB", (bytes + KB / 2) / KB)
    } else {
        let tenths = (bytes * 10).div_ceil(MB);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    }
}

/// The sentence above the file list: what will happen to the files, never a hold. Empty when no
/// file is over.
pub fn over_limit_text(block: &OverLimitBlock, floor_on: bool) -> String {
    let n = block.files.len();
    if n == 0 {
        return String::new();
    }
    let (file, it) = if n == 1 {
        ("file", "It is")
    } else {
        ("files", "Each one is")
    };
    let limit = if block.limit.is_empty() {
        "the file size limit".to_string()
    } else {
        format!("the {} file size limit", block.limit)
    };
    let cause = match block
        .files
        .iter()
        .map(|f| f.reason.as_str())
        .find(|r| !r.is_empty())
    {
        Some(reason) => format!("over {limit} or {reason}"),
        None => format!("over {limit}"),
    };
    let fate = if floor_on {
        let ceiling = if block.secret_limit.is_empty() {
            "that is too large".to_string()
        } else {
            format!("over {}", block.secret_limit)
        };
        format!(
            "{it} still searched for secrets by pattern checks, never by the model; one {ceiling} or not text is checked by name only."
        )
    } else {
        format!(
            "{} not searched for secrets in this run: the security floor is off.",
            if n == 1 { "It is" } else { "Each one is" }
        )
    };
    format!("{n} {file} will not be read for the inspection ({cause}). {fate} A start is not held.")
}

/// The sentence above the unmatched patterns. Empty when there are none.
pub fn unmatched_text(block: &OverLimitBlock) -> String {
    match block.unmatched.len() {
        0 => String::new(),
        1 => "Excluded by agreement holds a pattern that matches no file. Remove it, or fix it in the inspection settings, before you start.".to_string(),
        _ => "Excluded by agreement holds patterns that match no file. Remove them, or fix them in the inspection settings, before you start.".to_string(),
    }
}

/// The sentence above the warned patterns. Empty when there are none.
pub fn warned_text(block: &OverLimitBlock) -> String {
    match block.warned.len() {
        0 => String::new(),
        1 => "Excluded by agreement holds a pattern that matches only files Camerata never reads, so it changes nothing. A start is not held; you can remove it.".to_string(),
        _ => "Excluded by agreement holds patterns that match only files Camerata never reads, so they change nothing. A start is not held; you can remove them.".to_string(),
    }
}

/// The sentence above the patterns that cannot be checked. Empty when there are none.
pub fn unchecked_text(block: &OverLimitBlock) -> String {
    match block.unchecked.len() {
        0 => String::new(),
        1 => "Excluded by agreement holds a pattern that cannot be checked. A start is refused until the repo it names can be read; it is not removed for you.".to_string(),
        _ => "Excluded by agreement holds patterns that cannot be checked. A start is refused until the repos they name can be read; they are not removed for you.".to_string(),
    }
}

/// One listed file: its pattern form and size, `repo/path (size)`.
pub fn over_limit_file_line(file: &OverLimitFile) -> String {
    let name = if file.display.is_empty() {
        format!("{}/{}", file.repo, file.path)
    } else {
        file.display.clone()
    };
    if file.reason.is_empty() {
        format!("{name} ({})", format_file_size(file.bytes))
    } else {
        format!("{name} ({}, {})", format_file_size(file.bytes), file.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(repo: &str, resolved: bool) -> RepoResolution {
        RepoResolution {
            repo: repo.to_string(),
            resolved,
            path: if resolved {
                format!("/ws/{repo}")
            } else {
                String::new()
            },
            reason: if resolved {
                "ok".to_string()
            } else {
                "no local match".to_string()
            },
            source_deleted: false,
            deleted_at: None,
        }
    }

    fn deleted(repo: &str, at: &str) -> RepoResolution {
        RepoResolution {
            source_deleted: true,
            deleted_at: Some(at.to_string()),
            reason: "source deleted".to_string(),
            ..res(repo, false)
        }
    }

    #[test]
    fn unresolved_repos_excludes_deleted() {
        let repos = vec![
            res("a/one", false),
            deleted("a/two", "2026-10-24T17:40:19Z"),
            res("a/three", true),
        ];
        let un = unresolved_repos(&repos);
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].repo, "a/one");
        // A project whose only unresolved repo was deleted has nothing for the modal to offer.
        assert!(unresolved_repos(&[deleted("a/two", "2026-10-24T17:40:19Z")]).is_empty());
    }

    #[test]
    fn scan_block_lists_deleted_repos_with_dates() {
        let repos = vec![
            res("a/one", true),
            deleted("a/two", "2026-10-24T17:40:19Z"),
            deleted("a/four", "2026-11-03T09:00:00Z"),
        ];
        let block = scan_block(&repos).expect("blocked");
        assert_eq!(
            block.repos,
            vec![
                ("a/two".to_string(), "2026-10-24T17:40:19Z".to_string()),
                ("a/four".to_string(), "2026-11-03T09:00:00Z".to_string()),
            ]
        );
        assert_eq!(
            scan_block_text(&ScanBlock {
                repos: vec![("acme/app".to_string(), "2026-10-24T17:40:19Z".to_string())]
            }),
            "Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to rescan."
        );
        assert_eq!(
            scan_block_text(&block),
            "Source was deleted for a/two (24 Oct 2026), a/four (3 Nov 2026). Re-fetch source from the Workspace tab to rescan."
        );
    }

    #[test]
    fn scan_block_for_keeps_only_the_repos_an_action_touches() {
        let block = scan_block(&[
            deleted("a/two", "2026-10-24T17:40:19Z"),
            deleted("a/four", "2026-11-03T09:00:00Z"),
        ])
        .expect("blocked");
        let scope = vec!["A/Two".to_string(), " a/one ".to_string()];
        let touched = scan_block_for(&block, &scope).expect("a/two is in scope");
        assert_eq!(touched.repos.len(), 1);
        assert_eq!(touched.repos[0].0, "a/two");
        assert_eq!(scan_block_for(&block, &["a/one".to_string()]), None);
        assert_eq!(scan_block_for(&block, &[]), None);
    }

    #[test]
    fn scan_block_none_when_all_present() {
        assert_eq!(scan_block(&[res("a/one", true), res("a/two", false)]), None);
        assert_eq!(scan_block(&[]), None);
        // A missing date still reads as a sentence.
        let undated = ScanBlock {
            repos: vec![("a/b".to_string(), String::new())],
        };
        assert_eq!(
            scan_block_text(&undated),
            "Source for a/b was deleted. Re-fetch source from the Workspace tab to rescan."
        );
    }

    #[test]
    fn parse_maps_known_strings() {
        assert_eq!(ProjectReadiness::parse("ready"), ProjectReadiness::Ready);
        assert_eq!(
            ProjectReadiness::parse("partial"),
            ProjectReadiness::Partial
        );
        assert_eq!(
            ProjectReadiness::parse("unlinked"),
            ProjectReadiness::Unlinked
        );
    }

    #[test]
    fn parse_unknown_fails_safe_to_unlinked() {
        assert_eq!(ProjectReadiness::parse("wat"), ProjectReadiness::Unlinked);
        assert_eq!(ProjectReadiness::parse(""), ProjectReadiness::Unlinked);
    }

    #[test]
    fn is_ready_and_is_paused_are_complementary() {
        assert!(ProjectReadiness::Ready.is_ready());
        assert!(!ProjectReadiness::Ready.is_paused());
        assert!(ProjectReadiness::Unlinked.is_paused());
        assert!(!ProjectReadiness::Unlinked.is_ready());
        assert!(ProjectReadiness::Partial.is_paused());
    }

    #[test]
    fn unresolved_repos_filters_and_preserves_order() {
        let repos = vec![
            res("a/one", false),
            res("a/two", true),
            res("a/three", false),
        ];
        let un = unresolved_repos(&repos);
        assert_eq!(un.len(), 2);
        assert_eq!(un[0].repo, "a/one");
        assert_eq!(un[1].repo, "a/three");
    }

    #[test]
    fn ready_has_no_banner() {
        assert_eq!(paused_banner_text(ProjectReadiness::Ready, 0), None);
    }

    #[test]
    fn unlinked_banner_text() {
        let t = paused_banner_text(ProjectReadiness::Unlinked, 1).expect("banner");
        assert!(t.contains("Paused"));
        assert!(t.to_lowercase().contains("link"));
    }

    #[test]
    fn partial_banner_pluralizes() {
        let one = paused_banner_text(ProjectReadiness::Partial, 1).expect("banner");
        assert!(one.contains("1 repo "), "singular: {one}");
        let many = paused_banner_text(ProjectReadiness::Partial, 3).expect("banner");
        assert!(many.contains("3 repos "), "plural: {many}");
    }

    #[test]
    fn resolve_prompt_names_the_repo_and_offers_both_paths() {
        let t = resolve_prompt_text("owner/repo");
        assert!(t.contains("owner/repo"));
        assert!(t.contains("Clone it now"));
        assert!(t.contains("select a local clone"));
    }

    #[test]
    fn action_gated_when_paused_only() {
        assert!(!action_gated(ProjectReadiness::Ready));
        assert!(action_gated(ProjectReadiness::Unlinked));
        assert!(action_gated(ProjectReadiness::Partial));
    }

    #[test]
    fn modal_opens_when_paused_only() {
        assert!(!should_open_resolve_modal(ProjectReadiness::Ready));
        assert!(should_open_resolve_modal(ProjectReadiness::Unlinked));
        assert!(should_open_resolve_modal(ProjectReadiness::Partial));
    }

    // ── Bug regression: ResolveModal early-return guard (Bug 2) ─────────────────────────────────
    //
    // `ResolveModal` early-returns `rsx! {}` when `unresolved().is_empty()` so a Ready project
    // never shows the empty modal even if `modal_open` is still true after the last resolve.
    // This test confirms the load-bearing condition: a fully-resolved repo list yields an empty
    // unresolved set, which triggers the early-return.
    #[test]
    fn ready_project_has_empty_unresolved_repos() {
        // All repos resolved — this is the post-link state that triggers the empty-modal bug.
        let repos = vec![res("a/one", true), res("a/two", true)];
        let unresolved = unresolved_repos(&repos);
        assert!(
            unresolved.is_empty(),
            "a fully-resolved repo list must produce an empty unresolved set (ResolveModal early-return guard)"
        );
    }

    #[test]
    fn nothing_over_the_limit_and_no_void_pattern_is_no_block() {
        assert_eq!(
            over_limit_block(
                "1 MB",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new()
            ),
            None
        );
    }

    fn file(repo: &str, path: &str, display: &str, bytes: u64) -> OverLimitFile {
        OverLimitFile {
            repo: repo.into(),
            path: path.into(),
            display: display.into(),
            reason: String::new(),
            bytes,
        }
    }

    #[test]
    fn the_block_names_each_file_in_pattern_form_with_its_size() {
        let files = vec![
            file("acme/api", "seeds/data.js", "api/seeds/data.js", 1_258_291),
            file("acme/api", "dump.sql", "api/dump.sql", 6_100_000),
        ];
        let block = over_limit_block(
            "1 MB",
            files.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            over_limit_text(&block, true),
            "2 files will not be read for the inspection (over the 1 MB file size limit). Each one is still searched for secrets by pattern checks, never by the model; one that is too large or not text is checked by name only. A start is not held."
        );
        assert_eq!(
            over_limit_file_line(&files[0]),
            "api/seeds/data.js (1.2 MB)"
        );
        let one = over_limit_block(
            "1 MB",
            files[..1].to_vec(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert!(over_limit_text(&one, true).starts_with(
            "1 file will not be read for the inspection (over the 1 MB file size limit)."
        ));
        assert!(over_limit_text(&one, true).contains("It is still searched for secrets"));
        assert!(over_limit_text(
            &over_limit_block("", files, Vec::new(), Vec::new(), Vec::new(), Vec::new()).unwrap(),
            true
        )
        .starts_with("2 files will not be read for the inspection (over the file size limit)."));
    }

    #[test]
    fn a_file_over_the_limit_never_prints_as_one_point_zero() {
        assert_eq!(format_file_size(1_048_576), "1.0 MB", "exactly the limit");
        assert_eq!(format_file_size(1_048_577), "1.1 MB");
        assert_eq!(format_file_size(1_258_291), "1.2 MB");
    }

    #[test]
    fn a_pattern_that_matches_nothing_blocks_with_its_own_words() {
        let block = over_limit_block(
            "1 MB",
            Vec::new(),
            vec!["app/sedes/**".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert!(over_limit_text(&block, true).is_empty());
        assert!(unmatched_text(&block)
            .starts_with("Excluded by agreement holds a pattern that matches no file."));
        assert_eq!(
            over_limit_file_line(&file("acme/api", "a.js", "", 5)),
            "acme/api/a.js (5 B)",
            "no display from the server: the repo and path"
        );
    }

    #[test]
    fn a_file_with_a_long_line_is_listed_with_its_reason_and_the_text_says_both_causes() {
        let mut wide = file("acme/api", "d/wide.js", "api/d/wide.js", 150_000);
        wide.reason = "has a line longer than 97 KB".to_string();
        assert_eq!(
            over_limit_file_line(&wide),
            "api/d/wide.js (146 KB, has a line longer than 97 KB)"
        );
        let block = over_limit_block(
            "1 MB",
            vec![wide],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            over_limit_text(&block, true),
            "1 file will not be read for the inspection (over the 1 MB file size limit or has a line longer than 97 KB). It is still searched for secrets by pattern checks, never by the model; one that is too large or not text is checked by name only. A start is not held."
        );
    }

    /// A pattern in a never-read folder is a warning: it makes a block (so it is listed) but never
    /// holds the start; a pattern that matches nothing, a file over the limit, or a pattern that
    /// cannot be checked still does.
    #[test]
    fn a_warned_pattern_is_listed_but_never_holds_the_start() {
        let warned = vec![(
            "api/node_modules/**".to_string(),
            "node_modules is never read, so this pattern is not needed".to_string(),
        )];
        let only_warned = over_limit_block(
            "1 MB",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            warned.clone(),
        )
        .unwrap();
        assert!(!only_warned.blocks());
        // A file over the limit is a warning too: listed, never a hold.
        let only_files = over_limit_block(
            "1 MB",
            vec![file("acme/api", "d/big.js", "api/d/big.js", 2_000_000)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert!(!only_files.blocks());
        assert_eq!(
            warned_text(&only_warned),
            "Excluded by agreement holds a pattern that matches only files Camerata never reads, so it changes nothing. A start is not held; you can remove it."
        );
        let with_unmatched = over_limit_block(
            "1 MB",
            Vec::new(),
            vec!["api/typo/**".to_string()],
            vec![String::new()],
            Vec::new(),
            warned,
        )
        .unwrap();
        assert!(with_unmatched.blocks());
    }

    /// With the security floor off the warning must not say the file is searched; and the ceiling
    /// is the one the server sends, printed as "over 20 MB".
    #[test]
    fn the_warning_follows_the_floor_and_names_the_secret_search_ceiling() {
        let block = over_limit_block(
            "1 MB",
            vec![file("acme/api", "d/big.js", "api/d/big.js", 2_000_000)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .with_secret_limit("20 MB");
        let on = over_limit_text(&block, true);
        assert!(
            on.contains("one over 20 MB or not text is checked by name only"),
            "{on}"
        );
        assert!(!on.contains("too large"), "{on}");
        let off = over_limit_text(&block, false);
        assert!(
            off.contains("not searched for secrets in this run: the security floor is off"),
            "{off}"
        );
        assert!(!off.contains("still searched"), "{off}");
    }
    #[test]
    fn the_generated_sentence_counts_files_and_lines_in_the_one_coverage_phrase() {
        let block = OverLimitBlock::default().with_generated(1, 1);
        assert_eq!(
            generated_text(&block, true),
            "1 file (1 line) will not be read for the inspection, generated: scanned for secrets only, not billed."
        );
        let block = OverLimitBlock::default().with_generated(4, 1_234_567);
        assert!(generated_text(&block, true).starts_with("4 files (1,234,567 lines)"));
        assert!(generated_text(&block, false).contains("not searched for secrets in this run"));
        assert!(generated_text(&OverLimitBlock::default(), true).is_empty());
    }
}
