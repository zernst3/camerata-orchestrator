//! The L3 reviewer's diff is client code going to a model, so it is redacted (docs/design/2026-10-23_
//! secret-redaction-and-release-gate.md, path P5).

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::*;

#[test]
fn l3_prompt_diff_is_redacted() {
    let p = planted(SecretShape::GitHubToken);
    let diff = format!("+++ b/.env\n@@\n+{}\n", p.text);
    let input = L3ReviewInput {
        story: "Add the integration",
        rules_prose: "No secrets in the repo.",
        diff: &diff,
        model: "m",
    };
    let prompt = build_l3_prompt(&input);
    assert!(
        prompt.contains("+++ b/.env"),
        "the rest of the diff is untouched"
    );
    assert!(
        !prompt.contains(&p.value),
        "the L3 prompt carried a credential value"
    );
    assert!(prompt.contains("ghp_\u{2026}"));
}
