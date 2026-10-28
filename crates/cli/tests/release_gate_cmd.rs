#![allow(clippy::unwrap_used)]
//! `camerata release-gate <zip>`: exit codes and the output, which must never carry a value.

use camerata::release_gate_cmd::{
    format_verdict, run_release_gate, ReleaseGateArgs, EXIT_BLOCKED, EXIT_GATE_UNAVAILABLE,
};
use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;
use camerata_server::{assemble_unreleased_zip, ManifestProvenance};

#[test]
fn an_unreadable_path_means_the_gate_could_not_run_and_is_not_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.zip");
    let err = run_release_gate(&ReleaseGateArgs { zip: missing }).expect_err("must not pass");
    assert!(err.contains("could not read"), "{err}");
    assert_eq!(
        EXIT_GATE_UNAVAILABLE, 2,
        "the unavailable code is 2, never 0"
    );

    let junk = dir.path().join("junk.zip");
    std::fs::write(&junk, b"this is not a zip").unwrap();
    assert!(run_release_gate(&ReleaseGateArgs { zip: junk }).is_err());
}

#[test]
fn a_zip_carrying_a_secret_exits_3_and_the_output_never_prints_the_value() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let readme = format!("README\nThe production key is {}\n", p.value);
    let zip = assemble_unreleased_zip(
        "acme-inspection",
        b"%PDF-1.7 stand-in",
        b"PK stand-in workbook",
        b"{\"findings\":[]}\n",
        &readme,
        &[],
        &ManifestProvenance::default(),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acme-inspection.zip");
    std::fs::write(&path, zip).unwrap();

    let verdict = run_release_gate(&ReleaseGateArgs { zip: path }).expect("the gate runs");
    assert_eq!(verdict.exit_code(), EXIT_BLOCKED);
    assert_eq!(EXIT_BLOCKED, 3);
    let shown = format_verdict(&verdict);
    assert!(shown.starts_with("EXPORT BLOCKED by the release gate for acme-inspection"));
    assert!(shown.contains("README.txt"), "{shown}");
    assert!(shown.contains("sk_live_\u{2026}"), "{shown}");
    assert!(
        !shown.contains(&p.value),
        "the verdict must never carry the value"
    );
}
