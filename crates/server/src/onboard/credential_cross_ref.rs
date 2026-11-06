//! A credential flagged in more than one service of a project.
//!
//! The same secret in two repos is two findings, on purpose: each service's owner has to rotate
//! it, so the rows stay separate and each is counted. What this module adds is a display-only
//! note on each of those rows, "the same credential name is also flagged in <repo>", so a
//! reader of one row learns the other exists. It never merges, drops or re-rates a row.
//!
//! The link is the credential's name (`STRIPE_SECRET_KEY`), the identity the merge pass already
//! uses within a repo ([`crate::ai_audit::root_cause_credential_identities`]). A secret's value
//! is redacted before it reaches a report, so the name is the one stable thing two rows share.

use std::collections::{BTreeMap, BTreeSet};

use super::Finding;

/// Whether `f` is about a leaked credential: the server classed it `secret-exposure`, or its
/// rule id names a secret, a token, a key or a password. Mirrors the cockpit's own test
/// (`FindingView::is_secret_exposure`), so the two never disagree about which rows these are.
pub fn is_secret_exposure(f: &Finding) -> bool {
    if f.category.as_deref() == Some("secret-exposure") {
        return true;
    }
    let id = f.rule_id.to_ascii_uppercase();
    let words: Vec<&str> = id.split('-').collect();
    let word_starts = |t: &str| words.iter().any(|w| w.starts_with(t));
    word_starts("SECRET")
        || word_starts("HARDCODED")
        || word_starts("CREDENTIAL")
        || word_starts("APIKEY")
        || word_starts("PASSWORD")
        || word_starts("TOKEN")
        || ["API-KEY", "PRIVATE-KEY", "VENDOR-TOKEN", "SUPABASE-KEY"]
            .iter()
            .any(|run| {
                let run: Vec<&str> = run.split('-').collect();
                words.windows(run.len()).any(|w| w == run.as_slice())
            })
}

/// The repos each credential name is flagged in, over a report's findings.
#[derive(Debug, Default)]
pub struct CredentialIndex {
    repos_by_name: BTreeMap<String, BTreeSet<String>>,
}

impl CredentialIndex {
    pub fn new(findings: &[Finding]) -> Self {
        Self::from_findings(findings)
    }

    /// The index over any set of rows: a report builds it over the rows it carries, so a row the
    /// reviewer dismissed as a false positive is never what a note points at.
    pub fn from_findings<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Self {
        let mut repos_by_name: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for f in findings.into_iter().filter(|f| is_secret_exposure(f)) {
            for name in crate::ai_audit::root_cause_credential_identities(f) {
                repos_by_name
                    .entry(name)
                    .or_default()
                    .insert(f.repo.clone());
            }
        }
        Self { repos_by_name }
    }

    /// The other repos that flag a credential `f` names, sorted. Empty for a finding that is
    /// not about a credential, names none, or is the only one to flag it.
    pub fn also_flagged_in(&self, f: &Finding) -> Vec<String> {
        if !is_secret_exposure(f) {
            return Vec::new();
        }
        let mut out: BTreeSet<String> = BTreeSet::new();
        for name in crate::ai_audit::root_cause_credential_identities(f) {
            if let Some(repos) = self.repos_by_name.get(&name) {
                out.extend(repos.iter().filter(|r| **r != f.repo).cloned());
            }
        }
        out.into_iter().collect()
    }
}

/// The finished note for a row: "Same credential name also flagged in acme/api, acme/worker."
/// Empty when there is nothing to say.
pub fn note(repos: &[String]) -> String {
    if repos.is_empty() {
        String::new()
    } else {
        format!("Same credential name also flagged in {}.", repos.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(repo: &str, name: &str) -> Finding {
        Finding {
            repo: repo.to_string(),
            path: ".env".to_string(),
            line: 1,
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            severity: "high".to_string(),
            snippet: format!("{name}=[redacted]"),
            detail: format!("{name} is committed"),
            ..Finding::default()
        }
    }

    #[test]
    fn the_same_credential_name_in_two_repos_is_noted_on_each_row() {
        let findings = vec![
            secret("acme/web", "STRIPE_SECRET_KEY"),
            secret("acme/api", "STRIPE_SECRET_KEY"),
            secret("acme/worker", "OTHER_SECRET_KEY"),
        ];
        let index = CredentialIndex::new(&findings);
        assert_eq!(index.also_flagged_in(&findings[0]), vec!["acme/api"]);
        assert_eq!(index.also_flagged_in(&findings[1]), vec!["acme/web"]);
        // A different credential name is a different credential.
        assert!(index.also_flagged_in(&findings[2]).is_empty());
        // The rows stay separate: nothing here merges or drops one.
        assert_eq!(findings.len(), 3);
    }

    #[test]
    fn a_row_that_is_not_about_a_credential_gets_no_note() {
        let mut other = secret("acme/api", "STRIPE_SECRET_KEY");
        other.rule_id = "ARCH-PAGINATION-1".to_string();
        let findings = vec![secret("acme/web", "STRIPE_SECRET_KEY"), other.clone()];
        let index = CredentialIndex::new(&findings);
        assert!(index.also_flagged_in(&other).is_empty());
        // The only repo to flag the name gets no note either.
        assert!(index.also_flagged_in(&findings[0]).is_empty());
    }

    #[test]
    fn the_note_reads_as_one_sentence() {
        assert_eq!(note(&[]), "");
        assert_eq!(
            note(&["acme/api".to_string(), "acme/worker".to_string()]),
            "Same credential name also flagged in acme/api, acme/worker."
        );
    }
}
