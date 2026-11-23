//! The project's exclusion list: files the client and the operator agreed are not part of the
//! inspection ("excluded by agreement"), such as seed data or a vendored export.
//!
//! One list decides what a run reads. A file is read and billed in full, or it is not read at
//! all; the list is applied in the shared walk ([`super::files`]) and by every other reader of
//! the checkout, so a file named here appears nowhere: not in a finding, the secrets floor, a
//! linter result, a dependency result, a symbol search, the code viewer or the billed count.
//!
//! A pattern is a glob matched against `<repo name>/<path within the repo>`, with forward
//! slashes, so one form works for a project of several repos. `*` stays inside one folder
//! name, `**` crosses folders, and an exact path is a pattern with no wildcard.
//! Example: `acme-field-data/seeds/**`.

use std::sync::Arc;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// The compiled exclusion list of one project. Cheap to clone.
#[derive(Clone, Default, Debug)]
pub struct ScanScope {
    patterns: Vec<String>,
    /// The project's repos, set by [`Self::check_names`]: what a pattern's first segment names.
    repos: Vec<String>,
    set: Option<Arc<GlobSet>>,
}

impl PartialEq for ScanScope {
    fn eq(&self, other: &Self) -> bool {
        self.patterns == other.patterns
    }
}

/// The repo name a pattern starts with: the last part of an `owner/repo` spec.
pub fn repo_name(spec: &str) -> &str {
    spec.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
}

/// Trim every pattern, drop blank ones and repeats, keeping the order given.
pub fn normalize_patterns(patterns: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in patterns {
        let p = p.trim();
        if !p.is_empty() && !out.iter().any(|seen| seen == p) {
            out.push(p.to_string());
        }
    }
    out
}

fn compile(pattern: &str) -> Result<globset::Glob, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("\"{pattern}\" is not a valid pattern ({})", e.kind()))
}

/// Refuse the first pattern that is not a valid glob, naming it.
pub fn validate_patterns(patterns: &[String]) -> Result<(), String> {
    for p in patterns {
        compile(p.trim())?;
    }
    Ok(())
}

impl ScanScope {
    /// No exclusions: every admissible file is read.
    pub fn none() -> Self {
        Self::default()
    }

    /// Compile a project's patterns. `Err` names the first invalid pattern.
    pub fn from_patterns(patterns: &[String]) -> Result<Self, String> {
        let patterns = normalize_patterns(patterns);
        if patterns.is_empty() {
            return Ok(Self::none());
        }
        // Two globs for each pattern: the path itself and everything under it, so a trailing
        // slash or a bare folder name means the folder and all it holds.
        let mut builder = GlobSetBuilder::new();
        for p in &patterns {
            let base = p.trim_end_matches('/');
            builder.add(compile(base)?);
            builder.add(compile(&format!("{base}/**"))?);
        }
        let set = builder
            .build()
            .map_err(|e| format!("Excluded by agreement could not be used ({e})"))?;
        Ok(Self {
            patterns,
            repos: Vec::new(),
            set: Some(Arc::new(set)),
        })
    }

    /// The scope of a project (`None` = no project selected = no exclusions).
    pub fn of_project(
        project: Option<&camerata_app_core::project::Project>,
    ) -> Result<Self, String> {
        match project {
            Some(p) => Self::from_patterns(&p.scan_exclusions)?.check_names(&p.repos),
            None => Ok(Self::none()),
        }
    }

    /// Refuse a pattern that starts with a repo name two of `repos` share (`app/...` when both
    /// `acme/app` and `other/app` are in the project), naming the pattern and both repos. The
    /// `owner/repo/...` form is never ambiguous.
    pub fn check_names(mut self, repos: &[String]) -> Result<Self, String> {
        self.repos = repos.to_vec();
        for pattern in &self.patterns {
            let first = pattern.split('/').next().unwrap_or("");
            // The same notion of "shares a name" as `pattern_form`: any two specs with it.
            let sharing: Vec<&String> = repos.iter().filter(|r| repo_name(r) == first).collect();
            // Only a spec with an owner has an owner form.
            let owner_form = sharing
                .iter()
                .any(|r| r.contains('/') && pattern.starts_with(&format!("{r}/")));
            if sharing.len() > 1 && !owner_form {
                let names: Vec<&str> = sharing.iter().map(|r| r.as_str()).collect();
                return Err(format!(
                    "\"{pattern}\" is ambiguous: {} are both called {first}. Write it as owner/repo/path, for example {}/{}",
                    names.join(" and "),
                    names[0],
                    pattern.split_once('/').map_or("", |x| x.1)
                ));
            }
        }
        Ok(self)
    }

    /// The patterns this scope was built from (normalized), for display.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// The project's repos this scope knows (empty when it was built without a project).
    pub fn repos(&self) -> &[String] {
        &self.repos
    }

    /// [`Self::of_project`] for a reader that has no way to refuse: a list that cannot be used
    /// excludes EVERY file, so the reader sees nothing rather than a file the agreement leaves
    /// out. The start paths refuse with the reason first ([`start_refusal`]).
    pub fn of_project_or_nothing(project: Option<&camerata_app_core::project::Project>) -> Self {
        Self::of_project(project).unwrap_or_else(|_| {
            let mut builder = GlobSetBuilder::new();
            builder.add(
                GlobBuilder::new("**")
                    .build()
                    .expect("`**` is a valid pattern"),
            );
            Self {
                patterns: vec!["**".to_string()],
                repos: Vec::new(),
                set: Some(Arc::new(builder.build().expect("one pattern builds"))),
            }
        })
    }

    /// True when `rel` (path within the repo `spec`) is excluded by agreement.
    pub fn excludes(&self, spec: &str, rel: &str) -> bool {
        !self.matching(spec, rel).is_empty()
    }

    /// The indexes (into [`Self::patterns`]) of every pattern the file `rel` of the repo `spec`
    /// matches, by `repo/path` or `owner/repo/path`.
    pub fn matching(&self, spec: &str, rel: &str) -> Vec<usize> {
        let Some(set) = &self.set else {
            return Vec::new();
        };
        let rel = rel.trim_start_matches("./");
        let mut hits: Vec<usize> = set
            .matches(format!("{}/{rel}", repo_name(spec)))
            .into_iter()
            .collect();
        if spec.trim().contains('/') {
            hits.extend(set.matches(format!("{}/{rel}", spec.trim().trim_end_matches('/'))));
        }
        let mut hits: Vec<usize> = hits.into_iter().map(|g| g / 2).collect();
        hits.sort_unstable();
        hits.dedup();
        hits
    }

    /// The shared "is this path admitted for this project" check every reader of a checkout
    /// uses besides the walk itself: false for a path excluded by agreement.
    pub fn admits(&self, spec: &str, rel: &str) -> bool {
        !self.excludes(spec, rel)
    }

    /// This scope for the one repo `spec`, as the walk takes it.
    pub fn for_repo(&self, spec: &str) -> RepoScope {
        RepoScope {
            scope: self.clone(),
            repo: spec.trim().to_string(),
        }
    }
}

/// The exclusion list a reader of the checkout applies for a request that names `project` (a
/// project id), else the active project: the code viewer, the finding context, the symbol
/// search and the evidence check all read one file by path and refuse one the list names.
pub(crate) fn scope_for_read(state: &crate::AppState, project: Option<&str>) -> ScanScope {
    ScanScope::of_project_or_nothing(
        project
            .and_then(|id| state.projects.get(id))
            .or_else(|| state.projects.active())
            .as_ref(),
    )
}

/// A [`ScanScope`] bound to one repo: what [`super::files::read_local_repo_files`] takes.
#[derive(Clone, Debug, Default)]
pub struct RepoScope {
    scope: ScanScope,
    repo: String,
}

impl RepoScope {
    /// No exclusions (the repo name is then not needed).
    pub fn none() -> Self {
        Self::default()
    }

    /// True when `rel` is excluded by agreement in this repo.
    pub fn excludes(&self, rel: &str) -> bool {
        self.scope.excludes(&self.repo, rel)
    }

    /// The indexes of the patterns `rel` matches in this repo.
    pub fn matching(&self, rel: &str) -> Vec<usize> {
        self.scope.matching(&self.repo, rel)
    }

    /// The patterns this scope holds.
    pub fn patterns(&self) -> &[String] {
        self.scope.patterns()
    }

    /// The repo this scope is bound to, as it was named.
    pub fn spec(&self) -> &str {
        &self.repo
    }

    /// Whether `pattern` is written for this repo (by `owner/repo/...` or by its name).
    pub fn is_for_this_repo(&self, pattern: &str) -> bool {
        names_any_repo(pattern) || target_repo(pattern, std::slice::from_ref(&self.repo)).is_some()
    }
}

/// The project repo (spec) a pattern names: by `owner/repo/...`, else by its first segment.
pub fn target_repo<'a>(pattern: &str, repos: &'a [String]) -> Option<&'a String> {
    repos
        .iter()
        .find(|r| r.contains('/') && pattern.starts_with(&format!("{}/", r.trim())))
        .or_else(|| {
            let first = pattern.split('/').next().unwrap_or("");
            repos.iter().find(|r| repo_name(r) == first)
        })
}

/// Whether a pattern's first segment is a glob (`*/seeds/**`): it names no one repo, so it can
/// match in any.
pub fn names_any_repo(pattern: &str) -> bool {
    pattern
        .split('/')
        .next()
        .is_some_and(|seg| seg.contains(['*', '?', '[', '{']))
}

/// The folder a pattern names that the walk never reads, when there is one (`node_modules` in
/// `app/node_modules/**`): such a pattern can never match a file, and is not needed. The repo
/// segment(s) at the front are not folders (a repo called `dist` is still read).
pub fn never_read_folder(pattern: &str, repos: &[String]) -> Option<String> {
    let skip = match target_repo(pattern, repos) {
        Some(r) if r.contains('/') && pattern.starts_with(&format!("{}/", r.trim())) => 2,
        _ => 1,
    };
    pattern
        .split('/')
        .skip(skip)
        .find(|seg| super::files::is_noise_dir_name(seg))
        .map(str::to_string)
}

/// For each pattern, why it matches nothing when its folder is never read, else "".
pub fn unmatched_notes(patterns: &[String], repos: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| {
            never_read_folder(p, repos)
                .map(|f| format!("{f} is never read, so this pattern is not needed"))
                .unwrap_or_default()
        })
        .collect()
}

/// What the walk could say about each pattern that matched no file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PatternChecks {
    /// Patterns that match no file in a repo that WAS read, or name no repo of the project. Safe
    /// to remove: nothing they name exists.
    pub unmatched: Vec<String>,
    /// Patterns that match only files Camerata never reads anyway (a never-read folder such as
    /// `node_modules`), with the reason: `(pattern, "node_modules is never read, ...")`. A
    /// warning only: they never block a start, and removing them changes nothing.
    pub warned: Vec<(String, String)>,
    /// Patterns naming a project repo that was NOT read: `(pattern, reason)`. They cannot be
    /// judged, so they block a start but are never offered for removal.
    pub unchecked: Vec<(String, String)>,
}

/// Judge each pattern that no walked file matched (`matched[i]` false). A pattern is judged only
/// when the repo it names was read: `read_ok` holds the specs read without error, `project_repos`
/// every repo of the project (the scope's own when it knows them, else `fallback_repos`).
pub fn classify_patterns(
    scope: &ScanScope,
    fallback_repos: &[String],
    read_ok: &[String],
    matched: &[bool],
) -> PatternChecks {
    let repos: &[String] = if scope.repos().is_empty() {
        fallback_repos
    } else {
        scope.repos()
    };
    let mut out = PatternChecks::default();
    for (i, p) in scope.patterns().iter().enumerate() {
        if matched.get(i).copied().unwrap_or(false) {
            continue;
        }
        // A pattern that names no one repo can match in any: it is judged only when every repo
        // of the project was read.
        let unread = if names_any_repo(p) {
            repos.iter().find(|r| !read_ok.iter().any(|ok| ok == *r))
        } else {
            target_repo(p, repos).filter(|r| !read_ok.iter().any(|ok| ok == *r))
        };
        match unread {
            Some(r) => out
                .unchecked
                .push((p.clone(), format!("cannot be checked: {r} was not read"))),
            None => {
                let names_a_repo = names_any_repo(p) || target_repo(p, repos).is_some();
                match never_read_folder(p, repos).filter(|_| names_a_repo) {
                    Some(folder) => out.warned.push((
                        p.clone(),
                        format!("{folder} is never read, so this pattern is not needed"),
                    )),
                    None => out.unmatched.push(p.clone()),
                }
            }
        }
    }
    out
}

/// The patterns that match no file, judged only against repos that were read, in list order.
/// Walks each repo once.
pub async fn unmatched_patterns(
    sources: &[(String, std::path::PathBuf)],
    scope: &ScanScope,
) -> PatternChecks {
    let w = walk_checks(sources, scope).await;
    PatternChecks {
        unmatched: w.unmatched,
        warned: w.warned,
        unchecked: w.unchecked,
    }
}

struct WalkChecks {
    /// Files over the limit that the secret checks still search (up to the secret search
    /// ceiling), as `repo/path (size)` in pattern form, with `, <reason>` for a long line.
    named: Vec<String>,
    /// Files over the secret search ceiling: checked by name only.
    name_only: Vec<String>,
    unmatched: Vec<String>,
    warned: Vec<(String, String)>,
    unchecked: Vec<(String, String)>,
}

/// One walk of every source: the files over the read limit not excluded by agreement (as
/// `repo/path (size)` in pattern form), and the patterns that match nothing.
async fn walk_checks(sources: &[(String, std::path::PathBuf)], scope: &ScanScope) -> WalkChecks {
    let repos: Vec<String> = sources.iter().map(|(s, _)| s.clone()).collect();
    let mut named: Vec<String> = Vec::new();
    let mut name_only: Vec<String> = Vec::new();
    let mut read_ok: Vec<String> = Vec::new();
    let mut matched = vec![false; scope.patterns().len()];
    for (spec, dir) in sources {
        let (dir, repo_scope) = (dir.clone(), scope.for_repo(spec));
        let Ok(Ok(extracted)) = tokio::task::spawn_blocking(move || {
            super::files::read_local_repo_files(&dir, &repo_scope)
        })
        .await
        else {
            // A repo that cannot be read is reported by the run itself, in its own words. Its
            // patterns are not judged: they come back as "cannot be checked".
            continue;
        };
        read_ok.push(spec.clone());
        for i in &extracted.matched_patterns {
            matched[*i] = true;
        }
        for f in extracted.over_limit_files() {
            let why = if f.reason.is_empty() {
                String::new()
            } else {
                format!(", {}", f.reason)
            };
            let line = format!(
                "{} ({}{why})",
                pattern_form(&repos, spec, &f.path),
                crate::report_export::coverage_gaps::format_size(f.bytes)
            );
            if f.bytes > super::files::MAX_SECRET_SCAN_BYTES as u64 {
                name_only.push(line);
            } else {
                named.push(line);
            }
        }
    }
    let checks = classify_patterns(scope, &repos, &read_ok, &matched);
    WalkChecks {
        named,
        name_only,
        unmatched: checks.unmatched,
        warned: checks.warned,
        unchecked: checks.unchecked,
    }
}

/// The refusal for a run that cannot start, or `None`. Two causes: an Excluded by agreement
/// pattern that matches no file (named, so an intended exclusion can never be silently void), and
/// a pattern naming a repo that was not read. A file over the 1 MB file size limit is not a cause:
/// it is a warning ([`start_checks`]). Walks each repo once; no model is called.
pub async fn over_limit_refusal(
    sources: &[(String, std::path::PathBuf)],
    scope: &ScanScope,
) -> Option<String> {
    start_checks(sources, scope).await.0
}

/// [`over_limit_refusal`] plus the warnings a start goes ahead with: a pattern that matches only
/// files Camerata never reads is a warning, never a block.
pub async fn start_checks(
    sources: &[(String, std::path::PathBuf)],
    scope: &ScanScope,
) -> (Option<String>, Vec<String>) {
    start_checks_for(sources, scope, true).await
}

/// [`start_checks`] for a run whose security floor is on (`floor_on`) or off (the AI only
/// choice): with it off, the file warning says the files are not searched for secrets.
pub async fn start_checks_for(
    sources: &[(String, std::path::PathBuf)],
    scope: &ScanScope,
    floor_on: bool,
) -> (Option<String>, Vec<String>) {
    let w = walk_checks(sources, scope).await;
    let mut warnings: Vec<String> = w
        .warned
        .iter()
        .map(|(p, note)| {
            format!("Excluded by agreement holds \"{p}\" (matches no file: {note}); the start goes ahead")
        })
        .collect();
    warnings.extend(file_warnings(&w, floor_on));
    (refusal_text(w, true, scope.repos()), warnings)
}

/// Only the file warnings of [`start_checks_for`], for the paths that never refuse on a pattern
/// (a rescan reads some of a project's repos, so it cannot judge a pattern for a repo not read).
pub async fn over_limit_warnings(
    sources: &[(String, std::path::PathBuf)],
    scope: &ScanScope,
    floor_on: bool,
) -> Vec<String> {
    file_warnings(&walk_checks(sources, scope).await, floor_on)
}

/// The warnings for files the run will not read: over the 1 MB file size limit or with a line
/// longer than the read page. They never hold a start. Up to the secret search ceiling the secret
/// checks still search the file's text by pattern, never by a model; over it, or for a file that is
/// not text, only the file name is checked.
fn file_warnings(w: &WalkChecks, floor_on: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut part = |list: &[String], qualifier: &str, tail_one: &str, tail_each: &str| {
        if list.is_empty() {
            return;
        }
        let one = list.len() == 1;
        out.push(format!(
            "{} {}{qualifier} will not be read for the inspection: {}. {}",
            list.len(),
            if one { "file" } else { "files" },
            list.join("; "),
            if one { tail_one } else { tail_each }
        ));
    };
    let off = "It is not searched for secrets in this run: the security floor is off";
    let off_each = "Each is not searched for secrets in this run: the security floor is off";
    part(
        &w.named,
        "",
        if floor_on {
            "It is still searched for secrets by pattern checks, never by the model, if it is text, and checked by name only if not"
        } else {
            off
        },
        if floor_on {
            "Each is still searched for secrets by pattern checks, never by the model, if it is text, and checked by name only if not"
        } else {
            off_each
        },
    );
    part(
        &w.name_only,
        &format!(" over {}", super::files::secret_scan_label()),
        if floor_on {
            "It is checked by name only"
        } else {
            off
        },
        if floor_on {
            "Each is checked by name only"
        } else {
            off_each
        },
    );
    out
}

fn refusal_text(w: WalkChecks, with_unmatched: bool, repos: &[String]) -> Option<String> {
    let WalkChecks {
        named: _,
        name_only: _,
        unmatched,
        unchecked,
        warned: _,
    } = w;
    let (unmatched, unchecked) = if with_unmatched {
        (unmatched, unchecked)
    } else {
        (Vec::new(), Vec::new())
    };
    let mut parts: Vec<String> = Vec::new();
    if !unmatched.is_empty() {
        let notes = unmatched_notes(&unmatched, repos);
        let list: Vec<String> = unmatched
            .iter()
            .zip(notes)
            .map(|(p, note)| {
                if note.is_empty() {
                    format!("\"{p}\"")
                } else {
                    format!("\"{p}\" (matches no file: {note})")
                }
            })
            .collect();
        parts.push(format!(
            "Excluded by agreement holds {} that match{} no file: {}. Fix or remove {}",
            if unmatched.len() == 1 {
                "a pattern"
            } else {
                "patterns"
            },
            if unmatched.len() == 1 { "es" } else { "" },
            list.join(", "),
            if unmatched.len() == 1 { "it" } else { "them" },
        ));
    }
    if !unchecked.is_empty() {
        let list: Vec<String> = unchecked
            .iter()
            .map(|(p, why)| format!("\"{p}\" ({why})"))
            .collect();
        parts.push(format!(
            "Excluded by agreement holds {} that cannot be checked: {}. Set the folder of the repo it names, or run again once it can be read",
            if unchecked.len() == 1 { "a pattern" } else { "patterns" },
            list.join(", ")
        ));
    }
    (!parts.is_empty())
        .then(|| format!("Cannot start: {}, then start again.", parts.join(". Also ")))
}

/// [`over_limit_refusal`] for a project's start: also refuses when the project's Excluded by
/// agreement list cannot be used, so a bad list never reads as "nothing is excluded".
pub async fn start_refusal(
    project: Option<&camerata_app_core::project::Project>,
    sources: &[(String, std::path::PathBuf)],
) -> Option<String> {
    start_check(project, sources, true).await.0
}

/// [`start_refusal`] with the warnings the start goes ahead with.
pub async fn start_check(
    project: Option<&camerata_app_core::project::Project>,
    sources: &[(String, std::path::PathBuf)],
    floor_on: bool,
) -> (Option<String>, Vec<String>) {
    match ScanScope::of_project(project) {
        Err(why) => (
            Some(format!(
                "Cannot start: Excluded by agreement in the inspection settings cannot be used: {why}."
            )),
            Vec::new(),
        ),
        Ok(scope) => start_checks_for(sources, &scope, floor_on).await,
    }
}

/// The path as a pattern writes it: `repo/path`, or `owner/repo/path` when another repo in
/// `repos` has the same name.
pub fn pattern_form(repos: &[String], spec: &str, rel: &str) -> String {
    let name = repo_name(spec);
    let shared = repos.iter().filter(|r| repo_name(r) == name).count() > 1;
    if shared {
        format!("{}/{rel}", spec.trim())
    } else {
        format!("{name}/{rel}")
    }
}

/// [`exact_pattern`] in the form [`pattern_form`] chooses for `repos`.
pub fn exact_pattern_in(repos: &[String], spec: &str, rel: &str) -> String {
    escape_glob(&pattern_form(repos, spec, rel))
}

fn escape_glob(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The pattern that names exactly the file `rel` of the repo `spec`: `<repo name>/<path>` with
/// every glob character escaped, so a path holding `[` or `*` still names only itself.
pub fn exact_pattern(spec: &str, rel: &str) -> String {
    escape_glob(&join_repo_path(spec, rel))
}

/// `<repo name>/<path>`, the form an exclusion pattern is written against.
pub fn join_repo_path(spec: &str, rel: &str) -> String {
    format!("{}/{}", repo_name(spec), rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(patterns: &[&str]) -> ScanScope {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        ScanScope::from_patterns(&patterns).expect("valid patterns")
    }

    #[test]
    fn an_exact_path_is_a_pattern_with_no_wildcard() {
        let s = scope(&["app/seeds/data.js"]);
        assert!(s.excludes("acme/app", "seeds/data.js"));
        assert!(!s.excludes("acme/app", "seeds/data.json"));
        assert!(!s.excludes("acme/app", "other/seeds/data.js"));
    }

    #[test]
    fn a_double_star_crosses_folders_and_a_single_star_does_not() {
        let s = scope(&["app/seeds/**", "app/*.sql"]);
        assert!(s.excludes("acme/app", "seeds/a/b/c.js"));
        assert!(s.excludes("acme/app", "dump.sql"));
        assert!(
            !s.excludes("acme/app", "db/dump.sql"),
            "`*` stays in one folder"
        );
    }

    #[test]
    fn the_repo_name_is_the_last_part_of_the_spec_so_each_repo_has_its_own_list() {
        let s = scope(&["acme-field-data/seeds/**"]);
        assert!(s.excludes("acme/acme-field-data", "seeds/x.js"));
        assert!(s.excludes("acme-field-data", "seeds/x.js"));
        assert!(!s.excludes("acme/acme-api", "seeds/x.js"));
    }

    #[test]
    fn an_empty_list_excludes_nothing_and_blank_lines_are_dropped() {
        assert!(!ScanScope::none().excludes("acme/app", "a.rs"));
        let s = scope(&["  ", "app/a.rs", "app/a.rs", ""]);
        assert_eq!(s.patterns(), ["app/a.rs"]);
    }

    #[test]
    fn an_invalid_pattern_is_refused_and_named() {
        let err =
            validate_patterns(&["app/ok/**".to_string(), "app/[broken".to_string()]).unwrap_err();
        assert!(err.contains("app/[broken"), "{err}");
        assert!(ScanScope::from_patterns(&["app/[broken".to_string()]).is_err());
    }

    #[test]
    fn a_list_that_cannot_be_used_excludes_everything_for_a_reader_that_cannot_refuse() {
        let project: camerata_app_core::project::Project = serde_json::from_value(
            serde_json::json!({ "id": "p1", "name": "P", "scan_exclusions": ["app/[broken"] }),
        )
        .expect("a project with an exclusion list");
        let s = ScanScope::of_project_or_nothing(Some(&project));
        assert!(s.excludes("acme/app", "any/file.rs"));
        assert!(ScanScope::of_project(Some(&project)).is_err());
    }

    #[test]
    fn an_exact_pattern_names_only_that_file_even_with_glob_characters_in_the_path() {
        let pattern = exact_pattern("acme/app", "seeds/[a]*.js");
        assert_eq!(pattern, "app/seeds/\\[a\\]\\*.js");
        let s = scope(&[pattern.as_str()]);
        assert!(s.excludes("acme/app", "seeds/[a]*.js"));
        assert!(!s.excludes("acme/app", "seeds/a.js"));
        assert!(!s.excludes("acme/app", "seeds/[a]x.js"));
    }

    #[test]
    fn admits_is_the_opposite_of_excludes() {
        let s = scope(&["app/seeds/**"]);
        assert!(!s.admits("acme/app", "seeds/a.js"));
        assert!(s.admits("acme/app", "src/a.js"));
    }

    /// Item 38 / round 3 ruling: a file over the file size limit is a WARNING, never a refusal.
    /// The warning names every file with its size and says what still happens to it.
    #[tokio::test]
    async fn a_file_over_the_limit_warns_with_its_size_and_never_refuses_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(super::super::files::MAX_FILE_BYTES + 1);
        for rel in ["src/big.rs", "seeds/huge.js"] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &big).unwrap();
        }
        std::fs::write(dir.path().join("src/small.rs"), "fn a() {}\n").unwrap();
        let sources = vec![("acme/app".to_string(), dir.path().to_path_buf())];

        let (refusal, warnings) = start_checks(&sources, &ScanScope::none()).await;
        assert_eq!(refusal, None, "a file over the limit never refuses a start");
        let text = warnings.join("\n");
        assert!(text.contains("app/seeds/huge.js (1.1 MB)"), "{text}");
        assert!(text.contains("app/src/big.rs (1.1 MB)"), "{text}");
        assert!(
            text.contains("2 files will not be read for the inspection"),
            "{text}"
        );
        assert!(
            text.contains("searched for secrets by pattern checks, never by the model"),
            "{text}"
        );
        assert!(!text.contains("small.rs"), "{text}");
        assert_eq!(over_limit_refusal(&sources, &ScanScope::none()).await, None);

        // Excluded by agreement, a file is no longer named; the other still is.
        let one = scope(&["app/seeds/**"]);
        let (refusal, warnings) = start_checks(&sources, &one).await;
        assert_eq!(refusal, None);
        let text = warnings.join("\n");
        assert!(text.contains("1 file will not be read"), "{text}");
        assert!(!text.contains("huge.js"), "{text}");
        let both = scope(&["app/seeds/**", "app/src/big.rs"]);
        assert_eq!(start_checks(&sources, &both).await, (None, Vec::new()));
    }

    /// With the security floor off (AI only), the warning must not say the file is searched.
    #[tokio::test]
    async fn with_the_floor_off_the_warning_says_the_file_is_not_searched_for_secrets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("big.js"),
            "x".repeat(super::super::files::MAX_FILE_BYTES + 1),
        )
        .unwrap();
        let sources = vec![("acme/app".to_string(), dir.path().to_path_buf())];
        let (refusal, warnings) = start_checks_for(&sources, &ScanScope::none(), false).await;
        assert_eq!(refusal, None);
        let text = warnings.join("\n");
        assert!(text.contains("app/big.js"), "{text}");
        assert!(
            text.contains("not searched for secrets in this run: the security floor is off"),
            "{text}"
        );
        assert!(!text.contains("is still searched for secrets"), "{text}");
        let (_, on) = start_checks_for(&sources, &ScanScope::none(), true).await;
        assert!(on.join("\n").contains("is still searched for secrets"));
        assert_eq!(
            over_limit_warnings(&sources, &ScanScope::none(), false).await,
            warnings
        );
    }

    /// A file over 20 MB is checked by name only, and the warning says so.
    #[tokio::test]
    async fn a_file_over_twenty_megabytes_warns_that_it_is_checked_by_name_only() {
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::create(dir.path().join("huge.sql")).unwrap();
        f.set_len(super::super::files::MAX_SECRET_SCAN_BYTES as u64 + 1)
            .unwrap();
        let sources = vec![("acme/app".to_string(), dir.path().to_path_buf())];
        let (refusal, warnings) = start_checks(&sources, &ScanScope::none()).await;
        assert_eq!(refusal, None);
        let text = warnings.join("\n");
        assert!(text.contains("app/huge.sql"), "{text}");
        assert!(
            text.contains("over 20 MB will not be read for the inspection"),
            "{text}"
        );
        assert!(text.contains("checked by name only"), "{text}");
    }

    // ── Round 1 fixes ───────────────────────────────────────────────────────────────────

    #[test]
    fn a_trailing_slash_or_a_bare_folder_name_means_the_folder_and_everything_under_it() {
        for pattern in ["app/seeds/", "app/seeds", "app/seeds/**"] {
            let s = scope(&[pattern]);
            assert!(s.excludes("acme/app", "seeds/a/b.js"), "{pattern}");
            assert!(!s.excludes("acme/app", "src/a.js"), "{pattern}");
        }
        assert!(
            scope(&["app/seeds"]).excludes("acme/app", "seeds"),
            "a file by that name too"
        );
    }

    #[test]
    fn a_pattern_may_start_with_owner_slash_repo() {
        let s = scope(&["acme/app/seeds/**"]);
        assert!(s.excludes("acme/app", "seeds/a.js"));
        assert!(!s.excludes("other/app", "seeds/a.js"));
    }

    #[test]
    fn matching_names_the_patterns_a_file_matches() {
        let s = scope(&["app/seeds/**", "app/dump.sql", "app/nothing/**"]);
        assert_eq!(s.for_repo("acme/app").matching("seeds/x.js"), [0]);
        assert_eq!(s.for_repo("acme/app").matching("dump.sql"), [1]);
        assert!(s.for_repo("acme/app").matching("src/a.rs").is_empty());
    }

    #[test]
    fn two_repos_with_one_name_make_a_bare_repo_pattern_ambiguous() {
        let repos = vec![
            "acme/app".to_string(),
            "other/app".to_string(),
            "acme/web".to_string(),
        ];
        let err = ScanScope::from_patterns(&["app/seeds/**".to_string()])
            .unwrap()
            .check_names(&repos)
            .unwrap_err();
        assert!(
            err.contains("app/seeds/**") && err.contains("acme/app") && err.contains("other/app"),
            "{err}"
        );
        let ok = ScanScope::from_patterns(&["acme/app/seeds/**".to_string(), "web/x".to_string()])
            .unwrap()
            .check_names(&repos);
        assert!(ok.is_ok(), "{ok:?}");
        assert_eq!(pattern_form(&repos, "acme/app", "a.js"), "acme/app/a.js");
        assert_eq!(pattern_form(&repos, "acme/web", "a.js"), "web/a.js");
    }

    #[test]
    fn the_exact_pattern_uses_the_owner_form_when_the_name_is_ambiguous() {
        let repos = vec!["acme/app".to_string(), "other/app".to_string()];
        assert_eq!(
            exact_pattern_in(&repos, "acme/app", "seeds/a.js"),
            "acme/app/seeds/a.js"
        );
        let one = vec!["acme/app".to_string()];
        assert_eq!(
            exact_pattern_in(&one, "acme/app", "seeds/a.js"),
            "app/seeds/a.js"
        );
    }

    #[tokio::test]
    async fn a_start_is_refused_while_a_pattern_matches_nothing_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("seeds")).unwrap();
        std::fs::write(dir.path().join("seeds/a.js"), "var a;\n").unwrap();
        let sources = vec![("acme/app".to_string(), dir.path().to_path_buf())];
        let why = over_limit_refusal(&sources, &scope(&["app/seeds/**", "app/sedes/**"]))
            .await
            .expect("a pattern that names nothing refuses the start");
        assert!(why.contains("app/sedes/**"), "{why}");
        assert!(!why.contains("\"app/seeds/**\""), "{why}");
        assert_eq!(
            over_limit_refusal(&sources, &scope(&["app/seeds/**"])).await,
            None
        );
        assert_eq!(
            unmatched_patterns(&sources, &scope(&["app/seeds", "app/x"]))
                .await
                .unmatched,
            ["app/x"]
        );
    }

    #[tokio::test]
    async fn a_file_with_a_line_longer_than_the_page_warns_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wide.js"), "y".repeat(100_001)).unwrap();
        let sources = vec![("acme/app".to_string(), dir.path().to_path_buf())];
        let (refusal, warnings) = start_checks(&sources, &ScanScope::none()).await;
        assert_eq!(refusal, None, "never a refusal");
        let text = warnings.join("\n");
        assert!(
            text.contains("app/wide.js") && text.contains("has a line longer than 97 KB"),
            "{text}"
        );
        assert_eq!(
            start_checks(&sources, &scope(&["app/wide.js"])).await,
            (None, Vec::new())
        );
    }

    /// Zach's ruling: a pattern that matches only files Camerata never reads (a never-read
    /// folder) is a WARNING and never blocks a start; a pattern that matches nothing at all
    /// still blocks.
    #[tokio::test]
    async fn a_pattern_in_a_never_read_folder_warns_and_does_not_block_the_start() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.js"), "var a;\n").unwrap();
        let sources = vec![("acme/app".to_string(), dir.path().to_path_buf())];
        let (refusal, warnings) = start_checks(&sources, &scope(&["app/node_modules/**"])).await;
        assert_eq!(refusal, None, "a never-read pattern never blocks");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("node_modules is never read, so this pattern is not needed"),
            "{warnings:?}"
        );
        let checks =
            unmatched_patterns(&sources, &scope(&["app/node_modules/**", "app/sedes/**"])).await;
        assert_eq!(checks.warned.len(), 1);
        assert_eq!(
            checks.unmatched,
            ["app/sedes/**"],
            "matching nothing at all still blocks"
        );
        let (refusal, _) =
            start_checks(&sources, &scope(&["app/node_modules/**", "app/sedes/**"])).await;
        assert!(refusal.unwrap().contains("app/sedes/**"));
        // A never-read pattern that names no repo of the project is not a warning: it blocks.
        let checks = unmatched_patterns(
            &sources,
            &project_scope(&["zzz/node_modules/**"], &["acme/app"]),
        )
        .await;
        assert_eq!(checks.unmatched, ["zzz/node_modules/**"]);
    }

    // ── Round 2 ─────────────────────────────────────────────────────────────────────────

    fn project_scope(patterns: &[&str], repos: &[&str]) -> ScanScope {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        let repos: Vec<String> = repos.iter().map(|r| r.to_string()).collect();
        ScanScope::from_patterns(&patterns)
            .unwrap()
            .check_names(&repos)
            .unwrap()
    }

    /// Item 1: a pattern naming a project repo that was not read is not judged: it cannot be
    /// checked, it still blocks the start, and it is never reported as unmatched (which the
    /// desktop offers to remove).
    #[tokio::test]
    async fn a_pattern_for_a_repo_that_was_not_read_cannot_be_checked_and_is_not_unmatched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.js"), "var a;\n").unwrap();
        let sources = vec![("acme/a".to_string(), dir.path().to_path_buf())];
        let scope = project_scope(&["b/seeds/**", "zzz/x/**"], &["acme/a", "acme/b"]);
        let checks = unmatched_patterns(&sources, &scope).await;
        assert_eq!(
            checks.unmatched,
            ["zzz/x/**"],
            "naming no project repo stays unmatched"
        );
        assert_eq!(
            checks.unchecked,
            [(
                "b/seeds/**".to_string(),
                "cannot be checked: acme/b was not read".to_string()
            )]
        );
        let why = over_limit_refusal(&sources, &scope)
            .await
            .expect("still blocks");
        assert!(
            why.contains("\"b/seeds/**\" (cannot be checked: acme/b was not read)"),
            "{why}"
        );
    }

    #[tokio::test]
    async fn a_repo_that_fails_to_read_leaves_its_patterns_unchecked_not_unmatched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.js"), "var a;\n").unwrap();
        let sources = vec![
            ("acme/a".to_string(), dir.path().to_path_buf()),
            ("acme/b".to_string(), dir.path().join("gone")),
        ];
        let scope = project_scope(&["b/seeds/**"], &["acme/a", "acme/b"]);
        let checks = unmatched_patterns(&sources, &scope).await;
        assert!(checks.unmatched.is_empty(), "{checks:?}");
        assert_eq!(checks.unchecked.len(), 1);
    }

    /// Item 8: a repo called `dist` is still read; only folders after the repo segment count.
    #[test]
    fn the_repo_segment_is_not_a_never_read_folder() {
        let repos = vec!["acme/dist".to_string()];
        assert_eq!(never_read_folder("dist/src/**", &repos), None);
        assert_eq!(never_read_folder("acme/dist/src/**", &repos), None);
        assert_eq!(
            never_read_folder("acme/dist/node_modules/**", &repos).as_deref(),
            Some("node_modules")
        );
        assert_eq!(
            never_read_folder("dist/dist/x", &repos).as_deref(),
            Some("dist")
        );
    }

    /// Item 8: check_names and pattern_form agree on what "shares a name" means.
    #[test]
    fn check_names_and_pattern_form_agree_on_shared_names() {
        let repos = vec!["acme/app".to_string(), "app".to_string()];
        assert_eq!(pattern_form(&repos, "acme/app", "a.js"), "acme/app/a.js");
        let err = ScanScope::from_patterns(&["app/x/**".to_string()])
            .unwrap()
            .check_names(&repos)
            .unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
    }
}
