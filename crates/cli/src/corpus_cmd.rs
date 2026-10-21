//! `camerata corpus` - the operator surface for the corpus staleness watermark.
//!
//! Design: `docs/design/2026-10-20_corpus-staleness-check.md`. Two actions:
//!
//! - `corpus check` runs the staleness nag: which domains are overdue for a sweep, and which
//!   have never been confirmed at all. Non-fatal, read-only.
//! - `corpus confirm <domain> <date>` advances ONE domain's watermark after a human has
//!   reviewed that domain's sweep. This is the ONLY thing that advances a watermark; running a
//!   sweep never does. It writes `crates/rules/CORPUS_FRESHNESS.toml`; the operator commits it.
//! - `corpus lock` regenerates `crates/rules/CORPUS_LOCK.toml` (each rule's `version` and
//!   content hash). It REFUSES, writing nothing, when a rule's content changed without a
//!   `version` bump, so the command cannot launder a missed bump into the lock.

use std::path::Path;

use anyhow::{bail, Context, Result};
use camerata_rules::{
    confirm_domain, corpus_freshness_path, corpus_lock_path, corpus_path, format_staleness_nag,
    load_corpus, load_corpus_lenient, CorpusFreshness, CorpusLock, DEFAULT_CORPUS_PATH,
};
use chrono::Datelike;

/// Top-level domain (the part before any `:`), the pack granularity the watermark tracks.
fn top_level(domain: &str) -> String {
    domain.split(':').next().unwrap_or(domain).to_string()
}

/// The distinct top-level domains the live corpus contains, sorted.
async fn corpus_domains() -> Vec<String> {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    if !errors.is_empty() {
        eprintln!(
            "warning: {} corpus file(s) failed to load: {errors:?}",
            errors.len()
        );
    }
    let mut domains: Vec<String> = corpus.iter().map(|r| top_level(&r.domain)).collect();
    domains.sort();
    domains.dedup();
    domains
}

/// `corpus check`: print the staleness nag (overdue + never-confirmed), or a clean-bill line.
pub async fn run_corpus_check() -> Result<()> {
    let domains = corpus_domains().await;

    let Some(freshness) =
        CorpusFreshness::load(&corpus_freshness_path()).context("parsing CORPUS_FRESHNESS.toml")?
    else {
        println!(
            "No CORPUS_FRESHNESS.toml found. Every domain is unconfirmed:\n  {}",
            domains.join(", ")
        );
        return Ok(());
    };

    let unparseable = freshness.unparseable();
    if !unparseable.is_empty() {
        eprintln!(
            "warning: CORPUS_FRESHNESS.toml has unparseable dates for: {}",
            unparseable.join(", ")
        );
    }

    let today = chrono::Utc::now().date_naive();
    let overdue = freshness.overdue((today.year() as i64, today.month(), today.day()));
    let never = freshness.never_confirmed(&domains);

    match format_staleness_nag(&overdue, &never) {
        Some(msg) => println!("{msg}"),
        None => println!("Corpus is fresh: every domain confirmed within its staleness window."),
    }
    Ok(())
}

/// `corpus confirm <domain> <date>`: advance one domain's watermark to `date` (YYYY-MM-DD).
pub async fn run_corpus_confirm(domain: String, date: String) -> Result<()> {
    // Validate the date shape here so a typo never lands in the file.
    if !is_ymd(&date) {
        bail!("date must be YYYY-MM-DD (got {date:?})");
    }
    // Validate the domain is one the corpus actually contains, so a typo cannot create a
    // watermark for a pack that does not exist.
    let domains = corpus_domains().await;
    if !domains.contains(&domain) {
        bail!(
            "'{domain}' is not a corpus domain. Known domains:\n  {}",
            domains.join(", ")
        );
    }

    confirm_domain(&corpus_freshness_path(), &domain, &date)
        .with_context(|| format!("advancing the '{domain}' watermark"))?;

    println!("Confirmed '{domain}' as reviewed current on {date}.");
    println!("Commit crates/rules/CORPUS_FRESHNESS.toml to record the confirmation.");
    Ok(())
}

/// What a `corpus lock` run changed, for the summary line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LockSummary {
    /// Rules newly entered in the lock.
    pub added: usize,
    /// Rules whose `version` moved up since the lock was last written.
    pub bumped: usize,
    /// Lock entries dropped because the rule no longer exists.
    pub removed: usize,
    /// Rules whose version matches the previous lock.
    pub unchanged: usize,
    /// Whether the file on disk was rewritten (false when it was already current).
    pub wrote: bool,
}

/// `corpus lock`: regenerate `CORPUS_LOCK.toml` for the BUNDLED corpus.
///
/// Deliberately ignores `CAMERATA_CORPUS_PATH`: the lock belongs to the corpus that ships in
/// this repo, and pointing the env override at someone else's corpus must not rewrite it.
pub async fn run_corpus_lock() -> Result<()> {
    let lock_path = corpus_lock_path();
    let summary = lock_corpus_at(Path::new(DEFAULT_CORPUS_PATH), &lock_path).await?;
    println!(
        "Locked {} rule(s): {} new, {} bumped, {} removed, {} unchanged.",
        summary.added + summary.bumped + summary.unchanged,
        summary.added,
        summary.bumped,
        summary.removed,
        summary.unchanged
    );
    if summary.wrote {
        println!("Commit crates/rules/CORPUS_LOCK.toml together with the rule changes.");
    } else {
        println!("CORPUS_LOCK.toml was already current; nothing written.");
    }
    Ok(())
}

/// The testable core of `corpus lock`: load `corpus_dir` STRICTLY, compare against the lock at
/// `lock_path` (if any), and write the regenerated lock only when every transition is allowed.
///
/// Strict load (`load_corpus`, not the lenient one) on purpose: a rule file that fails to parse
/// would otherwise vanish from the corpus, and the regeneration would treat it as a removal and
/// quietly drop its lock entry.
///
/// A malformed existing lock is an error rather than "no lock": regenerating over a lock we
/// cannot read would discard exactly the baseline the refusal logic compares against.
pub async fn lock_corpus_at(corpus_dir: &Path, lock_path: &Path) -> Result<LockSummary> {
    let set = load_corpus(corpus_dir)
        .await
        .with_context(|| format!("loading the corpus at {}", corpus_dir.display()))?;

    let existing = if lock_path.exists() {
        Some(CorpusLock::load(lock_path).with_context(|| {
            format!(
                "reading {} (restore it from git rather than regenerating over it)",
                lock_path.display()
            )
        })?)
    } else {
        None
    };

    let fresh = match CorpusLock::regenerate(&set, existing.as_ref()) {
        Ok(lock) => lock,
        Err(problems) => {
            let lines: Vec<String> = problems.iter().map(|p| format!("  {p}")).collect();
            bail!(
                "refusing to write {}: {} rule(s) cannot be locked.\n{}\nNothing was written.",
                lock_path.display(),
                problems.len(),
                lines.join("\n")
            );
        }
    };

    let mut summary = LockSummary::default();
    let empty = CorpusLock::default();
    let old = existing.as_ref().unwrap_or(&empty);
    for (id, entry) in &fresh.rules {
        match old.rules.get(id) {
            None => summary.added += 1,
            Some(prev) if entry.version > prev.version => summary.bumped += 1,
            Some(_) => summary.unchanged += 1,
        }
    }
    summary.removed = old
        .rules
        .keys()
        .filter(|id| !fresh.rules.contains_key(*id))
        .count();

    let text = fresh.render();
    let current = std::fs::read_to_string(lock_path).ok();
    if current.as_deref() != Some(text.as_str()) {
        std::fs::write(lock_path, text)
            .with_context(|| format!("writing {}", lock_path.display()))?;
        summary.wrote = true;
    }
    Ok(summary)
}

/// Minimal `YYYY-MM-DD` shape check (4-digit year, month 1-12, day 1-31).
fn is_ymd(s: &str) -> bool {
    let mut parts = s.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && matches!(m.parse::<u32>(), Ok(1..=12))
        && matches!(d.parse::<u32>(), Ok(1..=31))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ymd_validation() {
        assert!(is_ymd("2026-10-20"));
        assert!(is_ymd("2026-01-01"));
        assert!(!is_ymd("2026-13-01")); // bad month
        assert!(!is_ymd("2026-10-40")); // bad day
        assert!(!is_ymd("26-10-20")); // short year
        assert!(!is_ymd("2026/10/20")); // wrong separator
        assert!(!is_ymd("last tuesday"));
        assert!(!is_ymd("2026-10-20-1")); // trailing
    }

    // ── corpus lock ─────────────────────────────────────────────────────────

    fn rule_file(id: &str, version: u32, directive: &str) -> String {
        format!(
            r#"id = "{id}"
version = {version}
title = "Fixture {id}"
enforcement = "prose"
kind = "decision"
inspectable = true
default = true
verification = "policy"

[decision]
question = "Which?"
default = "only"

[[option]]
id = "only"
label = "The only option"
directive = "{directive}"
why = "Fixture."
"#
        )
    }

    /// A corpus dir plus a lock path beside it, so the tests never touch the bundled lock.
    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(files: &[(&str, String)]) -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir_all(dir.path().join("corpus/universal")).expect("mkdir");
            let f = Fixture { dir };
            for (name, body) in files {
                f.write_rule(name, body);
            }
            f
        }
        fn corpus(&self) -> std::path::PathBuf {
            self.dir.path().join("corpus")
        }
        fn lock(&self) -> std::path::PathBuf {
            self.dir.path().join("CORPUS_LOCK.toml")
        }
        fn write_rule(&self, name: &str, body: &str) {
            std::fs::write(self.corpus().join("universal").join(name), body).expect("write");
        }
        fn remove_rule(&self, name: &str) {
            std::fs::remove_file(self.corpus().join("universal").join(name)).expect("rm");
        }
    }

    #[tokio::test]
    async fn lock_creates_the_file_when_there_is_none() {
        let f = Fixture::new(&[
            ("a.toml", rule_file("LOCK-A-1", 1, "Do A.")),
            ("b.toml", rule_file("LOCK-B-1", 3, "Do B.")),
        ]);
        let summary = lock_corpus_at(&f.corpus(), &f.lock()).await.expect("locks");
        assert_eq!(
            summary,
            LockSummary {
                added: 2,
                wrote: true,
                ..LockSummary::default()
            }
        );
        let lock = CorpusLock::load(&f.lock()).expect("written file parses");
        assert_eq!(lock.rules.len(), 2);
        assert_eq!(lock.rules["LOCK-B-1"].version, 3);
    }

    #[tokio::test]
    async fn lock_is_idempotent_and_does_not_rewrite_a_current_file() {
        let f = Fixture::new(&[("a.toml", rule_file("LOCK-A-1", 1, "Do A."))]);
        lock_corpus_at(&f.corpus(), &f.lock()).await.expect("first");
        let before = std::fs::read_to_string(f.lock()).expect("read");

        let again = lock_corpus_at(&f.corpus(), &f.lock())
            .await
            .expect("second");
        assert_eq!(
            again,
            LockSummary {
                unchanged: 1,
                wrote: false,
                ..LockSummary::default()
            }
        );
        assert_eq!(std::fs::read_to_string(f.lock()).expect("read"), before);
    }

    #[tokio::test]
    async fn lock_refuses_a_changed_rule_with_no_bump_and_writes_nothing() {
        let f = Fixture::new(&[
            ("a.toml", rule_file("LOCK-A-1", 1, "Do A.")),
            ("b.toml", rule_file("LOCK-B-1", 1, "Do B.")),
        ]);
        lock_corpus_at(&f.corpus(), &f.lock()).await.expect("first");
        let before = std::fs::read_to_string(f.lock()).expect("read");

        // Edit B's directive, leave version = 1.
        f.write_rule("b.toml", &rule_file("LOCK-B-1", 1, "Do B differently."));
        let err = lock_corpus_at(&f.corpus(), &f.lock())
            .await
            .expect_err("a missed bump must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("LOCK-B-1"), "names the rule: {msg}");
        assert!(msg.contains("bump `version`"), "says what to do: {msg}");
        assert!(msg.contains("Nothing was written"), "{msg}");
        assert!(
            !msg.contains("LOCK-A-1"),
            "does not blame an innocent rule: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(f.lock()).expect("read"),
            before,
            "the lock must be untouched after a refusal"
        );
    }

    #[tokio::test]
    async fn lock_accepts_the_change_once_the_version_is_bumped() {
        let f = Fixture::new(&[("b.toml", rule_file("LOCK-B-1", 1, "Do B."))]);
        lock_corpus_at(&f.corpus(), &f.lock()).await.expect("first");

        f.write_rule("b.toml", &rule_file("LOCK-B-1", 2, "Do B differently."));
        let summary = lock_corpus_at(&f.corpus(), &f.lock())
            .await
            .expect("bumped");
        assert_eq!(
            summary,
            LockSummary {
                bumped: 1,
                wrote: true,
                ..LockSummary::default()
            }
        );
        let lock = CorpusLock::load(&f.lock()).expect("parses");
        assert_eq!(lock.rules["LOCK-B-1"].version, 2);
    }

    #[tokio::test]
    async fn lock_records_additions_and_removals() {
        let f = Fixture::new(&[
            ("a.toml", rule_file("LOCK-A-1", 1, "Do A.")),
            ("b.toml", rule_file("LOCK-B-1", 1, "Do B.")),
        ]);
        lock_corpus_at(&f.corpus(), &f.lock()).await.expect("first");

        f.remove_rule("b.toml");
        f.write_rule("c.toml", &rule_file("LOCK-C-1", 1, "Do C."));
        let summary = lock_corpus_at(&f.corpus(), &f.lock()).await.expect("again");
        assert_eq!(
            summary,
            LockSummary {
                added: 1,
                removed: 1,
                unchanged: 1,
                wrote: true,
                ..LockSummary::default()
            }
        );
        let lock = CorpusLock::load(&f.lock()).expect("parses");
        let ids: Vec<&str> = lock.rules.keys().map(String::as_str).collect();
        assert_eq!(ids, vec!["LOCK-A-1", "LOCK-C-1"]);
    }

    #[tokio::test]
    async fn lock_refuses_a_rule_with_no_version() {
        let unversioned = rule_file("LOCK-A-1", 1, "Do A.").replace("version = 1\n", "");
        let f = Fixture::new(&[("a.toml", unversioned)]);
        let err = lock_corpus_at(&f.corpus(), &f.lock())
            .await
            .expect_err("unversioned rule cannot be locked");
        assert!(format!("{err:#}").contains("LOCK-A-1"));
        assert!(!f.lock().exists(), "nothing written on refusal");
    }

    #[tokio::test]
    async fn lock_refuses_to_regenerate_over_a_corrupt_lock() {
        let f = Fixture::new(&[("a.toml", rule_file("LOCK-A-1", 1, "Do A."))]);
        std::fs::write(f.lock(), "this is [not a lock").expect("write");
        let err = lock_corpus_at(&f.corpus(), &f.lock())
            .await
            .expect_err("a corrupt lock is not 'no lock'");
        assert!(format!("{err:#}").contains("restore it from git"));
        assert_eq!(
            std::fs::read_to_string(f.lock()).expect("read"),
            "this is [not a lock"
        );
    }

    #[tokio::test]
    async fn lock_refuses_a_corpus_with_a_file_that_does_not_parse() {
        // A lenient load would drop the broken file and the run would treat it as a removal.
        let f = Fixture::new(&[
            ("a.toml", rule_file("LOCK-A-1", 1, "Do A.")),
            ("broken.toml", "id = [unclosed".to_string()),
        ]);
        assert!(lock_corpus_at(&f.corpus(), &f.lock()).await.is_err());
        assert!(!f.lock().exists());
    }

    #[test]
    fn top_level_strips_subdomain() {
        assert_eq!(top_level("rust:seaorm"), "rust");
        assert_eq!(top_level("supabase:auth"), "supabase");
        assert_eq!(top_level("universal"), "universal");
    }
}
