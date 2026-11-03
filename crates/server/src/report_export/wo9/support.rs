//! Builders the WO9 export tests share: a small sealed report, decisions of every shape and an
//! edit. Home paths in fixtures use /Users/auditor/....

use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use camerata_api_types::edits::{Edit, EditField};

use crate::onboard::{Finding, ScanReport, SealedReport};

pub const NOW: &str = "2026-11-04T09:00:00Z";

/// One SQL-concatenation finding at `path:line`.
pub fn sql_row(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("format!(\"select {{}}\", x{line})"),
        detail: "Raw SQL \u{2014} The query is built from the request.".to_string(),
        ..Finding::default()
    }
}

/// A report of `n` SQL rows in `src/file_<i>.rs`, sealed.
pub fn sealed_of(n: usize) -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = (0..n)
        .map(|i| sql_row(&format!("src/file_{i}.rs"), 10 + i))
        .collect();
    report.seal()
}

pub fn decision(id: &str, key: &str, state: DecisionState, reason: &str) -> Decision {
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "SECRET-REVIEWER".to_string(),
        decided_at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

/// A decision on the tree node `node_id` covering `keys`: the node record, then one per-site
/// record each, as the decision store writes it.
pub fn node_decision(
    id: &str,
    node_id: &str,
    keys: &[String],
    state: DecisionState,
    reason: &str,
) -> Vec<Decision> {
    let mut out = vec![Decision {
        scope: Scope::Node(node_id.to_string()),
        applies_to: keys.to_vec(),
        ..decision(id, node_id, state.clone(), reason)
    }];
    for (i, key) in keys.iter().enumerate() {
        out.push(Decision {
            id: format!("{id}.{i:05}"),
            inherited_from: Some(id.to_string()),
            ..decision("x", key, state.clone(), reason)
        });
    }
    out
}

pub fn edit_of(n: usize, f: &Finding, field: EditField, new: &str) -> Edit {
    Edit {
        id: format!("edit-{n:06}"),
        key: f.site_key.clone(),
        field,
        old: String::new(),
        new: new.to_string(),
        reason: "SECRET-REASON the wording was wrong".to_string(),
        by: "SECRET-REVIEWER".to_string(),
        at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}
