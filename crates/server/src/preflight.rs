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
}

/// Read each source the way the scan does and count what the AI tier will be given. A source
/// that cannot be read is NOT skipped: it comes back with `read_error` set and nothing counted,
/// so the caller can refuse or say that repo is missing from the estimate.
pub async fn gather_scan_facts(
    sources: &[(String, std::path::PathBuf)],
    selected: &[SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
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
        let read =
            tokio::task::spawn_blocking(move || crate::onboard::files::read_local_repo_files(&dir))
                .await;
        let extracted = match read {
            Ok(Ok(extracted)) => extracted,
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
        out.push(RepoScanFacts {
            repo: spec.to_string(),
            files: extracted.files,
            code_chars,
            semantic,
            multi_option_rules,
            read_error: None,
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
    }
}

/// One repo's size as the cockpit's estimate needs it: what the scan will read from it, or why
/// it cannot be read. The wire form of [`RepoScanFacts`] without the file bodies.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoSizeWire {
    pub repo: String,
    pub files: usize,
    pub code_chars: usize,
    /// Set when the repo could not be read (no folder, not a git clone, read failure).
    #[serde(default)]
    pub error: Option<String>,
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
            },
        })
        .collect()
}

/// `POST /api/onboard/preflight`: what each repo would be fed to the scan, for the pre-audit cost
/// estimate. Reads the same files the scan reads (the scan chunks and calibrates PER REPO, so the
/// estimate has to be built per repo too). Makes no model call and reads no rule.
pub(crate) async fn onboard_preflight(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    axum::Json(req): axum::Json<PreflightReq>,
) -> axum::Json<Vec<RepoSizeWire>> {
    let local = crate::resolve_local_sources(&state, &req.repos);
    let facts = gather_scan_facts(&local.sources, &[], None).await;
    axum::Json(repo_sizes(&req.repos, &facts, &local.unresolved))
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
        let facts = gather_scan_facts(&sources, &[], None).await;
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
        let facts = gather_scan_facts(&sources, &[], None).await;
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
}
