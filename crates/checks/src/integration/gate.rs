//! The integration gate orchestrator: wire extractors → engine → waivers →
//! verdict, over the ASSEMBLED tree, driven by the project's SELECTED
//! `INTEGRATION-*` rules.
//!
//! This is the top-level entry the server calls after fan-out assembly and
//! before push/PR. It is the deterministic REPLACEMENT for the old
//! model-backed `check_integration_gate_live` path: no model is ever consulted;
//! every verdict is a reproducible comparison of neutral records.
//!
//! # Flow
//!
//! 1. For each in-scope repo, detect its language(s) and select the per-stack
//!    [`Extractor`]s. Run them to get the repo's produced/consumed lists.
//! 2. Record any seam with NO extractor for that stack as REVIEW-TIER (honest,
//!    never green).
//! 3. Assemble all repos' lists into one [`AssembledTree`].
//! 4. For each SELECTED `INTEGRATION-*` rule, [`reconcile`] the tree.
//! 5. Apply inline `camerata:allow` waivers (+ baseline) to Fail findings — an
//!    intentional-public endpoint waives the auth-seam rule per-endpoint.
//! 6. Split the surviving findings into FAIL (bounce/escalate) and REVIEW (human
//!    QA), and return a [`GateVerdict`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::integration::engine::{reconcile, AssembledTree, Finding, FindingLevelKind, SeamRule};
use crate::integration::extractor::{select_extractors, uncovered_seams, Seam};
use crate::integration::vocab::RepoArtifacts;
use crate::multilang::detect_languages;

/// One in-scope repo the gate reconciles: its `owner/repo` label + its worktree.
#[derive(Debug, Clone)]
pub struct GateRepo {
    /// `owner/repo`, stamped on every extracted record.
    pub repo: String,
    /// The resolved worktree dir to extract from.
    pub dir: PathBuf,
}

/// A per-endpoint (or per-artifact) inline waiver the caller pre-parsed from the
/// assembled tree's source. Reuses the same `camerata:allow RULE-ID -- reason`
/// model as the per-agent tiers; here it waives a RELATIONAL finding for a
/// specific artifact identity (e.g. an intentionally-public endpoint that the
/// auth-seam rule would otherwise flag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateWaiver {
    /// The `INTEGRATION-*` rule id being waived.
    pub rule_id: String,
    /// The artifact identity the waiver applies to (matches [`Finding::artifact`]).
    /// When empty, the waiver applies to EVERY artifact under the rule (a blanket
    /// waiver — discouraged, but supported for parity with the baseline).
    pub artifact: String,
    /// The mandatory reason. A reason-less waiver does NOT suppress (mirrors the
    /// per-agent suppression invariant); the caller flags it separately.
    pub reason: Option<String>,
}

impl GateWaiver {
    /// Does this waiver suppress `finding`? Requires a matching rule id, a
    /// reason (reason-less waivers are invalid), and an artifact match (or a
    /// blanket waiver).
    fn applies_to(&self, finding: &Finding) -> bool {
        self.reason.is_some()
            && self.rule_id == finding.rule_id
            && (self.artifact.is_empty() || self.artifact == finding.artifact)
    }
}

/// The gate's binary, reproducible verdict for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    /// Deterministic FAIL findings that survived waivers. Non-empty → the gate
    /// FAILS: each is bounced to the responsible agent (or escalated).
    pub failures: Vec<Finding>,
    /// REVIEW-TIER findings: seams that could not be checked deterministically
    /// (guard status unknown, or a stack with no extractor). Routed to human QA,
    /// honestly labeled — NEVER rendered as a pass and NEVER as a fail.
    pub review: Vec<ReviewItem>,
    /// FAIL findings suppressed by a waiver (kept for the audit trail).
    pub waived: Vec<Finding>,
}

impl GateVerdict {
    /// True when there are no deterministic failures. NOTE: review-tier items may
    /// still be present — a `passed()` gate with `review` items means "no
    /// mechanical break found; these seams still need human QA."
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// Group failures by responsible repo → the bounce targets. A repo with a
    /// failure gets the concatenated deltas for its revise pass.
    pub fn bounce_targets(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &self.failures {
            out.entry(f.repo.clone())
                .or_default()
                .push(format!("[{}] {} ({})", f.rule_id, f.detail, f.location));
        }
        out
    }

    /// A single-line summary for the run log.
    pub fn summary(&self) -> String {
        format!(
            "integration gate: {} failure(s), {} review-tier seam(s), {} waived",
            self.failures.len(),
            self.review.len(),
            self.waived.len(),
        )
    }
}

/// A review-tier seam: reported to human QA, never scored pass/fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    /// The rule / seam this pertains to.
    pub rule_id: String,
    /// The repo the uncovered seam belongs to (or the producer for an
    /// undeterminable auth seam).
    pub repo: String,
    /// Why it is review-tier (no extractor for the stack, or undeterminable
    /// guard status).
    pub detail: String,
}

/// Run the full integration gate over the in-scope repos and selected rules.
///
/// `selected_rules` are the corpus `INTEGRATION-*` ids the project turned on
/// (opt-in, like every rule). `waivers` are the pre-parsed `camerata:allow`
/// waivers (inline + baseline, rolled into [`GateWaiver`]s by the caller).
///
/// Deterministic and reproducible: same worktrees + same rules + same waivers →
/// same verdict, no model, no network.
pub fn run_gate(
    repos: &[GateRepo],
    selected_rules: &[String],
    waivers: &[GateWaiver],
) -> GateVerdict {
    // 1–3: extract per repo + record uncovered seams, then assemble.
    let mut all_artifacts: Vec<RepoArtifacts> = Vec::new();
    let mut review: Vec<ReviewItem> = Vec::new();

    // Which seams do the selected rules actually exercise? Only report an
    // uncovered seam as review-tier when a selected rule needs it.
    let needed_seams = seams_for_selected(selected_rules);

    for repo in repos {
        let (artifacts, uncovered) = extract_repo(&repo.repo, &repo.dir);
        all_artifacts.push(artifacts);
        for seam in uncovered {
            if !needed_seams.contains(&seam) {
                continue;
            }
            review.push(ReviewItem {
                rule_id: rule_id_for_seam(seam).to_string(),
                repo: repo.repo.clone(),
                detail: format!(
                    "no extractor covers the {} seam for repo `{}`'s stack; this seam is \
                     routed to human QA (review-tier) — NOT passed.",
                    seam.label(),
                    repo.repo,
                ),
            });
        }
    }

    let tree = AssembledTree::from_repos(&all_artifacts);

    // 4: reconcile each selected rule that maps to an engine seam.
    let mut failures: Vec<Finding> = Vec::new();
    let mut waived: Vec<Finding> = Vec::new();
    for rule_id in selected_rules {
        let Some(rule) = SeamRule::from_rule_id(rule_id) else {
            // A selected INTEGRATION-* rule with no engine seam (a future prose /
            // staged rule) is honestly review-tier, never silently passed.
            if rule_id.starts_with("INTEGRATION-") {
                review.push(ReviewItem {
                    rule_id: rule_id.clone(),
                    repo: String::new(),
                    detail: format!(
                        "rule `{rule_id}` has no deterministic engine seam yet; routed to human QA \
                         (review-tier)."
                    ),
                });
            }
            continue;
        };
        for finding in reconcile(&tree, rule) {
            match finding.level {
                FindingLevelKind::Review => {
                    review.push(ReviewItem {
                        rule_id: finding.rule_id.clone(),
                        repo: finding.repo.clone(),
                        detail: finding.detail.clone(),
                    });
                }
                FindingLevelKind::Fail => {
                    // 5: waivers — an intentional-public endpoint waives per-artifact.
                    if waivers.iter().any(|w| w.applies_to(&finding)) {
                        waived.push(finding);
                    } else {
                        failures.push(finding);
                    }
                }
            }
        }
    }

    // Stable output order for reproducibility.
    failures.sort_by(|a, b| a.artifact.cmp(&b.artifact).then_with(|| a.repo.cmp(&b.repo)));
    review.sort_by(|a, b| a.rule_id.cmp(&b.rule_id).then_with(|| a.repo.cmp(&b.repo)));

    GateVerdict {
        failures,
        review,
        waived,
    }
}

/// Extract one repo's artifacts across ALL its detected languages, returning the
/// combined artifacts plus the seams left uncovered (for review-tier).
///
/// A polyglot repo (several manifests) runs each language's extractors; a seam is
/// uncovered only when NO detected language covers it.
fn extract_repo(repo: &str, dir: &Path) -> (RepoArtifacts, Vec<Seam>) {
    let langs = detect_languages(dir);
    if langs.is_empty() {
        // No manifest at all: every seam is review-tier for this repo.
        return (
            RepoArtifacts {
                repo: repo.to_string(),
                ..Default::default()
            },
            vec![Seam::Endpoint, Seam::Event],
        );
    }

    let mut produced = Vec::new();
    let mut consumed = Vec::new();
    let mut covered: Vec<Seam> = Vec::new();

    for (lang, lang_dir) in &langs {
        let extractors = select_extractors(*lang);
        for ex in &extractors {
            covered.push(ex.seam());
            let a = ex.extract(repo, lang_dir);
            produced.extend(a.produced);
            consumed.extend(a.consumed);
        }
        // Any seam this language does not cover is (potentially) uncovered; we
        // dedupe against other languages below.
        let _ = uncovered_seams(&extractors);
    }

    let uncovered: Vec<Seam> = [Seam::Endpoint, Seam::Event]
        .into_iter()
        .filter(|s| !covered.contains(s))
        .collect();

    (
        RepoArtifacts {
            repo: repo.to_string(),
            produced,
            consumed,
        },
        uncovered,
    )
}

/// The seams a set of selected rules exercises (so an uncovered seam is only
/// reported review-tier when a rule needs it).
fn seams_for_selected(rules: &[String]) -> Vec<Seam> {
    let mut out = Vec::new();
    for r in rules {
        let seam = match SeamRule::from_rule_id(r) {
            Some(SeamRule::ApiContract) | Some(SeamRule::AuthSeam) => Seam::Endpoint,
            Some(SeamRule::EventWiring) => Seam::Event,
            None => continue,
        };
        if !out.contains(&seam) {
            out.push(seam);
        }
    }
    out
}

/// The representative corpus rule id for an uncovered seam's review-tier item.
fn rule_id_for_seam(seam: Seam) -> &'static str {
    match seam {
        Seam::Endpoint => "INTEGRATION-API-CONTRACT-1",
        Seam::Event => "INTEGRATION-EVENT-WIRING-1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    /// A JS repo with a package.json so language detection selects extractors.
    fn js_repo(files: &[(&str, &str)]) -> TempDir {
        let td = TempDir::new().unwrap();
        write(td.path(), "package.json", "{\"name\":\"x\"}\n");
        for (name, content) in files {
            write(td.path(), name, content);
        }
        td
    }

    #[test]
    fn matching_producer_consumer_passes() {
        let api = js_repo(&[("routes.js", "app.get('/users/:id', h)\n")]);
        let ui = js_repo(&[("client.js", "axios.get('/users/1')\n")]);
        let verdict = run_gate(
            &[
                GateRepo { repo: "api".into(), dir: api.path().into() },
                GateRepo { repo: "ui".into(), dir: ui.path().into() },
            ],
            &["INTEGRATION-API-CONTRACT-1".into()],
            &[],
        );
        assert!(verdict.passed(), "{:?}", verdict.failures);
    }

    #[test]
    fn drifting_producer_consumer_fails_and_bounces() {
        let api = js_repo(&[("routes.js", "app.post('/members/export', h)\n")]);
        let ui = js_repo(&[("client.js", "axios.post('/members/csv', b)\n")]);
        let verdict = run_gate(
            &[
                GateRepo { repo: "api".into(), dir: api.path().into() },
                GateRepo { repo: "ui".into(), dir: ui.path().into() },
            ],
            &["INTEGRATION-API-CONTRACT-1".into()],
            &[],
        );
        assert!(!verdict.passed());
        let targets = verdict.bounce_targets();
        assert!(targets.contains_key("ui"), "consumer bounced: {targets:?}");
    }

    #[test]
    fn auth_seam_waiver_passes_intentional_public_endpoint() {
        // A gated affordance on an unguarded endpoint fails...
        let api = js_repo(&[("routes.js", "app.post('/public/action', h)\n")]);
        let ui = js_repo(&[(
            "ui.js",
            "if (_can.act) axios.post('/public/action') // camerata:ui-gated\n",
        )]);
        let repos = vec![
            GateRepo { repo: "api".into(), dir: api.path().into() },
            GateRepo { repo: "ui".into(), dir: ui.path().into() },
        ];
        let no_waiver = run_gate(&repos, &["INTEGRATION-AUTH-SEAM-1".into()], &[]);
        assert!(!no_waiver.passed(), "unguarded gated affordance fails");

        // ...but an explicit waiver for that endpoint clears it.
        let waiver = GateWaiver {
            rule_id: "INTEGRATION-AUTH-SEAM-1".into(),
            artifact: "endpoint POST /public/action".into(),
            reason: Some("intentionally public webhook".into()),
        };
        let waived = run_gate(&repos, &["INTEGRATION-AUTH-SEAM-1".into()], &[waiver]);
        assert!(waived.passed(), "explicit waiver clears it: {:?}", waived.failures);
        assert_eq!(waived.waived.len(), 1);
    }

    #[test]
    fn reasonless_waiver_does_not_suppress() {
        let api = js_repo(&[("routes.js", "app.post('/x', h)\n")]);
        let ui = js_repo(&[("ui.js", "if (_can.x) axios.post('/x') // camerata:ui-gated\n")]);
        let repos = vec![
            GateRepo { repo: "api".into(), dir: api.path().into() },
            GateRepo { repo: "ui".into(), dir: ui.path().into() },
        ];
        let waiver = GateWaiver {
            rule_id: "INTEGRATION-AUTH-SEAM-1".into(),
            artifact: "endpoint POST /x".into(),
            reason: None, // reason-less → invalid
        };
        let verdict = run_gate(&repos, &["INTEGRATION-AUTH-SEAM-1".into()], &[waiver]);
        assert!(!verdict.passed(), "reason-less waiver must not suppress");
    }

    #[test]
    fn stack_with_no_extractor_is_review_tier_not_green() {
        // A repo with no recognized manifest → no extractor → review-tier seam.
        let td = TempDir::new().unwrap();
        write(td.path(), "README.txt", "no manifest here\n");
        let verdict = run_gate(
            &[GateRepo { repo: "mystery".into(), dir: td.path().into() }],
            &["INTEGRATION-API-CONTRACT-1".into()],
            &[],
        );
        // No mechanical failure, but the seam is REVIEW-TIER, not a silent pass.
        assert!(verdict.failures.is_empty());
        assert!(
            verdict.review.iter().any(|r| r.repo == "mystery"),
            "uncovered stack must be review-tier: {:?}",
            verdict.review
        );
    }

    #[test]
    fn unselected_rule_is_not_evaluated() {
        // Event rule NOT selected → an unconsumed emit does not fail.
        let api = js_repo(&[("emit.js", "bus.emit('lonely.event')\n")]);
        let verdict = run_gate(
            &[GateRepo { repo: "api".into(), dir: api.path().into() }],
            &["INTEGRATION-API-CONTRACT-1".into()], // event rule off
            &[],
        );
        assert!(verdict.passed());
        assert!(verdict.review.is_empty(), "event seam not needed → not review-tier");
    }

    #[test]
    fn event_rule_catches_dangling_emit() {
        let api = js_repo(&[("emit.js", "bus.emit('lonely.event')\n")]);
        let verdict = run_gate(
            &[GateRepo { repo: "api".into(), dir: api.path().into() }],
            &["INTEGRATION-EVENT-WIRING-1".into()],
            &[],
        );
        assert!(!verdict.passed());
        assert!(verdict.failures[0].detail.contains("dangling emit"));
    }

    #[test]
    fn verdict_is_reproducible() {
        let api = js_repo(&[("routes.js", "app.post('/a', h)\n")]);
        let ui = js_repo(&[("client.js", "axios.post('/b')\n")]);
        let repos = vec![
            GateRepo { repo: "api".into(), dir: api.path().into() },
            GateRepo { repo: "ui".into(), dir: ui.path().into() },
        ];
        let v1 = run_gate(&repos, &["INTEGRATION-API-CONTRACT-1".into()], &[]);
        let v2 = run_gate(&repos, &["INTEGRATION-API-CONTRACT-1".into()], &[]);
        assert_eq!(v1, v2, "same input → identical verdict");
    }
}
