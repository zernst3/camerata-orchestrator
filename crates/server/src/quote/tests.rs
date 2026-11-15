use super::*;

fn repo(name: &str, files: usize) -> RunRepoInput {
    RunRepoInput {
        repo: name.to_string(),
        code_chars: files * 740,
        files,
        selected: 20,
        multi_option_selected: 1,
        routing_groups: 1,
    }
}

fn ids() -> QuoteModelIds {
    QuoteModelIds {
        scan: "claude-sonnet-5-5".to_string(),
        calibration: "claude-sonnet-5-5".to_string(),
        group_review: "claude-sonnet-5-5".to_string(),
        fix: Some("claude-haiku-5-5".to_string()),
        clone_judge: None,
    }
}

fn plan(consensus: bool) -> QuotePlan {
    QuotePlan::new(
        &ids(),
        vec![repo("acme/api", 40), repo("acme/web", 30)],
        ProjectBackend::Cli,
        false,
        true,
        consensus,
    )
    .unwrap()
}

/// The plan's pre-flight figure is the run estimate the pre-flight prints, times the consensus
/// passes.
#[test]
fn the_plan_carries_the_preflight_figure() {
    let p = plan(false);
    let e = estimate_run_cost_by_repo(&p.repos, "parallel", &p.models(), p.flags());
    assert_eq!(p.preflight_expected_usd, e.dollars);
    assert_eq!(p.preflight_ceiling_usd, e.ceiling_usd);
    let triple = plan(true);
    assert_eq!(triple.runs, crate::consensus::CONSENSUS_PASSES);
    assert!((triple.preflight_expected_usd - 3.0 * e.dollars).abs() < 1e-9);
}

/// An unpriced scan model leaves no plan (the pre-flight prints no figure either); an unpriced
/// later stage is left out of it.
#[test]
fn an_unpriced_model_leaves_no_plan_or_no_stage() {
    let mut unpriced = ids();
    unpriced.scan = "some-model-without-a-price".to_string();
    assert!(QuotePlan::new(
        &unpriced,
        vec![repo("a", 1)],
        ProjectBackend::Api,
        false,
        false,
        false
    )
    .is_none());
    let mut no_fix = ids();
    no_fix.fix = Some("some-model-without-a-price".to_string());
    let p = QuotePlan::new(
        &no_fix,
        vec![repo("a", 1)],
        ProjectBackend::Api,
        false,
        false,
        false,
    )
    .unwrap();
    assert!(p.fix.is_none());
}

/// The re-quote after the first repo's scan prices the second at its pre-flight figure; after
/// the last repo there is nothing later; a repo the plan does not hold gets no re-quote.
#[test]
fn the_requote_follows_the_plans_repo_order() {
    let p = plan(false);
    assert_eq!(p.repos_after("acme/api").len(), 1);
    assert!(p.repos_after("acme/web").is_empty());
    assert!(p.repos_after("acme/elsewhere").is_empty());
    let at = |repo| RequotePoint {
        repo,
        sites: 100,
        scan_spent_usd: 0.2,
        run_spent_usd: 0.2,
        stored_ahead_usd: 0.0,
        cap_usd: None,
        resumed: false,
    };
    let first = p.requote(&at("acme/api")).unwrap();
    let last = p.requote(&at("acme/web")).unwrap();
    assert!(first.expected_usd > last.expected_usd, "{first:?} {last:?}");
    assert!(p.requote(&at("acme/elsewhere")).is_none());
}
