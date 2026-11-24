//! The facts a pre-flight cost estimate is built from, gathered with the SAME functions the scan
//! uses, so the estimate prices what the scan will actually be fed.
//!
//! The arithmetic itself is `camerata_ui_core::scan::estimate_audit_cost` (the estimator the
//! cockpit shows); there is deliberately no second estimator here. This module only answers the
//! questions that estimator asks: how much code, how many rules, how many with alternatives.
//! The caller (`camerata inspect`) formats the result.
//!
//! Nothing here makes a model call. The exact token count of one pass
//! ([`exact_sample_tokens`]) goes through the `count_tokens` endpoint, which Anthropic does not
//! bill.

use crate::ai_audit::{representative_scan_sample, ScanMode};
use crate::onboard::SelectedRule;

/// What one repo will be fed.
pub struct RepoScanFacts {
    pub repo: String,
    /// The audited files (the same collection the scan reads).
    pub files: Vec<(String, String)>,
    /// Characters of code across `files`.
    pub code_chars: usize,
    /// The rules the AI tier checks for this repo (every inspectable selected rule that applies).
    pub semantic: Vec<(String, String)>,
    /// How many of those offer 2+ alternatives (the one-off recommendation pass).
    pub multi_option_rules: usize,
    /// Why this repo could not be read, when it could not. Such a repo has no files and no
    /// characters, so its estimate is zero; the caller must say so rather than let a missing
    /// repo read as a cheap one.
    pub read_error: Option<String>,
    /// Files the run could not read because they are over [`crate::onboard::files::MAX_FILE_BYTES`]
    /// and not excluded by agreement. A warning before the start, never a refusal.
    pub over_limit: Vec<crate::onboard::files::OverLimitFile>,
    /// The patterns of the project's Excluded by agreement list this repo's files matched.
    pub matched_patterns: Vec<usize>,
    /// Files that declare themselves generated: not read, not billed, searched for secrets only.
    /// The count and the non-blank lines in them, so the pre-start check shows what was left out.
    pub generated_files: usize,
    pub generated_lines: usize,
    /// The generated files' paths, in path order, so the pre-start check names them.
    pub generated_paths: Vec<String>,
    /// Hand-written files of a type the inspection does not read yet, in path order: not read,
    /// billed or searched for secrets, and named by the pre-start check.
    pub not_inspected_paths: Vec<String>,
}

/// Read each source the way the scan does and count what the AI tier will be given. A source
/// that cannot be read is NOT skipped: it comes back with `read_error` set and nothing counted,
/// so the caller can refuse or say that repo is missing from the estimate.
pub async fn gather_scan_facts(
    sources: &[(String, std::path::PathBuf)],
    selected: &[SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
    scope: &crate::onboard::scope::ScanScope,
) -> Vec<RepoScanFacts> {
    let scan_rules = crate::onboard::inspectable_rules_for_scan(selected, corpus);
    let mut out = Vec::new();
    for (spec, dir) in sources {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        // The walk of a missing folder is an empty walk, not an error; without this check a repo
        // that is not there would read as an empty (free) one.
        if !dir.is_dir() {
            out.push(unreadable(
                spec,
                &format!("{} does not exist or is not a folder", dir.display()),
            ));
            continue;
        }
        let dir = dir.clone();
        let repo_scope = scope.for_repo(spec);
        let read = tokio::task::spawn_blocking(move || {
            crate::onboard::files::read_local_repo_files(&dir, &repo_scope).map(|extracted| {
                let generated_lines =
                    crate::onboard::count::non_blank_lines_in_files(&dir, &extracted.generated);
                (extracted, generated_lines)
            })
        })
        .await;
        let (extracted, generated_lines) = match read {
            Ok(Ok(read)) => read,
            Ok(Err(e)) => {
                out.push(unreadable(spec, &e.to_string()));
                continue;
            }
            Err(e) => {
                out.push(unreadable(spec, &e.to_string()));
                continue;
            }
        };
        let semantic = crate::onboard::semantic_rule_ids_for_repo_public(&scan_rules, spec);
        let multi_option_rules = corpus
            .map(|c| {
                crate::onboard::audit::build_rule_alternatives(
                    c,
                    semantic.iter().map(|(id, _)| id.as_str()),
                    &std::collections::HashMap::new(),
                )
                .len()
            })
            .unwrap_or(0);
        let code_chars = extracted.files.iter().map(|(_, c)| c.len()).sum();
        let over_limit = extracted.over_limit_files();
        let matched_patterns = extracted.matched_patterns.clone();
        let generated_files = extracted.generated.len();
        let generated_paths = extracted.generated.clone();
        let not_inspected_paths = extracted.not_inspected.clone();
        out.push(RepoScanFacts {
            repo: spec.to_string(),
            files: extracted.files,
            code_chars,
            semantic,
            multi_option_rules,
            read_error: None,
            over_limit,
            matched_patterns,
            generated_files,
            generated_lines,
            generated_paths,
            not_inspected_paths,
        });
    }
    out
}

/// The facts of a repo that could not be read: nothing counted, the reason kept.
fn unreadable(spec: &str, why: &str) -> RepoScanFacts {
    RepoScanFacts {
        repo: spec.to_string(),
        files: Vec::new(),
        code_chars: 0,
        semantic: Vec::new(),
        multi_option_rules: 0,
        read_error: Some(why.to_string()),
        over_limit: Vec::new(),
        matched_patterns: Vec::new(),
        generated_files: 0,
        generated_lines: 0,
        generated_paths: Vec::new(),
        not_inspected_paths: Vec::new(),
    }
}

/// One repo's size as the cockpit's estimate needs it: what the scan will read from it, or why
/// it cannot be read. The wire form of [`RepoScanFacts`] without the file bodies.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RepoSizeWire {
    pub repo: String,
    pub files: usize,
    pub code_chars: usize,
    /// Set when the repo could not be read (no folder, not a git clone, read failure).
    #[serde(default)]
    pub error: Option<String>,
    /// The agent finder's figures for this repo, so the cockpit shows the same numbers as the
    /// command line. `None` for a repo that could not be read.
    #[serde(default)]
    pub agent_finder: Option<AgentFinderWire>,
    /// Files over the read limit that are not excluded by agreement: the desktop lists them
    /// before an inspection starts and holds Start until each is excluded.
    #[serde(default)]
    pub over_limit: Vec<crate::onboard::files::OverLimitFile>,
    /// Files that declare themselves generated and the non-blank lines in them: not read, not
    /// billed, searched for secrets only. The desktop states both before an inspection starts.
    #[serde(default)]
    pub generated_files: usize,
    #[serde(default)]
    pub generated_lines: usize,
    /// The generated files' paths, in the form the exclusion list uses, so the desktop names
    /// them (capped there) as the command line does.
    #[serde(default)]
    pub generated_paths: Vec<String>,
    /// Hand-written files of a type the inspection does not read yet, in the form the exclusion
    /// list uses: not read, billed or searched for secrets. The desktop names them (capped there)
    /// as the command line does.
    #[serde(default)]
    pub not_inspected_paths: Vec<String>,
    /// The read limit as a person reads it ("1 MB"), from the engine's own constant, so the
    /// desktop never states a number the engine does not apply.
    #[serde(default)]
    pub read_limit: String,
    /// The largest over-limit file the secret checks still search ("20 MB"), from the engine's own
    /// constant, so the desktop prints the same ceiling as the command line and Coverage.
    #[serde(default)]
    pub secret_search_limit: String,
    /// Patterns of the project's Excluded by agreement list that match no file in any repo. The
    /// desktop lists each with "Remove pattern" and holds Start until none is left.
    #[serde(default)]
    pub unmatched_patterns: Vec<String>,
    /// One entry per unmatched pattern: why it can never match ("node_modules is never read, so
    /// this pattern is not needed"), or empty.
    #[serde(default)]
    pub unmatched_reasons: Vec<String>,
    /// Patterns naming a project repo that was not read: they cannot be checked, they block a
    /// start, and they are never offered for removal. Reasons in `unchecked_reasons`.
    #[serde(default)]
    pub unchecked_patterns: Vec<String>,
    #[serde(default)]
    pub unchecked_reasons: Vec<String>,
    /// Patterns that match only files never read anyway, with the reason in `warned_reasons`: a
    /// warning, never a block; the desktop offers Remove pattern (removing changes nothing).
    #[serde(default)]
    pub warned_patterns: Vec<String>,
    #[serde(default)]
    pub warned_reasons: Vec<String>,
}

/// The agent finder's pre-flight figures for one repo (the same function the command line
/// prints from: `camerata_ui_core::scan::estimate_agent_finder`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentFinderWire {
    pub pieces: usize,
    pub lines: usize,
    pub joints_calls: usize,
    pub expected_usd: f64,
    pub low_usd: f64,
    pub high_usd: f64,
}

/// The request body of `POST /api/onboard/preflight`.
#[derive(serde::Deserialize)]
pub(crate) struct PreflightReq {
    #[serde(default)]
    pub repos: Vec<String>,
}

/// One [`RepoSizeWire`] per requested repo, in request order. A requested repo that did not
/// resolve to a local clone (`unresolved`, `LocalSources::unresolved` from `resolve_local_sources`,
/// the one place that decides a repo has no usable folder) or could not
/// be read is a row with an `error`, never absent: an estimate that silently drops a repo prices
/// a smaller job than the scan will run.
pub fn repo_sizes(
    requested: &[String],
    facts: &[RepoScanFacts],
    unresolved: &[(String, String)],
) -> Vec<RepoSizeWire> {
    requested
        .iter()
        .map(|spec| spec.trim())
        .filter(|spec| !spec.is_empty())
        .map(|spec| match facts.iter().find(|f| f.repo == spec) {
            Some(f) => RepoSizeWire {
                repo: spec.to_string(),
                files: f.files.len(),
                code_chars: f.code_chars,
                error: f.read_error.clone(),
                agent_finder: f.read_error.is_none().then(|| agent_finder_wire(f)),
                over_limit: f
                    .over_limit
                    .iter()
                    .map(|o| crate::onboard::files::OverLimitFile {
                        display: crate::onboard::scope::pattern_form(requested, spec, &o.path),
                        ..o.clone()
                    })
                    .collect(),
                generated_files: f.generated_files,
                generated_lines: f.generated_lines,
                generated_paths: f
                    .generated_paths
                    .iter()
                    .map(|p| crate::onboard::scope::pattern_form(requested, spec, p))
                    .collect(),
                not_inspected_paths: f
                    .not_inspected_paths
                    .iter()
                    .map(|p| crate::onboard::scope::pattern_form(requested, spec, p))
                    .collect(),
                read_limit: crate::onboard::files::max_file_label(),
                secret_search_limit: crate::onboard::files::secret_scan_label(),
                unmatched_patterns: Vec::new(),
                unmatched_reasons: Vec::new(),
                unchecked_patterns: Vec::new(),
                unchecked_reasons: Vec::new(),
                warned_patterns: Vec::new(),
                warned_reasons: Vec::new(),
            },
            None => RepoSizeWire {
                repo: spec.to_string(),
                files: 0,
                code_chars: 0,
                error: Some(
                    unresolved
                        .iter()
                        .find(|(name, _)| name == spec)
                        .map(|(_, why)| why.as_str())
                        .unwrap_or("no local folder is set for this repo")
                        .to_string(),
                ),
                agent_finder: None,
                over_limit: Vec::new(),
                generated_files: 0,
                generated_lines: 0,
                generated_paths: Vec::new(),
                not_inspected_paths: Vec::new(),
                read_limit: crate::onboard::files::max_file_label(),
                secret_search_limit: crate::onboard::files::secret_scan_label(),
                unmatched_patterns: Vec::new(),
                unmatched_reasons: Vec::new(),
                unchecked_patterns: Vec::new(),
                unchecked_reasons: Vec::new(),
                warned_patterns: Vec::new(),
                warned_reasons: Vec::new(),
            },
        })
        .collect()
}

/// The pieces the agent finder would cut a repo into, with each piece's line count: build 1's
/// planner over the same model-facing (redacted) text the room is built from.
pub fn agent_finder_pieces(
    facts: &RepoScanFacts,
    force_pieces: Option<usize>,
) -> Vec<(crate::ai_audit::agent_finder::Piece, usize)> {
    let files = crate::onboard::redact::files_for_model(&facts.files);
    let units = crate::ai_audit::agent_finder::pieces::units_for(&facts.repo, &files);
    let lines: std::collections::HashMap<&str, usize> = files
        .iter()
        .map(|(p, text)| (p.as_str(), text.lines().count()))
        .collect();
    crate::ai_audit::agent_finder::pieces::plan_pieces(
        &units,
        &files,
        crate::ai_audit::agent_finder::pieces::TARGET_BYTES,
        force_pieces,
    )
    .into_iter()
    .map(|piece| {
        let n = piece
            .files
            .iter()
            .map(|p| lines.get(p.as_str()).copied().unwrap_or(0))
            .sum();
        (piece, n)
    })
    .collect()
}

/// The agent finder's estimate for one repo (the finder stage only).
pub fn agent_finder_estimate(
    facts: &RepoScanFacts,
    force_pieces: Option<usize>,
) -> camerata_ui_core::scan::AgentFinderEstimate {
    let sizes: Vec<camerata_ui_core::scan::AgentFinderPieceSize> =
        agent_finder_pieces(facts, force_pieces)
            .into_iter()
            .map(
                |(piece, lines)| camerata_ui_core::scan::AgentFinderPieceSize {
                    unit: piece.unit,
                    files: piece.files.len(),
                    lines,
                },
            )
            .collect();
    camerata_ui_core::scan::estimate_agent_finder(&sizes)
}

fn agent_finder_wire(facts: &RepoScanFacts) -> AgentFinderWire {
    let e = agent_finder_estimate(facts, None);
    AgentFinderWire {
        pieces: e.pieces,
        lines: e.rows.iter().map(|r| r.lines).sum(),
        joints_calls: e.joints_calls,
        expected_usd: e.expected_usd,
        low_usd: e.low_usd,
        high_usd: e.high_usd,
    }
}

/// The patterns of `scope` that no walked file matched, judged only against repos that were READ
/// (a pattern naming a repo that was not read cannot be checked), in list order.
pub fn unmatched_of(
    scope: &crate::onboard::scope::ScanScope,
    facts: &[RepoScanFacts],
) -> crate::onboard::scope::PatternChecks {
    let matched: Vec<bool> = (0..scope.patterns().len())
        .map(|i| facts.iter().any(|f| f.matched_patterns.contains(&i)))
        .collect();
    let read_ok: Vec<String> = facts
        .iter()
        .filter(|f| f.read_error.is_none())
        .map(|f| f.repo.clone())
        .collect();
    let all: Vec<String> = facts.iter().map(|f| f.repo.clone()).collect();
    crate::onboard::scope::classify_patterns(scope, &all, &read_ok, &matched)
}

/// `POST /api/onboard/preflight`: what each repo would be fed to the scan, for the pre-audit cost
/// estimate. Reads the same files the scan reads (the scan chunks and calibrates PER REPO, so the
/// estimate has to be built per repo too). Makes no model call and reads no rule.
pub(crate) async fn onboard_preflight(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    axum::Json(req): axum::Json<PreflightReq>,
) -> axum::Json<Vec<RepoSizeWire>> {
    let local = crate::resolve_local_sources(&state, &req.repos);
    // An exclusion list that cannot be used reads as an error on every repo: never as "nothing
    // is excluded", which would size and bill files the agreement leaves out.
    let scope = match crate::onboard::scope::ScanScope::of_project(state.projects.active().as_ref())
    {
        Ok(scope) => scope,
        Err(why) => {
            let facts: Vec<RepoScanFacts> = local
                .sources
                .iter()
                .map(|(spec, _)| unreadable(spec, &why))
                .collect();
            return axum::Json(repo_sizes(&req.repos, &facts, &local.unresolved));
        }
    };
    let facts = gather_scan_facts(&local.sources, &[], None, &scope).await;
    let mut sizes = repo_sizes(&req.repos, &facts, &local.unresolved);
    let checks = unmatched_of(&scope, &facts);
    let notes = crate::onboard::scope::unmatched_notes(&checks.unmatched, scope.repos());
    for row in &mut sizes {
        row.unmatched_reasons = notes.clone();
        row.unmatched_patterns = checks.unmatched.clone();
        row.unchecked_patterns = checks.unchecked.iter().map(|(p, _)| p.clone()).collect();
        row.unchecked_reasons = checks.unchecked.iter().map(|(_, r)| r.clone()).collect();
        row.warned_patterns = checks.warned.iter().map(|(p, _)| p.clone()).collect();
        row.warned_reasons = checks.warned.iter().map(|(_, r)| r.clone()).collect();
    }
    axum::Json(sizes)
}

/// The exact input size of one representative scan pass, from `count_tokens` (free, API backend
/// only), and the number of passes the main scan will make. `None` when there is nothing to
/// sample or the count was unavailable (no key, network error): the estimate then stands on its
/// heuristic alone and says so.
pub struct ExactSample {
    pub input_tokens: u64,
    pub passes: usize,
}

/// [`ExactSample`] for the first repo that has files, through the batch path's own sample
/// builder ([`representative_scan_sample`] and `exact_input_token_count`).
pub async fn exact_sample_tokens(
    llm: &crate::llm::Llm,
    facts: &[RepoScanFacts],
    mode: ScanMode,
    model: Option<&str>,
    efficiency: crate::efficiency::EfficiencyConfig,
) -> Option<ExactSample> {
    let repo = facts.iter().find(|f| !f.files.is_empty())?;
    let sample =
        representative_scan_sample(&repo.repo, &repo.files, &repo.semantic, mode, efficiency)?;
    let model = crate::ai_audit::resolve_audit_model(model);
    let input_tokens = crate::llm::exact_input_token_count(llm, &sample.request, &model).await?;
    Some(ExactSample {
        input_tokens,
        passes: sample.passes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// M1: two repos each with `src/index.ts`, one unreadable. Both come back; the missing one
    /// carries its reason and counts nothing. Before, the unreadable repo was dropped and the
    /// estimate priced a smaller job without saying so.
    #[tokio::test]
    async fn an_unreadable_repo_is_reported_not_skipped() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "src/index.ts", "export const a = 1;\n");
        let missing = a.path().join("does-not-exist");
        let sources = vec![
            ("acme/a".to_string(), a.path().to_path_buf()),
            ("acme/b".to_string(), missing),
        ];
        let facts = gather_scan_facts(
            &sources,
            &[],
            None,
            &crate::onboard::scope::ScanScope::none(),
        )
        .await;
        assert_eq!(facts.len(), 2, "no repo may be dropped");
        assert_eq!(facts[0].repo, "acme/a");
        assert!(facts[0].read_error.is_none());
        assert!(facts[0].code_chars > 0);
        assert_eq!(facts[1].repo, "acme/b");
        assert!(facts[1].read_error.is_some(), "the reason is kept");
        assert_eq!(facts[1].code_chars, 0);
        assert!(facts[1].files.is_empty());
    }

    #[tokio::test]
    async fn the_same_path_in_two_repos_is_counted_in_each() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write(a.path(), "src/index.ts", "export const a = 1;\n");
        write(b.path(), "src/index.ts", "export const b = 22222222;\n");
        let sources = vec![
            ("acme/a".to_string(), a.path().to_path_buf()),
            ("acme/b".to_string(), b.path().to_path_buf()),
        ];
        let facts = gather_scan_facts(
            &sources,
            &[],
            None,
            &crate::onboard::scope::ScanScope::none(),
        )
        .await;
        let sizes = repo_sizes(&["acme/a".into(), "acme/b".into()], &facts, &[]);
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes[0].files, 1);
        assert_eq!(sizes[1].files, 1);
        assert!(
            sizes[1].code_chars > sizes[0].code_chars,
            "counted per repo, not merged"
        );
        assert!(sizes.iter().all(|s| s.error.is_none()));
    }

    /// A repo that never resolved to a folder is a row with the reason from the resolver.
    #[test]
    fn a_repo_with_no_local_folder_is_a_row_with_its_reason() {
        let sizes = repo_sizes(
            &["acme/a".into(), " ".into(), "acme/gone".into()],
            &[],
            &[(
                "acme/gone".to_string(),
                "no local folder is set".to_string(),
            )],
        );
        let names: Vec<&str> = sizes.iter().map(|s| s.repo.as_str()).collect();
        assert_eq!(
            names,
            ["acme/a", "acme/gone"],
            "blank entries are not repos"
        );
        assert_eq!(
            sizes[1].error.as_deref(),
            Some("no local folder is set"),
            "the reason is the resolver's own, with no prefix to strip"
        );
        assert!(
            sizes[0].error.is_some(),
            "an unlisted repo is still reported"
        );
    }

    /// Item 31 step 6: the wire carries every file over the read limit, with its size and the
    /// limit's own words, and a file excluded by agreement is not on it.
    #[tokio::test]
    async fn the_wire_lists_files_over_the_read_limit_until_they_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn a() {}\n");
        write(
            dir.path(),
            "seeds/data.js",
            &"x".repeat(crate::onboard::files::MAX_FILE_BYTES + 200),
        );
        let sources = vec![("acme/api".to_string(), dir.path().to_path_buf())];

        let facts = gather_scan_facts(
            &sources,
            &[],
            None,
            &crate::onboard::scope::ScanScope::none(),
        )
        .await;
        let sizes = repo_sizes(&["acme/api".into()], &facts, &[]);
        assert_eq!(
            sizes[0].over_limit,
            [crate::onboard::files::OverLimitFile {
                path: "seeds/data.js".to_string(),
                bytes: crate::onboard::files::MAX_FILE_BYTES as u64 + 200,
                display: "api/seeds/data.js".to_string(),
                reason: String::new(),
            }]
        );
        assert_eq!(sizes[0].read_limit, "1 MB");
        let wire = serde_json::to_value(&sizes[0]).unwrap();
        assert_eq!(wire["over_limit"][0]["path"], "seeds/data.js");
        assert_eq!(wire["read_limit"], "1 MB");

        let scope =
            crate::onboard::scope::ScanScope::from_patterns(&["api/seeds/data.js".to_string()])
                .unwrap();
        let facts = gather_scan_facts(&sources, &[], None, &scope).await;
        let sizes = repo_sizes(&["acme/api".into()], &facts, &[]);
        assert!(
            sizes[0].over_limit.is_empty(),
            "excluded by agreement, so nothing blocks"
        );
        assert_eq!(sizes[0].files, 1);
    }
}
