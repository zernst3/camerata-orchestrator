//! Reconcile a project's repos with the rule-bank: read what's ACTUALLY emitted in
//! each repo (its `.camerata/rules.json` gate config — the ground truth of what's
//! applied) and rehydrate each rule's FULL source by matching the id back to the
//! corpus (alternatives, context) or, for custom rules, the project.
//!
//! The emitted files are lossy (the directive only). The Rules screen needs the
//! source rule so the architect sees the alternatives + context, not just the
//! adopted line. The link is the rule id.

use serde::Serialize;

use crate::arm::GateRule;

/// One alternative of an applied rule (rehydrated from the corpus).
#[derive(Debug, Clone, Serialize)]
pub struct AppliedOption {
    pub id: String,
    pub label: String,
    pub directive: String,
}

/// One rule as it is ACTUALLY applied in a repo, rehydrated with its source.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedRule {
    /// The rule id.
    pub id: String,
    /// The repo it's applied in.
    pub repo: String,
    /// Title (from the corpus source, or the id when unknown).
    pub title: String,
    /// One-paragraph context (the corpus summary), empty for custom/unknown.
    pub summary: String,
    /// The chosen alternative option id recorded in the repo.
    pub chosen_option: Option<String>,
    /// The label of the chosen alternative (resolved from the corpus).
    pub chosen_label: Option<String>,
    /// All alternatives (from the corpus source) so the architect sees the choices.
    pub options: Vec<AppliedOption>,
    /// True for an architect-authored custom rule (`CUSTOM-*`) — its source is the
    /// project, not the corpus.
    pub is_custom: bool,
    /// True when the id resolved to a corpus rule. False = drift (applied in the
    /// repo but not found in the rule-bank).
    pub in_corpus: bool,
}

/// Read a repo's `.camerata/rules.json` gate config via GitHub. Returns an empty
/// vec when the repo has no gate config (not armed).
async fn read_gate_config(owner: &str, repo: &str, token: &str) -> anyhow::Result<Vec<GateRule>> {
    use base64::Engine as _;
    use camerata_worktracker::{HttpTransport, ReqwestTransport};

    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/.camerata/rules.json");
    let resp = transport.get(&url).await?;
    if resp.status == 404 {
        return Ok(Vec::new());
    }
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GET gate config for {owner}/{repo}: HTTP {} {}", resp.status, resp.body);
    }
    let v: serde_json::Value = serde_json::from_str(&resp.body)?;
    if v["encoding"].as_str() != Some("base64") {
        return Ok(Vec::new());
    }
    let Some(b64) = v["content"].as_str() else {
        return Ok(Vec::new());
    };
    let cleaned: String = b64.split_whitespace().collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(cleaned)?;
    let text = String::from_utf8(bytes)?;
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

/// Reconcile every repo in `repos`: read its applied gate config and rehydrate each
/// rule's source from the corpus (or mark it custom/drift). The token is required.
pub async fn reconcile_repos(repos: &[String], token: &str) -> Vec<AppliedRule> {
    // Load the corpus once (the rule-bank source).
    let corpus_path = std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH);
    let set = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(corpus_path).await.0)
    } else {
        None
    };

    let mut applied = Vec::new();
    for spec in repos {
        let Some((owner, repo)) = spec.split_once('/') else {
            continue;
        };
        let gate = match read_gate_config(owner, repo, token).await {
            Ok(g) => g,
            Err(_) => continue, // a repo we can't read is skipped, not fatal
        };
        for g in gate {
            let is_custom = g.id.starts_with("CUSTOM-");
            let corpus_rule = set.as_ref().and_then(|s| s.get_by_id(&g.id));
            let (title, summary, options, in_corpus) = match corpus_rule {
                Some(r) => (
                    r.title.clone(),
                    r.summary.clone(),
                    r.options
                        .iter()
                        .map(|o| AppliedOption {
                            id: o.id.clone(),
                            label: o.label.clone(),
                            directive: o.directive.clone(),
                        })
                        .collect(),
                    true,
                ),
                None => (g.id.clone(), String::new(), Vec::new(), false),
            };
            let chosen_label = g.option.as_ref().and_then(|oid| {
                options.iter().find(|o| &o.id == oid).map(|o| o.label.clone())
            });
            applied.push(AppliedRule {
                id: g.id.clone(),
                repo: spec.clone(),
                title,
                summary,
                chosen_option: g.option.clone(),
                chosen_label,
                options,
                is_custom,
                in_corpus,
            });
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_rule_serializes_options_and_custom_flag() {
        let r = AppliedRule {
            id: "CUSTOM-house".into(),
            repo: "me/api".into(),
            title: "CUSTOM-house".into(),
            summary: String::new(),
            chosen_option: None,
            chosen_label: None,
            options: vec![],
            is_custom: true,
            in_corpus: false,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"is_custom\":true"));
        assert!(j.contains("\"in_corpus\":false"));
    }
}
