//! What the release gate reports: the checks, the problems, and the fail-loud message.
//!
//! Nothing in this file can carry a secret value. Every [`Problem`] is built through
//! [`Problem::new`], which redacts its own text, so even a file name or a JSON key that is
//! itself secret-shaped, or a bug that hands in a raw value, cannot reach [`GateFailure`]'s
//! `Display` or the HTTP body.

use serde::Serialize;

/// The gate's checks, in the order they run and are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    /// No secret value in any client-visible byte.
    Secrets,
    /// Exactly the promised files and datasets are in the package.
    DatasetSet,
    /// Every dataset's CSV agrees with its JSON; `findings.json` agrees with `all-findings`.
    DatasetParity,
    /// The PDF's rows and `findings.json`'s rows are the same set.
    RowParity,
    /// Every manifest hash equals the archive member's hash; every member is listed.
    Manifest,
    /// The README's provenance lines equal the manifest's.
    Provenance,
}

impl CheckId {
    /// Every check, in run order.
    pub const ALL: [CheckId; 6] = [
        CheckId::Secrets,
        CheckId::DatasetSet,
        CheckId::DatasetParity,
        CheckId::RowParity,
        CheckId::Manifest,
        CheckId::Provenance,
    ];

    /// The name the operator reads (`[secrets]`, and the README's "Release checks" line).
    pub fn label(self) -> &'static str {
        match self {
            CheckId::Secrets => "secrets",
            CheckId::DatasetSet => "dataset set",
            CheckId::DatasetParity => "dataset parity",
            CheckId::RowParity => "row parity",
            CheckId::Manifest => "manifest",
            CheckId::Provenance => "provenance",
        }
    }
}

/// One fail-loud line. Never carries a secret: `evidence` is the redacted form only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The file the problem is in (`findings.json`, `camerata-...-findings.xlsx`, `PDF data`).
    pub file: String,
    /// Where in that file (`findings[0].fix_specific`, `sheet "All Findings" cell V2`).
    pub location: String,
    /// What is wrong, in a sentence (`Supabase secret key present in a client file`).
    pub message: String,
    /// The offending value in REDACTED form (`sb_secret_…WvXy (51 chars)`), when there is one.
    pub evidence: Option<String>,
}

impl Problem {
    /// Build a problem. Every field is redacted here, once, so no caller has to remember to.
    pub fn new(
        file: impl AsRef<str>,
        location: impl AsRef<str>,
        message: impl AsRef<str>,
        evidence: Option<String>,
    ) -> Self {
        let clean = |text: &str| camerata_secrets::redact_str(text).into_owned();
        Self {
            file: clean(file.as_ref()),
            location: clean(location.as_ref()),
            message: clean(message.as_ref()),
            evidence: evidence.map(|e| clean(&e)),
        }
    }
}

/// One check's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckOutcome {
    pub check: CheckId,
    pub problems: Vec<Problem>,
}

/// Which checks ran, in order, and what each found (all empty on success). Written to the
/// operator log and returned on success so an operator can see the gate ran.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GateReport {
    pub checks: Vec<CheckOutcome>,
}

impl GateReport {
    /// Record a check's problems. A check recorded twice (a pass over the package, then a pass
    /// over the archive) has its problems merged into the one outcome.
    pub fn record(&mut self, check: CheckId, problems: Vec<Problem>) {
        match self.checks.iter_mut().find(|o| o.check == check) {
            Some(existing) => existing.problems.extend(problems),
            None => self.checks.push(CheckOutcome { check, problems }),
        }
    }

    /// The outcomes in [`CheckId::ALL`] order, whatever order they were recorded in.
    pub fn sorted(mut self) -> Self {
        self.checks
            .sort_by_key(|o| CheckId::ALL.iter().position(|c| *c == o.check));
        self
    }

    /// Every problem, with the check that raised it.
    pub fn problems(&self) -> impl Iterator<Item = (CheckId, &Problem)> {
        self.checks
            .iter()
            .flat_map(|o| o.problems.iter().map(move |p| (o.check, p)))
    }

    pub fn problem_count(&self) -> usize {
        self.checks.iter().map(|o| o.problems.len()).sum()
    }

    pub fn is_clean(&self) -> bool {
        self.problem_count() == 0
    }

    /// The names of the checks that ran, for the success summary.
    pub fn check_names(&self) -> Vec<&'static str> {
        self.checks.iter().map(|o| o.check.label()).collect()
    }

    /// The HTTP body's `problems` array: `{check, file, location, message, evidence}`.
    pub fn problems_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.problems()
                .map(|(check, p)| {
                    serde_json::json!({
                        "check": check.label(),
                        "file": p.file,
                        "location": p.location,
                        "message": p.message,
                        "evidence": p.evidence,
                    })
                })
                .collect(),
        )
    }
}

/// The export was refused. Its `Display` is the operator-facing block (stderr for the CLI, the
/// HTTP body's `message`, the cockpit dialog).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateFailure {
    pub stem: String,
    pub report: GateReport,
}

impl GateFailure {
    /// The filename stem, redacted. The stem is derived from the repository name and commit, so
    /// it is client-controlled text like any other: a secret-shaped repo or branch name must not
    /// reach the failure block or the HTTP body through the one field no [`Problem`] wraps.
    pub fn stem_for_display(&self) -> String {
        camerata_secrets::redact_str(&self.stem).into_owned()
    }
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.report.problem_count();
        let noun = if count == 1 { "problem" } else { "problems" };
        writeln!(
            f,
            "EXPORT BLOCKED by the release gate for {} ({count} {noun})",
            self.stem_for_display()
        )?;
        for (check, p) in self.report.problems() {
            writeln!(
                f,
                "  [{:<12}] {} \u{203a} {}",
                check.label(),
                p.file,
                p.location
            )?;
            match &p.evidence {
                Some(evidence) => writeln!(f, "                 {}: {evidence}", p.message)?,
                None => writeln!(f, "                 {}", p.message)?,
            }
        }
        write!(
            f,
            "Nothing was written. Any value above is shown redacted; the finding's own file:line \
             names where it lives in the client's code. Fix the producer, rescan or re-export, \
             and the gate runs again."
        )
    }
}

impl std::error::Error for GateFailure {}
