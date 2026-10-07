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
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_server::ai_audit::{ActualUsage, FailedPass, ScanMode};
use camerata_server::llm::{resolve_backend, ProjectBackend};
use camerata_server::onboard::{ProposedRule, ScanReport, SelectedRule};
use camerata_server::report_export::ReportOptions;
use camerata_server::scan_cache::ScanManifest;
use camerata_server::scan_ledger::{RuleLedgerEntry, RuleTier, ScanLedger};
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
    /// Additionally print the per-rule detail section of the pipeline-integrity ledger
    /// summary (see [`render_ledger_summary`]) — rule id, ran?, files evaluated, findings, one
    /// line per rule. Default output stays at the concise stage/family/not-run/failed-passes
    /// summary.
    pub verbose: bool,
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
    /// The pipeline-integrity ledger summary (see [`render_ledger_summary`]) — stages,
    /// detection families, not-run rules, and any `FailedPass` disclosures, rendered from
    /// `report.ledger`/`report.failed_passes` exactly as they stood after the scan's final
    /// reconciliation pass. Printed by [`format_outcome_report`]; tests assert on this field
    /// directly rather than parsing stdout, matching this module's existing convention.
    pub ledger_summary: String,
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

    // The ledger summary is rendered from `report.ledger`/`report.failed_passes` AS THEY STAND
    // right here — after `audit_repos` (Phase 3) and `merge_scan_preview` (Phase 4, which runs
    // `reconcile_external_tool_ledger` and is the LAST thing that corrects the ledger's
    // `ExternalTool` entries from speculative to real; see that function's own doc comment).
    // Computed before export so a PDF/typst failure below still leaves `report` fully
    // reconciled, even though the summary itself is only surfaced via the returned
    // `InspectOutcome`, never written into the export.
    let ledger_summary = render_ledger_summary(&report.ledger, &report.failed_passes, args.verbose);

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
        ledger_summary,
    })
}

/// Render the scan's pipeline-integrity ledger (`crate::scan_ledger::ScanLedger`) into a
/// concise, human-scannable block for `camerata inspect`'s final stdout — see this module's
/// doc comment and the ledger module's own: a scan that completes "successfully" while an
/// entire detection layer (e.g. the external-tool/taint pass) silently never ran was, before
/// this, invisible from the console. The ledger already recorded the facts; nothing printed
/// them to a human.
///
/// Pure function — deterministic given `ledger`/`failed_passes`, no I/O, no wall-clock —
/// which is what makes it unit-testable directly against hand-built ledgers (see this module's
/// `tests` below) rather than only through a full scan.
///
/// Four sections, always in this order, concise mode (`verbose == false`):
/// 1. **PIPELINE STAGES** — every stage's `rows_in -> rows_out` plus its accounted
///    dispositions; any stage with `unaccounted > 0` gets a loud `!!` marker (never silently
///    folded into a clean-looking line).
/// 2. **DETECTION FAMILIES** — one line per [`RuleTier`] (`RuleTier::all()`, so a family with
///    NO recorded rules still gets a line, not silent omission), each showing how many rules
///    ran/were skipped and how many findings it emitted, plus the most common skip reasons. A
///    family with zero rules recorded OR zero rules that actually ran is flagged with `!!` —
///    the exact "an entire detection layer never executed" shape this was built to catch.
/// 3. **NOT RUN** — every rule [`ScanLedger::excluded_rules`] confirms did not run, with its
///    real reason, so a dark layer can never hide even when every stage happens to reconcile
///    cleanly.
/// 4. **FAILED PASSES** — every `FailedPass` disclosure, printed plainly (repo, pass name,
///    reason), verbatim — never summarized or truncated.
///
/// `verbose == true` appends a fifth PER-RULE DETAIL section: one line per rule (id, ran?,
/// files evaluated, findings), sorted by rule id for stable output.
pub fn render_ledger_summary(
    ledger: &ScanLedger,
    failed_passes: &[FailedPass],
    verbose: bool,
) -> String {
    let mut out = String::new();

    writeln!(out, "== PIPELINE STAGES ==").ok();
    if ledger.stages().is_empty() {
        writeln!(out, "  (no stages recorded this scan)").ok();
    } else {
        for stage in ledger.stages() {
            let marker = if stage.unaccounted > 0 {
                "  !! UNACCOUNTED"
            } else {
                ""
            };
            writeln!(
                out,
                "  {:<28} rows {:>5} -> {:<5}  merged={:<3} held={:<3} informational={:<3} \
                 deduped={:<3} unaccounted={}{}",
                stage.stage,
                stage.rows_in,
                stage.rows_out,
                stage.merged_into.len(),
                stage.routed_held,
                stage.routed_informational,
                stage.deduped,
                stage.unaccounted,
                marker,
            )
            .ok();
        }
    }

    writeln!(out, "\n== DETECTION FAMILIES ==").ok();
    for tier in RuleTier::all() {
        let rules: Vec<&RuleLedgerEntry> = ledger.rules().filter(|r| r.tier == tier).collect();
        let ran = rules.iter().filter(|r| r.ran).count();
        let skipped = rules.len() - ran;
        let findings: usize = rules.iter().map(|r| r.findings_emitted).sum();

        if rules.is_empty() {
            writeln!(
                out,
                "  {:<40} !! ZERO RULES RECORDED — this family never ran at all this scan",
                tier.family_label(),
            )
            .ok();
            continue;
        }
        if ran == 0 {
            writeln!(
                out,
                "  {:<40} !! ran=0 skipped={skipped} findings={findings} — NO rule in this \
                 family ran this scan",
                tier.family_label(),
            )
            .ok();
        } else {
            writeln!(
                out,
                "  {:<40} ran={ran} skipped={skipped} findings={findings}",
                tier.family_label(),
            )
            .ok();
        }
        if skipped > 0 {
            let mut reason_counts: HashMap<&str, usize> = HashMap::new();
            for r in rules.iter().filter(|r| !r.ran) {
                *reason_counts
                    .entry(r.skip_reason.as_deref().unwrap_or("(no reason given)"))
                    .or_insert(0) += 1;
            }
            let mut reasons: Vec<(&str, usize)> = reason_counts.into_iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            for (reason, count) in reasons.into_iter().take(3) {
                writeln!(out, "      skipped x{count}: {reason}").ok();
            }
        }
    }

    let excluded = ledger.excluded_rules();
    writeln!(out, "\n== NOT RUN ({}) ==", excluded.len()).ok();
    if excluded.is_empty() {
        writeln!(out, "  (every selected rule ran this scan)").ok();
    } else {
        for (id, reason) in &excluded {
            writeln!(out, "  {id:<40} {reason}").ok();
        }
    }

    writeln!(out, "\n== FAILED PASSES ({}) ==", failed_passes.len()).ok();
    if failed_passes.is_empty() {
        writeln!(out, "  (none)").ok();
    } else {
        for fp in failed_passes {
            writeln!(out, "  [{}] {}: {}", fp.repo, fp.pass, fp.reason).ok();
        }
    }

    if verbose {
        let mut rules: Vec<&RuleLedgerEntry> = ledger.rules().collect();
        rules.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
        writeln!(out, "\n== PER-RULE DETAIL ({}) ==", rules.len()).ok();
        for r in rules {
            writeln!(
                out,
                "  {:<40} ran={:<5} files_evaluated={:<6} findings={}",
                r.rule_id, r.ran, r.files_evaluated, r.findings_emitted,
            )
            .ok();
        }
    }

    out.trim_end().to_string()
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
        "camerata inspect: wrote {} ({} bytes) — backend: {backend_label}, findings: {}\n{}\n\n{}",
        outcome.export_path.display(),
        outcome.export_zip_bytes,
        outcome.findings_count,
        format_usage_report(outcome.actual_usage.as_ref(), outcome.batch),
        outcome.ledger_summary,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_server::scan_ledger::StageAccounting;

    fn base_args(repo: PathBuf, export: PathBuf) -> InspectArgs {
        InspectArgs {
            repo,
            export,
            backend: ProjectBackend::Cli,
            batch: false,
            model: None,
            calibration_model: None,
            full: false,
            verbose: false,
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
            ledger_summary: "== PIPELINE STAGES ==\n  (no stages recorded this scan)".to_string(),
        };
        let text = format_outcome_report(&outcome);
        assert!(text.contains("/tmp/out.zip"), "{text}");
        assert!(text.contains("4096"), "{text}");
        assert!(text.contains("backend: api"), "{text}");
        assert!(text.contains("findings: 3"), "{text}");
        assert!(
            text.contains("PIPELINE STAGES"),
            "the ledger summary must be appended to the final report block: {text}"
        );
    }

    // ── render_ledger_summary ────────────────────────────────────────────────────────

    /// A fully healthy ledger: one rule per family, all ran, a single cleanly-reconciled
    /// stage, no not-run rules, no failed passes. No `!!` marker anywhere — a clean scan must
    /// read as unambiguously clean.
    #[test]
    fn render_ledger_summary_clean_ledger_has_no_alarms() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );
        ledger.record_rule(
            "ARCH-STRICT-LAYERING-1",
            RuleTier::Architectural,
            true,
            None,
            5,
            0,
        );
        ledger.record_rule(
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::ExternalTool,
            true,
            None,
            8,
            0,
        );
        ledger.record_rule("AI-SITE-DEFECT-1", RuleTier::Semantic, true, None, 3, 1);
        ledger.record_stage("cross-family-merge", 1, 1, StageAccounting::default());

        let text = render_ledger_summary(&ledger, &[], false);

        assert!(text.contains("PIPELINE STAGES"), "{text}");
        assert!(text.contains("DETECTION FAMILIES"), "{text}");
        assert!(text.contains("NOT RUN (0)"), "{text}");
        assert!(text.contains("FAILED PASSES (0)"), "{text}");
        assert!(
            !text.contains("!!"),
            "a clean ledger must raise no alarm markers: {text}"
        );
    }

    /// The core integrity check, surfaced to a human: a stage that lost a row with no recorded
    /// disposition must render with the loud `!!` marker, never blend into a normal-looking
    /// line.
    #[test]
    fn render_ledger_summary_flags_a_stage_with_unaccounted_rows() {
        let mut ledger = ScanLedger::new();
        ledger.record_stage("lossy-stage", 10, 8, StageAccounting::default()); // 2 vanish

        let text = render_ledger_summary(&ledger, &[], false);

        let stage_line = text
            .lines()
            .find(|l| l.contains("lossy-stage"))
            .expect("the lossy stage must have its own line");
        assert!(
            stage_line.contains("!!"),
            "a stage with unaccounted rows must carry a loud marker: {stage_line}"
        );
        assert!(stage_line.contains("unaccounted=2"), "{stage_line}");
    }

    /// The exact defect this feature exists to catch: an entire detection family (here,
    /// external-tool/taint) recorded ZERO rules this scan. It must be impossible to miss —
    /// flagged loudly, not silently absent from the output.
    #[test]
    fn render_ledger_summary_flags_a_family_with_zero_rules_recorded() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );
        // No ExternalTool, Architectural, or Semantic rule recorded at all this scan.

        let text = render_ledger_summary(&ledger, &[], false);

        let family_line = text
            .lines()
            .find(|l| l.contains(RuleTier::ExternalTool.family_label()))
            .expect("every family must get its own line even with zero rules");
        assert!(
            family_line.contains("!!") && family_line.to_uppercase().contains("ZERO"),
            "a family with no recorded rules must be flagged loudly, not silently omitted: {family_line}"
        );
    }

    /// The flip side of the same defect: a family DID get ledger entries, but every single one
    /// is recorded as not-run (e.g. the commodity taint pass's rules were all corrected to
    /// `ran = false` after the tool failed to provision). `ran == 0` must be flagged exactly
    /// like zero rules recorded — the family never actually executed either way.
    #[test]
    fn render_ledger_summary_flags_a_family_whose_rules_all_failed_to_run() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::ExternalTool,
            false,
            Some("commodity taint pass did not run: semgrep binary not found".to_string()),
            0,
            0,
        );

        let text = render_ledger_summary(&ledger, &[], false);

        let family_line = text
            .lines()
            .find(|l| l.contains(RuleTier::ExternalTool.family_label()))
            .expect("the external-tool family line must be present");
        assert!(
            family_line.contains("!!") && family_line.contains("ran=0"),
            "a family with rules recorded but none that ran must be flagged: {family_line}"
        );
    }

    /// Not-run rules (any reason) must render with their real reason text in the NOT RUN
    /// section, so a dark layer can never hide even when every stage happens to reconcile.
    #[test]
    fn render_ledger_summary_renders_not_run_reasons() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "PYTHON-PARAMETERIZED-SQL-1",
            RuleTier::Architectural,
            false,
            Some("declares mechanical enforcement but has no wired detector".to_string()),
            0,
            0,
        );
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            false,
            Some("deterministic scan deselected for this run".to_string()),
            0,
            0,
        );

        let text = render_ledger_summary(&ledger, &[], false);

        assert!(text.contains("NOT RUN (2)"), "{text}");
        assert!(
            text.contains("PYTHON-PARAMETERIZED-SQL-1") && text.contains("no wired detector"),
            "{text}"
        );
        assert!(
            text.contains("SEC-NO-HARDCODED-SECRETS-1")
                && text.contains("deterministic scan deselected for this run"),
            "{text}"
        );
    }

    /// `FailedPass` disclosures must be printed plainly — repo, pass name, and the real reason
    /// text verbatim, never summarized away.
    #[test]
    fn render_ledger_summary_prints_failed_passes_plainly() {
        let ledger = ScanLedger::new();
        let failed_passes = vec![FailedPass {
            repo: "acme/widgets".to_string(),
            pass: "commodity taint pass".to_string(),
            reason: "SEC-NO-RAW-SQL-CONCAT-1: commodity taint pass did not run: semgrep absent"
                .to_string(),
        }];

        let text = render_ledger_summary(&ledger, &failed_passes, false);

        assert!(text.contains("FAILED PASSES (1)"), "{text}");
        assert!(text.contains("acme/widgets"), "{text}");
        assert!(text.contains("commodity taint pass"), "{text}");
        assert!(text.contains("semgrep absent"), "{text}");
    }

    /// Default (non-verbose) output must NOT list individual rules — only `--verbose` does.
    #[test]
    fn render_ledger_summary_default_omits_per_rule_detail() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );

        let concise = render_ledger_summary(&ledger, &[], false);
        assert!(!concise.contains("PER-RULE DETAIL"), "{concise}");

        let verbose = render_ledger_summary(&ledger, &[], true);
        assert!(verbose.contains("PER-RULE DETAIL"), "{verbose}");
        assert!(verbose.contains("SEC-NO-HARDCODED-SECRETS-1"), "{verbose}");
        assert!(verbose.contains("files_evaluated=10"), "{verbose}");
    }
}
