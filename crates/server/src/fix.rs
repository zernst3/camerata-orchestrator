//! Fix verification — the differentiator's proof.
//!
//! Camerata's claim that nobody else can make is "it fixes the problem AND proves the
//! fix didn't create a new one." A remediation run already goes through the same gate
//! and layer-2 checks as any dev task (so a fix that itself violates a rule is denied
//! before it lands, and a fix that breaks the build/tests bounces). This module adds the
//! last check: compare the findings BEFORE and AFTER the fix by violation identity
//! (rule plus content fingerprint), so a fix that silently leaves the violation or
//! introduces a NEW one is caught, not assumed away.

use std::collections::HashSet;

use serde::Serialize;

use crate::suppression::{fingerprint, FindingRef};

/// The result of comparing pre-fix and post-fix findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixOutcome {
    /// Violations present before and gone after — the fix worked on these.
    pub resolved: Vec<String>,
    /// Violations still present after — the fix did NOT resolve these.
    pub remaining: Vec<String>,
    /// Violations present only after — regressions / new debt the fix introduced.
    pub introduced: Vec<String>,
}

impl FixOutcome {
    /// A fix is clean when it introduced no new violations. (Resolving everything is the
    /// goal; introducing nothing is the non-negotiable.)
    pub fn clean(&self) -> bool {
        self.introduced.is_empty()
    }

    /// A fix fully succeeded: it introduced nothing and left nothing of what it targeted.
    pub fn complete(&self) -> bool {
        self.introduced.is_empty() && self.remaining.is_empty()
    }
}

/// Identity of a violation: its rule + the content fingerprint + path. Two findings are
/// "the same violation" iff these match — so moving lines around doesn't look like a
/// fix, and editing the offending code does.
fn key(f: &FindingRef) -> String {
    format!("{}@{}", fingerprint(&f.rule_id, &f.snippet), f.path)
}

/// Compare the findings before and after a remediation run.
pub fn verify(before: &[FindingRef], after: &[FindingRef]) -> FixOutcome {
    let before_set: HashSet<String> = before.iter().map(key).collect();
    let after_set: HashSet<String> = after.iter().map(key).collect();
    FixOutcome {
        resolved: before_set.difference(&after_set).cloned().collect(),
        remaining: before_set.intersection(&after_set).cloned().collect(),
        introduced: after_set.difference(&before_set).cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(rule: &str, path: &str, snip: &str) -> FindingRef {
        FindingRef {
            rule_id: rule.to_string(),
            path: path.to_string(),
            line: 0,
            snippet: snip.to_string(),
        }
    }

    #[test]
    fn a_clean_complete_fix() {
        let before = vec![f("SEC-X", "a.rs", "bad()")];
        let after: Vec<FindingRef> = vec![]; // the violation is gone, nothing new
        let o = verify(&before, &after);
        assert_eq!(o.resolved.len(), 1);
        assert!(o.remaining.is_empty());
        assert!(o.introduced.is_empty());
        assert!(o.clean());
        assert!(o.complete());
    }

    #[test]
    fn a_fix_that_introduces_new_debt_is_not_clean() {
        let before = vec![f("SEC-X", "a.rs", "bad()")];
        // Resolved the target but introduced a new violation elsewhere.
        let after = vec![f("ARCH-Y", "a.rs", "new_smell()")];
        let o = verify(&before, &after);
        assert_eq!(o.resolved.len(), 1);
        assert_eq!(o.introduced.len(), 1);
        assert!(!o.clean(), "a fix that creates new debt must NOT pass");
    }

    #[test]
    fn a_fix_that_did_nothing_leaves_it_remaining() {
        let before = vec![f("SEC-X", "a.rs", "bad()")];
        let after = vec![f("SEC-X", "a.rs", "bad()")]; // unchanged
        let o = verify(&before, &after);
        assert!(o.resolved.is_empty());
        assert_eq!(o.remaining.len(), 1);
        assert!(o.clean(), "introduced nothing");
        assert!(!o.complete(), "but did not resolve the target");
    }
}
