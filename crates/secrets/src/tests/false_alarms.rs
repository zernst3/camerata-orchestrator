//! False-alarm classes of the hardcoded-secret rule: each shape stays quiet while the real shape
//! beside it still fires. Every value is synthetic and built at run time.

use crate::fixtures::body;
use crate::{match_lines_for_rule, match_lines_for_rule_in_file};

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn lines(text: &str) -> Vec<usize> {
    match_lines_for_rule(text, "SEC-NO-HARDCODED-SECRETS-1", true)
}

fn in_file(path: &str, text: &str) -> Vec<usize> {
    match_lines_for_rule_in_file(text, "SEC-NO-HARDCODED-SECRETS-1", path, true)
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

#[test]
fn hcl_references_and_block_labels_are_not_values() {
    // Built from fragments: the crate's source holds no literal the detector would flag.
    let label = ["reporting_reader_", "password"].concat();
    let tf = format!(
        "resource \"random_password\" \"{label}\" {{\n  length = 32\n}}\n\
         resource \"db_role\" \"reader\" {{\n  password = random_password.{label}.result\n}}\n\
         provider \"cloud\" {{\n  secret_key = var.CLOUD_SPACES_SECRET_KEY\n}}\n"
    );
    assert!(in_file("grants/main.tf", &tf).is_empty());
    let opaque = body(32, ALNUM, 13);
    let loud = format!("resource \"db_role\" \"reader\" {{\n  password = \"{opaque}\"\n}}\n");
    assert_eq!(in_file("grants/main.tf", &loud), vec![2]);
    assert_eq!(
        in_file(
            ".env",
            "DB_PASSWORD=random_password.reporting_reader.result\n"
        ),
        vec![1]
    );
}
