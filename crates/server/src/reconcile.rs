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
    /// For a custom rule: its directive body (round-tripped via the gate config), so the
    /// architect sees it and reconcile can adopt it back into the project. None for corpus rules.
    #[serde(default)]
    pub body: Option<String>,
}

/// Rehydrate one applied gate entry into an [`AppliedRule`], matching its id back to the corpus
/// (alternatives + context) or marking it custom/drift. Shared by the GitHub and local readers.
fn applied_from_gate(
    set: Option<&camerata_rules::RuleSet>,
    spec: &str,
    g: GateRule,
) -> AppliedRule {
    let is_custom = g.id.starts_with("CUSTOM-");
    let corpus_rule = set.and_then(|s| s.get_by_id(&g.id));
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
        options
            .iter()
            .find(|o| &o.id == oid)
            .map(|o| o.label.clone())
    });
    AppliedRule {
        id: g.id.clone(),
        repo: spec.to_string(),
        title,
        summary,
        chosen_option: g.option.clone(),
        chosen_label,
        options,
        is_custom,
        in_corpus,
        body: g.body.clone(),
    }
}

/// Read a repo's `.camerata/rules.json` gate config via GitHub. Returns an empty
/// vec when the repo has no gate config (not armed).
async fn read_gate_config(
    owner: &str,
    repo: &str,
    token: &str,
    r#ref: Option<&str>,
) -> anyhow::Result<Vec<GateRule>> {
    use base64::Engine as _;
    use camerata_worktracker::{HttpTransport, ReqwestTransport};

    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let mut url =
        format!("https://api.github.com/repos/{owner}/{repo}/contents/.camerata/rules.json");
    if let Some(r) = r#ref.filter(|r| !r.trim().is_empty()) {
        url.push_str(&format!("?ref={r}"));
    }
    let resp = transport.get(&url).await?;
    if resp.status == 404 {
        return Ok(Vec::new());
    }
    if !(200..300).contains(&resp.status) {
        anyhow::bail!(
            "GET gate config for {owner}/{repo}: HTTP {} {}",
            resp.status,
            resp.body
        );
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
    let corpus_path = camerata_rules::corpus_path();
    let set = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };

    let mut applied = Vec::new();
    for spec in repos {
        let Some((owner, repo)) = spec.split_once('/') else {
            continue;
        };
        let gate = match read_gate_config(owner, repo, token, None).await {
            Ok(g) => g,
            Err(_) => continue, // a repo we can't read is skipped, not fatal
        };
        for g in gate {
            applied.push(applied_from_gate(set.as_ref(), spec, g));
        }
    }
    applied
}

/// Read a repo's `.camerata/rules.json` from its LOCAL working copy. Returns empty when absent
/// or unreadable (a repo with no governance files is simply not reconciled, not an error).
fn read_gate_config_local(dir: &std::path::Path) -> Vec<GateRule> {
    let path = dir.join(".camerata/rules.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Reconcile using LOCAL working copies first (where `emit-local` writes by default), falling
/// back to the GitHub governance branch (`camerata/onboard-governance`, where the push toggle
/// puts them) for repos without a local clone. `sources` pairs each `owner/repo` spec with its
/// resolved local dir (None when no local path is configured). This is the path the Rules page
/// uses — the old default-branch reader missed rules that live on the governance branch / locally.
pub async fn reconcile_repos_local(
    sources: &[(String, Option<std::path::PathBuf>)],
    token: &str,
) -> Vec<AppliedRule> {
    let corpus_path = camerata_rules::corpus_path();
    let set = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };

    let mut applied = Vec::new();
    for (spec, dir) in sources {
        // Local working copy first; then the GitHub governance branch when a token is available.
        let mut gate = dir
            .as_deref()
            .map(read_gate_config_local)
            .unwrap_or_default();
        if gate.is_empty() {
            if let Some((owner, repo)) = spec.split_once('/') {
                if !token.is_empty() {
                    gate = read_gate_config(owner, repo, token, Some(crate::arm::ARM_BRANCH))
                        .await
                        .unwrap_or_default();
                }
            }
        }
        for g in gate {
            applied.push(applied_from_gate(set.as_ref(), spec, g));
        }
    }
    applied
}

/// Convert reconciled [`AppliedRule`]s into project ruleset pieces so project state can mirror
/// what is in the repos: base [`RuleSelection`]s (grouped by rule id, collecting the repos and
/// the chosen option) and custom [`CustomRule`]s (rebuilt from the round-tripped body + scoping).
/// `cross_repo` and `process` rules are project-level (not in repo gate configs) and are NOT
/// touched by reconcile.
pub fn adopt_from_applied(
    applied: &[AppliedRule],
) -> (
    Vec<crate::project::RuleSelection>,
    Vec<crate::project::CustomRule>,
) {
    use crate::project::{CustomRule, RuleSelection};
    use std::collections::BTreeMap;

    let mut base: BTreeMap<String, RuleSelection> = BTreeMap::new();
    let mut custom: BTreeMap<String, CustomRule> = BTreeMap::new();
    for a in applied {
        if a.is_custom {
            let name = a.id.strip_prefix("CUSTOM-").unwrap_or(&a.id).to_string();
            let entry = custom.entry(name.clone()).or_insert_with(|| CustomRule {
                name,
                body: a.body.clone().unwrap_or_default(),
                domain: String::new(),
                repos: Vec::new(),
            });
            if entry.body.is_empty() {
                if let Some(b) = &a.body {
                    entry.body = b.clone();
                }
            }
            if !entry.repos.contains(&a.repo) {
                entry.repos.push(a.repo.clone());
            }
        } else {
            let entry = base.entry(a.id.clone()).or_insert_with(|| RuleSelection {
                rule_id: a.id.clone(),
                chosen_option: a.chosen_option.clone(),
                repos: Vec::new(),
            });
            if entry.chosen_option.is_none() {
                entry.chosen_option = a.chosen_option.clone();
            }
            if !entry.repos.contains(&a.repo) {
                entry.repos.push(a.repo.clone());
            }
        }
    }
    (base.into_values().collect(), custom.into_values().collect())
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
            body: Some("Prefer X.".into()),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"is_custom\":true"));
        assert!(j.contains("\"in_corpus\":false"));
    }

    #[test]
    fn adopt_groups_base_repos_and_rebuilds_custom_from_body() {
        let applied = vec![
            AppliedRule {
                id: "RULE-A".into(),
                repo: "me/api".into(),
                title: "A".into(),
                summary: String::new(),
                chosen_option: Some("opt-1".into()),
                chosen_label: None,
                options: vec![],
                is_custom: false,
                in_corpus: true,
                body: None,
            },
            AppliedRule {
                id: "RULE-A".into(),
                repo: "me/web".into(),
                title: "A".into(),
                summary: String::new(),
                chosen_option: Some("opt-1".into()),
                chosen_label: None,
                options: vec![],
                is_custom: false,
                in_corpus: true,
                body: None,
            },
            AppliedRule {
                id: "CUSTOM-house".into(),
                repo: "me/api".into(),
                title: "CUSTOM-house".into(),
                summary: String::new(),
                chosen_option: None,
                chosen_label: None,
                options: vec![],
                is_custom: true,
                in_corpus: false,
                body: Some("Prefer X.".into()),
            },
        ];
        let (selections, custom) = adopt_from_applied(&applied);
        assert_eq!(selections.len(), 1, "RULE-A grouped across both repos");
        assert_eq!(selections[0].rule_id, "RULE-A");
        assert_eq!(selections[0].chosen_option.as_deref(), Some("opt-1"));
        assert_eq!(selections[0].repos, vec!["me/api", "me/web"]);
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].name, "house");
        assert_eq!(custom[0].body, "Prefer X.");
        assert_eq!(custom[0].repos, vec!["me/api"]);
    }
}
