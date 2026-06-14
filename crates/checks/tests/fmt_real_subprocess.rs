//! Integration test for the REAL layer-2 fmt subprocess path.
//!
//! Unlike the unit tests in `src/parse.rs` (which feed static fixture strings
//! into the pure mapping functions), these tests actually shell out to
//! `cargo fmt --check` against a temp worktree on disk. This proves the
//! [`camerata_checks::FmtCheckRunner`] / [`camerata_checks::RustCheckRunner`]
//! real-subprocess path end-to-end: a badly-formatted crate yields the
//! `RUST-FMT` rule id, a clean crate yields nothing.
//!
//! These are deliberately NOT in `src/` so they exercise the crate exactly as a
//! downstream consumer (the coordinator) would: through the public
//! [`camerata_core::CheckRunner`] trait.

use std::fs;
use std::path::{Path, PathBuf};

use camerata_checks::{fmt_rule, FmtCheckRunner};
use camerata_core::{CheckRunner, Role, RuleId};

/// A self-cleaning temp directory. We avoid pulling the `tempfile` crate so the
/// test introduces no new workspace dependency; this 20-line helper is enough
/// for a single scratch cargo project per test.
struct TempWorktree {
    path: PathBuf,
}

impl TempWorktree {
    /// Create a unique temp directory under the system temp dir.
    fn new(tag: &str) -> std::io::Result<Self> {
        let mut path = std::env::temp_dir();
        // Uniqueness: tag + pid + a monotonic-ish nanosecond stamp.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!(
            "camerata-checks-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Write a relative file under the worktree, creating parent dirs.
    fn write(&self, rel: &str, contents: &str) -> std::io::Result<()> {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(full, contents)
    }
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        // Best-effort cleanup; ignore errors (temp dir may already be gone).
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Lay down a minimal cargo binary crate with the given `main.rs` body so
/// `cargo fmt` has a real package to discover and format.
fn scaffold_crate(wt: &TempWorktree, main_rs: &str) {
    wt.write(
        "Cargo.toml",
        "[package]\nname = \"fmt-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .expect("write Cargo.toml");
    wt.write("src/main.rs", main_rs).expect("write main.rs");
}

/// The Role passed to the runner is irrelevant to fmt (the fmt runner ignores
/// it), but the trait requires one.
fn any_role() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: vec![RuleId("RUST-FMT".to_string())],
        allowed_paths: vec![".".to_string()],
    }
}

#[tokio::test]
async fn badly_formatted_crate_yields_rust_fmt_violation() {
    let wt = TempWorktree::new("dirty").expect("create temp worktree");
    // Mangled whitespace that `cargo fmt --check` will object to, but which
    // still parses (rustfmt only needs valid syntax, not a successful compile).
    scaffold_crate(&wt, "fn   main( ){let x=1;println!(\"{}\",x );}\n");

    let runner = FmtCheckRunner;
    let violations = runner
        .check(&any_role(), wt.path())
        .await
        .expect("fmt check should run the real subprocess without erroring");

    assert!(
        !violations.is_empty(),
        "a badly-formatted crate must produce at least one violation, got {violations:?}"
    );
    assert!(
        violations.contains(&fmt_rule()),
        "the violation set must contain the RUST-FMT rule id, got {violations:?}"
    );
}

#[tokio::test]
async fn cleanly_formatted_crate_yields_no_violations() {
    let wt = TempWorktree::new("clean").expect("create temp worktree");
    // rustfmt-canonical form: exactly what `cargo fmt` would emit.
    scaffold_crate(
        &wt,
        "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
    );

    let runner = FmtCheckRunner;
    let violations = runner
        .check(&any_role(), wt.path())
        .await
        .expect("fmt check should run the real subprocess without erroring");

    assert!(
        violations.is_empty(),
        "a cleanly-formatted crate must produce no violations, got {violations:?}"
    );
}
