//! W6 / I2 — `camerata integrity-gate`, the command you run before you spend money.
//!
//! ```text
//! camerata integrity-gate --ledger <PATH> [--profile graded|smoke]
//! ```
//!
//! Reads a ledger written by `camerata inspect` (which now persists one next to its scan
//! cache — see `inspect_cmd::save_ledger`), runs
//! [`camerata_server::integrity_gate::evaluate`] over it, prints the verdict, and exits:
//!
//! - **`0`** — every required engine ran, every skip is justified, every stage reconciles.
//! - **`1`** — at least one integrity problem. Do not spend a graded run on this scan.
//! - **`2`** — the gate could not run at all (ledger missing or unreadable).
//!
//! The three-way split matters: `2` must never be mistaken for `0`. A wrapper script that
//! treats "the gate could not run" as "the gate passed" reintroduces the exact silent
//! degradation this whole workstream exists to close, one level up, in the gate itself.
//!
//! # Why a separate command rather than a flag on `inspect`
//!
//! The gate's input is a FILE, so the same verdict can be re-derived later, on another
//! machine, or in CI, without re-running (and re-paying for) the scan that produced it. A
//! ledger is a few kB of JSON; a scan is dollars and hours. It also means the gate's
//! decision is auditable after the fact: "why did we spend $26 on that run" has a file to
//! point at.
//!
//! All model-cost logic is deliberately absent. This command does no I/O beyond reading one
//! JSON file and writing to stdout: no network, no model call, no scan.

use std::path::PathBuf;

use camerata_server::integrity_gate::{self, IntegrityGateOptions, IntegrityVerdict};
use camerata_server::scan_ledger::ScanLedger;

/// Exit code for "the gate itself could not run". Deliberately distinct from both `0` and
/// `1` — see the module doc comment.
pub const EXIT_GATE_UNAVAILABLE: i32 = 2;

/// Which strictness profile to evaluate under. Clap-free so the handler and its tests never
/// depend on clap, matching `inspect_cmd::InspectArgs`'s convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateProfile {
    /// The default. Before a graded run: every engine that must run for any repo must have
    /// run, and a scoped-out semantic tier is a blocker.
    Graded,
    /// Structural integrity only, for a cheap local smoke scan. Cannot catch a dark tier.
    Smoke,
}

impl GateProfile {
    pub fn options(self) -> IntegrityGateOptions {
        match self {
            GateProfile::Graded => IntegrityGateOptions::default(),
            GateProfile::Smoke => IntegrityGateOptions::smoke(),
        }
    }
}

/// Parse the `--profile` value. Rejects anything else rather than silently defaulting: a
/// typo'd profile must not quietly relax the gate.
pub fn parse_profile(s: &str) -> Result<GateProfile, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "graded" => Ok(GateProfile::Graded),
        "smoke" => Ok(GateProfile::Smoke),
        other => Err(format!(
            "unknown --profile `{other}` (expected `graded` or `smoke`). Refusing to guess: a \
             typo must never silently relax the gate."
        )),
    }
}

/// `integrity-gate`'s arguments, clap-free.
#[derive(Debug, Clone)]
pub struct IntegrityGateArgs {
    pub ledger: PathBuf,
    pub profile: GateProfile,
}

/// Load a persisted ledger and evaluate it. Returns `Err` ONLY when the gate could not run
/// (the `EXIT_GATE_UNAVAILABLE` case); a FAILING verdict is a successful gate run and comes
/// back as `Ok`.
pub fn run_integrity_gate(args: &IntegrityGateArgs) -> Result<IntegrityVerdict, String> {
    let bytes = std::fs::read(&args.ledger).map_err(|e| {
        format!(
            "could not read the scan ledger at {}: {e}. Run `camerata inspect` first — it \
             writes one next to its scan cache.",
            args.ledger.display()
        )
    })?;
    let ledger: ScanLedger = serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "{} is not a readable scan ledger: {e}",
            args.ledger.display()
        )
    })?;
    Ok(integrity_gate::evaluate(&ledger, &args.profile.options()))
}

/// Render a verdict for stdout. Thin pass-through so the command's output format lives in
/// one place with the gate's own logic.
pub fn format_verdict(verdict: &IntegrityVerdict) -> String {
    integrity_gate::render(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_server::scan_ledger::RuleTier;

    fn write_ledger(dir: &std::path::Path, ledger: &ScanLedger) -> PathBuf {
        let p = dir.join("ledger.json");
        std::fs::write(&p, serde_json::to_vec_pretty(ledger).unwrap()).unwrap();
        p
    }

    /// req_i2: the command's exit semantics, end to end through a real file on disk — a
    /// healthy ledger exits 0.
    #[test]
    fn req_i2_cmd_a_healthy_ledger_exits_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut l = ScanLedger::new();
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 5, 0);
        l.record_rule("TAINT-1", RuleTier::ExternalTool, true, None, 5, 0);
        l.record_rule("AI-1", RuleTier::Semantic, true, None, 5, 0);
        let path = write_ledger(tmp.path(), &l);

        let verdict = run_integrity_gate(&IntegrityGateArgs {
            ledger: path,
            profile: GateProfile::Graded,
        })
        .expect("the gate must run");
        assert!(verdict.passed(), "{:#?}", verdict.failures);
        assert_eq!(verdict.exit_code(), 0);
    }

    /// req_i2: a degraded ledger exits 1 — and this is a SUCCESSFUL gate run (`Ok`), not an
    /// error, so a wrapper can tell "the scan is bad" from "the gate is broken".
    #[test]
    fn req_i2_cmd_a_dark_tier_exits_one_and_is_still_a_successful_gate_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut l = ScanLedger::new();
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 5, 0);
        l.record_rule("AI-1", RuleTier::Semantic, true, None, 5, 0);
        // The external-tool tier never ran: the 41-rule dark-taint-layer shape.
        let path = write_ledger(tmp.path(), &l);

        let verdict = run_integrity_gate(&IntegrityGateArgs {
            ledger: path,
            profile: GateProfile::Graded,
        })
        .expect("a FAILING verdict is a successful gate run, never an Err");
        assert!(!verdict.passed());
        assert_eq!(verdict.exit_code(), 1);
        assert!(format_verdict(&verdict).contains("Do NOT spend a graded run"));
    }

    /// req_i2: a MISSING ledger is `Err` → exit 2, never exit 0. A wrapper that treated "no
    /// ledger" as "gate passed" would reintroduce the silent degradation one level up, inside
    /// the gate itself.
    #[test]
    fn req_i2_cmd_a_missing_ledger_is_gate_unavailable_never_a_pass() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = run_integrity_gate(&IntegrityGateArgs {
            ledger: tmp.path().join("nope.json"),
            profile: GateProfile::Graded,
        })
        .expect_err("a missing ledger must not be a pass");
        assert!(err.contains("could not read"), "{err}");
        assert_ne!(EXIT_GATE_UNAVAILABLE, 0);
        assert_ne!(EXIT_GATE_UNAVAILABLE, 1);
    }

    /// req_i2: an unreadable/corrupt ledger is likewise gate-unavailable, not a pass.
    #[test]
    fn req_i2_cmd_a_corrupt_ledger_is_gate_unavailable_never_a_pass() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ledger.json");
        std::fs::write(&p, b"this is not json").unwrap();
        let err = run_integrity_gate(&IntegrityGateArgs {
            ledger: p,
            profile: GateProfile::Graded,
        })
        .expect_err("a corrupt ledger must not be a pass");
        assert!(err.contains("not a readable scan ledger"), "{err}");
    }

    /// req_i2: `--profile` rejects anything it does not recognise. A typo must never silently
    /// relax the gate to the permissive profile.
    #[test]
    fn req_i2_cmd_an_unknown_profile_is_rejected_never_silently_relaxed() {
        assert_eq!(parse_profile("graded").unwrap(), GateProfile::Graded);
        assert_eq!(parse_profile("  SMOKE ").unwrap(), GateProfile::Smoke);
        let err = parse_profile("smoek").expect_err("a typo must be rejected");
        assert!(err.contains("Refusing to guess"), "{err}");
    }

    /// req_i2: the two profiles genuinely differ on the SAME ledger — otherwise `--profile`
    /// would be decorative, and the smoke profile would be a false reassurance.
    #[test]
    fn req_i2_cmd_the_graded_profile_is_strictly_stronger_than_the_smoke_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut l = ScanLedger::new();
        l.record_rule("SEC-FLOOR-1", RuleTier::Deterministic, true, None, 5, 0);
        let path = write_ledger(tmp.path(), &l);

        let smoke = run_integrity_gate(&IntegrityGateArgs {
            ledger: path.clone(),
            profile: GateProfile::Smoke,
        })
        .unwrap();
        let graded = run_integrity_gate(&IntegrityGateArgs {
            ledger: path,
            profile: GateProfile::Graded,
        })
        .unwrap();

        assert!(smoke.passed(), "{:#?}", smoke.failures);
        assert!(
            !graded.passed(),
            "the graded profile must catch the two engines that never ran"
        );
    }
}
