//! `camerata worktracker-live`: the LIVE GitHub round-trip harness.
//!
//! This is the command to run once you supply a GitHub token. It exercises the real
//! `GithubProvider` over `ReqwestTransport` against a real repo and issue:
//!   1. ingest the issue as a CanonicalStory,
//!   2. post a clarifying-question comment on it,
//!   3. poll for inbound events.
//!
//! Everything it uses is the same code path the BFF wires; this harness just drives
//! it directly so you can confirm the live connection before running the app in
//! GitHub mode. See `docs/GITHUB_SETUP.md` for the env vars and token scopes.

use camerata_worktracker::{
    ExternalRef, GithubConfig, GithubProvider, Provider, ReqwestTransport, WorkItemProvider,
};

fn env_required(key: &str) -> anyhow::Result<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{key} is not set (see docs/GITHUB_SETUP.md)"))
}

/// Run the live GitHub round-trip. Reads:
///   CAMERATA_GITHUB_TOKEN, CAMERATA_GITHUB_REPO (owner/repo), CAMERATA_GITHUB_ISSUE.
pub async fn run_worktracker_live() -> anyhow::Result<()> {
    println!("== Camerata live work-tracker round-trip (GitHub) ==\n");

    let token = env_required("CAMERATA_GITHUB_TOKEN")?;
    let repo_spec = env_required("CAMERATA_GITHUB_REPO")?;
    let issue = env_required("CAMERATA_GITHUB_ISSUE")?;
    let (owner, repo) = repo_spec
        .split_once('/')
        .map(|(o, r)| (o.to_string(), r.to_string()))
        .ok_or_else(|| anyhow::anyhow!("CAMERATA_GITHUB_REPO must be `owner/repo`"))?;

    println!("repo:  {owner}/{repo}");
    println!("issue: #{issue}\n");

    // Token-only connection (no baked-in repo): the per-request container on the
    // reference is what selects the repo, exactly as the multi-repo app path works.
    let config = GithubConfig::from_token(token);
    let transport = ReqwestTransport::new(config.auth_header())?;
    let provider = GithubProvider::new(config, transport);

    let reference = ExternalRef {
        provider: Provider::GitHub,
        external_id: issue.clone(),
        container: Some(format!("{owner}/{repo}")),
        url: format!("https://github.com/{owner}/{repo}/issues/{issue}"),
        revision: None,
    };

    // 1. Ingest.
    println!("[1/3] ingest_story…");
    let story = provider.ingest_story(&reference).await?;
    println!("      title:  {}", story.title);
    println!("      status: {:?}", story.status);

    // 2. Post a clarifying question (a real comment on the issue).
    println!("\n[2/3] post_clarifying_questions…");
    let questions =
        ["Camerata live test: please ignore. Confirming the clarify-bridge round-trip.".to_string()];
    let comment_ref = provider
        .post_clarifying_questions(&reference, &questions)
        .await?;
    println!("      posted comment ref: {comment_ref}");

    // 3. Poll for inbound events.
    println!("\n[3/3] poll (cursor=None)…");
    let (events, next_cursor) = provider.poll(None).await?;
    println!("      {} event(s); next cursor: {next_cursor}", events.len());
    for ev in events.iter().take(5) {
        println!("      - {:?} on {}", ev.kind, ev.reference.external_id);
    }

    println!("\nLIVE ROUND-TRIP: OK");
    println!("Check issue #{issue} on GitHub: it should now have a Camerata comment.");
    Ok(())
}
