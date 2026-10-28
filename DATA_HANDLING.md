# Data handling

This page says what happens to a client's code when Camerata inspects it: which service the code is sent to, what is removed before it is sent, where copies are kept on the operator's machine and how they are deleted. Every statement was checked against the code, and the comment next to it names the file it was checked against (view this file's source to see them).

## The backend for client work

Client work runs on the Anthropic API, under [Anthropic's Commercial Terms of Service](https://www.anthropic.com/legal/commercial-terms).
<!-- crates/llm/src/llm.rs: Llm::complete_api posts to https://api.anthropic.com/v1/messages; the batch path posts to /v1/messages/batches -->

Every inspection is treated as a client engagement unless the operator says it is their own development: `camerata inspect --internal` on the command line, or turning off **Client engagement** in the project's Settings in the desktop app. A project starts as a client engagement, and so does a project saved before this setting existed.
<!-- crates/cli/src/main.rs: Command::Inspect, `internal` flag -> InspectArgs::client_engagement = !internal -->
<!-- crates/app-core/src/project.rs: Project::client_engagement, serde default default_client_engagement() = true -->
<!-- crates/server/src/project.rs: ProjectStore::create and import_or_overwrite set client_engagement: true -->
<!-- crates/ui/src/cockpit/rules.rs: ClientEngagementEditor -->

A client engagement refuses two things before any file is read or any model is called: the `cli` backend (the operator's own Claude Code subscription) and any model that is not an Anthropic model, which includes every model offered through OpenRouter. The run stops with a message that names what was refused.
<!-- crates/server/src/client_lock.rs: refusal, is_anthropic_api_model, models_a_scan_may_use -->
<!-- crates/cli/src/inspect_cmd.rs: validate_args calls require_client_lock first, before scan_repos reads any file -->
<!-- crates/server/src/lib.rs: onboard_audit and start_audit_job call client_lock_refusal_for_scan before resolve_local_sources; rescan_alternatives_handler likewise; AppState::llm_for_project refuses the cli backend for a client engagement -->
<!-- crates/server/src/dev_implement_run.rs: dev_loop_client_lock_refusal -->
<!-- tests: crates/cli/tests/inspect_client_lock.rs; crates/server/src/lib.rs a_client_engagement_audit_is_refused_before_any_repository_is_read -->

Three of Camerata's development runs can only use the Claude Code subscription (investigating a story, updating a branch and resolving PR feedback), so a client engagement refuses them.
<!-- crates/server/src/lib.rs: cli_only_run_client_lock_refusal, called by spawn_investigation_agent_run, uow_update_branch and uow_pr_resolve; test a_client_engagement_refuses_the_cli_only_runs -->

If the API is selected and no API key is configured, the AI review does not run. It never falls back to the subscription.
<!-- crates/llm/src/llm.rs: resolve_backend returns BackendResolution::Blocked for Api without a key -->
<!-- crates/cli/src/inspect_cmd.rs: require_api_key_for_client_inspection -->

The backend, the provider and every model that ran are recorded in the export (`manifest.json` and `README.txt`), in the report and in the scan's ledger, along with whether the run was a client engagement.
<!-- crates/server/src/lib.rs: ModelRecord (backend, provider, models, client_engagement); ManifestProvenance::inspection_json and readme_lines -->
<!-- crates/server/src/release_gate.rs: provenance.model filled with ModelRecord::with_models_from(report.provenance) -->
<!-- crates/server/src/release_gate/consistency.rs: verify_readme_provenance holds the README model lines equal to the manifest -->
<!-- crates/server/src/report_export.rs: CoverJson::model_backend, audit_model, calibration_model; crates/server/templates/audit_report.typ methodology line -->
<!-- crates/server/src/scan_ledger.rs: ScanLedger::record_engagement -->

The chat assistant that is not tied to a project follows its own backend setting and is not part of an inspection.
<!-- crates/server/src/lib.rs: AppState::chat_llm uses settings.chat_backend -->

## Model calls have no tools

A request to the model carries only a system prompt, the prompt text and output settings. It defines no tools, so the model can only answer with text: it cannot run commands, read files or reach the network on Camerata's behalf.
<!-- crates/llm/src/llm.rs: LlmRequest has no tools field; messages_request_body and build_batch_item build model, max_tokens, system, messages (and output_config) only -->
<!-- crates/llm/src/llm.rs test: api_request_bodies_never_carry_tools -->

On the API, Camerata reads the files itself and places their text in the prompt. The setting that lets a model read a folder applies only to the `cli` backend, which client work does not use.
<!-- crates/llm/src/llm.rs: LlmRequest::repo_read_dir, documented as a no-op on the API backend -->

## What is removed before code is sent

Before any code reaches a model, every value that matches a known credential shape is replaced with a short redacted form (`prefix…tail (N chars)`). Line numbers are kept, so a finding still points at the right line. The shapes cover:

- API keys and tokens (Supabase, Stripe, AWS, Azure storage, GitHub, Slack, Google, Anthropic, OpenAI, SendGrid, Twilio, Shopify, Mailgun, npm, Mapbox and Discord)
- private keys
- credentials in Docker, npm and Postgres configuration files
- passwords in connection strings and URLs
- bearer and basic-auth credentials
- string literals assigned to variables named like secrets

<!-- crates/secrets/src/shapes.rs: SecretShape labels; crates/secrets/src/redact.rs -->
<!-- crates/server/src/onboard/redact.rs: files_for_model (same paths, same line count, every secret value redacted) -->

This applies to every inspection step that sends code: the scan pass, calibration, alternative recommendations, fix suggestions, the optional deep tier and the duplicated-logic judge.
<!-- crates/server/src/ai_audit.rs: audit_repo, run_deep_tier and generate_fix_specifics replace `files` with files_for_model(files) before any model call; verify_findings and recommend_alternatives run inside audit_repo on that set; representative_scan_sample also uses files_for_model -->
<!-- crates/checks/src/clone_rules/judge.rs: judge_input redacts the code and strings it quotes; test crates/server/src/onboard/clone_advisory/tests/redaction.rs the_judge_prompt_carries_no_secret_value -->

The checks that run on the operator's machine read the code as it is, because they have to find the secret to report it. Every finding's text is redacted when the finding is built, and the export's release gate checks every shipped file for secret values before a package is written.
<!-- crates/server/src/onboard/redact.rs: module docs (floor reads the raw set), redact_finding -->
<!-- crates/server/src/release_gate.rs: release returns no bytes unless every check, secrets included, passed -->

Redaction can be turned off only with an environment variable (`CAMERATA_FEATURE_MODEL_PROMPT_REDACTION=false`), never by a file in the client's repository, and turning it off prints a warning. The code itself is still sent: redaction removes credential values, not the rest of the source. A secret in a shape that is not on the list is not redacted.
<!-- crates/server/src/feature_flags.rs: model_prompt_redaction_enabled reads the environment only; redaction_disabled_warning -->

## Client code is not executed

An inspection reads the client's code as data. Steps that would run it are skipped in a client engagement unless the operator opts in (`--run-client-tooling`, or **Run client tooling** in the project's Settings). Today that is the `cargo clippy` preview, which compiles a Rust repository and so would run its build scripts and procedural macros. When it is skipped the report says so, and the ledger records whether client tooling was allowed.
<!-- crates/server/src/scan_tools.rs: ScanTool::executes_repo_code, run_scan_tools skips such a tool unless ClientTooling::allowed(), client_tooling_skipped_note and client_tooling_ran_note -->
<!-- crates/server/src/client_lock.rs: client_tooling_allowed; EngagementRecord::client_tooling_opt_in and client_tooling_allowed -->
<!-- tests: crates/server/src/scan_tools.rs a_client_engagement_skips_clippy_without_running_the_build_script, an_opted_in_client_engagement_runs_clippy_and_says_so -->

The other preview tools do not run the repository's code: `ruff` and `semgrep` read it with Camerata's own rule selection, and `eslint` runs with Camerata's bundled configuration, so it never loads the repository's own `eslint.config` file.
<!-- crates/server/src/scan_tools.rs: ScanTool::executes_repo_code docs; ruff_args, semgrep_args, eslint_args (--config <bundled>) -->

Camerata's development loop (agents writing code, then running the repository's build and tests) refuses a client engagement unless the operator has opted in to running client tooling.
<!-- crates/server/src/dev_implement_run.rs: dev_loop_client_lock_refusal -->

Git commands Camerata runs inside a checkout switch off `core.fsmonitor`, a setting in a repository's own git configuration that names a program git would otherwise run.
<!-- crates/server/src/workspace.rs: GIT_NO_REPO_COMMANDS, used by git_command; crates/server/src/onboard.rs capture_audited_ref and commits_touching_path; crates/server/src/onboard/files.rs git_tracked_files -->
<!-- test: crates/server/src/onboard.rs recording_the_commit_never_runs_the_repositorys_fsmonitor_command -->

## Where clones and caches live

**Clones.** The desktop app clones each repository into its own folder, `<workspace root>/<owner>/<repo>`, where the workspace root is set in Settings. A repository name that is not exactly `owner/repo` gets no folder, so two repositories never share one. The operator can instead link a folder that already holds the repository.
<!-- crates/server/src/workspace.rs: repo_dir, is_owner_repo_spec, resolve_repo_dir (an override path wins), clone_or_pull -->

`camerata inspect` reads the folder given with `--repo` where it is and makes no copy of it.
<!-- crates/cli/src/inspect_cmd.rs: run_inspect_pipeline_with uses args.repo as the only source -->

**The app's stores** live in the user account's application data folder, under `camerata/` (or under `CAMERATA_DATA_DIR` when that is set). Three of them hold client code excerpts: `inspections/` and `onboarding-draft.json` (the findings, with the lines of code they quote) and `scan-cache.json` (what the engine needs to skip unchanged files on the next scan).
<!-- crates/server/src/app_dirs.rs: camerata_dir = data_dir()/camerata, CAMERATA_DATA_DIR override -->
<!-- crates/server/src/client_source/storage.rs: DATA_DIR_ENTRIES and their Treatment -->

**The command line's cache and ledger** live in `~/Library/Caches/camerata` on macOS and in `$XDG_CACHE_HOME/camerata` (or `~/.cache/camerata`) elsewhere, or under `CAMERATA_STATE_DIR` when that is set. They are never written into the scanned repository.
<!-- crates/server/src/scan_cache/state_dir.rs: dir, dir_from -->

**Tool binaries** (the linters Camerata provisions) live in their own folder and hold no client files.
<!-- crates/server/src/client_source/storage.rs: ("tooling", Treatment::NoClientCode) -->

## Deleting a client's code

In the desktop app, each repository card in the **Workspace** tab has **Delete source code from this machine**. It first shows the list of everything it will remove and then removes, in order:

1. the repository's worktrees
2. the checkout
3. this repository's part of the app's scan cache
4. the command-line cache and ledger for that checkout
5. the Claude Code session files recorded for the project's inspections
6. the scan transcripts and finished scan jobs held in memory

It writes a deletion record of what was removed, from where and when.
<!-- crates/server/src/client_source/purge.rs: module table and Action -->
<!-- crates/server/src/client_source/delete.rs: delete_preview, delete -->
<!-- crates/server/src/lib.rs: routes /api/projects/:id/repos/:repo/source/delete-preview and /source/delete -->
<!-- crates/ui/src/source_ui.rs: "Delete source code from this machine" -->

It keeps, on purpose, any export already written and the findings with the lines of code they quote (`inspections/` and `onboarding-draft.json`), because they are the delivered product.
<!-- crates/server/src/client_source/storage.rs: Treatment::Kept entries -->

There is no delete command on the command line. After a `camerata inspect` run, the operator removes the inspected folder, the export and the command-line cache files for that folder themselves.
<!-- crates/cli/src/main.rs: Command has no delete or purge subcommand -->

## Retention

There is no 30-day retention policy in the code. Nothing deletes client code, caches or findings after a period of time; they stay until the operator deletes them as described above.
<!-- checked: no age-based deletion in crates/server/src/client_source/ (delete runs only on request) or anywhere in crates/ -->

## Prompt-injection posture

<!-- Left for hardening item H3. -->
