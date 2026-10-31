# Delete and re-fetch client source: the build-ready design (2026-10-24)

Binding decisions: the private decision log, the 2026-10-22 entries "Delete
client source after delivery" and its re-fetch revision. Forward-compatibility note:
the post-V1 roadmap notes (private working notes), "Keep V1 forward-compatible". Downstream consumer:
`_workspace/brownfield-audits/WORK_ORDER_REVIEW_SPEED.md` (the code viewer, built AFTER this).

Branch: `feat/audit-report-export`. This document is the spec Sonnet builds from and Fable
reviews against. Every file:line below was read on 2026-10-24; line numbers drift, names do not.

Plain-language summary for Zach. After an inspection is delivered, the client's code should
not stay on the laptop. This adds one red button per repo, "Delete source code from this
machine", confirmed by typing the repo's name. Pressing it removes the checkout and every other
copy Camerata made, writes a record of exactly what was removed (the proof you hand the
client), and leaves the project fully working: findings, the review screen, exports and package
regeneration all keep working with no code on disk. When you need the code again (remediation
work, the monthly retest, or to reproduce a finding), "Re-fetch source" clones it back with
full history, and "Rescan changes" inspects only what changed since the inspection.

---

## 0. What the investigation found (summary)

1. **Clones are already full history.** The one clone command is `git clone <authed-url> <dir>`
   with no `--depth` and no `--single-branch` (`crates/server/src/workspace.rs:248`). Nothing in
   the repo passes `--depth` anywhere (`rg -- --depth crates` is empty). Hand-made clones are
   whatever the operator made; re-fetch (section 7) always produces a full clone.

2. **The project never stores a path.** A project knows its repos only as `owner/repo`
   (`crates/app-core/src/project.rs:322`). The local folder is resolved at every call from two
   machine-local settings: the per-repo override `settings.repo_paths` (`settings.rs:48`, read via
   `repo_path` `:253`, written via `set_repo_path` `:262`) and the workspace convention
   `<workspace_root>/<owner>/<repo>` (`workspace::repo_dir` `:62`, `resolve_repo_dir` `:373`). So
   "whatever codebase the project points to" is exactly `resolve_repo_dir(override, root, repo)`,
   whether the folder was cloned by the UI (`checkout_project`, `lib.rs:10773`), linked by hand
   (`link_repo` `lib.rs:7646`, `set_repo_path` `:7525`, the scan surface's folder picker
   `crates/ui/src/cockpit/scan.rs:84-100`), or auto-detected (`detect_repo`).

3. **The remote-match guard already exists as a pure function.** `origin_matches_repo`
   (`workspace.rs:325`) normalises https and ssh forms to `owner/repo`, case-insensitive;
   `validate_link_target` (`:338`) and `repo_resolution` (`:400`) refuse a folder whose origin
   is a different repo. Deletion reuses these, plus new refusals (section 4).

4. **The inspection result is in memory only.** `last_scan` is a `HashMap<project_id,
   ScanReport>` that "does not survive a process restart" (`lib.rs:190-199`). Every export
   handler reads it and returns 404 "No scan results available" when it is empty
   (`export_audit_report` `lib.rs:16278`, `export_product` `:16419`, `export_deep_report`
   `:16152`). The only persisted copy is the UI-owned onboarding draft (`onboarding-draft.json`,
   `draft.rs:16`), an opaque blob. "Package regeneration keeps working without the repo" is
   therefore already broken across a restart today, independent of deletion. This design adds
   the persisted per-inspection entry the roadmap asked for and makes exports read it.

5. **Exports never read source.** `report_export.rs`, `xlsx_export.rs`, `onboard/report.rs`,
   `client_provenance.rs` and `release_gate.rs` contain no `resolve_repo_dir`, `repo_path`, or
   file read of repo content (the only production writes are the Typst compile's own temp dir,
   `report_export.rs:4972-4981`). Section 10 adds a source-level invariant test so it stays that way.

6. **The readiness gate is the biggest "repo must exist" block.** `project_readiness`
   (`lib.rs:7566`) classifies a project `Unlinked`/`Partial` whenever any repo does not resolve to a
   matching git clone, and the cockpit then replaces the ENTIRE Onboard surface (scan, findings
   review, triage, exports) with "This action is paused until the project's repo is linked"
   (`crates/ui/src/cockpit.rs:2687-2760`), auto-opening the resolve modal
   (`readiness_gate.rs`). After deletion that would hide the delivered inspection. Section 6 fixes it.

7. **Where client code lands outside the checkout** (full list in section 5): the server
   incremental-scan cache (`scan-cache.json`), the CLI cache and ledger under
   `~/Library/Caches/camerata/`, UoW worktrees and the shared cargo target inside the checkout,
   and, when a project runs on the `Cli` backend, Claude Code's own session files under
   `~/.claude/projects/` (every `claude -p` call persists its prompt, which contains client
   source). Transcripts (`transcript.rs:37`) and audit jobs (`jobs.rs:10`) are in memory only.
   The eslint, semgrep and osv-scanner runs read in place with `current_dir = repo`
   (`scan_tools.rs:1194,1211`) and write only to stdout; ruff writes `.ruff_cache/` INTO the
   client repo (defect, fixed in section 11).

8. **The sha and the origin are enough.** `AuditedRef { repo, sha, branch, dirty }`
   (`onboard.rs:586`) is stamped per repo at scan time (`capture_audited_ref` `:2460`) into
   `ScanProvenance.audited_refs` (`:607`) and already reaches the UI
   (`ScanProvenanceView.audited_refs`, `cockpit/scan.rs:776`). The origin URL is not stored, but
   it is derivable: the recorded identity IS `owner/repo`, and the only remote Camerata ever
   writes is `https://github.com/<owner>/<repo>.git` (`clean_url`, `workspace.rs:71`). The guard
   compares parsed identities, not URL strings, so a hand-made ssh clone matches too. The
   observed origin URL is added to the records (section 2) for the paper trail and for the
   renamed-origin report, not for the guard.

9. **The delta rescan needs nothing new in the engine.** `audit_repos` already partitions each
   repo's files into changed and unchanged against a prior `ScanManifest` and AI-audits only the
   changed set (`onboard.rs:1942`, `scan_cache::partition` `scan_cache.rs:164`); the deterministic
   floor always runs over everything. "Rescan changes" feeds it a prior built from `git diff
   --name-status <inspected>..HEAD` and the inspected entry's findings (section 8).

---

## 1. What already EXISTS (reuse, do not rebuild)

| Piece | Where | Reused for |
|---|---|---|
| Path resolution | `workspace::resolve_repo_dir` (`workspace.rs:373`), `settings.repo_path` (`settings.rs:253`), `settings.workspace_root()` | the one way to find the folder to delete, read, or re-fetch into |
| Origin guard | `detect_remote_repo` (`:300`), `origin_matches_repo` (`:325`), `validate_link_target` (`:338`), `parse_owner_repo` (`:360`) | the remote-match refusal |
| Resolution status | `repo_resolution` -> `RepoResolution { repo, path, resolved, reason }` (`:386-437`) | extended with `source_deleted` (section 6) |
| Checkout status | `checkout_status_resolved` -> `RepoCheckout` (`:137`) | extended with the source status (section 2.4) |
| Clone with token scrub | `clone_or_pull` (`:219`): authed URL for the network step, `remote set-url origin <clean>` after | re-fetch reuses `authed_url`, `clean_url`, `git()` and the scrub, with a new clone function (section 7) |
| Disk headroom | `ensure_disk_headroom` (`:1005`), `available_disk_bytes` (`:980`) | re-fetch precheck |
| Worktree admin | `worktree_for_branch` (`:657`, parses `git worktree list --porcelain`), `remove_uow_worktree` (`:795`), `prune_worktrees` (`:882`), `shared_target_dir` (`:921`) | enumerating and removing worktrees before the checkout goes |
| Git log/commit parse | `git_log`, `parse_git_log`, `Commit` (`:1239-1290`) | the "N commits newer" line and the dialog's dates |
| Audited ref capture | `capture_audited_ref` (`onboard.rs:2460`) | extended to also capture the origin URL (section 2.2) |
| Incremental partition | `scan_cache::partition`, `content_fingerprint`, `ScanManifest` (`scan_cache.rs:46-164`) | the delta rescan's prior |
| Audit pipeline + job | `audit_repos` (`onboard.rs:1240`), `onboard_audit_start` (`lib.rs:6620`), `JobStore` (`jobs.rs`) | rescan-changes runs through the same job path the UI already polls |
| Finding context | `finding_context::lookup` + `FindingContextOutcome` (`onboard/finding_context.rs:32,114`), `resolve_contained_path` (`:204`), `redact_str` (`:153,174`) | becomes the git-object reader the viewer will also use (section 9) |
| Redaction | `camerata_secrets::redact_str` | every line any new endpoint returns |
| GitHub token | `AppState::github_token` (`lib.rs:473`): keychain, then `CAMERATA_GITHUB_TOKEN` | re-fetch auth |
| Danger button styles | `.btn-danger-outline`, `.btn-solid-danger`, `.pg-btn-danger.confirm` (`crates/ui/src/style.rs:2427-2433, 4969-4987`) | the danger zone |
| Clipboard bridge | `use_clipboard_bridge` (`crates/ui/src/desktop_chrome.rs:249`) | "Copy record" |
| Readiness logic | `camerata_app_core::readiness` (`readiness.rs:20,57`), `camerata_ui_core::readiness` (`readiness.rs:69-103`), `crates/ui/src/readiness_gate.rs` | extended, not replaced |
| Source-grep invariant test pattern | `lib.rs:26224` (`.find("async fn export_product(")`) | the "exports never touch the repo" guard test |
| Git fixture helpers | `init_repo_with_initial_commit` (`workspace.rs:1529`), test-local `init_repo_with_origin` (`:2961`), `init_repo_with_commit` (`:2208`) | every test below that needs a real git repo with a file-based remote |

---

## 2. Data model

Everything new is additive: `#[serde(default)]` on every new field, no migration, old files load.

### 2.1 The inspection entry (new store, `inspections`)

New module `crates/server/src/inspections.rs`, store `InspectionStore`, same pattern as
`ProjectStore` (`Arc<Mutex<_>>` mirror, optional path, write-through). ONE FILE PER PROJECT so a
large report never rewrites the others: `<dirs::data_dir()>/camerata/inspections/<project_id>.json`.
Opened in `AppState::from_env` beside the other stores (`lib.rs:773-924`); in-memory in tests.

```rust
/// Everything one inspection run produced, persisted the moment it completes. V1 reads only
/// the latest entry; V2's inspection-history UI browses the list. (Roadmap: "store
/// per-inspection facts as an entry", not a single last-scan slot.)
#[derive(Serialize, Deserialize, Clone)]
pub struct InspectionEntry {
    pub id: String,                    // "insp-<project_id>-<started_at compact>-<4 hex>"
    pub project_id: String,
    pub kind: InspectionKind,          // Full | DeltaRetest { base_inspection_id, from_sha, to_sha }
    pub started_at: String,            // copied from report.provenance
    pub finished_at: String,
    pub repos: Vec<AuditedRef>,        // report.provenance.audited_refs (now with origin_url, default_branch)
    pub rules_fingerprint: String,
    pub camerata_version: String,
    /// The sealed, redacted report exactly as the exporters consume it. Snippets inside
    /// findings are part of the delivered product (Zach, 2026-10-22) and stay.
    pub report: ScanReport,
    /// Claude Code sessions this inspection's model calls created (section 5, item P5).
    /// Empty on the Api backend.
    #[serde(default)]
    pub model_sessions: Vec<ModelSession>,  // { session_id: String, cwd: String, created_at: String }
    /// Set when a client package was generated from this entry (V1 writes it from
    /// `export_product`; V2 attaches the zip bytes).
    #[serde(default)]
    pub package: Option<PackageRecord>,     // { generated_at, zip_sha256, manifest_json }
}
```

Writers: every place that calls `state.set_last_scan` with a COMPLETED audit (`lib.rs:5124`
floor-only scan, `:5714` sync audit, `:6855` job completion). `set_last_scan` keeps its signature
and additionally appends an `InspectionEntry` when the report has a non-empty
`provenance.audited_refs` (a real scan, not a merge of a rescan into an existing report; the
alternatives rescan at `:4596` updates the LATEST entry's report in place instead).

Reader: `AppState::get_last_scan(project_id)` (`lib.rs:729`) falls back to
`inspections.latest(project_id).map(|e| e.report)` when the in-memory map has nothing. That one
change makes `export_product`, `export_audit_report`, `export_deep_report`, chat grounding and
`rescan_alternatives_handler` work after a restart with no further edits.

Store API: `latest(project_id) -> Option<InspectionEntry>`, `list(project_id) -> Vec<InspectionEntry>`
(newest first), `append(entry)`, `update_latest_report(project_id, report)`,
`record_model_session(project_id, session)`, `set_package(project_id, PackageRecord)`.

### 2.2 `AuditedRef` gains the origin and default branch

`onboard.rs:586`, additive:

```rust
pub struct AuditedRef {
    pub repo: String,
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
    /// `git config --get remote.origin.url` as observed at scan time (tokenless by construction:
    /// the clone path scrubs it, and a hand-made clone's URL is the operator's own). `None` on a
    /// non-git folder or no origin. Informational and for the renamed-origin report; the
    /// remote-match guard compares `owner/repo` identities, never this string.
    #[serde(default)] pub origin_url: Option<String>,
    /// `git symbolic-ref refs/remotes/origin/HEAD` -> "main"/"master"; `None` when absent.
    #[serde(default)] pub default_branch: Option<String>,
}
```

`capture_audited_ref` (`:2460`) runs the two extra commands with the same fail-soft pattern.
The UI mirror `AuditedRefView` (`cockpit/scan.rs:770`) gets the same two optional fields.

### 2.3 The deletion record (new store, `source_records`)

Same module as the delete logic (section 3): `crates/server/src/client_source.rs`. Persisted at
`<data_dir>/camerata/source-records.json` as `{ project_id: Vec<DeletionRecord> }`. Not part of
the project export (`export_project`, `lib.rs:3608`, is config-only by decision
`2026-06-21_project_config_vs_data_separation.md`); the record is handed to the client via the
"Copy record" / "Save record" actions (section 6.3), which render `DeletionRecord` as the
plain-text block in 2.3.1.

```rust
pub struct DeletionRecord {
    pub id: String,                        // "del-<project_id>-<repo sanitised>-<completed_at compact>"
    pub project_id: String,
    pub repo: String,                      // owner/repo
    pub origin_url: Option<String>,        // observed at deletion, before removal
    pub path: String,                      // the folder that was removed (canonical)
    pub inspected_sha: Option<String>,     // latest inspection entry's sha for this repo
    pub inspection_id: Option<String>,
    pub head_sha_at_deletion: Option<String>,
    pub requested_at: String,              // RFC3339 UTC
    pub completed_at: String,
    pub confirmed_with: String,            // the text the operator typed (must equal `repo`)
    pub include_uncommitted: bool,         // the extra tick (section 4, R10/R11)
    pub items: Vec<PurgedItem>,            // one per location in section 5, in that order
    pub total_files: u64,
    pub total_bytes: u64,
    pub method: String,                    // the fixed sentence in 2.3.2
    pub residuals: Vec<String>,            // fixed sentences for what deletion does NOT cover
    pub camerata_version: String,
    pub machine: String,                   // hostname (gethostname), for the client's record
}

pub struct PurgedItem {
    pub kind: PurgedKind,     // Checkout | Worktree | ServerScanCache | CliScanCache | CliLedger |
                              // ModelSessionFile | InMemory
    pub path: String,         // exact path, or "scan-cache.json#<project_id>/<repo>" for the entry
    pub files: u64,           // counted BEFORE removal (walk); 0 for an in-file entry
    pub bytes: u64,
    pub outcome: PurgeOutcome,// Removed | AlreadyAbsent | Failed { error: String }
}
```

Counting is a plain recursive walk over the checkout (including `.git/`), so "file count" is
"every file under the folder" and the record says so.

#### 2.3.1 The record as the client sees it (rendered text)

```
Camerata source deletion record
Repository        acme/app  (origin https://github.com/acme/app.git)
Inspected commit  3f2a9c14e0b7... (inspection insp-..., finished 2026-10-24 15:02 UTC)
Removed from      /Users/auditor/Camerata/acme/app   on   MacBook-Pro.local
Requested         2026-10-24 17:40:12 UTC     Completed  2026-10-24 17:40:19 UTC
Removed
  checkout            /Users/auditor/Camerata/acme/app                        2,314 files   148.2 MB
  worktree            /Users/auditor/Camerata/acme/app/.camerata-worktrees/..    412 files    31.0 MB
  scan cache entry    scan-cache.json#proj-3/acme/app                           -          0.9 MB
  CLI scan cache      ~/Library/Caches/camerata/app-3f2a9c14.cache.json         1           0.4 MB
  CLI ledger          ~/Library/Caches/camerata/app-3f2a9c14.ledger.json        1          12.1 KB
  model session file  ~/.claude/projects/-Users-auditor-Camerata/9d1c....jsonl  1           2.3 MB
Total                                                                      2,729 files   182.9 MB
Kept                code excerpts inside the inspection findings (part of the delivered report)
Method              Ordinary file removal on a FileVault-encrypted APFS volume. Not a forensic
                    wipe; no overwrite pass. Backups made by the operator's own tools (Time
                    Machine or similar) are outside this record.
Camerata            0.x.y
```

#### 2.3.2 The fixed sentences

`method`: "Ordinary file removal (std::fs::remove_dir_all) on a FileVault-encrypted APFS volume.
Not a forensic wipe; no overwrite pass."

`residuals` (each added only when it applies): "Backups made by the operator's own tools (Time
Machine or similar) are outside this record." (always); "Code excerpts inside the inspection
findings are kept as part of the delivered report." (always); "This repository was inspected on
the Cli backend; Claude Code session files recorded by Camerata were removed, any session not
recorded by Camerata was not." (when `model_sessions` non-empty); "Uncommitted changes and
unpushed commits were removed at the operator's request." (when `include_uncommitted`).

### 2.4 Per-repo source status (derived, never stored)

Computed on request by `client_source::source_status(state, project, repo) -> RepoSourceStatus`
from disk plus the two stores. Stored facts are only the records above; the status is a view.

```rust
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RepoSourceStatus {
    /// A matching clone is on disk.
    Present {
        repo: String, path: String,
        head_sha: Option<String>, head_branch: Option<String>, dirty: bool,
        inspected_sha: Option<String>, inspected_at: Option<String>,
        /// `git rev-list --count <inspected>..HEAD`; None when no inspection or unreachable.
        newer_commits: Option<u64>,
        /// `git cat-file -e <inspected>^{commit}` succeeded.
        inspected_commit_reachable: bool,
        has_deletion_history: bool,     // records exist (it was deleted and re-fetched)
    },
    /// The resolved path does not exist (or holds no .git) AND a deletion record exists and
    /// no newer Present observation exists for it.
    Deleted { repo: String, path: String, deleted_at: String, record_id: String,
              inspected_sha: Option<String>, total_files: u64, total_bytes: u64 },
    /// Nothing on disk and no record: the pre-existing "not cloned / not linked" state.
    Unresolved { repo: String, path: Option<String>, reason: String },
    /// A folder is there but fails the guard (wrong origin, no .git). Reported, never touched.
    Mismatch { repo: String, path: String, reason: String },
}
```

Rule for `Deleted` versus `Unresolved`: a repo is `Deleted` when the newest `DeletionRecord` for
it is newer than any re-fetch (the re-fetch handler appends a `RefetchRecord { repo, path, sha,
target, completed_at }` to the same store; the newest of the two wins). This is what lets a
freshly IMPORTED project (identity travelled, no clone, no record) still land in the existing
paused "Link repo" flow, while a deleted one does not.

---

## 3. Server endpoints

All new handlers live in `crates/server/src/client_source.rs` and are registered from `lib.rs`
next to the checkout routes (`lib.rs:1172`). Responses follow the onboard convention: 200 with a
`status`/`ok` field for expected outcomes, 4xx only for malformed input and refusals. Every
error body is `{ "ok": false, "code": "<snake_case>", "message": "<plain sentence>" }`.

### 3.1 `GET /api/projects/:id/source`

Per-repo source status for the project. Response `{ "ok": true, "repos": [RepoSourceStatus] }`
in `project.repos` order. 404 `unknown_project`. Used by the Workspace cards, the scan surface's
rescan gating, and the finding modal's banner.

### 3.2 `POST /api/projects/:id/repos/:repo/source/delete-preview`

`:repo` percent-encoded as in `link_repo` (`lib.rs:7646`). No body. Runs every guard in section 4
and builds the purge plan WITHOUT removing anything.

Response 200 `{ "ok": true, "plan": { "path", "origin_url", "head_sha", "dirty", "unpushed":
[{branch, ahead}], "items": [PurgedItem with outcome "planned"], "total_files", "total_bytes",
"warnings": [String] } }`. Refusal 409 `{ ok:false, code, message }` with the codes in section 4.
The UI shows the plan in the confirmation modal; the record written by 3.3 lists the same items.

### 3.3 `POST /api/projects/:id/repos/:repo/source/delete`

Body `{ "confirm": "<owner/repo exactly as displayed>", "include_uncommitted": false }`.

Order of operations (all inside one handler, no partial success hidden):
1. Guards (section 4). Any refusal: 409, nothing touched, nothing recorded.
2. Capture: origin URL, HEAD sha, dirty flag, unpushed branches, the registered worktree list
   (`git worktree list --porcelain` in the checkout), the file walk counts.
3. Purge in the order of section 5. Each item records `Removed | AlreadyAbsent | Failed`.
4. Clear in-memory copies for the project (section 5, P11).
5. Write the `DeletionRecord` (even when some item `Failed`; the record is the truth).
6. Response 200 `{ "ok": true, "record": DeletionRecord, "status": RepoSourceStatus::Deleted }`
   when every item is `Removed`/`AlreadyAbsent`; 200 `{ "ok": false, "code": "partial",
   "record": ..., "message": "<n> locations could not be removed; see the record" }` otherwise.

Idempotent: a second call on a `Deleted` repo returns 200 `{ ok:true, code:"already_deleted",
record: <existing> }` without a new record.

### 3.4 `GET /api/projects/:id/source/records`

`{ "ok": true, "records": [DeletionRecord], "refetches": [RefetchRecord] }`, newest first. Also
`GET /api/projects/:id/source/records/:record_id/text` returns the 2.3.1 rendering as
`text/plain` for "Save record".

### 3.5 `GET /api/projects/:id/repos/:repo/source/remote`

Probes the remote for the re-fetch dialog. Runs `git ls-remote --symref <authed_url> HEAD`
(no GitHub API needed): the `ref: refs/heads/<name> HEAD` line gives the default branch, the
following line its sha. Then, when the inspected sha is known, `git ls-remote <authed_url>`
is NOT enough to prove a commit exists (ls-remote lists refs, not commits), so reachability of
the inspected commit is reported only after re-fetch; the dialog says so.

Response `{ "ok": true, "default_branch": "main", "head_sha": "...", "head_date": null }`
(the date is filled in after clone; the dialog shows the short sha only before). Errors, each a
plain sentence: `no_token` ("No GitHub token is set. Add one under Credentials."),
`auth_failed` ("GitHub refused Camerata's token for acme/app. The token may be revoked or lack
access to this repository."), `repo_not_found` ("GitHub has no repository at acme/app. It was
deleted, renamed, or made private to this token."), `network` (the git stderr, trimmed).
Renamed repos: GitHub redirects clones silently, so a rename is detected after re-fetch by
comparing the clone's `origin` identity (`detect_remote_repo`) against `project.repos`; a
mismatch is reported in the re-fetch response as `renamed_to` and the card shows "GitHub now
calls this repository <new>; the project still records <old>."

### 3.6 `POST /api/projects/:id/repos/:repo/source/refetch`

Body `{ "target": { "kind": "latest" } | { "kind": "commit", "sha": "<40 hex>" } }`.
Synchronous, like `POST /api/projects/:id/checkout` (`lib.rs:10773`); the UI shows a busy state.
Behaviour in section 7. Response 200 `{ "ok": true, "status": RepoSourceStatus::Present,
"renamed_to": null }`. Errors: `no_token`, `auth_failed`, `repo_not_found`, `network`,
`destination_not_empty` ("<path> already contains files; move them aside or link that folder
instead."), `no_disk_headroom` (reuses `ensure_disk_headroom`'s message), `commit_not_on_remote`
("The inspected commit <sha> is not reachable from any branch or tag on GitHub (history was
rewritten). The latest source was fetched instead; finding code views for this inspection are
unavailable."), `invalid_sha` (400).

### 3.7 `POST /api/projects/:id/repos/:repo/rescan-changes`

Body `{}` (V1 has one mode). Preconditions: status `Present`, an inspection entry with a sha for
this repo exists, `inspected_commit_reachable`. Runs section 8 through the existing audit job
machinery and returns exactly what `onboard_audit_start` returns (`{ "job_id": ... }`), so the
UI reuses its polling of `GET /api/onboard/audit/job/:id`. Errors: `source_deleted` ("Source
deleted on <date>. Re-fetch source to rescan."), `no_inspection` ("No inspection has run for
acme/app yet; run a scan instead."), `inspected_commit_unreachable` (offers a full scan),
`scan_in_progress`.

### 3.8 Changes to existing endpoints

- `GET /api/onboard/finding-context` (`lib.rs:6944`): new optional query `sha`; new statuses
  `source_deleted` and `commit_unavailable`; reads via git objects. Contract in section 9.
- `GET /api/projects/:id/readiness`, `/repo-health`, and `/checkout` (`lib.rs:7566, 7494,
  10725`): `RepoResolution` and `RepoCheckout` gain `#[serde(default)] source_deleted: bool` and
  `deleted_at: Option<String>`. Section 6.
- `GET /api/projects/:id/suppressions` (`lib.rs:7921`): returns `{ ok:true, source_deleted:
  true, deleted_at, entries: [] }` for a deleted repo instead of reading a missing tree; and it
  stops calling `pull_local_ff` on a GET (section 11, D3).
- `POST /api/onboard/scan`, `/audit`, `/audit/start`, `/apply`, `/arm`, `/open-pr`,
  `/greenfield`, `POST /api/projects/:id/rescan-alternatives`: when a repo in scope is `Deleted`,
  they return 409 `source_deleted` with the date BEFORE resolving sources (today they would
  silently note "not a local git clone" and scan nothing).
- `POST /api/projects/:id/checkout` ("Clone / update all repos", `lib.rs:10773`): skips repos
  whose status is `Deleted` and reports them as `detail: "source deleted on <date>; use
  Re-fetch source"` rather than re-cloning them as a side effect of updating the others.

---

## 4. The remote-match guard and its refusal reasons

`client_source::guard(state, project, repo, req) -> Result<GuardedTarget, Refusal>` runs before
any removal, in this order; the first failing check is the one reported. `Refusal { code, message }`.

| # | Code | Check | Message (plain sentence) |
|---|---|---|---|
| R1 | `unknown_project` / `repo_not_in_project` | project exists; `repo` is in `project.repos` (case-insensitive) | "acme/app is not one of this project's repositories." |
| R2 | `no_path` | `resolve_repo_dir(override, root, repo)` is `Some` | "No local folder is recorded for acme/app, so there is nothing to delete." |
| R3 | `not_a_directory` | the path exists and is a directory | "<path> does not exist (or is not a folder). Nothing to delete." (if a deletion record exists this is reported as `already_deleted`, not a refusal) |
| R4 | `symlinked_path` | no component of the path, including the last, is a symlink (`symlink_metadata` on each prefix) | "<path> is a symbolic link. Camerata deletes only a real folder; link the real folder instead." |
| R5 | `protected_path` | the canonical path is not `/`, not `$HOME`, not `settings.workspace_root`, not an ancestor of `dirs::data_dir()/camerata`, not an ancestor of the running executable's directory, and has at least 3 components | "<path> is a protected location, not a repository checkout." |
| R6 | `not_a_git_repo` | `<path>/.git` exists | "<path> is not a git repository (no .git). Camerata deletes only a git checkout whose origin matches the project." |
| R6b | `linked_worktree` | `<path>/.git` is a directory, not a file (a file means a linked worktree of another clone) | "<path> is a worktree of the clone at <clone>; delete that clone instead." |
| R7 | `no_origin` / `origin_not_github` | `detect_remote_repo(path)` is `Ok` | its message ("that folder has no `origin` remote ..." / "the origin remote isn't a GitHub URL: ...") |
| R8 | `origin_mismatch` | `found.eq_ignore_ascii_case(repo)` (same comparison as `repo_resolution`, `workspace.rs:413`) | "<path> is a clone of <found>, not acme/app. Nothing was deleted." |
| R9 | `confirmation_mismatch` | `req.confirm == repo` byte-for-byte (the dialog shows the exact string) | "Type the repository name exactly as shown: acme/app." |
| R10 | `dirty_tree` | `git status --porcelain` empty, OR `include_uncommitted` | "acme/app has uncommitted changes (<n> files). Tick 'Delete uncommitted work too' to remove them." |
| R11 | `unpushed_commits` | every local branch has `git rev-list --count origin/<b>..<b>` == 0 (branches with no upstream count as unpushed), OR `include_uncommitted` | "acme/app has commits that are not on GitHub (<branch>: <n>). Tick 'Delete uncommitted work too' to remove them." |
| R12 | `scan_in_progress` | no running audit job for the project (`JobStore` status), no re-fetch in flight for the repo (a per-repo `Mutex` in `client_source`) | "A scan is running for this project. Wait for it to finish or cancel it first." |
| R13 | `worktree_outside_refused` | every registered worktree path (from `git worktree list --porcelain`, after `git worktree prune`, minus `prunable` entries and the target itself) passes R4 and R5 itself, and each existing one has a `.git` file pointing into this clone's `worktrees/` area | "A worktree of acme/app at <path> is in a protected location; remove it by hand first." |

R10 and R11 exist because Camerata's own governed-dev flows commit on branches in these
checkouts (`commit_all`, `workspace.rs:1498`); deleting unpushed client remediation silently
would be a worse failure than a refusal. The tick is a second explicit act in the same modal.

Safe twins the tests must cover (section 10): a sibling folder that is a clone of `acme/app-2`
(prefix lookalike), a clone whose origin is `github.com/Acme/App.git` (case: MUST match), an ssh
origin `git@github.com:acme/app.git` (MUST match), a `github.com/acme/app/` with trailing slash
(MUST match), a non-GitHub mirror `gitlab.com/acme/app` (MUST refuse), the workspace root itself
when `repo_paths` points the override at it (MUST refuse, R5 then R7).

---

## 5. The full purge list, with exact paths

Order matters: worktrees are enumerated and removed BEFORE the checkout (their admin records live
inside `<checkout>/.git/worktrees/`), and the record is written last.

| # | Location | Exact path | How found | Action |
|---|---|---|---|---|
| P1 | Registered git worktrees | each `worktree <path>` line of `git worktree list --porcelain` run in the checkout, excluding the first (the checkout itself). Camerata's own are `<checkout>/.camerata-worktrees/<branch>` (`workspace.rs:689,697`); hand-made ones may be anywhere | git | `git worktree remove --force <path>`; if the dir still exists, `remove_dir_all`; then `git worktree prune` |
| P2 | The checkout | `resolve_repo_dir(settings.repo_path(repo), settings.workspace_root(), repo)`, canonicalised. Contains `.git/`, the working tree, `.camerata-shared-target/` (`workspace.rs:921`), `.camerata/baseline.json` and any `.camerata/*.toml` Camerata emitted (`arm.rs`), legacy `.camerata-scan-cache.json` / `.camerata-scan-ledger.json` (`inspect_cmd.rs:589-593`), and ruff's `.ruff_cache/` (section 11) | resolver | walk for counts, then `std::fs::remove_dir_all`; then remove the now-empty `<owner>` parent only when it is empty AND under `workspace_root` |
| P3 | Server incremental-scan cache | `<dirs::data_dir()>/camerata/scan-cache.json` (`lib.rs:924`), the project's `ScanManifest` (`scan_cache.rs:53`): `files[repo]` and every `findings[i]` with `repo == <repo>` | `ScanCacheStore` | new `ScanCacheStore::remove_repo(project_id, repo)` (lock held through save, like `put`, `scan_cache.rs:335`); the whole project entry is dropped when `files` becomes empty |
| P4 | CLI scan cache | `~/Library/Caches/camerata/<name>-<fnv32>.cache.json` where the key is `repo_state_key(canonical checkout path)` (`inspect_cmd.rs:286-310`, `manifest_cache_path` `:245`); Linux: `$XDG_CACHE_HOME/camerata/` (`camerata_state_dir` `:273`) | the key function, moved to `camerata_server::scan_cache::state_dir` so the server and the CLI share ONE implementation (the CLI re-exports it) | `remove_file` |
| P5 | CLI integrity ledger | `~/Library/Caches/camerata/<same key>.ledger.json` (`ledger_path` `:408`). Counts only, no code (`scan_ledger.rs`), purged because the owner listed the CLI cache dir and it is keyed to the client path | same | `remove_file` |
| P6 | Claude Code session files (Cli backend only) | `~/.claude/projects/<cwd with every "/" and "." replaced by "-">/<session_id>.jsonl` (verified on this machine: 23 such directories exist, e.g. `-Users-auditor-Documents-Repos-camerata-orchestrator`). Camerata spawns `claude -p <prompt>` (`llm.rs:1310-1331`) with the prompt, which contains client source, as an argument; Claude Code persists every print-mode session. cwd is the Camerata process cwd for single-shot calls and the repo dir for repo-read calls (`harden_completion`, `llm.rs:152-178`) | `InspectionEntry.model_sessions` (section 5.1) | glob `~/.claude/projects/*/<session_id>.jsonl` and `~/.claude/projects/*/<session_id>/` (sidecar dir, if present) for each recorded id; `remove_file` / `remove_dir_all` |
| P7 | In-memory copies | `TranscriptStore` (`transcript.rs:37`) entries for the project's scan job ids; `JobStore` (`jobs.rs:121`) finished jobs for the project; NOT `last_scan` (findings are kept) | `AppState` | `transcripts.clear(job_id)`, `jobs.remove_finished(project_id)`; listed in the record as `InMemory` with `files: 0` |

Locations checked and found NOT to hold client code (stated in the design so the completeness
test enumerates them too): `onboarding-draft.json` (findings and excerpts, kept by decision);
`projects.json`, `settings.json` (identity and paths only); `uow.json`, `stories.json`,
`clarifications.json`, `clarify-resume.json`, `routines.json`, `escalations.json`,
`checkpoints.json` (governed-dev state; may hold agent chat text but no source files; outside
the inspection; listed under `residuals` only when the project has UoWs); `artifacts.db`
(`agent_sessions`, `provenance_log`: ids, descriptions, rule ids; `persistence/store.rs:31-50`),
`enforcement_catches.db`, `governance_events.db`, `orchestrator_decisions.db`, `feedback.db`
(`feedback` rows are scrubbed, `persistence/feedback.rs:40`); the tooling dir
`<data_dir>/camerata/tooling/` (semgrep venv, eslint workspace, `tool_provisioning.rs:94,245,393`:
tool binaries and Camerata's bundled configs, never client files); the Typst compile temp dir
(`tempfile::tempdir()`, removed on drop, `report_export.rs:4972`); `sample-report/` in this
repository (Camerata's own sample audit exports, not client code); eslint (no `--cache`,
`scan_tools.rs:361-372`), semgrep (`--sarif --config --quiet .`, `:1519`) and osv-scanner
(`--format json -r`, `dep_audit.rs:417`) write to stdout only.

Verified by probing, to be re-verified by the completeness test: semgrep writes `~/.semgrep/`
(settings, `last.log`). The log carries paths and rule ids; whether it can carry matched source
lines is not established. Build step 5 runs the probe (a fixture with a unique sentinel string,
then `grep -r <sentinel> ~/.semgrep`); if it hits, `~/.semgrep/last.log` joins the purge list as
P8 and semgrep gets `--metrics=off` regardless (section 11).

### 5.1 Recording model sessions

`Llm::complete_cli_streaming` (`llm.rs:1304`) adds `--session-id <uuid v4>` to every spawn and
reports the id on the `LlmResponse` (`session_id: Option<String>`). `audit_repos` collects them
from every model call it makes (audit, calibration, clone judge, alternatives) into
`ScanReport.provenance.model_sessions` (additive field, `ModelSession { session_id, cwd }`), and
`set_last_scan` copies them into the `InspectionEntry`. The Api backend records nothing. The
`cwd` recorded is `std::env::current_dir()` for single-shot calls or `repo_read_dir` otherwise,
so the record can also name the directory.

Build step 5 verifies the two assumptions with a probe on the installed CLI: (a) `claude -p
--session-id <uuid>` is accepted and the session persists at the glob above; (b) the file
contains the prompt text. If (a) fails on the installed version, the fallback is a time-window
match (files under the cwd's project dir modified between `started_at` and `finished_at`), which
is less precise and is recorded as such.

---

## 6. Every "repo must exist" path, and its fix

Classification: **S** = scan-only or needs the source by nature (disable with an explanation and
a pointer to Re-fetch); **F** = must keep working without the repo (needs a change).

### 6.1 The gate and the surfaces it hides (F)

| Where | Today | Fix |
|---|---|---|
| `workspace::repo_resolution` (`workspace.rs:400`) | not-a-dir -> `resolved: false, reason: "no `origin` ..."` | add `source_deleted: bool, deleted_at: Option<String>`; filled by the caller from the source status (keeps `workspace.rs` free of store knowledge): new `client_source::resolve_with_records(state, project, repo) -> RepoResolution` used by the three handlers below |
| `project_readiness` (`lib.rs:7566`), `derive_project_readiness` (`:7603`), `project_repo_health` (`:7494`) | gate on `resolved` only | pass `RepoLink::{Resolved, Deleted, Unresolved}` per repo |
| `camerata_app_core::readiness::classify_readiness(&[bool])` (`readiness.rs:57`) | all true -> Ready | new signature `classify_readiness(&[RepoLink])`: `Deleted` counts like `Resolved` for readiness (there is nothing to LINK; the source is intentionally absent). Pure, tested. The `&[bool]` form stays as a thin wrapper for existing tests |
| `camerata_ui_core::readiness` (`readiness.rs:69-103`) | `action_gated(readiness)`, `paused_banner_text` | add `scan_block(repos: &[RepoResolutionView]) -> Option<ScanBlock { repos: Vec<(repo, deleted_at)> }>` and `scan_block_text(&ScanBlock) -> String` ("Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to rescan.") |
| `crates/ui/src/cockpit.rs:2687-2760` | `gated` hides Onboard, Rules, Design, GovernedDev; `ResolveModal` auto-opens | `gated` stays driven by readiness (now Ready with deleted repos). NEW `scan_blocked = scan_block(...)` passed into `OnboardView`, which renders fully and disables ONLY: the scan start button, "Rescan" controls, the alternatives sticky rescan bar (`RescanStickyBar`, `scan.rs:5218`), and "Apply"/"Arm"/"Open PR", each with the `scan_block_text` as the hint. `ResolveModal` never auto-opens for `Deleted` repos (`unresolved_repos` filters them out) |
| `crates/ui/src/readiness_gate.rs` | lists unresolved repos | deleted repos are not listed; the modal copy is unchanged |
| `crates/ui/src/cockpit/scan.rs:84-100` ("Choose this repo's local folder" nag per unresolved repo) | shown for any unresolved repo | hidden for `Deleted` repos; replaced by the one-line hint with a "Re-fetch source" link to the Workspace tab |

### 6.2 Reading code for a finding (F)

| Where | Today | Fix |
|---|---|---|
| `onboard_finding_context` (`lib.rs:6944`) + `finding_context::lookup` (`finding_context.rs:114`) | reads the WORKING TREE file at `resolve_repo_dir`; unresolved repo -> `file_missing` | section 9: read `git show <sha>:<path>` at the inspected sha; `source_deleted` when the repo is `Deleted`; never touch the working tree when the dir is missing |
| the finding modal (`crates/ui/src/cockpit/scan.rs`, `FindingDetailPane`) | renders `status` | renders the two new statuses and the "References commit <sha>; source on disk is newer" note when `head_sha != inspected_sha` |

### 6.3 Exports and regeneration (F, restart gap)

| Where | Today | Fix |
|---|---|---|
| `export_audit_report` (`lib.rs:16262`), `export_product` (`:16403`), `export_deep_report` (`:16152`) | `get_last_scan` only; 404 after restart | `get_last_scan` falls back to the inspection store (section 2.1). No other change. `export_product` additionally records `PackageRecord` on the entry |
| chat grounding `active_project_context` (`lib.rs:14358-14364`) | draft -> last_scan -> recent_scan | inherits the fallback |
| `project_grounding` (`lib.rs:525`) digests repo trees for agent grounding | unresolved -> "local folder not found" note | wording: "source deleted on <date>" for `Deleted`; rule and scan-result grounding unaffected |

### 6.4 Workspace surfaces (F)

| Where | Today | Fix |
|---|---|---|
| `checkout_status` (`lib.rs:10725`) via `checkout_status_resolved` (`workspace.rs:137`) | missing dir -> `detail: "not cloned yet"` | `RepoCheckout` gains `source: RepoSourceStatus`; `detail` becomes "source deleted on <date>" for `Deleted` |
| `checkout_project` ("Clone / update all repos", `lib.rs:10773`) | would re-clone a deleted repo | skips `Deleted` repos (section 3.8) |
| Workspace repo card (`crates/ui/src/workspace.rs:805-915`) | two states: cloned / not cloned | four states, section 6.6 |
| `project_suppressions` (`lib.rs:7921`) | reads the tree, pulls first | `source_deleted` response; no pull on GET (section 11) |

### 6.5 Needs the source by nature (S): disable with the explanation, no further change

These resolve the folder because they scan it, write governance files into it, or develop in it.
Each already fails with a message when the folder is missing; the UI now disables their controls
up front with `scan_block_text` and the server returns 409 `source_deleted` before resolving.

Scan: `onboard_scan` (`lib.rs:5102`, `resolve_local_sources` `:5068` which requires
`<dir>/.git`), `onboard_audit` (`:5531`), `onboard_audit_start` (`:6620`),
`rescan_alternatives_handler` (`:4358`, re-reads source at `:4468`), the CLI `camerata inspect`
(`inspect_cmd.rs:52-55`, `validate_args`).

Apply/arm (write into the repo): `onboard_apply_preflight` (`:7253`), `onboard_apply`
(`:7350`), `emit_project_local` (`:9172`), `generate_ci_workflow` (`:8705`, writes the workflow
file at `:8743`), `onboard_greenfield` (`:8796`), `onboard_arm`, `onboard_open_pr`.

Governed development (edit, build, test, ship): `spawn_brownfield_dev_run` (`:2067`),
`start_governed_run` (`:2302`), `sign_off_run` (`:3234`), `answer_clarification` (`:3450`),
`reconcile_project` (`:4993`), `decompose_propose` (`:9494`), `answer_escalation` (`:9894`),
`checkout_branch` (`:10830`), `ship_repo`, `git_branches` through `git_cherry_pick`
(`:10955-11180`, all via `resolve_git_dir` `:10939`), `uow_author` (`:11961`), `design_author`
(`:12779`), `uow_intake_ship` (`:13527`), `spawn_investigation_agent_run` (`:14599`),
`collect_integration_waivers` (`:14806`), `uow_list_branches` (`:14909`), `uow_update_branch`
(`:14951`), `uow_pr_open` (`:15092`), `uow_pr_resolve` (`:15286`),
`orchestrator_message.rs:103`, `active_repo_dirs`/`active_repo_dir` (`:652-700`, the agents'
read scope; a deleted repo is simply absent from it), `ensure_uow_worktree`
(`workspace.rs:697`). The cockpit already hides `GovernedDevPage` behind readiness; with deleted
repos no longer pausing readiness, `GovernedDevPage` instead shows the `scan_block_text` banner
and keeps its controls disabled (one `if let Some(block)` at the page top).

Unaffected (read no repo): rule drift (`drift_for_projects`, `lib.rs:15756`, compares
`selection.applied_hash` with the corpus), the corpus staleness check, the release gate, the
project export/import, the worktracker adapters, routines.

### 6.6 The Workspace repo card: four states

Rendered from `RepoSourceStatus` by one pure mapping in `camerata_ui_core::source::card_state`
(tested without a browser), words and pixels only in `crates/ui/src/workspace.rs`:

1. **Present, at the inspected commit** (`newer_commits == Some(0)`): badge "Source at
   `3f2a9c1` (the inspected commit)". Actions as today (branch, git panel). Danger zone below.
2. **Present, newer**: badge "Source at `9b7e21c`, 14 commits newer than the inspection
   (`3f2a9c1`)". PRIMARY button "Rescan changes" (section 8); secondary "Full re-inspection"
   (the existing scan). Danger zone below. If `inspected_commit_reachable == false`: badge
   "Source at `9b7e21c`; the inspected commit `3f2a9c1` is not in this clone (history was
   rewritten). Finding code views for the inspection are unavailable." and only "Full
   re-inspection" is offered.
3. **Deleted**: badge (class `ws-repo-state deleted`, `--danger-color`) "Source deleted on 24
   Oct 2026" with the one-line summary "2,729 files, 182.9 MB removed; record del-...".
   PRIMARY button "Re-fetch source" (section 6.8). Links: "View deletion record" (opens the
   2.3.1 text in a modal with "Copy record" and "Save record as .txt"). No branch field, no git
   panel, no danger zone.
4. **Unresolved / Mismatch**: exactly today's "Not cloned" card with the link affordance
   (`workspace.rs:910`), plus the mismatch reason when present.

### 6.7 The danger zone and the typed confirmation

Inside the card for states 1 and 2, a collapsed `details` element titled "Danger zone" (muted),
containing one button `.btn-danger-outline` "Delete source code from this machine" and the
sentence "Removes the checkout and every other copy Camerata holds. Findings and exports stay."

Clicking it calls `delete-preview` and opens a modal (`crates/ui/src/source_danger.rs`,
props-only like `FindingDetailPane`):

- Title: "Delete acme/app from this machine".
- The plan: the item list from the preview (path, files, size) and the totals.
- Warnings in amber: "This checkout has uncommitted changes (<n> files)" and "<branch> has <n>
  commits not on GitHub", each present only when true, with ONE checkbox "Delete uncommitted work
  too" that enables the action when either warning is present.
- The method sentence (2.3.2) in muted text.
- Input labelled "Type `acme/app` to confirm" (monospace placeholder shows the exact string).
- Button `.btn-solid-danger` "Delete source code", `disabled` until `confirm_enabled(typed,
  repo, warnings_present, ticked)` (pure, in `ui_core::source`) is true: `typed == repo`
  byte-for-byte AND (no warnings OR ticked). No paste blocking; the exact match is the gate.
- Busy state: button text "Deleting..."; all inputs disabled.
- Result: the record rendering (2.3.1) with "Copy record" and "Save record as .txt", and a
  "Done" button. The card re-fetches `/source` and shows state 3.
- Refusal (409): the message in red inside the modal; nothing else changes.

### 6.8 The re-fetch dialog

Opened by "Re-fetch source". On open it calls `/source/remote`; until it answers, the radios
are disabled with "Checking GitHub...".

- Radio A (pre-selected): "Latest from `main` (`9b7e21c`)". When the inspected sha equals the
  remote head: "Latest from `main` (`3f2a9c1`, the inspected commit)".
- Radio B: "The inspected commit `3f2a9c1`, 24 Oct 2026" (date from the inspection entry).
  Hint: "For reproducing or disputing a finding. The clone still has full history."
- Destination line: "Into /Users/auditor/Camerata/acme/app".
- Errors from `/source/remote` replace the radios with the message and a "Retry" button; "Add a
  token" links to Credentials for `no_token`.
- Button "Re-fetch" (primary); busy "Cloning..." (a full clone can take minutes; the dialog
  stays open, the request is synchronous, `kill_on_drop` on the git child protects the server).
- Success closes the dialog; the card shows state 1 or 2. `renamed_to` shows the rename sentence
  under the badge. `commit_not_on_remote` (radio B) shows its message and leaves the Latest
  clone in place.

### 6.9 The finding views

In the finding modal (`FindingDetailPane`) and later the code viewer:

- `status: "source_deleted"`: the block area shows "Source deleted on 24 Oct 2026, re-fetch to
  view" with a link to the Workspace tab. The stored `snippet` is still shown above as today.
- `status: "ok"` with `source: "git"` and the repo status `Present` with `newer_commits > 0`:
  a muted note under the block "References commit `3f2a9c1`; source on disk is newer".
- `status: "commit_unavailable"`, `source: "working_tree"`: amber banner "Inspected commit not
  available in this clone; showing the working tree" (the work order's wording).

---

## 7. Re-fetch: command, history, auth, selection, failures

`client_source::refetch(state, project, repo, target) -> Result<RefetchOutcome, Refusal>`.

1. **Preconditions.** Status is `Deleted` or `Unresolved` (re-fetching over a `Present` clone is
   refused with `already_present`: "acme/app is already on disk at <path>; use Clone / update or
   the git panel"). Token from `state.github_token()` else `no_token`. Per-repo in-flight mutex
   (R12). No running scan job for the project.
2. **Destination.** The path from `resolve_repo_dir(settings.repo_path(repo),
   settings.workspace_root(), repo)`: the recorded override when one exists (so a hand-linked
   folder comes back where it was), else the workspace layout. If neither exists, `no_path`
   ("Pick a workspace folder first", same as `checkout_project`). The destination must not exist
   or must be an empty directory (`destination_not_empty`). Parents are created.
3. **Headroom.** `ensure_disk_headroom(parent, disk_headroom_threshold_bytes())`
   (`workspace.rs:1005,961`).
4. **Clone, full history.** New `workspace::clone_full(dir, repo, token) -> anyhow::Result<()>`:
   `git clone --no-single-branch <authed_url(repo, token)> <dir>` (explicitly no `--depth`, no
   `--filter`; `--no-single-branch` is the default and is passed so the intent is in the command
   line and the test can assert it), then `git remote set-url origin <clean_url(repo)>` (the
   existing scrub), then a test-asserted check that `.git/config` does not contain `x-access-token`.
   `clone_or_pull` (`:219`) is left as is for the Workspace "Clone / update" flow; it also clones
   full history already. The git child runs with `kill_on_drop(true)` and no timeout (a 2 GB repo
   on a slow link is legitimate); the UI shows progress as "Cloning..." only.
5. **Checkout per target.** `latest`: nothing further (clone leaves HEAD on the default branch;
   `default_branch` recorded from `git symbolic-ref refs/remotes/origin/HEAD`). `commit { sha }`:
   `git cat-file -e <sha>^{commit}`; if it fails, `git fetch origin <sha>` (GitHub serves
   reachable-by-ref commits only); if still absent, return `commit_not_on_remote` with the Latest
   clone left in place; else `git checkout --detach <sha>`.
6. **Rename detection.** `detect_remote_repo(dir)` after clone; when the identity differs from
   `repo`, set `renamed_to` (the clone is still valid; GitHub redirected).
7. **Record and status.** Append `RefetchRecord { repo, path, sha: HEAD, target, completed_at }`;
   return `RepoSourceStatus::Present` computed fresh (so `newer_commits` and
   `inspected_commit_reachable` are real).

Failure classification from git stderr (one pure function `classify_git_failure(stderr) ->
RefetchError`, tested with captured strings): contains "Authentication failed" or "could not
read Username" or "Invalid username or token" -> `auth_failed`; "Repository not found" or "not
found" with status 128 -> `repo_not_found`; "Could not resolve host" or "unable to access" ->
`network`; anything else -> `network` with the trimmed stderr as the message. The token never
appears in any message: `authed_url` is replaced by `clean_url` in stderr before classification
(`redact_token(stderr, token)`), tested.

Why the token is safe: it lives only in the clone and fetch command lines, as today; `ps` can see
it for the duration, which is the existing accepted exposure (`workspace.rs:10-13`).

---

## 8. The delta rescan ("Rescan changes"), V1 minimum

`client_source::rescan_changes(state, project, repo) -> Result<job_id, Refusal>`.

1. Load `E = inspections.latest(project_id)`; find `E.repos[repo]` with `sha = inspected`.
   Refuse `no_inspection` when absent. Status must be `Present` with
   `inspected_commit_reachable` (else `inspected_commit_unreachable`).
2. `new = git rev-parse HEAD`. If `new == inspected` and the tree is clean, return 200
   `{ "ok": true, "nothing_changed": true }` (no job).
3. `git diff --name-status -z -M <inspected> <new>` in the repo. Parse into
   `changed: HashSet<String>` containing every path on either side of A/M/D/R/C/T lines (a
   rename marks both the old and new path).
4. Collect the current tree exactly as the scan does: `read_local_repo_files(dir)`
   (`onboard/files.rs:486`), so exclusions and the 400 KB cap are identical.
5. Build the synthetic prior:
   ```rust
   ScanManifest {
       version: MANIFEST_VERSION,
       rules_fingerprint: <fingerprint of the project's CURRENT selected rules, same call audit_repos makes>,
       files: { repo => { path => content_fingerprint(content) for every (path, content) in the current
                         tree whose path is NOT in `changed` } },
       findings: E.report.findings.iter().filter(|f| f.repo == repo && !changed.contains(&f.path)
                 && f.is_ai_advisory()).cloned(),   // deterministic findings are recomputed, as today
       ai_covered: E.report.ai_ran(),  // false when the inspection was floor-only: then nothing is carried
       clone_verdicts: state.scan_cache.get(project_id).map(|m| m.clone_verdicts).unwrap_or_default(),
   }
   ```
   `partition` (`scan_cache.rs:164`) then sees every unchanged file as unchanged (its fingerprint
   is, by construction, the fingerprint of its current bytes), so only `changed` files go to the
   AI audit, and the inspected findings for unchanged files are carried forward with their line
   numbers intact. If the operator changed the rule selection since E, `matches_rules` fails
   (`onboard.rs:1422`) and the engine does a full re-scan, which is the right answer and is
   already noted in `notes`.
6. Dirty inspection: if `E.repos[repo].dirty` was true, add the note "The inspection ran on a
   tree with uncommitted changes; the delta is computed from the committed state
   (<inspected>). Consider a full re-inspection." to the job's notes and the response.
7. Run through the same job path as `onboard_audit_start` (`lib.rs:6620`): the handler builds
   the same request the UI's scan start builds (selected rules from `project.ruleset`, AI review
   on, floor on, project backend) with `incremental_prior: Some(&prior)`, and returns `job_id`.
8. On completion the usual `set_last_scan` runs; the new entry is written with
   `kind: DeltaRetest { base_inspection_id: E.id, from_sha: inspected, to_sha: new }`. The review
   UI shows it as the current scan. The scan surface's header shows "Delta retest of changes
   since `3f2a9c1` (14 commits, <n> files changed)". A side-by-side "what changed" view is V2.

The server scan cache is purged on deletion (P3) and is NOT needed here: the git diff is the
authority on what changed, and the inspected findings come from the persisted entry.

---

## 9. The contract the code viewer will rely on

One resolver, used by `finding-context` now and by `source-file`, `symbol-refs` and the batch
context endpoint later: `client_source::source_for_read(state, repo, sha: Option<&str>) ->
SourceRead`.

```rust
pub enum SourceRead {
    /// Read through git objects at `sha` (the inspected commit by default).
    Git { dir: PathBuf, sha: String, head_differs: bool },
    /// No sha is known (the scan ran on a non-git folder): working tree, labelled.
    WorkingTree { dir: PathBuf },
    /// The repo was deleted and not re-fetched.
    Deleted { deleted_at: String, record_id: String },
    /// No folder and no deletion record (never linked / imported project).
    Unresolved { reason: String },
}
```

Resolution: `sha` = the query's `sha` if given (validated 40-hex, else 400) else the latest
inspection entry's `AuditedRef.sha` for `repo`. Status `Deleted` wins over everything (the
directory is gone by construction). `Present` + sha -> `Git`; `Present` + no sha -> `WorkingTree`.

`finding_context::lookup` gains a `FileSource` parameter: `Git { dir, sha }` reads with
`git show <sha>:<path>` (run with `--` and the path as one argument, `-c core.quotepath=off`,
stdout capped at `MAX_FILE_BYTES` + 1 to detect `too_large` without buffering the whole blob),
`WorkingTree { dir }` reads as today. Path containment (`resolve_contained_path`, `:204`) runs
BEFORE either read; for the git form the relative path is additionally rejected when it starts
with `/`, contains `..` segments, or contains a NUL. Redaction stays at `:153,174`.

Statuses the viewer (and the modal) handle, all 200 unless noted:

| `status` | `source` | When | UI text |
|---|---|---|---|
| `ok` | `git` | object read at the inspected sha | none; plus "References commit <sha>; source on disk is newer" when `head_differs` |
| `ok` | `working_tree` | no sha known | "Showing the working tree (the scan ran on a folder without git history)" |
| `commit_unavailable` | `working_tree` | sha given/known but `git show` exits 128 ("does not exist in"/"bad object") AND the directory exists | amber "Inspected commit not available in this clone; showing the working tree" (the handler then performs the working-tree read and includes `lines`) |
| `source_deleted` | none | `SourceRead::Deleted` | "Source deleted on <date>, re-fetch to view" (`deleted_at`, `record_id` in the body) |
| `file_missing` | git or working_tree | path not in the tree at that sha / not on disk; also `Unresolved` | as today |
| `not_utf8`, `too_large`, `line_gone` | as today | | as today |
| `path_traversal` | 400 | | as today |

Invariant (tested): when the repo directory does not exist, the handler never calls
`tokio::fs::read` on anything under it and never returns `source: "working_tree"`.
`commit_unavailable` is the ONLY labelled fallback, and only while the directory exists.

`Cache-Control: no-store` is set on `finding-context`, `source-file`, `symbol-refs` and the
batch endpoint so the desktop webview's HTTP cache never retains code lines (section 11, D4).

---

## 10. Tests, by name

Fixtures: every git test builds a bare "remote" with `git init --bare` in a temp dir, a working
clone of it with `init_repo_with_origin`-style helpers, and uses `file://` URLs; `clone_full`
takes the URL through `authed_url`, so the tests pass a URL override hook
(`workspace::clone_full_from(url, dir)`) used by `clone_full`. No network, no model, no `claude`.
A test seam for the state directories: `CAMERATA_DATA_DIR` (honoured by `AppState::from_env`
before `dirs::data_dir()`) and `CAMERATA_STATE_DIR` (honoured by `scan_cache::state_dir::dir()`
before the platform default), both read ONLY when set, both documented in TECHNICAL.md.

### 10.1 `client_source` guard (server unit tests, `crates/server/src/client_source.rs`)

- `guard_accepts_matching_https_origin`
- `guard_accepts_matching_ssh_origin`
- `guard_accepts_case_different_origin` (origin `Acme/App`, project `acme/app`)
- `guard_accepts_trailing_slash_and_dot_git_variants`
- `guard_refuses_repo_not_in_project`
- `guard_refuses_when_no_path_is_recorded`
- `guard_refuses_missing_directory_as_nothing_to_delete`
- `guard_refuses_symlinked_path`
- `guard_refuses_protected_paths_root_home_workspace_root_and_data_dir`
- `guard_refuses_non_git_folder`
- `guard_refuses_folder_without_origin`
- `guard_refuses_non_github_origin`
- `guard_refuses_origin_mismatch_prefix_lookalike` (safe twin `acme/app-2`)
- `guard_refuses_origin_mismatch_different_owner`
- `guard_refuses_confirmation_mismatch_including_case_and_whitespace`
- `guard_refuses_dirty_tree_without_opt_in`
- `guard_refuses_unpushed_commits_without_opt_in`
- `guard_refuses_branch_with_no_upstream_as_unpushed`
- `guard_accepts_dirty_and_unpushed_with_opt_in`
- `guard_refuses_while_scan_job_is_running`
- `guard_refuses_worktree_in_protected_location`
- `guard_runs_checks_in_documented_order_and_reports_the_first`

### 10.2 Deletion (positive, record, idempotence)

- `delete_removes_checkout_and_writes_record_with_counts`
- `delete_record_items_match_preview_plan`
- `delete_removes_registered_worktree_inside_checkout`
- `delete_removes_registered_worktree_outside_checkout_and_prunes`
- `delete_purges_server_scan_cache_for_that_repo_only` (multi-repo project: the other repo's
  `files` and findings survive)
- `delete_purges_cli_cache_and_ledger_under_state_dir_by_path_key`
- `delete_purges_recorded_model_session_files_and_leaves_unrecorded_ones`
- `delete_clears_transcripts_and_finished_jobs_for_the_project`
- `delete_keeps_onboarding_draft_and_inspection_entry_snippets`
- `delete_keeps_repo_path_override_so_refetch_returns_to_the_same_folder`
- `delete_removes_empty_owner_parent_only_under_workspace_root`
- `delete_second_call_reports_already_deleted_without_new_record`
- `delete_records_partial_when_one_item_fails` (a read-only worktree dir)
- `delete_record_text_rendering_matches_fixture` (2.3.1 golden)
- `delete_record_never_contains_a_token` (origin captured after scrub; stderr redaction)

### 10.3 Purge completeness (the enumerating test)

`purge_completeness_no_client_file_remains_in_any_known_location` (`crates/server/tests/
source_deletion_e2e.rs`): builds a fixture repo with TWO sentinel strings, `SENTINEL_FINDING`
inside a line that trips a deterministic floor rule (so an excerpt is legitimately kept) and
`SENTINEL_SILENT` inside an otherwise clean file; points `CAMERATA_DATA_DIR`, `CAMERATA_STATE_DIR`
and a fake `HOME` (for `~/.claude/projects` and `~/.semgrep`) at temp dirs; runs a floor-only
scan through `onboard_audit_start` AND `camerata inspect` (CLI, so both caches exist); plants a
fake recorded model session file; creates a UoW worktree; then deletes. Assertions: walking
`client_source::KNOWN_STORAGE_LOCATIONS(state)` (a `pub fn` returning every path in section 5
plus the "checked, not purged" list, so a new store cannot be added without appearing here) finds
`SENTINEL_SILENT` in zero files; `SENTINEL_FINDING` appears ONLY in `onboarding-draft.json`,
`inspections/<id>.json` and the record's "Kept" line; the fixture's worktree, checkout, cache
files and session file do not exist. A companion `known_storage_locations_covers_every_store_opened_in_from_env`
source-greps `lib.rs` for `dir.join("` inside `from_env` and asserts each basename is listed in
`KNOWN_STORAGE_LOCATIONS` (purged or checked).

### 10.4 Project fully usable after deletion (end to end)

`project_fully_usable_after_source_deletion_e2e` (same file): scan a fixture (floor only), export
a package (200, release gate passes), delete the source, DROP the `AppState` and rebuild it from
the same `CAMERATA_DATA_DIR` (restart), then assert: `GET /readiness` is `ready`; `GET /source`
shows `deleted` with the date; `GET /onboard/draft` still carries the findings; `POST
/product-export` returns 200 with a byte-identical `findings.json` to the pre-deletion export
(provenance and generated_at are scan-time values, so the zip's datasets match; the manifest
hashes match); `POST /audit-report` 200; `GET /onboard/finding-context` returns
`source_deleted`; `POST /onboard/audit/start`, `/onboard/apply`, `/rescan-alternatives`,
`/rescan-changes` return 409 `source_deleted`; `GET /suppressions` returns `source_deleted` with
empty entries; `GET /checkout` detail reads "source deleted on ...".

### 10.5 Re-fetch

- `refetch_latest_clones_full_history` (asserts `git rev-list --count --all` equals the remote's
  and `git rev-parse --is-shallow-repository` is `false`)
- `refetch_passes_no_depth_and_no_single_branch` (command-line assertion through the URL hook)
- `refetch_scrubs_token_from_git_config`
- `refetch_inspected_commit_checks_out_detached_sha`
- `refetch_reports_commit_not_on_remote_and_keeps_latest_clone`
- `refetch_refuses_non_empty_destination`
- `refetch_uses_recorded_override_path_when_present`
- `refetch_clears_deleted_state_and_keeps_deletion_records`
- `refetch_reports_newer_commit_count_and_reachability`
- `refetch_detects_renamed_origin`
- `classify_git_failure_maps_auth_not_found_and_network` (captured stderr strings)
- `redact_token_removes_authed_url_from_stderr`
- `remote_probe_parses_symref_default_branch_and_head`
- `refetch_refuses_when_already_present`
- `refetch_refuses_while_scan_job_is_running`

### 10.6 Delta rescan

- `rescan_changes_audits_only_files_changed_since_inspected_sha` (a recording LLM stub asserts
  the changed file is in the prompt and the unchanged one is not)
- `rescan_changes_carries_forward_ai_findings_for_unchanged_files_with_line_numbers`
- `rescan_changes_drops_findings_for_deleted_files_and_treats_renames_as_changed`
- `rescan_changes_falls_back_to_full_when_rules_changed`
- `rescan_changes_nothing_changed_returns_without_a_job`
- `rescan_changes_warns_when_inspection_was_dirty`
- `rescan_changes_refuses_when_inspected_commit_unreachable`
- `rescan_changes_refuses_on_deleted_source`
- `rescan_changes_writes_delta_retest_entry_with_from_and_to_sha`
- `synthetic_prior_fingerprints_equal_partition_fingerprints` (unit, pure)

### 10.7 Finding context / viewer contract

- `finding_context_reads_inspected_commit_not_working_tree` (edit the tree after the "scan";
  the response shows the committed line)
- `finding_context_reports_source_deleted_with_date_and_record`
- `finding_context_commit_unavailable_labels_working_tree_fallback`
- `finding_context_never_reads_fs_when_directory_missing` (a `tokio::fs::read` counter seam)
- `finding_context_git_read_rejects_dotdot_and_absolute_paths`
- `finding_context_git_read_applies_too_large_cap_and_redaction`
- `finding_context_defaults_sha_to_latest_inspection_entry`
- `code_endpoints_set_cache_control_no_store`

### 10.8 Inspection store and exports

- `inspection_entry_written_on_floor_scan_sync_audit_and_job_completion`
- `alternatives_rescan_updates_latest_entry_in_place`
- `get_last_scan_falls_back_to_persisted_entry_after_restart`
- `export_product_works_after_restart_and_records_package`
- `exports_never_touch_resolve_repo_dir_or_read_the_repo` (source-level: the bodies of
  `export_product`, `export_audit_report`, `export_deep_report`, and the files `report_export.rs`,
  `xlsx_export.rs`, `onboard/report.rs`, `client_provenance.rs`, `release_gate.rs` contain none
  of `resolve_repo_dir`, `repo_path(`, `read_local_repo_files`, `tokio::fs::read(`,
  `std::fs::read_to_string(` outside `#[cfg(test)]`; follows the `lib.rs:26224` pattern)
- `audited_ref_captures_origin_url_and_default_branch`
- `model_sessions_recorded_on_cli_backend_and_empty_on_api`

### 10.9 Readiness and gating (pure, `app-core` + `ui-core`)

- `classify_readiness_treats_deleted_as_not_gating`
- `classify_readiness_bool_wrapper_unchanged`
- `scan_block_lists_deleted_repos_with_dates`
- `scan_block_none_when_all_present`
- `unresolved_repos_excludes_deleted`
- `card_state_maps_each_source_status` (four states, newer/at-inspected/unreachable variants)
- `confirm_enabled_requires_exact_name_and_tick_when_warned`
- `refetch_dialog_default_is_latest_and_labels_inspected_commit_when_equal`
- `source_status_deleted_only_when_record_newer_than_refetch`

### 10.10 UI render tests (SSR, `crates/ui`)

- `repo_card_deleted_renders_badge_summary_and_refetch_primary`
- `repo_card_newer_renders_commits_newer_and_rescan_changes_primary`
- `repo_card_at_inspected_commit_renders_badge_and_danger_zone`
- `repo_card_unreachable_commit_offers_only_full_reinspection`
- `danger_modal_confirm_disabled_until_exact_name`
- `danger_modal_shows_tick_only_when_warnings_present`
- `danger_modal_renders_refusal_message`
- `onboard_view_renders_findings_with_deleted_source_and_disabled_rescan_controls`
- `finding_pane_renders_source_deleted_and_newer_source_notes`
- `resolve_modal_does_not_open_for_deleted_repos`

---

## 11. Defects found on the way (fix in this work, small commits)

- **D1 ruff writes into the client repo.** `ruff check` (`scan_tools.rs:1541`) runs with
  `current_dir = repo` and no `--no-cache`, so `.ruff_cache/` lands in the client's tree (the
  same class as the old in-repo scan cache the 2026-10-18 fix removed). Add `--no-cache`. Test:
  `ruff_args_pass_no_cache`.
- **D2 semgrep metrics.** `scan_tools.rs:1519` passes no `--metrics=off`; semgrep's default
  sends anonymous metrics (rule ids, hashes, no code) to its vendor. Add `--metrics=off`. Test:
  `semgrep_args_disable_metrics`.
- **D3 a GET mutates the checkout.** `project_suppressions` (`lib.rs:7921`) runs
  `pull_local_ff` on every read. After a re-fetch at the inspected commit (detached HEAD) the
  pull fails harmlessly, but on a branch it moves the tree past the inspection on a read. Remove
  the pull; the Workspace "Clone / update" button is the refresh. Test:
  `suppressions_get_does_not_touch_the_checkout`.
- **D4 webview cache.** Code lines returned by `finding-context` can be retained by the desktop
  webview's HTTP cache. `Cache-Control: no-store` on every code-returning endpoint (section 9).
- **D5 exports lost on restart.** `last_scan` in memory only (`lib.rs:190-199`); fixed by the
  inspection store (section 2.1). Named here because it is a client-visible defect today.

---

## 12. Build order (small green commits; Sonnet builds, Fable reviews)

1. `feat(server): InspectionStore persists each completed inspection; get_last_scan falls back`
   (sections 2.1, 10.8 first four tests; `CAMERATA_DATA_DIR` seam).
2. `feat(server): AuditedRef captures origin_url and default_branch; model sessions recorded`
   (2.2, 5.1; `llm.rs` `--session-id`; the probe documented in the commit message).
3. `refactor(server,cli): one state-dir key implementation shared by server and CLI`
   (P4/P5; `CAMERATA_STATE_DIR` seam; CLI re-exports; existing CLI tests unchanged).
4. `feat(server): source status, deletion and refetch records, GET /source and /source/records`
   (2.3, 2.4, 3.1, 3.4; 10.9 `source_status_deleted_only_when_record_newer_than_refetch`).
5. `feat(server): delete-preview and delete with the remote-match guard and the purge list`
   (3.2, 3.3, 4, 5; 10.1, 10.2, 10.3 including the semgrep probe outcome; `ScanCacheStore::remove_repo`).
6. `feat(app-core,ui-core,server): deleted source does not pause readiness; scan block reason`
   (6.1 server side, 10.9 readiness tests; `RepoResolution` fields; 409 `source_deleted` on the
   scan/apply/rescan endpoints; `checkout_project` skip; suppressions response).
7. `feat(server): finding-context reads git objects at the inspected sha; source_deleted status`
   (9, 10.7, D4).
8. `feat(server): re-fetch endpoint, remote probe, full-history clone`
   (3.5, 3.6, 7, 10.5).
9. `feat(server): rescan-changes delta via git diff and a synthetic prior`
   (3.7, 8, 10.6).
10. `feat(ui): repo card states, danger zone with typed confirmation, re-fetch dialog, finding notes`
    (6.6 to 6.9, 10.10; `ui_core::source` pure helpers).
11. `fix(server): ruff --no-cache, semgrep --metrics=off, suppressions GET stops pulling`
    (11 D1 to D3).
12. `test(server): project fully usable after deletion e2e; purge completeness e2e`
    (10.3, 10.4, wired into the normal test run; they need only `git`).
13. `docs: TECHNICAL.md section 7 (new stores, seams), USER_GUIDE "Delete and re-fetch source",
    decisions log entry`.

Each commit leaves `cargo test --workspace` green; commits 1 to 9 ship no UI change, so the app
keeps working at every step.

---

## 13. Owner questions (only the ones that are genuinely his)

1. **Deleting Claude Code's own files.** When a project runs on the `Cli` backend (the default),
   every model call goes through the `claude` command, and that program saves a copy of the whole
   conversation, including the client source it was shown, in its own folder
   (`~/.claude/projects/...`), outside Camerata. The design has Camerata tag each call with an id,
   remember the ids, and delete exactly those files during "Delete source". That means Camerata
   removes files belonging to another program in your home folder. Alternatives: (a) do that
   (recommended; it is the only way the record can honestly say the code is gone); (b) refuse to
   delete while any recorded session file exists and tell you to clear Claude Code's history by
   hand; (c) require the `Api` backend for client projects, which never writes such files. Which
   do you want? If (a), the first build step runs a one-call probe to confirm the file is where
   the design says and contains the prompt; if the installed `claude` version behaves
   differently, you will be told before anything is built on it.

2. **Uncommitted or unpushed work.** If the checkout has changes that are not on GitHub (from
   Camerata's own remediation work, or yours), deletion refuses unless you tick "Delete
   uncommitted work too" in the same dialog. This is an engineering call (a refusal is cheaper
   than silent loss); say so if you would rather it never ask and simply refuse, or simply delete.

Everything else in this document is an engineering decision made here; where it narrows a
binding decision, the narrowing is stated next to it (the deletion record stays local and is
handed over as text rather than travelling in the config-only project export; worktree
enumeration uses git's own list rather than a path convention; the delta uses git's diff rather
than the purged cache).
