//! Pure line-classifier for `dx serve` stdout/stderr.
//!
//! Grounded in the exact signal classes measured in
//! `docs/spikes/2026-07-09_dioxus-live-preview-spike.md` (Q3): dx tags most lines with an
//! elapsed-seconds prefix and a level (`INFO`/`WARN`/`ERROR`/`DEBUG`, the latter only visible
//! with `--verbose`), e.g. `"40.28s ERROR Build failed: cargo build finished with errors..."`.
//! [`parse_dx_line`] strips that prefix (if present — some illustrative lines in the spike
//! doc omit it) and classifies the remaining message.
//!
//! Two of the five event variants are **not** grounded in a verbatim line from the spike:
//! the spike never captured (nor needed to — it confirmed the URL via `curl` against a
//! guessed default port, and confirmed rebuild-in-flight via `ps aux`, not a log line) an
//! explicit "now serving at <url>" or "rebuild started" line. See the doc comments on
//! [`extract_serving_url`] and [`looks_like_rebuild_started_hint`] for how those two are
//! handled — both are deliberately best-effort/forward-compatible rather than load-bearing;
//! `PreviewServer` gets the URL from the port *it* chose to pass to `dx serve --port`, not
//! from parsing dx's own output (see `crates/preview/src/process.rs`).

use serde::{Deserialize, Serialize};

/// A classified signal parsed out of one `dx serve` stdout/stderr line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PreviewEvent {
    /// dx reported (or the line otherwise indicates) the app is being served at `url`.
    /// Best-effort: see the module docs — `PreviewServer` does not depend on this arriving.
    Serving { url: String },
    /// `Hotreloading: <path>` — the ~1s RSX/text hot-patch fast path. `path` is `None` only
    /// if the line has no path segment at all (not observed in the spike, but tolerated).
    Hotreload { path: Option<String> },
    /// Best-effort signal that a full rebuild has kicked off. UNCONFIRMED by the spike doc
    /// (see [`looks_like_rebuild_started_hint`]) — treat as advisory, not load-bearing.
    RebuildStarted,
    /// `Build completed [successfully] in <secs>s[, launching app! 💫]` — a full rebuild
    /// finished and the reload is live. `secs` is `None` only if the duration couldn't be
    /// parsed out of the line (format drift), not if the line lacks a number entirely.
    BuildOk { secs: Option<f64> },
    /// A `Build failed: ...` line, or a bracketed rustc diagnostic (`error[EXXXX]: ...`), or
    /// any dx-tagged `ERROR` line. `summary` is the message with the prefix/level stripped.
    BuildFailed { summary: String },
    /// A recognized dx log line (has the elapsed-seconds+level shape, or is clearly dx's own
    /// output) that doesn't match any of the above — e.g. the DEBUG `Diff rsx returned not
    /// parseable` / `Ignoring file change` lines from the spike's silent-ignore case (Q3 #4).
    /// Deliberately NOT specially classified: the spike found this signal to be DEBUG-only,
    /// non-uniform across dx versions, and not a substitute for the timeout+`cargo check`
    /// fallback in `verify.rs` — see that module for how the silent-ignore gap is actually
    /// closed.
    Unknown,
}

/// Benign noise the spike doc explicitly calls out as harmless: macOS atomic-rename writes
/// (`sed -i ''`, many editors) race dx's FSEvents watcher, which logs a transient
/// "can't find the temp path" DEBUG line before falling through to the real file content
/// regardless. Never surface these as [`PreviewEvent::Unknown`] — they're pure watcher noise.
const NOISE_MARKERS: &[&str] = &[
    "Failed to canonicalize hotreloaded asset",
    "Failed to read rust file while hotreloading",
];

/// Parse one `dx serve` output line into a [`PreviewEvent`]. Returns `None` for blank lines
/// and the benign watcher-race noise in [`NOISE_MARKERS`]; everything else recognizable but
/// unclassified becomes `Some(PreviewEvent::Unknown)`.
pub fn parse_dx_line(line: &str) -> Option<PreviewEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (_level, msg) = split_prefix(trimmed);

    if NOISE_MARKERS.iter().any(|marker| msg.contains(marker)) {
        return None;
    }

    if let Some(path) = msg.strip_prefix("Hotreloading:") {
        let path = path.trim();
        return Some(PreviewEvent::Hotreload {
            path: if path.is_empty() { None } else { Some(path.to_string()) },
        });
    }

    if msg.contains("Build failed") || is_rustc_error_marker(msg) {
        return Some(PreviewEvent::BuildFailed { summary: msg.to_string() });
    }

    if msg.starts_with("Build completed") {
        return Some(PreviewEvent::BuildOk { secs: parse_build_completed_secs(msg) });
    }

    // Any other dx-tagged ERROR line is a build/startup failure worth surfacing (e.g. the
    // Dioxus.toml schema-mismatch error from the spike's Blocker 1, or an individual
    // `cannot find module or crate` rustc diagnostic ahead of the terminal "Build failed:"
    // line) even though it doesn't literally contain "Build failed".
    if _level == Some(Level::Error) {
        return Some(PreviewEvent::BuildFailed { summary: msg.to_string() });
    }

    if let Some(url) = extract_serving_url(msg) {
        return Some(PreviewEvent::Serving { url });
    }

    if looks_like_rebuild_started_hint(msg) {
        return Some(PreviewEvent::RebuildStarted);
    }

    Some(PreviewEvent::Unknown)
}

// ── prefix stripping ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

/// Strip dx's optional `"<elapsed>s "` prefix (e.g. `"40.28s "`, `"251.77s  "`) and its
/// optional level token (`ERROR`/`WARN`/`INFO`/`DEBUG`), returning whichever were found plus
/// the remaining message. Both are optional and independently detected so lines missing the
/// elapsed-seconds prefix (as some illustrative lines in the spike doc are) still classify.
fn split_prefix(trimmed: &str) -> (Option<Level>, &str) {
    let mut rest = trimmed;

    if let Some((first, remainder)) = rest.split_once(char::is_whitespace) {
        if let Some(digits) = first.strip_suffix('s') {
            if digits.parse::<f64>().is_ok() {
                rest = remainder.trim_start();
            }
        }
    }

    for (token, level) in
        [("ERROR", Level::Error), ("WARN", Level::Warn), ("INFO", Level::Info), ("DEBUG", Level::Debug)]
    {
        if let Some(stripped) = rest.strip_prefix(token) {
            if stripped.is_empty() || stripped.starts_with(char::is_whitespace) {
                return (Some(level), stripped.trim_start());
            }
        }
    }

    (None, rest)
}

// ── individual classifiers ───────────────────────────────────────────────────

/// Rustc's own bracketed diagnostic code marker (`error[E0433]: ...`), or the `WARN`-level
/// "could not compile" summary line dx prints alongside it — both quoted verbatim in the
/// spike's Q3 #3 sample.
fn is_rustc_error_marker(msg: &str) -> bool {
    msg.contains("error[E") || msg.contains("could not compile")
}

/// `"Build completed in 3.65s"` / `"Build completed successfully in 5.37s, launching app! 💫"`
/// — both quoted verbatim in the spike (Q3 #2). Returns `None` if the "in <n>s" shape isn't
/// found rather than panicking on format drift.
fn parse_build_completed_secs(msg: &str) -> Option<f64> {
    let after_in = msg.split_once("in ")?.1;
    let token = after_in.split(|c: char| c.is_whitespace() || c == ',').next()?;
    token.strip_suffix('s')?.parse::<f64>().ok()
}

/// Best-effort "the app is being served at <url>" detection. NOT grounded in a verbatim
/// spike-doc line (the spike confirmed the URL via `curl` against a guessed port, not a log
/// line) — kept for forward-compat with dx versions/configs that do print a startup banner
/// with the URL. `PreviewServer` builds the URL itself from the `--port` it chose, so nothing
/// load-bearing depends on this matching.
fn extract_serving_url(msg: &str) -> Option<String> {
    let lower = msg.to_ascii_lowercase();
    if !(lower.contains("serving") || lower.contains("listening")) {
        return None;
    }
    let idx = msg.find("http://").or_else(|| msg.find("https://"))?;
    let candidate = &msg[idx..];
    let end = candidate.find(char::is_whitespace).unwrap_or(candidate.len());
    Some(candidate[..end].trim_end_matches(['.', ',']).to_string())
}

/// Best-effort "a full rebuild just kicked off" hint. UNCONFIRMED by the spike: the doc's
/// samples never show a distinct default-verbosity "rebuild started" line (only the
/// terminal `Build completed`/`Build failed` outcomes and, at `--verbose`, incidental DEBUG
/// lines like `Running wasm-bindgen dx_src=bundle` that appear partway through a rebuild
/// already in flight). This matches common cargo/dx phrasing defensively; tighten once a
/// real captured line is available.
fn looks_like_rebuild_started_hint(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.starts_with("compiling") || lower.contains("rebuilding") || lower.starts_with("running cargo")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Hotreloading (Q2 RSX fast path, quoted verbatim in Q3 #1) ───────────

    #[test]
    fn hotreloading_line_with_elapsed_prefix() {
        let ev = parse_dx_line("177.27s  INFO Hotreloading: /src/frontend/timeline.rs.tmp.2169.de41133820de");
        assert_eq!(
            ev,
            Some(PreviewEvent::Hotreload {
                path: Some("/src/frontend/timeline.rs.tmp.2169.de41133820de".to_string())
            })
        );
    }

    #[test]
    fn hotreloading_line_without_elapsed_prefix() {
        let ev = parse_dx_line("INFO Hotreloading: /src/frontend/timeline.rs");
        assert_eq!(ev, Some(PreviewEvent::Hotreload { path: Some("/src/frontend/timeline.rs".to_string()) }));
    }

    #[test]
    fn hotreloading_atomic_rename_temp_path_still_parses() {
        // Q3 "benign noise" section: dx logs the transient temp-file path, not the real
        // filename, on atomic-write watchers (BSD sed -i '', many editors). The adapter must
        // not choke on it -- it's still a valid Hotreload event, just with a temp path.
        let ev = parse_dx_line("INFO Hotreloading: /src/frontend/.!81005!timeline.rs");
        assert_eq!(ev, Some(PreviewEvent::Hotreload { path: Some("/src/frontend/.!81005!timeline.rs".to_string()) }));
    }

    // ── Build completed / success (Q3 #2, quoted verbatim) ──────────────────

    #[test]
    fn build_completed_first_launch_suffix() {
        let ev = parse_dx_line("INFO Build completed successfully in 5.37s, launching app! \u{1f4ab}");
        assert_eq!(ev, Some(PreviewEvent::BuildOk { secs: Some(5.37) }));
    }

    #[test]
    fn build_completed_subsequent_rebuild_with_elapsed_prefix() {
        let ev = parse_dx_line("251.77s  INFO Build completed in 4.85s");
        assert_eq!(ev, Some(PreviewEvent::BuildOk { secs: Some(4.85) }));
    }

    #[test]
    fn build_completed_warm_target_variant() {
        let ev = parse_dx_line(" 76.12s  INFO Build completed in 3.65s   (fresh process, warm target/)");
        assert_eq!(ev, Some(PreviewEvent::BuildOk { secs: Some(3.65) }));
    }

    // ── Build failure: real rustc E0433 (Q3 #3, quoted verbatim) ────────────

    #[test]
    fn rustc_missing_crate_error_line() {
        let ev = parse_dx_line("39.86s ERROR cannot find module or crate `reqwest` in this scope");
        assert_eq!(
            ev,
            Some(PreviewEvent::BuildFailed { summary: "cannot find module or crate `reqwest` in this scope".to_string() })
        );
    }

    #[test]
    fn could_not_compile_warn_line_is_classified_as_build_failed() {
        let ev = parse_dx_line("40.27s  WARN error: could not compile `itinerary-app` (lib) due to 6 previous errors");
        assert!(matches!(ev, Some(PreviewEvent::BuildFailed { .. })));
    }

    #[test]
    fn terminal_build_failed_line() {
        let ev = parse_dx_line(
            "40.28s ERROR Build failed: cargo build finished with errors for target: itinerary-app [wasm32-unknown-unknown]",
        );
        assert_eq!(
            ev,
            Some(PreviewEvent::BuildFailed {
                summary: "Build failed: cargo build finished with errors for target: itinerary-app [wasm32-unknown-unknown]"
                    .to_string()
            })
        );
    }

    #[test]
    fn startup_config_error_is_also_build_failed() {
        // Blocker 1 from Q1: a Dioxus.toml schema mismatch fails before any build even
        // starts. It's still an ERROR-level dx line and the user needs to see it.
        let ev = parse_dx_line(
            "ERROR dx serve: Failed to parse Dioxus.toml at \"itinerary-app/Dioxus.toml\": TOML parse error at line 11, column 1",
        );
        assert!(matches!(ev, Some(PreviewEvent::BuildFailed { .. })));
    }

    #[test]
    fn generic_warn_caused_by_line_is_unknown_not_build_failed() {
        // Supplementary detail lines (WARN, not ERROR, no "could not compile"/"Build failed")
        // shouldn't each independently fire BuildFailed -- the terminal ERROR line already did.
        let ev = parse_dx_line("40.27s  WARN Caused by:");
        assert_eq!(ev, Some(PreviewEvent::Unknown));
    }

    // ── Benign noise (Q3 "benign noise" section, quoted verbatim) ───────────

    #[test]
    fn canonicalize_race_is_noise_not_unknown() {
        let ev = parse_dx_line("10.98s DEBUG Failed to canonicalize hotreloaded asset: No such file or directory (os error 2)");
        assert_eq!(ev, None);
    }

    #[test]
    fn failed_to_read_rust_file_is_noise() {
        let ev = parse_dx_line("10.98s DEBUG Failed to read rust file while hotreloading: \".!79680!timeline.rs\"");
        assert_eq!(ev, None);
    }

    #[test]
    fn blank_line_is_none() {
        assert_eq!(parse_dx_line(""), None);
        assert_eq!(parse_dx_line("   "), None);
    }

    // ── The silent-ignore DEBUG lines (Q3 #4, quoted verbatim) — Unknown ────

    #[test]
    fn rsx_not_parseable_debug_line_is_unknown() {
        // Deliberately NOT specially classified -- see the Unknown variant's doc comment and
        // verify.rs for how the silent-ignore gap is actually closed (timeout + cargo check).
        let ev = parse_dx_line("10.98s DEBUG Diff rsx returned not parseable");
        assert_eq!(ev, Some(PreviewEvent::Unknown));
    }

    #[test]
    fn ignoring_file_change_debug_line_is_unknown() {
        let ev = parse_dx_line("10.98s DEBUG Ignoring file change: /src/frontend/.!79680!timeline.rs dx_src=dev");
        assert_eq!(ev, Some(PreviewEvent::Unknown));
    }

    #[test]
    fn wasm_bindgen_bundling_debug_line_is_unknown() {
        let ev = parse_dx_line("73.78s DEBUG Running wasm-bindgen dx_src=bundle");
        assert_eq!(ev, Some(PreviewEvent::Unknown));
    }

    // ── Serving URL (best-effort, unconfirmed format — see module docs) ─────

    #[test]
    fn serving_url_best_effort_match() {
        let ev = parse_dx_line("INFO Serving your app at http://127.0.0.1:8080/");
        assert_eq!(ev, Some(PreviewEvent::Serving { url: "http://127.0.0.1:8080/".to_string() }));
    }

    #[test]
    fn a_url_without_serving_or_listening_wording_is_not_misclassified() {
        // Guards against false positives: a non-ERROR line that merely mentions a URL (e.g.
        // dx auto-installing wasm-bindgen-cli from GitHub, per Q1) must not be treated as a
        // Serving event just because it contains "http://".
        let ev = parse_dx_line("INFO fetching wasm-bindgen-cli@0.2.126 from http://github.com/example/release.tar.gz");
        assert_eq!(ev, Some(PreviewEvent::Unknown));
    }

    // ── RebuildStarted (best-effort, unconfirmed — see module docs) ─────────

    #[test]
    fn compiling_line_is_rebuild_started_hint() {
        let ev = parse_dx_line("INFO Compiling itinerary-app v0.1.0");
        assert_eq!(ev, Some(PreviewEvent::RebuildStarted));
    }

    // ── Noise / unrecognized ─────────────────────────────────────────────────

    #[test]
    fn unrelated_info_line_is_unknown() {
        let ev = parse_dx_line("INFO watching for file changes");
        assert_eq!(ev, Some(PreviewEvent::Unknown));
    }
}
