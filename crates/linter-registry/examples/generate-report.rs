//! Example binary: scan corpus and write citation-validation.md.
//!
//! Usage: cargo run -p camerata-linter-registry --example generate-report

use std::path::Path;

fn main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // Corpus lives in crates/rules/principles/ relative to workspace root.
    let workspace_root = Path::new(manifest_dir)
        .parent() // crates/linter-registry → crates
        .and_then(|p| p.parent()) // crates → workspace root
        .expect("workspace root");

    let corpus_dir = workspace_root.join("crates/rules/principles");
    let output_path = workspace_root.join("docs/rule-grounding/citation-validation.md");

    let registry = camerata_linter_registry::LinterRegistry::global();

    match camerata_linter_registry::generate_report(&corpus_dir, &output_path, &registry) {
        Ok((md, errors)) => {
            if !errors.is_empty() {
                eprintln!("Scan completed with {} errors:", errors.len());
                for e in &errors {
                    eprintln!("  {e}");
                }
            }
            // Print summary line count as a sanity check.
            let lines = md.lines().count();
            println!(
                "Report written to {} ({lines} lines)",
                output_path.display()
            );
        }
        Err(e) => {
            eprintln!("Failed to generate report: {e}");
            std::process::exit(1);
        }
    }
}
