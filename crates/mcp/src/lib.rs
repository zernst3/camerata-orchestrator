//! `camerata-mcp` — the FIRST rung of the MCP adapter ladder (Phase E, GAP-1; see
//! `docs/plans/2026-07-01_backend-headless-core.md`).
//!
//! An rmcp 1.7 MCP **server** over **stdio** that exposes Camerata's first-rung verbs
//! as MCP tools. Every tool is a thin delegation to [`camerata_client::Client`] — a
//! real HTTP round trip to the running BFF (`camerata-server`) — so this crate links
//! ZERO camerata behavior crates. An MCP host (e.g. Claude Code) driving these tools
//! is consuming the exact same `/api/*` capability contract the Dioxus cockpit uses.
//! That is the point: the contract is machine-consumable.
//!
//! # First-rung tools
//!
//! | Tool | Kind | Client verb | BFF route |
//! |---|---|---|---|
//! | `list_stories` | READ | [`Client::list_stories`] | `GET /api/stories` |
//! | `get_run` | READ | [`Client::get_run`] | `GET /api/runs/:id` |
//! | `list_uows` | READ | [`Client::list_uows`] | `GET /api/uows` |
//! | `assign_work_item` | GOVERNED WRITE | [`Client::assign_work_item`] | `POST /api/workitems/assign` |
//! | `start_run` | GOVERNED WRITE | [`Client::start_run`] | `POST /api/stories/:id/run` |
//!
//! "Governed write" here means the WRITE SEMANTICS live behind the BFF (tracker
//! mutation / governed-run spawn with its gate stack) — this adapter adds no policy of
//! its own; it forwards and reports.
//!
//! # Result shaping
//!
//! Every tool returns a [`CallToolResult`]:
//! - success → `CallToolResult::success` with ONE text content item: the api-types DTO
//!   serialized as pretty JSON (machine-parseable, human-skimmable).
//! - [`ClientError`] → `CallToolResult::error` (`is_error: Some(true)`) with the
//!   error's Display text (`BFF returned <status>: <body>` for API errors, transport
//!   detail otherwise) — an MCP tool-level error the host model can read and react to,
//!   never a protocol failure and never a panic.
//!
//! # Base URL resolution
//!
//! [`Camerata::new`] uses [`camerata_client::Client::new`], which resolves
//! `CAMERATA_BFF_URL` and falls back to the embedded BFF's default
//! `http://127.0.0.1:8787` (see `camerata_client::bff_base`). Tests use
//! [`Camerata::with_client`] + `Client::with_base` pointed at a `wiremock` server.
//!
//! # Not the governance gateway
//!
//! `crates/gateway` is Camerata's OTHER rmcp server — the layer-1 governance gate an
//! agent subprocess is locked to (`gated_write` etc.). This crate is the outward-facing
//! product surface over the BFF. Same rmcp idioms (mirrored deliberately), different
//! job.

use camerata_api_types::run::StartRunRequest;
use camerata_client::{Client, ClientError};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler,
};
use camerata_api_types::workitems::AssignWorkItemRequest;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Arguments for the `get_run` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetRunArgs {
    /// The run id returned by `start_run` (e.g. "run-3").
    pub run_id: String,
}

/// Arguments for the `assign_work_item` tool — mirrors
/// [`camerata_api_types::workitems::AssignWorkItemRequest`] field-for-field so the MCP
/// arg schema and the BFF request body cannot drift apart silently.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssignWorkItemArgs {
    /// Stable cross-provider work-item id, e.g. "github:OWNER/REPO#123".
    pub work_item_id: String,
    /// The tracker login to assign the item to, e.g. "octocat".
    pub assignee: String,
}

/// Arguments for the `start_run` tool: the story id (a path segment on the BFF route)
/// plus the [`camerata_api_types::run::StartRunRequest`] body fields, flattened.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StartRunArgs {
    /// The canonical story id to run, e.g. "owner/repo#123" (as listed by
    /// `list_stories`).
    pub story_id: String,
    /// Optional single model override for the run (e.g. "claude-sonnet-4-6"). Leave
    /// unset to use the server's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional raw tier-map JSON object (fast/balanced/strongest/vision). Only needed
    /// for tiered runs; first-rung callers normally leave this unset. Passed through to
    /// the BFF verbatim.
    #[serde(default)]
    pub tier_map: Option<serde_json::Value>,
    /// Optional: skip the layer-2 post-task check gate. Leave unset for the default
    /// (gated) behavior.
    #[serde(default)]
    pub skip_layer2: Option<bool>,
}

/// Shape a client verb's outcome into a [`CallToolResult`]:
/// - `Ok(dto)` → success result with the DTO as pretty JSON text content.
/// - `Err(ClientError)` → error result (`is_error: Some(true)`) carrying the error's
///   Display text. A tool-level error, not a protocol error — the MCP host's model
///   sees the BFF status + body and can react (e.g. a `start_run` 409 with its
///   structured refusal reason).
///
/// Serialization of an api-types DTO cannot realistically fail (plain data, no maps
/// with non-string keys), but if it ever did that is ALSO reported as a tool error
/// rather than a panic — this adapter never takes the server down over one bad value.
fn dto_tool_result<T: Serialize>(res: Result<T, ClientError>) -> CallToolResult {
    match res {
        Ok(dto) => match serde_json::to_string_pretty(&dto) {
            Ok(json) => CallToolResult::success(vec![Content::text(json)]),
            Err(e) => CallToolResult::error(vec![Content::text(format!(
                "response serialization failed: {e}"
            ))]),
        },
        Err(e) => CallToolResult::error(vec![Content::text(e.to_string())]),
    }
}

/// The first-rung MCP server: a [`camerata_client::Client`] plus the generated
/// [`ToolRouter`]. Construct with [`Camerata::new`] (production: BFF base from
/// `CAMERATA_BFF_URL` / the local default) or [`Camerata::with_client`] (tests: inject
/// a client pointed at a mock BFF).
pub struct Camerata {
    tool_router: ToolRouter<Self>,
    client: Client,
}

impl Camerata {
    /// A server whose client resolves the BFF base URL the production way
    /// (`CAMERATA_BFF_URL`, else `http://127.0.0.1:8787`).
    pub fn new() -> Self {
        Self::with_client(Client::new())
    }

    /// A server over an explicit [`Client`] — the test seam (pair with
    /// `Client::with_base` pointed at a `wiremock` mock server).
    pub fn with_client(client: Client) -> Self {
        Self {
            tool_router: Self::tool_router(),
            client,
        }
    }
}

impl Default for Camerata {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl Camerata {
    /// READ — `GET /api/stories` via [`Client::list_stories`].
    #[tool(
        name = "list_stories",
        description = "List Camerata's canonical stories for the active project. READ-ONLY. Returns a JSON array of stories, each with `id` (e.g. \"owner/repo#123\" — the id `start_run` takes), `title`, `description`, `status` (intake/planned/executing/...), `created_by`, and `targets` (the repos the story touches). Empty array when no project is active."
    )]
    pub async fn list_stories(&self) -> CallToolResult {
        dto_tool_result(self.client.list_stories().await)
    }

    /// READ — `GET /api/runs/:id` via [`Client::get_run`].
    #[tool(
        name = "get_run",
        description = "Get the current state of a governed run by its run id (as returned by `start_run`). READ-ONLY. Returns JSON with `status` (planned/executing/gating/awaiting_review/failed/...), `done`, the gate `events` so far, `last_progress_label`, and live stall diagnostics (`idle_ms`, `stalled`, `stall_threshold_ms`). Poll this after `start_run` to follow a run."
    )]
    pub async fn get_run(&self, args: Parameters<GetRunArgs>) -> CallToolResult {
        dto_tool_result(self.client.get_run(&args.0.run_id).await)
    }

    /// READ — `GET /api/uows` via [`Client::list_uows`].
    #[tool(
        name = "list_uows",
        description = "List every Unit of Work for the active project. READ-ONLY. Returns JSON `{ uows: [...] }`, each entry carrying its `id`, the tracker `work_item` it references (null if not yet linked), its lifecycle `stage`, and whether it is currently `authoring`. Empty list when no project is active."
    )]
    pub async fn list_uows(&self) -> CallToolResult {
        dto_tool_result(self.client.list_uows().await)
    }

    /// GOVERNED WRITE — `POST /api/workitems/assign` via [`Client::assign_work_item`].
    #[tool(
        name = "assign_work_item",
        description = "Assign a tracker work item to a login (GOVERNED WRITE: mutates the tracker through the BFF). Args: `work_item_id` (stable cross-provider id, e.g. \"github:OWNER/REPO#123\") and `assignee` (the login). Returns JSON `{ ok, assignees, updated_at }` with the item's updated assignee list. A BFF refusal comes back as a tool error carrying the HTTP status and body."
    )]
    pub async fn assign_work_item(&self, args: Parameters<AssignWorkItemArgs>) -> CallToolResult {
        let AssignWorkItemArgs {
            work_item_id,
            assignee,
        } = args.0;
        dto_tool_result(
            self.client
                .assign_work_item(AssignWorkItemRequest {
                    work_item_id,
                    assignee,
                })
                .await,
        )
    }

    /// GOVERNED WRITE — `POST /api/stories/:id/run` via [`Client::start_run`].
    #[tool(
        name = "start_run",
        description = "Start a governed run for a story (GOVERNED WRITE: spawns real agent work behind Camerata's gate stack). Args: `story_id` (from `list_stories`), plus optional `model` (single-model override), `tier_map` (raw tier-map JSON for tiered runs), and `skip_layer2`. Returns JSON `{ run_id, story_id, mode }` immediately — poll `get_run` with `run_id` for progress. The BFF refuses with 409 when a run is already active for the story or the development gate is not satisfied; that surfaces as a tool error whose text includes the structured JSON reason."
    )]
    pub async fn start_run(&self, args: Parameters<StartRunArgs>) -> CallToolResult {
        let StartRunArgs {
            story_id,
            model,
            tier_map,
            skip_layer2,
        } = args.0;
        dto_tool_result(
            self.client
                .start_run(
                    &story_id,
                    StartRunRequest {
                        model,
                        tier_map,
                        skip_layer2,
                    },
                )
                .await,
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Camerata {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        // Self-identify as `camerata-mcp` (distinct from the governance gateway's
        // `camerata`). As with the gateway: an MCP host derives the tool prefix from
        // its own mcp-config KEY, not this field; we set it so the server
        // self-identifies consistently.
        info.server_info = Implementation::new("camerata-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Camerata orchestrator over MCP (first rung). Read the story spine \
             (list_stories), Units of Work (list_uows), and run state (get_run); \
             assign tracker work items (assign_work_item) and start governed runs \
             (start_run). Every tool is a live HTTP call to the Camerata BFF — it \
             must be running (default http://127.0.0.1:8787, override via \
             CAMERATA_BFF_URL)."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The registered tool surface IS the machine-consumable contract — guard the
    /// exact name set so a rename/removal fails loudly here before any MCP host
    /// notices.
    #[test]
    fn tool_router_registers_exactly_the_first_rung_tools() {
        let server = Camerata::with_client(Client::with_base("http://127.0.0.1:0"));
        let mut names: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "assign_work_item",
                "get_run",
                "list_stories",
                "list_uows",
                "start_run",
            ]
        );
    }

    /// Every registered tool must carry a non-empty description — the surface is only
    /// machine-consumable if the host model can read what each tool does.
    #[test]
    fn every_tool_has_a_description() {
        let server = Camerata::with_client(Client::with_base("http://127.0.0.1:0"));
        for tool in server.tool_router.list_all() {
            let desc = tool.description.as_deref().unwrap_or_default();
            assert!(
                !desc.trim().is_empty(),
                "tool `{}` has no description",
                tool.name
            );
        }
    }

    /// Pull the single text content item out of a CallToolResult (all our tools emit
    /// exactly one).
    fn text_of(result: &CallToolResult) -> &str {
        assert_eq!(result.content.len(), 1, "expected exactly one content item");
        &result.content[0]
            .as_text()
            .expect("content must be text")
            .text
    }

    /// READ delegation: the `list_stories` tool method drives a real HTTP GET against
    /// the (mock) BFF and returns the DTO as JSON text content.
    #[tokio::test]
    async fn list_stories_tool_hits_the_bff_and_returns_json_content() {
        let bff = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "owner/repo#1",
                    "external_ref": null,
                    "title": "Fix the thing",
                    "description": "It is broken",
                    "status": "intake",
                    "created_by": "agent",
                    "targets": [{ "repo": "owner/repo", "role": null }],
                }
            ])))
            .expect(1)
            .mount(&bff)
            .await;

        let server = Camerata::with_client(Client::with_base(bff.uri()));
        let result = server.list_stories().await;

        assert_eq!(result.is_error, Some(false));
        let json: serde_json::Value =
            serde_json::from_str(text_of(&result)).expect("content must be valid JSON");
        assert_eq!(json[0]["id"], "owner/repo#1");
        assert_eq!(json[0]["status"], "intake");
    }

    /// GOVERNED-WRITE delegation: `assign_work_item` posts the exact request body the
    /// BFF contract expects (wiremock's `body_json` matcher enforces it) and surfaces
    /// the response DTO.
    #[tokio::test]
    async fn assign_work_item_tool_posts_the_contract_body() {
        let bff = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/workitems/assign"))
            .and(body_json(serde_json::json!({
                "work_item_id": "github:owner/repo#1",
                "assignee": "octocat",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "assignees": ["octocat"],
                "updated_at": "2026-07-01T00:00:00Z",
            })))
            .expect(1)
            .mount(&bff)
            .await;

        let server = Camerata::with_client(Client::with_base(bff.uri()));
        let result = server
            .assign_work_item(Parameters(AssignWorkItemArgs {
                work_item_id: "github:owner/repo#1".to_string(),
                assignee: "octocat".to_string(),
            }))
            .await;

        assert_eq!(result.is_error, Some(false));
        let json: serde_json::Value =
            serde_json::from_str(text_of(&result)).expect("content must be valid JSON");
        assert_eq!(json["ok"], true);
        assert_eq!(json["assignees"][0], "octocat");
    }

    /// GOVERNED-WRITE delegation: `start_run` hits the id-scoped route with the
    /// request-body fields carried through from the tool args (unset optionals are
    /// omitted from the wire body, matching `StartRunRequest`'s skip_serializing_if).
    #[tokio::test]
    async fn start_run_tool_posts_to_the_id_scoped_route() {
        let bff = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stories/story-1/run"))
            .and(body_json(serde_json::json!({ "model": "claude-sonnet-4-6" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "run_id": "run-42",
                "story_id": "story-1",
                "mode": "scripted",
            })))
            .expect(1)
            .mount(&bff)
            .await;

        let server = Camerata::with_client(Client::with_base(bff.uri()));
        let result = server
            .start_run(Parameters(StartRunArgs {
                story_id: "story-1".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
                tier_map: None,
                skip_layer2: None,
            }))
            .await;

        assert_eq!(result.is_error, Some(false));
        let json: serde_json::Value =
            serde_json::from_str(text_of(&result)).expect("content must be valid JSON");
        assert_eq!(json["run_id"], "run-42");
        assert_eq!(json["mode"], "scripted");
    }

    /// A ClientError (here: BFF 404) maps to an MCP TOOL error — `is_error:
    /// Some(true)` with the status + body in the text — never a panic and never a
    /// protocol error.
    #[tokio::test]
    async fn client_error_maps_to_tool_error_not_panic() {
        let bff = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/nope"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({ "error": "run not found: nope" })),
            )
            .expect(1)
            .mount(&bff)
            .await;

        let server = Camerata::with_client(Client::with_base(bff.uri()));
        let result = server
            .get_run(Parameters(GetRunArgs {
                run_id: "nope".to_string(),
            }))
            .await;

        assert_eq!(result.is_error, Some(true));
        let text = text_of(&result);
        assert!(text.contains("404"), "error text must carry the status: {text}");
        assert!(
            text.contains("run not found"),
            "error text must carry the BFF body: {text}"
        );
    }

    /// A transport-level failure (nothing listening at the base URL) also maps to a
    /// tool error, exercising the ClientError::Request arm.
    #[tokio::test]
    async fn transport_failure_maps_to_tool_error() {
        // Port 1 on localhost: reliably nothing listening.
        let server = Camerata::with_client(Client::with_base("http://127.0.0.1:1"));
        let result = server.list_uows().await;

        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).contains("request failed"),
            "expected ClientError::Request's Display prefix"
        );
    }
}
