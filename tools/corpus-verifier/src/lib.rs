//! corpus-verifier CORE — the shared logic behind the CLI and the GUI.
//!
//! # What this tool is (and is NOT)
//!
//! This is a **MAINTAINER-ONLY repo-governance tool**. It promotes corpus rules
//! from `grounded` to `verified` (see [`camerata_rules::Verification`]) by:
//!
//! 1. locating the rule's `.toml` in `crates/rules/principles/`,
//! 2. editing it in place — setting `verification = "verified"` and writing a
//!    `[verified]` table with `by` / `at` / `against`, preserving all other
//!    fields, comments and formatting (via `toml_edit`),
//! 3. committing the edit on a `verify/<rule-id>` branch and opening a PR into
//!    `main` (the source of truth).
//!
//! `verified` is therefore ONLY ever set through a **reviewed commit in main**.
//! The shipped app (camerata-ui / camerata-server) is READ-ONLY on verification;
//! this tool is the single writer. It is NOT part of the product, is excluded
//! from the app deploy, and must never be a dependency of an app crate.
//!
//! ## Surfaces
//!
//! - [`locate_rule`] / [`apply_verification`] — the in-place TOML edit primitives.
//! - [`list_grounded`] — the risk-ordered grounded queue.
//! - [`meta_domains`] / [`self_source_targets`] — the maintainer-authored corpora.
//! - [`VcsOps`] — the git/PR seam: a real [`GitVcs`] shelling out to `git` + `gh`,
//!   and a [`DryRunVcs`] that records the plan without touching git or the network.
//! - [`verify_one`] / [`self_source`] — the end-to-end flows the CLI/GUI call.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use camerata_rules::{load_corpus, RuleSource, Verification};

// ────────────────────────────────────────────────────────────────────────────
// Corpus path resolution
// ────────────────────────────────────────────────────────────────────────────

/// The bundled corpus directory (`crates/rules/principles`), resolved relative to
/// THIS crate's manifest dir so the tool works from any working directory.
///
/// `camerata_rules::DEFAULT_CORPUS_PATH` resolves relative to the *rules* crate
/// manifest; we recompute it from our own manifest for robustness, but honour the
/// `CAMERATA_CORPUS_PATH` override exactly as the loader does.
pub fn corpus_dir() -> PathBuf {
    if let Some(p) = std::env::var("CAMERATA_CORPUS_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return PathBuf::from(p);
    }
    // tools/corpus-verifier -> repo root -> crates/rules/principles
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/rules/principles")
}

/// The repo root, derived from this crate's manifest dir (`tools/corpus-verifier`).
/// Used as the default working directory for git operations.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ────────────────────────────────────────────────────────────────────────────
// locate_rule
// ────────────────────────────────────────────────────────────────────────────

/// Find the `.toml` file in `corpus_dir` whose `id` field equals `rule_id`.
///
/// Scans every `.toml` under the corpus (recursively) and matches on the parsed
/// `id`, not the filename — filenames are lowercased/hyphenated and not a reliable
/// key. Returns the path, or an error if no rule with that id exists.
pub fn locate_rule(corpus_dir: &Path, rule_id: &str) -> Result<PathBuf> {
    let mut paths = Vec::new();
    collect_toml_paths(corpus_dir, &mut paths)
        .with_context(|| format!("scanning corpus at {}", corpus_dir.display()))?;

    for path in &paths {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        // A cheap pre-filter avoids fully parsing every file, then a precise
        // parse confirms the `id` field (not a substring match).
        if !text.contains(rule_id) {
            continue;
        }
        if let Ok(doc) = text.parse::<toml_edit::DocumentMut>() {
            if doc.get("id").and_then(|v| v.as_str()) == Some(rule_id) {
                return Ok(path.clone());
            }
        }
    }
    Err(anyhow!(
        "no rule with id `{rule_id}` found under {}",
        corpus_dir.display()
    ))
}

fn collect_toml_paths(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_toml_paths(&path, out)?;
        } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// apply_verification — the in-place, formatting-preserving TOML edit
// ────────────────────────────────────────────────────────────────────────────

/// Render the `[verified]` TOML block that [`apply_verification`] writes, for the
/// given provenance. Used by the GUI to show an inline EXAMPLE before committing.
pub fn verified_block_preview(by: &str, at: &str, against: &[String]) -> String {
    let mut s = String::from("[verified]\n");
    s.push_str(&format!("by = {}\n", toml_edit::value(by)));
    s.push_str(&format!("at = {}\n", toml_edit::value(at)));
    let mut arr = toml_edit::Array::new();
    for a in against {
        arr.push(a.as_str());
    }
    s.push_str(&format!("against = {arr}\n"));
    s
}

/// Edit `path` in place: set `verification = "verified"` and add/replace a
/// `[verified]` table with `by` / `at` / `against`.
///
/// This is a TARGETED edit via `toml_edit`: every other field, comment, and the
/// existing formatting are preserved. Re-applying is idempotent (the same inputs
/// produce the same file). The function does NOT touch git — that is the
/// [`VcsOps`] layer's job.
pub fn apply_verification(path: &Path, by: &str, at: &str, against: &[String]) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading rule file {}", path.display()))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing rule file {} as TOML", path.display()))?;

    edit_doc(&mut doc, by, at, against);

    std::fs::write(path, doc.to_string())
        .with_context(|| format!("writing rule file {}", path.display()))?;
    Ok(())
}

/// The pure document edit, factored out so it is unit-testable without disk I/O.
fn edit_doc(doc: &mut toml_edit::DocumentMut, by: &str, at: &str, against: &[String]) {
    use toml_edit::{value, Item, Table};

    // 1. verification = "verified"
    doc["verification"] = value("verified");

    // 2. [verified] table — add or replace, building from scratch so a stale
    //    table (e.g. an older `against` list) is fully overwritten.
    let mut table = Table::new();
    table["by"] = value(by);
    table["at"] = value(at);
    let mut arr = toml_edit::Array::new();
    for a in against {
        arr.push(a.as_str());
    }
    table["against"] = value(arr);
    doc["verified"] = Item::Table(table);
}

// ────────────────────────────────────────────────────────────────────────────
// list_grounded — the risk-ordered queue
// ────────────────────────────────────────────────────────────────────────────

/// One row of the grounded queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundedRow {
    pub id: String,
    pub domain: String,
    pub enforcement: String,
    /// First `[[sources]].url`, when present.
    pub primary_source: Option<String>,
}

/// Risk rank for a rule, lower sorts first. Mirrors the
/// `docs/QA/VERIFICATION_QUEUE.md` ordering:
///   Tier A: mechanical (highest blast radius) first,
///   then by domain familiarity (rust/csharp/web known first), then the rest.
fn risk_rank(enforcement: &str, domain: &str) -> (u8, u8) {
    let enforcement_rank = match enforcement {
        "mechanical" => 0,
        "architectural" => 1,
        "structured" => 2,
        "prose" => 3,
        _ => 4,
    };
    // Primary domain component (strip ":react", ":seaorm", etc.).
    let primary = domain.split(':').next().unwrap_or(domain);
    let domain_rank = match primary {
        // Known/first-party languages the maintainer can verify fastest.
        "rust" => 0,
        "csharp" => 1,
        "javascript" | "typescript" | "ui" => 2,
        // First-party meta corpora.
        "agentic" | "api-layer" | "permissions" | "universal" => 3,
        // Everything else.
        _ => 4,
    };
    (enforcement_rank, domain_rank)
}

/// Load the corpus and return every rule currently at `verification = "grounded"`,
/// risk-ordered (mechanical first, then known languages, then the rest; ties
/// broken by id for determinism).
pub async fn list_grounded(corpus_dir: &Path) -> Result<Vec<GroundedRow>> {
    let set = load_corpus(corpus_dir)
        .await
        .with_context(|| format!("loading corpus at {}", corpus_dir.display()))?;

    let mut rows: Vec<GroundedRow> = set
        .iter()
        .filter(|r| r.verification == Verification::Grounded)
        .map(|r| GroundedRow {
            id: r.id_str().to_owned(),
            domain: r.domain.clone(),
            enforcement: r.enforcement.as_str().to_owned(),
            primary_source: r.sources.first().map(|s| s.url.clone()),
        })
        .collect();

    rows.sort_by(|a, b| {
        risk_rank(&a.enforcement, &a.domain)
            .cmp(&risk_rank(&b.enforcement, &b.domain))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

// ────────────────────────────────────────────────────────────────────────────
// meta / self-source set
// ────────────────────────────────────────────────────────────────────────────

/// The maintainer-authored ("meta") domains — corpora the maintainer designed
/// rather than mirroring an external linter/style-guide. These can be
/// "self-sourced": flipped to `verified` with `against = ["self-sourced: <domain>"]`
/// because the maintainer IS the authority for them.
///
/// Sub-domains (e.g. `api-layer:foo`) match on their primary component.
pub const META_DOMAINS: &[&str] = &["agentic", "api-layer", "ui", "permissions", "universal"];

/// Whether `domain` belongs to the maintainer-authored meta set.
pub fn is_meta_domain(domain: &str) -> bool {
    let primary = domain.split(':').next().unwrap_or(domain);
    META_DOMAINS.contains(&primary)
}

/// One self-source target: a meta-domain rule eligible to be flipped to verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfSourceTarget {
    pub id: String,
    pub domain: String,
    pub path: PathBuf,
}

/// Find the meta-domain rules to self-source.
///
/// - `domain = Some(d)` restricts to that one meta domain.
/// - `domain = None` covers ALL meta domains (the `--all-meta` flow).
///
/// Only rules currently `grounded` are returned (already-`verified` rules are
/// skipped, making the flow idempotent). Returns each rule's id, domain, and path.
pub async fn self_source_targets(
    corpus_dir: &Path,
    domain: Option<&str>,
) -> Result<Vec<SelfSourceTarget>> {
    if let Some(d) = domain {
        if !is_meta_domain(d) {
            return Err(anyhow!(
                "`{d}` is not a maintainer-authored meta domain; meta domains are: {}",
                META_DOMAINS.join(", ")
            ));
        }
    }

    let set = load_corpus(corpus_dir)
        .await
        .with_context(|| format!("loading corpus at {}", corpus_dir.display()))?;

    let mut targets = Vec::new();
    for rule in set.iter() {
        if rule.verification != Verification::Grounded {
            continue;
        }
        let in_scope = match domain {
            Some(d) => rule.domain.split(':').next() == d.split(':').next(),
            None => is_meta_domain(&rule.domain),
        };
        if !in_scope {
            continue;
        }
        let path = locate_rule(corpus_dir, rule.id_str())?;
        targets.push(SelfSourceTarget {
            id: rule.id_str().to_owned(),
            domain: rule.domain.clone(),
            path,
        });
    }
    targets.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(targets)
}

/// Build the `against` anchor for a `verify <id>` when `--against` was omitted:
/// derive a version anchor from each `[[sources]]` (its title + url). Returns the
/// list used, so the CLI can print "what it used".
pub fn against_from_sources(sources: &[RuleSource]) -> Vec<String> {
    sources
        .iter()
        .map(|s| {
            if let Some(linter) = &s.linter {
                format!("{} ({}) — {}", s.title, linter, s.url)
            } else {
                format!("{} — {}", s.title, s.url)
            }
        })
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────
// VcsOps seam
// ────────────────────────────────────────────────────────────────────────────

/// The git/PR operations the verify flow needs, behind a trait so the real
/// implementation ([`GitVcs`]) and the test/dry-run implementation ([`DryRunVcs`])
/// are interchangeable. The flow code never shells out directly; it talks to this
/// seam, which is why tests run with NO git and NO network.
pub trait VcsOps {
    /// Create `branch` off the current `main` (and switch to it).
    fn create_branch(&self, branch: &str) -> Result<()>;
    /// Stage `paths` and commit with `message`.
    fn commit(&self, paths: &[PathBuf], message: &str) -> Result<()>;
    /// Push `branch` to origin.
    fn push(&self, branch: &str) -> Result<()>;
    /// Open a PR from `branch` into `main` with `title`/`body`; return the PR URL.
    fn open_pr(&self, branch: &str, title: &str, body: &str) -> Result<String>;
}

/// Real VCS impl: shells out to `git` (branch/commit/push) and `gh pr create`.
///
/// IMPORTANT: this is the production path. During tool *development* it must not
/// be exercised against a live repo (no real `verify/*` branch, no real PR). The
/// CLI's `--dry-run` and all tests use [`DryRunVcs`] instead.
pub struct GitVcs {
    /// Working directory for git operations (the repo root).
    pub cwd: PathBuf,
    /// Base branch PRs target. Always `main` for this tool's contract.
    pub base: String,
}

impl GitVcs {
    /// Construct against the repo root, basing PRs on `main`.
    pub fn new() -> Self {
        Self {
            cwd: repo_root(),
            base: "main".to_owned(),
        }
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<String> {
        let out = std::process::Command::new(program)
            .args(args)
            .current_dir(&self.cwd)
            .output()
            .with_context(|| format!("running `{program} {}`", args.join(" ")))?;
        if !out.status.success() {
            return Err(anyhow!(
                "`{program} {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

impl Default for GitVcs {
    fn default() -> Self {
        Self::new()
    }
}

impl VcsOps for GitVcs {
    fn create_branch(&self, branch: &str) -> Result<()> {
        // Branch off the current main without leaving the user's checkout dirty.
        self.run("git", &["checkout", &self.base])?;
        self.run("git", &["checkout", "-b", branch])?;
        Ok(())
    }

    fn commit(&self, paths: &[PathBuf], message: &str) -> Result<()> {
        let mut args = vec!["add"];
        let path_strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        args.extend(path_strs.iter().map(String::as_str));
        self.run("git", &args)?;
        self.run("git", &["commit", "-m", message])?;
        Ok(())
    }

    fn push(&self, branch: &str) -> Result<()> {
        self.run("git", &["push", "-u", "origin", branch])?;
        Ok(())
    }

    fn open_pr(&self, branch: &str, title: &str, body: &str) -> Result<String> {
        self.run(
            "gh",
            &[
                "pr", "create", "--base", &self.base, "--head", branch, "--title", title, "--body",
                body,
            ],
        )
    }
}

/// A no-op [`VcsOps`] that records each call instead of running git/gh. Used by
/// `--dry-run` and by every test, so the verify path can be exercised with no
/// real branch, push, or PR.
#[derive(Debug, Default)]
pub struct DryRunVcs {
    pub log: std::cell::RefCell<Vec<String>>,
}

impl DryRunVcs {
    pub fn new() -> Self {
        Self::default()
    }
    /// The recorded plan (one line per operation), for printing or assertions.
    pub fn plan(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

impl VcsOps for DryRunVcs {
    fn create_branch(&self, branch: &str) -> Result<()> {
        self.log
            .borrow_mut()
            .push(format!("create_branch {branch} (off main)"));
        Ok(())
    }
    fn commit(&self, paths: &[PathBuf], message: &str) -> Result<()> {
        let files: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        self.log
            .borrow_mut()
            .push(format!("commit [{}] -- {message}", files.join(", ")));
        Ok(())
    }
    fn push(&self, branch: &str) -> Result<()> {
        self.log.borrow_mut().push(format!("push {branch}"));
        Ok(())
    }
    fn open_pr(&self, branch: &str, title: &str, _body: &str) -> Result<String> {
        self.log
            .borrow_mut()
            .push(format!("open_pr {branch} -> main :: {title}"));
        Ok(format!("https://example.invalid/DRY-RUN/pr-for/{branch}"))
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Flows
// ────────────────────────────────────────────────────────────────────────────

/// Today's date as an ISO-8601 `YYYY-MM-DD` string, for the `at` field. Uses
/// `chrono::Local` and never panics.
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// The outcome of a verify flow: the branch created and the resulting PR URL.
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub branch: String,
    pub pr_url: String,
    /// The `against` list actually used (after source-derivation), so callers can
    /// report it.
    pub against: Vec<String>,
}

/// Verify a SINGLE rule end-to-end: edit the TOML, then branch + commit + push +
/// PR through `vcs`.
///
/// If `against` is empty, it is prefilled from the rule's `[[sources]]` (their
/// title/linter/url as the version anchor); the resolved list is returned in the
/// outcome.
pub async fn verify_one(
    corpus_dir: &Path,
    rule_id: &str,
    by: &str,
    at: &str,
    against: Vec<String>,
    vcs: &dyn VcsOps,
) -> Result<VerifyOutcome> {
    // Resolve the rule + its path.
    let set = load_corpus(corpus_dir).await?;
    let rule = set
        .get_by_id(rule_id)
        .ok_or_else(|| anyhow!("no rule with id `{rule_id}` in corpus"))?;
    if rule.verification != Verification::Grounded {
        return Err(anyhow!(
            "rule `{rule_id}` is `{}`, not `grounded` — only grounded rules are verified",
            rule.verification
        ));
    }
    let against = if against.is_empty() {
        against_from_sources(&rule.sources)
    } else {
        against
    };
    let path = locate_rule(corpus_dir, rule_id)?;

    let branch = format!("verify/{}", rule_id.to_ascii_lowercase());
    vcs.create_branch(&branch)?;
    apply_verification(&path, by, at, &against)?;
    let message = format!(
        "verify(corpus): mark {rule_id} verified by {by}\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
    );
    vcs.commit(&[path], &message)?;
    vcs.push(&branch)?;
    let title = format!("verify(corpus): {rule_id}");
    let body = format!(
        "Promotes `{rule_id}` from `grounded` to `verified`.\n\nVerified by **{by}** on {at}.\n\nAgainst:\n{}\n",
        against
            .iter()
            .map(|a| format!("- {a}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let pr_url = vcs.open_pr(&branch, &title, &body)?;
    Ok(VerifyOutcome {
        branch,
        pr_url,
        against,
    })
}

/// Bulk self-source the maintainer-authored meta rules: flip every grounded
/// meta-domain rule to `verified` with `against = ["self-sourced: <domain>"]`,
/// batched into ONE branch and ONE PR.
///
/// `domain = Some(d)` scopes to one meta domain; `domain = None` covers all meta
/// domains (the `--all-meta` flow).
pub async fn self_source(
    corpus_dir: &Path,
    domain: Option<&str>,
    by: &str,
    at: &str,
    vcs: &dyn VcsOps,
) -> Result<VerifyOutcome> {
    let targets = self_source_targets(corpus_dir, domain).await?;
    if targets.is_empty() {
        return Err(anyhow!(
            "no grounded meta rules to self-source for {}",
            domain.unwrap_or("all meta domains")
        ));
    }

    let branch = match domain {
        Some(d) => format!("verify/self-source-{}", d.split(':').next().unwrap_or(d)),
        None => "verify/self-source-all-meta".to_owned(),
    };
    vcs.create_branch(&branch)?;

    let mut edited_paths = Vec::new();
    for t in &targets {
        let against = vec![format!("self-sourced: {}", t.domain)];
        apply_verification(&t.path, by, at, &against)?;
        edited_paths.push(t.path.clone());
    }

    let scope = domain.unwrap_or("all meta domains");
    let message = format!(
        "verify(corpus): self-source {} meta rules ({scope}) by {by}\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
        targets.len()
    );
    vcs.commit(&edited_paths, &message)?;
    vcs.push(&branch)?;

    let title = format!("verify(corpus): self-source {} meta rules ({scope})", targets.len());
    let body = format!(
        "Self-sources {} maintainer-authored meta rules ({scope}) to `verified` (against = self-sourced).\n\nVerified by **{by}** on {at}.\n\nRules:\n{}\n",
        targets.len(),
        targets
            .iter()
            .map(|t| format!("- `{}` ({})", t.id, t.domain))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let pr_url = vcs.open_pr(&branch, &title, &body)?;

    Ok(VerifyOutcome {
        branch,
        pr_url,
        against: vec![format!("self-sourced ({} rules)", targets.len())],
    })
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_rules::RuleSet;

    const GROUNDED_FIXTURE: &str = r#"
# A grounded rust rule, with a comment that must survive the edit.
id = "FIXTURE-RULE-1"
title = "A grounded fixture rule"
domain = "rust"
enforcement = "mechanical"
default = true
verification = "grounded"

[[sources]]
url = "https://example.com/clippy"
title = "Clippy docs"
linter = "clippy: some_lint"

[decision]
question = "What position?"
why = "Because reasons."
"#;

    fn write_fixture(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("fixture-rule-1.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    async fn load_one_from_dir(dir: &Path, id: &str) -> camerata_rules::Rule {
        let set: RuleSet = load_corpus(dir).await.unwrap();
        set.get_by_id(id).cloned().unwrap()
    }

    #[tokio::test]
    async fn apply_verification_round_trips_to_verified() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_fixture(tmp.path(), GROUNDED_FIXTURE);

        apply_verification(
            &path,
            "zach",
            "2026-06-20",
            &["clippy 1.83".to_owned(), "Rust Book 2024".to_owned()],
        )
        .unwrap();

        // Round-trip through camerata-rules: the rule now reads as Verified with
        // the provenance we wrote.
        let rule = load_one_from_dir(tmp.path(), "FIXTURE-RULE-1").await;
        assert_eq!(rule.verification, Verification::Verified);
        assert!(rule.is_verified());
        let prov = rule.verified.expect("[verified] table present");
        assert_eq!(prov.by, "zach");
        assert_eq!(prov.at, "2026-06-20");
        assert_eq!(prov.against, vec!["clippy 1.83", "Rust Book 2024"]);
    }

    #[tokio::test]
    async fn apply_verification_preserves_other_fields_and_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_fixture(tmp.path(), GROUNDED_FIXTURE);
        apply_verification(&path, "zach", "2026-06-20", &["clippy 1.83".to_owned()]).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        // The leading comment and other fields survive the targeted edit.
        assert!(after.contains("must survive the edit"));
        assert!(after.contains("[decision]"));
        assert!(after.contains("[[sources]]"));
        assert!(after.contains("Because reasons."));
        // And the sources still parse (round-trip).
        let rule = load_one_from_dir(tmp.path(), "FIXTURE-RULE-1").await;
        assert_eq!(rule.sources.len(), 1);
        assert_eq!(rule.sources[0].title, "Clippy docs");
    }

    #[tokio::test]
    async fn apply_verification_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_fixture(tmp.path(), GROUNDED_FIXTURE);
        let against = ["clippy 1.83".to_owned()];
        apply_verification(&path, "zach", "2026-06-20", &against).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        apply_verification(&path, "zach", "2026-06-20", &against).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second, "re-applying same inputs is a no-op");
    }

    #[tokio::test]
    async fn apply_verification_replaces_stale_against() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_fixture(tmp.path(), GROUNDED_FIXTURE);
        apply_verification(&path, "zach", "2026-06-20", &["old 1.0".to_owned()]).unwrap();
        apply_verification(&path, "zach", "2026-06-21", &["new 2.0".to_owned()]).unwrap();
        let rule = load_one_from_dir(tmp.path(), "FIXTURE-RULE-1").await;
        let prov = rule.verified.unwrap();
        assert_eq!(prov.at, "2026-06-21");
        assert_eq!(prov.against, vec!["new 2.0"], "stale against fully replaced");
    }

    #[tokio::test]
    async fn locate_rule_matches_on_id_not_filename() {
        let tmp = tempfile::tempdir().unwrap();
        // Filename deliberately does NOT contain the id.
        let p = tmp.path().join("zzz.toml");
        std::fs::write(&p, GROUNDED_FIXTURE).unwrap();
        let found = locate_rule(tmp.path(), "FIXTURE-RULE-1").unwrap();
        assert_eq!(found, p);
        assert!(locate_rule(tmp.path(), "NOPE-1").is_err());
    }

    #[tokio::test]
    async fn list_grounded_orders_mechanical_first() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.toml"),
            r#"id="A-PROSE-1"
title="p"
domain="rust"
enforcement="prose"
verification="grounded""#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("b.toml"),
            r#"id="B-MECH-1"
title="m"
domain="rust"
enforcement="mechanical"
verification="grounded""#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("c.toml"),
            r#"id="C-DRAFT-1"
title="d"
domain="rust"
enforcement="mechanical""#,
        )
        .unwrap();
        let rows = list_grounded(tmp.path()).await.unwrap();
        // Only the two grounded rules; draft excluded.
        assert_eq!(rows.len(), 2);
        // Mechanical sorts before prose.
        assert_eq!(rows[0].id, "B-MECH-1");
        assert_eq!(rows[1].id, "A-PROSE-1");
    }

    #[test]
    fn is_meta_domain_recognizes_authored_corpora() {
        assert!(is_meta_domain("agentic"));
        assert!(is_meta_domain("api-layer"));
        assert!(is_meta_domain("permissions:foo"));
        assert!(is_meta_domain("universal"));
        assert!(!is_meta_domain("rust"));
        assert!(!is_meta_domain("python:django"));
    }

    #[tokio::test]
    async fn verify_one_dry_run_records_plan_and_edits_toml() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path(), GROUNDED_FIXTURE);
        let vcs = DryRunVcs::new();
        let outcome = verify_one(
            tmp.path(),
            "FIXTURE-RULE-1",
            "zach",
            "2026-06-20",
            vec![], // empty -> derive from sources
            &vcs,
        )
        .await
        .unwrap();

        // against was prefilled from the single source.
        assert_eq!(outcome.against.len(), 1);
        assert!(outcome.against[0].contains("Clippy docs"));
        assert!(outcome.against[0].contains("clippy: some_lint"));
        assert_eq!(outcome.branch, "verify/fixture-rule-1");
        assert!(outcome.pr_url.contains("DRY-RUN"));

        // The plan is recorded in order: branch, commit, push, pr.
        let plan = vcs.plan();
        assert_eq!(plan.len(), 4);
        assert!(plan[0].starts_with("create_branch verify/fixture-rule-1"));
        assert!(plan[1].starts_with("commit"));
        assert!(plan[2].starts_with("push"));
        assert!(plan[3].starts_with("open_pr"));

        // And the TOML on disk is now verified.
        let rule = load_one_from_dir(tmp.path(), "FIXTURE-RULE-1").await;
        assert_eq!(rule.verification, Verification::Verified);
    }

    #[tokio::test]
    async fn verify_one_refuses_non_grounded() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("d.toml"),
            r#"id="DRAFT-1"
title="d"
domain="rust"
enforcement="mechanical""#,
        )
        .unwrap();
        let vcs = DryRunVcs::new();
        let err = verify_one(tmp.path(), "DRAFT-1", "zach", "2026-06-20", vec![], &vcs)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not `grounded`"));
        assert!(vcs.plan().is_empty(), "no vcs ops on a refused verify");
    }

    #[tokio::test]
    async fn self_source_batches_meta_rules_into_one_pr() {
        let tmp = tempfile::tempdir().unwrap();
        // A rule's `domain` is DERIVED FROM ITS FOLDER PATH relative to the corpus root
        // (see camerata_rules::load_one) — the in-file `domain` is only a cross-check. So each
        // fixture must live in its own domain subfolder, exactly like a real corpus, for the
        // meta-vs-non-meta filtering to be exercised. agentic + permissions are meta domains;
        // rust is NOT, so r2 must be excluded from the self-source batch.
        for (i, dom) in ["agentic", "permissions", "rust"].iter().enumerate() {
            let dir = tmp.path().join(dom);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("r{i}.toml")),
                format!(
                    "id=\"META-{i}\"\ntitle=\"t\"\ndomain=\"{dom}\"\nenforcement=\"structured\"\nverification=\"grounded\""
                ),
            )
            .unwrap();
        }
        let vcs = DryRunVcs::new();
        let outcome = self_source(tmp.path(), None, "zach", "2026-06-20", &vcs)
            .await
            .unwrap();
        assert_eq!(outcome.branch, "verify/self-source-all-meta");

        // ONE branch, ONE commit (with both meta files), ONE push, ONE pr.
        let plan = vcs.plan();
        assert_eq!(plan.len(), 4);
        assert!(plan[1].starts_with("commit"));
        // The rust rule (non-meta) must NOT be in the commit.
        assert!(!plan[1].contains("r2.toml"));

        // Both meta rules are now verified with self-sourced against; rust untouched.
        let agentic = load_one_from_dir(tmp.path(), "META-0").await;
        assert_eq!(agentic.verification, Verification::Verified);
        assert_eq!(
            agentic.verified.unwrap().against,
            vec!["self-sourced: agentic"]
        );
        let rust = load_one_from_dir(tmp.path(), "META-2").await;
        assert_eq!(rust.verification, Verification::Grounded);
    }

    #[tokio::test]
    async fn self_source_rejects_non_meta_domain() {
        let tmp = tempfile::tempdir().unwrap();
        let err = self_source(tmp.path(), Some("rust"), "zach", "2026-06-20", &DryRunVcs::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a maintainer-authored meta domain"));
    }

    #[test]
    fn verified_block_preview_renders_toml() {
        let block = verified_block_preview("zach", "2026-06-20", &["clippy 1.83".to_owned()]);
        assert!(block.contains("[verified]"));
        assert!(block.contains("by = \"zach\""));
        assert!(block.contains("at = \"2026-06-20\""));
        assert!(block.contains("clippy 1.83"));
        // It parses as valid TOML.
        block.parse::<toml_edit::DocumentMut>().unwrap();
    }
}
