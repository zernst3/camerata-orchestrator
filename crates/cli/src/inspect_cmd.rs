//! `camerata inspect` — Step 0 of the refinement loop (see
//! `~/Documents/Repos/_workspace/brownfield-audits/CAMERATA_LOOP_HANDOFF.md`): a HEADLESS
//! brownfield inspection that runs the exact pipeline the cockpit's "Run audit" + "product
//! export" buttons drive, with no UI and no running BFF, so it can be scripted from a shell
//! (CI, batch scans, and the refinement loop that needs to grade a scan without a browser).
//!
//! Deliberately thin: every heavy-lifting call goes straight into `camerata_server`'s own
//! functions —
//! - [`camerata_server::onboard::scan_repos`] (stack detection + rule proposal),
//! - [`camerata_server::split_scannable_rules`] / [`camerata_server::merge_scan_preview`] (the
//!   same rule-tiering + scan-time mechanical-preview pass `onboard_audit`'s HTTP handler
//!   runs),
//! - [`camerata_server::onboard::audit_repos`] (the two-tier scan: the deterministic floor +
//!   the AI review, calibration, and per-rule alternative recommendation whenever a corpus is
//!   supplied — see that function's own doc comment on `corpus`/`chosen_options`),
//! - [`camerata_server::dep_audit::run_dep_audit`] (the dependency-vulnerability pass),
//! - [`camerata_server::report_export::build_report_json`] / `compile_pdf`,
//!   [`camerata_server::xlsx_export::build_workbook`] / `build_findings_export`, and
//!   [`camerata_server::report_filename_stem`] / `product_export_readme` / `build_product_zip`
//!   — the SAME functions `POST /api/projects/:id/product-export` (`export_product` in
//!   `camerata_server::lib`) calls, in the same order, producing the same zip.
//!
//! Nothing here re-implements any of that. This module supplies only the headless GLUE a
//! project-backed HTTP request would otherwise supply: turning a bare filesystem path into
//! the `(repo, PathBuf)` "sources" the server functions expect, picking the CURATION-DEFAULT
//! rule selection a fresh onboarding pre-checks (`ProposedRule::is_auto_recommended`, with no
//! human override — see [`curated_rule_selection`]), a tiny on-disk incremental-scan cache
//! (there is no project/database to key one to headlessly — see [`manifest_cache_path`]), and
//! printing the run's real token usage + cost at the end.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_server::ai_audit::{ActualUsage, ScanMode};
use camerata_server::llm::{resolve_backend, ProjectBackend};
use camerata_server::onboard::{ProposedRule, ScanReport, SelectedRule};
use camerata_server::report_export::ReportOptions;
use camerata_server::scan_cache::ScanManifest;
use camerata_server::usage_ledger::UsageLedger;

/// Parsed + validated arguments for `camerata inspect`. Kept separate from clap's `Command`
/// enum variant so the pipeline ([`run_inspect`]) and its tests never depend on clap at all.
#[derive(Debug, Clone)]
pub struct InspectArgs {
    /// The repo's local working tree to inspect. Must exist and be a directory
    /// ([`validate_args`] checks eagerly).
    pub repo: PathBuf,
    /// Where to write the product-export ZIP (PDF + xlsx + findings.json + README.txt). Its
    /// parent directory must already exist.
    pub export: PathBuf,
    /// The compliance-safety backend setting (`docs/design/2026-09-22_per-project-backend.md`)
    /// — resolved against API-key presence via [`resolve_backend`], exactly like a real
    /// project's `backend` field. Defaults to `Cli` (see `ProjectBackend`'s own `Default`).
    pub backend: ProjectBackend,
    /// Use the Anthropic Message Batches path (`ScanMode::Batch`): ~50% cheaper, latency is
    /// not returned until the whole batch completes. Requires `backend: Api` plus a
    /// configured key — [`validate_args`] rejects the combination eagerly otherwise.
    pub batch: bool,
    /// Explicit model override for the audit pass. `None`/blank falls back to
    /// [`camerata_server::llm::DEFAULT_MODEL`] — the same floor `step_model` uses for the
    /// project-less edge.
    pub model: Option<String>,
    /// Explicit model override for the calibration pass. Same fallback as `model`.
    pub calibration_model: Option<String>,
    /// Ignore the on-disk incremental-scan cache (see [`manifest_cache_path`]) and force a
    /// full re-scan of every file.
    pub full: bool,
}

/// What [`run_inspect`] reports back to its caller. `main.rs`'s handler prints this; tests
/// assert on the struct directly rather than parsing stdout.
#[derive(Debug, Clone)]
pub struct InspectOutcome {
    pub export_path: PathBuf,
    pub export_zip_bytes: usize,
    pub backend: ProjectBackend,
    pub batch: bool,
    /// Real token usage + cost for this run's AI passes (audit + calibration), when the AI
    /// review actually ran. `None` on an AI-off / compliance-blocked (floor-only) run — see
    /// `ScanReport::actual_usage`'s own doc comment.
    pub actual_usage: Option<ActualUsage>,
    pub findings_count: usize,
}

/// Validate the arguments BEFORE touching the filesystem for real work or resolving a
/// backend — every case here is a user-fixable mistake, never a model call, so it's cheap to
/// check eagerly with a clear message rather than surfacing a deep-stack I/O error later.
///
/// `has_api_key` is passed in (rather than this function reading the environment itself) so
/// callers — including tests — can exercise the api+key / api+no-key / batch branches
/// deterministically without mutating real process env state. [`run_inspect`] is the only
/// production caller, and it passes the REAL `ANTHROPIC_API_KEY` presence.
pub fn validate_args(args: &InspectArgs, has_api_key: bool) -> Result<(), String> {
    if !args.repo.is_dir() {
        return Err(format!(
            "--repo {} does not exist or is not a directory",
            args.repo.display()
        ));
    }
    let export_dir: &Path = match args.export.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !export_dir.is_dir() {
        return Err(format!(
            "--export directory {} does not exist",
            export_dir.display()
        ));
    }
    if args.batch && !matches!(args.backend, ProjectBackend::Api) {
        return Err(
            "--batch requires --backend api (Anthropic Message Batches is an API-only feature; \
             the CLI/subscription backend has no batches endpoint)"
                .to_string(),
        );
    }
    if args.batch && !has_api_key {
        return Err(
            "--batch requires an ANTHROPIC_API_KEY (Message Batches needs the API backend WITH \
             a configured key)"
                .to_string(),
        );
    }
    Ok(())
}

/// The rule ids/directives a FRESH onboarding scan pre-checks with no human override — i.e.
/// every [`ProposedRule`] with `is_auto_recommended` set (grounded/verified rules; see that
/// field's own doc comment — `draft`/`needs_recheck` rules are listed but require an explicit
/// opt-in, so they're excluded here exactly like an untouched onboarding table would leave
/// them unchecked). This is the "curation defaults" half of the brownfield flow.
///
/// Mirrors `crates/ui/src/cockpit/rules.rs`'s `resolve_directive` closure evaluated with an
/// EMPTY "chosen" map (nothing manually picked) — precisely the state a first, human-untouched
/// scan is in. The model's OWN automatic alternative recommendation (deciding WHICH
/// alternative actually applies, e.g. by reading the codebase) still happens downstream,
/// inside `audit_repos`, whenever it is given the loaded corpus (see this module's own doc
/// comment); this function only decides which rules enter the scan at all, using each rule's
/// pre-declared `default_option` as the starting directive — exactly the directive a human who
/// changed nothing in the table would have sent.
pub fn curated_rule_selection(proposed: &[ProposedRule]) -> Vec<SelectedRule> {
    proposed
        .iter()
        .filter(|r| r.is_auto_recommended)
        .map(|r| SelectedRule {
            id: r.id.clone(),
            directive: resolve_default_directive(r),
            repos: r.repos.clone(),
        })
        .collect()
}

/// A single proposed rule's default directive: its default option's text when it has options,
/// else its title (mechanical/content rules with no alternatives carry no directive text of
/// their own). Falls back to the title if the declared `default_option` id doesn't resolve to
/// a real option or resolves to blank text — never an empty directive.
fn resolve_default_directive(r: &ProposedRule) -> String {
    if r.options.is_empty() {
        return r.title.clone();
    }
    r.default_option
        .as_ref()
        .and_then(|oid| r.options.iter().find(|o| &o.id == oid))
        .map(|o| o.directive.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r.title.clone())
}

/// Where the incremental-scan manifest is cached BETWEEN headless runs against the same repo,
/// absent any project/database to key it to. A single dotfile inside the scanned repo,
/// mirroring `.eslintcache`/`.turbo`-style tool caches: disposable, not meant to be committed
/// (callers should `.gitignore` it), and read only as input to `audit_repos`'s own
/// `incremental_prior` parameter — never consulted by anything else.
fn manifest_cache_path(repo: &Path) -> PathBuf {
    repo.join(".camerata-scan-cache.json")
}

/// Best-effort load: any error (missing file, corrupt JSON, a schema from a future/older
/// binary version) degrades to `None` — exactly `audit_repos`'s own "no cache" full-scan path,
/// never a hard failure over a cache file that is, by construction, disposable.
fn load_prior_manifest(repo: &Path) -> Option<ScanManifest> {
    let bytes = std::fs::read(manifest_cache_path(repo)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Best-effort save. Failing to persist the cache never fails the inspection itself — it just
/// means the NEXT run does a full scan instead of an incremental one.
fn save_manifest(repo: &Path, manifest: &ScanManifest) {
    let Ok(bytes) = serde_json::to_vec_pretty(manifest) else {
        return;
    };
    if let Err(e) = std::fs::write(manifest_cache_path(repo), bytes) {
        eprintln!(
            "camerata inspect: could not persist the incremental-scan cache at {}: {e} (the \
             next run will do a full scan instead)",
            manifest_cache_path(repo).display()
        );
    }
}

/// Build the product-export ZIP bytes for `report`, via the EXACT SAME `report_export` /
/// `xlsx_export` / zip-assembly functions the `POST /api/projects/:id/product-export` HTTP
/// handler (`export_product`) calls, in the same order, with the same "nothing manually
/// triaged yet" defaults a headless run implies: an empty client-disposition map and
/// `ReportOptions::default()` folded through `apply_env_defaults()` — exactly like that
/// handler. Pure aside from the `compile_pdf` shell-out to `typst`; no filesystem writes.
/// Returns `(filename_stem, zip_bytes)`.
pub async fn build_product_export_bytes(
    report: &ScanReport,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Result<(String, Vec<u8>), String> {
    let dispositions: HashMap<String, camerata_server::report_export::DispositionWire> =
        HashMap::new();
    let mut options = ReportOptions::default();
    options.apply_env_defaults();

    let json =
        camerata_server::report_export::build_report_json(report, &dispositions, corpus, &options);
    let pdf_bytes = camerata_server::report_export::compile_pdf(&json)
        .await
        .map_err(|e| e.to_string())?;
    let xlsx_bytes =
        camerata_server::xlsx_export::build_workbook(report, &dispositions, corpus, &options)
            .map_err(|e| e.to_string())?;
    let findings_export = camerata_server::xlsx_export::build_findings_export(
        report,
        &dispositions,
        corpus,
        &json,
        &options.chosen_options,
    );
    let findings_json_bytes = serde_json::to_vec_pretty(&findings_export)
        .map_err(|e| format!("could not serialize findings.json: {e}"))?;

    let stem = camerata_server::report_filename_stem(report);
    let readme = camerata_server::product_export_readme(&stem, &json);
    let zip_bytes = camerata_server::build_product_zip(
        &stem,
        &pdf_bytes,
        &xlsx_bytes,
        &findings_json_bytes,
        &readme,
    )
    .map_err(|e| format!("could not assemble the product export zip: {e}"))?;

    Ok((stem, zip_bytes))
}

/// Render the run's actual token usage + cost for the CLI's final stdout line — the number
/// the refinement loop's budget depends on (see this module's doc comment). Sourced entirely
/// from [`ScanReport::actual_usage`] (`ai_audit::ActualUsage`), the SAME per-run meter
/// `audit_repos` stamps onto every report; this function only formats it, it does no
/// accounting of its own.
pub fn format_usage_report(usage: Option<&ActualUsage>, batch: bool) -> String {
    let mode = if batch {
        "batch (Anthropic Message Batches, ~50% off real-time pricing)"
    } else {
        "real-time"
    };
    match usage {
        None => format!("Token usage: none recorded (no AI review ran this scan) — mode: {mode}"),
        Some(u) => {
            let cost = if u.cost_complete {
                format!("${:.4}", u.cost_usd)
            } else if u.calls == 0 {
                "$0.0000".to_string()
            } else {
                format!(
                    "${:.4} (PARTIAL — not every call reported a cost)",
                    u.cost_usd
                )
            };
            format!(
                "Token usage — input: {}, output: {}, cache read: {}, cache write: {}, calls: \
                 {}, cost: {cost}, mode: {mode}",
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens,
                u.calls,
            )
        }
    }
}

/// The real pipeline, parameterized by API-key presence so tests never depend on (or mutate)
/// process env state — see [`validate_args`]'s doc comment for the same rationale.
/// [`run_inspect`] is the thin, production-only wrapper that reads `ANTHROPIC_API_KEY` for
/// real and delegates here.
pub async fn run_inspect_with_key_presence(
    args: InspectArgs,
    has_api_key: bool,
) -> Result<InspectOutcome, String> {
    validate_args(&args, has_api_key)?;

    // A headless run has no `owner/repo` spec — the scanned directory's own name stands in
    // for it (tags every finding/provenance entry, same role `spec` plays in `resolve_local_sources`).
    let repo_spec = args
        .repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string();
    let sources = vec![(repo_spec, args.repo.clone())];

    // ── Phase 1: stack detection + rule proposal ────────────────────────────────────────
    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new()).await;
    let selected = curated_rule_selection(&scan.proposed_rules);

    // ── Phase 2: load the corpus ONCE + derive the excluded-mechanical/preview lists ─────
    // `split_scannable_rules` is still the source of `excluded_mechanical` (the report's
    // "enforced in CI, not scanned" disclosure) and `preview_rules` (the scan-time preview
    // linter pass) — but its first return value (the CI-tier-STRIPPED rule list) is
    // deliberately discarded here rather than fed into `audit_repos` below. `audit_repos`
    // needs the FULL curated `selected` (including CI-tier ids) to arm the deterministic
    // architectural engine (`repo_selected_ids`) correctly; it excludes CI-tier ids from its
    // OWN LLM-prompt construction itself now (see `onboard::audit::is_ci_tier_rule`). Handing
    // it the pre-stripped list here — as this function used to — silently starved that engine
    // of every CI-tier corpus rule id in a real headless scan (the C3-3 bug).
    let (_ai_scannable_only, excluded_mechanical, preview_rules, corpus) =
        camerata_server::split_scannable_rules(selected.clone()).await;

    // ── Backend resolution + the process-local usage ledger ────────────────────────────
    let backend_resolution = resolve_backend(args.backend, has_api_key);
    let mode = if args.batch {
        ScanMode::parse(Some("batch"))
    } else {
        ScanMode::Parallel
    };
    let model = args.model.as_deref().filter(|m| !m.trim().is_empty());
    let calibration_model = args
        .calibration_model
        .as_deref()
        .filter(|m| !m.trim().is_empty());
    let ledger = Arc::new(UsageLedger::new());

    let incremental_prior = if args.full {
        None
    } else {
        load_prior_manifest(&args.repo)
    };

    // ── Phase 3: the two-tier scan (deterministic floor + AI review), calibration, and
    // per-rule alternative recommendation (when the corpus resolves a rule's alternatives —
    // the "investigation" of which option actually applies) — ALL inside `audit_repos`, the
    // exact function `onboard_audit`'s HTTP handler calls. `chosen_options` stays empty: no
    // project-level override exists headlessly, so the model recommends fully automatically —
    // the curation-default state. ──
    let (mut report, manifest) = camerata_server::onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(),
        model,
        calibration_model,
        mode,
        false, // thorough — not exposed on Step 0's flag surface; matches AuditReq's default
        None,  // feedback — no live transcript sink headlessly
        None,  // job — no job store headlessly (this call blocks until done, like `onboard_audit`)
        incremental_prior.as_ref(),
        false, // deep — the opt-in compliance tier; not exposed on Step 0's flag surface
        true,  // soc2_enabled — irrelevant with `deep` off
        true,  // run_ai_review
        true,  // run_deterministic
        Some(ledger.clone()),
        backend_resolution,
        corpus.as_ref(),
        &HashMap::new(),
    )
    .await;

    save_manifest(&args.repo, &manifest);
    report.excluded_mechanical_rules = excluded_mechanical;

    // ── Phase 4: the SAME scan-time preview-linter + dep-audit passes `onboard_audit` runs ──
    camerata_server::merge_scan_preview(
        &mut report,
        &sources,
        &preview_rules,
        corpus.as_ref(),
        None,
    )
    .await;
    for (spec, dir) in &sources {
        let (dep_findings, dep_note) = camerata_server::dep_audit::run_dep_audit(spec, dir).await;
        if !dep_findings.is_empty() {
            report.findings.extend(dep_findings);
        }
        if let Some(note) = dep_note {
            report.coverage_notes.push(note);
        }
    }

    // ── Phase 5: export — the exact `export_product` HTTP handler's assembly ───────────
    let (_stem, zip_bytes) = build_product_export_bytes(&report, corpus.as_ref()).await?;
    std::fs::write(&args.export, &zip_bytes)
        .map_err(|e| format!("could not write {}: {e}", args.export.display()))?;

    Ok(InspectOutcome {
        export_path: args.export,
        export_zip_bytes: zip_bytes.len(),
        backend: args.backend,
        batch: args.batch,
        actual_usage: report.actual_usage.clone(),
        findings_count: report.findings.len(),
    })
}

/// Production entry point: reads the real `ANTHROPIC_API_KEY` presence from the environment
/// and delegates to [`run_inspect_with_key_presence`]. See that function's doc comment, and
/// [`validate_args`]'s, for why the key check is a separate, injectable parameter.
pub async fn run_inspect(args: InspectArgs) -> Result<InspectOutcome, String> {
    let has_api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .is_some();
    run_inspect_with_key_presence(args, has_api_key).await
}

/// Format the final report block `main.rs` prints after a successful run.
pub fn format_outcome_report(outcome: &InspectOutcome) -> String {
    let backend_label = match outcome.backend {
        ProjectBackend::Cli => "cli",
        ProjectBackend::Api => "api",
    };
    format!(
        "camerata inspect: wrote {} ({} bytes) — backend: {backend_label}, findings: {}\n{}",
        outcome.export_path.display(),
        outcome.export_zip_bytes,
        outcome.findings_count,
        format_usage_report(outcome.actual_usage.as_ref(), outcome.batch),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args(repo: PathBuf, export: PathBuf) -> InspectArgs {
        InspectArgs {
            repo,
            export,
            backend: ProjectBackend::Cli,
            batch: false,
            model: None,
            calibration_model: None,
            full: false,
        }
    }

    // ── validate_args ────────────────────────────────────────────────────────────────

    #[test]
    fn validate_args_rejects_a_missing_repo_directory() {
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(dir.path().join("does-not-exist"), dir.path().to_path_buf());
        let err = validate_args(&args, false).expect_err("must reject a missing --repo");
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("--repo"), "{err}");
    }

    #[test]
    fn validate_args_rejects_a_repo_path_that_is_a_file_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not-a-dir.txt");
        std::fs::write(&file_path, "x").unwrap();
        let args = base_args(file_path, dir.path().to_path_buf());
        assert!(validate_args(&args, false).is_err());
    }

    #[test]
    fn validate_args_rejects_a_missing_export_directory() {
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(
            dir.path().to_path_buf(),
            dir.path().join("nope").join("out.zip"),
        );
        let err = validate_args(&args, false).expect_err("must reject a missing --export dir");
        assert!(err.contains("--export directory"), "{err}");
    }

    #[test]
    fn validate_args_accepts_an_export_path_with_no_directory_component() {
        // `--export out.zip` (relative, no leading dir) must resolve against ".", not error.
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(dir.path().to_path_buf(), PathBuf::from("out.zip"));
        assert!(validate_args(&args, false).is_ok());
    }

    #[test]
    fn validate_args_rejects_batch_on_the_cli_backend() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.batch = true;
        args.backend = ProjectBackend::Cli;
        let err = validate_args(&args, true).expect_err("batch must require the api backend");
        assert!(err.contains("--batch requires --backend api"), "{err}");
    }

    #[test]
    fn validate_args_rejects_batch_on_api_without_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.batch = true;
        args.backend = ProjectBackend::Api;
        let err = validate_args(&args, false).expect_err("batch must require a key");
        assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
    }

    #[test]
    fn validate_args_accepts_batch_on_api_with_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.batch = true;
        args.backend = ProjectBackend::Api;
        assert!(validate_args(&args, true).is_ok());
    }

    #[test]
    fn validate_args_accepts_a_healthy_cli_backend_request() {
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        assert!(validate_args(&args, false).is_ok());
    }

    // ── curated_rule_selection ───────────────────────────────────────────────────────

    fn opt(id: &str, directive: &str) -> camerata_server::onboard::RuleOptionView {
        camerata_server::onboard::RuleOptionView {
            id: id.to_string(),
            label: id.to_string(),
            directive: directive.to_string(),
            why: String::new(),
        }
    }

    fn proposed(id: &str, is_auto_recommended: bool) -> ProposedRule {
        ProposedRule {
            id: id.to_string(),
            title: format!("Title for {id}"),
            kind: "review".to_string(),
            enforcement: "structured".to_string(),
            options: Vec::new(),
            default_option: None,
            verification: "grounded".to_string(),
            sources: Vec::new(),
            decision_question: None,
            decision_why: None,
            scope: "repo-local".to_string(),
            domain: "architecture".to_string(),
            enforcement_point: "content".to_string(),
            repos: Vec::new(),
            placement: "content".to_string(),
            finding_count: 0,
            recommended: is_auto_recommended,
            is_auto_recommended,
        }
    }

    #[test]
    fn curated_rule_selection_keeps_only_auto_recommended_rules() {
        let proposed_rules = vec![proposed("RULE-A", true), proposed("RULE-B", false)];
        let selected = curated_rule_selection(&proposed_rules);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "RULE-A");
    }

    #[test]
    fn curated_rule_selection_uses_the_default_options_directive() {
        let mut rule = proposed("RULE-A", true);
        rule.options = vec![opt("a", "Do it the A way"), opt("b", "Do it the B way")];
        rule.default_option = Some("b".to_string());
        let selected = curated_rule_selection(&[rule]);
        assert_eq!(selected[0].directive, "Do it the B way");
    }

    #[test]
    fn curated_rule_selection_falls_back_to_the_title_with_no_options() {
        let rule = proposed("RULE-A", true);
        let selected = curated_rule_selection(&[rule]);
        assert_eq!(selected[0].directive, "Title for RULE-A");
    }

    #[test]
    fn curated_rule_selection_falls_back_to_the_title_when_default_option_is_unresolvable() {
        let mut rule = proposed("RULE-A", true);
        rule.options = vec![opt("a", "Do it the A way")];
        rule.default_option = Some("missing-id".to_string());
        let selected = curated_rule_selection(&[rule]);
        assert_eq!(selected[0].directive, "Title for RULE-A");
    }

    // ── format_usage_report / format_outcome_report ─────────────────────────────────

    fn synthetic_usage() -> ActualUsage {
        ActualUsage {
            input_tokens: 123_456,
            output_tokens: 7_890,
            cost_usd: 2.5,
            calls: 4,
            cost_complete: true,
            cache_read_input_tokens: 50_000,
            cache_creation_input_tokens: 1_000,
        }
    }

    #[test]
    fn format_usage_report_shows_input_output_cache_and_cost() {
        let usage = synthetic_usage();
        let text = format_usage_report(Some(&usage), false);
        assert!(text.contains("input: 123456"), "{text}");
        assert!(text.contains("output: 7890"), "{text}");
        assert!(text.contains("cache read: 50000"), "{text}");
        assert!(text.contains("cache write: 1000"), "{text}");
        assert!(text.contains("calls: 4"), "{text}");
        assert!(text.contains("$2.5000"), "{text}");
        assert!(text.contains("mode: real-time"), "{text}");
    }

    #[test]
    fn format_usage_report_labels_batch_mode() {
        let usage = synthetic_usage();
        let text = format_usage_report(Some(&usage), true);
        assert!(text.to_lowercase().contains("batch"), "{text}");
    }

    #[test]
    fn format_usage_report_flags_a_partial_cost_total() {
        let mut usage = synthetic_usage();
        usage.cost_complete = false;
        let text = format_usage_report(Some(&usage), false);
        assert!(text.contains("PARTIAL"), "{text}");
    }

    #[test]
    fn format_usage_report_handles_no_ai_review_having_run() {
        let text = format_usage_report(None, false);
        assert!(text.contains("none recorded"), "{text}");
    }

    #[test]
    fn format_outcome_report_names_the_export_path_and_backend() {
        let outcome = InspectOutcome {
            export_path: PathBuf::from("/tmp/out.zip"),
            export_zip_bytes: 4096,
            backend: ProjectBackend::Api,
            batch: false,
            actual_usage: Some(synthetic_usage()),
            findings_count: 3,
        };
        let text = format_outcome_report(&outcome);
        assert!(text.contains("/tmp/out.zip"), "{text}");
        assert!(text.contains("4096"), "{text}");
        assert!(text.contains("backend: api"), "{text}");
        assert!(text.contains("findings: 3"), "{text}");
    }
}
