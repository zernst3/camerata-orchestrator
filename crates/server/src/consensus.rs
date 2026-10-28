//! H5 item 3: the three-pass consensus option for client work, OFF by default
//! (`RunControls::consensus`).
//!
//! When on, `audit_repos` runs each repo's whole AI audit ([`crate::ai_audit::audit_repo`]:
//! the scan passes, the alternatives choice, calibration and fixes) [`CONSENSUS_PASSES`] times
//! and keeps a finding only when at least [`CONSENSUS_QUORUM`] passes reported it. Findings are
//! matched across passes with [`crate::repeatability::finding_match_key`], the same identity
//! `camerata repeatability` uses, and repeats are counted: a site reported twice by two passes
//! and once by the third is kept twice.
//!
//! It costs about three times the AI spend of a normal run and trades recall for stability: a
//! real issue the model only notices one time in three is dropped. That is why it is off by
//! default and meant for client deliverables where a finding that comes and goes between runs
//! does more harm than one that is missed. Every drop is recorded as a ledger stage, so the
//! integrity ledger reconciles.

use std::collections::{HashMap, HashSet};

use crate::onboard::Finding;
use crate::scan_ledger::{StageAccounting, StageSample};

/// How many times the AI audit runs with consensus on.
pub const CONSENSUS_PASSES: usize = 3;
/// How many of those passes must report a finding for it to be kept.
pub const CONSENSUS_QUORUM: usize = 2;

/// The ledger stage name for the consensus vote.
pub const CONSENSUS_STAGE: &str = "consensus";

/// Keep each finding (with its repeats) that at least `quorum` of `passes` reported. Returns
/// the kept findings, in the order they first appear across the passes, and the ledger sample
/// accounting for every row the vote removed. The kept copy of a finding is the one from the
/// earliest pass that reported it enough times.
pub fn consensus_findings(passes: &[Vec<Finding>], quorum: usize) -> (Vec<Finding>, StageSample) {
    let quorum = quorum.max(1);
    let key = crate::repeatability::finding_match_key;
    // Each pass grouped by match key, keeping that pass's own order inside a key.
    let per_pass: Vec<HashMap<String, Vec<&Finding>>> = passes
        .iter()
        .map(|pass| {
            let mut groups: HashMap<String, Vec<&Finding>> = HashMap::new();
            for f in pass {
                groups.entry(key(f)).or_default().push(f);
            }
            groups
        })
        .collect();
    // Keys in the order they first appear across the passes (deterministic output order).
    let mut first_seen: HashSet<String> = HashSet::new();
    let mut order: Vec<String> = Vec::new();
    for f in passes.iter().flatten() {
        let k = key(f);
        if first_seen.insert(k.clone()) {
            order.push(k);
        }
    }

    let mut kept: Vec<Finding> = Vec::new();
    let mut agreeing_copies = 0usize;
    let mut minority_dropped = 0usize;
    for k in &order {
        let counts: Vec<usize> = per_pass
            .iter()
            .map(|groups| groups.get(k).map_or(0, Vec::len))
            .collect();
        // The number of copies at least `quorum` passes agree on: the quorum-th largest count.
        let mut descending = counts.clone();
        descending.sort_unstable_by(|a, b| b.cmp(a));
        let keep = descending.get(quorum - 1).copied().unwrap_or(0);
        if keep > 0 {
            let source = per_pass
                .iter()
                .find_map(|groups| groups.get(k).filter(|copies| copies.len() >= keep))
                .expect("the quorum-th largest count is reached by some pass");
            kept.extend(source[..keep].iter().map(|f| (*f).clone()));
        }
        let total: usize = counts.iter().sum();
        let agreeing: usize = counts.iter().map(|c| (*c).min(keep)).sum::<usize>() - keep;
        agreeing_copies += agreeing;
        minority_dropped += total - keep - agreeing;
    }

    let sample = StageSample {
        stage: CONSENSUS_STAGE.to_string(),
        rows_in: passes.iter().map(Vec::len).sum(),
        rows_out: kept.len(),
        accounting: StageAccounting {
            deduped: agreeing_copies,
            other: vec![("consensus_minority_dropped".to_string(), minority_dropped)],
            ..StageAccounting::default()
        },
    };
    (kept, sample)
}

/// What one [`crate::ai_audit::audit_repo`] call returns.
pub type AuditRepoOutput = (
    Vec<Finding>,
    Vec<crate::onboard::ProposedRule>,
    Vec<crate::ai_audit::RuleRecommendation>,
    Vec<crate::ai_audit::FailedPass>,
    Vec<StageSample>,
);

/// Run `audit` once, or with `consensus` on [`CONSENSUS_PASSES`] times in sequence, and merge.
///
/// Off: `audit`'s result, untouched. On: findings by [`consensus_findings`] with
/// [`CONSENSUS_QUORUM`]; proposed rules and alternative recommendations from the first pass that
/// answered (the alternatives decide which option a rule is judged against, so mixing passes
/// would mix questions); failed passes from every pass, deduplicated; every pass's stage
/// samples followed by the vote's. Sequential, so the spend limit is checked between passes
/// exactly as it is between any other passes.
///
/// A pass that errors is disclosed as a failed pass and the vote runs over the passes that
/// answered, with the quorum capped at their number: two answers must still agree, one answer
/// stands alone and says so. When no pass answers, the first error is returned, exactly as a
/// single audit's would be.
///
/// Each audit still reports to a live job (`audit_repo`'s `job`) as it runs, so a job's
/// preview would show every audit's raw findings and triple its progress counts. Only
/// `camerata inspect`, which has no job, can turn consensus on today; wiring it into the app
/// means voting before the preview, not after.
pub async fn run_audit_passes<F, Fut>(
    repo: &str,
    consensus: bool,
    mut audit: F,
) -> anyhow::Result<AuditRepoOutput>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<AuditRepoOutput>>,
{
    if !consensus {
        return audit().await;
    }
    let mut answered: Vec<AuditRepoOutput> = Vec::new();
    let mut errors: Vec<(usize, anyhow::Error)> = Vec::new();
    for pass in 1..=CONSENSUS_PASSES {
        match audit().await {
            Ok(out) => answered.push(out),
            Err(e) => errors.push((pass, e)),
        }
    }
    if answered.is_empty() {
        let (_, first) = errors.remove(0);
        return Err(first);
    }

    let quorum = CONSENSUS_QUORUM.min(answered.len());
    let finding_sets: Vec<Vec<Finding>> = answered.iter().map(|o| o.0.clone()).collect();
    let (findings, vote) = consensus_findings(&finding_sets, quorum);

    let mut failed: Vec<crate::ai_audit::FailedPass> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut samples: Vec<StageSample> = Vec::new();
    for out in &answered {
        for p in &out.3 {
            if seen.insert((p.repo.clone(), p.pass.clone(), p.reason.clone())) {
                failed.push(p.clone());
            }
        }
        samples.extend(out.4.iter().cloned());
    }
    for (pass, e) in &errors {
        failed.push(crate::ai_audit::FailedPass {
            repo: repo.to_string(),
            pass: format!("consensus audit {pass} of {CONSENSUS_PASSES}"),
            reason: format!(
                "{e:#} ({} of {CONSENSUS_PASSES} consensus audits answered; findings were kept \
                 when {quorum} of them agreed)",
                answered.len()
            ),
            files_not_reviewed: 0,
        });
    }
    samples.push(vote);

    let first = answered.swap_remove(0);
    Ok((findings, first.1, first.2, failed, samples))
}

#[cfg(test)]
mod tests;
