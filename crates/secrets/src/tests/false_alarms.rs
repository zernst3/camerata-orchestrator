//! False-alarm classes of the hardcoded-secret rule: each shape stays quiet while the real shape
//! beside it still fires. Every value is synthetic and built at run time.

use crate::fixtures::body;
use crate::match_lines_for_rule;

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn lines(text: &str) -> Vec<usize> {
    match_lines_for_rule(text, "SEC-NO-HARDCODED-SECRETS-1", true)
}

#[test]
fn a_qualified_label_name_under_a_bare_key_is_a_name() {
    let quiet =
        "matchExpressions:\n  - key: nodes.cloudhost.internal/worker-pool\n    operator: In\n";
    assert!(lines(quiet).is_empty());
    let opaque = body(32, ALNUM, 11);
    assert_eq!(lines(&format!("settings:\n  - key: {opaque}\n")), vec![2]);
    assert_eq!(
        lines("db_password: nodes.cloudhost.internal/worker-pool\n"),
        vec![1]
    );
}
