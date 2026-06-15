//! `camerata projects-live`: list a real GitHub Projects v2 board as Camerata
//! stories, demonstrating the board-spans-repos source (Phase C).
//!
//! Unlike `worktracker-live` (one repo's Issues over REST), this reads a Projects
//! v2 board over GraphQL and prints each item as a story with its SOURCE
//! container and BUILD TARGETS — so a board drawing from several repos shows
//! several distinct source repos in one listing.
//!
//! Env:
//!   CAMERATA_GITHUB_TOKEN          a PAT with read:project + repo read
//!   CAMERATA_GITHUB_PROJECT_OWNER  the user/org login that owns the board
//!   CAMERATA_GITHUB_PROJECT_NUMBER the project number (the integer in its URL)
//!   CAMERATA_GITHUB_PROJECT_KIND   "user" (default) or "org"

use camerata_worktracker::{
    GithubProjectConfig, GithubProjectsSource, ProjectOwnerKind, ReqwestTransport,
};

fn env_required(key: &str) -> anyhow::Result<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{key} is not set (see docs/GITHUB_SETUP.md)"))
}

/// Run the live Projects v2 board listing.
pub async fn run_projects_live() -> anyhow::Result<()> {
    println!("== Camerata live GitHub Projects v2 board listing ==\n");

    let token = env_required("CAMERATA_GITHUB_TOKEN")?;
    let owner = env_required("CAMERATA_GITHUB_PROJECT_OWNER")?;
    let number: u64 = env_required("CAMERATA_GITHUB_PROJECT_NUMBER")?
        .parse()
        .map_err(|_| anyhow::anyhow!("CAMERATA_GITHUB_PROJECT_NUMBER must be an integer"))?;
    let owner_kind = match std::env::var("CAMERATA_GITHUB_PROJECT_KIND")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "org" | "organization" => ProjectOwnerKind::Organization,
        _ => ProjectOwnerKind::User,
    };

    println!("owner:   {owner} ({owner_kind:?})");
    println!("project: #{number}\n");

    let config = GithubProjectConfig {
        owner,
        owner_kind,
        number,
        token,
    };
    let transport = ReqwestTransport::new(config.auth_header())?;
    let source = GithubProjectsSource::new(config, transport);

    println!("[1/1] list_all (paged GraphQL)…");
    let stories = source.list_all().await?;
    println!("      {} item(s) on the board\n", stories.len());

    // Collect the distinct source repos to make the board-spans-repos point.
    let mut repos: Vec<String> = stories
        .iter()
        .filter_map(|s| s.external_ref.as_ref().and_then(|r| r.container.clone()))
        .collect();
    repos.sort();
    repos.dedup();

    for (i, s) in stories.iter().enumerate() {
        let source = match s.external_ref.as_ref() {
            Some(r) => format!("{:?} {} in {}", r.provider, r.external_id, r.container.as_deref().unwrap_or("?")),
            None => "draft (board-only)".to_string(),
        };
        let targets = if s.targets.is_empty() {
            "(none)".to_string()
        } else {
            s.targets
                .iter()
                .map(|t| t.repo.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("  {}. {:?}  \"{}\"", i + 1, s.status, s.title);
        println!("       source:  {source}");
        println!("       targets: {targets}");
    }

    println!("\nDISTINCT SOURCE REPOS ON THIS ONE BOARD: {}", repos.len());
    if !repos.is_empty() {
        println!("  {}", repos.join("\n  "));
    }
    println!("\nLIVE PROJECTS LISTING: OK");
    Ok(())
}
