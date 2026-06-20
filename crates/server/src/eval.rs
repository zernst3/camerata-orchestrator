//! Precision/recall eval harness for the audit's DETERMINISTIC FLOOR (issue #11).
//!
//! The floor is the three pure content rules the brownfield audit always runs —
//! `SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`, `ARCH-NO-SECRETS-IN-URL-1`
//! (see [`crate::onboard::AUDIT_RULES`]). They are deterministic regex arms over file
//! content, so they can be scored with NO model and NO network — which makes them
//! CI-able as a regression gate.
//!
//! This harness pins a labeled corpus of small code snippets with KNOWN planted
//! violations (the ground truth: which rule SHOULD fire on which snippet) plus clean
//! controls (snippets that look close to a violation but must NOT fire — the precision
//! guards). It runs the real [`crate::onboard::audit_files`] over them and computes
//! precision / recall / F1 per rule and overall.
//!
//! ## What the numbers mean
//!
//! For a rule, scored over the fixtures:
//!   - **true positive**  — the rule fired on a fixture that expected it,
//!   - **false positive** — the rule fired on a fixture that did NOT expect it,
//!   - **false negative** — the rule did NOT fire on a fixture that expected it.
//!
//! `precision = tp / (tp + fp)` (of what we flagged, how much was real) and
//! `recall = tp / (tp + fn)` (of what was real, how much we caught).
//!
//! ## The bound the test asserts
//!
//! The floor's design contract (ORCH-PRECISION-RECALL-1: *default to recall*) is that
//! it must NEVER miss a planted floor violation. So the harness asserts **recall == 1.0
//! on every floor rule** — a regression that silently stops catching a hardcoded secret
//! is exactly the "false negatives are silent, compound forever" failure that rule
//! protects against. Precision is asserted at a documented FLOOR ([`MIN_PRECISION`]):
//! the curated controls are chosen so the floor produces no false positives today, but
//! the assertion is a floor (not `== 1.0`) so a future fixture that exercises a known
//! heuristic limit doesn't have to be perfect to land — only good enough.

use serde::Serialize;
use std::collections::BTreeMap;

use crate::onboard::{audit_files, AUDIT_RULES};

/// The minimum overall precision the deterministic floor must hold on this corpus.
///
/// The curated controls are chosen so the floor produces ZERO false positives today
/// (precision is 1.0). This is asserted as a FLOOR rather than equality so a future
/// fixture deliberately exercising a documented heuristic limit (e.g. a name-vs-value
/// secret false-positive the regex can't yet distinguish) can be added without breaking
/// the gate — recall stays the hard `== 1.0` invariant; precision is allowed to dip to
/// this bound. Raise this number as the corpus and the regexes improve.
pub const MIN_PRECISION: f64 = 1.0;

/// One labeled snippet in the eval corpus.
///
/// A fixture is a small, self-contained piece of code with KNOWN ground truth: the set
/// of floor rule ids that SHOULD fire on it. An empty `expected` is a clean control —
/// a snippet that looks close to a violation but must produce NO finding (the precision
/// guard). `name` is a stable label for the metrics table and assertion messages.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Stable label for the fixture (shown in the metrics table / failure messages).
    pub name: &'static str,
    /// The file path the snippet is audited under (extension drives nothing in the
    /// content rules, but a realistic path keeps the fixture honest).
    pub path: &'static str,
    /// The snippet's content.
    pub content: &'static str,
    /// Ground truth: the floor rule ids that SHOULD fire. Empty = clean control.
    pub expected: &'static [&'static str],
}

/// Per-rule confusion counts and derived metrics over the eval corpus.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RuleMetrics {
    /// The rule id these metrics are for, or `"OVERALL"` for the aggregate row.
    pub rule_id: String,
    /// Fired where expected.
    pub true_positives: usize,
    /// Fired where not expected.
    pub false_positives: usize,
    /// Did not fire where expected.
    pub false_negatives: usize,
    /// `tp / (tp + fp)` — 1.0 when nothing was flagged (vacuously precise).
    pub precision: f64,
    /// `tp / (tp + fn)` — 1.0 when nothing was expected (vacuously complete).
    pub recall: f64,
    /// Harmonic mean of precision and recall; 0.0 when both are 0.
    pub f1: f64,
}

impl RuleMetrics {
    /// Build metrics from raw confusion counts.
    fn from_counts(rule_id: &str, tp: usize, fp: usize, fn_: usize) -> Self {
        // Vacuous-true conventions: a rule with nothing flagged is precise (no false
        // alarms), a rule with nothing expected is complete (nothing missed). This keeps
        // a clean-only or absent rule from poisoning the aggregate with a misleading 0.0.
        let precision = if tp + fp == 0 {
            1.0
        } else {
            tp as f64 / (tp + fp) as f64
        };
        let recall = if tp + fn_ == 0 {
            1.0
        } else {
            tp as f64 / (tp + fn_) as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        Self {
            rule_id: rule_id.to_string(),
            true_positives: tp,
            false_positives: fp,
            false_negatives: fn_,
            precision,
            recall,
            f1,
        }
    }
}

/// The full eval result: per-rule metrics plus the overall (micro-averaged) row.
#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    /// One row per floor rule, in [`AUDIT_RULES`] order.
    pub per_rule: Vec<RuleMetrics>,
    /// The micro-averaged aggregate across all floor rules (`rule_id = "OVERALL"`).
    pub overall: RuleMetrics,
    /// How many fixtures were scored.
    pub fixtures_scored: usize,
}

impl EvalReport {
    /// Render the metrics as a fixed-width table (what the CLI subcommand prints).
    pub fn render_table(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "{:<28} {:>4} {:>4} {:>4} {:>9} {:>7} {:>5}\n",
            "rule", "TP", "FP", "FN", "precision", "recall", "F1"
        ));
        s.push_str(&format!("{}\n", "-".repeat(28 + 5 * 4 + 10 + 8 + 6)));
        let row = |m: &RuleMetrics| {
            format!(
                "{:<28} {:>4} {:>4} {:>4} {:>9.3} {:>7.3} {:>5.3}\n",
                m.rule_id,
                m.true_positives,
                m.false_positives,
                m.false_negatives,
                m.precision,
                m.recall,
                m.f1,
            )
        };
        for m in &self.per_rule {
            s.push_str(&row(m));
        }
        s.push_str(&format!("{}\n", "-".repeat(28 + 5 * 4 + 10 + 8 + 6)));
        s.push_str(&row(&self.overall));
        s.push_str(&format!("\n{} fixtures scored\n", self.fixtures_scored));
        s
    }
}

/// The labeled eval corpus: planted violations (one or more rules each) plus clean
/// controls (precision guards that must NOT fire). Returned as owned [`Fixture`]s so
/// the same corpus drives the in-crate test and the CLI subcommand.
///
/// The controls are deliberately adversarial: each one sits right next to a real
/// violation in syntactic shape but is benign, exercising a specific precision guard the
/// floor's regexes were tuned for (a UI `Selection: {n}` string, an env-var read, a
/// parameterised query, a templated URL with no secret param).
pub fn fixtures() -> Vec<Fixture> {
    vec![
        // ── SEC-NO-HARDCODED-SECRETS-1 — planted violations ────────────────────
        Fixture {
            name: "secret_github_pat",
            path: "src/config.rs",
            content: "let token = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789\";\n",
            expected: &["SEC-NO-HARDCODED-SECRETS-1"],
        },
        Fixture {
            name: "secret_aws_access_key",
            path: "src/aws.py",
            content: "AWS_ACCESS_KEY_ID = \"AKIAIOSFODNN7EXAMPLE\"\n",
            expected: &["SEC-NO-HARDCODED-SECRETS-1"],
        },
        Fixture {
            name: "secret_openai_sk_key",
            path: "src/llm.ts",
            content: "const apiKey = \"sk-abcdefghijklmnopqrstuvwxyz0123456789\";\n",
            expected: &["SEC-NO-HARDCODED-SECRETS-1"],
        },
        Fixture {
            name: "secret_named_opaque_literal",
            path: "src/settings.rs",
            // No known prefix, but a SECRET-named identifier next to a 24+ char opaque
            // literal — the heuristic arm of the secrets regex.
            content: "let api_secret = \"Zk19fJ20alQ8xPmN4rTbVwYc7\";\n",
            expected: &["SEC-NO-HARDCODED-SECRETS-1"],
        },
        // ── SEC-NO-HARDCODED-SECRETS-1 — clean controls (precision guards) ──────
        Fixture {
            name: "clean_env_var_read",
            path: "src/config.rs",
            // Reading a secret from the environment is the CORRECT pattern — no literal.
            content: "let token = std::env::var(\"GITHUB_TOKEN\").unwrap_or_default();\n",
            expected: &[],
        },
        Fixture {
            name: "clean_short_token_name",
            path: "src/auth.rs",
            // A SECRET-named identifier, but the value is short (< 24 chars) and is an
            // env-var NAME, not an opaque literal — must not trip the heuristic arm.
            content: "let token_key = \"X-Auth-Token\";\n",
            expected: &[],
        },
        // ── SEC-NO-RAW-SQL-CONCAT-1 — planted violations ───────────────────────
        Fixture {
            name: "sql_format_interpolation",
            path: "src/repo.rs",
            content: "let q = format!(\"SELECT * FROM users WHERE id = {}\", user_id);\n",
            expected: &["SEC-NO-RAW-SQL-CONCAT-1"],
        },
        Fixture {
            name: "sql_string_concat",
            path: "src/repo.ts",
            content: "const q = \"SELECT name FROM accounts WHERE org = \" + orgId;\n",
            expected: &["SEC-NO-RAW-SQL-CONCAT-1"],
        },
        Fixture {
            name: "sql_multiline_interpolation",
            path: "src/repo.rs",
            // Keyword and interpolation on different lines — whole-content matching catches it.
            content: "let q = format!(\n    \"UPDATE orders SET status = {status}\n     WHERE id = {id}\"\n);\n",
            expected: &["SEC-NO-RAW-SQL-CONCAT-1"],
        },
        // ── SEC-NO-RAW-SQL-CONCAT-1 — clean controls (precision guards) ─────────
        Fixture {
            name: "clean_parameterised_query",
            path: "src/repo.rs",
            // Parameterised placeholders ($1) — the CORRECT pattern, no concat/interp.
            content: "let rows = sqlx::query(\"SELECT * FROM users WHERE id = $1\").bind(id);\n",
            expected: &[],
        },
        Fixture {
            name: "clean_ui_selection_label",
            path: "src/cockpit.rs",
            // A UI string with a keyword-ish prefix and a `{}` but NO confirming SQL clause —
            // the exact false positive the SQL regex's clause-gate was added to reject.
            content: "let label = format!(\"Selection: {n} row(s) selected\");\n",
            expected: &[],
        },
        // ── ARCH-NO-SECRETS-IN-URL-1 — planted violations ──────────────────────
        Fixture {
            name: "url_literal_api_key",
            path: "src/client.rs",
            content: "let url = \"https://api.example.com/data?api_key=sk_live_9f8a7b6c5d4e3f2a1b\";\n",
            expected: &["ARCH-NO-SECRETS-IN-URL-1"],
        },
        Fixture {
            name: "url_templated_token_param",
            path: "src/quote.rs",
            // A query-string SHAPE carrying a token param even without a literal scheme.
            content: "let url = format!(\"{base}?symbol={symbol}&token={token}\");\n",
            expected: &["ARCH-NO-SECRETS-IN-URL-1"],
        },
        // ── ARCH-NO-SECRETS-IN-URL-1 — clean control (precision guard) ─────────
        Fixture {
            name: "clean_url_no_secret_param",
            path: "src/client.rs",
            // A templated URL with ordinary query params (no api_key/token/secret/…).
            content: "let url = format!(\"{base}/v1/quotes?symbol={symbol}&interval={interval}\");\n",
            expected: &[],
        },
        // ── Multi-rule fixture: one snippet violating TWO floor rules at once ───
        Fixture {
            name: "secret_and_sql_in_one_file",
            path: "src/leaky.rs",
            content: "let token = \"ghp_0123456789abcdefghijklmnopqrstuvwx\";\nlet q = format!(\"DELETE FROM sessions WHERE user = {}\", uid);\n",
            expected: &["SEC-NO-HARDCODED-SECRETS-1", "SEC-NO-RAW-SQL-CONCAT-1"],
        },
        // ── Wholly clean fixture: ordinary code, no rule should fire ───────────
        Fixture {
            name: "clean_ordinary_code",
            path: "src/util.rs",
            content: "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
            expected: &[],
        },
    ]
}

/// Run the deterministic floor over the labeled corpus and score precision / recall /
/// F1 per rule and overall. Pure: no model, no network — runs in CI.
///
/// Each fixture is audited with the real [`audit_files`] (the same code the brownfield
/// scan uses), so the harness scores the SHIPPING detector, not a copy. Only floor rules
/// (`AUDIT_RULES`) are scored; the audit emits nothing else on this corpus.
pub fn run_eval() -> EvalReport {
    let fxs = fixtures();

    // Per-rule confusion counts, keyed by rule id, seeded for every floor rule so a rule
    // that never fires still gets a row.
    let mut tp: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fp: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fn_: BTreeMap<&str, usize> = BTreeMap::new();
    for &id in AUDIT_RULES {
        tp.insert(id, 0);
        fp.insert(id, 0);
        fn_.insert(id, 0);
    }

    for fx in &fxs {
        // Run the real audit over this single-file fixture.
        let files = vec![(fx.path.to_string(), fx.content.to_string())];
        let findings = audit_files("eval/fixture", &files);

        // Which floor rules ACTUALLY fired on this fixture (deduped — multiple lines
        // firing the same rule still count as one detection of that rule per fixture, so
        // a many-line violation can't inflate the score).
        let mut fired: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for f in &findings {
            // Only score floor rules; ignore anything outside the audit floor.
            if let Some(&id) = AUDIT_RULES.iter().find(|&&r| r == f.rule_id) {
                fired.insert(id);
            }
        }
        let expected: std::collections::BTreeSet<&str> = fx.expected.iter().copied().collect();

        for &id in AUDIT_RULES {
            match (expected.contains(id), fired.contains(id)) {
                (true, true) => *tp.get_mut(id).expect("seeded") += 1,
                (false, true) => *fp.get_mut(id).expect("seeded") += 1,
                (true, false) => *fn_.get_mut(id).expect("seeded") += 1,
                (false, false) => {} // true negative — not counted in P/R
            }
        }
    }

    // Per-rule rows in AUDIT_RULES order.
    let per_rule: Vec<RuleMetrics> = AUDIT_RULES
        .iter()
        .map(|&id| RuleMetrics::from_counts(id, tp[id], fp[id], fn_[id]))
        .collect();

    // Overall = micro-average: sum the confusion counts across all rules, then derive.
    let sum_tp: usize = tp.values().sum();
    let sum_fp: usize = fp.values().sum();
    let sum_fn: usize = fn_.values().sum();
    let overall = RuleMetrics::from_counts("OVERALL", sum_tp, sum_fp, sum_fn);

    EvalReport {
        per_rule,
        overall,
        fixtures_scored: fxs.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hard floor invariant (issue #11): the deterministic floor must catch EVERY
    /// planted floor violation — recall == 1.0 per rule and overall. A regression that
    /// silently stops firing a floor rule is the silent-false-negative failure
    /// ORCH-PRECISION-RECALL-1 ("default to recall") exists to prevent.
    #[test]
    fn floor_recall_is_perfect() {
        let report = run_eval();
        for m in &report.per_rule {
            assert_eq!(
                m.recall, 1.0,
                "floor rule {} missed a planted violation (recall {:.3}, {} false negatives)",
                m.rule_id, m.recall, m.false_negatives
            );
        }
        assert_eq!(
            report.overall.recall, 1.0,
            "overall floor recall regressed to {:.3}",
            report.overall.recall
        );
    }

    /// Precision must hold the documented floor: the curated controls are chosen so the
    /// deterministic floor raises no false alarms today.
    #[test]
    fn floor_precision_meets_documented_bound() {
        let report = run_eval();
        assert!(
            report.overall.precision >= MIN_PRECISION,
            "overall floor precision {:.3} fell below the documented bound {:.3} \
             ({} false positives) — a control fixture is now misfiring",
            report.overall.precision,
            MIN_PRECISION,
            report.overall.false_positives,
        );
    }

    /// The corpus must actually exercise every floor rule (both a planted positive and a
    /// clean control), so a green run means the gate is real, not vacuous. Guards against
    /// a future edit that deletes all of a rule's fixtures and leaves recall trivially 1.0.
    #[test]
    fn corpus_exercises_every_floor_rule_and_has_controls() {
        let fxs = fixtures();
        for &id in AUDIT_RULES {
            let positives = fxs.iter().filter(|f| f.expected.contains(&id)).count();
            assert!(
                positives > 0,
                "no planted-violation fixture exercises floor rule {id}"
            );
        }
        let controls = fxs.iter().filter(|f| f.expected.is_empty()).count();
        assert!(
            controls >= AUDIT_RULES.len(),
            "expected at least one clean control per floor rule; found {controls}"
        );
    }

    /// Sanity-check the metric arithmetic on a hand-computed case so a bug in
    /// precision/recall/F1 derivation can't pass as a "green" gate.
    #[test]
    fn metric_arithmetic_is_correct() {
        // tp=3, fp=1, fn=1 -> precision 0.75, recall 0.75, F1 0.75.
        let m = RuleMetrics::from_counts("X", 3, 1, 1);
        assert!((m.precision - 0.75).abs() < 1e-9);
        assert!((m.recall - 0.75).abs() < 1e-9);
        assert!((m.f1 - 0.75).abs() < 1e-9);

        // Nothing flagged, nothing expected -> vacuously perfect, F1 1.0.
        let v = RuleMetrics::from_counts("Y", 0, 0, 0);
        assert_eq!(v.precision, 1.0);
        assert_eq!(v.recall, 1.0);
        assert_eq!(v.f1, 1.0);
    }
}
