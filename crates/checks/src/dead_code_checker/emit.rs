//! Turning the dead set into findings: the per-file message forms, the directory roll-up and
//! the stable ordering. Messages are in the client's voice: they name no product and state a
//! fact the reader can confirm with one `grep`.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use crate::arch_checker::{ArchViolation, ViolationGrade, SEVERITY_MEDIUM};

use super::{Family, RULE_DEAD_CODE};

/// How many paths a message names before saying "and N more".
const SHOWN: usize = 3;

/// One file the scan proved unreachable, with the evidence the message needs.
#[derive(Debug, Clone)]
pub(super) struct DeadFile {
    pub(super) path: String,
    pub(super) family: Family,
    /// What a `grep` for the file's name would search for (see `mentions::key_of`).
    pub(super) key: String,
    /// Files that import it (every one is itself unreachable).
    pub(super) importers: Vec<String>,
    /// Other files whose text mentions its name but do not import it (all unreachable).
    pub(super) mentioners: Vec<String>,
}

/// Findings for `dead`, rolled up by directory where a whole folder is dead. `sources` is every
/// source-language file in the scan (emitted twins excluded): a folder is rolled up only when
/// ALL of its source files are in `dead`, so "every source file under it is unreachable" is
/// literally true, and `live_assets` (non-source files, such as a stylesheet or a data file, that
/// a live file names) are all outside it, so "delete the folder" is never said about a folder
/// holding something in use.
pub(super) fn findings(
    dead: &[DeadFile],
    sources: &BTreeSet<String>,
    live_assets: &BTreeSet<String>,
) -> Vec<ArchViolation> {
    let by_path: BTreeMap<&str, &DeadFile> = dead.iter().map(|d| (d.path.as_str(), d)).collect();
    let mut out = Vec::new();
    let mut rolled: BTreeSet<&str> = BTreeSet::new();

    // Shallowest folder first, so the outermost fully dead folder wins and a nested one is
    // already claimed. The repository root is never rolled up.
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for d in dead {
        let mut dir = d.path.as_str();
        while let Some(i) = dir.rfind('/') {
            dir = &dir[..i];
            dirs.insert(dir);
        }
    }
    let mut ordered: Vec<&str> = dirs.into_iter().collect();
    ordered.sort_by_key(|d| (d.matches('/').count(), *d));
    for dir in ordered {
        let prefix = format!("{dir}/");
        let under: Vec<&str> = sources
            .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
            .take_while(|p| p.starts_with(&prefix))
            .map(String::as_str)
            .collect();
        if under.len() < 2
            || under.iter().any(|p| rolled.contains(p))
            || !under.iter().all(|p| by_path.contains_key(p))
            || live_assets.iter().any(|a| a.starts_with(&prefix))
        {
            continue;
        }
        rolled.extend(under.iter().copied());
        out.push(roll_up(dir, &under));
    }
    for d in dead {
        if !rolled.contains(d.path.as_str()) {
            out.push(per_file(d));
        }
    }
    out.sort_by(|a, b| {
        (a.file.as_str(), a.line, a.message.as_str()).cmp(&(
            b.file.as_str(),
            b.line,
            b.message.as_str(),
        ))
    });
    out
}

fn violation(file: &str, line: usize, object: String, message: String) -> ArchViolation {
    ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_DEAD_CODE.to_string(),
        file: file.to_string(),
        line,
        object: Some(object),
        message,
        // The corpus rule's `max_severity = "low"` clamps this at report time; the checker
        // contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    }
}

fn roll_up(dir: &str, under: &[&str]) -> ArchViolation {
    let message = format!(
        "Every source file under `{dir}/` ({} files: {}) is unreachable: nothing imports into \
         the folder, no framework, build tool, manifest or test loads it, and no reachable file \
         mentions any of their names. Delete the folder, or document why it is kept with a \
         `keep-file:` comment in the first lines of its files.",
        under.len(),
        quoted_list(under),
    );
    violation(under[0], 0, format!("{dir}/"), message)
}

fn per_file(d: &DeadFile) -> ArchViolation {
    let message = match d.family {
        Family::Rust => rust_message(d),
        Family::Ecma | Family::Python => source_message(d),
    };
    violation(&d.path, 1, d.path.clone(), message)
}

const NOT_AN_ENTRY_POINT: &str = "is not an entry point that a framework, build tool, manifest \
                                  or test loads by convention";

fn source_message(d: &DeadFile) -> String {
    let (f, key) = (&d.path, &d.key);
    if !d.importers.is_empty() {
        let (verb, them) = if d.importers.len() == 1 {
            ("is", "that file")
        } else {
            ("are", "those files")
        };
        return format!(
            "`{f}` is imported only by {}, which {verb} itself unreachable (see {} \
             finding), and its name `{key}` appears nowhere outside the unreachable files. \
             Delete it along with {them}, or keep them and document why with a `keep-file:` \
             comment.",
            quoted_list(&refs(&d.importers)),
            if d.importers.len() == 1 {
                "that"
            } else {
                "each"
            },
        );
    }
    if !d.mentioners.is_empty() {
        let verb = if d.mentioners.len() == 1 { "is" } else { "are" };
        return format!(
            "`{f}` is not imported by any file in the repository, {NOT_AN_ENTRY_POINT}, and \
             its name `{key}` appears only in {}, which {verb} itself unreachable. Delete the \
             file, or document why it is kept with a `keep-file:` comment in its first lines.",
            quoted_list(&refs(&d.mentioners)),
        );
    }
    format!(
        "`{f}` is not imported by any file in the repository, {NOT_AN_ENTRY_POINT}, and its \
         name `{key}` appears nowhere else in the repository. Delete the file. If it is loaded \
         in a way the repository does not show, say how in a `keep-file:` comment in its first \
         lines so the reason travels with the code."
    )
}

fn rust_message(d: &DeadFile) -> String {
    let (f, key) = (&d.path, &d.key);
    if !d.importers.is_empty() {
        let (see, them) = if d.importers.len() == 1 {
            ("that finding", "that file")
        } else {
            ("each finding", "those files")
        };
        return format!(
            "`{f}` is declared only by {}, which `cargo` never compiles (see {see}), and no \
             `#[path]` attribute, `include!` or Cargo target names it. Its name `{key}` appears \
             nowhere outside the unreachable files. Delete it along with {them}, or add the \
             `mod` declaration that was meant to bring them into the build.",
            quoted_list(&refs(&d.importers)),
        );
    }
    let name_clause = if d.mentioners.is_empty() {
        format!("Its name `{key}` appears nowhere else in the repository.")
    } else {
        format!(
            "Its name `{key}` appears only in {}, which is not compiled either.",
            quoted_list(&refs(&d.mentioners)),
        )
    };
    format!(
        "`{f}` is not declared by any `mod` statement in its crate, and no `#[path]` \
         attribute, `include!` or Cargo target names it, so `cargo` never compiles it. \
         {name_clause} Delete the file, or add the `mod` declaration that was meant to bring \
         it into the build."
    )
}

fn refs(items: &[String]) -> Vec<&str> {
    items.iter().map(String::as_str).collect()
}

/// `` `a`, `b`, `c` and 2 more `` (everything, comma-separated, when there are few).
fn quoted_list(items: &[&str]) -> String {
    let shown: Vec<String> = items.iter().take(SHOWN).map(|i| format!("`{i}`")).collect();
    let mut text = shown.join(", ");
    let more = items.len().saturating_sub(SHOWN);
    if more > 0 {
        text.push_str(&format!(" and {more} more"));
    }
    text
}
