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
}

/// Read each source the way the scan does and count what the AI tier will be given. A source
/// that cannot be read is skipped (the scan reports that failure itself).
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
        let dir = dir.clone();
        let read =
            tokio::task::spawn_blocking(move || crate::onboard::files::read_local_repo_files(&dir))
                .await;
        let Ok(Ok(extracted)) = read else { continue };
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
        });
    }
    out
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
