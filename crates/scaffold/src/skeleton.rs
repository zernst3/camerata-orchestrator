use std::fs;
use std::path::Path;

use chrono::Utc;

use crate::substitution::substitute;
use crate::{AppRequirements, ScaffoldError, ScaffoldOutcome};

/// Every file the vetted skeleton ships, as `(relative_path, raw_contents)` pairs.
/// Contents are embedded at COMPILE time via `include_str!` (no runtime directory
/// walk, no `include_dir`/`rust-embed` dependency — the file list is small and fixed,
/// so a plain array is simpler and one fewer dependency). Every file gets the same
/// substitution pass (`crate::substitution::substitute`); files with no placeholders
/// just pass through unchanged.
///
/// Keep this list and the actual `templates/skeleton/` tree in sync — the ignored
/// `generated_skeleton_compiles` test (and the plain `scaffold_skeleton_*` unit
/// tests) will catch a mismatch, but there is no build-time check that this array
/// covers every file on disk (a possible follow-up: a build script that asserts the
/// two agree).
const TEMPLATE_FILES: &[(&str, &str)] = &[
    (".gitignore", include_str!("../templates/skeleton/.gitignore")),
    ("Cargo.toml", include_str!("../templates/skeleton/Cargo.toml")),
    ("Dioxus.toml", include_str!("../templates/skeleton/Dioxus.toml")),
    ("README.md", include_str!("../templates/skeleton/README.md")),
    ("index.html", include_str!("../templates/skeleton/index.html")),
    ("CONVENTIONS.md", include_str!("../templates/skeleton/CONVENTIONS.md")),
    ("AGENTS.md", include_str!("../templates/skeleton/AGENTS.md")),
    (
        ".github/workflows/ci.yml",
        include_str!("../templates/skeleton/.github/workflows/ci.yml"),
    ),
    ("src/main.rs", include_str!("../templates/skeleton/src/main.rs")),
    ("src/lib.rs", include_str!("../templates/skeleton/src/lib.rs")),
    ("src/app.rs", include_str!("../templates/skeleton/src/app.rs")),
    ("src/routes.rs", include_str!("../templates/skeleton/src/routes.rs")),
    ("src/server.rs", include_str!("../templates/skeleton/src/server.rs")),
    (
        "src/wasm_bridge.rs",
        include_str!("../templates/skeleton/src/wasm_bridge.rs"),
    ),
    (
        "src/server_fns.rs",
        include_str!("../templates/skeleton/src/server_fns.rs"),
    ),
    (
        "src/pages/mod.rs",
        include_str!("../templates/skeleton/src/pages/mod.rs"),
    ),
    (
        "src/pages/home.rs",
        include_str!("../templates/skeleton/src/pages/home.rs"),
    ),
    (
        "src/components/mod.rs",
        include_str!("../templates/skeleton/src/components/mod.rs"),
    ),
    (
        "src/components/button.rs",
        include_str!("../templates/skeleton/src/components/button.rs"),
    ),
    (
        "src/components/field.rs",
        include_str!("../templates/skeleton/src/components/field.rs"),
    ),
    (
        "src/components/card.rs",
        include_str!("../templates/skeleton/src/components/card.rs"),
    ),
    (
        "src/components/app_shell.rs",
        include_str!("../templates/skeleton/src/components/app_shell.rs"),
    ),
    (
        "assets/manifest.json",
        include_str!("../templates/skeleton/assets/manifest.json"),
    ),
    ("assets/icon.svg", include_str!("../templates/skeleton/assets/icon.svg")),
    (
        "assets/service-worker.js",
        include_str!("../templates/skeleton/assets/service-worker.js"),
    ),
    (
        "assets/error-reporter.js",
        include_str!("../templates/skeleton/assets/error-reporter.js"),
    ),
    (
        "assets/styles/index.css",
        include_str!("../templates/skeleton/assets/styles/index.css"),
    ),
    (
        "assets/design/tokens.css",
        include_str!("../templates/skeleton/assets/design/tokens.css"),
    ),
    (
        "assets/design/components.css",
        include_str!("../templates/skeleton/assets/design/components.css"),
    ),
    (
        "terraform/main.tf",
        include_str!("../templates/skeleton/terraform/main.tf"),
    ),
    (
        "terraform/variables.tf",
        include_str!("../templates/skeleton/terraform/variables.tf"),
    ),
    (
        "terraform/outputs.tf",
        include_str!("../templates/skeleton/terraform/outputs.tf"),
    ),
];

/// The default auto-capture reporter target when `AppRequirements::capture_url` is
/// `None`: a relative path, resolved against whatever origin serves the app.
const DEFAULT_CAPTURE_URL: &str = "/api/feedback";

/// First alphanumeric character of `name`, uppercased, for the placeholder monogram
/// icon (`assets/icon.svg`). Falls back to `"A"` when `name` has none.
fn app_initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "A".to_string())
}

/// Materialize the vetted Dioxus-fullstack PWA skeleton into `target_dir`, with
/// every `{{PLACEHOLDER}}` in the template files substituted from `reqs`.
///
/// Always emits the Skeleton path — callers decide whether the skeleton is the
/// right fit by calling [`crate::choose_strategy`] first and only invoking this when
/// it returns [`crate::ScaffoldStrategy::Skeleton`].
///
/// Creates `target_dir` (and every file's parent directory) if it doesn't already
/// exist. Overwrites files that already exist at the destination path.
pub fn scaffold_skeleton(
    reqs: &AppRequirements,
    target_dir: &Path,
) -> Result<ScaffoldOutcome, ScaffoldError> {
    let package_name = reqs.package_name();
    let description = if reqs.description.trim().is_empty() {
        "A Camerata-scaffolded app.".to_string()
    } else {
        reqs.description.clone()
    };
    let display_name = if reqs.name.trim().is_empty() {
        "Camerata App".to_string()
    } else {
        reqs.name.clone()
    };
    let capture_url = reqs
        .capture_url
        .clone()
        .unwrap_or_else(|| DEFAULT_CAPTURE_URL.to_string());
    let year = Utc::now().format("%Y").to_string();
    let initial = app_initial(&display_name);

    let subs: Vec<(&str, &str)> = vec![
        ("APP_NAME", display_name.as_str()),
        ("APP_NAME_SNAKE", package_name.as_str()),
        ("APP_DESCRIPTION", description.as_str()),
        ("CAPTURE_URL", capture_url.as_str()),
        ("YEAR", year.as_str()),
        ("APP_INITIAL", initial.as_str()),
    ];

    fs::create_dir_all(target_dir).map_err(|source| ScaffoldError::CreateDir {
        path: target_dir.to_path_buf(),
        source,
    })?;

    let mut files_written = Vec::with_capacity(TEMPLATE_FILES.len());
    for (relative_path, raw_contents) in TEMPLATE_FILES {
        let dest = target_dir.join(relative_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| ScaffoldError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let contents = substitute(raw_contents, &subs);
        fs::write(&dest, contents).map_err(|source| ScaffoldError::WriteFile {
            path: dest.clone(),
            source,
        })?;
        files_written.push(dest);
    }

    let mut notes = vec![
        "No database (DB-on-demand): add persistence in a later phase only if the app's requirements need it.".to_string(),
        "No end-user auth by default: add an auth module in a later phase only if the app's requirements need it.".to_string(),
        format!(
            "Auto-capture reporter POSTs to \"{capture_url}\" — that endpoint is Part 2 of the scaffolder (not implemented by this skeleton); POSTs 404 harmlessly until then."
        ),
        "Run `dx serve --platform web` from the target directory to preview.".to_string(),
    ];
    if reqs.needs_persistence {
        notes.push(
            "AppRequirements.needs_persistence is set, but this skeleton never adds a database itself — a later phase layers persistence on top.".to_string(),
        );
    }
    if reqs.needs_auth {
        notes.push(
            "AppRequirements.needs_auth is set, but this skeleton never adds auth itself — a later phase layers an auth module on top.".to_string(),
        );
    }

    Ok(ScaffoldOutcome {
        files_written,
        package_name,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::substitution::leftover_placeholders;

    fn sample_reqs() -> AppRequirements {
        AppRequirements {
            name: "Trip Planner".to_string(),
            description: "Track flights, stays, and an active itinerary.".to_string(),
            target: crate::AppTarget::WebPwa,
            needs_persistence: false,
            needs_auth: false,
            summary: "an app that tracks my flights and shows them on a timeline".to_string(),
            capture_url: None,
        }
    }

    #[test]
    fn writes_every_declared_template_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outcome = scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");

        assert_eq!(outcome.files_written.len(), TEMPLATE_FILES.len());
        for (relative_path, _) in TEMPLATE_FILES {
            let path = tmp.path().join(relative_path);
            assert!(path.is_file(), "expected {relative_path} to exist at {path:?}");
        }
        assert_eq!(outcome.package_name, "trip_planner");
    }

    #[test]
    fn no_leftover_placeholders_in_any_written_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");

        for (relative_path, _) in TEMPLATE_FILES {
            let path = tmp.path().join(relative_path);
            let contents = fs::read_to_string(&path).expect("read written file");
            let leftover = leftover_placeholders(&contents);
            assert!(
                leftover.is_empty(),
                "{relative_path} still has unfilled placeholders: {leftover:?}"
            );
        }
    }

    #[test]
    fn substitutions_actually_land_in_key_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");

        let cargo_toml = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("name = \"trip_planner\""));
        assert!(cargo_toml.contains("Track flights, stays, and an active itinerary."));

        let manifest = fs::read_to_string(tmp.path().join("assets/manifest.json")).unwrap();
        assert!(manifest.contains("\"name\": \"Trip Planner\""));

        let index_html = fs::read_to_string(tmp.path().join("index.html")).unwrap();
        // index.html's own `<title>` is deliberately left empty — `dx` appends
        // Dioxus.toml's `web.app.title` into it rather than replacing it (verified
        // empirically against a live `dx serve`; see Dioxus.toml's comment), so
        // Dioxus.toml is the actual source of truth for the rendered title.
        assert!(index_html.contains("<title></title>"));
        assert!(index_html.contains("window.CAMERATA_CAPTURE_URL = \"/api/feedback\";"));

        let dioxus_toml = fs::read_to_string(tmp.path().join("Dioxus.toml")).unwrap();
        assert!(dioxus_toml.contains("title = \"Trip Planner\""));
    }

    #[test]
    fn capture_url_override_is_honored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut reqs = sample_reqs();
        reqs.capture_url = Some("https://example.test/ingest".to_string());
        scaffold_skeleton(&reqs, tmp.path()).expect("scaffold");

        let index_html = fs::read_to_string(tmp.path().join("index.html")).unwrap();
        assert!(index_html.contains("window.CAMERATA_CAPTURE_URL = \"https://example.test/ingest\";"));
    }

    #[test]
    fn skeleton_ships_with_no_database_or_migration_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");

        let cargo_toml = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        let lower = cargo_toml.to_lowercase();
        assert!(!lower.contains("sqlx"));
        assert!(!lower.contains("postgres"));
        assert!(!lower.contains("diesel"));
        assert!(!tmp.path().join("migrations").exists());
    }

    #[test]
    fn conventions_md_reformats_invented_rules_as_custom_blocks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");

        let conventions = fs::read_to_string(tmp.path().join("CONVENTIONS.md")).unwrap();

        // The two invented rules are CUSTOM blocks (FIX B), matching
        // `render_custom`'s exact shape (`### CUSTOM-<name>`), not invented
        // corpus-style rule IDs.
        assert!(conventions.contains("### CUSTOM-db-on-demand"));
        assert!(conventions.contains("### CUSTOM-pwa-auto-capture"));
        assert!(!conventions.contains("DB-ON-DEMAND-1"));
        assert!(!conventions.contains("PWA-AUTO-CAPTURE-1"));

        // Real corpus references are untouched.
        assert!(conventions.contains("RUST-DIOXUS-9"));
        assert!(conventions.contains("RUST-DIOXUS-11"));
        assert!(conventions.contains("ARCH-STRUCTURED-ERRORS-1"));

        // The template's CUSTOM block bodies read identically to
        // `default_custom_rules`'s bodies, so the freshly scaffolded repo and the
        // Camerata project seeded from it (Part 2) never drift apart.
        for (name, body) in crate::default_custom_rules() {
            assert!(
                conventions.contains(&format!("### CUSTOM-{name}")),
                "missing CUSTOM-{name} block"
            );
            assert!(
                conventions.contains(body),
                "CONVENTIONS.md body for {name} does not match default_custom_rules"
            );
        }
    }

    #[test]
    fn manifest_and_service_worker_and_governance_files_are_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");

        for expected in [
            "assets/manifest.json",
            "assets/service-worker.js",
            "assets/error-reporter.js",
            "CONVENTIONS.md",
            "AGENTS.md",
            ".github/workflows/ci.yml",
        ] {
            assert!(tmp.path().join(expected).is_file(), "missing {expected}");
        }
    }

    #[test]
    fn blank_name_and_description_fall_back_to_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reqs = AppRequirements::default();
        let outcome = scaffold_skeleton(&reqs, tmp.path()).expect("scaffold");
        assert_eq!(outcome.package_name, "camerata_app");

        let cargo_toml = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("A Camerata-scaffolded app."));
    }

    /// The real "just works" proof: generate the skeleton to a tempdir and run
    /// `cargo check` against BOTH targets the app actually ships for — the native
    /// server binary (host target) and the wasm client (wasm32-unknown-unknown,
    /// with `--features web`, matching how `dx` builds the web platform). This is
    /// slow (a fresh dependency fetch + build of dioxus/axum/tokio) so it is
    /// `#[ignore]`d for normal `cargo test` runs; run explicitly with:
    ///
    /// ```sh
    /// cargo test -p camerata-scaffold generated_skeleton_compiles -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "slow: fetches + builds the full dioxus/axum/tokio dependency graph twice (native + wasm32)"]
    fn generated_skeleton_compiles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outcome = scaffold_skeleton(&sample_reqs(), tmp.path()).expect("scaffold");
        println!("scaffolded {} files into {:?}", outcome.files_written.len(), tmp.path());

        let run_cargo_check = |extra_args: &[&str]| -> (bool, String) {
            let output = std::process::Command::new("cargo")
                .arg("check")
                .args(extra_args)
                .current_dir(tmp.path())
                .output()
                .expect("failed to spawn cargo check");
            let combined = format!(
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            (output.status.success(), combined)
        };

        let (native_ok, native_log) = run_cargo_check(&[
            "--all-targets",
            "--no-default-features",
            "--features",
            "server",
        ]);
        println!("=== native `cargo check` ===\n{native_log}");

        let (wasm_ok, wasm_log) = run_cargo_check(&[
            "--target",
            "wasm32-unknown-unknown",
            "--no-default-features",
            "--features",
            "web",
        ]);
        println!("=== wasm32 `cargo check` ===\n{wasm_log}");

        assert!(native_ok, "native cargo check failed:\n{native_log}");
        assert!(wasm_ok, "wasm32 cargo check failed:\n{wasm_log}");
    }
}
