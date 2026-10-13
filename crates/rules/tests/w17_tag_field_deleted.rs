//! W17 item 1: the rule-level `tag` field is DELETED — from all 417 corpus TOMLs and from the
//! `RuleToml` schema, so the unknown-field gate now rejects any reappearance.
//!
//! ## Why it went
//!
//! W14 declared `tag` accepted-but-unconsumed (`RuleToml`, gotcha 8) so
//! `corpus_files_with_unknown_fields` could treat every OTHER unrecognized rule-level key as a
//! build failure without first rejecting the entire corpus. That was a staging decision, not an
//! endorsement. The owner ruled it dead weight on three measured grounds:
//!
//! - **Three values across 417 rules** — `"stack"` 388, `"universal"` 28, `"process"` 1. 93
//!   percent of the corpus carries one value, so the field distinguishes almost nothing.
//! - **It does not agree with the domain.** Compared against the folder-derived `Rule::domain`
//!   (the value that actually gates selection), it matched on only 21 of 417 rules.
//! - **The 28 `"universal"` rules are exactly the `testing/` and `universal/` folders**, which
//!   is already derivable from the path — so even the one informative split is redundant.
//!
//! ## Why `layer` is NOT deleted alongside it
//!
//! `layer` looks superficially identical (also on all 417, also read by nothing) and is not the
//! same shape: seven values, genuinely not folder-derivable, and duplicating no existing domain
//! constant. It is a plausible latent feature the owner has not ruled on. The deletion sweep must
//! therefore be PROVEN not to have taken it, which is what
//! `req_w17_layer_is_untouched_on_every_corpus_rule` is for — a 417-file mechanical edit that
//! removed one line per file is exactly the shape that takes the wrong line in a handful of them.

use camerata_rules::{corpus_files_with_unknown_fields, DEFAULT_CORPUS_PATH};
use std::collections::BTreeMap;

fn corpus_dir() -> Option<&'static std::path::Path> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    path.exists().then_some(path)
}

/// Every `*.toml` under the corpus root, as `(relative path, contents)`. Mirrors
/// `camerata_rules::collect_toml_paths`'s discovery (recursive, `*.toml` only) without depending
/// on a private function.
fn corpus_files(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", d.display()))
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "toml") {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
                let rel = path
                    .strip_prefix(dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, text));
            }
        }
    }
    out.sort();
    out
}

/// The rule-level (top-of-file, outside every `[table]`) value of `key` in a rule TOML, read
/// through a real TOML parse rather than a line regex so a key nested inside `[[sources]]` or a
/// `"""` block string can never be miscounted as rule-level.
fn rule_level_value(text: &str, key: &str) -> Option<String> {
    let value: toml::Value = text.parse().expect("every corpus file must parse as TOML");
    let table = value.as_table()?;
    Some(match table.get(key)? {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// W17 item 1: NO corpus rule carries a rule-level `tag` key.
///
/// FAILS BEFORE THE FIX: all 417 files carried one. Proven by `git stash push --
/// crates/rules/principles/` (restoring the pre-fix corpus) and re-running: 417 offenders.
///
/// This is the data half of the deletion. The SCHEMA half — that the field's removal from
/// `RuleToml` and `RULE_LEVEL_FIELDS` makes a reappearance a build failure rather than a silent
/// no-op — is `req_w17_gate_3_rejects_a_reappearing_tag_field` below. Both halves are needed: data
/// alone would let the next author re-add it unnoticed, and schema alone would fail the build
/// immediately.
#[test]
fn req_w17_no_corpus_rule_carries_a_tag_field() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    let files = corpus_files(dir);

    // Vacuity guard FIRST: an empty offender list proves nothing if the walker found no files.
    assert!(
        files.len() >= 400,
        "walker is broken or the corpus shrank: found only {} TOML files (417 on 2026-10-12)",
        files.len()
    );

    let offenders: Vec<String> = files
        .iter()
        .filter_map(|(rel, text)| {
            rule_level_value(text, "tag").map(|v| format!("{rel}: tag = {v:?}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "`tag` is deleted from the schema (W17), so a rule carrying it is parsed by nothing and \
         vanishes silently — and `corpus_files_with_unknown_fields` will fail the build on it. \
         {} offender(s):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// W17 item 1, the CONTROL: `layer` is still on every corpus rule, with its distribution
/// unchanged. The owner ruled on `tag` only and explicitly did not rule on `layer`; a 417-file
/// mechanical deletion is precisely the operation that takes an adjacent line by accident, so the
/// field left standing has to be asserted, not assumed.
///
/// Distribution pinned to the values measured on 2026-10-11 (and re-measured after the `tag`
/// sweep): a drift here means the sweep touched `layer` somewhere.
///
/// PASSES BEFORE AND AFTER the fix, deliberately — this test's job is to be a constant, so a
/// change in it is a change in `layer`.
#[test]
fn req_w17_layer_is_untouched_on_every_corpus_rule() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    let files = corpus_files(dir);
    assert!(
        files.len() >= 400,
        "walker is broken or the corpus shrank: found only {} TOML files",
        files.len()
    );

    let missing: Vec<&str> = files
        .iter()
        .filter(|(_, text)| rule_level_value(text, "layer").is_none())
        .map(|(rel, _)| rel.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "`layer` must still be present on every corpus rule — the owner ruled on `tag` only, and \
         `layer` is NOT the same shape (7 values, not folder-derivable, duplicating no domain \
         constant). Missing on: {missing:#?}"
    );

    let mut histogram: BTreeMap<String, usize> = BTreeMap::new();
    for (_, text) in &files {
        if let Some(v) = rule_level_value(text, "layer") {
            *histogram.entry(v).or_default() += 1;
        }
    }
    let expected: BTreeMap<String, usize> = [
        ("architecture", 7usize),
        ("framework", 113),
        ("language", 90),
        ("library", 34),
        ("platform", 35),
        ("testing", 7),
        ("universal", 131),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(
        histogram, expected,
        "`layer`'s value distribution must be byte-identical to the pre-`tag`-deletion \
         measurement; a difference means the W17 sweep touched the wrong field"
    );
}

/// W17 item 1 FIRE PROOF, the schema half: with `tag` removed from `RuleToml` and from
/// `RULE_LEVEL_FIELDS`, `corpus_files_with_unknown_fields` now REPORTS a reappearing `tag` at
/// rule level. Corpus-independent, so it cannot go inert the way `max_severity` did.
///
/// This is the test that makes the deletion durable rather than cosmetic. Removing the field from
/// 417 files without removing it from the schema would have left the gate silently accepting its
/// return; removing it from the schema is what converts a reappearance into a red build.
///
/// FAILS BEFORE THE FIX: `tag` was in `RULE_LEVEL_FIELDS`, so the gate reported zero offenders
/// for this fixture.
#[tokio::test]
async fn req_w17_gate_3_rejects_a_reappearing_tag_field() {
    let body = r#"
id = "W17-TAG-1"
title = "W17 reappearing-tag fixture"
tag = "stack"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"
"#;
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    std::fs::write(universal.join("w17-tag-1.toml"), body).expect("write fixture");

    let offenders = corpus_files_with_unknown_fields(dir.path())
        .await
        .expect("fixture dir is readable");
    assert_eq!(
        offenders.len(),
        1,
        "gate 3 must report exactly one unknown field (the reappearing `tag`), got: {offenders:#?}"
    );
    assert!(
        offenders[0].contains("\"tag\"") && offenders[0].contains("rule level"),
        "the offender must name `tag` at rule level: {:?}",
        offenders[0]
    );

    // And the CONTROL in the same fixture: `layer` sits on the very next line and must NOT be
    // reported. One offender, not two, is what proves the two fields were separated deliberately.
    assert!(
        !offenders[0].contains("\"layer\""),
        "`layer` is still declared accepted-and-unconsumed and must not fire: {:?}",
        offenders[0]
    );
}
