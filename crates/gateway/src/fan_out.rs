//! Fan-out: concurrent multi-repo / multi-partition dispatch for the
#![allow(dead_code)] // assemble_by_repo + NotOrchestrator are public API; callers land in a followup PR
//! governance gateway.
//!
//! The orchestrator calls `fan_out` when a unit-of-work spans MULTIPLE repos
//! (or logical partitions of a single repo). Unlike `delegate` — which spawns
//! one child at a time — `fan_out` spawns all workers concurrently via
//! `tokio::spawn`, returns when ALL workers complete, then assembles their
//! outputs keyed by repo.
//!
//! # Write isolation
//!
//! Each worker's jail is narrowed to its own repo subdirectory. The
//! per-entry `OrchestratorConfig.worktree_root` is set to
//! `config.worktree_root.join(&entry.repo)` (for relative entries) or
//! the entry value directly (for absolute paths). This means:
//! - Worker A writing to `backend/` cannot touch `frontend/` (different jail).
//! - Camerata is the SOLE committer: workers produce output but do not commit;
//!   the orchestrator reads `assemble_by_repo` and drives commits.
//!
//! # Depth
//!
//! `run_fan_out` calls `delegate::run_delegated` for each worker. `run_delegated`
//! already carries the depth+1 increment, so fan-out children sit at depth 1 and
//! cannot fan out or delegate further (structural + env guard).
//!
//! # Assembly
//!
//! `assemble_by_repo` collects all `WorkerResult`s into a `BTreeMap<repo, &result>`.
//! Because `DuplicateRepo` is rejected at validation time, the map is conflict-free
//! by construction (no two workers wrote to the same partition).

use std::path::PathBuf;

use camerata_core::RuleId;

use crate::delegate::{run_delegated, OrchestratorConfig};

// ─── public types ────────────────────────────────────────────────────────────

/// One entry in a `fan_out` call: work for one repo/domain/partition.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct FanOutEntry {
    /// The repo this worker writes to. In a multi-repo project this is the repo
    /// name or path. Workers are write-isolated per repo — no two workers write
    /// to the same partition.
    pub repo: String,
    /// The domain/slice label (e.g. "backend", "frontend", "migration"). For
    /// observability and prompt framing only; the jail is on `repo`.
    pub domain: String,
    /// The specific subtask for this worker to carry out.
    pub subtask: String,
}

/// Result of ONE fanned-out worker.
#[derive(Debug, Clone)]
pub struct WorkerResult {
    pub repo: String,
    pub domain: String,
    pub output: String,
    /// True if the worker returned `INCOMPLETE:`.
    pub incomplete: bool,
}

/// Error variants for fan_out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanOutError {
    /// Not in orchestrator mode (depth guard tripped or not configured).
    NotOrchestrator,
    /// Depth guard tripped.
    DepthExceeded { depth: u32, max_depth: u32 },
    /// Two or more entries share the same `repo` (partition collision — violates isolation).
    DuplicateRepo(String),
    /// The entry list was empty.
    EmptyEntries,
}

impl std::fmt::Display for FanOutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FanOutError::NotOrchestrator => write!(
                f,
                "FAN_OUT REFUSED: gateway is not in orchestrator mode; fan_out is unavailable"
            ),
            FanOutError::DepthExceeded { depth, max_depth } => write!(
                f,
                "FAN_OUT REFUSED: depth guard tripped (depth={depth} >= max_depth={max_depth}); \
                 do the work yourself"
            ),
            FanOutError::DuplicateRepo(repo) => write!(
                f,
                "FAN_OUT REFUSED: partition collision — two or more entries share repo '{repo}'; \
                 each repo must appear at most once (write-isolation invariant)"
            ),
            FanOutError::EmptyEntries => write!(
                f,
                "FAN_OUT REFUSED: entries list is empty; provide at least one repo/domain/subtask"
            ),
        }
    }
}

// ─── per-worker jail narrowing ────────────────────────────────────────────────

/// Compute the per-worker worktree jail from the orchestrator's worktree root
/// and the entry's `repo` field.
///
/// - If `repo` is an absolute path (starts with `/`), use it as-is.
/// - Otherwise, join it onto `worktree_root`.
///
/// This narrows each child's write jail to exactly its own partition so no
/// worker can touch another worker's repo directory.
fn worker_jail(base: &std::path::Path, repo: &str) -> PathBuf {
    let r = std::path::Path::new(repo);
    if r.is_absolute() {
        r.to_path_buf()
    } else {
        base.join(repo)
    }
}

// ─── core fan_out logic ───────────────────────────────────────────────────────

/// Spawn all workers concurrently and collect their results.
///
/// # Validation (fail-fast before any spawn)
/// 1. `config.may_delegate()` — depth guard.
/// 2. `entries` non-empty — empty dispatch is a caller bug.
/// 3. No duplicate `repo` values — partition collision violates write isolation.
///
/// # Concurrency
/// Each worker is spawned via `tokio::task::JoinSet` so they run concurrently.
/// All spawned futures are `'static` (values are moved/cloned in before spawn).
///
/// # Per-worker jail
/// Each worker's `OrchestratorConfig` is cloned with `worktree_root` narrowed
/// to `worker_jail(config.worktree_root, entry.repo)`. The child's gateway boots
/// WITHOUT orchestrator env, so it cannot fan out or delegate further.
///
/// # Subtask framing
/// The subtask is prefixed with repo/domain context so the worker knows which
/// partition it is responsible for.
pub async fn run_fan_out(
    config: &OrchestratorConfig,
    rule_subset: Vec<RuleId>,
    entries: Vec<FanOutEntry>,
) -> Result<Vec<WorkerResult>, FanOutError> {
    // 1) Depth guard (belt-and-suspenders over structural depth-1).
    if !config.may_delegate() {
        return Err(FanOutError::DepthExceeded {
            depth: config.depth,
            max_depth: config.max_depth,
        });
    }

    // 2) Empty entries check.
    if entries.is_empty() {
        return Err(FanOutError::EmptyEntries);
    }

    // 3) Duplicate-repo check: collect into a set; first collision wins.
    {
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            if !seen.insert(entry.repo.clone()) {
                return Err(FanOutError::DuplicateRepo(entry.repo.clone()));
            }
        }
    }

    // 4) Spawn all workers concurrently via JoinSet.
    let mut join_set: tokio::task::JoinSet<WorkerResult> = tokio::task::JoinSet::new();

    for entry in entries {
        // Clone everything that must be 'static before moving into the task.
        let jail = worker_jail(&config.worktree_root, &entry.repo);
        let worker_config = OrchestratorConfig {
            models: config.models.clone(),
            worktree_root: jail,
            gateway_bin: config.gateway_bin.clone(),
            depth: config.depth,
            max_depth: config.max_depth,
        };
        let rule_subset_clone = rule_subset.clone();

        // Frame the subtask with repo/domain context so the worker knows its scope.
        let framed_subtask = format!(
            "[fan-out worker | repo={repo} domain={domain}]\n\n{subtask}",
            repo = entry.repo,
            domain = entry.domain,
            subtask = entry.subtask,
        );

        let repo = entry.repo.clone();
        let domain = entry.domain.clone();

        // Use "balanced" tier as the default for fan-out workers (they are scoped
        // to one repo partition; the orchestrator on "strongest" dispatched them).
        let tier = "balanced".to_string();

        join_set.spawn(async move {
            let output = run_delegated(
                &worker_config,
                rule_subset_clone,
                &framed_subtask,
                &tier,
            )
            .await
            .unwrap_or_else(|e| e.to_string());

            let incomplete = output.contains("INCOMPLETE:");
            WorkerResult {
                repo,
                domain,
                output,
                incomplete,
            }
        });
    }

    // 5) Collect all results. JoinSet::join_next returns None when empty.
    let mut results = Vec::new();
    while let Some(r) = join_set.join_next().await {
        results.push(r.expect("fan-out worker task panicked"));
    }

    Ok(results)
}

// ─── assembly (sole-committer pattern) ───────────────────────────────────────

/// Per-repo assembly: collect worker outputs keyed by repo.
///
/// Camerata is the sole committer — agents have no git. After workers finish,
/// this produces a map `repo → &WorkerResult` ready for commit-phase processing.
///
/// # Conflict-free by construction
/// `DuplicateRepo` is rejected at `run_fan_out` validation time, so by
/// construction no two workers share a repo key — this map is always
/// conflict-free.
pub fn assemble_by_repo<'a>(
    results: &'a [WorkerResult],
) -> std::collections::BTreeMap<String, &'a WorkerResult> {
    let mut map = std::collections::BTreeMap::new();
    for result in results {
        map.insert(result.repo.clone(), result);
    }
    map
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegate::{DelegateModels, OrchestratorConfig};

    fn models() -> DelegateModels {
        DelegateModels {
            fast: "claude-haiku-4-5-20251001".to_string(),
            balanced: "claude-sonnet-4-6".to_string(),
            strongest: "claude-opus-4-8".to_string(),
            vision: String::new(),
        }
    }

    fn cfg(depth: u32, max_depth: u32) -> OrchestratorConfig {
        OrchestratorConfig {
            models: models(),
            worktree_root: std::path::PathBuf::from("/work/project"),
            gateway_bin: std::path::PathBuf::from("/bin/camerata-gateway"),
            depth,
            max_depth,
        }
    }

    fn entry(repo: &str, domain: &str, subtask: &str) -> FanOutEntry {
        FanOutEntry {
            repo: repo.to_string(),
            domain: domain.to_string(),
            subtask: subtask.to_string(),
        }
    }

    // ── validation tests (no spawn, token-free) ───────────────────────────────

    #[tokio::test]
    async fn duplicate_repo_is_rejected() {
        let entries = vec![
            entry("backend", "api", "add endpoint"),
            entry("frontend", "ui", "add page"),
            entry("backend", "api2", "another backend task"), // collision!
        ];
        let err = run_fan_out(&cfg(0, 1), vec![], entries)
            .await
            .unwrap_err();
        assert_eq!(err, FanOutError::DuplicateRepo("backend".to_string()));
    }

    #[tokio::test]
    async fn empty_entries_is_rejected() {
        let err = run_fan_out(&cfg(0, 1), vec![], vec![]).await.unwrap_err();
        assert_eq!(err, FanOutError::EmptyEntries);
    }

    #[tokio::test]
    async fn fan_out_refused_at_depth_cap() {
        // depth == max_depth: depth guard fires BEFORE any spawn. No worker is
        // launched (no token spend), keeping CI token-free.
        let entries = vec![
            entry("backend", "api", "do something"),
            entry("frontend", "ui", "do something else"),
        ];
        let err = run_fan_out(&cfg(1, 1), vec![], entries)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            FanOutError::DepthExceeded {
                depth: 1,
                max_depth: 1
            }
        );
    }

    // ── worker jail narrowing ─────────────────────────────────────────────────

    #[test]
    fn relative_repo_is_joined_onto_worktree_root() {
        let base = std::path::Path::new("/work/project");
        let jail = worker_jail(base, "backend");
        assert_eq!(jail, std::path::PathBuf::from("/work/project/backend"));
    }

    #[test]
    fn absolute_repo_is_used_as_is() {
        let base = std::path::Path::new("/work/project");
        let jail = worker_jail(base, "/repos/other-service");
        assert_eq!(
            jail,
            std::path::PathBuf::from("/repos/other-service")
        );
    }

    // ── assembly tests ────────────────────────────────────────────────────────

    #[test]
    fn assemble_by_repo_groups_by_repo_key() {
        let results = vec![
            WorkerResult {
                repo: "backend".to_string(),
                domain: "api".to_string(),
                output: "did backend work".to_string(),
                incomplete: false,
            },
            WorkerResult {
                repo: "frontend".to_string(),
                domain: "ui".to_string(),
                output: "did frontend work".to_string(),
                incomplete: false,
            },
            WorkerResult {
                repo: "migration".to_string(),
                domain: "db".to_string(),
                output: "INCOMPLETE: schema conflict".to_string(),
                incomplete: true,
            },
        ];
        let map = assemble_by_repo(&results);
        assert_eq!(map.len(), 3);
        assert_eq!(map["backend"].output, "did backend work");
        assert_eq!(map["frontend"].output, "did frontend work");
        assert!(map["migration"].incomplete);
    }

    #[test]
    fn assemble_by_repo_empty_results_gives_empty_map() {
        let map = assemble_by_repo(&[]);
        assert!(map.is_empty());
    }

    // ── error display ─────────────────────────────────────────────────────────

    #[test]
    fn fan_out_error_display_messages_are_descriptive() {
        let e = FanOutError::DepthExceeded { depth: 1, max_depth: 1 };
        assert!(e.to_string().contains("depth guard tripped"));
        assert!(e.to_string().contains("depth=1"));

        let e2 = FanOutError::DuplicateRepo("backend".to_string());
        assert!(e2.to_string().contains("backend"));
        assert!(e2.to_string().contains("partition collision"));

        let e3 = FanOutError::EmptyEntries;
        assert!(e3.to_string().contains("empty"));

        let e4 = FanOutError::NotOrchestrator;
        assert!(e4.to_string().contains("not in orchestrator mode"));
    }
}
