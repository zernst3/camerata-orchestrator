//! False-alarm classes of the hardcoded-secret rule: each shape stays quiet while the real shape
//! beside it still fires. Every value is synthetic and built at run time.

use crate::fixtures::body;
use crate::{match_lines_for_rule, match_lines_for_rule_in_file};

const HEX: &str = "0123456789abcdef";
const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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

#[test]
fn a_module_path_in_an_import_or_require_is_not_a_value() {
    for quiet in [
        "const TokenService = require('../services/TokenService');",
        "import TokenListPanel from './components/TokenListPanel';",
        r#"import { fetchApiKey } from "../../lib/keys/fetchApiKey";"#,
    ] {
        assert!(lines(quiet).is_empty(), "{quiet}");
    }
    let opaque = body(32, ALNUM, 12);
    assert_eq!(lines(&format!("const TokenService = '{opaque}';")), vec![1]);
    assert_eq!(
        lines(&format!("const apiKey = require('{opaque}');")),
        vec![1]
    );
}

fn uuid() -> String {
    [
        body(8, HEX, 21),
        body(4, HEX, 22),
        body(4, HEX, 23),
        body(4, HEX, 24),
        body(12, HEX, 25),
    ]
    .join("-")
}

#[test]
fn a_uuid_on_an_id_named_field_is_an_entity_id() {
    let id = uuid();
    // The row stays (the audit holds it at low for a reviewer); it is not removed.
    assert_eq!(lines(&format!("example:\n  token_id: {id}\n")), vec![2]);
    assert_eq!(
        lines(&format!("const row = {{ tokenId: '{id}' }};")),
        vec![1]
    );
    assert_eq!(lines(&format!("api_token: {id}\n")), vec![1]);
    assert_eq!(lines(&format!("client_secret_id: {id}\n")), vec![1]);
    let opaque = body(36, ALNUM, 16);
    assert_eq!(lines(&format!("token_id: {opaque}\n")), vec![1]);
}

#[test]
fn sealed_secret_ciphertext_is_not_a_hardcoded_value() {
    let blob = body(700, B64, 14);
    let head = "apiVersion: bitnami.com/v1alpha1\nkind: SealedSecret\nspec:\n";
    let sealed =
        format!("{head}  encryptedData:\n    password: {blob}\n  template:\n    type: Opaque\n");
    assert!(lines(&sealed).is_empty());
    let short = body(32, ALNUM, 15);
    // A short value under encryptedData is not ciphertext.
    assert_eq!(
        lines(&format!("{head}  encryptedData:\n    password: {short}\n")),
        vec![5]
    );
    // A value under template is plain text.
    assert_eq!(
        lines(&format!(
            "{head}  template:\n    data:\n      password: {short}\n"
        )),
        vec![6]
    );
    // The same key in a plain Secret.
    assert_eq!(
        lines(&format!("kind: Secret\nstringData:\n  password: {blob}\n")),
        vec![3]
    );
    // A sealed secret does not clear a plain Secret in the next document.
    let stream = format!("{head}  encryptedData:\n    password: {blob}\n---\nkind: Secret\nstringData:\n  password: {blob}\n");
    assert_eq!(lines(&stream), vec![9]);
}

#[test]
fn an_environment_variable_name_is_not_a_row() {
    // On a key or token identifier, an all-capitals name is a variable name.
    let quiet = "const deployToken = pick({ prod: 'CLOUD_PRODUCTION_DEPLOY_TOKEN' });";
    assert!(lines(quiet).is_empty());
    assert!(lines("api_key: \"CLOUD_PRODUCTION_DEPLOY_KEY\"").is_empty());
    // On a password identifier the same shape is still a row.
    assert_eq!(
        lines("password = \"CLOUD_PRODUCTION_DEPLOY_TOKEN\""),
        vec![1]
    );
}

#[test]
fn a_terraform_heredoc_keeps_the_unquoted_form() {
    let opaque = body(32, ALNUM, 31);
    let tf =
        format!("resource \"vm\" \"web\" {{\n  user_data = <<EOT\nDB_PASSWORD={opaque}\nEOT\n}}\n");
    assert_eq!(in_file("infra/main.tf", &tf), vec![3]);
    // A dotted reference on the right of the same key is still an expression.
    let reference = "resource \"vm\" \"web\" {\n  user_data = <<EOT\nDB_PASSWORD=random_password.web.result\nEOT\n}\n";
    assert!(in_file("infra/main.tf", reference).is_empty());
}

#[test]
fn an_all_caps_real_key_is_not_an_env_name() {
    let key = ["LIVE_9A8B7C6D5E4F3G2H1J0K", "9L8M"].concat();
    assert_eq!(lines(&format!("api_key: \"{key}\"")), vec![1]);
    // Two digits or fewer is a readable name.
    assert!(lines("api_key: \"CLOUD_PRODUCTION_DEPLOY_KEY_V2\"").is_empty());
}

#[test]
fn a_one_line_resource_block_with_a_password_still_fires() {
    let opaque = body(32, ALNUM, 32);
    let tf = format!("resource \"a\" \"b\" {{ password = \"{opaque}\" }}\n");
    assert_eq!(in_file("main.tf", &tf), vec![1]);
}

#[test]
fn a_base64_key_with_one_slash_and_no_dotted_domain_still_fires() {
    let opaque = format!("{}/{}", body(20, ALNUM, 33), body(20, ALNUM, 34));
    assert_eq!(lines(&format!("settings:\n  key: {opaque}\n")), vec![2]);
}

#[test]
fn a_word_ending_in_from_is_not_a_module_import() {
    let opaque = body(32, ALNUM, 35);
    assert_eq!(
        lines(&format!("token_fromage = \"{opaque}/{opaque}\"")),
        vec![1]
    );
    assert_eq!(
        lines(&format!("tokenimport \"{opaque}/{opaque}\"")),
        vec![1]
    );
    assert_eq!(lines(&format!("tokenfrom \"{opaque}/{opaque}\"")), vec![1]);
    assert!(lines("import TokenListPanel from './components/TokenListPanel';").is_empty());
}
