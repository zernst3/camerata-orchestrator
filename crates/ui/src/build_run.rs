//! Bridge from the Build screen to the REAL governed fleet runner.
//!
//! `run_build` locates the gateway binary, scaffolds a temp worktree, and runs the
//! same `camerata_fleet::build_from_plan` the CLI po-demo uses: each plan task
//! becomes one governed `claude -p` agent locked to the Rust gateway, gated per
//! stage by `RustCheckRunner`, then the produced crate is cargo build + tested.
//!
//! It returns an error if the environment is not set up for a live build (the
//! gateway binary is not compiled, `claude` is unavailable, etc.). The Build screen
//! is expected to degrade GRACEFULLY on error: the consumer never sees an error
//! message, so the screen falls back to its calm staged narrative instead.

use camerata_intake::Plan;

pub use camerata_fleet::{BuildEvent, BuildOutcome};

/// Run a real governed build of `plan`, emitting progress via `on_event`. Locates
/// the gateway binary and scaffolds a fresh temp worktree under the OS temp dir.
pub async fn run_build(
    plan: &Plan,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    let gateway_bin = camerata_fleet::locate_gateway_bin()?;
    let root = std::env::temp_dir().join(format!("camerata-ui-build-{}", std::process::id()));
    // Best-effort clean of any prior run's artifacts.
    let _ = std::fs::remove_dir_all(&root);
    camerata_fleet::build_from_plan(plan, &root, &gateway_bin, on_event).await
}

/// A human-readable, calm label for a [`BuildEvent`], for the consumer Build
/// narrative. Returns `None` for events that do not warrant their own line.
pub fn event_label(ev: &BuildEvent) -> Option<String> {
    match ev {
        BuildEvent::Scaffolding => Some("Setting up the project".to_string()),
        BuildEvent::StageStarted { index, total, kind, .. } => {
            Some(format!("Building {} ({} of {})", humanize_kind(kind), index + 1, total))
        }
        BuildEvent::Verifying => Some("Checking it against the rules".to_string()),
        BuildEvent::Done { .. } => Some("Putting it together for you to try".to_string()),
        // Stage-finished is folded into the next line's progress, no own label.
        BuildEvent::StageFinished { .. } => None,
    }
}

/// Map an engineering task-kind label to consumer words.
fn humanize_kind(kind: &str) -> &str {
    match kind {
        "database" => "the data store",
        "backend" => "the data model",
        "frontend" => "the screens",
        "test" => "the checks",
        _ => "your app",
    }
}
