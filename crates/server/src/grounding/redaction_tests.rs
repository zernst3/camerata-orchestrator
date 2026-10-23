//! The governed-dev repo digest goes to a model, so it is redacted (docs/design/2026-10-23_secret-
//! redaction-and-release-gate.md, path P5).

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::*;

#[test]
fn render_repo_digest_is_redacted() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("README.md"),
        format!("# Demo\n\nSet it up with {}.\n", p.text),
    )
    .expect("write readme");
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").expect("write source");

    let digest = render_repo_digest("acme/demo", dir.path()).expect("a digest");
    assert!(digest.contains("Demo"), "the digest still carries the doc");
    assert!(
        !digest.contains(&p.value),
        "the digest carried a credential value"
    );
    assert!(
        digest.contains("sk_live_\u{2026}"),
        "the redacted form stands in"
    );
}
