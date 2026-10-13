//! Requirement traceability gate.
//!
//! TEST DISCIPLINE item 2, decided by Zach on 2026-10-09 and recorded in
//! `docs/plans/2026-10-09_audit-findings-tracker.md`:
//!
//! > "A traceability check fails the build if any requirement has **zero** tests referencing it."
//!
//! The motivating fear, in his words: *"I don't want code that confidently thinks it's doing what
//! is written here in the product requirements, but actually isn't."*
//!
//! ## What this gate can and cannot prove
//!
//! It proves a requirement is **referenced** by at least one test. It does NOT prove the test is
//! adequate. The E2 severity-ceiling mechanism is the cautionary example: `max_severity`,
//! `clamp_to_rule_max_severity` and every `req_e2_*` test shipped green against a corpus in which
//! **zero rules authored a ceiling**, so the whole mechanism was inert while fully "traced". This
//! gate would not have caught that. Adequacy is the fail-before-the-fix rule (item 4), not this.
//!
//! ## Why a registry rather than scraping the tracker
//!
//! Parsing requirement ids out of a prose markdown document makes the gate hostage to that
//! document's formatting, and a gate that breaks when a heading is reworded gets disabled. The
//! registry below is explicit and reviewable, and adding a requirement without a `Coverage`
//! decision is itself a compile-visible omission.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// How a tracker requirement is expected to relate to the test suite.
enum Coverage {
    /// At least one test function whose name starts with one of these prefixes must exist.
    Required(&'static [&'static str]),
    /// Owned by a workstream that has not landed, or not yet owned by any workstream. Reported
    /// loudly on every run but does NOT fail the build. This list must be EMPTY before Pass 1 is
    /// declared complete; it is a visible countdown, not a permanent allowlist.
    InFlight(&'static str),
    /// Not a code requirement, or deliberately not implemented with the reason recorded in the
    /// tracker. Exempt entries must cite that reason here so the exemption is auditable.
    Exempt(&'static str),
    /// Traceability is ACTIVELY BROKEN for this requirement: its natural test prefix is already
    /// occupied by tests that measure something else, so a prefix-matching gate reports it as
    /// covered when it is not. This is strictly worse than zero tests, because zero tests is at
    /// least honest. The fix is always to rename the offending tests, never to widen the registry.
    Collision(&'static str),
}

use Coverage::{Collision, Exempt, InFlight, Required};

/// Every requirement id the tracker declares, and its coverage expectation.
///
/// Note on ids: `req_d1_*` / `req_d2_*` are W1's own ids for the sealed-row-set work and legitimately
/// cover B1/B2/B3 and B4 respectively. That indirection is recorded here rather than renamed,
/// because the owning file is held by a concurrent workstream.
const REGISTRY: &[(&str, Coverage)] = &[
    // ── AUDIT A — architecture coverage ────────────────────────────────────────────────────────
    ("A1", Exempt("analysis finding: the 'rules not armed for this stack' hypothesis was REFUTED. \
                   There is nothing to implement, and a test asserting a refuted hypothesis would \
                   assert current behaviour is correct, which is not a requirement.")),
    ("A3", Required(&["req_a3"])),
    ("A4", Required(&["req_a4"])),
    ("A5", Required(&["req_a5"])),
    ("A5.1", Required(&["req_a5_1"])),
    ("A5.2", Required(&["req_a5_2"])),
    ("A5.3", Required(&["req_a5_3"])),
    ("A5.4", Required(&["req_a5_4"])),
    ("A5.5", Required(&["req_a5_5"])),
    ("A5.6", Required(&["req_a5_6"])),
    ("A5.7", Exempt("the 58 ambiguous rule classifications are ON HOLD at Zach's explicit request \
                     ('pause the rule review for me for when I can sit down and look at these'). \
                     No code may depend on a classification he has not made.")),
    ("A6", Required(&["req_a6"])),
    ("A7", Required(&["req_a7", "req_h1"])),
    ("A8", Required(&["req_a8"])),
    ("A9", InFlight("unassigned: our export taxonomy still has no 'architecture' family; `category` \
                     is the slugified rule domain. W4's A3 fix corrected the tier LABEL but not the \
                     export taxonomy.")),
    ("A10", Exempt("a per-class verdict tally over the audited plants (1 not-armed, 4 \
                    predicate-mismatch, 3 no-rule, 2 combination). A measurement, not a behaviour.")),
    // ── AUDIT B — artifact parity, hedge counts, merge ─────────────────────────────────────────
    ("B1", Required(&["req_d1"])),
    ("B4", Required(&["req_d2"])),
    ("B5", Exempt("marked UNDER-EVIDENCED by the independent review, which could not reconcile \
                   'post-merge delta == semgrep row count' against the ledgers. Its instruction was \
                   explicit: 'Plausible but unproven. Do not build on it.'")),
    ("B6", Exempt("the credential keyword list is BANNED as a fix surface. The review: 'the detector \
                   that matched the secret already KNOWS its identity and should emit it as a \
                   structured capture.' Covered by the structured-identity work, not by a list.")),
    ("B7", Required(&["req_b7"])),
    ("B8", Required(&["req_b8"])),
    ("B9", Required(&["req_b9"])),
    ("B10", Required(&["req_d1"])),
    ("B11", Required(&["req_d1"])),
    // ── AUDIT C — severity, decoys, ranking ────────────────────────────────────────────────────
    ("C1", Collision("finding C1 is that both regressed classes have no deterministic detector, so \
                      detection is a single LLM sample and run-to-run variance is by design. The \
                      existing `req_c1_*` tests in report_export.rs do NOT test that at all -- they \
                      test row-addition ACCOUNTING (W1's conservation work). So C1 reads as covered \
                      to any prefix-matching gate while having no tests, and the accounting tests \
                      are untraceable to the requirement they actually serve. Fix: rename them to \
                      the conservation requirement they belong to. Not done here because \
                      report_export.rs is held by a concurrent workstream.")),
    ("C2", Required(&["req_c2"])),
    ("C3", Exempt("`evidence_negates_requirement`'s six-phrase substring scan is on the review's \
                   watch list: 'Do NOT tune the evidence gate's six phrases; fixing A5 at the corpus \
                   demotes it to belt-and-braces.' A5.4 landed, so it is now the backstop it should \
                   always have been.")),
    ("C4", Required(&["req_c4"])),
    ("C5", Required(&["req_c5", "req_i1_c5"])),
    ("C6", Required(&["req_c6"])),
    ("C7", Exempt("the D5 severity floor reads model prose and is class-blind and raise-only. \
                   Deliberately deferred on the review's judgement that once structured identity and \
                   the severity ceiling land, 'most layers become DELETABLE, not rewritable.' \
                   Rewriting it now would be work thrown away.")),
    ("C8", Required(&["req_e2"])),
    ("C9", InFlight("W7: top-3 distinctness keys on the same credential allowlist, and empty keys \
                     short-circuit the guard to false, so no distinctness check applies.")),
    ("C10", Exempt("STRUCK AS WRONG by the independent review. The test concerns effort-total \
                    labelling and its three criticals are three DISTINCT rules in three DISTINCT \
                    files, so asserting 3 items is correct behaviour, not a codified bug.")),
    ("C11", Required(&["req_c11"])),
    ("C12", Exempt("a synthesis claim ('the remedy is a different division of labour'), and the \
                    review judged its 'all three outcomes are interaction failures' framing to be \
                    overreach. A conclusion, not a behaviour.")),
    ("C13", Exempt("the paraphrase-invariance gate is the right ACCEPTANCE test only after \
                    structured fields exist; the review called it 'useless as a first measurement \
                    (it would fail trivially today and teach nothing)'.")),
    // ── Owner-decided requirement groups ───────────────────────────────────────────────────────
    ("E2", Required(&["req_e2"])),
    // W5-Q2, ruled 2026-10-11: "Floor, it's the minimum rating." E2's symmetric other half —
    // rules may declare a MINIMUM severity, and no report may rank below it. Covered by W8's
    // `req_w8_*` tests (the floor mechanism, the corpus-load gate, the anti-inertness gate, the
    // held-rule allow-list, and `tests/real_corpus_severity_floor_e2e.rs` against the real
    // corpus). Registered under the ruling's own id because the tracker has no requirement-group
    // letter for floors; same recorded-indirection pattern as B1/B4 -> `req_d1`/`req_d2`.
    ("W5-Q2", Required(&["req_w8"])),
    // W5-Q4, ruled 2026-10-11: "yes, widen these tests." The pre-existing `req_e2_*` tests read
    // only `curated_findings` and therefore panicked on, rather than asserting over, the rules
    // whose severity bound is most emphatically authored. Fixed in place under test-discipline
    // exception (a) — the widened tests keep their own `req_e2` ids, so this entry records the
    // ruling rather than claiming a separate test prefix.
    // W5-Q3, ruled 2026-10-11: "yes these should match." The ~20 per-stack mirrors of the
    // universal architectural spine inherit the spine rule's `deviation_ceiling = "high"`,
    // because severity must describe the PROBLEM rather than which rule happened to fire.
    // Asserted per-mirror (never by folder) with an attribution control, so a blanket
    // assignment fails the gate.
    ("W5-Q3", Required(&["req_w5_q3"])),
    ("W5-Q4", Required(&["req_e2", "req_w8"])),
    // G3's tests carry W7's `req_g1`/`req_g1_g2` ids (the coverage-honesty group); the
    // conciseness half Zach specified is asserted directly — exactly ONE line per affected
    // tool, and never the notes' internal diagnostics. Same recorded-indirection pattern as
    // B1/B4 -> `req_d1`/`req_d2` above.
    ("G3", Required(&["req_g1"])),
    ("H1", Required(&["req_h1", "req_a6"])),
    ("I1", Required(&["req_i1"])),
    ("I2", Required(&["req_i2"])),
    // W13, found 2026-10-11: every PRODUCTION corpus load was lenient AND discarded the parse
    // errors (`let (corpus, _errs) = load_corpus_lenient(..)`), while several test files bound
    // and asserted on them. So a rule file with a typo'd enum value failed to parse, vanished
    // from every scan, and nothing reported it — not the ledger, not the coverage sheet, not
    // `camerata integrity-gate`, not a log line. A second route to the H1 outcome, and one that
    // walks under the gate built to stop it: a rule that never loaded is not a skip, it is
    // absent from the ledger's universe. Registered under its workstream id because the tracker
    // has no requirement letter for corpus-load integrity; same recorded-indirection pattern as
    // B1/B4 -> `req_d1`/`req_d2` and W5-Q2 -> `req_w8`.
    ("W13", Required(&["req_w13"])),
    ("W14", Required(&["req_w14"])),
    // W8-Q4, found 2026-10-11: the D5 severity floor matched its phrase tables against the
    // MODEL'S PROSE with a bare `text.contains(phrase)`, and the tables contain one-syllable
    // entries ("read", "view", "list", "see", "rls"). So al`read`y, re`view`, allow`list`,
    // `each` transfer, `password`less and signed-u`rls` all tripped the floor: measured, prose
    // containing "another user's ... already ..." floored to High, and the bare word
    // "credential" anywhere in the same text took it to Critical. The badged-CRITICAL
    // `ARCH-STRUCTURED-ERRORS-1` case is this bug, not a separate quirk. Fixed with word
    // boundaries plus a closed inflection set (`contains_phrase_as_word`), the same fix shape
    // as W7/B9's `dotted_run_is_identifier`; no phrase table changed. Registered under its own
    // id rather than folded into `req_w8`, because W5-Q2 already claims that prefix for the
    // floor MECHANISM and this is the floor's MATCHING — same recorded-indirection pattern as
    // B1/B4 -> `req_d1`/`req_d2`.
    ("W8-Q4", Required(&["req_w8_q4"])),
    // ── Pass 3 — CLIENT DELIVERABLES ───────────────────────────────────────────────────────────
    // Zach's three-pass sequence: "PDF = the summary ... Excel = the meat and potatoes. Needs
    // rethinking: separate worksheets for findings, conventions, deviations, coverage ... JSON =
    // just the data structure." Only the Excel half is a code requirement: the PDF is "already
    // correct in shape; leave it alone" and the JSON is "fine as-is; no work needed", so neither
    // has a behaviour to assert that is not already covered elsewhere.
    ("PASS3", Required(&["req_pass3"])),
    // ── Routed operational decisions ───────────────────────────────────────────────────────────
    ("R1", Exempt("the repo-wide `cargo fmt` decision is a topology-level change routed to Zach. \
                   Not a testable product behaviour.")),
    ("R2", Exempt("adding the never-run-a-formatter directive as a Camerata custom rule for \
                   Camerata's own repo. A governance change routed to Zach, not a code behaviour.")),
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/server`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/server should sit two levels below the workspace root")
        .to_path_buf()
}

/// Every `fn req_*` test name declared anywhere under `crates/`.
fn collect_requirement_test_names(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut stack = vec![root.join("crates")];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            // A missing or unreadable directory must not silently shrink the evidence set.
            Err(e) => panic!("could not read {}: {e}", dir.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `target` can be gigabytes of build output and contains no first-party tests.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let src = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
                for (_, rest) in src.match_indices("fn req_").map(|(i, _)| (i, &src[i + 3..])) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        names.insert(name);
                    }
                }
            }
        }
    }
    names
}

/// A requirement is covered when some test name starts with one of its prefixes, and the character
/// after the prefix is `_` or end-of-name. The boundary check is what stops `req_a5_1` from
/// satisfying a lookup for `req_a5_11`, and `req_a5_6` from counting as `req_a5`.
fn is_covered(prefix: &str, names: &BTreeSet<String>) -> bool {
    names.iter().any(|n| {
        n.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
    })
}

#[test]
fn req_traceability_every_required_requirement_has_at_least_one_test() {
    let names = collect_requirement_test_names(&workspace_root());
    assert!(
        !names.is_empty(),
        "found zero `fn req_*` tests in the whole workspace — the collector is broken, which would \
         make this gate vacuously pass"
    );

    let mut uncovered = Vec::new();
    for (id, coverage) in REGISTRY {
        if let Required(prefixes) = coverage {
            if !prefixes.iter().any(|p| is_covered(p, &names)) {
                uncovered.push(format!("  {id} — expected a test named {prefixes:?}"));
            }
        }
    }

    assert!(
        uncovered.is_empty(),
        "{} requirement(s) marked Required have ZERO tests referencing them:\n{}\n\n\
         Either write the test, or move the requirement to InFlight/Exempt with a recorded reason. \
         Do not widen a prefix to make this pass.",
        uncovered.len(),
        uncovered.join("\n")
    );
}

/// Not a failure today, by design — but the InFlight list is a countdown that must reach zero
/// before Pass 1 is declared complete, so it reports itself on every run.
#[test]
fn req_traceability_in_flight_requirements_are_reported_not_silently_tolerated() {
    let names = collect_requirement_test_names(&workspace_root());
    let pending: Vec<&str> = REGISTRY
        .iter()
        .filter_map(|(id, c)| matches!(c, InFlight(_)).then_some(*id))
        .collect();

    // An InFlight requirement that HAS acquired tests should be promoted to Required, otherwise the
    // list silently rots into a permanent allowlist — the exact antipattern that produced (and was
    // deleted with) `KNOWN_PRE_W1_MECHANICAL_GAPS`.
    let promotable: Vec<&str> = REGISTRY
        .iter()
        .filter_map(|(id, c)| match c {
            InFlight(_) => {
                let prefix = format!("req_{}", id.to_lowercase().replace('.', "_"));
                is_covered(&prefix, &names).then_some(*id)
            }
            _ => None,
        })
        .collect();

    let collisions: Vec<&str> = REGISTRY
        .iter()
        .filter_map(|(id, c)| matches!(c, Collision(_)).then_some(*id))
        .collect();

    eprintln!(
        "requirement traceability: {} Required covered · {} InFlight pending {:?} · {} id COLLISION(S) {:?}",
        REGISTRY.iter().filter(|(_, c)| matches!(c, Required(_))).count(),
        pending.len(),
        pending,
        collisions.len(),
        collisions
    );

    assert!(
        promotable.is_empty(),
        "these requirements are marked InFlight but now HAVE tests — promote them to Required so \
         the gate actually guards them: {promotable:?}"
    );
}

#[test]
fn req_traceability_the_registry_itself_is_well_formed() {
    let mut seen = BTreeSet::new();
    for (id, coverage) in REGISTRY {
        assert!(seen.insert(*id), "duplicate requirement id in the registry: {id}");
        match coverage {
            Required(prefixes) => assert!(
                !prefixes.is_empty(),
                "{id} is Required with no prefixes, which would make it vacuously covered"
            ),
            // An exemption without a stated reason is indistinguishable from giving up quietly.
            InFlight(reason) | Exempt(reason) | Collision(reason) => assert!(
                reason.len() > 30,
                "{id}'s reason is too short to be auditable: {reason:?}"
            ),
        }
    }
}
