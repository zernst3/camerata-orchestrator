#![allow(clippy::unwrap_used)]
//! Acceptance of the release gate over the three REAL sweep exports made before the gate existed
//! (docs/design/2026-10-23_secret-redaction-and-release-gate.md section 8.7 and 10).
//!
//! `#[ignore]`d: it reads exports that live outside the repository. Run by hand:
//!
//!   cargo test -p camerata-server --test release_gate_acceptance -- --ignored --nocapture
//!
//! `CAMERATA_SWEEP_EXPORTS` names the directory; without it the exports are read from
//! `$CAMERATA_FIXTURES_DIR/_runs/sweep5b-2026-10-16` (the folder holding the private fixtures).
//! With neither set, each test prints why and skips. The exports are only READ.
//! Nothing here prints a value: assertions compare locations and REDACTED forms.
//!
//! What it proves: the leaks the spec confirmed in `faktura` and `hearth` are FLAGGED, at the
//! documented fields; `supabase-portal-fixture` (whose two "secrets" are fixture placeholders)
//! raises NO secrets problem. These old exports also predate the CSV/JSON datasets and the
//! manifest, so the dataset-set and manifest checks report them missing; that is not asserted
//! here beyond "the secrets check is the one that matters".

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use camerata_server::release_gate::{audit_zip, CheckId, Problem};

const EXPORTS_ENV_VAR: &str = "CAMERATA_SWEEP_EXPORTS";
const FIXTURES_DIR_ENV_VAR: &str = "CAMERATA_FIXTURES_DIR";
const EXPORTS_SUBDIR: &str = "_runs/sweep5b-2026-10-16";

/// Where the exports should be, from the two env vars: the explicit directory wins, else the
/// sweep folder inside the shared fixtures directory. `None` when neither is set: no
/// machine-specific default is baked in, because this repository is public.
fn exports_dir_from(explicit: Option<OsString>, fixtures_dir: Option<OsString>) -> Option<PathBuf> {
    explicit
        .map(PathBuf::from)
        .or_else(|| fixtures_dir.map(|dir| PathBuf::from(dir).join(EXPORTS_SUBDIR)))
}

fn exports_dir() -> Option<PathBuf> {
    exports_dir_from(
        std::env::var_os(EXPORTS_ENV_VAR),
        std::env::var_os(FIXTURES_DIR_ENV_VAR),
    )
}

/// Skip (not fail) the calling test when the exports directory is not configured.
macro_rules! require_exports {
    () => {
        match exports_dir() {
            Some(dir) => dir,
            None => {
                eprintln!(
                    "skipping: sweep exports not configured; set {} to the folder holding the \
                     private fixtures (or {} to the exports directory itself)",
                    FIXTURES_DIR_ENV_VAR, EXPORTS_ENV_VAR
                );
                return;
            }
        }
    };
}

#[test]
fn exports_dir_prefers_the_explicit_override() {
    assert_eq!(
        exports_dir_from(Some("/x/sweep".into()), Some("/fixtures".into())),
        Some(PathBuf::from("/x/sweep"))
    );
}

#[test]
fn exports_dir_falls_back_to_the_shared_fixtures_dir() {
    assert_eq!(
        exports_dir_from(None, Some("/fixtures".into())),
        Some(PathBuf::from("/fixtures/_runs/sweep5b-2026-10-16"))
    );
}

#[test]
fn exports_dir_is_none_when_neither_env_var_is_set() {
    assert_eq!(exports_dir_from(None, None), None);
}

/// The secrets problems of `name`.zip, with the PDF's rendered text checked too.
fn secrets_problems(dir: &Path, name: &str) -> Vec<Problem> {
    let path = dir.join(format!("{name}.zip"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let entries = camerata_server::release_gate::read_zip_entries(&bytes).expect("a zip");
    let pdf_text = entries
        .iter()
        .find(|(n, _)| n.ends_with(".pdf"))
        .and_then(|(_, pdf)| pdf_extract::extract_text_from_mem(pdf).ok());
    audit_zip(&bytes, pdf_text.as_deref())
        .checks
        .into_iter()
        .filter(|o| o.check == CheckId::Secrets)
        .flat_map(|o| o.problems)
        .collect()
}

fn located<'a>(problems: &'a [Problem], file: &str, location: &str) -> Option<&'a Problem> {
    problems
        .iter()
        .find(|p| p.file.ends_with(file) && p.location == location)
}

#[test]
#[ignore = "reads the sweep exports outside the repo; run by hand"]
fn acceptance_known_leaks_are_flagged_in_the_old_exports() {
    let dir = require_exports!();
    // ---- faktura: one real Supabase secret key (51 chars) in findings.json and the workbook ----
    let faktura = secrets_problems(&dir, "faktura");
    for (file, location) in [
        ("findings.json", "findings[0].snippet"),
        ("findings.json", "findings[0].fix_specific"),
        ("findings.json", "findings[0].also_locations[1].snippet"),
        ("findings.json", "findings[43].fix_specific"),
        ("-findings.xlsx", "sheet \"All Findings\" cell T2"),
        ("-findings.xlsx", "sheet \"All Findings\" cell V2"),
        ("-findings.xlsx", "sheet \"All Findings\" cell V45"),
        ("-findings.xlsx", "sheet \"Universal\" cell T2"),
        ("-findings.xlsx", "sheet \"Universal\" cell V2"),
        ("-findings.xlsx", "sheet \"Universal\" cell V8"),
    ] {
        let hit = located(&faktura, file, location)
            .unwrap_or_else(|| panic!("faktura: no problem at {file} {location}"));
        let evidence = hit.evidence.as_deref().unwrap_or_default();
        assert!(
            evidence.starts_with("sb_secret_\u{2026}") && evidence.ends_with("(51 chars)"),
            "faktura {file} {location}: {evidence}"
        );
    }
    assert!(
        faktura.iter().any(|p| p.file.ends_with(".pdf")),
        "faktura: the key on the PDF pages is flagged in the rendered text"
    );
    // The page-40 NAME literal (SUPABASE_SERVICE_ROLE_KEY) is a safe twin, not a problem.
    assert!(
        faktura.iter().all(|p| p
            .evidence
            .as_deref()
            .is_some_and(|e| e.starts_with("sb_secret_\u{2026}"))),
        "faktura: only the Supabase secret key is flagged"
    );

    // ---- hearth: four real secrets, all on the one merged row findings[1] -------------------
    let hearth = secrets_problems(&dir, "hearth");
    for (location, prefix, length) in [
        ("findings[1].snippet", "sb_secret_", "(42 chars)"),
        ("findings[1].fix_specific", "sb_secret_", "(42 chars)"),
    ] {
        let hit = located(&hearth, "findings.json", location)
            .unwrap_or_else(|| panic!("hearth: no problem at {location}"));
        let evidence = hit.evidence.as_deref().unwrap_or_default();
        assert!(
            evidence.starts_with(&format!("{prefix}\u{2026}")) && evidence.ends_with(length),
            "hearth {location}: {evidence}"
        );
    }
    let evidences: Vec<&str> = hearth
        .iter()
        .filter(|p| p.file == "findings.json" && p.location.contains("also_locations"))
        .filter_map(|p| p.evidence.as_deref())
        .collect();
    assert!(
        evidences
            .iter()
            .filter(|e| e.starts_with("sk_live_\u{2026}") && e.ends_with("(77 chars)"))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            >= 2,
        "hearth: two distinct Stripe live keys in also_locations: {evidences:?}"
    );
    assert!(
        evidences
            .iter()
            .any(|e| e.starts_with("whsec_\u{2026}") && e.ends_with("(38 chars)")),
        "hearth: the webhook secret in also_locations: {evidences:?}"
    );
    // Stripe's documented test key (a published example) and the live PUBLISHABLE key are
    // readable by design: neither is flagged.
    assert!(
        hearth.iter().all(|p| !p
            .evidence
            .as_deref()
            .unwrap_or_default()
            .starts_with("sk_test_")),
        "hearth: the published Stripe docs key must stay readable"
    );
    assert!(
        hearth.iter().all(|p| !p
            .evidence
            .as_deref()
            .unwrap_or_default()
            .starts_with("pk_live_")),
        "hearth: a publishable key is public by design"
    );
}

#[test]
#[ignore = "reads the sweep exports outside the repo; run by hand"]
fn acceptance_portal_placeholders_stay_readable_and_the_secrets_check_is_clean() {
    let dir = require_exports!();
    let portal = secrets_problems(&dir, "supabase-portal-fixture");
    assert!(
        portal.is_empty(),
        "the portal fixture's values are placeholders (DO_NOT / FAKE / your...here), which stay \
         readable: {portal:?}"
    );
}
