//! Close the spike's headline risk (Q3 #4): a syntax-invalid fleet edit is silently dropped
//! by dx's RSX-diffing pre-pass with **zero** default-log output, and even `--verbose` only
//! surfaces a `DEBUG`-level "not parseable" line that dx does not treat as a build trigger.
//! Relying on "no dx event arrived" as "the edit is fine" is therefore unsafe.
//!
//! [`decide_edit_verdict`] is the pure decision core: given whatever DECISIVE dx event (if
//! any) arrived within a bounded window after an edit, plus an authoritative `cargo check`
//! run ONLY when nothing decisive arrived, decide what actually happened. [`verify_after_edit`]
//! is the thin async wrapper that watches a live event stream and runs `cargo check` as the
//! fallback (per the spike's Recommendation #4).

use std::path::Path;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::parser::PreviewEvent;

/// The outcome of an authoritative `cargo check` run against the app directory, used ONLY as
/// the fallback when dx's own log stream stayed silent past the verification timeout.
#[derive(Debug, Clone, PartialEq)]
pub struct CargoCheckResult {
    pub success: bool,
    pub output: String,
}

/// What actually happened to a fleet-driven edit, per the spike's three observed classes
/// (Q3): a hot-patch/rebuild that dx reported normally, a compile error dx reported normally
/// (dx SURVIVES and reports these clearly), or the blind spot -- dx silently dropped the edit
/// and only the fallback `cargo check` reveals why.
#[derive(Debug, Clone, PartialEq)]
pub enum EditVerdict {
    /// dx accepted the edit (hot-patched or rebuilt and relaunched) -- the preview reflects it.
    Applied,
    /// dx itself reported a build failure for this edit; `diagnostic` is its message.
    BuildFailed { diagnostic: String },
    /// No `Hotreload`/`BuildOk`/`BuildFailed` event arrived within the verification window --
    /// dx's silent-ignore blind spot (Q3 #4). `cargo_check_output` is the authoritative
    /// fallback diagnosis: combined stdout+stderr of `cargo check` run in the app dir (empty
    /// success output is possible -- see `decide_edit_verdict`'s doc comment on that edge case).
    SilentlyIgnored { cargo_check_output: String },
}

/// The pure decision: `observed` is the first DECISIVE dx event (Hotreload/BuildOk/BuildFailed)
/// seen within the verification window, or `None` on timeout (RebuildStarted/Unknown/Serving
/// lines seen while waiting don't count as decisive and are filtered out before this is
/// called -- see [`verify_after_edit`]). `cargo_check` is only meaningful (and only ever
/// `Some`) when `observed` is `None`; it's ignored otherwise, since a decisive dx event is
/// authoritative on its own and running `cargo check` in that case would just be redundant
/// extra latency on the fast path.
pub fn decide_edit_verdict(observed: Option<&PreviewEvent>, cargo_check: Option<&CargoCheckResult>) -> EditVerdict {
    match observed {
        Some(PreviewEvent::Hotreload { .. }) | Some(PreviewEvent::BuildOk { .. }) => EditVerdict::Applied,
        Some(PreviewEvent::BuildFailed { summary }) => EditVerdict::BuildFailed { diagnostic: summary.clone() },
        // RebuildStarted/Unknown/Serving/Some(other) is not decisive on its own; treat like a
        // timeout (verify_after_edit never actually passes these through, but stay defensive).
        _ => match cargo_check {
            Some(result) if result.success => {
                // Edge case: cargo says the code is valid, but dx never reacted at all. From
                // the user's vantage point the preview still never reflected the edit, so
                // this is still "silently ignored" -- just report the (empty/clean) check
                // output rather than inventing a diagnostic that doesn't exist.
                EditVerdict::SilentlyIgnored { cargo_check_output: result.output.clone() }
            }
            Some(result) => EditVerdict::SilentlyIgnored { cargo_check_output: result.output.clone() },
            // No cargo_check supplied at all: only reachable if a caller skips running the
            // fallback after a real timeout, which verify_after_edit never does. Still return
            // something sane rather than panicking.
            None => EditVerdict::SilentlyIgnored { cargo_check_output: String::new() },
        },
    }
}

/// Live wrapper: wait up to `timeout` for a decisive [`PreviewEvent`] on `events` (a
/// [`PreviewServer::subscribe_events`](crate::process::PreviewServer::subscribe_events)
/// receiver), filtering out non-decisive events (RebuildStarted/Unknown/Serving) while
/// waiting; on timeout, run `cargo check` in `app_dir` as the fallback and decide.
pub async fn verify_after_edit(
    app_dir: &Path,
    events: &mut broadcast::Receiver<PreviewEvent>,
    timeout: Duration,
) -> EditVerdict {
    let decisive = tokio::time::timeout(timeout, async {
        loop {
            match events.recv().await {
                Ok(ev @ PreviewEvent::Hotreload { .. })
                | Ok(ev @ PreviewEvent::BuildOk { .. })
                | Ok(ev @ PreviewEvent::BuildFailed { .. }) => return Some(ev),
                Ok(_) => continue,
                // Channel closed (the PreviewServer/its child died) or lagged (we fell behind
                // the broadcast buffer): either way, treat like "nothing decisive arrived".
                Err(_) => return None,
            }
        }
    })
    .await
    .unwrap_or(None);

    if let Some(ev) = decisive {
        return decide_edit_verdict(Some(&ev), None);
    }

    let cargo_check = run_cargo_check(app_dir).await;
    decide_edit_verdict(None, Some(&cargo_check))
}

/// Run `cargo check` in `app_dir`, capturing combined stdout+stderr. Best-effort: a failure
/// to even spawn `cargo` is reported as a failed check with the spawn error as the "output",
/// rather than propagating -- the caller (verify_after_edit) always wants SOME verdict.
async fn run_cargo_check(app_dir: &Path) -> CargoCheckResult {
    match tokio::process::Command::new("cargo").arg("check").current_dir(app_dir).output().await {
        Ok(output) => CargoCheckResult {
            success: output.status.success(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        },
        Err(e) => CargoCheckResult { success: false, output: format!("failed to run `cargo check` in {}: {e}", app_dir.display()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(msg: &str) -> CargoCheckResult {
        CargoCheckResult { success: false, output: msg.to_string() }
    }

    fn passed() -> CargoCheckResult {
        CargoCheckResult { success: true, output: String::new() }
    }

    #[test]
    fn hotreload_event_is_applied() {
        let ev = PreviewEvent::Hotreload { path: Some("timeline.rs".into()) };
        assert_eq!(decide_edit_verdict(Some(&ev), None), EditVerdict::Applied);
    }

    #[test]
    fn build_ok_event_is_applied() {
        let ev = PreviewEvent::BuildOk { secs: Some(4.85) };
        assert_eq!(decide_edit_verdict(Some(&ev), None), EditVerdict::Applied);
    }

    #[test]
    fn build_failed_event_carries_the_dx_diagnostic() {
        let ev = PreviewEvent::BuildFailed { summary: "cannot find crate `reqwest`".into() };
        assert_eq!(
            decide_edit_verdict(Some(&ev), None),
            EditVerdict::BuildFailed { diagnostic: "cannot find crate `reqwest`".into() }
        );
    }

    #[test]
    fn timeout_with_failing_cargo_check_is_silently_ignored_with_diagnosis() {
        // The headline spike scenario (Q3 #4): a syntax-invalid edit produces no dx event at
        // all. The fallback cargo check fails and its output IS the diagnosis we can finally
        // show the user.
        let check = failed("error: expected expression, found `>>>`");
        let verdict = decide_edit_verdict(None, Some(&check));
        assert_eq!(
            verdict,
            EditVerdict::SilentlyIgnored { cargo_check_output: "error: expected expression, found `>>>`".into() }
        );
    }

    #[test]
    fn timeout_with_passing_cargo_check_is_still_silently_ignored() {
        // Rarer edge case: the code is valid Rust but dx never reacted anyway (e.g. a stalled
        // watcher). Still "silently ignored" from the observable-preview standpoint -- the
        // verdict just carries the (clean) check output instead of a fabricated diagnostic.
        let check = passed();
        let verdict = decide_edit_verdict(None, Some(&check));
        assert_eq!(verdict, EditVerdict::SilentlyIgnored { cargo_check_output: String::new() });
    }

    #[test]
    fn timeout_with_no_cargo_check_supplied_does_not_panic() {
        assert_eq!(decide_edit_verdict(None, None), EditVerdict::SilentlyIgnored { cargo_check_output: String::new() });
    }

    #[test]
    fn non_decisive_event_falls_back_to_cargo_check_like_a_timeout() {
        // Defensive: decide_edit_verdict should treat a stray RebuildStarted/Unknown the same
        // as "nothing decisive" if a caller ever passes one through directly.
        let ev = PreviewEvent::RebuildStarted;
        let check = failed("boom");
        let verdict = decide_edit_verdict(Some(&ev), Some(&check));
        assert_eq!(verdict, EditVerdict::SilentlyIgnored { cargo_check_output: "boom".into() });
    }

    // ── the live wrapper, exercised with a real broadcast channel (no real dx/cargo) ────

    #[tokio::test]
    async fn verify_after_edit_returns_applied_when_hotreload_arrives_in_time() {
        let (tx, mut rx) = broadcast::channel(8);
        tx.send(PreviewEvent::Hotreload { path: Some("timeline.rs".into()) }).unwrap();
        let verdict = verify_after_edit(Path::new("."), &mut rx, Duration::from_secs(2)).await;
        assert_eq!(verdict, EditVerdict::Applied);
    }

    #[tokio::test]
    async fn verify_after_edit_skips_non_decisive_events_before_the_decisive_one() {
        let (tx, mut rx) = broadcast::channel(8);
        tx.send(PreviewEvent::RebuildStarted).unwrap();
        tx.send(PreviewEvent::Unknown).unwrap();
        tx.send(PreviewEvent::BuildOk { secs: Some(3.0) }).unwrap();
        let verdict = verify_after_edit(Path::new("."), &mut rx, Duration::from_secs(2)).await;
        assert_eq!(verdict, EditVerdict::Applied);
    }

    #[tokio::test]
    async fn verify_after_edit_times_out_and_falls_back_when_channel_closes_silently() {
        // Simulates the Q3 #4 blind spot end-to-end at the wrapper level: no events at all,
        // sender dropped (as if the reader task produced nothing and the server line stopped).
        // `cargo check` runs against `.` (this crate) which is valid Rust, so we just assert
        // it resolves to SilentlyIgnored rather than hanging or panicking -- the exact command
        // output isn't asserted here (that's covered by the pure decide_edit_verdict tests).
        let (tx, mut rx) = broadcast::channel::<PreviewEvent>(8);
        drop(tx);
        let verdict = verify_after_edit(env!("CARGO_MANIFEST_DIR").as_ref(), &mut rx, Duration::from_secs(30)).await;
        assert!(matches!(verdict, EditVerdict::SilentlyIgnored { .. }));
    }
}
