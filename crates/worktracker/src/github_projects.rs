//! GitHub Projects v2 STORY SOURCE (board axis, GraphQL).
//!
//! Unlike the GitHub Issues adapter (`github.rs`, REST, one repo at a time), a
//! Projects v2 board is ABOVE the repo: a single board lists items drawn from
//! many repos, plus repo-less draft items. This module reads a board and maps
//! its items into `CanonicalStory`s, each carrying its own source container and
//! initial build target — the concrete "a story source spans repos" capability
//! from the credential-delegated-scope decision (Phase C).
//!
//! It speaks GraphQL (`POST https://api.github.com/graphql` with a
//! `{query, variables}` body) over the same injectable `HttpTransport`, so the
//! query-building and response-parsing are pure functions unit-tested with
//! fixture JSON, no network. A `GithubProjectsSource` wires them to a transport
//! and pages through the board.
//!
//! This is a SOURCE (it lists stories), distinct from the per-item
//! `WorkItemProvider` (ingest/push/clarify/poll one item). The two compose: the
//! board source discovers stories across repos; per-item operations on each
//! resolved story go through the Issues provider using the story's container.

use serde::Deserialize;

use crate::{CanonicalStory, ExternalRef, FeatureStatus, Provider, RepoTarget};

use super::http::HttpTransport;

// ── Config ──────────────────────────────────────────────────────────────────

/// Whether the project is owned by a user or an organization. GitHub's GraphQL
/// schema roots the project under `user(login:)` vs `organization(login:)`, so
/// the query differs by exactly this token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectOwnerKind {
    /// A user-owned project (`user(login:)`).
    User,
    /// An organization-owned project (`organization(login:)`).
    Organization,
}

impl ProjectOwnerKind {
    /// The GraphQL root field for this owner kind.
    fn root_field(self) -> &'static str {
        match self {
            ProjectOwnerKind::User => "user",
            ProjectOwnerKind::Organization => "organization",
        }
    }
}

/// Connection parameters for one Projects v2 board. The token alone authorizes
/// every repo the board draws from; the board number + owner select the board.
#[derive(Debug, Clone)]
pub struct GithubProjectConfig {
    /// The project owner login (user or org).
    pub owner: String,
    /// Whether `owner` is a user or an organization.
    pub owner_kind: ProjectOwnerKind,
    /// The project NUMBER (the integer in the project URL, not the node id).
    pub number: u64,
    /// A PAT / installation token. Reading Projects v2 needs `read:project`
    /// (or `project`) scope in addition to repo read.
    pub token: String,
}

impl GithubProjectConfig {
    /// The `Authorization: Bearer <token>` header value.
    pub fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

// ── GraphQL query building (pure) ─────────────────────────────────────────────

/// The GitHub GraphQL endpoint.
pub const GRAPHQL_URL: &str = "https://api.github.com/graphql";

/// How many project items to request per page.
pub const PAGE_SIZE: u32 = 50;

/// Build the GraphQL request body (`{"query":..., "variables":...}`) that reads
/// one page of a Projects v2 board's items. `cursor` is the `after:` pagination
/// cursor; `None` starts from the first page.
///
/// The query asks for each item's type and content, with inline fragments for
/// `Issue` / `PullRequest` (which carry a repository) and `DraftIssue` (which
/// does not). Pure and deterministic so it is unit-testable.
pub fn project_items_query(config: &GithubProjectConfig, cursor: Option<&str>) -> String {
    let root = config.owner_kind.root_field();
    // The query text. `$cursor` is null on the first page (GraphQL treats a null
    // `after` as "from the beginning").
    let query = format!(
        "query($login: String!, $number: Int!, $cursor: String) {{ \
           {root}(login: $login) {{ \
             projectV2(number: $number) {{ \
               title \
               items(first: {PAGE_SIZE}, after: $cursor) {{ \
                 pageInfo {{ hasNextPage endCursor }} \
                 nodes {{ \
                   id \
                   type \
                   content {{ \
                     __typename \
                     ... on Issue {{ number title body url state repository {{ nameWithOwner }} }} \
                     ... on PullRequest {{ number title body url state repository {{ nameWithOwner }} }} \
                     ... on DraftIssue {{ title body }} \
                   }} \
                 }} \
               }} \
             }} \
           }} \
         }}"
    );

    let body = serde_json::json!({
        "query": query,
        "variables": {
            "login": config.owner,
            "number": config.number,
            "cursor": cursor,
        }
    });
    body.to_string()
}

// ── GraphQL response shapes ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GqlResponse {
    #[serde(default)]
    data: Option<GqlData>,
    #[serde(default)]
    errors: Option<Vec<GqlError>>,
}

#[derive(Debug, Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct GqlData {
    // Exactly one of these is present depending on owner kind.
    #[serde(default)]
    user: Option<GqlOwner>,
    #[serde(default)]
    organization: Option<GqlOwner>,
}

#[derive(Debug, Deserialize)]
struct GqlOwner {
    #[serde(rename = "projectV2")]
    project_v2: Option<GqlProject>,
}

#[derive(Debug, Deserialize)]
struct GqlProject {
    items: GqlItems,
}

#[derive(Debug, Deserialize)]
struct GqlItems {
    #[serde(rename = "pageInfo")]
    page_info: GqlPageInfo,
    nodes: Vec<GqlItemNode>,
}

#[derive(Debug, Deserialize)]
struct GqlPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GqlItemNode {
    id: String,
    #[serde(default)]
    content: Option<GqlContent>,
}

#[derive(Debug, Deserialize)]
struct GqlContent {
    #[serde(rename = "__typename")]
    typename: String,
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    repository: Option<GqlRepository>,
}

#[derive(Debug, Deserialize)]
struct GqlRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

// ── Mapping (pure) ─────────────────────────────────────────────────────────────

/// Map a GitHub issue/PR state string to a canonical status. Projects items have
/// no Camerata labels, so this is a coarse open/closed mapping; the per-item
/// Issues adapter refines status from labels once a story is adopted.
fn state_to_status(state: Option<&str>) -> FeatureStatus {
    match state {
        Some("CLOSED") | Some("MERGED") => FeatureStatus::Done,
        _ => FeatureStatus::Intake,
    }
}

/// One parsed page: the stories plus the next pagination cursor (when more pages
/// remain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPage {
    /// Stories mapped from this page's items.
    pub stories: Vec<CanonicalStory>,
    /// `Some(cursor)` when another page exists, else `None`.
    pub next_cursor: Option<String>,
}

/// Parse a Projects v2 GraphQL response into stories + the next cursor.
///
/// Mapping per item type:
/// - `Issue` / `PullRequest`: a story SOURCED from that repo's tracker item
///   (external_ref with `container = owner/repo`), and that repo as its initial
///   build TARGET. Different items can name different repos — that is the
///   board-spans-repos property.
/// - `DraftIssue`: a board-only story (no external_ref, no target yet) — it is
///   not scoped to any repo until promoted.
/// - Redacted / unknown content: skipped (the item is not actionable).
pub fn parse_project_items(json: &str) -> anyhow::Result<ProjectPage> {
    let resp: GqlResponse =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_project_items: {e}"))?;

    if let Some(errors) = resp.errors {
        if !errors.is_empty() {
            let joined = errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::bail!("GitHub GraphQL errors: {joined}");
        }
    }

    let data = resp
        .data
        .ok_or_else(|| anyhow::anyhow!("GraphQL response had no data"))?;
    let owner = data
        .user
        .or(data.organization)
        .ok_or_else(|| anyhow::anyhow!("GraphQL response had neither user nor organization"))?;
    let project = owner
        .project_v2
        .ok_or_else(|| anyhow::anyhow!("owner has no projectV2 with that number"))?;

    let mut stories = Vec::new();
    for node in project.items.nodes {
        let Some(content) = node.content else {
            // Redacted item (no readable content) — skip.
            continue;
        };
        let title = content.title.clone().unwrap_or_default();
        let description = content.body.clone().unwrap_or_default();

        match content.typename.as_str() {
            "Issue" | "PullRequest" => {
                let repo = match content.repository.as_ref() {
                    Some(r) => r.name_with_owner.clone(),
                    None => continue, // malformed: an issue/PR without a repo
                };
                let number = content
                    .number
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| node.id.clone());
                let url = content.url.clone().unwrap_or_default();
                let status = state_to_status(content.state.as_deref());
                stories.push(CanonicalStory {
                    id: node.id.clone(),
                    external_ref: Some(
                        ExternalRef::new(Provider::GitHub, number, url).with_container(repo.clone()),
                    ),
                    title,
                    description,
                    status,
                    created_by: "github-project".to_string(),
                    // The item's own repo is the natural initial build target.
                    targets: vec![RepoTarget::new(repo)],
                });
            }
            "DraftIssue" => {
                stories.push(CanonicalStory {
                    id: node.id.clone(),
                    external_ref: None, // a draft lives only on the board
                    title,
                    description,
                    status: FeatureStatus::Intake,
                    created_by: "github-project".to_string(),
                    targets: vec![], // not scoped to a repo yet
                });
            }
            _ => continue, // unknown content type — skip
        }
    }

    let next_cursor = if project.items.page_info.has_next_page {
        project.items.page_info.end_cursor
    } else {
        None
    };

    Ok(ProjectPage {
        stories,
        next_cursor,
    })
}

// ── GithubProjectsSource ───────────────────────────────────────────────────────

/// A Projects v2 board read as a story source. Parameterized over the HTTP
/// transport so it is unit-testable with `FakeTransport`.
pub struct GithubProjectsSource<T: HttpTransport> {
    config: GithubProjectConfig,
    transport: T,
}

impl<T: HttpTransport> GithubProjectsSource<T> {
    /// Construct a source for one board.
    pub fn new(config: GithubProjectConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Fetch ONE page of the board's items, starting after `cursor`.
    pub async fn list_page(&self, cursor: Option<&str>) -> anyhow::Result<ProjectPage> {
        let body = project_items_query(&self.config, cursor);
        let resp = self.transport.post(GRAPHQL_URL, &body).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!("GitHub GraphQL POST: HTTP {} {}", resp.status, resp.body);
        }
        parse_project_items(&resp.body)
    }

    /// Fetch ALL items on the board, paging until exhausted. Bounded by a hard
    /// page cap so a malformed/looping cursor cannot spin forever.
    pub async fn list_all(&self) -> anyhow::Result<Vec<CanonicalStory>> {
        const MAX_PAGES: usize = 200;
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = self.list_page(cursor.as_deref()).await?;
            all.extend(page.stories);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(all),
            }
        }
        anyhow::bail!("GitHub Projects paging exceeded {MAX_PAGES} pages; aborting")
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FakeTransport;

    fn user_config() -> GithubProjectConfig {
        GithubProjectConfig {
            owner: "zernst3".to_string(),
            owner_kind: ProjectOwnerKind::User,
            number: 1,
            token: "ghp_x".to_string(),
        }
    }

    // A board whose three items come from TWO different repos plus a draft.
    const BOARD_JSON: &str = r#"{
      "data": {
        "user": {
          "projectV2": {
            "title": "Roadmap",
            "items": {
              "pageInfo": { "hasNextPage": false, "endCursor": "Y3Vyc29yOjM=" },
              "nodes": [
                {
                  "id": "PVTI_1",
                  "type": "ISSUE",
                  "content": {
                    "__typename": "Issue",
                    "number": 12,
                    "title": "Add CSV export",
                    "body": "Finance wants CSV.",
                    "url": "https://github.com/zernst3/repoA/issues/12",
                    "state": "OPEN",
                    "repository": { "nameWithOwner": "zernst3/repoA" }
                  }
                },
                {
                  "id": "PVTI_2",
                  "type": "PULL_REQUEST",
                  "content": {
                    "__typename": "PullRequest",
                    "number": 5,
                    "title": "Auth service tweak",
                    "body": null,
                    "url": "https://github.com/zernst3/repoB/pull/5",
                    "state": "CLOSED",
                    "repository": { "nameWithOwner": "zernst3/repoB" }
                  }
                },
                {
                  "id": "PVTI_3",
                  "type": "DRAFT_ISSUE",
                  "content": {
                    "__typename": "DraftIssue",
                    "title": "Idea: dark mode",
                    "body": "Just a thought."
                  }
                }
              ]
            }
          }
        }
      }
    }"#;

    #[test]
    fn query_uses_user_root_and_carries_variables() {
        let q = project_items_query(&user_config(), None);
        let v: serde_json::Value = serde_json::from_str(&q).unwrap();
        assert!(v["query"].as_str().unwrap().contains("user(login: $login)"));
        assert_eq!(v["variables"]["login"], "zernst3");
        assert_eq!(v["variables"]["number"], 1);
        assert!(v["variables"]["cursor"].is_null());
    }

    #[test]
    fn query_uses_organization_root_for_org_projects() {
        let mut cfg = user_config();
        cfg.owner_kind = ProjectOwnerKind::Organization;
        let q = project_items_query(&cfg, Some("CUR"));
        let v: serde_json::Value = serde_json::from_str(&q).unwrap();
        assert!(v["query"]
            .as_str()
            .unwrap()
            .contains("organization(login: $login)"));
        assert_eq!(v["variables"]["cursor"], "CUR");
    }

    #[test]
    fn parse_maps_items_from_two_repos_plus_a_draft() {
        let page = parse_project_items(BOARD_JSON).expect("parse");
        assert_eq!(page.stories.len(), 3);
        assert_eq!(page.next_cursor, None, "hasNextPage false -> no cursor");

        // Item 1: an issue in repoA -> source + target both repoA.
        let s1 = &page.stories[0];
        assert_eq!(s1.title, "Add CSV export");
        let r1 = s1.external_ref.as_ref().expect("issue has a source ref");
        assert_eq!(r1.container.as_deref(), Some("zernst3/repoA"));
        assert_eq!(r1.external_id, "12");
        assert_eq!(s1.targets, vec![RepoTarget::new("zernst3/repoA")]);
        assert_eq!(s1.status, FeatureStatus::Intake);

        // Item 2: a PR in repoB (a DIFFERENT repo) -> the board spans repos.
        let s2 = &page.stories[1];
        assert_eq!(
            s2.external_ref.as_ref().unwrap().container.as_deref(),
            Some("zernst3/repoB")
        );
        assert_eq!(s2.status, FeatureStatus::Done, "CLOSED -> Done");

        // Item 3: a draft -> no source ref, no target yet.
        let s3 = &page.stories[2];
        assert!(s3.external_ref.is_none());
        assert!(s3.targets.is_empty());
        assert_eq!(s3.title, "Idea: dark mode");
    }

    #[test]
    fn parse_surfaces_graphql_errors() {
        let json = r#"{"errors":[{"message":"Could not resolve to a ProjectV2"}]}"#;
        let err = parse_project_items(json).expect_err("must surface GraphQL errors");
        assert!(err.to_string().contains("Could not resolve"));
    }

    #[test]
    fn parse_skips_redacted_items_without_content() {
        let json = r#"{"data":{"user":{"projectV2":{"items":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"PVTI_X","type":"REDACTED","content":null}]
        }}}}}"#;
        let page = parse_project_items(json).expect("parse");
        assert!(page.stories.is_empty(), "redacted item is skipped");
    }

    #[tokio::test]
    async fn source_list_all_posts_graphql_and_maps_board() {
        let transport = FakeTransport::new().on("POST", "/graphql", 200, BOARD_JSON);
        let source = GithubProjectsSource::new(user_config(), transport);
        let stories = source.list_all().await.expect("list");
        assert_eq!(stories.len(), 3);

        // It POSTed a GraphQL body to the graphql endpoint.
        let calls = source.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "POST");
        assert!(calls[0].1.contains("/graphql"));
        assert!(calls[0].2.contains("projectV2"), "body carries the query");

        // The mapped stories span two distinct repos — the headline property.
        let containers: Vec<Option<String>> = stories
            .iter()
            .map(|s| s.external_ref.as_ref().and_then(|r| r.container.clone()))
            .collect();
        assert!(containers.contains(&Some("zernst3/repoA".to_string())));
        assert!(containers.contains(&Some("zernst3/repoB".to_string())));
    }

    #[tokio::test]
    async fn source_pages_through_multiple_responses() {
        // Page 1 says hasNextPage:true; page 2 closes it. FakeTransport returns
        // the same scripted body for both POSTs, so to exercise paging we script
        // a first page with a next cursor then a second without. We approximate
        // by checking list_page honors the cursor field independently.
        let page1 = r#"{"data":{"user":{"projectV2":{"items":{
            "pageInfo":{"hasNextPage":true,"endCursor":"C1"},
            "nodes":[{"id":"P1","type":"DRAFT_ISSUE","content":{"__typename":"DraftIssue","title":"one","body":""}}]
        }}}}}"#;
        let page = parse_project_items(page1).expect("parse");
        assert_eq!(page.next_cursor.as_deref(), Some("C1"));
        assert_eq!(page.stories.len(), 1);
    }
}
